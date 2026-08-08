use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    time::SystemTime,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::{StorageAction, StorageCategory, StorageItem, StorageReport, StorageSafety};

#[derive(Debug, Clone, Default)]
struct PathStats {
    bytes: u64,
    files: u64,
    newest: Option<SystemTime>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageCleanupReceipt {
    pub created_at: DateTime<Utc>,
    pub recycled_paths: Vec<PathBuf>,
    pub failed_paths: Vec<(PathBuf, String)>,
    pub journal_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageCleanupPreview {
    pub schema_version: u32,
    pub generated_at: DateTime<Utc>,
    pub dry_run: bool,
    pub codex_running: bool,
    pub requires_codex_exit: bool,
    pub candidate_count: usize,
    pub candidate_bytes: u64,
    pub candidates: Vec<StorageItem>,
    pub manual_review_count: usize,
    pub protected_count: usize,
    pub warnings: Vec<String>,
}

pub fn scan_codex_storage(codex_home: impl AsRef<Path>) -> StorageReport {
    let local_app_data = env::var_os("LOCALAPPDATA").map(PathBuf::from);
    let mut report = scan_codex_storage_base_at(codex_home.as_ref(), local_app_data.as_deref());
    add_installed_codex_app_packages(&mut report);
    finalize_storage_report(&mut report);
    report
}

pub fn apply_safe_storage_rules(report: &mut StorageReport) {
    for item in &mut report.items {
        item.action = if item.safety == StorageSafety::SafeAfterExit
            && item.stale_days.is_some_and(|days| days >= 7)
            && matches!(
                item.category,
                StorageCategory::Cache | StorageCategory::Temporary
            ) {
            StorageAction::Clean
        } else if item.safety == StorageSafety::Review {
            StorageAction::Review
        } else {
            StorageAction::Keep
        };
    }
}

pub fn build_safe_storage_cleanup_preview(
    report: &StorageReport,
    codex_running: bool,
) -> StorageCleanupPreview {
    let mut planned = report.clone();
    apply_safe_storage_rules(&mut planned);
    let candidates = planned
        .items
        .iter()
        .filter(|item| item.action == StorageAction::Clean)
        .cloned()
        .collect::<Vec<_>>();
    let candidate_bytes = candidates
        .iter()
        .map(|item| item.size)
        .fold(0_u64, u64::saturating_add);
    let mut warnings = planned.warnings;
    if codex_running {
        warnings.push("Codex 正在运行；这份清单仅是 dry-run，实际执行将被拒绝".to_string());
    }
    StorageCleanupPreview {
        schema_version: 1,
        generated_at: Utc::now(),
        dry_run: true,
        codex_running,
        requires_codex_exit: true,
        candidate_count: candidates.len(),
        candidate_bytes,
        candidates,
        manual_review_count: planned
            .items
            .iter()
            .filter(|item| item.safety == StorageSafety::Review)
            .count(),
        protected_count: planned
            .items
            .iter()
            .filter(|item| item.safety == StorageSafety::Protected)
            .count(),
        warnings,
    }
}

pub fn codex_process_running() -> bool {
    let mut command = Command::new("tasklist");
    command.args(["/FO", "CSV", "/NH"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let Ok(output) = command.output() else {
        return false;
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split(',').next())
        .map(|value| value.trim_matches('"').to_ascii_lowercase())
        .any(|name| name == "codex.exe")
}

pub fn execute_storage_cleanup(
    report: &StorageReport,
    journal_root: impl AsRef<Path>,
) -> Result<StorageCleanupReceipt, String> {
    if codex_process_running() {
        return Err("检测到 Codex 正在运行；请退出 Codex 后再清理全局缓存和备份".to_string());
    }
    let targets = report
        .items
        .iter()
        .filter(|item| {
            item.action == StorageAction::Clean
                && item.safety != StorageSafety::Protected
                && item.path.exists()
        })
        .map(|item| item.path.clone())
        .collect::<Vec<_>>();
    let mut targets = targets;
    targets.sort_by_key(|path| path.components().count());
    targets.dedup();
    let mut executable_targets = Vec::<PathBuf>::new();
    for target in targets {
        if executable_targets
            .iter()
            .any(|parent| target == *parent || target.starts_with(parent))
        {
            continue;
        }
        executable_targets.push(target);
    }
    if executable_targets.is_empty() {
        return Err("没有已标记且允许自动清理的项目".to_string());
    }
    let mut receipt = StorageCleanupReceipt {
        created_at: Utc::now(),
        recycled_paths: Vec::new(),
        failed_paths: Vec::new(),
        journal_path: PathBuf::new(),
    };
    for path in executable_targets {
        if let Err(error) = validate_storage_target(&path) {
            receipt.failed_paths.push((path, error));
            continue;
        }
        match trash::delete(&path) {
            Ok(()) => receipt.recycled_paths.push(path),
            Err(error) => receipt.failed_paths.push((path, error.to_string())),
        }
    }
    let root = journal_root.as_ref();
    fs::create_dir_all(root).map_err(|error| error.to_string())?;
    let journal_path = root.join(format!(
        "storage-{}-{}-{}.json",
        receipt.created_at.format("%Y%m%d-%H%M%S"),
        receipt.created_at.timestamp_subsec_millis(),
        std::process::id()
    ));
    receipt.journal_path = journal_path.clone();
    fs::write(
        &journal_path,
        serde_json::to_vec_pretty(&receipt).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    Ok(receipt)
}

fn validate_storage_target(path: &Path) -> Result<(), String> {
    if !path.is_absolute() || path.parent().is_none() {
        return Err(format!("拒绝清理不安全路径：{}", path.display()));
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("无法重新核对 {}：{error}", path.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "拒绝直接清理符号链接或目录联接：{}",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
fn scan_codex_storage_at(codex_home: &Path, local_app_data: Option<&Path>) -> StorageReport {
    let mut report = scan_codex_storage_base_at(codex_home, local_app_data);
    finalize_storage_report(&mut report);
    report
}

fn scan_codex_storage_base_at(codex_home: &Path, local_app_data: Option<&Path>) -> StorageReport {
    let mut report = StorageReport {
        roots: vec![codex_home.to_path_buf()],
        items: Vec::new(),
        total_bytes: path_stats(codex_home).bytes,
        warnings: Vec::new(),
    };

    add_known(
        &mut report,
        codex_home.join("sessions"),
        "活跃对话记录",
        StorageCategory::Conversation,
        StorageSafety::Protected,
        "必须按任务使用 Codex 官方 thread/delete；不能整目录删除",
    );
    add_known(
        &mut report,
        codex_home.join("archived_sessions"),
        "已归档对话记录",
        StorageCategory::Conversation,
        StorageSafety::Protected,
        "归档仍是用户历史，只能按任务确认后删除",
    );
    add_children(
        &mut report,
        &codex_home.join("migration-backups"),
        StorageCategory::Backup,
        StorageSafety::Review,
        "迁移或修复回滚副本；确认当前状态正常并至少保留最近一份",
    );
    add_children(
        &mut report,
        &codex_home.join("backups"),
        StorageCategory::Backup,
        StorageSafety::Review,
        "历史配置或状态备份；确认对应功能正常后可清理旧版本",
    );
    add_children(
        &mut report,
        &codex_home.join(".cache"),
        StorageCategory::Cache,
        StorageSafety::SafeAfterExit,
        "可重新下载或重建的 Codex 运行缓存；再次使用时会产生下载和启动成本",
    );
    add_children(
        &mut report,
        &codex_home.join(".tmp"),
        StorageCategory::Temporary,
        StorageSafety::SafeAfterExit,
        "插件同步、市场目录和安装过程的临时副本；仅在 Codex 完全退出后处理",
    );
    add_children(
        &mut report,
        &codex_home.join("cache"),
        StorageCategory::Cache,
        StorageSafety::SafeAfterExit,
        "应用目录和插件目录缓存，可重建；近期使用项不会被一键规则选中",
    );
    add_granular_children(
        &mut report,
        &codex_home.join("temp"),
        StorageCategory::Temporary,
        StorageSafety::Review,
        "任务过程临时项可能包含预览、转换中间件或候选成果，需逐项核对",
        32 * 1024 * 1024,
    );
    add_uuid_asset_directories(
        &mut report,
        &codex_home.join("visualizations"),
        "任务可视化",
    );
    add_uuid_asset_directories(
        &mut report,
        &codex_home.join("generated_images"),
        "任务生成图片",
    );
    add_children(
        &mut report,
        &codex_home.join("attachments"),
        StorageCategory::UserAsset,
        StorageSafety::Review,
        "随机 UUID 附件副本；应结合任务分析和原始输入材料逐项判断",
    );

    for (name, label, category, safety, reason) in [
        (
            "plugins",
            "已安装插件",
            StorageCategory::Extension,
            StorageSafety::Protected,
            "包含当前插件和 App Server，直接清理会破坏功能",
        ),
        (
            "skills",
            "个人与系统技能",
            StorageCategory::Extension,
            StorageSafety::Protected,
            "包含用户工作流和插件技能，不属于残留",
        ),
        (
            "packages",
            "当前 Codex 独立运行包",
            StorageCategory::Runtime,
            StorageSafety::Protected,
            "current 指向正在使用的正式版本，不应按重复文件计算或删除",
        ),
        (
            ".sandbox",
            "Windows 沙箱状态",
            StorageCategory::State,
            StorageSafety::Protected,
            "可能被当前任务使用，需由 Codex 自身维护",
        ),
        (
            ".sandbox-secrets",
            "Windows 沙箱密钥状态",
            StorageCategory::State,
            StorageSafety::Protected,
            "包含权限隔离所需状态，不属于残留文件",
        ),
        (
            ".sandbox-bin",
            "Windows 沙箱组件",
            StorageCategory::Runtime,
            StorageSafety::Protected,
            "Codex 权限隔离依赖的运行组件",
        ),
        (
            "process_manager",
            "任务进程状态",
            StorageCategory::State,
            StorageSafety::Protected,
            "可能对应仍在运行或可继续的任务",
        ),
        (
            "browser",
            "Codex 浏览器状态",
            StorageCategory::State,
            StorageSafety::Protected,
            "可能包含登录态、站点数据和任务浏览记录",
        ),
        (
            "computer-use",
            "桌面控制状态",
            StorageCategory::State,
            StorageSafety::Protected,
            "可能包含正在使用的桌面控制运行状态",
        ),
        (
            "node_repl",
            "Node REPL 会话状态",
            StorageCategory::State,
            StorageSafety::Protected,
            "可能被仍可继续的任务引用，不能按缓存直接删除",
        ),
        (
            "memories",
            "Codex 记忆数据",
            StorageCategory::State,
            StorageSafety::Protected,
            "属于用户记忆和任务上下文，不是缓存",
        ),
        (
            "rules",
            "Codex 规则",
            StorageCategory::State,
            StorageSafety::Protected,
            "属于用户配置，不是残留",
        ),
        (
            "thread-writer-locks",
            "任务写入锁",
            StorageCategory::State,
            StorageSafety::Protected,
            "由 Codex 管理任务写入一致性，运行期间不得处理",
        ),
        (
            "sqlite",
            "数据库运行组件",
            StorageCategory::Runtime,
            StorageSafety::Protected,
            "当前数据库功能依赖的组件，不属于可清理备份",
        ),
        (
            "ambient-suggestions",
            "智能建议状态",
            StorageCategory::State,
            StorageSafety::Protected,
            "属于当前产品功能状态，不能仅凭目录名称判断为缓存",
        ),
        (
            "dictation-history",
            "听写历史",
            StorageCategory::UserAsset,
            StorageSafety::Protected,
            "属于用户历史数据",
        ),
        (
            "pets",
            "Codex Pets 数据",
            StorageCategory::UserAsset,
            StorageSafety::Protected,
            "属于用户功能数据，不是清理残留",
        ),
        (
            "vendor_imports",
            "外部代理导入数据",
            StorageCategory::State,
            StorageSafety::Protected,
            "可能用于导入会话恢复和追踪",
        ),
    ] {
        add_known(
            &mut report,
            codex_home.join(name),
            label,
            category,
            safety,
            reason,
        );
    }

    add_standalone_release_inventory(&mut report, &codex_home.join("packages/standalone"));

    for entry in fs::read_dir(codex_home)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
    {
        let name = entry.file_name().to_string_lossy().to_string();
        let lower = name.to_ascii_lowercase();
        if lower.starts_with("backup-") || lower.starts_with("before-force-restore-") {
            add_known(
                &mut report,
                entry.path(),
                &format!("修复前备份 {name}"),
                StorageCategory::Backup,
                StorageSafety::Review,
                "人工修复产生的回滚副本；确认修复稳定后可删旧留新",
            );
        } else if lower.starts_with("..codex-global-state.json.tmp-") {
            add_known(
                &mut report,
                entry.path(),
                "遗留状态临时文件",
                StorageCategory::Temporary,
                StorageSafety::SafeAfterExit,
                "原子写入未完成后遗留的旧临时文件，不是当前状态文件",
            );
        }
    }

    for name in [
        "logs_2.sqlite",
        "state_5.sqlite",
        "goals_1.sqlite",
        "memories_1.sqlite",
        "session_index.jsonl",
        ".codex-global-state.json",
        "config.toml",
        "auth.json",
        "AGENTS.md",
        "version.json",
    ] {
        add_known(
            &mut report,
            codex_home.join(name),
            name,
            StorageCategory::State,
            StorageSafety::Protected,
            "当前配置、身份或共享数据库；不得绕过官方接口直接删除",
        );
    }
    add_diagnostic_children(&mut report, &codex_home.join("log"), "Codex CLI 日志");
    add_diagnostic_children(&mut report, &codex_home.join("restore-logs"), "恢复日志");
    add_diagnostic_children(&mut report, &codex_home.join("migration-logs"), "迁移日志");
    add_diagnostic_children(&mut report, &codex_home.join(".sandbox"), "沙箱日志");
    add_known(
        &mut report,
        codex_home.join("models_cache.json"),
        "模型目录缓存",
        StorageCategory::Cache,
        StorageSafety::SafeAfterExit,
        "模型元数据可由 Codex 重新获取；退出 Codex 后可重建",
    );
    add_known(
        &mut report,
        codex_home.join("computer-use-turn-ended"),
        "已结束的桌面控制回合",
        StorageCategory::Temporary,
        StorageSafety::SafeAfterExit,
        "只记录已结束的桌面控制回合，可在退出 Codex 后清理旧项",
    );
    protect_database_companions(&mut report, codex_home);
    add_top_level_backup_files(&mut report, codex_home);

    if let Some(local) = local_app_data {
        let runtime_root = local.join("OpenAI/Codex");
        if runtime_root.exists() {
            report.roots.push(runtime_root.clone());
            report.total_bytes = report
                .total_bytes
                .saturating_add(path_stats(&runtime_root).bytes);
            add_known(
                &mut report,
                runtime_root.join("bin"),
                "Codex 本机运行组件",
                StorageCategory::Runtime,
                StorageSafety::Protected,
                "包含 Codex、Node、沙箱和命令执行器，不能按版本文件名猜测删除",
            );
            add_content_addressed_runtime_directories(
                &mut report,
                &runtime_root.join("bin"),
                "Codex 本机运行组件",
            );
            add_unmanaged_runtime_component_files(&mut report, &runtime_root.join("bin"));
            add_known(
                &mut report,
                runtime_root.join("runtimes"),
                "桌面控制运行时",
                StorageCategory::Runtime,
                StorageSafety::Protected,
                "Computer Use 等功能依赖的运行时，不是普通缓存",
            );
            add_known(
                &mut report,
                runtime_root.clone(),
                "Codex 本机运行目录的其他数据",
                StorageCategory::Runtime,
                StorageSafety::Protected,
                "用于解释未被子项单列的运行数据；父目录不会整体清理",
            );
        }

        let packages = local.join("Packages");
        if let Ok(entries) = fs::read_dir(&packages) {
            for entry in entries.filter_map(Result::ok) {
                if !entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("OpenAI.Codex_")
                {
                    continue;
                }
                let package_root = entry.path();
                report.roots.push(package_root.clone());
                report.total_bytes = report
                    .total_bytes
                    .saturating_add(path_stats(&package_root).bytes);
                let roaming = package_root.join("LocalCache/Roaming/Codex");
                let packaged_runtime = package_root.join("LocalCache/Local/OpenAI/Codex");
                add_known(
                    &mut report,
                    packaged_runtime.join("bin"),
                    "桌面应用内置运行组件",
                    StorageCategory::Runtime,
                    StorageSafety::Protected,
                    "包含桌面应用当前使用的 Codex、Node 和辅助程序，不能按重复文件删除",
                );
                add_content_addressed_runtime_directories(
                    &mut report,
                    &packaged_runtime.join("bin"),
                    "桌面应用运行组件",
                );
                add_known(
                    &mut report,
                    packaged_runtime.join("runtimes"),
                    "桌面应用控制运行时",
                    StorageCategory::Runtime,
                    StorageSafety::Protected,
                    "Computer Use 等桌面能力依赖的运行时",
                );
                for relative in [
                    "Cache",
                    "GPUCache",
                    "DawnGraphiteCache",
                    "DawnWebGPUCache",
                    "Code Cache",
                    "web/Codex/Default/Cache",
                    "web/Codex/Default/Code Cache",
                    "web/Codex/Default/GPUCache",
                    "web/Codex/GrShaderCache",
                    "web/Codex/GraphiteDawnCache",
                    "web/Codex/ShaderCache",
                    "web/Codex/component_crx_cache",
                    "web/Codex/extensions_crx_cache",
                ] {
                    add_known(
                        &mut report,
                        roaming.join(relative),
                        &format!("桌面端浏览器缓存 · {relative}"),
                        StorageCategory::Cache,
                        StorageSafety::SafeAfterExit,
                        "Chromium 可重建缓存；会保留登录、Cookie、Local Storage 和浏览器配置",
                    );
                }
                add_dated_log_folders(
                    &mut report,
                    &package_root.join("LocalCache/Local/Codex/Logs"),
                    "桌面端日志",
                );
                add_known(
                    &mut report,
                    roaming.join("Partitions"),
                    "旧浏览器分区数据",
                    StorageCategory::State,
                    StorageSafety::Review,
                    "可能包含站点会话或历史浏览器分区，不能当缓存自动删除",
                );
                add_known(
                    &mut report,
                    roaming.join("web/Codex/Default/Partitions"),
                    "浏览器站点分区数据",
                    StorageCategory::State,
                    StorageSafety::Review,
                    "可能包含任务浏览器站点会话；仅在不再需要相关站点状态时处理",
                );
                for relative in [
                    "web/Codex/Default/Local Storage",
                    "web/Codex/Default/Network",
                    "web/Codex/Default/Extensions",
                    "web/Codex/Default/Login Data",
                    "web/Codex/Default/Cookies",
                ] {
                    add_known(
                        &mut report,
                        roaming.join(relative),
                        &format!("浏览器用户状态 · {relative}"),
                        StorageCategory::State,
                        StorageSafety::Protected,
                        "包含登录态、扩展或站点持久数据，不属于可重建缓存",
                    );
                }
                add_known(
                    &mut report,
                    package_root.clone(),
                    "桌面应用包的其他数据",
                    StorageCategory::Runtime,
                    StorageSafety::Protected,
                    "用于解释未被缓存、日志、登录态和运行组件子项单列的占用；应用包不会整体清理",
                );
            }
        }

        add_codex_temp_inventory(&mut report, &local.join("Temp"));
        add_codex_crash_dumps(&mut report, &local.join("CrashDumps"));
    }

    add_user_runtime_cache(&mut report, codex_home);

    add_unclassified_top_level(&mut report, codex_home);
    report
}

fn finalize_storage_report(report: &mut StorageReport) {
    normalize_nested_item_sizes(report);
    report.items.sort_by(|left, right| {
        right
            .size
            .cmp(&left.size)
            .then_with(|| left.label.cmp(&right.label))
    });
    for (index, item) in report.items.iter_mut().enumerate() {
        item.id = (index + 1) as u64;
    }
}

fn add_installed_codex_app_packages(report: &mut StorageReport) {
    #[cfg(not(windows))]
    let _ = report;

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        let output = Command::new("powershell.exe")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Get-AppxPackage -Name 'OpenAI.Codex' | ForEach-Object { $_.InstallLocation }",
            ])
            .creation_flags(0x0800_0000)
            .output();
        let Ok(output) = output else {
            return;
        };
        if !output.status.success() {
            return;
        }
        let mut locations = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(PathBuf::from)
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        locations.sort();
        locations.dedup();
        for location in locations {
            let stats = path_stats(&location);
            report.roots.push(location.clone());
            report.total_bytes = report.total_bytes.saturating_add(stats.bytes);
            let version = location
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| "OpenAI.Codex".to_string());
            add_known(
                report,
                location,
                &format!("当前 Windows Codex 应用包 · {version}"),
                StorageCategory::Runtime,
                StorageSafety::Protected,
                "这是 Windows 当前注册的 Codex 应用安装包；安装、更新和旧版本回收必须交给 Windows 应用部署服务，不能直接删除 WindowsApps 文件",
            );
        }
    }
}

fn add_user_runtime_cache(report: &mut StorageReport, codex_home: &Path) {
    let is_default_home = codex_home
        .file_name()
        .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case(".codex"));
    if !is_default_home {
        return;
    }
    let Some(user_root) = codex_home.parent() else {
        return;
    };
    let runtime_root = user_root.join(".cache").join("codex-runtimes");
    if !runtime_root.is_dir() {
        return;
    }

    report.roots.push(runtime_root.clone());
    report.total_bytes = report
        .total_bytes
        .saturating_add(path_stats(&runtime_root).bytes);
    add_known(
        report,
        runtime_root.clone(),
        "Codex 下载运行时的其他数据",
        StorageCategory::Runtime,
        StorageSafety::Protected,
        "该目录由桌面端维护；已识别的当前版本、旧版本和安装临时项会在下面单列，父目录不会整体清理",
    );

    let Ok(entries) = fs::read_dir(&runtime_root) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().to_string();
        let lower = name.to_ascii_lowercase();
        let (label, category, safety, reason) = if lower == "codex-primary-runtime" {
            (
                format!("当前下载运行时 · {name}"),
                StorageCategory::Runtime,
                StorageSafety::Protected,
                "这是桌面端当前使用的插件、文档和处理运行时，删除会导致功能失效或重新下载",
            )
        } else if lower.starts_with("codex-primary-runtime.previous-") {
            (
                format!("旧版下载运行时（需确认）· {name}"),
                StorageCategory::Runtime,
                StorageSafety::Review,
                "名称明确标记为 previous，通常是更新回滚副本；仅在 Codex 退出且当前版本工作正常时人工清理",
            )
        } else if lower.starts_with("codex-runtime-install-") {
            (
                format!("运行时安装临时目录 · {name}"),
                StorageCategory::Temporary,
                StorageSafety::SafeAfterExit,
                "运行时安装阶段的临时目录；Codex 完全退出且至少 7 天未更新后可重建或重新下载",
            )
        } else {
            (
                format!("未识别的下载运行时 · {name}"),
                StorageCategory::Runtime,
                StorageSafety::Protected,
                "位于 Codex 专用运行时目录但用途未确认，默认保护",
            )
        };
        add_known(report, entry.path(), &label, category, safety, reason);
    }
}

fn add_codex_temp_inventory(report: &mut StorageReport, temp_root: &Path) {
    let Ok(entries) = fs::read_dir(temp_root) else {
        return;
    };
    let candidates = entries
        .filter_map(Result::ok)
        .filter(|entry| is_codex_temp_name(&entry.file_name().to_string_lossy()))
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return;
    }

    report.roots.push(temp_root.to_path_buf());
    let bytes = candidates
        .iter()
        .map(|entry| path_stats(&entry.path()).bytes)
        .fold(0_u64, u64::saturating_add);
    report.total_bytes = report.total_bytes.saturating_add(bytes);

    for entry in candidates {
        let name = entry.file_name().to_string_lossy().to_string();
        let lower = name.to_ascii_lowercase();
        let (label, category, safety, reason) = if lower.starts_with("codex-index-") {
            (
                format!("Codex 索引临时项 · {name}"),
                StorageCategory::Temporary,
                StorageSafety::SafeAfterExit,
                "索引构建遗留的临时目录或占位文件；Codex 完全退出且至少 7 天未更新后可重建",
            )
        } else if lower == "openai-docs-cache" {
            (
                "OpenAI 文档缓存".to_string(),
                StorageCategory::Cache,
                StorageSafety::SafeAfterExit,
                "官方文档检索缓存；Codex 完全退出且至少 7 天未更新后可重新获取",
            )
        } else if lower.starts_with("codex-clipboard-") {
            (
                format!("任务剪贴板临时附件 · {name}"),
                StorageCategory::UserAsset,
                StorageSafety::Review,
                "可能仍是任务图片或附件的唯一原始路径，不能仅因位于 Temp 自动删除",
            )
        } else {
            (
                format!("Codex 相关系统临时项（需确认）· {name}"),
                StorageCategory::Temporary,
                StorageSafety::Review,
                "名称表明它由 Codex 或相关操作产生，但可能包含输入、预览或过程成果，只能人工核对",
            )
        };
        add_known(report, entry.path(), &label, category, safety, reason);
    }
}

fn is_codex_temp_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower.starts_with("codex-cleaner") || lower.starts_with("codexcleaner") {
        return false;
    }
    lower.starts_with("codex-")
        || lower.starts_with("codex_")
        || lower.starts_with("openai-")
        || lower.starts_with("chatgpt-")
        || lower == "openai-docs-cache"
}

