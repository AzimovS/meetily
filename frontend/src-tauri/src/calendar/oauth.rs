//! Google OAuth 2.0 Desktop flow (PKCE + loopback + client_secret).
//!
//! See RFC 8252 (OAuth for Native Apps) and RFC 7636 (PKCE). Google's
//! Desktop flow requires both PKCE *and* client_secret on token
//! exchange — see `credentials.rs` for the reasoning. We use the
//! `oauth2` crate for authorization-URL building and PKCE helpers (pure
//! Rust, no HTTP) and run the token-exchange POST manually via the
//! project's existing `reqwest` 0.11 client.

use std::time::Duration;

use oauth2::basic::BasicClient;
use oauth2::{AuthUrl, ClientId, CsrfToken, PkceCodeChallenge, RedirectUrl, Scope, TokenUrl};
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::time::timeout;

use crate::calendar::credentials;

/// Timeout for the user to complete the browser consent step.
const CONSENT_TIMEOUT: Duration = Duration::from_secs(120);

/// Google OAuth 2.0 authorization endpoint.
const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";

/// Google OAuth 2.0 token endpoint.
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

/// Scope required for reading the user's calendar events.
const CALENDAR_SCOPE: &str = "https://www.googleapis.com/auth/calendar.events.readonly";

#[derive(Debug)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in_secs: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct TokenError {
    error: String,
    error_description: Option<String>,
}

/// Exchange a refresh_token for a fresh access_token. Google may or may
/// not return a new refresh_token — the caller preserves the existing
/// one if the response omits it.
pub async fn refresh_access_token(refresh_token: &str) -> Result<Tokens, String> {
    let client_id = credentials::client_id().ok_or_else(|| {
        "Meetily was built without a Google OAuth client id.".to_string()
    })?;
    let client_secret = credentials::client_secret().ok_or_else(|| {
        "Meetily was built without a Google OAuth client secret.".to_string()
    })?;

    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {e}"))?;

    let form = [
        ("client_id", client_id),
        ("client_secret", client_secret),
        ("refresh_token", refresh_token),
        ("grant_type", "refresh_token"),
    ];

    let response = http
        .post(TOKEN_URL)
        .form(&form)
        .send()
        .await
        .map_err(|e| format!("Refresh request failed: {e}"))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| format!("Failed to read refresh response body: {e}"))?;

    if !status.is_success() {
        let detail = serde_json::from_str::<TokenError>(&body)
            .map(|e| {
                format!(
                    "{}{}",
                    e.error,
                    e.error_description
                        .map(|d| format!(" — {d}"))
                        .unwrap_or_default()
                )
            })
            .unwrap_or_else(|_| body.clone());
        return Err(format!("refresh_failed:{status}:{detail}"));
    }

    let parsed: TokenResponse = serde_json::from_str(&body)
        .map_err(|e| format!("Refresh response not valid JSON: {e}; body: {body}"))?;

    Ok(Tokens {
        access_token: parsed.access_token,
        refresh_token: parsed.refresh_token,
        expires_in_secs: parsed.expires_in,
    })
}

