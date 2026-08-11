use keyring::{Entry, Error as KeyringError};

const SERVICE_NAME: &str = "com.atrishub.atrisbridge";
const OAUTH_PREFIX: &str = "oauth.google_drive";
const CRYPT_PREFIX: &str = "crypt.workspace";
const ATRIS_REFRESH_ACCOUNT: &str = "auth.atrishub.refresh";

fn entry(account: &str) -> Result<Entry, String> {
    Entry::new(SERVICE_NAME, account)
        .map_err(|error| format!("OS secure credential store is unavailable: {error}"))
}

fn oauth_account(provider_id: &str) -> String {
    format!("{OAUTH_PREFIX}.{provider_id}")
}

pub fn store_google_drive_token(provider_id: &str, token: &str) -> Result<(), String> {
    if provider_id.trim().is_empty() || token.trim().is_empty() {
        return Err("Refusing to persist an empty provider credential.".into());
    }
    entry(&oauth_account(provider_id))?
        .set_password(token)
        .map_err(|error| format!("Could not save Google Drive credential in the OS vault: {error}"))
}

pub fn load_google_drive_token(provider_id: &str) -> Result<Option<String>, String> {
    load_password(&oauth_account(provider_id), "Google Drive credential")
}

pub fn delete_google_drive_token(provider_id: &str) -> Result<(), String> {
    delete_password(&oauth_account(provider_id), "Google Drive credential")
}

pub fn store_atrishub_refresh_token(token: &str) -> Result<(), String> {
    if token.trim().is_empty() {
        return Err("Refusing to persist an empty AtrisHub refresh credential.".into());
    }
    entry(ATRIS_REFRESH_ACCOUNT)?
        .set_password(token)
        .map_err(|error| format!("Could not save AtrisHub session in the OS vault: {error}"))
}

pub fn load_atrishub_refresh_token() -> Result<Option<String>, String> {
    load_password(ATRIS_REFRESH_ACCOUNT, "AtrisHub session")
}

pub fn delete_atrishub_refresh_token() -> Result<(), String> {
    delete_password(ATRIS_REFRESH_ACCOUNT, "AtrisHub session")
}

pub fn workspace_key_reference(
    account_identity: &str,
    remote_path: &str,
) -> Result<String, String> {
    let account = account_identity.trim().to_ascii_lowercase();
    let remote = remote_path.trim();
    if account.is_empty() || remote.is_empty() {
        return Err(
            "Encrypted workspace keys require a verified provider account and remote root.".into(),
        );
    }
    let scope = format!("google_drive\n{account}\n{remote}");
    let digest = blake3::hash(scope.as_bytes());
    Ok(format!("{CRYPT_PREFIX}.{}", digest.to_hex()))
}

pub fn store_workspace_master_key(key_ref: &str, recovery_key: &str) -> Result<(), String> {
    if !key_ref.starts_with(CRYPT_PREFIX) || recovery_key.trim().is_empty() {
        return Err("Invalid encrypted-workspace credential reference.".into());
    }
    entry(key_ref)?.set_password(recovery_key).map_err(|error| {
        format!("Could not save workspace encryption key in the OS vault: {error}")
    })
}

pub fn load_workspace_master_key(key_ref: &str) -> Result<Option<String>, String> {
    if !key_ref.starts_with(CRYPT_PREFIX) {
        return Err("Invalid encrypted-workspace credential reference.".into());
    }
    load_password(key_ref, "workspace encryption key")
}

fn load_password(account: &str, label: &str) -> Result<Option<String>, String> {
    match entry(account)?.get_password() {
        Ok(value) => Ok(Some(value)),
        Err(KeyringError::NoEntry) => Ok(None),
        Err(error) => Err(format!("Could not read {label} from the OS vault: {error}")),
    }
}

fn delete_password(account: &str, label: &str) -> Result<(), String> {
    match entry(account)?.delete_credential() {
        Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
        Err(error) => Err(format!(
            "Could not remove {label} from the OS vault: {error}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_key_reference_is_stable_and_does_not_expose_scope() {
        let first = workspace_key_reference("dev@example.com", "AtrisBridge/Project").expect("ref");
        let second =
            workspace_key_reference("DEV@example.com", "AtrisBridge/Project").expect("ref");
        assert_eq!(first, second);
        assert!(first.starts_with(CRYPT_PREFIX));
        assert!(!first.contains("AtrisBridge/Project"));
        assert!(!first.contains("dev@example.com"));
    }

    #[test]
    fn different_accounts_or_roots_get_different_key_references() {
        assert_ne!(
            workspace_key_reference("a@example.com", "AtrisBridge/A").expect("a"),
            workspace_key_reference("a@example.com", "AtrisBridge/B").expect("b")
        );
        assert_ne!(
            workspace_key_reference("a@example.com", "AtrisBridge/A").expect("a"),
            workspace_key_reference("b@example.com", "AtrisBridge/A").expect("b")
        );
    }
}