fn add_codex_crash_dumps(report: &mut StorageReport, crash_root: &Path) {
    let Ok(entries) = fs::read_dir(crash_root) else {
        return;
    };
    let candidates = entries
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            entry.path().is_file()
                && !name.starts_with("codexcleaner")
                && (name.starts_with("codex.exe.")
                    || name.starts_with("chatgpt.exe.")
                    || name.starts_with("openai"))
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return;
    }

    report.roots.push(crash_root.to_path_buf());
    let bytes = candidates
        .iter()
        .map(|entry| path_stats(&entry.path()).bytes)
        .fold(0_u64, u64::saturating_add);
    report.total_bytes = report.total_bytes.saturating_add(bytes);
    for entry in candidates {
        let name = entry.file_name().to_string_lossy().to_string();
        add_known(
            report,
            entry.path(),
            &format!("Codex 崩溃转储（需确认）· {name}"),
            StorageCategory::Diagnostic,
            StorageSafety::Review,
            "仅用于崩溃排查；确认不再需要提交故障信息后可人工清理",
        );
    }
}

fn normalize_nested_item_sizes(report: &mut StorageReport) {
    let snapshot = report
        .items
        .iter()
        .map(|item| (item.path.clone(), item.size, item.file_count))
        .collect::<Vec<_>>();
    for item in &mut report.items {
        let descendants = snapshot
            .iter()
            .filter(|(path, _, _)| path != &item.path && path.starts_with(&item.path))
            .collect::<Vec<_>>();
        let top_level_descendants = descendants
            .iter()
            .filter(|(candidate, _, _)| {
                !descendants.iter().any(|(other, _, _)| {
                    other != candidate
                        && candidate.starts_with(other)
                        && candidate.as_path() != other.as_path()
                })
            })
            .map(|(_, size, file_count)| (*size, *file_count))
            .fold((0_u64, 0_u64), |(bytes, files), (size, file_count)| {
                (bytes.saturating_add(size), files.saturating_add(file_count))
            });
        if top_level_descendants.0 == 0 {
            continue;
        }
        item.size = item.size.saturating_sub(top_level_descendants.0);
        item.file_count = item.file_count.saturating_sub(top_level_descendants.1);
        if item.safety != StorageSafety::Protected {
            item.safety = StorageSafety::Protected;
            item.reason
                .push_str("；该父目录含已单列子项，为避免连带删除，父目录本身不会进入清理计划");
        }
    }
    let classified_bytes = report
        .items
        .iter()
        .map(|item| item.size)
        .fold(0_u64, u64::saturating_add);
    if classified_bytes < report.total_bytes {
        report.warnings.push(format!(
            "仍有 {} 未能稳定归类，界面不会自动清理这些数据",
            crate::format_bytes(report.total_bytes - classified_bytes)
        ));
    } else if classified_bytes > report.total_bytes {
        report.warnings.push(format!(
            "扫描期间文件发生变化，分类合计比根目录快照多 {}",
            crate::format_bytes(classified_bytes - report.total_bytes)
        ));
    }
}

