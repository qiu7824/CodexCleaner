use std::{
    collections::{BTreeMap, HashMap},
    env,
    fs::File,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    time::SystemTime,
};

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;
use walkdir::WalkDir;

use crate::{ScanReport, SessionStatus, SessionSummary};

#[derive(Debug, Error)]
pub enum ScanError {
    #[error("Codex home does not exist: {0}")]
    MissingHome(PathBuf),
    #[error("failed to read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug, Clone, Deserialize)]
struct IndexEntry {
    id: String,
    #[serde(default, alias = "title")]
    thread_name: String,
    #[serde(default)]
    updated_at: String,
}

#[derive(Debug, Default)]
struct RolloutMetadata {
    id: Option<String>,
    started_at: Option<DateTime<Utc>>,
    cwd: Option<PathBuf>,
    source: Option<String>,
    parent_id: Option<String>,
    inferred_title: Option<String>,
}

pub fn discover_codex_home(explicit: Option<PathBuf>) -> PathBuf {
    explicit
        .or_else(|| env::var_os("CODEX_HOME").map(PathBuf::from))
        .or_else(|| env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join(".codex")))
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
        .unwrap_or_else(|| PathBuf::from(".codex"))
}

pub fn scan_codex_home(home: impl AsRef<Path>) -> Result<ScanReport, ScanError> {
    let home = home.as_ref().to_path_buf();
    if !home.is_dir() {
        return Err(ScanError::MissingHome(home));
    }

    let (index, malformed_index_lines) = read_index(&home)?;
    let mut sessions = BTreeMap::<String, SessionSummary>::new();
    let mut warnings = Vec::new();

    for entry in index.values() {
        sessions.insert(
            entry.id.clone(),
            SessionSummary {
                id: entry.id.clone(),
                title: if entry.thread_name.trim().is_empty() {
                    short_id(&entry.id)
                } else {
                    entry.thread_name.clone()
                },
                // The legacy JSONL index can outlive both the rollout and the
                // state-db row.  Do not present an index-only entry as an
                // active task until a canonical transcript is found (or the
                // app-server confirms it later).
                status: SessionStatus::Orphaned,
                updated_at: parse_timestamp(&entry.updated_at),
                started_at: None,
                cwd: None,
                source: None,
                parent_id: None,
                transcript_paths: Vec::new(),
                transcript_bytes: 0,
            },
        );
    }

    for (root_name, status) in [
        ("sessions", SessionStatus::Active),
        ("archived_sessions", SessionStatus::Archived),
    ] {
        let root = home.join(root_name);
        if !root.is_dir() {
            continue;
        }
        for entry in WalkDir::new(&root).follow_links(false).into_iter() {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    warnings.push(format!("{}: {error}", root.display()));
                    continue;
                }
            };
            if !entry.file_type().is_file() || !is_rollout(entry.path()) {
                continue;
            }
            let path = entry.path().to_path_buf();
            let file_size = entry.metadata().map(|value| value.len()).unwrap_or(0);
            let metadata = read_rollout_metadata(&path).unwrap_or_default();
            let id = metadata
                .id
                .clone()
                .or_else(|| id_from_filename(&path))
                .unwrap_or_else(|| format!("path:{}", path.display()));
            let indexed = index.get(&id);
            let modified = entry
                .metadata()
                .ok()
                .and_then(|value| value.modified().ok())
                .map(DateTime::<Utc>::from);
            let summary = sessions
                .entry(id.clone())
                .or_insert_with(|| SessionSummary {
                    id: id.clone(),
                    title: indexed
                        .map(|value| value.thread_name.clone())
                        .filter(|value| !value.trim().is_empty())
                        .or_else(|| metadata.inferred_title.clone())
                        .unwrap_or_else(|| format!("本地任务 {}", short_id(&id))),
                    // `sessions` and `archived_sessions` are the canonical
                    // local status surfaces.  Modern spawned threads are
                    // intentionally absent from session_index.jsonl, so lack
                    // of a legacy index row does not make them orphaned.
                    status,
                    updated_at: indexed
                        .and_then(|value| parse_timestamp(&value.updated_at))
                        .or(modified),
                    started_at: metadata.started_at,
                    cwd: metadata.cwd.clone(),
                    source: metadata.source.clone(),
                    parent_id: metadata.parent_id.clone(),
                    transcript_paths: Vec::new(),
                    transcript_bytes: 0,
                });
            if status == SessionStatus::Archived {
                summary.status = SessionStatus::Archived;
            } else if indexed.is_some() {
                summary.status = SessionStatus::Active;
            }
            summary.started_at = summary.started_at.or(metadata.started_at);
            summary.cwd = summary.cwd.clone().or(metadata.cwd);
            summary.source = summary.source.clone().or(metadata.source);
            summary.parent_id = summary.parent_id.clone().or(metadata.parent_id);
            if (summary.title == short_id(&id) || summary.title.starts_with("本地任务 "))
                && metadata.inferred_title.is_some()
            {
                summary.title = metadata.inferred_title.unwrap_or(summary.title.clone());
            }
            summary.updated_at = summary.updated_at.or(modified);
            summary.transcript_bytes = summary.transcript_bytes.saturating_add(file_size);
            summary.transcript_paths.push(path);
        }
    }

    let index_only_entries = sessions
        .values()
        .filter(|session| session.transcript_paths.is_empty())
        .count();
    if index_only_entries > 0 {
        warnings.push(format!(
            "session_index.jsonl 中有 {index_only_entries} 条记录没有对应的活跃或已归档 transcript；已从任务列表排除，不计入可释放空间，也不会直接删除共享数据库"
        ));
        sessions.retain(|_, session| !session.transcript_paths.is_empty());
    }

    let mut sessions = sessions.into_values().collect::<Vec<_>>();
    sessions.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    let transcript_bytes = sessions.iter().map(|value| value.transcript_bytes).sum();

    Ok(ScanReport {
        codex_home: home,
        sessions,
        transcript_bytes,
        malformed_index_lines,
        warnings,
    })
}

