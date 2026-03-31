//! Credential storage and OAuth flows for API providers.
//!
//! Auth priority:
//! 1. Environment variable (ANTHROPIC_API_KEY, etc.)
//! 2. OAuth token from macOS Keychain (auto-refreshed)
//! 3. API key from macOS Keychain
//!
//! Credentials are stored in the macOS Keychain via the `security` CLI.
//! Each provider gets a keychain entry with service "nerv-{provider}".

use std::io::{BufRead, Write};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Credential {
    #[serde(rename = "api_key")]
    ApiKey { key: String },
    #[serde(rename = "oauth")]
    OAuth(OAuthCredentials),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthCredentials {
    pub refresh: String,
    pub access: String,
    pub expires: u64, // epoch millis
}

pub struct AuthStorage {
    // Credentials are loaded on demand from Keychain, not cached in memory
}

impl AuthStorage {
    pub fn load(_nerv_dir: &std::path::Path) -> Self {
        Self {}
    }

    pub fn set(&mut self, provider: &str, cred: Credential) {
        let json = serde_json::to_string(&cred).unwrap_or_default();
        keychain_set(provider, &json);
    }

    pub fn remove(&mut self, provider: &str) {
        keychain_delete(provider);
    }

    pub fn get(&self, provider: &str) -> Option<Credential> {
        let json = keychain_get(provider)?;
        serde_json::from_str(&json).ok()
    }

    pub fn is_oauth(&self, provider: &str) -> bool {
        matches!(self.get(provider), Some(Credential::OAuth(_)))
    }

    /// Get an API key for a provider, checking env vars first, then Keychain.
    /// For OAuth credentials, auto-refreshes if expired.
    pub fn api_key(&mut self, provider: &str) -> Option<String> {
        // 1. Environment variable
        let env_key = match provider {
            "anthropic" => std::env::var("ANTHROPIC_API_KEY").ok(),
            "codex" => std::env::var("OPENAI_API_KEY").ok(),
            "openrouter" => std::env::var("OPENROUTER_API_KEY").ok(),
            _ => None,
        };
        if let Some(ref key) = env_key {
            crate::log::debug(&format!(
                "auth: using env var for {} ({}...)",
                provider,
                &key[..key.len().min(8)]
            ));
            return env_key;
        }

        // 2. Keychain credentials
        let cred = self.get(provider);
        crate::log::debug(&format!(
            "auth: keychain lookup for {}: {}",
            provider,
            match &cred {
                Some(Credential::ApiKey { .. }) => "api_key",
                Some(Credential::OAuth(o)) => {
                    if epoch_millis() >= o.expires { "oauth (expired)" } else { "oauth (valid)" }
                }
                None => "not found",
            }
        ));
        match cred? {
            Credential::ApiKey { key } => Some(key),
            Credential::OAuth(creds) => {
                let now = epoch_millis();
                if now >= creds.expires {
                    match refresh_oauth_token(provider, &creds) {
                        Ok(new_creds) => {
                            let access = new_creds.access.clone();
                            self.set(provider, Credential::OAuth(new_creds));
                            Some(access)
                        }
                        Err(e) => {
                            crate::log::error(&format!(
                                "OAuth refresh failed for {}: {}",
                                provider, e
                            ));
                            None
                        }
                    }
                } else {
                    Some(creds.access)
                }
            }
        }
    }

    pub fn has_auth(&self, provider: &str) -> bool {
        let env = match provider {
            "anthropic" => std::env::var("ANTHROPIC_API_KEY").is_ok(),
            "codex" => std::env::var("OPENAI_API_KEY").is_ok(),
            "openrouter" => std::env::var("OPENROUTER_API_KEY").is_ok(),
            _ => false,
        };
        env || keychain_get(provider).is_some()
    }
}

const KEYCHAIN_ACCOUNT: &str = "nerv";

fn keychain_service(provider: &str) -> String {
    format!("nerv-{}", provider)
}

fn keychain_set(provider: &str, value: &str) {
    let service = keychain_service(provider);
    // -U updates if exists, creates if not
    let _ = std::process::Command::new("security")
        .args(["add-generic-password", "-a", KEYCHAIN_ACCOUNT, "-s", &service, "-w", value, "-U"])
        .output();
}

fn keychain_get(provider: &str) -> Option<String> {
    let service = keychain_service(provider);
    let output = std::process::Command::new("security")
        .args(["find-generic-password", "-a", KEYCHAIN_ACCOUNT, "-s", &service, "-w"])
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

fn keychain_delete(provider: &str) {
    let service = keychain_service(provider);
    let _ = std::process::Command::new("security")
        .args(["delete-generic-password", "-a", KEYCHAIN_ACCOUNT, "-s", &service])
        .output();
}

const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const CALLBACK_PORT: u16 = 53692;
const CALLBACK_PATH: &str = "/callback";
const SCOPES: &str = "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";

const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CODEX_AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const CODEX_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const CODEX_REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const CODEX_SCOPE: &str = "openid profile email offline_access";

fn base64url_encode(bytes: &[u8]) -> String {
    use base64url_chars::*;
    let mut out = String::with_capacity((bytes.len() * 4 / 3) + 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(CHARS[(n >> 18 & 63) as usize]);
        out.push(CHARS[(n >> 12 & 63) as usize]);
        if chunk.len() > 1 {
            out.push(CHARS[(n >> 6 & 63) as usize]);
        }
        if chunk.len() > 2 {
            out.push(CHARS[(n & 63) as usize]);
        }
    }
    out
}

mod base64url_chars {
    pub const CHARS: [char; 64] = [
        'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O', 'P', 'Q', 'R',
        'S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j',
        'k', 'l', 'm', 'n', 'o', 'p', 'q', 'r', 's', 't', 'u', 'v', 'w', 'x', 'y', 'z', '0', '1',
        '2', '3', '4', '5', '6', '7', '8', '9', '-', '_',
    ];
}

fn generate_pkce() -> (String, String) {
    use sha2::{Digest, Sha256};

    let mut verifier_bytes = [0u8; 32];
    getrandom(&mut verifier_bytes);
    let verifier = base64url_encode(&verifier_bytes);

    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let hash = hasher.finalize();
    let challenge = base64url_encode(&hash);

    (verifier, challenge)
}

fn getrandom(buf: &mut [u8]) {
    use std::fs::File;
    use std::io::Read;
    File::open("/dev/urandom")
        .expect("failed to open /dev/urandom")
        .read_exact(buf)
        .expect("failed to read random bytes");
}

fn epoch_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Run the Anthropic OAuth login flow. Blocks until complete.
pub fn login_anthropic(
    on_url: &dyn Fn(&str),
    on_status: &dyn Fn(&str),
) -> Result<OAuthCredentials, String> {
    let (verifier, challenge) = generate_pkce();
    let redirect_uri = format!("http://localhost:{}{}", CALLBACK_PORT, CALLBACK_PATH);

    let auth_url = format!(
        "{}?code=true&client_id={}&response_type=code&redirect_uri={}&scope={}&code_challenge={}&code_challenge_method=S256&state={}",
        AUTHORIZE_URL,
        CLIENT_ID,
        urlencoded(&redirect_uri),
        urlencoded(SCOPES),
        challenge,
        &verifier,
    );

    let listener = std::net::TcpListener::bind(format!("127.0.0.1:{}", CALLBACK_PORT))
        .map_err(|e| format!("Failed to bind callback server on port {}: {}", CALLBACK_PORT, e))?;
    listener.set_nonblocking(false).map_err(|e| format!("Failed to set blocking: {}", e))?;

    on_url(&auth_url);

    let (code, state) = wait_for_callback(&listener)?;

    if state != verifier {
        return Err("OAuth state mismatch".into());
    }

    on_status("Exchanging authorization code for tokens...");
    exchange_code(&code, &state, &verifier, &redirect_uri)
}

fn wait_for_callback(listener: &std::net::TcpListener) -> Result<(String, String), String> {
    let (mut stream, _addr) =
        listener.accept().map_err(|e| format!("Failed to accept callback connection: {}", e))?;

    let mut reader = std::io::BufReader::new(&stream);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).map_err(|e| format!("Failed to read request: {}", e))?;

    let path = request_line.split_whitespace().nth(1).ok_or("Invalid HTTP request")?;

    let query = path.split('?').nth(1).ok_or("No query string in callback")?;

    let mut code = None;
    let mut state = None;
    for param in query.split('&') {
        let mut kv = param.splitn(2, '=');
        match (kv.next(), kv.next()) {
            (Some("code"), Some(v)) => code = Some(urldecoded(v)),
            (Some("state"), Some(v)) => state = Some(urldecoded(v)),
            _ => {}
        }
    }

    let body = "<!DOCTYPE html><html><body><h2>Authentication successful!</h2><p>You can close this window and return to nerv.</p></body></html>";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();

    match (code, state) {
        (Some(c), Some(s)) => Ok((c, s)),
        _ => Err("Missing code or state in callback".into()),
    }
}

fn exchange_code(
    code: &str,
    state: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<OAuthCredentials, String> {
    let body = serde_json::json!({
        "grant_type": "authorization_code",
        "client_id": CLIENT_ID,
        "code": code,
        "state": state,
        "redirect_uri": redirect_uri,
        "code_verifier": verifier,
    });

    let response = crate::http::agent()
        .post(TOKEN_URL)
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .send_json(&body)
        .map_err(|e| format!("Token exchange failed: {}", e))?;

    parse_token_response(response, "Token exchange")
}

fn refresh_oauth_token(
    provider: &str,
    creds: &OAuthCredentials,
) -> Result<OAuthCredentials, String> {
    match provider {
        "anthropic" => refresh_anthropic_token(&creds.refresh),
        "codex" => refresh_codex_token(&creds.refresh),
        _ => Err(format!("No OAuth refresh for provider: {}", provider)),
    }
}

fn refresh_anthropic_token(refresh_token: &str) -> Result<OAuthCredentials, String> {
    let body = serde_json::json!({
        "grant_type": "refresh_token",
        "client_id": CLIENT_ID,
        "refresh_token": refresh_token,
    });

    let response = crate::http::agent()
        .post(TOKEN_URL)
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .send_json(&body)
        .map_err(|e| format!("Token refresh failed: {}", e))?;

    parse_token_response(response, "Token refresh")
}

fn parse_token_response(
    response: ureq::http::Response<ureq::Body>,
    context: &str,
) -> Result<OAuthCredentials, String> {
    let status = response.status();
    if status != 200 {
        let err_body = response.into_body().read_to_string().unwrap_or_default();
        return Err(format!("{} failed: HTTP {} — {}", context, status, err_body));
    }

    let data: serde_json::Value = response
        .into_body()
        .read_json()
        .map_err(|e| format!("{} returned invalid JSON: {}", context, e))?;

    let access = data["access_token"].as_str().ok_or("Missing access_token")?.to_string();
    let refresh = data["refresh_token"].as_str().ok_or("Missing refresh_token")?.to_string();
    let expires_in = data["expires_in"].as_u64().unwrap_or(3600);

    Ok(OAuthCredentials {
        refresh,
        access,
        expires: epoch_millis() + expires_in * 1000 - 5 * 60 * 1000,
    })
}

fn urlencoded(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push(HEX[(b >> 4) as usize]);
                out.push(HEX[(b & 0xf) as usize]);
            }
        }
    }
    out
}

