# Agent identity, current-turn evidence and confirmation

## Status

Implemented in the local candidate; runtime acceptance is tracked separately.

## Context

A restored WSL CLI can miss shell command-start events. Codex also keeps its
composer visible while working. An old helper can exit successfully without
understanding the authenticated WSL relay. These facts cannot establish either
an active turn or its completion.

## Evidence

Focused regressions cover discovery without command-start, stale process owners,
same-kind owner ambiguity, ambiguous composers, unconfirmed waits, helper protocol
responses and relay acknowledgements. The process-owned rollout fixtures include
PID replacement, partial records, later turns and changing file-descriptor maps.

## Decision

Keep identity and lifecycle separate. WSL discovery returns a revocable process
receipt, rechecked before delivery and bound to the pane's PTY, input and request
epochs. Cancellation does not free a worker's single-flight slot before it exits.

A verified foreground receipt also restores screen observation after a shell-end
boundary. It does not clear that boundary's rejection of delayed old Hooks or
assert that the new process is working. Real no-Hook execution and a focused
regression caught identity being restored while observation stayed disabled.

The existing shared lifecycle retains its last status and records uncertainty
when current evidence is insufficient. Runtime reads, readiness, waits and tab
badges consume that same confirmation. Unknown evidence never satisfies a wait
or permits an automatic completion callback.

Only an already authenticated Codex owner/session/turn can supply a bounded
structured terminal record. Ordinary finish/cancellation settles to Idle without
success notifications. A separate retired-turn marker rejects late nonterminal
events while preserving a later authoritative terminal Hook.

Helper capability, authenticated relay acceptance and lifecycle acceptance remain
distinct observations. The private ACK is consumed inside the bridge. Selecting
the authenticated WSL channel remains fail closed even when its receiver fails.

Export the selected helper before starting the existing background configuration
guard. An isolated startup caught the reverse order inspecting an absent export
and reporting an uninspected pane path until a later refresh.

## Rejected alternatives

- Empty composer samples, process existence or quiet time as completion evidence.
- Replacing a helper once without checking its capability at use and packaging.
- UI-only uncertainty while waits still consume an apparently completed status.
- Disabling owner/session guards or falling back to unauthenticated OSC.

## Consequences

An unbound or unreadable session may remain explicitly unconfirmed. The repair
does not invent missing provider events or bind a session by cwd/latest filename.
Native Hook compatibility and Claude's daemon-view identity contract remain
outside the WSL receipt's guarantees.

## Validation

See the owning lifecycle, screen, Runtime, helper and owner/bridge regression
tests. Builds and fixture tests do not establish real provider or visible UI
acceptance; those require isolated product execution.

## Supersedes

None.

## Revisit when

Providers expose an authenticated current-turn query or a stable foreground-view
identity contract that can replace private rollout evidence.
