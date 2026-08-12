//! Terminal client for LingClaw's existing local HTTP/WebSocket protocol.
//!
//! The TUI deliberately remains a client: Session, Plan, Group, configuration,
//! and usage semantics stay owned by the daemon so the browser and terminal
//! surfaces cannot drift into separate products.

use std::{
    collections::HashSet,
    fs::OpenOptions,
    io::{self, Stdout, Write},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use crossterm::{
    event::{
        DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent,
        KeyModifiers,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures::{SinkExt, StreamExt};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};
#[cfg(feature = "tui-images")]
use ratatui_image::{
    StatefulImage,
    picker::{Picker, ProtocolType},
    protocol::StatefulProtocol,
};
use reqwest::{Client, multipart};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};

use crate::{Config, cli};

type TuiResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

const TUI_USAGE: &str = "lingclaw tui [PATH] [--session ID] [--port PORT] [--lang auto|zh-CN|en] [--theme auto|dark|light]";
const SESSION_REASONING_STREAM_ID: &str = "session:reasoning";
const CONTROL_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const CONTROL_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const LONG_CONTROL_REQUEST_TIMEOUT: Duration = Duration::from_secs(35);
const SOCKET_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) fn print_help() {
    println!("LingClaw terminal workspace");
    println!();
    println!("Usage: {TUI_USAGE}");
    println!();
    println!("Arguments:");
    println!("  [PATH]          Workspace directory (defaults to the current directory)");
    println!();
    println!("Options:");
    println!("  --session ID    Open an existing Session");
    println!("  --port PORT     Connect to or launch the daemon on this port");
    println!("  --lang VALUE    auto, zh-CN, or en");
    println!("  --theme VALUE   auto, dark, or light");
    println!("  -h, --help      Show this help message");
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UiLanguage {
    ZhCn,
    En,
}

impl UiLanguage {
    fn detect(value: &str) -> Self {
        match value {
            "zh-CN" | "zh" => Self::ZhCn,
            "en" => Self::En,
            _ => Self::detect_automatic(),
        }
    }

    fn detect_automatic() -> Self {
        let environment = ["LC_ALL", "LC_MESSAGES", "LANG"]
            .map(std::env::var)
            .map(Result::ok);
        Self::from_locale_candidates(
            environment.iter().filter_map(|value| value.as_deref()),
            Self::detect_windows_ui_locale().as_deref(),
        )
    }

    fn from_locale_candidates<'a>(
        environment: impl IntoIterator<Item = &'a str>,
        platform_locale: Option<&str>,
    ) -> Self {
        environment
            .into_iter()
            .find(|locale| !locale.trim().is_empty())
            .or_else(|| platform_locale.filter(|locale| !locale.trim().is_empty()))
            .map(Self::from_locale)
            .unwrap_or(Self::En)
    }

    fn from_locale(locale: &str) -> Self {
        let locale = locale.trim().to_ascii_lowercase();
        if locale == "zh"
            || locale.starts_with("zh-")
            || locale.starts_with("zh_")
            || locale.starts_with("chinese")
        {
            Self::ZhCn
        } else {
            Self::En
        }
    }

    #[cfg(windows)]
    fn detect_windows_ui_locale() -> Option<String> {
        use windows_sys::Win32::Globalization::GetUserDefaultLocaleName;

        // LOCALE_NAME_MAX_LENGTH is 85 including the trailing NUL. Keeping the
        // fixed-size buffer here avoids enabling an unrelated SystemServices
        // feature solely for the constant.
        let mut locale = [0u16; 85];
        // SAFETY: `locale` is a writable UTF-16 buffer whose exact length is
        // supplied to the Windows API.
        let length = unsafe {
            GetUserDefaultLocaleName(locale.as_mut_ptr(), i32::try_from(locale.len()).ok()?)
        };
        if length <= 1 {
            return None;
        }
        String::from_utf16(&locale[..usize::try_from(length - 1).ok()?]).ok()
    }

    #[cfg(not(windows))]
    fn detect_windows_ui_locale() -> Option<String> {
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UiTheme {
    Dark,
    Light,
}

impl UiTheme {
    fn detect(value: &str) -> Self {
        match value {
            "light" => Self::Light,
            "dark" => Self::Dark,
            _ => Self::detect_automatic().unwrap_or(Self::Dark),
        }
    }

    fn detect_automatic() -> Option<Self> {
        std::env::var("COLORFGBG")
            .ok()
            .as_deref()
            .and_then(Self::from_colorfgbg)
            .or_else(|| {
                std::env::var("TERM_BACKGROUND")
                    .ok()
                    .as_deref()
                    .and_then(Self::from_theme_hint)
            })
            .or_else(|| {
                std::env::var("VSCODE_THEME_KIND")
                    .ok()
                    .as_deref()
                    .and_then(Self::from_vscode_theme_kind)
            })
            .or_else(Self::detect_windows_app_theme)
    }

    fn from_colorfgbg(value: &str) -> Option<Self> {
        let background = value
            .rsplit([';', ':'])
            .next()?
            .trim()
            .parse::<u16>()
            .ok()?;
        let (red, green, blue) = Self::ansi_rgb(background)?;
        let luminance = u32::from(red) * 299 + u32::from(green) * 587 + u32::from(blue) * 114;
        Some(if luminance >= 150_000 {
            Self::Light
        } else {
            Self::Dark
        })
    }

    fn ansi_rgb(index: u16) -> Option<(u8, u8, u8)> {
        const ANSI: [(u8, u8, u8); 16] = [
            (0, 0, 0),
            (128, 0, 0),
            (0, 128, 0),
            (128, 128, 0),
            (0, 0, 128),
            (128, 0, 128),
            (0, 128, 128),
            (192, 192, 192),
            (128, 128, 128),
            (255, 0, 0),
            (0, 255, 0),
            (255, 255, 0),
            (0, 0, 255),
            (255, 0, 255),
            (0, 255, 255),
            (255, 255, 255),
        ];
        const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];

        match index {
            0..=15 => Some(ANSI[usize::from(index)]),
            16..=231 => {
                let offset = index - 16;
                Some((
                    CUBE[usize::from(offset / 36)],
                    CUBE[usize::from((offset % 36) / 6)],
                    CUBE[usize::from(offset % 6)],
                ))
            }
            232..=255 => {
                let value = 8 + 10 * (index - 232) as u8;
                Some((value, value, value))
            }
            _ => None,
        }
    }

    fn from_theme_hint(value: &str) -> Option<Self> {
        let value = value.to_ascii_lowercase();
        if value.contains("light") {
            Some(Self::Light)
        } else if value.contains("dark") {
            Some(Self::Dark)
        } else {
            None
        }
    }

    fn from_vscode_theme_kind(value: &str) -> Option<Self> {
        match value.trim() {
            "1" | "4" => Some(Self::Light),
            "2" | "3" => Some(Self::Dark),
            _ => Self::from_theme_hint(value),
        }
    }

    #[cfg(windows)]
    fn detect_windows_app_theme() -> Option<Self> {
        use std::os::windows::ffi::OsStrExt as _;
        use windows_sys::Win32::{
            Foundation::ERROR_SUCCESS,
            System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW},
        };

        let subkey =
            std::ffi::OsStr::new(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
                .encode_wide()
                .chain(std::iter::once(0))
                .collect::<Vec<_>>();
        let value_name = std::ffi::OsStr::new("AppsUseLightTheme")
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let mut value = 0u32;
        let mut value_size = std::mem::size_of::<u32>() as u32;
        // SAFETY: both names are NUL-terminated, and `value`/`value_size`
        // describe a writable DWORD output buffer for the duration of the call.
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                subkey.as_ptr(),
                value_name.as_ptr(),
                RRF_RT_REG_DWORD,
                std::ptr::null_mut(),
                (&mut value as *mut u32).cast(),
                &mut value_size,
            )
        };
        if status != ERROR_SUCCESS || value_size != std::mem::size_of::<u32>() as u32 {
            return None;
        }
        Some(if value == 0 { Self::Dark } else { Self::Light })
    }

    #[cfg(not(windows))]
    fn detect_windows_app_theme() -> Option<Self> {
        None
    }
}

#[derive(Debug)]
struct TuiOptions {
    path: Option<PathBuf>,
    session: Option<String>,
    port: u16,
    language: UiLanguage,
    theme: UiTheme,
}

fn canonicalize_workspace_path(path: PathBuf) -> TuiResult<PathBuf> {
    let path = path
        .canonicalize()
        .map_err(|error| format!("cannot use workspace '{}': {error}", path.display()))?;
    if !path.is_dir() {
        return Err(format!("workspace is not a directory: {}", path.display()).into());
    }
    if path.to_str().is_none() {
        return Err("workspace path must be valid UTF-8".into());
    }
    Ok(path)
}

impl TuiOptions {
    fn parse(args: &[String]) -> TuiResult<Self> {
        let mut path = None;
        let mut session = None;
        let mut port = None;
        let mut language = "auto".to_string();
        let mut theme = "auto".to_string();
        let mut index = 0;
        while index < args.len() {
            match args[index].as_str() {
                "--session" | "--port" | "--lang" | "--theme" => {
                    let flag = args[index].as_str();
                    let value = args
                        .get(index + 1)
                        .ok_or_else(|| format!("{flag} requires a value"))?;
                    match flag {
                        "--session" => session = Some(value.clone()),
                        "--port" => {
                            port = Some(value.parse::<u16>().map_err(|_| "invalid --port")?)
                        }
                        "--lang" if matches!(value.as_str(), "auto" | "zh-CN" | "en") => {
                            language = value.clone();
                        }
                        "--theme" if matches!(value.as_str(), "auto" | "dark" | "light") => {
                            theme = value.clone();
                        }
                        "--lang" => return Err("--lang must be auto, zh-CN, or en".into()),
                        "--theme" => return Err("--theme must be auto, dark, or light".into()),
                        _ => {}
                    }
                    index += 2;
                }
                "--help" | "-h" => {
                    return Err(format!("usage: {TUI_USAGE}").into());
                }
                value if value.starts_with('-') => {
                    return Err(format!("unknown TUI option: {value}").into());
                }
                value => {
                    if path.is_some() {
                        return Err("only one workspace PATH may be supplied".into());
                    }
                    path = Some(PathBuf::from(value));
                    index += 1;
                }
            }
        }
        let path = match path {
            Some(path) => Some(canonicalize_workspace_path(path)?),
            None if session.is_none() => {
                Some(canonicalize_workspace_path(std::env::current_dir()?)?)
            }
            // `--session` without an explicit PATH opens the stored Session directly.
            // Do not make that operation depend on the shell's current directory.
            None => None,
        };
        let config = Config::load();
        let port = port.unwrap_or(config.port);
        if port == 0 {
            return Err("--port must be between 1 and 65535".into());
        }
        Ok(Self {
            path,
            session,
            port,
            language: UiLanguage::detect(&language),
            theme: UiTheme::detect(&theme),
        })
    }
}

#[derive(Clone, Debug, Deserialize)]
struct WorkspaceSummary {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    path: String,
    #[serde(default = "default_true")]
    available: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize)]
struct SessionSummary {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    workspace: Option<WorkspaceSummary>,
}

#[derive(Debug, Deserialize)]
struct SessionsResponse {
    #[serde(default)]
    sessions: Vec<SessionSummary>,
}

#[derive(Debug, Deserialize)]
struct CreatedSessionResponse {
    session: SessionSummary,
}

#[derive(Debug, Deserialize)]
struct MutatedGroupResponse {
    group: GroupSummary,
}

#[derive(Clone, Debug, Deserialize)]
struct GroupSummary {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    members: usize,
}

#[derive(Debug, Deserialize)]
struct GroupsResponse {
    #[serde(default)]
    groups: Vec<GroupSummary>,
}

#[derive(Clone, Debug)]
struct ChatLine {
    role: String,
    content: String,
    style: LineKind,
    stream_id: Option<String>,
}

#[derive(Clone, Copy, Debug)]
enum LineKind {
    User,
    Assistant,
    Reasoning,
    Tool,
    System,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page {
    Chat,
    Plan,
    Todos,
    Models,
    Skills,
    Mcp,
    Usage,
    Settings,
    Groups,
}

impl Page {
    fn label(self, language: UiLanguage) -> &'static str {
        match (self, language) {
            (Self::Chat, UiLanguage::ZhCn) => "对话",
            (Self::Plan, UiLanguage::ZhCn) => "计划",
            (Self::Todos, UiLanguage::ZhCn) => "任务",
            (Self::Models, UiLanguage::ZhCn) => "模型",
            (Self::Skills, UiLanguage::ZhCn) => "技能",
            (Self::Mcp, UiLanguage::ZhCn) => "MCP",
            (Self::Usage, UiLanguage::ZhCn) => "用量",
            (Self::Settings, UiLanguage::ZhCn) => "设置",
            (Self::Groups, UiLanguage::ZhCn) => "群聊",
            (Self::Chat, UiLanguage::En) => "Chat",
            (Self::Plan, UiLanguage::En) => "Plan",
            (Self::Todos, UiLanguage::En) => "Todos",
            (Self::Models, UiLanguage::En) => "Models",
            (Self::Skills, UiLanguage::En) => "Skills",
            (Self::Mcp, UiLanguage::En) => "MCP",
            (Self::Usage, UiLanguage::En) => "Usage",
            (Self::Settings, UiLanguage::En) => "Settings",
            (Self::Groups, UiLanguage::En) => "Groups",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Navigation,
    Content,
    Composer,
}

#[derive(Clone, Debug)]
struct PlanSnapshot {
    id: String,
    revision: u64,
    status: String,
    title: String,
    raw: Value,
}

#[derive(Clone, Debug)]
struct PlanStaleSnapshot {
    confirmation_token: String,
    paths: Vec<String>,
    evidence_incomplete: bool,
    action: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct UserMessageFingerprint {
    content: String,
    images: Vec<String>,
}

impl UserMessageFingerprint {
    fn from_composer(input: &str, images: &[Value]) -> Self {
        Self {
            content: input.trim().to_string(),
            images: images.iter().map(image_identity).collect(),
        }
    }

