use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufRead, BufReader, Read},
    path::{Component, Path, PathBuf},
    sync::OnceLock,
};

use regex::Regex;
use serde_json::Value;
use walkdir::WalkDir;

use crate::{
    ArtifactStage, Confidence, Evidence, Ownership, ResourceAction, ResourceKind, ResourceLocation,
    ResourceNode, RetentionProfile, ScanReport, SessionAnalysis, SessionSummary,
};

#[derive(Debug, Clone, Copy)]
pub struct AnalysisOptions {
    pub max_transcript_bytes: u64,
    pub max_resource_entries: usize,
    pub max_directory_entries: usize,
}

impl Default for AnalysisOptions {
    fn default() -> Self {
        Self {
            max_transcript_bytes: 1024 * 1024 * 1024,
            max_resource_entries: 10_000,
            max_directory_entries: 100_000,
        }
    }
}

pub fn analyze_session(
    report: &ScanReport,
    session_id: &str,
    options: AnalysisOptions,
) -> Result<SessionAnalysis, String> {
    let session = report
        .sessions
        .iter()
        .find(|value| value.id == session_id)
        .cloned()
        .ok_or_else(|| format!("session not found: {session_id}"))?;
    let related_session_ids = descendant_session_ids(report, session_id);
    let related_transcript_bytes = report
        .sessions
        .iter()
        .filter(|candidate| related_session_ids.contains(&candidate.id))
        .map(|candidate| candidate.transcript_bytes)
        .sum();
    let analysis_sessions = std::iter::once(&session)
        .chain(
            report
                .sessions
                .iter()
                .filter(|candidate| related_session_ids.contains(&candidate.id)),
        )
        .collect::<Vec<_>>();
    let mut resources = BTreeMap::<String, ResourceNode>::new();
    let mut warnings = Vec::new();
    let mut analyzed_bytes = 0_u64;
    let mut truncated = false;

    for analyzed_session in &analysis_sessions {
        for path in &analyzed_session.transcript_paths {
            insert_path_resource(
                &mut resources,
                path.clone(),
                ResourceKind::Conversation,
                Ownership::Exclusive,
                Confidence::Confirmed,
                "Codex rollout",
                if analyzed_session.id == session_id {
                    "文件名与所选任务 UUID 精确匹配"
                } else {
                    "文件名与将一并删除的子任务 UUID 精确匹配"
                },
                options.max_directory_entries,
            );
        }
    }

    for surface in ["session_index.jsonl", "state_5.sqlite", "logs_2.sqlite"] {
        let path = report.codex_home.join(surface);
        if path.is_file() {
            insert_state_resource(&mut resources, path, session_id);
        }
    }
    let task_ids = analysis_sessions
        .iter()
        .map(|value| value.id.as_str())
        .collect::<BTreeSet<_>>();
    collect_process_manager_state(&report.codex_home, &task_ids, &mut resources);

    let mut candidates = BTreeSet::new();
    let mut output_observed_candidates = BTreeSet::new();
    let mut confirmed_changes = BTreeSet::new();
    let mut created_changes = BTreeSet::new();
    'sessions: for analyzed_session in &analysis_sessions {
        for path in &analyzed_session.transcript_paths {
            let remaining = options.max_transcript_bytes.saturating_sub(analyzed_bytes);
            if remaining == 0 {
                truncated = true;
                break 'sessions;
            }
            match extract_paths_from_rollout(
                path,
                remaining,
                analyzed_session.cwd.as_deref().or(session.cwd.as_deref()),
                &mut candidates,
                &mut output_observed_candidates,
                &mut confirmed_changes,
                &mut created_changes,
            ) {
                Ok((bytes, was_truncated)) => {
                    analyzed_bytes = analyzed_bytes.saturating_add(bytes);
                    truncated |= was_truncated;
                }
                Err(error) => warnings.push(format!("{}: {error}", path.display())),
            }
        }
    }

    if let Some(cwd) = session.cwd.as_ref() {
        if is_managed_worktree(cwd, &report.codex_home) {
            insert_path_resource(
                &mut resources,
                cwd.clone(),
                ResourceKind::Worktree,
                Ownership::Exclusive,
                Confidence::Confirmed,
                "session_meta.cwd",
                "会话工作目录位于 Codex 管理的 worktrees 根目录",
                options.max_directory_entries,
            );
        }
    }

    for path in &confirmed_changes {
        candidates.insert(path.clone());
    }

    if candidates.len() > options.max_resource_entries {
        truncated = true;
        warnings.push(format!(
            "识别到 {} 个候选路径，仅分析前 {} 个；其余项目不会自动删除",
            candidates.len(),
            options.max_resource_entries
        ));
    }
    let mut external_reference_count = 0_usize;
    let mut omitted_external_references = 0_usize;
    for path in candidates.into_iter().take(options.max_resource_entries) {
        if session.cwd.as_deref() == Some(path.as_path())
            || !path.exists()
            || is_ignored_reference_path(&path, &report.codex_home)
        {
            continue;
        }
        let changed = confirmed_changes.contains(&path);
        let created = created_changes.contains(&path);
        let (kind, ownership, confidence, detail) = classify_path(
            &path,
            &report.codex_home,
            session.cwd.as_deref(),
            changed,
            created,
        );
        if kind == ResourceKind::ExternalReference {
            if external_reference_count >= options.max_resource_entries {
                omitted_external_references = omitted_external_references.saturating_add(1);
                continue;
            }
            external_reference_count = external_reference_count.saturating_add(1);
        }
        let detail = if created {
            "补丁完成事件确认本会话创建了该文件"
        } else {
            detail
        };
        insert_path_resource(
            &mut resources,
            path,
            kind,
            ownership,
            confidence,
            if changed {
                "patch_apply_end"
            } else {
                "tool event"
            },
            detail,
            options.max_directory_entries,
        );
    }
    if omitted_external_references > 0 {
        warnings.push(format!(
            "已省略 {omitted_external_references} 个只有弱引用证据的外部路径；它们不会进入清理计划"
        ));
    }

    let mut output_observed_candidates = output_observed_candidates
        .into_iter()
        .filter(|path| {
            path.exists()
                && session.cwd.as_deref() != Some(path.as_path())
                && !is_ignored_reference_path(path, &report.codex_home)
        })
        .collect::<Vec<_>>();
    output_observed_candidates.sort_by(|left, right| {
        observed_path_priority(left, session.cwd.as_deref())
            .cmp(&observed_path_priority(right, session.cwd.as_deref()))
            .then_with(|| path_key(left).cmp(&path_key(right)))
    });
    let max_output_observed_paths = options.max_resource_entries;
    if output_observed_candidates.len() > max_output_observed_paths {
        warnings.push(format!(
            "工具输出还报告了 {} 个现存路径，仅展示关联性最高的前 {} 个；这些弱证据路径均不会自动删除",
            output_observed_candidates.len(),
            max_output_observed_paths
        ));
    }
    for path in output_observed_candidates
        .into_iter()
        .take(max_output_observed_paths)
    {
        let (kind, _, _, _) = classify_path(
            &path,
            &report.codex_home,
            session.cwd.as_deref(),
            false,
            false,
        );
        insert_path_resource(
            &mut resources,
            path,
            kind,
            Ownership::Unknown,
            Confidence::Weak,
            "tool output observation",
            "工具输出曾报告该现存路径；这只证明任务观察或处理过它，不证明文件由任务创建，默认不会自动清理",
            options.max_directory_entries,
        );
    }

    collect_named_session_files(
        &report.codex_home,
        session_id,
        &mut resources,
        options.max_directory_entries,
    );
    for descendant_id in &related_session_ids {
        collect_named_session_files(
            &report.codex_home,
            descendant_id,
            &mut resources,
            options.max_directory_entries,
        );
    }
    collect_matching_diagnostic_logs(
        &report.codex_home,
        &session,
        &mut resources,
        options.max_directory_entries,
    );
    refine_ownership_for_shared_workspaces(&mut resources, report, &session, &related_session_ids);
    let project_related = report
        .sessions
        .iter()
        .filter(|value| same_project_session(&session, value))
        .collect::<Vec<_>>();
    let project_related_session_ids = project_related
        .iter()
        .map(|value| value.id.clone())
        .collect::<Vec<_>>();
    let project_transcript_bytes = project_related
        .iter()
        .map(|value| value.transcript_bytes)
        .sum();
    let title_key = normalized_title(&session.title);
    let duplicate_title_session_ids =
        if title_key.chars().count() >= 6 && !session.title.starts_with("本地任务 ") {
            report
                .sessions
                .iter()
                .filter(|value| normalized_title(&value.title) == title_key)
                .map(|value| value.id.clone())
                .collect()
        } else {
            vec![session.id.clone()]
        };

    let mut resources = resources.into_values().collect::<Vec<_>>();
    refine_artifact_series(&mut resources);
    for (index, resource) in resources.iter_mut().enumerate() {
        resource.id = (index + 1) as u64;
    }
    let mut analysis = SessionAnalysis {
        session,
        related_session_ids,
        related_transcript_bytes,
        project_related_session_ids,
        duplicate_title_session_ids,
        project_transcript_bytes,
        resources,
        profile: RetentionProfile::ResultsAndSource,
        analyzed_bytes,
        truncated,
        warnings,
    };
    apply_retention_profile(&mut analysis, RetentionProfile::ResultsAndSource);
    Ok(analysis)
}

