use std::{collections::HashMap, sync::Mutex};

#[derive(Default)]
pub struct ProviderSessionStore {
    google_drive_tokens: Mutex<HashMap<String, String>>,
}

impl ProviderSessionStore {
    pub fn set_google_drive_token(&self, provider_id: &str, token: String) -> Result<(), String> {
        self.google_drive_tokens
            .lock()
            .map_err(|_| "Cloud session store is unavailable.".to_string())?
            .insert(provider_id.to_string(), token);
        Ok(())
    }

    pub fn google_drive_token(&self, provider_id: &str) -> Result<Option<String>, String> {
        Ok(self
            .google_drive_tokens
            .lock()
            .map_err(|_| "Cloud session store is unavailable.".to_string())?
            .get(provider_id)
            .cloned())
    }

    pub fn is_active(&self, provider_id: &str) -> Result<bool, String> {
        Ok(self
            .google_drive_tokens
            .lock()
            .map_err(|_| "Cloud session store is unavailable.".to_string())?
            .contains_key(provider_id))
    }

    pub fn remove(&self, provider_id: &str) -> Result<(), String> {
        self.google_drive_tokens
            .lock()
            .map_err(|_| "Cloud session store is unavailable.".to_string())?
            .remove(provider_id);
        Ok(())
    }
}
