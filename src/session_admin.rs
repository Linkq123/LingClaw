use crate::AppState;

use crate::session_store::{build_global_today_usage, load_saved_sessions_not_in};

pub(crate) async fn gather_global_today_usage(state: &AppState) -> String {
    let sessions_guard = state.sessions.lock().await;
    let loaded_ids = sessions_guard.keys().cloned().collect::<std::collections::HashSet<_>>();
    let saved_sessions = load_saved_sessions_not_in(&loaded_ids);
    build_global_today_usage(sessions_guard.values().chain(saved_sessions.iter()))
}