fn is_ignored_reference_path(path: &Path, codex_home: &Path) -> bool {
    if path.parent().is_none() {
        return true;
    }
    let key = path_key(path);
    let components = lower_components(path);
    if components.iter().any(|component| {
        matches!(
            component.as_str(),
            "windows" | "program files" | "program files (x86)" | "package cache"
        )
    }) {
        return true;
    }
    let codex_root = path_key(codex_home);
    if key == codex_root || key.starts_with(&format!("{codex_root}/")) {
        let allowed = ["worktrees", "attachments"]
            .iter()
            .map(|folder| path_key(&codex_home.join(folder)))
            .any(|root| key == root || key.starts_with(&format!("{root}/")));
        return !allowed;
    }
    false
}

fn is_relevant_external_reference(path: &Path) -> bool {
    if path.is_dir() {
        return true;
    }
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "doc"
                    | "docx"
                    | "xls"
                    | "xlsx"
                    | "csv"
                    | "pdf"
                    | "ppt"
                    | "pptx"
                    | "txt"
                    | "md"
                    | "json"
                    | "toml"
                    | "yaml"
                    | "yml"
                    | "xml"
                    | "zip"
                    | "7z"
                    | "png"
                    | "jpg"
                    | "jpeg"
                    | "svg"
                    | "webp"
                    | "mp4"
                    | "webm"
                    | "mp3"
                    | "wav"
                    | "rtf"
                    | "docm"
                    | "xlsm"
                    | "pptm"
                    | "html"
                    | "css"
                    | "ini"
                    | "cfg"
                    | "log"
                    | "db"
                    | "sqlite"
                    | "exe"
                    | "dll"
                    | "wasm"
                    | "jar"
                    | "rs"
                    | "py"
                    | "js"
                    | "ts"
                    | "tsx"
                    | "jsx"
                    | "ps1"
                    | "bat"
                    | "cmd"
                    | "sh"
            )
        })
}

fn observed_path_priority(path: &Path, cwd: Option<&Path>) -> u8 {
    if cwd.is_some_and(|root| path.starts_with(root)) {
        0
    } else if lower_components(path)
        .iter()
        .any(|component| component == "codex操作目录")
    {
        1
    } else if path.is_file() && is_relevant_external_reference(path) {
        2
    } else {
        3
    }
}

pub fn apply_retention_profile(analysis: &mut SessionAnalysis, profile: RetentionProfile) {
    analysis.profile = profile;
    for resource in &mut analysis.resources {
        let recommended = desired_action(resource, profile);
        resource.recommended_action = recommended;
        resource.user_override = None;
        resource.action = recommended;
    }
}

fn desired_action(resource: &ResourceNode, profile: RetentionProfile) -> ResourceAction {
    if matches!(resource.ownership, Ownership::Shared | Ownership::Global) {
        return match resource.kind {
            ResourceKind::Conversation | ResourceKind::StateReference => ResourceAction::Delete,
            ResourceKind::Log | ResourceKind::Cache | ResourceKind::Temporary => {
                ResourceAction::StorageReview
            }
            _ => ResourceAction::Protected,
        };
    }
    if resource.ownership == Ownership::Unknown {
        return match profile {
            RetentionProfile::ResultsOnly => match resource.kind {
                ResourceKind::ResultArtifact
                    if resource.artifact_stage == Some(ArtifactStage::Intermediate) =>
                {
                    ResourceAction::Review
                }
                ResourceKind::ResultArtifact | ResourceKind::ExternalReference => {
                    ResourceAction::Keep
                }
                ResourceKind::SourceChange
                | ResourceKind::WorkspaceFile
                | ResourceKind::SupportLibrary
                | ResourceKind::Worktree
                | ResourceKind::Log
                | ResourceKind::Cache
                | ResourceKind::Temporary => ResourceAction::Review,
                _ => ResourceAction::Protected,
            },
            RetentionProfile::ResultsAndSource | RetentionProfile::DevelopmentEnvironment => {
                match resource.kind {
                    ResourceKind::ResultArtifact
                        if resource.artifact_stage == Some(ArtifactStage::Intermediate) =>
                    {
                        ResourceAction::Review
                    }
                    ResourceKind::ResultArtifact
                    | ResourceKind::SourceChange
                    | ResourceKind::WorkspaceFile
                    | ResourceKind::ExternalReference => ResourceAction::Keep,
                    ResourceKind::SupportLibrary
                    | ResourceKind::Worktree
                    | ResourceKind::Log
                    | ResourceKind::Cache
                    | ResourceKind::Temporary => ResourceAction::Review,
                    _ => ResourceAction::Protected,
                }
            }
            RetentionProfile::ConversationOnly => ResourceAction::Keep,
        };
    }
    match profile {
        RetentionProfile::ResultsOnly => match resource.kind {
            ResourceKind::ResultArtifact
                if resource.artifact_stage == Some(ArtifactStage::Intermediate) =>
            {
                ResourceAction::Delete
            }
            ResourceKind::ResultArtifact => ResourceAction::Keep,
            ResourceKind::SourceChange | ResourceKind::WorkspaceFile => ResourceAction::Review,
            ResourceKind::Worktree => ResourceAction::Review,
            _ => ResourceAction::Delete,
        },
        RetentionProfile::ResultsAndSource => match resource.kind {
            ResourceKind::ResultArtifact
                if resource.artifact_stage == Some(ArtifactStage::Intermediate) =>
            {
                ResourceAction::Review
            }
            ResourceKind::ResultArtifact
            | ResourceKind::SourceChange
            | ResourceKind::WorkspaceFile
            | ResourceKind::Worktree => ResourceAction::Keep,
            ResourceKind::SupportLibrary => ResourceAction::Review,
            _ => ResourceAction::Delete,
        },
        RetentionProfile::DevelopmentEnvironment => match resource.kind {
            ResourceKind::Conversation
            | ResourceKind::StateReference
            | ResourceKind::TaskState
            | ResourceKind::ShellSnapshot
            | ResourceKind::Log
            | ResourceKind::Cache
            | ResourceKind::Temporary => ResourceAction::Delete,
            _ => ResourceAction::Keep,
        },
        RetentionProfile::ConversationOnly => match resource.kind {
            ResourceKind::Conversation | ResourceKind::StateReference => ResourceAction::Delete,
            _ => ResourceAction::Keep,
        },
    }
}

