# Long-running daemon operations run as async Jobs off the request loop

The **Daemon** handles each client connection with one serial request loop
(`handle_client`): it reads a request, runs it to completion inline, then reads
the next. The GUI uses a single connection, so a long operation — `git push`,
`pull`, `fetch`, `clone`, or a **Draft Generator** run (up to ~120 s) — blocks
every subsequent request on that connection behind it, including PTY input.

To fix this, long-running operations become **Jobs**: a request dispatches the
work onto a worker, returns a job id immediately so the request loop is never
blocked, and the worker reports progress/completion as broadcast events the GUI
tracks by job id. Jobs are cancellable (`CancelJob` signals the worker to kill
its git/agent child) and ephemeral — they live in daemon memory only and a Job
that was *running* when the daemon stops is reported *failed* on restart, not
resumed. Fast git reads (`status`, `diff`) stay synchronous and are NOT Jobs.

## Considered Options

- **Async Jobs off the request loop** (chosen) — the handler spawns a worker and
  returns a job id; lifecycle (queued/running/succeeded/failed/cancelled) and
  progress travel on the existing broadcast event bus, keyed by job id. Adds
  `StartJob`/`CancelJob` + job events to the protocol. Non-blocking, cancellable,
  observable; decouples the **Draft Generator** from the client's 120 s response
  deadline. Stays on the single per-GUI connection (no connection split needed).
- **Keep everything synchronous** — simplest, no protocol change; rejected
  because a single slow op stalls the whole connection (the observed defect).
- **Split the GUI into multiple connections** (one for slow ops) — unblocks the
  fast path without a Job model, but multiplies the connection/reconnect surface
  and still gives no cancellation, progress, or status; rejected.
- **Persist Jobs to the store and resume across restart** — rejected: git pushes
  and headless runs don't resume cleanly, and reconciling half-applied state is
  more risk than re-triggering. Jobs are deliberately ephemeral.

## Consequences

- New protocol messages (`StartJob`/`CancelJob` and job lifecycle/progress
  events) join the control plane; the binary PTY data plane (ADR 0007) is
  untouched.
- The **Draft Generator** is reclassified as a Job kind; its run no longer has to
  be clamped below the client's synchronous response deadline.
- A worker that panics is isolated (caught) and surfaces as a *failed* Job rather
  than taking down the daemon.
- Jobs are surfaced as quiet progress in the UI (design principle #2); a Job is
  internal async plumbing, NOT the rejected user-facing "Task" tree work-item
  (see `CONTEXT.md`).
- A future Job kind — a headless **Agent** run (run an Agent non-interactively to
  completion) — slots into the same machinery without further protocol changes.
