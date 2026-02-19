use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::Rng;
use sha2::{Digest, Sha256};

use crate::error::{AgentError, Result};

const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const AUTH_URL: &str = "https://claude.ai/oauth/authorize";
const TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
const SCOPES: &str = "org:create_api_key user:profile user:inference";

/// Stored OAuth tokens.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OAuthTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64, // Unix timestamp in seconds
    pub scopes: Vec<String>,
}

/// Authentication provider — supports OAuth tokens or API key.
pub struct AuthProvider {
    tokens: Option<OAuthTokens>,
    api_key: Option<String>,
    credentials_path: PathBuf,
}

/// What kind of auth is active.
pub enum AuthMode {
    OAuth,
    ApiKey,
    None,
}

impl AuthProvider {
    /// Load credentials: try stored tokens first, then env var.
    pub fn load() -> Result<Self> {
        let credentials_path = credentials_file_path();
        let mut tokens = None;
        let api_key = std::env::var("ANTHROPIC_API_KEY").ok();

        // Try loading stored OAuth tokens
        if credentials_path.exists() {
            match fs::read_to_string(&credentials_path) {
                Ok(contents) => match serde_json::from_str::<OAuthTokens>(&contents) {
                    Ok(t) => tokens = Some(t),
                    Err(e) => tracing::warn!("Failed to parse credentials: {e}"),
                },
                Err(e) => tracing::warn!("Failed to read credentials: {e}"),
            }
        }

        Ok(Self {
            tokens,
            api_key,
            credentials_path,
        })
    }

    /// Check current auth mode.
    pub fn mode(&self) -> AuthMode {
        if self.tokens.is_some() {
            AuthMode::OAuth
        } else if self.api_key.is_some() {
            AuthMode::ApiKey
        } else {
            AuthMode::None
        }
    }

    /// Run the browser-based OAuth login flow.
    pub async fn login(&mut self) -> Result<()> {
        let tokens = run_oauth_flow().await?;
        self.store_tokens(&tokens)?;
        self.tokens = Some(tokens);
        Ok(())
    }

    /// Get current authorization header value. Refreshes OAuth token if needed.
    pub async fn get_auth_headers(&mut self) -> Result<Vec<(String, String)>> {
        if let Some(tokens) = &self.tokens {
            // Check if token needs refresh (within 5 minutes of expiry)
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();

            if now + 300 >= tokens.expires_at {
                self.refresh_token().await?;
            }

            let access_token = self.tokens.as_ref().unwrap().access_token.clone();
            Ok(vec![
                ("Authorization".to_string(), format!("Bearer {access_token}")),
                (
                    "anthropic-beta".to_string(),
                    "oauth-2025-04-20".to_string(),
                ),
            ])
        } else if let Some(api_key) = &self.api_key {
            Ok(vec![("x-api-key".to_string(), api_key.clone())])
        } else {
            Err(AgentError::Auth(
                "No authentication configured. Run `infinity-agent login` or set ANTHROPIC_API_KEY"
                    .to_string(),
            ))
        }
    }

    /// Refresh the OAuth access token.
    async fn refresh_token(&mut self) -> Result<()> {
        let refresh_token = self
            .tokens
            .as_ref()
            .ok_or_else(|| AgentError::Auth("No tokens to refresh".to_string()))?
            .refresh_token
            .clone();

        let client = reqwest::Client::new();
        let resp = client
            .post(TOKEN_URL)
            .json(&serde_json::json!({
                "grant_type": "refresh_token",
                "refresh_token": refresh_token,
                "client_id": CLIENT_ID,
            }))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(AgentError::Auth(format!(
                "Token refresh failed ({status}): {body}"
            )));
        }

        let token_resp: TokenResponse = resp.json().await?;
        let tokens = OAuthTokens {
            access_token: token_resp.access_token,
            refresh_token: token_resp
                .refresh_token
                .unwrap_or_else(|| self.tokens.as_ref().unwrap().refresh_token.clone()),
            expires_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
                + token_resp.expires_in,
            scopes: token_resp
                .scope
                .unwrap_or_default()
                .split_whitespace()
                .map(|s| s.to_string())
                .collect(),
        };

        self.store_tokens(&tokens)?;
        self.tokens = Some(tokens);
        Ok(())
    }

    /// Store tokens to file.
    fn store_tokens(&self, tokens: &OAuthTokens) -> Result<()> {
        if let Some(parent) = self.credentials_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(tokens)?;
        fs::write(&self.credentials_path, &json)?;

        // Set file permissions to 0600 on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                &self.credentials_path,
                fs::Permissions::from_mode(0o600),
            )?;
        }

        Ok(())
    }

    /// Clear stored credentials.
    pub fn logout(&self) -> Result<()> {
        if self.credentials_path.exists() {
            fs::remove_file(&self.credentials_path)?;
        }
        Ok(())
    }

    /// Check if authenticated.
    pub fn is_authenticated(&self) -> bool {
        self.tokens.is_some() || self.api_key.is_some()
    }

    /// Get a description of the current auth status.
    pub fn status_string(&self) -> String {
        match self.mode() {
            AuthMode::OAuth => "Authenticated via OAuth".to_string(),
            AuthMode::ApiKey => "Authenticated via ANTHROPIC_API_KEY".to_string(),
            AuthMode::None => "Not authenticated".to_string(),
        }
    }
}

/// OAuth token response from Anthropic.
#[derive(serde::Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
    scope: Option<String>,
    #[allow(dead_code)]
    token_type: Option<String>,
}

