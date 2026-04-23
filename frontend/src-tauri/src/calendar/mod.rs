//! Google Calendar integration scaffolding.
//!
//! OAuth flow, provider trait, token store, and event list implementation
//! land in the follow-up PR. See
//! `docs/plans/2026-04-22-feat-google-calendar-pr1-login-and-events.md`
//! for the intended shape.

pub mod commands;
pub mod credentials;
pub mod oauth;
pub mod token_store;
pub mod types;

pub use commands::*;
pub use types::*;
