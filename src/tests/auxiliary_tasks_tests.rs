use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[tokio::test]
async fn session_close_cancels_waits_and_invalidates_old_permits() {
    let registry = AuxiliaryTaskRegistry::new(true, true);
    let permit = registry
        .permit("Aux-Lifecycle", AuxiliaryTaskKind::Memory)
        .expect("initial permit");
    let exited = Arc::new(AtomicBool::new(false));
    let exited_task = Arc::clone(&exited);
    let task = registry
        .spawn(permit.clone(), move |cancel| async move {
            cancel.cancelled().await;
            exited_task.store(true, Ordering::Release);
        })
        .expect("registered task");

    let closure = registry.begin_session_close("Aux-Lifecycle").await;
    assert!(exited.load(Ordering::Acquire));
    task.wait().await.expect("task supervisor result");
    assert_eq!(registry.task_count(), 0);
    assert!(registry.spawn(permit, |_| async {}).is_err());
    closure.commit();
    assert!(
        registry
            .permit("Aux-Lifecycle", AuxiliaryTaskKind::Memory)
            .is_err()
    );

    registry.activate_session("Aux-Lifecycle");
    let replacement = registry
        .permit("Aux-Lifecycle", AuxiliaryTaskKind::Memory)
        .expect("replacement lifetime permit");
    registry
        .spawn(replacement, |_| async {})
        .expect("replacement task")
        .wait()
        .await
        .expect("replacement task result");
}

#[cfg(windows)]
#[tokio::test]
async fn windows_session_aliases_share_cancellation_and_reactivation() {
    let registry = AuxiliaryTaskRegistry::new(true, true);
    let permit = registry
        .permit("Aux-Alias", AuxiliaryTaskKind::Memory)
        .unwrap();
    let exited = Arc::new(AtomicBool::new(false));
    let task_exited = exited.clone();
    let task = registry
        .spawn(permit.clone(), move |cancel| async move {
            cancel.cancelled().await;
            task_exited.store(true, Ordering::Release);
        })
        .unwrap();
    registry.begin_session_close("aux-alias").await.commit();
    task.wait().await.unwrap();
    assert!(exited.load(Ordering::Acquire));
    assert!(
        registry
            .permit("AUX-ALIAS", AuxiliaryTaskKind::Memory)
            .is_err()
    );
    registry.activate_session("AUX-ALIAS");
    assert!(registry.spawn(permit, |_| async {}).is_err());
    let current = registry
        .permit("aux-alias", AuxiliaryTaskKind::Memory)
        .unwrap();
    registry
        .spawn(current, |_| async {})
        .unwrap()
        .wait()
        .await
        .unwrap();
}

#[cfg(not(windows))]
#[tokio::test]
async fn differently_cased_sessions_keep_independent_lifetimes() {
    let registry = AuxiliaryTaskRegistry::new(true, true);
    let upper = registry
        .permit("Aux-Isolated", AuxiliaryTaskKind::Memory)
        .unwrap();
    let lower = registry
        .permit("aux-isolated", AuxiliaryTaskKind::Memory)
        .unwrap();
    let upper_exited = Arc::new(AtomicBool::new(false));
    let upper_task_exited = upper_exited.clone();
    let upper_task = registry
        .spawn(upper.clone(), move |cancel| async move {
            cancel.cancelled().await;
            upper_task_exited.store(true, Ordering::Release);
        })
        .unwrap();
    let lower_task = registry
        .spawn(lower, |cancel| async move {
            cancel.cancelled().await;
        })
        .unwrap();
    registry.begin_session_close("aux-isolated").await.commit();
    lower_task.wait().await.unwrap();
    assert!(!upper_exited.load(Ordering::Acquire));
    assert!(
        registry
            .permit("Aux-Isolated", AuxiliaryTaskKind::Memory)
            .is_ok()
    );
    assert!(
        registry
            .permit("aux-isolated", AuxiliaryTaskKind::Memory)
            .is_err()
    );
    registry.activate_session("aux-isolated");
    registry
        .spawn(upper, |_| async {})
        .unwrap()
        .wait()
        .await
        .unwrap();
    registry.begin_session_close("Aux-Isolated").await.commit();
    upper_task.wait().await.unwrap();
    assert!(upper_exited.load(Ordering::Acquire));
    assert!(
        registry
            .permit("aux-isolated", AuxiliaryTaskKind::Memory)
            .is_ok()
    );
}

#[tokio::test]
async fn feature_cycle_rejects_queued_work_from_before_disable() {
    let registry = AuxiliaryTaskRegistry::new(true, true);
    let stale = registry
        .permit("feature-cycle", AuxiliaryTaskKind::Reflection)
        .expect("initial permit");

    registry
        .disable_kind_and_wait(AuxiliaryTaskKind::Reflection)
        .await;
    assert!(registry.spawn(stale, |_| async {}).is_err());
    assert!(registry.enable_kind(AuxiliaryTaskKind::Reflection));

    let current = registry
        .permit("feature-cycle", AuxiliaryTaskKind::Reflection)
        .expect("new feature-cycle permit");
    registry
        .spawn(current, |_| async {})
        .expect("current feature-cycle task")
        .wait()
        .await
        .expect("current feature-cycle result");
}

