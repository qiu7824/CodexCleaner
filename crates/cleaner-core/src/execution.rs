use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    delete_thread_official, find_existing_threads_official, Ownership, ResourceAction,
    ResourceKind, ResourceLocation, RetentionProfile, SessionAnalysis,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanupPlan {
    pub session_id: String,
    #[serde(default)]
    pub descendant_ids: Vec<String>,
    pub profile: RetentionProfile,
    pub official_thread_delete: bool,
    pub descendant_count: usize,
    pub descendant_transcript_bytes: u64,
    #[serde(default)]
    pub transcript_paths: Vec<PathBuf>,
    pub recycle_paths: Vec<PathBuf>,
    pub blocked_resources: Vec<String>,
    pub delete_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupStatus {
    Completed,
    Partial,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanupReceipt {
    pub operation_id: String,
    pub session_id: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub status: CleanupStatus,
    pub recycled_paths: Vec<PathBuf>,
    pub official_thread_deleted: bool,
    #[serde(default)]
    pub official_delete_verified: bool,
    #[serde(default)]
    pub remaining_thread_ids: Vec<String>,
    #[serde(default)]
    pub remaining_transcript_paths: Vec<PathBuf>,
    pub error: Option<String>,
    pub journal_path: PathBuf,
}

pub fn build_cleanup_plan(analysis: &SessionAnalysis) -> CleanupPlan {
    let mut official_thread_delete = false;
    let mut transcript_paths = Vec::new();
    let mut recycle_candidates = Vec::new();
    let mut blocked_resources = Vec::new();

    for resource in &analysis.resources {
        if resource.action != ResourceAction::Delete {
            continue;
        }
        match &resource.location {
            ResourceLocation::StateRecord { .. } => official_thread_delete = true,
            ResourceLocation::Path { path } if resource.kind == ResourceKind::Conversation => {
                official_thread_delete = true;
                transcript_paths.push(path.clone());
            }
            ResourceLocation::Path { path }
                if resource.ownership == Ownership::Exclusive
                    || (resource.ownership == Ownership::Unknown
                        && resource.user_override == Some(ResourceAction::Delete)) =>
            {
                recycle_candidates.push((path.clone(), resource.size));
            }
            _ => blocked_resources.push(resource.location.display()),
        }
    }

    let protected_paths = analysis
        .resources
        .iter()
        .filter(|resource| resource.action != ResourceAction::Delete)
        .filter_map(|resource| resource.location.path())
        .collect::<Vec<_>>();
    recycle_candidates.retain(|(candidate, _)| {
        let conflict = protected_paths
            .iter()
            .any(|protected| is_strict_descendant(protected, candidate));
        if conflict {
            blocked_resources.push(format!(
                "{}（目录内含保留、受保护或待确认项目）",
                candidate.display()
            ));
            false
        } else {
            true
        }
    });

    recycle_candidates.sort_by(|left, right| {
        path_depth(&left.0)
            .cmp(&path_depth(&right.0))
            .then_with(|| normalized_path_key(&left.0).cmp(&normalized_path_key(&right.0)))
    });
    let mut executable_candidates: Vec<(PathBuf, u64)> = Vec::new();
    for candidate in recycle_candidates {
        if executable_candidates
            .iter()
            .any(|(parent, _)| is_same_or_descendant(&candidate.0, parent))
        {
            continue;
        }
        executable_candidates.push(candidate);
    }
    let recycle_bytes = executable_candidates
        .iter()
        .fold(0_u64, |total, (_, size)| total.saturating_add(*size));
    let recycle_paths = executable_candidates
        .into_iter()
        .map(|(path, _)| path)
        .collect();
    let transcript_bytes = if official_thread_delete {
        analysis
            .session
            .transcript_bytes
            .saturating_add(analysis.related_transcript_bytes)
    } else {
        0
    };
    blocked_resources.sort();
    blocked_resources.dedup();
    transcript_paths.sort_by_key(|path| normalized_path_key(path));
    transcript_paths
        .dedup_by(|left, right| normalized_path_key(left) == normalized_path_key(right));

    CleanupPlan {
        session_id: analysis.session.id.clone(),
        descendant_ids: analysis.related_session_ids.clone(),
        profile: analysis.profile,
        official_thread_delete,
        descendant_count: analysis.related_session_ids.len(),
        descendant_transcript_bytes: analysis.related_transcript_bytes,
        transcript_paths,
        recycle_paths,
        blocked_resources,
        delete_bytes: transcript_bytes.saturating_add(recycle_bytes),
    }
}

fn normalized_path_key(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

fn path_depth(path: &Path) -> usize {
    path.components().count()
}

fn is_same_or_descendant(path: &Path, parent: &Path) -> bool {
    let path = normalized_path_key(path);
    let parent = normalized_path_key(parent);
    path == parent
        || path
            .strip_prefix(&parent)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn is_strict_descendant(path: &Path, parent: &Path) -> bool {
    normalized_path_key(path) != normalized_path_key(parent) && is_same_or_descendant(path, parent)
}

pub fn execute_cleanup_plan(
    plan: &CleanupPlan,
    codex_binary: &Path,
    journal_root: &Path,
) -> Result<CleanupReceipt, String> {
    if !plan.blocked_resources.is_empty() {
        return Err(format!(
            "cleanup plan contains {} blocked resources",
            plan.blocked_resources.len()
        ));
    }
    if !plan.official_thread_delete && plan.recycle_paths.is_empty() {
        return Err("cleanup plan has no executable actions".to_string());
    }

    fs::create_dir_all(journal_root).map_err(|error| error.to_string())?;
    let started_at = Utc::now();
    let operation_id = format!(
        "{}-{}-{}",
        started_at.format("%Y%m%d-%H%M%S"),
        started_at.timestamp_subsec_millis(),
        std::process::id()
    );
    let journal_path = journal_root.join(format!("{operation_id}.json"));
    let mut receipt = CleanupReceipt {
        operation_id,
        session_id: plan.session_id.clone(),
        started_at,
        finished_at: started_at,
        status: CleanupStatus::Failed,
        recycled_paths: Vec::new(),
        official_thread_deleted: false,
        official_delete_verified: false,
        remaining_thread_ids: Vec::new(),
        remaining_transcript_paths: Vec::new(),
        error: None,
        journal_path: journal_path.clone(),
    };
    persist_receipt(&receipt)?;

    for path in &plan.recycle_paths {
        if !path.exists() {
            continue;
        }
        if let Err(error) = validate_cleanup_path(path) {
            receipt.status = if receipt.recycled_paths.is_empty() {
                CleanupStatus::Failed
            } else {
                CleanupStatus::Partial
            };
            receipt.error = Some(error);
            receipt.finished_at = Utc::now();
            persist_receipt(&receipt)?;
            return Ok(receipt);
        }
        if let Err(error) = trash::delete(path) {
            receipt.status = if receipt.recycled_paths.is_empty() {
                CleanupStatus::Failed
            } else {
                CleanupStatus::Partial
            };
            receipt.error = Some(format!(
                "failed to move {} to Recycle Bin: {error}",
                path.display()
            ));
            receipt.finished_at = Utc::now();
            persist_receipt(&receipt)?;
            return Ok(receipt);
        }
        receipt.recycled_paths.push(path.clone());
        persist_receipt(&receipt)?;
    }

    if plan.official_thread_delete {
        if let Err(error) =
            delete_thread_official(codex_binary, &plan.session_id, Duration::from_secs(20))
        {
            receipt.status = if receipt.recycled_paths.is_empty() {
                CleanupStatus::Failed
            } else {
                CleanupStatus::Partial
            };
            receipt.error = Some(error);
            receipt.finished_at = Utc::now();
            persist_receipt(&receipt)?;
            return Ok(receipt);
        }
        receipt.official_thread_deleted = true;
        persist_receipt(&receipt)?;

        let mut expected_ids = Vec::with_capacity(plan.descendant_ids.len() + 1);
        expected_ids.push(plan.session_id.clone());
        expected_ids.extend(plan.descendant_ids.iter().cloned());
        expected_ids.sort();
        expected_ids.dedup();
        receipt.remaining_transcript_paths = plan
            .transcript_paths
            .iter()
            .filter(|path| path.exists())
            .cloned()
            .collect();
        match find_existing_threads_official(codex_binary, &expected_ids, Duration::from_secs(20)) {
            Ok(remaining) => receipt.remaining_thread_ids = remaining,
            Err(error) => {
                receipt.status = CleanupStatus::Partial;
                receipt.error = Some(format!(
                    "Codex 已接受官方删除，但无法完成删除后校验：{error}"
                ));
                receipt.finished_at = Utc::now();
                persist_receipt(&receipt)?;
                return Ok(receipt);
            }
        }
        if !receipt.remaining_thread_ids.is_empty()
            || !receipt.remaining_transcript_paths.is_empty()
        {
            receipt.status = CleanupStatus::Partial;
            receipt.error = Some(format!(
                "Codex 官方删除返回成功，但校验仍发现 {} 个任务索引和 {} 份 transcript；界面不会乐观移除这些任务",
                receipt.remaining_thread_ids.len(),
                receipt.remaining_transcript_paths.len()
            ));
            receipt.finished_at = Utc::now();
            persist_receipt(&receipt)?;
            return Ok(receipt);
        }
        receipt.official_delete_verified = true;
    }

    receipt.status = CleanupStatus::Completed;
    receipt.finished_at = Utc::now();
    persist_receipt(&receipt)?;
    Ok(receipt)
}

fn validate_cleanup_path(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err(format!(
            "refusing to clean a relative path: {}",
            path.display()
        ));
    }
    if path.parent().is_none() {
        return Err(format!(
            "refusing to clean a filesystem root: {}",
            path.display()
        ));
    }
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        format!(
            "failed to inspect cleanup target {}: {error}",
            path.display()
        )
    })?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "refusing to clean a symbolic link or junction: {}",
            path.display()
        ));
    }
    Ok(())
}

