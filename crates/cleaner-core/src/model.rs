use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Active,
    Archived,
    Orphaned,
}

impl SessionStatus {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Active => "活跃",
            Self::Archived => "已归档",
            Self::Orphaned => "仅本地",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub status: SessionStatus,
    pub updated_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub cwd: Option<PathBuf>,
    pub source: Option<String>,
    pub parent_id: Option<String>,
    pub transcript_paths: Vec<PathBuf>,
    pub transcript_bytes: u64,
}

impl SessionSummary {
    pub fn updated_label(&self) -> String {
        self.updated_at
            .map(|value| value.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|| "未知".to_string())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanReport {
    pub codex_home: PathBuf,
    pub sessions: Vec<SessionSummary>,
    pub transcript_bytes: u64,
    pub malformed_index_lines: usize,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    Conversation,
    StateReference,
    TaskState,
    ShellSnapshot,
    Worktree,
    ResultArtifact,
    SourceChange,
    SupportLibrary,
    Cache,
    Log,
    Temporary,
    WorkspaceFile,
    ExternalReference,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactStage {
    Intermediate,
    Final,
    Undetermined,
}

impl ArtifactStage {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Intermediate => "过程成果",
            Self::Final => "最终成果",
            Self::Undetermined => "待判定成果",
        }
    }
}

impl ResourceKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Conversation => "对话记录",
            Self::StateReference => "状态索引",
            Self::TaskState => "任务运行状态",
            Self::ShellSnapshot => "Shell 快照",
            Self::Worktree => "Worktree",
            Self::ResultArtifact => "成果",
            Self::SourceChange => "源码改动",
            Self::SupportLibrary => "支持库",
            Self::Cache => "缓存",
            Self::Log => "日志",
            Self::Temporary => "临时文件",
            Self::WorkspaceFile => "工作文件",
            Self::ExternalReference => "外部引用",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ownership {
    Exclusive,
    Shared,
    Global,
    Unknown,
}

impl Ownership {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Exclusive => "专属",
            Self::Shared => "共享",
            Self::Global => "全局",
            Self::Unknown => "未知",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Weak,
    Likely,
    Confirmed,
}

impl Confidence {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Weak => "弱",
            Self::Likely => "可能",
            Self::Confirmed => "已确认",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResourceLocation {
    Path { path: PathBuf },
    StateRecord { surface: PathBuf, key: String },
}

impl ResourceLocation {
    pub fn display(&self) -> String {
        match self {
            Self::Path { path } => path.display().to_string(),
            Self::StateRecord { surface, key } => format!("{} # {}", surface.display(), key),
        }
    }