fn add_standalone_release_inventory(report: &mut StorageReport, standalone_root: &Path) {
    let current = standalone_root.join("current");
    let active_release = fs::canonicalize(&current).ok();
    add_standalone_release_inventory_with_active(
        report,
        standalone_root,
        active_release.as_deref(),
    );
}

fn add_standalone_release_inventory_with_active(
    report: &mut StorageReport,
    standalone_root: &Path,
    active_release: Option<&Path>,
) {
    let current = standalone_root.join("current");
    add_known_pointer(
        report,
        current,
        "Codex CLI 当前版本指针",
        StorageCategory::Runtime,
        StorageSafety::Protected,
        "current 是 Codex 维护的版本指针，不能清理；目标发布包已单独统计，指针本身不重复计算占用",
    );
    add_known(
        report,
        standalone_root.join("install.lock"),
        "Codex CLI 安装锁",
        StorageCategory::State,
        StorageSafety::Protected,
        "Codex 更新器用它协调安装，即使当前为空也不应删除",
    );

    let releases = standalone_root.join("releases");
    let Ok(entries) = fs::read_dir(&releases) else {
        return;
    };
    for entry in entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
    {
        let path = entry.path();
        let resolved = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        let is_active = active_release.is_some_and(|active| resolved == active);
        let can_prove_old = active_release.is_some() && !is_active;
        let version = entry.file_name().to_string_lossy().to_string();
        let label = if is_active {
            format!("当前 Codex CLI 发布包 · {version}")
        } else if can_prove_old {
            format!("旧版 Codex CLI 发布包 · {version}")
        } else {
            format!("Codex CLI 发布包（当前版本未确认）· {version}")
        };
        add_known(
            report,
            path,
            &label,
            StorageCategory::Runtime,
            if can_prove_old {
                StorageSafety::Review
            } else {
                StorageSafety::Protected
            },
            if is_active {
                "current 指针正在引用该版本，删除会破坏 Codex CLI"
            } else if can_prove_old {
                "current 指针已明确指向其他发布包；该目录可能用于回滚，仅在 Codex 完全退出且确认新版正常后人工复核"
            } else {
                "无法解析 current 版本指针，不能证明这是旧版，因此保护"
            },
        );
    }
}

