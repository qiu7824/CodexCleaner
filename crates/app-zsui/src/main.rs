#![cfg_attr(windows, windows_subsystem = "windows")]

mod windows_startup;

use std::{
    env, fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use chrono::Utc;
use cleaner_core::{
    analyze_session, apply_retention_profile, apply_safe_storage_rules, build_cleanup_plan,
    codex_process_running, discover_codex_binary, discover_codex_home,
    enrich_session_titles_official, execute_cleanup_plan, execute_storage_cleanup, format_bytes,
    probe_app_server, read_thread_official, scan_codex_home, scan_codex_storage, AnalysisOptions,
    ArtifactStage, CleanupReceipt, CleanupStatus, ResourceAction, ResourceKind, RetentionProfile,
    ScanReport, SessionAnalysis, SessionStatus, StorageAction, StorageCleanupReceipt,
    StorageReport, StorageSafety,
};
use zsui::*;

use windows_startup::set_app_identity;

const SESSION_TABLE: WidgetId = WidgetId::new(100);
const RESOURCE_TABLE: WidgetId = WidgetId::new(200);
const STORAGE_TABLE: WidgetId = WidgetId::new(250);
const EXECUTE_DIALOG: WidgetId = WidgetId::new(300);
const ANALYZE_BUTTON: WidgetId = WidgetId::new(400);
const NAV_STORAGE_BUTTON: WidgetId = WidgetId::new(401);
const NAV_CONVERSATIONS_BUTTON: WidgetId = WidgetId::new(402);
const NAV_OVERVIEW_BUTTON: WidgetId = WidgetId::new(414);
const NAV_HISTORY_BUTTON: WidgetId = WidgetId::new(415);
const NAV_SETTINGS_BUTTON: WidgetId = WidgetId::new(416);
const NAVIGATION_VIEW: WidgetId = WidgetId::new(417);
const REFRESH_SESSIONS_BUTTON: WidgetId = WidgetId::new(403);
const RESULTS_ONLY_BUTTON: WidgetId = WidgetId::new(404);
const RESULTS_SOURCE_BUTTON: WidgetId = WidgetId::new(405);
const DEVELOPMENT_BUTTON: WidgetId = WidgetId::new(406);
const CONVERSATION_ONLY_BUTTON: WidgetId = WidgetId::new(407);
const PREVIEW_BUTTON: WidgetId = WidgetId::new(408);
const EXECUTE_BUTTON: WidgetId = WidgetId::new(409);
const STORAGE_REFRESH_BUTTON: WidgetId = WidgetId::new(410);
const STORAGE_SAFE_RULES_BUTTON: WidgetId = WidgetId::new(411);
const STORAGE_PREVIEW_BUTTON: WidgetId = WidgetId::new(412);
const STORAGE_EXECUTE_BUTTON: WidgetId = WidgetId::new(413);
const TASK_ALL_BUTTON: WidgetId = WidgetId::new(420);
const TASK_ACTIVE_BUTTON: WidgetId = WidgetId::new(421);
const TASK_ARCHIVED_BUTTON: WidgetId = WidgetId::new(422);
const TASK_LOCAL_BUTTON: WidgetId = WidgetId::new(423);
const TASK_DUPLICATE_BUTTON: WidgetId = WidgetId::new(424);
const TASK_PROJECT_BUTTON: WidgetId = WidgetId::new(425);
const TASK_CHILDREN_BUTTON: WidgetId = WidgetId::new(426);
const TASK_PREVIOUS_BUTTON: WidgetId = WidgetId::new(430);
const TASK_NEXT_BUTTON: WidgetId = WidgetId::new(431);
const RESOURCE_PREVIOUS_BUTTON: WidgetId = WidgetId::new(432);
const RESOURCE_NEXT_BUTTON: WidgetId = WidgetId::new(433);
const STORAGE_PREVIOUS_BUTTON: WidgetId = WidgetId::new(434);
const STORAGE_NEXT_BUTTON: WidgetId = WidgetId::new(435);
const OVERVIEW_TABLE: WidgetId = WidgetId::new(436);
const HISTORY_TABLE: WidgetId = WidgetId::new(437);
const START_SCAN_BUTTON: WidgetId = WidgetId::new(438);
const OVERVIEW_OPEN_BUTTON: WidgetId = WidgetId::new(439);
const OVERVIEW_ANALYZE_BUTTON: WidgetId = WidgetId::new(440);
const HISTORY_REFRESH_BUTTON: WidgetId = WidgetId::new(441);
const SETTINGS_TASK_SCAN_BUTTON: WidgetId = WidgetId::new(442);
const SETTINGS_STORAGE_SCAN_BUTTON: WidgetId = WidgetId::new(443);
const HISTORY_PREVIOUS_BUTTON: WidgetId = WidgetId::new(444);
const HISTORY_NEXT_BUTTON: WidgetId = WidgetId::new(445);
const CONTENT_SCROLL: WidgetId = WidgetId::new(446);
const BACK_TO_TASKS_BUTTON: WidgetId = WidgetId::new(447);
const HOME_TASKS_BUTTON: WidgetId = WidgetId::new(448);
const HOME_STORAGE_BUTTON: WidgetId = WidgetId::new(449);
const RESOURCE_DETAIL_DIALOG: WidgetId = WidgetId::new(450);
const STATUS_INFO_BAR: WidgetId = WidgetId::new(451);

const TASKS_PER_PAGE: usize = 5;
const RESOURCES_PER_PAGE: usize = 6;
const STORAGE_ITEMS_PER_PAGE: usize = 5;
const HISTORY_ITEMS_PER_PAGE: usize = 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Overview,
    Conversations,
    Storage,
    History,
    Settings,
}