/// Run the full OAuth 2.0 Authorization Code flow with PKCE.
async fn run_oauth_flow() -> Result<OAuthTokens> {
    // 1. Generate PKCE verifier and challenge
    let code_verifier = generate_random_string(128);
    let code_challenge = {
        let hash = Sha256::digest(code_verifier.as_bytes());
        URL_SAFE_NO_PAD.encode(hash)
    };

    // 2. Generate state for CSRF protection
    let state = generate_random_string(32);

    // 3. Start local HTTP server on a random port
    let server = Arc::new(
        tiny_http::Server::http("127.0.0.1:0")
            .map_err(|e| AgentError::Auth(format!("Failed to start callback server: {e}")))?,
    );
    let port = server.server_addr().to_ip().unwrap().port();

    let redirect_uri = format!("http://localhost:{port}/callback");

    // 4. Build authorization URL
    let auth_url = format!(
        "{AUTH_URL}?client_id={CLIENT_ID}\
         &response_type=code\
         &redirect_uri={redirect_uri}\
         &scope={}\
         &code_challenge={code_challenge}\
         &code_challenge_method=S256\
         &state={state}",
        SCOPES.replace(' ', "+"),
    );

    println!("Opening browser for authentication...");
    println!("If the browser doesn't open, visit:\n{auth_url}\n");

    // Open browser
    if let Err(e) = open::that(&auth_url) {
        tracing::warn!("Failed to open browser: {e}");
    }

    // 5. Wait for the callback
    println!("Waiting for authentication callback...");
    let (code, returned_state) = wait_for_callback(server, &redirect_uri).await?;

    // 6. Verify state
    if returned_state != state {
        return Err(AgentError::Auth(
            "State mismatch — possible CSRF attack".to_string(),
        ));
    }

    // 7. Exchange code for tokens (Anthropic expects JSON, not form-urlencoded)
    let client = reqwest::Client::new();
    let resp = client
        .post(TOKEN_URL)
        .json(&serde_json::json!({
            "grant_type": "authorization_code",
            "code": code,
            "state": returned_state,
            "code_verifier": code_verifier,
            "client_id": CLIENT_ID,
            "redirect_uri": redirect_uri,
        }))
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        return Err(AgentError::Auth(format!(
            "Token exchange failed ({status}): {body}"
        )));
    }

    let token_resp: TokenResponse = resp.json().await?;

    Ok(OAuthTokens {
        access_token: token_resp.access_token,
        refresh_token: token_resp
            .refresh_token
            .unwrap_or_default(),
        expires_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + token_resp.expires_in,
        scopes: token_resp
            .scope
            .unwrap_or_default()
            .split_whitespace()
            .map(|s| s.to_string())
            .collect(),
    })
}

/// Wait for the OAuth callback on the local server.
async fn wait_for_callback(
    server: Arc<tiny_http::Server>,
    _redirect_uri: &str,
) -> Result<(String, String)> {
    // Run in a blocking thread since tiny_http is synchronous
    let result = tokio::task::spawn_blocking(move || -> Result<(String, String)> {
        // Wait up to 120 seconds for the callback
        let request = server
            .recv_timeout(std::time::Duration::from_secs(120))
            .map_err(|e| AgentError::Auth(format!("Callback server error: {e}")))?
            .ok_or_else(|| AgentError::Auth("Timed out waiting for callback".to_string()))?;

        let url = request.url().to_string();

        // Parse query parameters
        let query = url
            .split('?')
            .nth(1)
            .unwrap_or("");
        let params: std::collections::HashMap<String, String> = query
            .split('&')
            .filter_map(|pair: &str| {
                let mut parts = pair.splitn(2, '=');
                let key = parts.next()?.to_string();
                let value = parts.next().unwrap_or("").to_string();
                Some((key, value))
            })
            .collect();

        // Check for errors
        if let Some(error) = params.get("error") {
            let desc = params
                .get("error_description")
                .map(|s: &String| s.as_str())
                .unwrap_or("Unknown error");
            let response = tiny_http::Response::from_string(format!(
                "<html><body><h2>Authentication Failed</h2><p>{desc}</p></body></html>"
            ))
            .with_header(
                "Content-Type: text/html"
                    .parse::<tiny_http::Header>()
                    .unwrap(),
            );
            let _ = request.respond(response);
            return Err(AgentError::Auth(format!("OAuth error: {error} — {desc}")));
        }

        let code = params
            .get("code")
            .ok_or_else(|| AgentError::Auth("No authorization code in callback".to_string()))?
            .clone();
        let state = params
            .get("state")
            .ok_or_else(|| AgentError::Auth("No state in callback".to_string()))?
            .clone();

        // Send success response
        let response = tiny_http::Response::from_string(
            "<html><body><h2>Authentication Successful!</h2>\
             <p>You can close this tab and return to infinity-agent.</p></body></html>",
        )
        .with_header(
            "Content-Type: text/html"
                .parse::<tiny_http::Header>()
                .unwrap(),
        );
        let _ = request.respond(response);

        Ok((code, state))
    })
    .await
    .map_err(|e| AgentError::Auth(format!("Callback task failed: {e}")))?;

    result
}

/// Generate a random alphanumeric string.
fn generate_random_string(len: usize) -> String {
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    let mut rng = rand::rng();
    (0..len)
        .map(|_| {
            let idx = rng.random_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect()
}

/// Get the path to the credentials file.
fn credentials_file_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".infinity")
        .join(".credentials.json")
}