#[tokio::test]
async fn failed_session_close_reopens_only_the_same_lifetime() {
    let registry = AuxiliaryTaskRegistry::new(true, true);
    let closure = registry.begin_session_close("reopen-session").await;
    assert!(closure.still_owns_closed_lifetime());
    drop(closure);
    assert!(
        registry
            .permit("reopen-session", AuxiliaryTaskKind::Memory)
            .is_ok()
    );
}

#[tokio::test]
async fn session_close_wins_private_write_authorization_before_write_starts() {
    let registry = AuxiliaryTaskRegistry::new(true, true);
    let permit = registry
        .permit("write-close", AuxiliaryTaskKind::Memory)
        .expect("initial permit");
    let reached = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let authorized = Arc::new(AtomicBool::new(true));
    let task = registry
        .spawn(permit, {
            let reached = Arc::clone(&reached);
            let release = Arc::clone(&release);
            let authorized = Arc::clone(&authorized);
            move |context| async move {
                reached.notify_one();
                release.notified().await;
                authorized.store(context.begin_private_write().is_some(), Ordering::Release);
            }
        })
        .expect("registered task");
    reached.notified().await;

    let close_registry = registry.clone();
    let mut closing =
        tokio::spawn(async move { close_registry.begin_session_close("write-close").await });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if registry
                .permit("write-close", AuxiliaryTaskKind::Memory)
                .is_err()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("close should invalidate the allocation");
    assert!(
        !closing.is_finished(),
        "close must supervise the registered task"
    );

    release.notify_one();
    task.wait().await.expect("task should finish");
    let closure = (&mut closing).await.expect("close should join");
    assert!(!authorized.load(Ordering::Acquire));
    drop(closure);
}

#[tokio::test]
async fn authorized_private_write_is_drained_before_close_returns() {
    let registry = AuxiliaryTaskRegistry::new(true, true);
    let permit = registry
        .permit("write-started", AuxiliaryTaskKind::Memory)
        .expect("initial permit");
    let authorized = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let finished = Arc::new(AtomicBool::new(false));
    let task = registry
        .spawn(permit, {
            let authorized = Arc::clone(&authorized);
            let release = Arc::clone(&release);
            let finished = Arc::clone(&finished);
            move |context| async move {
                let _write_permit = context
                    .begin_private_write()
                    .expect("the live task should authorize its private write");
                authorized.notify_one();
                release.notified().await;
                finished.store(true, Ordering::Release);
            }
        })
        .expect("registered task");
    authorized.notified().await;

    let close_registry = registry.clone();
    let mut closing =
        tokio::spawn(async move { close_registry.begin_session_close("write-started").await });
    tokio::task::yield_now().await;
    assert!(
        !closing.is_finished(),
        "close must wait for an authorized write"
    );

    release.notify_one();
    task.wait().await.expect("task should finish");
    let closure = (&mut closing).await.expect("close should join");
    assert!(finished.load(Ordering::Acquire));
    drop(closure);
}

#[tokio::test]
async fn stale_feature_cycle_cannot_authorize_private_write_after_reenable() {
    let registry = AuxiliaryTaskRegistry::new(true, true);
    let permit = registry
        .permit("write-feature", AuxiliaryTaskKind::Reflection)
        .expect("initial permit");
    let reached = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let authorized = Arc::new(AtomicBool::new(true));
    let task = registry
        .spawn(permit, {
            let reached = Arc::clone(&reached);
            let release = Arc::clone(&release);
            let authorized = Arc::clone(&authorized);
            move |context| async move {
                reached.notify_one();
                release.notified().await;
                authorized.store(context.begin_private_write().is_some(), Ordering::Release);
            }
        })
        .expect("registered task");
    reached.notified().await;

    let disable_registry = registry.clone();
    let mut disabling = tokio::spawn(async move {
        disable_registry
            .disable_kind_and_wait(AuxiliaryTaskKind::Reflection)
            .await;
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while registry
            .permit("write-feature", AuxiliaryTaskKind::Reflection)
            .is_ok()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("disable should invalidate the feature cycle");
    assert!(registry.enable_kind(AuxiliaryTaskKind::Reflection));

    release.notify_one();
    task.wait().await.expect("task should finish");
    (&mut disabling).await.expect("disable should join");
    assert!(!authorized.load(Ordering::Acquire));
}

#[tokio::test]
async fn shutdown_rejects_not_yet_started_private_write_and_drains_task() {
    let registry = AuxiliaryTaskRegistry::new(true, true);
    let permit = registry
        .permit("write-shutdown", AuxiliaryTaskKind::Reflection)
        .expect("initial permit");
    let cancelled = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let authorized = Arc::new(AtomicBool::new(true));
    let task = registry
        .spawn(permit, {
            let cancelled = Arc::clone(&cancelled);
            let release = Arc::clone(&release);
            let authorized = Arc::clone(&authorized);
            move |context| async move {
                context.cancelled().await;
                cancelled.notify_one();
                release.notified().await;
                authorized.store(context.begin_private_write().is_some(), Ordering::Release);
            }
        })
        .expect("registered task");
    let shutdown_registry = registry.clone();
    let mut shutdown = tokio::spawn(async move { shutdown_registry.shutdown_and_wait().await });
    cancelled.notified().await;
    assert!(!shutdown.is_finished(), "shutdown must drain the task");
    release.notify_one();
    task.wait().await.expect("task should finish");
    (&mut shutdown).await.expect("shutdown should join");
    assert!(!authorized.load(Ordering::Acquire));
    assert_eq!(registry.task_count(), 0);
}