const HEX: [char; 16] =
    ['0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'A', 'B', 'C', 'D', 'E', 'F'];

fn urldecoded(s: &str) -> String {
    let mut out = Vec::with_capacity(s.len());
    let mut bytes = s.bytes();
    while let Some(b) = bytes.next() {
        if b == b'%' {
            let h = bytes.next().and_then(hex_val);
            let l = bytes.next().and_then(hex_val);
            if let (Some(h), Some(l)) = (h, l) {
                out.push(h << 4 | l);
            }
        } else if b == b'+' {
            out.push(b' ');
        } else {
            out.push(b);
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn refresh_codex_token(refresh_token: &str) -> Result<OAuthCredentials, String> {
    let body = serde_json::json!({
        "grant_type": "refresh_token",
        "client_id": CODEX_CLIENT_ID,
        "refresh_token": refresh_token,
    });
    let response = crate::http::agent()
        .post(CODEX_TOKEN_URL)
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .send_json(&body)
        .map_err(|e| format!("Token refresh failed: {}", e))?;
    parse_token_response(response, "Token refresh")
}

/// Run the OpenAI Codex OAuth login flow. Blocks until complete.
pub fn login_codex(
    on_url: &dyn Fn(&str),
    on_status: &dyn Fn(&str),
) -> Result<OAuthCredentials, String> {
    let (verifier, challenge) = generate_pkce();

    let auth_url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&code_challenge={}&code_challenge_method=S256&state={}&codex_cli_simplified_flow=true&originator=nerv",
        CODEX_AUTHORIZE_URL,
        CODEX_CLIENT_ID,
        urlencoded(CODEX_REDIRECT_URI),
        urlencoded(CODEX_SCOPE),
        challenge,
        &verifier,
    );

    let listener = std::net::TcpListener::bind("127.0.0.1:1455")
        .map_err(|e| format!("Failed to bind callback server on port 1455: {}", e))?;

    on_url(&auth_url);

    let (code, state) = wait_for_callback(&listener)?;

    if state != verifier {
        return Err("OAuth state mismatch".into());
    }

    on_status("Exchanging authorization code for tokens...");

    let body = serde_json::json!({
        "grant_type": "authorization_code",
        "client_id": CODEX_CLIENT_ID,
        "code": code,
        "redirect_uri": CODEX_REDIRECT_URI,
        "code_verifier": verifier,
    });
    let response = crate::http::agent()
        .post(CODEX_TOKEN_URL)
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .send_json(&body)
        .map_err(|e| format!("Token exchange failed: {}", e))?;
    parse_token_response(response, "Token exchange")
}
