# Shell and Agent lifecycle boundaries

## Status

Implemented in the local candidate. Automated regression coverage passes; real
multi-pane UI acceptance is still separate.

## Context

A live Claude pane could change its sidebar state while the user scrolled its
transcript. A different pane received valid Codex lifecycle hooks after a late
shell command boundary, but the hooks were then rejected. A failed Codex resume
could also retain the saved identity while a separately confirmed Claude
process owned the same pane.

These symptoms shared an ownership problem. Shell command edges, provider
lifecycle hooks, recovery intent and screen observations were allowed to
overwrite state that another source had already proved.

## Evidence

- The 2026-10-19 live snapshot captured a Claude `magpie` form and task list
  below the composer, while the saved target for that pane still identified a
  failed Codex resume.
- The Codex pane continued emitting authenticated `PostToolUse` events for the
  same owner, session and turn after its shell coverage had been cleared.
- Focused regressions reproduce late `CommandDone`, process-confirmed provider
  takeover, retry after provider exit, stale prompt frames, history viewports,
  labeled composers, compaction and task-list chrome.
- The incident evidence and diagnosis are recorded in
  [the repair summary](../../../../docs/issues/261019-agent-state-mismatch/summary.md).

## Decision

- Treat an authenticated native lifecycle Hook as live ownership across a late
  shell boundary. The boundary records that the shell command ended, but a
  `Turns` or `Lifecycle` owner remains in force until `SessionEnd`, process exit
  or an explicit foreground reset.
- Keep recovery intent as three identities: the pending target being restored,
  the confirmed live provider, and the failed retry candidate. A live identity
  always wins publication and persistence; a failed candidate remains available
  only while no foreground provider is running.
- Permit any provider to take over a failed recovery only when its Hook source
  and PID/start time both match the independently probed foreground receipt.
  A stale Hook from the same provider is no more authoritative than a foreign
  one.
- Native Windows takeover additionally binds a pre-input shell instance and
  launch boundary, excludes already existing direct-child branches (including
  their future children), and requires exactly one new provider. A creation-time
  cutoff alone is insufficient after clock correction. Missing evidence leaves
  recovery unconfirmed.
- Pin the Windows pipe client and owner before consuming its message. The
  helper waits for pipe consumption before exit, within the existing two-second
  whole-process deadline. EOF does not prove that a short-lived client remains
  in the system process table. Keep the app and helper in the same delivery.
- Capture native takeover proof only for failed local Windows recovery, including
  bracketed runtime paste and its delayed Enter. Preserve early hooks associated
  with that captured command boundary across a delayed shell start event.
- Parse Claude's current composer as a bounded structure in the final 32 rows.
  Locate the latest prompt before a labeled bottom rule and the nearest rule
  above that prompt, so a stale prompt frame cannot enlarge the current activity
  region.
- Treat transcript history as a frozen observation of the current turn. It may
  revoke a screen-derived retry, but it cannot change the confirmed Agent state.

The authoritative implementations are
[`lifecycle.rs`](../../../../nebula_app/src/ai_hook/lifecycle.rs),
[`startup_command.rs`](../../../../nebula_app/src/gpui_shell/terminal/view/startup_command.rs),
[`agent_activity.rs`](../../../../nebula_app/src/gpui_shell/terminal/view/agent_activity.rs), and
[`screen_context.rs`](../../../../nebula_app/src/ai_agents/screen_context.rs).

## Rejected alternatives

- Resetting all Agent coverage on every `CommandDone`. Shell startup and
  redraw paths can deliver that edge after the provider has already published a
  valid session.
- Treating a Hook source as sufficient proof that a new provider owns the
  foreground process. That permits stale or foreign bridge output to replace a
  verified live provider.
- Reusing one recovery target for pending intent, live identity and retry
  history. `SessionEnd` then either erases a useful retry candidate or lets the
  failed target hide the provider that is actually running.
- Recognizing only unlabeled full-width rules or scanning the last three visual
  rows. Claude can place forms, controls and task chrome above a labeled bottom
  rule.
- Promoting a quiet composer, elapsed time or transcript prose to completion.
  Screen evidence remains fallback evidence with explicit uncertainty.

## Consequences

Late shell edges no longer clear a live Hook owner. A true `SessionEnd`, process
exit or frontend reset still clears the foreground Agent state.

A failed resume can coexist with a different live provider without overwriting
that provider's identity. After the live provider exits, the failed target is
available for an explicit retry and remains the fallback identity for a session
snapshot.

Claude forms and compaction can be recognized independently of lower task
chrome, while scrolling cannot reclassify the current turn. Unhooked sessions
can still remain uncertain when the visible evidence is incomplete.

## Validation

Focused Windows test groups passed:

```text
screen_context::tests              11/11
view::activity_tests               33/33
ai_hook::lifecycle::tests          36/36
view::auto_continue::tests         16/16
view::startup_tests                42/42
view::agent_identity tests         14/14
platform::wsl_hooks::channel         8/8
isolated WSL PTY/helper/owner proof  1/1
```

The live snapshot is
`F:/ProjectsWork/pebrel/tmp/agent-scroll-state-20261018/live-aligned-20261019.json`.
Compilation and headless GPUI tests do not prove the installed application or
multiple real providers converge correctly.

## Supersedes

None. This extends the ownership rules in
[`2026-10-17-agent-state-evidence.md`](2026-10-17-agent-state-evidence.md).

## Revisit when

Providers expose ordered shell/provider generation tokens or an authenticated
current-view identity that removes the need to correlate shell edges, process
probes and terminal structure.
