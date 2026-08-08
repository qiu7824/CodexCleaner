use std::{
    collections::HashMap,
    env, fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{ScanReport, SessionStatus, SessionSummary};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppServerResponse {
    pub result: Value,
    pub notifications: Vec<Value>,
}

#[derive(Debug, Clone)]
struct OfficialSession {
    id: String,
    title: Option<String>,
    status: SessionStatus,
    updated_at: Option<DateTime<Utc>>,
    started_at: Option<DateTime<Utc>>,
    cwd: Option<PathBuf>,
    source: Option<String>,
    parent_id: Option<String>,
    transcript_path: Option<PathBuf>,
}

pub fn discover_codex_binary(codex_home: &Path, explicit: Option<PathBuf>) -> Option<PathBuf> {
    if let Some(path) = explicit.or_else(|| env::var_os("CODEX_BIN").map(PathBuf::from)) {
        if path.is_file() {
            return Some(path);
        }
    }

    let releases = codex_home.join("packages/standalone/releases");
    if let Some(path) = newest_release_binary(&releases) {
        return Some(path);
    }

    let plugin_binary = codex_home.join("plugins/.plugin-appserver/codex.exe");
    if plugin_binary.is_file() {
        return Some(plugin_binary);
    }

    find_on_path("codex.exe").or_else(|| find_on_path("codex"))
}

pub fn probe_app_server(codex_binary: &Path, timeout: Duration) -> Result<(), String> {
    app_server_request(codex_binary, "model/list", json!({}), timeout).map(|_| ())
}

pub fn delete_thread_official(
    codex_binary: &Path,
    thread_id: &str,
    timeout: Duration,
) -> Result<AppServerResponse, String> {
    if thread_id.trim().is_empty() {
        return Err("thread id is empty".to_string());
    }
    app_server_request(
        codex_binary,
        "thread/delete",
        json!({ "threadId": thread_id }),
        timeout,
    )
}

pub fn read_thread_official(
    codex_binary: &Path,
    thread_id: &str,
    timeout: Duration,
) -> Result<AppServerResponse, String> {
    if thread_id.trim().is_empty() {
        return Err("thread id is empty".to_string());
    }
    app_server_request(
        codex_binary,
        "thread/read",
        json!({ "threadId": thread_id, "includeTurns": false }),
        timeout,
    )
}

pub fn find_existing_threads_official(
    codex_binary: &Path,
    thread_ids: &[String],
    timeout: Duration,
) -> Result<Vec<String>, String> {
    if thread_ids.is_empty() {
        return Ok(Vec::new());
    }
    let requested = thread_ids
        .iter()
        .map(String::as_str)
        .collect::<std::collections::HashSet<_>>();
    let source_kinds = all_source_kinds();
    let mut remaining = std::collections::BTreeSet::<String>::new();
    for archived in [false, true] {
        let mut cursor: Option<String> = None;
        for _ in 0..100 {
            let response = app_server_request(
                codex_binary,
                "thread/list",
                json!({
                    "cursor": cursor,
                    "limit": 1000,
                    "sortKey": "updated_at",
                    "archived": archived,
                    "sourceKinds": source_kinds.clone(),
                    "useStateDbOnly": false
                }),
                timeout,
            )?;
            if let Some(items) = response.result.get("data").and_then(Value::as_array) {
                for id in items
                    .iter()
                    .filter_map(|item| item.get("id").and_then(Value::as_str))
                {
                    if requested.contains(id) {
                        remaining.insert(id.to_string());
                    }
                }
            }
            cursor = response
                .result
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_string);
            if cursor.is_none() || remaining.len() == requested.len() {
                break;
            }
        }
    }
    Ok(remaining.into_iter().collect())
}

