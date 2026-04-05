use std::sync::Arc;

use tokio::{
    sync::{Mutex, mpsc},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use crate::{
    AppState, LiveTx, MAIN_SESSION_ID, WsTx,
    session_store::{save_session_to_disk, trim_incomplete_tool_calls},
};

pub(crate) struct SocketTaskHandles {
    pub(crate) live_dispatcher: JoinHandle<()>,
    pub(crate) disconnect_watcher: JoinHandle<()>,
}

pub(crate) struct ConnectionCleanup {
    pub(crate) tx: WsTx,
    pub(crate) live_tx: LiveTx,
    pub(crate) tasks: SocketTaskHandles,
    pub(crate) reader: JoinHandle<()>,
    pub(crate) writer: JoinHandle<()>,
}

pub(crate) fn spawn_connection_tasks(
    state: Arc<AppState>,
    connection_cancel: CancellationToken,
    current_session_ref: Arc<Mutex<String>>,
    connection_id: u64,
) -> (LiveTx, SocketTaskHandles) {
    let (live_tx, mut live_rx) = mpsc::channel::<serde_json::Value>(256);

    let live_state = state.clone();
    let live_session_ref = current_session_ref.clone();
    let live_dispatcher = tokio::spawn(async move {
        while let Some(event) = live_rx.recv().await {
            let session_id = {
                let guard = live_session_ref.lock().await;
                guard.clone()
            };
            super::dispatch_live_event(&live_state, &session_id, connection_id, event).await;
        }
    });

    let disconnect_state = state.clone();
    let disconnect_session_ref = current_session_ref.clone();
    let disconnect_cancel = connection_cancel.clone();
    let disconnect_watcher = tokio::spawn(async move {
        disconnect_cancel.cancelled().await;
        let session_id = {
            let guard = disconnect_session_ref.lock().await;
            guard.clone()
        };
        super::unbind_session_connection_if_matches(&disconnect_state, &session_id, connection_id)
            .await;
    });

    (
        live_tx,
        SocketTaskHandles {
            live_dispatcher,
            disconnect_watcher,
        },
    )
}

pub(crate) async fn finalize_connection(
    state: &AppState,
    session_id: &str,
    connection_id: u64,
    connection_cancel: &CancellationToken,
    cleanup: ConnectionCleanup,
) {
    connection_cancel.cancel();

    // Clean up connection_cancels entry only if it still belongs to this connection.
    {
        let mut cancels = state.connection_cancels.lock().await;
        if cancels.get(session_id).map(|binding| binding.connection_id) == Some(connection_id) {
            cancels.remove(session_id);
        }
    }

    {
        let mut sessions = state.sessions.lock().await;
        if let Some(session) = sessions.get_mut(session_id) {
            trim_incomplete_tool_calls(&mut session.messages);
        }
    }

    super::unbind_session_connection_if_matches(state, session_id, connection_id).await;

    let snapshot = {
        let sessions = state.sessions.lock().await;
        sessions.get(session_id).cloned()
    };

    if let Some(ref session) = snapshot {
        match save_session_to_disk(session).await {
            Ok(()) => {
                let has_active_connection = state
                    .active_connections
                    .lock()
                    .await
                    .contains_key(session_id);
                if !has_active_connection && session_id != MAIN_SESSION_ID {
                    let mut sessions = state.sessions.lock().await;
                    sessions.remove(session_id);
                }
            }
            Err(error) => {
                eprintln!(
                    "Warning: failed to save session {} on disconnect: {error}; keeping in memory",
                    session.id
                );
            }
        }
    } else {
        let has_active_connection = state
            .active_connections
            .lock()
            .await
            .contains_key(session_id);
        if !has_active_connection && session_id != MAIN_SESSION_ID {
            let mut sessions = state.sessions.lock().await;
            sessions.remove(session_id);
        }
    }

    {
        let mut live_rounds = state.live_rounds.lock().await;
        if live_rounds.get(session_id).map(|r| r.connection_id) == Some(connection_id) {
            live_rounds.remove(session_id);
        }
    }

    drop(cleanup.tx);
    drop(cleanup.live_tx);

    let _ = cleanup.tasks.disconnect_watcher.await;
    let _ = cleanup.tasks.live_dispatcher.await;
    let _ = cleanup.reader.await;
    let _ = cleanup.writer.await;
}