impl Page {
    const fn label(self) -> &'static str {
        match self {
            Self::Overview => "首页",
            Self::Conversations => "任务清理",
            Self::Storage => "空间清理",
            Self::History => "清理记录",
            Self::Settings => "设置",
        }
    }

    const fn description(self) -> &'static str {
        match self {
            Self::Overview => "从这里选择要清理的内容",
            Self::Conversations => "选择任务、核对保留内容，然后执行清理",
            Self::Storage => "查找 Codex 缓存、备份和更新残留",
            Self::History => "查看永久任务删除和回收站清理的执行结果",
            Self::Settings => "调整外观，并查看扫描与安全边界",
        }
    }

    const fn icon(self) -> ZsIcon {
        match self {
            Self::Overview => ZsIcon::App,
            Self::Conversations => ZsIcon::Group,
            Self::Storage => ZsIcon::Folder,
            Self::History => ZsIcon::History,
            Self::Settings => ZsIcon::Settings,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExecuteKind {
    Conversation,
    Storage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskFilter {
    All,
    Active,
    Archived,
    Local,
    Children,
    Duplicates,
    SameProject,
}

impl TaskFilter {
    const fn label(self) -> &'static str {
        match self {
            Self::All => "主任务",
            Self::Active => "活跃",
            Self::Archived => "已归档",
            Self::Local => "无本地文件",
            Self::Children => "子任务",
            Self::Duplicates => "同名任务",
            Self::SameProject => "同一目录",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResourceFilter {
    Cleanup,
    Keep,
    Decide,
    Storage,
    All,
}

impl ResourceFilter {
    const fn label(self) -> &'static str {
        match self {
            Self::Cleanup => "已选清理",
            Self::Keep => "已选保留",
            Self::Decide => "需要决定",
            Self::Storage => "不可在此清理",
            Self::All => "全部资源",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StorageFilter {
    Recommended,
    Review,
    Protected,
    Selected,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackgroundKind {
    FullScan,
    Sessions,
    Storage,
    History,
    Analysis,
    ConversationCleanup,
    StorageCleanup,
}

impl BackgroundKind {
    const fn label(self) -> &'static str {
        match self {
            Self::FullScan => "全面扫描",
            Self::Sessions => "刷新任务",
            Self::Storage => "扫描存储",
            Self::History => "读取记录",
            Self::Analysis => "分析任务",
            Self::ConversationCleanup => "执行任务清理",
            Self::StorageCleanup => "执行存储清理",
        }
    }
}

#[derive(Debug, Clone)]
enum BackgroundResult {
    FullScan(Result<FullScanResult, String>),
    Sessions(Result<(ScanReport, usize), String>),
    Storage(StorageReport),
    History(Vec<HistoryEntry>),
    Analysis(Result<SessionAnalysis, String>),
    ConversationCleanup(Result<CleanupReceipt, String>),
    StorageCleanup(Result<StorageCleanupReceipt, String>),
}

#[derive(Debug, Clone, Default)]
struct BackgroundState {
    kind: Option<BackgroundKind>,
    percent: u8,
    stage: String,
    running: bool,
    result: Option<BackgroundResult>,
}

#[derive(Debug, Clone)]
struct HistoryEntry {
    id: u64,
    occurred_at: String,
    kind: String,
    result: String,
    summary: String,
    detail: String,
    journal_path: PathBuf,
    recycled_count: usize,
    failed_count: usize,
    permanent_thread_deleted: bool,
}

#[derive(Debug, Clone, Copy)]
struct DriveUsage {
    used_bytes: u64,
    total_bytes: u64,
}

#[derive(Debug, Clone)]
struct FullScanResult {
    report: ScanReport,
    storage: StorageReport,
    official_count: usize,
    drive_usage: Option<DriveUsage>,
    history: Vec<HistoryEntry>,
}

impl StorageFilter {
    const fn label(self) -> &'static str {
        match self {
            Self::Recommended => "推荐清理",
            Self::Review => "人工判断",
            Self::Protected => "受保护",
            Self::Selected => "已选清理",
            Self::All => "全部",
        }
    }
}

#[derive(Debug, Clone)]
enum Msg {
    Navigate(Page),
    StartFullScan,
    OverviewSelected(ZsTableRowId),
    OverviewOpenSelected,
    OverviewAnalyzeSelected,
    RefreshSessions,
    TaskFilterSelected(TaskFilter),
    SessionSelected(ZsTableRowId),
    SessionInvoked(ZsTableRowId),
    SessionSorted(ZsTableSort),
    TaskPreviousPage,
    TaskNextPage,
    ResourceSelected(ZsTableRowId),
    ResourceFilterSelected(ResourceFilter),
    ResourcePreviousPage,
    ResourceNextPage,
    ShowResourceDetail,
    ResourceDetailClosed(ZsContentDialogResult),
    AnalyzeSelected,
    BackToTaskSelection,
    ResultsOnly,
    ResultsAndSource,
    DevelopmentEnvironment,
    ConversationOnly,
    KeepSelected,
    DeleteSelected,
    ReviewSelected,
    Preview,
    ExecuteRequested,
    StorageRefresh,
    StorageSelected(ZsTableRowId),
    StorageFilterSelected(StorageFilter),
    StoragePreviousPage,
    StorageNextPage,
    StorageApplySafeRules,
    StorageKeepSelected,
    StorageCleanSelected,
    StorageReviewSelected,
    StoragePreview,
    StorageExecuteRequested,
    HistorySelected(ZsTableRowId),
    HistoryPreviousPage,
    HistoryNextPage,
    RefreshHistory,
    DarkModeChanged(bool),
    ExecuteDialogResult(ZsContentDialogResult),
}

#[derive(Clone)]
struct AppState {
    page: Page,
    dark_mode: bool,
    report: ScanReport,
    storage: StorageReport,
    drive_usage: Option<DriveUsage>,
    overview_selected_session_id: Option<String>,
    selected_session: Option<ZsTableRowId>,
    task_filter: TaskFilter,
    project_anchor: Option<String>,
    session_sort: Option<ZsTableSort>,
    task_page: usize,
    selected_resource: Option<ZsTableRowId>,
    resource_filter: ResourceFilter,
    resource_page: usize,
    selected_storage: Option<ZsTableRowId>,
    storage_filter: StorageFilter,
    storage_page: usize,
    history: Vec<HistoryEntry>,
    selected_history: Option<ZsTableRowId>,
    history_page: usize,
    analysis: Option<SessionAnalysis>,
    status: String,
    codex_binary: Option<PathBuf>,
    execute_dialog: Option<ExecuteKind>,
    show_resource_detail: bool,
    background: Arc<Mutex<BackgroundState>>,
}

fn main() {
    if let Err(error) = run_app() {
        windows_startup::show_fatal_error(&format!(
            "Codex Cleaner 无法启动。\n\n{error}\n\n请确认 Codex 数据目录可读取，然后重试。"
        ));
    }
}

fn run_app() -> Result<(), Box<dyn std::error::Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    set_app_identity();
    let smoke_mode = args.iter().any(|value| value == "--smoke");
    let explicit_home = argument_value(&args, "--codex-home").map(PathBuf::from);
    let home = discover_codex_home(explicit_home);
    let explicit_binary = argument_value(&args, "--codex-bin").map(PathBuf::from);
    let codex_binary = discover_codex_binary(&home, explicit_binary);

    if args.iter().any(|value| value == "--probe-official") {
        let binary = codex_binary.ok_or("未找到 Codex CLI")?;
        probe_app_server(&binary, Duration::from_secs(20))?;
        println!(
            "{}",
            serde_json::json!({ "available": true, "codex_binary": binary })
        );
        return Ok(());
    }
    if let Some(thread_id) = argument_value(&args, "--probe-thread") {
        let binary = codex_binary.ok_or("未找到 Codex CLI")?;
        let response = read_thread_official(&binary, thread_id, Duration::from_secs(20))?;
        let returned_id = response
            .result
            .get("thread")
            .and_then(|value| value.get("id"))
            .and_then(serde_json::Value::as_str);
        println!(
            "{}",
            serde_json::json!({ "found": returned_id == Some(thread_id) })
        );
        return Ok(());
    }

    let needs_report = smoke_mode
        || args.iter().any(|value| value == "--scan-json")
        || argument_value(&args, "--analyze").is_some();
    let mut report = if needs_report {
        scan_codex_home(&home)?
    } else {
        ScanReport {
            codex_home: home.clone(),
            sessions: Vec::new(),
            transcript_bytes: 0,
            malformed_index_lines: 0,
            warnings: Vec::new(),
        }
    };
    if needs_report && !smoke_mode {
        if let Some(binary) = codex_binary.as_ref() {
            let _ = enrich_session_titles_official(&mut report, binary, Duration::from_secs(12));
        }
    }
    if args.iter().any(|value| value == "--scan-json") {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    if let Some(session_id) = argument_value(&args, "--analyze") {
        let analysis = analyze_session(&report, session_id, AnalysisOptions::default())?;
        println!("{}", serde_json::to_string_pretty(&analysis)?);
        return Ok(());
    }
    let storage_json = args.iter().any(|value| value == "--storage-json");
    let storage = if smoke_mode || storage_json {
        scan_codex_storage(&home)
    } else {
        StorageReport {
            roots: vec![home.clone()],
            items: Vec::new(),
            total_bytes: 0,
            warnings: Vec::new(),
        }
    };
    if storage_json {
        println!("{}", serde_json::to_string_pretty(&storage)?);
        return Ok(());
    }
    if let Some(smoke_session_id) = argument_value(&args, "--smoke-session") {
        if let Some(index) = report
            .sessions
            .iter()
            .position(|session| session.id == smoke_session_id)
        {
            let session = report.sessions.remove(index);
            report.sessions.insert(0, session);
        }
    }
    let selected_session_row = argument_value(&args, "--smoke-session")
        .and_then(|id| {
            report
                .sessions
                .iter()
                .filter(|session| session.parent_id.is_none())
                .position(|session| session.id == id)
        })
        .map(|index| ZsTableRowId::new((index + 1) as u64))
        .or_else(|| (!report.sessions.is_empty()).then(|| ZsTableRowId::new(1)));
    let overview_selected_session_id = overview_session_indices(&report)
        .first()
        .map(|index| report.sessions[*index].id.clone());
    let selected_storage = storage
        .items
        .iter()
        .find(|item| storage_item_matches_filter(item, StorageFilter::Recommended))
        .map(|item| ZsTableRowId::new(item.id));
    let history = load_cleanup_history(&report);
    let selected_history = history.first().map(|entry| ZsTableRowId::new(entry.id));
    let mut state = AppState {
        page: if args.iter().any(|value| value == "--smoke-storage") {
            Page::Storage
        } else if args.iter().any(|value| {
            value == "--smoke-conversations"
                || value == "--smoke-actions"
                || value == "--smoke-analyzed"
        }) {
            Page::Conversations
        } else if args.iter().any(|value| value == "--smoke-history") {
            Page::History
        } else if args.iter().any(|value| value == "--smoke-settings") {
            Page::Settings
        } else {
            Page::Overview
        },
        dark_mode: if smoke_mode { false } else { load_dark_mode() },
        status: format!(
            "已盘点 {}，其中安全候选 {}；共识别 {} 个任务",
            format_bytes(storage.total_bytes),
            format_bytes(storage.safe_candidate_bytes()),
            report.sessions.len()
        ),
        report,
        storage,
        drive_usage: system_drive_usage(),
        overview_selected_session_id,
        selected_session: selected_session_row,
        task_filter: TaskFilter::All,
        project_anchor: None,
        session_sort: None,
        task_page: 0,
        selected_resource: None,
        resource_filter: ResourceFilter::All,
        resource_page: 0,
        selected_storage,
        storage_filter: StorageFilter::Recommended,
        storage_page: 0,
        history,
        selected_history,
        history_page: 0,
        analysis: None,
        codex_binary,
        execute_dialog: None,
        show_resource_detail: false,
        background: Arc::new(Mutex::new(BackgroundState::default())),
    };
    if args.iter().any(|value| value == "--smoke-analyzed") {
        let analysis = selected_session(&state)
            .map(|session| session.id.clone())
            .ok_or_else(|| "没有可供烟雾验收的任务".to_string())
            .and_then(|session_id| {
                analyze_session(&state.report, &session_id, AnalysisOptions::default())
            });
        apply_background_result(&mut state, BackgroundResult::Analysis(analysis));
    }
    if !smoke_mode {
        start_full_scan(&mut state);
    }

    let icon_path = embedded_icon_path()?;
    let initial_size = if args.iter().any(|value| value == "--smoke-small") {
        (1060, 860)
    } else {
        (1360, 860)
    };
    let builder = native_window("Codex Cleaner · 存储与任务清理")
        .size(initial_size.0, initial_size.1)
        .min_size(1060, 860)
        .icon_path(icon_path.to_string_lossy())
        .stateful_view(state, view, update);

    if args.iter().any(|value| value == "--smoke") {
        let artifact_dir = argument_value(&args, "--output")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("artifacts/smoke"));
        fs::create_dir_all(&artifact_dir)?;
        let mut smoke_options = NativeWindowSmokeRunOptions::new(5000)
            .screenshot_file(artifact_dir.join("window.png").to_string_lossy())
            .require_screenshot(true);
        if args.iter().any(|value| value == "--smoke-actions") {
            smoke_options = NativeWindowSmokeRunOptions::new(45000)
                .screenshot_file(artifact_dir.join("window.png").to_string_lossy())
                .require_screenshot(true)
                .native_view_clicks([Point { x: 860, y: 428 }]);
        } else if args.iter().any(|value| value == "--smoke-project-filter") {
            smoke_options = NativeWindowSmokeRunOptions::new(7000)
                .screenshot_file(artifact_dir.join("window.png").to_string_lossy())
                .require_screenshot(true)
                .native_view_clicks([Point { x: 324, y: 252 }]);
        }
        let smoke_report = builder.run_smoke(smoke_options)?;
        fs::write(
            artifact_dir.join("report.json"),
            serde_json::to_vec_pretty(&smoke_report)?,
        )?;
        return Ok(());
    }

    builder.run()?;
    Ok(())
}

fn embedded_icon_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    const ICON: &[u8] = include_bytes!("../../../assets/codex-cleaner.ico");
    let root = env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir)
        .join("CodexCleaner")
        .join("assets");
    fs::create_dir_all(&root)?;
    let path = root.join("codex-cleaner.ico");
    let should_write = fs::metadata(&path)
        .map(|metadata| metadata.len() != ICON.len() as u64)
        .unwrap_or(true);
    if should_write {
        fs::write(&path, ICON)?;
    }
    Ok(path)
}

fn view(state: &AppState) -> ViewNode<Msg> {
    let mut snapshot = state.clone();
    apply_background_preview(&mut snapshot);
    render_view(&snapshot)
}

fn role_text(value: impl Into<String>, role: TextRole) -> ViewNode<Msg> {
    styled_text(value, SemanticTextStyle::for_role(role))
}

fn body_strong(value: impl Into<String>) -> ViewNode<Msg> {
    let mut style = SemanticTextStyle::body();
    style.weight = TextWeight::Semibold;
    style.vertical_align = VerticalAlign::Start;
    style.wrap = TextWrap::Word;
    style.ellipsis = false;
    styled_text(value, style)
}

fn body_text(value: impl Into<String>) -> ViewNode<Msg> {
    let mut style = SemanticTextStyle::body();
    style.vertical_align = VerticalAlign::Start;
    style.wrap = TextWrap::Word;
    style.ellipsis = false;
    styled_text(value, style)
}

fn secondary_text(value: impl Into<String>, role: TextRole) -> ViewNode<Msg> {
    let mut style = SemanticTextStyle::for_role(role);
    style.color = ColorRole::SecondaryText;
    style.wrap = TextWrap::Word;
    style.ellipsis = false;
    styled_text(value, style)
}

fn status_text(value: impl Into<String>) -> ViewNode<Msg> {
    let mut style = SemanticTextStyle::for_role(TextRole::Caption);
    style.color = ColorRole::SecondaryText;
    style.vertical_align = VerticalAlign::Start;
    style.wrap = TextWrap::Word;
    style.ellipsis = false;
    styled_text(value, style)
}

fn status_bar_text(value: impl Into<String>) -> ViewNode<Msg> {
    let mut style = SemanticTextStyle::for_role(TextRole::Caption);
    style.color = ColorRole::SecondaryText;
    style.vertical_align = VerticalAlign::Center;
    style.wrap = TextWrap::NoWrap;
    style.ellipsis = true;
    styled_text(value, style)
}

fn render_view(state: &AppState) -> ViewNode<Msg> {
    let body = match state.page {
        Page::Overview => view_overview(state),
        Page::Conversations => view_conversations(state),
        Page::Storage => view_storage(state),
        Page::History => view_history(state),
        Page::Settings => view_settings(state),
    };
    let status = info_bar(
        STATUS_INFO_BAR,
        ZsInfoBarSpec::new(&state.status)
            .title("当前状态")
            .severity(ZsInfoBarSeverity::Informational)
            .closable(false),
    );
    let header = row([
        role_text(state.page.label(), TextRole::WindowTitle).min_width(Dp::new(180.0)),
        status_bar_text(state.page.description()).flex(1.0),
    ])
    .min_height(Dp::new(38.0))
    .gap(Dp::new(16.0));
    let content = column([header, status, body, background_activity(state)])
        .id(CONTENT_SCROLL)
        .flex(1.0)
        .gap(Dp::new(8.0))
        .padding(Dp::new(16.0));
    let navigation_items = [
        (Page::Overview, NAV_OVERVIEW_BUTTON),
        (Page::Conversations, NAV_CONVERSATIONS_BUTTON),
        (Page::Storage, NAV_STORAGE_BUTTON),
        (Page::History, NAV_HISTORY_BUTTON),
        (Page::Settings, NAV_SETTINGS_BUTTON),
    ]
    .into_iter()
    .map(|(page, id)| {
        navigation_item(page.label(), page.icon(), page == state.page)
            .id(id)
            .on_click(Msg::Navigate(page))
    });
    let page = navigation_view(
        ZsNavigationViewSpec::new("Codex 清理", "Windows 安全清理助手")
            .items(navigation_items)
            .footer_item(
                row([
                    text(if state.dark_mode {
                        "深色模式"
                    } else {
                        "日间模式"
                    }),
                    spacer(),
                    toggle(state.dark_mode).on_toggle(Msg::DarkModeChanged),
                ])
                .gap(Dp::new(8.0)),
            )
            .pane_width(Dp::new(220.0))
            .minimum_content_width(Dp::new(800.0))
            .content(NAVIGATION_VIEW, content),
    )
    .bg(ThemeColorToken::Surface)
    .theme_mode(if state.dark_mode {
        ZsuiThemeMode::Dark
    } else {
        ZsuiThemeMode::Light
    });

    if state.show_resource_detail {
        let detail = selected_resource(state)
            .map(resource_detail_text)
            .unwrap_or_else(|| "所选资源已经不在当前分析结果中。".to_string());
        return content_dialog(
            RESOURCE_DETAIL_DIALOG,
            true,
            ZsContentDialogSpec::new(detail, "关闭").title("完整判断依据"),
            page,
        )
        .on_dialog_result(Msg::ResourceDetailClosed);
    }

    let Some(kind) = state.execute_dialog else {
        return page;
    };
    let (title, content, primary) = match kind {
        ExecuteKind::Conversation => {
            let content = state
                .analysis
                .as_ref()
                .map(|analysis| (analysis, build_cleanup_plan(analysis)))
                .map(|(analysis, plan)| {
                    format!(
                        "将永久删除\n• “{}”任务\n• {} 个子任务\n• {} 份任务记录，共 {}\n\n将移入 Windows 回收站\n• {} 个任务专属路径\n\n不会删除\n• 保留资源 {} 项\n• 共享、全局或待决定资源 {} 项\n\n计划可回收 {}；进入回收站的文件需清空回收站后才释放空间。永久删除的任务记录无法从回收站恢复。",
                        analysis.session.title,
                        plan.descendant_count,
                        1 + plan.descendant_count,
                        format_bytes(
                            analysis
                                .session
                                .transcript_bytes
                                .saturating_add(analysis.related_transcript_bytes)
                        ),
                        plan.recycle_paths.len(),
                        analysis
                            .resources
                            .iter()
                            .filter(|resource| resource.action == ResourceAction::Keep)
                            .count(),
                        analysis.review_count(),
                        format_bytes(plan.delete_bytes)
                    )
                })
                .unwrap_or_else(|| "请先分析任务并生成清理计划。".to_string());
            ("确认任务清理", content, "永久删除任务并清理")
        }
        ExecuteKind::Storage => {
            let review_count = state
                .storage
                .items
                .iter()
                .filter(|item| {
                    item.action == StorageAction::Clean && item.safety == StorageSafety::Review
                })
                .count();
            (
                "确认存储清理",
                format!(
                    "{} 个项目将进入 Windows 回收站，计划可回收 {}；清空回收站后才会释放空间。其中 {} 项属于人工复核类别。必须先完全退出 Codex。",
                    state
                        .storage
                        .items
                        .iter()
                        .filter(|item| item.action == StorageAction::Clean)
                        .count(),
                    format_bytes(state.storage.clean_bytes()),
                    review_count
                ),
                "移入回收站",
            )
        }
    };
    content_dialog(
        EXECUTE_DIALOG,
        true,
        ZsContentDialogSpec::new(content, "取消")
            .title(title)
            .primary_button(primary)
            .default_button(ZsContentDialogButton::Close)
            .destructive_button(ZsContentDialogButton::Primary),
        page,
    )
    .on_dialog_result(Msg::ExecuteDialogResult)
}

fn background_activity(state: &AppState) -> ViewNode<Msg> {
    let background = state
        .background
        .lock()
        .map(|value| value.clone())
        .unwrap_or_default();
    virtual_list(1, [(0, ())], |_, _| spacer())
        .loading(background.running)
        .placeholders(false)
        .height(Dp::new(1.0))
}

fn apply_background_preview(state: &mut AppState) {
    let background = state
        .background
        .lock()
        .map(|value| value.clone())
        .unwrap_or_default();
    if background.running {
        state.status = format!(
            "正在{} · {}% · {}",
            background
                .kind
                .map(BackgroundKind::label)
                .unwrap_or("处理中"),
            background.percent,
            background.stage
        );
    }
    if let Some(result) = background.result {
        apply_background_result(state, result);
    }
}

fn harvest_background(state: &mut AppState) {
    let result = state
        .background
        .lock()
        .ok()
        .and_then(|mut value| value.result.take());
    if let Some(result) = result {
        apply_background_result(state, result);
    }
}

fn apply_background_result(state: &mut AppState, result: BackgroundResult) {
    match result {
        BackgroundResult::FullScan(Ok(scan)) => {
            state.report = scan.report;
            state.storage = scan.storage;
            state.drive_usage = scan.drive_usage;
            state.task_filter = TaskFilter::All;
            state.project_anchor = None;
            state.task_page = 0;
            state.selected_session =
                (!visible_sessions(state).is_empty()).then(|| ZsTableRowId::new(1));
            state.overview_selected_session_id = overview_session_indices(&state.report)
                .first()
                .map(|index| state.report.sessions[*index].id.clone())
                .or_else(|| {
                    state
                        .report
                        .sessions
                        .iter()
                        .find(|session| session.parent_id.is_none())
                        .map(|session| session.id.clone())
                });
            state.storage_filter = StorageFilter::Recommended;
            state.storage_page = 0;
            state.selected_storage = first_storage_id_for_filter(state, state.storage_filter);
            state.analysis = None;
            state.selected_resource = None;
            state.history = scan.history;
            state.selected_history = state
                .history
                .first()
                .map(|entry| ZsTableRowId::new(entry.id));
            state.history_page = 0;
            state.status = format!(
                "全面扫描完成：{} 个任务，官方名称更新 {} 个，Codex 数据 {}",
                state.report.sessions.len(),
                scan.official_count,
                format_bytes(state.storage.total_bytes)
            );
        }
        BackgroundResult::FullScan(Err(error)) => {
            state.status = format!("全面扫描失败：{error}");
        }
        BackgroundResult::Sessions(Ok((report, official_count))) => {
            state.report = report;
            state.task_filter = TaskFilter::All;
            state.project_anchor = None;
            state.task_page = 0;
            state.selected_session =
                (!state.report.sessions.is_empty()).then(|| ZsTableRowId::new(1));
            state.analysis = None;
            state.selected_resource = None;
            state.overview_selected_session_id = overview_session_indices(&state.report)
                .first()
                .map(|index| state.report.sessions[*index].id.clone());
            state.history = load_cleanup_history(&state.report);
            state.status = format!(
                "任务刷新完成：{} 个任务，官方名称更新 {} 个",
                state.report.sessions.len(),
                official_count
            );
        }
        BackgroundResult::Sessions(Err(error)) => {
            state.status = format!("刷新任务失败：{error}");
        }
        BackgroundResult::Storage(storage) => {
            state.storage = storage;
            state.storage_page = 0;
            state.selected_storage = first_storage_id_for_filter(state, state.storage_filter);
            state.status = format!(
                "存储扫描完成：已盘点 {}，推荐安全清理 {}",
                format_bytes(state.storage.total_bytes),
                format_bytes(state.storage.safe_candidate_bytes())
            );
        }
        BackgroundResult::History(history) => {
            state.history = history;
            state.history_page = 0;
            state.selected_history = state
                .history
                .first()
                .map(|entry| ZsTableRowId::new(entry.id));
            state.status = format!("已读取 {} 条清理执行记录", state.history.len());
        }
        BackgroundResult::Analysis(Ok(analysis)) => {
            let decision_resource = analysis
                .resources
                .iter()
                .find(|resource| resource.action == ResourceAction::Review)
                .map(|resource| ZsTableRowId::new(resource.id));
            let final_count = analysis
                .resources
                .iter()
                .filter(|resource| resource.artifact_stage == Some(ArtifactStage::Final))
                .count();
            let intermediate_count = analysis
                .resources
                .iter()
                .filter(|resource| resource.artifact_stage == Some(ArtifactStage::Intermediate))
                .count();
            state.status = format!(
                "分析完成：{} 项资源，最终成果 {}，过程成果 {}，关联子任务 {}；已打开需要决定的项目",
                analysis.resources.len(),
                final_count,
                intermediate_count,
                analysis.related_session_ids.len()
            );
            state.analysis = Some(analysis);
            state.selected_resource = decision_resource;
            state.resource_filter = if state.selected_resource.is_some() {
                ResourceFilter::Decide
            } else {
                ResourceFilter::All
            };
            state.resource_page = 0;
        }
        BackgroundResult::Analysis(Err(error)) => {
            state.status = format!("分析失败：{error}");
        }
        BackgroundResult::ConversationCleanup(Ok(receipt))
            if receipt.status == CleanupStatus::Completed =>
        {
            let mut removed_ids = state
                .analysis
                .as_ref()
                .map(|analysis| analysis.related_session_ids.clone())
                .unwrap_or_default();
            removed_ids.push(receipt.session_id.clone());
            state
                .report
                .sessions
                .retain(|session| !removed_ids.contains(&session.id));
            if state
                .overview_selected_session_id
                .as_ref()
                .is_some_and(|id| removed_ids.contains(id))
            {
                state.overview_selected_session_id = overview_session_indices(&state.report)
                    .first()
                    .map(|index| state.report.sessions[*index].id.clone());
            }
            state.analysis = None;
            state.selected_resource = None;
            state.task_page = 0;
            state.selected_session =
                (!visible_sessions(state).is_empty()).then(|| ZsTableRowId::new(1));
            state.status = format!(
                "任务清理完成：永久删除 {} 个任务，{} 个关联路径进入回收站；记录：{}",
                removed_ids.len(),
                receipt.recycled_paths.len(),
                receipt.journal_path.display()
            );
            state.history = load_cleanup_history(&state.report);
            state.selected_history = state
                .history
                .first()
                .map(|entry| ZsTableRowId::new(entry.id));
        }
        BackgroundResult::ConversationCleanup(Ok(receipt)) => {
            state.status = format!(
                "任务清理未完全完成：{}；记录：{}",
                receipt.error.as_deref().unwrap_or("未知错误"),
                receipt.journal_path.display()
            );
            state.history = load_cleanup_history(&state.report);
        }
        BackgroundResult::ConversationCleanup(Err(error)) => {
            state.status = format!("无法执行任务清理：{error}");
        }
        BackgroundResult::StorageCleanup(Ok(receipt)) => {
            let recycled_bytes = state
                .storage
                .items
                .iter()
                .filter(|item| receipt.recycled_paths.contains(&item.path))
                .map(|item| item.size)
                .fold(0_u64, u64::saturating_add);
            state
                .storage
                .items
                .retain(|item| !receipt.recycled_paths.contains(&item.path));
            state.storage.total_bytes = state.storage.total_bytes.saturating_sub(recycled_bytes);
            state.storage_page = 0;
            state.selected_storage = first_storage_id_for_filter(state, state.storage_filter);
            state.status = format!(
                "存储清理完成：{} 项进入回收站，清空回收站后才会释放空间；{} 项失败；记录：{}",
                receipt.recycled_paths.len(),
                receipt.failed_paths.len(),
                receipt.journal_path.display()
            );
            state.history = load_cleanup_history(&state.report);
            state.selected_history = state
                .history
                .first()
                .map(|entry| ZsTableRowId::new(entry.id));
        }
        BackgroundResult::StorageCleanup(Err(error)) => {
            state.status = format!("无法执行存储清理：{error}");
        }
    }
}

fn view_overview(state: &AppState) -> ViewNode<Msg> {
    let review_bytes = state
        .storage
        .items
        .iter()
        .filter(|item| item.safety == StorageSafety::Review)
        .map(|item| item.size)
        .sum();
    let scan_action = row([
        secondary_text(
            "扫描只读取任务与存储结构，不会自动选择或删除内容。",
            TextRole::Caption,
        )
        .flex(1.0),
        button("重新扫描数据")
            .id(START_SCAN_BUTTON)
            .on_click(Msg::StartFullScan),
    ])
    .min_height(Dp::new(38.0))
    .gap(Dp::new(10.0));

    let drive_section = if let Some(drive) = state.drive_usage {
        let used_percent = if drive.total_bytes == 0 {
            0.0
        } else {
            (drive.used_bytes as f64 * 100.0 / drive.total_bytes as f64) as f32
        };
        section(
            "Windows 系统盘",
            [
                row([
                    text(format!(
                        "已用 {}  ·  总容量 {}",
                        format_bytes(drive.used_bytes),
                        format_bytes(drive.total_bytes)
                    )),
                    spacer().flex(1.0),
                    body_strong(format!(
                        "Codex 安全候选 {}",
                        format_bytes(state.storage.safe_candidate_bytes())
                    ))
                    .min_width(Dp::new(150.0)),
                ]),
                progress_bar(used_percent, ProgressRange::new(0.0, 100.0)),
            ],
        )
    } else {
        section(
            "Windows 系统盘",
            [secondary_text(
                "暂时无法读取磁盘容量；Codex 数据仍可独立扫描。",
                TextRole::Body,
            )],
        )
    };

    let metrics = row([
        summary_card("Codex 数据", format_bytes(state.storage.total_bytes)),
        summary_card(
            "安全候选",
            format_bytes(state.storage.safe_candidate_bytes()),
        ),
        summary_card("需要核对", format_bytes(review_bytes)),
    ])
    .min_height(Dp::new(58.0))
    .gap(Dp::new(10.0));

    let quick_actions = row([
        section(
            "清理一个任务",
            [
                secondary_text("选择任务，确认要保留的成果和源码。", TextRole::Body),
                primary_button("选择任务")
                    .id(HOME_TASKS_BUTTON)
                    .on_click(Msg::Navigate(Page::Conversations)),
            ],
        )
        .flex(1.0),
        section(
            "释放磁盘空间",
            [
                secondary_text("安全处理缓存、临时文件、备份和更新残留。", TextRole::Body),
                primary_button("查看可释放空间")
                    .id(HOME_STORAGE_BUTTON)
                    .on_click(Msg::Navigate(Page::Storage)),
            ],
        )
        .flex(1.0),
    ])
    .min_height(Dp::new(92.0))
    .gap(Dp::new(10.0));

    let recommendation_indices = overview_session_indices(&state.report);
    let rows = recommendation_indices
        .iter()
        .take(3)
        .map(|index| {
            let session = &state.report.sessions[*index];
            let stats = task_tree_stats(&state.report, &session.id);
            ZsTableRow::new(
                (*index + 1) as u64,
                [
                    session.title.clone(),
                    task_project_name(session),
                    stats
                        .last_activity
                        .map(|value| value.format("%Y-%m-%d").to_string())
                        .unwrap_or_else(|| "未知".to_string()),
                    format_bytes(stats.transcript_bytes),
                    task_recommendation_label(&state.report, session).to_string(),
                ],
            )
        })
        .collect::<Vec<_>>();
    let overview_row = state
        .overview_selected_session_id
        .as_deref()
        .and_then(|id| {
            state
                .report
                .sessions
                .iter()
                .position(|session| session.id == id)
        })
        .map(|index| ZsTableRowId::new((index + 1) as u64));
    let grid = data_grid(
        [
            ZsTableColumn::new(1, "任务").fill_width(5),
            ZsTableColumn::new(2, "项目").fill_width(3),
            ZsTableColumn::new(3, "最后活动").fill_width(3),
            ZsTableColumn::new(4, "记录大小")
                .fill_width(2)
                .alignment(HorizontalAlign::End),
            ZsTableColumn::new(5, "建议").fill_width(2),
        ],
        rows,
    )
    .id(OVERVIEW_TABLE)
    .height(table_viewport_height(3))
    .selected_table_row(overview_row)
    .on_table_select(Msg::OverviewSelected);
    let selected_action = if let Some(session) = overview_selected_session(state) {
        let stats = task_tree_stats(&state.report, &session.id);
        let reasons = task_recommendation_reasons(&state.report, session).join("、");
        let analyzed = state
            .analysis
            .as_ref()
            .is_some_and(|analysis| analysis.session.id == session.id);
        column([
            row([
                status_bar_text(format!(
                    "已选：{} · {} · {} 个子任务",
                    ellipsize(&session.title, 22),
                    format_bytes(stats.transcript_bytes),
                    stats.descendant_count
                ))
                .flex(1.0),
                row([
                    button("查看全部任务")
                        .id(OVERVIEW_OPEN_BUTTON)
                        .on_click(Msg::OverviewOpenSelected),
                    primary_button(if analyzed {
                        "继续核对清理方案"
                    } else {
                        "分析此任务"
                    })
                    .id(OVERVIEW_ANALYZE_BUTTON)
                    .on_click(Msg::OverviewAnalyzeSelected),
                ])
                .min_height(Dp::new(36.0))
                .gap(Dp::new(8.0)),
            ])
            .gap(Dp::new(8.0)),
            status_bar_text(format!("建议原因：{}", ellipsize(&reasons, 36))),
        ])
        .min_height(Dp::new(60.0))
        .gap(Dp::new(4.0))
    } else {
        row([
            secondary_text("暂无建议项，可查看全部任务。", TextRole::Body).flex(1.0),
            primary_button("查看全部任务")
                .id(OVERVIEW_OPEN_BUTTON)
                .on_click(Msg::OverviewOpenSelected),
        ])
        .min_height(Dp::new(40.0))
    };
    let recommendation_panel = section("建议分析的任务", [grid, selected_action]);

    column([
        scan_action,
        quick_actions,
        drive_section,
        metrics,
        recommendation_panel,
    ])
    .flex(1.0)
    .gap(Dp::new(10.0))
}

fn view_storage(state: &AppState) -> ViewNode<Msg> {
    let safe_bytes = state.storage.safe_candidate_bytes();
    let review_bytes = state
        .storage
        .items
        .iter()
        .filter(|item| item.safety == StorageSafety::Review)
        .map(|item| item.size)
        .sum();
    let protected_bytes = state
        .storage
        .items
        .iter()
        .filter(|item| item.safety == StorageSafety::Protected)
        .map(|item| item.size)
        .sum();
    let cards = row([
        summary_card("Codex 总占用", format_bytes(state.storage.total_bytes)),
        summary_card("推荐安全清理", format_bytes(safe_bytes)),
        summary_card("需要核对", format_bytes(review_bytes)),
        summary_card("已选择", format_bytes(state.storage.clean_bytes())),
    ])
    .min_height(Dp::new(62.0))
    .gap(Dp::new(8.0));
    let toolbar = row([
        button("重新扫描")
            .id(STORAGE_REFRESH_BUTTON)
            .on_click(Msg::StorageRefresh),
        primary_button(format!("智能选择安全项（{}）", format_bytes(safe_bytes)))
            .id(STORAGE_SAFE_RULES_BUTTON)
            .on_click(Msg::StorageApplySafeRules),
        spacer().flex(1.0),
        body_text(format!(
            "受保护，不会被清理：{}",
            format_bytes(protected_bytes)
        ))
        .flex(1.0),
    ])
    .gap(Dp::new(8.0));
    let filter_button = |filter: StorageFilter| {
        let marker = if state.storage_filter == filter {
            "● "
        } else {
            ""
        };
        let count = state
            .storage
            .items
            .iter()
            .filter(|item| storage_item_matches_filter(item, filter))
            .count();
        button(format!("{marker}{} {count}", filter.label()))
            .on_click(Msg::StorageFilterSelected(filter))
    };
    let filters = row([
        filter_button(StorageFilter::Recommended),
        filter_button(StorageFilter::Review),
        filter_button(StorageFilter::Protected),
        filter_button(StorageFilter::Selected),
        filter_button(StorageFilter::All),
    ])
    .min_height(Dp::new(36.0))
    .gap(Dp::new(6.0));
    let visible = visible_storage_items(state);
    let page_count = page_count(visible.len(), STORAGE_ITEMS_PER_PAGE);
    let page = state.storage_page.min(page_count.saturating_sub(1));
    let page_start = page.saturating_mul(STORAGE_ITEMS_PER_PAGE);
    let rows = visible
        .iter()
        .skip(page_start)
        .take(STORAGE_ITEMS_PER_PAGE)
        .map(|item| {
            ZsTableRow::new(
                item.id,
                [
                    item.label.clone(),
                    item.category.label().to_string(),
                    item.safety.label().to_string(),
                    format_bytes(item.size),
                    item.action.label().to_string(),
                ],
            )
        })
        .collect::<Vec<_>>();
    let grid = data_grid(
        [
            ZsTableColumn::new(1, "项目").fill_width(5),
            ZsTableColumn::new(2, "类别").fill_width(2),
            ZsTableColumn::new(3, "安全判断").fill_width(3),
            ZsTableColumn::new(4, "大小")
                .fill_width(2)
                .alignment(HorizontalAlign::End),
            ZsTableColumn::new(5, "选择").fill_width(2),
        ],
        rows,
    )
    .id(STORAGE_TABLE)
    .height(table_viewport_height(STORAGE_ITEMS_PER_PAGE))
    .selected_table_row(state.selected_storage)
    .on_table_select(Msg::StorageSelected);
    let pager = row([
        button("上一页")
            .id(STORAGE_PREVIOUS_BUTTON)
            .on_click(Msg::StoragePreviousPage),
        status_text(format!(
            "第 {} / {} 页 · 共 {} 项",
            page + 1,
            page_count,
            visible.len()
        ))
        .flex(1.0),
        button("下一页")
            .id(STORAGE_NEXT_BUTTON)
            .on_click(Msg::StorageNextPage),
    ])
    .min_height(Dp::new(34.0))
    .gap(Dp::new(8.0));
    let selected = selected_storage(state);
    let detail = selected
        .map(|item| {
            let newest = item
                .newest_at
                .map(|value| value.format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_else(|| "未知".to_string());
            let activity = item
                .stale_days
                .map(|days| format!("{days} 天未活动"))
                .unwrap_or_else(|| "活动时间未知".to_string());
            format!(
                "位置：{}\n{} 个文件 · {} · {} · {}\n最近修改：{}\n判断依据：{}",
                wrap_all_for_display(&item.path.display().to_string(), 92),
                item.file_count,
                item.category.label(),
                item.safety.label(),
                activity,
                newest,
                item.reason
            )
        })
        .unwrap_or_else(|| "选择一项可查看为何能清理或为何受保护。".to_string());
    let can_clean = selected.is_some_and(|item| item.safety != StorageSafety::Protected);
    let selected_actions = row([
        primary_button("保留这个项目")
            .enabled(selected.is_some())
            .on_click(Msg::StorageKeepSelected),
        button("清理这个项目")
            .enabled(selected.is_some() && can_clean)
            .on_click(Msg::StorageCleanSelected),
        button("稍后决定")
            .enabled(selected.is_some())
            .on_click(Msg::StorageReviewSelected),
        spacer().flex(1.0),
        status_text(format!(
            "已选 {} 项，共 {}",
            state
                .storage
                .items
                .iter()
                .filter(|item| item.action == StorageAction::Clean)
                .count(),
            format_bytes(state.storage.clean_bytes())
        ))
        .flex(1.0),
    ])
    .gap(Dp::new(8.0));
    let execute_actions = row([
        body_strong("3  核对并执行").min_width(Dp::new(130.0)),
        spacer().flex(1.0),
        button("导出清理清单")
            .id(STORAGE_PREVIEW_BUTTON)
            .on_click(Msg::StoragePreview),
        primary_button("核对并移入回收站")
            .id(STORAGE_EXECUTE_BUTTON)
            .on_click(Msg::StorageExecuteRequested),
    ])
    .min_height(Dp::new(38.0))
    .gap(Dp::new(8.0));
    column([
        body_strong("1  选择清理项目"),
        secondary_text(
            "推荐先使用“智能选择安全项”，再检查需要核对的备份与更新残留。",
            TextRole::Caption,
        ),
        cards,
        toolbar,
        filters,
        grid,
        pager,
        body_strong(if selected.is_some() {
            "2  当前项目：请选择保留或清理"
        } else {
            "2  选择表格中的一个项目"
        }),
        secondary_text(detail, TextRole::Body).min_height(Dp::new(66.0)),
        selected_actions,
        execute_actions,
    ])
    .gap(Dp::new(8.0))
}

fn view_history(state: &AppState) -> ViewNode<Msg> {
    let completed = state
        .history
        .iter()
        .filter(|entry| entry.result == "完成")
        .count();
    let issues = state.history.len().saturating_sub(completed);
    let recycled = state
        .history
        .iter()
        .map(|entry| entry.recycled_count)
        .sum::<usize>();
    let metrics = row([
        summary_card("执行记录", format!("{} 次", state.history.len())),
        summary_card("已完成", format!("{completed} 次")),
        summary_card("部分/失败", format!("{issues} 次")),
        summary_card("回收站路径", format!("{recycled} 项")),
    ])
    .min_height(Dp::new(72.0))
    .gap(Dp::new(10.0));
    let page_total = page_count(state.history.len(), HISTORY_ITEMS_PER_PAGE);
    let page = state.history_page.min(page_total.saturating_sub(1));
    let page_start = page.saturating_mul(HISTORY_ITEMS_PER_PAGE);
    let rows = state
        .history
        .iter()
        .skip(page_start)
        .take(HISTORY_ITEMS_PER_PAGE)
        .map(|entry| {
            ZsTableRow::new(
                entry.id,
                [
                    entry.occurred_at.clone(),
                    entry.kind.clone(),
                    entry.summary.clone(),
                    entry.result.clone(),
                    entry.recycled_count.to_string(),
                ],
            )
        })
        .collect::<Vec<_>>();
    let grid = data_grid(
        [
            ZsTableColumn::new(1, "时间").fill_width(3),
            ZsTableColumn::new(2, "类型").fill_width(2),
            ZsTableColumn::new(3, "对象/结果").fill_width(5),
            ZsTableColumn::new(4, "状态").fill_width(2),
            ZsTableColumn::new(5, "回收站")
                .fill_width(2)
                .alignment(HorizontalAlign::End),
        ],
        rows,
    )
    .id(HISTORY_TABLE)
    .height(table_viewport_height(HISTORY_ITEMS_PER_PAGE))
    .selected_table_row(state.selected_history)
    .on_table_select(Msg::HistorySelected);
    let selected = selected_history_entry(state);
    let detail = selected
        .map(|entry| {
            format!(
                "{}\n{}\n记录：{}\n恢复边界：{}",
                entry.summary,
                entry.detail,
                wrap_all_for_display(&entry.journal_path.display().to_string(), 72),
                history_recovery_label(entry)
            )
        })
        .unwrap_or_else(|| {
            "尚无执行记录。清理预览不会出现在此处，只记录实际执行结果。".to_string()
        });
    let history_section = section(
        "执行记录",
        [
            grid,
            column([
                row([
                    button("上一页")
                        .id(HISTORY_PREVIOUS_BUTTON)
                        .on_click(Msg::HistoryPreviousPage),
                    status_text(format!(
                        "第 {} / {} 页 · 共 {} 条",
                        page + 1,
                        page_total,
                        state.history.len()
                    ))
                    .flex(1.0),
                    button("下一页")
                        .id(HISTORY_NEXT_BUTTON)
                        .on_click(Msg::HistoryNextPage),
                ])
                .min_height(Dp::new(34.0))
                .gap(Dp::new(8.0)),
                row([
                    spacer().flex(1.0),
                    button("重新读取记录")
                        .id(HISTORY_REFRESH_BUTTON)
                        .on_click(Msg::RefreshHistory),
                ])
                .min_height(Dp::new(34.0)),
            ])
            .gap(Dp::new(6.0)),
        ],
    )
    .flex(1.0);
    let detail_section = section(
        "记录详情",
        [
            secondary_text(detail, TextRole::Body).min_height(Dp::new(230.0)),
            button("返回任务清理").on_click(Msg::Navigate(Page::Conversations)),
            button("返回空间清理").on_click(Msg::Navigate(Page::Storage)),
        ],
    )
    .min_width(Dp::new(320.0))
    .flex(1.0);
    column([
        secondary_text(
            "这里仅显示已产生回执的执行。任务记录可能已永久删除；普通文件只移入 Windows 回收站。",
            TextRole::Caption,
        ),
        metrics,
        row([history_section.flex(2.2), detail_section])
            .flex(1.0)
            .gap(Dp::new(12.0)),
    ])
    .flex(1.0)
    .gap(Dp::new(10.0))
}

fn view_settings(state: &AppState) -> ViewNode<Msg> {
    let paths = format!(
        "Codex 数据：{}\n执行记录：{}\n清理预览：{}",
        state.report.codex_home.display(),
        local_app_data_root().join("journals").display(),
        local_app_data_root().join("previews").display()
    );
    let appearance = section(
        "外观",
        [row([
            column([
                body_strong("界面主题"),
                secondary_text(
                    if state.dark_mode {
                        "当前使用深色界面"
                    } else {
                        "当前使用日间界面（默认）"
                    },
                    TextRole::Caption,
                ),
            ])
            .gap(Dp::new(2.0)),
            spacer().flex(1.0),
            toggle(state.dark_mode).on_toggle(Msg::DarkModeChanged),
        ])
        .min_height(Dp::new(52.0))],
    );
    let scanning = section(
        "扫描与更新",
        [
            secondary_text(
                "任务名称优先由 Codex 官方接口补全；数据不可用时再使用本地记录。存储扫描只读，不会自动选择清理项。",
                TextRole::Body,
            ),
            row([
                button("重新扫描任务")
                    .id(SETTINGS_TASK_SCAN_BUTTON)
                    .on_click(Msg::RefreshSessions),
                button("重新扫描存储")
                    .id(SETTINGS_STORAGE_SCAN_BUTTON)
                    .on_click(Msg::StorageRefresh),
            ])
            .min_height(Dp::new(34.0))
            .gap(Dp::new(8.0)),
        ],
    );
    let locations = section("数据位置", [secondary_text(paths, TextRole::Body)]);
    let safety = section(
        "固定安全保护",
        [
            body_text("✓ 普通文件仅移入 Windows 回收站"),
            body_text("✓ 任务记录永久删除前必须单独确认"),
            body_text("✓ 共享、全局、受保护资源不可从任务页删除"),
            body_text("✓ 存储清理前必须完全退出 Codex"),
            body_text("✓ 安全缓存规则要求至少 7 天未活动"),
        ],
    );
    let about = status_bar_text(format!(
        "Codex Cleaner {} · Rust · ZSUI 0.2.0-preview.6（本地源码）",
        env!("CARGO_PKG_VERSION")
    ))
    .min_height(Dp::new(24.0));
    column([appearance, scanning, locations, safety, about])
        .flex(1.0)
        .gap(Dp::new(10.0))
}

fn view_conversations(state: &AppState) -> ViewNode<Msg> {
    let visible = visible_sessions(state);
    let task_page_count = page_count(visible.len(), TASKS_PER_PAGE);
    let task_page = state.task_page.min(task_page_count.saturating_sub(1));
    let task_start = task_page.saturating_mul(TASKS_PER_PAGE);
    let session_rows = visible
        .iter()
        .enumerate()
        .skip(task_start)
        .take(TASKS_PER_PAGE)
        .map(|(index, session)| {
            ZsTableRow::new(
                (index + 1) as u64,
                [
                    task_display_title(session),
                    task_table_state(&state.report, session),
                    session
                        .updated_at
                        .map(|value| value.format("%m-%d %H:%M").to_string())
                        .unwrap_or_else(|| "未知".to_string()),
                    format_bytes(session.transcript_bytes),
                ],
            )
        })
        .collect::<Vec<_>>();
    let session_grid = data_grid(
        [
            ZsTableColumn::new(1, "任务").fill_width(5).sortable(true),
            ZsTableColumn::new(2, "状态").fill_width(3).sortable(true),
            ZsTableColumn::new(3, "更新").fill_width(3).sortable(true),
            ZsTableColumn::new(4, "大小")
                .fill_width(2)
                .alignment(HorizontalAlign::End)
                .sortable(true),
        ],
        session_rows,
    )
    .id(SESSION_TABLE)
    .height(table_viewport_height(TASKS_PER_PAGE))
    .selected_table_row(state.selected_session)
    .table_sort(state.session_sort)
    .on_table_select(Msg::SessionSelected)
    .on_table_invoke(Msg::SessionInvoked)
    .on_table_sort(Msg::SessionSorted);
    let filter_button = |filter: TaskFilter, id: WidgetId| {
        let marker = if state.task_filter == filter {
            "● "
        } else {
            ""
        };
        button(format!("{marker}{}", filter.label()))
            .id(id)
            .on_click(Msg::TaskFilterSelected(filter))
    };
    let filters = column([
        row([
            filter_button(TaskFilter::All, TASK_ALL_BUTTON),
            filter_button(TaskFilter::Active, TASK_ACTIVE_BUTTON),
            filter_button(TaskFilter::Archived, TASK_ARCHIVED_BUTTON),
            filter_button(TaskFilter::Local, TASK_LOCAL_BUTTON),
        ])
        .min_height(Dp::new(34.0))
        .gap(Dp::new(6.0)),
        row([
            filter_button(TaskFilter::Children, TASK_CHILDREN_BUTTON),
            filter_button(TaskFilter::Duplicates, TASK_DUPLICATE_BUTTON),
            filter_button(TaskFilter::SameProject, TASK_PROJECT_BUTTON),
        ])
        .min_height(Dp::new(34.0))
        .gap(Dp::new(6.0)),
    ])
    .min_height(Dp::new(74.0))
    .gap(Dp::new(6.0));
    if state.analysis.is_some() {
        let title = selected_session(state)
            .map(|session| session.title.clone())
            .unwrap_or_else(|| "已分析任务".to_string());
        return column([
            row([
                body_strong(format!("当前任务：{title}")).flex(1.0),
                button("返回任务列表")
                    .id(BACK_TO_TASKS_BUTTON)
                    .on_click(Msg::BackToTaskSelection),
            ])
            .min_height(Dp::new(36.0))
            .gap(Dp::new(8.0)),
            analyzed_task_view(state),
        ])
        .flex(1.0)
        .gap(Dp::new(10.0));
    }
    let selected = section(
        "已选择任务",
        [
            selected_task_detail_view(state),
            row([
                button("刷新任务")
                    .id(REFRESH_SESSIONS_BUTTON)
                    .on_click(Msg::RefreshSessions),
                spacer().flex(1.0),
                primary_button("分析所选任务")
                    .id(ANALYZE_BUTTON)
                    .on_click(Msg::AnalyzeSelected),
            ])
            .min_height(Dp::new(36.0))
            .gap(Dp::new(8.0)),
        ],
    );
    column([
        body_strong(format!(
            "1  选择任务 · {} {} 个 · 全部 {} 个",
            state.task_filter.label(),
            visible.len(),
            state.report.sessions.len()
        )),
        secondary_text(
            "选择一行后点击“分析所选任务”，也可以直接双击任务；分析前不会出现清理按钮。",
            TextRole::Caption,
        ),
        filters,
        session_grid,
        row([
            button("上一页")
                .id(TASK_PREVIOUS_BUTTON)
                .on_click(Msg::TaskPreviousPage),
            status_text(format!("第 {} / {} 页", task_page + 1, task_page_count)).flex(1.0),
            button("下一页")
                .id(TASK_NEXT_BUTTON)
                .on_click(Msg::TaskNextPage),
        ])
        .min_height(Dp::new(34.0))
        .gap(Dp::new(8.0)),
        selected,
    ])
    .flex(1.0)
    .gap(Dp::new(8.0))
}

fn summary_card(label: &str, value: String) -> ViewNode<Msg> {
    column([text(label), text(value)])
        .padding(Dp::new(10.0))
        .bg(ThemeColorToken::SurfaceRaised)
        .flex(1.0)
        .gap(Dp::new(3.0))
}

fn profile_label(state: &AppState, profile: RetentionProfile) -> String {
    if state.analysis.as_ref().map(|value| value.profile) == Some(profile) {
        format!("● {}", profile.label())
    } else {
        profile.label().to_string()
    }
}

fn analyzed_task_view(state: &AppState) -> ViewNode<Msg> {
    let analysis = state.analysis.as_ref().expect("analysis exists");
    let plan = build_cleanup_plan(analysis);
    let stages = |stage| {
        analysis
            .resources
            .iter()
            .filter(|resource| resource.artifact_stage == Some(stage))
            .count()
    };
    let strategy = row([
        button(profile_label(state, RetentionProfile::ResultsOnly))
            .id(RESULTS_ONLY_BUTTON)
            .on_click(Msg::ResultsOnly),
        button(profile_label(state, RetentionProfile::ResultsAndSource))
            .id(RESULTS_SOURCE_BUTTON)
            .on_click(Msg::ResultsAndSource),
        button(profile_label(
            state,
            RetentionProfile::DevelopmentEnvironment,
        ))
        .id(DEVELOPMENT_BUTTON)
        .on_click(Msg::DevelopmentEnvironment),
        button(profile_label(state, RetentionProfile::ConversationOnly))
            .id(CONVERSATION_ONLY_BUTTON)
            .on_click(Msg::ConversationOnly),
    ])
    .min_height(Dp::new(34.0))
    .gap(Dp::new(6.0));
    let cards = row([
        summary_card(
            "任务删除",
            format!("{} 个（永久）", 1 + plan.descendant_count),
        ),
        summary_card("可回收", format_bytes(plan.delete_bytes)),
        summary_card(
            "成果",
            format!(
                "最终 {} / 过程 {}",
                stages(ArtifactStage::Final),
                stages(ArtifactStage::Intermediate)
            ),
        ),
        summary_card(
            "需要决定",
            format!(
                "{} 项",
                analysis
                    .resources
                    .iter()
                    .filter(|resource| resource.action == ResourceAction::Review)
                    .count()
            ),
        ),
    ])
    .min_height(Dp::new(50.0))
    .gap(Dp::new(6.0));
    let warning = if analysis.truncated {
        "分析达到读取上限：未确认项目已默认保留或列入“需要决定”。".to_string()
    } else if !plan.blocked_resources.is_empty() {
        format!(
            "{} 项存在归属或父目录覆盖冲突，执行前必须处理。",
            plan.blocked_resources.len()
        )
    } else {
        format!(
            "任务记录 {}；{} 个专属路径将进入回收站。",
            format_bytes(
                analysis
                    .session
                    .transcript_bytes
                    .saturating_add(analysis.related_transcript_bytes)
            ),
            plan.recycle_paths.len()
        )
    };
    let filter_button = |filter: ResourceFilter| {
        let marker = if state.resource_filter == filter {
            "● "
        } else {
            ""
        };
        let count = analysis
            .resources
            .iter()
            .filter(|resource| resource_matches_filter(resource, filter))
            .count();
        button(format!("{marker}{} {count}", filter.label()))
            .on_click(Msg::ResourceFilterSelected(filter))
    };
    let filters = row([
        filter_button(ResourceFilter::Cleanup),
        filter_button(ResourceFilter::Keep),
        filter_button(ResourceFilter::Decide),
        filter_button(ResourceFilter::Storage),
        filter_button(ResourceFilter::All),
    ])
    .min_height(Dp::new(34.0))
    .gap(Dp::new(5.0));
    let visible = visible_resources(state);
    let page_total = page_count(visible.len(), RESOURCES_PER_PAGE);
    let page = state.resource_page.min(page_total.saturating_sub(1));
    let start = page.saturating_mul(RESOURCES_PER_PAGE);
    let rows = visible
        .iter()
        .skip(start)
        .take(RESOURCES_PER_PAGE)
        .map(|resource| {
            let kind = resource
                .artifact_stage
                .map(|stage| format!("{} · {}", resource.kind.label(), stage.label()))
                .unwrap_or_else(|| resource.kind.label().to_string());
            ZsTableRow::new(
                resource.id,
                [
                    resource.location.display().to_string(),
                    kind,
                    resource.ownership.label().to_string(),
                    format_bytes(resource.size),
                    if let Some(action) = resource.user_override {
                        format!("用户：{}", action.label())
                    } else {
                        format!("建议：{}", resource.recommended_action.label())
                    },
                ],
            )
        })
        .collect::<Vec<_>>();
    let grid = data_grid(
        [
            ZsTableColumn::new(1, "资源位置").fill_width(6),
            ZsTableColumn::new(2, "类型与阶段").fill_width(3),
            ZsTableColumn::new(3, "归属").fill_width(2),
            ZsTableColumn::new(4, "大小")
                .fill_width(2)
                .alignment(HorizontalAlign::End),
            ZsTableColumn::new(5, "处理").fill_width(2),
        ],
        rows,
    )
    .id(RESOURCE_TABLE)
    .height(table_viewport_height(RESOURCES_PER_PAGE))
    .selected_table_row(state.selected_resource)
    .on_table_select(Msg::ResourceSelected);
    let pager = row([
        button("上一页")
            .id(RESOURCE_PREVIOUS_BUTTON)
            .on_click(Msg::ResourcePreviousPage),
        status_text(format!(
            "{} / {} 页 · {} / {} 项",
            page + 1,
            page_total,
            visible.len(),
            analysis.resources.len()
        ))
        .flex(1.0),
        button("下一页")
            .id(RESOURCE_NEXT_BUTTON)
            .on_click(Msg::ResourceNextPage),
    ])
    .min_height(Dp::new(34.0))
    .gap(Dp::new(8.0));
    let selected = selected_resource(state);
    let detail = selected
        .map(resource_decision_text)
        .unwrap_or_else(|| "请选择表格中的一个项目。".to_string());
    let can_delete = selected.is_some_and(|resource| {
        matches!(
            resource.ownership,
            cleaner_core::Ownership::Exclusive | cleaner_core::Ownership::Unknown
        ) || matches!(
            resource.kind,
            ResourceKind::Conversation | ResourceKind::StateReference
        )
    });
    let decision_title = selected.map_or("当前未选择项目", |resource| {
        if matches!(
            resource.action,
            ResourceAction::Review | ResourceAction::Protected | ResourceAction::StorageReview
        ) {
            "当前项目需要你决定"
        } else {
            "当前项目的处理方式"
        }
    });
    let decision = column([
        body_strong(decision_title),
        secondary_text(detail, TextRole::Body).min_height(Dp::new(48.0)),
        row([
            primary_button("保留这个项目")
                .enabled(selected.is_some())
                .on_click(Msg::KeepSelected),
            button("清理这个项目")
                .enabled(selected.is_some() && can_delete)
                .on_click(Msg::DeleteSelected),
            button("稍后决定")
                .enabled(selected.is_some())
                .on_click(Msg::ReviewSelected),
            button("查看完整判断")
                .enabled(selected.is_some())
                .on_click(Msg::ShowResourceDetail),
        ])
        .min_height(Dp::new(34.0))
        .gap(Dp::new(6.0)),
    ])
    .gap(Dp::new(4.0));
    column([
        row([
            body_strong("2  选择保留方案").min_width(Dp::new(180.0)),
            spacer().flex(1.0),
            status_bar_text(format!(
                "共 {} 项 · 当前列表 {} 项",
                analysis.resources.len(),
                visible.len()
            )),
        ]),
        strategy,
        cards,
        status_bar_text(warning).min_height(Dp::new(24.0)),
        filters,
        grid,
        pager,
        decision,
        row([
            body_strong("3  核对并执行").min_width(Dp::new(130.0)),
            spacer().flex(1.0),
            button("导出清理清单")
                .id(PREVIEW_BUTTON)
                .on_click(Msg::Preview),
            primary_button("核对永久删除")
                .id(EXECUTE_BUTTON)
                .on_click(Msg::ExecuteRequested),
        ])
        .min_height(Dp::new(36.0))
        .gap(Dp::new(8.0)),
    ])
    .flex(1.0)
    .gap(Dp::new(7.0))
}

fn resource_detail_text(resource: &cleaner_core::ResourceNode) -> String {
    let evidence = if resource.evidence.is_empty() {
        "没有附加证据".to_string()
    } else {
        resource
            .evidence
            .iter()
            .enumerate()
            .map(|(index, value)| {
                format!("证据 {} · {}：{}", index + 1, value.source, value.detail)
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let stage = resource.artifact_reason.as_deref().unwrap_or("非成果文件");
    format!(
        "位置：{}\n类型：{} · {}\n大小：{}{} · 可信度：{}\n建议：{} · 用户选择：{}\n成果阶段：{}\n{}",
        wrap_all_for_display(&resource.location.display().to_string(), 64),
        resource.kind.label(),
        resource.ownership.label(),
        format_bytes(resource.size),
        if resource.size_complete { "" } else { "（统计不完整）" },
        resource.confidence.label(),
        resource.recommended_action.label(),
        resource
            .user_override
            .map(ResourceAction::label)
            .unwrap_or("未覆盖建议"),
        stage,
        evidence
    )
}

fn resource_decision_text(resource: &cleaner_core::ResourceNode) -> String {
    let evidence = resource
        .evidence
        .first()
        .map(|value| format!("{}：{}", value.source, value.detail))
        .unwrap_or_else(|| "没有附加证据".to_string());
    format!(
        "{}\n{} · {} · {} · 当前：{}\n主要判断：{}",
        wrap_all_for_display(&resource.location.display().to_string(), 96),
        resource.kind.label(),
        resource.ownership.label(),
        format_bytes(resource.size),
        resource.action.label(),
        ellipsize(&evidence, 180)
    )
}

fn visible_resources(state: &AppState) -> Vec<&cleaner_core::ResourceNode> {
    let Some(analysis) = state.analysis.as_ref() else {
        return Vec::new();
    };
    analysis
        .resources
        .iter()
        .filter(|resource| resource_matches_filter(resource, state.resource_filter))
        .collect()
}

fn resource_matches_filter(resource: &cleaner_core::ResourceNode, filter: ResourceFilter) -> bool {
    match filter {
        ResourceFilter::Cleanup => resource.action == ResourceAction::Delete,
        ResourceFilter::Keep => resource.action == ResourceAction::Keep,
        ResourceFilter::Decide => resource.action == ResourceAction::Review,
        ResourceFilter::Storage => matches!(
            resource.action,
            ResourceAction::StorageReview | ResourceAction::Protected
        ),
        ResourceFilter::All => true,
    }
}

#[derive(Debug, Clone)]
struct TaskTreeStats {
    transcript_bytes: u64,
    descendant_count: usize,
    last_activity: Option<chrono::DateTime<Utc>>,
}

fn task_tree_stats(report: &ScanReport, root_id: &str) -> TaskTreeStats {
    let mut discovered = std::collections::BTreeSet::new();
    discovered.insert(root_id.to_string());
    let mut frontier = vec![root_id.to_string()];
    while let Some(parent) = frontier.pop() {
        for child in report
            .sessions
            .iter()
            .filter(|session| session.parent_id.as_deref() == Some(parent.as_str()))
        {
            if discovered.insert(child.id.clone()) {
                frontier.push(child.id.clone());
            }
        }
    }
    let mut transcript_bytes = 0_u64;
    let mut last_activity: Option<chrono::DateTime<Utc>> = None;
    for session in report
        .sessions
        .iter()
        .filter(|session| discovered.contains(&session.id))
    {
        transcript_bytes = transcript_bytes.saturating_add(session.transcript_bytes);
        if let Some(updated_at) = session.updated_at {
            last_activity =
                Some(last_activity.map_or(updated_at, |current| current.max(updated_at)));
        }
    }
    TaskTreeStats {
        transcript_bytes,
        descendant_count: discovered.len().saturating_sub(1),
        last_activity,
    }
}

fn overview_session_indices(report: &ScanReport) -> Vec<usize> {
    let now = Utc::now();
    let mut candidates = report
        .sessions
        .iter()
        .enumerate()
        .filter(|(_, session)| session.parent_id.is_none())
        .filter_map(|(index, session)| {
            let stats = task_tree_stats(report, &session.id);
            let age_days = stats
                .last_activity
                .map(|value| (now - value).num_days().max(0));
            if age_days.is_some_and(|days| days < 7) {
                return None;
            }
            let duplicate_roots = duplicate_root_task_count(report, session);
            let eligible = session.status != SessionStatus::Active
                || age_days.is_some_and(|days| days >= 14)
                || stats.transcript_bytes >= 64 * 1024 * 1024
                || duplicate_roots > 1;
            if !eligible {
                return None;
            }
            let mut score = match session.status {
                SessionStatus::Archived => 25,
                SessionStatus::Orphaned => 15,
                SessionStatus::Active => 0,
            };
            score += age_days.unwrap_or(0).div_euclid(7).min(52) as i32;
            score += if stats.transcript_bytes >= 1024 * 1024 * 1024 {
                40
            } else if stats.transcript_bytes >= 512 * 1024 * 1024 {
                30
            } else if stats.transcript_bytes >= 128 * 1024 * 1024 {
                20
            } else if stats.transcript_bytes >= 32 * 1024 * 1024 {
                10
            } else {
                0
            };
            if duplicate_roots > 1 {
                score += 10;
            }
            score += (stats.descendant_count.saturating_mul(2).min(10)) as i32;
            Some((index, score, stats.transcript_bytes, stats.last_activity))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| right.2.cmp(&left.2))
            .then_with(|| left.3.cmp(&right.3))
            .then_with(|| report.sessions[left.0].id.cmp(&report.sessions[right.0].id))
    });
    candidates
        .into_iter()
        .map(|candidate| candidate.0)
        .collect()
}

fn duplicate_root_task_count(report: &ScanReport, session: &cleaner_core::SessionSummary) -> usize {
    let key = normalized_task_title(&session.title);
    if key.chars().count() < 6 || session.title.starts_with("本地任务 ") {
        return 1;
    }
    report
        .sessions
        .iter()
        .filter(|candidate| candidate.parent_id.is_none())
        .filter(|candidate| normalized_task_title(&candidate.title) == key)
        .count()
}

fn task_recommendation_label(
    report: &ScanReport,
    session: &cleaner_core::SessionSummary,
) -> &'static str {
    if duplicate_root_task_count(report, session) > 1 {
        "检查同名任务"
    } else {
        let stats = task_tree_stats(report, &session.id);
        let age_days = stats
            .last_activity
            .map(|value| (Utc::now() - value).num_days().max(0))
            .unwrap_or(0);
        if session.status == SessionStatus::Archived
            || stats.transcript_bytes >= 128 * 1024 * 1024
            || age_days >= 60
        {
            "优先分析"
        } else {
            "建议分析"
        }
    }
}

fn task_recommendation_reasons(
    report: &ScanReport,
    session: &cleaner_core::SessionSummary,
) -> Vec<String> {
    let stats = task_tree_stats(report, &session.id);
    let mut reasons = Vec::new();
    match session.status {
        SessionStatus::Archived => reasons.push("已归档".to_string()),
        SessionStatus::Orphaned => reasons.push("仅本地记录".to_string()),
        SessionStatus::Active => {}
    }
    if let Some(age_days) = stats
        .last_activity
        .map(|value| (Utc::now() - value).num_days().max(0))
    {
        if age_days >= 14 {
            reasons.push(format!("{age_days} 天未活动"));
        }
    }
    if stats.transcript_bytes >= 32 * 1024 * 1024 {
        reasons.push(format!(
            "任务树记录 {}",
            format_bytes(stats.transcript_bytes)
        ));
    }
    if duplicate_root_task_count(report, session) > 1 {
        reasons.push("存在同名主任务".to_string());
    }
    if stats.descendant_count > 0 {
        reasons.push(format!("{} 个子任务", stats.descendant_count));
    }
    if reasons.is_empty() {
        reasons.push("建议核对占用与成果".to_string());
    }
    reasons
}

fn task_project_name(session: &cleaner_core::SessionSummary) -> String {
    session
        .cwd
        .as_ref()
        .and_then(|path| path.file_name())
        .map(|value| value.to_string_lossy().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| session.cwd.as_ref().map(|path| path.display().to_string()))
        .unwrap_or_else(|| "未识别项目".to_string())
}

fn overview_selected_session(state: &AppState) -> Option<&cleaner_core::SessionSummary> {
    let id = state.overview_selected_session_id.as_deref()?;
    state
        .report
        .sessions
        .iter()
        .find(|session| session.id == id)
}

fn visible_storage_items(state: &AppState) -> Vec<&cleaner_core::StorageItem> {
    state
        .storage
        .items
        .iter()
        .filter(|item| storage_item_matches_filter(item, state.storage_filter))
        .collect()
}

fn first_storage_id_for_filter(state: &AppState, filter: StorageFilter) -> Option<ZsTableRowId> {
    state
        .storage
        .items
        .iter()
        .find(|item| storage_item_matches_filter(item, filter))
        .map(|item| ZsTableRowId::new(item.id))
}

fn storage_item_matches_filter(item: &cleaner_core::StorageItem, filter: StorageFilter) -> bool {
    match filter {
        StorageFilter::Recommended => {
            item.safety == StorageSafety::SafeAfterExit
                && item.stale_days.is_some_and(|days| days >= 7)
                && matches!(
                    item.category,
                    cleaner_core::StorageCategory::Cache | cleaner_core::StorageCategory::Temporary
                )
        }
        StorageFilter::Review => item.safety == StorageSafety::Review,
        StorageFilter::Protected => item.safety == StorageSafety::Protected,
        StorageFilter::Selected => item.action == StorageAction::Clean,
        StorageFilter::All => true,
    }
}

fn page_count(item_count: usize, page_size: usize) -> usize {
    item_count.div_ceil(page_size).max(1)
}

fn table_viewport_height(row_count: usize) -> Dp {
    let metrics = ZsTableMetrics::for_platform(ZsTablePlatformStyle::current());
    Dp::new(metrics.header_height.0 + metrics.row_height.0 * row_count as f32 + 1.0)
}

fn ellipsize(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut result = value
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    result.push('…');
    result
}

fn wrap_all_for_display(value: &str, line_chars: usize) -> String {
    let line_chars = line_chars.max(1);
    value
        .split('\n')
        .flat_map(|line| {
            let characters = line.chars().collect::<Vec<_>>();
            if characters.is_empty() {
                vec![String::new()]
            } else {
                characters
                    .chunks(line_chars)
                    .map(|chunk| chunk.iter().collect::<String>())
                    .collect::<Vec<_>>()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn update(state: &mut AppState, msg: Msg, _cx: &mut AppCx) {
    harvest_background(state);
    match msg {
        Msg::Navigate(page) => {
            state.page = page;
            state.status = match page {
                Page::Overview => "首页只提供扫描与清理入口，不会直接删除内容".to_string(),
                Page::Conversations => "选择任务后按“分析所选任务”进入保留方案".to_string(),
                Page::Storage => "仅扫描 Codex 的系统盘数据；选择一项可查看安全判断".to_string(),
                Page::History => "执行记录区分永久删除的任务与进入回收站的文件".to_string(),
                Page::Settings => "固定安全边界不能在设置中关闭".to_string(),
            };
        }
        Msg::StartFullScan => start_full_scan(state),
        Msg::OverviewSelected(row) => {
            state.overview_selected_session_id = state
                .report
                .sessions
                .get(row.get().saturating_sub(1) as usize)
                .map(|session| session.id.clone());
            if let Some(session) = overview_selected_session(state) {
                state.status = format!("已选择分析候选：{}", session.title);
            }
        }
        Msg::OverviewOpenSelected => {
            if let Some(id) = state.overview_selected_session_id.clone() {
                select_conversation_session(state, &id, false);
            } else {
                state.page = Page::Conversations;
                state.status = "已打开全部任务".to_string();
            }
        }
        Msg::OverviewAnalyzeSelected => {
            let Some(id) = state.overview_selected_session_id.clone() else {
                state.status = "请先选择一个建议任务".to_string();
                return;
            };
            let already_analyzed = state
                .analysis
                .as_ref()
                .is_some_and(|analysis| analysis.session.id == id);
            select_conversation_session(state, &id, already_analyzed);
            if already_analyzed {
                state.status = "已进入该任务的清理方案".to_string();
            } else {
                start_analyze_selected(state);
            }
        }
        Msg::RefreshSessions => start_refresh_sessions(state),
        Msg::TaskFilterSelected(filter) => {
            if filter == TaskFilter::SameProject {
                state.project_anchor = selected_session(state).map(|session| session.id.clone());
                if state.project_anchor.is_none() {
                    state.status = "请先在其他分组中选择一个任务，再查看同项目任务".to_string();
                    return;
                }
            }
            state.task_filter = filter;
            state.task_page = 0;
            state.selected_session =
                (!visible_sessions(state).is_empty()).then(|| ZsTableRowId::new(1));
            state.analysis = None;
            state.selected_resource = None;
            state.status = if filter == TaskFilter::Duplicates {
                "同名任务通常来自子任务、分支或重试；请按 UUID 和工作目录逐个核对".to_string()
            } else {
                format!(
                    "已切换到“{}”分组，共 {} 个任务",
                    filter.label(),
                    visible_sessions(state).len()
                )
            };
        }
        Msg::SessionSelected(row) => {
            state.selected_session = Some(row);
            state.selected_resource = None;
            state.resource_page = 0;
            state.analysis = None;
            if let Some(session) = selected_session(state) {
                state.status = format!("已选择：{}", session.title);
            }
        }
        Msg::SessionInvoked(row) => {
            state.selected_session = Some(row);
            state.selected_resource = None;
            state.resource_page = 0;
            state.analysis = None;
            start_analyze_selected(state);
        }
        Msg::SessionSorted(sort) => {
            state.session_sort = Some(sort);
            sort_sessions(&mut state.report, sort);
            state.task_page = 0;
            state.selected_session =
                (!state.report.sessions.is_empty()).then(|| ZsTableRowId::new(1));
            state.analysis = None;
            state.status = "任务列表已重排".to_string();
        }
        Msg::TaskPreviousPage => {
            state.task_page = state.task_page.saturating_sub(1);
            let start = state.task_page.saturating_mul(TASKS_PER_PAGE);
            state.selected_session = (start < visible_sessions(state).len())
                .then(|| ZsTableRowId::new((start + 1) as u64));
            state.analysis = None;
        }
        Msg::TaskNextPage => {
            let pages = page_count(visible_sessions(state).len(), TASKS_PER_PAGE);
            state.task_page = (state.task_page + 1).min(pages.saturating_sub(1));
            let start = state.task_page.saturating_mul(TASKS_PER_PAGE);
            state.selected_session = (start < visible_sessions(state).len())
                .then(|| ZsTableRowId::new((start + 1) as u64));
            state.analysis = None;
        }
        Msg::ResourceSelected(row) => {
            state.selected_resource = Some(row);
            if let Some(resource) = selected_resource(state) {
                state.status = format!(
                    "{} · {} · {}",
                    resource.kind.label(),
                    resource
                        .artifact_stage
                        .map(ArtifactStage::label)
                        .unwrap_or("非成果"),
                    resource.action.label()
                );
            }
        }
        Msg::ResourceFilterSelected(filter) => {
            state.resource_filter = filter;
            state.resource_page = 0;
            state.selected_resource = visible_resources(state)
                .first()
                .map(|resource| ZsTableRowId::new(resource.id));
            state.status = format!(
                "“{}”共 {} 项",
                filter.label(),
                visible_resources(state).len()
            );
        }
        Msg::ResourcePreviousPage => {
            state.resource_page = state.resource_page.saturating_sub(1);
            state.selected_resource = visible_resources(state)
                .get(state.resource_page.saturating_mul(RESOURCES_PER_PAGE))
                .map(|resource| ZsTableRowId::new(resource.id));
        }
        Msg::ResourceNextPage => {
            let pages = page_count(visible_resources(state).len(), RESOURCES_PER_PAGE);
            state.resource_page = (state.resource_page + 1).min(pages.saturating_sub(1));
            state.selected_resource = visible_resources(state)
                .get(state.resource_page.saturating_mul(RESOURCES_PER_PAGE))
                .map(|resource| ZsTableRowId::new(resource.id));
        }
        Msg::ShowResourceDetail => {
            if state.selected_resource.is_some() {
                state.show_resource_detail = true;
            } else {
                state.status = "请先选择一个资源".to_string();
            }
        }
        Msg::ResourceDetailClosed(result) => {
            state.show_resource_detail = false;
            if result == ZsContentDialogResult::Close {
                state.status = "已关闭完整判断依据".to_string();
            }
        }
        Msg::AnalyzeSelected => start_analyze_selected(state),
        Msg::BackToTaskSelection => {
            state.analysis = None;
            state.selected_resource = None;
            state.resource_page = 0;
            state.status = "已返回任务列表；重新分析前不会显示清理操作".to_string();
        }
        Msg::ResultsOnly => apply_profile(state, RetentionProfile::ResultsOnly),
        Msg::ResultsAndSource => apply_profile(state, RetentionProfile::ResultsAndSource),
        Msg::DevelopmentEnvironment => {
            apply_profile(state, RetentionProfile::DevelopmentEnvironment)
        }
        Msg::ConversationOnly => apply_profile(state, RetentionProfile::ConversationOnly),
        Msg::KeepSelected => set_selected_action(state, ResourceAction::Keep),
        Msg::DeleteSelected => set_selected_action(state, ResourceAction::Delete),
        Msg::ReviewSelected => set_selected_action(state, ResourceAction::Review),
        Msg::Preview => match state.analysis.as_ref() {
            Some(analysis) => match write_preview(analysis) {
                Ok(path) => state.status = format!("任务清理预览已保存：{}", path.display()),
                Err(error) => state.status = format!("预览保存失败：{error}"),
            },
            None => state.status = "请先分析所选任务".to_string(),
        },
        Msg::ExecuteRequested => request_conversation_execute(state),
        Msg::StorageRefresh => {
            start_storage_refresh(state);
        }
        Msg::StorageSelected(row) => {
            state.selected_storage = Some(row);
            if let Some(item) = selected_storage(state) {
                state.status = format!("{}：{}", item.safety.label(), item.reason);
            }
        }
        Msg::StorageFilterSelected(filter) => {
            state.storage_filter = filter;
            state.storage_page = 0;
            state.selected_storage = visible_storage_items(state)
                .first()
                .map(|item| ZsTableRowId::new(item.id));
            state.status = format!(
                "“{}”共 {} 项",
                filter.label(),
                visible_storage_items(state).len()
            );
        }
        Msg::StoragePreviousPage => {
            state.storage_page = state.storage_page.saturating_sub(1);
            state.selected_storage = visible_storage_items(state)
                .get(state.storage_page.saturating_mul(STORAGE_ITEMS_PER_PAGE))
                .map(|item| ZsTableRowId::new(item.id));
        }
        Msg::StorageNextPage => {
            let pages = page_count(visible_storage_items(state).len(), STORAGE_ITEMS_PER_PAGE);
            state.storage_page = (state.storage_page + 1).min(pages.saturating_sub(1));
            state.selected_storage = visible_storage_items(state)
                .get(state.storage_page.saturating_mul(STORAGE_ITEMS_PER_PAGE))
                .map(|item| ZsTableRowId::new(item.id));
        }
        Msg::StorageApplySafeRules => {
            apply_safe_storage_rules(&mut state.storage);
            state.storage_filter = StorageFilter::Selected;
            state.storage_page = 0;
            state.selected_storage = first_storage_id_for_filter(state, StorageFilter::Selected);
            state.status = format!(
                "安全规则已应用：选中 {}，备份、成果、日志和状态未自动选择",
                format_bytes(state.storage.clean_bytes())
            );
        }
        Msg::StorageKeepSelected => set_selected_storage_action(state, StorageAction::Keep),
        Msg::StorageCleanSelected => set_selected_storage_action(state, StorageAction::Clean),
        Msg::StorageReviewSelected => set_selected_storage_action(state, StorageAction::Review),
        Msg::StoragePreview => match write_storage_preview(&state.storage) {
            Ok(path) => state.status = format!("存储清理预览已保存：{}", path.display()),
            Err(error) => state.status = format!("存储预览保存失败：{error}"),
        },
        Msg::StorageExecuteRequested => request_storage_execute(state),
        Msg::HistorySelected(row) => {
            state.selected_history = Some(row);
            if let Some(entry) = selected_history_entry(state) {
                state.status = format!("{} · {} · {}", entry.kind, entry.result, entry.summary);
            }
        }
        Msg::HistoryPreviousPage => {
            state.history_page = state.history_page.saturating_sub(1);
            state.selected_history = state
                .history
                .get(state.history_page.saturating_mul(HISTORY_ITEMS_PER_PAGE))
                .map(|entry| ZsTableRowId::new(entry.id));
        }
        Msg::HistoryNextPage => {
            let pages = page_count(state.history.len(), HISTORY_ITEMS_PER_PAGE);
            state.history_page = (state.history_page + 1).min(pages.saturating_sub(1));
            state.selected_history = state
                .history
                .get(state.history_page.saturating_mul(HISTORY_ITEMS_PER_PAGE))
                .map(|entry| ZsTableRowId::new(entry.id));
        }
        Msg::RefreshHistory => start_history_refresh(state),
        Msg::DarkModeChanged(value) => {
            state.dark_mode = value;
            state.status = if let Err(error) = save_dark_mode(value) {
                format!("主题已切换，但无法保存设置：{error}")
            } else if value {
                "已切换为深色界面".to_string()
            } else {
                "已切换为浅色界面".to_string()
            };
        }
        Msg::ExecuteDialogResult(result) => {
            let kind = state.execute_dialog.take();
            if result != ZsContentDialogResult::Primary {
                state.status = "已取消清理".to_string();
            } else {
                match kind {
                    Some(ExecuteKind::Conversation) => start_conversation_cleanup(state),
                    Some(ExecuteKind::Storage) => start_storage_cleanup(state),
                    None => state.status = "清理计划已经失效".to_string(),
                }
            }
        }
    }
}

fn select_conversation_session(state: &mut AppState, session_id: &str, preserve_analysis: bool) {
    let filter = state
        .report
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .map(|session| {
            if session.parent_id.is_some() {
                TaskFilter::Children
            } else {
                TaskFilter::All
            }
        })
        .unwrap_or(TaskFilter::All);
    state.page = Page::Conversations;
    state.task_filter = filter;
    state.project_anchor = None;
    let position = visible_sessions(state)
        .iter()
        .position(|session| session.id == session_id);
    state.task_page = position.map_or(0, |index| index / TASKS_PER_PAGE);
    state.selected_session = position.map(|index| ZsTableRowId::new((index + 1) as u64));
    state.selected_resource = None;
    state.resource_page = 0;
    if !preserve_analysis
        || state
            .analysis
            .as_ref()
            .is_none_or(|analysis| analysis.session.id != session_id)
    {
        state.analysis = None;
    }
    if position.is_none() {
        state.status = "所选任务已不在当前扫描结果中".to_string();
    }
}

fn begin_background(state: &mut AppState, kind: BackgroundKind, stage: &str) -> bool {
    let Ok(mut background) = state.background.lock() else {
        state.status = "后台任务状态不可用，请重新启动软件".to_string();
        return false;
    };
    if background.running {
        state.status = format!(
            "{}仍在进行：{}",
            background
                .kind
                .map(BackgroundKind::label)
                .unwrap_or("后台任务"),
            background.stage
        );
        return false;
    }
    *background = BackgroundState {
        kind: Some(kind),
        percent: 5,
        stage: stage.to_string(),
        running: true,
        result: None,
    };
    state.status = format!("{}：{stage}", kind.label());
    true
}

fn start_full_scan(state: &mut AppState) {
    if !begin_background(state, BackgroundKind::FullScan, "读取任务索引与本地记录") {
        return;
    }
    let home = state.report.codex_home.clone();
    let codex_binary = state.codex_binary.clone();
    let background = Arc::clone(&state.background);
    let spawn = thread::Builder::new()
        .name("codex-cleaner-full-scan".to_string())
        .spawn(move || {
            set_background_progress(&background, 12, "扫描任务树、归档与本地记录");
            let result = scan_codex_home(&home)
                .map(|mut report| {
                    set_background_progress(&background, 42, "通过 Codex 官方接口补全任务名称");
                    let official_count = codex_binary
                        .as_ref()
                        .and_then(|binary| {
                            enrich_session_titles_official(
                                &mut report,
                                binary,
                                Duration::from_secs(12),
                            )
                            .ok()
                        })
                        .unwrap_or(0);
                    set_background_progress(
                        &background,
                        64,
                        "分类统计缓存、备份、日志、成果与状态",
                    );
                    let storage = scan_codex_storage(&home);
                    set_background_progress(&background, 90, "读取系统盘容量与清理执行记录");
                    let drive_usage = system_drive_usage();
                    let history = load_cleanup_history(&report);
                    FullScanResult {
                        report,
                        storage,
                        official_count,
                        drive_usage,
                        history,
                    }
                })
                .map_err(|error| error.to_string());
            finish_background(
                &background,
                "全面扫描已完成",
                BackgroundResult::FullScan(result),
            );
        });
    if let Err(error) = spawn {
        finish_background(
            &state.background,
            "无法启动全面扫描",
            BackgroundResult::FullScan(Err(error.to_string())),
        );
    }
}

fn set_background_progress(background: &Arc<Mutex<BackgroundState>>, percent: u8, stage: &str) {
    if let Ok(mut value) = background.lock() {
        value.percent = value.percent.max(percent.min(99));
        value.stage = stage.to_string();
    }
}

fn finish_background(
    background: &Arc<Mutex<BackgroundState>>,
    stage: &str,
    result: BackgroundResult,
) {
    if let Ok(mut value) = background.lock() {
        value.percent = 100;
        value.stage = stage.to_string();
        value.running = false;
        value.result = Some(result);
    }
}

fn start_refresh_sessions(state: &mut AppState) {
    if !begin_background(state, BackgroundKind::Sessions, "读取本地任务索引") {
        return;
    }
    let home = state.report.codex_home.clone();
    let codex_binary = state.codex_binary.clone();
    let background = Arc::clone(&state.background);
    let spawn = thread::Builder::new()
        .name("codex-cleaner-task-scan".to_string())
        .spawn(move || {
            set_background_progress(&background, 15, "扫描任务记录和父子关系");
            let result = scan_codex_home(&home)
                .map(|mut report| {
                    set_background_progress(&background, 70, "通过 Codex 官方接口补全名称与状态");
                    let official_count = codex_binary
                        .as_ref()
                        .and_then(|binary| {
                            enrich_session_titles_official(
                                &mut report,
                                binary,
                                Duration::from_secs(12),
                            )
                            .ok()
                        })
                        .unwrap_or(0);
                    (report, official_count)
                })
                .map_err(|error| error.to_string());
            finish_background(
                &background,
                "任务列表已更新",
                BackgroundResult::Sessions(result),
            );
        });
    if let Err(error) = spawn {
        finish_background(
            &state.background,
            "无法启动任务扫描",
            BackgroundResult::Sessions(Err(error.to_string())),
        );
    }
}

fn start_storage_refresh(state: &mut AppState) {
    if !begin_background(state, BackgroundKind::Storage, "建立 Codex 存储清单") {
        return;
    }
    let home = state.report.codex_home.clone();
    let background = Arc::clone(&state.background);
    let spawn = thread::Builder::new()
        .name("codex-cleaner-storage-scan".to_string())
        .spawn(move || {
            set_background_progress(&background, 20, "逐项统计缓存、备份、日志与运行组件");
            let report = scan_codex_storage(&home);
            finish_background(
                &background,
                "存储分类与互斥计数已完成",
                BackgroundResult::Storage(report),
            );
        });
    if let Err(error) = spawn {
        let mut failed = state.storage.clone();
        failed
            .warnings
            .push(format!("无法启动存储扫描线程：{error}"));
        finish_background(
            &state.background,
            "无法启动存储扫描",
            BackgroundResult::Storage(failed),
        );
    }
}

fn start_history_refresh(state: &mut AppState) {
    if !begin_background(state, BackgroundKind::History, "核对本地执行回执") {
        return;
    }
    let report = state.report.clone();
    let background = Arc::clone(&state.background);
    let spawn = thread::Builder::new()
        .name("codex-cleaner-history-scan".to_string())
        .spawn(move || {
            set_background_progress(&background, 35, "读取任务清理与存储清理回执");
            let history = load_cleanup_history(&report);
            finish_background(
                &background,
                "执行记录已更新",
                BackgroundResult::History(history),
            );
        });
    if let Err(error) = spawn {
        state.status = format!("无法启动记录读取：{error}");
        if let Ok(mut background) = state.background.lock() {
            background.running = false;
            background.percent = 100;
            background.stage = "记录读取失败".to_string();
        }
    }
}

fn start_analyze_selected(state: &mut AppState) {
    let Some(session) = selected_session(state) else {
        state.status = "请先选择任务".to_string();
        return;
    };
    let session_id = session.id.clone();
    if !begin_background(state, BackgroundKind::Analysis, "核对任务树和全部关联记录") {
        return;
    }
    let report = state.report.clone();
    let background = Arc::clone(&state.background);
    let spawn = thread::Builder::new()
        .name("codex-cleaner-task-analysis".to_string())
        .spawn(move || {
            set_background_progress(
                &background,
                18,
                "读取结构化事件、补丁、生成文件和任务专属目录",
            );
            let result = analyze_session(&report, &session_id, AnalysisOptions::default());
            finish_background(
                &background,
                "资源归属、成果阶段与清理冲突检查已完成",
                BackgroundResult::Analysis(result),
            );
        });
    if let Err(error) = spawn {
        finish_background(
            &state.background,
            "无法启动分析",
            BackgroundResult::Analysis(Err(error.to_string())),
        );
    }
}

fn request_conversation_execute(state: &mut AppState) {
    if state.analysis.is_none() {
        state.status = "请先完成任务分析，再核对永久删除计划".to_string();
        return;
    }
    let analysis = state.analysis.as_ref().expect("analysis checked");
    let plan = build_cleanup_plan(analysis);
    if !plan.blocked_resources.is_empty() {
        state.status = format!(
            "计划含 {} 项受保护资源，请改为保留或需确认",
            plan.blocked_resources.len()
        );
        return;
    }
    if state.codex_binary.is_none() {
        state.status = "未找到 Codex CLI，无法调用官方任务删除接口".to_string();
        return;
    }
    state.execute_dialog = Some(ExecuteKind::Conversation);
    state.status = "请核对永久删除任务警告".to_string();
}

fn request_storage_execute(state: &mut AppState) {
    if state.storage.clean_bytes() == 0 {
        state.status = "没有待清理项目；可先应用安全规则或手动标记复核项".to_string();
        return;
    }
    if codex_process_running() {
        state.status = "检测到 Codex 正在运行；请完全退出 Codex 后再执行存储清理".to_string();
        return;
    }
    state.execute_dialog = Some(ExecuteKind::Storage);
    state.status = "请核对将进入回收站的存储项目".to_string();
}

fn start_conversation_cleanup(state: &mut AppState) {
    let Some(analysis) = state.analysis.as_ref() else {
        state.status = "任务计划已失效，请重新分析".to_string();
        return;
    };
    let Some(codex_binary) = state.codex_binary.clone() else {
        state.status = "未找到 Codex CLI".to_string();
        return;
    };
    let plan = build_cleanup_plan(analysis);
    if !begin_background(
        state,
        BackgroundKind::ConversationCleanup,
        "重新核对路径并执行回收站与官方删除",
    ) {
        return;
    }
    let background = Arc::clone(&state.background);
    let journal_root = local_app_data_root().join("journals");
    let spawn = thread::Builder::new()
        .name("codex-cleaner-task-cleanup".to_string())
        .spawn(move || {
            set_background_progress(&background, 25, "先将任务专属文件移入 Windows 回收站");
            let result = execute_cleanup_plan(&plan, &codex_binary, &journal_root);
            finish_background(
                &background,
                "任务清理执行完毕",
                BackgroundResult::ConversationCleanup(result),
            );
        });
    if let Err(error) = spawn {
        finish_background(
            &state.background,
            "无法启动任务清理",
            BackgroundResult::ConversationCleanup(Err(error.to_string())),
        );
    }
}

fn start_storage_cleanup(state: &mut AppState) {
    if !begin_background(
        state,
        BackgroundKind::StorageCleanup,
        "重新核对安全级别与 Codex 进程状态",
    ) {
        return;
    }
    let report = state.storage.clone();
    let background = Arc::clone(&state.background);
    let journal_root = local_app_data_root().join("journals");
    let spawn = thread::Builder::new()
        .name("codex-cleaner-storage-cleanup".to_string())
        .spawn(move || {
            set_background_progress(&background, 30, "将已选项目移入 Windows 回收站");
            let result = execute_storage_cleanup(&report, journal_root);
            finish_background(
                &background,
                "存储清理执行完毕",
                BackgroundResult::StorageCleanup(result),
            );
        });
    if let Err(error) = spawn {
        finish_background(
            &state.background,
            "无法启动存储清理",
            BackgroundResult::StorageCleanup(Err(error.to_string())),
        );
    }
}

fn set_selected_action(state: &mut AppState, action: ResourceAction) {
    let Some(row) = state.selected_resource else {
        state.status = "请先选择关联资源".to_string();
        return;
    };
    let Some(resource) = state.analysis.as_mut().and_then(|analysis| {
        analysis
            .resources
            .iter_mut()
            .find(|resource| resource.id == row.get())
    }) else {
        state.status = "所选关联资源已不在列表中".to_string();
        return;
    };
    let deletable = matches!(
        resource.ownership,
        cleaner_core::Ownership::Exclusive | cleaner_core::Ownership::Unknown
    ) || matches!(
        resource.kind,
        ResourceKind::Conversation | ResourceKind::StateReference
    );
    if action == ResourceAction::Delete && !deletable {
        resource.user_override = Some(ResourceAction::Review);
        resource.action = ResourceAction::Review;
        state.status = "共享或全局资源不能从任务清理中直接删除，已改为需决定".to_string();
        return;
    }
    resource.user_override = Some(action);
    resource.action = action;
    state.status = if action == ResourceAction::Delete
        && resource.ownership == cleaner_core::Ownership::Unknown
    {
        "已按你的明确选择标记清理；执行前会重新检查父目录覆盖并移入回收站".to_string()
    } else {
        format!("所选关联资源已标记：{}", action.label())
    };
    if state.resource_filter == ResourceFilter::Decide && action != ResourceAction::Review {
        state.resource_page = 0;
        state.selected_resource = state.analysis.as_ref().and_then(|analysis| {
            analysis
                .resources
                .iter()
                .find(|resource| resource.action == ResourceAction::Review)
                .map(|resource| ZsTableRowId::new(resource.id))
        });
        if state.selected_resource.is_none() {
            state.status.push_str("；需要决定的项目已全部处理");
        } else {
            state.status.push_str("；已自动选中下一项");
        }
    }
}

fn set_selected_storage_action(state: &mut AppState, action: StorageAction) {
    let Some(row) = state.selected_storage else {
        state.status = "请先选择存储项目".to_string();
        return;
    };
    let Some(item) = state
        .storage
        .items
        .iter_mut()
        .find(|item| item.id == row.get())
    else {
        state.status = "所选存储项目已不在列表中".to_string();
        return;
    };
    if action == StorageAction::Clean && item.safety == StorageSafety::Protected {
        item.action = StorageAction::Keep;
        state.status = "此项目包含当前状态、任务历史或运行组件，禁止自动清理".to_string();
        return;
    }
    item.action = action;
    state.status = if action == StorageAction::Clean && item.safety == StorageSafety::Review {
        "已手动标记复核项；执行前会再次显示风险和回收站确认".to_string()
    } else {
        format!("所选存储项目已标记：{}", action.label())
    };
}

fn write_preview(analysis: &SessionAnalysis) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let root = local_app_data_root().join("previews");
    fs::create_dir_all(&root)?;
    let path = root.join(format!(
        "task-{}-{}-{}.json",
        analysis.session.id,
        Utc::now().format("%Y%m%d-%H%M%S"),
        Utc::now().timestamp_subsec_millis()
    ));
    fs::write(&path, serde_json::to_vec_pretty(analysis)?)?;
    Ok(path)
}

fn write_storage_preview(report: &StorageReport) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let root = local_app_data_root().join("previews");
    fs::create_dir_all(&root)?;
    let path = root.join(format!(
        "storage-{}-{}.json",
        Utc::now().format("%Y%m%d-%H%M%S"),
        Utc::now().timestamp_subsec_millis()
    ));
    fs::write(&path, serde_json::to_vec_pretty(report)?)?;
    Ok(path)
}

fn load_cleanup_history(report: &ScanReport) -> Vec<HistoryEntry> {
    load_cleanup_history_from(&local_app_data_root().join("journals"), report)
}

fn load_cleanup_history_from(root: &std::path::Path, report: &ScanReport) -> Vec<HistoryEntry> {
    const MAX_JOURNAL_BYTES: u64 = 8 * 1024 * 1024;
    const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
    const MAX_RECORDS: usize = 2_000;
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    let mut incomplete_count = 0_usize;
    for entry in entries.filter_map(Result::ok) {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() || file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        let name = path
            .file_name()
            .map(|value| value.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if name.ends_with(".json.tmp") {
            incomplete_count = incomplete_count.saturating_add(1);
        } else if path.extension().and_then(|value| value.to_str()) == Some("json")
            && paths.len() < MAX_RECORDS
        {
            paths.push(path);
        }
    }
    paths.sort();
    let mut history = Vec::new();
    if incomplete_count > 0 {
        history.push(HistoryEntry {
            id: 0,
            occurred_at: "时间未知".to_string(),
            kind: "未完成记录".to_string(),
            result: "状态未确认".to_string(),
            summary: format!("发现 {incomplete_count} 个临时回执文件"),
            detail: "上次执行可能在回执完成前中断；不会将临时文件当作已完成结果。".to_string(),
            journal_path: root.to_path_buf(),
            recycled_count: 0,
            failed_count: 1,
            permanent_thread_deleted: false,
        });
    }
    let mut remaining_bytes = MAX_TOTAL_BYTES;
    for path in paths {
        let file_length = fs::metadata(&path).ok().map(|metadata| metadata.len());
        if file_length.is_some_and(|length| length > remaining_bytes) {
            history.push(HistoryEntry {
                id: 0,
                occurred_at: history_file_time(&path),
                kind: "记录读取上限".to_string(),
                result: "未完全读取".to_string(),
                summary: "执行记录总读取量已达 64 MiB".to_string(),
                detail: "其余文件未解析，避免异常记录占用过多内存。".to_string(),
                journal_path: root.to_path_buf(),
                recycled_count: 0,
                failed_count: 1,
                permanent_thread_deleted: false,
            });
            break;
        }
        if let Some(length) = file_length {
            remaining_bytes = remaining_bytes.saturating_sub(length);
        }
        let parsed = file_length
            .filter(|length| *length <= MAX_JOURNAL_BYTES)
            .and_then(|_| fs::read(&path).ok())
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
        let Some(value) = parsed else {
            history.push(HistoryEntry {
                id: 0,
                occurred_at: history_file_time(&path),
                kind: "记录读取异常".to_string(),
                result: "无法读取".to_string(),
                summary: path
                    .file_name()
                    .map(|value| value.to_string_lossy().to_string())
                    .unwrap_or_else(|| "未知记录".to_string()),
                detail: "文件已损坏、超过 8 MiB 限制或无法读取；未将其解释为清理结果。".to_string(),
                journal_path: path,
                recycled_count: 0,
                failed_count: 1,
                permanent_thread_deleted: false,
            });
            continue;
        };
        if value.get("operation_id").is_some() && value.get("session_id").is_some() {
            let session_id = value
                .get("session_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("未知任务");
            let title = report
                .sessions
                .iter()
                .find(|session| session.id == session_id)
                .map(|session| session.title.clone())
                .unwrap_or_else(|| {
                    format!(
                        "任务 {}\u{2026}",
                        session_id.chars().take(8).collect::<String>()
                    )
                });
            let status = value
                .get("status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("failed");
            let error = value.get("error").and_then(serde_json::Value::as_str);
            let result = match status {
                "completed" => "完成",
                "partial" => "部分完成",
                "failed" if error.is_none() => "状态未确认",
                "failed" => "失败",
                _ => "状态未确认",
            }
            .to_string();
            let recycled_count = value
                .get("recycled_paths")
                .and_then(serde_json::Value::as_array)
                .map_or(0, Vec::len);
            let permanent_thread_deleted = value
                .get("official_thread_deleted")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let detail = format!(
                "官方任务删除：{}；进入回收站：{} 项{}",
                if permanent_thread_deleted {
                    "已完成"
                } else {
                    "未确认完成"
                },
                recycled_count,
                error
                    .map(|message| format!("；错误：{}", ellipsize(message, 180)))
                    .unwrap_or_default()
            );
            history.push(HistoryEntry {
                id: 0,
                occurred_at: history_json_time(&value, "finished_at", &path),
                kind: "任务清理".to_string(),
                result,
                summary: title,
                detail,
                journal_path: path,
                recycled_count,
                failed_count: usize::from(status != "completed"),
                permanent_thread_deleted,
            });
        } else if value.get("created_at").is_some() && value.get("failed_paths").is_some() {
            let recycled_count = value
                .get("recycled_paths")
                .and_then(serde_json::Value::as_array)
                .map_or(0, Vec::len);
            let failed_count = value
                .get("failed_paths")
                .and_then(serde_json::Value::as_array)
                .map_or(0, Vec::len);
            let result = match (recycled_count > 0, failed_count > 0) {
                (true, false) => "完成",
                (true, true) => "部分完成",
                (false, true) => "失败",
                (false, false) => "状态未确认",
            }
            .to_string();
            history.push(HistoryEntry {
                id: 0,
                occurred_at: history_json_time(&value, "created_at", &path),
                kind: "存储清理".to_string(),
                result,
                summary: format!("回收站 {recycled_count} 项 · 失败 {failed_count} 项"),
                detail: if failed_count == 0 {
                    "已选路径均已交给 Windows 回收站。".to_string()
                } else {
                    format!("{failed_count} 个路径未能进入回收站；可查看回执 JSON 获取具体原因。")
                },
                journal_path: path,
                recycled_count,
                failed_count,
                permanent_thread_deleted: false,
            });
        }
    }
    history.sort_by(|left, right| right.occurred_at.cmp(&left.occurred_at));
    for (index, entry) in history.iter_mut().enumerate() {
        entry.id = (index + 1) as u64;
    }
    history
}

fn history_json_time(value: &serde_json::Value, key: &str, path: &std::path::Path) -> String {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .map(|value| {
            value
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|| history_file_time(path))
}

fn history_file_time(path: &std::path::Path) -> String {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .map(chrono::DateTime::<chrono::Local>::from)
        .map(|value| value.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "时间未知".to_string())
}

fn selected_history_entry(state: &AppState) -> Option<&HistoryEntry> {
    let id = state.selected_history?.get();
    state.history.iter().find(|entry| entry.id == id)
}

fn history_recovery_label(entry: &HistoryEntry) -> &'static str {
    match (entry.permanent_thread_deleted, entry.recycled_count > 0) {
        (true, true) => "任务记录不可恢复；文件可尝试从 Windows 回收站恢复",
        (true, false) => "任务记录不可恢复",
        (false, true) => "文件可尝试从 Windows 回收站恢复",
        (false, false) if entry.failed_count > 0 => "未证实有内容被删除",
        (false, false) => "记录未保存可恢复内容",
    }
}

#[cfg(windows)]
fn system_drive_usage() -> Option<DriveUsage> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let drive = env::var_os("SystemDrive").unwrap_or_else(|| "C:".into());
    let root = format!(
        "{}\\",
        drive.to_string_lossy().trim_end_matches(['\\', '/'])
    );
    let wide = std::ffi::OsStr::new(&root)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut available = 0_u64;
    let mut total = 0_u64;
    let mut free = 0_u64;
    let success =
        unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut available, &mut total, &mut free) };
    (success != 0 && total > 0).then_some(DriveUsage {
        used_bytes: total.saturating_sub(free),
        total_bytes: total,
    })
}

#[cfg(not(windows))]
fn system_drive_usage() -> Option<DriveUsage> {
    None
}

fn load_dark_mode() -> bool {
    fs::read(local_app_data_root().join("settings.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .as_ref()
        .is_some_and(dark_mode_from_settings)
}

fn dark_mode_from_settings(value: &serde_json::Value) -> bool {
    if value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        != Some(2)
    {
        // 0.4.0 及更早版本把“没有设置”误当成深色。旧设置统一迁移到日间主题，
        // 用户在新版本明确切换后再按 schema v2 持久化。
        return false;
    }
    match value.get("theme").and_then(serde_json::Value::as_str) {
        Some("dark") => true,
        Some("light") => false,
        _ => value
            .get("dark_mode")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    }
}

fn save_dark_mode(dark_mode: bool) -> Result<(), Box<dyn std::error::Error>> {
    let root = local_app_data_root();
    fs::create_dir_all(&root)?;
    let path = root.join("settings.json");
    if fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| {
            value
                .get("schema_version")
                .and_then(serde_json::Value::as_u64)
        })
        .is_some_and(|version| version > 2)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "设置文件来自更高版本，未自动覆盖",
        )
        .into());
    }
    fs::write(
        path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 2,
            "theme": if dark_mode { "dark" } else { "light" },
            "dark_mode": dark_mode
        }))?,
    )?;
    Ok(())
}

fn local_app_data_root() -> PathBuf {
    env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir)
        .join("CodexCleaner")
}

fn apply_profile(state: &mut AppState, profile: RetentionProfile) {
    if state.analysis.is_none() {
        state.status = "请先分析任务，再选择保留方案".to_string();
        return;
    }
    let summary = if let Some(analysis) = state.analysis.as_mut() {
        apply_retention_profile(analysis, profile);
        let decision_count = analysis
            .resources
            .iter()
            .filter(|resource| resource.action == ResourceAction::Review)
            .count();
        Some((
            analysis.delete_bytes(),
            analysis.keep_bytes(),
            decision_count,
        ))
    } else {
        None
    };
    if let Some((delete_bytes, keep_bytes, decision_count)) = summary {
        state.resource_filter = if decision_count > 0 {
            ResourceFilter::Decide
        } else {
            ResourceFilter::All
        };
        state.resource_page = 0;
        state.selected_resource = visible_resources(state)
            .first()
            .map(|resource| ZsTableRowId::new(resource.id));
        state.status = format!(
            "已应用“{}”：清理 {}，保留 {}；{}",
            profile.label(),
            format_bytes(delete_bytes),
            format_bytes(keep_bytes),
            if decision_count > 0 {
                format!("已自动列出 {decision_count} 个需要你决定的项目")
            } else {
                "没有需要你决定的项目".to_string()
            }
        );
    }
}

fn selected_session(state: &AppState) -> Option<&cleaner_core::SessionSummary> {
    state.selected_session.and_then(|row| {
        visible_sessions(state)
            .get(row.get().saturating_sub(1) as usize)
            .copied()
    })
}

fn visible_sessions(state: &AppState) -> Vec<&cleaner_core::SessionSummary> {
    let anchor = state.project_anchor.as_deref().and_then(|id| {
        state
            .report
            .sessions
            .iter()
            .find(|session| session.id == id)
    });
    let mut duplicate_counts = std::collections::HashMap::<String, usize>::new();
    if state.task_filter == TaskFilter::Duplicates {
        for session in &state.report.sessions {
            let key = normalized_task_title(&session.title);
            if key.chars().count() >= 6 && !session.title.starts_with("本地任务 ") {
                *duplicate_counts.entry(key).or_default() += 1;
            }
        }
    }
    state
        .report
        .sessions
        .iter()
        .filter(|session| match state.task_filter {
            TaskFilter::All => session.parent_id.is_none(),
            TaskFilter::Active => session.status == cleaner_core::SessionStatus::Active,
            TaskFilter::Archived => session.status == cleaner_core::SessionStatus::Archived,
            TaskFilter::Local => session.transcript_paths.is_empty(),
            TaskFilter::Children => session.parent_id.is_some(),
            TaskFilter::Duplicates => {
                duplicate_counts
                    .get(&normalized_task_title(&session.title))
                    .copied()
                    .unwrap_or(1)
                    > 1
            }
            TaskFilter::SameProject => anchor.is_some_and(|anchor| same_project(anchor, session)),
        })
        .collect()
}

fn same_project(left: &cleaner_core::SessionSummary, right: &cleaner_core::SessionSummary) -> bool {
    match (left.cwd.as_ref(), right.cwd.as_ref()) {
        (Some(left), Some(right)) => {
            let left_key = left
                .to_string_lossy()
                .replace('\\', "/")
                .trim_end_matches('/')
                .to_ascii_lowercase();
            let right_key = right
                .to_string_lossy()
                .replace('\\', "/")
                .trim_end_matches('/')
                .to_ascii_lowercase();
            if left_key == right_key {
                return true;
            }
            let (ancestor, descendant) = if right_key.starts_with(&format!("{left_key}/")) {
                (left, right)
            } else if left_key.starts_with(&format!("{right_key}/")) {
                (right, left)
            } else {
                return false;
            };
            let ancestor_depth = ancestor.components().count();
            let depth_difference = descendant
                .components()
                .count()
                .saturating_sub(ancestor_depth);
            let generic = ancestor
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|value| {
                    matches!(
                        value.to_ascii_lowercase().as_str(),
                        "users"
                            | "documents"
                            | "desktop"
                            | "downloads"
                            | "workspace"
                            | "workspaces"
                            | "projects"
                    )
                });
            ancestor_depth >= 2 && depth_difference <= 2 && !generic
        }
        _ => left.id == right.id,
    }
}