fn persist_receipt(receipt: &CleanupReceipt) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(receipt).map_err(|error| error.to_string())?;
    let temporary = receipt.journal_path.with_extension("json.tmp");
    fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
    fs::rename(&temporary, &receipt.journal_path).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::{Confidence, Evidence, ResourceNode, SessionStatus, SessionSummary};

    #[test]
    fn plan_separates_official_delete_recycle_and_blocked_paths() {
        let transcript = PathBuf::from("C:/codex/rollout.jsonl");
        let analysis = SessionAnalysis {
            session: SessionSummary {
                id: "thread-1".to_string(),
                title: "test".to_string(),
                status: SessionStatus::Active,
                updated_at: Some(Utc::now()),
                started_at: None,
                cwd: None,
                source: None,
                parent_id: None,
                transcript_paths: vec![transcript.clone()],
                transcript_bytes: 10,
            },
            related_session_ids: vec![],
            related_transcript_bytes: 0,
            project_related_session_ids: vec!["thread-1".to_string()],
            duplicate_title_session_ids: vec!["thread-1".to_string()],
            project_transcript_bytes: 10,
            resources: vec![
                resource(
                    1,
                    transcript,
                    ResourceKind::Conversation,
                    Ownership::Exclusive,
                ),
                resource(
                    2,
                    PathBuf::from("C:/codex/snapshot.bin"),
                    ResourceKind::ShellSnapshot,
                    Ownership::Exclusive,
                ),
                resource(
                    3,
                    PathBuf::from("C:/shared/cache"),
                    ResourceKind::Cache,
                    Ownership::Global,
                ),
            ],
            profile: RetentionProfile::ResultsOnly,
            analyzed_bytes: 0,
            truncated: false,
            warnings: vec![],
        };
        let plan = build_cleanup_plan(&analysis);
        assert!(plan.official_thread_delete);
        assert_eq!(
            plan.recycle_paths,
            vec![PathBuf::from("C:/codex/snapshot.bin")]
        );
        assert_eq!(plan.blocked_resources.len(), 1);
        assert_eq!(plan.delete_bytes, 20);
    }

    #[test]
    fn plan_blocks_parent_directory_when_it_contains_a_kept_resource() {
        let mut parent = resource(
            1,
            PathBuf::from("C:/project"),
            ResourceKind::Worktree,
            Ownership::Exclusive,
        );
        parent.size = 1_000;
        let mut kept = resource(
            2,
            PathBuf::from("C:/project/final.docx"),
            ResourceKind::ResultArtifact,
            Ownership::Exclusive,
        );
        kept.action = ResourceAction::Keep;
        let analysis = analysis_with(vec![parent, kept]);

        let plan = build_cleanup_plan(&analysis);

        assert!(plan.recycle_paths.is_empty());
        assert_eq!(plan.delete_bytes, 0);
        assert_eq!(plan.blocked_resources.len(), 1);
        assert!(plan.blocked_resources[0].contains("目录内含"));
    }

    #[test]
    fn plan_deduplicates_nested_delete_targets_and_counts_parent_once() {
        let mut parent = resource(
            1,
            PathBuf::from("C:/project/cache"),
            ResourceKind::Cache,
            Ownership::Exclusive,
        );
        parent.size = 100;
        let mut child = resource(
            2,
            PathBuf::from("C:/project/cache/item.bin"),
            ResourceKind::Temporary,
            Ownership::Exclusive,
        );
        child.size = 25;
        let analysis = analysis_with(vec![child, parent]);

        let plan = build_cleanup_plan(&analysis);

        assert_eq!(plan.recycle_paths, vec![PathBuf::from("C:/project/cache")]);
        assert_eq!(plan.delete_bytes, 100);
    }

    #[test]
    fn descendant_transcripts_are_left_to_official_thread_delete() {
        let root_transcript = PathBuf::from("C:/codex/sessions/root.jsonl");
        let child_transcript = PathBuf::from("C:/codex/sessions/child.jsonl");
        let mut analysis = analysis_with(vec![
            resource(
                1,
                root_transcript.clone(),
                ResourceKind::Conversation,
                Ownership::Exclusive,
            ),
            resource(
                2,
                child_transcript,
                ResourceKind::Conversation,
                Ownership::Exclusive,
            ),
        ]);
        analysis.session.transcript_paths = vec![root_transcript];
        analysis.session.transcript_bytes = 100;
        analysis.related_session_ids = vec!["child".to_string()];
        analysis.related_transcript_bytes = 75;

        let plan = build_cleanup_plan(&analysis);

        assert!(plan.official_thread_delete);
        assert!(plan.recycle_paths.is_empty());
        assert_eq!(plan.delete_bytes, 175);
    }

    #[test]
    fn plan_allows_unknown_path_only_after_explicit_user_delete_override() {
        let mut resource = resource(
            1,
            PathBuf::from("D:/codex-ops/process.docx"),
            ResourceKind::ResultArtifact,
            Ownership::Unknown,
        );
        let blocked = build_cleanup_plan(&analysis_with(vec![resource.clone()]));
        assert!(blocked.recycle_paths.is_empty());
        assert_eq!(blocked.blocked_resources.len(), 1);

        resource.user_override = Some(ResourceAction::Delete);
        let approved = build_cleanup_plan(&analysis_with(vec![resource]));
        assert_eq!(
            approved.recycle_paths,
            vec![PathBuf::from("D:/codex-ops/process.docx")]
        );
        assert!(approved.blocked_resources.is_empty());
    }

    fn analysis_with(resources: Vec<ResourceNode>) -> SessionAnalysis {
        SessionAnalysis {
            session: SessionSummary {
                id: "thread-1".to_string(),
                title: "test".to_string(),
                status: SessionStatus::Active,
                updated_at: Some(Utc::now()),
                started_at: None,
                cwd: None,
                source: None,
                parent_id: None,
                transcript_paths: vec![],
                transcript_bytes: 0,
            },
            related_session_ids: vec![],
            related_transcript_bytes: 0,
            project_related_session_ids: vec!["thread-1".to_string()],
            duplicate_title_session_ids: vec!["thread-1".to_string()],
            project_transcript_bytes: 0,
            resources,
            profile: RetentionProfile::ResultsOnly,
            analyzed_bytes: 0,
            truncated: false,
            warnings: vec![],
        }
    }

    fn resource(id: u64, path: PathBuf, kind: ResourceKind, ownership: Ownership) -> ResourceNode {
        ResourceNode {
            id,
            location: ResourceLocation::Path { path },
            kind,
            artifact_stage: None,
            artifact_reason: None,
            size: 10,
            size_complete: true,
            ownership,
            confidence: Confidence::Confirmed,
            evidence: Vec::<Evidence>::new(),
            recommended_action: ResourceAction::Delete,
            user_override: None,
            action: ResourceAction::Delete,
        }
    }
}
