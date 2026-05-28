# Session output staging lives on both sides of the IPC seam, by design

Hitch keeps two separate Session-output staging modules — `OutputBroadcaster` in the **Daemon** and `WebviewChannelRegistry` in the **Tauri** client — and intentionally does not unify them behind a single named interface. The two look superficially alike (both buffer per-Session bytes until the next consumer is ready), but they solve different problems on opposite sides of the Tauri IPC seam, and merging them would couple Daemon broadcast latency to the slowest webview.

## What each module owns

- **`OutputBroadcaster` (Daemon, `hitch-daemon/src/main.rs`)** — owns the *replay-snapshot-vs-live-broadcast* FIFO. The dispatcher thread appends to a per-Session live log on the same thread that broadcasts to clients and that builds a reconnecting client's replay snapshot, so a replay and the live stream that follows it cannot interleave. This is the documented fix for the in-flight gap left by snapshotting `pty.scrollback()` at a different pipeline stage from the broadcast point (see [0007](0007-pty-output-channel.md)).
- **`WebviewChannelRegistry` (Tauri client, `apps/desktop/src-tauri/src/lib.rs`)** — owns the *channel-registration round-trip*. Tauri's binary `Channel<&[u8]>` is created in JS and handed to the Rust side by an `invoke('register_session_output')` call; until that invoke completes there is no consumer on the Rust side for the Daemon's bytes. The registry stages bytes during that gap, distinguishes brand-new Sessions (keep pre-`SessionOpened` staging) from reconnects (drop staging, replay is coming), and re-stages on a stale channel.

The contract between them is the existing wire protocol from [0007](0007-pty-output-channel.md): the Daemon-side `SessionOpened` event plus length-prefixed raw byte frames per Session.

## Considered Options

- **Two staging modules, asymmetric by design** (chosen) — Daemon owns the dispatcher-thread FIFO; Tauri client owns the IPC registration gap. Each is small, has a single responsibility, and is unit-tested without process boundaries.
- **Unify behind a `SessionOutputStream` interface with an explicit replay marker** — add `Event::SessionReplayBegins`/`SessionReplayComplete` to `hitch-proto` so the client no longer infers reconnect from a second `SessionOpened` for the same id, and so the registry's role shrinks to a `HashMap<SessionId, Channel>` with no `opened_sessions` heuristic. Rejected: the staging on the client side does not go away — the registration round-trip is still there, since `Channel` handles cannot exist on the Rust side before the `invoke` lands. Naming the seam without removing the asymmetric work would not earn back the protocol churn.
- **Move all staging into the Daemon (ACK-on-registration)** — the Daemon withholds live bytes for a Session until each client ACKs that its channel is registered. Rejected: one slow webview stalls the Daemon's broadcast for that Session for every other client, and Daemon memory grows with client count × Session count. The current model isolates each client's gap to its own process.
- **Move all staging into the webview (JS)** — drop the Tauri-side staging and let xterm-side JS reorder. Rejected: the gap is *before* the webview's JS code can see the bytes — the Tauri process literally has no `Channel` to forward through during the round-trip.

## Consequences

- The two modules are deliberately *not* peers and do not share an interface. The Rust-side naming (`OutputBroadcaster` vs `WebviewChannelRegistry`) makes the asymmetry visible at the call site; a future review that proposes merging them should land on this ADR first.
- Non-Tauri clients (CLI inspector, headless integration tests against the Daemon socket) need only the Daemon-side guarantees from `OutputBroadcaster` — they do not inherit the registry, because they read the socket directly with no `invoke` indirection.
- A future Tauri version that exposes binary `Channel` handles synchronously on Session open (no `invoke` round-trip) would let us delete `WebviewChannelRegistry`. Until then it stays, and Tauri-version changes are the only signal that should trigger reconsidering this decision.
