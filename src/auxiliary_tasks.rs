use std::{
    collections::{HashMap, HashSet},
    future::Future,
    ops::Deref,
    sync::{Arc, Mutex},
};

use tokio::sync::{Notify, oneshot};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum AuxiliaryTaskKind {
    Memory,
    Reflection,
}

impl AuxiliaryTaskKind {
    fn label(self) -> &'static str {
        match self {
            Self::Memory => "Memory",
            Self::Reflection => "Reflection",
        }
    }
}

#[derive(Clone)]
pub(crate) struct AuxiliaryTaskPermit {
    session_key: String,
    session_lifetime: Arc<()>,
    kind: AuxiliaryTaskKind,
    kind_lifetime: Arc<()>,
}

/// Runtime-owned context for one supervised auxiliary task.
///
/// `begin_private_write` is the linearization point between a task that is
/// about to mutate Session-private files and lifecycle close/feature-disable/
/// shutdown. If the write wins, the task remains registered until the write
/// finishes, so close waits for it. If lifecycle cancellation wins, the write
/// is rejected before it starts.
#[derive(Clone)]
pub(crate) struct AuxiliaryTaskContext {
    registry: AuxiliaryTaskRegistry,
    task_id: u64,
    permit: AuxiliaryTaskPermit,
    cancel: CancellationToken,
}

impl AuxiliaryTaskContext {
    pub(crate) fn cancellation_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    pub(crate) fn begin_private_write(&self) -> Option<AuxiliaryPrivateWritePermit> {
        let state = self.registry.lock_state();
        if !state.accepting
            || state.disabled_kinds.contains(&self.permit.kind)
            || self.cancel.is_cancelled()
        {
            return None;
        }
        let task_is_current = state.tasks.get(&self.task_id).is_some_and(|entry| {
            entry.session_key == self.permit.session_key
                && entry.kind == self.permit.kind
                && Arc::ptr_eq(&entry.session_lifetime, &self.permit.session_lifetime)
        });
        let session_is_current =
            state
                .sessions
                .get(&self.permit.session_key)
                .is_some_and(|current| {
                    current.open && Arc::ptr_eq(&current.identity, &self.permit.session_lifetime)
                });
        let kind_is_current = state
            .kind_lifetimes
            .get(&self.permit.kind)
            .is_some_and(|current| Arc::ptr_eq(current, &self.permit.kind_lifetime));
        (task_is_current && session_is_current && kind_is_current).then(|| {
            AuxiliaryPrivateWritePermit {
                _registry: self.registry.clone(),
                _task_id: self.task_id,
            }
        })
    }
}

/// Proof that a private-file operation won the lifecycle race. The permit is
/// kept for the complete filesystem operation; the owning task remains in the
/// registry, so teardown cannot return until the operation drops it and exits.
pub(crate) struct AuxiliaryPrivateWritePermit {
    _registry: AuxiliaryTaskRegistry,
    _task_id: u64,
}

impl Deref for AuxiliaryTaskContext {
    type Target = CancellationToken;

    fn deref(&self) -> &Self::Target {
        &self.cancel
    }
}

#[derive(Clone)]
struct SessionLifetime {
    identity: Arc<()>,
    open: bool,
}

struct AuxiliaryTaskEntry {
    session_key: String,
    session_lifetime: Arc<()>,
    kind: AuxiliaryTaskKind,
    cancel: CancellationToken,
}

struct AuxiliaryTaskState {
    accepting: bool,
    disabled_kinds: HashSet<AuxiliaryTaskKind>,
    kind_lifetimes: HashMap<AuxiliaryTaskKind, Arc<()>>,
    sessions: HashMap<String, SessionLifetime>,
    tasks: HashMap<u64, AuxiliaryTaskEntry>,
    next_task_id: u64,
}

impl AuxiliaryTaskState {
    fn new(memory_enabled: bool, reflection_enabled: bool) -> Self {
        let mut disabled_kinds = HashSet::new();
        if !memory_enabled {
            disabled_kinds.insert(AuxiliaryTaskKind::Memory);
        }
        if !reflection_enabled {
            disabled_kinds.insert(AuxiliaryTaskKind::Reflection);
        }
        Self {
            accepting: true,
            disabled_kinds,
            kind_lifetimes: HashMap::from([
                (AuxiliaryTaskKind::Memory, Arc::new(())),
                (AuxiliaryTaskKind::Reflection, Arc::new(())),
            ]),
            sessions: HashMap::new(),
            tasks: HashMap::new(),
            next_task_id: 1,
        }
    }
}

struct AuxiliaryTaskRegistryInner {
    state: Mutex<AuxiliaryTaskState>,
    changed: Notify,
}