    fn from_history(message: &Value) -> Option<Self> {
        (message.get("role").and_then(Value::as_str) == Some("user")).then(|| Self {
            content: message
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            images: message
                .get("images")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(image_identity)
                .collect(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct HistoricalUserMessage {
    key: Option<String>,
    fingerprint: UserMessageFingerprint,
}

impl HistoricalUserMessage {
    fn from_value(message: &Value) -> Option<Self> {
        let fingerprint = UserMessageFingerprint::from_history(message)?;
        let key = message
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(|id| format!("id:{id}"))
            .or_else(|| {
                message
                    .get("message_index")
                    .and_then(Value::as_u64)
                    .map(|index| format!("index:{index}"))
            });
        Some(Self { key, fingerprint })
    }
}

fn image_identity(image: &Value) -> String {
    if let Some(url) = image
        .get("url")
        .and_then(Value::as_str)
        .filter(|url| !url.is_empty())
    {
        let without_query = url.split_once('?').map_or(url, |(value, _)| value);
        let without_fragment = without_query
            .split_once('#')
            .map_or(without_query, |(value, _)| value);
        return format!("url:{without_fragment}");
    }
    if let Some(object_key) = image
        .get("object_key")
        .and_then(Value::as_str)
        .filter(|key| !key.is_empty())
    {
        return format!("object:{object_key}");
    }
    format!("json:{image}")
}

struct App {
    language: UiLanguage,
    theme: UiTheme,
    session: SessionSummary,
    sessions: Vec<SessionSummary>,
    groups: Vec<GroupSummary>,
    groups_enabled: bool,
    active_group: Option<String>,
    active_group_runs: HashSet<String>,
    group_target_mode: String,
    group_targets: Vec<String>,
    pages: Vec<Page>,
    page_index: usize,
    nav_index: usize,
    focus: Focus,
    lines: Vec<ChatLine>,
    input: String,
    scroll: u16,
    inspector_scroll: u16,
    busy: bool,
    connected: bool,
    storage_writable: bool,
    plan_mode: bool,
    active_plan: Option<PlanSnapshot>,
    plan_feedback_mode: bool,
    plan_stale: Option<PlanStaleSnapshot>,
    pending_plan_action: Option<String>,
    confirm_stale_plan: bool,
    inspector: String,
    inspector_payload: Option<Value>,
    inspector_index: usize,
    inspector_choice: usize,
    todos_snapshot: String,
    status: String,
    show_help: bool,
    show_commands: bool,
    quit_armed: bool,
    image_protocol: String,
    last_image_url: Option<String>,
    pending_images: Vec<Value>,
    pending_outbound_write: Option<ComposerSnapshot>,
    history_user_messages: Vec<HistoricalUserMessage>,
    history_baseline_available: bool,
    pending_confirmation: Option<PendingConfirmation>,
    settings_edit: Option<SettingsEdit>,
    current_model: String,
    current_effort: String,
    current_model_supports_image: bool,
    model_config_revision: u64,
    current_s3_config_id: Option<String>,
    socket_generation: u64,
    upload_in_progress: bool,
    outbound_reconnect_pending: bool,
    #[cfg(feature = "tui-images")]
    image_picker: Option<Picker>,
    #[cfg(feature = "tui-images")]
    image_preview: Option<StatefulProtocol>,
}

impl App {
    fn new(
        options: &TuiOptions,
        session: SessionSummary,
        sessions: Vec<SessionSummary>,
        groups_enabled: bool,
        image_protocol: String,
    ) -> Self {
        let mut pages = vec![
            Page::Chat,
            Page::Plan,
            Page::Todos,
            Page::Models,
            Page::Skills,
            Page::Mcp,
            Page::Usage,
            Page::Settings,
        ];
        if groups_enabled {
            pages.push(Page::Groups);
        }
        Self {
            language: options.language,
            theme: options.theme,
            session,
            sessions,
            groups: Vec::new(),
            groups_enabled,
            active_group: None,
            active_group_runs: HashSet::new(),
            group_target_mode: "all".to_string(),
            group_targets: Vec::new(),
            pages,
            page_index: 0,
            nav_index: 0,
            focus: Focus::Composer,
            lines: Vec::new(),
            input: String::new(),
            scroll: 0,
            inspector_scroll: 0,
            busy: false,
            connected: false,
            storage_writable: true,
            plan_mode: false,
            active_plan: None,
            plan_feedback_mode: false,
            plan_stale: None,
            pending_plan_action: None,
            confirm_stale_plan: false,
            inspector: String::new(),
            inspector_payload: None,
            inspector_index: 0,
            inspector_choice: 0,
            todos_snapshot: String::new(),
            status: String::new(),
            show_help: false,
            show_commands: false,
            quit_armed: false,
            image_protocol,
            last_image_url: None,
            pending_images: Vec::new(),
            pending_outbound_write: None,
            history_user_messages: Vec::new(),
            history_baseline_available: false,
            pending_confirmation: None,
            settings_edit: None,
            current_model: String::new(),
            current_effort: String::new(),
            current_model_supports_image: false,
            model_config_revision: 0,
            current_s3_config_id: None,
            socket_generation: 0,
            upload_in_progress: false,
            outbound_reconnect_pending: false,
            #[cfg(feature = "tui-images")]
            image_picker: None,
            #[cfg(feature = "tui-images")]
            image_preview: None,
        }
    }

    fn page(&self) -> Page {
        self.pages[self.page_index.min(self.pages.len().saturating_sub(1))]
    }

    fn accent(&self) -> Color {
        Color::Rgb(112, 88, 226)
    }

    fn text_color(&self) -> Color {
        match self.theme {
            UiTheme::Dark => Color::Gray,
            UiTheme::Light => Color::Black,
        }
    }

    fn push(&mut self, role: impl Into<String>, content: impl Into<String>, style: LineKind) {
        self.lines.push(ChatLine {
            role: role.into(),
            content: content.into(),
            style,
            stream_id: None,
        });
        self.scroll = u16::MAX;
    }

    fn push_stream(
        &mut self,
        stream_id: impl Into<String>,
        role: impl Into<String>,
        content: impl Into<String>,
        style: LineKind,
    ) {
        self.lines.push(ChatLine {
            role: role.into(),
            content: content.into(),
            style,
            stream_id: Some(stream_id.into()),
        });
        self.scroll = u16::MAX;
    }

    fn target_count(&self) -> usize {
        self.sessions.len()
            + if self.groups_enabled {
                self.groups.len()
            } else {
                0
            }
    }
}

fn reset_target_scoped_state(app: &mut App) {
    app.active_group_runs.clear();
    app.busy = false;
    app.connected = false;
    app.plan_mode = false;
    app.active_plan = None;
    app.plan_feedback_mode = false;
    app.plan_stale = None;
    app.pending_plan_action = None;
    app.confirm_stale_plan = false;
    app.pending_images.clear();
    app.pending_outbound_write = None;
    app.history_user_messages.clear();
    app.history_baseline_available = false;
    app.pending_confirmation = None;
    if app.settings_edit.take().is_some() {
        app.input.clear();
    }
    app.lines.clear();
    app.scroll = 0;
    app.inspector_scroll = 0;
    app.inspector.clear();
    app.inspector_payload = None;
    app.inspector_index = 0;
    app.inspector_choice = 0;
    app.todos_snapshot.clear();
    app.current_model.clear();
    app.current_effort.clear();
    app.current_model_supports_image = false;
    app.model_config_revision = 0;
    app.current_s3_config_id = None;
    app.outbound_reconnect_pending = false;
    app.last_image_url = None;
    #[cfg(feature = "tui-images")]
    {
        app.image_preview = None;
    }
}

fn activate_group_target(app: &mut App, group_id: &str) -> Result<(), String> {
    let main = app
        .sessions
        .iter()
        .find(|session| crate::is_main(&session.id))
        .cloned()
        .ok_or_else(|| {
            tr(
                app,
                "无法打开群聊：Main Session 不可用",
                "Cannot open the Group because the Main Session is unavailable",
            )
            .to_string()
        })?;

    // Group sockets always run as the implicit Main owner. Keep the local
    // Session context aligned with that protocol identity so a previously
    // selected worker can never become the accidental target of a later API.
    app.session = main;
    app.active_group = Some(group_id.to_string());
    app.group_target_mode = "all".to_string();
    app.group_targets.clear();
    reset_target_scoped_state(app);
    Ok(())
}

fn session_scoped_page(page: Page) -> bool {
    matches!(
        page,
        Page::Plan | Page::Todos | Page::Models | Page::Skills | Page::Mcp | Page::Usage
    )
}

fn group_session_scope_error(app: &App) -> String {
    tr(
        app,
        "此页面属于单个 Session，选择群聊时不可用；请先切换到一个 Session。",
        "This page belongs to a single Session and is unavailable while a Group is selected; select a Session first.",
    )
    .to_string()
}

fn active_target_name(app: &App) -> String {
    let Some(group_id) = app.active_group.as_deref() else {
        return app.session.name.clone();
    };
    let name = app
        .groups
        .iter()
        .find(|group| group.id == group_id)
        .map(|group| group.name.as_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(group_id);
    format!("{} · {}", name, tr(app, "群聊", "Group"))
}

#[derive(Debug)]
enum UserAction {
    None,
    Send(String),
    Stop(String),
    SwitchSession(String),
    SwitchGroup(String),
    Load(Page),
    OpenImage,
    UploadImage(PathBuf),
    EditConfig,
    ToggleGroups,
    MutatePage(PageMutation),
    Manage(ManagementAction),
    Exit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SocketEventAction {
    None,
    ReconnectMain,
    RefreshGroups,
}

#[derive(Clone, Debug)]
enum PageMutation {
    Model {
        model: String,
        effort: String,
    },
    Skills {
        enabled_system_skills: Vec<String>,
        known_system_skills: Vec<String>,
    },
    McpPolicy(Value),
    Todos {
        base_revision: u64,
        items: Vec<Value>,
    },
    Config(ConfigMutation),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SettingsValueKind {
    Toggle,
    Unsigned { optional: bool },
    Text { optional: bool, secret: bool },
}

#[derive(Clone, Debug)]
struct SettingsRow {
    section: String,
    label: String,
    path: Vec<String>,
    kind: SettingsValueKind,
    value: Option<Value>,
}

#[derive(Clone, Debug)]
struct SettingsEdit {
    row: SettingsRow,
}

#[derive(Clone, Debug)]
struct ConfigMutation {
    path: Vec<String>,
    value: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum WorkspaceSelection {
    Managed,
    Directory(PathBuf),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ManagementAction {
    CreateSession {
        name: String,
        workspace: WorkspaceSelection,
    },
    RenameSession(String),
    RebindSession(WorkspaceSelection),
    SwitchSession(String),
    FindSession(String),
    DeleteSession(String),
    CreateGroup {
        name: String,
        members: Vec<String>,
    },
    RenameGroup(String),
    ReplaceGroupMembers(Vec<String>),
    SwitchGroup(String),
    DeleteGroup,
    PromoteGroupMember(String),
    RemoveGroupMember(String),
    StartMcpOauth(String),
    DisconnectMcpOauth(String),
}

impl ManagementAction {
    fn requires_storage_write(&self) -> bool {
        !matches!(
            self,
            Self::SwitchSession(_)
                | Self::FindSession(_)
                | Self::SwitchGroup(_)
                | Self::StartMcpOauth(_)
                | Self::DisconnectMcpOauth(_)
        )
    }
}

#[derive(Clone, Debug)]
enum ManagementOutcome {
    Stay,
    SwitchSession(SessionSummary),
    SwitchGroup(String),
}

#[derive(Clone, Debug)]
struct PendingConfirmation {
    action: ManagementAction,
    draft: String,
    prompt: String,
}

#[derive(Clone, Debug)]
struct ComposerSnapshot {
    input: String,
    pending_images: Vec<Value>,
    plan_mode: bool,
    plan_feedback_mode: bool,
    line_count: usize,
    user_message: Option<UserMessageFingerprint>,
    matching_user_message_count: usize,
    matching_keyless_user_message_count: usize,
    known_user_message_keys: HashSet<String>,
    history_baseline_available: bool,
    active_plan_id: Option<String>,
    active_plan_revision: Option<u64>,
}

impl ComposerSnapshot {
    fn capture(app: &App) -> Self {
        let input = app.input.clone();
        let pending_images = app.pending_images.clone();
        let has_message = !input.trim().is_empty() || !pending_images.is_empty();
        let is_slash_command = pending_images.is_empty() && input.trim_start().starts_with('/');
        let user_message = if has_message && !is_slash_command && !app.plan_feedback_mode {
            Some(UserMessageFingerprint::from_composer(
                &input,
                &pending_images,
            ))
        } else {
            None
        };
        let matching_history = app
            .history_user_messages
            .iter()
            .filter(|message| {
                user_message
                    .as_ref()
                    .is_some_and(|expected| message.fingerprint == *expected)
            })
            .collect::<Vec<_>>();
        Self {
            input,
            pending_images,
            plan_mode: app.plan_mode,
            plan_feedback_mode: app.plan_feedback_mode,
            line_count: app.lines.len(),
            user_message,
            matching_user_message_count: matching_history.len(),
            matching_keyless_user_message_count: matching_history
                .iter()
                .filter(|message| message.key.is_none())
                .count(),
            known_user_message_keys: matching_history
                .iter()
                .filter_map(|message| message.key.clone())
                .collect(),
            history_baseline_available: app.history_baseline_available,
            active_plan_id: app.active_plan.as_ref().map(|plan| plan.id.clone()),
            active_plan_revision: app.active_plan.as_ref().map(|plan| plan.revision),
        }
    }

    fn restore_composer(self, app: &mut App) {
        app.input = self.input;
        app.pending_images = self.pending_images;
        app.plan_mode = self.plan_mode;
        app.plan_feedback_mode = self.plan_feedback_mode;
    }

    /// A WebSocket sink can fail after the composer has optimistically cleared
    /// its draft. Restore the exact draft and remove only locally-added lines so
    /// reconnecting never turns a transient disconnect into lost user input.
    fn restore_after_failed_send(self, app: &mut App) {
        let line_count = self.line_count;
        self.restore_composer(app);
        app.lines.truncate(line_count);
    }

    fn is_slash_command(&self) -> bool {
        self.pending_images.is_empty() && self.input.trim_start().starts_with('/')
    }

    fn is_replayable_user_message(&self) -> bool {
        self.user_message.is_some()
    }

    fn was_replayed(&self, messages: &[Value], history: &Value) -> bool {
        if self.plan_feedback_mode {
            let Some(plan_id) = self.active_plan_id.as_deref() else {
                return false;
            };
            let revision = self.active_plan_revision.unwrap_or_default();
            return history
                .get("plans")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|plan| {
                    plan.get("plan_id").and_then(Value::as_str) == Some(plan_id)
                        && plan.get("historical").and_then(Value::as_bool) != Some(true)
                })
                .any(|plan| {
                    plan.get("revision")
                        .and_then(Value::as_u64)
                        .is_some_and(|current| current > revision)
                        || plan.get("status").and_then(Value::as_str) == Some("planning")
                });
        }
        if !self.is_replayable_user_message() {
            return true;
        }
        let Some(expected) = self.user_message.as_ref() else {
            return true;
        };
        if !self.history_baseline_available {
            return false;
        }
        let matching = messages
            .iter()
            .filter_map(HistoricalUserMessage::from_value)
            .filter(|message| message.fingerprint == *expected)
            .collect::<Vec<_>>();
        let unknown_stable_keys = matching
            .iter()
            .filter_map(|message| message.key.as_ref())
            .filter(|key| !self.known_user_message_keys.contains(*key))
            .count();
        matching.len() > self.matching_user_message_count
            || unknown_stable_keys > self.matching_keyless_user_message_count
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ImageUploadContext {
    session_id: String,
    active_group: Option<String>,
    socket_generation: u64,
    model: String,
    effort: String,
    config_revision: u64,
    s3_config_id: String,
    workspace: Option<PathBuf>,
}

#[cfg(feature = "tui-images")]
#[derive(Clone, Debug, PartialEq, Eq)]
struct TerminalImagePreviewContext {
    session_id: String,
    active_group: Option<String>,
    socket_generation: u64,
    url: String,
}

#[cfg(feature = "tui-images")]
impl TerminalImagePreviewContext {
    fn capture(app: &App) -> Option<Self> {
        let url = app.last_image_url.clone()?;
        app.image_picker.as_ref()?;
        Some(Self {
            session_id: app.session.id.clone(),
            active_group: app.active_group.clone(),
            socket_generation: app.socket_generation,
            url,
        })
    }

    fn is_current(&self, app: &App) -> bool {
        self.session_id == app.session.id
            && self.active_group == app.active_group
            && self.socket_generation == app.socket_generation
            && app.last_image_url.as_deref() == Some(self.url.as_str())
    }
}

#[cfg(feature = "tui-images")]
struct TerminalImagePreviewEvent {
    context: TerminalImagePreviewContext,
    result: TuiResult<StatefulProtocol>,
}

#[cfg(not(feature = "tui-images"))]
type TerminalImagePreviewContext = ();

#[cfg(not(feature = "tui-images"))]
type TerminalImagePreviewEvent = ();

impl ImageUploadContext {
    fn capture(app: &App) -> Result<Self, String> {
        if !app.storage_writable {
            return Err(tr(
                app,
                "本地存储处于保护模式，无法上传图片",
                "Local storage is protected; image upload is unavailable",
            )
            .to_string());
        }
        if app.active_group.is_some() {
            return Err(tr(
                app,
                "群聊协议暂不支持图片附件",
                "Group chat does not currently accept image attachments",
            )
            .to_string());
        }
        if !app.connected {
            return Err(tr(
                app,
                "尚未连接，无法上传图片",
                "Not connected; image upload is unavailable",
            )
            .to_string());
        }
        if !app.current_model_supports_image {
            return Err(tr(
                app,
                "当前模型不支持图片输入",
                "The current model does not support image input",
            )
            .to_string());
        }
        if app.current_model.is_empty() || app.model_config_revision == 0 {
            return Err(tr(
                app,
                "模型能力仍在同步，请稍后重试",
                "Model capabilities are still syncing; try again shortly",
            )
            .to_string());
        }
        let s3_config_id = app.current_s3_config_id.clone().ok_or_else(|| {
            tr(
                app,
                "尚未配置可用的 S3 图片存储",
                "No usable S3 image storage is configured",
            )
            .to_string()
        })?;
        Ok(Self {
            session_id: app.session.id.clone(),
            active_group: app.active_group.clone(),
            socket_generation: app.socket_generation,
            model: app.current_model.clone(),
            effort: app.current_effort.clone(),
            config_revision: app.model_config_revision,
            s3_config_id,
            workspace: app
                .session
                .workspace
                .as_ref()
                .map(|workspace| PathBuf::from(&workspace.path))
                .filter(|path| !path.as_os_str().is_empty()),
        })
    }

    fn is_current(&self, app: &App) -> bool {
        app.connected
            && app.storage_writable
            && self.session_id == app.session.id
            && self.active_group == app.active_group
            && self.socket_generation == app.socket_generation
            && self.model == app.current_model
            && self.effort == app.current_effort
            && self.config_revision == app.model_config_revision
            && app.current_model_supports_image
            && app.current_s3_config_id.as_deref() == Some(self.s3_config_id.as_str())
    }
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> TuiResult<(Self, Terminal<CrosstermBackend<Stdout>>)> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen, EnableMouseCapture) {
            let _ = disable_raw_mode();
            return Err(error.into());
        }
        let terminal = match Terminal::new(CrosstermBackend::new(stdout)) {
            Ok(terminal) => terminal,
            Err(error) => {
                let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
                let _ = disable_raw_mode();
                return Err(error.into());
            }
        };
        Ok((Self, terminal))
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SetupProviderKind {
    OpenAi,
    Anthropic,
    Ollama,
    Gemini,
}

impl SetupProviderKind {
    const ALL: [Self; 4] = [Self::OpenAi, Self::Anthropic, Self::Ollama, Self::Gemini];

    fn label(self) -> &'static str {
        match self {
            Self::OpenAi => "OpenAI-compatible",
            Self::Anthropic => "Anthropic-compatible",
            Self::Ollama => "Ollama",
            Self::Gemini => "Gemini",
        }
    }

    fn provider_name(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
            Self::Ollama => "ollama",
            Self::Gemini => "gemini",
        }
    }

    fn api(self) -> &'static str {
        match self {
            Self::OpenAi => "openai-completions",
            Self::Anthropic => "anthropic",
            Self::Ollama => "ollama",
            Self::Gemini => "gemini",
        }
    }

    fn base_url(self) -> &'static str {
        match self {
            Self::OpenAi => "https://api.openai.com/v1",
            Self::Anthropic => "https://api.anthropic.com",
            Self::Ollama => "http://127.0.0.1:11434",
            Self::Gemini => "https://generativelanguage.googleapis.com/v1beta",
        }
    }

    fn model_id(self) -> &'static str {
        match self {
            Self::OpenAi => "gpt-4o-mini",
            Self::Anthropic => "claude-sonnet-4-5",
            Self::Ollama => "llama3.2",
            Self::Gemini => "gemini-2.5-flash",
        }
    }

    fn api_key_optional(self) -> bool {
        self == Self::Ollama
    }
}

struct NativeSetup {
    step: usize,
    provider_index: usize,
    provider_name: String,
    base_url: String,
    api_key: String,
    model_id: String,
    reasoning: bool,
    error: String,
    existing_providers: HashSet<String>,
}

impl NativeSetup {
    const LAST_STEP: usize = 7;

    fn new(existing_providers: HashSet<String>) -> Self {
        Self {
            step: 0,
            provider_index: 0,
            provider_name: String::new(),
            base_url: String::new(),
            api_key: String::new(),
            model_id: String::new(),
            reasoning: false,
            error: String::new(),
            existing_providers,
        }
    }

    fn provider(&self) -> SetupProviderKind {
        SetupProviderKind::ALL[self.provider_index]
    }

    fn prepare_provider_defaults(&mut self) {
        let provider = self.provider();
        let base = provider.provider_name();
        let mut candidate = base.to_string();
        let mut suffix = 2usize;
        while self.existing_providers.contains(&candidate) {
            candidate = format!("{base}-{suffix}");
            suffix += 1;
        }
        self.provider_name = candidate;
        self.base_url = provider.base_url().to_string();
        self.model_id = provider.model_id().to_string();
        self.api_key.clear();
        self.reasoning = false;
    }

    fn editable_value_mut(&mut self) -> Option<&mut String> {
        match self.step {
            2 => Some(&mut self.provider_name),
            3 => Some(&mut self.base_url),
            4 => Some(&mut self.api_key),
            5 => Some(&mut self.model_id),
            _ => None,
        }
    }

    fn advance(&mut self) -> Result<bool, String> {
        self.error.clear();
        match self.step {
            0 => self.step = 1,
            1 => {
                self.prepare_provider_defaults();
                self.step = 2;
            }
            2 => {
                let name = self.provider_name.trim();
                crate::config::validate_provider_name(name)?;
                if self.existing_providers.contains(name) {
                    return Err(format!("Provider '{name}' already exists"));
                }
                self.provider_name = name.to_string();
                self.step = 3;
            }
            3 => {
                let value = self.base_url.trim();
                if value.is_empty() {
                    return Err("Base URL is required".to_string());
                }
                self.base_url = value.to_string();
                self.step = 4;
            }
            4 => {
                if !self.provider().api_key_optional() && self.api_key.trim().is_empty() {
                    return Err("API key is required for this provider".to_string());
                }
                self.api_key = self.api_key.trim().to_string();
                self.step = 5;
            }
            5 => {
                let value = self.model_id.trim();
                if value.is_empty() {
                    return Err("Model ID is required".to_string());
                }
                self.model_id = value.to_string();
                self.step = 6;
            }
            6 => self.step = 7,
            Self::LAST_STEP => return Ok(true),
            _ => unreachable!("native setup step must be bounded"),
        }
        Ok(false)
    }
}

fn load_setup_config() -> TuiResult<Option<Value>> {
    let Some(path) = crate::config_file_path() else {
        return Err("Cannot determine the LingClaw configuration path".into());
    };
    if !path.exists() {
        return Ok(Some(json!({})));
    }
    let content = std::fs::read_to_string(&path)?;
    Ok(serde_json::from_str::<Value>(&content)
        .ok()
        .filter(Value::is_object))
}

fn should_run_native_setup(
    running_config: Option<&ConfigSnapshot>,
    local_setup_config: Option<&Value>,
    local_explicit_primary_model_configured: bool,
) -> bool {
    match running_config {
        Some(snapshot) => snapshot.config.is_some() && !snapshot.explicit_primary_model_configured,
        None => local_setup_config.is_some() && !local_explicit_primary_model_configured,
    }
}

fn configured_provider_names(config: &Value) -> HashSet<String> {
    config
        .pointer("/models/providers")
        .and_then(Value::as_object)
        .map(|providers| providers.keys().cloned().collect())
        .unwrap_or_default()
}

fn object_field_mut<'a>(
    object: &'a mut serde_json::Map<String, Value>,
    key: &str,
) -> TuiResult<&'a mut serde_json::Map<String, Value>> {
    let value = object.entry(key.to_string()).or_insert_with(|| json!({}));
    if value.is_null() {
        *value = json!({});
    }
    value
        .as_object_mut()
        .ok_or_else(|| format!("Configuration field '{key}' must be an object").into())
}

fn next_config_backup(path: &Path) -> PathBuf {
    let first = path.with_extension("json.bak");
    if !first.exists() {
        return first;
    }
    (1_u32..)
        .map(|suffix| path.with_extension(format!("json.bak.{suffix}")))
        .find(|candidate| !candidate.exists())
        .expect("the filesystem cannot contain every numeric backup name")
}

fn write_private_config_temp(path: &Path, contents: &[u8]) -> io::Result<PathBuf> {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut file_name = path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| std::ffi::OsString::from("config"));
    file_name.push(format!(".tui-{}-{nonce}.tmp", std::process::id()));
    let temporary_path = path.with_file_name(file_name);

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&temporary_path)?;
    #[cfg(unix)]
    {
        let mut permissions = file.metadata()?.permissions();
        permissions.set_mode(0o600);
        file.set_permissions(permissions)?;
    }
    if let Err(error) = file
        .write_all(contents)
        .and_then(|()| file.flush())
        .and_then(|()| file.sync_all())
    {
        drop(file);
        let _ = std::fs::remove_file(&temporary_path);
        return Err(error);
    }
    drop(file);
    Ok(temporary_path)
}

fn build_native_setup_config(setup: &NativeSetup, mut config: Value) -> TuiResult<Value> {
    let provider = setup.provider();
    let model_ref = format!("{}/{}", setup.provider_name, setup.model_id);
    let root = config
        .as_object_mut()
        .ok_or("configuration root must be an object")?;
    let models = object_field_mut(root, "models")?;
    let providers = object_field_mut(models, "providers")?;
    if providers.contains_key(&setup.provider_name) {
        return Err(format!("Provider '{}' already exists", setup.provider_name).into());
    }
    providers.insert(
        setup.provider_name.clone(),
        json!({
            "baseUrl": setup.base_url,
            "apiKey": setup.api_key,
            "api": provider.api(),
            "models": [{
                "id": setup.model_id,
                "name": setup.model_id,
                "reasoning": setup.reasoning,
                "input": ["text"],
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
                "contextWindow": 128000,
                "maxTokens": 32768
            }]
        }),
    );
    let agents = object_field_mut(root, "agents")?;
    let defaults = object_field_mut(agents, "defaults")?;
    let model = object_field_mut(defaults, "model")?;
    model.insert("primary".to_string(), json!(model_ref));

    crate::config::normalize_json_model_effort_order(&mut config);
    let parsed = serde_json::from_value::<crate::config::JsonConfig>(config.clone())?;
    crate::config::validate_json_provider_names(&parsed)?;
    crate::config::validate_json_provider_models(&parsed)?;
    crate::config::validate_json_agent_model_refs(&parsed)?;

    Ok(config)
}

fn save_native_setup_config(config: &Value) -> TuiResult<()> {
    let path = crate::config_file_path().ok_or("Cannot determine configuration path")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let pretty = serde_json::to_string_pretty(&config)?;
    let tmp = write_private_config_temp(&path, pretty.as_bytes())?;
    if path.exists() {
        let backup = next_config_backup(&path);
        if let Err(error) = std::fs::copy(&path, &backup) {
            let _ = std::fs::remove_file(&tmp);
            return Err(io::Error::other(format!(
                "could not back up '{}' to '{}': {error}",
                path.display(),
                backup.display()
            ))
            .into());
        }
    }
    if let Err(error) = crate::replace_file_from_temp(&path, &tmp) {
        let _ = std::fs::remove_file(&tmp);
        return Err(error.into());
    }
    Ok(())
}

async fn run_native_setup(
    options: &TuiOptions,
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    config: Value,
) -> TuiResult<Value> {
    let mut setup = NativeSetup::new(configured_provider_names(&config));
    let mut events = EventStream::new();
    loop {
        terminal.draw(|frame| render_native_setup(frame, options, &setup))?;
        match events.next().await {
            Some(Ok(Event::Key(key))) if key.kind == crossterm::event::KeyEventKind::Press => {
                if key.code == KeyCode::Esc {
                    return Err("TUI setup cancelled".into());
                }
                match setup.step {
                    1 => match key.code {
                        KeyCode::Up => {
                            setup.provider_index = setup.provider_index.saturating_sub(1)
                        }
                        KeyCode::Down => {
                            setup.provider_index =
                                (setup.provider_index + 1).min(SetupProviderKind::ALL.len() - 1)
                        }
                        KeyCode::Enter => match setup.advance() {
                            Ok(_) => {}
                            Err(error) => setup.error = error,
                        },
                        _ => {}
                    },
                    6 => match key.code {
                        KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') => {
                            setup.reasoning = !setup.reasoning
                        }
                        KeyCode::Enter => match setup.advance() {
                            Ok(_) => {}
                            Err(error) => setup.error = error,
                        },
                        _ => {}
                    },
                    _ => match key.code {
                        KeyCode::Backspace => {
                            if let Some(value) = setup.editable_value_mut() {
                                value.pop();
                            }
                        }
                        KeyCode::Char(character)
                            if !key
                                .modifiers
                                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                        {
                            if let Some(value) = setup.editable_value_mut() {
                                value.push(character);
                            }
                        }
                        KeyCode::Enter => match setup.advance() {
                            Ok(true) => {
                                return build_native_setup_config(&setup, config);
                            }
                            Ok(false) => {}
                            Err(error) => setup.error = error,
                        },
                        _ => {}
                    },
                }
            }
            Some(Ok(Event::Paste(text))) => {
                if let Some(value) = setup.editable_value_mut() {
                    value.push_str(text.trim_end_matches(['\r', '\n']));
                }
            }
            Some(Ok(_)) => {}
            Some(Err(error)) => return Err(error.into()),
            None => return Err("terminal input closed during setup".into()),
        }
    }
}

fn render_native_setup(frame: &mut Frame<'_>, options: &TuiOptions, setup: &NativeSetup) {
    let area = centered_rect(78, 76, frame.area());
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(tr_raw(
            options.language,
            "LingClaw 首次配置",
            "LingClaw first-time setup",
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(112, 88, 226)));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let provider = setup.provider();
    let (heading, value, hint, secret) = match setup.step {
        0 => (
            tr_raw(options.language, "欢迎使用 LingClaw", "Welcome to LingClaw"),
            tr_raw(
                options.language,
                "TUI 将配置一个 Provider、模型和主 Agent。凭据保存在本机配置文件中。",
                "The TUI will configure one provider, model, and primary Agent. Credentials stay in the local configuration file.",
            )
            .to_string(),
            tr_raw(options.language, "Enter 继续 · Esc 取消", "Enter continue · Esc cancel"),
            false,
        ),
        1 => (
            tr_raw(options.language, "选择 Provider 类型", "Choose provider type"),
            SetupProviderKind::ALL
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    format!("{} {}", if index == setup.provider_index { "›" } else { " " }, item.label())
                })
                .collect::<Vec<_>>()
                .join("\n"),
            tr_raw(options.language, "↑/↓ 选择 · Enter 确认", "↑/↓ select · Enter confirm"),
            false,
        ),
        2 => (
            tr_raw(options.language, "Provider 名称", "Provider name"),
            setup.provider_name.clone(),
            tr_raw(options.language, "用于模型引用的前缀", "Prefix used in model references"),
            false,
        ),
        3 => ("Base URL", setup.base_url.clone(), "Enter confirm", false),
        4 => (
            "API Key",
            if setup.api_key.is_empty() {
                String::new()
            } else {
                "•".repeat(setup.api_key.chars().count())
            },
            if provider.api_key_optional() {
                tr_raw(options.language, "本地 Ollama 可留空", "Optional for local Ollama")
            } else {
                tr_raw(options.language, "输入内容不会显示", "Input remains hidden")
            },
            true,
        ),
        5 => (
            tr_raw(options.language, "模型 ID", "Model ID"),
            setup.model_id.clone(),
            tr_raw(options.language, "不要包含 Provider 前缀", "Do not include the provider prefix"),
            false,
        ),
        6 => (
            tr_raw(options.language, "推理模型", "Reasoning model"),
            if setup.reasoning {
                tr_raw(options.language, "是", "Yes").to_string()
            } else {
                tr_raw(options.language, "否", "No").to_string()
            },
            tr_raw(options.language, "←/→ 或 Space 切换", "←/→ or Space toggle"),
            false,
        ),
        _ => (
            tr_raw(options.language, "确认配置", "Confirm configuration"),
            format!(
                "{}\n{}\n{}\n{}",
                provider.label(), setup.provider_name, setup.base_url, setup.model_id
            ),
            tr_raw(options.language, "Enter 保存并启动 · Esc 取消", "Enter save and start · Esc cancel"),
            false,
        ),
    };
    let progress = format!(
        "{} {}/{}",
        tr_raw(options.language, "步骤", "Step"),
        setup.step + 1,
        NativeSetup::LAST_STEP + 1
    );
    let mut text = vec![
        Line::styled(progress, Style::default().fg(Color::DarkGray)),
        Line::default(),
        Line::styled(heading, Style::default().add_modifier(Modifier::BOLD)),
        Line::default(),
    ];
    text.extend(value.lines().map(|line| {
        Line::styled(
            line.to_string(),
            if secret {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default()
            },
        )
    }));
    text.push(Line::default());
    text.push(Line::styled(hint, Style::default().fg(Color::DarkGray)));
    if !setup.error.is_empty() {
        text.push(Line::default());
        text.push(Line::styled(
            setup.error.clone(),
            Style::default().fg(Color::Red),
        ));
    }
    frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), inner);
}

fn render_starting(frame: &mut Frame<'_>, options: &TuiOptions) {
    let area = centered_rect(64, 30, frame.area());
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(tr_raw(
            options.language,
            "正在连接或启动本地 LingClaw daemon…",
            "Connecting to or starting the local LingClaw daemon…",
        ))
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .title("LingClaw TUI")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Rgb(112, 88, 226))),
        ),
        area,
    );
}

fn acknowledge_outbound_send(app: &mut App) {
    // A successful WebSocket write only confirms transport delivery. The
    // daemon's start/group_run_started events are authoritative for run state;
    // commands and validation failures may reply without ever starting a run.
    app.quit_armed = false;
}

fn mark_socket_connected(app: &mut App) {
    app.socket_generation = app.socket_generation.wrapping_add(1).max(1);
    app.connected = true;
}

fn mark_socket_disconnected(app: &mut App) {
    app.connected = false;
    if app.pending_outbound_write.is_some() {
        app.outbound_reconnect_pending = true;
    }
}

