# Codex WSL Hook Delivery Without a Controlling TTY

## Status
Implemented and targeted tests pass. The installed WSL asset and a live Codex
session have not yet been revalidated. The Windows hook timeout is now eight
seconds; its actual slow stage was not observed in this session.

## Context
Codex native hooks can run in a detached process without a controlling
terminal. The WSL bridge previously delivered OSC only through `/dev/tty`,
except for Claude's documented `terminalSequence` stdout fallback. A detached
Codex hook therefore lost lifecycle events even when Hooks were enabled and
registered. Separately, the Codex command deadline was three seconds while the
native forwarding helper permits two seconds for delivery.

## Evidence
- WSL Codex 0.162.1 reports `hooks stable true`; its config does not explicitly
  disable Hooks, and `hooks.json` registers all eight managed events.
- The installed WSL `bridge.py` hash matched the pre-fix source. The prior
  implementation re-raised Codex's `/dev/tty` `ENXIO`, which the bridge's
  fail-open entry point swallowed.
- A regression fixture launches the Codex hook in a new session with captured
  stdout/stderr. Before the fix, its event produced no terminal frame.
- Windows `hooks.json` also uses the three-second timeout. A 90-second
  read-only process observation saw no active helper invocation, so it did not
  identify which Windows startup stage caused the reported timeout.
- Codex 0.162.1 command runner source: [command_runner.rs](https://github.com/openai/codex/blob/rust-v0.162.1/codex-rs/hooks/src/engine/command_runner.rs).

## Decision
- Only after `/dev/tty` fails with `ENXIO`, a native Codex event may use the
  matching Codex ancestor's stdout descriptor. The bridge rechecks the Linux
  process start time and requires a character-device PTY under `/dev/pts`.
- Never emit Codex OSC to the detached hook's captured stdout. Claude retains
  its separate documented `terminalSequence` fallback.
- Set the shared Codex command timeout to eight seconds. This leaves startup
  margin while `pebrel-hook` retains its independent two-second forwarding
  bound. User-edited hook groups remain protected by existing ownership checks.

## Rejected alternatives
- Printing a Claude-style `terminalSequence` for Codex: no verified Codex
  contract sends captured hook stdout to the terminal.
- Writing to an arbitrary ancestor descriptor or trusting a PID from payload:
  neither proves ownership of the current provider PTY.
- Extending the helper's forwarding deadline: this would increase transport
  blocking without addressing the outer shell/startup budget.

## Consequences
WSL Codex events can reach the owning terminal despite the detached hook
runner, without weakening pane/session identity. A rebuilt Pebrel must refresh
the managed WSL bridge asset before this source fix is active there. A fresh
provider session and `hook_seen=true` are still required before claiming that
automatic continuation works end to end.

## Validation
- `python3 -m unittest scripts.tests.test_remote_hooks`: 15 passed. The new
  PTY fixture verifies a detached Codex hook emits one OSC frame to its owning
  PTY and writes nothing to captured stdout/stderr.
- Targeted Windows GPUI test verified the eight-second Codex deadline; existing
  local install/upgrade and WSL install tests passed.
- No live provider invocation, post-build WSL reconciliation, or actual
  automatic `continue` dispatch was performed.

## Supersedes
None as a whole. This narrows the prior Claude-only no-TTY fallback without
changing its behavior; the rejection of arbitrary stderr delivery remains.

## Revisit when
Codex offers a supported detached-hook-to-terminal delivery channel, or its
runner again provides a controlling TTY.
