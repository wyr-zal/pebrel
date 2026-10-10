# Claude 会话当前附着与 WSL 无 TTY Hook：G0 补充计划

- 日期：2026-10-10
- 状态：**用户已批准随分支发布记录文档；G0 仍未通过，产品实现未获授权。**
- 工作区：同级隔离 feature branch `fix/claude-session-handoff-native-wsl-20261010`
- 基线：`244d6fe1ffcc0acf771847b85d47c6facf329cb5`
- 依据：本计划第 2 节的已核验事实；本机 Ralplan context/state 与原始探针不属于公开记录集。
- 关联原范围：已批准的方案二（完整当前查看会话；Windows 原生与 WSL）；原始本地实施计划不随本次记录文档发布。
- 本计划不改产品代码、不运行测试、不写真实 Claude 配置、不创建服务、不发送 Claude/Anthropic 上游请求。

## 1. 目标与不变成功标准

在实现原批准的方案二之前，为 G0 提供两个**独立且都必须通过**的前置条件：

1. Claude 前台客户端提供可支持、可版本识别的当前会话附着/切换/解绑合同，能够区分“当前 UI 正在查看的完整 session”与“后台 worker 正在运行的 session”。
2. WSL 后台 Hook 在没有 controlling TTY 时有一个经过认证、有界、只够上报 Hook 的投递路径；该路径能绑定到确切 Pebrel 实例、pane/PTY generation 与所选会话，不依赖宽权限 Runtime 凭据。

最终产品成功标准沿用已批准范围（公开交接记录第 2.2 节）：Windows 原生和 WSL 都能安全通过后台移交、列表→A→B→A、已有会话重附着、返回前台与应用失焦场景；身份/状态/恢复目标/登记/通知一致，旧或歧义事件不改状态、不消耗排序、不广播、不复播历史完成。

**仅获得 `session_id` Hook 字段、仅让事件到达原 pane、仅通过编译/fixture，均不算两个 G0 条件已通过。**

## 2. 已核验事实与事实边界

- 官方 Hooks reference 列出通用 `session_id`、`SessionStart`/`SessionEnd` 等，但没有前台 attach/switch/detach 的独立公开事件：<https://code.claude.com/docs/en/hooks>。`SessionStart` 的 resume 来源不等价于 `claude attach` 的每次 UI 选择或 Agent 列表内部切换。该在线文档未固定到本机 2.1.294 版本；结论严格限定为“当前公开文档未承诺此合同”，不宣称所有旧版/未来版都不存在其它受支持接口。
- Claude 2.1.294 的本机 attach-journal/daemon 私有资料未能把当前前台连接映射到目标完整 session；`CLAUDE_AGENTS_SELECT` 属于内部实现线索，不作为公开合同或授权来源。
- `nebula_app/src/ai_hook/remote.rs` 安装的 WSL Claude 命令最终运行 `nebula_app/res/hooks/remote_bridge.py`。后者 `write_terminal` 只向 `/dev/tty` 写 OSC；无 controlling TTY 时没有替代发送分支，并保留原 notifier 的 fail-open 行为。
- WSL PTY 侧已有 per-PTY Hook token，并由 PTY parser 验证；现有 OSC token 只证明远端帧可被对应 PTY 接收，不证明当前前台 session 或 worker 身份。
- WSLENV 透传 `PEBREL_RUNTIME_ENDPOINT`，但该 endpoint 的 token 是实例级控制凭据。Runtime API 当前提供多个窗口/pane/agent 能力，没有 Hook 专用提交方法。不得把宽权限 token 作为 Hook transport。
- 本地 native named pipe / Unix socket 和 `PEBREL_HOOK_EXE` 尚未被证明是 WSL guest Hook 的可用替代路径：当前 guest bridge 不调用 host helper，通知 pipe 也未见于 WSLENV 透传合同。
- `A→B→A` 尚未实测；历史无 TTY 负例、Windows 完整切换等边界仍按原跟踪文档标为未验证。

