//! Shared backend logic for SuperTerminal.
//!
//! Consumed by both the Tauri app (`src-tauri`, via thin `#[tauri::command]`
//! wrappers) and the native gpui app (`native`). Everything here is free of
//! UI-framework dependencies.

pub mod buddy;
pub mod buddy_gate;
pub mod buddy_probe;
pub mod cue;
pub mod git;
pub mod proc_cwd;
pub mod session;
pub mod shell_env;