fn insert_state_resource(
    resources: &mut BTreeMap<String, ResourceNode>,
    surface: PathBuf,
    session_id: &str,
) {
    let key = format!("state:{}#{session_id}", surface.display());
    resources.entry(key).or_insert(ResourceNode {
        id: 0,
        location: ResourceLocation::StateRecord {
            surface,
            key: session_id.to_string(),
        },
        kind: ResourceKind::StateReference,
        artifact_stage: None,
        artifact_reason: None,
        size: 0,
        size_complete: true,
        ownership: Ownership::Exclusive,
        confidence: Confidence::Confirmed,
        evidence: vec![Evidence {
            source: "Codex state surface".to_string(),
            detail: "按精确会话 UUID 删除记录，不删除整个共享文件".to_string(),
        }],
        recommended_action: ResourceAction::Delete,
        user_override: None,
        action: ResourceAction::Delete,
    });
}

fn collect_process_manager_state(
    codex_home: &Path,
    task_ids: &BTreeSet<&str>,
    resources: &mut BTreeMap<String, ResourceNode>,
) {
    let surface = codex_home
        .join("process_manager")
        .join("chat_processes.json");
    let Ok(bytes) = fs::read(&surface) else {
        return;
    };
    let Ok(Value::Array(entries)) = serde_json::from_slice::<Value>(&bytes) else {
        return;
    };
    let matched_ids = entries
        .iter()
        .filter_map(|entry| entry.get("conversationId").and_then(Value::as_str))
        .filter(|id| task_ids.contains(id))
        .collect::<BTreeSet<_>>();
    for session_id in matched_ids {
        let key = format!("task-state:{}#{session_id}", surface.display());
        resources.entry(key).or_insert(ResourceNode {
            id: 0,
            location: ResourceLocation::StateRecord {
                surface: surface.clone(),
                key: session_id.to_string(),
            },
            kind: ResourceKind::TaskState,
            artifact_stage: None,
            artifact_reason: None,
            size: 0,
            size_complete: true,
            ownership: Ownership::Global,
            confidence: Confidence::Confirmed,
            evidence: vec![Evidence {
                source: "process_manager".to_string(),
                detail: "全局进程状态中存在与任务 UUID 精确匹配的记录；由官方任务删除流程维护"
                    .to_string(),
            }],
            recommended_action: ResourceAction::Protected,
            user_override: None,
            action: ResourceAction::Protected,
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn insert_path_resource(
    resources: &mut BTreeMap<String, ResourceNode>,
    path: PathBuf,
    kind: ResourceKind,
    ownership: Ownership,
    confidence: Confidence,
    evidence_source: &str,
    evidence_detail: &str,
    max_entries: usize,
) {
    let key = path_key(&path);
    let (artifact_stage, artifact_reason) = if kind == ResourceKind::ResultArtifact {
        let (stage, reason) = classify_artifact_stage(&path);
        (Some(stage), Some(reason.to_string()))
    } else {
        (None, None)
    };
    let (size, complete) = if path.is_dir() && ownership != Ownership::Exclusive {
        (0, false)
    } else {
        path_size(&path, max_entries)
    };
    resources
        .entry(key)
        .and_modify(|resource| {
            let stronger_evidence = confidence > resource.confidence;
            let more_specific = resource.kind != ResourceKind::ResultArtifact
                && resource_kind_specificity(kind) > resource_kind_specificity(resource.kind);
            if stronger_evidence {
                resource.confidence = confidence;
                resource.ownership = ownership;
            }
            if !resource
                .evidence
                .iter()
                .any(|value| value.source == evidence_source && value.detail == evidence_detail)
            {
                let evidence = Evidence {
                    source: evidence_source.to_string(),
                    detail: evidence_detail.to_string(),
                };
                if stronger_evidence || more_specific {
                    resource.evidence.insert(0, evidence);
                } else {
                    resource.evidence.push(evidence);
                }
            }
            if more_specific {
                resource.kind = kind;
                resource.ownership = ownership;
            }
            if resource.artifact_stage.is_none() && artifact_stage.is_some() {
                resource.artifact_stage = artifact_stage;
                resource.artifact_reason = artifact_reason.clone();
            }
        })
        .or_insert(ResourceNode {
            id: 0,
            location: ResourceLocation::Path { path },
            kind,
            artifact_stage,
            artifact_reason,
            size,
            size_complete: complete,
            ownership,
            confidence,
            evidence: vec![Evidence {
                source: evidence_source.to_string(),
                detail: evidence_detail.to_string(),
            }],
            recommended_action: ResourceAction::Keep,
            user_override: None,
            action: ResourceAction::Keep,
        });
}

fn extract_paths_from_rollout(
    path: &Path,
    max_bytes: u64,
    cwd: Option<&Path>,
    paths: &mut BTreeSet<PathBuf>,
    output_observed_paths: &mut BTreeSet<PathBuf>,
    confirmed_changes: &mut BTreeSet<PathBuf>,
    created_changes: &mut BTreeSet<PathBuf>,
) -> Result<(u64, bool), String> {
    let metadata_len = fs::metadata(path).map(|value| value.len()).unwrap_or(0);
    let reader: Box<dyn Read> = if path.extension().and_then(|value| value.to_str()) == Some("zst")
    {
        let file = File::open(path).map_err(|error| error.to_string())?;
        Box::new(zstd::stream::read::Decoder::new(file).map_err(|error| error.to_string())?)
    } else {
        Box::new(File::open(path).map_err(|error| error.to_string())?)
    };
    let mut reader = BufReader::new(reader.take(max_bytes));
    let mut bytes = 0_u64;
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader
            .read_line(&mut line)
            .map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        bytes = bytes.saturating_add(read as u64);
        if line.contains("function_call")
            || line.contains("tool_call")
            || line.contains("command_execution_output")
            || line.contains("computer_output")
            || line.contains("tool_result")
            || line.contains("patch_apply_end")
            || line.contains("session_meta")
            || line.contains("turn_context")
        {
            if let Ok(value) = serde_json::from_str::<Value>(&line) {
                if is_tool_output_event(&value) {
                    collect_paths_from_json(&value, output_observed_paths);
                } else {
                    collect_paths_from_json(&value, paths);
                }
                collect_confirmed_changes(&value, cwd, confirmed_changes);
                collect_created_changes(&value, cwd, created_changes);
                collect_command_created_paths(&value, cwd, created_changes);
            }
        }
    }
    Ok((bytes, bytes >= max_bytes && metadata_len > max_bytes))
}

fn is_tool_output_event(value: &Value) -> bool {
    let payload = value.get("payload").unwrap_or(value);
    payload
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|event_type| {
            event_type.ends_with("_output")
                || matches!(
                    event_type,
                    "command_execution_output" | "computer_output" | "tool_result"
                )
        })
}

fn collect_created_changes(value: &Value, cwd: Option<&Path>, paths: &mut BTreeSet<PathBuf>) {
    let payload = value.get("payload").unwrap_or(value);
    if payload.get("type").and_then(Value::as_str) != Some("patch_apply_end")
        || payload.get("success").and_then(Value::as_bool) == Some(false)
    {
        return;
    }
    let Some(changes) = payload.get("changes").and_then(Value::as_object) else {
        return;
    };
    for (name, change) in changes {
        if change.get("type").and_then(Value::as_str) != Some("add") {
            continue;
        }
        if let Some(path) = resolve_existing_path(name, cwd) {
            paths.insert(path);
        }
    }
}

fn collect_command_created_paths(value: &Value, cwd: Option<&Path>, paths: &mut BTreeSet<PathBuf>) {
    let payload = value.get("payload").unwrap_or(value);
    let event_type = payload
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if (!event_type.contains("tool_call") && event_type != "function_call")
        || event_type.ends_with("_output")
    {
        return;
    }
    let mut strings = Vec::new();
    collect_json_strings(payload, &mut strings);
    for value in strings {
        for regex in [created_path_flag_regex(), created_path_call_regex()] {
            for captures in regex.captures_iter(value) {
                let Some(candidate) = captures.get(1) else {
                    continue;
                };
                if let Some(path) = resolve_existing_path(candidate.as_str(), cwd) {
                    paths.insert(path);
                }
            }
        }
    }
}

fn collect_json_strings<'a>(value: &'a Value, strings: &mut Vec<&'a str>) {
    match value {
        Value::String(value) => strings.push(value),
        Value::Array(values) => {
            for value in values {
                collect_json_strings(value, strings);
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                collect_json_strings(value, strings);
            }
        }
        _ => {}
    }
}

fn created_path_flag_regex() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    VALUE.get_or_init(|| {
        Regex::new(
            r#"(?i)(?:-destination|-outfile|output[_-]?path|destination)\s*(?:=|:)?\s*["']([a-z]:[\\/][^"'\r\n]+)["']"#,
        )
        .expect("created path flag regex")
    })
}

