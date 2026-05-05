//! Google OAuth 2.0 Desktop flow (PKCE + loopback + client_secret).
//!
//! PKCE, CSRF state, and authorization-URL construction are done inline
//! (no oauth2 crate, to avoid pulling in a duplicate reqwest/hyper/rustls
//! stack). Token exchange and refresh go through the project's existing
//! reqwest 0.11 client. Google's Desktop flow requires both PKCE *and*
//! client_secret on token exchange — see `credentials.rs` for the
//! reasoning.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};
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

/// Guard against overlapping consent flows. A user double-clicking
/// Connect would otherwise bind two loopback listeners and open two
/// browser tabs; the "losing" flow also causes token churn via the
/// deleting/writing interleaving in `api_calendar_connect`.
static CONNECT_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

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
    let client_id = credentials::client_id()
        .ok_or_else(|| "Missing Google OAuth client id (build-time)".to_string())?;
    let client_secret = credentials::client_secret()
        .ok_or_else(|| "Missing Google OAuth client secret (build-time)".to_string())?;

    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(crate::calendar::api::HTTP_CONNECT_TIMEOUT)
        .timeout(crate::calendar::api::HTTP_TOTAL_TIMEOUT)
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
        return Err(format_token_error("refresh", status, &body));
    }

    let parsed: TokenResponse = serde_json::from_str(&body)
        .map_err(|_| format!("Refresh response malformed (status {status})"))?;

    Ok(Tokens {
        access_token: parsed.access_token,
        refresh_token: parsed.refresh_token,
        expires_in_secs: parsed.expires_in,
    })
}

/// Run the full connect flow. Blocks until the user consents in their
/// browser or the 120-second timeout elapses. Returns `Err` immediately
/// if another connect is already in progress.
pub async fn connect() -> Result<Tokens, String> {
    if CONNECT_IN_FLIGHT
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Acquire)
        .is_err()
    {
        return Err("A Google Calendar connect is already in progress".to_string());
    }
    // RAII guard so we always clear the flag, even on panics.
    let _guard = InFlightGuard;

    let client_id = credentials::client_id()
        .ok_or_else(|| "Missing Google OAuth client id (build-time)".to_string())?;
    let client_secret = credentials::client_secret()
        .ok_or_else(|| "Missing Google OAuth client secret (build-time)".to_string())?;

    // 1. Bind loopback first so we know the port before building the auth URL.
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("Failed to bind loopback listener: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("Failed to read listener addr: {e}"))?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}");

    // 2. PKCE challenge + verifier + CSRF state.
    let pkce_verifier = random_base64_url(32);
    let pkce_challenge = sha256_base64_url(&pkce_verifier);
    let csrf_state = random_base64_url(24);

    // 3. Build the authorization URL. No client_secret here — that ships
    //    only with the token-exchange POST below.
    let auth_url = build_authorize_url(client_id, &redirect_uri, &pkce_challenge, &csrf_state);

    // 4. Open the user's default browser.
    opener::open(&auth_url).map_err(|e| format!("Failed to open browser: {e}"))?;

    log::info!(
        "[calendar] OAuth consent URL opened; awaiting loopback callback on 127.0.0.1:{port}"
    );

    // 5. Wait for the browser to redirect back to loopback.
    let outcome = timeout(CONSENT_TIMEOUT, wait_for_callback(&listener))
        .await
        .map_err(|_| "OAuth consent timed out after 120 seconds".to_string())?
        .map_err(|e| format!("Loopback callback failed: {e}"))?;

    let (code, returned_state) = match outcome {
        CallbackOutcome::Success { code, state } => (code, state),
        CallbackOutcome::Denied { error, description } => {
            let detail = description
                .filter(|d| !d.is_empty())
                .map(|d| format!(" — {d}"))
                .unwrap_or_default();
            return Err(format!("OAuth consent declined ({error}){detail}"));
        }
        CallbackOutcome::Missing { .. } => {
            return Err("OAuth callback returned no authorization code".to_string());
        }
    };

    // 6. CSRF check.
    if returned_state != csrf_state {
        return Err("OAuth state mismatch (possible CSRF). Aborting.".to_string());
    }

    // 7. Exchange the authorization code for tokens.
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(crate::calendar::api::HTTP_CONNECT_TIMEOUT)
        .timeout(crate::calendar::api::HTTP_TOTAL_TIMEOUT)
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {e}"))?;

    let form = [
        ("code", code.as_str()),
        ("code_verifier", pkce_verifier.as_str()),
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
        return Err(format_token_error("token exchange", status, &body));
    }

    let parsed: TokenResponse = serde_json::from_str(&body)
        .map_err(|_| format!("Token response malformed (status {status})"))?;

    Ok(Tokens {
        access_token: parsed.access_token,
        refresh_token: parsed.refresh_token,
        expires_in_secs: parsed.expires_in,
    })
}

