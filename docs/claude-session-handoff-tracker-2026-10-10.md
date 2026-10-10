# Claude 后台会话移交：问题、证据与状态跟踪

> 最后更新：2026-10-10（Asia/Shanghai）。当前状态：**G0 未通过，产品修复未实施；脱敏记录集已获准提交到分支并推送到 origin，Claude 上游请求尚未提交**。
> 本文是公开工程交接记录，不是用户功能说明或发行说明；机器路径、个人/测试会话 ID、PID、原始配置、transcript 与运行日志已从公开副本中排除。

## 1. 任务身份与工作区

- 原始排查和接续过程保存在维护者本地会话记录中；会话 ID 与原始日志不随本记录发布。
- 原始诉求：Claude 后台会话移交后，Pebrel pane 的当前会话身份、状态和通知可能失步。
- 已选范围：方案二；Windows 原生 Claude 与 WSL Claude；pane 跟随当前查看会话；使用独立 worktree。方案二和 G0 补充设计已批准；本次公开仅限脱敏记录集。
- 正式修复分支：`fix/claude-session-handoff-native-wsl-20261010`，基线 `244d6fe1ffcc0acf771847b85d47c6facf329cb5`。该基线是当时本地 main，不声称为上游最新。
- 本报告只公开经核实的事实、方案门槛和可审查记录；机器绝对路径、个人 session IDs、PID 与原始运行日志不发布。
- 隔离配置、probe 输出、完整会话日志及原用户报告本身未包含在公开记录集内。

## 2. 问题表现与目标

### 2.1 用户报告与独立复现的区别

- 原用户验收表象：后台会话移交后 pane 仍显示旧会话身份和 `running`，而新会话已继续运行；要求状态来源、登记和完成通知随当前会话一致变化。具体用户与测试 session IDs 不在公开记录中。
- 原始登记文件的机器路径及账户信息仅供本地核验，未复制到本报告；发布版不披露。
- 隔离复现使用独立的测试会话 A/B（标识符省略）：后台 worker 确实产生 Hook/Stop，但 pane 保留旧身份/运行状态；同 pane 用完整新 ID 前台 resume 后身份和状态恢复。
- 上述恢复是临时操作路径，不是修复；真实用户任务未被停止。完成通知横幅未目视验证，不能由恢复结果推导“通知已修好”。

### 2.2 成功标准

1. Windows 原生与 WSL 都能证明“pane 前台连接当前查看的是哪个完整 session”，并核验该 session 对应的后台 worker。
2. 合法移交后，身份、Hook 状态、恢复目标、会话登记和通知一致更新；不只改一个 `session_id` 字段。
3. 列表→A→B→A、已有会话重新附着、返回前台均正确；列表不继续借用旧后台会话的状态。
4. “当前查看”指 Claude 前台连接呈现的会话，不是 Pebrel 窗口是否拥有键盘焦点；应用失焦不等于解绑。
5. 旧 Hook、跨 pane、跨实例、PID 复用、错发行版/用户和嵌套 Agent 不能接管；切回会话不重播历史完成通知。
6. Windows/WSL 隔离产品链路及相关单点回归通过；视觉通知单独验收。未验证项必须保留，不将假 API 或状态断言冒充完整实机通过。

## 3. 背景与已确认的故障链

```text
原 pane -> 前台 Claude（旧 session / 原进程树）
              |
              +-- 后台移交 --> daemon worker（新 session / 新 PID）
                                  |
                                  +-- 继承旧 pane / 管道环境
                                  +-- Hook 已产生
                                         |
                           旧 pane 路由 + 进程树 / owner 校验不匹配
                                         |
                           pane 身份、状态、通知未正确随移交更新

WSL 另一个缺口：后台 Hook 无 controlling TTY -> 原 bridge 不发事件帧

已知连接：pane -> 前台进程；daemon -> worker -> session
待证明连接：前台连接 -> 当前正在查看的 session
```

- 历史 Windows 2.1.280 隔离复现确认：后台移交改变 PID/进程祖先关系，却继承旧 pane 信息。这解释了现有防护为何拒收，不说明这些防护应被删除。
- `continued-in` 只证明历史移交；job/session 元数据只证明任务身份。它们不能独立证明前台此刻正在查看该任务。
- 只放宽 owner 不够：窗口路由发生得更早；WSL 还可能根本没有收到事件。

