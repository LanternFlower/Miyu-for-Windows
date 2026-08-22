//! 并发闸门与限流。

use crate::platforms::*;
use super::shared::*;

#[test]
fn rate_window_allows_then_drops_with_single_notice() {
    let mut window = RateWindow::new();
    let start = Instant::now();
    let limit = PlatformRateLimit {
        max_messages: 3,
        window_seconds: 60,
    };
    for _ in 0..3 {
        assert_eq!(
            window.check_at(start, "group:1", limit),
            RateDecision::Allow
        );
    }
    assert_eq!(
        window.check_at(start, "group:1", limit),
        RateDecision::DropWithNotice
    );
    assert_eq!(
        window.check_at(start, "group:1", limit),
        RateDecision::DropSilently
    );
    // Another conversation is unaffected by the first group's quota.
    assert_eq!(
        window.check_at(start, "group:2", limit),
        RateDecision::Allow
    );
    // The window resets after a minute.
    let later = start + Duration::from_secs(61);
    assert_eq!(
        window.check_at(later, "group:1", limit),
        RateDecision::Allow
    );
}

#[test]
fn rate_availability_preflight_never_consumes_quota() {
    let mut window = RateWindow::new();
    let start = Instant::now();
    let limit = PlatformRateLimit {
        max_messages: 1,
        window_seconds: 60,
    };
    assert!(window.available_at(start, "group:1", limit));
    assert!(window.available_at(start, "group:1", limit));
    assert_eq!(
        window.check_at(start, "group:1", limit),
        RateDecision::Allow
    );
    assert!(!window.available_at(start, "group:1", limit));
    assert_eq!(
        window.check_at(start, "group:1", limit),
        RateDecision::DropWithNotice
    );
}

#[test]
fn rate_windows_are_independent_and_support_three_minute_quotas() {
    let mut window = RateWindow::new();
    let start = Instant::now();
    let limit = PlatformRateLimit {
        max_messages: 1,
        window_seconds: 180,
    };
    assert_eq!(
        window.check_at(start, "private:1", limit),
        RateDecision::Allow
    );
    assert_eq!(
        window.check_at(start + Duration::from_secs(30), "private:2", limit),
        RateDecision::Allow
    );
    assert_eq!(
        window.check_at(start + Duration::from_secs(179), "private:1", limit),
        RateDecision::DropWithNotice
    );
    assert_eq!(
        window.check_at(start + Duration::from_secs(180), "private:1", limit),
        RateDecision::Allow
    );
}

#[test]
fn rate_window_zero_is_unlimited() {
    let mut unlimited = RateWindow::new();
    let start = Instant::now();
    let limit = PlatformRateLimit::default();
    for i in 0..100 {
        assert_eq!(
            unlimited.check_at(start, &format!("group:{i}"), limit),
            RateDecision::Allow
        );
    }
}

#[tokio::test]
async fn session_turns_are_fifo_and_lock_entries_are_reclaimed() {
    let runtime = PlatformRuntime::new().unwrap();
    let limits = PlatformSessionLimits {
        running: 1,
        queued: 2,
    };
    let first = runtime
        .acquire_session_turn("session-a", limits)
        .await
        .unwrap();
    let (order_tx, mut order_rx) = tokio::sync::mpsc::unbounded_channel();

    let second_runtime = runtime.clone();
    let second_tx = order_tx.clone();
    let second = tokio::spawn(async move {
        let _lease = second_runtime
            .acquire_session_turn("session-a", limits)
            .await
            .unwrap();
        second_tx.send(2).unwrap();
    });
    while runtime
        .session_turn_locks
        .lock()
        .unwrap()
        .get("session-a")
        .map(Weak::strong_count)
        .unwrap_or(0)
        < 2
    {
        tokio::task::yield_now().await;
    }

    let third_runtime = runtime.clone();
    let third = tokio::spawn(async move {
        let _lease = third_runtime
            .acquire_session_turn("session-a", limits)
            .await
            .unwrap();
        order_tx.send(3).unwrap();
    });
    while runtime
        .session_turn_locks
        .lock()
        .unwrap()
        .get("session-a")
        .map(Weak::strong_count)
        .unwrap_or(0)
        < 3
    {
        tokio::task::yield_now().await;
    }

    drop(first);
    assert_eq!(order_rx.recv().await, Some(2));
    assert_eq!(order_rx.recv().await, Some(3));
    second.await.unwrap();
    third.await.unwrap();
    assert!(runtime.session_turn_locks.lock().unwrap().is_empty());
}

