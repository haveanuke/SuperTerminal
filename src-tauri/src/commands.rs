//! Thin `#[tauri::command]` wrappers over `superterminal_core`.
//!
//! All logic lives in the core crate (shared with the native app); this file
//! only adapts it to Tauri's managed state and IPC layer.

use superterminal_core::buddy::{self, BuddyRequest, BuddyResult};
use superterminal_core::git::actions::{self, ActionKind, ActionResult};
use superterminal_core::git::graph::{self, GraphData};
use superterminal_core::git::network;
use superterminal_core::git::status::StatusReport;
use superterminal_core::git::{self, GitState, RepoInfo};
use superterminal_core::session::SessionManager;
use tauri::State;

#[tauri::command]
pub fn git_resolve_repo(cwd: String, state: State<'_, GitState>) -> Option<RepoInfo> {
    state.resolve(&cwd)
}

#[tauri::command]
pub fn git_status(repo_id: String, state: State<'_, GitState>) -> Result<StatusReport, String> {
    git::status_guarded(&state, &repo_id)
}

#[tauri::command]
pub fn git_stage(
    repo_id: String,
    paths: Vec<String>,
    state: State<'_, GitState>,
) -> Result<ActionResult, String> {
    actions::run_action(&state, &repo_id, Some(paths), ActionKind::Stage)
}

#[tauri::command]
pub fn git_stage_all(repo_id: String, state: State<'_, GitState>) -> Result<ActionResult, String> {
    actions::run_action(&state, &repo_id, None, ActionKind::Stage)
}

#[tauri::command]
pub fn git_unstage(
    repo_id: String,
    paths: Vec<String>,
    state: State<'_, GitState>,
) -> Result<ActionResult, String> {
    actions::run_action(&state, &repo_id, Some(paths), ActionKind::Unstage)
}

#[tauri::command]
pub fn git_unstage_all(
    repo_id: String,
    state: State<'_, GitState>,
) -> Result<ActionResult, String> {
    actions::run_action(&state, &repo_id, None, ActionKind::Unstage)
}

#[tauri::command]
pub fn git_discard(
    repo_id: String,
    paths: Vec<String>,
    state: State<'_, GitState>,
) -> Result<ActionResult, String> {
    actions::run_action(&state, &repo_id, Some(paths), ActionKind::Discard)
}

#[tauri::command]
pub fn git_commit(
    repo_id: String,
    message: String,
    state: State<'_, GitState>,
) -> Result<ActionResult, String> {
    actions::run_commit(&state, &repo_id, &message)
}

#[tauri::command]
pub fn git_push(
    repo_id: String,
    set_upstream: bool,
    state: State<'_, GitState>,
) -> Result<ActionResult, String> {
    network::run_push(&state, &repo_id, set_upstream)
}

#[tauri::command]
pub fn git_pull(repo_id: String, state: State<'_, GitState>) -> Result<ActionResult, String> {
    network::run_pull(&state, &repo_id)
}

#[tauri::command]
pub fn git_fetch(repo_id: String, state: State<'_, GitState>) -> Result<ActionResult, String> {
    network::run_fetch(&state, &repo_id)
}

#[tauri::command]
pub fn git_graph(
    repo_id: String,
    limit: u32,
    state: State<'_, GitState>,
) -> Result<GraphData, String> {
    graph::run_graph(&state, &repo_id, limit)
}

#[tauri::command]
pub fn session_save(
    name: String,
    layout: serde_json::Value,
    state: State<'_, SessionManager>,
) -> Result<bool, String> {
    state.save(&name, &layout)
}

#[tauri::command]
pub fn session_load(
    name: String,
    state: State<'_, SessionManager>,
) -> Result<Option<serde_json::Value>, String> {
    state.load(&name)
}

#[tauri::command]
pub fn session_list(state: State<'_, SessionManager>) -> Vec<String> {
    state.list()
}

#[tauri::command]
pub fn session_delete(name: String, state: State<'_, SessionManager>) -> bool {
    state.delete(&name)
}

#[tauri::command]
pub fn buddy_react(req: BuddyRequest) -> BuddyResult {
    buddy::run(req)
}
