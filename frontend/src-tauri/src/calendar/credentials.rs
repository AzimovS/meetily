//! Google OAuth client credentials for the Calendar integration.
//!
//! ## Design
//!
//! Meetily uses Google's OAuth 2.0 Desktop flow with PKCE (RFC 7636).
//! Google's Desktop flow requires BOTH a PKCE challenge AND a client_secret
//! on token exchange — see Step 5 of
//! https://developers.google.com/identity/protocols/oauth2/native-app.
//! This is a Google-specific quirk; the generic "public client" model of
//! RFC 8252 §8.5 allows PKCE-only token exchange without a secret, but
//! Google's token endpoint rejects that with `invalid_request — client_secret
//! is missing`. So we ship both.
//!
//! The client_secret is not cryptographically confidential — anyone can
//! extract it from the shipped binary with `strings(1)` or a debugger.
//! Per Google's and RFC 8252's explicit guidance, it's fine to ship it
//! with a Desktop client because PKCE is what actually authenticates the
//! request. The value is best thought of as an app identifier, not a
//! secret in the cryptographic sense.
//!
//! ## How the values get in
//!
//! `build.rs` reads `MEETILY_GOOGLE_CLIENT_ID` and
//! `MEETILY_GOOGLE_CLIENT_SECRET` from the build environment and
//! re-exports them as `rustc-env` so `option_env!` below can pick them
//! up. Three ways to set them:
//!
//! 1. **Local dev (shell export)**:
//!    ```sh
//!    export MEETILY_GOOGLE_CLIENT_ID="123-abc.apps.googleusercontent.com"
//!    export MEETILY_GOOGLE_CLIENT_SECRET="GOCSPX-..."
//!    ./clean_run.sh
//!    ```
//!
//! 2. **Local dev (cargo config)**: uncomment the lines in
//!    `frontend/src-tauri/.cargo/config.toml` to persist across shells.
//!
//! 3. **Release builds (CI)**: GitHub Actions exports both variables from
//!    repo secrets before `cargo tauri build`. The shipped binary carries
//!    the production (verified) credentials.
//!
//! ## Rotation
//!
//! - **Reset client_secret in Google Console**: update the GitHub
//!   Actions secret, cut a new release. Existing users keep working —
//!   refresh tokens reference client_id, not client_secret.
//! - **New client_id entirely**: same as above plus a one-time
//!   reconnect for existing users (`invalid_grant` flow handles this).
//! - **Transfer the GCP project to another Google account**: client_id
//!   stays the same → zero user impact. Right path when Meetily
//!   graduates from a personal project.
//!
//! ## For contributors
//!
//! If you're hacking on Meetily and want to test Calendar, register your
//! own OAuth Desktop client in your own Google Cloud project (~15 min,
//! free) and export both env vars in your shell. You do not need the
//! maintainers' production credentials. Builds without the env vars
//! compile fine — Calendar just reports "not configured" at connect time.

/// Resolves the client_id from build-time env. `None` when the binary
/// was built without `MEETILY_GOOGLE_CLIENT_ID` set.
pub fn client_id() -> Option<&'static str> {
    option_env!("MEETILY_GOOGLE_CLIENT_ID").filter(|v| !v.is_empty())
}

/// Resolves the client_secret from build-time env. `None` when the
/// binary was built without `MEETILY_GOOGLE_CLIENT_SECRET` set.
pub fn client_secret() -> Option<&'static str> {
    option_env!("MEETILY_GOOGLE_CLIENT_SECRET").filter(|v| !v.is_empty())
}