pub(crate) async fn run(args: &[String]) -> TuiResult<()> {
    let mut options = TuiOptions::parse(args)?;
    let port_was_explicit = args.iter().any(|argument| argument == "--port");
    // Keep control-plane requests bounded so a stuck daemon cannot freeze the
    // terminal event loop. Uploads use a separate client because a large image
    // may legitimately keep making progress beyond the control timeout.
    let client = build_control_client(CONTROL_REQUEST_TIMEOUT)?;
    let upload_client = build_upload_client()?;
    let initial_base = format!("http://127.0.0.1:{}", options.port);
    let initial_health = daemon_health_status(&client, &initial_base).await;
    if initial_health == DaemonHealth::IncompatibleLegacy {
        return Err(incompatible_daemon_message(&initial_base).into());
    }
    let running_config = match initial_health {
        DaemonHealth::Compatible => Some(fetch_config_snapshot(&client, &initial_base).await?),
        DaemonHealth::IncompatibleLegacy | DaemonHealth::Unavailable => None,
    };
    let local_setup_config = if running_config.is_none() {
        load_setup_config()?
    } else {
        None
    };
    let local_config = Config::load();
    let config_needs_repair = running_config
        .as_ref()
        .is_some_and(|snapshot| snapshot.config.is_none())
        || (running_config.is_none() && local_setup_config.is_none());
    let needs_native_setup = should_run_native_setup(
        running_config.as_ref(),
        local_setup_config.as_ref(),
        local_config.explicit_primary_model_configured,
    );
    let (_guard, mut terminal) = TerminalGuard::enter()?;
    terminal.clear()?;
    if needs_native_setup {
        let setup_config = running_config
            .as_ref()
            .and_then(|snapshot| snapshot.config.clone())
            .or_else(|| local_setup_config.clone())
            .expect("native setup requires a valid object configuration");
        let configured = run_native_setup(&options, &mut terminal, setup_config).await?;
        if let Some(snapshot) = running_config.as_ref() {
            save_edited_config(
                &client,
                &initial_base,
                crate::MAIN_SESSION_ID,
                configured,
                snapshot.etag.as_deref(),
            )
            .await?;
        } else {
            save_native_setup_config(&configured)?;
        }
        if !port_was_explicit && running_config.is_none() {
            options.port = Config::load().port;
        }
    }
    terminal.draw(|frame| render_starting(frame, &options))?;
    let port = ensure_daemon(&client, options.port).await?;
    let base = format!("http://127.0.0.1:{port}");
    let groups_enabled = fetch_group_feature(&client, &base).await?;
    let sessions = fetch_sessions(&client, &base, None).await?;

    terminal.clear()?;
    #[cfg(feature = "tui-images")]
    let (image_protocol, image_picker) = detect_image_support();
    #[cfg(not(feature = "tui-images"))]
    let (image_protocol, _) = detect_image_support();
    let session = choose_start_session(&client, &base, &options, &sessions, &mut terminal).await?;
    let all_sessions = fetch_sessions(&client, &base, None).await?;
    let mut app = App::new(
        &options,
        session,
        all_sessions,
        groups_enabled,
        image_protocol,
    );
    if config_needs_repair {
        app.status = tr(
            &app,
            "配置 JSON 无效；请打开设置并使用 Raw JSON 编辑器修复。",
            "Configuration JSON is invalid; open Settings and use the Raw JSON editor to repair it.",
        )
        .to_string();
    }
    #[cfg(feature = "tui-images")]
    {
        app.image_picker = image_picker;
    }
    if groups_enabled {
        app.groups = fetch_groups(&client, &base).await.unwrap_or_default();
    }
    let mut socket = Some(connect_socket(&base, &app.session.id, None).await?);
    mark_socket_connected(&mut app);
    let mut events = EventStream::new();
    let mut reconnect_tick = tokio::time::interval(Duration::from_secs(1));
    reconnect_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut image_upload_task: Option<tokio::task::JoinHandle<TuiResult<Vec<Value>>>> = None;
    let mut image_upload_context: Option<ImageUploadContext> = None;
    let mut image_upload_draft: Option<ComposerSnapshot> = None;
    let (image_preview_tx, mut image_preview_rx) = mpsc::channel::<TerminalImagePreviewEvent>(1);
    let mut image_preview_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut image_preview_context: Option<TerminalImagePreviewContext> = None;

    loop {
        reconcile_terminal_image_preview(
            &client,
            &mut app,
            &image_preview_tx,
            &mut image_preview_task,
            &mut image_preview_context,
        );
        terminal.draw(|frame| render(frame, &mut app))?;
        tokio::select! {
            terminal_event = events.next() => {
                match terminal_event {
                    Some(Ok(Event::Key(key))) if key.kind == crossterm::event::KeyEventKind::Press => {
                        let composer_before = ComposerSnapshot::capture(&app);
                        match handle_key(&mut app, key) {
                            UserAction::None => {}
                            UserAction::Exit => break,
                            UserAction::Send(text) => {
                                if block_image_upload_interaction(&mut app) {
                                    composer_before.restore_after_failed_send(&mut app);
                                    continue;
                                }
                                let tracks_storage_write =
                                    outbound_payload_requires_storage_write(&text);
                                if let Some(current) = socket.as_mut() {
                                    let result = current.send(Message::Text(text.into())).await;
                                    match result {
                                        Ok(()) => {
                                            acknowledge_outbound_send(&mut app);
                                            if tracks_storage_write {
                                                app.pending_outbound_write =
                                                    Some(composer_before.clone());
                                            }
                                        }
                                        Err(error) => {
                                            composer_before.restore_after_failed_send(&mut app);
                                            socket = None;
                                            mark_socket_disconnected(&mut app);
                                            app.status = reconnect_status(&app, &error.to_string());
                                        }
                                    }
                                } else {
                                    app.status = tr(&app, "尚未连接，消息未发送", "Not connected; message was not sent").to_string();
                                }
                            }
                            UserAction::Stop(text) => {
                                if let Some(current) = socket.as_mut() {
                                    if let Err(error) = current.send(Message::Text(text.into())).await {
                                        socket = None;
                                        mark_socket_disconnected(&mut app);
                                        app.status = reconnect_status(&app, &error.to_string());
                                    }
                                } else {
                                    app.status = tr(&app, "尚未连接，无法停止运行", "Not connected; the run could not be stopped").to_string();
                                }
                            }
                            UserAction::SwitchSession(session_id) => {
                                if block_image_upload_interaction(&mut app) {
                                    continue;
                                }
                                if let Some(session) = app.sessions.iter().find(|item| crate::session_ids_match(&item.id, &session_id)).cloned() {
                                    if let Some(mut current) = socket.take() {
                                        let _ = current.close(None).await;
                                    }
                                    app.session = session;
                                    app.active_group = None;
                                    app.group_target_mode = "all".to_string();
                                    app.group_targets.clear();
                                    reset_target_scoped_state(&mut app);
                                    match connect_socket(&base, &app.session.id, None).await {
                                        Ok(connected) => {
                                            socket = Some(connected);
                                            mark_socket_connected(&mut app);
                                        }
                                        Err(error) => {
                                            app.connected = false;
                                            app.status = reconnect_status(&app, &error.to_string());
                                        }
                                    }
                                    if app.page() != Page::Chat {
                                        reload_current_page(&client, &base, &mut app).await;
                                    }
                                }
                            }
                            UserAction::SwitchGroup(group_id) => {
                                if block_image_upload_interaction(&mut app) {
                                    continue;
                                }
                                if app.groups_enabled {
                                    if let Err(error) = activate_group_target(&mut app, &group_id) {
                                        app.status = error;
                                        continue;
                                    }
                                    if let Some(mut current) = socket.take() {
                                        let _ = current.close(None).await;
                                    }
                                    match connect_socket(&base, "main", Some(&group_id)).await {
                                        Ok(connected) => {
                                            socket = Some(connected);
                                            mark_socket_connected(&mut app);
                                        }
                                        Err(error) => {
                                            app.connected = false;
                                            app.status = reconnect_status(&app, &error.to_string());
                                        }
                                    }
                                    if app.page() != Page::Chat {
                                        reload_current_page(&client, &base, &mut app).await;
                                    }
                                }
                            }
                            UserAction::Load(page) => {
                                app.inspector_scroll = 0;
                                reload_page(&client, &base, &mut app, page).await;
                            }
                            UserAction::OpenImage => {
                                if let Some(url) = app.last_image_url.as_deref()
                                    && let Err(error) = open_external(url)
                                {
                                    app.status = error;
                                }
                            }
                            UserAction::UploadImage(path) => {
                                match ImageUploadContext::capture(&app) {
                                    Ok(context) => {
                                        let upload_client = upload_client.clone();
                                        let upload_base = base.clone();
                                        let task_context = context.clone();
                                        image_upload_task = Some(tokio::spawn(async move {
                                            upload_local_image(
                                                &upload_client,
                                                &upload_base,
                                                &task_context,
                                                &path,
                                            )
                                            .await
                                        }));
                                        image_upload_context = Some(context);
                                        image_upload_draft = Some(composer_before);
                                        app.upload_in_progress = true;
                                        app.status = tr(&app, "正在上传图片…", "Uploading image…")
                                            .to_string();
                                    }
                                    Err(error) => {
                                        composer_before.restore_after_failed_send(&mut app);
                                        app.status = error;
                                    }
                                }
                            }
                            UserAction::EditConfig => {
                                if block_image_upload_interaction(&mut app) {
                                    continue;
                                }
                                match edit_config(
                                    &client,
                                    &base,
                                    &mut terminal,
                                    &mut events,
                                    app.language,
                                    &app.session.id,
                                )
                                .await
                                {
                                    Ok(message) => {
                                        app.status = message;
                                        if app.page() == Page::Settings {
                                            reload_page(&client, &base, &mut app, Page::Settings).await;
                                        }
                                    }
                                    Err(error) => {
                                        if error.downcast_ref::<TerminalRestoreError>().is_some() {
                                            return Err(error);
                                        }
                                        app.status = error.to_string();
                                    }
                                }
                            }
                            UserAction::ToggleGroups => {
                                if block_image_upload_interaction(&mut app) {
                                    continue;
                                }
                                let next = !app.groups_enabled;
                                match set_groups_enabled(&client, &base, &app.session.id, next).await {
                                    Ok(message) => {
                                        let socket_action = apply_socket_event(
                                            &mut app,
                                            json!({
                                                "type": "feature_status",
                                                "features": { "groups": next },
                                            }),
                                        );
                                        apply_socket_event_action(
                                            socket_action,
                                            &client,
                                            &mut socket,
                                            &base,
                                            &mut app,
                                        )
                                        .await;
                                        if app.status.is_empty() {
                                            app.status = message;
                                        }
                                        if app.page() == Page::Settings {
                                            reload_page(&client, &base, &mut app, Page::Settings).await;
                                        }
                                    }
                                    Err(error) => app.status = error.to_string(),
                                }
                            }
                            UserAction::MutatePage(mutation) => {
                                if block_image_upload_interaction(&mut app) {
                                    continue;
                                }
                                match execute_page_mutation(&client, &base, &mut app, mutation).await {
                                    Ok(message) => {
                                        app.status = message;
                                        reload_current_page(&client, &base, &mut app).await;
                                    }
                                    Err(error) => app.status = error.to_string(),
                                }
                            }
                            UserAction::Manage(action) => {
                                if block_image_upload_interaction(&mut app) {
                                    app.input = composer_before.input;
                                    continue;
                                }
                                match execute_management_action(&client, &base, &mut app, action).await {
                                    Ok(ManagementOutcome::Stay) => {
                                        if app.page() != Page::Chat {
                                            reload_current_page(&client, &base, &mut app).await;
                                        }
                                    }
                                    Ok(ManagementOutcome::SwitchSession(session)) => {
                                        if let Some(mut current) = socket.take() {
                                            let _ = current.close(None).await;
                                        }
                                        app.session = session;
                                        app.active_group = None;
                                        app.group_target_mode = "all".to_string();
                                        app.group_targets.clear();
                                        reset_target_scoped_state(&mut app);
                                        app.nav_index = app
                                            .sessions
                                            .iter()
                                            .position(|item| crate::session_ids_match(&item.id, &app.session.id))
                                            .unwrap_or(0);
                                        match connect_socket(&base, &app.session.id, None).await {
                                            Ok(connected) => {
                                                socket = Some(connected);
                                                mark_socket_connected(&mut app);
                                            }
                                            Err(error) => {
                                                app.connected = false;
                                                app.status = reconnect_status(&app, &error.to_string());
                                            }
                                        }
                                        if app.page() != Page::Chat {
                                            reload_current_page(&client, &base, &mut app).await;
                                        }
                                    }
                                    Ok(ManagementOutcome::SwitchGroup(group_id)) => {
                                        if !app.groups_enabled {
                                            app.status = tr(
                                                &app,
                                                "群聊已由配置关闭",
                                                "Group chat is disabled by configuration",
                                            )
                                            .to_string();
                                            continue;
                                        }
                                        if let Err(error) = activate_group_target(&mut app, &group_id) {
                                            app.status = error;
                                            continue;
                                        }
                                        if let Some(mut current) = socket.take() {
                                            let _ = current.close(None).await;
                                        }
                                        app.nav_index = app.sessions.len()
                                            + app.groups.iter().position(|group| group.id == group_id).unwrap_or(0);
                                        match connect_socket(&base, "main", Some(&group_id)).await {
                                            Ok(connected) => {
                                                socket = Some(connected);
                                                mark_socket_connected(&mut app);
                                            }
                                            Err(error) => {
                                                app.connected = false;
                                                app.status = reconnect_status(&app, &error.to_string());
                                            }
                                        }
                                        if app.page() != Page::Chat {
                                            reload_current_page(&client, &base, &mut app).await;
                                        }
                                    }
                                    Err(error) => {
                                        app.input = composer_before.input;
                                        app.status = error.to_string();
                                    }
                                }
                            }
                        }
                    }
                    Some(Ok(Event::Resize(_, _))) => {}
                    Some(Ok(_)) => {}
                    Some(Err(error)) => app.status = format!("terminal event error: {error}"),
                    None => break,
                }
            }
            websocket_event = async {
                match socket.as_mut() {
                    Some(socket) => socket.next().await,
                    None => std::future::pending().await,
                }
            } => {
                match websocket_event {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<Value>(&text) {
                            Ok(value) => {
                                let socket_action = apply_socket_event(&mut app, value);
                                apply_socket_event_action(
                                    socket_action,
                                    &client,
                                    &mut socket,
                                    &base,
                                    &mut app,
                                )
                                .await;
                            }
                            Err(_) => app.push("event", text.to_string(), LineKind::System),
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        socket = None;
                        mark_socket_disconnected(&mut app);
                        app.status = reconnect_status(&app, "");
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        if let Some(current) = socket.as_mut()
                            && let Err(error) = current.send(Message::Pong(payload)).await
                        {
                            socket = None;
                            mark_socket_disconnected(&mut app);
                            app.status = reconnect_status(&app, &error.to_string());
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(error)) => {
                        socket = None;
                        mark_socket_disconnected(&mut app);
                        app.status = reconnect_status(&app, &error.to_string());
                    }
                }
            }
            upload_result = async {
                image_upload_task
                    .as_mut()
                    .expect("image upload branch is guarded")
                    .await
            }, if image_upload_task.is_some() => {
                image_upload_task = None;
                app.upload_in_progress = false;
                let context = image_upload_context.take();
                let draft = image_upload_draft.take();
                match (upload_result, context, draft) {
                    (Ok(Ok(images)), Some(context), _) if context.is_current(&app) => {
                        let count = images.len();
                        app.pending_images.extend(images);
                        app.status = format!(
                            "{}: {count}",
                            tr(&app, "已附加图片", "Images attached")
                        );
                    }
                    (Ok(Ok(_)), Some(_), Some(draft)) => {
                        draft.restore_composer(&mut app);
                        app.status = tr(
                            &app,
                            "上传期间 Session、连接或模型能力发生变化；请重新附加图片",
                            "The Session, connection, or model capability changed while uploading; attach the image again",
                        )
                        .to_string();
                    }
                    (Ok(Err(error)), _, Some(draft)) => {
                        draft.restore_composer(&mut app);
                        app.status = error.to_string();
                    }
                    (Err(error), _, Some(draft)) => {
                        draft.restore_composer(&mut app);
                        app.status = format!("image upload task failed: {error}");
                    }
                    (Ok(Ok(_)), _, _) => {
                        app.status = tr(
                            &app,
                            "图片上传上下文已失效；请重新附加图片",
                            "The image upload context expired; attach the image again",
                        )
                        .to_string();
                    }
                    (Ok(Err(error)), _, _) => app.status = error.to_string(),
                    (Err(error), _, _) => {
                        app.status = format!("image upload task failed: {error}")
                    }
                }
            }
            preview_event = image_preview_rx.recv() => {
                if let Some(preview_event) = preview_event
                    && apply_terminal_image_preview(
                        &mut app,
                        preview_event,
                        image_preview_context.as_ref(),
                    )
                {
                    image_preview_task = None;
                }
            }
            _ = reconnect_tick.tick(), if socket.is_none() => {
                match connect_socket(&base, &app.session.id, app.active_group.as_deref()).await {
                    Ok(connected) => {
                        socket = Some(connected);
                        mark_socket_connected(&mut app);
                        app.status = tr(&app, "已重新连接", "Reconnected").to_string();
                    }
                    Err(error) => {
                        let feature_action = if app.active_group.is_some() {
                            match tokio::time::timeout(
                                Duration::from_millis(800),
                                fetch_group_feature(&client, &base),
                            )
                            .await
                            {
                                Ok(Ok(groups_enabled)) => {
                                    apply_group_reconnect_feature_probe(&mut app, groups_enabled)
                                }
                                Ok(Err(_)) | Err(_) => SocketEventAction::None,
                            }
                        } else {
                            SocketEventAction::None
                        };
                        if feature_action == SocketEventAction::None {
                            app.connected = false;
                            app.status = reconnect_status(&app, &error.to_string());
                        } else {
                            apply_socket_event_action(
                                feature_action,
                                &client,
                                &mut socket,
                                &base,
                                &mut app,
                            )
                            .await;
                        }
                    }
                }
            }
        }
    }
    if let Some(task) = image_upload_task.take() {
        task.abort();
    }
    if let Some(task) = image_preview_task.take() {
        task.abort();
    }
    terminal.clear()?;
    Ok(())
}

fn reconnect_status(app: &App, error: &str) -> String {
    let message = tr(app, "连接已断开，正在重连…", "Disconnected; reconnecting…");
    if error.is_empty() {
        message.to_string()
    } else {
        format!("{message} {error}")
    }
}

async fn ensure_daemon(client: &Client, port: u16) -> TuiResult<u16> {
    let base = format!("http://127.0.0.1:{port}");
    match daemon_health_status(client, &base).await {
        DaemonHealth::Compatible => return Ok(port),
        DaemonHealth::IncompatibleLegacy => {
            return Err(incompatible_daemon_message(&base).into());
        }
        DaemonHealth::Unavailable => {}
    }
    let started_port = cli::start_daemon_for_tui(Some(port)).map_err(io::Error::other)?;
    let started_base = format!("http://127.0.0.1:{started_port}");
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        match daemon_health_status(client, &started_base).await {
            DaemonHealth::Compatible => return Ok(started_port),
            DaemonHealth::IncompatibleLegacy => {
                return Err(incompatible_daemon_message(&started_base).into());
            }
            DaemonHealth::Unavailable => {}
        }
    }
    Err(format!("LingClaw daemon did not become healthy at {started_base}").into())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DaemonHealth {
    Compatible,
    IncompatibleLegacy,
    Unavailable,
}

fn incompatible_daemon_message(base: &str) -> String {
    format!(
        "An incompatible older LingClaw daemon is running at {base}. Stop or restart it with the current LingClaw executable before using TUI workspace selection."
    )
}

async fn daemon_health_status(client: &Client, base: &str) -> DaemonHealth {
    let Ok(response) = client
        .get(format!("{base}/api/health"))
        .timeout(Duration::from_millis(800))
        .send()
        .await
    else {
        return DaemonHealth::Unavailable;
    };
    if !response.status().is_success() {
        return DaemonHealth::Unavailable;
    }
    response
        .json::<Value>()
        .await
        .map_or(DaemonHealth::Unavailable, |payload| {
            classify_lingclaw_health_payload(&payload)
        })
}

fn classify_lingclaw_health_payload(payload: &Value) -> DaemonHealth {
    if payload.get("status").and_then(Value::as_str) != Some("ok") {
        return DaemonHealth::Unavailable;
    }
    if let Some(service) = payload.get("service") {
        return if service.as_str() == Some("lingclaw") {
            DaemonHealth::Compatible
        } else {
            DaemonHealth::Unavailable
        };
    }

    // LingClaw versions released before the explicit service marker used this
    // distinctive shape. Detect it only to produce an actionable restart
    // message: those daemons do not implement workspace-filtered Session APIs.
    if payload.get("version").is_some_and(Value::is_string)
        && payload
            .get("model_configured")
            .is_some_and(Value::is_boolean)
        && payload.get("sessions").is_some_and(Value::is_u64)
        && payload
            .pointer("/storage/mode")
            .is_some_and(Value::is_string)
    {
        DaemonHealth::IncompatibleLegacy
    } else {
        DaemonHealth::Unavailable
    }
}

fn build_control_client(timeout: Duration) -> TuiResult<Client> {
    Ok(Client::builder()
        .connect_timeout(CONTROL_CONNECT_TIMEOUT)
        .timeout(timeout)
        .build()?)
}

fn build_upload_client() -> TuiResult<Client> {
    Ok(Client::builder()
        .connect_timeout(CONTROL_CONNECT_TIMEOUT)
        .build()?)
}

async fn fetch_group_feature(client: &Client, base: &str) -> TuiResult<bool> {
    let value: Value = client
        .get(format!("{base}/api/client-config"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(value
        .pointer("/features/groups")
        .and_then(Value::as_bool)
        .unwrap_or(false))
}

fn apply_group_reconnect_feature_probe(app: &mut App, groups_enabled: bool) -> SocketEventAction {
    if groups_enabled || app.active_group.is_none() {
        return SocketEventAction::None;
    }
    apply_socket_event(
        app,
        json!({"type":"feature_status","features":{"groups":false}}),
    )
}

async fn fetch_sessions(
    client: &Client,
    base: &str,
    workspace: Option<&Path>,
) -> TuiResult<Vec<SessionSummary>> {
    let mut request = client.get(format!("{base}/api/sessions"));
    if let Some(workspace) = workspace {
        let workspace = workspace
            .to_str()
            .ok_or("workspace path must be valid UTF-8")?;
        request = request.query(&[("workspace", workspace)]);
    }
    let response: SessionsResponse = request.send().await?.error_for_status()?.json().await?;
    Ok(response.sessions)
}

async fn fetch_groups(client: &Client, base: &str) -> TuiResult<Vec<GroupSummary>> {
    let response: GroupsResponse = client
        .get(format!("{base}/api/session-groups"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(response.groups)
}

async fn checked_json(response: reqwest::Response) -> TuiResult<Value> {
    let status = response.status();
    let body = response.text().await?;
    let value = if body.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str::<Value>(&body).map_err(|error| {
            format!(
                "LingClaw returned invalid JSON (HTTP {}): {error}",
                status.as_u16()
            )
        })?
    };
    if status.is_success() {
        return Ok(value);
    }
    let message = value
        .get("error")
        .and_then(Value::as_str)
        .or_else(|| value.get("message").and_then(Value::as_str))
        .map(str::to_string)
        .unwrap_or_else(|| format!("LingClaw request failed with HTTP {}", status.as_u16()));
    let code = value
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or_default();
    Err(if code.is_empty() {
        message.into()
    } else {
        format!("{message} ({code})").into()
    })
}

fn workspace_request(workspace: &WorkspaceSelection) -> TuiResult<Value> {
    Ok(match workspace {
        WorkspaceSelection::Managed => json!({ "kind": "managed" }),
        WorkspaceSelection::Directory(path) => {
            let path = path.to_str().ok_or("workspace path must be valid UTF-8")?;
            json!({ "kind": "directory", "path": path })
        }
    })
}

fn find_session_summary(app: &App, requested: &str) -> Result<SessionSummary, String> {
    if let Some(session) = app
        .sessions
        .iter()
        .find(|session| crate::session_ids_match(&session.id, requested))
    {
        return Ok(session.clone());
    }
    let matches = app
        .sessions
        .iter()
        .filter(|session| session.name.eq_ignore_ascii_case(requested))
        .cloned()
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [session] => Ok(session.clone()),
        [] => Err(match app.language {
            UiLanguage::ZhCn => format!("未找到 Session“{requested}”"),
            UiLanguage::En => format!("Session '{requested}' was not found"),
        }),
        _ => Err(match app.language {
            UiLanguage::ZhCn => {
                format!("多个 Session 名称均为“{requested}”，请改用 Session ID")
            }
            UiLanguage::En => {
                format!("more than one Session is named '{requested}'; use its id instead")
            }
        }),
    }
}

fn find_group_summary(app: &App, requested: &str) -> Result<GroupSummary, String> {
    if let Some(group) = app.groups.iter().find(|group| group.id == requested) {
        return Ok(group.clone());
    }
    let matches = app
        .groups
        .iter()
        .filter(|group| group.name.eq_ignore_ascii_case(requested))
        .cloned()
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [group] => Ok(group.clone()),
        [] => Err(match app.language {
            UiLanguage::ZhCn => format!("未找到群聊“{requested}”"),
            UiLanguage::En => format!("Group '{requested}' was not found"),
        }),
        _ => Err(match app.language {
            UiLanguage::ZhCn => format!("多个群聊名称均为“{requested}”，请改用群聊 ID"),
            UiLanguage::En => {
                format!("more than one Group is named '{requested}'; use its id instead")
            }
        }),
    }
}

fn require_session_view(app: &App) -> TuiResult<()> {
    if app.active_group.is_some() {
        Err(tr(
            app,
            "修改名称或工作目录前请先返回 Session",
            "Return to a Session before changing its name or workspace",
        )
        .into())
    } else {
        Ok(())
    }
}

async fn execute_management_action(
    client: &Client,
    base: &str,
    app: &mut App,
    action: ManagementAction,
) -> TuiResult<ManagementOutcome> {
    match action {
        ManagementAction::CreateSession { name, workspace } => {
            let value = checked_json(
                client
                    .post(format!("{base}/api/session"))
                    .json(&json!({
                        "name": name,
                        "workspace": workspace_request(&workspace)?,
                    }))
                    .send()
                    .await?,
            )
            .await?;
            let created: CreatedSessionResponse = serde_json::from_value(value)?;
            app.sessions = fetch_sessions(client, base, None).await?;
            Ok(ManagementOutcome::SwitchSession(created.session))
        }
        ManagementAction::RenameSession(name) => {
            require_session_view(app)?;
            let value = checked_json(
                client
                    .put(format!("{base}/api/session"))
                    .query(&[("session", app.session.id.as_str())])
                    .json(&json!({ "name": name }))
                    .send()
                    .await?,
            )
            .await?;
            let updated: CreatedSessionResponse = serde_json::from_value(value)?;
            app.session = updated.session;
            app.sessions = fetch_sessions(client, base, None).await?;
            app.status = tr(app, "Session 已重命名", "Session renamed").to_string();
            Ok(ManagementOutcome::Stay)
        }
        ManagementAction::RebindSession(workspace) => {
            require_session_view(app)?;
            let value = checked_json(
                client
                    .put(format!("{base}/api/session"))
                    .query(&[("session", app.session.id.as_str())])
                    .json(&json!({ "workspace": workspace_request(&workspace)? }))
                    .send()
                    .await?,
            )
            .await?;
            let updated: CreatedSessionResponse = serde_json::from_value(value)?;
            app.session = updated.session;
            app.sessions = fetch_sessions(client, base, None).await?;
            app.status = tr(app, "Session 工作目录已更新", "Session workspace updated").to_string();
            Ok(ManagementOutcome::Stay)
        }
        ManagementAction::SwitchSession(requested) => Ok(ManagementOutcome::SwitchSession(
            find_session_summary(app, &requested)?,
        )),
        ManagementAction::FindSession(query) => {
            let query = query.to_ascii_lowercase();
            if let Some((index, session)) = app.sessions.iter().enumerate().find(|(_, session)| {
                session.id.to_ascii_lowercase().contains(&query)
                    || session.name.to_ascii_lowercase().contains(&query)
            }) {
                app.nav_index = index;
                app.focus = Focus::Navigation;
                app.status = format!("Session: {} ({})", session.name, session.id);
            } else {
                app.status = tr(app, "没有匹配的 Session", "No matching Session").to_string();
            }
            Ok(ManagementOutcome::Stay)
        }
        ManagementAction::DeleteSession(requested) => {
            let session = find_session_summary(app, &requested)?;
            if crate::is_main(&session.id) {
                return Err(tr(app, "Main 不能删除", "Main cannot be deleted").into());
            }
            if crate::session_ids_match(&session.id, &app.session.id) {
                return Err(tr(
                    app,
                    "删除当前 Session 前请先切换到其他 Session",
                    "Switch to another Session before deleting the active Session",
                )
                .into());
            }
            checked_json(
                client
                    .delete(format!("{base}/api/session"))
                    .query(&[("session", session.id.as_str())])
                    .send()
                    .await?,
            )
            .await?;
            app.sessions = fetch_sessions(client, base, None).await?;
            app.nav_index = app.nav_index.min(app.target_count().saturating_sub(1));
            app.status = match app.language {
                UiLanguage::ZhCn => format!("Session“{}”已删除", session.name),
                UiLanguage::En => format!("Session '{}' deleted", session.name),
            };
            Ok(ManagementOutcome::Stay)
        }
        ManagementAction::CreateGroup { name, members } => {
            if !app.groups_enabled {
                return Err(tr(
                    app,
                    "群聊已由配置关闭",
                    "Group chat is disabled by configuration",
                )
                .into());
            }
            let value = checked_json(
                client
                    .post(format!("{base}/api/session-group"))
                    .json(&json!({ "name": name, "members": members }))
                    .send()
                    .await?,
            )
            .await?;
            let created: MutatedGroupResponse = serde_json::from_value(value)?;
            app.groups = fetch_groups(client, base).await?;
            Ok(ManagementOutcome::SwitchGroup(created.group.id))
        }
        ManagementAction::RenameGroup(name) => {
            let group_id = app.active_group.clone().ok_or_else(|| {
                tr(
                    app,
                    "请先打开一个群聊再重命名",
                    "Open a Group before renaming it",
                )
                .to_string()
            })?;
            checked_json(
                client
                    .put(format!("{base}/api/session-group"))
                    .query(&[("group", group_id.as_str())])
                    .json(&json!({ "name": name }))
                    .send()
                    .await?,
            )
            .await?;
            app.groups = fetch_groups(client, base).await?;
            app.status = tr(app, "群聊已重命名", "Group renamed").to_string();
            Ok(ManagementOutcome::Stay)
        }
        ManagementAction::ReplaceGroupMembers(members) => {
            let group_id = app.active_group.clone().ok_or_else(|| {
                tr(
                    app,
                    "请先打开一个群聊再修改成员",
                    "Open a Group before changing its members",
                )
                .to_string()
            })?;
            checked_json(
                client
                    .put(format!("{base}/api/session-group"))
                    .query(&[("group", group_id.as_str())])
                    .json(&json!({ "members": members }))
                    .send()
                    .await?,
            )
            .await?;
            app.groups = fetch_groups(client, base).await?;
            app.status = tr(app, "群聊成员已更新", "Group members updated").to_string();
            Ok(ManagementOutcome::Stay)
        }
        ManagementAction::SwitchGroup(requested) => Ok(ManagementOutcome::SwitchGroup(
            find_group_summary(app, &requested)?.id,
        )),
        ManagementAction::DeleteGroup => {
            let group_id = app.active_group.clone().ok_or_else(|| {
                tr(
                    app,
                    "请先打开一个群聊再删除",
                    "Open a Group before deleting it",
                )
                .to_string()
            })?;
            checked_json(
                client
                    .delete(format!("{base}/api/session-group"))
                    .query(&[("group", group_id.as_str())])
                    .send()
                    .await?,
            )
            .await?;
            app.groups = fetch_groups(client, base).await?;
            app.status = tr(app, "群聊已删除", "Group deleted").to_string();
            Ok(ManagementOutcome::SwitchSession(app.session.clone()))
        }
        ManagementAction::PromoteGroupMember(session_id) => {
            let group_id = app.active_group.clone().ok_or_else(|| {
                tr(
                    app,
                    "请先打开一个群聊再设置管理员",
                    "Open a Group before promoting a member",
                )
                .to_string()
            })?;
            checked_json(
                client
                    .put(format!("{base}/api/session-group/member"))
                    .query(&[
                        ("group", group_id.as_str()),
                        ("session", session_id.as_str()),
                    ])
                    .send()
                    .await?,
            )
            .await?;
            app.status = match app.language {
                UiLanguage::ZhCn => format!("群成员“{session_id}”已设为管理员"),
                UiLanguage::En => format!("Group member '{session_id}' promoted"),
            };
            Ok(ManagementOutcome::Stay)
        }
        ManagementAction::RemoveGroupMember(session_id) => {
            let group_id = app.active_group.clone().ok_or_else(|| {
                tr(
                    app,
                    "请先打开一个群聊再移除成员",
                    "Open a Group before removing a member",
                )
                .to_string()
            })?;
            checked_json(
                client
                    .delete(format!("{base}/api/session-group/member"))
                    .query(&[
                        ("group", group_id.as_str()),
                        ("session", session_id.as_str()),
                    ])
                    .send()
                    .await?,
            )
            .await?;
            app.groups = fetch_groups(client, base).await?;
            app.status = match app.language {
                UiLanguage::ZhCn => format!("群成员“{session_id}”已移除"),
                UiLanguage::En => format!("Group member '{session_id}' removed"),
            };
            Ok(ManagementOutcome::Stay)
        }
        ManagementAction::StartMcpOauth(server) => {
            let value = checked_json(
                client
                    .post(format!("{base}/api/mcp/auth/start"))
                    .json(&json!({ "server": server }))
                    .send()
                    .await?,
            )
            .await?;
            if value.get("ok").and_then(Value::as_bool) == Some(false) {
                return Err(value
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("MCP OAuth could not be started")
                    .to_string()
                    .into());
            }
            let url = value
                .get("authorizationUrl")
                .and_then(Value::as_str)
                .filter(|url| !url.is_empty())
                .ok_or("MCP OAuth response did not include an authorization URL")?;
            open_external(url).map_err(io::Error::other)?;
            app.status = match app.language {
                UiLanguage::ZhCn => format!("请在浏览器中完成“{server}”的 OAuth 授权"),
                UiLanguage::En => format!("Complete OAuth for '{server}' in your browser"),
            };
            Ok(ManagementOutcome::Stay)
        }
        ManagementAction::DisconnectMcpOauth(server) => {
            checked_json(
                client
                    .post(format!("{base}/api/mcp/auth/disconnect"))
                    .json(&json!({ "server": server }))
                    .send()
                    .await?,
            )
            .await?;
            app.status = match app.language {
                UiLanguage::ZhCn => format!("已断开“{server}”的 MCP OAuth"),
                UiLanguage::En => format!("MCP OAuth disconnected for '{server}'"),
            };
            Ok(ManagementOutcome::Stay)
        }
    }
}

async fn create_directory_session(
    client: &Client,
    base: &str,
    path: &Path,
) -> TuiResult<SessionSummary> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("Workspace");
    let path = path.to_str().ok_or("workspace path must be valid UTF-8")?;
    let response: CreatedSessionResponse = client
        .post(format!("{base}/api/session"))
        .json(&json!({
            "name": name,
            "workspace": { "kind": "directory", "path": path },
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(response.session)
}

async fn choose_start_session(
    client: &Client,
    base: &str,
    options: &TuiOptions,
    all_sessions: &[SessionSummary],
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
) -> TuiResult<SessionSummary> {
    if let Some(requested) = options.session.as_deref() {
        let session = all_sessions
            .iter()
            .find(|session| crate::session_ids_match(&session.id, requested))
            .cloned()
            .ok_or_else(|| format!("Session '{requested}' was not found"))?;
        if let Some(path) = options.path.as_deref() {
            let matches = fetch_sessions(client, base, Some(path)).await?;
            if !matches
                .iter()
                .any(|candidate| crate::session_ids_match(&candidate.id, &session.id))
            {
                return Err(format!(
                    "Session '{}' is not bound to workspace '{}'",
                    session.id,
                    path.display()
                )
                .into());
            }
        }
        return Ok(session);
    }

    let path = options
        .path
        .as_deref()
        .ok_or("TUI startup requires a workspace path when --session is not supplied")?;
    let matches = fetch_sessions(client, base, Some(path)).await?;
    if matches.len() == 1 {
        return Ok(matches[0].clone());
    }
    let prompt = if matches.is_empty() {
        StartupPrompt::Create
    } else {
        StartupPrompt::Choose(matches)
    };
    run_startup_prompt(client, base, options, terminal, prompt).await
}

enum StartupPrompt {
    Create,
    Choose(Vec<SessionSummary>),
}

async fn run_startup_prompt(
    client: &Client,
    base: &str,
    options: &TuiOptions,
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    prompt: StartupPrompt,
) -> TuiResult<SessionSummary> {
    let workspace_path = options
        .path
        .as_deref()
        .ok_or("TUI startup requires a workspace path when --session is not supplied")?;
    let mut selected = 0usize;
    let mut events = EventStream::new();
    loop {
        terminal.draw(|frame| {
            let area = centered_rect(70, 60, frame.area());
            frame.render_widget(Clear, area);
            let title = match prompt {
                StartupPrompt::Create => tr_raw(
                    options.language,
                    "创建目录工作空间",
                    "Create directory workspace",
                ),
                StartupPrompt::Choose(_) => tr_raw(
                    options.language,
                    "选择工作空间会话",
                    "Choose workspace session",
                ),
            };
            let block = Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Rgb(112, 88, 226)));
            let inner = block.inner(area);
            frame.render_widget(block, area);
            match &prompt {
                StartupPrompt::Create => {
                    let text = format!(
                        "{}\n\n{}\n\n{}",
                        tr_raw(
                            options.language,
                            "此目录还没有匹配的 Session：",
                            "No Session matches this directory:"
                        ),
                        workspace_path.display(),
                        tr_raw(
                            options.language,
                            "Enter 创建 · Esc 取消",
                            "Enter create · Esc cancel"
                        ),
                    );
                    frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), inner);
                }
                StartupPrompt::Choose(items) => {
                    let list = items
                        .iter()
                        .map(|session| ListItem::new(format!("{}  {}", session.name, session.id)))
                        .collect::<Vec<_>>();
                    let mut state = ListState::default().with_selected(Some(selected));
                    frame.render_stateful_widget(
                        List::new(list).highlight_style(
                            Style::default()
                                .fg(Color::White)
                                .bg(Color::Rgb(112, 88, 226)),
                        ),
                        inner,
                        &mut state,
                    );
                }
            }
        })?;
        match events.next().await {
            Some(Ok(Event::Key(key))) if key.kind == crossterm::event::KeyEventKind::Press => {
                match key.code {
                    KeyCode::Esc => return Err("TUI startup cancelled".into()),
                    KeyCode::Up => selected = selected.saturating_sub(1),
                    KeyCode::Down => {
                        if let StartupPrompt::Choose(items) = &prompt {
                            selected = (selected + 1).min(items.len().saturating_sub(1));
                        }
                    }
                    KeyCode::Enter => match &prompt {
                        StartupPrompt::Create => {
                            return create_directory_session(client, base, workspace_path).await;
                        }
                        StartupPrompt::Choose(items) => return Ok(items[selected].clone()),
                    },
                    _ => {}
                }
            }
            Some(Ok(_)) => {}
            Some(Err(error)) => return Err(error.into()),
            None => return Err("terminal input closed".into()),
        }
    }
}