fn add_content_addressed_runtime_directories(
    report: &mut StorageReport,
    bin_root: &Path,
    label: &str,
) {
    let Ok(entries) = fs::read_dir(bin_root) else {
        return;
    };
    for entry in entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
    {
        let name = entry.file_name().to_string_lossy().to_string();
        if !is_content_address_key(&name) {
            continue;
        }
        add_known(
            report,
            entry.path(),
            &format!("版本化 {label}（需确认）· {name}"),
            StorageCategory::Runtime,
            StorageSafety::Review,
            "内容地址目录可能是已下载的新组件、更新中间件或当前组件来源；不会一键选中，只能在 Codex 退出且确认更新完成后人工复核",
        );
    }
}

fn add_unmanaged_runtime_component_files(report: &mut StorageReport, bin_root: &Path) {
    let Ok(entries) = fs::read_dir(bin_root) else {
        return;
    };
    for entry in entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_file())
    {
        let name = entry.file_name().to_string_lossy().to_string();
        let extension = entry
            .path()
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !matches!(extension.as_str(), "exe" | "dll") {
            continue;
        }
        add_known(
            report,
            entry.path(),
            &format!("本机运行组件副本（需确认）· {name}"),
            StorageCategory::Runtime,
            StorageSafety::Review,
            "该副本可能来自旧版桌面端，也可能仍被 CLI、Chrome 集成或更新器引用；扫描器不会仅按日期或同名判定可删，只供退出 Codex 后人工复核",
        );
    }
}