pub fn enrich_session_titles_official(
    report: &mut ScanReport,
    codex_binary: &Path,
    timeout: Duration,
) -> Result<usize, String> {
    let source_kinds = all_source_kinds();
    let mut official = HashMap::<String, OfficialSession>::new();
    for archived in [false, true] {
        let mut cursor: Option<String> = None;
        for _ in 0..100 {
            let response = app_server_request(
                codex_binary,
                "thread/list",
                json!({
                    "cursor": cursor,
                    "limit": 1000,
                    "sortKey": "updated_at",
                    "archived": archived,
                    "sourceKinds": source_kinds.clone(),
                    "useStateDbOnly": false
                }),
                timeout,
            )?;
            if let Some(items) = response.result.get("data").and_then(Value::as_array) {
                for item in items {
                    let Some(id) = item.get("id").and_then(Value::as_str) else {
                        continue;
                    };
                    let title = item
                        .get("name")
                        .and_then(Value::as_str)
                        .filter(|value| !value.trim().is_empty())
                        .or_else(|| item.get("preview").and_then(Value::as_str))
                        .map(compact_official_title)
                        .filter(|value| !value.is_empty());
                    official.insert(
                        id.to_string(),
                        OfficialSession {
                            id: id.to_string(),
                            title,
                            status: if archived {
                                SessionStatus::Archived
                            } else {
                                SessionStatus::Active
                            },
                            updated_at: unix_timestamp(item, "updatedAt"),
                            started_at: unix_timestamp(item, "createdAt"),
                            cwd: item
                                .get("cwd")
                                .and_then(Value::as_str)
                                .filter(|value| !value.trim().is_empty())
                                .map(PathBuf::from),
                            source: item.get("source").map(compact_official_source),
                            parent_id: item
                                .get("parentThreadId")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                            transcript_path: item
                                .get("path")
                                .and_then(Value::as_str)
                                .filter(|value| !value.trim().is_empty())
                                .map(PathBuf::from),
                        },
                    );
                }
            }
            cursor = response
                .result
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_string);
            if cursor.is_none() {
                break;
            }
        }
    }
    Ok(merge_official_sessions(report, official))
}

fn merge_official_sessions(
    report: &mut ScanReport,
    official: HashMap<String, OfficialSession>,
) -> usize {
    let mut changed_count = 0_usize;
    let mut official_without_transcript = 0_usize;

    for official_session in official.into_values() {
        if let Some(session) = report
            .sessions
            .iter_mut()
            .find(|session| session.id == official_session.id)
        {
            let mut changed = false;
            if session.status != official_session.status {
                session.status = official_session.status;
                changed = true;
            }
            if let Some(title) = official_session.title.as_ref() {
                if session.title != *title {
                    session.title = title.clone();
                    changed = true;
                }
            }
            if official_session.updated_at.is_some()
                && session.updated_at != official_session.updated_at
            {
                session.updated_at = official_session.updated_at;
                changed = true;
            }
            if official_session.started_at.is_some()
                && session.started_at != official_session.started_at
            {
                session.started_at = official_session.started_at;
                changed = true;
            }
            if official_session.cwd.is_some() && session.cwd != official_session.cwd {
                session.cwd = official_session.cwd.clone();
                changed = true;
            }
            if official_session.source.is_some() && session.source != official_session.source {
                session.source = official_session.source.clone();
                changed = true;
            }
            if official_session.parent_id.is_some()
                && session.parent_id != official_session.parent_id
            {
                session.parent_id = official_session.parent_id.clone();
                changed = true;
            }
            if let Some(path) = official_session
                .transcript_path
                .as_ref()
                .filter(|path| path.is_file())
            {
                if !session
                    .transcript_paths
                    .iter()
                    .any(|existing| same_path(existing, path))
                {
                    session.transcript_bytes = session
                        .transcript_bytes
                        .saturating_add(fs::metadata(path).map(|value| value.len()).unwrap_or(0));
                    session.transcript_paths.push(path.clone());
                    changed = true;
                }
            }
            changed_count += usize::from(changed);
            continue;
        }

        let transcript_paths = official_session
            .transcript_path
            .filter(|path| path.is_file())
            .into_iter()
            .collect::<Vec<_>>();
        let transcript_bytes = transcript_paths
            .iter()
            .map(|path| fs::metadata(path).map(|value| value.len()).unwrap_or(0))
            .sum();
        if transcript_paths.is_empty() {
            official_without_transcript = official_without_transcript.saturating_add(1);
        }
        report.sessions.push(SessionSummary {
            id: official_session.id.clone(),
            title: official_session.title.unwrap_or_else(|| {
                format!(
                    "官方任务 {}",
                    official_session.id.chars().take(8).collect::<String>()
                )
            }),
            status: official_session.status,
            updated_at: official_session.updated_at,
            started_at: official_session.started_at,
            cwd: official_session.cwd,
            source: official_session.source,
            parent_id: official_session.parent_id,
            transcript_paths,
            transcript_bytes,
        });
        changed_count = changed_count.saturating_add(1);
    }

    if official_without_transcript > 0 {
        report.warnings.push(format!(
            "Codex 官方目录返回 {official_without_transcript} 个当前未落盘的任务；已展示任务信息，但不计入本地可释放空间"
        ));
    }
    report.transcript_bytes = report
        .sessions
        .iter()
        .map(|session| session.transcript_bytes)
        .sum();
    report.sessions.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    changed_count
}

