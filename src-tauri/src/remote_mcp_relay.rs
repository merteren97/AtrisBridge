use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Instant as StdInstant,
};

use futures_util::{
    stream::{SplitSink, SplitStream},
    FutureExt, SinkExt, StreamExt,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};
use tokio::{
    sync::{mpsc, Semaphore},
    time::{interval, sleep, timeout, Duration, MissedTickBehavior},
};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        client::IntoClientRequest,
        http::{header::AUTHORIZATION, HeaderValue},
        Message,
    },
    MaybeTlsStream, WebSocketStream,
};
use uuid::Uuid;

use crate::{
    atris_auth::{self, AtrisHubAuthState, DesktopSessionCredential},
    remote_mcp_protocol::{self, RelayRequest, ResponseBinding},
};

const RELAY_URL: &str = "wss://atrishub.com/api/mcp/relay/v1/connect";
const CONNECTOR_URL: &str = "https://atrishub.com/api/mcp/v1/mcp";
const RELAY_READY_HEADER: &str = "x-atris-relay-ready";
const MAX_RELAY_PAYLOAD_BYTES: usize = 6 * 1024 * 1024;
const MAX_LOCAL_INFLIGHT: usize = 32;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const RELAY_READY_TIMEOUT: Duration = Duration::from_secs(15);
const AUTH_CHECK_INTERVAL: Duration = Duration::from_secs(15);
const WATCHDOG_INTERVAL: Duration = Duration::from_secs(10);
const WATCHDOG_RESPONSE_TIMEOUT: Duration = Duration::from_secs(25);
const SIGNED_OUT_RETRY: Duration = Duration::from_secs(5);
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(30);

type RelaySocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
type RelayWriter = SplitSink<RelaySocket, Message>;
type RelayReader = SplitStream<RelaySocket>;

#[derive(Clone, Default)]
pub struct RemoteMcpRelayManager {
    inner: Arc<Mutex<RelayManagerRuntime>>,
}