fn is_content_address_key(value: &str) -> bool {
    (12..=64).contains(&value.len()) && value.chars().all(|character| character.is_ascii_hexdigit())
}

fn add_children(
    report: &mut StorageReport,
    root: &Path,
    category: StorageCategory,
    safety: StorageSafety,
    reason: &str,
) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().to_string();
        add_known(
            report,
            entry.path(),
            &format!("{} · {name}", category.label()),
            category,
            safety,
            reason,
        );
    }
}

fn add_granular_children(
    report: &mut StorageReport,
    root: &Path,
    category: StorageCategory,
    safety: StorageSafety,
    reason: &str,
    split_threshold: u64,
) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let stats = path_stats(&path);
        add_known(
            report,
            path.clone(),
            &format!(
                "{} · {}",
                category.label(),
                entry.file_name().to_string_lossy()
            ),
            category,
            safety,
            reason,
        );
        if !path.is_dir() || stats.bytes < split_threshold {
            continue;
        }
        add_children(report, &path, category, safety, reason);
    }
}

fn add_uuid_asset_directories(report: &mut StorageReport, root: &Path, label: &str) {
    if !root.is_dir() {
        return;
    }
    let mut found = false;
    for entry in WalkDir::new(root)
        .follow_links(false)
        .min_depth(1)
        .max_depth(5)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_dir())
    {
        let name = entry.file_name().to_string_lossy();
        if !looks_like_uuid(&name) {
            continue;
        }
        found = true;
        let task_path = entry.path().to_path_buf();
        add_known(
            report,
            task_path.clone(),
            &format!("{label} · {name}"),
            StorageCategory::UserAsset,
            StorageSafety::Review,
            "目录名是任务 UUID；需在任务分析中区分最终成果、过程成果、源码和依赖后再处理",
        );
        add_task_asset_children(report, &task_path);
    }
    if !found {
        add_children(
            report,
            root,
            StorageCategory::UserAsset,
            StorageSafety::Review,
            "用户生成资产，需按任务和成果阶段人工判断",
        );
    }
}

fn add_task_asset_children(report: &mut StorageReport, task_root: &Path) {
    let Ok(entries) = fs::read_dir(task_root) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let (category, safety, prefix, reason) = classify_task_asset_path(&path);
        add_known(
            report,
            path,
            &format!("{prefix} · {}", entry.file_name().to_string_lossy()),
            category,
            safety,
            reason,
        );
    }
}

