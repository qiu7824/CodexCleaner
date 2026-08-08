# Codex Cleaner

Codex Cleaner 是面向普通 Windows 用户的 Codex 本地数据清理工具，使用 Rust 和 ZSUI 开发。首页只保留“清理一个任务”和“释放磁盘空间”两个主要入口，任务清理和空间清理都按选择、核对、执行的顺序完成。

## 能做什么

### 首页

- 从“清理一个任务”或“释放磁盘空间”直接进入对应流程，不需要在多个区域寻找入口。
- 全面扫描任务索引、任务树、Codex 存储分类、系统盘容量和执行记录。
- 将 Codex 数据、安全候选和需要核对分开显示；扫描本身不会选择或删除内容。
- 只建议分析已归档、长期未活动、记录较大或同名的主任务；分析前不声称可清理。

### 任务清理

- 以 `sessions`、`archived_sessions` 中的 canonical transcript 为本地任务清单，并与 Codex 官方目录合并；无 transcript 的旧索引残留只告警，不伪装成可清理任务。
- 同时区分活跃、已归档、主任务、子任务、同名任务和官方目录中尚无本地文件的任务。
- 优先通过 Codex 官方 App Server 补全任务名称和状态；官方接口不可用时，从本地记录恢复可读名称。
- 递归分析所选主任务及全部子任务的 transcript，不以同名或同一工作目录代替父子关系。
- 识别任务记录、状态索引、专属临时文件、缓存、日志、源码、支持库、用户输入和生成文件；工具输出中实际观察到的现存路径也会列入弱证据资源，不会自动清理。
- 把成果分成“最终成果”“过程成果”和“证据不足”，同时显示归属、可信度、判断依据、软件建议和用户选择。
- 提供四种保留方案：仅保留成果、保留成果和源码、保留开发环境、仅删除对话。
- 选择保留方案后会自动打开“需要决定”列表并选中第一项；点击“保留这个项目”或“清理这个项目”后自动前进到下一项。
- 主页面保持单屏显示；完整路径和全部判断证据可通过“查看完整判断”弹窗查看。

### 空间清理

- 盘点 `%USERPROFILE%\.codex`、Codex 桌面运行目录和 Windows 应用包目录。
- 分开显示对话历史、迁移备份、缓存、临时文件、日志、浏览器状态、运行组件、插件、技能、附件与任务资产。
- 识别 `.codex\packages\standalone\current` 正在引用的 CLI 发布包、未被引用的旧发布包、内容地址更新目录和本机运行组件副本；旧版候选只进入人工复核，不按日期自动删除。
- 大型任务资产按任务 UUID 及目录内容继续细分为最终成果、过程成果、源码和支持库。
- “智能选择安全项”只选择 Codex 退出后可重建、且至少 7 天未更新的缓存或临时项。
- 备份、日志、浏览器状态和过程成果仅供人工复核；运行组件、数据库、登录状态、对话目录和最终成果受保护。

### 清理记录与设置

- 清理记录从实际 JSON 回执读取任务永久删除、进入回收站的路径和失败结果；不重放旧清理计划。
- 默认使用日间主题；设置页可切换深浅主题、重新扫描，并展示数据位置和不可关闭的安全边界。

## 使用流程

1. 关闭正在写入数据的 Codex 窗口；只扫描和分析时可以保持当前窗口打开。
2. 打开 `CodexCleaner.exe`。程序立即进入“首页”，任务和存储盘点在后台完成，状态只占一行。
3. 点击“清理一个任务”，在任务表中选择任务，再点击“分析所选任务”；双击任务也可以直接分析。
4. 选择保留方案，软件会直接列出需要决定的项目；逐项选择保留、清理或稍后决定。
5. 点击“核对永久删除”，确认永久删除的任务树、进入回收站的路径及不会删除的内容。

存储清理使用“空间清理”：依次选择清理项目、核对所选项目、核对并移入 Windows 回收站。移入回收站不会立即增加磁盘可用空间，需清空回收站后才会释放。

## 删除与恢复边界

- Codex 任务通过官方 `thread/delete` 删除。根任务、全部派生子任务及对应 transcript 会永久删除，不能从 Windows 回收站恢复。官方返回成功后，软件会再次枚举活跃和归档任务并检查 transcript 路径；只有两者都不存在才在界面移除任务。
- 被确认清理的任务专属文件、缓存、临时项和备份进入 Windows 回收站。
- 共享、全局、受保护或归属不明资源不会成为自动删除项。
- 用户可以显式选择归属不明的单个路径；软件仍会拒绝盘符根目录、符号链接、junction，以及会覆盖保留项的父目录。
- 全局存储清理在检测到 `codex.exe` 运行时拒绝执行。
- 每次执行都会在 `%LOCALAPPDATA%\CodexCleaner\journals` 写入 JSON 操作记录；导出的核对清单位于 `%LOCALAPPDATA%\CodexCleaner\previews`。

完整规则见 [安全模型](docs/SAFETY.md)。

## 构建

要求：Windows、Rust 1.85 或更高版本。

```powershell
cargo build --workspace
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
```

运行桌面界面：

```powershell
cargo run --bin codex-cleaner
```

命令行诊断：

```powershell
cargo run --bin codex-cleaner-cli -- --scan-json
cargo run --bin codex-cleaner-cli -- --storage-json
cargo run --bin codex-cleaner-cli -- --analyze <完整任务 UUID>
cargo run --bin codex-cleaner-cli -- --task-dry-run-json <完整任务 UUID>
cargo run --bin codex-cleaner-cli -- --storage-dry-run-json
cargo run --bin codex-cleaner-cli -- --probe-official
```

可用 `--codex-home <目录>` 指定 Codex 数据目录，用 `--codex-bin <文件>` 指定 Codex CLI。当前 CLI 面向盘点、分析、官方接口诊断和 dry-run；`--task-dry-run-json`、`--storage-dry-run-json` 均不执行删除，实际清理由 GUI 的分步确认流程完成。

发布构建：

```powershell
cargo build --release --workspace
```

- `target\release\codex-cleaner.exe`：Windows GUI 程序，不创建控制台窗口。
- `target\release\codex-cleaner-cli.exe`：控制台诊断程序。

## 项目结构

- `crates/cleaner-core`：任务扫描、资源分析、存储分类、安全计划和执行日志。
- `crates/app-zsui`：ZSUI 原生 Windows 界面、后台扫描状态和 CLI 入口。
- `vendor/zsui`：从本机最新 ZSUI 0.2.0-preview.6 源码快照引入的可移植 path 依赖，不使用 Git 依赖。
- `assets`：嵌入 EXE 并释放到本地应用目录的程序图标。
- `docs`：安全模型说明。
- `CHANGELOG.md`：版本变化与兼容性说明。

## 已知边界

- 外部程序若没有在结构化事件、命令参数或工具输出中报告路径，软件无法发现对应文件；只在工具输出出现的路径按弱证据展示并保持“需要决定”。
- 单次分析默认最多读取 1 GiB transcript，并分别处理最多 10,000 个结构化候选路径和 10,000 个工具输出观察路径；达到上限时会明确标记“分析不完整”，且未分析内容不会进入清理计划。
- 文件名中的 `final` 只是证据之一；位于 QA、测试、预览、审计或渲染路径时仍按过程成果处理。
- 同名任务不等于重复任务，同一工作目录也不等于可以一起删除。
- Codex 的本地格式和官方 App Server 接口可能随版本变化；无法确认的内容默认保留。

## 许可证

Mozilla Public License 2.0。
