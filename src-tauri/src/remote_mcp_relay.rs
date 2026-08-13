use std::sync::{Arc, Mutex};

use futures_util::{FutureExt, SinkExt, StreamExt};
use tauri::{AppHandle, Manager};
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
};

use crate::{
    atris_auth::{self, AtrisHubAuthState, DesktopSessionCredential},
    remote_mcp_protocol::{self, RelayRequest, ResponseBinding},
};

const RELAY_URL: &str = "wss://atrishub.com/api/mcp/relay/v1/connect";
const MAX_RELAY_PAYLOAD_BYTES: usize = 6 * 1024 * 1024;
const MAX_LOCAL_INFLIGHT: usize = 32;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const AUTH_CHECK_INTERVAL: Duration = Duration::from_secs(15);
const SIGNED_OUT_RETRY: Duration = Duration::from_secs(5);
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(30);

#[derive(Clone, Default)]
pub struct RemoteMcpRelayManager {
    inner: Arc<Mutex<RelayManagerRuntime>>,
}

#[derive(Default)]
struct RelayManagerRuntime {
    started: bool,
    state: RelayLifecycleState,
    wake_tx: Option<mpsc::UnboundedSender<()>>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum RelayLifecycleState {
    #[default]
    SignedOut,
    Connecting,
    Online,
    Reconnecting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionExit {
    CredentialChanged,
    RemoteClosed,
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

fn set_lifecycle(manager: &RemoteMcpRelayManager, state: RelayLifecycleState) {
    if let Ok(mut runtime) = manager.inner.lock() {
        runtime.state = state;
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
                set_lifecycle(&manager, RelayLifecycleState::SignedOut);
                reconnect_seconds = 1;
                wait_or_wake(&mut wake_rx, SIGNED_OUT_RETRY).await;
                continue;
            }
            Err(error) => {
                set_lifecycle(&manager, RelayLifecycleState::Reconnecting);
                eprintln!(
                    "AtrisBridge remote MCP credential refresh failed: {}",
                    remote_mcp_protocol::bounded_error(&error)
                );
                wait_or_wake(&mut wake_rx, backoff_delay(reconnect_seconds)).await;
                reconnect_seconds = (reconnect_seconds.saturating_mul(2)).min(30);
                continue;
            }
        };

        set_lifecycle(&manager, RelayLifecycleState::Connecting);
        match connect_relay(&credential).await {
            Ok(socket) => {
                reconnect_seconds = 1;
                set_lifecycle(&manager, RelayLifecycleState::Online);
                match run_connection(app.clone(), credential, socket, &mut wake_rx).await {
                    Ok(ConnectionExit::CredentialChanged) => continue,
                    Ok(ConnectionExit::RemoteClosed) => {}
                    Err(error) => {
                        eprintln!(
                            "AtrisBridge remote MCP relay disconnected: {}",
                            remote_mcp_protocol::bounded_error(&error)
                        );
                    }
                }
            }
            Err(error) => {
                eprintln!(
                    "AtrisBridge remote MCP relay connection failed: {}",
                    remote_mcp_protocol::bounded_error(&error)
                );
            }
        }

        set_lifecycle(&manager, RelayLifecycleState::Reconnecting);
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

async fn connect_relay(
    credential: &DesktopSessionCredential,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    String,
> {
    let mut request = RELAY_URL
        .into_client_request()
        .map_err(|error| format!("Could not build AtrisHub relay request: {error}"))?;
    let authorization = HeaderValue::from_str(&format!("Bearer {}", credential.access_token))
        .map_err(|_| "AtrisHub desktop access token could not be encoded safely.".to_string())?;
    request.headers_mut().insert(AUTHORIZATION, authorization);

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
    socket: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    wake_rx: &mut mpsc::UnboundedReceiver<()>,
) -> Result<ConnectionExit, String> {
    let (mut writer, mut reader) = socket.split();
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel::<Message>(MAX_LOCAL_INFLIGHT * 2);
    let permits = Arc::new(Semaphore::new(MAX_LOCAL_INFLIGHT));
    let mut auth_tick = interval(AUTH_CHECK_INTERVAL);
    auth_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    auth_tick.tick().await;

    loop {
        let outgoing = outgoing_rx.recv().fuse();
        let incoming = reader.next().fuse();
        let auth = auth_tick.tick().fuse();
        let wake = wake_rx.recv().fuse();
        futures_util::pin_mut!(outgoing, incoming, auth, wake);

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
                match incoming.map_err(|error| format!("Could not read AtrisHub relay WebSocket: {error}"))? {
                    Message::Text(text) => {
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
                        writer.send(Message::Pong(payload)).await
                            .map_err(|error| format!("Could not answer AtrisHub relay heartbeat: {error}"))?;
                    }
                    Message::Pong(_) => {}
                    Message::Close(_) => return Ok(ConnectionExit::RemoteClosed),
                    Message::Binary(_) => return Err("AtrisHub relay sent an unexpected binary payload.".into()),
                    Message::Frame(_) => {}
                }
            },
            _ = auth => {
                if !credential_still_current(app.clone(), &credential).await? {
                    let _ = writer.send(Message::Close(None)).await;
                    return Ok(ConnectionExit::CredentialChanged);
                }
            },
            wake = wake => {
                if wake.is_none() || !credential_still_current(app.clone(), &credential).await? {
                    let _ = writer.send(Message::Close(None)).await;
                    return Ok(ConnectionExit::CredentialChanged);
                }
            },
        }
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
