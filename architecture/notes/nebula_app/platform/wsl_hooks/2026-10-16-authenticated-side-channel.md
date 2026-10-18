# WSL Hook delivery outside the terminal stream

## Status

Implemented candidate. Local transport, ownership regressions and real WSL
Claude/Codex CLI acceptance with a fixture provider pass. Live providers and
daily installation remain unverified. No installed configuration is migrated here.

## Context

Claude and Codex repaint their terminal while hooks run. A lock shared by hook
writers cannot serialize those writes with a provider's TUI. A long OSC can be
interrupted by a redraw and expose encoded content instead of delivering an event.

## Evidence

Isolated PTY and ConPTY probes reproduced long OSC corruption with concurrent
CSI redraws. The same payloads passed intact through a dedicated Windows pipe.
The historical incident has no raw terminal capture, so its individual byte
boundaries are not claimed as proven.

WSL's configured default UID can differ from the user actually chosen by its
login configuration. Looking up the mutable default again cannot establish the
owner of an existing pane.

## Decision

The PTY session owns a cancellable pipe and a fresh Hook-only token. Establish
the endpoint before spawning the PTY. Freeze the distribution, then verify the
unique credential-bearing PTY root from kernel evidence. Freeze its UID, process
epoch, terminal and session; verify every provider against that root. An explicit
launch user must also match. Root only reads this evidence with isolated system
Python; no user-selected interpreter or startup file is executed as root.

The bridge sends credentials and the existing envelope only on helper stdin.
The helper relays a bounded length-prefixed message and waits for authentication
and queue acknowledgement. A client receipt makes disconnection safe without an
unbounded pipe flush. Sequence locking and helper execution share one deadline.
An abandoned client does not retire the endpoint; permanent accept errors do.

Authenticated events use a private application event type and PTY generation.
The receiver rejects stale generations before domain parsing. Terminal OSC input
cannot claim that type; WSL Claude/Codex OSC is rejected while the channel exists.
Domain session, prompt, ordering and lifecycle gates still apply. Linux process
identities never become Windows provider/client PIDs. The npm Codex Node launcher
is allowed only immediately above its native child; outer Agents remain rejected.

Codex 0.162.1 can display an HTTP 500 error and record `task_complete.error`
without emitting Stop or Interrupt. Screen error chrome may wake a bounded
background probe, but cannot decide the outcome. Revalidate the frozen owner and
read only its open regular rollout, owned by the same UID. Require one matching
main CLI session, an exact current turn and root-turn ID, and a complete error
record in a bounded tail. Do not inspect recent files or guess from cwd. Recheck
the PTY generation and domain target before applying Failed once; no synthetic
Hook ordering sequence or success result is created. Retire late completion,
tool and attention events for that failed turn, including during local resubmission.
Real Codex metadata occupied 21,936 bytes; an 8 KiB first-line limit silently
rejected that valid session. Bound metadata at 64 KiB, matching existing session
discovery, with both a long valid record and an oversized negative regression.

## Rejected alternatives

- Shorter OSC or more hook locks: neither controls provider redraws.
- Reusing the Runtime token: would grant unrelated application capabilities.
- An authenticated-looking envelope header: a terminal writer can copy it.
- Guessing the current default user or recent session: does not prove ownership.
- Silent terminal fallback: reintroduces the original corruption path.
- Screen-only failure overrides: old terminal errors can remain during a new turn.

## Consequences

Each WSL PTY has an owned worker and bounded kernel probes on hook delivery.
Shutdown, process exit and session drop revoke the channel. Moving the same session
between windows preserves its channel. Acknowledgement proves queue acceptance,
not application of a lifecycle event. Missing end events are not successful work.
Failure probing has a two-second deadline and cadence, runs only on error chrome
in an active native Codex turn, and shares PTY cancellation. It reads at most one
bounded metadata line and 64 KiB of tail per candidate. A changed rollout schema,
ambiguous handles or missing evidence leaves the existing state intact.

## Validation

Focused Python/Rust regressions cover input bounds, concurrent delivery, foreign
credentials, process epochs, nested providers, stale generations, cancellation
and pipe disconnection. The opt-in live test exercises a real WSL PTY, production
bridge/helper, owner proof and long content during redraws. Its fixture provider
does not replace acceptance with the actual Claude/Codex CLIs or product UI.
Real Codex 0.162.1 resume retained its native session ID. An HTTP 500 without Stop
became Failed from verified active-rollout evidence; a new continue stayed Working
while the old error remained, then finished from its own Stop. Two real instances
retained distinct session IDs, pipes and credentials with independent concurrent
states. Foreign credentials, foreign owners and stale process epochs were rejected
without changing the target pane's state or sequence.

## Supersedes

None. Other POSIX providers and SSH retain their existing transport contracts.

## Revisit when

The provider offers an authenticated lifecycle transport, WSL kernel ownership
evidence changes, or measurements show the bounded per-event probe is inadequate.
