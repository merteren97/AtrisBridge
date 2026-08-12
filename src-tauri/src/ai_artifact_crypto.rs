use std::{
    fs,
    fs::File,
    io::Write,
    path::Path,
    sync::{Mutex, OnceLock},
};

use chacha20poly1305::{
    aead::{Aead, Generate, Key, KeyInit, Payload},
    XChaCha20Poly1305, XNonce,
};

use crate::secure_store;

const MAGIC: &[u8; 8] = b"ABAIENC1";
const KEY_BYTES: usize = 32;
const NONCE_BYTES: usize = 24;
pub const MAX_SENSITIVE_ARTIFACT_BYTES: u64 = 2 * 1024 * 1024;

static KEY_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

pub fn write_encrypted_artifact(
    path: &Path,
    plaintext: &[u8],
    associated_data: &[u8],
) -> Result<(), String> {
    if u64::try_from(plaintext.len()).unwrap_or(u64::MAX) > MAX_SENSITIVE_ARTIFACT_BYTES {
        return Err(format!(
            "Sensitive AI artifacts are limited to {MAX_SENSITIVE_ARTIFACT_BYTES} bytes."
        ));
    }
    let key = load_or_create_key()?;
    let nonce = XNonce::generate();
    let cipher = XChaCha20Poly1305::new(&key);
    let ciphertext = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext,
                aad: associated_data,
            },
        )
        .map_err(|_| "Could not encrypt sensitive AI artifact.".to_string())?;

    let mut file = File::create(path)
        .map_err(|error| format!("Could not create encrypted AI artifact: {error}"))?;
    file.write_all(MAGIC)
        .and_then(|_| file.write_all(nonce.as_ref()))
        .and_then(|_| file.write_all(&ciphertext))
        .map_err(|error| format!("Could not write encrypted AI artifact: {error}"))?;
    file.sync_all()
        .map_err(|error| format!("Could not flush encrypted AI artifact: {error}"))
}

pub fn read_encrypted_artifact(path: &Path, associated_data: &[u8]) -> Result<Vec<u8>, String> {
    let encoded =
        fs::read(path).map_err(|error| format!("Could not read encrypted AI artifact: {error}"))?;
    if encoded.len() < MAGIC.len() + NONCE_BYTES + 16 || &encoded[..MAGIC.len()] != MAGIC {
        return Err("Sensitive AI artifact header is invalid.".into());
    }
    let nonce_start = MAGIC.len();
    let ciphertext_start = nonce_start + NONCE_BYTES;
    let nonce = XNonce::try_from(&encoded[nonce_start..ciphertext_start])
        .map_err(|_| "Sensitive AI artifact nonce is invalid.".to_string())?;
    let key = load_existing_key()?;
    let cipher = XChaCha20Poly1305::new(&key);
    let plaintext = cipher
        .decrypt(
            &nonce,
            Payload {
                msg: &encoded[ciphertext_start..],
                aad: associated_data,
            },
        )
        .map_err(|_| "Sensitive AI artifact authentication failed.".to_string())?;
    if u64::try_from(plaintext.len()).unwrap_or(u64::MAX) > MAX_SENSITIVE_ARTIFACT_BYTES {
        return Err("Decrypted sensitive AI artifact exceeds the configured safety bound.".into());
    }
    Ok(plaintext)
}

pub fn is_encrypted_artifact(path: &Path) -> Result<bool, String> {
    let encoded = fs::read(path)
        .map_err(|error| format!("Could not inspect encrypted AI artifact: {error}"))?;
    Ok(encoded.len() >= MAGIC.len() && &encoded[..MAGIC.len()] == MAGIC)
}

fn load_or_create_key() -> Result<Key<XChaCha20Poly1305>, String> {
    let lock = KEY_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = lock
        .lock()
        .map_err(|_| "AI artifact encryption key lock is unavailable.".to_string())?;
    if let Some(encoded) = secure_store::load_ai_artifact_key()? {
        return decode_key(&encoded);
    }
    let key = Key::<XChaCha20Poly1305>::generate();
    secure_store::store_ai_artifact_key(&hex_encode(key.as_ref()))?;
    Ok(key)
}

fn load_existing_key() -> Result<Key<XChaCha20Poly1305>, String> {
    let encoded = secure_store::load_ai_artifact_key()?.ok_or_else(|| {
        "Sensitive AI artifact encryption key is missing from the OS credential vault.".to_string()
    })?;
    decode_key(&encoded)
}

fn decode_key(encoded: &str) -> Result<Key<XChaCha20Poly1305>, String> {
    let bytes = hex_decode(encoded)?;
    let bytes: [u8; KEY_BYTES] = bytes
        .try_into()
        .map_err(|_| "Stored AI artifact encryption key has an invalid length.".to_string())?;
    Ok(Key::<XChaCha20Poly1305>::from(bytes))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn hex_decode(value: &str) -> Result<Vec<u8>, String> {
    if value.len() % 2 != 0 {
        return Err("Stored AI artifact encryption key is invalid.".into());
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = hex_nibble(pair[0])?;
            let low = hex_nibble(pair[1])?;
            Ok((high << 4) | low)
        })
        .collect()
}

fn hex_nibble(value: u8) -> Result<u8, String> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err("Stored AI artifact encryption key is invalid.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_key_round_trip_is_stable() {
        let input = [0x00, 0x01, 0x7f, 0x80, 0xfe, 0xff];
        assert_eq!(hex_decode(&hex_encode(&input)).expect("decode"), input);
    }

    #[test]
    fn malformed_hex_is_rejected() {
        assert!(hex_decode("abc").is_err());
        assert!(hex_decode("zz").is_err());
    }
}
