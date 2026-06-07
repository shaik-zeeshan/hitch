// Native OS notifications for LIVE agent-state transitions (ADR 0011). Hitch
// agents report state through hooks; the daemon broadcasts `agent-state`
// events, applied in daemon.ts. This module is the single place that decides
// whether a transition warrants an OS notification, formats its copy, and fires
// it — daemon.ts calls `noteAgentState` once from its state-application path and
// owns nothing of the notify logic itself.
//
// Two hard rules from the spec:
//   1. Notify only on an actual state CHANGE (prev ≠ next), never on a repeated
//      same-state report.
//   2. NEVER notify from REPLAYED state. State arrives both live (the
//      `agent-state` event) and via `SessionOpened` replay on attach/reconnect,
//      so a window catching up must SET THE BASELINE (the prev-state map lives
//      in daemon.ts's `agentStates`) WITHOUT notifying. The `replay` flag on the
//      call distinguishes the two paths; replay only primes the turn-start map.
import { get } from "svelte/store";
import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";
import {
  notificationMinTurnSeconds,
  notificationMode,
  type NotificationMode,
} from "./settings";
import { agentDisplayName } from "./sessionDisplay";
import type { AgentState, Id, KnownAgent } from "./types";

// Per-session monotonic timestamp (ms) of when the current turn started — i.e.
// when the session most recently ENTERED the running/needs-approval pair from
// outside it. Preserved across `running` ↔ `needs-approval` round-trips so a
// long task that pauses for one approval and finishes 2s after still measures
// the whole turn, and cleared when the session leaves the pair (waiting / error
// / null). Read only to gate the "finished" notification against
// `notificationMinTurnSeconds`. Owned here; daemon.ts never touches it.
const turnStartAt = new Map<Id, number>();

// The OS permission decision is cached after the first resolve so each send
// doesn't await an IPC round-trip. `null` = not yet checked; `false` = denied
// (every send silently no-ops); `true` = granted.
let permissionGranted: boolean | null = null;

// Whether a session is currently in the running/needs-approval pair — the turn
// is "in flight". A `waiting`/`error`/`null` next-state has left the pair.
function inTurnPair(state: AgentState | null): boolean {
  return state === "running" || state === "needs-approval";
}

// Maintain the per-session turn-start clock for one transition. Runs for BOTH
// live and replayed transitions so the baseline is correct on attach: a window
// that joins mid-turn must know the turn is in flight (it just can't date it,
// hence Date.now() — a replayed in-flight turn is treated as having started
// "now", which only ever shortens a measured turn, never invents a long one).
function trackTurn(
  sessionId: Id,
  prevState: AgentState | null,
  nextState: AgentState | null,
): void {
  if (inTurnPair(nextState)) {
    // Entering the pair from OUTSIDE it starts a new turn; staying within it
    // (running ↔ needs-approval) preserves the existing start.
    if (!inTurnPair(prevState) && !turnStartAt.has(sessionId)) {
      turnStartAt.set(sessionId, Date.now());
    }
  } else {
    // Left the pair (waiting / error / cleared): the turn is over.
    turnStartAt.delete(sessionId);
  }
}

// Drop all per-session notification state. Called by daemon.ts when a session
// closes or its state clears to null, so the maps don't leak across the app's
// lifetime as sessions come and go.
export function forgetSession(sessionId: Id): void {
  turnStartAt.delete(sessionId);
}

type NotifyCopy = { title: string; body: string | null };

// The notification copy for a notifying transition, or `null` when the
// transition does not warrant one. `agentName`/`body` are resolved by the
// caller (daemon.ts owns the store reads for session → worktree → project), so
// this stays a pure function easy to unit-test. Exported for the unit tests.
export function copyForTransition(
  prevState: AgentState | null,
  nextState: AgentState,
  agentName: string,
  body: string | null,
  detail: string | null,
  minTurnSeconds: number,
  turnStartedAt: number | undefined,
  now: number,
): NotifyCopy | null {
  if (nextState === "needs-approval") {
    return { title: `${agentName} needs approval`, body };
  }
  if (nextState === "error") {
    // Append the agent-state event's detail (an error reason) when present.
    const trimmed = detail?.trim();
    const errorBody = trimmed ? appendDetail(body, trimmed) : body;
    return { title: `${agentName} hit an error`, body: errorBody };
  }
  if (nextState === "waiting" && prevState === "running") {
    // Turn end. Gate on the turn having run at least `minTurnSeconds`; 0 means
    // ungated. A missing start (never observed the turn begin) is treated as
    // ungated rather than suppressed — better to notify than to swallow.
    if (minTurnSeconds > 0 && turnStartedAt !== undefined) {
      const elapsedSeconds = (now - turnStartedAt) / 1000;
      if (elapsedSeconds < minTurnSeconds) return null;
    }
    return { title: `${agentName} finished`, body };
  }
  return null;
}