fn read_index(home: &Path) -> Result<(HashMap<String, IndexEntry>, usize), ScanError> {
    let path = home.join("session_index.jsonl");
    if !path.is_file() {
        return Ok((HashMap::new(), 0));
    }
    let file = File::open(&path).map_err(|source| ScanError::Io {
        path: path.clone(),
        source,
    })?;
    let mut entries = HashMap::new();
    let mut malformed = 0;
    for line in BufReader::new(file).lines() {
        let line = line.map_err(|source| ScanError::Io {
            path: path.clone(),
            source,
        })?;
        match serde_json::from_str::<IndexEntry>(&line) {
            Ok(entry) if !entry.id.trim().is_empty() => {
                entries.insert(entry.id.clone(), entry);
            }
            Ok(_) | Err(_) => malformed += 1,
        }
    }
    Ok((entries, malformed))
}

fn read_rollout_metadata(path: &Path) -> Option<RolloutMetadata> {
    if path.extension().and_then(|value| value.to_str()) == Some("zst") {
        let file = File::open(path).ok()?;
        let decoder = zstd::stream::read::Decoder::new(file).ok()?;
        let mut reader = BufReader::new(decoder);
        return read_rollout_head(&mut reader);
    }
    let file = File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    read_rollout_head(&mut reader)
}

fn read_rollout_head(reader: &mut impl BufRead) -> Option<RolloutMetadata> {
    // A desktop task can begin with a large injected context block before the
    // first real user request.  Two MiB was too small for those rollouts and
    // left otherwise healthy tasks with fallback titles.
    const MAX_BYTES: usize = 8 * 1024 * 1024;
    const MAX_LINES: usize = 2_048;
    let mut metadata = RolloutMetadata::default();
    let mut consumed = 0_usize;
    for _ in 0..MAX_LINES {
        let mut line = String::new();
        let bytes = reader.read_line(&mut line).ok()?;
        if bytes == 0 {
            break;
        }
        consumed = consumed.saturating_add(bytes);
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            if consumed >= MAX_BYTES {
                break;
            }
            continue;
        };
        let payload = value.get("payload").unwrap_or(&value);
        if metadata.id.is_none() {
            metadata.id = payload
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string);
            metadata.cwd = payload
                .get("cwd")
                .and_then(Value::as_str)
                .map(PathBuf::from);
            metadata.source = payload.get("source").map(compact_json_value);
            metadata.parent_id = find_string_key(payload, "parent_thread_id")
                .or_else(|| find_string_key(payload, "parent_id"));
            metadata.started_at = value
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(parse_timestamp);
        }
        if metadata.inferred_title.is_none() {
            metadata.inferred_title = extract_user_title(&value);
        }
        if metadata.id.is_some() && metadata.inferred_title.is_some() {
            break;
        }
        if consumed >= MAX_BYTES {
            break;
        }
    }
    (metadata.id.is_some() || metadata.inferred_title.is_some()).then_some(metadata)
}