## 3. ADR — 先取得两项合同，保持 G0 关闭

### Decision

保留已批准的完整支持范围，但在 Claude 侧公开前台附着合同与 WSL 窄权限无 TTY 投递路径均可验证之前，不实现 owner 转交或 Hook 新 transport。先完成本计划的合同规格、威胁模型和 G0 证据方案；外部请求及协议/服务实现另行授权。

### Drivers

- attach/switch 的可靠来源在前，才能把 Hook 放到正确窗口/pane 及 lifecycle owner。
- WSL Hook 当前依赖 TTY；只修 PID/owner 无法使无 TTY Hook 到达。
- Runtime endpoint token 授予多种控制能力，不符合最小权限 Hook 上报。
- 私有 CLI 数据、环境继承、屏幕文字和短 ID 均不能承担身份授权。

### Alternatives

| 方案 | 结论 | 原因 |
| --- | --- | --- |
| 依赖 `SessionStart`、内部 daemon/attach-journal 或 `CLAUDE_AGENTS_SELECT` | 排除为产品合同 | 公开合同未保证任意 attach/switch 回调；静态私有实现不稳定且缺前台目标映射 |
| 仅放宽 PID/owner 校验、按 pane/cwd/最近 job 接管 | 排除 | 不能证明当前查看者，可能串 pane、实例或嵌套 Agent |
| 复用 `PEBREL_RUNTIME_ENDPOINT` token 直接投递 Hook | 排除 | 实例级 token 保护多种控制 API，权限范围过宽且不解决前台绑定 |
| WSL 调用现有 `PEBREL_HOOK_EXE` / named pipe | 暂不视为已有解 | WSL guest 当前未走此链；pipe 透传、宿主 caller PID 树和 guest 身份均未证明 |
| 改为只支持显式 `claude attach <id>` | 不纳入当前计划 | 会改变原成功标准，必须另获用户批准 |

### Consequences

- 当前任务保持 G0 blocked，不改产品代码、不实施 protocol/broker、不发送 Claude 上游请求；本计划等记录文档的发布已获用户单独授权。
- 可能需要 Claude 上游增加受支持接口；未获用户授权前只准备本地规格，不对外发送。
- 如果 WSL 需要新的权限边界或常驻 broker，必须先对认证、生命周期、运维成本和回滚做独立设计并另获批准。

### Follow-ups

1. 完成本计划的合同字段与 transport 威胁模型，供用户审阅。
2. 若用户另行授权，才将 E11 请求草案提交到 Claude 上游并等待正式合同或版本；否则只保留为分支记录。
3. 合同可用后，另行批准 WSL no-TTY transport 的 G0 探针/协议实现。
4. 两个 G0 gate 都通过后，才执行本计划任务表列出的定向回归与产品实现。

## 4. 设计合同草案（拟议，非现有能力）

### 4.1 Claude 前台会话绑定

建议向上游请求一个公开、版本化的前台会话视图变化合同（拟议概念名 `SessionViewChanged`，名称由上游决定）：

- 由**交互式前台客户端**在当前呈现会话变化时发出；不能由后台 worker 的 `SessionStart` / `Stop` 代替。
- 一个事件明确描述变更后当前可见的完整 `foreground_session_id`；列表/无会话时必须能明确表达“无前台会话”，不能沿用最后一个 worker。
- 至少有稳定的 `frontend_connection_id`、单调递增的 `view_generation`、变更 `reason`（首次打开、resume、显式 attach、列表选择、切换、detach/返回 shell）和可幂等识别的事件身份。必要时提供 `previous_foreground_session_id`，使 A→B、A→列表、列表→B 可区分。
- `view_generation` 只在同一前台连接内递增；进程退出/重连创建新连接身份。事件顺序/重复语义必须文档化；旧代次不可覆盖新代次。
- 宿主仍以 OS 可核验的前台进程及创建期、pane/命令代次绑定来源；Hook JSON 自报 PID、pane 或 session 不作为单独授权证据。若 Claude 无法提供可验证的前台来源合同，G0 不通过。
- 窗口焦点变化（Pebrel/其它应用之间）不得隐式解绑；“Agent 列表”视图必须显式清除旧前台 session；身份切换本身不得生成 Done/Attention 或重播旧通知。
- 该合同必须有官方文档、版本支持范围和实际可验证实现。当前 Hook 文档没有此合同，以上仅为请求规格。

