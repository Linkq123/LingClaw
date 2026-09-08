use super::*;
use ratatui::backend::TestBackend;

async fn spawn_test_http_server(router: axum::Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (format!("http://{address}"), task)
}

async fn spawn_protocol_websocket_server(
    initial_payload: Value,
) -> (
    String,
    std::sync::Arc<std::sync::Mutex<Value>>,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
    std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    tokio::task::JoinHandle<()>,
) {
    let payload = std::sync::Arc::new(std::sync::Mutex::new(initial_payload));
    let websocket_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let websocket_targets = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

    let config_payload = payload.clone();
    let websocket_count_for_route = websocket_count.clone();
    let websocket_targets_for_route = websocket_targets.clone();
    let router = axum::Router::new()
        .route(
            "/api/client-config",
            axum::routing::get(move || {
                let payload = config_payload
                    .lock()
                    .expect("protocol test payload lock")
                    .clone();
                async move { axum::Json(payload) }
            }),
        )
        .route(
            "/ws",
            axum::routing::get(
                move |upgrade: axum::extract::ws::WebSocketUpgrade, uri: axum::http::Uri| {
                    websocket_count_for_route.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    websocket_targets_for_route
                        .lock()
                        .expect("protocol test target lock")
                        .push(uri.to_string());
                    async move { upgrade.on_upgrade(|_| async {}) }
                },
            ),
        );
    let (base, task) = spawn_test_http_server(router).await;
    (base, payload, websocket_count, websocket_targets, task)
}

#[tokio::test]
async fn health_probe_rejects_unrelated_successful_services() {
    let unrelated = axum::Router::new().route(
        "/api/health",
        axum::routing::get(|| async { axum::Json(json!({ "status": "ok" })) }),
    );
    let (base, task) = spawn_test_http_server(unrelated).await;
    assert_eq!(
        daemon_health_status(&Client::new(), &base).await,
        DaemonHealth::Unavailable
    );
    task.abort();

    let lingclaw = axum::Router::new().route(
        "/api/health",
        axum::routing::get(|| async {
            axum::Json(json!({
                "service": "lingclaw",
                "status": "ok",
                "version": "0.9.2",
            }))
        }),
    );
    let (base, task) = spawn_test_http_server(lingclaw).await;
    assert_eq!(
        daemon_health_status(&Client::new(), &base).await,
        DaemonHealth::Compatible
    );
    task.abort();
}

#[test]
fn health_probe_rejects_legacy_daemons_that_lack_workspace_capabilities() {
    assert_eq!(
        classify_lingclaw_health_payload(&json!({
            "status": "ok",
            "version": "0.9.0",
            "model_configured": true,
            "sessions": 2,
            "storage": { "mode": "healthy" },
        })),
        DaemonHealth::IncompatibleLegacy
    );
    assert_eq!(
        classify_lingclaw_health_payload(&json!({
            "service": "another-service",
            "status": "ok",
            "version": "0.9.2",
            "model_configured": true,
            "sessions": 2,
            "storage": { "mode": "healthy" },
        })),
        DaemonHealth::Unavailable
    );
}

#[tokio::test]
async fn client_capabilities_negotiate_strict_legacy_and_unsupported_execution_identity() {
    for (payload, expected) in [
        (
            json!({
                "features": {"groups": true},
                "protocols": {"execution_identity": crate::EXECUTION_IDENTITY_PROTOCOL_VERSION}
            }),
            Some(ExecutionIdentityProtocol::Strict),
        ),
        (
            json!({"features": {"groups": false}}),
            Some(ExecutionIdentityProtocol::Legacy),
        ),
        (
            json!({
                "features": {"groups": false},
                "protocols": {"execution_identity": 99}
            }),
            None,
        ),
    ] {
        let router = axum::Router::new().route(
            "/api/client-config",
            axum::routing::get(move || {
                let payload = payload.clone();
                async move { axum::Json(payload) }
            }),
        );
        let (base, task) = spawn_test_http_server(router).await;
        let result = fetch_client_capabilities(&Client::new(), &base).await;
        match expected {
            Some(protocol) => {
                let capabilities = result.expect("supported capability should negotiate");
                assert_eq!(capabilities.execution_identity_protocol, protocol);
                assert_eq!(
                    capabilities.groups_enabled,
                    protocol == ExecutionIdentityProtocol::Strict
                );
            }
            None => assert!(
                result
                    .expect_err("unknown execution identity version must fail")
                    .to_string()
                    .contains("unsupported daemon execution identity protocol")
            ),
        }
        task.abort();
    }
}

#[tokio::test]
async fn every_tui_socket_generation_renegotiates_before_websocket_io() {
    let strict = json!({
        "features": {"groups": true},
        "protocols": {"execution_identity": crate::EXECUTION_IDENTITY_PROTOCOL_VERSION}
    });
    let (base, payload, websocket_count, websocket_targets, task) =
        spawn_protocol_websocket_server(strict.clone()).await;
    let client = build_control_client(Duration::from_secs(1)).unwrap();
    let mut app = test_app(true);

    let mut first = connect_socket_for_app(&client, &base, "main", None, &mut app)
        .await
        .expect("initial strict socket should connect");
    mark_socket_connected(&mut app);
    assert_eq!(websocket_count.load(std::sync::atomic::Ordering::SeqCst), 1);
    let _ = first.close(None).await;

    *payload.lock().unwrap() = json!({"features":{"groups":true}});
    let legacy_error = connect_socket_for_app(&client, &base, "main", None, &mut app)
        .await
        .expect_err("strict to legacy must fail before a second websocket");
    assert!(
        legacy_error
            .to_string()
            .contains("older daemon cannot open a second")
    );
    assert_eq!(websocket_count.load(std::sync::atomic::Ordering::SeqCst), 1);

    *payload.lock().unwrap() = json!({
        "features":{"groups":true},
        "protocols":{"execution_identity":99}
    });
    let unknown_error = connect_socket_for_app(&client, &base, "main", None, &mut app)
        .await
        .expect_err("an unknown protocol must fail before websocket I/O");
    assert!(
        unknown_error
            .to_string()
            .contains("unsupported daemon execution identity")
    );
    assert_eq!(
        app.execution_identity_protocol,
        ExecutionIdentityProtocol::Unavailable
    );
    assert_eq!(websocket_count.load(std::sync::atomic::Ordering::SeqCst), 1);

    *payload.lock().unwrap() = strict.clone();
    let mut group = connect_socket_for_app(&client, &base, "main", Some("review-group"), &mut app)
        .await
        .expect("a fresh strict negotiation may recover and switch targets");
    mark_socket_connected(&mut app);
    assert_eq!(websocket_count.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert!(
        websocket_targets
            .lock()
            .unwrap()
            .last()
            .is_some_and(|target| target.contains("group=review-group"))
    );
    let _ = group.close(None).await;

    let mut upgraded = test_app(true);
    *payload.lock().unwrap() = json!({"features":{"groups":true}});
    let mut legacy = connect_socket_for_app(&client, &base, "main", None, &mut upgraded)
        .await
        .expect("the first legacy socket is compatible");
    mark_socket_connected(&mut upgraded);
    let _ = legacy.close(None).await;
    *payload.lock().unwrap() = strict;
    let mut strict_after_upgrade =
        connect_socket_for_app(&client, &base, "main", None, &mut upgraded)
            .await
            .expect("legacy to strict renegotiation should recover");
    mark_socket_connected(&mut upgraded);
    assert_eq!(
        upgraded.execution_identity_protocol,
        ExecutionIdentityProtocol::Strict
    );
    let _ = strict_after_upgrade.close(None).await;

    task.abort();
    let _ = task.await;
    let websocket_count_before_failure = websocket_count.load(std::sync::atomic::Ordering::SeqCst);
    upgraded.input = "restore after negotiation failure".into();
    let snapshot = ComposerSnapshot::capture(&upgraded);
    upgraded.input.clear();
    upgraded.pending_outbound_write = Some(snapshot);
    let fetch_error = connect_socket_for_app(&client, &base, "main", None, &mut upgraded)
        .await
        .expect_err("capability fetch failure must precede websocket I/O");
    assert!(
        fetch_error
            .to_string()
            .contains("could not negotiate daemon capabilities")
    );
    assert_eq!(upgraded.input, "restore after negotiation failure");
    assert!(upgraded.pending_outbound_write.is_none());
    assert_eq!(
        upgraded.execution_identity_protocol,
        ExecutionIdentityProtocol::Unavailable
    );
    assert_eq!(
        websocket_count.load(std::sync::atomic::Ordering::SeqCst),
        websocket_count_before_failure
    );
}

#[tokio::test]
async fn real_tui_send_keeps_its_snapshot_until_a_valid_start_identity() {
    let router = axum::Router::new()
        .route(
            "/api/client-config",
            axum::routing::get(|| async {
                axum::Json(json!({
                    "features":{"groups":false},
                    "protocols":{"execution_identity":crate::EXECUTION_IDENTITY_PROTOCOL_VERSION}
                }))
            }),
        )
        .route(
            "/ws",
            axum::routing::get(|upgrade: axum::extract::ws::WebSocketUpgrade| async move {
                upgrade.on_upgrade(|mut socket| async move {
                    if socket.recv().await.is_some() {
                        let _ = socket
                            .send(axum::extract::ws::Message::Text(
                                json!({"type":"start","round":1}).to_string().into(),
                            ))
                            .await;
                        let _ = socket.recv().await;
                    }
                })
            }),
        );
    let (base, task) = spawn_test_http_server(router).await;
    let client = build_control_client(Duration::from_secs(1)).unwrap();
    let mut app = test_app(false);
    app.connected = true;
    app.input = "sent over a real websocket".into();
    app.pending_images
        .push(json!({"url":"https://images.example/real-send.png"}));
    app.plan_mode = true;
    let snapshot = ComposerSnapshot::capture(&app);
    let UserAction::Send(payload) =
        handle_composer_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
    else {
        panic!("composer should produce a real outbound payload");
    };

    let mut socket = connect_socket_for_app(&client, &base, "main", None, &mut app)
        .await
        .expect("strict websocket should connect");
    mark_socket_connected(&mut app);
    socket
        .send(Message::Text(payload.into()))
        .await
        .expect("websocket write should succeed");
    acknowledge_outbound_send(&mut app);
    app.pending_outbound_write = Some(snapshot);

    let message = socket
        .next()
        .await
        .expect("server should send identityless start")
        .expect("identityless start frame should decode");
    let Message::Text(text) = message else {
        panic!("expected text start event");
    };
    let value: Value = serde_json::from_str(&text).unwrap();
    let action = apply_socket_event(&mut app, value);
    assert_eq!(action, SocketEventAction::CloseExecutionProtocol);
    let mut socket_slot = Some(socket);
    apply_socket_event_action(action, &client, &mut socket_slot, &base, &mut app).await;

    assert!(socket_slot.is_none());
    assert_eq!(app.input, "sent over a real websocket");
    assert_eq!(app.pending_images.len(), 1);
    assert!(app.plan_mode);
    assert!(app.pending_outbound_write.is_none());
    assert!(!app.outbound_reconnect_pending);
    assert!(!app.busy);
    assert!(!app.connected);
    assert!(
        app.status
            .contains("omitted the required execution identity")
    );

    task.abort();
}

#[tokio::test]
async fn control_client_times_out_a_stalled_daemon_request() {
    let stalled = axum::Router::new().route(
        "/stall",
        axum::routing::get(|| async {
            tokio::time::sleep(Duration::from_secs(60)).await;
            "late"
        }),
    );
    let (base, task) = spawn_test_http_server(stalled).await;
    let client = build_control_client(Duration::from_millis(50)).unwrap();

    let error = client
        .get(format!("{base}/stall"))
        .send()
        .await
        .unwrap_err();

    assert!(error.is_timeout());
    task.abort();
}

#[test]
fn cli_defaults_to_current_directory_and_rejects_conflicting_paths() {
    let parsed = TuiOptions::parse(&["--lang".into(), "en".into()]).unwrap();
    assert!(parsed.path.as_ref().is_some_and(|path| path.is_absolute()));
    assert_eq!(parsed.language, UiLanguage::En);
    assert!(TuiOptions::parse(&["one".into(), "two".into()]).is_err());
    assert!(TuiOptions::parse(&["--port".into(), "0".into()]).is_err());
}

#[test]
fn cli_session_without_path_does_not_resolve_the_current_directory() {
    let parsed = TuiOptions::parse(&["--session".into(), "main".into()]).unwrap();
    assert_eq!(parsed.session.as_deref(), Some("main"));
    assert!(parsed.path.is_none());
}

#[test]
fn automatic_language_uses_locale_precedence_and_platform_fallback() {
    assert_eq!(
        UiLanguage::from_locale_candidates(["zh_CN.UTF-8", "en_US"], Some("en-US")),
        UiLanguage::ZhCn
    );
    assert_eq!(
        UiLanguage::from_locale_candidates(["C", "zh_CN.UTF-8"], Some("zh-CN")),
        UiLanguage::En
    );
    assert_eq!(
        UiLanguage::from_locale_candidates(std::iter::empty(), Some("zh-CN")),
        UiLanguage::ZhCn
    );
    assert_eq!(
        UiLanguage::from_locale_candidates([""], None),
        UiLanguage::En
    );
}

#[test]
fn automatic_theme_understands_common_terminal_hints() {
    assert_eq!(UiTheme::from_colorfgbg("15;0"), Some(UiTheme::Dark));
    assert_eq!(UiTheme::from_colorfgbg("0;15"), Some(UiTheme::Light));
    assert_eq!(UiTheme::from_colorfgbg("15;231"), Some(UiTheme::Light));
    assert_eq!(UiTheme::from_colorfgbg("15;232"), Some(UiTheme::Dark));
    assert_eq!(UiTheme::from_colorfgbg("15;unknown"), None);
    assert_eq!(
        UiTheme::from_theme_hint("background=light"),
        Some(UiTheme::Light)
    );
    assert_eq!(UiTheme::from_theme_hint("DARK"), Some(UiTheme::Dark));
    assert_eq!(UiTheme::from_vscode_theme_kind("4"), Some(UiTheme::Light));
    assert_eq!(UiTheme::from_vscode_theme_kind("2"), Some(UiTheme::Dark));
}

#[test]
fn markdown_renderer_preserves_hashes_inside_fenced_code() {
    let app = test_app(false);
    let chat_line = ChatLine {
        role: "assistant".into(),
        content: "```c\n#include <stdio.h>\n# shell comment\n```\n# Heading\n#not-a-heading".into(),
        style: LineKind::Assistant,
        stream_id: None,
    };
    let rendered = styled_chat_lines(&app, &chat_line);
    let text = rendered
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>();

    assert_eq!(
        text,
        vec![
            "assistant ›",
            "```c",
            "#include <stdio.h>",
            "# shell comment",
            "```",
            "Heading",
            "#not-a-heading",
            "",
        ]
    );
}

fn test_app(groups_enabled: bool) -> App {
    let options = TuiOptions {
        path: Some(PathBuf::from(".")),
        session: None,
        port: 18989,
        language: UiLanguage::En,
        theme: UiTheme::Dark,
    };
    let session = SessionSummary {
        id: "main".into(),
        name: "Main".into(),
        workspace: None,
    };
    App::new(
        &options,
        session.clone(),
        vec![session],
        groups_enabled,
        "fallback".into(),
    )
}

fn image_capable_test_app() -> App {
    let mut app = test_app(false);
    app.connected = true;
    app.socket_generation = 7;
    app.current_model = "provider/vision".into();
    app.current_effort = "medium".into();
    app.current_model_supports_image = true;
    app.model_config_revision = 11;
    app.current_s3_config_id = Some("s3-a".into());
    app
}

#[test]
fn keyboard_contract_supports_backtab_ctrl_j_and_two_stage_ctrl_c() {
    let mut app = test_app(false);
    app.focus = Focus::Composer;
    assert!(matches!(
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)
        ),
        UserAction::None
    ));
    assert_eq!(app.focus, Focus::Content);

    app.focus = Focus::Composer;
    assert!(matches!(
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL)
        ),
        UserAction::None
    ));
    assert_eq!(app.input, "\n");

    app.busy = true;
    assert!(matches!(
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
        ),
        UserAction::Stop(payload) if payload == "/stop"
    ));
    assert!(app.quit_armed);
    assert!(matches!(
        handle_key(&mut app, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)),
        UserAction::None
    ));
    assert!(!app.quit_armed);
    assert!(matches!(
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
        ),
        UserAction::Stop(payload) if payload == "/stop"
    ));
    assert!(matches!(
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
        ),
        UserAction::Exit
    ));
}