fn created_path_call_regex() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    VALUE.get_or_init(|| {
        Regex::new(
            r#"(?i)(?:saveas2?|save_as|fs::write|file::create|writefilesync)\s*\(\s*["']([a-z]:[\\/][^"'\r\n]+)["']"#,
        )
        .expect("created path call regex")
    })
}

fn collect_confirmed_changes(value: &Value, cwd: Option<&Path>, paths: &mut BTreeSet<PathBuf>) {
    let payload = value.get("payload").unwrap_or(value);
    if payload.get("type").and_then(Value::as_str) != Some("patch_apply_end")
        || payload.get("success").and_then(Value::as_bool) == Some(false)
    {
        return;
    }
    let Some(changes) = payload.get("changes").and_then(Value::as_object) else {
        return;
    };
    for value in changes.keys() {
        if let Some(path) = resolve_existing_path(value, cwd) {
            paths.insert(path);
        }
    }
}

fn resolve_existing_path(value: &str, cwd: Option<&Path>) -> Option<PathBuf> {
    let path = trim_candidate(value);
    let path = if path.is_absolute() {
        path
    } else {
        cwd?.join(path)
    };
    path.exists().then_some(path)
}

fn collect_paths_from_json(value: &Value, paths: &mut BTreeSet<PathBuf>) {
    match value {
        Value::String(value) => collect_paths_from_text(value, paths),
        Value::Array(values) => {
            for value in values {
                collect_paths_from_json(value, paths);
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                collect_paths_from_json(value, paths);
            }
        }
        _ => {}
    }
}

fn collect_paths_from_text(value: &str, paths: &mut BTreeSet<PathBuf>) {
    let normalized = value.replace("\\\\", "\\");
    if looks_like_absolute_path(&normalized) {
        let path = trim_candidate(&normalized);
        if path.exists() {
            paths.insert(path);
        }
    }
    for captures in quoted_path_regex().captures_iter(&normalized) {
        if let Some(value) = captures.get(1) {
            let path = trim_candidate(value.as_str());
            if path.exists() {
                paths.insert(path);
            }
        }
    }
    for captures in bare_path_regex().captures_iter(&normalized) {
        if let Some(value) = captures.get(1) {
            let path = trim_candidate(value.as_str());
            if path.exists() {
                paths.insert(path);
            }
        }
    }
}

fn quoted_path_regex() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    VALUE.get_or_init(|| Regex::new(r#"(?i)[\"']([a-z]:[\\/][^\"']+)[\"']"#).unwrap())
}

fn bare_path_regex() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    VALUE.get_or_init(|| Regex::new(r#"(?i)\b([a-z]:[\\/][^\s\"'<>|;,]+)"#).unwrap())
}

fn looks_like_absolute_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() > 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
        && !value.contains('\n')
}

fn trim_candidate(value: &str) -> PathBuf {
    let value = value.trim().trim_matches(|character: char| {
        matches!(character, '"' | '\'' | ')' | ']' | '}' | ',' | ';')
    });
    PathBuf::from(value)
}

fn classify_path(
    path: &Path,
    codex_home: &Path,
    cwd: Option<&Path>,
    git_changed: bool,
    created_by_task: bool,
) -> (ResourceKind, Ownership, Confidence, &'static str) {
    let components = cwd
        .and_then(|root| path.strip_prefix(root).ok())
        .map(lower_components)
        .unwrap_or_else(|| lower_components(path));
    let workspace_ownership = || {
        if created_by_task && !path.starts_with(codex_home) {
            Ownership::Exclusive
        } else {
            ownership_for_workspace(path, codex_home, cwd)
        }
    };
    if path.starts_with(codex_home.join("attachments")) {
        return (
            ResourceKind::WorkspaceFile,
            Ownership::Shared,
            Confidence::Confirmed,
            "任务记录精确引用了 Codex 附件副本；附件目录使用随机 UUID，不能按任务名直接删除",
        );
    }
    if components
        .iter()
        .any(|value| matches!(value.as_str(), ".cache" | "cache" | "caches" | "gpu_cache"))
    {
        return (
            ResourceKind::Cache,
            if path.starts_with(codex_home) {
                Ownership::Global
            } else {
                workspace_ownership()
            },
            Confidence::Likely,
            "路径位于缓存目录",
        );
    }
    if components
        .iter()
        .any(|value| matches!(value.as_str(), ".tmp" | "tmp" | "temp" | "temporary"))
    {
        return (
            ResourceKind::Temporary,
            workspace_ownership(),
            Confidence::Likely,
            "路径位于临时目录；即使文件名含 final，也不据此认定为最终成果",
        );
    }
    if path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| matches!(value.to_ascii_lowercase().as_str(), "log" | "trace" | "etl"))
    {
        return (
            ResourceKind::Log,
            workspace_ownership(),
            Confidence::Likely,
            "扩展名表明这是日志或跟踪文件",
        );
    }
    if !git_changed
        && (components.iter().any(|value| {
            matches!(
                value.as_str(),
                "node_modules" | ".venv" | "venv" | "vendor" | "packages"
            )
        }) || components.last().is_some_and(|value| value == "target"))
    {
        return (
            ResourceKind::SupportLibrary,
            workspace_ownership(),
            Confidence::Likely,
            "路径位于依赖或构建支持目录",
        );
    }
    if is_result_artifact(path, git_changed || created_by_task) {
        return (
            ResourceKind::ResultArtifact,
            workspace_ownership(),
            if git_changed {
                Confidence::Confirmed
            } else {
                Confidence::Likely
            },
            if git_changed {
                "补丁事件确认本会话改动了该成果文件"
            } else {
                "文件类型和路径符合可交付成果特征"
            },
        );
    }
    if git_changed {
        return (
            ResourceKind::SourceChange,
            workspace_ownership(),
            Confidence::Confirmed,
            "补丁完成事件精确记录了本会话改动的文件",
        );
    }
    if cwd.is_some_and(|root| path.starts_with(root)) {
        return (
            ResourceKind::WorkspaceFile,
            workspace_ownership(),
            Confidence::Likely,
            "会话工具曾引用该工作文件，不等于由本会话创建",
        );
    }
    (
        ResourceKind::ExternalReference,
        Ownership::Unknown,
        Confidence::Weak,
        "会话工具曾引用该外部路径，不能证明归属",
    )
}