#[tokio::test]
async fn session_turn_limits_bound_running_and_waiting_work() {
    let runtime = PlatformRuntime::new().unwrap();
    let limits = PlatformSessionLimits {
        running: 4,
        queued: 8,
    };
    let mut running = Vec::new();
    for _ in 0..4 {
        running.push(
            runtime
                .acquire_session_turn("bounded", limits)
                .await
                .unwrap(),
        );
    }
    let mut queued = Vec::new();
    for _ in 0..8 {
        let runtime = runtime.clone();
        queued.push(tokio::spawn(async move {
            runtime
                .acquire_session_turn("bounded", limits)
                .await
                .unwrap()
        }));
    }
    loop {
        let waiting = runtime
            .session_turn_locks
            .lock()
            .unwrap()
            .get("bounded")
            .and_then(Weak::upgrade)
            .map(|state| state.waiting.load(Ordering::Acquire))
            .unwrap_or_default();
        if waiting == 8 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(matches!(
        runtime.acquire_session_turn("bounded", limits).await,
        Err(SessionTurnAcquireError::Full)
    ));
    drop(running);
    for task in queued {
        drop(task.await.unwrap());
    }
    assert!(runtime.session_turn_locks.lock().unwrap().is_empty());
}

#[tokio::test]
async fn session_preemption_invalidates_old_waiters_but_not_new_arrivals() {
    let runtime = PlatformRuntime::new().unwrap();
    let limits = PlatformSessionLimits {
        running: 1,
        queued: 8,
    };
    let first = runtime
        .acquire_session_turn("session-a", limits)
        .await
        .unwrap();
    let old_ticket = runtime.session_turn_ticket("session-a", limits);
    let (order_tx, mut order_rx) = tokio::sync::mpsc::unbounded_channel();
    let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel();

    let old_tx = order_tx.clone();
    let old_started = started_tx.clone();
    let old = tokio::spawn(async move {
        old_started.send("old").unwrap();
        let lease = old_ticket.acquire().await.unwrap();
        old_tx.send(("old", lease.is_valid())).unwrap();
    });
    assert_eq!(started_rx.recv().await, Some("old"));

    let command_ticket = runtime.preempt_session_turns("session-a");
    assert!(!first.is_valid());
    let command_tx = order_tx.clone();
    let command_started = started_tx.clone();
    let command = tokio::spawn(async move {
        command_started.send("command").unwrap();
        let lease = command_ticket.acquire().await.unwrap();
        command_tx.send(("command", lease.is_valid())).unwrap();
    });
    assert_eq!(started_rx.recv().await, Some("command"));

    let new_ticket = runtime.session_turn_ticket("session-a", limits);
    let new = tokio::spawn(async move {
        let lease = new_ticket.acquire().await.unwrap();
        order_tx.send(("new", lease.is_valid())).unwrap();
    });

    drop(first);
    assert_eq!(order_rx.recv().await, Some(("command", true)));
    assert_eq!(order_rx.recv().await, Some(("old", false)));
    assert_eq!(order_rx.recv().await, Some(("new", true)));
    old.await.unwrap();
    command.await.unwrap();
    new.await.unwrap();
    assert!(runtime.session_turn_locks.lock().unwrap().is_empty());
}

#[tokio::test]
async fn different_platform_sessions_do_not_block_each_other() {
    let runtime = PlatformRuntime::new().unwrap();
    let limits = PlatformSessionLimits {
        running: 1,
        queued: 1,
    };
    let _first = runtime
        .acquire_session_turn("session-a", limits)
        .await
        .unwrap();
    let independent = tokio::time::timeout(
        Duration::from_secs(1),
        runtime.acquire_session_turn("session-b", limits),
    )
    .await;
    assert!(independent.is_ok());
}

#[tokio::test]
async fn running_platform_turn_does_not_block_an_independent_dispatch() {
    let daemon_temp = tempfile::tempdir().unwrap();
    let state = DaemonState::for_test(test_paths(daemon_temp.path()), 8300).unwrap();
    let session = state
        .state_store
        .create_session("miyu", "queued platform test", "user", None)
        .unwrap();
    state
        .state_store
        .pinned(&session.session_id)
        .start_turn("running-platform-turn", "first", std::process::id())
        .unwrap();

    let error = match run_platform_turn(
        &state,
        Arc::from(session.session_id.as_str()),
        "must stay separate".to_string(),
        Vec::new(),
        TurnProfile::default(),
    )
    .await
    {
        Err(error) => error,
        Ok(_) => panic!("independent platform turn should reach the unavailable worker"),
    };

    assert!(error.to_string().contains("worker is unavailable"));
    assert!(state
        .state_store
        .pinned(&session.session_id)
        .load_queued_prompts()
        .unwrap()
        .is_empty());
}