/// App-owned registry for every Memory/Reflection request lifetime.
///
/// A permit binds both the Session allocation lifetime and the feature-enable
/// cycle. Closing or recreating either allocation makes queued permits stale,
/// while running tasks remain visible until their supervisor observes exit.
#[derive(Clone)]
pub(crate) struct AuxiliaryTaskRegistry {
    inner: Arc<AuxiliaryTaskRegistryInner>,
}

impl AuxiliaryTaskRegistry {
    pub(crate) fn new(memory_enabled: bool, reflection_enabled: bool) -> Self {
        Self {
            inner: Arc::new(AuxiliaryTaskRegistryInner {
                state: Mutex::new(AuxiliaryTaskState::new(memory_enabled, reflection_enabled)),
                changed: Notify::new(),
            }),
        }
    }

    fn session_key(session_id: &str) -> String {
        if cfg!(windows) {
            session_id.to_lowercase()
        } else {
            session_id.to_string()
        }
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, AuxiliaryTaskState> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(crate) fn permit(
        &self,
        session_id: &str,
        kind: AuxiliaryTaskKind,
    ) -> Result<AuxiliaryTaskPermit, String> {
        let session_key = Self::session_key(session_id);
        let mut state = self.lock_state();
        if !state.accepting {
            return Err("Auxiliary background work is shutting down".to_string());
        }
        if state.disabled_kinds.contains(&kind) {
            return Err(format!("{} background work is disabled", kind.label()));
        }
        let kind_lifetime = state
            .kind_lifetimes
            .get(&kind)
            .cloned()
            .unwrap_or_else(|| Arc::new(()));
        let session_lifetime = state
            .sessions
            .entry(session_key.clone())
            .or_insert_with(|| SessionLifetime {
                identity: Arc::new(()),
                open: true,
            });
        if !session_lifetime.open {
            return Err("Session auxiliary background work is closed".to_string());
        }
        Ok(AuxiliaryTaskPermit {
            session_key,
            session_lifetime: Arc::clone(&session_lifetime.identity),
            kind,
            kind_lifetime,
        })
    }

    pub(crate) fn activate_session(&self, session_id: &str) {
        let session_key = Self::session_key(session_id);
        let mut state = self.lock_state();
        if !state.accepting {
            return;
        }
        match state.sessions.get_mut(&session_key) {
            Some(lifetime) if !lifetime.open => {
                lifetime.identity = Arc::new(());
                lifetime.open = true;
            }
            Some(_) => {}
            None => {
                state.sessions.insert(
                    session_key,
                    SessionLifetime {
                        identity: Arc::new(()),
                        open: true,
                    },
                );
            }
        }
    }

    pub(crate) fn enable_kind(&self, kind: AuxiliaryTaskKind) -> bool {
        let mut state = self.lock_state();
        if !state.accepting {
            return false;
        }
        if !state.disabled_kinds.remove(&kind) {
            return true;
        }
        state.kind_lifetimes.insert(kind, Arc::new(()));
        true
    }

    pub(crate) fn spawn<T, F, Fut>(
        &self,
        permit: AuxiliaryTaskPermit,
        future: F,
    ) -> Result<AuxiliaryTaskJoin<T>, String>
    where
        T: Send + 'static,
        F: FnOnce(AuxiliaryTaskContext) -> Fut + Send + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        let cancel = CancellationToken::new();
        let task_id = {
            let mut state = self.lock_state();
            if !state.accepting || state.disabled_kinds.contains(&permit.kind) {
                return Err("Auxiliary background work is no longer accepting tasks".to_string());
            }
            let kind_is_current = state
                .kind_lifetimes
                .get(&permit.kind)
                .is_some_and(|current| Arc::ptr_eq(current, &permit.kind_lifetime));
            let session_is_current =
                state
                    .sessions
                    .get(&permit.session_key)
                    .is_some_and(|current| {
                        current.open && Arc::ptr_eq(&current.identity, &permit.session_lifetime)
                    });
            if !kind_is_current || !session_is_current {
                return Err("Auxiliary task permit belongs to a stale lifecycle".to_string());
            }
            let task_id = state.next_task_id;
            state.next_task_id = state.next_task_id.saturating_add(1);
            state.tasks.insert(
                task_id,
                AuxiliaryTaskEntry {
                    session_key: permit.session_key.clone(),
                    session_lifetime: Arc::clone(&permit.session_lifetime),
                    kind: permit.kind,
                    cancel: cancel.clone(),
                },
            );
            task_id
        };

        let kind = permit.kind;
        let task_context = AuxiliaryTaskContext {
            registry: self.clone(),
            task_id,
            permit,
            cancel,
        };
        let worker = tokio::spawn(future(task_context));
        let registry = self.clone();
        let (result_tx, result_rx) = oneshot::channel();
        std::mem::drop(tokio::spawn(async move {
            let result = match worker.await {
                Ok(value) => Ok(value),
                Err(error) => {
                    eprintln!(
                        "ERROR: supervised {} background task failed: {error}",
                        kind.label()
                    );
                    Err(format!("{} background task failed: {error}", kind.label()))
                }
            };
            registry.finish_task(task_id);
            let _ = result_tx.send(result);
        }));

        Ok(AuxiliaryTaskJoin { result_rx })
    }