fn duplicate_task_count(report: &ScanReport, session: &cleaner_core::SessionSummary) -> usize {
    let key = normalized_task_title(&session.title);
    if key.chars().count() < 6 || session.title.starts_with("本地任务 ") {
        return 1;
    }
    report
        .sessions
        .iter()
        .filter(|candidate| normalized_task_title(&candidate.title) == key)
        .count()
}

fn normalized_task_title(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .take(100)
        .collect()
}

fn task_relation_label(report: &ScanReport, session: &cleaner_core::SessionSummary) -> String {
    if session.parent_id.is_some() {
        return "子任务".to_string();
    }
    let descendant_count = task_descendant_count(report, &session.id);
    if descendant_count > 0 {
        return format!("主任务 · {descendant_count} 子任务");
    }
    let duplicate_count = duplicate_task_count(report, session);
    if duplicate_count > 1 {
        format!("同名×{duplicate_count}")
    } else {
        "独立".to_string()
    }
}

fn task_table_state(report: &ScanReport, session: &cleaner_core::SessionSummary) -> String {
    let status = match session.status {
        SessionStatus::Active => "活跃",
        SessionStatus::Archived => "归档",
        SessionStatus::Orphaned => "仅本地",
    };
    if session.parent_id.is_some() {
        return format!("{status}·子");
    }
    let descendant_count = task_descendant_count(report, &session.id);
    if descendant_count > 0 {
        return format!("{status}·主·{descendant_count}子");
    }
    let duplicate_count = duplicate_task_count(report, session);
    if duplicate_count > 1 {
        format!("{status}·同名×{duplicate_count}")
    } else {
        format!("{status}·独立")
    }
}

