use crate::AppState;
use crate::session_store::build_global_today_usage_totals;

#[cfg(test)]
use crate::session_store::{build_global_today_usage, load_saved_sessions_not_in};

pub(crate) fn build_global_today_usage_from_parts(
    loaded_input: u64,
    loaded_output: u64,
    persisted: Option<(u64, u64)>,
) -> String {
    let (persisted_input, persisted_output) = persisted.unwrap_or_default();
    let mut report = build_global_today_usage_totals(
        loaded_input.saturating_add(persisted_input),
        loaded_output.saturating_add(persisted_output),
    );
    if persisted.is_none() {
        report.push_str(
            "\n\tdata_status: partial\n\twarning: persisted Session usage unavailable; totals include loaded Sessions only",
        );
    }
    report
}

pub(crate) async fn gather_global_today_usage(state: &AppState) -> String {
    #[cfg(not(test))]
    {
        let today = crate::prompts::current_local_snapshot().today();
        let (loaded_ids, loaded_input, loaded_output) = {
            let sessions = state.sessions.lock().await;
            let loaded_ids = sessions
                .keys()
                .cloned()
                .collect::<std::collections::HashSet<_>>();
            let (input, output) = crate::accumulate_daily_token_usage(sessions.values());
            (loaded_ids, input, output)
        };
        let persisted = match crate::storage::Database::global() {
            Ok(database) => database
                .current_usage_excluding(&today, &loaded_ids)
                .await
                .map(Some)
                .unwrap_or_else(|error| {
                    eprintln!("WARNING: Failed to aggregate persisted daily usage: {error}");
                    None
                }),
            Err(error) => {
                eprintln!("WARNING: Failed to access persisted daily usage: {error}");
                None
            }
        };
        build_global_today_usage_from_parts(loaded_input, loaded_output, persisted)
    }

    #[cfg(test)]
    {
        let sessions_guard = state.sessions.lock().await;
        let loaded_ids = sessions_guard
            .keys()
            .cloned()
            .collect::<std::collections::HashSet<_>>();
        let saved_sessions = load_saved_sessions_not_in(&loaded_ids);
        build_global_today_usage(sessions_guard.values().chain(saved_sessions.iter()))
    }
}