struct InFlightGuard;
impl Drop for InFlightGuard {
    fn drop(&mut self) {
        CONNECT_IN_FLIGHT.store(false, Ordering::Release);
    }
}

/// Format a Google OAuth error response without including the raw body
/// (which on malformed responses may contain tokens). We only surface
/// the `error` + `error_description` fields, which Google's spec says
/// are safe.
fn format_token_error(kind: &str, status: reqwest::StatusCode, body: &str) -> String {
    match serde_json::from_str::<TokenError>(body) {
        Ok(e) => {
            let desc = e
                .error_description
                .map(|d| format!(" — {d}"))
                .unwrap_or_default();
            format!("OAuth {kind} failed ({status}): {}{desc}", e.error)
        }
        Err(_) => format!("OAuth {kind} failed ({status}): malformed response"),
    }
}

fn build_authorize_url(
    client_id: &str,
    redirect_uri: &str,
    pkce_challenge: &str,
    state: &str,
) -> String {
    let pairs = [
        ("client_id", client_id),
        ("redirect_uri", redirect_uri),
        ("response_type", "code"),
        ("scope", CALENDAR_SCOPE),
        ("access_type", "offline"),
        ("prompt", "consent"),
        ("code_challenge", pkce_challenge),
        ("code_challenge_method", "S256"),
        ("state", state),
    ];

    let query = pairs
        .iter()
        .map(|(k, v)| format!("{}={}", k, urlencode(v)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{AUTH_URL}?{query}")
}

fn random_base64_url(byte_len: usize) -> String {
    let mut bytes = vec![0u8; byte_len];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(&bytes)
}

fn sha256_base64_url(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    URL_SAFE_NO_PAD.encode(hasher.finalize())
}

/// RFC 3986 unreserved + `%XX` percent-encoding, sufficient for OAuth
/// URL query values (scopes, state, etc).
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Outcome of the loopback OAuth callback. Surfaces consent denial as
/// a distinct case so we don't render a success page when Google
/// actually returned `error=access_denied`.
#[derive(Debug, PartialEq)]
enum CallbackOutcome {
    Success {
        code: String,
        state: String,
    },
    /// User clicked "Cancel" on the Google consent screen, or Google
    /// returned an error (e.g. `access_denied`, `invalid_scope`).
    Denied {
        error: String,
        description: Option<String>,
    },
    /// No `error` and no `code` — typically a stray request to the
    /// loopback port, or a misconfigured Google Cloud client.
    Missing {
        state: String,
    },
}

/// Pure parser for the loopback callback query string. Split out from
/// the I/O wrapper so the success/denied/missing branches are unit-testable.
fn parse_callback_query(query: &str) -> CallbackOutcome {
    let mut code = String::new();
    let mut state = String::new();
    let mut error = String::new();
    let mut description: Option<String> = None;
    for pair in query.split('&') {
        let mut kv = pair.splitn(2, '=');
        let k = kv.next().unwrap_or("");
        let v = kv.next().unwrap_or("");
        let decoded = urldecode(v);
        match k {
            "code" => code = decoded,
            "state" => state = decoded,
            "error" => error = decoded,
            "error_description" => description = Some(decoded),
            _ => {}
        }
    }

    if !error.is_empty() {
        return CallbackOutcome::Denied { error, description };
    }
    if code.is_empty() {
        return CallbackOutcome::Missing { state };
    }
    CallbackOutcome::Success { code, state }
}

/// Accept one incoming HTTP GET on the loopback listener, parse the
/// callback query, and render an appropriate HTML response back to
/// the browser tab. Returns the parsed outcome to the caller, who
/// decides whether to attempt a token exchange.
async fn wait_for_callback(listener: &TcpListener) -> std::io::Result<CallbackOutcome> {
    let (mut socket, _) = listener.accept().await?;

    let (reader, mut writer) = socket.split();
    let mut reader = BufReader::new(reader);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).await?;

    let path = request_line.split_whitespace().nth(1).unwrap_or("");
    let query = path.split('?').nth(1).unwrap_or("");
    let outcome = parse_callback_query(query);

    // Drain remaining headers so the client doesn't hang.
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 || line == "\r\n" || line == "\n" {
            break;
        }
    }

    let body = match &outcome {
        CallbackOutcome::Success { .. } => {
            r#"<!doctype html><html><head><meta charset="utf-8"><title>Meetily</title></head>
<body style="font-family:system-ui,sans-serif;text-align:center;padding-top:80px;color:#111;">
<h1>Meetily connected to Google Calendar</h1>
<p>You can close this tab and return to the app.</p>
</body></html>"#
                .to_string()
        }
        CallbackOutcome::Denied { error, description } => {
            // Show the user the same error the Rust side will surface,
            // so the browser tab and the in-app toast tell a consistent
            // story. Both fields are pre-encoded by Google as URL-safe
            // ASCII; html-escape just `<`/`>`/`&` defensively.
            let detail = description
                .as_deref()
                .filter(|d| !d.is_empty())
                .map(|d| format!("<p>{}</p>", html_escape(d)))
                .unwrap_or_default();
            format!(
                r#"<!doctype html><html><head><meta charset="utf-8"><title>Meetily</title></head>
<body style="font-family:system-ui,sans-serif;text-align:center;padding-top:80px;color:#111;">
<h1>Connection not completed</h1>
<p>Google reported: <code>{}</code></p>
{}
<p>You can close this tab and return to the app to try again.</p>
</body></html>"#,
                html_escape(error),
                detail
            )
        }
        CallbackOutcome::Missing { .. } => {
            r#"<!doctype html><html><head><meta charset="utf-8"><title>Meetily</title></head>
<body style="font-family:system-ui,sans-serif;text-align:center;padding-top:80px;color:#111;">
<h1>Connection not completed</h1>
<p>The callback returned no authorization code. Please try again from the app.</p>
</body></html>"#
                .to_string()
        }
    };
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = writer.write_all(response.as_bytes()).await;
    let _ = writer.flush().await;

    Ok(outcome)
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_success_callback() {
        let outcome = parse_callback_query("code=4%2F0Adeu5BX&state=abc123&scope=calendar");
        assert_eq!(
            outcome,
            CallbackOutcome::Success {
                code: "4/0Adeu5BX".to_string(),
                state: "abc123".to_string(),
            }
        );
    }

    #[test]
    fn parse_denied_callback_uses_error_arm() {
        let outcome = parse_callback_query(
            "error=access_denied&error_description=The+user+denied+the+request&state=abc",
        );
        assert_eq!(
            outcome,
            CallbackOutcome::Denied {
                error: "access_denied".to_string(),
                description: Some("The user denied the request".to_string()),
            }
        );
    }

    #[test]
    fn parse_denied_with_only_error_no_description() {
        let outcome = parse_callback_query("error=invalid_scope&state=abc");
        assert_eq!(
            outcome,
            CallbackOutcome::Denied {
                error: "invalid_scope".to_string(),
                description: None,
            }
        );
    }

    /// Defense-in-depth: `error` wins even if Google somehow includes a
    /// stray `code` field. Prevents a malicious response with both
    /// fields from sliding into the success arm.
    #[test]
    fn parse_treats_error_as_authoritative_when_both_present() {
        let outcome = parse_callback_query("code=foo&error=access_denied&state=abc");
        assert!(matches!(outcome, CallbackOutcome::Denied { .. }));
    }

    #[test]
    fn parse_missing_when_neither_code_nor_error_present() {
        let outcome = parse_callback_query("state=abc&scope=calendar");
        assert_eq!(
            outcome,
            CallbackOutcome::Missing {
                state: "abc".to_string(),
            }
        );
    }

    #[test]
    fn parse_empty_query_yields_missing() {
        assert!(matches!(
            parse_callback_query(""),
            CallbackOutcome::Missing { .. }
        ));
    }
}