async fn connect_socket(base: &str, session: &str, group: Option<&str>) -> TuiResult<Socket> {
    let mut url = base
        .replacen("http://", "ws://", 1)
        .replacen("https://", "wss://", 1);
    url.push_str("/ws?");
    if let Some(group) = group {
        url.push_str(&format!("session=main&group={}", percent_encode(group)));
    } else {
        url.push_str(&format!("session={}", percent_encode(session)));
    }
    let (socket, _) = tokio::time::timeout(SOCKET_CONNECT_TIMEOUT, connect_async(&url))
        .await
        .map_err(|_| format!("timed out connecting to LingClaw WebSocket at {url}"))??;
    Ok(socket)
}

async fn apply_socket_event_action(
    action: SocketEventAction,
    client: &Client,
    socket: &mut Option<Socket>,
    base: &str,
    app: &mut App,
) {
    match action {
        SocketEventAction::None => {}
        SocketEventAction::RefreshGroups => match fetch_groups(client, base).await {
            Ok(groups) => app.groups = groups,
            Err(error) => app.status = error.to_string(),
        },
        SocketEventAction::ReconnectMain => {
            if let Some(mut current) = socket.take() {
                let _ = current.close(None).await;
            }
            match connect_socket(base, &app.session.id, None).await {
                Ok(connected) => {
                    *socket = Some(connected);
                    mark_socket_connected(app);
                }
                Err(error) => {
                    app.connected = false;
                    app.status = reconnect_status(app, &error.to_string());
                }
            }
        }
    }
}

fn percent_encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

fn handle_key(app: &mut App, key: KeyEvent) -> UserAction {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        if app.quit_armed {
            return UserAction::Exit;
        }
        app.quit_armed = true;
        if app.busy {
            app.status = tr(
                app,
                "已请求停止；再次按 Ctrl+C 退出",
                "Stop requested; press Ctrl+C again to exit",
            )
            .to_string();
            return if app.active_group.is_some() {
                UserAction::Stop(
                    json!({
                        "type": "group_stop",
                        "targets": &app.group_targets,
                    })
                    .to_string(),
                )
            } else {
                UserAction::Stop("/stop".to_string())
            };
        }
        app.status = tr(app, "再次按 Ctrl+C 退出", "Press Ctrl+C again to exit").to_string();
        return UserAction::None;
    }
    if app.confirm_stale_plan {
        return handle_stale_plan_confirmation_key(app, key);
    }
    if app.pending_confirmation.is_some() {
        return handle_confirmation_key(app, key);
    }
    // "Press again" is a consecutive-key contract, not a sticky mode that
    // should unexpectedly terminate the TUI minutes after the first Ctrl+C.
    app.quit_armed = false;
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('p') {
        app.show_commands = !app.show_commands;
        app.show_help = false;
        return UserAction::None;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
        if app.settings_edit.take().is_some() {
            app.input.clear();
        }
        return UserAction::EditConfig;
    }
    if key.code == KeyCode::Char('?') && app.input.is_empty() {
        app.show_help = !app.show_help;
        app.show_commands = false;
        return UserAction::None;
    }
    if key.code == KeyCode::Esc {
        if app.show_help || app.show_commands {
            app.show_help = false;
            app.show_commands = false;
        } else if app.settings_edit.take().is_some() {
            app.input.clear();
            app.status = tr(app, "设置编辑已取消", "Settings edit cancelled").to_string();
        } else if app.plan_feedback_mode {
            app.plan_feedback_mode = false;
            app.status = tr(app, "计划修订已取消", "Plan revision cancelled").to_string();
        } else {
            app.focus = Focus::Composer;
        }
        return UserAction::None;
    }
    if app.show_commands {
        return handle_command_palette(app, key);
    }
    match key.code {
        KeyCode::BackTab => cycle_focus(app, true),
        KeyCode::Tab => cycle_focus(app, key.modifiers.contains(KeyModifiers::SHIFT)),
        KeyCode::F(number) if number >= 1 && (number as usize) <= app.pages.len() => {
            if app.settings_edit.take().is_some() {
                app.input.clear();
            }
            app.page_index = number as usize - 1;
            return UserAction::Load(app.page());
        }
        KeyCode::Char('o') if app.focus == Focus::Content && app.last_image_url.is_some() => {
            return UserAction::OpenImage;
        }
        _ => match app.focus {
            Focus::Navigation => return handle_navigation_key(app, key),
            Focus::Content => return handle_content_key(app, key),
            Focus::Composer => return handle_composer_key(app, key),
        },
    }
    UserAction::None
}

fn handle_confirmation_key(app: &mut App, key: KeyEvent) -> UserAction {
    match key.code {
        KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
            let blocked = app
                .pending_confirmation
                .as_ref()
                .is_some_and(|confirmation| confirmation.action.requires_storage_write())
                && block_storage_write(app);
            if blocked {
                return UserAction::None;
            }
            let Some(confirmation) = app.pending_confirmation.take() else {
                return UserAction::None;
            };
            app.status = tr(app, "正在执行已确认的操作…", "Applying confirmed action…").to_string();
            UserAction::Manage(confirmation.action)
        }
        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
            if let Some(confirmation) = app.pending_confirmation.take() {
                app.input = confirmation.draft;
            }
            app.status = tr(app, "操作已取消", "Action cancelled").to_string();
            UserAction::None
        }
        _ => UserAction::None,
    }
}

fn handle_stale_plan_confirmation_key(app: &mut App, key: KeyEvent) -> UserAction {
    match key.code {
        KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
            if block_storage_write(app) {
                return UserAction::None;
            }
            app.confirm_stale_plan = false;
            execute_stale_plan(app)
        }
        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
            app.confirm_stale_plan = false;
            app.status = tr(
                app,
                "已取消按旧证据执行；可刷新计划后再执行",
                "Stale execution cancelled; refresh the plan before executing",
            )
            .to_string();
            UserAction::None
        }
        _ => UserAction::None,
    }
}

fn cycle_focus(app: &mut App, backwards: bool) {
    app.focus = if backwards {
        match app.focus {
            Focus::Navigation => Focus::Composer,
            Focus::Content => Focus::Navigation,
            Focus::Composer => Focus::Content,
        }
    } else {
        match app.focus {
            Focus::Navigation => Focus::Content,
            Focus::Content => Focus::Composer,
            Focus::Composer => Focus::Navigation,
        }
    };
}

fn handle_navigation_key(app: &mut App, key: KeyEvent) -> UserAction {
    match key.code {
        KeyCode::Up => app.nav_index = app.nav_index.saturating_sub(1),
        KeyCode::Down => {
            app.nav_index = (app.nav_index + 1).min(app.target_count().saturating_sub(1))
        }
        KeyCode::Enter => {
            if block_image_upload_interaction(app) {
                return UserAction::None;
            }
            if app.nav_index < app.sessions.len() {
                return UserAction::SwitchSession(app.sessions[app.nav_index].id.clone());
            }
            let group_index = app.nav_index.saturating_sub(app.sessions.len());
            if let Some(group) = app.groups.get(group_index) {
                return UserAction::SwitchGroup(group.id.clone());
            }
        }
        _ => {}
    }
    UserAction::None
}

fn interactive_page(page: Page) -> bool {
    matches!(
        page,
        Page::Models | Page::Skills | Page::Mcp | Page::Todos | Page::Settings
    )
}

fn move_interactive_selection(app: &mut App, delta: isize) -> bool {
    let page = app.page();
    if !interactive_page(page) {
        return false;
    }
    let count = app
        .inspector_payload
        .as_ref()
        .map(|payload| interactive_row_count(app, page, payload))
        .unwrap_or(0);
    if count == 0 {
        return false;
    }
    app.inspector_index = if delta < 0 {
        app.inspector_index.saturating_sub(delta.unsigned_abs())
    } else {
        (app.inspector_index + delta as usize).min(count.saturating_sub(1))
    };
    if page == Page::Models {
        reset_model_effort_choice(app);
    }
    refresh_interactive_inspector(app);
    true
}

fn cycle_model_effort(app: &mut App, delta: isize) -> bool {
    if app.page() != Page::Models {
        return false;
    }
    let count = app
        .inspector_payload
        .as_ref()
        .and_then(|payload| payload.get("models"))
        .and_then(Value::as_array)
        .and_then(|models| models.get(app.inspector_index))
        .and_then(|model| model.get("efforts"))
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    if count == 0 {
        return false;
    }
    app.inspector_choice = if delta < 0 {
        app.inspector_choice.saturating_sub(delta.unsigned_abs())
    } else {
        (app.inspector_choice + delta as usize).min(count.saturating_sub(1))
    };
    refresh_interactive_inspector(app);
    true
}

fn selected_model_mutation(app: &App) -> Result<PageMutation, String> {
    let payload = app.inspector_payload.as_ref().ok_or_else(|| {
        tr(app, "模型目录尚未加载", "The model catalog is not loaded").to_string()
    })?;
    let model = payload
        .get("models")
        .and_then(Value::as_array)
        .and_then(|models| models.get(app.inspector_index))
        .ok_or_else(|| tr(app, "请选择一个模型", "Select a model").to_string())?;
    let model_ref = model
        .get("ref")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| tr(app, "模型引用无效", "The model reference is invalid").to_string())?;
    let effort = model
        .get("efforts")
        .and_then(Value::as_array)
        .and_then(|efforts| efforts.get(app.inspector_choice))
        .and_then(Value::as_str)
        .or_else(|| model.get("defaultEffort").and_then(Value::as_str))
        .unwrap_or("off");
    Ok(PageMutation::Model {
        model: model_ref.to_string(),
        effort: effort.to_string(),
    })
}

fn selected_settings_row(app: &App) -> Result<SettingsRow, String> {
    let payload = app
        .inspector_payload
        .as_ref()
        .ok_or_else(|| tr(app, "设置尚未加载", "Settings are not loaded").to_string())?;
    settings_rows(app, payload)
        .get(app.inspector_index)
        .cloned()
        .ok_or_else(|| tr(app, "请选择一个设置项", "Select a setting").to_string())
}

fn is_group_feature_setting(row: &SettingsRow) -> bool {
    row.path == ["settings".to_string(), "enableGroups".to_string()]
}

fn toggle_settings_row(row: SettingsRow) -> UserAction {
    if is_group_feature_setting(&row) {
        return UserAction::ToggleGroups;
    }
    let next = !row.value.as_ref().and_then(Value::as_bool).unwrap_or(false);
    UserAction::MutatePage(PageMutation::Config(ConfigMutation {
        path: row.path,
        value: Some(json!(next)),
    }))
}

fn begin_settings_edit(app: &mut App) -> UserAction {
    let row = match selected_settings_row(app) {
        Ok(row) => row,
        Err(error) => {
            app.status = error;
            return UserAction::None;
        }
    };
    if matches!(row.kind, SettingsValueKind::Toggle) {
        return toggle_settings_row(row);
    }
    app.input = match &row.kind {
        SettingsValueKind::Unsigned { .. } => row
            .value
            .as_ref()
            .and_then(Value::as_u64)
            .map(|value| value.to_string())
            .unwrap_or_default(),
        SettingsValueKind::Text { secret: true, .. } => String::new(),
        SettingsValueKind::Text { .. } => row
            .value
            .as_ref()
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        SettingsValueKind::Toggle => String::new(),
    };
    app.status = match app.language {
        UiLanguage::ZhCn => format!("编辑 {}；Enter 保存，Esc 取消", row.label),
        UiLanguage::En => format!("Editing {}; Enter saves and Esc cancels", row.label),
    };
    app.settings_edit = Some(SettingsEdit { row });
    app.focus = Focus::Composer;
    UserAction::None
}

fn parse_settings_edit_mutation(
    app: &App,
    edit: &SettingsEdit,
    input: &str,
) -> Result<PageMutation, String> {
    let input = input.trim();
    let value = match edit.row.kind {
        SettingsValueKind::Toggle => {
            return Err(tr(
                app,
                "布尔设置请使用 Space 切换",
                "Use Space to toggle this setting",
            )
            .to_string());
        }
        SettingsValueKind::Unsigned { optional } => {
            if input.is_empty() && optional {
                None
            } else {
                Some(json!(input.parse::<u64>().map_err(|_| {
                    tr(app, "请输入非负整数", "Enter a non-negative integer").to_string()
                })?))
            }
        }
        SettingsValueKind::Text { optional, .. } => {
            if input.is_empty() {
                if optional {
                    None
                } else {
                    return Err(
                        tr(app, "该设置不能为空", "This setting cannot be empty").to_string()
                    );
                }
            } else {
                Some(json!(input))
            }
        }
    };
    Ok(PageMutation::Config(ConfigMutation {
        path: edit.row.path.clone(),
        value,
    }))
}

fn selected_skill_mutation(app: &App) -> Result<PageMutation, String> {
    let skills = app
        .inspector_payload
        .as_ref()
        .and_then(|payload| payload.get("skills"))
        .and_then(Value::as_array)
        .ok_or_else(|| tr(app, "Skill 列表尚未加载", "The Skill list is not loaded").to_string())?;
    let selected = skills
        .get(app.inspector_index)
        .ok_or_else(|| tr(app, "请选择一个 Skill", "Select a Skill").to_string())?;
    let selected_id = selected
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| tr(app, "Skill ID 无效", "The Skill id is invalid").to_string())?;
    let selected_enabled = selected
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let known_system_skills = skills
        .iter()
        .filter_map(|skill| skill.get("id").and_then(Value::as_str))
        .map(str::to_string)
        .collect::<Vec<_>>();
    let enabled_system_skills = skills
        .iter()
        .filter_map(|skill| {
            let id = skill.get("id").and_then(Value::as_str)?;
            let enabled = skill
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            ((id == selected_id && !selected_enabled) || (id != selected_id && enabled))
                .then(|| id.to_string())
        })
        .collect::<Vec<_>>();
    Ok(PageMutation::Skills {
        enabled_system_skills,
        known_system_skills,
    })
}

fn selected_mcp_mutation(app: &App) -> Result<PageMutation, String> {
    let payload = app
        .inspector_payload
        .as_ref()
        .ok_or_else(|| tr(app, "MCP 目录尚未加载", "The MCP catalog is not loaded").to_string())?;
    let servers = payload
        .get("servers")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let tools = payload
        .get("tools")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let policy = payload.get("policy").cloned().unwrap_or_else(|| json!({}));
    let mut enabled_servers = policy
        .get("enabledServers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect::<HashSet<_>>();
    let mut enabled_tools = policy
        .get("enabledTools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect::<HashSet<_>>();

    if app.inspector_index < servers.len() {
        let server = &servers[app.inspector_index];
        let id = server.get("id").and_then(Value::as_str).ok_or_else(|| {
            tr(app, "MCP 服务 ID 无效", "The MCP server id is invalid").to_string()
        })?;
        if enabled_servers.contains(id) {
            enabled_servers.remove(id);
            enabled_tools.retain(|tool_id| {
                !tools.iter().any(|tool| {
                    tool.get("id").and_then(Value::as_str) == Some(tool_id.as_str())
                        && tool.get("server").and_then(Value::as_str) == Some(id)
                })
            });
        } else {
            if !server
                .get("configuredEnabled")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                return Err(tr(
                    app,
                    "该 MCP 服务已在配置中停用",
                    "This MCP server is disabled in configuration",
                )
                .to_string());
            }
            enabled_servers.insert(id.to_string());
        }
    } else {
        let tool_index = app.inspector_index.saturating_sub(servers.len());
        let tool = tools
            .get(tool_index)
            .ok_or_else(|| tr(app, "请选择一个 MCP 项目", "Select an MCP item").to_string())?;
        let id = tool
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| tr(app, "MCP Tool ID 无效", "The MCP tool id is invalid").to_string())?;
        let server = tool.get("server").and_then(Value::as_str).ok_or_else(|| {
            tr(app, "MCP Tool 服务无效", "The MCP tool server is invalid").to_string()
        })?;
        if enabled_tools.contains(id) {
            enabled_tools.remove(id);
        } else {
            let configured = servers.iter().any(|entry| {
                entry.get("id").and_then(Value::as_str) == Some(server)
                    && entry
                        .get("configuredEnabled")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
            });
            if !configured {
                return Err(tr(
                    app,
                    "该 Tool 所属 MCP 服务已停用",
                    "The MCP server for this tool is disabled",
                )
                .to_string());
            }
            enabled_servers.insert(server.to_string());
            enabled_tools.insert(id.to_string());
        }
    }
    let mut enabled_servers = enabled_servers.into_iter().collect::<Vec<_>>();
    let mut enabled_tools = enabled_tools.into_iter().collect::<Vec<_>>();
    enabled_servers.sort();
    enabled_tools.sort();
    Ok(PageMutation::McpPolicy(json!({
        "enabledServers": enabled_servers,
        "enabledTools": enabled_tools,
        "confirmMutatingTools": policy
            .get("confirmMutatingTools")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        "clientCapabilities": policy
            .get("clientCapabilities")
            .cloned()
            .unwrap_or_else(|| json!({"roots": false, "sampling": false, "elicitation": false})),
    })))
}

fn todos_payload(app: &App) -> Result<Value, String> {
    app.inspector_payload
        .as_ref()
        .filter(|_| app.page() == Page::Todos)
        .cloned()
        .or_else(|| serde_json::from_str(&app.todos_snapshot).ok())
        .ok_or_else(|| tr(app, "Todo 状态尚未加载", "Todo state is not loaded").to_string())
}

fn selected_todo_mutation(app: &App, remove: bool) -> Result<PageMutation, String> {
    let payload = todos_payload(app)?;
    let base_revision = payload.get("revision").and_then(Value::as_u64).unwrap_or(0);
    let mut items = payload
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if app.inspector_index >= items.len() {
        return Err(tr(app, "请选择一个 Todo", "Select a todo").to_string());
    }
    if remove {
        items.remove(app.inspector_index);
    } else if let Some(item) = items
        .get_mut(app.inspector_index)
        .and_then(Value::as_object_mut)
    {
        let next = match item
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("pending")
        {
            "pending" => "in_progress",
            "in_progress" => "completed",
            _ => "pending",
        };
        item.insert("status".to_string(), json!(next));
    }
    Ok(PageMutation::Todos {
        base_revision,
        items,
    })
}

fn add_todo_mutation(app: &App, content: &str) -> Result<PageMutation, String> {
    let content = content.trim();
    if content.is_empty() {
        return Err(tr(app, "Todo 内容不能为空", "Todo content cannot be empty").to_string());
    }
    let payload = todos_payload(app)?;
    let base_revision = payload.get("revision").and_then(Value::as_u64).unwrap_or(0);
    let mut items = payload
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    items.push(json!({
        "id": format!("tui-{nonce:x}"),
        "content": content,
        "status": "pending",
    }));
    Ok(PageMutation::Todos {
        base_revision,
        items,
    })
}

fn handle_content_key(app: &mut App, key: KeyEvent) -> UserAction {
    if matches!(key.code, KeyCode::Up | KeyCode::PageUp) && move_interactive_selection(app, -1) {
        return UserAction::None;
    }
    if matches!(key.code, KeyCode::Down | KeyCode::PageDown) && move_interactive_selection(app, 1) {
        return UserAction::None;
    }
    if key.code == KeyCode::Left && cycle_model_effort(app, -1) {
        return UserAction::None;
    }
    if key.code == KeyCode::Right && cycle_model_effort(app, 1) {
        return UserAction::None;
    }
    match key.code {
        KeyCode::Up | KeyCode::PageUp => {
            let scroll = if app.page() == Page::Chat {
                &mut app.scroll
            } else {
                &mut app.inspector_scroll
            };
            *scroll = scroll.saturating_sub(4);
        }
        KeyCode::Down | KeyCode::PageDown => {
            let scroll = if app.page() == Page::Chat {
                &mut app.scroll
            } else {
                &mut app.inspector_scroll
            };
            *scroll = scroll.saturating_add(4);
        }
        KeyCode::Home => {
            if app.page() == Page::Chat {
                app.scroll = 0;
            } else {
                app.inspector_scroll = 0;
            }
        }
        KeyCode::End => {
            if app.page() == Page::Chat {
                app.scroll = u16::MAX;
            } else {
                app.inspector_scroll = u16::MAX;
            }
        }
        KeyCode::Enter | KeyCode::Char('e') if app.page() == Page::Plan => {
            return plan_action(app, "execute", false);
        }
        KeyCode::Char('r') if app.page() == Page::Plan => {
            return plan_action(app, "resume", false);
        }
        KeyCode::Char('f') if app.page() == Page::Plan => {
            return plan_action(app, "refresh", false);
        }
        KeyCode::Char('d') if app.page() == Page::Plan => {
            return plan_action(app, "discard", false);
        }
        KeyCode::Char('v') if app.page() == Page::Plan => {
            let can_receive_feedback = app
                .active_plan
                .as_ref()
                .is_some_and(|plan| plan_can_receive_feedback(&plan.status));
            if !can_receive_feedback {
                app.status = tr(
                    app,
                    "当前计划状态不能修订",
                    "The current plan state cannot be revised",
                )
                .to_string();
                return UserAction::None;
            }
            if block_storage_write(app) {
                return UserAction::None;
            }
            app.plan_feedback_mode = true;
            app.focus = Focus::Composer;
            app.status = tr(
                app,
                "输入计划修订要求并按 Enter 提交，Esc 取消",
                "Enter plan revision feedback, press Enter to submit, or Esc to cancel",
            )
            .to_string();
        }
        KeyCode::Char('x') if app.page() == Page::Plan => {
            if app.plan_stale.is_none() {
                app.status = tr(
                    app,
                    "当前计划没有待确认的过期证据",
                    "This plan has no stale evidence awaiting confirmation",
                )
                .to_string();
                return UserAction::None;
            }
            if block_storage_write(app) {
                return UserAction::None;
            }
            app.confirm_stale_plan = true;
            app.status = tr(
                app,
                "确认仍按旧证据执行",
                "Confirm execution with stale evidence",
            )
            .to_string();
        }
        KeyCode::Char('r')
            if matches!(
                app.page(),
                Page::Todos
                    | Page::Models
                    | Page::Skills
                    | Page::Mcp
                    | Page::Usage
                    | Page::Settings
            ) =>
        {
            return UserAction::Load(app.page());
        }
        KeyCode::Char('g') if app.page() == Page::Settings => {
            return UserAction::ToggleGroups;
        }
        KeyCode::Char(' ') if app.page() == Page::Settings => match selected_settings_row(app) {
            Ok(row) if matches!(row.kind, SettingsValueKind::Toggle) => {
                return toggle_settings_row(row);
            }
            Ok(_) => {
                app.status = tr(
                    app,
                    "按 Enter 编辑当前设置",
                    "Press Enter to edit the selected setting",
                )
                .to_string();
            }
            Err(error) => app.status = error,
        },
        KeyCode::Enter if app.page() == Page::Settings => {
            return begin_settings_edit(app);
        }
        KeyCode::Enter if app.page() == Page::Models => {
            if app.busy {
                app.status = tr(
                    app,
                    "Session 运行期间不能切换模型",
                    "The model cannot be changed while the Session is running",
                )
                .to_string();
            } else if !block_storage_write(app) {
                match selected_model_mutation(app) {
                    Ok(mutation) => return UserAction::MutatePage(mutation),
                    Err(error) => app.status = error,
                }
            }
        }
        KeyCode::Char(' ') if app.page() == Page::Skills => {
            if !block_storage_write(app) {
                match selected_skill_mutation(app) {
                    Ok(mutation) => return UserAction::MutatePage(mutation),
                    Err(error) => app.status = error,
                }
            }
        }
        KeyCode::Char(' ') if app.page() == Page::Mcp => {
            if !block_storage_write(app) {
                match selected_mcp_mutation(app) {
                    Ok(mutation) => return UserAction::MutatePage(mutation),
                    Err(error) => app.status = error,
                }
            }
        }
        KeyCode::Char(' ') if app.page() == Page::Todos => {
            if !block_storage_write(app) {
                match selected_todo_mutation(app, false) {
                    Ok(mutation) => return UserAction::MutatePage(mutation),
                    Err(error) => app.status = error,
                }
            }
        }
        KeyCode::Delete if app.page() == Page::Todos => {
            if !block_storage_write(app) {
                match selected_todo_mutation(app, true) {
                    Ok(mutation) => return UserAction::MutatePage(mutation),
                    Err(error) => app.status = error,
                }
            }
        }
        KeyCode::Char('a') if app.page() == Page::Todos => {
            app.input = "/todo add ".to_string();
            app.focus = Focus::Composer;
            app.status = tr(
                app,
                "输入 Todo 内容并按 Enter",
                "Enter the todo content and press Enter",
            )
            .to_string();
        }
        _ => {}
    }
    UserAction::None
}

