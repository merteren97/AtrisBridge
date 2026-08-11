use std::{collections::HashMap, sync::Mutex};

use crate::secure_store;

#[derive(Default)]
pub struct ProviderSessionStore {
    google_drive_tokens: Mutex<HashMap<String, String>>,
}

impl ProviderSessionStore {
    pub fn set_google_drive_token(&self, provider_id: &str, token: String) -> Result<(), String> {
        secure_store::store_google_drive_token(provider_id, &token)?;
        self.google_drive_tokens
            .lock()
            .map_err(|_| "Cloud session store is unavailable.".to_string())?
            .insert(provider_id.to_string(), token);
        Ok(())
    }

    pub fn google_drive_token(&self, provider_id: &str) -> Result<Option<String>, String> {
        if let Some(token) = self
            .google_drive_tokens
            .lock()
            .map_err(|_| "Cloud session store is unavailable.".to_string())?
            .get(provider_id)
            .cloned()
        {
            return Ok(Some(token));
        }

        let Some(token) = secure_store::load_google_drive_token(provider_id)? else {
            return Ok(None);
        };
        self.google_drive_tokens
            .lock()
            .map_err(|_| "Cloud session store is unavailable.".to_string())?
            .insert(provider_id.to_string(), token.clone());
        Ok(Some(token))
    }

    pub fn is_active(&self, provider_id: &str) -> Result<bool, String> {
        self.google_drive_token(provider_id)
            .map(|token| token.is_some())
    }

    pub fn is_persisted(&self, provider_id: &str) -> Result<bool, String> {
        secure_store::load_google_drive_token(provider_id).map(|token| token.is_some())
    }

    pub fn remove(&self, provider_id: &str) -> Result<(), String> {
        secure_store::delete_google_drive_token(provider_id)?;
        self.google_drive_tokens
            .lock()
            .map_err(|_| "Cloud session store is unavailable.".to_string())?
            .remove(provider_id);
        Ok(())
    }
}
