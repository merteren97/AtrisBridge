use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::Duration,
};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use uuid::Uuid;

const LOCK_VERSION: u32 = 1;
const LOCK_FILE_NAME: &str = "atrishub-atrisbridge.instance";
const FOCUS_MAGIC: &str = "atrisbridge-focus-v1";
const MAX_LOCK_BYTES: u64 = 512;
const MAX_FOCUS_LINE_BYTES: u64 = 256;
const CONNECT_TIMEOUT: Duration = Duration::from_millis(250);
const IO_TIMEOUT: Duration = Duration::from_millis(500);
const INITIALIZATION_RETRIES: usize = 8;
const INITIALIZATION_RETRY_DELAY: Duration = Duration::from_millis(25);
const STALE_CONNECT_RETRIES: usize = 3;
const STALE_CONNECT_RETRY_DELAY: Duration = Duration::from_millis(40);
const ACQUIRE_RETRIES: usize = 4;

#[derive(Debug)]
struct LockRecord {
    port: u16,
    nonce: String,
}

pub struct SingleInstanceGuard {
    lock_path: PathBuf,
    nonce: String,
}

pub struct PrimaryInstance {
    pub guard: SingleInstanceGuard,
    pub focus_requests: Receiver<()>,
}

pub enum InstanceRole {
    Primary(PrimaryInstance),
    Secondary,
}

pub fn acquire_or_notify() -> Result<InstanceRole, String> {
    let lock_path = lock_path();

    for _ in 0..ACQUIRE_RETRIES {
        let listener =
            TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).map_err(|error| {
                format!("Could not reserve the AtrisBridge instance channel: {error}")
            })?;
        let port = listener
            .local_addr()
            .map_err(|error| {
                format!("Could not inspect the AtrisBridge instance channel: {error}")
            })?
            .port();
        let nonce = Uuid::new_v4().simple().to_string();

        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);

        match options.open(&lock_path) {
            Ok(mut file) => {
                let encoded = encode_lock_record(port, &nonce);
                if let Err(error) = file
                    .write_all(encoded.as_bytes())
                    .and_then(|_| file.flush())
                    .and_then(|_| file.sync_all())
                {
                    let _ = fs::remove_file(&lock_path);
                    return Err(format!(
                        "Could not publish the AtrisBridge instance authority: {error}"
                    ));
                }

                let (focus_tx, focus_rx) = mpsc::channel();
                start_listener(listener, nonce.clone(), focus_tx)?;
                return Ok(InstanceRole::Primary(PrimaryInstance {
                    guard: SingleInstanceGuard { lock_path, nonce },
                    focus_requests: focus_rx,
                }));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                drop(listener);
                match notify_existing_instance(&lock_path)? {
                    NotifyOutcome::Notified => return Ok(InstanceRole::Secondary),
                    NotifyOutcome::Stale => {
                        remove_stale_lock(&lock_path)?;
                        thread::sleep(INITIALIZATION_RETRY_DELAY);
                    }
                }
            }
            Err(error) => {
                return Err(format!(
                    "Could not claim the AtrisBridge instance authority: {error}"
                ));
            }
        }
    }

    Err(
        "AtrisBridge could not establish an exclusive process authority after bounded retries."
            .into(),
    )
}

impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        let Ok(record) = read_lock_record_once(&self.lock_path) else {
            return;
        };
        if record.nonce == self.nonce {
            let _ = fs::remove_file(&self.lock_path);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NotifyOutcome {
    Notified,
    Stale,
}

fn notify_existing_instance(lock_path: &Path) -> Result<NotifyOutcome, String> {
    let record = match read_lock_record_with_retry(lock_path)? {
        Some(record) => record,
        None => return Ok(NotifyOutcome::Stale),
    };
    let address = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, record.port));

    for attempt in 0..STALE_CONNECT_RETRIES {
        match TcpStream::connect_timeout(&address, CONNECT_TIMEOUT) {
            Ok(mut stream) => {
                stream.set_read_timeout(Some(IO_TIMEOUT)).map_err(|error| {
                    format!("Could not configure the AtrisBridge instance channel: {error}")
                })?;
                stream
                    .set_write_timeout(Some(IO_TIMEOUT))
                    .map_err(|error| {
                        format!("Could not configure the AtrisBridge instance channel: {error}")
                    })?;
                let request = focus_request(&record.nonce);
                stream
                    .write_all(request.as_bytes())
                    .and_then(|_| stream.flush())
                    .map_err(|error| {
                        format!("Could not notify the running AtrisBridge instance: {error}")
                    })?;

                let mut response = String::new();
                let mut reader = BufReader::new(stream);
                reader
                    .by_ref()
                    .take(MAX_FOCUS_LINE_BYTES)
                    .read_line(&mut response)
                    .map_err(|error| {
                        format!("Could not verify the running AtrisBridge instance: {error}")
                    })?;
                return Ok(if response.trim_end() == "OK" {
                    NotifyOutcome::Notified
                } else {
                    NotifyOutcome::Stale
                });
            }
            Err(_) if attempt + 1 < STALE_CONNECT_RETRIES => {
                thread::sleep(STALE_CONNECT_RETRY_DELAY);
            }
            Err(_) => return Ok(NotifyOutcome::Stale),
        }
    }

    Ok(NotifyOutcome::Stale)
}