fn trim_command_value(value: &str) -> String {
    value
        .trim()
        .trim_matches(|character| matches!(character, '"' | '\''))
        .trim()
        .to_string()
}

fn parse_workspace_selection(app: &App, value: &str) -> Result<WorkspaceSelection, String> {
    let value = trim_command_value(value);
    if value.eq_ignore_ascii_case("managed") {
        return Ok(WorkspaceSelection::Managed);
    }
    if value.is_empty() {
        return Err(tr(
            app,
            "工作空间必须为 managed 或绝对目录路径",
            "Workspace must be 'managed' or an absolute directory path",
        )
        .to_string());
    }
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err(tr(
            app,
            "工作目录必须使用绝对路径",
            "Workspace directory must be an absolute path",
        )
        .to_string());
    }
    Ok(WorkspaceSelection::Directory(path))
}

fn parse_member_ids(value: &str) -> Vec<String> {
    value
        .split([',', ' '])
        .map(trim_command_value)
        .filter(|value| !value.is_empty())
        .fold(Vec::new(), |mut members, value| {
            if !members.iter().any(|member| member == &value) {
                members.push(value);
            }
            members
        })
}

fn active_group_for_management(app: &App) -> Result<(), String> {
    if !app.groups_enabled {
        return Err(tr(
            app,
            "群聊已由配置关闭",
            "Group chat is disabled by configuration",
        )
        .to_string());
    }
    if app.active_group.is_none() {
        return Err(tr(
            app,
            "使用此命令前请先打开一个群聊",
            "Open a Group before using this command",
        )
        .to_string());
    }
    Ok(())
}

fn management_confirmation_prompt(app: &App, action: &ManagementAction) -> Option<String> {
    match action {
        ManagementAction::DeleteSession(requested) => {
            let label = find_session_summary(app, requested)
                .map(|session| format!("{} ({})", session.name, session.id))
                .unwrap_or_else(|_| requested.clone());
            Some(format!(
                "{} {label}?",
                tr(app, "确认删除 Session", "Delete Session")
            ))
        }
        ManagementAction::DeleteGroup => {
            let group_id = app.active_group.as_deref().unwrap_or_default();
            let label = app
                .groups
                .iter()
                .find(|group| group.id == group_id)
                .map(|group| format!("{} ({})", group.name, group.id))
                .unwrap_or_else(|| group_id.to_string());
            Some(format!(
                "{} {label}?",
                tr(app, "确认删除群聊", "Delete Group")
            ))
        }
        ManagementAction::RemoveGroupMember(session_id) => Some(format!(
            "{} {session_id}?",
            tr(app, "确认移除群成员", "Remove Group member")
        )),
        _ => None,
    }
}

fn parse_management_command(app: &App, input: &str) -> Result<Option<ManagementAction>, String> {
    let input = input.trim();
    if let Some(command) = input.strip_prefix("/session") {
        let command = command.trim_start();
        if command.is_empty() {
            return Err(tr(
                app,
                "用法：/session create [managed|PATH] | NAME; rename NAME; rebind managed|PATH; switch ID; find TEXT; delete ID",
                "Usage: /session create [managed|PATH] | NAME; rename NAME; rebind managed|PATH; switch ID; find TEXT; delete ID",
            ).to_string());
        }
        let (action, rest) = command
            .split_once(char::is_whitespace)
            .map_or((command, ""), |(action, rest)| (action, rest.trim()));
        return match action {
            "create" | "new" => {
                let (workspace, name) = if let Some((workspace, name)) = rest.split_once('|') {
                    (
                        parse_workspace_selection(app, workspace)?,
                        trim_command_value(name),
                    )
                } else {
                    (WorkspaceSelection::Managed, trim_command_value(rest))
                };
                if name.is_empty() {
                    Err(tr(app, "Session 名称不能为空", "Session name cannot be empty").to_string())
                } else {
                    Ok(Some(ManagementAction::CreateSession { name, workspace }))
                }
            }
            "rename" => {
                let name = trim_command_value(rest);
                if name.is_empty() {
                    Err(tr(app, "Session 名称不能为空", "Session name cannot be empty").to_string())
                } else {
                    Ok(Some(ManagementAction::RenameSession(name)))
                }
            }
            "rebind" | "workspace" => Ok(Some(ManagementAction::RebindSession(
                parse_workspace_selection(app, rest)?,
            ))),
            "switch" | "open" => {
                let id = trim_command_value(rest);
                if id.is_empty() {
                    Err(tr(app, "Session ID 不能为空", "Session id cannot be empty").to_string())
                } else {
                    Ok(Some(ManagementAction::SwitchSession(id)))
                }
            }
            "find" | "search" => {
                let query = trim_command_value(rest);
                if query.is_empty() {
                    Err(tr(
                        app,
                        "Session 搜索内容不能为空",
                        "Session search cannot be empty",
                    )
                    .to_string())
                } else {
                    Ok(Some(ManagementAction::FindSession(query)))
                }
            }
            "delete" | "remove" => {
                let id = trim_command_value(rest);
                if id.is_empty() {
                    Err(tr(app, "Session ID 不能为空", "Session id cannot be empty").to_string())
                } else {
                    Ok(Some(ManagementAction::DeleteSession(id)))
                }
            }
            _ => Err(match app.language {
                UiLanguage::ZhCn => format!("未知的 Session 命令：{action}"),
                UiLanguage::En => format!("Unknown Session command: {action}"),
            }),
        };
    }

    if let Some(command) = input.strip_prefix("/group") {
        if !app.groups_enabled {
            return Err(tr(
                app,
                "群聊已由配置关闭",
                "Group chat is disabled by configuration",
            )
            .to_string());
        }
        let command = command.trim_start();
        if command.is_empty() {
            return Err(tr(
                app,
                "用法：/group create NAME | MEMBER,...; rename NAME; members ID,...; switch ID; promote ID; remove ID; delete",
                "Usage: /group create NAME | MEMBER,...; rename NAME; members ID,...; switch ID; promote ID; remove ID; delete",
            ).to_string());
        }
        let (action, rest) = command
            .split_once(char::is_whitespace)
            .map_or((command, ""), |(action, rest)| (action, rest.trim()));
        return match action {
            "create" | "new" => {
                let Some((name, members)) = rest.split_once('|') else {
                    return Err(tr(
                        app,
                        "用法：/group create NAME | MEMBER,...",
                        "Usage: /group create NAME | MEMBER,...",
                    )
                    .to_string());
                };
                let name = trim_command_value(name);
                let members = parse_member_ids(members);
                if name.is_empty() {
                    Err(tr(app, "群聊名称不能为空", "Group name cannot be empty").to_string())
                } else if members.is_empty() {
                    Err(tr(
                        app,
                        "请至少选择一名群成员",
                        "Select at least one Group member",
                    )
                    .to_string())
                } else {
                    Ok(Some(ManagementAction::CreateGroup { name, members }))
                }
            }
            "rename" => {
                active_group_for_management(app)?;
                let name = trim_command_value(rest);
                if name.is_empty() {
                    Err(tr(app, "群聊名称不能为空", "Group name cannot be empty").to_string())
                } else {
                    Ok(Some(ManagementAction::RenameGroup(name)))
                }
            }
            "members" => {
                active_group_for_management(app)?;
                let members = parse_member_ids(rest);
                if members.is_empty() {
                    Err(tr(
                        app,
                        "请至少选择一名群成员",
                        "Select at least one Group member",
                    )
                    .to_string())
                } else {
                    Ok(Some(ManagementAction::ReplaceGroupMembers(members)))
                }
            }
            "switch" | "open" => {
                let id = trim_command_value(rest);
                if id.is_empty() {
                    Err(tr(app, "群聊 ID 不能为空", "Group id cannot be empty").to_string())
                } else {
                    Ok(Some(ManagementAction::SwitchGroup(id)))
                }
            }
            "delete" => {
                active_group_for_management(app)?;
                Ok(Some(ManagementAction::DeleteGroup))
            }
            "promote" => {
                active_group_for_management(app)?;
                let id = trim_command_value(rest);
                if id.is_empty() {
                    Err(tr(
                        app,
                        "成员 Session ID 不能为空",
                        "Member Session id cannot be empty",
                    )
                    .to_string())
                } else {
                    Ok(Some(ManagementAction::PromoteGroupMember(id)))
                }
            }
            "remove" => {
                active_group_for_management(app)?;
                let id = trim_command_value(rest);
                if id.is_empty() {
                    Err(tr(
                        app,
                        "成员 Session ID 不能为空",
                        "Member Session id cannot be empty",
                    )
                    .to_string())
                } else {
                    Ok(Some(ManagementAction::RemoveGroupMember(id)))
                }
            }
            _ => Err(match app.language {
                UiLanguage::ZhCn => format!("未知的群聊命令：{action}"),
                UiLanguage::En => format!("Unknown Group command: {action}"),
            }),
        };
    }

    if let Some(command) = input.strip_prefix("/mcp ") {
        let (action, rest) = command
            .trim()
            .split_once(char::is_whitespace)
            .map_or((command.trim(), ""), |(action, rest)| (action, rest.trim()));
        let server = trim_command_value(rest);
        return match action {
            "oauth" | "connect" if !server.is_empty() => {
                Ok(Some(ManagementAction::StartMcpOauth(server)))
            }
            "disconnect" if !server.is_empty() => {
                Ok(Some(ManagementAction::DisconnectMcpOauth(server)))
            }
            "oauth" | "connect" | "disconnect" => Err(tr(
                app,
                "MCP 服务名称不能为空",
                "MCP server name cannot be empty",
            )
            .to_string()),
            // Preserve the existing `/mcp [refresh]` WebSocket command.
            _ => Ok(None),
        };
    }

    Ok(None)
}

fn handle_composer_key(app: &mut App, key: KeyEvent) -> UserAction {
    if app.upload_in_progress {
        block_image_upload_interaction(app);
        return UserAction::None;
    }
    if key.code == KeyCode::Enter
        && !key
            .modifiers
            .intersects(KeyModifiers::ALT | KeyModifiers::CONTROL)
        && let Some(edit) = app.settings_edit.clone()
    {
        match parse_settings_edit_mutation(app, &edit, &app.input) {
            Ok(mutation) => {
                app.settings_edit = None;
                app.input.clear();
                return UserAction::MutatePage(mutation);
            }
            Err(error) => {
                app.status = error;
                return UserAction::None;
            }
        }
    }
    match key.code {
        KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => app.input.push('\n'),
        KeyCode::Enter
            if key
                .modifiers
                .intersects(KeyModifiers::ALT | KeyModifiers::CONTROL) =>
        {
            app.input.push('\n')
        }
        KeyCode::Enter => {
            let input = app.input.trim().to_string();
            if input.is_empty() && app.pending_images.is_empty() {
                return UserAction::None;
            }
            if let Some(path) = input
                .strip_prefix("/attach ")
                .or_else(|| input.strip_prefix("/image "))
            {
                if block_storage_write(app) {
                    return UserAction::None;
                }
                if app.active_group.is_some() {
                    app.status = tr(
                        app,
                        "群聊协议暂不支持图片附件",
                        "Group chat does not currently accept image attachments",
                    )
                    .to_string();
                    return UserAction::None;
                }
                let path = PathBuf::from(path.trim());
                if path.as_os_str().is_empty() {
                    app.status = tr(app, "请输入图片路径", "Enter an image path").to_string();
                    return UserAction::None;
                }
                app.input.clear();
                return UserAction::UploadImage(path);
            }
            if app.active_group.is_some()
                && let Some(targets) = input.strip_prefix("/target ")
            {
                let targets = targets.trim();
                if targets.eq_ignore_ascii_case("all") {
                    app.group_target_mode = "all".to_string();
                    app.group_targets.clear();
                } else if targets.eq_ignore_ascii_case("mentions") {
                    app.group_target_mode = "mentions".to_string();
                    app.group_targets.clear();
                } else {
                    app.group_target_mode = "selected".to_string();
                    app.group_targets = targets
                        .split([',', ' '])
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(str::to_string)
                        .collect();
                    if app.group_targets.is_empty() {
                        app.status = tr(
                            app,
                            "用法：/target all|mentions|session-id[,session-id]",
                            "Usage: /target all|mentions|session-id[,session-id]",
                        )
                        .to_string();
                        return UserAction::None;
                    }
                }
                app.input.clear();
                app.status = format!(
                    "{}: {}{}",
                    tr(app, "群聊目标", "Group target"),
                    app.group_target_mode,
                    if app.group_targets.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", app.group_targets.join(", "))
                    }
                );
                return UserAction::None;
            }
            if let Some(content) = input.strip_prefix("/todo add ") {
                if block_storage_write(app) {
                    return UserAction::None;
                }
                match add_todo_mutation(app, content) {
                    Ok(mutation) => {
                        app.input.clear();
                        return UserAction::MutatePage(mutation);
                    }
                    Err(error) => {
                        app.status = error;
                        return UserAction::None;
                    }
                }
            }
            match parse_management_command(app, &input) {
                Ok(Some(action)) => {
                    if action.requires_storage_write() && block_storage_write(app) {
                        return UserAction::None;
                    }
                    app.input.clear();
                    if let Some(prompt) = management_confirmation_prompt(app, &action) {
                        app.pending_confirmation = Some(PendingConfirmation {
                            action,
                            draft: input,
                            prompt,
                        });
                        app.show_help = false;
                        app.show_commands = false;
                        app.status = tr(
                            app,
                            "Enter/Y 确认 · Esc/N 取消",
                            "Enter/Y confirm · Esc/N cancel",
                        )
                        .to_string();
                        return UserAction::None;
                    }
                    return UserAction::Manage(action);
                }
                Ok(None) => {}
                Err(error) => {
                    app.status = error;
                    return UserAction::None;
                }
            }
            if composer_input_requires_storage_write(&input, !app.pending_images.is_empty())
                && block_storage_write(app)
            {
                return UserAction::None;
            }
            if !app.connected {
                app.status = tr(
                    app,
                    "尚未连接，消息保留在输入框中",
                    "Not connected; the message remains in the composer",
                )
                .to_string();
                return UserAction::None;
            }
            if let Some(plan) = app.active_plan.as_ref() {
                match plan.status.as_str() {
                    status
                        if status == "needs_input"
                            || (app.plan_feedback_mode && plan_can_receive_feedback(status)) =>
                    {
                        let payload = json!({
                            "plan_action": {
                                "action": "feedback",
                                "plan_id": plan.id,
                                "revision": plan.revision,
                                "text": input,
                            }
                        });
                        app.input.clear();
                        app.plan_feedback_mode = false;
                        return UserAction::Send(payload.to_string());
                    }
                    "planning" | "ready" | "executing" => {
                        app.status = tr(
                            app,
                            "请先在计划页处理当前计划",
                            "Resolve the active plan from the Plan page first",
                        )
                        .to_string();
                        return UserAction::None;
                    }
                    _ => {}
                }
            }
            app.input.clear();
            app.push(
                "you",
                if input.is_empty() { "(image)" } else { &input },
                LineKind::User,
            );
            if app.active_group.is_some() {
                if input.starts_with('/') {
                    return UserAction::Send(input);
                }
                return UserAction::Send(
                    json!({
                        "type": "group_message",
                        "text": input,
                        "targets": &app.group_targets,
                        "target_mode": &app.group_target_mode,
                        "start_runs": true,
                        "run_mode": "execute",
                    })
                    .to_string(),
                );
            }
            if input.starts_with('/') && app.pending_images.is_empty() {
                return UserAction::Send(input);
            }
            let images = std::mem::take(&mut app.pending_images);
            let plan_mode = std::mem::take(&mut app.plan_mode);
            return if images.is_empty() {
                UserAction::Send(json!({"text": input, "plan_mode": plan_mode}).to_string())
            } else {
                UserAction::Send(
                    json!({"text": input, "plan_mode": plan_mode, "images": images}).to_string(),
                )
            };
        }
        KeyCode::Backspace => {
            app.input.pop();
        }
        KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::ALT) => {
            if app.active_group.is_some() {
                app.status = tr(
                    app,
                    "群聊暂不支持规划模式",
                    "Plan mode is unavailable in Group chat",
                )
                .to_string();
                return UserAction::None;
            }
            app.plan_mode = !app.plan_mode;
            app.status = if app.plan_mode {
                tr(app, "规划模式已选择", "Plan mode selected")
            } else {
                tr(app, "执行模式", "Execute mode")
            }
            .to_string();
        }
        KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => app.input.push(ch),
        _ => {}
    }
    UserAction::None
}

fn plan_action(app: &mut App, action: &str, allow_stale: bool) -> UserAction {
    if block_image_upload_interaction(app) {
        return UserAction::None;
    }
    if block_storage_write(app) {
        return UserAction::None;
    }
    if !app.connected {
        app.status = tr(
            app,
            "尚未连接，计划操作未发送",
            "Not connected; the plan action was not sent",
        )
        .to_string();
        return UserAction::None;
    }
    let Some(plan) = app.active_plan.as_ref() else {
        app.status = tr(app, "当前没有活动计划", "No active plan").to_string();
        return UserAction::None;
    };
    let payload = json!({
        "plan_action": {
            "action": action,
            "plan_id": plan.id,
            "revision": plan.revision,
            "allow_stale": allow_stale,
        }
    });
    if matches!(action, "execute" | "resume") {
        app.pending_plan_action = Some(action.to_string());
    }
    UserAction::Send(payload.to_string())
}

fn execute_stale_plan(app: &mut App) -> UserAction {
    if block_image_upload_interaction(app) {
        return UserAction::None;
    }
    if !app.connected {
        app.status = tr(
            app,
            "尚未连接，计划操作未发送",
            "Not connected; the plan action was not sent",
        )
        .to_string();
        return UserAction::None;
    }
    let Some(plan) = app.active_plan.as_ref() else {
        app.status = tr(app, "当前没有活动计划", "No active plan").to_string();
        return UserAction::None;
    };
    let Some(stale) = app.plan_stale.as_ref() else {
        app.status = tr(
            app,
            "当前计划没有待确认的过期证据",
            "This plan has no stale evidence awaiting confirmation",
        )
        .to_string();
        return UserAction::None;
    };
    let payload = json!({
        "plan_action": {
            "action": stale.action,
            "plan_id": plan.id,
            "revision": plan.revision,
            "allow_stale": true,
            "stale_confirmation_token": stale.confirmation_token,
        }
    });
    app.pending_plan_action = Some(stale.action.clone());
    UserAction::Send(payload.to_string())
}

fn plan_can_receive_feedback(status: &str) -> bool {
    matches!(status, "needs_input" | "ready" | "failed" | "stopped")
}

fn composer_input_requires_storage_write(input: &str, has_images: bool) -> bool {
    if has_images || !input.starts_with('/') {
        return true;
    }
    let command = input
        .strip_prefix('/')
        .and_then(|value| value.split_whitespace().next())
        .unwrap_or_default()
        .to_ascii_lowercase();
    !matches!(command.as_str(), "help" | "status" | "usage" | "sessions")
}

fn outbound_payload_requires_storage_write(payload: &str) -> bool {
    if payload.trim_start().starts_with('{') {
        return true;
    }
    composer_input_requires_storage_write(payload.trim(), false)
}

fn block_storage_write(app: &mut App) -> bool {
    if app.storage_writable {
        return false;
    }
    app.status = tr(
        app,
        "本地存储处于保护模式；修复后重启 LingClaw。当前操作未执行。",
        "Local storage is protected. Repair it and restart LingClaw; this action was not performed.",
    )
    .to_string();
    true
}

fn block_image_upload_interaction(app: &mut App) -> bool {
    if !app.upload_in_progress {
        return false;
    }
    app.status = tr(
        app,
        "图片正在上传；完成前不能发送消息、执行计划或切换工作区",
        "An image is uploading; sending, Plan execution, and workspace navigation are disabled until it finishes",
    )
    .to_string();
    true
}

fn handle_command_palette(app: &mut App, key: KeyEvent) -> UserAction {
    match key.code {
        KeyCode::Char('1'..='9') => {
            let index = match key.code {
                KeyCode::Char(ch) => ch.to_digit(10).unwrap_or(1) as usize - 1,
                _ => 0,
            };
            if index < app.pages.len() {
                if app.settings_edit.take().is_some() {
                    app.input.clear();
                }
                app.page_index = index;
                app.show_commands = false;
                return UserAction::Load(app.page());
            }
        }
        KeyCode::Char('p') => {
            if app.active_group.is_some() {
                app.status = tr(
                    app,
                    "群聊暂不支持规划模式",
                    "Plan mode is unavailable in Group chat",
                )
                .to_string();
            } else {
                app.plan_mode = !app.plan_mode;
            }
            app.show_commands = false;
        }
        KeyCode::Char('o') if app.last_image_url.is_some() => {
            app.show_commands = false;
            return UserAction::OpenImage;
        }
        KeyCode::Char('e') => {
            app.show_commands = false;
            return UserAction::EditConfig;
        }
        _ => {}
    }
    UserAction::None
}

fn plan_snapshot_from_value(plan: &Value) -> Option<PlanSnapshot> {
    let id = plan
        .get("plan_id")
        .or_else(|| plan.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())?
        .to_string();
    let revision = plan.get("revision").and_then(Value::as_u64).unwrap_or(1);
    let status = plan
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("planning")
        .to_string();
    let title = plan
        .pointer("/artifact/title")
        .or_else(|| plan.get("title"))
        .and_then(Value::as_str)
        .unwrap_or("Plan")
        .to_string();
    Some(PlanSnapshot {
        id,
        revision,
        status,
        title,
        raw: plan.clone(),
    })
}

fn refresh_plan_inspector(app: &mut App) {
    if app.page() != Page::Plan {
        return;
    }
    app.inspector = app
        .active_plan
        .as_ref()
        .map(|plan| serde_json::to_string_pretty(&plan.raw).unwrap_or_default())
        .unwrap_or_else(|| tr(app, "当前没有活动计划", "No active plan").to_string());
}

fn restore_plan_history(app: &mut App, value: &Value) {
    app.active_plan = None;
    app.plan_feedback_mode = false;
    app.plan_stale = None;
    app.pending_plan_action = None;
    app.confirm_stale_plan = false;
    if let Some(plans) = value.get("plans").and_then(Value::as_array) {
        let snapshots = plans
            .iter()
            .filter_map(plan_snapshot_from_value)
            .collect::<Vec<_>>();
        // Match the WebUI's recovery semantics: the last revision explicitly
        // marked as current wins; legacy histories without that marker fall back
        // to their final valid revision.
        app.active_plan = snapshots
            .iter()
            .rev()
            .find(|plan| plan.raw.get("historical").and_then(Value::as_bool) != Some(true))
            .or_else(|| snapshots.last())
            .cloned();
    }
    refresh_plan_inspector(app);
}

fn reconcile_pending_outbound_history(app: &mut App, messages: &[Value], history: &Value) {
    if !app.outbound_reconnect_pending {
        return;
    }
    app.outbound_reconnect_pending = false;
    let Some(snapshot) = app.pending_outbound_write.take() else {
        return;
    };
    if snapshot.was_replayed(messages, history) {
        return;
    }
    snapshot.restore_composer(app);
    app.status = tr(
        app,
        "连接中断前的消息未保存，已恢复到输入框",
        "The message was not saved before the connection closed and was restored to the composer",
    )
    .to_string();
}

fn replace_history_user_messages(app: &mut App, messages: &[Value]) {
    app.history_user_messages = messages
        .iter()
        .filter_map(HistoricalUserMessage::from_value)
        .collect();
    app.history_baseline_available = true;
}

fn settle_pending_outbound(app: &mut App) {
    if let Some(snapshot) = app.pending_outbound_write.take()
        && app.history_baseline_available
        && let Some(fingerprint) = snapshot.user_message
    {
        // A start/plan/group acknowledgement proves that the user message was
        // accepted. Keep a keyless raw-history baseline until the next replay
        // replaces it with the daemon's stable message id/index.
        app.history_user_messages.push(HistoricalUserMessage {
            key: None,
            fingerprint,
        });
    }
    app.outbound_reconnect_pending = false;
}

fn restore_pending_outbound(app: &mut App) {
    if let Some(snapshot) = app.pending_outbound_write.take() {
        snapshot.restore_after_failed_send(app);
    }
    app.outbound_reconnect_pending = false;
}