fn task_descendant_count(report: &ScanReport, root_id: &str) -> usize {
    let mut discovered = std::collections::BTreeSet::new();
    let mut frontier = vec![root_id.to_string()];
    while let Some(parent) = frontier.pop() {
        for child in report
            .sessions
            .iter()
            .filter(|session| session.parent_id.as_deref() == Some(parent.as_str()))
        {
            if discovered.insert(child.id.clone()) {
                frontier.push(child.id.clone());
            }
        }
    }
    discovered.len()
}

fn task_display_title(session: &cleaner_core::SessionSummary) -> String {
    if session.parent_id.is_some() {
        let identity = child_task_identity(session)
            .unwrap_or_else(|| session.id.chars().take(8).collect::<String>());
        format!("子任务 {identity} · {}", session.title)
    } else {
        session.title.clone()
    }
}

fn child_task_identity(session: &cleaner_core::SessionSummary) -> Option<String> {
    let source: serde_json::Value = serde_json::from_str(session.source.as_deref()?).ok()?;
    let spawn = source.get("subagent")?.get("thread_spawn")?;
    let nickname = spawn
        .get("agent_nickname")
        .and_then(serde_json::Value::as_str);
    let task_name = spawn
        .get("agent_path")
        .and_then(serde_json::Value::as_str)
        .and_then(|path| path.rsplit('/').find(|part| !part.is_empty()));
    match (nickname, task_name) {
        (Some(nickname), Some(task_name)) => Some(format!("{nickname} · {task_name}")),
        (Some(nickname), None) => Some(nickname.to_string()),
        (None, Some(task_name)) => Some(task_name.to_string()),
        (None, None) => None,
    }
}