fn ownership_for_workspace(path: &Path, codex_home: &Path, cwd: Option<&Path>) -> Ownership {
    if path.starts_with(codex_home.join("worktrees")) {
        Ownership::Exclusive
    } else if path.starts_with(codex_home) {
        Ownership::Global
    } else if cwd
        .is_some_and(|root| is_managed_worktree(root, codex_home) && path.starts_with(root))
    {
        Ownership::Exclusive
    } else {
        Ownership::Unknown
    }
}

fn is_managed_worktree(path: &Path, codex_home: &Path) -> bool {
    path.starts_with(codex_home.join("worktrees"))
}

fn is_result_artifact(path: &Path, strong_creation_evidence: bool) -> bool {
    let components = lower_components(path);
    let explicit_output_location = components.iter().any(|value| {
        matches!(
            value.as_str(),
            "output"
                | "outputs"
                | "artifact"
                | "artifacts"
                | "export"
                | "exports"
                | "dist"
                | "codex操作目录"
                | "成果"
        )
    });
    (strong_creation_evidence || explicit_output_location)
        && path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| {
                matches!(
                    value.to_ascii_lowercase().as_str(),
                    "docx"
                        | "xlsx"
                        | "xls"
                        | "pdf"
                        | "pptx"
                        | "csv"
                        | "zip"
                        | "7z"
                        | "png"
                        | "jpg"
                        | "jpeg"
                        | "svg"
                        | "mp4"
                        | "webm"
                        | "mp3"
                        | "wav"
                )
            })
}

fn classify_artifact_stage(path: &Path) -> (ArtifactStage, &'static str) {
    let components = lower_components(path);
    let filename = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let quality_check_name = filename
        .split(|character: char| !character.is_alphanumeric())
        .any(|token| matches!(token, "qa" | "audit" | "verify" | "verification"));
    if quality_check_name
        || components.iter().any(|value| {
            matches!(
                value.as_str(),
                "draft"
                    | "drafts"
                    | "preview"
                    | "previews"
                    | "render"
                    | "renders"
                    | "smoke"
                    | "test"
                    | "tests"
                    | "intermediate"
                    | "scratch"
                    | "草稿"
                    | "预览"
                    | "过程"
                    | "中间"
                    | "测试"
                    | "qa"
                    | "audit"
                    | "audits"
                    | "validation"
                    | "verification"
            )
        })
        || [
            "draft", "preview", "smoke", "test", "temp", "tmp", "cached", "source", "work",
            "check", "backup", "before", "tocsave", "wpssave", "render", "草稿", "预览", "测试",
            "过程",
        ]
        .iter()
        .any(|marker| filename.contains(marker))
    {
        return (
            ArtifactStage::Intermediate,
            "路径或文件名具有草稿、预览、测试或渲染过程特征",
        );
    }
    if components.iter().any(|value| {
        matches!(
            value.as_str(),
            "final"
                | "final-output"
                | "deliverable"
                | "deliverables"
                | "release"
                | "releases"
                | "export"
                | "exports"
                | "dist"
                | "成果"
                | "交付"
                | "最终"
                | "定稿"
        )
    }) || [
        "final",
        "deliverable",
        "release",
        "最终",
        "定稿",
        "交付",
        "成品",
        "修正版",
        "修改版",
    ]
    .iter()
    .any(|marker| filename.contains(marker))
    {
        return (
            ArtifactStage::Final,
            "路径或文件名具有交付、导出、发布或定稿特征",
        );
    }
    if components.iter().any(|value| value == "codex操作目录") {
        return (
            ArtifactStage::Intermediate,
            "文件位于 Codex 指定操作目录，且没有独立交付或定稿证据，按过程成果处理",
        );
    }
    (
        ArtifactStage::Undetermined,
        "已确认是成果类型，但现有事件没有证明它是过程版本还是最终版本",
    )
}

fn refine_artifact_series(resources: &mut [ResourceNode]) {
    let mut series = BTreeMap::<String, Vec<(usize, u64)>>::new();
    for (index, resource) in resources.iter().enumerate() {
        if resource.kind != ResourceKind::ResultArtifact {
            continue;
        }
        let Some(path) = resource.location.path() else {
            continue;
        };
        let Some((key, version)) = artifact_series_key(path) else {
            continue;
        };
        series.entry(key).or_default().push((index, version));
    }
    for entries in series.values().filter(|entries| entries.len() > 1) {
        let newest_version = entries
            .iter()
            .map(|(_, version)| *version)
            .max()
            .unwrap_or(0);
        for (index, version) in entries {
            let resource = &mut resources[*index];
            if *version < newest_version && resource.artifact_stage != Some(ArtifactStage::Final) {
                resource.artifact_stage = Some(ArtifactStage::Intermediate);
                resource.artifact_reason =
                    Some("同系列中存在更高版本号，当前文件判定为过程成果".to_string());
            } else if *version == newest_version
                && resource.artifact_stage == Some(ArtifactStage::Undetermined)
            {
                resource.artifact_reason =
                    Some("这是同系列最高版本，但文件名没有明确证明已经定稿".to_string());
            }
        }
    }
}

fn artifact_series_key(path: &Path) -> Option<(String, u64)> {
    static SERIES: OnceLock<Regex> = OnceLock::new();
    let regex = SERIES.get_or_init(|| {
        Regex::new(r"(?i)^(.*?)(?:[_\-\s]*(?:修正版|修改版|版本|version|ver|v)[_\-\s]*(\d+))$")
            .expect("artifact series regex")
    });
    let stem = path.file_stem()?.to_string_lossy();
    let captures = regex.captures(&stem)?;
    let base = captures.get(1)?.as_str().trim().to_ascii_lowercase();
    let version = captures.get(2)?.as_str().parse().ok()?;
    let parent = path.parent().map(path_key).unwrap_or_default();
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    Some((format!("{parent}/{base}.{extension}"), version))
}