#[test]
fn management_commands_cover_session_workspace_group_and_mcp_actions() {
    let mut app = test_app(true);
    let workspace = std::env::temp_dir().join("lingclaw tui project");
    assert_eq!(
        parse_management_command(
            &app,
            &format!(
                "/session create \"{}\" | Project Session",
                workspace.display()
            )
        )
        .unwrap(),
        Some(ManagementAction::CreateSession {
            name: "Project Session".into(),
            workspace: WorkspaceSelection::Directory(workspace),
        })
    );
    assert_eq!(
        parse_management_command(&app, "/session rebind managed").unwrap(),
        Some(ManagementAction::RebindSession(WorkspaceSelection::Managed))
    );
    assert_eq!(
        parse_management_command(&app, "/group create Reviewers | one, two one").unwrap(),
        Some(ManagementAction::CreateGroup {
            name: "Reviewers".into(),
            members: vec!["one".into(), "two".into()],
        })
    );
    app.active_group = Some("group-1".into());
    assert_eq!(
        parse_management_command(&app, "/group promote two").unwrap(),
        Some(ManagementAction::PromoteGroupMember("two".into()))
    );
    assert_eq!(
        parse_management_command(&app, "/mcp oauth filesystem").unwrap(),
        Some(ManagementAction::StartMcpOauth("filesystem".into()))
    );
    assert_eq!(
        parse_management_command(&app, "/mcp refresh").unwrap(),
        None
    );
}

#[test]
fn disabled_groups_reject_management_before_network_io() {
    let app = test_app(false);
    let error = parse_management_command(&app, "/group create Team | worker").unwrap_err();
    assert!(error.contains("disabled"));
}

#[test]
fn composer_dispatches_management_commands_without_rendering_chat_lines() {
    let mut app = test_app(false);
    app.focus = Focus::Composer;
    app.input = "/session rename New Name".into();
    assert!(matches!(
        handle_composer_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
        ),
        UserAction::Manage(ManagementAction::RenameSession(name)) if name == "New Name"
    ));
    assert!(app.input.is_empty());
    assert!(app.lines.is_empty());
}

#[test]
fn destructive_management_commands_require_confirmation_and_restore_cancelled_drafts() {
    let mut app = test_app(false);
    app.focus = Focus::Composer;
    app.input = "/session delete worker".into();

    assert!(matches!(
        handle_composer_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        UserAction::None
    ));
    assert!(app.pending_confirmation.is_some());
    assert!(app.input.is_empty());

    assert!(matches!(
        handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        UserAction::None
    ));
    assert!(app.pending_confirmation.is_none());
    assert_eq!(app.input, "/session delete worker");

    assert!(matches!(
        handle_composer_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        UserAction::None
    ));
    assert!(matches!(
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
        ),
        UserAction::Manage(ManagementAction::DeleteSession(session_id))
            if session_id == "worker"
    ));
    assert!(app.pending_confirmation.is_none());
}

#[test]
fn socket_list_events_refresh_navigation_without_losing_current_session() {
    let mut app = test_app(true);
    apply_socket_event(
        &mut app,
        json!({
            "type": "session_list",
            "sessions": [
                {"id":"main","name":"Renamed","workspace":{"kind":"managed","path":"C:\\home","available":true}},
                {"id":"worker","name":"Worker"}
            ]
        }),
    );
    assert_eq!(app.sessions.len(), 2);
    assert_eq!(app.session.name, "Renamed");
    assert_eq!(app.session.workspace.as_ref().unwrap().path, "C:\\home");

    apply_socket_event(
        &mut app,
        json!({
            "type": "session_group_list",
            "groups": [{"id":"team","name":"Team","members":2}]
        }),
    );
    assert_eq!(app.groups.len(), 1);
    assert_eq!(app.groups[0].id, "team");
}

#[test]
fn session_replay_reconciles_busy_state_from_history_and_start() {
    let mut app = test_app(false);
    app.busy = true;

    apply_socket_event(&mut app, json!({"type":"history","messages":[]}));
    assert!(!app.busy, "history clears stale local run state");

    apply_socket_event(
        &mut app,
        json!({"type":"start","round":2,"run_connection_id":"run-2"}),
    );
    assert!(app.busy, "a replayed live round restores busy state");

    apply_socket_event(&mut app, json!({"type":"done","run_connection_id":"run-2"}));
    assert!(!app.busy, "terminal events still clear busy state");
}

#[test]
fn nonterminal_errors_do_not_end_an_active_tui_run() {
    let mut app = test_app(false);
    apply_socket_event(&mut app, json!({"type":"history","messages":[]}));
    apply_socket_event(
        &mut app,
        json!({"type":"start","round":2,"run_connection_id":"run-errors"}),
    );
    assert!(app.busy);
    assert!(app.direct_run_active);

    apply_socket_event(
        &mut app,
        json!({
            "type":"error",
            "run_terminal":false,
            "content":"Busy /think failed while the run continued."
        }),
    );
    apply_socket_event(
        &mut app,
        json!({
            "type":"error",
            "content":"An unclassified legacy error is also nonterminal."
        }),
    );

    assert!(app.busy);
    assert!(app.direct_run_active);
    assert_eq!(
        app.lines
            .iter()
            .filter(|line| matches!(line.style, LineKind::Error))
            .count(),
        2
    );

    apply_socket_event(
        &mut app,
        json!({
            "type":"error",
            "run_terminal":true,
            "run_connection_id":"run-errors",
            "content":"The provider terminated the top-level run."
        }),
    );
    assert!(!app.busy);
    assert!(!app.direct_run_active);
}

#[test]
fn late_done_from_an_old_connection_does_not_end_the_replacement_tui_run() {
    let mut app = test_app(false);
    apply_socket_event(&mut app, json!({"type":"history","messages":[]}));
    apply_socket_event(
        &mut app,
        json!({"type":"start","round":1,"run_connection_id":"old-run"}),
    );
    apply_socket_event(
        &mut app,
        json!({
            "type":"error",
            "run_terminal":true,
            "run_connection_id":"old-run",
            "content":"The old run failed."
        }),
    );
    assert!(!app.direct_run_active);

    apply_socket_event(&mut app, json!({"type":"history","messages":[]}));
    apply_socket_event(
        &mut app,
        json!({"type":"start","round":2,"run_connection_id":"new-run"}),
    );
    apply_socket_event(
        &mut app,
        json!({"type":"done","phase":"failed","run_connection_id":"old-run"}),
    );
    assert!(app.busy);
    assert!(app.direct_run_active);
    assert_eq!(app.direct_run_identity.as_deref(), Some("new-run"));

    apply_socket_event(
        &mut app,
        json!({"type":"done","phase":"finish","run_connection_id":"new-run"}),
    );
    assert!(!app.busy);
    assert!(!app.direct_run_active);
    assert!(app.direct_run_identity.is_none());
}

#[test]
fn identityless_legacy_runs_close_only_on_their_first_socket_generation() {
    for (terminal, expected_busy) in [
        (
            json!({"type":"done","phase":"finish","reason":"complete"}),
            false,
        ),
        (
            json!({"type":"done","phase":"stopped","reason":"user_stop"}),
            false,
        ),
        (
            json!({"type":"done","phase":"failed","reason":"provider_error"}),
            false,
        ),
    ] {
        let mut app = test_app(false);
        app.execution_identity_protocol = ExecutionIdentityProtocol::Legacy;
        mark_socket_connected(&mut app);
        apply_socket_event(&mut app, json!({"type":"history","messages":[]}));
        apply_socket_event(&mut app, json!({"type":"start","round":1}));
        assert!(app.direct_run_active);
        assert_eq!(app.direct_run_identity.as_deref(), Some("legacy-socket-1"));

        apply_socket_event(&mut app, terminal);
        assert_eq!(app.busy, expected_busy);
        assert!(!app.direct_run_active);
        assert!(app.direct_run_identity.is_none());
    }
}

