use super::*;
use crate::ChatMessage;
use std::path::PathBuf;
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

fn unique_session_id(prefix: &str) -> String {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time should be after unix epoch")
        .as_nanos();
    format!("{prefix}-{unique}")
}

fn test_config() -> crate::Config {
    crate::Config {
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
        enable_state_digest: true,
    }
}

#[tokio::test]
async fn think_command_waits_for_session_persist_gate_before_mutating_session() {
    let workspace = unique_temp_workspace("lingclaw-think-persist-gate");
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    tokio::fs::create_dir_all(&workspace)
        .await
        .expect("workspace should be created");
    prompts::init_session_prompt_files(&workspace);

    let session_id = unique_session_id("think-persist-gate");
    let state = Arc::new(AppState {
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
            enable_state_digest: true,
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
    });

    state.sessions.lock().await.insert(
        session_id.clone(),
        Session {
            id: session_id.clone(),
            name: "Persist Gate".to_string(),
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
        },
    );

    let persist_gate = crate::session_store::session_persist_gate(&session_id);
    let persist_guard = persist_gate.lock().await;

    let task_state = state.clone();
    let task_session_id = session_id.clone();
    let think_task = tokio::spawn(async move {
        handle_think_command("high", &task_session_id, task_state.as_ref()).await
    });

    tokio::task::yield_now().await;

    let current_think = {
        let sessions = state.sessions.lock().await;
        sessions
            .get(&session_id)
            .expect("session should exist")
            .think_level
            .clone()
    };
    assert_eq!(current_think, "auto");
    assert!(crate::session_store::load_session_from_disk(&session_id).is_none());

    drop(persist_guard);

    let result = think_task.await.expect("think task should complete");
    assert_eq!(result.response_type, "system");
    assert_eq!(result.response, "Think mode set to: high");

    let current_think = {
        let sessions = state.sessions.lock().await;
        sessions
            .get(&session_id)
            .expect("session should exist")
            .think_level
            .clone()
    };
    assert_eq!(current_think, "high");

    let persisted =
        crate::session_store::load_session_from_disk(&session_id).expect("session should persist");
    assert_eq!(persisted.think_level, "high");

    let _ = tokio::fs::remove_file(
        crate::session_store::sessions_dir().join(format!("{session_id}.json")),
    )
    .await;
    let _ = tokio::fs::remove_dir_all(&workspace).await;
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
                id: "claude-opus-4-7".to_string(),
                name: None,
                reasoning: Some(false),
                input: None,
                cost: None,
                context_window: Some(1_000_000),
                max_tokens: Some(64_000),
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
            enable_state_digest: true,
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
        model_override: Some("anthropic/claude-opus-4-7".to_string()),
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
        .contains("request_note: includes refreshed system prompt, built-in/runtime tool schemas, and runtime reply reserve"));

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn status_command_uses_runtime_auto_policy_for_idle_auto_sessions() {
    let workspace = unique_temp_workspace("lingclaw-command-status-idle-auto");
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
            enable_state_digest: true,
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
        id: "status-idle-auto".to_string(),
        name: "Status Idle Auto".to_string(),
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
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("Fix the parser to handle trailing commas.".into()),
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

    let result = handle_status_command("status-idle-auto", &state).await;

    assert_eq!(result.response_type, "system");
    assert!(result.response.contains("think high"));
    assert!(!result.response.contains("think medium"));
    assert!(result.response.contains(
        "auto_decision: selected=high baseline=high reason=initial_change escalators=none dampeners=none clamps=none"
    ));

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn status_command_reports_compression_recorded_before_start_event() {
    let workspace = unique_temp_workspace("lingclaw-command-status-compression-prestart");
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
            enable_state_digest: true,
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
        id: "status-compression-prestart".to_string(),
        name: "Status Compression Prestart".to_string(),
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
        "status-compression-prestart".to_string(),
        crate::LiveRoundState {
            cycle: Some(0),
            effective_model: Some("openai/gpt-4o-reasoner".to_string()),
            effective_think: Some("medium".to_string()),
            latest_compression: crate::LiveCompressionState {
                outcome: Some("skipped".to_string()),
                reason: Some("insufficient_savings".to_string()),
                messages_removed: None,
                before_estimate: None,
                after_estimate: None,
                saved_tokens: None,
                saved_percent: None,
                pruned_messages_removed: None,
            },
            ..Default::default()
        },
    );

    let result = handle_status_command("status-compression-prestart", &state).await;

    assert_eq!(result.response_type, "system");
    assert!(
        result
            .response
            .contains("compression: skipped reason=insufficient_savings")
    );

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn status_command_reports_prune_only_state() {
    let workspace = unique_temp_workspace("lingclaw-command-status-prune-only");
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
            enable_state_digest: true,
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
        id: "status-prune-only".to_string(),
        name: "Status Prune Only".to_string(),
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
        "status-prune-only".to_string(),
        crate::LiveRoundState {
            cycle: Some(5),
            effective_model: Some("openai/gpt-4o-reasoner".to_string()),
            effective_think: Some("high".to_string()),
            latest_compression: crate::LiveCompressionState {
                pruned_messages_removed: Some(3),
                ..Default::default()
            },
            ..Default::default()
        },
    );

    let result = handle_status_command("status-prune-only", &state).await;

    assert_eq!(result.response_type, "system");
    assert!(
        result
            .response
            .contains("pruned: removed 3 additional message(s) to fit request budget")
    );
    assert!(!result.response.contains("compression:"));

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn status_command_reports_replayed_compression_outcome_after_reconnect() {
    let workspace = unique_temp_workspace("lingclaw-command-status-compression-replay");
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
            enable_state_digest: true,
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
        id: "status-compression-replay".to_string(),
        name: "Status Compression Replay".to_string(),
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
        "status-compression-replay".to_string(),
        crate::LiveRoundState {
            cycle: Some(0),
            effective_model: Some("openai/gpt-4o-reasoner".to_string()),
            effective_think: Some("medium".to_string()),
            latest_compression: crate::LiveCompressionState {
                outcome: Some("compressed".to_string()),
                reason: None,
                messages_removed: Some(2),
                before_estimate: Some(4096),
                after_estimate: Some(3072),
                saved_tokens: Some(512),
                saved_percent: Some(12),
                pruned_messages_removed: None,
            },
            has_observation: false,
            ..Default::default()
        },
    );

    let result = handle_status_command("status-compression-replay", &state).await;

    assert_eq!(result.response_type, "system");
    assert!(
        result
            .response
            .contains("compression: compressed saved_tokens=512 saved_percent=12")
    );

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn status_command_reports_latest_compression_outcome() {
    let workspace = unique_temp_workspace("lingclaw-command-status-compression");
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
            enable_state_digest: true,
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
        id: "status-compression".to_string(),
        name: "Status Compression".to_string(),
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
        "status-compression".to_string(),
        crate::LiveRoundState {
            cycle: Some(2),
            effective_model: Some("openai/gpt-4o-reasoner".to_string()),
            effective_think: Some("high".to_string()),
            latest_compression: crate::LiveCompressionState {
                outcome: Some("compressed".to_string()),
                reason: None,
                messages_removed: Some(4),
                before_estimate: Some(5000),
                after_estimate: Some(4000),
                saved_tokens: Some(1024),
                saved_percent: Some(18),
                pruned_messages_removed: None,
            },
            ..Default::default()
        },
    );

    let result = handle_status_command("status-compression", &state).await;

    assert_eq!(result.response_type, "system");
    assert!(
        result
            .response
            .contains("compression: compressed saved_tokens=1024 saved_percent=18")
    );

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn status_command_prefers_live_round_effective_think() {
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
            enable_state_digest: true,
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
            effective_think: Some("xhigh".to_string()),
            auto_observation_strength: Some("strong".to_string()),
            auto_stagnation_streak: Some(1),
            auto_error_streak: Some(2),
            auto_task_pressure: Some(3),
            auto_action_oriented: Some("true".parse().expect("bool should parse")),
            auto_ready_to_finish: Some(false),
            auto_has_blocking_uncertainty: Some(true),
            has_observation: true,
            ..Default::default()
        },
    );

    let result = handle_status_command("status-auto", &state).await;

    assert_eq!(result.response_type, "system");
    assert!(result.response.contains(&format!("runtime_model: {model}")));
    assert!(result.response.contains("runtime_provider: openai"));
    assert!(result.response.contains("runtime_phase: analyze"));
    assert!(result.response.contains("runtime_cycle: 2"));
    assert!(result.response.contains("runtime_think: xhigh"));
    assert!(result.response.contains("think xhigh"));
    assert!(result.response.contains(
        "auto_signals: live cycles=2 obs=strong stagnation=1 errors=2 pressure=3 action=yes ready_signal=no blocked_signal=yes"
    ));

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn status_command_prefers_live_round_effective_think_for_manual_sessions() {
    let workspace = unique_temp_workspace("lingclaw-command-status-manual-live-think");
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
            enable_state_digest: true,
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
        id: "status-manual-live-think".to_string(),
        name: "Status Manual Live Think".to_string(),
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

    state
        .sessions
        .lock()
        .await
        .insert(session.id.clone(), session);
    state.live_rounds.lock().await.insert(
        "status-manual-live-think".to_string(),
        crate::LiveRoundState {
            cycle: Some(1),
            effective_model: Some("openai/gpt-4o-reasoner".to_string()),
            effective_think: Some("off".to_string()),
            phase: Some("analyze".to_string()),
            has_observation: false,
            ..Default::default()
        },
    );

    let result = handle_status_command("status-manual-live-think", &state).await;

    assert_eq!(result.response_type, "system");
    assert!(result.response.contains("runtime_think: off"));
    assert!(result.response.contains("think off"));
    assert!(!result.response.contains("runtime_think: medium"));

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn status_command_prefers_live_round_effective_think_over_base_model_support() {
    let workspace = unique_temp_workspace("lingclaw-command-status-live-fast-model");
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
            models: vec![
                crate::config::JsonModelEntry {
                    id: "gpt-4o-mini".to_string(),
                    name: None,
                    reasoning: Some(false),
                    input: None,
                    cost: None,
                    context_window: Some(128000),
                    max_tokens: Some(8192),
                    compat: None,
                },
                crate::config::JsonModelEntry {
                    id: "gpt-4o-reasoner".to_string(),
                    name: None,
                    reasoning: Some(true),
                    input: None,
                    cost: None,
                    context_window: Some(64000),
                    max_tokens: Some(2048),
                    compat: None,
                },
            ],
        },
    );

    let state = AppState {
        config: std::sync::Mutex::new(Arc::new(crate::Config {
            api_key: "env-key".to_string(),
            api_base: "https://api.openai.com/v1".to_string(),
            model: "openai/gpt-4o-mini".to_string(),
            fast_model: Some("openai/gpt-4o-reasoner".to_string()),
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
            enable_state_digest: true,
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
        id: "status-live-fast-model".to_string(),
        name: "Status Fast Model".to_string(),
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
        model_override: Some("openai/gpt-4o-mini".to_string()),
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
        "status-live-fast-model".to_string(),
        crate::LiveRoundState {
            cycle: Some(0),
            effective_model: Some("openai/gpt-4o-reasoner".to_string()),
            effective_think: Some("high".to_string()),
            auto_observation_strength: Some("none".to_string()),
            auto_stagnation_streak: Some(0),
            auto_error_streak: Some(0),
            auto_task_pressure: Some(2),
            auto_action_oriented: Some(true),
            auto_ready_to_finish: Some(false),
            auto_has_blocking_uncertainty: Some(false),
            has_observation: false,
            ..Default::default()
        },
    );

    let result = handle_status_command("status-live-fast-model", &state).await;
    let base_budget = crate::context::context_input_budget_for_runtime(&config, &model, "high");
    let live_budget =
        crate::context::context_input_budget_for_runtime(&config, "openai/gpt-4o-reasoner", "high");

    assert_eq!(result.response_type, "system");
    assert!(
        result
            .response
            .contains("runtime_model: openai/gpt-4o-reasoner")
    );
    assert!(result.response.contains("runtime_provider: openai"));
    assert!(
        result
            .response
            .contains("runtime_model_id: gpt-4o-reasoner")
    );
    assert!(result.response.contains("runtime_think: high"));
    assert!(result.response.contains("think high"));
    assert!(result.response.contains(
        "auto_signals: live cycles=0 obs=none stagnation=0 errors=0 pressure=2 action=yes ready_signal=no blocked_signal=no"
    ));
    assert!(!result.response.contains("auto_signals: unavailable"));
    assert_ne!(base_budget, live_budget);
    assert!(result.response.contains(&format!(
        "/{} (tools",
        crate::format_token_count(live_budget as u64)
    )));
    assert!(!result.response.contains(&format!(
        "/{} (tools",
        crate::format_token_count(base_budget as u64)
    )));

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn status_command_reports_latest_auto_trace_summary() {
    let workspace = unique_temp_workspace("lingclaw-command-status-auto-trace");
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
            enable_state_digest: true,
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
        id: "status-auto-trace".to_string(),
        name: "Status Auto Trace".to_string(),
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
        "status-auto-trace".to_string(),
        crate::LiveRoundState {
            cycle: Some(4),
            effective_model: Some("openai/gpt-4o-reasoner".to_string()),
            effective_think: Some("medium".to_string()),
            latest_auto_trace: Some(agent::AutoThinkTrace {
                round: 2,
                cycle: 4,
                phase: "analyze".to_string(),
                model: "openai/gpt-4o-reasoner".to_string(),
                provider: "openai".to_string(),
                selected_think: "high".to_string(),
                baseline_level: "medium".to_string(),
                baseline_reason: "mid_loop_investigate".to_string(),
                escalators: vec!["stagnation_streak".to_string()],
                dampeners: Vec::new(),
                clamps: Vec::new(),
                signals: agent::AutoThinkTraceSignals {
                    intent: "investigate".to_string(),
                    user_msg_chars: 96,
                    observation_strength: "medium".to_string(),
                    tool_results_count: 2,
                    tool_error_count: 1,
                    summary_count: 1,
                    summary_bytes: 4096,
                    stagnation_streak: 3,
                    error_streak: 1,
                    task_pressure: 2,
                    ready_to_finish: false,
                    action_oriented: true,
                    has_blocking_uncertainty: true,
                    progress_made: false,
                    retry_pattern: "same_tool".to_string(),
                    error_kind: "timeout".to_string(),
                    evidence_delta_quality: "no_meaningful_progress".to_string(),
                },
            }),
            has_observation: true,
            ..Default::default()
        },
    );

    let result = handle_status_command("status-auto-trace", &state).await;

    assert_eq!(result.response_type, "system");
    assert!(result.response.contains("runtime_think: high"));
    assert!(result.response.contains(
        "auto_decision: selected=high baseline=medium reason=mid_loop_investigate escalators=stagnation_streak dampeners=none clamps=none"
    ));
    assert!(result.response.contains(
        "auto_signals: intent=investigate chars=96 obs=medium results=2 tool_errors=1 summaries=1 bytes=4096 stagnation=3 error_streak=1 pressure=2 ready_signal=no action=yes blocked_signal=yes progress=no retry=same_tool error_kind=timeout evidence_delta=no_meaningful_progress"
    ));
    assert!(!result.response.contains("finish_deferrals"));
    assert!(!result.response.contains("finish_deferred"));

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn status_command_reports_live_runtime_provider_for_cross_provider_fast_model() {
    let workspace = unique_temp_workspace("lingclaw-command-status-cross-provider-fast-model");
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
                id: "gpt-4o-mini".to_string(),
                name: None,
                reasoning: Some(false),
                input: None,
                cost: None,
                context_window: Some(128000),
                max_tokens: Some(8192),
                compat: None,
            }],
        },
    );
    providers.insert(
        "anthropic".to_string(),
        crate::config::JsonProviderConfig {
            base_url: "https://api.anthropic.com".to_string(),
            api_key: "anthropic-key".to_string(),
            api: "anthropic".to_string(),
            models: vec![crate::config::JsonModelEntry {
                id: "claude-opus-4-7".to_string(),
                name: None,
                reasoning: Some(true),
                input: None,
                cost: None,
                context_window: Some(200000),
                max_tokens: Some(32000),
                compat: None,
            }],
        },
    );

    let state = AppState {
        config: std::sync::Mutex::new(Arc::new(crate::Config {
            api_key: "env-key".to_string(),
            api_base: "https://api.openai.com/v1".to_string(),
            model: "openai/gpt-4o-mini".to_string(),
            fast_model: Some("anthropic/claude-opus-4-7".to_string()),
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
            enable_state_digest: true,
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
        id: "status-cross-provider-fast-model".to_string(),
        name: "Status Cross Provider Fast Model".to_string(),
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
        model_override: Some("openai/gpt-4o-mini".to_string()),
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
        "status-cross-provider-fast-model".to_string(),
        crate::LiveRoundState {
            cycle: Some(0),
            effective_model: Some("anthropic/claude-opus-4-7".to_string()),
            effective_think: Some("high".to_string()),
            auto_observation_strength: Some("none".to_string()),
            auto_stagnation_streak: Some(0),
            auto_error_streak: Some(0),
            auto_task_pressure: Some(2),
            auto_action_oriented: Some(true),
            auto_ready_to_finish: Some(false),
            auto_has_blocking_uncertainty: Some(false),
            has_observation: false,
            ..Default::default()
        },
    );

    let result = handle_status_command("status-cross-provider-fast-model", &state).await;
    let base_budget = crate::context::context_input_budget_for_runtime(&config, &model, "high");
    let live_budget = crate::context::context_input_budget_for_runtime(
        &config,
        "anthropic/claude-opus-4-7",
        "high",
    );

    assert_eq!(result.response_type, "system");
    assert!(result.response.contains("model: openai/gpt-4o-mini"));
    assert!(
        result
            .response
            .contains("runtime_model: anthropic/claude-opus-4-7")
    );
    assert!(result.response.contains("runtime_provider: anthropic"));
    assert!(
        result
            .response
            .contains("runtime_model_id: claude-opus-4-7")
    );
    assert!(result.response.contains("runtime_think: high"));
    assert_ne!(base_budget, live_budget);
    assert!(result.response.contains(&format!(
        "/{} (tools",
        crate::format_token_count(live_budget as u64)
    )));
    assert!(!result.response.contains(&format!(
        "/{} (tools",
        crate::format_token_count(base_budget as u64)
    )));

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
            enable_state_digest: true,
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
async fn delete_command_rejects_active_session() {
    let workspace = unique_temp_workspace("lingclaw-command-delete-active");
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    tokio::fs::create_dir_all(&workspace)
        .await
        .expect("workspace should be created");
    prompts::init_session_prompt_files(&workspace);

    let active_session_id = unique_session_id("delete-active");
    let state = AppState {
        config: std::sync::Mutex::new(Arc::new(test_config())),
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

    let mut session = Session::new_with_id(&active_session_id, "Active Delete Target");
    let config = state.config();
    let model = session.effective_model(&config.model).to_string();
    session.messages.push(build_system_prompt(
        &config,
        &session.workspace,
        &model,
        &session.disabled_system_skills,
    ));
    state
        .sessions
        .lock()
        .await
        .insert(active_session_id.clone(), session);
    state
        .active_connections
        .lock()
        .await
        .insert(active_session_id.clone(), 99);

    let (tx, _rx) = tokio::sync::mpsc::channel::<String>(4);
    let result = handle_command(
        &format!("/delete {active_session_id}"),
        MAIN_SESSION_ID,
        1,
        &state,
        &tx,
        &CancellationToken::new(),
    )
    .await
    .expect("command should resolve");

    assert_eq!(result.response_type, "error");
    assert_eq!(
        result.response,
        format!("Cannot delete active session: {active_session_id}")
    );
    assert!(state.sessions.lock().await.contains_key(&active_session_id));

    let _ = tokio::fs::remove_dir_all(workspace.parent().unwrap_or(&workspace)).await;
}

#[tokio::test]
async fn delete_command_rejects_running_session_without_active_connection() {
    let workspace = unique_temp_workspace("lingclaw-command-delete-running");
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    tokio::fs::create_dir_all(&workspace)
        .await
        .expect("workspace should be created");
    prompts::init_session_prompt_files(&workspace);

    let running_session_id = unique_session_id("delete-running");
    let state = AppState {
        config: std::sync::Mutex::new(Arc::new(test_config())),
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

    let mut session = Session::new_with_id(&running_session_id, "Running Delete Target");
    let config = state.config();
    let model = session.effective_model(&config.model).to_string();
    session.messages.push(build_system_prompt(
        &config,
        &session.workspace,
        &model,
        &session.disabled_system_skills,
    ));
    state
        .sessions
        .lock()
        .await
        .insert(running_session_id.clone(), session);
    state.active_runs.lock().await.insert(
        running_session_id.clone(),
        crate::SessionRunBinding {
            connection_id: 42,
            cancel: CancellationToken::new(),
            stop_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            deferred_interventions: Arc::new(Mutex::new(crate::DeferredInterventionState::open())),
        },
    );

    let (tx, _rx) = tokio::sync::mpsc::channel::<String>(4);
    let result = handle_command(
        &format!("/delete {running_session_id}"),
        MAIN_SESSION_ID,
        1,
        &state,
        &tx,
        &CancellationToken::new(),
    )
    .await
    .expect("command should resolve");

    assert_eq!(result.response_type, "error");
    assert_eq!(
        result.response,
        format!("Cannot delete running session: {running_session_id}")
    );
    assert!(
        state
            .sessions
            .lock()
            .await
            .contains_key(&running_session_id)
    );

    let _ = tokio::fs::remove_dir_all(workspace.parent().unwrap_or(&workspace)).await;
}

#[tokio::test]
async fn delete_command_reports_filesystem_failure_without_removing_memory_session() {
    let session_id = unique_session_id("delete-fs-failure");
    let state = AppState {
        config: std::sync::Mutex::new(Arc::new(test_config())),
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

    let mut session = Session::new_with_id(&session_id, "FS Failure Target");
    let config = state.config();
    let model = session.effective_model(&config.model).to_string();
    session.messages.push(build_system_prompt(
        &config,
        &session.workspace,
        &model,
        &session.disabled_system_skills,
    ));
    let session_dir = session
        .workspace
        .parent()
        .map(PathBuf::from)
        .expect("session dir should exist");
    let sentinel_path = session_dir.join("sentinel.txt");
    tokio::fs::write(&sentinel_path, b"keep")
        .await
        .expect("sentinel should be written");
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);

    let session_file = sessions_dir().join(format!("{session_id}.json"));
    tokio::fs::write(&session_file, b"persisted")
        .await
        .expect("session file should be written");

    tokio::fs::remove_dir_all(&session_dir)
        .await
        .expect("session directory should be removable before test setup");
    tokio::fs::write(&session_dir, b"blocking-file")
        .await
        .expect("blocking file should be written");

    let (tx, _rx) = tokio::sync::mpsc::channel::<String>(4);
    let result = handle_command(
        &format!("/delete {session_id}"),
        MAIN_SESSION_ID,
        1,
        &state,
        &tx,
        &CancellationToken::new(),
    )
    .await
    .expect("command should resolve");

    assert_eq!(result.response_type, "error");
    assert!(
        result
            .response
            .contains("Failed to delete session workspace")
    );
    assert!(state.sessions.lock().await.contains_key(&session_id));
    assert!(
        tokio::fs::try_exists(&session_dir)
            .await
            .expect("workspace check should succeed")
    );
    assert!(
        tokio::fs::try_exists(&session_file)
            .await
            .expect("file check should succeed")
    );

    let _ = tokio::fs::remove_file(&session_dir).await;
    let _ = tokio::fs::remove_dir_all(session_dir).await;
    let _ = tokio::fs::remove_file(&session_file).await;
}

#[tokio::test]
async fn switch_command_rejects_corrupt_persisted_session_target() {
    let session_id = unique_session_id("switch-corrupt");
    let session_file = crate::session_store::sessions_dir().join(format!("{session_id}.json"));
    tokio::fs::write(&session_file, b"not valid json")
        .await
        .expect("corrupt session file should be written");

    let state = AppState {
        config: std::sync::Mutex::new(Arc::new(test_config())),
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
        &format!("/switch {session_id}"),
        MAIN_SESSION_ID,
        1,
        &state,
        &tx,
        &CancellationToken::new(),
    )
    .await
    .expect("command should resolve");

    assert_eq!(result.response_type, "error");
    assert_eq!(
        result.response,
        format!("Session '{session_id}' is corrupt and could not be loaded.")
    );
    assert_eq!(result.switch_to_session, None);
    assert!(state.sessions.lock().await.get(&session_id).is_none());

    let persisted_contents = tokio::fs::read_to_string(&session_file)
        .await
        .expect("corrupt session file should remain on disk");
    assert_eq!(persisted_contents, "not valid json");

    let _ = tokio::fs::remove_file(&session_file).await;
}

#[tokio::test]
async fn delete_command_allows_targeting_corrupt_persisted_session() {
    let session_id = unique_session_id("delete-corrupt");
    let session_file = crate::session_store::sessions_dir().join(format!("{session_id}.json"));
    tokio::fs::write(&session_file, b"not valid json")
        .await
        .expect("corrupt session file should be written");

    let state = AppState {
        config: std::sync::Mutex::new(Arc::new(test_config())),
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
        &format!("/delete {session_id}"),
        MAIN_SESSION_ID,
        1,
        &state,
        &tx,
        &CancellationToken::new(),
    )
    .await
    .expect("command should resolve");

    assert_eq!(result.response_type, "system");
    assert_eq!(
        result.response,
        format!("Deleted saved session: {session_id}")
    );
    assert!(
        !tokio::fs::try_exists(&session_file)
            .await
            .expect("session file check should succeed")
    );
}

#[tokio::test]
async fn switch_command_creates_or_switches_session() {
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
            enable_state_digest: true,
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

    assert_eq!(result.response, "Switching to session: another-session");
    assert_eq!(result.switch_to_session.as_deref(), Some("another-session"));
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
            enable_state_digest: true,
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
            enable_state_digest: true,
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
            enable_state_digest: true,
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
        enable_state_digest: true,
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
            enable_state_digest: true,
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
            enable_state_digest: true,
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
            enable_state_digest: true,
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
            enable_state_digest: true,
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
            enable_state_digest: true,
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
            enable_state_digest: true,
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
            enable_state_digest: true,
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