fn lower_components(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().to_ascii_lowercase()),
            _ => None,
        })
        .collect()
}

fn collect_named_session_files(
    codex_home: &Path,
    session_id: &str,
    resources: &mut BTreeMap<String, ResourceNode>,
    max_entries: usize,
) {
    for folder in ["visualizations", "generated_images"] {
        let root = codex_home.join(folder);
        if !root.is_dir() {
            continue;
        }
        for entry in WalkDir::new(&root)
            .follow_links(false)
            .max_depth(8)
            .into_iter()
            .filter_map(Result::ok)
        {
            if !entry.file_type().is_dir() || entry.file_name().to_string_lossy() != session_id {
                continue;
            }
            collect_session_asset_units(entry.path(), resources, max_entries);
        }
    }
    for folder in ["shell_snapshots", "process_manager", "attachments"] {
        let root = codex_home.join(folder);
        if !root.is_dir() {
            continue;
        }
        for entry in WalkDir::new(&root)
            .follow_links(false)
            .max_depth(5)
            .into_iter()
            .filter_map(Result::ok)
        {
            if !entry.file_type().is_file()
                || !entry.file_name().to_string_lossy().contains(session_id)
            {
                continue;
            }
            insert_path_resource(
                resources,
                entry.path().to_path_buf(),
                match folder {
                    "shell_snapshots" => ResourceKind::ShellSnapshot,
                    "process_manager" => ResourceKind::TaskState,
                    _ => ResourceKind::WorkspaceFile,
                },
                Ownership::Exclusive,
                Confidence::Confirmed,
                "session filename",
                "文件名包含精确会话 UUID",
                max_entries,
            );
        }
    }
}

fn collect_session_asset_units(
    task_asset_root: &Path,
    resources: &mut BTreeMap<String, ResourceNode>,
    max_entries: usize,
) {
    let Ok(entries) = fs::read_dir(task_asset_root) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let (kind, detail) = classify_session_asset_unit(&path);
        insert_path_resource(
            resources,
            path,
            kind,
            Ownership::Exclusive,
            Confidence::Confirmed,
            "task UUID asset",
            detail,
            max_entries,
        );
    }
}

fn classify_session_asset_unit(path: &Path) -> (ResourceKind, &'static str) {
    let components = lower_components(path);
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if components.iter().any(|value| {
        matches!(
            value.as_str(),
            "node_modules" | ".venv" | "venv" | "vendor" | "target" | "classes" | "__pycache__"
        )
    }) || name.starts_with("reference-")
    {
        return (
            ResourceKind::SupportLibrary,
            "任务 UUID 目录中的依赖、编译产物或参考工程，可与成果分开处理",
        );
    }
    if components
        .iter()
        .any(|value| matches!(value.as_str(), ".cache" | "cache" | "caches"))
    {
        return (ResourceKind::Cache, "任务 UUID 目录中的可重建缓存");
    }
    if components
        .iter()
        .any(|value| matches!(value.as_str(), "tmp" | "temp" | "temporary" | "scratch"))
    {
        return (ResourceKind::Temporary, "任务 UUID 目录中的临时或草稿数据");
    }
    if is_result_artifact(path, true) {
        return (
            ResourceKind::ResultArtifact,
            "任务 UUID 目录中的生成成果，继续按最终/过程阶段细分",
        );
    }
    let source_extension = path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "rs" | "py"
                    | "js"
                    | "ts"
                    | "tsx"
                    | "jsx"
                    | "html"
                    | "css"
                    | "json"
                    | "toml"
                    | "yaml"
                    | "yml"
                    | "md"
            )
        });
    if source_extension
        || components
            .iter()
            .any(|value| matches!(value.as_str(), "src" | "source" | "sources"))
    {
        return (
            ResourceKind::SourceChange,
            "任务 UUID 目录中的源码或生成脚本",
        );
    }
    (
        ResourceKind::WorkspaceFile,
        "任务 UUID 目录中的其他专属工作文件",
    )
}

fn collect_matching_diagnostic_logs(
    codex_home: &Path,
    session: &SessionSummary,
    resources: &mut BTreeMap<String, ResourceNode>,
    max_entries: usize,
) {
    let mut roots = vec![codex_home.join(".sandbox")];
    if let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
        let packages = local.join("Packages");
        if let Ok(entries) = fs::read_dir(packages) {
            for entry in entries.filter_map(Result::ok) {
                if entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("OpenAI.Codex_")
                {
                    roots.push(entry.path().join("LocalCache/Local/Codex/Logs"));
                }
            }
        }
    }
    let cwd_marker = session
        .cwd
        .as_ref()
        .map(|value| value.to_string_lossy().to_ascii_lowercase());
    let mut files = roots
        .iter()
        .filter(|root| root.is_dir())
        .flat_map(|root| {
            WalkDir::new(root)
                .follow_links(false)
                .max_depth(4)
                .into_iter()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_type().is_file())
                .map(|entry| entry.path().to_path_buf())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    files.sort_by_key(|path| {
        std::cmp::Reverse(
            fs::metadata(path)
                .and_then(|value| value.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
        )
    });
    for path in files.into_iter().take(96) {
        if !file_head_contains(&path, &session.id, cwd_marker.as_deref(), 4 * 1024 * 1024) {
            continue;
        }
        insert_path_resource(
            resources,
            path,
            ResourceKind::Log,
            Ownership::Global,
            Confidence::Likely,
            "diagnostic log content",
            "日志内容包含任务 UUID 或工作目录，但日志同时服务其他任务，只能按日志规则清理",
            max_entries,
        );
    }
}

fn file_head_contains(
    path: &Path,
    session_id: &str,
    cwd_marker: Option<&str>,
    max_bytes: u64,
) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    let mut bytes = Vec::new();
    if file.take(max_bytes).read_to_end(&mut bytes).is_err() {
        return false;
    }
    let text = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
    text.contains(&session_id.to_ascii_lowercase())
        || cwd_marker.is_some_and(|marker| marker.len() >= 3 && text.contains(marker))
}

fn same_project_session(left: &SessionSummary, right: &SessionSummary) -> bool {
    match (left.cwd.as_ref(), right.cwd.as_ref()) {
        (Some(left), Some(right)) => same_project_paths(left, right),
        _ => left.id == right.id,
    }
}

