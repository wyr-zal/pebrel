# Ralplan 审查记录 — Claude handoff G0 补充计划

- 计划：`.omx/plans/claude-session-handoff-g0-design-20261010.md`
- 审查事实依据：本计划第 2 节和公开交接记录中的事实摘要；本机 Ralplan context/state 与原始探针未纳入公开集。
- 日期：2026-10-10
- 审查方式：本会话依次执行 Planner、Architect view、Critic view 与 Revision；没有调用独立审查 Agent，不将本记录描述为独立第三方批准。

## Planner 基线

- 保留用户已批准的方案二：Windows 原生与 WSL Claude，pane 跟随当前前台查看会话，覆盖列表选择、A→B→A、重附着和返回前台。
- G0 有两个互相独立的硬门：Claude 前台 attach/switch/detach 的公开可验证合同；WSL 无 TTY 的窄权限 Hook 交付。
- 两门未通过前不放宽 PID/owner/order，不实现产品代码，不提交上游请求。

## Architect view

1. **A1 — 两条信任链不可合并。** 前台 session 绑定负责确定“谁当前被看”；Hook transport 只负责把事件送到受约束 pane。`session_id`、token 或 worker PID 单独都不能同时证明两者。**判定：accepted。** 计划使用 AND gate 与两套独立证据。
2. **A2 — Runtime listener 的现有鉴权是全局 token 先验。** 只添加 `hook.ingest` 动词而继续使用 `PEBREL_RUNTIME_ENDPOINT` 不构成最小权限。**判定：accepted。** 计划明确独立认证分支必须在普通 Runtime dispatch 之前完成，Hook capability 不得回落或调用普通动词。
3. **A3 — 不能增加第二份 pane/session 权威状态。** transport 注册应指向现有 Runtime/PTY 所有者，领域生命周期仍归唯一 `AgentActivity`。**判定：accepted。** 计划要求服务端从已注册的 capability 映射 pane，并在现有 owner/order gate 后投影。
4. **A4 — Windows 与 WSL PID 命名空间不同。** 来宾自报 PID、pane ID 或继承环境不等同内核可核验来源。**判定：accepted。** 计划要求分别证明 guest distro/user/process-start 到宿主 PTY 的可信绑定，否则 G0 fail。
5. **A5 — 新 broker/常驻服务扩大运维面。** **判定：accepted。** 其实现不在授权内，需另提设计并获批。

## Critic view

1. **C1 — 在线 Hooks 文档未钉到本机 CLI 版本。** 若泛化为“所有版本不存在接口”会过度断言。**判定：accepted。** 计划限定为当前公开 reference 未承诺，静态数据限定 Claude 2.1.294。
2. **C2 — 多会话反例尚缺 A→B→A。** 原有同会话重附着不能替代完整矩阵。**判定：accepted。** A→B→A、返回 list、重附着及无 TTY 均保留为未验证 G0 样本。
3. **C3 — 后台 worker 不能自己推进前台代次。** 否则合法 token 可携带旧/伪造 session 夺取 owner。**判定：accepted。** `view_generation` 必须由通过验证的前台合同建立；worker Hook `session_id` 只能匹配现有绑定，不能创建或推进它。
4. **C4 — WSL helper 可执行文件存在不等于 helper 路由可用。** 必须验证通知管道透传、Windows caller process tree、guest distro/user 和 pane generation。**判定：accepted。** `PEBREL_HOOK_EXE` 路径只列为需证伪候选，不当作现有替代通道。
5. **C5 — 外部上游请求和本地计划批准是两种授权。** **判定：accepted。** 本计划不发送消息；发布支持请求须另获明确授权。
6. **C6 — 计划 `complete` 容易被误读成 G0 已通过。** **判定：accepted。** 计划首页、状态 JSON 与验收节都注明 `complete` 仅指规划闭环，G0 仍 blocked。
7. **C7 — 产品任务写集需有可追溯入口，但不能预先授权全量触碰。** **判定：accepted。** 由 G0 证据确定实际最小写集；不把候选路径清单当作全量修改授权。

## Revision / 处理结果

- 已将 transport 选项补为对比表；Runtime listener 被标为“优先评估候选”，不是已决定的产品接口。
- 补充现有 Runtime auth-before-dispatch 约束及单用途凭据的独立 auth 分支要求。
- 将 worker Hook 的 session ID 和 generation 与前台绑定分离；无经过验证的前台转换时事件不得建立或推进 owner。
- 标记官方文档版本范围限制；要求 G0 实测实际 Claude/WSL 版本，不把当前网页说明外推。
- 增列 native helper 的明确验证条件及失败即排除标准；无 TTY 路径当前依然未实现、未测试。
- 保留原任务成功标准和禁止自动缩小范围的要求。

## 共识结论

- **内部计划阻断异议：None。** Architect 与 Critic 意见均已逐项处理。
- **外部/实施门槛：仍未解决，且不是本轮规划可消除的事实。** Claude 尚无本任务可引用的公开 attach/switch 合同；当前 WSL no-TTY path 无后备实现；A→B→A 未实测。
- 因此 Ralplan 规划与记录文档发布已获批，但产品 G0 仍 **blocked / not passed**。对外发布的范围仅限指定记录文件；不等于授权向 Claude/Anthropic 提交支持请求、实施协议或改产品源码。
