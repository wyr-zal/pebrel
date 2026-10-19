@CONTEXT.md
# Pebrel 项目指引

- 产品名与命令用 `pebrel`；内部 Cargo 包名和源码目录保留 `nebula*`，不顺手重命名。
- 修改前读取目标目录最近的 `AGENTS.md`；只处理当前需求，保留其他工作区改动。
- 界面复用共享设置和生命周期；异步结果检查 pane、进程、会话与任务代次，不能让旧结果覆盖新状态。
- 保留 Hook 的 owner/PID/session 校验；屏幕文本不能冒充可靠的完成事件。
- 保持既有 CRLF。禁止 `cargo fmt --all`；需要格式化时只处理明确的单文件。
- 只做受影响模块的验证，编译/状态单测不等于实际 UI 验收；未验证的路径明确说明。
- 架构门禁用 `python3 scripts/check_architecture.py --base <共同基线>`；不抬预算、不删覆盖来通过检查。

## 按需查阅

- 应用与 UI：`nebula_app/AGENTS.md`；终端：`nebula_terminal/AGENTS.md`；设置：`nebula_settings/AGENTS.md`。
- 脚本：`scripts/AGENTS.md`；文档：`docs/AGENTS.md`。
- 模块职责、公共接口或依赖变更：查 `docs/architecture.md`、`docs/project-constraints.md` 和 `architecture/` 中对应规则。
- 新 UI 文案：查 `docs/internationalization.md`。
- 版本、构建、打包或发布：读 `packaging/AGENTS.md`、`docs/release-notes/AGENTS.md`。
- 仅在协议、所有权、持久化或生命周期有重要取舍时按 `architecture/notes/AGENTS.md` 记录原因；普通修复不另写说明。
