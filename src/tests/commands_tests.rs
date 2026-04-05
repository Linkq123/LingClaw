use super::*;
use std::{
    collections::{HashMap, HashSet},
    sync::atomic::AtomicU64,
    time::Duration,
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

fn unique_temp_workspace(prefix: &str) -> std::path::PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time should be after unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("{prefix}-{unique}"))
}

#[tokio::test]
async fn append_daily_memory_entry_creates_new_file_with_header() {
    let workspace = unique_temp_workspace("lingclaw-command-memory-new");
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    let memory_dir = workspace.join("memory");
    tokio::fs::create_dir_all(&memory_dir)
        .await
        .expect("memory dir should be created");
    let memory_path = memory_dir.join("2026-03-19.md");

    append_daily_memory_entry(&memory_path, "2026-03-19", "09:30", "first summary")
        .await
        .expect("memory entry should be written");

    let content = tokio::fs::read_to_string(&memory_path)
        .await
        .expect("memory file should be readable");
    assert_eq!(
        content,
        "# 2026-03-19\n\n\n---\n\n## 09:30 Local\n\nfirst summary"
    );

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn append_daily_memory_entry_appends_without_overwriting_existing_content() {
    let workspace = unique_temp_workspace("lingclaw-command-memory-append");
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    let memory_dir = workspace.join("memory");
    tokio::fs::create_dir_all(&memory_dir)
        .await
        .expect("memory dir should be created");
    let memory_path = memory_dir.join("2026-03-19.md");

    tokio::fs::write(
        &memory_path,
        "# 2026-03-19\n\n\n---\n\n## 08:00 Local\n\nexisting summary",
    )
    .await
    .expect("seed memory file should be written");

    append_daily_memory_entry(&memory_path, "2026-03-19", "09:30", "next summary")
        .await
        .expect("memory entry should append");

    let content = tokio::fs::read_to_string(&memory_path)
        .await
        .expect("memory file should be readable");
    assert!(content.contains("## 08:00 Local\n\nexisting summary"));
    assert!(content.contains("## 09:30 Local\n\nnext summary"));
    assert_eq!(content.matches("# 2026-03-19").count(), 1);

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn status_command_reports_runtime_request_estimate() {
    let workspace = unique_temp_workspace("lingclaw-command-status");
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    tokio::fs::create_dir_all(&workspace)
        .await
        .expect("workspace should be created");
    prompts::init_session_prompt_files(&workspace);

    let mut providers = HashMap::new();
    providers.insert(
        "anthropic".to_string(),
        crate::config::JsonProviderConfig {
            base_url: "https://api.anthropic.com".to_string(),
            api_key: "anthropic-key".to_string(),
            api: "anthropic".to_string(),
            models: vec![crate::config::JsonModelEntry {
                id: "claude-sonnet-4-20250514".to_string(),
                name: None,
                reasoning: Some(false),
                input: None,
                cost: None,
                context_window: Some(200000),
                max_tokens: Some(8192),
                compat: None,
            }],
        },
    );

    let state = AppState {
        config: crate::Config {
            api_key: "env-key".to_string(),
            api_base: "https://fallback.example/v1".to_string(),
            model: "gpt-4o-mini".to_string(),
            fast_model: None,
            sub_agent_model: None,
            memory_model: None,
            provider: crate::Provider::OpenAI,
            anthropic_prompt_caching: false,
            providers,
            mcp_servers: HashMap::new(),
            port: crate::DEFAULT_PORT,
            max_context_tokens: 32000,
            exec_timeout: Duration::from_secs(30),
            tool_timeout: Duration::from_secs(30),
            max_output_bytes: 50 * 1024,
            max_file_bytes: 200 * 1024,
            openai_stream_include_usage: false,
            structured_memory: false,
        },
        http: reqwest::Client::new(),
        sessions: Mutex::new(HashMap::new()),
        active_connections: Mutex::new(HashMap::new()),
        session_clients: Mutex::new(HashMap::new()),
        live_rounds: Mutex::new(HashMap::new()),
        active_runs: Mutex::new(HashMap::new()),
        connection_cancels: Mutex::new(HashMap::new()),
        next_connection_id: AtomicU64::new(1),
        shutdown: CancellationToken::new(),
        shutdown_token: "test-shutdown-token".to_string(),
        hooks: crate::HookRegistry::new(),
        memory_queue: None,
    };

    let mut session = Session {
        id: "status-session".to_string(),
        name: "Status Session".to_string(),
        messages: Vec::new(),
        created_at: 0,
        updated_at: 0,
        tool_calls_count: 0,
        input_tokens: 0,
        output_tokens: 0,
        daily_input_tokens: 0,
        daily_output_tokens: 0,
        input_token_source: "estimated".to_string(),
        output_token_source: "estimated".to_string(),
        token_usage_day: prompts::current_local_snapshot().today(),
        model_override: Some("anthropic/claude-sonnet-4-20250514".to_string()),
        think_level: "medium".to_string(),
        show_react: true,
        show_tools: true,
        show_reasoning: true,
        disabled_system_skills: HashSet::new(),
        version: 4,
        workspace: workspace.clone(),
    };
    let model = session.effective_model(&state.config.model).to_string();
    session.messages.push(build_system_prompt(
        &state.config,
        &workspace,
        &model,
        &session.disabled_system_skills,
    ));
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("Summarize the current backend architecture.".into()),
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });

    state
        .sessions
        .lock()
        .await
        .insert(session.id.clone(), session);

    let result = handle_status_command("status-session", &state).await;

    assert_eq!(result.response_type, "system");
    assert!(result.response.contains("request_est:"));
    assert!(result.response.contains("request_status: ok"));
    assert!(result
        .response
        .contains("request_note: includes refreshed system prompt, built-in/runtime tool schemas, and runtime reasoning reserve"));

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn status_command_uses_live_round_for_auto_think_estimate() {
    let workspace = unique_temp_workspace("lingclaw-command-status-auto");
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    tokio::fs::create_dir_all(&workspace)
        .await
        .expect("workspace should be created");
    prompts::init_session_prompt_files(&workspace);

    let mut providers = HashMap::new();
    providers.insert(
        "openai".to_string(),
        crate::config::JsonProviderConfig {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: "openai-key".to_string(),
            api: "openai-completions".to_string(),
            models: vec![crate::config::JsonModelEntry {
                id: "gpt-4o-reasoner".to_string(),
                name: None,
                reasoning: Some(true),
                input: None,
                cost: None,
                context_window: Some(128000),
                max_tokens: Some(8192),
                compat: None,
            }],
        },
    );

    let state = AppState {
        config: crate::Config {
            api_key: "env-key".to_string(),
            api_base: "https://api.openai.com/v1".to_string(),
            model: "openai/gpt-4o-reasoner".to_string(),
            fast_model: None,
            sub_agent_model: None,
            memory_model: None,
            provider: crate::Provider::OpenAI,
            anthropic_prompt_caching: false,
            providers,
            mcp_servers: HashMap::new(),
            port: crate::DEFAULT_PORT,
            max_context_tokens: 32000,
            exec_timeout: Duration::from_secs(30),
            tool_timeout: Duration::from_secs(30),
            max_output_bytes: 50 * 1024,
            max_file_bytes: 200 * 1024,
            openai_stream_include_usage: false,
            structured_memory: false,
        },
        http: reqwest::Client::new(),
        sessions: Mutex::new(HashMap::new()),
        active_connections: Mutex::new(HashMap::new()),
        session_clients: Mutex::new(HashMap::new()),
        live_rounds: Mutex::new(HashMap::new()),
        active_runs: Mutex::new(HashMap::new()),
        connection_cancels: Mutex::new(HashMap::new()),
        next_connection_id: AtomicU64::new(1),
        shutdown: CancellationToken::new(),
        shutdown_token: "test-shutdown-token".to_string(),
        hooks: crate::HookRegistry::new(),
        memory_queue: None,
    };

    let mut session = Session {
        id: "status-auto".to_string(),
        name: "Status Auto".to_string(),
        messages: Vec::new(),
        created_at: 0,
        updated_at: 0,
        tool_calls_count: 0,
        input_tokens: 0,
        output_tokens: 0,
        daily_input_tokens: 0,
        daily_output_tokens: 0,
        input_token_source: "estimated".to_string(),
        output_token_source: "estimated".to_string(),
        token_usage_day: prompts::current_local_snapshot().today(),
        model_override: Some("openai/gpt-4o-reasoner".to_string()),
        think_level: "auto".to_string(),
        show_react: true,
        show_tools: true,
        show_reasoning: true,
        disabled_system_skills: HashSet::new(),
        version: 4,
        workspace: workspace.clone(),
    };
    let model = session.effective_model(&state.config.model).to_string();
    session.messages.push(build_system_prompt(
        &state.config,
        &workspace,
        &model,
        &session.disabled_system_skills,
    ));

    state
        .sessions
        .lock()
        .await
        .insert(session.id.clone(), session);
    state.live_rounds.lock().await.insert(
        "status-auto".to_string(),
        crate::LiveRoundState {
            cycle: Some(2),
            has_observation: true,
            ..Default::default()
        },
    );

    let result = handle_status_command("status-auto", &state).await;

    assert_eq!(result.response_type, "system");
    assert!(result.response.contains("think high"));

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn switch_command_is_blocked_in_single_session_mode() {
    let state = AppState {
        config: crate::Config {
            api_key: "env-key".to_string(),
            api_base: "https://api.openai.com/v1".to_string(),
            model: "gpt-4o-mini".to_string(),
            fast_model: None,
            sub_agent_model: None,
            memory_model: None,
            provider: crate::Provider::OpenAI,
            anthropic_prompt_caching: false,
            providers: HashMap::new(),
            mcp_servers: HashMap::new(),
            port: crate::DEFAULT_PORT,
            max_context_tokens: 32000,
            exec_timeout: Duration::from_secs(30),
            tool_timeout: Duration::from_secs(30),
            max_output_bytes: 50 * 1024,
            max_file_bytes: 200 * 1024,
            openai_stream_include_usage: false,
            structured_memory: false,
        },
        http: reqwest::Client::new(),
        sessions: Mutex::new(HashMap::new()),
        active_connections: Mutex::new(HashMap::new()),
        session_clients: Mutex::new(HashMap::new()),
        live_rounds: Mutex::new(HashMap::new()),
        active_runs: Mutex::new(HashMap::new()),
        connection_cancels: Mutex::new(HashMap::new()),
        next_connection_id: AtomicU64::new(1),
        shutdown: CancellationToken::new(),
        shutdown_token: "test-shutdown-token".to_string(),
        hooks: crate::HookRegistry::new(),
        memory_queue: None,
    };

    let (tx, _rx) = tokio::sync::mpsc::channel::<String>(4);
    let result = handle_command(
        "/switch another-session",
        MAIN_SESSION_ID,
        1,
        &state,
        &tx,
        &CancellationToken::new(),
    )
    .await
    .expect("command should resolve");

    assert!(
        result
            .response
            .contains("LingClaw only keeps the main session")
    );
}

#[tokio::test]
async fn memory_command_stats_reports_unavailable_without_runtime_queue() {
    let workspace = unique_temp_workspace("lingclaw-command-memory-stats");
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    tokio::fs::create_dir_all(&workspace)
        .await
        .expect("workspace should be created");
    prompts::init_session_prompt_files(&workspace);

    let state = AppState {
        config: crate::Config {
            api_key: "env-key".to_string(),
            api_base: "https://api.openai.com/v1".to_string(),
            model: "gpt-4o-mini".to_string(),
            fast_model: None,
            sub_agent_model: None,
            memory_model: None,
            provider: crate::Provider::OpenAI,
            anthropic_prompt_caching: false,
            providers: HashMap::new(),
            mcp_servers: HashMap::new(),
            port: crate::DEFAULT_PORT,
            max_context_tokens: 32000,
            exec_timeout: Duration::from_secs(30),
            tool_timeout: Duration::from_secs(30),
            max_output_bytes: 50 * 1024,
            max_file_bytes: 200 * 1024,
            openai_stream_include_usage: false,
            structured_memory: true,
        },
        http: reqwest::Client::new(),
        sessions: Mutex::new(HashMap::new()),
        active_connections: Mutex::new(HashMap::new()),
        session_clients: Mutex::new(HashMap::new()),
        live_rounds: Mutex::new(HashMap::new()),
        active_runs: Mutex::new(HashMap::new()),
        connection_cancels: Mutex::new(HashMap::new()),
        next_connection_id: AtomicU64::new(1),
        shutdown: CancellationToken::new(),
        shutdown_token: "test-shutdown-token".to_string(),
        hooks: crate::HookRegistry::new(),
        memory_queue: None,
    };

    let mut session = Session {
        id: MAIN_SESSION_ID.to_string(),
        name: "Main".to_string(),
        messages: Vec::new(),
        created_at: 0,
        updated_at: 0,
        tool_calls_count: 0,
        input_tokens: 0,
        output_tokens: 0,
        daily_input_tokens: 0,
        daily_output_tokens: 0,
        input_token_source: "estimated".to_string(),
        output_token_source: "estimated".to_string(),
        token_usage_day: prompts::current_local_snapshot().today(),
        model_override: None,
        think_level: "auto".to_string(),
        show_react: true,
        show_tools: true,
        show_reasoning: true,
        disabled_system_skills: HashSet::new(),
        version: 4,
        workspace: workspace.clone(),
    };
    let model = session.effective_model(&state.config.model).to_string();
    session.messages.push(build_system_prompt(
        &state.config,
        &workspace,
        &model,
        &session.disabled_system_skills,
    ));
    state
        .sessions
        .lock()
        .await
        .insert(MAIN_SESSION_ID.to_string(), session);

    let (tx, _rx) = tokio::sync::mpsc::channel::<String>(4);
    let result = handle_command(
        "/memory stats",
        MAIN_SESSION_ID,
        1,
        &state,
        &tx,
        &CancellationToken::new(),
    )
    .await
    .expect("command should resolve");

    assert_eq!(result.response_type, "system");
    assert!(result.response.contains("Memory Updater"));
    assert!(result.response.contains("unavailable in this process"));

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn memory_command_rejects_unknown_subcommand() {
    let workspace = unique_temp_workspace("lingclaw-command-memory-invalid");
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    tokio::fs::create_dir_all(&workspace)
        .await
        .expect("workspace should be created");
    prompts::init_session_prompt_files(&workspace);

    let state = AppState {
        config: crate::Config {
            api_key: "env-key".to_string(),
            api_base: "https://api.openai.com/v1".to_string(),
            model: "gpt-4o-mini".to_string(),
            fast_model: None,
            sub_agent_model: None,
            memory_model: None,
            provider: crate::Provider::OpenAI,
            anthropic_prompt_caching: false,
            providers: HashMap::new(),
            mcp_servers: HashMap::new(),
            port: crate::DEFAULT_PORT,
            max_context_tokens: 32000,
            exec_timeout: Duration::from_secs(30),
            tool_timeout: Duration::from_secs(30),
            max_output_bytes: 50 * 1024,
            max_file_bytes: 200 * 1024,
            openai_stream_include_usage: false,
            structured_memory: true,
        },
        http: reqwest::Client::new(),
        sessions: Mutex::new(HashMap::new()),
        active_connections: Mutex::new(HashMap::new()),
        session_clients: Mutex::new(HashMap::new()),
        live_rounds: Mutex::new(HashMap::new()),
        active_runs: Mutex::new(HashMap::new()),
        connection_cancels: Mutex::new(HashMap::new()),
        next_connection_id: AtomicU64::new(1),
        shutdown: CancellationToken::new(),
        shutdown_token: "test-shutdown-token".to_string(),
        hooks: crate::HookRegistry::new(),
        memory_queue: None,
    };

    let mut session = Session {
        id: MAIN_SESSION_ID.to_string(),
        name: "Main".to_string(),
        messages: Vec::new(),
        created_at: 0,
        updated_at: 0,
        tool_calls_count: 0,
        input_tokens: 0,
        output_tokens: 0,
        daily_input_tokens: 0,
        daily_output_tokens: 0,
        input_token_source: "estimated".to_string(),
        output_token_source: "estimated".to_string(),
        token_usage_day: prompts::current_local_snapshot().today(),
        model_override: None,
        think_level: "auto".to_string(),
        show_react: true,
        show_tools: true,
        show_reasoning: true,
        disabled_system_skills: HashSet::new(),
        version: 4,
        workspace: workspace.clone(),
    };
    let model = session.effective_model(&state.config.model).to_string();
    session.messages.push(build_system_prompt(
        &state.config,
        &workspace,
        &model,
        &session.disabled_system_skills,
    ));
    state
        .sessions
        .lock()
        .await
        .insert(MAIN_SESSION_ID.to_string(), session);

    let (tx, _rx) = tokio::sync::mpsc::channel::<String>(4);
    let result = handle_command(
        "/memory noisy",
        MAIN_SESSION_ID,
        1,
        &state,
        &tx,
        &CancellationToken::new(),
    )
    .await
    .expect("command should resolve");

    assert_eq!(result.response_type, "system");
    assert!(result.response.contains("Usage: /memory [stats|debug]"));

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}