fn classify_task_asset_path(
    path: &Path,
) -> (StorageCategory, StorageSafety, &'static str, &'static str) {
    let components = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_ascii_lowercase())
        .collect::<Vec<_>>();
    let name = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let tokens = name
        .split(|character: char| !character.is_alphanumeric())
        .collect::<Vec<_>>();
    let is_reference = path
        .file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.to_ascii_lowercase().starts_with("reference-"));
    if is_reference
        || components.iter().any(|value| {
            matches!(
                value.as_str(),
                "node_modules" | ".venv" | "venv" | "vendor" | "target" | "classes" | "__pycache__"
            )
        })
    {
        return (
            StorageCategory::SupportLibrary,
            StorageSafety::Review,
            "支持库",
            "任务目录中的参考工程、依赖或编译支持文件；确认不再需要复现环境后再清理",
        );
    }
    let source_extension = matches!(
        extension.as_str(),
        "rs" | "py"
            | "js"
            | "ts"
            | "tsx"
            | "jsx"
            | "java"
            | "kt"
            | "c"
            | "cc"
            | "cpp"
            | "h"
            | "hpp"
            | "cs"
            | "go"
            | "html"
            | "css"
            | "json"
            | "toml"
            | "yaml"
            | "yml"
            | "md"
    );
    let source_directory = path.is_dir()
        && (path.join("src").is_dir()
            || path.join("Cargo.toml").is_file()
            || path.join("package.json").is_file()
            || path.join("pom.xml").is_file()
            || path.join("build.gradle").is_file());
    if source_extension || source_directory {
        return (
            StorageCategory::Source,
            StorageSafety::Protected,
            "源码",
            "任务目录中的项目源码或生成脚本；全局清理不会自动处理",
        );
    }
    let process_marker = tokens.iter().any(|token| {
        matches!(
            *token,
            "draft"
                | "preview"
                | "render"
                | "smoke"
                | "test"
                | "temp"
                | "tmp"
                | "audit"
                | "qa"
                | "check"
                | "verify"
                | "verification"
                | "before"
                | "backup"
        )
    });
    let final_marker = tokens.iter().any(|token| {
        matches!(
            *token,
            "final" | "deliverable" | "release" | "最终" | "定稿" | "交付" | "成品"
        )
    });
    let artifact_extension = matches!(
        extension.as_str(),
        "docx"
            | "xlsx"
            | "xls"
            | "pdf"
            | "pptx"
            | "csv"
            | "zip"
            | "7z"
            | "jar"
            | "png"
            | "jpg"
            | "jpeg"
            | "svg"
            | "mp4"
            | "webm"
            | "mp3"
            | "wav"
    );
    if artifact_extension && final_marker && !process_marker {
        return (
            StorageCategory::FinalArtifact,
            StorageSafety::Protected,
            "最终成果",
            "文件名具有明确的最终、定稿或交付标记；全局清理不会自动处理",
        );
    }
    (
        StorageCategory::IntermediateArtifact,
        StorageSafety::Review,
        "过程成果",
        if process_marker {
            "文件名具有预览、测试、渲染或质量核对特征；可结合对应任务复核"
        } else {
            "任务资产没有充分的最终交付证据，按过程成果保守展示并等待复核"
        },
    )
}

fn looks_like_uuid(value: &str) -> bool {
    value.len() == 36
        && value
            .chars()
            .enumerate()
            .all(|(index, character)| match index {
                8 | 13 | 18 | 23 => character == '-',
                _ => character.is_ascii_hexdigit(),
            })
}

fn add_known(
    report: &mut StorageReport,
    path: PathBuf,
    label: &str,
    category: StorageCategory,
    safety: StorageSafety,
    reason: &str,
) {
    if !path.exists() || report.items.iter().any(|item| item.path == path) {
        return;
    }
    let stats = path_stats(&path);
    let newest_at = stats.newest.map(DateTime::<Utc>::from);
    let stale_days = newest_at.map(|value| (Utc::now() - value).num_days().max(0));
    report.items.push(StorageItem {
        id: 0,
        label: label.to_string(),
        path,
        category,
        safety,
        size: stats.bytes,
        file_count: stats.files,
        newest_at,
        stale_days,
        reason: reason.to_string(),
        action: StorageAction::Keep,
    });
}

fn add_known_pointer(
    report: &mut StorageReport,
    path: PathBuf,
    label: &str,
    category: StorageCategory,
    safety: StorageSafety,
    reason: &str,
) {
    if !path.exists() || report.items.iter().any(|item| item.path == path) {
        return;
    }
    let newest_at = fs::symlink_metadata(&path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .map(DateTime::<Utc>::from);
    let stale_days = newest_at.map(|value| (Utc::now() - value).num_days().max(0));
    report.items.push(StorageItem {
        id: 0,
        label: label.to_string(),
        path,
        category,
        safety,
        size: 0,
        file_count: 0,
        newest_at,
        stale_days,
        reason: reason.to_string(),
        action: StorageAction::Keep,
    });
}

fn add_dated_log_folders(report: &mut StorageReport, root: &Path, label: &str) {
    if !root.is_dir() {
        return;
    }
    for entry in WalkDir::new(root)
        .follow_links(false)
        .min_depth(3)
        .max_depth(3)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_dir())
    {
        let relative = entry
            .path()
            .strip_prefix(root)
            .unwrap_or(entry.path())
            .display()
            .to_string();
        let safety = diagnostic_safety(entry.path());
        add_known(
            report,
            entry.path().to_path_buf(),
            &format!("{label} · {relative}"),
            StorageCategory::Diagnostic,
            safety,
            if safety == StorageSafety::SafeAfterExit {
                "仅用于历史故障排查，已超过 7 天；退出 Codex 后可进入回收站"
            } else {
                "近期日志可能仍用于故障排查；建议保留 7 天后再清理"
            },
        );
    }
}

fn add_diagnostic_children(report: &mut StorageReport, root: &Path, label: &str) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let is_log = path.is_dir()
            || path
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| {
                    matches!(value.to_ascii_lowercase().as_str(), "log" | "trace" | "etl")
                });
        if !is_log {
            continue;
        }
        let safety = diagnostic_safety(&path);
        add_known(
            report,
            path,
            &format!("{label} · {}", entry.file_name().to_string_lossy()),
            StorageCategory::Diagnostic,
            safety,
            if safety == StorageSafety::SafeAfterExit {
                "历史诊断数据已超过 7 天；退出 Codex 后可清理"
            } else {
                "近期诊断数据建议暂时保留，避免影响故障排查"
            },
        );
    }
}

fn diagnostic_safety(path: &Path) -> StorageSafety {
    let newest = path_stats(path).newest.map(DateTime::<Utc>::from);
    let stale_days = newest.map(|value| (Utc::now() - value).num_days().max(0));
    if stale_days.is_some_and(|days| days >= 7) {
        StorageSafety::SafeAfterExit
    } else {
        StorageSafety::Review
    }
}