### 当前源码定位（本轮已核对）

- `nebula_app/src/gpui_shell/workspace/windowing.rs:947`：`dispatch_ai_events` 先按事件 pane 选择窗口。
- `nebula_app/src/gpui_shell/workspace/agents.rs:233`：`handle_ai_hook` 继续选择目标 pane。
- `nebula_app/src/gpui_shell/terminal/view/agent_activity.rs:13`：校验 client PID 是否属于 shell 进程树，再检查生命周期归属、恢复选择和事件排序，之后投影状态。
- `nebula_app/src/ai_hook/lifecycle.rs:143`：`accepts_hook` 检查 owner；不同 PID、未满足同进程 SessionStart 条件的会话变化会被拒收。
- `nebula_app/res/hooks/remote_bridge.py:163`：`write_terminal` 打开 `/dev/tty` 写 OSC；无 controlling TTY 时这条发送路径不可用。
- 模块内简称 `res/hooks/remote_bridge.py` 对应正式仓库相对路径 `nebula_app/res/hooks/remote_bridge.py`。

## 4. 已探明的状态与证据边界

### F1：原生后台移交故障与临时恢复【历史实测】

原始测试确认后台 Stop 已记录、pane 停在旧 ID、同 pane 前台 resume 可恢复；`conclusion.json` 同时明确 `product_fix_implemented=false`、`notification_banner_visually_verified=false`。本轮复核记录，未重新执行这条实机链路。

### F2：两端后台 Hook 能产生【前一会话实测】

WSL Claude 2.1.294 与 Windows Claude 2.1.280 的隔离任务均通过本地假 API 完成固定回合；Hook logger 记录到 SessionStart/UserPromptSubmit/Stop。标识符、完整配置和日志未公开。该结果证明 Hook 产生，不证明 Pebrel 已接收、归属正确或通知弹出。

### F3：无 TTY 投递失败【前一会话实测负例】

WSL 后台 Hook 没有 controlling TTY。以独立 session 调用当前 bridge：退出码 0、stdout 0 字节、无有效事件帧。退出码 0 是对 Agent 不报错的行为，不能解释为送达成功。

### F4：共用传输有正对照【前一会话单点测试通过】

`test_installed_launcher_delivers_native_stdin_over_real_controlling_tty`：1 test OK；测试用 Codex envelope 验证共用传输的 5 个事件。它不是 Claude daemon 的端到端验收，也不覆盖无 TTY 路径。

### F5：现有候选输入不能判定当前查看会话【前一会话实测反例】

WSL 隔离观察通过 `claude attach` 返回 Agent 列表后重新打开同一测试会话；终端视图发生变化，但前台进程/argv/选择提示、daemon jobs/leases、Hook 计数保持相同，且没有新的 SessionStart。attach-journal 没有目标 session/job ID。具体 PID、session ID 和环境值已省略。这是“仅靠这些输入不能判断当前所看会话”的反例，不是对所有可能接口不存在的证明。

### F6：Windows 完整内部切换仍缺验收【未验证】

原生 CLI 附着后已显示测试会话；返回列表遇到隔离配置 trust 提示，后续未确认完整切换。测试窗口默认 WSL shell，再经 Windows pwsh 启动原生 CLI；不能据此声称纯 Windows shell/ConPTY 路径全覆盖，Runtime `send_key` 成功也不等于 UI 切换成功。

### F7：本轮发现 daemon 内部保存附着对象【静态线索，非可用接口】

- 只读检查本机 WSL Claude 2.1.294 可执行文件的嵌入代码，见到 `case"attach"` 按 `h.short` 取 worker，并将客户端记录放入 `n.attachers`；相关记录包含终端尺寸、能力、发送/断开回调和 `imarkNonce`。
- 同一已检查路径中，`list` 返回 job record，`leases` 返回 clients；尚未证明外部调用能得到“前台 PID/创建期 -> 当前 job/session”的完整实时映射。
- attach-journal 写入代码包含 gesture、surface、PID、procStart、时间及性能字段；所检查写入路径未见目标 session/job 字段，与 F5 记录一致。
- `replayInteractiveMarksTo` 等内部标记只构成进一步调查线索；未证明它们可供 Pebrel 安全取得当前身份或替代 Hook。
- **本轮没有修改 Claude 二进制、没有接入其私有协议、没有实测新绑定或新传输。内部存在 attach 状态，不等于已有受支持的外部接口。**