fn same_project_paths(left: &Path, right: &Path) -> bool {
    let left_key = path_key(left).trim_end_matches('/').to_string();
    let right_key = path_key(right).trim_end_matches('/').to_string();
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

fn normalized_title(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .take(100)
        .collect()
}

fn descendant_session_ids(report: &ScanReport, session_id: &str) -> Vec<String> {
    let mut discovered = BTreeSet::new();
    let mut frontier = vec![session_id.to_string()];
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
    discovered.into_iter().collect()
}

fn refine_ownership_for_shared_workspaces(
    resources: &mut BTreeMap<String, ResourceNode>,
    report: &ScanReport,
    selected: &SessionSummary,
    descendants: &[String],
) {
    for resource in resources.values_mut() {
        if resource.ownership != Ownership::Exclusive {
            continue;
        }
        let Some(path) = resource.location.path() else {
            continue;
        };
        let shared_count = report
            .sessions
            .iter()
            .filter(|candidate| candidate.id != selected.id)
            .filter(|candidate| !descendants.contains(&candidate.id))
            .filter_map(|candidate| candidate.cwd.as_deref())
            .filter(|cwd| path.starts_with(cwd))
            .count();
        if shared_count == 0 {
            continue;
        }
        resource.ownership = Ownership::Shared;
        resource.evidence.insert(
            0,
            Evidence {
                source: "cross-task workspace check".to_string(),
                detail: format!(
                    "另有 {shared_count} 个任务的工作目录覆盖此路径，不能按本任务专属文件自动删除"
                ),
            },
        );
    }
}

fn path_size(path: &Path, max_entries: usize) -> (u64, bool) {
    if path.is_file() {
        return (
            fs::metadata(path).map(|value| value.len()).unwrap_or(0),
            true,
        );
    }
    if !path.is_dir() {
        return (0, true);
    }
    let mut size = 0_u64;
    let mut count = 0_usize;
    for entry in WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
    {
        if entry.file_type().is_file() {
            size = size.saturating_add(entry.metadata().map(|value| value.len()).unwrap_or(0));
        }
        count += 1;
        if count >= max_entries {
            return (size, false);
        }
    }
    (size, true)
}

fn path_key(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase()
}

fn resource_kind_specificity(kind: ResourceKind) -> u8 {
    match kind {
        ResourceKind::Conversation | ResourceKind::StateReference => 100,
        ResourceKind::TaskState => 95,
        ResourceKind::ResultArtifact => 90,
        ResourceKind::SourceChange => 85,
        ResourceKind::Worktree | ResourceKind::ShellSnapshot => 80,
        ResourceKind::Log => 70,
        ResourceKind::Cache | ResourceKind::Temporary => 60,
        ResourceKind::SupportLibrary => 50,
        ResourceKind::WorkspaceFile => 40,
        ResourceKind::ExternalReference => 10,
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use chrono::Utc;
    use tempfile::tempdir;

    use super::*;
    use crate::{ScanReport, SessionStatus, SessionSummary};

    #[test]
    fn merges_windows_paths_with_mixed_separators_and_keeps_specific_kind() {
        let mut resources = BTreeMap::new();
        insert_path_resource(
            &mut resources,
            PathBuf::from("C:\\Codex\\Logs\\task.log"),
            ResourceKind::SupportLibrary,
            Ownership::Unknown,
            Confidence::Weak,
            "tool event",
            "generic reference",
            100,
        );
        insert_path_resource(
            &mut resources,
            PathBuf::from("C:/Codex/Logs/task.log"),
            ResourceKind::Log,
            Ownership::Global,
            Confidence::Likely,
            "diagnostic log content",
            "matched task",
            100,
        );

        assert_eq!(resources.len(), 1);
        let resource = resources.values().next().unwrap();
        assert_eq!(resource.kind, ResourceKind::Log);
        assert_eq!(resource.ownership, Ownership::Global);
    }

    #[test]
    fn reads_confirmed_paths_from_patch_event() {
        let root = tempdir().unwrap();
        let changed = root.path().join("src/main.rs");
        fs::create_dir_all(changed.parent().unwrap()).unwrap();
        fs::write(&changed, "fn main() {}\n").unwrap();
        let value = serde_json::json!({
            "payload": {
                "type": "patch_apply_end",
                "success": true,
                "changes": { changed.display().to_string(): { "type": "add" } }
            }
        });
        let mut paths = BTreeSet::new();
        collect_confirmed_changes(&value, None, &mut paths);
        assert!(paths.contains(&changed));
        let mut created = BTreeSet::new();
        collect_created_changes(&value, None, &mut created);
        assert!(created.contains(&changed));
    }

    #[test]
    fn separates_paths_that_only_appear_in_tool_output() {
        let root = tempdir().unwrap();
        let requested = root.path().join("requested.pdf");
        let unrelated = root.path().join("unrelated.pdf");
        fs::write(&requested, b"requested").unwrap();
        fs::write(&unrelated, b"unrelated").unwrap();
        let rollout = root.path().join("rollout.jsonl");
        let mut file = File::create(&rollout).unwrap();
        writeln!(
            file,
            "{}",
            serde_json::json!({
                "type": "response_item",
                "payload": {
                    "type": "function_call",
                    "arguments": serde_json::json!({ "path": requested }).to_string()
                }
            })
        )
        .unwrap();
        writeln!(
            file,
            "{}",
            serde_json::json!({
                "type": "response_item",
                "payload": {
                    "type": "custom_tool_call_output",
                    "output": unrelated.display().to_string()
                }
            })
        )
        .unwrap();

        let mut paths = BTreeSet::new();
        let mut observed = BTreeSet::new();
        extract_paths_from_rollout(
            &rollout,
            u64::MAX,
            None,
            &mut paths,
            &mut observed,
            &mut BTreeSet::new(),
            &mut BTreeSet::new(),
        )
        .unwrap();
        assert!(paths.contains(&requested));
        assert!(!paths.contains(&unrelated));
        assert!(observed.contains(&unrelated));
    }

    #[test]
    fn tool_output_paths_are_visible_but_never_implicitly_deletable() {
        let root = tempdir().unwrap();
        let home = root.path().join(".codex");
        let project = root.path().join("project");
        let observed = project.join("outputs/observed.pdf");
        fs::create_dir_all(observed.parent().unwrap()).unwrap();
        fs::create_dir_all(home.join("sessions")).unwrap();
        fs::write(&observed, b"observed").unwrap();
        let rollout = home.join("sessions/rollout-test.jsonl");
        fs::write(
            &rollout,
            format!(
                "{}\n",
                serde_json::json!({
                    "type": "response_item",
                    "payload": {
                        "type": "custom_tool_call_output",
                        "output": observed.display().to_string()
                    }
                })
            ),
        )
        .unwrap();
        let report = ScanReport {
            codex_home: home,
            sessions: vec![SessionSummary {
                id: "test".to_string(),
                title: "test".to_string(),
                status: SessionStatus::Active,
                updated_at: Some(Utc::now()),
                started_at: Some(Utc::now()),
                cwd: Some(project),
                source: None,
                parent_id: None,
                transcript_paths: vec![rollout.clone()],
                transcript_bytes: fs::metadata(rollout).unwrap().len(),
            }],
            transcript_bytes: 0,
            malformed_index_lines: 0,
            warnings: vec![],
        };

        let analysis = analyze_session(&report, "test", AnalysisOptions::default()).unwrap();
        let resource = analysis
            .resources
            .iter()
            .find(|resource| resource.location.path() == Some(observed.as_path()))
            .unwrap();
        assert_eq!(resource.ownership, Ownership::Unknown);
        assert_eq!(resource.confidence, Confidence::Weak);
        assert_ne!(resource.action, ResourceAction::Delete);
        assert!(resource
            .evidence
            .iter()
            .any(|evidence| evidence.source == "tool output observation"));
    }

    #[test]
    fn analysis_keeps_results_and_protects_unknown_workspace_files() {
        let root = tempdir().unwrap();
        let home = root.path().join(".codex");
        let project = root.path().join("project");
        let outputs = project.join("outputs");
        fs::create_dir_all(home.join("sessions")).unwrap();
        fs::create_dir_all(&outputs).unwrap();
        let artifact = outputs.join("report.pdf");
        fs::write(&artifact, b"result").unwrap();
        let source = project.join("main.rs");
        fs::write(&source, b"fn main() {}").unwrap();
        let rollout = home.join("sessions/rollout-test.jsonl");
        let mut file = File::create(&rollout).unwrap();
        writeln!(
            file,
            "{}",
            serde_json::json!({
                "type": "response_item",
                "payload": {
                    "type": "function_call",
                    "arguments": format!(r#"{{"path":"{}","source":"{}"}}"#, artifact.display(), source.display())
                }
            })
        )
        .unwrap();
        let session = SessionSummary {
            id: "test".to_string(),
            title: "test".to_string(),
            status: SessionStatus::Active,
            updated_at: Some(Utc::now()),
            started_at: Some(Utc::now()),
            cwd: Some(project.clone()),
            source: None,
            parent_id: None,
            transcript_paths: vec![rollout.clone()],
            transcript_bytes: fs::metadata(&rollout).unwrap().len(),
        };
        let report = ScanReport {
            codex_home: home,
            sessions: vec![session],
            transcript_bytes: 0,
            malformed_index_lines: 0,
            warnings: vec![],
        };

        let analysis = analyze_session(&report, "test", AnalysisOptions::default()).unwrap();
        let result = analysis
            .resources
            .iter()
            .find(|value| value.location.path() == Some(artifact.as_path()))
            .unwrap();
        assert_eq!(result.kind, ResourceKind::ResultArtifact);
        assert_eq!(result.action, ResourceAction::Keep);
        let conversation = analysis
            .resources
            .iter()
            .find(|value| value.location.path() == Some(rollout.as_path()))
            .unwrap();
        assert_eq!(conversation.action, ResourceAction::Delete);
    }

    #[test]
    fn retention_profile_never_deletes_unknown_resources() {
        let resource = ResourceNode {
            id: 1,
            location: ResourceLocation::Path {
                path: PathBuf::from("C:/shared/report.pdf"),
            },
            kind: ResourceKind::ResultArtifact,
            artifact_stage: Some(ArtifactStage::Undetermined),
            artifact_reason: Some("test".to_string()),
            size: 1,
            size_complete: true,
            ownership: Ownership::Unknown,
            confidence: Confidence::Weak,
            evidence: vec![],
            recommended_action: ResourceAction::Keep,
            user_override: None,
            action: ResourceAction::Keep,
        };
        assert_ne!(
            desired_action(&resource, RetentionProfile::ResultsOnly),
            ResourceAction::Delete
        );
    }

    #[test]
    fn cache_images_are_not_misclassified_as_results() {
        let path = PathBuf::from("C:/users/test/.cache/runtime/preview.png");
        let (kind, _, _, _) = classify_path(
            &path,
            Path::new("C:/users/test/.codex"),
            Some(Path::new("C:/work")),
            false,
            false,
        );
        assert_eq!(kind, ResourceKind::Cache);
    }

    #[test]
    fn distinguishes_intermediate_and_final_artifacts() {
        assert_eq!(
            classify_artifact_stage(Path::new("C:/work/artifacts/smoke/window.png")).0,
            ArtifactStage::Intermediate
        );
        assert_eq!(
            classify_artifact_stage(Path::new("C:/work/deliverables/最终报告.pdf")).0,
            ArtifactStage::Final
        );
        assert_eq!(
            classify_artifact_stage(Path::new("C:/work/output/qa_final_v5.pdf")).0,
            ArtifactStage::Intermediate
        );
        assert_eq!(
            classify_artifact_stage(Path::new("C:/work/report.pdf")).0,
            ArtifactStage::Undetermined
        );
    }

    #[test]
    fn classification_prioritizes_temp_and_logs_and_does_not_promote_input_final_name() {
        let home = Path::new("C:/users/test/.codex");
        let cwd = Path::new("C:/work");
        let (temp_kind, _, _, _) = classify_path(
            Path::new("C:/work/temp/final.png"),
            home,
            Some(cwd),
            false,
            false,
        );
        let (log_kind, _, _, _) = classify_path(
            Path::new("C:/work/packages/run.log"),
            home,
            Some(cwd),
            false,
            false,
        );
        let (input_kind, _, _, _) = classify_path(
            Path::new("D:/inputs/final.pdf"),
            home,
            Some(cwd),
            false,
            false,
        );
        assert_eq!(temp_kind, ResourceKind::Temporary);
        assert_eq!(log_kind, ResourceKind::Log);
        assert_eq!(input_kind, ResourceKind::ExternalReference);
    }

    #[test]
    fn conversation_only_keeps_attachments_and_task_state() {
        let attachment = ResourceNode {
            id: 1,
            location: ResourceLocation::Path {
                path: PathBuf::from("C:/users/test/.codex/attachments/random/input.txt"),
            },
            kind: ResourceKind::WorkspaceFile,
            artifact_stage: None,
            artifact_reason: None,
            size: 1,
            size_complete: true,
            ownership: Ownership::Shared,
            confidence: Confidence::Confirmed,
            evidence: vec![],
            recommended_action: ResourceAction::Keep,
            user_override: None,
            action: ResourceAction::Keep,
        };
        let mut task_state = attachment.clone();
        task_state.kind = ResourceKind::TaskState;
        task_state.ownership = Ownership::Exclusive;
        assert_ne!(
            desired_action(&attachment, RetentionProfile::ConversationOnly),
            ResourceAction::Delete
        );
        assert_ne!(
            desired_action(&task_state, RetentionProfile::ConversationOnly),
            ResourceAction::Delete
        );
    }

    #[test]
    fn analysis_includes_recursive_descendant_transcripts() {
        let root = tempdir().unwrap();
        let home = root.path().join(".codex");
        fs::create_dir_all(home.join("sessions")).unwrap();
        let make_rollout = |name: &str, bytes: &[u8]| {
            let path = home.join("sessions").join(name);
            fs::write(&path, bytes).unwrap();
            path
        };
        let root_rollout = make_rollout("rollout-root.jsonl", b"{}\n");
        let child_rollout = make_rollout("rollout-child.jsonl", b"{}\n{}\n");
        let grandchild_rollout = make_rollout("rollout-grandchild.jsonl", b"{}\n{}\n{}\n");
        let session = |id: &str, parent_id: Option<&str>, path: PathBuf| SessionSummary {
            id: id.to_string(),
            title: id.to_string(),
            status: SessionStatus::Active,
            updated_at: None,
            started_at: None,
            cwd: Some(root.path().join("work")),
            source: None,
            parent_id: parent_id.map(str::to_string),
            transcript_bytes: fs::metadata(&path).unwrap().len(),
            transcript_paths: vec![path],
        };
        let report = ScanReport {
            codex_home: home,
            sessions: vec![
                session("root", None, root_rollout),
                session("child", Some("root"), child_rollout),
                session("grandchild", Some("child"), grandchild_rollout),
            ],
            transcript_bytes: 0,
            malformed_index_lines: 0,
            warnings: vec![],
        };

        let analysis = analyze_session(&report, "root", AnalysisOptions::default()).unwrap();

        assert_eq!(analysis.related_session_ids.len(), 2);
        assert_eq!(
            analysis
                .resources
                .iter()
                .filter(|resource| resource.kind == ResourceKind::Conversation)
                .count(),
            3
        );
        assert_eq!(
            analysis.related_transcript_bytes,
            report.sessions[1].transcript_bytes + report.sessions[2].transcript_bytes
        );
        assert_eq!(
            analysis.analyzed_bytes,
            report
                .sessions
                .iter()
                .map(|session| session.transcript_bytes)
                .sum::<u64>()
        );
    }
}