fn protect_database_companions(report: &mut StorageReport, codex_home: &Path) {
    let Ok(entries) = fs::read_dir(codex_home) else {
        return;
    };
    for entry in entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_file())
    {
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if !name.ends_with(".sqlite-wal") && !name.ends_with(".sqlite-shm") {
            continue;
        }
        add_known(
            report,
            entry.path(),
            &entry.file_name().to_string_lossy(),
            StorageCategory::State,
            StorageSafety::Protected,
            "这是当前 SQLite 数据库的活动日志或共享内存文件，不能单独删除",
        );
    }
}

fn add_top_level_backup_files(report: &mut StorageReport, codex_home: &Path) {
    let Ok(entries) = fs::read_dir(codex_home) else {
        return;
    };
    for entry in entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_file())
    {
        let name = entry.file_name().to_string_lossy().to_string();
        let lower = name.to_ascii_lowercase();
        if !lower.contains(".bak") {
            continue;
        }
        let authentication = lower.starts_with("auth.") || lower.contains("codex_auth");
        add_known(
            report,
            entry.path(),
            &format!("备份文件 · {name}"),
            if authentication {
                StorageCategory::State
            } else {
                StorageCategory::Backup
            },
            if authentication {
                StorageSafety::Protected
            } else {
                StorageSafety::Review
            },
            if authentication {
                "可能包含身份凭据，不应由清理工具处理"
            } else {
                "旧配置或状态备份；确认当前版本正常后可人工清理"
            },
        );
    }
}

fn add_unclassified_top_level(report: &mut StorageReport, codex_home: &Path) {
    let Ok(entries) = fs::read_dir(codex_home) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let covered_exactly = report.items.iter().any(|item| item.path == path);
        if covered_exactly {
            continue;
        }
        let has_classified_children = report
            .items
            .iter()
            .any(|item| item.path != path && item.path.starts_with(&path));
        let name = entry.file_name().to_string_lossy().to_string();
        let label = if has_classified_children {
            format!("未单列的其他内容 · {name}")
        } else {
            format!("待识别 · {name}")
        };
        add_known(
            report,
            path,
            &label,
            StorageCategory::State,
            if has_classified_children {
                StorageSafety::Protected
            } else {
                StorageSafety::Review
            },
            if has_classified_children {
                "该目录已有子项单独分类；本行仅解释剩余占用，父目录不会整体清理"
            } else {
                "扫描器尚未建立该项目的稳定安全规则，默认不自动清理"
            },
        );
    }
}