fn selected_task_detail_view(state: &AppState) -> ViewNode<Msg> {
    let Some(session) = selected_session(state) else {
        return column([body_text("请选择任务")])
            .min_height(Dp::new(92.0))
            .flex(0.0);
    };
    let mut lines = vec![
        body_strong(session.title.clone()),
        secondary_text(
            format!(
                "{} · {} · {}",
                session.status.label(),
                task_relation_label(&state.report, session),
                format_bytes(session.transcript_bytes)
            ),
            TextRole::Body,
        ),
        secondary_text(
            format!("UUID：{}", wrap_all_for_display(&session.id, 42)),
            TextRole::Caption,
        ),
    ];
    if let Some(parent) = session.parent_id.as_deref() {
        let identity = child_task_identity(session).unwrap_or_else(|| "未命名子任务".to_string());
        lines.push(secondary_text(
            format!(
                "子任务：{} · 父任务：{}",
                identity,
                wrap_all_for_display(parent, 42)
            ),
            TextRole::Body,
        ));
    }
    let cwd = session
        .cwd
        .as_ref()
        .map(|value| value.display().to_string())
        .unwrap_or_else(|| "未知".to_string());
    lines.push(secondary_text(
        format!("工作目录：{}", wrap_all_for_display(&cwd, 42)),
        TextRole::Body,
    ));
    column(lines)
        .min_height(Dp::new(92.0))
        .flex(0.0)
        .gap(Dp::new(4.0))
}