### F8：隔离与清理【历史记录 + 本轮工作树核对】

- 历史隔离清理记录确认测试用 Claude daemon、测试窗口与假 API 已退出；真实 Claude / 日用 Pebrel 未被停止或重启。
- 本次公开记录集不包含探针目录、用户配置、daemon keys、transcripts、原始 Hook/API 日志或截图；也未从其它任务分支复制产品源码。
- 本任务尚未修改产品源码；恢复操作仅在历史隔离测试中完成，不能视为产品修复或通知视觉验收。

### F9：官方 Hooks 没有 attach/switch/detach 事件【官方文档核查，2026-10-10】

- 核对 [Claude Code Hooks reference](https://code.claude.com/docs/en/hooks) 的生命周期事件表：包含 `SessionStart`、`SessionEnd`、prompt、tool、agent/task 与通知等事件；未列出附着、切换到既有会话或解绑事件。
- 文档将 `SessionStart` 的来源列为 `startup`、`resume`、`clear`、`compact`、`fork`；`SessionEnd` 表示会话终止。它们不能被提升解释为任意后台会话的前台 attach/switch 信号。
- 这是对当前公开 Hook 合同的核查，不证明未来版本或其它受支持扩展接口绝对不存在；因此仍需具体接口证据才能通过 G0。

### F10：CLI 内部 attach 元数据不是可依赖的公开合同【本机 2.1.294 静态复核】

- 只读检查本机 Claude 2.1.294 的二进制嵌入代码，见到 `case"attach"` 按内部 short ID 取 worker，并将 client 记录放入 `attachers`；相关记录包含终端尺寸、能力和回调，但不等于受支持的外部绑定接口。二进制路径与 fingerprint 不随本记录发布。
- 已观测的 attach-journal 记录只有前台 PID/创建期、surface、gesture 和时间，没有目标 sessionId/jobId；daemon `leases` 观测只有 client label/cwd/PID。两者没有建立“当前前台连接正在查看哪个完整 session”的可验证映射。
- 不把 `CLAUDE_AGENTS_SELECT`、daemon 私有协议或 CLI bundle 的静态实现作为产品授权来源；版本变化、进程创建期与当前视图之间均缺少受支持合同。

### F11：多会话切换矩阵仍未完成【未验证】

- 历史 WSL 观察覆盖 Agent 列表与重新附着同一测试会话；不等于 `A→B→A` 覆盖。本轮检查到的隔离配置只有一个已停止任务、daemon roster 为空，未启动新 daemon 或重建第二个任务，因此本轮没有补做该序列。
- 即使后续在隔离 CLI 中测得额外内部信号，也必须先证明其公开/受支持、可校验且失效语义完整，不能仅凭一次静态或 UI 观察进入产品实现。

### F12：正确独立树没有已实现的 WSL 无 TTY Hook 后备通道【只读静态复核】

- `nebula_app/src/ai_hook/remote.rs` 为 WSL 安装 guest `remote_bridge.py`；`nebula_app/res/hooks/remote_bridge.py:163-184,187-202` 的唯一终端发送路径打开 `/dev/tty` 写 per-PTY token OSC，失败时保持 fail-open，没有 no-TTY IPC fallback。
- `nebula_app/src/platform/wsl_hooks/windows.rs:119-143` 生成每个 PTY 的 token；`nebula_terminal/src/event_loop.rs:234-237` 只在对应 PTY token 匹配时接收该 OSC。此 token/OSC 合同不能证明当前前台 session 或后台 worker 身份。
- `nebula_app/src/agent_env.rs:169-185` 会向 WSL 透传 `PEBREL_RUNTIME_ENDPOINT`，但 `nebula_app/src/runtime_api/server.rs:10-20` 将其定义为实例级 endpoint/token；`runtime_api/transport.rs:70-115,147-181` 先以该 token 认证，再暴露多种控制能力，当前没有 Hook 专用 submit method。不得拿此 token 当 Hook ingress credential。
- 本机 `nebula_hook` 的 native named-pipe 路径不在当前 WSL guest Python launcher 中；WSLENV 未透传 `PEBREL_NOTIFY_PIPE`，当前 `remote_bridge.py` 未调用 `PEBREL_HOOK_EXE`。Windows helper 被 WSL interop 调用时的 pipe/OS caller PID/guest identity 绑定均未验证。
- 结论严格限于本分支静态代码：**现有 WSL no-TTY Hook IPC 未发现，替代方案未实现/未实测**；不是“所有未来实现都不可能”。本轮没有启动测试服务或 daemon。

## 5. 证据索引与发布边界

- **官方合同来源**：[Claude Code Hooks reference](https://code.claude.com/docs/en/hooks)。其公开事件表不承诺前台 attach/switch/detach 回调；在线文档未固定到本机 CLI 版本，结论不外推到所有版本。
- **代码证据**：第 4 节的 `nebula_app/...`、`nebula_terminal/...` 均为仓库相对路径，可在同一源码树核对当前实现。
- 历史隔离探针确实支撑了文中的复现与反例结论，但原始 `.omx/g0` 配置、API/Hook 日志、transcript、daemon key、截图及个人本机路径属于维护者本地证据，不随本记录发布。
- **E10 / G0 补充设计**：`.omx/plans/claude-session-handoff-g0-design-20261010.md` 与 `.omx/plans/claude-session-handoff-g0-design-20261010-reviews.md`。
- **E11 / Claude 前台身份合同请求草案**：`.omx/plans/claude-session-view-upstream-request-draft-20261010.md`；仅作为 branch record 发布，未提交给 Claude/Anthropic upstream。
- 本次提交范围仅为本 tracker、E10、E11 与 `.gitignore` 的单文件 allowlist。原始实施计划、Ralplan context/state、探针配置/日志、transcripts 和会话记录不在公开集。
- 任何后续重现实验都必须重新核验 Claude/WSL 版本及隔离环境；本报告没有公开个人 session IDs、测试 session IDs、PID、创建时间或 transcript 内容。

## 6. 方案与约束

### 已排除：直接放行 fork / 换 PID / 改登记

改动小，但不能证明当前归属，无法解决提前路由与 WSL 投递，可能串 pane、串实例或错误接管嵌套 Agent。不通过取消校验、旧 pane 环境、cwd、mtime、标题、屏幕文字或短 ID 建立绑定。

### 已批准方向：方案二，核验当前关联后显式转交 owner

```text
pane / 前台进程及创建期 / 命令代次
  -> 独立核验当前完整 session + worker 生命周期
  -> 内部已验证绑定（不是 Hook JSON 自报）
  -> 先解析目标窗口 / pane，再过 owner / 恢复 / 顺序门
  -> 唯一 AgentActivity 状态机 -> 身份 / 状态 / 登记 / 通知
```

- 优点：保留隔离与身份防护，统一后续投影；代价：依赖可靠关联信号及有界安全传输。目前仅方向获批，G0 条件未满足。
- 绑定至少约束实例、pane/命令代次、前台 PID/创建期、session、worker PID/创建期；WSL 另约束发行版、用户及来宾身份。宿主和来宾 PID 不混用。
- 列表、切换、前台退出、worker 更换、PID 复用、pane 关闭或重连使旧绑定失效；结果应用前重验代次。歧义不广播、不选 MRU、不猜最近任务。
- 不依赖新的 SessionStart 才发现附着；拒收事件不得消耗新 owner 排序。切回同 session 不重发历史完成；身份发现本身不生成 Done 或通知。
- 平台 I/O 只产事实，归属规则在 `ai_hook`，GPUI 不新增第二套状态机；验证必须后台、有界、可取消，不在渲染回调阻塞扫描。

### 修复可行性判断（2026-10-10）

- **完整范围不是当前 Pebrel 单方面就能安全完成的修复。** 根据已检查的官方 Hooks reference 与本机 2.1.294，当前没有受支持的 Claude 前台 attach/switch/detach 身份合同；Pebrel 无法仅凭现有公开 Hook 数据知道用户此刻在 Agent 列表中选择查看的是哪个完整 session。不能用私有实现或猜测信号补这个权威输入。
- **这不表示整个修复都要 Claude 官方代做。** Claude 侧需提供受支持的前台会话身份/生命周期信号；WSL 无 TTY 的安全 Hook transport 仍属于 Pebrel 的实现责任。本分支静态检查未发现现成替代通道，之后仍需设计、实现并验证窄权限投递。
- 因此结论是：**原范围原则上有修复路径，但当前公开合同与 Pebrel 现有传输不足以安全开工。** 若未来获得官方合同且 Pebrel transport 的 G0 正负样本通过，完整修复可继续；若拿不到该合同，则不能声称原范围已修复，任何缩小范围都需另获用户批准。
- E11 草案已纳入分支记录文档发布集；这不是向 Claude/Anthropic 上游提交请求，后者仍需单独授权。

### G0 未通过时的补充路径【待确认】

1. 优先证明 Claude 是否提供外部可查询的 attach/detach 合同，能绑定前台 PID/创建期与完整 session。内部 `attachers` 仅是调查入口。
2. 关联成立后验证 WSL 无 TTY 投递，优先评估现有安全通道；继承 token/环境本身不能成为当前 pane 的授权。
3. 若需要新协议、broker、常驻服务或依赖其它 PR，先提交最小设计、认证/失效/预算/清理代价，等待用户确认再实现。
4. 若确实没有合适扩展点，报告具体上游缺口，或请用户批准缩小为受控显式 attach 入口；后者不覆盖任意内部切换，不能算原范围完成。

不动 Codex 重连修复、自动续跑策略、供应商、CC Switch、真实 Hook/信任配置、持久化 schema；不暗中合入 PR #542；不新增设置或通用 Agent 框架；不扩展 SSH/macOS/Linux 原生 daemon 支持。主 Context 中其它通知/无 TTY 修复属于其它任务，不能替代本分支后台移交链路的验证。

## 7. 阶段进度

1. **原始问题与根因复现：已完成历史验证。** 输出 M01；产品修复与通知横幅未完成。
2. **方案二范围与行动清单：已批准。** 原范围与验收边界见本报告第 2、6 节；上游接口、协议实现或缩小范围均需分别授权。
3. **独立分支与同级目录：已完成，本轮核对通过。** 保持基线，不搬入主树其它任务改动。
4. **官方合同与正确独立树的 WSL 传输复核：已完成只读核查。** 官方 Hooks reference 未承诺前台 attach/switch/detach 事件；本机 CLI 私有元数据无完整当前 session 映射；目标分支 guest bridge 只走 `/dev/tty` OSC，无现成无 TTY Hook IPC。Runtime endpoint 是实例级多能力控制凭据，不能复用为 Hook token。A→B→A 未实测。
5. **G0 补充设计 Ralplan：计划与审查完成，用户已批准随分支记录发布。** E10 定义拟议前台 view-change 合同、候选 WSL 窄权限 ingress、正负样本和双 gate；内部架构/批评异议已逐项处理。E11 随分支记录文档发布，但未向上游提交；Ralplan 标记 `complete` 仅指规划闭环，不代表 G0 通过或产品实施授权。
6. **G0 实际原型/失败回归与产品实现：未开始。** 需要公开可支持 attach 合同、可验证的 WSL no-TTY 投递路径和后续明确实施授权；不得取消 PID/owner/order 防护。
7. **构建/产品验收/终审：未开始。** 本轮仅完成只读核查与记录文档脱敏；未运行 Cargo、测试、产品、Claude daemon 或 mock server，产品源码未改。
8. **提交/推送/部署：** 经用户明确授权，仅发布本 tracker、G0 补充计划、计划审查、未发布请求草案及精确 docs allowlist；产品源码、原始证据和真实配置不在提交范围。产品功能仍未实现或部署。

## 8. 下一步执行顺序与退出门槛

1. **工作区/保护核对：已完成。** 已确认目标分支与远端；本次发布集未包含任何产品源文件、探针或真实配置。
2. **记录文档发布：已获明确授权。** 仅提交本 tracker、G0 补充计划、计划审查、未发布请求草案及精确 docs allowlist；`.omx/g0`、本机 Context/state、原批准计划、配置、logs、transcripts、keys 与既存 `AGENTS.md` 差异均排除。
3. **外部上游请求：仍未授权/未提交。** 将请求草案作为分支记录发布，不等于向 Claude/Anthropic 建立 Issue/PR 或发送消息；若要对外提交，需另行明确授权。
4. **WSL transport G0：** 在候选路径经单独设计批准后，用隔离配置验证 no-TTY positive、wrong pane/instance/distro/user、旧 token/generation、重放/超时；不得使用 Runtime master token。A→B→A、list/detach、re-attach 同步采集。
5. **G0 评审：** 两端均能验证前台当前完整 session 与来源，WSL 无 TTY event 安全到达并且负例不污染状态/排序/登记/通知，才进入实现；任一缺口都停下报告，不缩小范围。
6. **定向 TDD 与产品修复：** 先写失败用例；按 G0 证据决定实际最小写集；通过窗口路由前建立绑定并由唯一 lifecycle 投影状态。
7. **局部验收：** 按本报告和 G0 验收矩阵单点测试、产品 feature build、两端隔离真链路与通知目视验收；不以 compile/state test 替代实机视觉验证。
8. **完成/交接：** 记录实际通过、失败、未验证和清理情况，更新 tracker/Context；最终验收通过后另询问是否提交。


本地已有单点正对照命令（历史已通过，本轮未重跑）：

```sh
python3 -m unittest scripts.tests.test_remote_hooks.RemoteHooksTests.test_installed_launcher_delivers_native_stdin_over_real_controlling_tty
```

未来实机观察仅使用环境导出的 `PEBREL_CLI` / `NEBULA_CLI` 发现 Runtime，再调用 `agent list` / `ctl`。无变量记 `runtime_unavailable`，不猜端口或扫描进程发现服务。Cargo 在 Windows PowerShell 7、独立工作区和独立 target 下，带 `--locked`；Git 只用 Windows Git 与 Windows `-C` 路径。具体测试集合按实际改动确定，不默认全仓。

## 9. 验证汇总与维护规则

- **通过（历史）**：原生独立复现及同 pane 前台恢复；两端假 API 回合和 Hook 产生；WSL 选择反例成立；共用传输真实 TTY 单点 1 项；隔离清理。
- **失败/未满足（历史）**：无 TTY 原 bridge 投递；当前候选数据无法证明前台查看的会话；G0 总门槛未通过。
- **通过（本轮核对）**：同级工作树/分支/基线及清理状态；官方 Hooks reference 的范围结论；正确独立树的 WSL guest Hook launcher、`/dev/tty` 写入、每 PTY OSC token、WSLENV 与 Runtime API 认证边界只读复核；本次公开集包括 tracker、G0 plan/review 与 E11 草案，Ralplan context/state 和原始探针仍排除。
- **未验证**：官方受支持 attach/switch/detach 合同是否会提供；WSL 真实 no-TTY 窄权限 transport；`A→B→A` 多会话切换；Windows 完整列表/多会话切换；产品修改后的构建/恢复/通知、Windows 系统横幅与真实供应商链路；未跑全仓或本地测试。
- **安全边界**：不停止/恢复真实后台任务，不改真实配置或数据库，不部署、不改产品源码；仅提交本次获批且脱敏的记录文档，不提交原始探针/日志/凭据，不向 Claude 上游提交请求。
- **状态更新规则**：每次确认重要事实、作出方案决策或完成验收，更新页首状态、第 7 节和本节；新结论附证据，旧证据标历史/失效原因，不无声覆盖；主 Context 只同步入口、阶段、下一步和关键风险。
- **2026-10-10 首轮接续记录**：接手原会话，核对已迁移同级目录；补充 F7 静态线索；按用户新要求建立统一跟踪文档。业务源码未改，G0 状态未升级。
- **2026-10-10 本轮继续记录**：核对官方 Hooks reference 与正确独立树代码。官方文档未建立 attach/switch/detach 合同；目标 WSL bridge 只有 `/dev/tty` OSC 路径，无现成 no-TTY Hook IPC；实例级 Runtime endpoint token 不可复用作 Hook 凭据。用户明确授权本记录集随分支提交并推到 origin；E11 仅是仓库记录，不是向 Claude/Anthropic 上游提交的 Issue/PR。G0 仍 blocked；未运行本地测试/daemon/产品，未改产品源码或真实配置。