fn path_stats(path: &Path) -> PathStats {
    if path.is_file() {
        return fs::metadata(path)
            .map(|metadata| PathStats {
                bytes: metadata.len(),
                files: 1,
                newest: metadata.modified().ok(),
            })
            .unwrap_or_default();
    }
    let mut stats = PathStats {
        newest: fs::symlink_metadata(path)
            .ok()
            .and_then(|metadata| metadata.modified().ok()),
        ..PathStats::default()
    };
    for entry in WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        stats.bytes = stats.bytes.saturating_add(metadata.len());
        stats.files = stats.files.saturating_add(1);
        if let Ok(modified) = metadata.modified() {
            stats.newest = Some(stats.newest.map_or(modified, |value| value.max(modified)));
        }
    }
    stats
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn classifies_history_cache_and_results_separately() {
        let root = tempdir().unwrap();
        fs::create_dir_all(root.path().join("sessions")).unwrap();
        fs::create_dir_all(root.path().join(".cache/runtime")).unwrap();
        fs::create_dir_all(root.path().join("visualizations/task")).unwrap();
        fs::write(root.path().join("sessions/rollout.jsonl"), b"history").unwrap();
        fs::write(root.path().join(".cache/runtime/cache.bin"), b"cache").unwrap();
        fs::write(root.path().join("visualizations/task/final.png"), b"result").unwrap();

        let report = scan_codex_storage_at(root.path(), None);
        let history = report
            .items
            .iter()
            .find(|item| item.label == "活跃对话记录")
            .unwrap();
        let cache = report
            .items
            .iter()
            .find(|item| item.path.ends_with(".cache/runtime"))
            .unwrap();
        let result = report
            .items
            .iter()
            .find(|item| item.path.ends_with("visualizations/task"))
            .unwrap();
        assert_eq!(history.safety, StorageSafety::Protected);
        assert_eq!(cache.safety, StorageSafety::SafeAfterExit);
        assert_eq!(cache.action, StorageAction::Keep);
        assert_eq!(result.category, StorageCategory::UserAsset);
        assert_eq!(result.safety, StorageSafety::Review);
        assert_eq!(
            report.items.iter().map(|item| item.size).sum::<u64>(),
            report.total_bytes
        );
    }

    #[test]
    fn scan_is_read_only_and_safe_rules_exclude_diagnostic_logs() {
        let item = |id, category| StorageItem {
            id,
            label: format!("item-{id}"),
            path: PathBuf::from(format!("C:/item-{id}")),
            category,
            safety: StorageSafety::SafeAfterExit,
            size: 100,
            file_count: 1,
            newest_at: None,
            stale_days: Some(8),
            reason: "test".to_string(),
            action: StorageAction::Keep,
        };
        let mut report = StorageReport {
            roots: vec![],
            items: vec![
                item(1, StorageCategory::Cache),
                item(2, StorageCategory::Temporary),
                item(3, StorageCategory::Diagnostic),
                item(4, StorageCategory::Backup),
            ],
            total_bytes: 400,
            warnings: vec![],
        };

        apply_safe_storage_rules(&mut report);

        assert_eq!(report.items[0].action, StorageAction::Clean);
        assert_eq!(report.items[1].action, StorageAction::Clean);
        assert_eq!(report.items[2].action, StorageAction::Keep);
        assert_eq!(report.items[3].action, StorageAction::Keep);
        assert_eq!(report.safe_candidate_bytes(), 200);
    }

    #[test]
    fn splits_task_assets_into_final_process_and_support_groups() {
        assert_eq!(
            classify_task_asset_path(Path::new("C:/task/release_final.pdf")).0,
            StorageCategory::FinalArtifact
        );
        assert_eq!(
            classify_task_asset_path(Path::new("C:/task/qa_final.pdf")).0,
            StorageCategory::IntermediateArtifact
        );
        assert_eq!(
            classify_task_asset_path(Path::new("C:/task/reference-project")).0,
            StorageCategory::SupportLibrary
        );
    }

    #[test]
    fn separates_active_and_old_standalone_cli_releases() {
        let root = tempdir().unwrap();
        let standalone = root.path().join("packages/standalone");
        let active = standalone.join("releases/0.146.0-x86_64-pc-windows-msvc");
        let old = standalone.join("releases/0.145.0-x86_64-pc-windows-msvc");
        fs::create_dir_all(&active).unwrap();
        fs::create_dir_all(&old).unwrap();
        fs::write(active.join("codex.exe"), b"current").unwrap();
        fs::write(old.join("codex.exe"), b"old").unwrap();
        let active = fs::canonicalize(&active).unwrap();
        let mut report = StorageReport {
            roots: vec![root.path().to_path_buf()],
            items: Vec::new(),
            total_bytes: 10,
            warnings: Vec::new(),
        };

        add_standalone_release_inventory_with_active(&mut report, &standalone, Some(&active));

        let current_item = report
            .items
            .iter()
            .find(|item| item.label.contains("当前 Codex CLI 发布包"))
            .unwrap();
        let old_item = report
            .items
            .iter()
            .find(|item| item.label.contains("旧版 Codex CLI 发布包"))
            .unwrap();
        assert_eq!(current_item.safety, StorageSafety::Protected);
        assert_eq!(old_item.safety, StorageSafety::Review);
        assert_eq!(old_item.category, StorageCategory::Runtime);
    }

    #[test]
    fn current_release_pointer_does_not_duplicate_target_bytes() {
        let root = tempdir().unwrap();
        let standalone = root.path().join("packages/standalone");
        let current = standalone.join("current");
        fs::create_dir_all(&current).unwrap();
        fs::write(current.join("codex.exe"), vec![0_u8; 4096]).unwrap();
        let mut report = StorageReport {
            roots: vec![root.path().to_path_buf()],
            items: Vec::new(),
            total_bytes: 4096,
            warnings: Vec::new(),
        };

        add_known_pointer(
            &mut report,
            current,
            "Codex CLI 当前版本指针",
            StorageCategory::Runtime,
            StorageSafety::Protected,
            "pointer",
        );

        assert_eq!(report.items.len(), 1);
        assert_eq!(report.items[0].size, 0);
        assert_eq!(report.items[0].file_count, 0);
        assert_eq!(report.items[0].safety, StorageSafety::Protected);
    }

    #[test]
    fn runtime_update_copies_are_review_only_and_never_safe_rule_candidates() {
        let root = tempdir().unwrap();
        let bin = root.path().join("OpenAI/Codex/bin");
        let addressed = bin.join("cfac6bda2d141e07");
        fs::create_dir_all(&addressed).unwrap();
        fs::write(addressed.join("codex.exe"), b"staged").unwrap();
        fs::write(bin.join("codex.exe"), b"legacy-or-active").unwrap();
        let mut report = StorageReport {
            roots: vec![root.path().to_path_buf()],
            items: Vec::new(),
            total_bytes: 22,
            warnings: Vec::new(),
        };

        add_content_addressed_runtime_directories(&mut report, &bin, "Codex 本机运行组件");
        add_unmanaged_runtime_component_files(&mut report, &bin);
        apply_safe_storage_rules(&mut report);

        assert_eq!(report.items.len(), 2);
        assert!(report.items.iter().all(|item| {
            item.category == StorageCategory::Runtime
                && item.safety == StorageSafety::Review
                && item.action == StorageAction::Review
        }));
    }

    #[test]
    fn scans_downloaded_runtimes_and_attributable_windows_temp_without_overclaiming() {
        let profile = tempdir().unwrap();
        let codex_home = profile.path().join(".codex");
        let runtime_root = profile.path().join(".cache/codex-runtimes");
        let local = profile.path().join("AppData/Local");
        let temp = local.join("Temp");
        let crash = local.join("CrashDumps");
        fs::create_dir_all(&codex_home).unwrap();
        fs::create_dir_all(runtime_root.join("codex-primary-runtime")).unwrap();
        fs::create_dir_all(runtime_root.join("codex-primary-runtime.previous-old")).unwrap();
        fs::create_dir_all(runtime_root.join("codex-runtime-install-stage")).unwrap();
        fs::create_dir_all(&temp).unwrap();
        fs::create_dir_all(&crash).unwrap();
        fs::write(
            runtime_root.join("codex-primary-runtime/current.bin"),
            vec![0_u8; 1],
        )
        .unwrap();
        fs::write(
            runtime_root.join("codex-primary-runtime.previous-old/old.bin"),
            vec![0_u8; 2],
        )
        .unwrap();
        fs::write(
            runtime_root.join("codex-runtime-install-stage/staged.bin"),
            vec![0_u8; 3],
        )
        .unwrap();
        fs::write(temp.join("codex-index-test"), vec![0_u8; 4]).unwrap();
        fs::write(temp.join("codex-clipboard-test.png"), vec![0_u8; 5]).unwrap();
        fs::write(temp.join("codex-cleaner-test.json"), vec![0_u8; 7]).unwrap();
        fs::write(crash.join("Codex.exe.100.dmp"), vec![0_u8; 6]).unwrap();
        fs::write(crash.join("CodexCleaner.exe.100.dmp"), vec![0_u8; 8]).unwrap();

        let report = scan_codex_storage_at(&codex_home, Some(&local));

        let find = |suffix: &str| {
            report
                .items
                .iter()
                .find(|item| item.path.ends_with(suffix))
                .unwrap()
        };
        assert_eq!(
            find("codex-primary-runtime").safety,
            StorageSafety::Protected
        );
        assert_eq!(
            find("codex-primary-runtime.previous-old").safety,
            StorageSafety::Review
        );
        assert_eq!(
            find("codex-runtime-install-stage").safety,
            StorageSafety::SafeAfterExit
        );
        assert_eq!(
            find("codex-index-test").safety,
            StorageSafety::SafeAfterExit
        );
        assert_eq!(
            find("codex-clipboard-test.png").category,
            StorageCategory::UserAsset
        );
        assert_eq!(find("Codex.exe.100.dmp").safety, StorageSafety::Review);
        assert!(!report
            .items
            .iter()
            .any(|item| item.path.ends_with("codex-cleaner-test.json")
                || item.path.ends_with("CodexCleaner.exe.100.dmp")));
        assert_eq!(report.total_bytes, 21);
        assert_eq!(
            report.items.iter().map(|item| item.size).sum::<u64>(),
            report.total_bytes
        );
    }

    #[test]
    fn dry_run_preview_is_json_and_contains_only_safe_stale_candidates() {
        let item = |id, category, safety, stale_days| StorageItem {
            id,
            label: format!("item-{id}"),
            path: PathBuf::from(format!("C:/item-{id}")),
            category,
            safety,
            size: 100,
            file_count: 1,
            newest_at: None,
            stale_days,
            reason: "test".to_string(),
            action: StorageAction::Keep,
        };
        let report = StorageReport {
            roots: vec![PathBuf::from("C:/codex")],
            items: vec![
                item(
                    1,
                    StorageCategory::Cache,
                    StorageSafety::SafeAfterExit,
                    Some(8),
                ),
                item(2, StorageCategory::Runtime, StorageSafety::Review, Some(90)),
                item(
                    3,
                    StorageCategory::Temporary,
                    StorageSafety::SafeAfterExit,
                    Some(2),
                ),
            ],
            total_bytes: 300,
            warnings: vec!["inventory warning".to_string()],
        };

        let preview = build_safe_storage_cleanup_preview(&report, true);
        let json = serde_json::to_value(&preview).unwrap();

        assert_eq!(preview.candidate_count, 1);
        assert_eq!(preview.candidate_bytes, 100);
        assert_eq!(preview.candidates[0].id, 1);
        assert_eq!(preview.manual_review_count, 1);
        assert_eq!(json["dry_run"], true);
        assert_eq!(json["schema_version"], 1);
        assert!(preview
            .warnings
            .iter()
            .any(|warning| warning.contains("Codex 正在运行")));
    }
}