fn apply_socket_event(app: &mut App, value: Value) -> SocketEventAction {
    let mut action = SocketEventAction::None;
    let event_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    match event_type {
        "session" => {
            if let Some(name) = value.get("name").and_then(Value::as_str) {
                app.session.name = name.to_string();
                if let Some(summary) = app
                    .sessions
                    .iter_mut()
                    .find(|summary| crate::session_ids_match(&summary.id, &app.session.id))
                {
                    summary.name = name.to_string();
                }
            }
            update_session_model_status(app, &value);
            app.connected = true;
        }
        "session_model_configuration" => {
            let event_session = value.get("id").and_then(Value::as_str).unwrap_or_default();
            if event_session.is_empty() || crate::session_ids_match(event_session, &app.session.id)
            {
                if app
                    .pending_outbound_write
                    .as_ref()
                    .is_some_and(ComposerSnapshot::is_slash_command)
                {
                    settle_pending_outbound(app);
                }
                update_session_model_status(app, &value);
            }
        }
        "session_list" => {
            let Some(items) = value.get("sessions").cloned() else {
                app.status = tr(
                    app,
                    "Session 列表事件缺少 sessions 字段",
                    "Session list event is missing the sessions field",
                )
                .to_string();
                return SocketEventAction::None;
            };
            match serde_json::from_value::<Vec<SessionSummary>>(items) {
                Ok(sessions) => {
                    if let Some(current) = sessions
                        .iter()
                        .find(|summary| crate::session_ids_match(&summary.id, &app.session.id))
                    {
                        app.session = current.clone();
                    }
                    app.sessions = sessions;
                    app.nav_index = app.nav_index.min(app.target_count().saturating_sub(1));
                }
                Err(error) => {
                    app.status = format!(
                        "{}: {error}",
                        tr(app, "无法解析 Session 列表", "Could not parse Session list")
                    );
                }
            }
        }
        "session_group_list" => {
            if !app.groups_enabled {
                return SocketEventAction::None;
            }
            let Some(items) = value.get("groups").cloned() else {
                app.status = tr(
                    app,
                    "群聊列表事件缺少 groups 字段",
                    "Group list event is missing the groups field",
                )
                .to_string();
                return SocketEventAction::None;
            };
            match serde_json::from_value::<Vec<GroupSummary>>(items) {
                Ok(groups) => {
                    app.groups = groups;
                    app.nav_index = app.nav_index.min(app.target_count().saturating_sub(1));
                }
                Err(error) => {
                    app.status = format!(
                        "{}: {error}",
                        tr(app, "无法解析群聊列表", "Could not parse Group list")
                    );
                }
            }
        }
        "history" => {
            // A history payload starts an authoritative session replay. Clear any
            // stale local run state first; an immediately following `start` event
            // restores it when the daemon still has an active round.
            app.busy = false;
            app.lines.clear();
            app.last_image_url = None;
            let messages = value
                .get("messages")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            for message in messages {
                let role = message
                    .get("role")
                    .and_then(Value::as_str)
                    .unwrap_or("message");
                let content = message
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !content.is_empty() {
                    let kind = match role {
                        "user" => LineKind::User,
                        "assistant" => LineKind::Assistant,
                        "tool_call" | "tool_result" => LineKind::Tool,
                        _ => LineKind::System,
                    };
                    app.push(role, content, kind);
                }
                collect_images(app, message);
            }
            restore_plan_history(app, &value);
            reconcile_pending_outbound_history(app, messages, &value);
            replace_history_user_messages(app, messages);
        }
        "start" => {
            settle_pending_outbound(app);
            app.busy = true;
        }
        "group_history" => {
            app.lines.clear();
            app.active_group_runs.clear();
            app.last_image_url = None;
            let messages = value
                .get("messages")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            for message in messages {
                let role = message
                    .get("author_name")
                    .or_else(|| message.get("session_name"))
                    .or_else(|| message.get("role"))
                    .and_then(Value::as_str)
                    .unwrap_or("group");
                let content = message
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !content.is_empty() {
                    app.push(role, content, LineKind::Assistant);
                }
            }
            reconcile_pending_outbound_history(app, messages, &value);
            replace_history_user_messages(app, messages);
            if let Some(runs) = value.get("runs").and_then(Value::as_array) {
                for run in runs {
                    let status = run
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let id = run.get("id").and_then(Value::as_str).unwrap_or_default();
                    if matches!(status, "queued" | "running") && !id.is_empty() {
                        app.active_group_runs.insert(id.to_string());
                    }
                }
            }
            app.busy = !app.active_group_runs.is_empty();
        }
        "delta" => {
            let content = value
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if let Some(last) = app.lines.last_mut().filter(|line| {
                matches!(line.style, LineKind::Assistant) && line.role == "assistant-stream"
            }) {
                last.content.push_str(content);
            } else {
                app.push("assistant-stream", content, LineKind::Assistant);
            }
        }
        "thinking_start" => {
            for line in app
                .lines
                .iter_mut()
                .filter(|line| line.stream_id.as_deref() == Some(SESSION_REASONING_STREAM_ID))
            {
                line.stream_id = None;
            }
        }
        "reasoning_delta" | "thinking" | "thinking_delta" => {
            let content = value
                .get("content")
                .or_else(|| value.get("thinking"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            if content.is_empty() {
                return SocketEventAction::None;
            }
            if let Some(line) = app
                .lines
                .iter_mut()
                .rev()
                .find(|line| line.stream_id.as_deref() == Some(SESSION_REASONING_STREAM_ID))
            {
                line.content.push_str(content);
                app.scroll = u16::MAX;
            } else {
                app.push_stream(
                    SESSION_REASONING_STREAM_ID,
                    "reasoning",
                    content,
                    LineKind::Reasoning,
                );
            }
        }
        "thinking_done" => {
            for line in app
                .lines
                .iter_mut()
                .filter(|line| line.stream_id.as_deref() == Some(SESSION_REASONING_STREAM_ID))
            {
                line.stream_id = None;
            }
        }
        "tool_call" => {
            let name = value.get("name").and_then(Value::as_str).unwrap_or("tool");
            let arguments = value
                .get("arguments")
                .and_then(Value::as_str)
                .unwrap_or_default();
            app.push("tool", format!("▶ {name} {arguments}"), LineKind::Tool);
        }
        "tool_result" => {
            let name = value.get("name").and_then(Value::as_str).unwrap_or("tool");
            let result = value
                .get("result")
                .and_then(Value::as_str)
                .unwrap_or_default();
            app.push("tool", format!("✓ {name}\n{result}"), LineKind::Tool);
            collect_images(app, &value);
        }
        "plan_state" => {
            settle_pending_outbound(app);
            let plan = value.get("plan").unwrap_or(&value);
            if let Some(snapshot) = plan_snapshot_from_value(plan) {
                let status = snapshot.status.clone();
                let title = snapshot.title.clone();
                let revision = snapshot.revision;
                app.active_plan = Some(snapshot);
                app.plan_feedback_mode = false;
                app.plan_stale = None;
                app.pending_plan_action = None;
                app.confirm_stale_plan = false;
                refresh_plan_inspector(app);
                app.push(
                    "plan",
                    format!("{title} · {status} · r{revision}"),
                    LineKind::System,
                );
            }
        }
        "plan_stale" => {
            settle_pending_outbound(app);
            let event_plan_id = value
                .get("plan_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let event_revision = value.get("revision").and_then(Value::as_u64).unwrap_or(0);
            let matches_active_plan = app
                .active_plan
                .as_ref()
                .is_some_and(|plan| plan.id == event_plan_id && plan.revision == event_revision);
            let confirmation_token = value
                .get("confirmation_token")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if matches_active_plan && !confirmation_token.is_empty() {
                let paths = value
                    .get("paths")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>();
                let action =
                    app.pending_plan_action.take().unwrap_or_else(|| {
                        if app.active_plan.as_ref().is_some_and(|plan| {
                            matches!(plan.status.as_str(), "failed" | "stopped")
                        }) {
                            "resume".to_string()
                        } else {
                            "execute".to_string()
                        }
                    });
                app.plan_stale = Some(PlanStaleSnapshot {
                    confirmation_token: confirmation_token.to_string(),
                    paths,
                    evidence_incomplete: value
                        .get("evidence_incomplete")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    action,
                });
                app.confirm_stale_plan = false;
                app.status = tr(
                    app,
                    "计划证据已变化：按 F 刷新，或按 X 明确确认仍然执行",
                    "Plan evidence changed: press F to refresh, or X to explicitly execute anyway",
                )
                .to_string();
            } else {
                app.status = tr(
                    app,
                    "收到的过期计划确认已失效，请重新加载计划",
                    "The stale-plan confirmation is no longer current; reload the plan",
                )
                .to_string();
            }
        }
        "todos_state" => {
            settle_pending_outbound(app);
            app.todos_snapshot = serde_json::to_string_pretty(&value).unwrap_or_default();
            if app.page() == Page::Todos {
                app.inspector_payload = Some(value);
                let count = app
                    .inspector_payload
                    .as_ref()
                    .and_then(|payload| payload.get("items"))
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len);
                app.inspector_index = app.inspector_index.min(count.saturating_sub(1));
                refresh_interactive_inspector(app);
            }
        }
        "feature_status" => {
            let was_enabled = app.groups_enabled;
            let enabled = value
                .pointer("/features/groups")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            app.groups_enabled = enabled;
            if !enabled {
                let was_in_group = app.active_group.is_some();
                if was_in_group {
                    // The Group target is being removed. Settle its in-flight
                    // write instead of replaying that draft into Main, and keep
                    // any unsent Group composer text out of the new target.
                    settle_pending_outbound(app);
                    app.input.clear();
                }
                app.groups.clear();
                app.active_group = None;
                app.active_group_runs.clear();
                app.group_target_mode = "all".to_string();
                app.group_targets.clear();
                app.busy = false;
                if was_in_group {
                    if let Some((index, main)) = app
                        .sessions
                        .iter()
                        .enumerate()
                        .find(|(_, session)| crate::is_main(&session.id))
                    {
                        app.session = main.clone();
                        app.nav_index = index;
                    }
                    reset_target_scoped_state(app);
                    app.status = tr(
                        app,
                        "群聊已由配置关闭，已返回 Main",
                        "Groups were disabled by configuration; returned to Main",
                    )
                    .to_string();
                    action = SocketEventAction::ReconnectMain;
                }
                app.pages.retain(|page| *page != Page::Groups);
                app.page_index = app.page_index.min(app.pages.len().saturating_sub(1));
                app.nav_index = app.nav_index.min(app.target_count().saturating_sub(1));
            } else {
                if !app.pages.contains(&Page::Groups) {
                    app.pages.push(Page::Groups);
                }
                if !was_enabled {
                    action = SocketEventAction::RefreshGroups;
                }
            }
        }
        "storage_status" => {
            let mode = value.pointer("/storage/mode").and_then(Value::as_str);
            match mode {
                Some("protected") => {
                    let changed = app.storage_writable;
                    app.storage_writable = false;
                    restore_pending_outbound(app);
                    app.busy = false;
                    app.active_group_runs.clear();
                    app.quit_armed = false;
                    app.status = tr(
                        app,
                        "本地存储处于保护模式；核心写操作已禁用。修复后请重启 LingClaw。",
                        "Local storage is protected; core writes are disabled. Repair it and restart LingClaw.",
                    )
                    .to_string();
                    if changed {
                        app.push("storage", app.status.clone(), LineKind::Error);
                    }
                }
                Some("healthy") => {
                    let changed = !app.storage_writable;
                    app.storage_writable = true;
                    if changed {
                        app.status =
                            tr(app, "本地存储状态正常", "Local storage is healthy").to_string();
                    }
                }
                _ => {}
            }
        }
        "done" | "stopped" => {
            settle_pending_outbound(app);
            app.busy = false;
        }
        "error" => {
            restore_pending_outbound(app);
            app.busy = false;
            app.push("error", event_content(&value), LineKind::Error);
        }
        "system" => {
            let rejected_message = app
                .pending_outbound_write
                .as_ref()
                .is_some_and(|snapshot| !snapshot.is_slash_command());
            if rejected_message {
                restore_pending_outbound(app);
            } else {
                // Slash commands use `system` for both successful results and
                // validation feedback, so the response itself settles them.
                settle_pending_outbound(app);
            }
            app.push("system", event_content(&value), LineKind::System);
        }
        "group_message" => {
            if let Some(message) = value.get("message") {
                let author = message
                    .get("author_name")
                    .or_else(|| message.get("session_name"))
                    .and_then(Value::as_str)
                    .unwrap_or("group");
                let content = message
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                app.push(author, content, LineKind::Assistant);
            }
        }
        "group_member_event" => apply_group_member_event(app, &value),
        "group_run_started" => {
            settle_pending_outbound(app);
            if let Some(id) = value.pointer("/run/id").and_then(Value::as_str) {
                app.active_group_runs.insert(id.to_string());
                app.busy = true;
            }
        }
        "group_member_status" | "group_run_completed" => {
            let run = value.get("run").unwrap_or(&value);
            let id = run
                .get("id")
                .or_else(|| value.get("run_id"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let status = run
                .get("status")
                .or_else(|| value.get("status"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            if matches!(status, "completed" | "failed" | "stopped") && !id.is_empty() {
                app.active_group_runs.remove(id);
                app.busy = !app.active_group_runs.is_empty();
            }
        }
        "react_phase" | "tool_progress" | "task_progress" | "orchestrate_progress" => {
            app.status = event_content(&value);
        }
        other => {
            let compact = event_content(&value);
            app.push("event", format!("[{other}] {compact}"), LineKind::System);
        }
    }
    action
}

fn update_session_model_status(app: &mut App, value: &Value) {
    if let Some(model) = value.get("model").and_then(Value::as_str) {
        app.current_model = model.to_string();
    }
    if let Some(effort) = value.get("effort").and_then(Value::as_str) {
        app.current_effort = effort.to_string();
    }
    if let Some(revision) = value.get("configRevision").and_then(Value::as_u64) {
        app.model_config_revision = revision;
    }
    if let Some(capabilities) = value.get("capabilities") {
        if let Some(supports_image) = capabilities.get("image").and_then(Value::as_bool) {
            app.current_model_supports_image = supports_image;
        }
        if capabilities.get("s3_config_id").is_some() {
            app.current_s3_config_id = capabilities
                .get("s3_config_id")
                .and_then(Value::as_str)
                .filter(|config_id| !config_id.is_empty())
                .map(str::to_string);
        }
    }
}

fn group_member_label(app: &App, session_id: &str) -> String {
    app.sessions
        .iter()
        .find(|session| crate::session_ids_match(&session.id, session_id))
        .map(|session| {
            if session.name.is_empty() {
                session.id.clone()
            } else {
                session.name.clone()
            }
        })
        .unwrap_or_else(|| session_id.to_string())
}

fn apply_group_member_event(app: &mut App, value: &Value) {
    let session_id = value
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or("member");
    let run_id = value
        .get("run_id")
        .and_then(Value::as_str)
        .unwrap_or(session_id);
    let Some(event) = value.get("event") else {
        app.push(
            "group",
            "[group_member_event] missing event payload",
            LineKind::System,
        );
        return;
    };
    let event_type = event
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let label = group_member_label(app, session_id);
    let assistant_stream_id = format!("group:{run_id}:assistant");
    let reasoning_stream_id = format!("group:{run_id}:reasoning");

    match event_type {
        "delta" => {
            let content = event
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if content.is_empty() {
                return;
            }
            if let Some(line) = app
                .lines
                .iter_mut()
                .rev()
                .find(|line| line.stream_id.as_deref() == Some(assistant_stream_id.as_str()))
            {
                line.content.push_str(content);
                app.scroll = u16::MAX;
            } else {
                app.push_stream(assistant_stream_id, label, content, LineKind::Assistant);
            }
        }
        "thinking_delta" | "reasoning_delta" | "thinking" => {
            let content = event
                .get("content")
                .or_else(|| event.get("thinking"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            if content.is_empty() {
                return;
            }
            if let Some(line) = app
                .lines
                .iter_mut()
                .rev()
                .find(|line| line.stream_id.as_deref() == Some(reasoning_stream_id.as_str()))
            {
                line.content.push_str(content);
                app.scroll = u16::MAX;
            } else {
                app.push_stream(
                    reasoning_stream_id,
                    format!("{label} · reasoning"),
                    content,
                    LineKind::Reasoning,
                );
            }
        }
        "thinking_start"
        | "react_phase"
        | "tool_progress"
        | "task_progress"
        | "orchestrate_progress" => {
            if let Some(content) = event
                .get("content")
                .or_else(|| event.get("error"))
                .or_else(|| event.get("message"))
                .and_then(Value::as_str)
                .filter(|content| !content.is_empty())
            {
                app.status = format!("{label}: {content}");
            }
        }
        "thinking_done" => {
            for line in app
                .lines
                .iter_mut()
                .filter(|line| line.stream_id.as_deref() == Some(reasoning_stream_id.as_str()))
            {
                line.stream_id = None;
            }
        }
        "tool_call" => {
            let name = event.get("name").and_then(Value::as_str).unwrap_or("tool");
            let arguments = event
                .get("arguments")
                .and_then(Value::as_str)
                .unwrap_or_default();
            app.push(
                format!("{label} · tool"),
                format!("▶ {name} {arguments}"),
                LineKind::Tool,
            );
        }
        "tool_result" => {
            let name = event.get("name").and_then(Value::as_str).unwrap_or("tool");
            let result = event
                .get("result")
                .and_then(Value::as_str)
                .unwrap_or_default();
            app.push(
                format!("{label} · tool"),
                format!("✓ {name}\n{result}"),
                LineKind::Tool,
            );
            collect_images(app, event);
        }
        "error" => app.push(
            format!("{label} · error"),
            event_content(event),
            LineKind::Error,
        ),
        "done" | "stopped" => {
            for line in app.lines.iter_mut().filter(|line| {
                matches!(
                    line.stream_id.as_deref(),
                    Some(id) if id == assistant_stream_id || id == reasoning_stream_id
                )
            }) {
                line.stream_id = None;
            }
        }
        other => app.push(
            label,
            format!("[{other}] {}", event_content(event)),
            LineKind::System,
        ),
    }
}

fn collect_images(app: &mut App, value: &Value) {
    let Some(images) = value.get("images").and_then(Value::as_array) else {
        return;
    };
    for image in images {
        let url = image.get("url").and_then(Value::as_str).unwrap_or_default();
        let name = image.get("name").and_then(Value::as_str).unwrap_or("image");
        if !url.is_empty() {
            app.last_image_url = Some(url.to_string());
            app.push(
                "image",
                format!("🖼 {name} · {} · {url}", app.image_protocol),
                LineKind::Tool,
            );
        }
    }
}

#[cfg(feature = "tui-images")]
fn reconcile_terminal_image_preview(
    client: &Client,
    app: &mut App,
    sender: &mpsc::Sender<TerminalImagePreviewEvent>,
    task: &mut Option<tokio::task::JoinHandle<()>>,
    requested_context: &mut Option<TerminalImagePreviewContext>,
) {
    let next_context = TerminalImagePreviewContext::capture(app);
    if *requested_context == next_context {
        return;
    }
    if let Some(previous_task) = task.take() {
        previous_task.abort();
    }
    *requested_context = next_context.clone();
    app.image_preview = None;

    let Some(context) = next_context else {
        return;
    };
    let Some(picker) = app.image_picker.clone() else {
        return;
    };
    let client = client.clone();
    let sender = sender.clone();
    *task = Some(tokio::spawn(async move {
        let result = load_terminal_image_preview(&client, picker, &context.url).await;
        let _ = sender
            .send(TerminalImagePreviewEvent { context, result })
            .await;
    }));
}

#[cfg(not(feature = "tui-images"))]
fn reconcile_terminal_image_preview(
    _client: &Client,
    _app: &mut App,
    _sender: &mpsc::Sender<TerminalImagePreviewEvent>,
    _task: &mut Option<tokio::task::JoinHandle<()>>,
    _requested_context: &mut Option<TerminalImagePreviewContext>,
) {
}

#[cfg(feature = "tui-images")]
async fn load_terminal_image_preview(
    client: &Client,
    picker: Picker,
    url: &str,
) -> TuiResult<StatefulProtocol> {
    const MAX_PREVIEW_BYTES: usize = 10 * 1024 * 1024;
    let mut response = client.get(url).send().await?.error_for_status()?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_PREVIEW_BYTES as u64)
    {
        return Err("image preview exceeds 10 MB".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len().saturating_add(chunk.len()) > MAX_PREVIEW_BYTES {
            return Err("image preview exceeds 10 MB".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    tokio::task::spawn_blocking(move || {
        let image = image::load_from_memory(&bytes)?;
        Ok(picker.new_resize_protocol(image))
    })
    .await
    .map_err(|error| format!("image preview decode task failed: {error}"))?
}

#[cfg(feature = "tui-images")]
fn apply_terminal_image_preview(
    app: &mut App,
    event: TerminalImagePreviewEvent,
    requested_context: Option<&TerminalImagePreviewContext>,
) -> bool {
    if requested_context != Some(&event.context) || !event.context.is_current(app) {
        return false;
    }
    match event.result {
        Ok(preview) => app.image_preview = Some(preview),
        Err(error) => {
            app.image_preview = None;
            app.status = format!(
                "{}: {error}",
                tr(app, "图片预览不可用", "Image preview unavailable")
            );
        }
    }
    true
}

#[cfg(not(feature = "tui-images"))]
fn apply_terminal_image_preview(
    _app: &mut App,
    _event: TerminalImagePreviewEvent,
    _requested_context: Option<&TerminalImagePreviewContext>,
) -> bool {
    false
}

fn event_content(value: &Value) -> String {
    value
        .get("content")
        .or_else(|| value.get("error"))
        .or_else(|| value.get("message"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| serde_json::to_string(value).unwrap_or_default())
}

fn client_s3_config_id(value: &Value) -> Result<&str, String> {
    value
        .get("s3_config_id")
        .and_then(Value::as_str)
        .filter(|config_id| !config_id.is_empty())
        .ok_or_else(|| "daemon did not provide an S3 configuration identity".to_string())
}

async fn validate_image_upload_context(
    client: &Client,
    base: &str,
    context: &ImageUploadContext,
) -> TuiResult<Value> {
    let model_status: Value = client
        .get(format!("{base}/api/session-models"))
        .query(&[("session", context.session_id.as_str())])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let current_session = model_status
        .pointer("/session/id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let current_model = model_status
        .pointer("/session/model")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let current_effort = model_status
        .pointer("/session/effort")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let current_revision = model_status
        .get("configRevision")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let supports_image = model_status
        .pointer("/capabilities/image")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !crate::session_ids_match(current_session, &context.session_id)
        || current_model != context.model
        || current_effort != context.effort
        || current_revision != context.config_revision
        || !supports_image
    {
        return Err(
            "Session model or image capability changed while uploading; attach the image again"
                .to_string()
                .into(),
        );
    }

    let client_config: Value = client
        .get(format!("{base}/api/client-config"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if client_s3_config_id(&client_config)? != context.s3_config_id.as_str() {
        return Err(
            "S3 configuration changed while uploading; attach the image again"
                .to_string()
                .into(),
        );
    }
    Ok(client_config)
}

fn uploaded_images_for_s3_config(
    payload: &Value,
    expected_config_id: &str,
) -> Result<Vec<Value>, String> {
    let response_config_id = client_s3_config_id(payload)?;
    if response_config_id != expected_config_id {
        return Err("S3 configuration changed while uploading; attach the image again".to_string());
    }
    let images = payload
        .get("images")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if images.is_empty() {
        let error = payload
            .get("errors")
            .and_then(Value::as_array)
            .and_then(|errors| errors.first())
            .and_then(Value::as_str)
            .unwrap_or("the daemon did not accept this image");
        return Err(error.to_string());
    }
    if images
        .iter()
        .any(|image| image.get("s3_config_id").and_then(Value::as_str) != Some(response_config_id))
    {
        return Err("image upload response did not match the active S3 configuration".to_string());
    }
    Ok(images)
}

async fn upload_local_image(
    client: &Client,
    base: &str,
    context: &ImageUploadContext,
    requested_path: &Path,
) -> TuiResult<Vec<Value>> {
    let path = if requested_path.is_absolute() {
        requested_path.to_path_buf()
    } else {
        let root = context
            .workspace
            .clone()
            .unwrap_or(std::env::current_dir()?);
        root.join(requested_path)
    };
    let metadata = tokio::fs::metadata(&path)
        .await
        .map_err(|error| format!("cannot inspect image '{}': {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("image path is not a file: {}", path.display()).into());
    }
    if metadata.len() > crate::image_uploads::MAX_IMAGE_UPLOAD_BYTES as u64 {
        return Err(format!(
            "image '{}' exceeds the {} byte upload limit",
            path.display(),
            crate::image_uploads::MAX_IMAGE_UPLOAD_BYTES
        )
        .into());
    }
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|error| format!("cannot read image '{}': {error}", path.display()))?;
    if bytes.len() > crate::image_uploads::MAX_IMAGE_UPLOAD_BYTES {
        return Err(format!(
            "image '{}' exceeds the {} byte upload limit",
            path.display(),
            crate::image_uploads::MAX_IMAGE_UPLOAD_BYTES
        )
        .into());
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("image")
        .to_string();
    let client_config = validate_image_upload_context(client, base, context).await?;
    let token = client_config
        .get("upload_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .ok_or("daemon did not provide an image upload token")?;
    let upload_config_id = context.s3_config_id.clone();
    let form = multipart::Form::new().part("file", multipart::Part::bytes(bytes).file_name(name));
    let response = client
        .post(format!("{base}/api/upload-images"))
        .header("X-LingClaw-Upload-Token", token)
        .multipart(form)
        .send()
        .await?;
    let status = response.status();
    let payload: Value = response.json().await?;
    if !status.is_success() {
        return Err(payload
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("image upload failed")
            .to_string()
            .into());
    }
    let images = uploaded_images_for_s3_config(&payload, &upload_config_id)?;
    validate_image_upload_context(client, base, context).await?;
    Ok(images)
}

struct TempConfigFile {
    path: PathBuf,
}

impl TempConfigFile {
    fn create(contents: &[u8]) -> TuiResult<Self> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "lingclaw-config-edit-{}-{nonce}.json",
            std::process::id()
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&path)?;
        let temporary = Self { path };
        file.write_all(contents)?;
        file.flush()?;
        Ok(temporary)
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempConfigFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

struct ConfigSnapshot {
    config: Option<Value>,
    raw: String,
    parse_error: Option<String>,
    etag: Option<String>,
    explicit_primary_model_configured: bool,
}

impl ConfigSnapshot {
    fn structured_config(&self) -> TuiResult<Value> {
        self.config.clone().ok_or_else(|| {
            self.parse_error
                .clone()
                .unwrap_or_else(|| "the current configuration is not valid JSON".to_string())
                .into()
        })
    }
}

fn config_snapshot_from_payload(snapshot: Value) -> TuiResult<ConfigSnapshot> {
    let config_value = snapshot.get("config").cloned();
    let config = config_value.clone().filter(Value::is_object);
    let raw = snapshot
        .get("raw")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            config_value
                .as_ref()
                .map(serde_json::to_string_pretty)
                .transpose()
                .ok()
                .flatten()
        })
        .ok_or("the configuration response did not include editable content")?;
    let parse_error = if config.is_none() {
        Some(
            snapshot
                .get("parse_error")
                .and_then(Value::as_str)
                .unwrap_or("configuration root must be a JSON object")
                .to_string(),
        )
    } else {
        None
    };
    let etag = snapshot
        .get("configFileEtag")
        .and_then(Value::as_str)
        .map(str::to_string);
    Ok(ConfigSnapshot {
        config,
        raw,
        parse_error,
        etag,
        explicit_primary_model_configured: snapshot
            .get("explicitPrimaryModelConfigured")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

async fn fetch_config_snapshot(client: &Client, base: &str) -> TuiResult<ConfigSnapshot> {
    let snapshot: Value = client
        .get(format!("{base}/api/config"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    config_snapshot_from_payload(snapshot)
}

async fn fetch_editable_config(client: &Client, base: &str) -> TuiResult<(Value, Option<String>)> {
    let snapshot = fetch_config_snapshot(client, base).await?;
    Ok((snapshot.structured_config()?, snapshot.etag))
}

async fn save_edited_config(
    client: &Client,
    base: &str,
    session_id: &str,
    config: Value,
    etag: Option<&str>,
) -> TuiResult<String> {
    if !config.is_object() {
        return Err("configuration root must be a JSON object".into());
    }
    let response = client
        .put(format!("{base}/api/config"))
        .timeout(LONG_CONTROL_REQUEST_TIMEOUT)
        .json(&json!({
            "config": config,
            "session": session_id,
            "baseConfigFileEtag": etag,
        }))
        .send()
        .await?;
    let status = response.status();
    let payload: Value = response.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        return Err(payload
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("configuration save failed")
            .to_string()
            .into());
    }
    Ok("Configuration saved and hot-reloaded.".to_string())
}

async fn set_groups_enabled(
    client: &Client,
    base: &str,
    session_id: &str,
    enabled: bool,
) -> TuiResult<String> {
    let (mut config, etag) = fetch_editable_config(client, base).await?;
    let root = config
        .as_object_mut()
        .ok_or("configuration root must be an object")?;
    let settings = object_field_mut(root, "settings")?;
    settings.insert("enableGroups".to_string(), json!(enabled));
    save_edited_config(client, base, session_id, config, etag.as_deref()).await?;
    Ok(if enabled {
        "Group chat enabled.".to_string()
    } else {
        "Group chat disabled; stored Group data was preserved.".to_string()
    })
}

fn apply_config_mutation(config: &mut Value, mutation: &ConfigMutation) -> TuiResult<()> {
    let Some((leaf, parents)) = mutation.path.split_last() else {
        return Err("configuration path cannot be empty".into());
    };
    let mut current = config
        .as_object_mut()
        .ok_or("configuration root must be an object")?;
    for segment in parents {
        if mutation.value.is_none() && !current.contains_key(segment) {
            return Ok(());
        }
        let child = current.entry(segment.clone()).or_insert_with(|| json!({}));
        current = child
            .as_object_mut()
            .ok_or_else(|| format!("configuration field '{}' must be an object", segment))?;
    }
    if let Some(value) = mutation.value.clone() {
        current.insert(leaf.clone(), value);
    } else {
        current.remove(leaf);
    }
    Ok(())
}

async fn execute_config_mutation(
    client: &Client,
    base: &str,
    app: &App,
    mutation: ConfigMutation,
) -> TuiResult<String> {
    // Always patch a fresh, unredacted snapshot. The Settings presentation is
    // deliberately redacted and must never be round-tripped over API keys or
    // other credentials that the user did not edit.
    let (mut config, etag) = fetch_editable_config(client, base).await?;
    apply_config_mutation(&mut config, &mutation)?;
    save_edited_config(client, base, &app.session.id, config, etag.as_deref()).await?;
    Ok(tr(app, "设置已保存并热重载", "Settings saved and hot-reloaded").to_string())
}

async fn execute_page_mutation(
    client: &Client,
    base: &str,
    app: &mut App,
    mutation: PageMutation,
) -> TuiResult<String> {
    if app.active_group.is_some() && !matches!(&mutation, PageMutation::Config(_)) {
        return Err(group_session_scope_error(app).into());
    }
    let mutation = match mutation {
        PageMutation::Config(mutation) => {
            return execute_config_mutation(client, base, app, mutation).await;
        }
        mutation => mutation,
    };
    let session_id = app.session.id.clone();
    let (endpoint, body, success_zh, success_en, todos_mutation) = match mutation {
        PageMutation::Model { model, effort } => (
            "/api/session-models",
            json!({ "model": model, "effort": effort }),
            "模型与 Effort 已更新",
            "Model and Effort updated",
            false,
        ),
        PageMutation::Skills {
            enabled_system_skills,
            known_system_skills,
        } => (
            "/api/session-skills",
            json!({
                "enabledSystemSkills": enabled_system_skills,
                "knownSystemSkills": known_system_skills,
            }),
            "Skill 权限已更新",
            "Skill permissions updated",
            false,
        ),
        PageMutation::McpPolicy(policy) => (
            "/api/mcp/session-policy",
            policy,
            "MCP 权限已更新",
            "MCP permissions updated",
            false,
        ),
        PageMutation::Todos {
            base_revision,
            items,
        } => (
            "/api/todos",
            json!({ "base_revision": base_revision, "items": items }),
            "Todo 已更新",
            "Todos updated",
            true,
        ),
        PageMutation::Config(_) => unreachable!("Config mutations return before page dispatch"),
    };
    let response = client
        .put(format!("{base}{endpoint}"))
        .query(&[("session", session_id.as_str())])
        .json(&body)
        .send()
        .await?;
    let status = response.status();
    let payload: Value = response.json().await.unwrap_or(Value::Null);
    if todos_mutation && payload.get("revision").is_some() {
        app.todos_snapshot = serde_json::to_string_pretty(&payload).unwrap_or_default();
        if app.page() == Page::Todos {
            app.inspector_payload = Some(payload.clone());
            let count = payload
                .get("items")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            app.inspector_index = app.inspector_index.min(count.saturating_sub(1));
            refresh_interactive_inspector(app);
        }
    }
    if !status.is_success() {
        let message = payload
            .get("error")
            .and_then(Value::as_str)
            .or_else(|| payload.get("code").and_then(Value::as_str))
            .map(str::to_string)
            .unwrap_or_else(|| format!("HTTP {status}"));
        if status == reqwest::StatusCode::CONFLICT && todos_mutation {
            return Err(format!(
                "{}: {message}",
                tr(
                    app,
                    "Todo 已在其他客户端更新，已载入最新版本",
                    "Todos changed in another client; the latest version was loaded"
                )
            )
            .into());
        }
        return Err(message.into());
    }
    Ok(tr(app, success_zh, success_en).to_string())
}

fn configured_editor() -> Option<String> {
    std::env::var("VISUAL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            std::env::var("EDITOR")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
}

async fn edit_config(
    client: &Client,
    base: &str,
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    events: &mut EventStream,
    language: UiLanguage,
    session_id: &str,
) -> TuiResult<String> {
    let snapshot = fetch_config_snapshot(client, base).await?;
    let Some(editor) = configured_editor() else {
        return edit_config_inline(
            client,
            base,
            terminal,
            events,
            ConfigEditContext {
                language,
                session_id,
            },
            snapshot.raw,
            snapshot.etag,
        )
        .await;
    };
    edit_config_with_external_editor(
        client,
        base,
        terminal,
        session_id,
        snapshot.raw,
        snapshot.etag,
        &editor,
    )
    .await
}

#[derive(Clone, Copy)]
struct ConfigEditContext<'a> {
    language: UiLanguage,
    session_id: &'a str,
}

async fn edit_config_with_external_editor(
    client: &Client,
    base: &str,
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    session_id: &str,
    raw_config: String,
    etag: Option<String>,
    editor: &str,
) -> TuiResult<String> {
    let editor_parts = split_command_line(editor)?;
    let temporary = TempConfigFile::create(raw_config.as_bytes())?;

    let suspend_result = (|| -> TuiResult<()> {
        // Ratatui keeps the cursor hidden while frames do not request one.
        // `Terminal` remains alive across the editor handoff, so its Drop
        // restoration cannot help the child process.
        terminal.show_cursor()?;
        disable_raw_mode()?;
        execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture)?;
        Ok(())
    })();
    if let Err(error) = suspend_result {
        if let Err(restore_error) = restore_terminal_after_external_editor(terminal) {
            return Err(Box::new(restore_error));
        }
        return Err(error);
    }
    let editor_result = Command::new(&editor_parts[0])
        .args(&editor_parts[1..])
        .arg(temporary.path())
        .status();
    if let Err(error) = restore_terminal_after_external_editor(terminal) {
        return Err(Box::new(error));
    }
    let status = editor_result?;
    if !status.success() {
        return Err("external editor exited unsuccessfully".into());
    }
    let edited = std::fs::read_to_string(temporary.path())?;
    drop(temporary);
    let config: Value = serde_json::from_str(&edited)?;
    save_edited_config(client, base, session_id, config, etag.as_deref()).await
}

#[derive(Debug)]
struct TerminalRestoreError(String);

impl std::fmt::Display for TerminalRestoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "could not restore terminal after external editor: {}",
            self.0
        )
    }
}

impl std::error::Error for TerminalRestoreError {}

/// Restore every terminal facility independently. A single failed operation
/// must not skip the remaining recovery work because the external editor may
/// have left raw mode, the alternate screen, and mouse capture in different
/// partial states.
fn restore_terminal_after_external_editor(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
) -> Result<(), TerminalRestoreError> {
    let mut errors = Vec::new();
    if let Err(error) = execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture) {
        errors.push(format!("screen: {error}"));
    }
    if let Err(error) = enable_raw_mode() {
        errors.push(format!("raw mode: {error}"));
    }
    if let Err(error) = terminal.clear() {
        errors.push(format!("clear: {error}"));
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(TerminalRestoreError(errors.join("; ")))
    }
}

#[derive(Clone, Debug)]
struct TextBuffer {
    characters: Vec<char>,
    cursor: usize,
}

impl TextBuffer {
    fn new(value: String) -> Self {
        let characters = value.chars().collect::<Vec<_>>();
        let cursor = characters.len();
        Self { characters, cursor }
    }

    fn value(&self) -> String {
        self.characters.iter().collect()
    }

    fn insert(&mut self, value: &str) {
        let inserted = value.chars().collect::<Vec<_>>();
        let count = inserted.len();
        self.characters.splice(self.cursor..self.cursor, inserted);
        self.cursor += count;
    }

    fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.characters.remove(self.cursor);
        }
    }

    fn delete(&mut self) {
        if self.cursor < self.characters.len() {
            self.characters.remove(self.cursor);
        }
    }

    fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    fn move_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.characters.len());
    }

    fn line_ranges(&self) -> Vec<(usize, usize)> {
        let mut start = 0usize;
        let mut ranges = Vec::new();
        for (index, character) in self.characters.iter().enumerate() {
            if *character == '\n' {
                ranges.push((start, index));
                start = index + 1;
            }
        }
        ranges.push((start, self.characters.len()));
        ranges
    }

    fn line_column(&self) -> (usize, usize) {
        let ranges = self.line_ranges();
        ranges
            .iter()
            .enumerate()
            .find_map(|(line, (start, end))| {
                (self.cursor >= *start && self.cursor <= *end)
                    .then_some((line, self.cursor - start))
            })
            .unwrap_or_else(|| {
                let line = ranges.len().saturating_sub(1);
                (line, ranges[line].1.saturating_sub(ranges[line].0))
            })
    }

    fn move_vertical(&mut self, delta: isize) {
        let ranges = self.line_ranges();
        let (line, column) = self.line_column();
        let target = line
            .saturating_add_signed(delta)
            .min(ranges.len().saturating_sub(1));
        let (start, end) = ranges[target];
        self.cursor = start + column.min(end - start);
    }

    fn line_home(&mut self) {
        let ranges = self.line_ranges();
        let (line, _) = self.line_column();
        self.cursor = ranges[line].0;
    }

    fn line_end(&mut self) {
        let ranges = self.line_ranges();
        let (line, _) = self.line_column();
        self.cursor = ranges[line].1;
    }
}

async fn edit_config_inline(
    client: &Client,
    base: &str,
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    events: &mut EventStream,
    context: ConfigEditContext<'_>,
    raw_config: String,
    etag: Option<String>,
) -> TuiResult<String> {
    let mut buffer = TextBuffer::new(raw_config);
    let mut status = String::new();
    loop {
        terminal.draw(|frame| render_inline_editor(frame, context.language, &buffer, &status))?;
        match events.next().await {
            Some(Ok(Event::Key(key))) if key.kind == crossterm::event::KeyEventKind::Press => {
                if key.code == KeyCode::Esc {
                    return Ok(tr_raw(
                        context.language,
                        "已取消配置编辑。",
                        "Configuration edit cancelled.",
                    )
                    .to_string());
                }
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
                    match serde_json::from_str::<Value>(&buffer.value()) {
                        Ok(value) => {
                            match save_edited_config(
                                client,
                                base,
                                context.session_id,
                                value,
                                etag.as_deref(),
                            )
                            .await
                            {
                                Ok(message) => return Ok(message),
                                Err(error) => status = error.to_string(),
                            }
                        }
                        Err(error) => status = format!("JSON: {error}"),
                    }
                    continue;
                }
                status.clear();
                match key.code {
                    KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        buffer.insert("\n")
                    }
                    KeyCode::Char(character)
                        if !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        buffer.insert(&character.to_string())
                    }
                    KeyCode::Enter => buffer.insert("\n"),
                    KeyCode::Tab => buffer.insert("  "),
                    KeyCode::Backspace => buffer.backspace(),
                    KeyCode::Delete => buffer.delete(),
                    KeyCode::Left => buffer.move_left(),
                    KeyCode::Right => buffer.move_right(),
                    KeyCode::Up => buffer.move_vertical(-1),
                    KeyCode::Down => buffer.move_vertical(1),
                    KeyCode::Home => buffer.line_home(),
                    KeyCode::End => buffer.line_end(),
                    _ => {}
                }
            }
            Some(Ok(Event::Paste(value))) => buffer.insert(&value),
            Some(Ok(_)) => {}
            Some(Err(error)) => return Err(error.into()),
            None => return Err("terminal input closed while editing configuration".into()),
        }
    }
}

fn render_inline_editor(
    frame: &mut Frame<'_>,
    language: UiLanguage,
    buffer: &TextBuffer,
    status: &str,
) {
    let area = frame.area();
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(4), Constraint::Length(2)])
        .split(area);
    let block = Block::default()
        .title(tr_raw(language, "Raw JSON 配置", "Raw JSON configuration"))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(112, 88, 226)));
    let inner = block.inner(sections[0]);
    let (line, column) = buffer.line_column();
    let visible_height = usize::from(inner.height.max(1));
    let scroll = line.saturating_sub(visible_height.saturating_sub(1));
    frame.render_widget(
        Paragraph::new(buffer.value())
            .wrap(Wrap { trim: false })
            .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0))
            .block(block),
        sections[0],
    );
    if line >= scroll && line - scroll < visible_height {
        let x = inner.x.saturating_add(
            u16::try_from(column)
                .unwrap_or(u16::MAX)
                .min(inner.width.saturating_sub(1)),
        );
        let y = inner.y.saturating_add(
            u16::try_from(line - scroll)
                .unwrap_or(u16::MAX)
                .min(inner.height.saturating_sub(1)),
        );
        frame.set_cursor_position((x, y));
    }
    let hint = if status.is_empty() {
        tr_raw(
            language,
            "Ctrl+S 校验并保存 · Esc 取消 · Tab 插入两个空格",
            "Ctrl+S validate and save · Esc cancel · Tab inserts two spaces",
        )
    } else {
        status
    };
    frame.render_widget(
        Paragraph::new(hint).style(if status.is_empty() {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default().fg(Color::Red)
        }),
        sections[1],
    );
}

struct LoadedPage {
    payload: Option<Value>,
    fallback: String,
}

fn selected_marker(selected: bool) -> &'static str {
    if selected { "›" } else { " " }
}