// Append an error detail to the base body as ` — <detail>`, or surface the
// detail alone when there is no base body to hang it off.
function appendDetail(body: string | null, detail: string): string {
  return body ? `${body} — ${detail}` : detail;
}

// Whether the suppression policy lets a transition for `sessionId` notify right
// now. `windowFocused`/`activeSessionId` are passed in (daemon.ts owns the
// focus + active-session reads) so this stays pure and testable.
export function shouldNotify(
  mode: NotificationMode,
  sessionId: Id,
  windowFocused: boolean,
  activeSessionId: Id | null,
): boolean {
  if (mode === "off") return false;
  if (mode === "app-in-background") return !windowFocused;
  // "background-or-other-session": notify unless the window is focused AND the
  // session that changed is the one the user is already looking at.
  return !(windowFocused && sessionId === activeSessionId);
}

// Best-effort window focus at fire time. `document.hasFocus()` is accepted by
// the spec; guard for SSR/test environments where `document` is absent.
function windowIsFocused(): boolean {
  return typeof document !== "undefined" ? document.hasFocus() : false;
}

// Note one agent-state transition. The ONLY entry point daemon.ts calls.
// `prevState` is the session's stored state BEFORE this report (from
// `agentStates`); `nextState` is the new one. `replay: true` means the
// transition came from `SessionOpened` replay on attach/reconnect — it primes
// the turn-start baseline but NEVER notifies.
export function noteAgentState(
  sessionId: Id,
  prevState: AgentState | null,
  nextState: AgentState | null,
  detail: string | null,
  context: {
    replay: boolean;
    agent: KnownAgent | null;
    body: string | null;
    activeSessionId: Id | null;
  },
): void {
  // A repeated same-state report is not a transition (and so neither tracks a
  // turn boundary nor notifies). Bail before touching the turn map.
  if (prevState === nextState) return;

  // Capture the turn's start BEFORE `trackTurn` mutates the map: a "finished"
  // transition (running → waiting) leaves the pair, so trackTurn clears the
  // start — we must read it first to gate against the turn's true duration.
  const turnStartedAt = turnStartAt.get(sessionId);
  trackTurn(sessionId, prevState, nextState);

  // Replayed state only sets the baseline; the live path is the sole notifier.
  if (context.replay) return;
  if (nextState === null) return;

  const mode = get(notificationMode);
  if (mode === "off") return;
  if (!shouldNotify(mode, sessionId, windowIsFocused(), context.activeSessionId)) return;

  const copy = copyForTransition(
    prevState,
    nextState,
    agentDisplayName(context.agent),
    context.body,
    detail,
    get(notificationMinTurnSeconds),
    turnStartedAt,
    Date.now(),
  );
  if (!copy) return;

  void fire(copy.title, copy.body);
}

// Send the notification, lazily resolving (and caching) OS permission. A denied
// or unresolved-then-denied permission silently no-ops. Default OS sound on all
// notifications ("default" maps to the system sound via notify-rust on macOS).
//
// IMPORTANT — desktop permission is a STUB: tauri-plugin-notification's desktop
// backend hard-codes `permission_state`/`request_permission` to `Granted`
// (desktop.rs), so on macOS/Linux/Windows `isPermissionGranted()` resolves true
// and `requestPermission()` is a no-op that shows NO dialog. Delivery (and the
// only macOS authorization prompt that ever appears) happens when `notify` is
// actually called: notify-rust → mac-notification-sys → NSUserNotificationCenter,
// keyed by the app's bundle identifier. The permission gate below is therefore
// portable structure for mobile (where it is real) and on desktop never blocks a
// send — it just avoids a needless IPC round-trip per fire.
async function fire(title: string, body: string | null): Promise<void> {
  try {
    if (permissionGranted === null) {
      permissionGranted = (await isPermissionGranted()) || (await requestPermission()) === "granted";
    }
    if (!permissionGranted) return;
    sendNotification({ title, body: body ?? undefined, sound: "default" });
  } catch {
    // Notifications are best-effort; a plugin/permission failure must never
    // surface to the user or break the state-application path that called us.
  }
}

// Warm the cached permission decision once at startup when notifications are
// enabled, so the first `fire()` doesn't pay an IPC round-trip. Note this does
// NOT pop a macOS prompt: desktop `request_permission` is the always-`Granted`
// stub described on `fire` above. The macOS authorization prompt (and actual
// delivery) only happens the first time a notification is POSTED, and only when
// the running app is a Launch-Services-registered bundle under its identifier —
// i.e. an installed .app, not `tauri dev`. A no-op when the mode is "off". Safe
// to call repeatedly — the cached decision short-circuits after the first resolve.
export async function primeNotificationPermission(): Promise<void> {
  if (get(notificationMode) === "off") return;
  if (permissionGranted !== null) return;
  try {
    permissionGranted = (await isPermissionGranted()) || (await requestPermission()) === "granted";
  } catch {
    permissionGranted = false;
  }
}