fn extract_user_title(value: &Value) -> Option<String> {
    let event_type = value.get("type").and_then(Value::as_str);
    let payload = value.get("payload")?;
    let payload_type = payload.get("type").and_then(Value::as_str);
    let is_user = (event_type == Some("event_msg") && payload_type == Some("user_message"))
        || (payload_type == Some("message")
            && payload.get("role").and_then(Value::as_str) == Some("user"));
    if !is_user {
        return None;
    }
    let text = payload
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| payload.get("text").and_then(Value::as_str))
        .or_else(|| {
            payload.get("content")?.as_array()?.iter().find_map(|item| {
                item.get("text")
                    .and_then(Value::as_str)
                    .or_else(|| item.get("input_text").and_then(Value::as_str))
            })
        })?;
    compact_title(text)
}

fn compact_title(value: &str) -> Option<String> {
    let mut cleaned = value.to_string();
    for tag in [
        "recommended_plugins",
        "app-context",
        "environment_context",
        "INSTRUCTIONS",
    ] {
        strip_tag_blocks(&mut cleaned, tag);
    }
    let normalized = cleaned
        .lines()
        .map(str::trim)
        .filter(|line| {
            !line.is_empty()
                && !line.starts_with('<')
                && !line.starts_with("# AGENTS.md")
                && !line.starts_with("<recommended_plugins>")
        })
        .collect::<Vec<_>>()
        .join(" ");
    if normalized.is_empty() {
        return None;
    }
    let mut title = normalized.chars().take(64).collect::<String>();
    if normalized.chars().count() > 64 {
        title.push('…');
    }
    Some(title)
}

fn strip_tag_blocks(value: &mut String, tag: &str) {
    let opening = format!("<{tag}>");
    let closing = format!("</{tag}>");
    while let Some(start) = value.find(&opening) {
        let Some(relative_end) = value[start + opening.len()..].find(&closing) else {
            value.truncate(start);
            return;
        };
        let end = start + opening.len() + relative_end + closing.len();
        value.replace_range(start..end, " ");
    }
}

fn find_string_key(value: &Value, key: &str) -> Option<String> {
    match value {
        Value::Object(map) => {
            if let Some(found) = map.get(key).and_then(Value::as_str) {
                return Some(found.to_string());
            }
            map.values().find_map(|value| find_string_key(value, key))
        }
        Value::Array(values) => values.iter().find_map(|value| find_string_key(value, key)),
        _ => None,
    }
}

fn compact_json_value(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string())
}

fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

fn is_rollout(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    name.starts_with("rollout-") && (name.ends_with(".jsonl") || name.ends_with(".jsonl.zst"))
}

fn id_from_filename(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let stem = name
        .strip_suffix(".jsonl.zst")
        .or_else(|| name.strip_suffix(".jsonl"))?;
    let candidate = stem.rsplit('-').take(5).collect::<Vec<_>>();
    if candidate.len() != 5 {
        return None;
    }
    Some(candidate.into_iter().rev().collect::<Vec<_>>().join("-"))
}

fn short_id(value: &str) -> String {
    value.chars().take(8).collect()
}

