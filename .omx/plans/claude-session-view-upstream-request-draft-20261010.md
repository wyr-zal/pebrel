# Claude Code upstream request draft — foreground session view lifecycle

> 状态：**仓库内的未发送请求草案。** 本文件经用户批准作为分支记录文档发布到 `origin`；它没有作为 Issue、PR、评论或消息提交给 Claude/Anthropic 上游。任何上游提交仍需单独授权。
> 依据：本地 Claude Code 2.1.294 的隔离观察，以及 2026-10-10 查询的官方 Hooks reference。该在线 reference 未固定到 2.1.294，本文不声称所有版本表现相同。

## Proposed title

Document a foreground session view-change event for attach, switch, and detach

## Draft body

### Problem

Terminal hosts that integrate with Claude Code need to know which full conversation the interactive foreground UI is currently displaying, separately from which background worker is running.

The documented hook payload includes `session_id`, but that identifies the session producing that hook event. `SessionStart` is not a reliable signal for every foreground selection or re-attachment from an existing-session/agent list. In an isolated WSL observation on Claude Code 2.1.294, returning to the agent list and opening an existing background session changed the visible conversation without a new `SessionStart`; the inspected attach metadata did not expose the selected full session ID to the host. This is a local version-specific observation, not a claim about every release.

Without a documented foreground transition contract, a host cannot safely distinguish “the user is now viewing session B” from “background worker A emitted a hook.” Guessing from inherited pane variables, recent jobs, transcript timestamps, or terminal text can route state to the wrong pane or session.

### Request

Please consider a stable, documented, versioned foreground-session view lifecycle event or API. The event/API should represent transitions performed by the interactive foreground client, including:

- entering an existing session through attach/resume or selecting it from the agent/session list;
- switching directly from session A to session B, including A→B→A;
- returning from a session to the agent list or shell, where there is no longer a foreground conversation;
- re-attaching to an already-running session without requiring a new conversation or `SessionStart`.

A minimal contract could provide:

- an explicit `foreground_session_id` containing the full current session ID, or an explicit “no foreground session” value when the UI is at a list/shell view;
- an opaque `frontend_connection_id` and monotonically increasing `view_generation` so a host can reject stale transitions after reconnects or rapid switches;
- a transition reason and, if available, the previous foreground session ID;
- documented ordering, duplicate, disconnect/reconnect, and delivery semantics;
- a clear distinction between a foreground UI transition and hooks emitted by a background worker.

The event should be emitted by the interactive foreground client, not inferred from the operating system window focus. Losing application focus should not implicitly detach the session. A suggested name such as `SessionViewChanged` is only illustrative; an equivalent supported API is fine. The host must still verify its own process/pane ownership and should not treat a payload-supplied PID as authentication.

### Acceptance examples

1. From the agent list, opening existing session A produces an observable transition to A even if no new `SessionStart` occurs.
2. Selecting A→B→A produces ordered generations for A, B, and A; a delayed event from an earlier generation cannot replace the latest view.
3. Returning to the list explicitly clears the foreground-session binding while leaving the background worker/session alive.
4. Re-attaching to A restores A as the foreground view without replaying historical completion events.
5. Switching application focus away from Claude Code does not emit a detach transition.
6. The contract documents which supported Claude Code versions expose the event/API and how hosts should handle older versions that do not.

### Scope note

This request concerns the Claude Code foreground session identity/lifecycle contract only. A terminal host still needs to provide its own authenticated, bounded transport for background hooks that have no controlling TTY; the foreground event must not be confused with hook delivery or treated as a replacement for host-side ownership checks.

### Evidence boundary

The local observation was made with an isolated WSL Claude Code 2.1.294 test setup. The complete A→B→A matrix has not been tested, and no claim is made about other versions or undocumented interfaces. The official Hooks reference currently documents `session_id` and session lifecycle events but does not promise a separate foreground attach/switch/detach callback: https://code.claude.com/docs/en/hooks