fn selected_resource(state: &AppState) -> Option<&cleaner_core::ResourceNode> {
    state.selected_resource.and_then(|row| {
        state
            .analysis
            .as_ref()?
            .resources
            .iter()
            .find(|resource| resource.id == row.get())
    })
}

fn selected_storage(state: &AppState) -> Option<&cleaner_core::StorageItem> {
    state
        .selected_storage
        .and_then(|row| state.storage.items.iter().find(|item| item.id == row.get()))
}

fn sort_sessions(report: &mut ScanReport, sort: ZsTableSort) {
    report.sessions.sort_by(|left, right| {
        let ordering = match sort.column.get() {
            1 => left.title.cmp(&right.title),
            2 => left.status.label().cmp(right.status.label()),
            3 => left.updated_at.cmp(&right.updated_at),
            4 => left.transcript_bytes.cmp(&right.transcript_bytes),
            _ => std::cmp::Ordering::Equal,
        };
        if sort.direction == ZsTableSortDirection::Descending {
            ordering.reverse()
        } else {
            ordering
        }
    });
}

fn argument_value<'a>(args: &'a [String], key: &str) -> Option<&'a str> {
    args.windows(2)
        .find(|pair| pair[0] == key)
        .map(|pair| pair[1].as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_state(page: Page) -> AppState {
        AppState {
            page,
            dark_mode: false,
            report: ScanReport {
                codex_home: PathBuf::from("C:/codex"),
                sessions: vec![],
                transcript_bytes: 0,
                malformed_index_lines: 0,
                warnings: vec![],
            },
            storage: StorageReport {
                roots: vec![],
                items: vec![],
                total_bytes: 0,
                warnings: vec![],
            },
            drive_usage: None,
            overview_selected_session_id: None,
            selected_session: None,
            task_filter: TaskFilter::All,
            project_anchor: None,
            session_sort: None,
            task_page: 0,
            selected_resource: None,
            resource_filter: ResourceFilter::All,
            resource_page: 0,
            selected_storage: None,
            storage_filter: StorageFilter::Recommended,
            storage_page: 0,
            history: vec![],
            selected_history: None,
            history_page: 0,
            analysis: None,
            status: "ready".to_string(),
            codex_binary: None,
            execute_dialog: None,
            show_resource_detail: false,
            background: Arc::new(Mutex::new(BackgroundState::default())),
        }
    }

    #[test]
    fn day_theme_is_default_and_legacy_settings_migrate_to_day() {
        assert!(!dark_mode_from_settings(&serde_json::json!({})));
        assert!(!dark_mode_from_settings(&serde_json::json!({
            "schema_version": 1,
            "dark_mode": true
        })));
        assert!(!dark_mode_from_settings(&serde_json::json!({
            "schema_version": 2,
            "theme": "light"
        })));
        assert!(dark_mode_from_settings(&serde_json::json!({
            "schema_version": 2,
            "theme": "dark"
        })));
    }

    #[test]
    fn table_viewport_reserves_every_paginated_row() {
        let metrics = ZsTableMetrics::for_platform(ZsTablePlatformStyle::current());
        let expected =
            metrics.header_height.0 + metrics.row_height.0 * STORAGE_ITEMS_PER_PAGE as f32 + 1.0;
        assert_eq!(
            table_viewport_height(STORAGE_ITEMS_PER_PAGE),
            Dp::new(expected)
        );
        assert_eq!(
            table_viewport_height(HISTORY_ITEMS_PER_PAGE),
            Dp::new(
                metrics.header_height.0
                    + metrics.row_height.0 * HISTORY_ITEMS_PER_PAGE as f32
                    + 1.0
            )
        );
    }

    #[test]
    fn full_detail_formatting_wraps_without_truncating() {
        let tail = "必须保留的最后一段完整证据";
        let resource = cleaner_core::ResourceNode {
            id: 1,
            location: cleaner_core::ResourceLocation::Path {
                path: PathBuf::from(format!(
                    "C:/very/long/path/{}/final-result.docx",
                    "nested/".repeat(18)
                )),
            },
            kind: ResourceKind::ResultArtifact,
            artifact_stage: Some(ArtifactStage::Final),
            artifact_reason: Some(format!("最终成果判断依据：{tail}")),
            size: 42,
            size_complete: true,
            ownership: cleaner_core::Ownership::Exclusive,
            confidence: cleaner_core::Confidence::Confirmed,
            evidence: vec![cleaner_core::Evidence {
                source: "工具输出".to_string(),
                detail: format!("一段很长的分析说明。{tail}"),
            }],
            recommended_action: ResourceAction::Keep,
            user_override: None,
            action: ResourceAction::Keep,
        };

        let detail = resource_detail_text(&resource);

        assert!(detail.contains(tail));
        assert!(detail.contains("final-result.docx"));
        assert!(!detail.contains('…'));
        assert_eq!(
            wrap_all_for_display("123456789", 4).replace('\n', ""),
            "123456789"
        );
    }

    #[test]
    fn retention_profile_opens_decisions_and_advances_after_a_choice() {
        let mut state = empty_state(Page::Conversations);
        let selected = session(
            "task-1",
            "任务",
            cleaner_core::SessionStatus::Active,
            "C:/work",
            None,
        );
        let review_resource = |id| cleaner_core::ResourceNode {
            id,
            location: cleaner_core::ResourceLocation::Path {
                path: PathBuf::from(format!("C:/unknown-{id}.dat")),
            },
            kind: ResourceKind::SupportLibrary,
            artifact_stage: None,
            artifact_reason: None,
            size: 1,
            size_complete: true,
            ownership: cleaner_core::Ownership::Unknown,
            confidence: cleaner_core::Confidence::Weak,
            evidence: vec![],
            recommended_action: ResourceAction::Review,
            user_override: None,
            action: ResourceAction::Review,
        };
        state.analysis = Some(SessionAnalysis {
            session: selected,
            related_session_ids: vec![],
            related_transcript_bytes: 0,
            project_related_session_ids: vec![],
            duplicate_title_session_ids: vec![],
            project_transcript_bytes: 0,
            resources: vec![review_resource(1), review_resource(2)],
            profile: RetentionProfile::ResultsAndSource,
            analyzed_bytes: 0,
            truncated: false,
            warnings: vec![],
        });

        apply_profile(&mut state, RetentionProfile::ResultsAndSource);

        assert_eq!(state.resource_filter, ResourceFilter::Decide);
        assert_eq!(state.selected_resource, Some(ZsTableRowId::new(1)));
        set_selected_action(&mut state, ResourceAction::Keep);
        assert_eq!(state.selected_resource, Some(ZsTableRowId::new(2)));
        assert!(state.status.contains("下一项"));
    }

    #[test]
    fn every_page_uses_a_single_screen_without_page_scrolling() {
        let cases = [
            (Page::Overview, START_SCAN_BUTTON),
            (Page::Conversations, ANALYZE_BUTTON),
            (Page::Storage, STORAGE_EXECUTE_BUTTON),
            (Page::History, HISTORY_REFRESH_BUTTON),
            (Page::Settings, SETTINGS_STORAGE_SCAN_BUTTON),
        ];
        for (page, target) in cases {
            let mut node = view(&empty_state(page));
            let bounds = Rect {
                x: 0,
                y: 0,
                width: 1060,
                height: 860,
            };
            node.layout(&mut ViewLayoutCx::new(bounds, Dpi::new(144.0)));
            assert_eq!(node.bounds(), Some(bounds));
            assert_eq!(node.widget_scroll_target(target), None);
        }
    }

    fn click_message(state: &AppState, widget: WidgetId) -> Msg {
        let mut node = view(state);
        let mut layout = ViewLayoutCx::new(
            Rect {
                x: 0,
                y: 0,
                width: 1360,
                // Use a tall surface here because this helper verifies message
                // routing only; the separate page test enforces no page scroll.
                height: 1600,
            },
            Dpi::standard(),
        );
        node.layout(&mut layout);
        node.interaction_plan()
            .hit_target_for_widget(widget)
            .unwrap_or_else(|| panic!("missing click target {widget:?}"));
        let mut events = ViewEventCx::new();
        node.event(&mut events, &ViewEvent::Click { widget });
        events.into_messages().into_iter().next().unwrap()
    }

    #[test]
    fn every_conversation_toolbar_button_dispatches() {
        let mut state = empty_state(Page::Conversations);
        let initial_checks = [
            (ANALYZE_BUTTON, "analyze"),
            (REFRESH_SESSIONS_BUTTON, "refresh"),
            (TASK_ALL_BUTTON, "all"),
            (TASK_ACTIVE_BUTTON, "active"),
            (TASK_ARCHIVED_BUTTON, "archived"),
            (TASK_LOCAL_BUTTON, "local"),
            (TASK_CHILDREN_BUTTON, "children"),
            (TASK_DUPLICATE_BUTTON, "duplicates"),
            (TASK_PROJECT_BUTTON, "project"),
        ];
        for (widget, _) in initial_checks {
            let _ = click_message(&state, widget);
        }
        let selected = session(
            "task-1",
            "任务",
            cleaner_core::SessionStatus::Active,
            "C:/work",
            None,
        );
        state.report.sessions.push(selected.clone());
        state.selected_session = Some(ZsTableRowId::new(1));
        state.analysis = Some(SessionAnalysis {
            session: selected,
            related_session_ids: vec![],
            related_transcript_bytes: 0,
            project_related_session_ids: vec!["task-1".to_string()],
            duplicate_title_session_ids: vec!["task-1".to_string()],
            project_transcript_bytes: 0,
            resources: vec![],
            profile: RetentionProfile::ResultsAndSource,
            analyzed_bytes: 0,
            truncated: false,
            warnings: vec![],
        });
        for widget in [
            RESULTS_ONLY_BUTTON,
            RESULTS_SOURCE_BUTTON,
            DEVELOPMENT_BUTTON,
            CONVERSATION_ONLY_BUTTON,
            PREVIEW_BUTTON,
            EXECUTE_BUTTON,
            BACK_TO_TASKS_BUTTON,
        ] {
            let _ = click_message(&state, widget);
        }
    }

    fn session(
        id: &str,
        title: &str,
        status: cleaner_core::SessionStatus,
        cwd: &str,
        parent_id: Option<&str>,
    ) -> cleaner_core::SessionSummary {
        cleaner_core::SessionSummary {
            id: id.to_string(),
            title: title.to_string(),
            status,
            updated_at: None,
            started_at: None,
            cwd: Some(PathBuf::from(cwd)),
            source: None,
            parent_id: parent_id.map(str::to_string),
            transcript_paths: vec![],
            transcript_bytes: 0,
        }
    }

    #[test]
    fn task_filters_distinguish_project_duplicates_and_children() {
        let mut state = empty_state(Page::Conversations);
        state.report.sessions = vec![
            session(
                "root-0001",
                "整理投标成果",
                cleaner_core::SessionStatus::Active,
                "E:/投标",
                None,
            ),
            session(
                "child-01",
                "整理投标成果",
                cleaner_core::SessionStatus::Archived,
                "E:/投标",
                Some("root-0001"),
            ),
            session(
                "other-01",
                "其他工作",
                cleaner_core::SessionStatus::Orphaned,
                "E:/其他",
                None,
            ),
        ];
        state.report.sessions[1].source = Some(
            serde_json::json!({
                "subagent": {
                    "thread_spawn": {
                        "agent_nickname": "Peirce",
                        "agent_path": "/root/audit"
                    }
                }
            })
            .to_string(),
        );
        state.selected_session = Some(ZsTableRowId::new(1));
        state.project_anchor = Some("root-0001".to_string());

        state.task_filter = TaskFilter::SameProject;
        assert_eq!(visible_sessions(&state).len(), 2);
        state.task_filter = TaskFilter::Duplicates;
        assert_eq!(visible_sessions(&state).len(), 2);
        assert!(
            task_relation_label(&state.report, &state.report.sessions[0]).starts_with("主任务")
        );
        assert!(task_display_title(&state.report.sessions[1]).starts_with("子任务 Peirce · audit"));
    }

    #[test]
    fn missing_local_file_filter_uses_transcript_presence() {
        let mut state = empty_state(Page::Conversations);
        let official_only = session(
            "official-only",
            "官方目录任务",
            cleaner_core::SessionStatus::Active,
            "C:/work",
            None,
        );
        let mut local = session(
            "local-transcript",
            "已有本地记录",
            cleaner_core::SessionStatus::Active,
            "C:/work",
            None,
        );
        local
            .transcript_paths
            .push(PathBuf::from("C:/codex/sessions/local.jsonl"));
        state.report.sessions = vec![official_only, local];
        state.task_filter = TaskFilter::Local;

        let visible = visible_sessions(&state);

        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].id, "official-only");
    }

    #[test]
    fn every_storage_toolbar_button_dispatches() {
        let state = empty_state(Page::Storage);
        for widget in [
            STORAGE_REFRESH_BUTTON,
            STORAGE_SAFE_RULES_BUTTON,
            STORAGE_PREVIEW_BUTTON,
            STORAGE_EXECUTE_BUTTON,
            NAV_CONVERSATIONS_BUTTON,
        ] {
            let _ = click_message(&state, widget);
        }
    }

    #[test]
    fn overview_navigation_history_and_settings_controls_dispatch() {
        let mut overview = empty_state(Page::Overview);
        overview.report.sessions.push(session(
            "task-old",
            "已归档的测试任务",
            cleaner_core::SessionStatus::Archived,
            "C:/work/demo",
            None,
        ));
        overview.overview_selected_session_id = Some("task-old".to_string());
        for widget in [
            START_SCAN_BUTTON,
            HOME_TASKS_BUTTON,
            HOME_STORAGE_BUTTON,
            OVERVIEW_OPEN_BUTTON,
            OVERVIEW_ANALYZE_BUTTON,
            NAV_OVERVIEW_BUTTON,
            NAV_CONVERSATIONS_BUTTON,
            NAV_STORAGE_BUTTON,
            NAV_HISTORY_BUTTON,
            NAV_SETTINGS_BUTTON,
        ] {
            let _ = click_message(&overview, widget);
        }

        let history = empty_state(Page::History);
        for widget in [
            HISTORY_PREVIOUS_BUTTON,
            HISTORY_NEXT_BUTTON,
            HISTORY_REFRESH_BUTTON,
        ] {
            let _ = click_message(&history, widget);
        }

        let settings = empty_state(Page::Settings);
        for widget in [SETTINGS_TASK_SCAN_BUTTON, SETTINGS_STORAGE_SCAN_BUTTON] {
            let _ = click_message(&settings, widget);
        }
    }

    #[test]
    fn overview_recommendations_skip_recent_active_tasks() {
        let mut state = empty_state(Page::Overview);
        let mut recent = session(
            "recent",
            "近期活跃任务",
            cleaner_core::SessionStatus::Active,
            "C:/work/recent",
            None,
        );
        recent.updated_at = Some(Utc::now() - chrono::Duration::days(1));
        recent.transcript_bytes = 512 * 1024 * 1024;
        let mut archived = session(
            "archived",
            "历史归档任务",
            cleaner_core::SessionStatus::Archived,
            "C:/work/archived",
            None,
        );
        archived.updated_at = Some(Utc::now() - chrono::Duration::days(30));
        state.report.sessions = vec![recent, archived];

        let indices = overview_session_indices(&state.report);

        assert_eq!(indices, vec![1]);
    }

    #[test]
    fn history_loader_normalizes_task_and_storage_receipts() {
        let root = tempfile::tempdir().unwrap();
        let report = ScanReport {
            codex_home: PathBuf::from("C:/codex"),
            sessions: vec![session(
                "task-1",
                "可识别任务名",
                cleaner_core::SessionStatus::Active,
                "C:/work",
                None,
            )],
            transcript_bytes: 0,
            malformed_index_lines: 0,
            warnings: vec![],
        };
        let task_path = root.path().join("task.json");
        fs::write(
            &task_path,
            serde_json::to_vec(&serde_json::json!({
                "operation_id": "op-1",
                "session_id": "task-1",
                "finished_at": "2026-08-08T01:00:00Z",
                "status": "completed",
                "recycled_paths": ["C:/temp/a"],
                "official_thread_deleted": true,
                "error": null,
                "journal_path": "C:/untrusted.json"
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            root.path().join("storage.json"),
            serde_json::to_vec(&serde_json::json!({
                "created_at": "2026-08-08T02:00:00Z",
                "recycled_paths": ["C:/cache"],
                "failed_paths": [["C:/locked", "in use"]],
                "journal_path": "C:/untrusted-storage.json"
            }))
            .unwrap(),
        )
        .unwrap();

        let history = load_cleanup_history_from(root.path(), &report);

        assert_eq!(history.len(), 2);
        assert_eq!(history[0].kind, "存储清理");
        assert_eq!(history[0].result, "部分完成");
        let task = history
            .iter()
            .find(|entry| entry.kind == "任务清理")
            .unwrap();
        assert_eq!(task.summary, "可识别任务名");
        assert_eq!(task.journal_path, task_path);
        assert!(task.permanent_thread_deleted);
    }
}