### 4.2 WSL 无 TTY Hook transport

优先调查“复用现有 Runtime listener，但增加权限独立的 Hook-only 接收能力”这一候选；这不是已经批准或证明可行的接口。

| 候选 | 优点 | 关键缺口 / 风险 | 规划结论 |
| --- | --- | --- | --- |
| 继续用 WSL OSC `/dev/tty` | 复用现有 per-PTY token 与 VT parser | 后台 worker 无 controlling TTY 即不能发送；不满足需求 | 负例基线，不是无 TTY 解 |
| WSL interop 调用 `PEBREL_HOOK_EXE` / named pipe | 复用宿主 helper 和本机 IPC 实现 | `PEBREL_NOTIFY_PIPE` 未纳入 WSLENV；interop helper 的 Windows PID 未证明属于 pane shell 树；guest distro/user 身份未证明 | 只做静态/隔离可行性核查，任一归属条件失败即排除 |
| Runtime listener 上单用途 Hook ingress + 独立 per-PTY capability | 可能复用现有 localhost listener，避免新常驻进程 | 现有 handler 在方法分派前统一校验实例级 endpoint token；新增 Hook-only auth 必须在普通 Runtime dispatch 外显式隔离。WSL 可达性、pane-token 注册和 guest 身份均未验证 | **优先评估的候选，不是已选接口或实现授权** |
| 新 broker / 常驻 guest 服务 / 跨发行版端口 | 可自定义身份握手和队列 | 新部署面、生命周期/升级/回滚/跨用户面扩大，超出当前已批准实现范围 | 当前不实施；只有单独设计与批准后才重开 |

候选 Runtime ingress 若进入设计，须在通用 endpoint token 校验/方法分派之前识别独立的 Hook capability，且 capability 只能到一个 Hook-ingest handler；不能让它回落到或调用任何普通 Runtime 方法。每个 per-PTY capability 在宿主内注册为 `{instance, pane, PTY generation}`，服务端据此派生目的 pane。仅将普通方法命名为 `hook.ingest`、却仍用或透传宽权限 `PEBREL_RUNTIME_ENDPOINT` token，不满足最小权限。

- **绝不复用** `PEBREL_RUNTIME_ENDPOINT` 中的实例级 bearer token；新接收端必须要求单用途 per-PTY capability，服务端将其映射到准确的 Runtime 实例、pane ID 和 PTY generation，并由服务端派生 pane 身份，不接受来宾自报 pane 覆盖映射。
- capability 需随机、短生命周期、单 pane 限定；pane 重建/关闭、shell generation 变化、Runtime 重启或绑定撤销时失效。评估是否复用现有 WSL per-PTY OSC token；若作用域或轮换合同不够，使用独立凭据。不能把拥有读写 pane/agent 等操作的 token 下发到 Hook。
- 单一窄 schema 只接收已支持的 Hook 生命周期 envelope，不开放通用 Runtime 命令；输入大小、单请求等待、并发连接、队列长度与 TTL 必须有明确上限，满队列/超时 fail closed，不阻塞 Hook/UI，也不吞掉用户自己的 notifier。
- 使用事件唯一 ID 与由已认证前台合同建立的 `view_generation` / 序号做重放、重复、乱序和 stale-result 防护。后台 worker Hook 的 `session_id` 必须匹配当前已验证绑定；worker Hook 本身不能创建、替换或推进前台 `view_generation`。先认证/验证来源，再确定窗口/pane，最后通过唯一 lifecycle owner/order gate。
- Windows PID 与 WSL guest PID 不混用。G0 必须证明 distro、uid、guest process start identity 与宿主 pane/PTY 的可信关联；来宾 payload 自报 PID 或继承的环境/token 不能独立证明 worker 身份。
- 另行评估从 WSL 调用 `PEBREL_HOOK_EXE` 的宿主 helper 方案：必须证明通知 pipe 可达、token 正确传递、OS caller PID 能关联到具体 pane、WSL distro/user 与重连代次可核验。缺任一项即排除，不因 helper 二进制存在而宣称已支持。
- 若唯一可行方案需要新 broker、常驻服务、跨发行版端口或全局 Runtime token，停止并提交独立设计请求；本计划不授权实现。