#[derive(Default)]
struct RelayManagerRuntime {
    started: bool,
    state: RelayLifecycleState,
    wake_tx: Option<mpsc::UnboundedSender<()>>,
    clients: HashMap<String, RemoteMcpClientRecord>,
    last_error: Option<String>,
    last_attempt_at: Option<String>,
    last_connected_at: Option<String>,
    last_frame_at: Option<String>,
    last_server_ping_at: Option<String>,
    last_pong_at: Option<String>,
    last_disconnect_code: Option<String>,
    reconnect_attempts: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteMcpClientRecord {
    pub principal: String,
    pub display_name: String,
    pub first_seen_at: String,
    pub last_seen_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteMcpRelayStatus {
    pub started: bool,
    pub state: &'static str,
    pub observed_clients: usize,
    pub connector_url: &'static str,
    pub last_error: Option<String>,
    pub last_attempt_at: Option<String>,
    pub last_connected_at: Option<String>,
    pub last_frame_at: Option<String>,
    pub last_server_ping_at: Option<String>,
    pub last_pong_at: Option<String>,
    pub last_disconnect_code: Option<String>,
    pub reconnect_attempts: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum RelayLifecycleState {
    #[default]
    SignedOut,
    Connecting,
    Online,
    Reconnecting,
}

impl RelayLifecycleState {
    fn as_str(self) -> &'static str {
        match self {
            Self::SignedOut => "signed_out",
            Self::Connecting => "connecting",
            Self::Online => "online",
            Self::Reconnecting => "reconnecting",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionExit {
    CredentialChanged,
    RemoteClosed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RelayReadyOutcome {
    Ready,
    RestartRequested,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RelayReady {
    #[serde(rename = "type")]
    kind: String,
    version: u32,
    device_id: String,
    desktop_session_id: String,
    session_generation: u64,
    connection_id: String,
}

pub fn setup(app: &AppHandle) -> Result<(), String> {
    let manager = app.state::<RemoteMcpRelayManager>().inner().clone();
    let (wake_tx, wake_rx) = mpsc::unbounded_channel();
    {
        let mut runtime = manager
            .inner
            .lock()
            .map_err(|_| "AtrisBridge remote MCP manager state is unavailable.".to_string())?;
        if runtime.started {
            return Ok(());
        }
        runtime.started = true;
        runtime.wake_tx = Some(wake_tx);
    }

    let relay_app = app.clone();
    tauri::async_runtime::spawn(async move {
        relay_loop(relay_app, manager, wake_rx).await;
    });
    Ok(())
}

fn status_snapshot(runtime: &RelayManagerRuntime) -> RemoteMcpRelayStatus {
    RemoteMcpRelayStatus {
        started: runtime.started,
        state: runtime.state.as_str(),
        observed_clients: runtime.clients.len(),
        connector_url: CONNECTOR_URL,
        last_error: runtime.last_error.clone(),
        last_attempt_at: runtime.last_attempt_at.clone(),
        last_connected_at: runtime.last_connected_at.clone(),
        last_frame_at: runtime.last_frame_at.clone(),
        last_server_ping_at: runtime.last_server_ping_at.clone(),
        last_pong_at: runtime.last_pong_at.clone(),
        last_disconnect_code: runtime.last_disconnect_code.clone(),
        reconnect_attempts: runtime.reconnect_attempts,
    }
}

#[tauri::command]
pub fn remote_mcp_relay_status(
    manager: State<'_, RemoteMcpRelayManager>,
) -> Result<RemoteMcpRelayStatus, String> {
    let runtime = manager
        .inner
        .lock()
        .map_err(|_| "AtrisBridge remote MCP manager state is unavailable.".to_string())?;
    Ok(status_snapshot(&runtime))
}

#[tauri::command]
pub fn retry_remote_mcp_relay(
    manager: State<'_, RemoteMcpRelayManager>,
) -> Result<RemoteMcpRelayStatus, String> {
    let sender = {
        let runtime = manager
            .inner
            .lock()
            .map_err(|_| "AtrisBridge remote MCP manager state is unavailable.".to_string())?;
        if !runtime.started {
            return Err("AtrisBridge remote MCP relay has not started.".into());
        }
        runtime.wake_tx.clone()
    };
    let sender = sender
        .ok_or_else(|| "AtrisBridge remote MCP relay wake channel is unavailable.".to_string())?;
    sender
        .send(())
        .map_err(|_| "AtrisBridge remote MCP relay worker is unavailable.".to_string())?;

    let runtime = manager
        .inner
        .lock()
        .map_err(|_| "AtrisBridge remote MCP manager state is unavailable.".to_string())?;
    Ok(status_snapshot(&runtime))
}

#[tauri::command]
pub fn list_remote_mcp_clients(
    manager: State<'_, RemoteMcpRelayManager>,
) -> Result<Vec<RemoteMcpClientRecord>, String> {
    let runtime = manager
        .inner
        .lock()
        .map_err(|_| "AtrisBridge remote MCP manager state is unavailable.".to_string())?;
    let mut clients = runtime.clients.values().cloned().collect::<Vec<_>>();
    clients.sort_by(|left, right| {
        right
            .last_seen_at
            .cmp(&left.last_seen_at)
            .then_with(|| left.principal.cmp(&right.principal))
    });
    Ok(clients)
}

pub(crate) fn remember_remote_client(app: &AppHandle, principal: &str, display_name: &str) {
    let now = chrono::Utc::now().to_rfc3339();
    let manager = app.state::<RemoteMcpRelayManager>();
    let Ok(mut runtime) = manager.inner.lock() else {
        return;
    };
    match runtime.clients.get_mut(principal) {
        Some(client) => {
            client.display_name = display_name.to_string();
            client.last_seen_at = now;
        }
        None => {
            runtime.clients.insert(
                principal.to_string(),
                RemoteMcpClientRecord {
                    principal: principal.to_string(),
                    display_name: display_name.to_string(),
                    first_seen_at: now.clone(),
                    last_seen_at: now,
                },
            );
        }
    }
}

pub(crate) fn notify_auth_changed(app: &AppHandle) {
    let manager = app.state::<RemoteMcpRelayManager>();
    let sender = manager
        .inner
        .lock()
        .ok()
        .and_then(|runtime| runtime.wake_tx.clone());
    if let Some(sender) = sender {
        let _ = sender.send(());
    }
}

fn mark_signed_out(manager: &RemoteMcpRelayManager) {
    if let Ok(mut runtime) = manager.inner.lock() {
        runtime.state = RelayLifecycleState::SignedOut;
        runtime.clients.clear();
        runtime.last_error = None;
        runtime.last_disconnect_code = None;
        runtime.reconnect_attempts = 0;
    }
}

fn mark_connecting(manager: &RemoteMcpRelayManager) {
    if let Ok(mut runtime) = manager.inner.lock() {
        runtime.state = RelayLifecycleState::Connecting;
        runtime.last_attempt_at = Some(chrono::Utc::now().to_rfc3339());
        runtime.reconnect_attempts = runtime.reconnect_attempts.saturating_add(1);
    }
}

fn mark_online(manager: &RemoteMcpRelayManager) {
    if let Ok(mut runtime) = manager.inner.lock() {
        let now = chrono::Utc::now().to_rfc3339();
        runtime.state = RelayLifecycleState::Online;
        runtime.last_connected_at = Some(now.clone());
        runtime.last_frame_at = Some(now);
        runtime.last_error = None;
        runtime.last_disconnect_code = None;
        runtime.reconnect_attempts = 0;
    }
}

fn mark_reconnecting(manager: &RemoteMcpRelayManager, code: &str, error: &str) {
    if let Ok(mut runtime) = manager.inner.lock() {
        runtime.state = RelayLifecycleState::Reconnecting;
        runtime.last_disconnect_code = Some(code.to_string());
        runtime.last_error = Some(remote_mcp_protocol::bounded_error(error));
    }
}

fn record_frame(manager: &RemoteMcpRelayManager) {
    if let Ok(mut runtime) = manager.inner.lock() {
        runtime.last_frame_at = Some(chrono::Utc::now().to_rfc3339());
    }
}

fn record_server_ping(manager: &RemoteMcpRelayManager) {
    if let Ok(mut runtime) = manager.inner.lock() {
        let now = chrono::Utc::now().to_rfc3339();
        runtime.last_frame_at = Some(now.clone());
        runtime.last_server_ping_at = Some(now);
    }
}

fn record_pong(manager: &RemoteMcpRelayManager) {
    if let Ok(mut runtime) = manager.inner.lock() {
        let now = chrono::Utc::now().to_rfc3339();
        runtime.last_frame_at = Some(now.clone());
        runtime.last_pong_at = Some(now);
    }
}

async fn relay_loop(
    app: AppHandle,
    manager: RemoteMcpRelayManager,
    mut wake_rx: mpsc::UnboundedReceiver<()>,
) {
    let mut reconnect_seconds = 1u64;
    loop {
        let credential = match load_relay_credential(app.clone()).await {
            Ok(Some(value)) => value,
            Ok(None) => {
                mark_signed_out(&manager);
                reconnect_seconds = 1;
                wait_or_wake(&mut wake_rx, SIGNED_OUT_RETRY).await;
                continue;
            }
            Err(error) => {
                mark_reconnecting(&manager, "credential_refresh_failed", &error);
                eprintln!(
                    "AtrisBridge remote MCP credential refresh failed: {}",
                    remote_mcp_protocol::bounded_error(&error)
                );
                wait_or_wake(&mut wake_rx, backoff_delay(reconnect_seconds)).await;
                reconnect_seconds = (reconnect_seconds.saturating_mul(2)).min(30);
                continue;
            }
        };

        mark_connecting(&manager);
        match connect_relay(&credential).await {
            Ok(socket) => {
                reconnect_seconds = 1;
                match run_connection(app.clone(), credential, socket, &mut wake_rx, &manager).await
                {
                    Ok(ConnectionExit::CredentialChanged) => continue,
                    Ok(ConnectionExit::RemoteClosed) => {
                        mark_reconnecting(
                            &manager,
                            "remote_closed",
                            "AtrisHub closed the remote MCP relay WebSocket.",
                        );
                    }
                    Err(error) => {
                        let code = if error.contains("watchdog") {
                            "desktop_watchdog_timeout"
                        } else {
                            "relay_disconnected"
                        };
                        mark_reconnecting(&manager, code, &error);
                        eprintln!(
                            "AtrisBridge remote MCP relay disconnected: {}",
                            remote_mcp_protocol::bounded_error(&error)
                        );
                    }
                }
            }
            Err(error) => {
                mark_reconnecting(&manager, "connect_failed", &error);
                eprintln!(
                    "AtrisBridge remote MCP relay connection failed: {}",
                    remote_mcp_protocol::bounded_error(&error)
                );
            }
        }

        wait_or_wake(&mut wake_rx, backoff_delay(reconnect_seconds)).await;
        reconnect_seconds = (reconnect_seconds.saturating_mul(2)).min(30);
    }
}

async fn wait_or_wake(wake_rx: &mut mpsc::UnboundedReceiver<()>, duration: Duration) {
    let timer = sleep(duration).fuse();
    let wake = wake_rx.recv().fuse();
    futures_util::pin_mut!(timer, wake);
    futures_util::select! {
        _ = timer => {},
        _ = wake => {},
    }
}

async fn load_relay_credential(app: AppHandle) -> Result<Option<DesktopSessionCredential>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AtrisHubAuthState>();
        atris_auth::ensure_desktop_session_credential(&app, state.inner())
    })
    .await
    .map_err(|error| format!("AtrisBridge remote MCP auth worker failed: {error}"))?
}

async fn connect_relay(credential: &DesktopSessionCredential) -> Result<RelaySocket, String> {
    let mut request = RELAY_URL
        .into_client_request()
        .map_err(|error| format!("Could not build AtrisHub relay request: {error}"))?;
    let authorization = HeaderValue::from_str(&format!("Bearer {}", credential.access_token))
        .map_err(|_| "AtrisHub desktop access token could not be encoded safely.".to_string())?;
    request.headers_mut().insert(AUTHORIZATION, authorization);
    request
        .headers_mut()
        .insert(RELAY_READY_HEADER, HeaderValue::from_static("1"));

    let connected = timeout(CONNECT_TIMEOUT, connect_async(request))
        .await
        .map_err(|_| "AtrisHub remote MCP relay connection timed out.".to_string())?;
    let (socket, _) = connected
        .map_err(|error| format!("Could not establish secure AtrisHub relay WebSocket: {error}"))?;
    Ok(socket)
}

async fn run_connection(
    app: AppHandle,
    credential: DesktopSessionCredential,
    socket: RelaySocket,
    wake_rx: &mut mpsc::UnboundedReceiver<()>,
    manager: &RemoteMcpRelayManager,
) -> Result<ConnectionExit, String> {
    let (mut writer, mut reader) = socket.split();
    match await_relay_ready(&mut writer, &mut reader, &credential, wake_rx).await? {
        RelayReadyOutcome::Ready => mark_online(manager),
        RelayReadyOutcome::RestartRequested => return Ok(ConnectionExit::CredentialChanged),
    }

    let (outgoing_tx, mut outgoing_rx) = mpsc::channel::<Message>(MAX_LOCAL_INFLIGHT * 2);
    let permits = Arc::new(Semaphore::new(MAX_LOCAL_INFLIGHT));
    let (auth_result_tx, mut auth_result_rx) = mpsc::unbounded_channel::<Result<bool, String>>();
    let mut auth_check_inflight = false;
    let mut auth_tick = interval(AUTH_CHECK_INTERVAL);
    auth_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    auth_tick.tick().await;
    let mut watchdog_tick = interval(WATCHDOG_INTERVAL);
    watchdog_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    watchdog_tick.tick().await;
    let mut awaiting_pong_since: Option<StdInstant> = None;

    loop {
        let outgoing = outgoing_rx.recv().fuse();
        let incoming = reader.next().fuse();
        let auth = auth_tick.tick().fuse();
        let auth_result = auth_result_rx.recv().fuse();
        let watchdog = watchdog_tick.tick().fuse();
        let wake = wake_rx.recv().fuse();
        futures_util::pin_mut!(outgoing, incoming, auth, auth_result, watchdog, wake);

        futures_util::select! {
            outgoing = outgoing => {
                let Some(outgoing) = outgoing else {
                    return Ok(ConnectionExit::RemoteClosed);
                };
                writer.send(outgoing).await
                    .map_err(|error| format!("Could not write AtrisHub relay WebSocket: {error}"))?;
            },
            incoming = incoming => {
                let Some(incoming) = incoming else {
                    return Ok(ConnectionExit::RemoteClosed);
                };
                let incoming = incoming
                    .map_err(|error| format!("Could not read AtrisHub relay WebSocket: {error}"))?;
                awaiting_pong_since = None;
                match incoming {
                    Message::Text(text) => {
                        record_frame(manager);
                        if text.len() > MAX_RELAY_PAYLOAD_BYTES {
                            return Err("AtrisHub relay request exceeded the Desktop safety bound.".into());
                        }
                        let request: RelayRequest = serde_json::from_str(text.as_ref())
                            .map_err(|_| "AtrisHub relay sent invalid request JSON.".to_string())?;
                        let permit = match permits.clone().try_acquire_owned() {
                            Ok(value) => value,
                            Err(_) => {
                                let response = remote_mcp_protocol::relay_error_response(
                                    &request,
                                    "device_busy",
                                    "AtrisBridge Desktop has reached its bounded remote request limit.",
                                );
                                send_json(&outgoing_tx, response).await?;
                                continue;
                            }
                        };
                        let worker_app = app.clone();
                        let worker_credential = credential.clone();
                        let worker_tx = outgoing_tx.clone();
                        let response_binding = ResponseBinding::from_request(&request);
                        tauri::async_runtime::spawn(async move {
                            let failure_binding = response_binding.clone();
                            let response = tauri::async_runtime::spawn_blocking(move || {
                                let _permit = permit;
                                remote_mcp_protocol::process_relay_request(
                                    &worker_app,
                                    &worker_credential,
                                    request,
                                )
                            }).await;
                            let value = match response {
                                Ok(value) => value,
                                Err(error) => remote_mcp_protocol::relay_error_response_parts(
                                    &failure_binding,
                                    "desktop_worker_failed",
                                    &format!("Remote MCP worker failed: {error}"),
                                ),
                            };
                            let _ = send_json(&worker_tx, value).await;
                        });
                    }
                    Message::Ping(payload) => {
                        record_server_ping(manager);
                        writer.send(Message::Pong(payload)).await
                            .map_err(|error| format!("Could not answer AtrisHub relay heartbeat: {error}"))?;
                    }
                    Message::Pong(_) => {
                        record_pong(manager);
                    }
                    Message::Close(frame) => return Err(close_message(frame.as_ref(), "after readiness")),
                    Message::Binary(_) => return Err("AtrisHub relay sent an unexpected binary payload.".into()),
                    Message::Frame(_) => record_frame(manager),
                }
            },
            _ = auth => {
                if !auth_check_inflight {
                    auth_check_inflight = true;
                    let auth_app = app.clone();
                    let expected = credential.clone();
                    let result_tx = auth_result_tx.clone();
                    tauri::async_runtime::spawn(async move {
                        let result = credential_still_current(auth_app, &expected).await;
                        let _ = result_tx.send(result);
                    });
                }
            },
            auth_result = auth_result => {
                auth_check_inflight = false;
                match auth_result {
                    Some(Ok(true)) => {},
                    Some(Ok(false)) => {
                        let _ = writer.send(Message::Close(None)).await;
                        return Ok(ConnectionExit::CredentialChanged);
                    }
                    Some(Err(error)) => {
                        let _ = writer.send(Message::Close(None)).await;
                        return Err(format!("AtrisHub credential watchdog failed: {error}"));
                    }
                    None => return Err("AtrisHub credential watchdog channel closed unexpectedly.".into()),
                }
            },
            _ = watchdog => {
                let now = StdInstant::now();
                if watchdog_expired(awaiting_pong_since, now) {
                    let _ = writer.send(Message::Close(None)).await;
                    return Err("AtrisBridge relay watchdog timed out waiting for AtrisHub transport activity.".into());
                }
                if awaiting_pong_since.is_none() {
                    let payload = Uuid::new_v4().as_bytes().to_vec();
                    writer.send(Message::Ping(payload.into())).await
                        .map_err(|error| format!("Could not send AtrisBridge relay watchdog ping: {error}"))?;
                    awaiting_pong_since = Some(now);
                }
            },
            wake = wake => {
                let _ = writer.send(Message::Close(None)).await;
                if wake.is_none() {
                    return Ok(ConnectionExit::CredentialChanged);
                }
                return Ok(ConnectionExit::CredentialChanged);
            },
        }
    }
}

fn watchdog_expired(waiting_since: Option<StdInstant>, now: StdInstant) -> bool {
    waiting_since.is_some_and(|started| now.saturating_duration_since(started) >= WATCHDOG_RESPONSE_TIMEOUT)
}

async fn await_relay_ready(
    writer: &mut RelayWriter,
    reader: &mut RelayReader,
    credential: &DesktopSessionCredential,
    wake_rx: &mut mpsc::UnboundedReceiver<()>,
) -> Result<RelayReadyOutcome, String> {
    timeout(RELAY_READY_TIMEOUT, async {
        loop {
            let incoming = reader.next().fuse();
            let wake = wake_rx.recv().fuse();
            futures_util::pin_mut!(incoming, wake);
            futures_util::select! {
                incoming = incoming => {
                    let Some(incoming) = incoming else {
                        return Err("AtrisHub closed the remote MCP relay before readiness was confirmed.".to_string());
                    };
                    match incoming.map_err(|error| format!("Could not read AtrisHub relay readiness: {error}"))? {
                        Message::Text(text) => {
                            if text.len() > 16 * 1024 {
                                return Err("AtrisHub relay readiness envelope exceeded the safety bound.".into());
                            }
                            let ready: RelayReady = serde_json::from_str(text.as_ref())
                                .map_err(|_| "AtrisHub relay did not return a valid readiness envelope.".to_string())?;
                            validate_relay_ready(credential, &ready)?;
                            return Ok(RelayReadyOutcome::Ready);
                        }
                        Message::Ping(payload) => {
                            writer.send(Message::Pong(payload)).await
                                .map_err(|error| format!("Could not answer AtrisHub relay heartbeat while connecting: {error}"))?;
                        }
                        Message::Pong(_) => {}
                        Message::Close(frame) => return Err(close_message(frame.as_ref(), "before readiness")),
                        Message::Binary(_) => return Err("AtrisHub relay sent binary data before readiness.".into()),
                        Message::Frame(_) => {}
                    }
                },
                wake = wake => {
                    let _ = writer.send(Message::Close(None)).await;
                    if wake.is_none() {
                        return Ok(RelayReadyOutcome::RestartRequested);
                    }
                    return Ok(RelayReadyOutcome::RestartRequested);
                },
            }
        }
    })
    .await
    .map_err(|_| "AtrisHub relay presence confirmation timed out.".to_string())?
}

fn validate_relay_ready(
    credential: &DesktopSessionCredential,
    ready: &RelayReady,
) -> Result<(), String> {
    if ready.kind != "relay_ready" || ready.version != 1 {
        return Err("AtrisHub relay readiness protocol is not supported.".into());
    }
    if ready.device_id != credential.device_id
        || ready.desktop_session_id != credential.desktop_session_id
        || ready.session_generation != credential.session_generation
    {
        return Err("AtrisHub relay readiness is bound to another Desktop session.".into());
    }
    if !canonical_uuid_v4(&ready.connection_id) {
        return Err("AtrisHub relay readiness connection identifier is invalid.".into());
    }
    Ok(())
}

fn canonical_uuid_v4(value: &str) -> bool {
    Uuid::parse_str(value).is_ok_and(|parsed| {
        parsed.get_version_num() == 4 && parsed.hyphenated().to_string().eq_ignore_ascii_case(value)
    })
}

fn close_message(
    frame: Option<&tokio_tungstenite::tungstenite::protocol::CloseFrame>,
    phase: &str,
) -> String {
    let reason = frame
        .map(|value| value.reason.trim().to_string())
        .filter(|value| !value.is_empty());
    match reason {
        Some(reason) => format!("AtrisHub closed the remote MCP relay {phase}: {reason}"),
        None => format!("AtrisHub closed the remote MCP relay {phase}."),
    }
}

async fn credential_still_current(
    app: AppHandle,
    expected: &DesktopSessionCredential,
) -> Result<bool, String> {
    let current = load_relay_credential(app).await?;
    Ok(current
        .as_ref()
        .is_some_and(|current| same_credential(current, expected)))
}

fn same_credential(left: &DesktopSessionCredential, right: &DesktopSessionCredential) -> bool {
    left.user_id == right.user_id
        && left.device_id == right.device_id
        && left.desktop_session_id == right.desktop_session_id
        && left.session_generation == right.session_generation
        && left.access_token == right.access_token
}

async fn send_json(sender: &mpsc::Sender<Message>, value: serde_json::Value) -> Result<(), String> {
    let encoded = serde_json::to_string(&value)
        .map_err(|error| format!("Could not encode AtrisHub relay response: {error}"))?;
    if encoded.len() > MAX_RELAY_PAYLOAD_BYTES {
        return Err("AtrisBridge remote MCP response exceeded the relay safety bound.".into());
    }
    sender
        .send(Message::Text(encoded.into()))
        .await
        .map_err(|_| "AtrisHub relay writer is no longer available.".to_string())
}

fn backoff_delay(seconds: u64) -> Duration {
    let bounded = seconds.min(MAX_RECONNECT_DELAY.as_secs());
    let jitter_ms = (chrono::Utc::now().timestamp_subsec_millis() as u64) % 500;
    Duration::from_millis(bounded * 1_000 + jitter_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration as ChronoDuration, Utc};

    fn credential() -> DesktopSessionCredential {
        DesktopSessionCredential {
            user_id: "user-1".into(),
            device_id: "device-1".into(),
            desktop_session_id: "11111111-1111-4111-8111-111111111111".into(),
            session_generation: 3,
            access_token: "token".into(),
            access_token_expires_at: Utc::now() + ChronoDuration::minutes(10),
            session_expires_at: Utc::now() + ChronoDuration::days(1),
        }
    }

    #[test]
    fn relay_ready_requires_the_current_desktop_binding() {
        let current = credential();
        let ready = RelayReady {
            kind: "relay_ready".into(),
            version: 1,
            device_id: current.device_id.clone(),
            desktop_session_id: current.desktop_session_id.clone(),
            session_generation: current.session_generation,
            connection_id: "22222222-2222-4222-8222-222222222222".into(),
        };
        assert!(validate_relay_ready(&current, &ready).is_ok());
        let stale = RelayReady {
            session_generation: current.session_generation + 1,
            ..ready
        };
        assert!(validate_relay_ready(&current, &stale).is_err());
    }

    #[test]
    fn watchdog_expires_only_after_the_response_deadline() {
        let started = StdInstant::now();
        assert!(!watchdog_expired(
            Some(started),
            started + WATCHDOG_RESPONSE_TIMEOUT - Duration::from_millis(1)
        ));
        assert!(watchdog_expired(
            Some(started),
            started + WATCHDOG_RESPONSE_TIMEOUT
        ));
        assert!(!watchdog_expired(None, started + WATCHDOG_RESPONSE_TIMEOUT));
    }
}