#[test]
fn identityless_legacy_terminal_error_closes_but_nonterminal_error_does_not() {
    let mut app = test_app(false);
    app.execution_identity_protocol = ExecutionIdentityProtocol::Legacy;
    mark_socket_connected(&mut app);
    apply_socket_event(&mut app, json!({"type":"history","messages":[]}));
    apply_socket_event(&mut app, json!({"type":"start","round":1}));

    apply_socket_event(
        &mut app,
        json!({
            "type":"error",
            "run_terminal":false,
            "content":"The legacy command failed."
        }),
    );
    assert!(app.busy);
    assert!(app.direct_run_active);

    apply_socket_event(
        &mut app,
        json!({
            "type":"error",
            "run_terminal":true,
            "content":"The legacy run failed."
        }),
    );
    assert!(!app.busy);
    assert!(!app.direct_run_active);
    assert!(app.direct_run_identity.is_none());
}

#[test]
fn strict_identityless_start_restores_the_real_outbound_draft_before_closing() {
    let mut app = test_app(false);
    app.connected = true;
    app.input = "preserve this draft".into();
    app.pending_images
        .push(json!({"url":"https://images.example/preserved.png"}));
    app.plan_mode = true;
    let snapshot = ComposerSnapshot::capture(&app);

    assert!(matches!(
        handle_composer_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        UserAction::Send(_)
    ));
    app.pending_outbound_write = Some(snapshot);

    let action = apply_socket_event(&mut app, json!({"type":"start","round":1}));

    assert_eq!(action, SocketEventAction::CloseExecutionProtocol);
    assert_eq!(app.input, "preserve this draft");
    assert_eq!(app.pending_images.len(), 1);
    assert!(app.plan_mode);
    assert!(app.pending_outbound_write.is_none());
    assert!(!app.outbound_reconnect_pending);
    assert!(!app.connected);
    assert!(!app.busy);
    assert!(!app.direct_run_active);
    assert_eq!(
        app.execution_identity_protocol,
        ExecutionIdentityProtocol::Unavailable
    );
}

#[test]
fn strict_identityless_terminal_error_closes_but_nonterminal_error_does_not() {
    let mut nonterminal = test_app(false);
    nonterminal.connected = true;
    nonterminal.input = "/think max".into();
    let snapshot = ComposerSnapshot::capture(&nonterminal);
    nonterminal.input.clear();
    nonterminal.pending_outbound_write = Some(snapshot);
    assert_eq!(
        apply_socket_event(
            &mut nonterminal,
            json!({"type":"error","run_terminal":false,"content":"invalid effort"}),
        ),
        SocketEventAction::None
    );
    assert_eq!(nonterminal.input, "/think max");
    assert!(nonterminal.connected);
    assert_eq!(
        nonterminal.execution_identity_protocol,
        ExecutionIdentityProtocol::Strict
    );

    let mut terminal = test_app(false);
    terminal.connected = true;
    terminal.input = "provider request".into();
    let snapshot = ComposerSnapshot::capture(&terminal);
    terminal.input.clear();
    terminal.pending_outbound_write = Some(snapshot);
    assert_eq!(
        apply_socket_event(
            &mut terminal,
            json!({"type":"error","run_terminal":true,"content":"provider failed"}),
        ),
        SocketEventAction::CloseExecutionProtocol
    );
    assert_eq!(terminal.input, "provider request");
    assert!(!terminal.connected);
    assert_eq!(
        terminal.execution_identity_protocol,
        ExecutionIdentityProtocol::Unavailable
    );

    let mut done = test_app(false);
    done.connected = true;
    apply_socket_event(
        &mut done,
        json!({"type":"start","run_connection_id":"strict-run"}),
    );
    assert!(done.direct_run_active);
    assert_eq!(
        apply_socket_event(&mut done, json!({"type":"done","phase":"failed"})),
        SocketEventAction::CloseExecutionProtocol
    );
    assert!(!done.direct_run_active);
    assert!(!done.busy);
}

#[test]
fn legacy_disconnect_restores_pending_composer_while_strict_waits_for_history() {
    let mut legacy = test_app(false);
    legacy.execution_identity_protocol = ExecutionIdentityProtocol::Legacy;
    mark_socket_connected(&mut legacy);
    legacy.input = "legacy retry".into();
    legacy
        .pending_images
        .push(json!({"url":"https://images.example/legacy.png"}));
    legacy.plan_mode = true;
    let legacy_snapshot = ComposerSnapshot::capture(&legacy);
    legacy.input.clear();
    legacy.pending_images.clear();
    legacy.plan_mode = false;
    legacy.pending_outbound_write = Some(legacy_snapshot);
    legacy.busy = true;

    mark_socket_disconnected(&mut legacy);

    assert_eq!(legacy.input, "legacy retry");
    assert_eq!(legacy.pending_images.len(), 1);
    assert!(legacy.plan_mode);
    assert!(legacy.pending_outbound_write.is_none());
    assert!(!legacy.outbound_reconnect_pending);
    assert!(!legacy.busy);

    let mut strict = test_app(false);
    strict.input = "strict pending".into();
    let strict_snapshot = ComposerSnapshot::capture(&strict);
    strict.input.clear();
    strict.pending_outbound_write = Some(strict_snapshot);
    mark_socket_connected(&mut strict);
    mark_socket_disconnected(&mut strict);

    assert!(strict.input.is_empty());
    assert!(strict.pending_outbound_write.is_some());
    assert!(strict.outbound_reconnect_pending);
}

#[tokio::test]
async fn legacy_reconnect_quarantines_identityless_terminals_and_blocks_a_second_socket() {
    let mut app = test_app(false);
    app.execution_identity_protocol = ExecutionIdentityProtocol::Legacy;
    mark_socket_connected(&mut app);
    apply_socket_event(&mut app, json!({"type":"history","messages":[]}));
    apply_socket_event(&mut app, json!({"type":"start","round":1}));
    mark_socket_disconnected(&mut app);
    assert!(!app.busy);
    assert!(!app.direct_run_active);
    assert!(reconnect_status(&app, "").contains("older daemon cannot reconnect"));

    mark_socket_connected(&mut app);
    assert_eq!(app.socket_generation, 2);
    assert_eq!(app.legacy_execution_socket_generation, Some(1));
    apply_socket_event(&mut app, json!({"type":"done","phase":"failed"}));
    apply_socket_event(
        &mut app,
        json!({"type":"error","run_terminal":true,"content":"late"}),
    );
    assert_eq!(
        apply_socket_event(&mut app, json!({"type":"start","round":2})),
        SocketEventAction::CloseExecutionProtocol
    );
    assert!(!app.busy);
    assert!(!app.direct_run_active);
    assert!(
        app.status
            .contains("omitted the required execution identity")
    );

    app.execution_identity_protocol = ExecutionIdentityProtocol::Strict;
    mark_socket_connected(&mut app);
    apply_socket_event(
        &mut app,
        json!({"type":"start","round":2,"run_connection_id":"strict-new"}),
    );
    assert_eq!(
        apply_socket_event(&mut app, json!({"type":"done","phase":"failed"})),
        SocketEventAction::CloseExecutionProtocol
    );
    assert!(!app.busy);
    assert!(!app.direct_run_active);

    app.execution_identity_protocol = ExecutionIdentityProtocol::Strict;
    mark_socket_connected(&mut app);
    apply_socket_event(
        &mut app,
        json!({"type":"start","round":3,"run_connection_id":"strict-final"}),
    );
    apply_socket_event(
        &mut app,
        json!({"type":"done","phase":"finish","run_connection_id":"strict-final"}),
    );
    assert!(!app.busy);
    assert!(!app.direct_run_active);

    app.execution_identity_protocol = ExecutionIdentityProtocol::Legacy;
    let router = axum::Router::new().route(
        "/api/client-config",
        axum::routing::get(|| async { axum::Json(json!({"features":{"groups":false}})) }),
    );
    let (base, task) = spawn_test_http_server(router).await;
    let error = connect_socket_for_app(&Client::new(), &base, "main", None, &mut app)
        .await
        .expect_err("a second legacy connection must fail before network I/O");
    assert!(
        error
            .to_string()
            .contains("older daemon cannot open a second")
    );
    task.abort();
}

#[test]
fn session_replay_restores_only_messages_missing_from_authoritative_history() {
    let mut missing = test_app(false);
    missing.connected = true;
    missing.input = "retry after reconnect".into();
    let snapshot = ComposerSnapshot::capture(&missing);
    assert!(matches!(
        handle_composer_key(
            &mut missing,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
        ),
        UserAction::Send(_)
    ));
    missing.pending_outbound_write = Some(snapshot);
    missing.outbound_reconnect_pending = true;
    apply_socket_event(&mut missing, json!({"type":"history","messages":[]}));
    assert_eq!(missing.input, "retry after reconnect");
    assert!(missing.pending_outbound_write.is_none());

    let mut persisted = test_app(false);
    apply_socket_event(&mut persisted, json!({"type":"history","messages":[]}));
    persisted.connected = true;
    persisted.input = "already persisted".into();
    let snapshot = ComposerSnapshot::capture(&persisted);
    assert!(matches!(
        handle_composer_key(
            &mut persisted,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
        ),
        UserAction::Send(_)
    ));
    persisted.pending_outbound_write = Some(snapshot);
    persisted.outbound_reconnect_pending = true;
    apply_socket_event(
        &mut persisted,
        json!({
            "type":"history",
            "messages":[{
                "role":"user",
                "content":"already persisted",
                "message_index":0
            }]
        }),
    );
    assert!(persisted.input.is_empty());
    assert!(persisted.pending_outbound_write.is_none());
    assert_eq!(persisted.lines.len(), 1);

    let mut initial_replay = test_app(false);
    initial_replay.connected = true;
    initial_replay.input = "sent during initial replay".into();
    let snapshot = ComposerSnapshot::capture(&initial_replay);
    assert!(matches!(
        handle_composer_key(
            &mut initial_replay,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
        ),
        UserAction::Send(_)
    ));
    initial_replay.pending_outbound_write = Some(snapshot);
    apply_socket_event(&mut initial_replay, json!({"type":"history","messages":[]}));
    assert!(initial_replay.input.is_empty());
    assert!(initial_replay.pending_outbound_write.is_some());
    apply_socket_event(
        &mut initial_replay,
        json!({"type":"start","run_connection_id":"initial-replay"}),
    );
    assert!(initial_replay.pending_outbound_write.is_none());
}

#[test]
fn replay_uses_raw_message_ids_and_image_fingerprints() {
    let mut acknowledged = test_app(false);
    apply_socket_event(&mut acknowledged, json!({"type":"history","messages":[]}));
    acknowledged.input = "repeat after acknowledgement".into();
    acknowledged.pending_outbound_write = Some(ComposerSnapshot::capture(&acknowledged));
    acknowledged.input.clear();
    apply_socket_event(
        &mut acknowledged,
        json!({"type":"start","run_connection_id":"acknowledged-replay"}),
    );
    acknowledged.input = "repeat after acknowledgement".into();
    acknowledged.pending_outbound_write = Some(ComposerSnapshot::capture(&acknowledged));
    acknowledged.input.clear();
    acknowledged.outbound_reconnect_pending = true;
    apply_socket_event(
        &mut acknowledged,
        json!({
            "type":"history",
            "messages":[{
                "role":"user",
                "content":"repeat after acknowledgement",
                "message_index":0
            }]
        }),
    );
    assert_eq!(acknowledged.input, "repeat after acknowledgement");

    let old_group_message = json!({
        "id":"old-message",
        "role":"user",
        "content":"repeat",
        "timestamp":42
    });
    let mut group = test_app(true);
    apply_socket_event(
        &mut group,
        json!({"type":"group_history","messages":[old_group_message.clone()]}),
    );
    group.input = "repeat".into();
    let snapshot = ComposerSnapshot::capture(&group);
    group.input.clear();
    group.pending_outbound_write = Some(snapshot);
    group.outbound_reconnect_pending = true;

    apply_socket_event(
        &mut group,
        json!({"type":"group_history","messages":[old_group_message.clone()]}),
    );
    assert_eq!(group.input, "repeat");

    group.input = "repeat".into();
    let snapshot = ComposerSnapshot::capture(&group);
    group.input.clear();
    group.pending_outbound_write = Some(snapshot);
    group.outbound_reconnect_pending = true;
    apply_socket_event(
        &mut group,
        json!({
            "type":"group_history",
            "messages":[
                old_group_message,
                {"id":"new-message","role":"user","content":"repeat","timestamp":42}
            ]
        }),
    );
    assert!(group.input.is_empty());

    let mut image = test_app(false);
    apply_socket_event(
        &mut image,
        json!({
            "type":"history",
            "messages":[{
                "role":"user",
                "content":"",
                "message_index":0,
                "images":[{"url":"https://images.example/old.png?signature=old"}]
            }]
        }),
    );
    image
        .pending_images
        .push(json!({"url":"https://images.example/new.png?signature=new"}));
    let snapshot = ComposerSnapshot::capture(&image);
    image.pending_images.clear();
    image.pending_outbound_write = Some(snapshot);
    image.outbound_reconnect_pending = true;
    apply_socket_event(
        &mut image,
        json!({
            "type":"history",
            "messages":[{
                "role":"user",
                "content":"",
                "message_index":0,
                "images":[{"url":"https://images.example/old.png?signature=rotated"}]
            }]
        }),
    );
    assert_eq!(image.pending_images.len(), 1);
    assert_eq!(
        image.pending_images[0]["url"],
        "https://images.example/new.png?signature=new"
    );
}

