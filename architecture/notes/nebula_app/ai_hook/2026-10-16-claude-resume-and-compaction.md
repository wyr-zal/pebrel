# Claude resume identity and compaction boundaries

## Status

Implemented for the personal candidate; installed-session acceptance pending.

## Context

A live WSL Claude session could show Finished while working. A hook-owned pane
and an unhooked pane need different evidence, not different state machines.

## Evidence

- A saved Claude transcript path referred to an old worktree. Real hooks carried
  the same session ID with a new project-directory path. Recovery rejected them.
- Claude 2.1.294 can show its composer without the older interrupt footer during
  work. The unhooked lifecycle promoted repeated idle observations to Done.
- Isolated real-CLI tests emitted PreCompact, SessionStart(compact), PostCompact
  on success, and StopFailure on compact failure. Esc emitted no end hook.
- Compaction hook stdout appeared as visible JSON even with suppressOutput.
  Verified owning-PTY delivery preserved all six lifecycle frames without JSON.

## Decision

- Allow a relocated Claude transcript only when both filenames still equal the
  same saved native session ID. Preserve other providers' file matching.
- Keep completion under hooks for Claude. Bound the new work indicators to the
  current composer chrome; neither a prompt nor historical prose proves success.
- Keep compaction private to AgentActivity, with the original activity and a
  distinct compact prompt ID. End hooks are idempotent and never complete a task.
- Cancel only after the owning pane sends Esc and current chrome becomes idle.
  This retires the phase without changing allows_screen or publishing success.
- Only the new POSIX compact events use the existing OSC channel via the owning
  foreground PTY. Check ancestor PID/start time, pane/instance/token, foreground
  group, and opened device against the owner's kernel-reported controlling TTY.
  Detached hooks cannot use tcgetpgrp themselves: Linux returns ENOTTY.

## Rejected alternatives

- Extending idle delays still manufactures eventual success.
- PID liveness cannot distinguish working from waiting at a prompt.
- Dropping ownership checks or using a multi-capability runtime token is unsafe.
- Routing every existing Claude event differently is unnecessary for this fix.

## Consequences

Unhooked successful turns may omit a completion notification. Unknown terminal
ownership fails open for the CLI but does not authorize a compact event. New
prompt, shell-exit, failure and cancellation boundaries invalidate old phases.
A no-op compact (not enough messages) also emits no end hook in 2.1.294; it
currently requires Esc to converge. Historical prose or elapsed time cannot
safely authenticate that phase boundary.

## Validation

Focused lifecycle, matcher, GPUI adapter, installation and POSIX bridge tests.
Real-CLI captures use an isolated HOME and local fake API. GUI and real-provider
acceptance are separate gates, not inferred from compilation or frame capture.
An isolated product window passed work/Stop, manual compact, Esc and continue
state checks against its own Runtime. Automatic compact succeeded twice in a
separate real-CLI capture; real-provider acceptance remains pending.

## Supersedes

None.

## Revisit when

Claude supplies an authenticated compact-cancellation hook or a non-visible
terminalSequence contract for compaction; remove the corresponding workaround
only after the same negative ownership and lifecycle tests pass.