## 5. 行动清单

| 顺序 | 任务 / 归属 | 依赖 | 完成条件 | 验证方法 |
| --- | --- | --- | --- | --- |
| 1 | **上游合同规格**：仅维护本计划第 4.1 节与 tracker 的核实事实；不发布本机 Ralplan context/state | 无 | 产出不可歧义的事件语义/字段、UI与worker边界、版本/兼容要求、失败行为；清楚标为提议 | 对照官方 Hooks reference 和 Claude 2.1.294 现状；检查没有把私有实现写为支持接口 |
| 2 | **WSL transport 威胁模型**：补充本计划第 4.2 节 | 任务 1 的身份字段草案 | 明确 capability 所有者、权限面、路由映射、guest 身份、失效/重放/预算/清理；比较 Runtime listener 窄入口与 native helper；若评估 Runtime ingress，明确独立认证分支且证明 Hook capability 不能调用普通 Runtime 动词 | 静态核对 `remote_bridge.py`、`wsl_hooks`、`agent_env`、`nebula_hook`、`runtime_api` 的职责；任何无法验证项显式标 unknown |
| 3 | **可证伪 G0 测试规格**：扩展本计划第 6 节 | 任务 1、2 | 为 native 与 WSL 列出 positive/negative、采样字段与通过/失败阈值；覆盖 A→B→A、list/detach、重附着、无 SessionStart、无 TTY、旧/错 pane 和身份复用 | 审查矩阵是否每条安全不变量都有正样本、负样本及可观察判据 |
| 4 | **外部支持请求授权点**：不在本计划内发送任何消息 | 用户审阅任务 1 规格 | 若需上游请求，先由用户明确授权目标、内容和是否发布；若无授权，则保留为仓库内的未发送请求草案 | 外部平台未被访问或写入；无 Issue/PR/评论 |
| 5 | **G0 原型/实现授权点**：不在本计划内改产品代码 | 正式可支持的 attach 合同 + transport 设计获批 | 两个合同均有证据，且用户单独批准最小 G0 probe/protocol write set | 另立实现任务，先写 failing tests；执行清单前重新检查正确 worktree、target/config 和真实任务隔离 |
| 6 | **原方案产品修复**：按本计划 G0 门槛和证据确认的最小写集；入口预期为 `nebula_app/src/ai_hook/{lifecycle.rs,ordering.rs,remote.rs}`、`nebula_app/src/platform/wsl_hooks/`、`nebula_app/res/hooks/remote_bridge.py`、`nebula_app/src/runtime_api/{server.rs,transport.rs}`、`nebula_app/src/gpui_shell/workspace/{windowing.rs,agents.rs}`、`nebula_app/src/gpui_shell/terminal/view/agent_activity.rs` 及其定向 tests | G0 两项全部通过，且用户另行批准实施 | 当前会话绑定在路由前建立；唯一生命周期投影身份/状态/登记/通知；保留所有旧 owner/PID/order 防护；实际写集必须按 G0 收窄，不代表以上文件全部都改 | 按本计划 G0 验收矩阵执行定向 TDD、GPUI feature check/build、两端隔离实测及通知目视验收 |

