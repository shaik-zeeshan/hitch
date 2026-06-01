# Forcing a full-screen app to repaint is a Daemon responsibility, not a GUI replay

A full-screen ("alt-screen") app like Claude Code paints a cursor-addressed
frame that is valid only at the grid size it was drawn for. That frame cannot be
reconstructed by replaying or reflowing bytes — neither the GUI's byte ring nor
the **Daemon**'s scrollback ([0007](0007-pty-output-channel.md),
[0003](0003-session-daemon.md)) can rebuild it, because the byte history encodes
motion relative to grids that have since changed. The only thing that produces a
correct frame is the app *re-emitting* it. So when the grid drifts under a hidden
terminal and no repaint follows, the garble is permanent (typing can't heal it —
typing is not a repaint).

We make "force the child to repaint" a real, reliable, **Daemon**-owned action.
The Daemon owns the PTY and can resolve the child's process group, so on a GUI
(re)activation it re-applies the terminal size and signals the child
(`SIGWINCH`) **regardless of whether the size changed**, forcing a full,
correctly-sized frame that overwrites any locally-reflowed garbage. The
GUI→Daemon resize is also made lossless: the final settled size always lands,
rather than being a best-effort, error-swallowed notify.

## Considered Options

- **Daemon forces the repaint** (chosen) — the Daemon is already the sole owner
  of the PTY, so it is the only place that can reliably make the child redraw.
  Decoupling the repaint signal from an actual size change closes the
  same-size-no-`SIGWINCH` gap, and because the repaint is a fresh full frame it
  heals corruption no matter how it arose (reflow-on-activate *or* the reconnect
  scrollback replay). Cost: the PTY contract gains a "repaint" verb and the GUI
  must announce (re)activation.
- **GUI-only freeze + serialize** — never reflow xterm while the app owns the
  alt-screen, and snapshot screen *state* (xterm `SerializeAddon`) instead of raw
  bytes. More GUI code, and it does nothing for the Daemon's reconnect replay
  path (app relaunch), where the xterm is genuinely rebuilt. Rejected: it fixes
  one path and leaves the other.
- **Minimal patch** — stop swallowing the resize error and always re-send the
  size on activate. Cheap, but a same-size re-fit still emits no `SIGWINCH` and
  the reconnect replay is still raw bytes, so corruption keeps happening "just
  less often." Rejected because #2 was specified as must-never-happen.

## Consequences

- The Daemon exposes a way to make a session's child redraw (re-apply size +
  signal the process group), invoked on GUI (re)activation and after a resize
  settles. A `SIGWINCH` to a shell at its prompt is harmless (readline redraws
  in place), so the signal can be sent unconditionally without sniffing for
  alt-screen state.
- A brief flash of the stale/garbled frame may remain visible for the one round
  trip until the app's repaint arrives; the *final* state is always correct,
  which is the property #2 demanded.
- This does not change the ownership model — scrollback stays the Daemon's, the
  GUI ring stays the repaint copy ([0007](0007-pty-output-channel.md)). It adds
  a repaint trigger, it does not move authority.
