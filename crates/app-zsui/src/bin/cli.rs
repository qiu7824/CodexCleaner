use std::{env, path::PathBuf, time::Duration};

use cleaner_core::{
    analyze_session, build_cleanup_plan, build_safe_storage_cleanup_preview, codex_process_running,
    discover_codex_binary, discover_codex_home, enrich_session_titles_official, probe_app_server,
    read_thread_official, scan_codex_home, scan_codex_storage, AnalysisOptions,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.is_empty()
        || args
            .iter()
            .any(|value| matches!(value.as_str(), "--help" | "-h"))
    {
        print_help();
        return Ok(());
    }
    if args
        .iter()
        .any(|value| matches!(value.as_str(), "--version" | "-V"))
    {
        println!("codex-cleaner-cli {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
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

    if args.iter().any(|value| value == "--storage-json") {
        let storage = scan_codex_storage(&home);
        println!("{}", serde_json::to_string_pretty(&storage)?);
        return Ok(());
    }
    if args.iter().any(|value| {
        matches!(
            value.as_str(),
            "--storage-dry-run-json" | "--storage-plan-json"
        )
    }) {
        let storage = scan_codex_storage(&home);
        let preview = build_safe_storage_cleanup_preview(&storage, codex_process_running());
        println!("{}", serde_json::to_string_pretty(&preview)?);
        return Ok(());
    }

    let mut report = scan_codex_home(&home)?;
    if let Some(binary) = codex_binary.as_ref() {
        if let Err(error) =
            enrich_session_titles_official(&mut report, binary, Duration::from_secs(20))
        {
            report.warnings.push(format!(
                "Codex 官方任务列表不可用，已保留本地扫描结果：{error}"
            ));
        }
    } else {
        report
            .warnings
            .push("未找到 Codex CLI，任务名称和状态仅来自本地记录".to_string());
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
    if let Some(session_id) = argument_value(&args, "--task-dry-run-json") {
        let analysis = analyze_session(&report, session_id, AnalysisOptions::default())?;
        let plan = build_cleanup_plan(&analysis);
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema_version": 1,
                "dry_run": true,
                "analysis_truncated": analysis.truncated,
                "analysis_warnings": analysis.warnings,
                "plan": plan
            }))?
        );
        return Ok(());
    }

    Err("未识别的命令；请使用 --help 查看用法".into())
}

fn argument_value<'a>(args: &'a [String], key: &str) -> Option<&'a str> {
    args.windows(2)
        .find(|pair| pair[0] == key)
        .map(|pair| pair[1].as_str())
}

fn print_help() {
    println!(
        "Codex Cleaner CLI\n\n\
只读命令：\n  \
--scan-json                         扫描任务、子任务与 transcript\n  \
--analyze <任务 UUID>             分析任务关联资源\n  \
--task-dry-run-json <任务 UUID>   输出任务清理计划，不执行\n  \
--storage-json                      盘点 Codex 存储和更新残留候选\n  \
--storage-dry-run-json              输出安全规则预览，不执行\n  \
--probe-official                    检查 Codex App Server\n  \
--probe-thread <任务 UUID>         检查官方接口能否读取任务\n\n\
通用选项：\n  \
--codex-home <目录>                指定 Codex 数据目录\n  \
--codex-bin <文件>                 指定 Codex CLI\n  \
--help                             显示帮助\n  \
--version                          显示版本\n\n\
安全边界：所有 dry-run 命令都只读，不删除任务或文件。"
    );
}