## 6. G0 验收矩阵与硬门槛

| 场景 | 通过判据 | 失败判据 / 处置 |
| --- | --- | --- |
| WSL/Windows 前台从列表打开 A | 官方前台事件能绑定完整 A 到该前端连接与 pane generation；非 worker Hook 可以通过验证 | 仅有 Hook `session_id`、环境提示、日志顺序或 UI 文本：失败，不投影 |
| A→B→A、已有任务重新 attach | 每次选择都产生新鲜、排序明确的当前绑定；切回不复播旧 Stop | 缺事件/顺序歧义/事件是后台 worker identity：失败并保持 Unknown |
| 返回 Agent list、返回 shell、应用失焦 | 返回列表/退出显式解绑旧前台 session；仅切换应用焦点不解绑 | 列表残留旧 session，或把窗口 focus 当作 detach：失败 |
| WSL Hook 无 controlling TTY | 测试事件通过窄权限 transport 到达正确 instance/pane，端到端 envelope 与顺序完整 | stdout/进程退出 0 但无确认、仍依赖 `/dev/tty`、转用 Runtime master token：失败 |
| 错 pane/错 instance/错 distro/user/旧 PTY generation | 认证或归属门拒收，且不进入 pane lifecycle | 任何跨 pane/实例可见、广播、旧 token 可继续使用：失败 |
| worker exit/PID reuse/重连/乱序/重放/队列满/超时 | 旧请求被取消或丢弃；时间/大小/队列有界；hook 不阻塞 UI | stale event 修改新 owner、消耗新排序、错误登记/通知：失败 |
| 进程归属 | 宿主 PID+创建期与 guest PID+创建期按各自命名空间分别校验；不能用 payload PID 代替 | 宿主/来宾 PID 混用、只相信继承 pane id/token：失败 |
| 通知/完成语义 | 身份变更本身不产完成/注意/成功；每个新完成只通知一次 | attach 或切回触发历史通知：失败 |

**进入实现的 AND 门：** (a) Claude 公布并实现可验证的前台 view/attach 合同；(b) WSL no-TTY Hook 在窄权限、正确 pane/PTY 归属下真实送达；(c) Windows 原生相同前台合同通过；(d) 全矩阵负例不污染 owner/order/registry/notifications。任一不成立即 G0 未通过，不修改 owner/PID 防护。缺少上游合同或需要新服务/协议时暂停并向用户报告，不自行改成显式 attach-only。

## 7. 不在本计划授权内

- 发送、发布或评论任何 Claude/GitHub 上游 Issue、PR 或消息。
- 修改 Claude hooks/settings、CC Switch、真实用户配置、真实 session、供应商账号或日用 Runtime。
- 实现新的 Runtime endpoint、pane capability、Windows helper 路由、broker、daemon 或协议。
- 放宽 `accepts_hook`、client PID、pane ordering、recovery 或事件去重；从 transcript/screen/cwd/mtime/MRU 推导当前视图。
- 产品代码/协议的版本递增、提交、推送或部署不在授权内；仅本次列明并脱敏的记录文档获准提交到该 feature branch 并推送到 `origin`。
- 变更“当前查看 session”范围或以显式 attach 代替 A→B→A；这需要另一次用户批准与新验收标准。

## 8. 验收方式与交接

- 本轮 Ralplan 只验证计划文件之间的引用、范围/安全边界、审查异议处理和状态文件；不运行 Cargo、Python tests、产品启动或 daemon。
- Ralplan 状态 `complete` 仅表示规划与内部架构/批评审查闭环，不表示 G0、产品修复或测试完成。
- 用户批准本补充计划后，再根据任务 4/5 的权限点决定是否准备外部请求或 G0 原型；批准计划不等于允许发布、协议实现、改范围或提交代码。
