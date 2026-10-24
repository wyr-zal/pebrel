@CONTEXT.md
# Pebrel 项目指引

- 产品名与命令用 `pebrel`；内部 Cargo 包名和源码目录保留 `nebula*`，不顺手重命名。
- 修改前读取目标目录最近的 `AGENTS.md`；只处理当前需求，保留其他工作区改动。
- 界面复用共享设置和生命周期；异步结果检查 pane、进程、会话与任务代次，不能让旧结果覆盖新状态。
- 保留 Hook 的 owner/PID/session 校验；屏幕文本不能冒充可靠的完成事件。
- 保持既有 CRLF。禁止 `cargo fmt --all`；需要格式化时只处理明确的单文件。
- 只做受影响模块的验证，编译/状态单测不等于实际 UI 验收；未验证的路径明确说明。
- 架构门禁用 `python3 scripts/check_architecture.py --base <共同基线>`；不抬预算、不删覆盖来通过检查。

## Windows Debug 构建与 WSL 标签修复验收

- 使用正式源码工作树 `F:/ProjectsWork/pebrel-personal-merge-20261010` 构建 Windows 产品；从 WSL 执行：
  `/mnt/c/Users/12704/AppData/Local/Microsoft/WindowsApps/pwsh.exe -NoProfile -File F:/ProjectsWork/pebrel-personal-merge-20261010/scripts/build-windows-product.ps1 -Configuration debug`
- 该脚本按 `nebula/gpui-shell` 构建 `pebrel.exe` 和 `pebrel-hook.exe`；`.cargo/config.toml` 将共享 target 固定到 `F:/ProjectsWork/pebrel-debug-daily-20261013/cargo-target`。不得中断正在此 target 中运行的 Cargo/linker。
- Debug 产物位于 `cargo-target/debug/`：`pebrel.exe`、`pebrel.pdb`、`pebrel-hook.exe`、`pebrel_hook.pdb`。部署时将前两项放目标根目录，后两项放 `runtime/`。
- WSL 标签修复的构建和交付目标是测试 bundle `F:/ProjectsWork/pebrel-debug-daily-20261013`：覆盖前将四个旧文件备份到 `F:/12704/temp-backups/yyyyMMdd/<task>/bundle/`，再复制构建产物并逐项比对 SHA256。`F:/Dev/Tools/Pebrel` 是用户的个人日用安装；除非用户本轮明确要求，否则不得备份、替换、重启或向它发送运行时命令。
- 隔离 UI 验收使用单独 EXE 副本、独立 `APPDATA`/`PEBREL_CONFIG_DIR`，清除继承的 `PEBREL_`/`NEBULA_`/`WSL_`、`WSLENV`、`TERM_PROGRAM`，并核对进程可执行路径。`F:/ProjectsWork` 的 Low Integrity 标签会令进程无法启动 WSL；测试 bundle 和候选副本中的 `pebrel.exe` 需单独设为 Medium Integrity，不改整个目录标签。验收用假 Agent 检查 guest cwd 标签变化、cwd 跟随、其他标签不变及命令退出后的宿主 cwd 恢复；真实 Claude/Codex 会话不用于测试。

## 按需查阅

- 应用与 UI：`nebula_app/AGENTS.md`；终端：`nebula_terminal/AGENTS.md`；设置：`nebula_settings/AGENTS.md`。
- 脚本：`scripts/AGENTS.md`；文档：`docs/AGENTS.md`。
- 模块职责、公共接口或依赖变更：查 `docs/architecture.md`、`docs/project-constraints.md` 和 `architecture/` 中对应规则。
- 新 UI 文案：查 `docs/internationalization.md`。
- 版本、构建、打包或发布：读 `packaging/AGENTS.md`、`docs/release-notes/AGENTS.md`。
- 仅在协议、所有权、持久化或生命周期有重要取舍时按 `architecture/notes/AGENTS.md` 记录原因；普通修复不另写说明。