fn format_models_page(app: &App, payload: &Value) -> String {
    let models = payload
        .get("models")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let current_model = payload
        .pointer("/session/model")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let current_effort = payload
        .pointer("/session/effort")
        .and_then(Value::as_str)
        .unwrap_or("off");
    let mut lines = vec![format!(
        "{}: {} · {}\n{}",
        tr(app, "当前", "Current"),
        current_model,
        current_effort,
        tr(
            app,
            "↑/↓ 选择模型 · ←/→ 选择 Effort · Enter 应用 · R 刷新",
            "↑/↓ select model · ←/→ select Effort · Enter apply · R refresh"
        )
    )];
    if models.is_empty() {
        lines.push(tr(app, "没有可用模型", "No models are available").to_string());
        return lines.join("\n");
    }
    for (index, model) in models.iter().enumerate() {
        let model_ref = model.get("ref").and_then(Value::as_str).unwrap_or_default();
        let name = model
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .unwrap_or(model_ref);
        let provider = model
            .get("provider")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let input = model
            .get("input")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(",");
        let current = if model_ref == current_model {
            "●"
        } else {
            " "
        };
        lines.push(format!(
            "{}{} {} · {} [{}]",
            selected_marker(index == app.inspector_index),
            current,
            name,
            provider,
            input
        ));
        if index == app.inspector_index {
            let efforts = model
                .get("efforts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>();
            if !efforts.is_empty() {
                let effort = efforts
                    .get(app.inspector_choice.min(efforts.len().saturating_sub(1)))
                    .copied()
                    .unwrap_or("off");
                lines.push(format!("    Effort: {effort} · {model_ref}"));
            } else {
                lines.push(format!("    {model_ref}"));
            }
        }
    }
    lines.join("\n")
}

fn format_skills_page(app: &App, payload: &Value) -> String {
    let skills = payload
        .get("skills")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut lines = vec![
        tr(
            app,
            "↑/↓ 选择 · Space 启用或停用 · R 刷新",
            "↑/↓ select · Space enable or disable · R refresh",
        )
        .to_string(),
    ];
    if skills.is_empty() {
        lines.push(tr(app, "没有可用系统 Skill", "No system Skills are available").to_string());
    }
    for (index, skill) in skills.iter().enumerate() {
        let enabled = skill
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let name = skill
            .get("name")
            .and_then(Value::as_str)
            .or_else(|| skill.get("id").and_then(Value::as_str))
            .unwrap_or("skill");
        let id = skill.get("id").and_then(Value::as_str).unwrap_or_default();
        lines.push(format!(
            "{} [{}] {} · {}",
            selected_marker(index == app.inspector_index),
            if enabled { "x" } else { " " },
            name,
            id
        ));
        if index == app.inspector_index
            && let Some(description) = skill.get("description").and_then(Value::as_str)
            && !description.is_empty()
        {
            lines.push(format!("    {description}"));
        }
    }
    lines.join("\n")
}

fn format_mcp_page(app: &App, payload: &Value) -> String {
    let servers = payload
        .get("servers")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let tools = payload
        .get("tools")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut lines = vec![
        tr(
            app,
            "↑/↓ 选择 · Space 切换权限 · R 刷新 · /mcp oauth NAME 连接",
            "↑/↓ select · Space toggle permission · R refresh · /mcp oauth NAME to connect",
        )
        .to_string(),
    ];
    let mut row = 0usize;
    for server in servers {
        let enabled = server
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let configured = server
            .get("configuredEnabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let authenticated = server
            .get("authenticated")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let name = server
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("server");
        lines.push(format!(
            "{} S [{}] {} · {} · {}",
            selected_marker(row == app.inspector_index),
            if enabled { "x" } else { " " },
            name,
            if configured { "configured" } else { "disabled" },
            if authenticated { "auth ok" } else { "no auth" }
        ));
        row += 1;
    }
    for tool in tools {
        let enabled = tool
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let name = tool.get("name").and_then(Value::as_str).unwrap_or("tool");
        let server = tool
            .get("server")
            .and_then(Value::as_str)
            .unwrap_or_default();
        lines.push(format!(
            "{}   [{}] {} · {}",
            selected_marker(row == app.inspector_index),
            if enabled { "x" } else { " " },
            name,
            server
        ));
        row += 1;
    }
    if row == 0 {
        lines.push(
            tr(
                app,
                "没有已配置的 MCP 服务",
                "No MCP servers are configured",
            )
            .to_string(),
        );
    }
    lines.join("\n")
}

fn format_todos_page(app: &App, payload: &Value) -> String {
    let items = payload
        .get("items")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let revision = payload.get("revision").and_then(Value::as_u64).unwrap_or(0);
    let mut lines = vec![format!(
        "r{revision} · {}",
        tr(
            app,
            "↑/↓ 选择 · Space 更新状态 · Delete 删除 · A 新增 · R 刷新",
            "↑/↓ select · Space cycle status · Delete remove · A add · R refresh"
        )
    )];
    if items.is_empty() {
        lines.push(tr(app, "当前没有 Todo", "No todos yet").to_string());
    }
    for (index, item) in items.iter().enumerate() {
        let status = item
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("pending");
        let symbol = match status {
            "completed" => "✓",
            "in_progress" => "◐",
            _ => "○",
        };
        let content = item
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default();
        lines.push(format!(
            "{} {symbol} {content} · {status}",
            selected_marker(index == app.inspector_index)
        ));
    }
    lines.join("\n")
}

fn token_pair(value: &Value) -> (u64, u64) {
    value
        .as_array()
        .map(|pair| {
            (
                pair.first().and_then(Value::as_u64).unwrap_or(0),
                pair.get(1).and_then(Value::as_u64).unwrap_or(0),
            )
        })
        .unwrap_or((0, 0))
}

fn format_usage_page(app: &App, payload: &Value) -> String {
    let daily_input = payload
        .get("daily_input")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let daily_output = payload
        .get("daily_output")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let total_input = payload
        .get("total_input")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let total_output = payload
        .get("total_output")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let mut lines = vec![format!(
        "{}\n{}  {} in · {} out\n{}  {} in · {} out",
        tr(app, "R 刷新", "R refresh"),
        tr(app, "今日", "Today"),
        daily_input,
        daily_output,
        tr(app, "累计", "Total"),
        total_input,
        total_output
    )];
    for (heading, key) in [
        (
            tr(app, "Provider 排名", "Provider ranking"),
            "total_providers",
        ),
        (
            tr(app, "Agent Role 排名", "Agent role ranking"),
            "total_roles",
        ),
    ] {
        lines.push(format!("\n{heading}"));
        let mut entries = payload
            .get(key)
            .and_then(Value::as_object)
            .map(|items| {
                items
                    .iter()
                    .map(|(name, value)| {
                        let pair = token_pair(value);
                        (name, pair.0.saturating_add(pair.1), pair)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        if entries.is_empty() {
            lines.push(format!("  {}", tr(app, "暂无数据", "No data")));
        } else {
            for (name, _, (input, output)) in entries.into_iter().take(8) {
                lines.push(format!("  {name}: {input} in · {output} out"));
            }
        }
    }
    lines.push(format!(
        "\n{}: {} / {}",
        tr(app, "最近一次统计来源", "Latest usage source"),
        payload
            .get("input_source")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
        payload
            .get("output_source")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    ));
    lines.join("\n")
}

fn value_at_settings_path<'a>(config: &'a Value, path: &[String]) -> Option<&'a Value> {
    path.iter()
        .try_fold(config, |value, segment| value.get(segment))
}

fn settings_row(
    config: &Value,
    section: impl Into<String>,
    label: impl Into<String>,
    path: &[&str],
    kind: SettingsValueKind,
    default: Option<Value>,
) -> SettingsRow {
    let path = path
        .iter()
        .map(|segment| (*segment).to_string())
        .collect::<Vec<_>>();
    let value = value_at_settings_path(config, &path).cloned().or(default);
    SettingsRow {
        section: section.into(),
        label: label.into(),
        path,
        kind,
        value,
    }
}

fn settings_rows(app: &App, payload: &Value) -> Vec<SettingsRow> {
    let config = payload.get("config").unwrap_or(payload);
    let general = tr(app, "常规", "General");
    let agents = tr(app, "Agent 路由", "Agent routing");
    let providers = tr(app, "Provider 连接", "Provider connections");
    let s3 = "S3";
    let toggle = |key: &'static str, label_zh, label_en, default| {
        settings_row(
            config,
            general,
            tr(app, label_zh, label_en),
            &["settings", key],
            SettingsValueKind::Toggle,
            Some(json!(default)),
        )
    };
    let unsigned = |key: &'static str, label_zh, label_en| {
        settings_row(
            config,
            general,
            tr(app, label_zh, label_en),
            &["settings", key],
            SettingsValueKind::Unsigned { optional: true },
            None,
        )
    };
    let mut rows = vec![
        toggle("enableGroups", "群聊", "Group chat", false),
        toggle("structuredMemory", "结构化记忆", "Structured memory", false),
        toggle("dailyReflection", "每日反思", "Daily reflection", false),
        toggle(
            "enableStateDigest",
            "工作状态摘要",
            "Working-state digest",
            true,
        ),
        toggle(
            "enableTaskPlan",
            "自动执行提纲",
            "Automatic execution outline",
            false,
        ),
        toggle(
            "enableS3",
            "图片上传",
            "Image uploads",
            config.get("s3").is_some_and(Value::is_object),
        ),
        unsigned("execTimeout", "Shell 超时（秒）", "Shell timeout (seconds)"),
        unsigned("toolTimeout", "工具超时（秒）", "Tool timeout (seconds)"),
        unsigned(
            "subAgentTimeout",
            "Sub-agent 超时（秒）",
            "Sub-agent timeout (seconds)",
        ),
        unsigned("maxLlmRetries", "模型重试次数", "Model retry count"),
        settings_row(
            config,
            general,
            tr(
                app,
                "监听端口（重启生效）",
                "Listen port (restart required)",
            ),
            &["settings", "port"],
            SettingsValueKind::Unsigned { optional: true },
            Some(json!(18989)),
        ),
    ];

    for (key, label_zh, label_en, optional) in [
        ("primary", "主 Agent", "Primary Agent", false),
        ("fast", "快速模型", "Fast model", true),
        ("sub-agent", "Sub-agent 模型", "Sub-agent model", true),
        ("memory", "记忆模型", "Memory model", true),
        ("reflection", "反思模型", "Reflection model", true),
        ("context", "上下文压缩模型", "Context model", true),
    ] {
        rows.push(settings_row(
            config,
            agents,
            tr(app, label_zh, label_en),
            &["agents", "defaults", "model", key],
            SettingsValueKind::Text {
                optional,
                secret: false,
            },
            None,
        ));
    }

    let mut provider_names = config
        .pointer("/models/providers")
        .and_then(Value::as_object)
        .map(|values| values.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    provider_names.sort();
    for provider in provider_names {
        for (key, label, secret) in [
            ("api", "API", false),
            ("baseUrl", "Base URL", false),
            ("apiKey", "API Key", true),
        ] {
            rows.push(settings_row(
                config,
                providers,
                format!("{provider} · {label}"),
                &["models", "providers", &provider, key],
                SettingsValueKind::Text {
                    optional: false,
                    secret,
                },
                None,
            ));
        }
    }

    for (key, label, kind) in [
        (
            "endpoint",
            "Endpoint",
            SettingsValueKind::Text {
                optional: true,
                secret: false,
            },
        ),
        (
            "region",
            "Region",
            SettingsValueKind::Text {
                optional: true,
                secret: false,
            },
        ),
        (
            "bucket",
            "Bucket",
            SettingsValueKind::Text {
                optional: true,
                secret: false,
            },
        ),
        (
            "accessKey",
            "Access Key",
            SettingsValueKind::Text {
                optional: true,
                secret: true,
            },
        ),
        (
            "secretKey",
            "Secret Key",
            SettingsValueKind::Text {
                optional: true,
                secret: true,
            },
        ),
        (
            "prefix",
            "Prefix",
            SettingsValueKind::Text {
                optional: true,
                secret: false,
            },
        ),
        (
            "urlExpirySecs",
            "URL expiry (seconds)",
            SettingsValueKind::Unsigned { optional: true },
        ),
        (
            "lifecycleDays",
            "Lifecycle (days)",
            SettingsValueKind::Unsigned { optional: true },
        ),
    ] {
        rows.push(settings_row(config, s3, label, &["s3", key], kind, None));
    }
    rows
}

fn settings_value_label(app: &App, row: &SettingsRow) -> String {
    match &row.kind {
        SettingsValueKind::Toggle => {
            if row.value.as_ref().and_then(Value::as_bool).unwrap_or(false) {
                tr(app, "开启", "On").to_string()
            } else {
                tr(app, "关闭", "Off").to_string()
            }
        }
        SettingsValueKind::Unsigned { .. } => row
            .value
            .as_ref()
            .and_then(Value::as_u64)
            .map(|value| value.to_string())
            .unwrap_or_else(|| tr(app, "未配置", "Not configured").to_string()),
        SettingsValueKind::Text { secret, .. } => {
            let value = row
                .value
                .as_ref()
                .and_then(Value::as_str)
                .unwrap_or_default();
            if value.is_empty() {
                tr(app, "未配置", "Not configured").to_string()
            } else if *secret {
                tr(app, "已配置", "Configured").to_string()
            } else {
                value.to_string()
            }
        }
    }
}

fn format_settings_page(app: &App, payload: &Value) -> String {
    let config = payload.get("config").unwrap_or(payload);
    let providers = config
        .pointer("/models/providers")
        .and_then(Value::as_object)
        .map_or(0, serde_json::Map::len);
    let models = config
        .pointer("/models/providers")
        .and_then(Value::as_object)
        .map(|providers| {
            providers
                .values()
                .filter_map(|provider| provider.get("models").and_then(Value::as_array))
                .map(Vec::len)
                .sum::<usize>()
        })
        .unwrap_or(0);
    let mcp_servers = config
        .get("mcpServers")
        .and_then(Value::as_object)
        .map_or(0, serde_json::Map::len);
    let primary = config
        .pointer("/agents/defaults/model/primary")
        .and_then(Value::as_str)
        .unwrap_or("not configured");
    let mut lines = vec![
        tr(
            app,
            "↑/↓ 选择 · Space 切换 · Enter 编辑 · R 刷新 · Ctrl+S 高级 Raw JSON",
            "↑/↓ select · Space toggle · Enter edit · R refresh · Ctrl+S advanced Raw JSON",
        )
        .to_string(),
        format!(
            "{}: {providers} · {}: {models} · MCP: {mcp_servers} · {}: {primary}",
            tr(app, "Provider", "Providers"),
            tr(app, "模型", "Models"),
            tr(app, "主 Agent", "Primary Agent"),
        ),
    ];
    let mut previous_section = None::<String>;
    for (index, row) in settings_rows(app, payload).into_iter().enumerate() {
        if previous_section.as_deref() != Some(row.section.as_str()) {
            lines.push(format!("\n{}", row.section));
            previous_section = Some(row.section.clone());
        }
        lines.push(format!(
            "{} {}: {}",
            selected_marker(index == app.inspector_index),
            row.label,
            settings_value_label(app, &row),
        ));
    }
    lines.join("\n")
}

fn format_loaded_page(app: &App, page: Page, payload: &Value) -> String {
    match page {
        Page::Models => format_models_page(app, payload),
        Page::Skills => format_skills_page(app, payload),
        Page::Mcp => format_mcp_page(app, payload),
        Page::Todos => format_todos_page(app, payload),
        Page::Usage => format_usage_page(app, payload),
        Page::Settings => format_settings_page(app, payload),
        _ => serde_json::to_string_pretty(payload).unwrap_or_default(),
    }
}

fn interactive_row_count(app: &App, page: Page, payload: &Value) -> usize {
    match page {
        Page::Models => payload
            .get("models")
            .and_then(Value::as_array)
            .map_or(0, Vec::len),
        Page::Skills => payload
            .get("skills")
            .and_then(Value::as_array)
            .map_or(0, Vec::len),
        Page::Mcp => {
            payload
                .get("servers")
                .and_then(Value::as_array)
                .map_or(0, Vec::len)
                + payload
                    .get("tools")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len)
        }
        Page::Todos => payload
            .get("items")
            .and_then(Value::as_array)
            .map_or(0, Vec::len),
        Page::Settings => settings_rows(app, payload).len(),
        _ => 0,
    }
}

fn reset_model_effort_choice(app: &mut App) {
    let Some(payload) = app.inspector_payload.as_ref() else {
        app.inspector_choice = 0;
        return;
    };
    let Some(model) = payload
        .get("models")
        .and_then(Value::as_array)
        .and_then(|models| models.get(app.inspector_index))
    else {
        app.inspector_choice = 0;
        return;
    };
    let current_model = payload
        .pointer("/session/model")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let model_ref = model.get("ref").and_then(Value::as_str).unwrap_or_default();
    let desired = if model_ref == current_model {
        payload.pointer("/session/effort").and_then(Value::as_str)
    } else {
        model.get("defaultEffort").and_then(Value::as_str)
    }
    .unwrap_or("off");
    app.inspector_choice = model
        .get("efforts")
        .and_then(Value::as_array)
        .and_then(|efforts| {
            efforts
                .iter()
                .position(|effort| effort.as_str() == Some(desired))
        })
        .unwrap_or(0);
}

fn refresh_interactive_inspector(app: &mut App) {
    if let Some(payload) = app.inspector_payload.as_ref() {
        app.inspector = format_loaded_page(app, app.page(), payload);
    }
}

fn apply_loaded_page(app: &mut App, page: Page, loaded: LoadedPage) {
    app.inspector_payload = loaded.payload;
    app.inspector_index = 0;
    app.inspector_choice = 0;
    if page == Page::Models
        && let Some(payload) = app.inspector_payload.as_ref()
        && let Some(current) = payload.pointer("/session/model").and_then(Value::as_str)
        && let Some(index) = payload
            .get("models")
            .and_then(Value::as_array)
            .and_then(|models| {
                models
                    .iter()
                    .position(|model| model.get("ref").and_then(Value::as_str) == Some(current))
            })
    {
        app.inspector_index = index;
        reset_model_effort_choice(app);
    }
    app.inspector = app
        .inspector_payload
        .as_ref()
        .map(|payload| format_loaded_page(app, page, payload))
        .unwrap_or(loaded.fallback);
}

async fn reload_page(client: &Client, base: &str, app: &mut App, page: Page) {
    let loaded = load_page(client, base, app, page).await;
    if app.page() == page {
        apply_loaded_page(app, page, loaded);
    }
}

async fn reload_current_page(client: &Client, base: &str, app: &mut App) {
    let page = app.page();
    reload_page(client, base, app, page).await;
}

async fn load_page(client: &Client, base: &str, app: &App, page: Page) -> LoadedPage {
    if app.active_group.is_some() && session_scoped_page(page) {
        return LoadedPage {
            payload: None,
            fallback: group_session_scope_error(app),
        };
    }
    let (endpoint, group) = match page {
        Page::Models => (Some("/api/session-models"), None),
        Page::Skills => (Some("/api/session-skills"), None),
        Page::Mcp => (Some("/api/mcp/catalog"), None),
        Page::Usage => (Some("/api/usage"), None),
        Page::Settings => (Some("/api/config"), None),
        Page::Groups if app.groups_enabled => match app.active_group.as_deref() {
            Some(group) => (Some("/api/session-group"), Some(group)),
            None => (Some("/api/session-groups"), None),
        },
        Page::Plan => {
            return LoadedPage {
                payload: None,
                fallback: app
                    .active_plan
                    .as_ref()
                    .map(|plan| serde_json::to_string_pretty(&plan.raw).unwrap_or_default())
                    .unwrap_or_else(|| tr(app, "当前没有活动计划", "No active plan").to_string()),
            };
        }
        Page::Todos => {
            return LoadedPage {
                payload: serde_json::from_str(&app.todos_snapshot).ok(),
                fallback: tr(app, "当前没有 Todo 数据", "No todo data is available").to_string(),
            };
        }
        Page::Chat | Page::Groups => {
            return LoadedPage {
                payload: None,
                fallback: String::new(),
            };
        }
    };
    let Some(endpoint) = endpoint else {
        return LoadedPage {
            payload: None,
            fallback: String::new(),
        };
    };
    let mut request = client
        .get(format!("{base}{endpoint}"))
        .query(&[("session", app.session.id.as_str())]);
    if let Some(group) = group {
        request = request.query(&[("group", group)]);
    }
    let response = request.send().await;
    match response {
        Ok(response) => match response.error_for_status() {
            Ok(response) => match response.json::<Value>().await {
                Ok(mut value) => {
                    if page == Page::Settings {
                        redact_secrets(&mut value);
                    }
                    LoadedPage {
                        payload: Some(value),
                        fallback: String::new(),
                    }
                }
                Err(error) => LoadedPage {
                    payload: None,
                    fallback: error.to_string(),
                },
            },
            Err(error) => LoadedPage {
                payload: None,
                fallback: error.to_string(),
            },
        },
        Err(error) => LoadedPage {
            payload: None,
            fallback: error.to_string(),
        },
    }
}

fn redact_secrets(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                if is_secret_key(key) {
                    *value = Value::String("••••••••".to_string());
                } else {
                    redact_secrets(value);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(redact_secrets),
        _ => {}
    }
}

fn is_secret_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    matches!(
        normalized.as_str(),
        "token"
            | "secret"
            | "password"
            | "authorization"
            | "credential"
            | "credentials"
            | "privatekey"
            | "accesskey"
    ) || normalized.ends_with("apikey")
        || normalized.ends_with("accesskey")
        || normalized.ends_with("secretkey")
        || normalized.ends_with("clientsecret")
        || normalized.ends_with("accesstoken")
        || normalized.ends_with("refreshtoken")
        || normalized.ends_with("sessiontoken")
        || normalized.ends_with("authtoken")
        || normalized.ends_with("bearertoken")
        || normalized.ends_with("password")
        || normalized.ends_with("credential")
        || normalized.ends_with("credentials")
        || normalized.ends_with("privatekey")
}

fn render(frame: &mut Frame<'_>, app: &mut App) {
    let area = frame.area();
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(6),
        ])
        .split(area);
    render_header(frame, app, root[0]);
    if area.width >= 120 {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(28),
                Constraint::Min(45),
                Constraint::Length(38),
            ])
            .split(root[1]);
        render_navigation(frame, app, columns[0]);
        render_content(frame, app, columns[1]);
        render_inspector(frame, app, columns[2]);
    } else if area.width >= 80 {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(26), Constraint::Min(40)])
            .split(root[1]);
        render_navigation(frame, app, columns[0]);
        if app.page() == Page::Chat {
            render_content(frame, app, columns[1]);
        } else {
            render_inspector(frame, app, columns[1]);
        }
    } else {
        match app.focus {
            Focus::Navigation => render_navigation(frame, app, root[1]),
            _ if app.page() == Page::Chat => render_content(frame, app, root[1]),
            _ => render_inspector(frame, app, root[1]),
        }
    }
    render_composer(frame, app, root[2]);
    if app.show_help {
        render_help(frame, app);
    }
    if app.show_commands {
        render_commands(frame, app);
    }
    if app.confirm_stale_plan {
        render_stale_plan_confirmation(frame, app);
    } else if app.pending_confirmation.is_some() {
        render_confirmation(frame, app);
    }
}