fn read_lock_record_with_retry(lock_path: &Path) -> Result<Option<LockRecord>, String> {
    for attempt in 0..INITIALIZATION_RETRIES {
        match read_lock_record_once(lock_path) {
            Ok(record) => return Ok(Some(record)),
            Err(_) if attempt + 1 < INITIALIZATION_RETRIES => {
                thread::sleep(INITIALIZATION_RETRY_DELAY);
            }
            Err(_) => return Ok(None),
        }
    }
    Ok(None)
}

fn read_lock_record_once(lock_path: &Path) -> Result<LockRecord, String> {
    let metadata = fs::metadata(lock_path)
        .map_err(|error| format!("Could not inspect the AtrisBridge instance record: {error}"))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_LOCK_BYTES {
        return Err("AtrisBridge instance record has an invalid size.".into());
    }
    let content = fs::read_to_string(lock_path)
        .map_err(|error| format!("Could not read the AtrisBridge instance record: {error}"))?;
    parse_lock_record(&content)
}

fn parse_lock_record(content: &str) -> Result<LockRecord, String> {
    let mut lines = content.lines();
    let version = lines
        .next()
        .ok_or_else(|| "AtrisBridge instance record is incomplete.".to_string())?
        .parse::<u32>()
        .map_err(|_| "AtrisBridge instance record version is invalid.".to_string())?;
    let port = lines
        .next()
        .ok_or_else(|| "AtrisBridge instance record is incomplete.".to_string())?
        .parse::<u16>()
        .map_err(|_| "AtrisBridge instance record port is invalid.".to_string())?;
    let nonce = lines
        .next()
        .ok_or_else(|| "AtrisBridge instance record is incomplete.".to_string())?;
    if version != LOCK_VERSION || port == 0 || lines.next().is_some() {
        return Err("AtrisBridge instance record is not supported.".into());
    }
    if nonce.len() != 32 || !nonce.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("AtrisBridge instance record nonce is invalid.".into());
    }
    Ok(LockRecord {
        port,
        nonce: nonce.to_ascii_lowercase(),
    })
}

fn encode_lock_record(port: u16, nonce: &str) -> String {
    format!("{LOCK_VERSION}\n{port}\n{nonce}\n")
}

fn focus_request(nonce: &str) -> String {
    format!("{FOCUS_MAGIC} {nonce}\n")
}

fn valid_focus_request(line: &str, nonce: &str) -> bool {
    line.trim_end() == format!("{FOCUS_MAGIC} {nonce}")
}

fn start_listener(
    listener: TcpListener,
    nonce: String,
    focus_tx: Sender<()>,
) -> Result<(), String> {
    thread::Builder::new()
        .name("atrisbridge-single-instance".into())
        .spawn(move || {
            for incoming in listener.incoming() {
                let Ok(mut stream) = incoming else {
                    continue;
                };
                let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
                let _ = stream.set_write_timeout(Some(IO_TIMEOUT));

                let cloned = match stream.try_clone() {
                    Ok(value) => value,
                    Err(_) => continue,
                };
                let mut line = String::new();
                let mut reader = BufReader::new(cloned);
                let read_ok = reader
                    .by_ref()
                    .take(MAX_FOCUS_LINE_BYTES)
                    .read_line(&mut line)
                    .is_ok();
                if !read_ok || !valid_focus_request(&line, &nonce) {
                    let _ = stream.write_all(b"DENY\n");
                    continue;
                }

                if focus_tx.send(()).is_err() {
                    break;
                }
                let _ = stream.write_all(b"OK\n");
                let _ = stream.flush();
            }
        })
        .map(|_| ())
        .map_err(|error| format!("Could not start the AtrisBridge instance listener: {error}"))
}

fn remove_stale_lock(lock_path: &Path) -> Result<(), String> {
    match fs::remove_file(lock_path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "Could not retire a stale AtrisBridge instance record: {error}"
        )),
    }
}

fn lock_path() -> PathBuf {
    let mut path = std::env::temp_dir();
    #[cfg(unix)]
    let scoped_name = format!("{LOCK_FILE_NAME}-{}", effective_user_id());
    #[cfg(not(unix))]
    let scoped_name = LOCK_FILE_NAME.to_string();
    path.push(scoped_name);
    path
}

#[cfg(unix)]
fn effective_user_id() -> u32 {
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    unsafe { geteuid() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_record_round_trips_and_rejects_unsafe_shapes() {
        let nonce = "0123456789abcdef0123456789abcdef";
        let encoded = encode_lock_record(43127, nonce);
        let record = parse_lock_record(&encoded).expect("record");
        assert_eq!(record.port, 43127);
        assert_eq!(record.nonce, nonce);

        assert!(parse_lock_record("2\n43127\n0123456789abcdef0123456789abcdef\n").is_err());
        assert!(parse_lock_record("1\n0\n0123456789abcdef0123456789abcdef\n").is_err());
        assert!(parse_lock_record("1\n43127\nnot-a-valid-nonce\n").is_err());
        assert!(parse_lock_record("1\n43127\n0123456789abcdef0123456789abcdef\nextra\n").is_err());
    }

    #[test]
    fn focus_request_is_nonce_bound() {
        let nonce = "0123456789abcdef0123456789abcdef";
        assert!(valid_focus_request(&focus_request(nonce), nonce));
        assert!(!valid_focus_request(
            &focus_request("fedcba9876543210fedcba9876543210"),
            nonce
        ));
    }
}
