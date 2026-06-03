// Windows custom title-bar helpers. The Windows window is frameless
// (`decorations: false`, tauri.windows.conf.json) so the app draws its own
// minimize / maximize / close controls into the unified top nav, mirroring the
// macOS Overlay traffic lights (ADR 0006). These wrap the Tauri window API and
// the native Snap-Layouts bridge implemented in `src-tauri/src/window_chrome.rs`.
import { getCurrentWindow } from "@tauri-apps/api/window";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export async function minimizeWindow(): Promise<void> {
  await getCurrentWindow().minimize();
}

export async function toggleMaximizeWindow(): Promise<void> {
  await getCurrentWindow().toggleMaximize();
}

export async function closeWindow(): Promise<void> {
  // Mirrors the native close box: the app intercepts CloseRequested and hides
  // to the tray rather than quitting (see lib.rs on_window_event), so the
  // daemon + menu-bar presence survive (ADR 0003).
  await getCurrentWindow().close();
}

/// Subscribe to maximize/restore transitions so the button can swap its glyph
/// (single square ⇄ stacked squares). Fires once with the current state, then on
/// every resize. Returns an unsubscribe function.
export function watchMaximized(onChange: (maximized: boolean) => void): () => void {
  const win = getCurrentWindow();
  let unlisten: (() => void) | undefined;
  let disposed = false;
  const sync = () => {
    void win.isMaximized().then((m) => {
      if (!disposed) onChange(m);
    });
  };
  sync();
  void win.onResized(sync).then((u) => {
    if (disposed) u();
    else unlisten = u;
  });
  return () => {
    disposed = true;
    unlisten?.();
  };
}

/// Report the maximize button's on-screen rectangle (physical pixels, relative
/// to the webview origin) so the native side can park its transparent hit-test
/// overlay exactly over the button — that overlay is what makes Windows 11 show
/// its Snap Layouts flyout on hover.
export async function reportMaxButtonRect(el: HTMLElement): Promise<void> {
  const r = el.getBoundingClientRect();
  const dpr = window.devicePixelRatio || 1;
  await invoke("set_max_button_rect", {
    left: Math.round(r.left * dpr),
    top: Math.round(r.top * dpr),
    right: Math.round(r.right * dpr),
    bottom: Math.round(r.bottom * dpr),
  });
}

/// The maximize button is covered by a transparent native overlay (for Snap
/// Layouts), so the webview never sees `:hover` or clicks over it. The native
/// side forwards both instead: `onMaxButtonHover` drives the highlight and
/// `onMaxButtonClick` toggles maximize. Each returns an unsubscribe function.
export function onMaxButtonHover(onChange: (hovered: boolean) => void): () => void {
  return subscribe<boolean>("hitch-max-button-hover", onChange);
}

export function onMaxButtonClick(onClick: () => void): () => void {
  return subscribe<boolean>("hitch-max-button-click", () => onClick());
}

function subscribe<T>(event: string, onEvent: (payload: T) => void): () => void {
  let unlisten: (() => void) | undefined;
  let disposed = false;
  void listen<T>(event, (e) => onEvent(e.payload)).then((u) => {
    if (disposed) u();
    else unlisten = u;
  });
  return () => {
    disposed = true;
    unlisten?.();
  };
}