fn unix_timestamp(value: &Value, key: &str) -> Option<DateTime<Utc>> {
    value
        .get(key)
        .and_then(Value::as_i64)
        .and_then(|seconds| DateTime::<Utc>::from_timestamp(seconds, 0))
}

fn compact_official_source(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string())
}

fn same_path(left: &Path, right: &Path) -> bool {
    left.to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .eq_ignore_ascii_case(
            right
                .to_string_lossy()
                .replace('\\', "/")
                .trim_end_matches('/'),
        )
}

fn all_source_kinds() -> Value {
    json!([
        "cli",
        "vscode",
        "exec",
        "appServer",
        "subAgent",
        "subAgentReview",
        "subAgentCompact",
        "subAgentThreadSpawn",
        "subAgentOther",
        "unknown"
    ])
}

fn compact_official_title(value: &str) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut title = normalized.chars().take(80).collect::<String>();
    if normalized.chars().count() > 80 {
        title.push('…');
    }
    title
}

fn app_server_request(
    codex_binary: &Path,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<AppServerResponse, String> {
    let mut command = Command::new(codex_binary);
    command
        .args(["app-server", "--listen", "stdio://"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to start {}: {error}", codex_binary.display()))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "app-server stdin is unavailable".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "app-server stdout is unavailable".to_string())?;
    let (sender, receiver) = mpsc::channel();
    let reader = thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let value = match line {
                Ok(line) => serde_json::from_str::<Value>(&line)
                    .map_err(|error| format!("invalid app-server response: {error}")),
                Err(error) => Err(format!("failed to read app-server response: {error}")),
            };
            if sender.send(value).is_err() {
                break;
            }
        }
    });

    let outcome = (|| {
        write_message(
            &mut stdin,
            &json!({
                "method": "initialize",
                "id": 1,
                "params": {
                    "clientInfo": {
                        "name": "codex_cleaner",
                        "title": "Codex Cleaner",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }
            }),
        )?;
        wait_for_response(&receiver, 1, timeout)?;
        write_message(
            &mut stdin,
            &json!({ "method": "initialized", "params": {} }),
        )?;
        write_message(
            &mut stdin,
            &json!({ "method": method, "id": 2, "params": params }),
        )?;

        let mut notifications = Vec::new();
        let deadline = Instant::now() + timeout;
        loop {
            let value = receive_before(&receiver, deadline)
                .map_err(|_| format!("app-server request timed out: {method}"))??;
            if value.get("id").and_then(Value::as_u64) == Some(2) {
                if let Some(error) = value.get("error") {
                    return Err(format!("app-server {method} failed: {error}"));
                }
                return Ok(AppServerResponse {
                    result: value.get("result").cloned().unwrap_or(Value::Null),
                    notifications,
                });
            }
            if value.get("method").is_some() {
                notifications.push(value);
            }
        }
    })();

    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    let _ = reader.join();
    outcome
}

fn write_message(stdin: &mut impl Write, value: &Value) -> Result<(), String> {
    serde_json::to_writer(&mut *stdin, value).map_err(|error| error.to_string())?;
    stdin.write_all(b"\n").map_err(|error| error.to_string())?;
    stdin.flush().map_err(|error| error.to_string())
}

fn wait_for_response(
    receiver: &mpsc::Receiver<Result<Value, String>>,
    id: u64,
    timeout: Duration,
) -> Result<Value, String> {
    let deadline = Instant::now() + timeout;
    loop {
        let value = receive_before(receiver, deadline)
            .map_err(|_| format!("app-server initialize timed out for request {id}"))??;
        if value.get("id").and_then(Value::as_u64) == Some(id) {
            if let Some(error) = value.get("error") {
                return Err(format!("app-server initialize failed: {error}"));
            }
            return Ok(value);
        }
    }
}

fn receive_before<T>(
    receiver: &mpsc::Receiver<T>,
    deadline: Instant,
) -> Result<T, mpsc::RecvTimeoutError> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(mpsc::RecvTimeoutError::Timeout);
    }
    receiver.recv_timeout(remaining)
}