fn render_header(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let connection = if app.connected { "●" } else { "○" };
    let workspace = if app.active_group.is_none() {
        app.session
            .workspace
            .as_ref()
            .map(|workspace| {
                let availability = if workspace.available { "" } else { " !" };
                format!("{} · {}{}", workspace.kind, workspace.path, availability)
            })
            .unwrap_or_default()
    } else {
        String::new()
    };
    let target = active_target_name(app);
    let storage = if app.storage_writable {
        ""
    } else {
        tr(app, "  [存储保护]", "  [STORAGE PROTECTED]")
    };
    let title = format!(
        " LingClaw TUI  {}  {}  {}{} ",
        connection, target, workspace, storage
    );
    frame.render_widget(
        Paragraph::new(title)
            .style(
                Style::default()
                    .fg(app.text_color())
                    .add_modifier(Modifier::BOLD),
            )
            .block(
                Block::default()
                    .borders(Borders::BOTTOM)
                    .border_style(Style::default().fg(app.accent())),
            ),
        area,
    );
}

fn render_navigation(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let mut items = app
        .sessions
        .iter()
        .map(|session| {
            let availability = session
                .workspace
                .as_ref()
                .is_none_or(|workspace| workspace.available);
            ListItem::new(format!(
                "{}{}",
                if availability { "" } else { "! " },
                if session.name.is_empty() {
                    &session.id
                } else {
                    &session.name
                }
            ))
        })
        .collect::<Vec<_>>();
    if app.groups_enabled {
        items.extend(app.groups.iter().map(|group| {
            ListItem::new(format!(
                "# {} ({})",
                if group.name.is_empty() {
                    &group.id
                } else {
                    &group.name
                },
                group.members
            ))
        }));
    }
    let mut state =
        ListState::default().with_selected(Some(app.nav_index.min(items.len().saturating_sub(1))));
    let border = if app.focus == Focus::Navigation {
        app.accent()
    } else {
        Color::DarkGray
    };
    frame.render_stateful_widget(
        List::new(items)
            .block(
                Block::default()
                    .title(tr(app, "工作空间", "Workspaces"))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(border)),
            )
            .highlight_symbol("› ")
            .highlight_style(
                Style::default()
                    .fg(app.accent())
                    .add_modifier(Modifier::BOLD),
            ),
        area,
        &mut state,
    );
}

fn render_content(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let lines = app
        .lines
        .iter()
        .flat_map(|line| styled_chat_lines(app, line))
        .collect::<Vec<_>>();
    let border = if app.focus == Focus::Content {
        app.accent()
    } else {
        Color::DarkGray
    };
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .scroll((app.scroll, 0))
            .block(
                Block::default()
                    .title(app.page().label(app.language))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(border)),
            ),
        area,
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MarkdownFence {
    marker: char,
    length: usize,
}

fn markdown_fence(line: &str) -> Option<MarkdownFence> {
    let indentation = line
        .len()
        .saturating_sub(line.trim_start_matches(' ').len());
    if indentation > 3 {
        return None;
    }
    let trimmed = &line[indentation..];
    let marker = trimmed.chars().next()?;
    if !matches!(marker, '`' | '~') {
        return None;
    }
    let length = trimmed
        .chars()
        .take_while(|character| *character == marker)
        .count();
    (length >= 3).then_some(MarkdownFence { marker, length })
}

fn closes_markdown_fence(line: &str, fence: MarkdownFence) -> bool {
    let Some(candidate) = markdown_fence(line) else {
        return false;
    };
    if candidate.marker != fence.marker || candidate.length < fence.length {
        return false;
    }
    let trimmed = line.trim_start_matches(' ');
    trimmed
        .chars()
        .skip(candidate.length)
        .all(char::is_whitespace)
}

fn markdown_heading_text(line: &str) -> Option<&str> {
    let indentation = line
        .len()
        .saturating_sub(line.trim_start_matches(' ').len());
    if indentation > 3 {
        return None;
    }
    let trimmed = &line[indentation..];
    let marker_length = trimmed
        .chars()
        .take_while(|character| *character == '#')
        .count();
    if !(1..=6).contains(&marker_length) {
        return None;
    }
    let remainder = &trimmed[marker_length..];
    if !remainder.is_empty() && !remainder.chars().next().is_some_and(char::is_whitespace) {
        return None;
    }
    Some(remainder.trim_start())
}

fn styled_chat_lines<'a>(app: &App, line: &'a ChatLine) -> Vec<Line<'a>> {
    let color = match line.style {
        LineKind::User => app.accent(),
        LineKind::Assistant => app.text_color(),
        LineKind::Reasoning => Color::DarkGray,
        LineKind::Tool => Color::Cyan,
        LineKind::System => Color::Yellow,
        LineKind::Error => Color::Red,
    };
    let mut lines = vec![Line::from(Span::styled(
        format!("{} ›", line.role),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    ))];
    let mut active_fence = None;
    for text in line.content.lines() {
        let (content, style) = if let Some(fence) = active_fence {
            if closes_markdown_fence(text, fence) {
                active_fence = None;
                (text, Style::default().fg(Color::Cyan))
            } else {
                // Fenced code is opaque Markdown content. In particular, keep
                // language syntax such as `#include` and shell comments exact.
                (text, Style::default().fg(Color::Cyan))
            }
        } else if let Some(fence) = markdown_fence(text) {
            active_fence = Some(fence);
            (text, Style::default().fg(Color::Cyan))
        } else if let Some(heading) = markdown_heading_text(text) {
            (
                heading,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            )
        } else if text.starts_with('>') {
            (
                text,
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )
        } else {
            (text, Style::default().fg(app.text_color()))
        };
        lines.push(Line::from(Span::styled(content, style)));
    }
    lines.push(Line::default());
    lines
}

fn render_inspector(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    let content = match app.page() {
        Page::Plan => app
            .active_plan
            .as_ref()
            .map(|plan| {
                let stale = app.plan_stale.as_ref().map_or_else(String::new, |stale| {
                    let paths = if stale.paths.is_empty() {
                        tr(app, "无具体路径", "no specific paths").to_string()
                    } else {
                        stale.paths.join("\n- ")
                    };
                    format!(
                        "\n\n{}{}\n- {}",
                        tr(app, "过期证据", "Stale evidence"),
                        if stale.evidence_incomplete {
                            tr(app, "（记录不完整）", " (incomplete record)")
                        } else {
                            ""
                        },
                        paths
                    )
                });
                format!(
                    "{}\nstatus: {}\nrevision: {}\nid: {}\n\n{}{}",
                    plan.title,
                    plan.status,
                    plan.revision,
                    plan.id,
                    serde_json::to_string_pretty(&plan.raw).unwrap_or_default(),
                    stale
                )
            })
            .unwrap_or_else(|| tr(app, "当前没有活动计划", "No active plan").to_string()),
        _ => app.inspector.clone(),
    };
    let block = Block::default()
        .title(app.page().label(app.language))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    #[cfg(feature = "tui-images")]
    if app.page() == Page::Chat
        && let Some(preview) = app.image_preview.as_mut()
    {
        let sections = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(68), Constraint::Percentage(32)])
            .split(inner);
        frame.render_stateful_widget(StatefulImage::default(), sections[0], preview);
        frame.render_widget(
            Paragraph::new(content)
                .wrap(Wrap { trim: false })
                .scroll((app.inspector_scroll, 0)),
            sections[1],
        );
        return;
    }
    frame.render_widget(
        Paragraph::new(content)
            .wrap(Wrap { trim: false })
            .scroll((app.inspector_scroll, 0)),
        inner,
    );
}

fn render_composer(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let mode = if app.plan_mode {
        tr(app, " · 规划", " · Plan")
    } else {
        ""
    };
    let group_target = if app.active_group.is_some() {
        if app.group_targets.is_empty() {
            format!(" · {}", app.group_target_mode)
        } else {
            format!(" · {}:{}", app.group_target_mode, app.group_targets.len())
        }
    } else {
        String::new()
    };
    let status = if !app.storage_writable {
        tr(
            app,
            "存储保护模式 · 仅保留读取与配置保存 · 修复后重启",
            "Storage protected · reads and config saves only · repair and restart",
        )
    } else if app.status.is_empty() {
        tr(
            app,
            "Enter 发送 · Alt+Enter 换行 · Ctrl+P 命令",
            "Enter send · Alt+Enter newline · Ctrl+P commands",
        )
    } else {
        &app.status
    };
    let attachments = if app.pending_images.is_empty() {
        String::new()
    } else {
        format!(" · 🖼 {}", app.pending_images.len())
    };
    let model = if app.active_group.is_none() && !app.current_model.is_empty() {
        if app.current_effort.is_empty() {
            format!(" · {}", app.current_model)
        } else {
            format!(" · {} · {}", app.current_model, app.current_effort)
        }
    } else {
        String::new()
    };
    let title = format!(
        " {}{}{}{}{} ",
        active_target_name(app),
        model,
        mode,
        group_target,
        attachments
    );
    let text = format!("{}\n{}", app.input, status);
    let border = if app.focus == Focus::Composer {
        app.accent()
    } else {
        Color::DarkGray
    };
    frame.render_widget(
        Paragraph::new(text).wrap(Wrap { trim: false }).block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border)),
        ),
        area,
    );
}

fn render_help(frame: &mut Frame<'_>, app: &App) {
    let area = centered_rect(64, 70, frame.area());
    frame.render_widget(Clear, area);
    let help = tr(
        app,
        "Tab / Shift+Tab  切换焦点\nEnter              发送 / 确认\nAlt+Enter / Ctrl+J 插入换行\nAlt+P              选择规划模式\n模型页 ↑/↓ ←/→     选择模型 / Effort，Enter 应用\nSkill/MCP 页 Space 切换当前权限\nTodo 页 Space/Del/A 更新/删除/新增\n设置页 Space/↵     切换或编辑当前字段\n数据页 R           重新载入当前页面\n/attach PATH       附加本地图片\n/session …         新建/查找/切换/重命名/重绑/删除\n/group …           新建/切换/成员/管理员/删除\n/mcp oauth NAME    连接 MCP OAuth\n计划页 E/R/F/D     执行/恢复/刷新/丢弃\n计划页 V/X         修订/确认按旧证据执行\nCtrl+P             命令面板\nCtrl+S             高级 Raw JSON 编辑器\nCtrl+C             停止 / 退出\nO                  打开最近图片\n? / Esc            帮助 / 关闭",
        "Tab / Shift+Tab  Change focus\nEnter              Send / confirm\nAlt+Enter / Ctrl+J Insert newline\nAlt+P              Select Plan mode\nModels ↑/↓ ←/→     Select model / Effort, Enter applies\nSkills/MCP Space   Toggle the selected permission\nTodos Space/Del/A  Cycle/remove/add items\nSettings Space/↵   Toggle or edit the selected field\nData pages R       Reload the current page\n/attach PATH       Attach a local image\n/session …         Create/find/switch/rename/rebind/delete\n/group …           Create/switch/members/admin/delete\n/mcp oauth NAME    Connect MCP OAuth\nPlan E/R/F/D       Execute/resume/refresh/discard\nPlan V/X           Revise/confirm stale execution\nCtrl+P             Command palette\nCtrl+S             Advanced Raw JSON editor\nCtrl+C             Stop / exit\nO                  Open latest image\n? / Esc            Help / close",
    );
    frame.render_widget(
        Paragraph::new(help).alignment(Alignment::Left).block(
            Block::default()
                .title(tr(app, "快捷键", "Keyboard"))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(app.accent())),
        ),
        area,
    );
}

fn render_commands(frame: &mut Frame<'_>, app: &App) {
    let area = centered_rect(62, 72, frame.area());
    frame.render_widget(Clear, area);
    let items = app
        .pages
        .iter()
        .enumerate()
        .map(|(index, page)| {
            Line::from(vec![
                Span::styled(
                    format!("{}  ", index + 1),
                    Style::default()
                        .fg(app.accent())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(page.label(app.language)),
            ])
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(items).block(
            Block::default()
                .title(tr(
                    app,
                    "命令面板 · P 规划 · E 配置 · O 图片 · 输入 /session 或 /group 管理",
                    "Commands · P Plan · E config · O image · type /session or /group to manage",
                ))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(app.accent())),
        ),
        area,
    );
}

fn render_confirmation(frame: &mut Frame<'_>, app: &App) {
    let Some(confirmation) = app.pending_confirmation.as_ref() else {
        return;
    };
    let area = centered_rect(62, 30, frame.area());
    frame.render_widget(Clear, area);
    let text = format!(
        "{}\n\n{}",
        confirmation.prompt,
        tr(
            app,
            "Enter / Y 确认 · Esc / N 取消",
            "Enter / Y confirm · Esc / N cancel"
        )
    );
    frame.render_widget(
        Paragraph::new(text)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .title(tr(app, "确认危险操作", "Confirm destructive action"))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Red)),
            ),
        area,
    );
}

fn render_stale_plan_confirmation(frame: &mut Frame<'_>, app: &App) {
    let Some(stale) = app.plan_stale.as_ref() else {
        return;
    };
    let area = centered_rect(68, 36, frame.area());
    frame.render_widget(Clear, area);
    let scope = if stale.paths.is_empty() {
        tr(
            app,
            "计划读取过的证据无法完整重新验证。",
            "The evidence read by this plan could not be fully revalidated.",
        )
        .to_string()
    } else {
        format!(
            "{}\n- {}",
            tr(app, "以下路径已变化：", "These paths changed:"),
            stale.paths.join("\n- ")
        )
    };
    let text = format!(
        "{}\n\n{}\n\n{}",
        scope,
        tr(
            app,
            "仍然执行会明确记录这次覆盖决定。",
            "Executing anyway records this override explicitly."
        ),
        tr(
            app,
            "Enter / Y 仍然执行 · Esc / N 取消",
            "Enter / Y execute anyway · Esc / N cancel"
        )
    );
    frame.render_widget(
        Paragraph::new(text)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .title(tr(app, "确认过期计划", "Confirm stale plan"))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Yellow)),
            ),
        area,
    );
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

fn tr<'a>(app: &App, zh: &'a str, en: &'a str) -> &'a str {
    tr_raw(app.language, zh, en)
}

fn tr_raw<'a>(language: UiLanguage, zh: &'a str, en: &'a str) -> &'a str {
    match language {
        UiLanguage::ZhCn => zh,
        UiLanguage::En => en,
    }
}

/// Split a user-owned `$VISUAL`/`$EDITOR` command without invoking a shell.
/// This accepts quoted executable paths and fixed arguments such as
/// `"C:\\Program Files\\Editor\\editor.exe" --wait` while keeping the
/// temporary configuration path as a separate, non-interpreted argument.
fn split_command_line(value: &str) -> TuiResult<Vec<String>> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut characters = value.trim().chars().peekable();
    while let Some(character) = characters.next() {
        match (character, quote) {
            ('\\', Some('"')) if matches!(characters.peek().copied(), Some('"' | '\\')) => {
                current.push(characters.next().expect("peeked character must exist"));
            }
            ('"' | '\'', None) => quote = Some(character),
            (character, Some(expected)) if character == expected => quote = None,
            (character, None) if character.is_whitespace() => {
                if !current.is_empty() {
                    parts.push(std::mem::take(&mut current));
                }
            }
            (character, _) => current.push(character),
        }
    }
    if quote.is_some() {
        return Err("VISUAL/EDITOR contains an unterminated quote".into());
    }
    if !current.is_empty() {
        parts.push(current);
    }
    if parts.is_empty() {
        return Err("VISUAL/EDITOR does not contain an executable".into());
    }
    Ok(parts)
}

#[cfg(feature = "tui-images")]
fn detect_image_support() -> (String, Option<Picker>) {
    match Picker::from_query_stdio() {
        Ok(picker) => {
            let label = match picker.protocol_type() {
                ProtocolType::Kitty => "Kitty",
                ProtocolType::Sixel => "Sixel",
                ProtocolType::Iterm2 => "iTerm2",
                ProtocolType::Halfblocks => "half-block fallback",
            };
            (label.to_string(), Some(picker))
        }
        Err(_) => ("link fallback".to_string(), None),
    }
}

#[cfg(not(feature = "tui-images"))]
fn detect_image_support() -> (String, Option<()>) {
    (detect_image_protocol_from_env(), None)
}

#[cfg(not(feature = "tui-images"))]
fn detect_image_protocol_from_env() -> String {
    // No-default-feature builds retain a link/viewer fallback without the
    // optional image decoder stack.
    if std::env::var_os("KITTY_WINDOW_ID").is_some() {
        "Kitty".to_string()
    } else if std::env::var("TERM_PROGRAM").is_ok_and(|value| value == "iTerm.app") {
        "iTerm2".to_string()
    } else if std::env::var("TERM").is_ok_and(|value| value.to_ascii_lowercase().contains("sixel"))
    {
        "Sixel".to_string()
    } else {
        "link fallback".to_string()
    }
}

#[cfg(windows)]
fn open_external(target: &str) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL};

    if target.contains('\0') {
        return Err("external viewer target contains a NUL byte".to_string());
    }
    let target = std::ffi::OsStr::new(target)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: `target` is a NUL-terminated UTF-16 string retained for the call;
    // the remaining null arguments request the target's registered open verb.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            std::ptr::null(),
            target.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    let code = result as isize;
    (code > 32)
        .then_some(())
        .ok_or_else(|| format!("cannot open external viewer (ShellExecuteW code {code})"))
}

#[cfg(target_os = "macos")]
fn open_external(target: &str) -> Result<(), String> {
    open_external_with_command("open", target)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn open_external(target: &str) -> Result<(), String> {
    open_external_with_command("xdg-open", target)
}

#[cfg(unix)]
fn open_external_with_command(program: &str, target: &str) -> Result<(), String> {
    let status = Command::new(program)
        .arg(target)
        .status()
        .map_err(|error| format!("cannot open external viewer: {error}"))?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| format!("external viewer exited with status {status}"))
}

#[cfg(test)]
#[path = "tests/tui_tests.rs"]
mod tests;
