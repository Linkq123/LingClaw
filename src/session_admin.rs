use std::collections::HashMap;

use crate::session_store::build_global_today_usage;
use crate::{AppState, MAIN_SESSION_ID};

pub(crate) async fn gather_global_today_usage(state: &AppState) -> String {
    let mut sessions_snapshot = HashMap::new();
    if let Some(session) = state.sessions.lock().await.get(MAIN_SESSION_ID).cloned() {
        sessions_snapshot.insert(session.id.clone(), session);
    }
    build_global_today_usage(&sessions_snapshot)
}