#[test]
fn session_reasoning_chunks_share_one_stream_until_thinking_done() {
    let mut app = test_app(false);
    apply_socket_event(&mut app, json!({"type":"thinking_start"}));
    apply_socket_event(
        &mut app,
        json!({"type":"thinking_delta","content":"first "}),
    );
    apply_socket_event(
        &mut app,
        json!({"type":"reasoning_delta","content":"second"}),
    );
    assert_eq!(app.lines.len(), 1);
    assert_eq!(app.lines[0].content, "first second");
    assert_eq!(
        app.lines[0].stream_id.as_deref(),
        Some(SESSION_REASONING_STREAM_ID)
    );

    apply_socket_event(&mut app, json!({"type":"thinking_done"}));
    assert!(app.lines[0].stream_id.is_none());
    apply_socket_event(&mut app, json!({"type":"thinking_start"}));
    apply_socket_event(&mut app, json!({"type":"thinking_delta","content":"next"}));
    assert_eq!(app.lines.len(), 2);
}

#[test]
fn session_replay_restores_the_current_plan_revision_and_clears_stale_state() {
    let mut app = test_app(false);
    app.page_index = app
        .pages
        .iter()
        .position(|page| *page == Page::Plan)
        .unwrap();
    app.active_plan = Some(PlanSnapshot {
        id: "stale-plan".into(),
        revision: 9,
        status: "ready".into(),
        title: "Stale".into(),
        raw: json!({"plan_id":"stale-plan"}),
    });
    app.last_image_url = Some("https://example.invalid/stale.png".into());

    apply_socket_event(
        &mut app,
        json!({
            "type": "history",
            "messages": [],
            "plans": [
                {
                    "plan_id": "plan-a",
                    "revision": 1,
                    "status": "ready",
                    "historical": true,
                    "artifact": {"title": "First revision"}
                },
                {
                    "plan_id": "plan-a",
                    "revision": 2,
                    "status": "failed",
                    "historical": false,
                    "artifact": {"title": "Current revision"}
                }
            ]
        }),
    );

    let plan = app
        .active_plan
        .as_ref()
        .expect("current plan should recover");
    assert_eq!(plan.id, "plan-a");
    assert_eq!(plan.revision, 2);
    assert_eq!(plan.status, "failed");
    assert!(app.inspector.contains("Current revision"));
    assert!(app.last_image_url.is_none());

    apply_socket_event(&mut app, json!({"type":"history","messages":[]}));
    assert!(app.active_plan.is_none());
    assert_eq!(app.inspector, "No active plan");
}

#[test]
fn storage_protection_restores_in_flight_drafts_and_blocks_core_writes() {
    let mut app = test_app(false);
    app.connected = true;
    app.input = "keep this draft".into();
    let snapshot = ComposerSnapshot::capture(&app);
    assert!(matches!(
        handle_composer_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        UserAction::Send(_)
    ));
    app.pending_outbound_write = Some(snapshot);

    apply_socket_event(
        &mut app,
        json!({
            "type":"storage_status",
            "storage":{"mode":"protected","code":"storage_protected"}
        }),
    );

    assert!(!app.storage_writable);
    assert_eq!(app.input, "keep this draft");
    assert!(app.pending_outbound_write.is_none());
    assert!(matches!(
        handle_composer_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        UserAction::None
    ));
    assert_eq!(app.input, "keep this draft");

    app.input = "/status".into();
    assert!(matches!(
        handle_composer_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        UserAction::Send(command) if command == "/status"
    ));
}

#[test]
fn storage_protection_keeps_config_backed_management_available() {
    let mut app = test_app(false);
    app.storage_writable = false;
    app.input = "/session rename Blocked".into();
    assert!(matches!(
        handle_composer_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        UserAction::None
    ));
    assert_eq!(app.input, "/session rename Blocked");

    app.input = "/mcp oauth filesystem".into();
    assert!(matches!(
        handle_composer_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        UserAction::Manage(ManagementAction::StartMcpOauth(server))
            if server == "filesystem"
    ));
}

#[test]
fn ready_plans_support_revision_feedback_and_explicit_stale_execution() {
    let mut app = test_app(false);
    app.connected = true;
    app.page_index = app
        .pages
        .iter()
        .position(|page| *page == Page::Plan)
        .unwrap();
    app.focus = Focus::Content;
    app.active_plan = Some(PlanSnapshot {
        id: "plan-a".into(),
        revision: 4,
        status: "ready".into(),
        title: "Plan".into(),
        raw: json!({"plan_id":"plan-a","revision":4,"status":"ready"}),
    });

    assert!(matches!(
        handle_content_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE)
        ),
        UserAction::None
    ));
    assert!(app.plan_feedback_mode);
    assert_eq!(app.focus, Focus::Composer);
    app.input = "Keep the public API unchanged".into();
    let UserAction::Send(feedback) =
        handle_composer_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
    else {
        panic!("ready plan feedback should be sent");
    };
    let feedback: Value = serde_json::from_str(&feedback).unwrap();
    assert_eq!(feedback["plan_action"]["action"], "feedback");
    assert_eq!(feedback["plan_action"]["revision"], 4);
    assert_eq!(
        feedback["plan_action"]["text"],
        "Keep the public API unchanged"
    );

    app.focus = Focus::Content;
    let _ = plan_action(&mut app, "execute", false);
    apply_socket_event(
        &mut app,
        json!({
            "type":"plan_stale",
            "code":"plan_stale",
            "plan_id":"plan-a",
            "revision":4,
            "paths":["src/main.rs"],
            "confirmation_token":"signed-token"
        }),
    );
    assert!(matches!(
        handle_content_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)
        ),
        UserAction::None
    ));
    assert!(app.confirm_stale_plan);
    let UserAction::Send(execute) =
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
    else {
        panic!("confirmed stale plan should be sent");
    };
    let execute: Value = serde_json::from_str(&execute).unwrap();
    assert_eq!(execute["plan_action"]["action"], "execute");
    assert_eq!(execute["plan_action"]["allow_stale"], true);
    assert_eq!(
        execute["plan_action"]["stale_confirmation_token"],
        "signed-token"
    );
}

#[test]
fn target_switch_reset_removes_all_session_scoped_derived_state() {
    let mut app = test_app(false);
    app.active_group_runs.insert("run-a".into());
    app.busy = true;
    app.connected = true;
    app.storage_writable = false;
    app.plan_mode = true;
    app.active_plan = Some(PlanSnapshot {
        id: "plan-a".into(),
        revision: 1,
        status: "ready".into(),
        title: "Plan".into(),
        raw: Value::Null,
    });
    app.plan_feedback_mode = true;
    app.plan_stale = Some(PlanStaleSnapshot {
        confirmation_token: "token".into(),
        paths: vec!["src/main.rs".into()],
        evidence_incomplete: false,
        action: "execute".into(),
    });
    app.pending_plan_action = Some("execute".into());
    app.confirm_stale_plan = true;
    app.pending_outbound_write = Some(ComposerSnapshot::capture(&app));
    app.pending_images
        .push(json!({"url":"https://example.invalid/image.png"}));
    app.push("assistant", "old conversation", LineKind::Assistant);
    app.inspector = "old inspector".into();
    app.todos_snapshot = "old todos".into();
    app.current_model = "provider/old".into();
    app.current_effort = "high".into();
    app.last_image_url = Some("https://example.invalid/image.png".into());

    reset_target_scoped_state(&mut app);

    assert!(app.active_group_runs.is_empty());
    assert!(!app.busy);
    assert!(!app.connected);
    assert!(
        !app.storage_writable,
        "storage protection is process scoped"
    );
    assert!(!app.plan_mode);
    assert!(app.active_plan.is_none());
    assert!(!app.plan_feedback_mode);
    assert!(app.plan_stale.is_none());
    assert!(app.pending_plan_action.is_none());
    assert!(!app.confirm_stale_plan);
    assert!(app.pending_outbound_write.is_none());
    assert!(app.pending_images.is_empty());
    assert!(app.lines.is_empty());
    assert!(app.inspector.is_empty());
    assert!(app.todos_snapshot.is_empty());
    assert!(app.current_model.is_empty());
    assert!(app.current_effort.is_empty());
    assert!(app.last_image_url.is_none());
}

#[test]
fn successful_outbound_write_does_not_invent_run_state() {
    let mut app = test_app(false);
    app.quit_armed = true;

    acknowledge_outbound_send(&mut app);

    assert!(!app.busy, "the daemon has not emitted start yet");
    assert!(
        !app.quit_armed,
        "successful activity disarms exit confirmation"
    );

    app.busy = true;
    acknowledge_outbound_send(&mut app);
    assert!(app.busy, "an already active run remains active");
}

#[test]
fn session_model_events_update_only_the_active_session() {
    let mut app = test_app(false);
    apply_socket_event(
        &mut app,
        json!({
            "type": "session",
            "id": "main",
            "model": "provider/reasoner",
            "effort": "high",
            "configRevision": 10,
            "capabilities": {"image": true, "s3_config_id": "s3-a"}
        }),
    );
    assert_eq!(app.current_model, "provider/reasoner");
    assert_eq!(app.current_effort, "high");
    assert!(app.current_model_supports_image);
    assert_eq!(app.model_config_revision, 10);
    assert_eq!(app.current_s3_config_id.as_deref(), Some("s3-a"));

    apply_socket_event(
        &mut app,
        json!({
            "type": "session_model_configuration",
            "id": "worker",
            "model": "provider/other",
            "effort": "low",
            "configRevision": 11,
            "capabilities": {"image": false, "s3_config_id": null}
        }),
    );
    assert_eq!(app.current_model, "provider/reasoner");
    assert_eq!(app.current_effort, "high");
    assert!(app.current_model_supports_image);
    assert_eq!(app.model_config_revision, 10);
    assert_eq!(app.current_s3_config_id.as_deref(), Some("s3-a"));

    apply_socket_event(
        &mut app,
        json!({
            "type": "session_model_configuration",
            "id": "main",
            "model": "provider/reasoner-v2",
            "effort": "medium",
            "configRevision": 12,
            "capabilities": {"image": false, "s3_config_id": null}
        }),
    );
    assert_eq!(app.current_model, "provider/reasoner-v2");
    assert_eq!(app.current_effort, "medium");
    assert!(!app.current_model_supports_image);
    assert_eq!(app.model_config_revision, 12);
    assert!(app.current_s3_config_id.is_none());
}

#[test]
fn todos_and_inspector_keep_independent_state_and_scroll() {
    let mut app = test_app(false);
    apply_socket_event(
        &mut app,
        json!({"type":"todos_state","todos":[{"content":"ship it"}]}),
    );
    let todos = app.todos_snapshot.clone();
    app.inspector = "models-page".into();
    app.scroll = 12;
    app.page_index = app
        .pages
        .iter()
        .position(|page| *page == Page::Models)
        .unwrap();
    handle_content_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(app.scroll, 12);
    assert_eq!(app.inspector_scroll, 4);
    assert!(todos.contains("ship it"));
    assert_eq!(app.todos_snapshot, todos);
}