fn newest_release_binary(root: &Path) -> Option<PathBuf> {
    fs::read_dir(root)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path().join("bin/codex.exe");
            let modified = fs::metadata(&path).ok()?.modified().ok()?;
            Some((modified, path))
        })
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path)
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    env::split_paths(&env::var_os("PATH")?).find_map(|folder| {
        let path = folder.join(name);
        path.is_file().then_some(path)
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn prefers_newest_packaged_release() {
        let root = tempdir().unwrap();
        let first = root.path().join("packages/standalone/releases/0.1/bin");
        let second = root.path().join("packages/standalone/releases/0.2/bin");
        fs::create_dir_all(&first).unwrap();
        fs::write(first.join("codex.exe"), b"first").unwrap();
        thread::sleep(Duration::from_millis(5));
        fs::create_dir_all(&second).unwrap();
        fs::write(second.join("codex.exe"), b"second").unwrap();
        let found = discover_codex_binary(root.path(), None).unwrap();
        assert_eq!(found, second.join("codex.exe"));
    }

    #[test]
    fn official_catalog_enriches_local_rows_and_adds_pathless_tasks_without_counting_bytes() {
        let root = tempdir().unwrap();
        let transcript = root.path().join("rollout-local.jsonl");
        fs::write(&transcript, b"local transcript").unwrap();
        let local_id = "019fd34f-df01-7080-9ec3-700dfe108aad";
        let cloud_id = "019fd34f-df01-7080-9ec3-700dfe108aae";
        let transcript_bytes = fs::metadata(&transcript).unwrap().len();
        let mut report = ScanReport {
            codex_home: root.path().to_path_buf(),
            sessions: vec![SessionSummary {
                id: local_id.to_string(),
                title: "本地任务".to_string(),
                status: SessionStatus::Orphaned,
                updated_at: None,
                started_at: None,
                cwd: None,
                source: None,
                parent_id: None,
                transcript_paths: vec![transcript.clone()],
                transcript_bytes,
            }],
            transcript_bytes,
            malformed_index_lines: 0,
            warnings: vec![],
        };
        let timestamp = DateTime::<Utc>::from_timestamp(1_786_134_800, 0).unwrap();
        let mut official = HashMap::new();
        official.insert(
            local_id.to_string(),
            OfficialSession {
                id: local_id.to_string(),
                title: Some("官方标题".to_string()),
                status: SessionStatus::Active,
                updated_at: Some(timestamp),
                started_at: Some(timestamp),
                cwd: Some(PathBuf::from("C:/work")),
                source: Some("vscode".to_string()),
                parent_id: None,
                transcript_path: Some(transcript),
            },
        );
        official.insert(
            cloud_id.to_string(),
            OfficialSession {
                id: cloud_id.to_string(),
                title: Some("未落盘任务".to_string()),
                status: SessionStatus::Archived,
                updated_at: Some(timestamp),
                started_at: Some(timestamp),
                cwd: Some(PathBuf::from("C:/cloud")),
                source: Some("appServer".to_string()),
                parent_id: None,
                transcript_path: None,
            },
        );

        assert_eq!(merge_official_sessions(&mut report, official), 2);
        assert_eq!(report.sessions.len(), 2);
        assert_eq!(report.transcript_bytes, transcript_bytes);
        let local = report
            .sessions
            .iter()
            .find(|session| session.id == local_id)
            .unwrap();
        assert_eq!(local.title, "官方标题");
        assert_eq!(local.status, SessionStatus::Active);
        assert_eq!(local.transcript_paths.len(), 1);
        let cloud = report
            .sessions
            .iter()
            .find(|session| session.id == cloud_id)
            .unwrap();
        assert_eq!(cloud.status, SessionStatus::Archived);
        assert_eq!(cloud.transcript_bytes, 0);
        assert!(report
            .warnings
            .iter()
            .any(|warning| warning.contains("1 个当前未落盘")));
    }
}
