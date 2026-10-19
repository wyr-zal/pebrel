# History viewport and current-turn screen evidence

## Status

Implemented in the local 2.2.9 candidate; daily runtime acceptance is pending.

## Context

An Agent can redraw conversation history into the terminal's live grid. Reading
grid rows independently of the terminal scrollback offset therefore does not
prove that those rows describe the current turn. Claude's history view can keep
the composer visible while hiding current activity.

## Evidence

- The initial `agent_scroll` regressions reproduced Working becoming Idle after
  a Claude return-to-bottom banner appeared above the composer.
- The installed WSL pane also had two shell roots with inherited pane identity.
  The existing owner verifier rejected it; a directly launched WSL pane passed.
  This explains reliance on screen fallback, but does not authorize either root.
- Screen error badges were recalculated from visible text without a turn boundary.
  The completion regression reproduced a warning surviving successful completion.

## Decision

- Keep the existing authenticated WSL owner and unique-root checks. Restore the
  affected conversation through a directly launched WSL pane.
- Share one bounded Claude history-chrome check among state detection, screen
  errors and retry safety. Recognize the paired return-to-bottom label and
  Ctrl+End control, including new-message and wrapped forms. Exclude quoted code,
  drafts and labels separated from a boxed composer by ordinary answer text.
- A history snapshot does not change the last activity observation. Revoke only
  a screen-derived retry; preserve its consumed error and prompt baseline.
- A positive terminal scrollback offset freezes the observation. A zero offset
  alone is not proof of the live view because applications can redraw history
  into the active grid.
- If neither provider's current composer can be found, preserve the last Agent
  and activity state rather than treating transient redraw fragments as a turn.
- Accepted Hook failures wait while history chrome is visible. Each timer checks
  the original pane, session, input epoch and settings before deferring; input is
  sent only after the live screen again passes the existing safety checks.
- Track the screen error at prompt submission independently of retry enablement.
  Successful completion clears the warning; existing errors do not become a new
  turn's warning. Confirmed failure remains visible.

## Rejected alternatives

- Treating offset zero as proof of the live view: application-managed history
  already occupies the live grid, so a zero offset only permits further checks;
  it does not establish current-turn evidence by itself.
- Treating a scroll or Ctrl+End key as a completed viewport transition: input
  acceptance does not prove that the application has returned to its live view.
- Selecting either inherited WSL root: the interop relationship lacks verified
  pane binding, so this would weaken ownership and routing checks.
- Reusing the retry enablement latch for presentation: disabling retries cancels
  that latch, while error presentation must still follow the accepted turn.

## Consequences

The history guard combines terminal position, recognized Claude chrome and the
presence of a current composer. Unknown or changed CLI layouts remain an
explicit coverage limit. An answer that precisely reproduces the same controls
in the same screen position cannot be distinguished by text alone. A valid Hook
remains the preferred lifecycle source.

## Validation

The focused `screen_context` 11/11, view activity 31/31 and auto-continue 16/16
regressions pass, together with the GPUI product check and architecture checker.
An unchanged settings-page click test failed independently and remains outside
this fix. Real provider, daily installation and original-session recovery are
not covered by these results.

## Supersedes

None.

## Revisit when

Claude changes history chrome, a provider exposes an authenticated viewport
signal, or nested WSL receives a verified cross-interop pane-binding contract.