    fn finish_task(&self, task_id: u64) {
        self.lock_state().tasks.remove(&task_id);
        self.inner.changed.notify_waiters();
    }

    pub(crate) async fn begin_session_close(&self, session_id: &str) -> AuxiliarySessionClosure {
        let session_key = Self::session_key(session_id);
        let (session_lifetime, newly_closed, cancels) = {
            let mut state = self.lock_state();
            let lifetime = state
                .sessions
                .entry(session_key.clone())
                .or_insert_with(|| SessionLifetime {
                    identity: Arc::new(()),
                    open: true,
                });
            let newly_closed = lifetime.open;
            lifetime.open = false;
            let session_lifetime = Arc::clone(&lifetime.identity);
            let cancels = state
                .tasks
                .values()
                .filter(|entry| {
                    entry.session_key == session_key
                        && Arc::ptr_eq(&entry.session_lifetime, &session_lifetime)
                })
                .map(|entry| entry.cancel.clone())
                .collect::<Vec<_>>();
            (session_lifetime, newly_closed, cancels)
        };
        for cancel in cancels {
            cancel.cancel();
        }
        self.wait_for_tasks(|entry| {
            entry.session_key == session_key
                && Arc::ptr_eq(&entry.session_lifetime, &session_lifetime)
        })
        .await;
        AuxiliarySessionClosure {
            registry: self.clone(),
            session_key,
            session_lifetime,
            newly_closed,
            committed: false,
        }
    }

    pub(crate) async fn disable_kind_and_wait(&self, kind: AuxiliaryTaskKind) {
        let cancels = {
            let mut state = self.lock_state();
            state.disabled_kinds.insert(kind);
            state
                .tasks
                .values()
                .filter(|entry| entry.kind == kind)
                .map(|entry| entry.cancel.clone())
                .collect::<Vec<_>>()
        };
        for cancel in cancels {
            cancel.cancel();
        }
        self.wait_for_tasks(|entry| entry.kind == kind).await;
    }

    pub(crate) async fn shutdown_and_wait(&self) {
        let cancels = {
            let mut state = self.lock_state();
            state.accepting = false;
            state
                .tasks
                .values()
                .map(|entry| entry.cancel.clone())
                .collect::<Vec<_>>()
        };
        for cancel in cancels {
            cancel.cancel();
        }
        self.wait_for_tasks(|_| true).await;
    }

    async fn wait_for_tasks(&self, predicate: impl Fn(&AuxiliaryTaskEntry) -> bool) {
        loop {
            let changed = self.inner.changed.notified();
            if !self.lock_state().tasks.values().any(&predicate) {
                return;
            }
            changed.await;
        }
    }

    #[cfg(test)]
    pub(crate) fn task_count(&self) -> usize {
        self.lock_state().tasks.len()
    }
}

pub(crate) struct AuxiliaryTaskJoin<T> {
    result_rx: oneshot::Receiver<Result<T, String>>,
}

impl<T> AuxiliaryTaskJoin<T> {
    pub(crate) async fn wait(self) -> Result<T, String> {
        self.result_rx.await.map_err(|_| {
            "Auxiliary task supervisor stopped before reporting a result".to_string()
        })?
    }
}

pub(crate) struct AuxiliarySessionClosure {
    registry: AuxiliaryTaskRegistry,
    session_key: String,
    session_lifetime: Arc<()>,
    newly_closed: bool,
    committed: bool,
}

impl AuxiliarySessionClosure {
    pub(crate) fn still_owns_closed_lifetime(&self) -> bool {
        self.registry
            .lock_state()
            .sessions
            .get(&self.session_key)
            .is_some_and(|current| {
                !current.open && Arc::ptr_eq(&current.identity, &self.session_lifetime)
            })
    }

    pub(crate) fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for AuxiliarySessionClosure {
    fn drop(&mut self) {
        if self.committed || !self.newly_closed {
            return;
        }
        let mut state = self.registry.lock_state();
        let accepting = state.accepting;
        if let Some(current) = state.sessions.get_mut(&self.session_key)
            && !current.open
            && Arc::ptr_eq(&current.identity, &self.session_lifetime)
            && accepting
        {
            current.open = true;
        }
    }
}

#[cfg(test)]
#[path = "tests/auxiliary_tasks_tests.rs"]
mod tests;
