use std::path::PathBuf;

use crate::auth::AuthProvider;
use crate::error::Result;

/// Agent configuration assembled from CLI args and environment.
#[allow(dead_code)]
pub struct AgentConfig {
    pub auth: AuthProvider,
    pub model: String,
    pub max_tokens: u32,
    pub db_path: PathBuf,
    pub system_prompt: Option<String>,
}

impl AgentConfig {
    /// Build config from CLI arguments.
    pub fn from_args(
        db_path: PathBuf,
        model: String,
        max_tokens: u32,
        system_prompt: Option<String>,
    ) -> Result<Self> {
        let auth = AuthProvider::load()?;
        Ok(Self {
            auth,
            model,
            max_tokens,
            db_path,
            system_prompt,
        })
    }
}