#[test]
fn settings_redaction_covers_common_credentials_without_hiding_token_limits() {
    let mut value = json!({
        "api_key": "one",
        "oauthAccessToken": "two",
        "password": "three",
        "privateKey": "four",
        "accessKey": "five",
        "maxTokens": 8192,
        "nested": {"client_secret": "six", "model": "safe"}
    });
    redact_secrets(&mut value);
    assert_eq!(value["api_key"], "••••••••");
    assert_eq!(value["oauthAccessToken"], "••••••••");
    assert_eq!(value["password"], "••••••••");
    assert_eq!(value["privateKey"], "••••••••");
    assert_eq!(value["accessKey"], "••••••••");
    assert_eq!(value["nested"]["client_secret"], "••••••••");
    assert_eq!(value["maxTokens"], 8192);
    assert_eq!(value["nested"]["model"], "safe");
}

#[test]
fn temporary_config_file_is_removed_on_drop() {
    let temporary = TempConfigFile::create(br#"{"settings":{}}"#).unwrap();
    let path = temporary.path().to_path_buf();
    assert!(path.exists());
    drop(temporary);
    assert!(!path.exists());
}

#[test]
fn malformed_config_snapshot_keeps_raw_json_available_for_repair() {
    let raw = "{\n  \"settings\": {\n";
    let snapshot = config_snapshot_from_payload(json!({
        "config": null,
        "raw": raw,
        "parse_error": "EOF while parsing an object",
        "configFileEtag": "etag-before-repair",
        "explicitPrimaryModelConfigured": false,
    }))
    .unwrap();

    assert!(snapshot.config.is_none());
    assert_eq!(snapshot.raw, raw);
    assert_eq!(snapshot.etag.as_deref(), Some("etag-before-repair"));
    assert!(
        snapshot
            .structured_config()
            .unwrap_err()
            .to_string()
            .contains("EOF while parsing an object")
    );
    assert!(!should_run_native_setup(Some(&snapshot), None, false));
}

#[test]
fn valid_config_snapshot_remains_available_to_native_setup() {
    let snapshot = config_snapshot_from_payload(json!({
        "config": {"settings": {"enableGroups": false}},
        "configFileEtag": "valid-etag",
        "explicitPrimaryModelConfigured": false,
    }))
    .unwrap();

    assert_eq!(
        snapshot.structured_config().unwrap()["settings"]["enableGroups"],
        false
    );
    assert!(snapshot.raw.contains("enableGroups"));
    assert!(should_run_native_setup(Some(&snapshot), None, false));
}

#[test]
fn native_setup_temp_file_is_unique_and_private() {
    let directory = std::env::temp_dir().join(format!(
        "lingclaw-tui-setup-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let target = directory.join(".lingclaw.json");
    let temporary = write_private_config_temp(&target, br#"{"apiKey":"secret"}"#).unwrap();

    assert_ne!(temporary, target.with_extension("tmp"));
    assert_eq!(
        std::fs::read_to_string(&temporary).unwrap(),
        r#"{"apiKey":"secret"}"#
    );
    #[cfg(unix)]
    assert_eq!(
        std::fs::metadata(&temporary).unwrap().permissions().mode() & 0o777,
        0o600
    );

    std::fs::remove_file(temporary).unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn native_setup_uses_unique_provider_names_and_validates_required_secrets() {
    let mut setup = NativeSetup::new(HashSet::from(["openai".to_string()]));
    assert!(!setup.advance().unwrap());
    assert!(!setup.advance().unwrap());
    assert_eq!(setup.provider_name, "openai-2");
    assert_eq!(setup.base_url, "https://api.openai.com/v1");

    setup.step = 4;
    setup.api_key.clear();
    assert_eq!(
        setup.advance().unwrap_err(),
        "API key is required for this provider"
    );
    setup.api_key = "  secret  ".into();
    assert!(!setup.advance().unwrap());
    assert_eq!(setup.api_key, "secret");

    setup.model_id = "anthropic/claude-sonnet".into();
    assert!(!setup.advance().unwrap());
    assert_eq!(setup.model_id, "anthropic/claude-sonnet");
}

#[test]
fn native_setup_builds_a_valid_config_without_writing_it() {
    let setup = NativeSetup {
        step: NativeSetup::LAST_STEP,
        provider_index: 0,
        provider_name: "primary".into(),
        base_url: "https://example.invalid/v1".into(),
        api_key: "secret".into(),
        model_id: "model-1".into(),
        reasoning: true,
        error: String::new(),
        existing_providers: HashSet::new(),
    };
    let config = build_native_setup_config(
        &setup,
        json!({"settings":{"enableGroups":true},"custom":{"kept":1}}),
    )
    .unwrap();

    assert_eq!(
        config["agents"]["defaults"]["model"]["primary"],
        "primary/model-1"
    );
    assert_eq!(config["models"]["providers"]["primary"]["apiKey"], "secret");
    assert_eq!(config["settings"]["enableGroups"], true);
    assert_eq!(config["custom"]["kept"], 1);
}

#[test]
fn native_setup_render_masks_api_keys() {
    let options = TuiOptions {
        path: Some(PathBuf::from(".")),
        session: None,
        port: 18989,
        language: UiLanguage::En,
        theme: UiTheme::Dark,
    };
    let mut setup = NativeSetup::new(HashSet::new());
    setup.step = 4;
    setup.api_key = "super-secret".into();
    let backend = TestBackend::new(90, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| render_native_setup(frame, &options, &setup))
        .unwrap();
    let rendered = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(!rendered.contains("super-secret"));
    assert!(rendered.contains("••••"));
}

#[test]
fn inline_editor_tracks_unicode_characters_across_lines() {
    let mut buffer = TextBuffer::new("a你\nxyz".into());
    buffer.move_vertical(-1);
    buffer.insert("🙂");
    assert_eq!(buffer.value(), "a你🙂\nxyz");
    buffer.line_home();
    buffer.delete();
    assert_eq!(buffer.value(), "你🙂\nxyz");
    buffer.line_end();
    buffer.backspace();
    assert_eq!(buffer.value(), "你\nxyz");
}

#[test]
fn settings_page_exposes_the_group_feature_toggle() {
    let mut app = test_app(false);
    app.page_index = app
        .pages
        .iter()
        .position(|page| *page == Page::Settings)
        .unwrap();
    assert!(matches!(
        handle_content_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE)
        ),
        UserAction::ToggleGroups
    ));
}

#[test]
fn settings_page_edits_common_fields_without_round_tripping_redacted_secrets() {
    let mut app = test_app(false);
    app.page_index = app
        .pages
        .iter()
        .position(|page| *page == Page::Settings)
        .unwrap();
    let payload = json!({
        "config": {
            "settings": {"enableGroups": false, "structuredMemory": false},
            "models": {"providers": {"openai": {
                "api": "openai-responses",
                "baseUrl": "https://api.example.test/v1",
                "apiKey": "••••••••",
                "models": [{"id": "gpt"}]
            }}},
            "agents": {"defaults": {"model": {"primary": "openai/gpt"}}},
            "custom": {"kept": true}
        }
    });
    apply_loaded_page(
        &mut app,
        Page::Settings,
        LoadedPage {
            payload: Some(payload),
            fallback: String::new(),
        },
    );
    assert!(
        interactive_row_count(
            &app,
            Page::Settings,
            app.inspector_payload.as_ref().unwrap()
        ) > 10
    );
    assert!(app.inspector.contains("Agent routing"));
    assert!(app.inspector.contains("Provider connections"));

    let rows = settings_rows(&app, app.inspector_payload.as_ref().unwrap());
    app.inspector_index = rows
        .iter()
        .position(|row| row.path == ["settings", "structuredMemory"])
        .unwrap();
    let UserAction::MutatePage(PageMutation::Config(toggle)) = handle_content_key(
        &mut app,
        KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
    ) else {
        panic!("Space should toggle a native boolean setting");
    };
    assert_eq!(toggle.path, ["settings", "structuredMemory"]);
    assert_eq!(toggle.value, Some(json!(true)));

    app.inspector_index = rows
        .iter()
        .position(|row| row.path == ["models", "providers", "openai", "apiKey"])
        .unwrap();
    assert!(matches!(
        handle_content_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        UserAction::None
    ));
    assert!(app.settings_edit.is_some());
    assert!(
        app.input.is_empty(),
        "masked credentials must not be prefilled"
    );
    app.input = "replacement-secret".into();
    let UserAction::MutatePage(PageMutation::Config(secret)) =
        handle_composer_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
    else {
        panic!("settings editor should produce a minimal config mutation");
    };
    assert_eq!(secret.value, Some(json!("replacement-secret")));

    let mut fresh = json!({
        "models": {"providers": {"openai": {"apiKey": "original-secret"}}},
        "custom": {"kept": true}
    });
    apply_config_mutation(&mut fresh, &toggle).unwrap();
    assert_eq!(fresh["settings"]["structuredMemory"], true);
    assert_eq!(
        fresh["models"]["providers"]["openai"]["apiKey"],
        "original-secret"
    );
    assert_eq!(fresh["custom"]["kept"], true);
}

#[tokio::test]
async fn image_upload_rejects_oversized_files_before_network_io() {
    let path = std::env::temp_dir().join(format!(
        "lingclaw-tui-oversized-{}-{}.png",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(crate::image_uploads::MAX_IMAGE_UPLOAD_BYTES as u64 + 1)
        .unwrap();
    drop(file);
    let context = ImageUploadContext::capture(&image_capable_test_app()).unwrap();
    let error = upload_local_image(&Client::new(), "http://127.0.0.1:1", &context, &path)
        .await
        .unwrap_err()
        .to_string();
    let _ = std::fs::remove_file(&path);
    assert!(error.contains("upload limit"));
}

#[test]
fn image_upload_context_binds_model_s3_session_and_socket_generation() {
    let mut app = image_capable_test_app();
    let context = ImageUploadContext::capture(&app).unwrap();
    assert!(context.is_current(&app));

    app.socket_generation += 1;
    assert!(!context.is_current(&app));
    app.socket_generation = context.socket_generation;
    app.current_model = "provider/other".into();
    assert!(!context.is_current(&app));
    app.current_model = context.model.clone();
    app.current_s3_config_id = Some("s3-b".into());
    assert!(!context.is_current(&app));

    let mut text_only = image_capable_test_app();
    text_only.current_model_supports_image = false;
    assert!(
        ImageUploadContext::capture(&text_only)
            .unwrap_err()
            .contains("does not support image")
    );
}

#[cfg(feature = "tui-images")]
#[test]
fn terminal_image_preview_context_rejects_stale_target_socket_and_url() {
    let mut app = image_capable_test_app();
    app.image_picker = Some(Picker::halfblocks());
    app.last_image_url = Some("https://example.invalid/first.png?signature=one".into());
    let context = TerminalImagePreviewContext::capture(&app).unwrap();
    assert!(context.is_current(&app));

    app.socket_generation += 1;
    assert!(!context.is_current(&app));
    app.socket_generation = context.socket_generation;
    app.active_group = Some("group-a".into());
    assert!(!context.is_current(&app));
    app.active_group = context.active_group.clone();
    app.last_image_url = Some("https://example.invalid/second.png".into());
    assert!(!context.is_current(&app));
}

#[test]
fn image_upload_response_must_match_the_original_s3_identity() {
    let valid = json!({
        "s3_config_id": "s3-a",
        "images": [{
            "url": "https://example.invalid/image.png",
            "object_key": "uploads/image.png",
            "attachment_token": "token",
            "s3_config_id": "s3-a"
        }]
    });
    assert_eq!(
        uploaded_images_for_s3_config(&valid, "s3-a")
            .expect("matching upload identity should be accepted")
            .len(),
        1
    );

    let changed_response = json!({
        "s3_config_id": "s3-b",
        "images": [{"s3_config_id": "s3-b"}]
    });
    assert!(
        uploaded_images_for_s3_config(&changed_response, "s3-a")
            .unwrap_err()
            .contains("changed while uploading")
    );

    let changed_image = json!({
        "s3_config_id": "s3-a",
        "images": [{"s3_config_id": "s3-b"}]
    });
    assert!(
        uploaded_images_for_s3_config(&changed_image, "s3-a")
            .unwrap_err()
            .contains("did not match")
    );
}

#[test]
fn external_editor_command_supports_quoted_paths_and_arguments() {
    assert_eq!(
        split_command_line(r#""C:\Program Files\Editor\editor.exe" --wait"#).unwrap(),
        vec![r"C:\Program Files\Editor\editor.exe", "--wait"]
    );
    assert_eq!(
        split_command_line("code --wait --reuse-window").unwrap(),
        vec!["code", "--wait", "--reuse-window"]
    );
    assert!(split_command_line("'unterminated").is_err());
}

#[test]
fn unknown_socket_events_remain_visible() {
    let options = TuiOptions {
        path: Some(PathBuf::from(".")),
        session: None,
        port: 18989,
        language: UiLanguage::En,
        theme: UiTheme::Dark,
    };
    let session = SessionSummary {
        id: "main".into(),
        name: "Main".into(),
        workspace: None,
    };
    let mut app = App::new(
        &options,
        session.clone(),
        vec![session],
        false,
        "fallback".into(),
    );
    apply_socket_event(&mut app, json!({"type":"future_event","answer":42}));
    assert!(app.lines.last().unwrap().content.contains("future_event"));
}

#[test]
fn group_member_streams_are_unwrapped_and_kept_separate() {
    let mut app = test_app(true);
    app.sessions.push(SessionSummary {
        id: "poet".into(),
        name: "Code Poet".into(),
        workspace: None,
    });
    for content in ["hello ", "world"] {
        apply_socket_event(
            &mut app,
            json!({
                "type":"group_member_event",
                "run_id":"run-1",
                "session_id":"poet",
                "event":{"type":"delta","content":content}
            }),
        );
    }
    apply_socket_event(
        &mut app,
        json!({
            "type":"group_member_event",
            "run_id":"run-2",
            "session_id":"main",
            "event":{"type":"delta","content":"second"}
        }),
    );

    assert_eq!(app.lines.len(), 2);
    assert_eq!(app.lines[0].role, "Code Poet");
    assert_eq!(app.lines[0].content, "hello world");
    assert_eq!(app.lines[1].content, "second");
    assert_ne!(app.lines[0].stream_id, app.lines[1].stream_id);
}

#[test]
fn group_page_is_opt_in() {
    let options = TuiOptions {
        path: Some(PathBuf::from(".")),
        session: None,
        port: 18989,
        language: UiLanguage::En,
        theme: UiTheme::Dark,
    };
    let session = SessionSummary {
        id: "main".into(),
        name: "Main".into(),
        workspace: None,
    };
    let disabled = App::new(
        &options,
        session.clone(),
        vec![session.clone()],
        false,
        "fallback".into(),
    );
    let enabled = App::new(
        &options,
        session.clone(),
        vec![session],
        true,
        "fallback".into(),
    );
    assert!(!disabled.pages.contains(&Page::Groups));
    assert!(enabled.pages.contains(&Page::Groups));
}

#[test]
fn hot_disabling_groups_returns_to_session_and_clears_group_runtime_state() {
    let options = TuiOptions {
        path: Some(PathBuf::from(".")),
        session: None,
        port: 18989,
        language: UiLanguage::En,
        theme: UiTheme::Dark,
    };
    let session = SessionSummary {
        id: "main".into(),
        name: "Main".into(),
        workspace: None,
    };
    let worker = SessionSummary {
        id: "worker".into(),
        name: "Worker".into(),
        workspace: None,
    };
    let mut app = App::new(
        &options,
        worker,
        vec![
            session,
            SessionSummary {
                id: "worker".into(),
                name: "Worker".into(),
                workspace: None,
            },
        ],
        true,
        "fallback".into(),
    );
    app.active_group = Some("reviewers".into());
    app.active_group_runs.insert("run-1".into());
    app.busy = true;
    app.groups.push(GroupSummary {
        id: "reviewers".into(),
        name: "Reviewers".into(),
        members: 2,
    });
    app.input = "pending group draft".into();
    let pending = ComposerSnapshot::capture(&app);
    app.input.clear();
    app.pending_outbound_write = Some(pending);
    app.outbound_reconnect_pending = true;

    let action = apply_socket_event(
        &mut app,
        json!({"type":"feature_status","features":{"groups":false}}),
    );

    assert_eq!(action, SocketEventAction::ReconnectMain);
    assert!(!app.groups_enabled);
    assert!(app.active_group.is_none());
    assert!(app.groups.is_empty());
    assert!(app.active_group_runs.is_empty());
    assert!(!app.busy);
    assert!(!app.pages.contains(&Page::Groups));
    assert_eq!(app.session.id, "main");
    assert!(app.input.is_empty());
    assert!(app.pending_outbound_write.is_none());
    assert!(!app.outbound_reconnect_pending);
    assert!(!app.connected);
    assert!(app.status.contains("returned to Main"));
}

#[test]
fn hot_enabling_groups_requests_a_preserved_group_list_refresh() {
    let mut app = test_app(false);

    let action = apply_socket_event(
        &mut app,
        json!({"type":"feature_status","features":{"groups":true}}),
    );

    assert_eq!(action, SocketEventAction::RefreshGroups);
    assert!(app.groups_enabled);
    assert!(app.pages.contains(&Page::Groups));
}

#[tokio::test]
async fn negotiated_group_enable_refreshes_after_connect_and_discards_stale_results() {
    let websocket_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let group_request_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let release_first_group_request = std::sync::Arc::new(tokio::sync::Notify::new());
    let ws_count = websocket_count.clone();
    let request_count = group_request_count.clone();
    let release_first = release_first_group_request.clone();
    let router = axum::Router::new()
        .route(
            "/api/client-config",
            axum::routing::get(|| async {
                axum::Json(json!({
                    "features":{"groups":true},
                    "protocols":{"execution_identity":crate::EXECUTION_IDENTITY_PROTOCOL_VERSION}
                }))
            }),
        )
        .route(
            "/api/session-groups",
            axum::routing::get(move || {
                let request_number =
                    request_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                let release_first = release_first.clone();
                async move {
                    if request_number == 1 {
                        release_first.notified().await;
                    }
                    axum::Json(json!({
                        "groups":[{
                            "id": if request_number == 1 {"stale-reviewers"} else {"reviewers"},
                            "name":"Reviewers",
                            "members":2
                        }]
                    }))
                }
            }),
        )
        .route(
            "/ws",
            axum::routing::get(move |upgrade: axum::extract::ws::WebSocketUpgrade| {
                ws_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async move { upgrade.on_upgrade(|_| async {}) }
            }),
        );
    let (base, task) = spawn_test_http_server(router).await;
    let client = build_control_client(Duration::from_secs(1)).unwrap();
    let (refresh_tx, mut refresh_rx) = mpsc::unbounded_channel();
    let mut app = test_app(false);

    let mut first = connect_socket_for_app_with_group_refresh(
        &client,
        &base,
        "main",
        None,
        &mut app,
        Some(&refresh_tx),
    )
    .await
    .expect("WebSocket should connect before the negotiated Group refresh completes");
    mark_socket_connected(&mut app);
    assert_eq!(websocket_count.load(std::sync::atomic::Ordering::SeqCst), 1);
    tokio::time::timeout(Duration::from_secs(1), async {
        while group_request_count.load(std::sync::atomic::Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the post-connect Group request should start");
    assert!(refresh_rx.try_recv().is_err());

    app.active_group = Some("new-target".into());
    release_first_group_request.notify_waiters();
    let stale = tokio::time::timeout(Duration::from_secs(1), refresh_rx.recv())
        .await
        .expect("stale Group response should finish")
        .expect("refresh sender should remain alive");
    assert!(!apply_negotiated_group_refresh(&mut app, stale));
    assert!(app.groups.is_empty());

    app.active_group = None;
    assert_eq!(
        apply_socket_event(
            &mut app,
            json!({"type":"feature_status","features":{"groups":false}}),
        ),
        SocketEventAction::None
    );
    let disabled_result = NegotiatedGroupRefresh {
        binding: group_refresh_binding(&app, app.socket_generation),
        attempt: 1,
        result: Ok(vec![GroupSummary {
            id: "disabled-stale".into(),
            name: "Disabled stale result".into(),
            members: 1,
        }]),
    };
    assert!(!apply_negotiated_group_refresh(&mut app, disabled_result));
    assert!(app.groups.is_empty());

    app.active_group = Some("reviewers".into());
    let mut second = connect_socket_for_app_with_group_refresh(
        &client,
        &base,
        "main",
        Some("reviewers"),
        &mut app,
        Some(&refresh_tx),
    )
    .await
    .expect("a current strict generation should reconnect");
    mark_socket_connected(&mut app);
    let current = tokio::time::timeout(Duration::from_secs(1), refresh_rx.recv())
        .await
        .expect("current Group response should finish")
        .expect("refresh sender should remain alive");
    assert!(apply_negotiated_group_refresh(&mut app, current));
    assert_eq!(app.groups.len(), 1);
    assert_eq!(app.groups[0].id, "reviewers");
    assert_eq!(websocket_count.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(
        group_request_count.load(std::sync::atomic::Ordering::SeqCst),
        2
    );
    assert!(refresh_rx.try_recv().is_err());

    let _ = first.close(None).await;
    let _ = second.close(None).await;
    task.abort();
}

#[tokio::test]
async fn negotiated_group_refresh_retries_once_on_the_same_healthy_socket() {
    let request_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let active_requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let max_active_requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let requests = request_count.clone();
    let active = active_requests.clone();
    let max_active = max_active_requests.clone();
    let router = axum::Router::new()
        .route(
            "/api/client-config",
            axum::routing::get(|| async {
                axum::Json(json!({
                    "features":{"groups":true},
                    "protocols":{"execution_identity":crate::EXECUTION_IDENTITY_PROTOCOL_VERSION}
                }))
            }),
        )
        .route(
            "/api/session-groups",
            axum::routing::get(move || {
                let request = requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                let concurrent = active.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                max_active.fetch_max(concurrent, std::sync::atomic::Ordering::SeqCst);
                let active = active.clone();
                async move {
                    active.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                    let status = if request == 1 {
                        axum::http::StatusCode::SERVICE_UNAVAILABLE
                    } else {
                        axum::http::StatusCode::OK
                    };
                    let groups = if request == 1 {
                        json!({"error":"temporary Group list failure"})
                    } else {
                        json!({"groups":[{"id":"recovered","name":"Recovered","members":1}]})
                    };
                    (status, axum::Json(groups))
                }
            }),
        )
        .route(
            "/ws",
            axum::routing::get(|upgrade: axum::extract::ws::WebSocketUpgrade| async move {
                upgrade.on_upgrade(|_| async {})
            }),
        );
    let (base, task) = spawn_test_http_server(router).await;
    let client = build_control_client(Duration::from_secs(1)).unwrap();
    let (refresh_tx, mut refresh_rx) = mpsc::unbounded_channel();
    let mut app = test_app(false);
    let mut socket = connect_socket_for_app_with_group_refresh(
        &client,
        &base,
        "main",
        None,
        &mut app,
        Some(&refresh_tx),
    )
    .await
    .expect("the socket should not wait for Group discovery");
    mark_socket_connected(&mut app);

    let first = tokio::time::timeout(Duration::from_secs(1), refresh_rx.recv())
        .await
        .expect("first Group refresh should finish")
        .expect("refresh sender should remain alive");
    let now = tokio::time::Instant::now();
    assert!(apply_negotiated_group_refresh_at(&mut app, first, now));
    assert_eq!(request_count.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(app.groups.is_empty());
    assert!(app.groups_refresh_pending);
    assert!(!take_due_group_refresh_retry(
        &mut app,
        now + GROUP_REFRESH_RETRY_BASE - Duration::from_millis(1)
    ));
    assert!(schedule_due_group_refresh_retry(
        &client,
        &base,
        &mut app,
        &refresh_tx,
        true,
        now + GROUP_REFRESH_RETRY_BASE,
    ));

    let second = tokio::time::timeout(Duration::from_secs(1), refresh_rx.recv())
        .await
        .expect("retry Group refresh should finish")
        .expect("refresh sender should remain alive");
    assert!(apply_negotiated_group_refresh_at(
        &mut app,
        second,
        now + GROUP_REFRESH_RETRY_BASE
    ));
    assert_eq!(request_count.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(
        max_active_requests.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert_eq!(app.groups.len(), 1);
    assert_eq!(app.groups[0].id, "recovered");
    assert!(!app.groups_refresh_pending);
    assert!(app.groups_refresh_retry_at.is_none());
    assert!(app.status.contains("recovered"));

    let _ = socket.close(None).await;
    task.abort();
}

async fn exercise_group_refresh_feature_cycle_aba(old_succeeds: bool, old_finishes_first: bool) {
    let request_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let active_requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let max_active_requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let releases = std::sync::Arc::new(vec![
        std::sync::Arc::new(tokio::sync::Notify::new()),
        std::sync::Arc::new(tokio::sync::Notify::new()),
    ]);
    let (started_tx, mut started_rx) = mpsc::unbounded_channel::<usize>();
    let requests = request_count.clone();
    let active = active_requests.clone();
    let max_active = max_active_requests.clone();
    let request_releases = releases.clone();
    let router = axum::Router::new().route(
        "/api/session-groups",
        axum::routing::get(move || {
            let request = requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let concurrent = active.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            max_active.fetch_max(concurrent, std::sync::atomic::Ordering::SeqCst);
            let release = request_releases.get(request).cloned();
            let active = active.clone();
            let started_tx = started_tx.clone();
            async move {
                let _ = started_tx.send(request);
                if let Some(release) = release {
                    release.notified().await;
                }
                active.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                if request == 0 && !old_succeeds {
                    return (
                        axum::http::StatusCode::SERVICE_UNAVAILABLE,
                        axum::Json(json!({"error":"stale first-cycle failure"})),
                    );
                }
                let id = if request == 0 { "stale" } else { "current" };
                (
                    axum::http::StatusCode::OK,
                    axum::Json(json!({
                        "groups":[{"id":id,"name":id,"members":1}]
                    })),
                )
            }
        }),
    );
    let (base, task) = spawn_test_http_server(router).await;
    let client = build_control_client(Duration::from_secs(2)).unwrap();
    let (refresh_tx, mut refresh_rx) = mpsc::unbounded_channel();
    let mut app = test_app(false);
    app.connected = true;
    app.socket_generation = 41;
    let mut socket = None;

    let first_action = apply_socket_event(
        &mut app,
        json!({"type":"feature_status","features":{"groups":true}}),
    );
    assert_eq!(first_action, SocketEventAction::RefreshGroups);
    let first_cycle = app.groups_refresh_cycle.clone();
    apply_socket_event_action_with_group_refresh(
        first_action,
        &client,
        &mut socket,
        &base,
        &mut app,
        Some(&refresh_tx),
    )
    .await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), started_rx.recv())
            .await
            .expect("the first-cycle request should start"),
        Some(0)
    );

    assert_eq!(
        apply_socket_event(
            &mut app,
            json!({"type":"feature_status","features":{"groups":true}}),
        ),
        SocketEventAction::None,
        "a repeated enabled status must not create another refresh cycle"
    );
    assert_eq!(app.groups_refresh_cycle, first_cycle);
    assert_eq!(request_count.load(std::sync::atomic::Ordering::SeqCst), 1);

    assert_eq!(
        apply_socket_event(
            &mut app,
            json!({"type":"feature_status","features":{"groups":false}}),
        ),
        SocketEventAction::None
    );
    let disabled_cycle = app.groups_refresh_cycle.clone();
    assert_ne!(disabled_cycle, first_cycle);
    assert!(!app.groups_refresh_pending);
    assert!(app.groups_refresh_in_flight.is_none());

    let second_action = apply_socket_event(
        &mut app,
        json!({"type":"feature_status","features":{"groups":true}}),
    );
    assert_eq!(second_action, SocketEventAction::RefreshGroups);
    assert_ne!(app.groups_refresh_cycle, disabled_cycle);
    assert_ne!(app.groups_refresh_cycle, first_cycle);
    apply_socket_event_action_with_group_refresh(
        second_action,
        &client,
        &mut socket,
        &base,
        &mut app,
        Some(&refresh_tx),
    )
    .await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), started_rx.recv())
            .await
            .expect("the re-enabled request should start"),
        Some(1)
    );
    let second_binding = app
        .groups_refresh_in_flight
        .clone()
        .expect("the second cycle owns the current in-flight request");
    assert_eq!(second_binding.socket_generation, 41);
    assert_eq!(second_binding.session_id, "main");
    assert_eq!(second_binding.active_group, None);
    assert_eq!(app.groups_refresh_attempts, 1);
    assert_eq!(request_count.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(
        max_active_requests.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "the new cycle must not wait for an invalidated HTTP request"
    );

    if old_finishes_first {
        let status_before_stale = app.status.clone();
        releases[0].notify_one();
        let stale = tokio::time::timeout(Duration::from_secs(1), refresh_rx.recv())
            .await
            .expect("the first-cycle response should finish")
            .expect("the refresh sender should remain alive");
        assert!(!apply_negotiated_group_refresh(&mut app, stale));
        assert_eq!(app.status, status_before_stale);
        assert_eq!(app.groups_refresh_in_flight, Some(second_binding.clone()));
        assert!(app.groups_refresh_pending);
        assert_eq!(app.groups_refresh_attempts, 1);
        assert!(app.groups_refresh_retry_at.is_none());
        assert!(app.groups.is_empty());

        releases[1].notify_one();
        let current = tokio::time::timeout(Duration::from_secs(1), refresh_rx.recv())
            .await
            .expect("the second-cycle response should finish")
            .expect("the refresh sender should remain alive");
        assert!(apply_negotiated_group_refresh(&mut app, current));
    } else {
        releases[1].notify_one();
        let current = tokio::time::timeout(Duration::from_secs(1), refresh_rx.recv())
            .await
            .expect("the second-cycle response should finish first")
            .expect("the refresh sender should remain alive");
        assert!(apply_negotiated_group_refresh(&mut app, current));
        let status_after_current = app.status.clone();

        releases[0].notify_one();
        let stale = tokio::time::timeout(Duration::from_secs(1), refresh_rx.recv())
            .await
            .expect("the stale first-cycle response should eventually finish")
            .expect("the refresh sender should remain alive");
        assert!(!apply_negotiated_group_refresh(&mut app, stale));
        assert_eq!(app.status, status_after_current);
    }

    assert_eq!(app.groups.len(), 1);
    assert_eq!(app.groups[0].id, "current");
    assert!(!app.groups_refresh_pending);
    assert!(app.groups_refresh_in_flight.is_none());
    assert_eq!(app.groups_refresh_attempts, 0);
    assert!(app.groups_refresh_retry_at.is_none());
    assert_eq!(request_count.load(std::sync::atomic::Ordering::SeqCst), 2);

    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn group_refresh_feature_cycle_rejects_old_success_before_current_success() {
    exercise_group_refresh_feature_cycle_aba(true, true).await;
}

#[tokio::test]
async fn group_refresh_feature_cycle_rejects_old_failure_before_current_success() {
    exercise_group_refresh_feature_cycle_aba(false, true).await;
}

#[tokio::test]
async fn group_refresh_feature_cycle_rejects_old_results_after_current_success() {
    exercise_group_refresh_feature_cycle_aba(true, false).await;
    exercise_group_refresh_feature_cycle_aba(false, false).await;
}

#[test]
fn group_refresh_retry_is_bounded_and_invalidated_by_identity_changes() {
    let start = tokio::time::Instant::now();
    let mut app = test_app(true);
    app.socket_generation = 7;
    app.connected = true;
    app.groups_refresh_pending = true;
    let session_binding = group_refresh_binding(&app, app.socket_generation);

    for attempt in 1..=GROUP_REFRESH_MAX_ATTEMPTS {
        app.groups_refresh_in_flight = Some(session_binding.clone());
        app.groups_refresh_retry_binding = Some(session_binding.clone());
        app.groups_refresh_attempts = attempt;
        assert!(apply_negotiated_group_refresh_at(
            &mut app,
            NegotiatedGroupRefresh {
                binding: session_binding.clone(),
                attempt,
                result: Err(format!("attempt {attempt} failed")),
            },
            start,
        ));
        if attempt < GROUP_REFRESH_MAX_ATTEMPTS {
            assert_eq!(
                app.groups_refresh_retry_at,
                Some(start + group_refresh_retry_delay(attempt))
            );
        } else {
            assert!(app.groups_refresh_retry_at.is_none());
        }
    }

    app.groups_refresh_retry_at = Some(start);
    app.active_group = Some("new-group-target".into());
    assert!(!take_due_group_refresh_retry(&mut app, start));
    assert!(app.groups_refresh_retry_at.is_none());

    let group_binding = group_refresh_binding(&app, app.socket_generation);
    app.groups_refresh_retry_binding = Some(group_binding);
    app.groups_refresh_retry_at = Some(start);
    app.active_group = None;
    assert!(!take_due_group_refresh_retry(&mut app, start));
    assert!(app.groups_refresh_retry_at.is_none());

    app.groups_refresh_retry_binding = Some(group_refresh_binding(&app, app.socket_generation));
    app.groups_refresh_retry_at = Some(start);
    app.groups_enabled = false;
    assert!(!take_due_group_refresh_retry(&mut app, start));
    assert!(app.groups_refresh_retry_at.is_none());

    app.groups_enabled = true;
    app.groups_refresh_pending = true;
    app.socket_generation += 1;
    let replacement = group_refresh_binding(&app, app.socket_generation);
    app.groups_refresh_retry_binding = Some(replacement.clone());
    app.groups_refresh_attempts = 1;
    app.groups_refresh_retry_at = Some(start);
    assert!(take_due_group_refresh_retry(&mut app, start));
}

#[test]
fn failed_group_reconnect_uses_feature_probe_to_return_to_main() {
    let mut app = test_app(true);
    app.active_group = Some("reviewers".into());
    app.groups.push(GroupSummary {
        id: "reviewers".into(),
        name: "Reviewers".into(),
        members: 2,
    });

    assert_eq!(
        apply_group_reconnect_feature_probe(&mut app, true),
        SocketEventAction::None
    );
    assert_eq!(app.active_group.as_deref(), Some("reviewers"));

    assert_eq!(
        apply_group_reconnect_feature_probe(&mut app, false),
        SocketEventAction::ReconnectMain
    );
    assert!(!app.groups_enabled);
    assert!(app.active_group.is_none());
    assert!(app.groups.is_empty());
    assert_eq!(app.session.id, "main");
}

#[test]
fn plan_action_and_image_message_payloads_preserve_protocol_fields() {
    let options = TuiOptions {
        path: Some(PathBuf::from(".")),
        session: None,
        port: 18989,
        language: UiLanguage::En,
        theme: UiTheme::Dark,
    };
    let session = SessionSummary {
        id: "main".into(),
        name: "Main".into(),
        workspace: None,
    };
    let mut app = App::new(
        &options,
        session.clone(),
        vec![session],
        false,
        "fallback".into(),
    );
    app.connected = true;
    app.active_plan = Some(PlanSnapshot {
        id: "plan-a".into(),
        revision: 7,
        status: "ready".into(),
        title: "Plan".into(),
        raw: Value::Null,
    });
    let UserAction::Send(plan_payload) = plan_action(&mut app, "execute", false) else {
        panic!("plan action should send a payload");
    };
    let plan_payload: Value = serde_json::from_str(&plan_payload).unwrap();
    assert_eq!(plan_payload["plan_action"]["plan_id"], "plan-a");
    assert_eq!(plan_payload["plan_action"]["revision"], 7);

    app.active_plan = None;
    app.input = "describe this".into();
    app.pending_images
        .push(json!({"url":"https://example.invalid/image.png","name":"image.png"}));
    let UserAction::Send(message_payload) =
        handle_composer_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
    else {
        panic!("composer should send the attached image");
    };
    let message_payload: Value = serde_json::from_str(&message_payload).unwrap();
    assert_eq!(message_payload["text"], "describe this");
    assert_eq!(message_payload["images"].as_array().unwrap().len(), 1);
    assert!(app.pending_images.is_empty(), "attachments are one-shot");
}

#[test]
fn group_composer_disables_plan_mode_and_sends_explicit_target_mode() {
    let options = TuiOptions {
        path: Some(PathBuf::from(".")),
        session: None,
        port: 18989,
        language: UiLanguage::En,
        theme: UiTheme::Dark,
    };
    let session = SessionSummary {
        id: "main".into(),
        name: "Main".into(),
        workspace: None,
    };
    let mut app = App::new(
        &options,
        session.clone(),
        vec![session],
        true,
        "fallback".into(),
    );
    app.connected = true;
    app.active_group = Some("reviewers".into());
    let action = handle_composer_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::ALT),
    );
    assert!(matches!(action, UserAction::None));
    assert!(!app.plan_mode);

    app.input = "/target worker-a,worker-b".into();
    assert!(matches!(
        handle_composer_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        UserAction::None
    ));
    app.input = "review this".into();
    let UserAction::Send(payload) =
        handle_composer_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
    else {
        panic!("Group composer should send a payload");
    };
    let payload: Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(payload["type"], "group_message");
    assert_eq!(payload["target_mode"], "selected");
    assert_eq!(payload["targets"], json!(["worker-a", "worker-b"]));
    assert_eq!(payload["run_mode"], "execute");
}

#[tokio::test]
async fn group_target_uses_main_and_rejects_session_scoped_pages() {
    let mut app = test_app(true);
    let worker = SessionSummary {
        id: "worker".into(),
        name: "Worker".into(),
        workspace: None,
    };
    app.sessions.push(worker.clone());
    app.session = worker;
    app.groups.push(GroupSummary {
        id: "reviewers".into(),
        name: "Reviewers".into(),
        members: 2,
    });

    activate_group_target(&mut app, "reviewers").unwrap();

    assert_eq!(app.session.id, "main");
    assert_eq!(app.active_group.as_deref(), Some("reviewers"));
    assert_eq!(active_target_name(&app), "Reviewers · Group");

    let client = Client::new();
    let loaded = load_page(&client, "http://127.0.0.1:1", &app, Page::Models).await;
    assert!(loaded.payload.is_none());
    assert!(loaded.fallback.contains("unavailable while a Group"));

    let error = execute_page_mutation(
        &client,
        "http://127.0.0.1:1",
        &mut app,
        PageMutation::Model {
            model: "provider/model".into(),
            effort: "high".into(),
        },
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("unavailable while a Group"));
}

#[test]
fn disconnected_composer_keeps_unsent_text_and_attachments() {
    let options = TuiOptions {
        path: Some(PathBuf::from(".")),
        session: None,
        port: 18989,
        language: UiLanguage::En,
        theme: UiTheme::Dark,
    };
    let session = SessionSummary {
        id: "main".into(),
        name: "Main".into(),
        workspace: None,
    };
    let mut app = App::new(
        &options,
        session.clone(),
        vec![session],
        false,
        "fallback".into(),
    );
    app.input = "send after reconnect".into();
    app.pending_images
        .push(json!({"url":"https://example.invalid/image.png"}));

    let action = handle_composer_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(matches!(action, UserAction::None));
    assert_eq!(app.input, "send after reconnect");
    assert_eq!(app.pending_images.len(), 1);
    assert!(app.status.contains("remains in the composer"));
}

#[test]
fn failed_socket_send_restores_optimistically_cleared_draft() {
    let mut app = test_app(false);
    app.connected = true;
    app.input = "retry me".into();
    app.plan_mode = true;
    app.pending_images
        .push(json!({"url":"https://example.invalid/image.png"}));
    let snapshot = ComposerSnapshot::capture(&app);

    assert!(matches!(
        handle_composer_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        UserAction::Send(_)
    ));
    assert!(app.input.is_empty());
    assert!(app.pending_images.is_empty());
    assert!(!app.plan_mode);
    assert_eq!(app.lines.len(), snapshot.line_count + 1);

    snapshot.restore_after_failed_send(&mut app);
    assert_eq!(app.input, "retry me");
    assert_eq!(app.pending_images.len(), 1);
    assert!(app.plan_mode);
    assert_eq!(app.lines.len(), 0);
}

#[test]
fn daemon_error_restores_optimistically_cleared_draft() {
    let mut app = test_app(false);
    app.connected = true;
    app.input = "retry after rejection".into();
    app.plan_mode = true;
    app.pending_images.push(json!({
        "url":"https://example.invalid/image.png",
        "s3_config_id":"s3-a"
    }));
    let snapshot = ComposerSnapshot::capture(&app);

    assert!(matches!(
        handle_composer_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        UserAction::Send(_)
    ));
    app.pending_outbound_write = Some(snapshot);
    apply_socket_event(
        &mut app,
        json!({"type":"error","code":"plan_already_active","content":"Plan active"}),
    );

    assert_eq!(app.input, "retry after rejection");
    assert_eq!(app.pending_images.len(), 1);
    assert!(app.plan_mode);
    assert!(app.pending_outbound_write.is_none());
    assert_eq!(app.lines.len(), 1);
    assert!(matches!(app.lines[0].style, LineKind::Error));
}

#[test]
fn daemon_system_rejection_restores_messages_but_settles_slash_commands() {
    let mut app = test_app(false);
    app.connected = true;
    app.input = "message rejected by capability gate".into();
    let snapshot = ComposerSnapshot::capture(&app);
    assert!(matches!(
        handle_composer_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        UserAction::Send(_)
    ));
    app.pending_outbound_write = Some(snapshot);
    apply_socket_event(
        &mut app,
        json!({"type":"system","content":"Current model does not support image input."}),
    );
    assert_eq!(app.input, "message rejected by capability gate");
    assert!(app.pending_outbound_write.is_none());

    let mut command_app = test_app(false);
    command_app.connected = true;
    command_app.input = "/new".into();
    let command_snapshot = ComposerSnapshot::capture(&command_app);
    assert!(matches!(
        handle_composer_key(
            &mut command_app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
        ),
        UserAction::Send(_)
    ));
    command_app.pending_outbound_write = Some(command_snapshot);
    apply_socket_event(
        &mut command_app,
        json!({"type":"system","content":"New conversation started."}),
    );
    assert!(command_app.input.is_empty());
    assert!(command_app.pending_outbound_write.is_none());
}

#[test]
fn renders_all_responsive_widths_with_test_backend() {
    let options = TuiOptions {
        path: Some(PathBuf::from(".")),
        session: None,
        port: 18989,
        language: UiLanguage::ZhCn,
        theme: UiTheme::Dark,
    };
    let session = SessionSummary {
        id: "main".into(),
        name: "Main".into(),
        workspace: Some(WorkspaceSummary {
            kind: "directory".into(),
            path: "E:\\work\\project".into(),
            available: true,
        }),
    };
    for width in [140, 100, 70] {
        let mut app = App::new(
            &options,
            session.clone(),
            vec![session.clone()],
            true,
            "fallback".into(),
        );
        app.push(
            "assistant",
            "# 标题\n正文\n```rust\nfn main() {}\n```",
            LineKind::Assistant,
        );
        let backend = TestBackend::new(width, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        assert_eq!(terminal.backend().buffer().area.width, width);
    }
}

#[test]
fn medium_layout_renders_the_selected_non_chat_page() {
    let mut app = test_app(false);
    app.page_index = app
        .pages
        .iter()
        .position(|page| *page == Page::Models)
        .unwrap();
    app.inspector = "MODEL_PAGE_SENTINEL".into();
    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| render(frame, &mut app)).unwrap();
    let rendered = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("MODEL_PAGE_SENTINEL"));
}

#[test]
fn model_page_selects_effort_and_returns_an_atomic_update() {
    let mut app = test_app(false);
    app.page_index = app
        .pages
        .iter()
        .position(|page| *page == Page::Models)
        .unwrap();
    app.focus = Focus::Content;
    apply_loaded_page(
        &mut app,
        Page::Models,
        LoadedPage {
            payload: Some(json!({
                "session": {"model":"provider/first","effort":"medium"},
                "models": [
                    {"ref":"provider/first","provider":"provider","name":"First","input":["text"],"efforts":["low","medium"],"defaultEffort":"low"},
                    {"ref":"provider/second","provider":"provider","name":"Second","input":["text","image"],"efforts":["low","high"],"defaultEffort":"high"}
                ]
            })),
            fallback: String::new(),
        },
    );
    assert_eq!(app.inspector_index, 0);
    assert_eq!(app.inspector_choice, 1);

    handle_content_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(app.inspector_index, 1);
    assert_eq!(app.inspector_choice, 1);
    handle_content_key(&mut app, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));

    assert!(matches!(
        handle_content_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        UserAction::MutatePage(PageMutation::Model { model, effort })
            if model == "provider/second" && effort == "low"
    ));
    assert!(app.inspector.contains("Second"));
}

#[test]
fn skills_and_mcp_pages_emit_complete_policy_updates() {
    let mut app = test_app(false);
    app.page_index = app
        .pages
        .iter()
        .position(|page| *page == Page::Skills)
        .unwrap();
    apply_loaded_page(
        &mut app,
        Page::Skills,
        LoadedPage {
            payload: Some(json!({
                "skills": [
                    {"id":"docs/pdf","name":"PDF","enabled":false},
                    {"id":"docs/xlsx","name":"XLSX","enabled":true}
                ]
            })),
            fallback: String::new(),
        },
    );
    assert!(matches!(
        handle_content_key(&mut app, KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)),
        UserAction::MutatePage(PageMutation::Skills {
            enabled_system_skills,
            known_system_skills,
        }) if enabled_system_skills == vec!["docs/pdf", "docs/xlsx"]
            && known_system_skills == vec!["docs/pdf", "docs/xlsx"]
    ));

    app.page_index = app
        .pages
        .iter()
        .position(|page| *page == Page::Mcp)
        .unwrap();
    apply_loaded_page(
        &mut app,
        Page::Mcp,
        LoadedPage {
            payload: Some(json!({
                "policy": {"enabledServers":[],"enabledTools":[],"confirmMutatingTools":true,"clientCapabilities":{"roots":true}},
                "servers": [{"id":"files","name":"files","configuredEnabled":true,"enabled":false}],
                "tools": [{"id":"mcp__files__read","name":"read","server":"files","enabled":false}]
            })),
            fallback: String::new(),
        },
    );
    let UserAction::MutatePage(PageMutation::McpPolicy(policy)) = handle_content_key(
        &mut app,
        KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
    ) else {
        panic!("MCP server toggle should emit a policy update");
    };
    assert_eq!(policy["enabledServers"], json!(["files"]));
    assert_eq!(policy["enabledTools"], json!([]));
    assert_eq!(policy["confirmMutatingTools"], true);
    assert_eq!(policy["clientCapabilities"]["roots"], true);
}

#[test]
fn todos_page_cycles_removes_and_adds_items_without_raw_json_editing() {
    let mut app = test_app(false);
    app.page_index = app
        .pages
        .iter()
        .position(|page| *page == Page::Todos)
        .unwrap();
    apply_socket_event(
        &mut app,
        json!({
            "type":"todos_state",
            "revision":4,
            "items":[{"id":"todo-1","content":"Ship TUI","status":"pending"}]
        }),
    );
    assert!(app.inspector.contains("Ship TUI"));
    assert!(!app.inspector.contains("\"items\""));

    let UserAction::MutatePage(PageMutation::Todos {
        base_revision,
        items,
    }) = handle_content_key(
        &mut app,
        KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
    )
    else {
        panic!("Todo status should be editable from the page");
    };
    assert_eq!(base_revision, 4);
    assert_eq!(items[0]["status"], "in_progress");

    app.input = "/todo add Update docs".into();
    let UserAction::MutatePage(PageMutation::Todos { items, .. }) =
        handle_composer_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
    else {
        panic!("Todo add command should emit a page mutation");
    };
    assert_eq!(items.len(), 2);
    assert_eq!(items[1]["content"], "Update docs");
}

#[test]
fn usage_and_settings_pages_render_operational_summaries() {
    let mut app = test_app(false);
    app.page_index = app
        .pages
        .iter()
        .position(|page| *page == Page::Usage)
        .unwrap();
    let usage = format_usage_page(
        &app,
        &json!({
            "daily_input":10,"daily_output":4,"total_input":100,"total_output":40,
            "total_providers":{"openai":[60,20]},"total_roles":{"primary":[40,10]}
        }),
    );
    assert!(usage.contains("Today"));
    assert!(usage.contains("openai: 60 in · 20 out"));
    assert!(matches!(
        handle_content_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE)
        ),
        UserAction::Load(Page::Usage)
    ));
    let settings = format_settings_page(
        &app,
        &json!({"config":{"settings":{"enableGroups":false},"models":{"providers":{"openai":{"models":[{"id":"gpt"}]}}},"agents":{"defaults":{"model":{"primary":"openai/gpt"}}}}}),
    );
    assert!(settings.contains("Providers: 1"));
    assert!(settings.contains("Primary Agent: openai/gpt"));
    assert!(!settings.contains("\"providers\""));
}