/// Run the full connect flow. Blocks until the user consents in their
/// browser or the 120-second timeout elapses.
pub async fn connect() -> Result<Tokens, String> {
    let client_id = credentials::client_id().ok_or_else(|| {
        "Meetily was built without a Google OAuth client id. \
         Set MEETILY_GOOGLE_CLIENT_ID at build time."
            .to_string()
    })?;
    let client_secret = credentials::client_secret().ok_or_else(|| {
        "Meetily was built without a Google OAuth client secret. \
         Set MEETILY_GOOGLE_CLIENT_SECRET at build time."
            .to_string()
    })?;

    // 1. Bind loopback first so we know the port before building the auth URL.
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("Failed to bind loopback listener: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("Failed to read listener addr: {e}"))?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}");

    // 2. PKCE challenge + verifier.
    let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();

    // 3. Use oauth2 to build the authorization URL. The client_secret is
    //    not sent here — it's only used in the token-exchange POST below.
    let client = BasicClient::new(ClientId::new(client_id.to_string()))
        .set_auth_uri(AuthUrl::new(AUTH_URL.to_string()).map_err(|e| e.to_string())?)
        .set_token_uri(TokenUrl::new(TOKEN_URL.to_string()).map_err(|e| e.to_string())?)
        .set_redirect_uri(RedirectUrl::new(redirect_uri.clone()).map_err(|e| e.to_string())?);

    let (auth_url, csrf_token) = client
        .authorize_url(CsrfToken::new_random)
        .add_scope(Scope::new(CALENDAR_SCOPE.to_string()))
        .set_pkce_challenge(pkce_challenge)
        // access_type=offline asks Google to issue a refresh_token.
        .add_extra_param("access_type", "offline")
        // prompt=consent forces the refresh_token to be re-issued each connect.
        .add_extra_param("prompt", "consent")
        .url();

    // 4. Open the user's default browser.
    opener::open(auth_url.as_str())
        .map_err(|e| format!("Failed to open browser: {e}"))?;

    log::info!(
        "[calendar] OAuth consent URL opened; awaiting loopback callback on 127.0.0.1:{port}"
    );

    // 5. Wait for the browser to redirect back to loopback.
    let (code, returned_state) = timeout(CONSENT_TIMEOUT, wait_for_callback(&listener))
        .await
        .map_err(|_| "OAuth consent timed out after 120 seconds".to_string())?
        .map_err(|e| format!("Loopback callback failed: {e}"))?;

    // 6. CSRF check.
    if returned_state != *csrf_token.secret() {
        return Err("OAuth state mismatch (possible CSRF). Aborting.".to_string());
    }

    // 7. Exchange the authorization code for tokens (manual POST, reqwest 0.11).
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {e}"))?;

    let form = [
        ("code", code.as_str()),
        ("code_verifier", pkce_verifier.secret().as_str()),
        ("client_id", client_id),
        ("client_secret", client_secret),
        ("redirect_uri", redirect_uri.as_str()),
        ("grant_type", "authorization_code"),
    ];

    let response = http
        .post(TOKEN_URL)
        .form(&form)
        .send()
        .await
        .map_err(|e| format!("Token exchange request failed: {e}"))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| format!("Failed to read token response body: {e}"))?;

    if !status.is_success() {
        let detail = serde_json::from_str::<TokenError>(&body)
            .map(|e| {
                format!(
                    "{}{}",
                    e.error,
                    e.error_description
                        .map(|d| format!(" — {d}"))
                        .unwrap_or_default()
                )
            })
            .unwrap_or_else(|_| body.clone());
        return Err(format!("Token exchange failed ({status}): {detail}"));
    }

    let parsed: TokenResponse = serde_json::from_str(&body)
        .map_err(|e| format!("Token response not valid JSON: {e}; body: {body}"))?;

    Ok(Tokens {
        access_token: parsed.access_token,
        refresh_token: parsed.refresh_token,
        expires_in_secs: parsed.expires_in,
    })
}

/// Accept one incoming HTTP GET on the loopback listener, parse the `code`
/// and `state` query params, and return them.
async fn wait_for_callback(listener: &TcpListener) -> std::io::Result<(String, String)> {
    let (mut socket, _) = listener.accept().await?;

    let (reader, mut writer) = socket.split();
    let mut reader = BufReader::new(reader);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).await?;

    let path = request_line.split_whitespace().nth(1).unwrap_or("");
    let query = path.split('?').nth(1).unwrap_or("");

    let mut code = String::new();
    let mut state = String::new();
    for pair in query.split('&') {
        let mut kv = pair.splitn(2, '=');
        let k = kv.next().unwrap_or("");
        let v = kv.next().unwrap_or("");
        let decoded = urldecode(v);
        match k {
            "code" => code = decoded,
            "state" => state = decoded,
            _ => {}
        }
    }

    // Drain remaining headers so the client doesn't hang.
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 || line == "\r\n" || line == "\n" {
            break;
        }
    }

    let body = r#"<!doctype html><html><head><meta charset="utf-8"><title>Meetily</title></head>
<body style="font-family:system-ui,sans-serif;text-align:center;padding-top:80px;color:#111;">
<h1>Meetily connected to Google Calendar</h1>
<p>You can close this tab and return to the app.</p>
</body></html>"#;
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = writer.write_all(response.as_bytes()).await;
    let _ = writer.flush().await;

    Ok((code, state))
}

fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        } else if bytes[i] == b'+' {
            out.push(b' ');
            i += 1;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}
