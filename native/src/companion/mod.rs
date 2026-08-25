//! Phone companion over Tailscale: an observer-pattern embedded server.
//!
//! Spec: docs/superpowers/specs/2026-08-24-phone-companion-design.md
//! Panes publish snapshots into a shared hub; a plain-thread HTTP/SSE server
//! bound only to the tailnet address serves one embedded page. Authorization
//! is a capability token carried in the bookmark's URL fragment — tailnet
//! membership authenticates devices, the token authenticates the page.

pub mod auth;
pub mod blender;
mod e2e_tests;
pub mod http;
pub mod hub;
pub mod input;
pub mod net;
pub mod previews;
pub mod qr;
pub mod server;
pub mod thumbs;
pub mod wire;
