use super::*;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, atomic::AtomicU64},
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
        config: std::sync::Mutex::new(Arc::new(crate::Config {
            api_key: "env-key".to_string(),
            api_base: "https://fallback.example/v1".to_string(),
            model: "gpt-4o-mini".to_string(),
            fast_model: None,
            sub_agent_model: None,
            sub_agent_model_overrides: Default::default(),
            memory_model: None,

            reflection_model: None,
            context_model: None,
            provider: crate::Provider::OpenAI,
            anthropic_prompt_caching: false,
            providers,
            mcp_servers: HashMap::new(),
            port: crate::DEFAULT_PORT,
            max_context_tokens: 32000,
            exec_timeout: Duration::from_secs(30),
            tool_timeout: Duration::from_secs(30),
            sub_agent_timeout: Duration::from_secs(300),
            max_llm_retries: 2,
            max_output_bytes: 50 * 1024,
            max_file_bytes: 200 * 1024,
            openai_stream_include_usage: false,
            structured_memory: false,

            daily_reflection: false,
            s3: None,
        })),
        http: reqwest::Client::new(),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        active_connections: Mutex::new(HashMap::new()),
        session_clients: Mutex::new(HashMap::new()),
        live_rounds: Mutex::new(HashMap::new()),
        active_runs: Mutex::new(HashMap::new()),
        connection_cancels: Mutex::new(HashMap::new()),
        next_connection_id: AtomicU64::new(1),
        shutdown: CancellationToken::new(),
        shutdown_token: "test-shutdown-token".to_string(),
        upload_token: "test-upload-token".to_string(),
        hooks: crate::HookRegistry::new(),
        memory_queue: std::sync::Mutex::new(None),
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
        daily_provider_usage: HashMap::new(),
        total_label_usage: HashMap::new(),
        usage_history: Vec::new(),
        model_override: Some("anthropic/claude-sonnet-4-20250514".to_string()),
        think_level: "medium".to_string(),
        show_react: true,
        show_tools: true,
        show_reasoning: true,
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::new(),
        version: 4,
        workspace: workspace.clone(),
    };
    let config = state.config();
    let model = session.effective_model(&config.model).to_string();
    session.messages.push(build_system_prompt(
        &config,
        &workspace,
        &model,
        &session.disabled_system_skills,
    ));
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("Summarize the current backend architecture.".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
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
        config: std::sync::Mutex::new(Arc::new(crate::Config {
            api_key: "env-key".to_string(),
            api_base: "https://api.openai.com/v1".to_string(),
            model: "openai/gpt-4o-reasoner".to_string(),
            fast_model: None,
            sub_agent_model: None,
            sub_agent_model_overrides: Default::default(),
            memory_model: None,

            reflection_model: None,
            context_model: None,
            provider: crate::Provider::OpenAI,
            anthropic_prompt_caching: false,
            providers,
            mcp_servers: HashMap::new(),
            port: crate::DEFAULT_PORT,
            max_context_tokens: 32000,
            exec_timeout: Duration::from_secs(30),
            tool_timeout: Duration::from_secs(30),
            sub_agent_timeout: Duration::from_secs(300),
            max_llm_retries: 2,
            max_output_bytes: 50 * 1024,
            max_file_bytes: 200 * 1024,
            openai_stream_include_usage: false,
            structured_memory: false,

            daily_reflection: false,
            s3: None,
        })),
        http: reqwest::Client::new(),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        active_connections: Mutex::new(HashMap::new()),
        session_clients: Mutex::new(HashMap::new()),
        live_rounds: Mutex::new(HashMap::new()),
        active_runs: Mutex::new(HashMap::new()),
        connection_cancels: Mutex::new(HashMap::new()),
        next_connection_id: AtomicU64::new(1),
        shutdown: CancellationToken::new(),
        shutdown_token: "test-shutdown-token".to_string(),
        upload_token: "test-upload-token".to_string(),
        hooks: crate::HookRegistry::new(),
        memory_queue: std::sync::Mutex::new(None),
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
        daily_provider_usage: HashMap::new(),
        total_label_usage: HashMap::new(),
        usage_history: Vec::new(),
        model_override: Some("openai/gpt-4o-reasoner".to_string()),
        think_level: "auto".to_string(),
        show_react: true,
        show_tools: true,
        show_reasoning: true,
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::new(),
        version: 4,
        workspace: workspace.clone(),
    };
    let config = state.config();
    let model = session.effective_model(&config.model).to_string();
    session.messages.push(build_system_prompt(
        &config,
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
async fn system_prompt_command_returns_current_prompt_and_token_estimate() {
    let workspace = unique_temp_workspace("lingclaw-command-system-prompt");
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    tokio::fs::create_dir_all(&workspace)
        .await
        .expect("workspace should be created");
    prompts::init_session_prompt_files(&workspace);

    let state = AppState {
        config: std::sync::Mutex::new(Arc::new(crate::Config {
            api_key: "env-key".to_string(),
            api_base: "https://api.openai.com/v1".to_string(),
            model: "gpt-4o-mini".to_string(),
            fast_model: None,
            sub_agent_model: None,
            sub_agent_model_overrides: Default::default(),
            memory_model: None,

            reflection_model: None,
            context_model: None,
            provider: crate::Provider::OpenAI,
            anthropic_prompt_caching: false,
            providers: HashMap::new(),
            mcp_servers: HashMap::new(),
            port: crate::DEFAULT_PORT,
            max_context_tokens: 32000,
            exec_timeout: Duration::from_secs(30),
            tool_timeout: Duration::from_secs(30),
            sub_agent_timeout: Duration::from_secs(300),
            max_llm_retries: 2,
            max_output_bytes: 50 * 1024,
            max_file_bytes: 200 * 1024,
            openai_stream_include_usage: false,
            structured_memory: false,

            daily_reflection: false,
            s3: None,
        })),
        http: reqwest::Client::new(),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        active_connections: Mutex::new(HashMap::new()),
        session_clients: Mutex::new(HashMap::new()),
        live_rounds: Mutex::new(HashMap::new()),
        active_runs: Mutex::new(HashMap::new()),
        connection_cancels: Mutex::new(HashMap::new()),
        next_connection_id: AtomicU64::new(1),
        shutdown: CancellationToken::new(),
        shutdown_token: "test-shutdown-token".to_string(),
        upload_token: "test-upload-token".to_string(),
        hooks: crate::HookRegistry::new(),
        memory_queue: std::sync::Mutex::new(None),
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
        daily_provider_usage: HashMap::new(),
        total_label_usage: HashMap::new(),
        usage_history: Vec::new(),
        model_override: None,
        think_level: "auto".to_string(),
        show_react: true,
        show_tools: true,
        show_reasoning: true,
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::new(),
        version: 4,
        workspace: workspace.clone(),
    };
    let config = state.config();
    let model = session.effective_model(&config.model).to_string();
    session.messages.push(build_system_prompt(
        &config,
        &workspace,
        &model,
        &session.disabled_system_skills,
    ));
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("Explain the current runtime architecture.".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });

    state
        .sessions
        .lock()
        .await
        .insert(MAIN_SESSION_ID.to_string(), session);

    let (tx, _rx) = tokio::sync::mpsc::channel::<String>(4);
    let result = handle_command(
        "/system-prompt",
        MAIN_SESSION_ID,
        1,
        &state,
        &tx,
        &CancellationToken::new(),
    )
    .await
    .expect("command should resolve");

    assert_eq!(result.response_type, "system");
    assert!(result.response.contains("Current system prompt"));
    assert!(result.response.contains("estimated_tokens:"));
    assert!(result.response.contains("provider: openai"));
    assert!(result.response.contains("## Environment"));
    assert!(result.response.contains("─"));

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn switch_command_is_blocked_in_single_session_mode() {
    let state = AppState {
        config: std::sync::Mutex::new(Arc::new(crate::Config {
            api_key: "env-key".to_string(),
            api_base: "https://api.openai.com/v1".to_string(),
            model: "gpt-4o-mini".to_string(),
            fast_model: None,
            sub_agent_model: None,
            sub_agent_model_overrides: Default::default(),
            memory_model: None,

            reflection_model: None,
            context_model: None,
            provider: crate::Provider::OpenAI,
            anthropic_prompt_caching: false,
            providers: HashMap::new(),
            mcp_servers: HashMap::new(),
            port: crate::DEFAULT_PORT,
            max_context_tokens: 32000,
            exec_timeout: Duration::from_secs(30),
            tool_timeout: Duration::from_secs(30),
            sub_agent_timeout: Duration::from_secs(300),
            max_llm_retries: 2,
            max_output_bytes: 50 * 1024,
            max_file_bytes: 200 * 1024,
            openai_stream_include_usage: false,
            structured_memory: false,

            daily_reflection: false,
            s3: None,
        })),
        http: reqwest::Client::new(),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        active_connections: Mutex::new(HashMap::new()),
        session_clients: Mutex::new(HashMap::new()),
        live_rounds: Mutex::new(HashMap::new()),
        active_runs: Mutex::new(HashMap::new()),
        connection_cancels: Mutex::new(HashMap::new()),
        next_connection_id: AtomicU64::new(1),
        shutdown: CancellationToken::new(),
        shutdown_token: "test-shutdown-token".to_string(),
        upload_token: "test-upload-token".to_string(),
        hooks: crate::HookRegistry::new(),
        memory_queue: std::sync::Mutex::new(None),
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
        config: std::sync::Mutex::new(Arc::new(crate::Config {
            api_key: "env-key".to_string(),
            api_base: "https://api.openai.com/v1".to_string(),
            model: "gpt-4o-mini".to_string(),
            fast_model: None,
            sub_agent_model: None,
            sub_agent_model_overrides: Default::default(),
            memory_model: None,

            reflection_model: None,
            context_model: None,
            provider: crate::Provider::OpenAI,
            anthropic_prompt_caching: false,
            providers: HashMap::new(),
            mcp_servers: HashMap::new(),
            port: crate::DEFAULT_PORT,
            max_context_tokens: 32000,
            exec_timeout: Duration::from_secs(30),
            tool_timeout: Duration::from_secs(30),
            sub_agent_timeout: Duration::from_secs(300),
            max_llm_retries: 2,
            max_output_bytes: 50 * 1024,
            max_file_bytes: 200 * 1024,
            openai_stream_include_usage: false,
            structured_memory: true,

            daily_reflection: false,
            s3: None,
        })),
        http: reqwest::Client::new(),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        active_connections: Mutex::new(HashMap::new()),
        session_clients: Mutex::new(HashMap::new()),
        live_rounds: Mutex::new(HashMap::new()),
        active_runs: Mutex::new(HashMap::new()),
        connection_cancels: Mutex::new(HashMap::new()),
        next_connection_id: AtomicU64::new(1),
        shutdown: CancellationToken::new(),
        shutdown_token: "test-shutdown-token".to_string(),
        upload_token: "test-upload-token".to_string(),
        hooks: crate::HookRegistry::new(),
        memory_queue: std::sync::Mutex::new(None),
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
        daily_provider_usage: HashMap::new(),
        total_label_usage: HashMap::new(),
        usage_history: Vec::new(),
        model_override: None,
        think_level: "auto".to_string(),
        show_react: true,
        show_tools: true,
        show_reasoning: true,
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::new(),
        version: 4,
        workspace: workspace.clone(),
    };
    let config = state.config();
    let model = session.effective_model(&config.model).to_string();
    session.messages.push(build_system_prompt(
        &config,
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
        config: std::sync::Mutex::new(Arc::new(crate::Config {
            api_key: "env-key".to_string(),
            api_base: "https://api.openai.com/v1".to_string(),
            model: "gpt-4o-mini".to_string(),
            fast_model: None,
            sub_agent_model: None,
            sub_agent_model_overrides: Default::default(),
            memory_model: None,

            reflection_model: None,
            context_model: None,
            provider: crate::Provider::OpenAI,
            anthropic_prompt_caching: false,
            providers: HashMap::new(),
            mcp_servers: HashMap::new(),
            port: crate::DEFAULT_PORT,
            max_context_tokens: 32000,
            exec_timeout: Duration::from_secs(30),
            tool_timeout: Duration::from_secs(30),
            sub_agent_timeout: Duration::from_secs(300),
            max_llm_retries: 2,
            max_output_bytes: 50 * 1024,
            max_file_bytes: 200 * 1024,
            openai_stream_include_usage: false,
            structured_memory: true,

            daily_reflection: false,
            s3: None,
        })),
        http: reqwest::Client::new(),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        active_connections: Mutex::new(HashMap::new()),
        session_clients: Mutex::new(HashMap::new()),
        live_rounds: Mutex::new(HashMap::new()),
        active_runs: Mutex::new(HashMap::new()),
        connection_cancels: Mutex::new(HashMap::new()),
        next_connection_id: AtomicU64::new(1),
        shutdown: CancellationToken::new(),
        shutdown_token: "test-shutdown-token".to_string(),
        upload_token: "test-upload-token".to_string(),
        hooks: crate::HookRegistry::new(),
        memory_queue: std::sync::Mutex::new(None),
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
        daily_provider_usage: HashMap::new(),
        total_label_usage: HashMap::new(),
        usage_history: Vec::new(),
        model_override: None,
        think_level: "auto".to_string(),
        show_react: true,
        show_tools: true,
        show_reasoning: true,
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::new(),
        version: 4,
        workspace: workspace.clone(),
    };
    let config = state.config();
    let model = session.effective_model(&config.model).to_string();
    session.messages.push(build_system_prompt(
        &config,
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

// ── /reflection tests ────────────────────────────────────────────────────────

#[tokio::test]
async fn reflection_command_disabled_shows_hint() {
    let _guard = crate::runtime_loop::reflection_test_guard().lock().await;
    let workspace = unique_temp_workspace("lingclaw-cmd-reflect-disabled");
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    tokio::fs::create_dir_all(&workspace)
        .await
        .expect("workspace should be created");

    let mut providers = HashMap::new();
    providers.insert(
        "openai".to_string(),
        crate::config::JsonProviderConfig {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: "test-key".to_string(),
            api: "openai-completions".to_string(),
            models: vec![crate::config::JsonModelEntry {
                id: "gpt-4o-mini".to_string(),
                name: None,
                reasoning: Some(false),
                input: None,
                cost: None,
                context_window: Some(128000),
                max_tokens: Some(16384),
                compat: None,
            }],
        },
    );

    let state = AppState {
        config: std::sync::Mutex::new(Arc::new(crate::Config {
            api_key: "test-key".to_string(),
            api_base: "https://api.openai.com/v1".to_string(),
            model: "gpt-4o-mini".to_string(),
            fast_model: None,
            sub_agent_model: None,
            sub_agent_model_overrides: Default::default(),
            memory_model: None,
            reflection_model: None,
            context_model: None,
            provider: crate::Provider::OpenAI,
            anthropic_prompt_caching: false,
            providers,
            mcp_servers: HashMap::new(),
            port: crate::DEFAULT_PORT,
            max_context_tokens: 32000,
            exec_timeout: Duration::from_secs(30),
            tool_timeout: Duration::from_secs(30),
            sub_agent_timeout: Duration::from_secs(300),
            max_llm_retries: 2,
            max_output_bytes: 50 * 1024,
            max_file_bytes: 200 * 1024,
            openai_stream_include_usage: false,
            structured_memory: false,
            daily_reflection: false,
            s3: None,
        })),
        http: reqwest::Client::new(),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        active_connections: Mutex::new(HashMap::new()),
        session_clients: Mutex::new(HashMap::new()),
        live_rounds: Mutex::new(HashMap::new()),
        active_runs: Mutex::new(HashMap::new()),
        connection_cancels: Mutex::new(HashMap::new()),
        next_connection_id: AtomicU64::new(1),
        shutdown: CancellationToken::new(),
        shutdown_token: "test-shutdown-token".to_string(),
        upload_token: "test-upload-token".to_string(),
        hooks: crate::HookRegistry::new(),
        memory_queue: std::sync::Mutex::new(None),
    };

    let mut session = Session {
        id: MAIN_SESSION_ID.to_string(),
        name: "main".to_string(),
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
        daily_provider_usage: HashMap::new(),
        total_label_usage: HashMap::new(),
        usage_history: Vec::new(),
        model_override: None,
        think_level: "auto".to_string(),
        show_react: false,
        show_tools: true,
        show_reasoning: true,
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::new(),
        version: 4,
        workspace: workspace.clone(),
    };
    let config = state.config();
    session.messages.push(build_system_prompt(
        &config,
        &workspace,
        &config.model,
        &session.disabled_system_skills,
    ));
    state
        .sessions
        .lock()
        .await
        .insert(MAIN_SESSION_ID.to_string(), session);
    state.apply_runtime_config(config.as_ref().clone());

    let (tx, _rx) = tokio::sync::mpsc::channel::<String>(4);
    let result = handle_command(
        "/reflection",
        MAIN_SESSION_ID,
        1,
        &state,
        &tx,
        &CancellationToken::new(),
    )
    .await
    .expect("command should resolve");

    assert_eq!(result.response_type, "system");
    assert!(result.response.contains("disabled"));

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn reflection_command_reads_runtime_daily_reflection_updates() {
    let _guard = crate::runtime_loop::reflection_test_guard().lock().await;
    let workspace = unique_temp_workspace("lingclaw-cmd-reflect-runtime-update");
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    tokio::fs::create_dir_all(&workspace)
        .await
        .expect("workspace should be created");

    let mut providers = HashMap::new();
    providers.insert(
        "openai".to_string(),
        crate::config::JsonProviderConfig {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: "test-key".to_string(),
            api: "openai-completions".to_string(),
            models: vec![crate::config::JsonModelEntry {
                id: "gpt-4o-mini".to_string(),
                name: None,
                reasoning: Some(false),
                input: None,
                cost: None,
                context_window: Some(128000),
                max_tokens: Some(16384),
                compat: None,
            }],
        },
    );

    let base_config = crate::Config {
        api_key: "test-key".to_string(),
        api_base: "https://api.openai.com/v1".to_string(),
        model: "gpt-4o-mini".to_string(),
        fast_model: None,
        sub_agent_model: None,
        sub_agent_model_overrides: Default::default(),
        memory_model: None,
        reflection_model: None,
        context_model: None,
        provider: crate::Provider::OpenAI,
        anthropic_prompt_caching: false,
        providers,
        mcp_servers: HashMap::new(),
        port: crate::DEFAULT_PORT,
        max_context_tokens: 32000,
        exec_timeout: Duration::from_secs(30),
        tool_timeout: Duration::from_secs(30),
        sub_agent_timeout: Duration::from_secs(300),
        max_llm_retries: 2,
        max_output_bytes: 50 * 1024,
        max_file_bytes: 200 * 1024,
        openai_stream_include_usage: false,
        structured_memory: false,
        daily_reflection: false,
        s3: None,
    };

    let state = AppState {
        config: std::sync::Mutex::new(Arc::new(base_config.clone())),
        http: reqwest::Client::new(),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        active_connections: Mutex::new(HashMap::new()),
        session_clients: Mutex::new(HashMap::new()),
        live_rounds: Mutex::new(HashMap::new()),
        active_runs: Mutex::new(HashMap::new()),
        connection_cancels: Mutex::new(HashMap::new()),
        next_connection_id: AtomicU64::new(1),
        shutdown: CancellationToken::new(),
        shutdown_token: "test-shutdown-token".to_string(),
        upload_token: "test-upload-token".to_string(),
        hooks: crate::HookRegistry::new(),
        memory_queue: std::sync::Mutex::new(None),
    };

    let mut session = Session {
        id: MAIN_SESSION_ID.to_string(),
        name: "main".to_string(),
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
        daily_provider_usage: HashMap::new(),
        total_label_usage: HashMap::new(),
        usage_history: Vec::new(),
        model_override: None,
        think_level: "auto".to_string(),
        show_react: false,
        show_tools: true,
        show_reasoning: true,
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::new(),
        version: 4,
        workspace: workspace.clone(),
    };
    let config = state.config();
    session.messages.push(build_system_prompt(
        &config,
        &workspace,
        &config.model,
        &session.disabled_system_skills,
    ));
    state
        .sessions
        .lock()
        .await
        .insert(MAIN_SESSION_ID.to_string(), session);
    state.apply_runtime_config(base_config.clone());

    let (tx, _rx) = tokio::sync::mpsc::channel::<String>(4);
    let disabled = handle_command(
        "/reflection",
        MAIN_SESSION_ID,
        1,
        &state,
        &tx,
        &CancellationToken::new(),
    )
    .await
    .expect("disabled reflection command should resolve");
    assert!(disabled.response.contains("disabled"));

    let mut enabled_config = base_config;
    enabled_config.daily_reflection = true;
    state.replace_config(enabled_config.clone());

    let config_only = handle_command(
        "/reflection",
        MAIN_SESSION_ID,
        1,
        &state,
        &tx,
        &CancellationToken::new(),
    )
    .await
    .expect("config-only reflection command should resolve");
    assert!(config_only.response.contains("disabled"));

    state.apply_runtime_config(enabled_config);

    let enabled = handle_command(
        "/reflection",
        MAIN_SESSION_ID,
        1,
        &state,
        &tx,
        &CancellationToken::new(),
    )
    .await
    .expect("enabled reflection command should resolve");
    assert!(enabled.response.contains("enabled"));
    assert!(enabled.response.contains("Last reflection:"));

    state.apply_runtime_config(config.as_ref().clone());
    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn reflection_command_disabled_allows_read_today() {
    let workspace = unique_temp_workspace("lingclaw-cmd-reflect-disabled-read");
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    let memory_dir = workspace.join("memory");
    tokio::fs::create_dir_all(&memory_dir)
        .await
        .expect("memory dir should be created");

    // Write a historical daily memory file for today.
    let local = prompts::current_local_snapshot();
    let today = local.today();
    let path = memory_dir.join(format!("{today}.md"));
    tokio::fs::write(
        &path,
        "## 14:00 Local — Reflection (4 cycles, 2 tools)\n\n- Historical insight",
    )
    .await
    .expect("write should succeed");

    let mut providers = HashMap::new();
    providers.insert(
        "openai".to_string(),
        crate::config::JsonProviderConfig {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: "test-key".to_string(),
            api: "openai-completions".to_string(),
            models: vec![crate::config::JsonModelEntry {
                id: "gpt-4o-mini".to_string(),
                name: None,
                reasoning: Some(false),
                input: None,
                cost: None,
                context_window: Some(128000),
                max_tokens: Some(16384),
                compat: None,
            }],
        },
    );

    let state = AppState {
        config: std::sync::Mutex::new(Arc::new(crate::Config {
            api_key: "test-key".to_string(),
            api_base: "https://api.openai.com/v1".to_string(),
            model: "gpt-4o-mini".to_string(),
            fast_model: None,
            sub_agent_model: None,
            sub_agent_model_overrides: Default::default(),
            memory_model: None,
            reflection_model: None,
            context_model: None,
            provider: crate::Provider::OpenAI,
            anthropic_prompt_caching: false,
            providers,
            mcp_servers: HashMap::new(),
            port: crate::DEFAULT_PORT,
            max_context_tokens: 32000,
            exec_timeout: Duration::from_secs(30),
            tool_timeout: Duration::from_secs(30),
            sub_agent_timeout: Duration::from_secs(300),
            max_llm_retries: 2,
            max_output_bytes: 50 * 1024,
            max_file_bytes: 200 * 1024,
            openai_stream_include_usage: false,
            structured_memory: false,
            daily_reflection: false,
            s3: None,
        })),
        http: reqwest::Client::new(),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        active_connections: Mutex::new(HashMap::new()),
        session_clients: Mutex::new(HashMap::new()),
        live_rounds: Mutex::new(HashMap::new()),
        active_runs: Mutex::new(HashMap::new()),
        connection_cancels: Mutex::new(HashMap::new()),
        next_connection_id: AtomicU64::new(1),
        shutdown: CancellationToken::new(),
        shutdown_token: "test-shutdown-token".to_string(),
        upload_token: "test-upload-token".to_string(),
        hooks: crate::HookRegistry::new(),
        memory_queue: std::sync::Mutex::new(None),
    };

    let mut session = Session {
        id: MAIN_SESSION_ID.to_string(),
        name: "main".to_string(),
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
        daily_provider_usage: HashMap::new(),
        total_label_usage: HashMap::new(),
        usage_history: Vec::new(),
        model_override: None,
        think_level: "auto".to_string(),
        show_react: false,
        show_tools: true,
        show_reasoning: true,
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::new(),
        version: 4,
        workspace: workspace.clone(),
    };
    let config = state.config();
    session.messages.push(build_system_prompt(
        &config,
        &workspace,
        &config.model,
        &session.disabled_system_skills,
    ));
    state
        .sessions
        .lock()
        .await
        .insert(MAIN_SESSION_ID.to_string(), session);

    let (tx, _rx) = tokio::sync::mpsc::channel::<String>(4);
    // Even with daily_reflection disabled, `/reflection today` should still read history.
    let result = handle_command(
        "/reflection today",
        MAIN_SESSION_ID,
        1,
        &state,
        &tx,
        &CancellationToken::new(),
    )
    .await
    .expect("command should resolve");

    assert_eq!(result.response_type, "system");
    assert!(result.response.contains("Historical insight"));

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn reflection_command_enabled_shows_status() {
    let _guard = crate::runtime_loop::reflection_test_guard().lock().await;
    let workspace = unique_temp_workspace("lingclaw-cmd-reflect-enabled");
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    tokio::fs::create_dir_all(&workspace)
        .await
        .expect("workspace should be created");

    let mut providers = HashMap::new();
    providers.insert(
        "openai".to_string(),
        crate::config::JsonProviderConfig {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: "test-key".to_string(),
            api: "openai-completions".to_string(),
            models: vec![crate::config::JsonModelEntry {
                id: "gpt-4o-mini".to_string(),
                name: None,
                reasoning: Some(false),
                input: None,
                cost: None,
                context_window: Some(128000),
                max_tokens: Some(16384),
                compat: None,
            }],
        },
    );

    let state = AppState {
        config: std::sync::Mutex::new(Arc::new(crate::Config {
            api_key: "test-key".to_string(),
            api_base: "https://api.openai.com/v1".to_string(),
            model: "gpt-4o-mini".to_string(),
            fast_model: None,
            sub_agent_model: None,
            sub_agent_model_overrides: Default::default(),
            memory_model: None,
            reflection_model: None,
            context_model: None,
            provider: crate::Provider::OpenAI,
            anthropic_prompt_caching: false,
            providers,
            mcp_servers: HashMap::new(),
            port: crate::DEFAULT_PORT,
            max_context_tokens: 32000,
            exec_timeout: Duration::from_secs(30),
            tool_timeout: Duration::from_secs(30),
            sub_agent_timeout: Duration::from_secs(300),
            max_llm_retries: 2,
            max_output_bytes: 50 * 1024,
            max_file_bytes: 200 * 1024,
            openai_stream_include_usage: false,
            structured_memory: false,
            daily_reflection: true,
            s3: None,
        })),
        http: reqwest::Client::new(),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        active_connections: Mutex::new(HashMap::new()),
        session_clients: Mutex::new(HashMap::new()),
        live_rounds: Mutex::new(HashMap::new()),
        active_runs: Mutex::new(HashMap::new()),
        connection_cancels: Mutex::new(HashMap::new()),
        next_connection_id: AtomicU64::new(1),
        shutdown: CancellationToken::new(),
        shutdown_token: "test-shutdown-token".to_string(),
        upload_token: "test-upload-token".to_string(),
        hooks: crate::HookRegistry::new(),
        memory_queue: std::sync::Mutex::new(None),
    };

    let mut session = Session {
        id: MAIN_SESSION_ID.to_string(),
        name: "main".to_string(),
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
        daily_provider_usage: HashMap::new(),
        total_label_usage: HashMap::new(),
        usage_history: Vec::new(),
        model_override: None,
        think_level: "auto".to_string(),
        show_react: false,
        show_tools: true,
        show_reasoning: true,
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::new(),
        version: 4,
        workspace: workspace.clone(),
    };
    let config = state.config();
    session.messages.push(build_system_prompt(
        &config,
        &workspace,
        &config.model,
        &session.disabled_system_skills,
    ));
    state
        .sessions
        .lock()
        .await
        .insert(MAIN_SESSION_ID.to_string(), session);
    state.apply_runtime_config(config.as_ref().clone());

    let (tx, _rx) = tokio::sync::mpsc::channel::<String>(4);
    let result = handle_command(
        "/reflection",
        MAIN_SESSION_ID,
        1,
        &state,
        &tx,
        &CancellationToken::new(),
    )
    .await
    .expect("command should resolve");

    assert_eq!(result.response_type, "system");
    assert!(result.response.contains("enabled"));
    assert!(result.response.contains("Last reflection:"));

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn reflection_command_today_shows_content() {
    let workspace = unique_temp_workspace("lingclaw-cmd-reflect-today");
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    let memory_dir = workspace.join("memory");
    tokio::fs::create_dir_all(&memory_dir)
        .await
        .expect("memory dir should be created");

    // Write a fake daily memory file for today.
    let local = prompts::current_local_snapshot();
    let today = local.today();
    let path = memory_dir.join(format!("{today}.md"));
    tokio::fs::write(
        &path,
        "# Today\n\n## 10:00 Local — Reflection (5 cycles, 3 tools)\n\n- Good stuff",
    )
    .await
    .expect("write should succeed");

    let mut providers = HashMap::new();
    providers.insert(
        "openai".to_string(),
        crate::config::JsonProviderConfig {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: "test-key".to_string(),
            api: "openai-completions".to_string(),
            models: vec![crate::config::JsonModelEntry {
                id: "gpt-4o-mini".to_string(),
                name: None,
                reasoning: Some(false),
                input: None,
                cost: None,
                context_window: Some(128000),
                max_tokens: Some(16384),
                compat: None,
            }],
        },
    );

    let state = AppState {
        config: std::sync::Mutex::new(Arc::new(crate::Config {
            api_key: "test-key".to_string(),
            api_base: "https://api.openai.com/v1".to_string(),
            model: "gpt-4o-mini".to_string(),
            fast_model: None,
            sub_agent_model: None,
            sub_agent_model_overrides: Default::default(),
            memory_model: None,
            reflection_model: None,
            context_model: None,
            provider: crate::Provider::OpenAI,
            anthropic_prompt_caching: false,
            providers,
            mcp_servers: HashMap::new(),
            port: crate::DEFAULT_PORT,
            max_context_tokens: 32000,
            exec_timeout: Duration::from_secs(30),
            tool_timeout: Duration::from_secs(30),
            sub_agent_timeout: Duration::from_secs(300),
            max_llm_retries: 2,
            max_output_bytes: 50 * 1024,
            max_file_bytes: 200 * 1024,
            openai_stream_include_usage: false,
            structured_memory: false,
            daily_reflection: true,
            s3: None,
        })),
        http: reqwest::Client::new(),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        active_connections: Mutex::new(HashMap::new()),
        session_clients: Mutex::new(HashMap::new()),
        live_rounds: Mutex::new(HashMap::new()),
        active_runs: Mutex::new(HashMap::new()),
        connection_cancels: Mutex::new(HashMap::new()),
        next_connection_id: AtomicU64::new(1),
        shutdown: CancellationToken::new(),
        shutdown_token: "test-shutdown-token".to_string(),
        upload_token: "test-upload-token".to_string(),
        hooks: crate::HookRegistry::new(),
        memory_queue: std::sync::Mutex::new(None),
    };

    let mut session = Session {
        id: MAIN_SESSION_ID.to_string(),
        name: "main".to_string(),
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
        daily_provider_usage: HashMap::new(),
        total_label_usage: HashMap::new(),
        usage_history: Vec::new(),
        model_override: None,
        think_level: "auto".to_string(),
        show_react: false,
        show_tools: true,
        show_reasoning: true,
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::new(),
        version: 4,
        workspace: workspace.clone(),
    };
    let config = state.config();
    session.messages.push(build_system_prompt(
        &config,
        &workspace,
        &config.model,
        &session.disabled_system_skills,
    ));
    state
        .sessions
        .lock()
        .await
        .insert(MAIN_SESSION_ID.to_string(), session);

    let (tx, _rx) = tokio::sync::mpsc::channel::<String>(4);
    let result = handle_command(
        "/reflection today",
        MAIN_SESSION_ID,
        1,
        &state,
        &tx,
        &CancellationToken::new(),
    )
    .await
    .expect("command should resolve");

    assert_eq!(result.response_type, "system");
    assert!(result.response.contains("Good stuff"));

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn reflection_command_today_filters_out_new_summaries() {
    // Daily memory file has both /new compression summaries and reflection entries.
    // `/reflection today` should only show the reflection entry.
    let workspace = unique_temp_workspace("lingclaw-cmd-reflect-mixed");
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    let memory_dir = workspace.join("memory");
    tokio::fs::create_dir_all(&memory_dir)
        .await
        .expect("memory dir should be created");

    let local = prompts::current_local_snapshot();
    let today = local.today();
    let path = memory_dir.join(format!("{today}.md"));
    // Simulate a file with a /new summary followed by a reflection entry.
    let mixed_content = format!(
        "# {today}\n\n\
         ---\n\n\
         ## 09:30 Local\n\n\
         Conversation summary from /new command\n\n\
         ---\n\n\
         ## 10:15 Local \u{2014} Reflection (5 cycles, 3 tools)\n\n\
         - Learned about error handling patterns"
    );
    tokio::fs::write(&path, &mixed_content)
        .await
        .expect("write should succeed");

    let mut providers = HashMap::new();
    providers.insert(
        "openai".to_string(),
        crate::config::JsonProviderConfig {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: "test-key".to_string(),
            api: "openai-completions".to_string(),
            models: vec![crate::config::JsonModelEntry {
                id: "gpt-4o-mini".to_string(),
                name: None,
                reasoning: Some(false),
                input: None,
                cost: None,
                context_window: Some(128000),
                max_tokens: Some(16384),
                compat: None,
            }],
        },
    );

    let state = AppState {
        config: std::sync::Mutex::new(Arc::new(crate::Config {
            api_key: "test-key".to_string(),
            api_base: "https://api.openai.com/v1".to_string(),
            model: "gpt-4o-mini".to_string(),
            fast_model: None,
            sub_agent_model: None,
            sub_agent_model_overrides: Default::default(),
            memory_model: None,
            reflection_model: None,
            context_model: None,
            provider: crate::Provider::OpenAI,
            anthropic_prompt_caching: false,
            providers,
            mcp_servers: HashMap::new(),
            port: crate::DEFAULT_PORT,
            max_context_tokens: 32000,
            exec_timeout: Duration::from_secs(30),
            tool_timeout: Duration::from_secs(30),
            sub_agent_timeout: Duration::from_secs(300),
            max_llm_retries: 2,
            max_output_bytes: 50 * 1024,
            max_file_bytes: 200 * 1024,
            openai_stream_include_usage: false,
            structured_memory: false,
            daily_reflection: true,
            s3: None,
        })),
        http: reqwest::Client::new(),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        active_connections: Mutex::new(HashMap::new()),
        session_clients: Mutex::new(HashMap::new()),
        live_rounds: Mutex::new(HashMap::new()),
        active_runs: Mutex::new(HashMap::new()),
        connection_cancels: Mutex::new(HashMap::new()),
        next_connection_id: AtomicU64::new(1),
        shutdown: CancellationToken::new(),
        shutdown_token: "test-shutdown-token".to_string(),
        upload_token: "test-upload-token".to_string(),
        hooks: crate::HookRegistry::new(),
        memory_queue: std::sync::Mutex::new(None),
    };

    let mut session = Session {
        id: MAIN_SESSION_ID.to_string(),
        name: "main".to_string(),
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
        daily_provider_usage: HashMap::new(),
        total_label_usage: HashMap::new(),
        usage_history: Vec::new(),
        model_override: None,
        think_level: "auto".to_string(),
        show_react: false,
        show_tools: true,
        show_reasoning: true,
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::new(),
        version: 4,
        workspace: workspace.clone(),
    };
    let config = state.config();
    session.messages.push(build_system_prompt(
        &config,
        &workspace,
        &config.model,
        &session.disabled_system_skills,
    ));
    state
        .sessions
        .lock()
        .await
        .insert(MAIN_SESSION_ID.to_string(), session);

    let (tx, _rx) = tokio::sync::mpsc::channel::<String>(4);
    let result = handle_command(
        "/reflection today",
        MAIN_SESSION_ID,
        1,
        &state,
        &tx,
        &CancellationToken::new(),
    )
    .await
    .expect("command should resolve");

    assert_eq!(result.response_type, "system");
    // Should contain the reflection entry.
    assert!(result.response.contains("error handling patterns"));
    // Should NOT contain the /new compression summary.
    assert!(
        !result
            .response
            .contains("Conversation summary from /new command")
    );

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn reflection_command_today_preserves_horizontal_rules_in_body() {
    let workspace = unique_temp_workspace("lingclaw-cmd-reflect-hr");
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    let memory_dir = workspace.join("memory");
    tokio::fs::create_dir_all(&memory_dir)
        .await
        .expect("memory dir should be created");

    let local = prompts::current_local_snapshot();
    let today = local.today();
    let path = memory_dir.join(format!("{today}.md"));
    let content = format!(
        "# {today}\n\n\
         ---\n\n\
         ## 10:15 Local \u{2014} Reflection (5 cycles, 3 tools)\n\n\
         First paragraph\n\n\
         ---\n\n\
         Second paragraph after horizontal rule"
    );
    tokio::fs::write(&path, &content)
        .await
        .expect("write should succeed");

    let mut providers = HashMap::new();
    providers.insert(
        "openai".to_string(),
        crate::config::JsonProviderConfig {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: "test-key".to_string(),
            api: "openai-completions".to_string(),
            models: vec![crate::config::JsonModelEntry {
                id: "gpt-4o-mini".to_string(),
                name: None,
                reasoning: Some(false),
                input: None,
                cost: None,
                context_window: Some(128000),
                max_tokens: Some(16384),
                compat: None,
            }],
        },
    );

    let state = AppState {
        config: std::sync::Mutex::new(Arc::new(crate::Config {
            api_key: "test-key".to_string(),
            api_base: "https://api.openai.com/v1".to_string(),
            model: "gpt-4o-mini".to_string(),
            fast_model: None,
            sub_agent_model: None,
            sub_agent_model_overrides: Default::default(),
            memory_model: None,
            reflection_model: None,
            context_model: None,
            provider: crate::Provider::OpenAI,
            anthropic_prompt_caching: false,
            providers,
            mcp_servers: HashMap::new(),
            port: crate::DEFAULT_PORT,
            max_context_tokens: 32000,
            exec_timeout: Duration::from_secs(30),
            tool_timeout: Duration::from_secs(30),
            sub_agent_timeout: Duration::from_secs(300),
            max_llm_retries: 2,
            max_output_bytes: 50 * 1024,
            max_file_bytes: 200 * 1024,
            openai_stream_include_usage: false,
            structured_memory: false,
            daily_reflection: true,
            s3: None,
        })),
        http: reqwest::Client::new(),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        active_connections: Mutex::new(HashMap::new()),
        session_clients: Mutex::new(HashMap::new()),
        live_rounds: Mutex::new(HashMap::new()),
        active_runs: Mutex::new(HashMap::new()),
        connection_cancels: Mutex::new(HashMap::new()),
        next_connection_id: AtomicU64::new(1),
        shutdown: CancellationToken::new(),
        shutdown_token: "test-shutdown-token".to_string(),
        upload_token: "test-upload-token".to_string(),
        hooks: crate::HookRegistry::new(),
        memory_queue: std::sync::Mutex::new(None),
    };

    let mut session = Session {
        id: MAIN_SESSION_ID.to_string(),
        name: "main".to_string(),
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
        daily_provider_usage: HashMap::new(),
        total_label_usage: HashMap::new(),
        usage_history: Vec::new(),
        model_override: None,
        think_level: "auto".to_string(),
        show_react: false,
        show_tools: true,
        show_reasoning: true,
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::new(),
        version: 4,
        workspace: workspace.clone(),
    };
    let config = state.config();
    session.messages.push(build_system_prompt(
        &config,
        &workspace,
        &config.model,
        &session.disabled_system_skills,
    ));
    state
        .sessions
        .lock()
        .await
        .insert(MAIN_SESSION_ID.to_string(), session);

    let (tx, _rx) = tokio::sync::mpsc::channel::<String>(4);
    let result = handle_command(
        "/reflection today",
        MAIN_SESSION_ID,
        1,
        &state,
        &tx,
        &CancellationToken::new(),
    )
    .await
    .expect("command should resolve");

    assert_eq!(result.response_type, "system");
    assert!(result.response.contains("First paragraph"));
    assert!(
        result
            .response
            .contains("Second paragraph after horizontal rule")
    );

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn reflection_command_list_shows_only_files_with_reflections() {
    let workspace = unique_temp_workspace("lingclaw-cmd-reflect-list");
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    let memory_dir = workspace.join("memory");
    tokio::fs::create_dir_all(&memory_dir)
        .await
        .expect("memory dir should be created");

    // Create one summary-only file and two files that actually contain reflections.
    tokio::fs::write(
        memory_dir.join("2026-04-15.md"),
        "# 2026-04-15\n\n---\n\n## 09:30 Local\n\nsummary only",
    )
    .await
    .unwrap();
    tokio::fs::write(
        memory_dir.join("2026-04-16.md"),
        "# 2026-04-16\n\n---\n\n## 10:00 Local — Reflection (4 cycles, 2 tools)\n\n- first reflection",
    )
    .await
    .unwrap();
    tokio::fs::write(
        memory_dir.join("2026-04-17.md"),
        "# 2026-04-17\n\n---\n\n## 11:00 Local — Reflection (6 cycles, 1 tools)\n\n- second reflection",
    )
        .await
        .unwrap();

    let mut providers = HashMap::new();
    providers.insert(
        "openai".to_string(),
        crate::config::JsonProviderConfig {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: "test-key".to_string(),
            api: "openai-completions".to_string(),
            models: vec![crate::config::JsonModelEntry {
                id: "gpt-4o-mini".to_string(),
                name: None,
                reasoning: Some(false),
                input: None,
                cost: None,
                context_window: Some(128000),
                max_tokens: Some(16384),
                compat: None,
            }],
        },
    );

    let state = AppState {
        config: std::sync::Mutex::new(Arc::new(crate::Config {
            api_key: "test-key".to_string(),
            api_base: "https://api.openai.com/v1".to_string(),
            model: "gpt-4o-mini".to_string(),
            fast_model: None,
            sub_agent_model: None,
            sub_agent_model_overrides: Default::default(),
            memory_model: None,
            reflection_model: None,
            context_model: None,
            provider: crate::Provider::OpenAI,
            anthropic_prompt_caching: false,
            providers,
            mcp_servers: HashMap::new(),
            port: crate::DEFAULT_PORT,
            max_context_tokens: 32000,
            exec_timeout: Duration::from_secs(30),
            tool_timeout: Duration::from_secs(30),
            sub_agent_timeout: Duration::from_secs(300),
            max_llm_retries: 2,
            max_output_bytes: 50 * 1024,
            max_file_bytes: 200 * 1024,
            openai_stream_include_usage: false,
            structured_memory: false,
            daily_reflection: true,
            s3: None,
        })),
        http: reqwest::Client::new(),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        active_connections: Mutex::new(HashMap::new()),
        session_clients: Mutex::new(HashMap::new()),
        live_rounds: Mutex::new(HashMap::new()),
        active_runs: Mutex::new(HashMap::new()),
        connection_cancels: Mutex::new(HashMap::new()),
        next_connection_id: AtomicU64::new(1),
        shutdown: CancellationToken::new(),
        shutdown_token: "test-shutdown-token".to_string(),
        upload_token: "test-upload-token".to_string(),
        hooks: crate::HookRegistry::new(),
        memory_queue: std::sync::Mutex::new(None),
    };

    let mut session = Session {
        id: MAIN_SESSION_ID.to_string(),
        name: "main".to_string(),
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
        daily_provider_usage: HashMap::new(),
        total_label_usage: HashMap::new(),
        usage_history: Vec::new(),
        model_override: None,
        think_level: "auto".to_string(),
        show_react: false,
        show_tools: true,
        show_reasoning: true,
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::new(),
        version: 4,
        workspace: workspace.clone(),
    };
    let config = state.config();
    session.messages.push(build_system_prompt(
        &config,
        &workspace,
        &config.model,
        &session.disabled_system_skills,
    ));
    state
        .sessions
        .lock()
        .await
        .insert(MAIN_SESSION_ID.to_string(), session);

    let (tx, _rx) = tokio::sync::mpsc::channel::<String>(4);
    let result = handle_command(
        "/reflection list",
        MAIN_SESSION_ID,
        1,
        &state,
        &tx,
        &CancellationToken::new(),
    )
    .await
    .expect("command should resolve");

    assert_eq!(result.response_type, "system");
    assert!(result.response.contains("2 total"));
    assert!(result.response.contains("2026-04-17.md"));
    assert!(result.response.contains("2026-04-16.md"));
    assert!(!result.response.contains("2026-04-15.md"));

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn reflection_command_invalid_arg_shows_usage() {
    let workspace = unique_temp_workspace("lingclaw-cmd-reflect-invalid");
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    tokio::fs::create_dir_all(&workspace)
        .await
        .expect("workspace should be created");

    let mut providers = HashMap::new();
    providers.insert(
        "openai".to_string(),
        crate::config::JsonProviderConfig {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: "test-key".to_string(),
            api: "openai-completions".to_string(),
            models: vec![crate::config::JsonModelEntry {
                id: "gpt-4o-mini".to_string(),
                name: None,
                reasoning: Some(false),
                input: None,
                cost: None,
                context_window: Some(128000),
                max_tokens: Some(16384),
                compat: None,
            }],
        },
    );

    let state = AppState {
        config: std::sync::Mutex::new(Arc::new(crate::Config {
            api_key: "test-key".to_string(),
            api_base: "https://api.openai.com/v1".to_string(),
            model: "gpt-4o-mini".to_string(),
            fast_model: None,
            sub_agent_model: None,
            sub_agent_model_overrides: Default::default(),
            memory_model: None,
            reflection_model: None,
            context_model: None,
            provider: crate::Provider::OpenAI,
            anthropic_prompt_caching: false,
            providers,
            mcp_servers: HashMap::new(),
            port: crate::DEFAULT_PORT,
            max_context_tokens: 32000,
            exec_timeout: Duration::from_secs(30),
            tool_timeout: Duration::from_secs(30),
            sub_agent_timeout: Duration::from_secs(300),
            max_llm_retries: 2,
            max_output_bytes: 50 * 1024,
            max_file_bytes: 200 * 1024,
            openai_stream_include_usage: false,
            structured_memory: false,
            daily_reflection: true,
            s3: None,
        })),
        http: reqwest::Client::new(),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        active_connections: Mutex::new(HashMap::new()),
        session_clients: Mutex::new(HashMap::new()),
        live_rounds: Mutex::new(HashMap::new()),
        active_runs: Mutex::new(HashMap::new()),
        connection_cancels: Mutex::new(HashMap::new()),
        next_connection_id: AtomicU64::new(1),
        shutdown: CancellationToken::new(),
        shutdown_token: "test-shutdown-token".to_string(),
        upload_token: "test-upload-token".to_string(),
        hooks: crate::HookRegistry::new(),
        memory_queue: std::sync::Mutex::new(None),
    };

    let mut session = Session {
        id: MAIN_SESSION_ID.to_string(),
        name: "main".to_string(),
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
        daily_provider_usage: HashMap::new(),
        total_label_usage: HashMap::new(),
        usage_history: Vec::new(),
        model_override: None,
        think_level: "auto".to_string(),
        show_react: false,
        show_tools: true,
        show_reasoning: true,
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::new(),
        version: 4,
        workspace: workspace.clone(),
    };
    let config = state.config();
    session.messages.push(build_system_prompt(
        &config,
        &workspace,
        &config.model,
        &session.disabled_system_skills,
    ));
    state
        .sessions
        .lock()
        .await
        .insert(MAIN_SESSION_ID.to_string(), session);

    let (tx, _rx) = tokio::sync::mpsc::channel::<String>(4);
    let result = handle_command(
        "/reflection bogus",
        MAIN_SESSION_ID,
        1,
        &state,
        &tx,
        &CancellationToken::new(),
    )
    .await
    .expect("command should resolve");

    assert_eq!(result.response_type, "system");
    assert!(result.response.contains("Usage:"));

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}
