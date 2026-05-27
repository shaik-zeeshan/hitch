# PTY output uses a per-session binary IPC channel, not the event bus

To keep the terminal fluid under heavy output, PTY bytes stream from the Tauri
layer to the webview over a per-**Session** binary `Channel<&[u8]>`, while
control-plane notifications (session/worktree/agent state) stay on the
`emit`/`listen` event bus. The **Daemon** remains the durable owner of bounded
scrollback ([0003](0003-session-daemon.md)); the GUI keeps only a bounded byte
ring for repaint and keeps live terminals mounted per active parent rather than
tearing them down on every tab/diff switch.

## Considered Options

- **Per-session binary `Channel<&[u8]>`** (chosen) — Tauri v2's purpose-built
  streaming primitive; carries raw bytes point-to-point without JSON framing.
  One channel per Session, opened on `session-opened`, closed on
  `session-closed`; long-lived in the Tauri process so it survives daemon
  reconnects.
- **base64 over the existing event** — smallest diff, but taxes the hot path
  twice (encode/decode) and inflates every frame ~33%; rejected because this
  branch is dedicated to terminal quality, not minimal change.
- **Raw `Vec<u8>` over events** — Tauri serializes it as a JSON array of
  integers (~6× blowup); rejected outright.
- **Webview → localhost WS/TCP straight to the daemon** — bypasses the Tauri
  relay entirely, but breaks the deliberate Unix-socket-local boundary of the
  daemon (ADR 0003); rejected.

## Consequences

- Bytes are never stringified in Rust. xterm's own streaming UTF-8 decoder
  handles frames, fixing the `from_utf8_lossy`-per-frame corruption that turned
  any multi-byte glyph split across an 8 KB read boundary into `�`.
- The GUI's working buffer holds bytes (`Uint8Array`) in a bounded ring, not an
  unbounded string. The daemon stays authoritative for scrollback across
  reconnect and app restart; the GUI copy exists only to repaint a (re)mounted
  terminal.
- Live terminals are kept mounted per active parent and written to in
  rAF-batched flushes, so switching tabs or peeking at a diff no longer tears
  down the xterm instance and replays its whole buffer.
- The event bus keeps its job — the control plane — and is no longer abused as
  a high-rate per-session data plane.