#[allow(dead_code)]
fn system_time(value: SystemTime) -> DateTime<Utc> {
    value.into()
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Write};

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn scans_indexed_and_orphaned_sessions() {
        let root = tempdir().unwrap();
        let sessions = root.path().join("sessions/2026/08/06");
        fs::create_dir_all(&sessions).unwrap();
        fs::write(
            root.path().join("session_index.jsonl"),
            r#"{"id":"019fd34f-df01-7080-9ec3-700dfe108aad","thread_name":"Cleaner","updated_at":"2026-08-06T08:00:00Z"}
not-json
"#,
        )
        .unwrap();
        let mut rollout = File::create(
            sessions.join("rollout-2026-08-06T08-00-00-019fd34f-df01-7080-9ec3-700dfe108aad.jsonl"),
        )
        .unwrap();
        writeln!(
            rollout,
            r#"{{"timestamp":"2026-08-06T07:59:00Z","type":"session_meta","payload":{{"id":"019fd34f-df01-7080-9ec3-700dfe108aad","cwd":"C:\\work"}}}}"#
        )
        .unwrap();
        drop(rollout);

        let report = scan_codex_home(root.path()).unwrap();
        assert_eq!(report.sessions.len(), 1);
        assert_eq!(report.sessions[0].title, "Cleaner");
        assert_eq!(report.sessions[0].status, SessionStatus::Active);
        assert_eq!(report.malformed_index_lines, 1);
        assert!(report.sessions[0].transcript_bytes > 0);
    }

    #[test]
    fn treats_unindexed_canonical_rollout_as_active_and_excludes_index_only_row() {
        let root = tempdir().unwrap();
        let sessions = root.path().join("sessions/2026/08/06");
        fs::create_dir_all(&sessions).unwrap();
        let active_id = "019fd34f-df01-7080-9ec3-700dfe108aad";
        let stale_id = "019fd34f-df01-7080-9ec3-700dfe108aae";
        fs::write(
            root.path().join("session_index.jsonl"),
            format!(
                "{{\"id\":\"{stale_id}\",\"thread_name\":\"stale\",\"updated_at\":\"2026-08-06T08:00:00Z\"}}\n"
            ),
        )
        .unwrap();
        fs::write(
            sessions.join(format!(
                "rollout-2026-08-06T08-00-00-{active_id}.jsonl"
            )),
            format!(
                "{{\"timestamp\":\"2026-08-06T08:00:00Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"{active_id}\"}}}}\n"
            ),
        )
        .unwrap();

        let report = scan_codex_home(root.path()).unwrap();
        let active = report
            .sessions
            .iter()
            .find(|session| session.id == active_id)
            .unwrap();
        assert_eq!(active.status, SessionStatus::Active);
        assert_eq!(report.sessions.len(), 1);
        assert!(!report.sessions.iter().any(|session| session.id == stale_id));
        assert!(report
            .warnings
            .iter()
            .any(|warning| warning.contains("1 条记录没有对应")));
    }

    #[test]
    fn ignores_rollouts_inside_migration_backups() {
        let root = tempdir().unwrap();
        let sessions = root.path().join("sessions/2026/08/06");
        let backup_sessions = root
            .path()
            .join("migration-backups/old/.codex/sessions/2026/08/05");
        fs::create_dir_all(&sessions).unwrap();
        fs::create_dir_all(&backup_sessions).unwrap();
        let active_id = "019fd34f-df01-7080-9ec3-700dfe108aad";
        let backup_id = "019fd34f-df01-7080-9ec3-700dfe108aae";
        fs::write(
            sessions.join(format!("rollout-2026-08-06T08-00-00-{active_id}.jsonl")),
            format!("{{\"payload\":{{\"id\":\"{active_id}\"}}}}\n"),
        )
        .unwrap();
        fs::write(
            backup_sessions.join(format!("rollout-2026-08-05T08-00-00-{backup_id}.jsonl")),
            format!("{{\"payload\":{{\"id\":\"{backup_id}\"}}}}\n"),
        )
        .unwrap();

        let report = scan_codex_home(root.path()).unwrap();
        assert_eq!(report.sessions.len(), 1);
        assert_eq!(report.sessions[0].id, active_id);
    }

    #[test]
    fn scans_compressed_archived_rollout_without_treating_it_as_a_backup() {
        let root = tempdir().unwrap();
        let archived = root.path().join("archived_sessions");
        fs::create_dir_all(&archived).unwrap();
        let id = "019fd34f-df01-7080-9ec3-700dfe108aad";
        let path = archived.join(format!("rollout-2026-08-06T08-00-00-{id}.jsonl.zst"));
        let file = File::create(&path).unwrap();
        let mut encoder = zstd::stream::write::Encoder::new(file, 1).unwrap();
        writeln!(
            encoder,
            "{}",
            serde_json::json!({"timestamp":"2026-08-06T08:00:00Z","type":"session_meta","payload":{"id":id,"cwd":"C:/work"}})
        )
        .unwrap();
        encoder.finish().unwrap();

        let report = scan_codex_home(root.path()).unwrap();
        assert_eq!(report.sessions.len(), 1);
        assert_eq!(report.sessions[0].id, id);
        assert_eq!(report.sessions[0].status, SessionStatus::Archived);
        assert_eq!(report.sessions[0].transcript_paths, vec![path]);
        assert!(report.sessions[0].transcript_bytes > 0);
    }

    #[test]
    fn extracts_uuid_from_rollout_filename() {
        let path =
            Path::new("rollout-2026-08-06T08-00-00-019fd34f-df01-7080-9ec3-700dfe108aad.jsonl");
        assert_eq!(
            id_from_filename(path).as_deref(),
            Some("019fd34f-df01-7080-9ec3-700dfe108aad")
        );
    }

    #[test]
    fn infers_missing_title_from_first_real_user_message() {
        let root = tempdir().unwrap();
        let sessions = root.path().join("sessions/2026/08/06");
        fs::create_dir_all(&sessions).unwrap();
        let id = "019fd34f-df01-7080-9ec3-700dfe108aad";
        let path = sessions.join(format!("rollout-2026-08-06T08-00-00-{id}.jsonl"));
        let mut rollout = File::create(path).unwrap();
        writeln!(
            rollout,
            "{}",
            serde_json::json!({"timestamp":"2026-08-06T08:00:00Z","type":"session_meta","payload":{"id":id,"cwd":"C:/work"}})
        )
        .unwrap();
        writeln!(
            rollout,
            "{}",
            serde_json::json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<recommended_plugins>ignore</recommended_plugins>"}]}})
        )
        .unwrap();
        writeln!(
            rollout,
            "{}",
            serde_json::json!({"type":"event_msg","payload":{"type":"user_message","message":"重新排版并修复所有按钮"}})
        )
        .unwrap();
        drop(rollout);

        let report = scan_codex_home(root.path()).unwrap();
        assert_eq!(report.sessions[0].title, "重新排版并修复所有按钮");
    }

    #[test]
    fn title_ignores_embedded_context_blocks_but_keeps_following_request() {
        let value = "<recommended_plugins>\n- GitHub\n</recommended_plugins>\n\
            <environment_context>\n<cw d=\"C:/work\" />\n</environment_context>\n\
            重新设计清理界面并修复滚动条";
        assert_eq!(
            compact_title(value).as_deref(),
            Some("重新设计清理界面并修复滚动条")
        );
    }

    #[test]
    fn finds_title_after_context_larger_than_the_old_two_megabyte_limit() {
        let root = tempdir().unwrap();
        let sessions = root.path().join("sessions/2026/08/06");
        fs::create_dir_all(&sessions).unwrap();
        let id = "019fd34f-df01-7080-9ec3-700dfe108aad";
        let path = sessions.join(format!("rollout-2026-08-06T08-00-00-{id}.jsonl"));
        let mut rollout = File::create(path).unwrap();
        writeln!(
            rollout,
            "{}",
            serde_json::json!({"timestamp":"2026-08-06T08:00:00Z","type":"session_meta","payload":{"id":id}})
        )
        .unwrap();
        let large_context = format!(
            "<environment_context>{}</environment_context>",
            "x".repeat(2 * 1024 * 1024 + 64)
        );
        writeln!(
            rollout,
            "{}",
            serde_json::json!({"type":"event_msg","payload":{"type":"user_message","message":large_context}})
        )
        .unwrap();
        writeln!(
            rollout,
            "{}",
            serde_json::json!({"type":"event_msg","payload":{"type":"user_message","message":"真正的任务名称"}})
        )
        .unwrap();
        drop(rollout);

        let report = scan_codex_home(root.path()).unwrap();
        assert_eq!(report.sessions[0].title, "真正的任务名称");
    }
}
