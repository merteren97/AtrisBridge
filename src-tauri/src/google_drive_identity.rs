use reqwest::blocking::Client;
use serde::Deserialize;

const DRIVE_ABOUT_URL: &str = "https://www.googleapis.com/drive/v3/about";

#[derive(Debug, Deserialize)]
struct OAuthToken {
    access_token: String,
}

#[derive(Debug, Deserialize)]
struct DriveAbout {
    user: Option<DriveUser>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DriveUser {
    display_name: Option<String>,
    email_address: Option<String>,
    permission_id: Option<String>,
}

pub fn account_label_from_token(token_json: &str) -> Result<String, String> {
    let token: OAuthToken = serde_json::from_str(token_json)
        .map_err(|error| format!("Google authorization token could not be read: {error}"))?;
    let access_token = token.access_token.trim();
    if access_token.is_empty() {
        return Err("Google authorization completed without an access token.".into());
    }

    let response = Client::new()
        .get(DRIVE_ABOUT_URL)
        .query(&[("fields", "user(displayName,emailAddress,permissionId)")])
        .bearer_auth(access_token)
        .send()
        .map_err(|error| format!("Could not query the Google Drive account: {error}"))?;

    if !response.status().is_success() {
        return Err(format!(
            "Google Drive account verification returned HTTP {}.",
            response.status()
        ));
    }

    let about: DriveAbout = response
        .json()
        .map_err(|error| format!("Google Drive account response was invalid: {error}"))?;
    account_label_from_user(about.user.as_ref())
        .ok_or_else(|| "Google Drive did not return a usable account identity.".to_string())
}

fn account_label_from_user(user: Option<&DriveUser>) -> Option<String> {
    let user = user?;
    normalized(&user.email_address)
        .or_else(|| normalized(&user.permission_id).map(|id| format!("drive:{id}")))
        .or_else(|| normalized(&user.display_name))
}

fn normalized(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_is_preferred_for_account_identity() {
        let user = DriveUser {
            display_name: Some("Mert".into()),
            email_address: Some("mert@example.com".into()),
            permission_id: Some("permission-1".into()),
        };
        assert_eq!(account_label_from_user(Some(&user)).as_deref(), Some("mert@example.com"));
    }

    #[test]
    fn permission_id_is_a_stable_fallback() {
        let user = DriveUser {
            display_name: Some("Mert".into()),
            email_address: None,
            permission_id: Some("permission-1".into()),
        };
        assert_eq!(account_label_from_user(Some(&user)).as_deref(), Some("drive:permission-1"));
    }

    #[test]
    fn display_name_is_only_the_last_fallback() {
        let user = DriveUser {
            display_name: Some("Mert".into()),
            email_address: None,
            permission_id: None,
        };
        assert_eq!(account_label_from_user(Some(&user)).as_deref(), Some("Mert"));
    }
}
