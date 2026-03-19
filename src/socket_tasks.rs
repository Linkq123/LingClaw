use std::{sync::Arc, time::Duration};

use serde_json::json;
use tokio::{
    sync::{mpsc, Mutex},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use crate::{
    session_store::{save_session_to_disk, trim_incomplete_tool_calls},
    ws_try_send, AppState, LiveTx, WsTx,
};

pub(crate) struct SocketTaskHandles {
    pub(crate) live_dispatcher: JoinHandle<()>,
    pub(crate) disconnect_watcher: JoinHandle<()>,
    pub(crate) avatar_poller: JoinHandle<()>,
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
    tx: WsTx,
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
            super::dispatch_live_event(&live_state, &session_id, event).await;
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

    let poll_cancel = connection_cancel.clone();
    let poll_state = state;
    let poll_session_ref = current_session_ref;
    let avatar_poller = tokio::spawn(async move {
        let mut avatar_poll = tokio::time::interval(Duration::from_secs(1));
        avatar_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                _ = poll_cancel.cancelled() => break,
                _ = avatar_poll.tick() => {
                    let session_id = {
                        let guard = poll_session_ref.lock().await;
                        guard.clone()
                    };
                    if let Some(avatar) = super::detect_session_avatar_update(&session_id, &poll_state).await {
                        if ws_try_send(&tx, &json!({"type":"avatar_update","avatar":avatar,"session_id":&session_id})) {
                            super::commit_session_avatar(&session_id, avatar, &poll_state).await;
                        }
                    }
                }
            }
        }
    });

    (
        live_tx,
        SocketTaskHandles {
            live_dispatcher,
            disconnect_watcher,
            avatar_poller,
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
                if !has_active_connection {
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
        if !has_active_connection {
            let mut sessions = state.sessions.lock().await;
            sessions.remove(session_id);
        }
    }

    state.live_rounds.lock().await.remove(session_id);

    drop(cleanup.tx);
    drop(cleanup.live_tx);

    let _ = cleanup.tasks.disconnect_watcher.await;
    let _ = cleanup.tasks.live_dispatcher.await;
    let _ = cleanup.tasks.avatar_poller.await;
    let _ = cleanup.reader.await;
    let _ = cleanup.writer.await;
}