    pub fn path(&self) -> Option<&std::path::Path> {
        match self {
            Self::Path { path } => Some(path),
            Self::StateRecord { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    pub source: String,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceAction {
    Keep,
    Delete,
    Review,
    StorageReview,
    Protected,
}

impl ResourceAction {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Keep => "保留",
            Self::Delete => "待清理",
            Self::Review => "需确认",
            Self::StorageReview => "转到存储清理",
            Self::Protected => "受保护",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceNode {
    pub id: u64,
    pub location: ResourceLocation,
    pub kind: ResourceKind,
    pub artifact_stage: Option<ArtifactStage>,
    pub artifact_reason: Option<String>,
    pub size: u64,
    pub size_complete: bool,
    pub ownership: Ownership,
    pub confidence: Confidence,
    pub evidence: Vec<Evidence>,
    pub recommended_action: ResourceAction,
    pub user_override: Option<ResourceAction>,
    pub action: ResourceAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageCategory {
    Conversation,
    Backup,
    Cache,
    Temporary,
    Diagnostic,
    Runtime,
    Extension,
    UserAsset,
    FinalArtifact,
    IntermediateArtifact,
    Source,
    SupportLibrary,
    State,
}

impl StorageCategory {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Conversation => "对话历史",
            Self::Backup => "备份",
            Self::Cache => "可重建缓存",
            Self::Temporary => "临时文件",
            Self::Diagnostic => "诊断日志",
            Self::Runtime => "运行组件",
            Self::Extension => "插件与技能",
            Self::UserAsset => "用户数据与附件",
            Self::FinalArtifact => "最终成果",
            Self::IntermediateArtifact => "过程成果",
            Self::Source => "项目源码",
            Self::SupportLibrary => "支持库",
            Self::State => "配置与状态",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageSafety {
    SafeAfterExit,
    Review,
    Protected,
}

impl StorageSafety {
    pub const fn label(self) -> &'static str {
        match self {
            Self::SafeAfterExit => "退出 Codex 后安全",
            Self::Review => "需复核",
            Self::Protected => "受保护，不会清理",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageAction {
    Keep,
    Clean,
    Review,
}

impl StorageAction {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Keep => "保留",
            Self::Clean => "待清理",
            Self::Review => "需确认",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageItem {
    pub id: u64,
    pub label: String,
    pub path: PathBuf,
    pub category: StorageCategory,
    pub safety: StorageSafety,
    pub size: u64,
    pub file_count: u64,
    pub newest_at: Option<DateTime<Utc>>,
    pub stale_days: Option<i64>,
    pub reason: String,
    pub action: StorageAction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageReport {
    pub roots: Vec<PathBuf>,
    pub items: Vec<StorageItem>,
    pub total_bytes: u64,
    pub warnings: Vec<String>,
}

impl StorageReport {
    pub fn clean_bytes(&self) -> u64 {
        self.items
            .iter()
            .filter(|item| item.action == StorageAction::Clean)
            .map(|item| item.size)
            .sum()
    }

    pub fn safe_candidate_bytes(&self) -> u64 {
        self.items
            .iter()
            .filter(|item| {
                item.safety == StorageSafety::SafeAfterExit
                    && item.stale_days.is_some_and(|days| days >= 7)
                    && matches!(
                        item.category,
                        StorageCategory::Cache | StorageCategory::Temporary
                    )
            })
            .map(|item| item.size)
            .sum()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionProfile {
    ResultsOnly,
    ResultsAndSource,
    DevelopmentEnvironment,
    ConversationOnly,
}

impl RetentionProfile {
    pub const fn label(self) -> &'static str {
        match self {
            Self::ResultsOnly => "只保留成果",
            Self::ResultsAndSource => "保留成果和源码",
            Self::DevelopmentEnvironment => "保留开发环境",
            Self::ConversationOnly => "只删除对话",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionAnalysis {
    pub session: SessionSummary,
    pub related_session_ids: Vec<String>,
    pub related_transcript_bytes: u64,
    pub project_related_session_ids: Vec<String>,
    pub duplicate_title_session_ids: Vec<String>,
    pub project_transcript_bytes: u64,
    pub resources: Vec<ResourceNode>,
    pub profile: RetentionProfile,
    pub analyzed_bytes: u64,
    pub truncated: bool,
    pub warnings: Vec<String>,
}

impl SessionAnalysis {
    pub fn delete_bytes(&self) -> u64 {
        self.resources
            .iter()
            .filter(|resource| resource.action == ResourceAction::Delete)
            .map(|resource| resource.size)
            .sum()
    }

    pub fn keep_bytes(&self) -> u64 {
        self.resources
            .iter()
            .filter(|resource| resource.action == ResourceAction::Keep)
            .map(|resource| resource.size)
            .sum()
    }

    pub fn review_count(&self) -> usize {
        self.resources
            .iter()
            .filter(|resource| {
                matches!(
                    resource.action,
                    ResourceAction::Review
                        | ResourceAction::StorageReview
                        | ResourceAction::Protected
                )
            })
            .count()
    }

    pub fn storage_review_count(&self) -> usize {
        self.resources
            .iter()
            .filter(|resource| resource.action == ResourceAction::StorageReview)
            .count()
    }
}

pub fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let value = bytes as f64;
    if value >= GB {
        format!("{:.2} GB", value / GB)
    } else if value >= MB {
        format!("{:.2} MB", value / MB)
    } else if value >= KB {
        format!("{:.1} KB", value / KB)
    } else {
        format!("{bytes} B")
    }
}
