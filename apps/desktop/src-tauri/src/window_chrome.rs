//! Windows custom title-bar chrome.
//!
//! The Windows window is frameless (`decorations: false`, see
//! `tauri.windows.conf.json`) so the app draws its own caption controls into the
//! unified top nav, matching the macOS Overlay title bar (ADR 0006). A frameless
//! window loses one affordance the HTML buttons cannot reproduce: the Windows 11
//! **Snap Layouts** flyout that the OS shows when the pointer hovers the real
//! maximize button. That flyout is driven purely by a window reporting
//! `HTMAXBUTTON` from `WM_NCHITTEST`.
//!
//! Subclassing the *top-level* window proc does not work here: WebView2 hosts the
//! page in a child HWND that fills the client area and consumes its mouse input,
//! so the top-level proc never sees `WM_NCHITTEST` over the page (this is also
//! why Tauri's `data-tauri-drag-region` is a JS shim, not native). The working
//! technique — used by `tauri-plugin-frame` and others — is a small, transparent
//! **child window placed over the maximize button** whose own proc returns
//! `HTMAXBUTTON`. It sits above the WebView2 in z-order for hit-testing, but
//! because WebView2 composites through DWM (not GDI) the empty overlay stays
//! visually transparent — the HTML button still shows through. The overlay's
//! region is non-client, so it receives `WM_NC*` mouse messages, which we forward
//! to the frontend as `hitch-max-button-hover` (for the highlight) and
//! `hitch-max-button-click` (to toggle maximize), keeping Tauri's window state in
//! sync. The frontend reports the button's rectangle via `set_max_button_rect` so
//! the overlay tracks it across layout, resize, and DPI changes.
//!
//! macOS and Linux compile the public surface to no-ops.

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::mem::size_of;
    use std::ptr::{null, null_mut};
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
    use std::sync::{Mutex, OnceLock};

    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use tauri::{AppHandle, Emitter, Manager, WebviewWindow};
    use windows_sys::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::{GetStockObject, HBRUSH, NULL_BRUSH};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        TrackMouseEvent, TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT,
    };
    use windows_sys::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, RegisterClassExW, SetWindowPos, CS_HREDRAW, CS_VREDRAW,
        HTMAXBUTTON, HWND_TOP, SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE, SWP_SHOWWINDOW, WM_DPICHANGED,
        WM_NCHITTEST, WM_NCLBUTTONDOWN, WM_NCLBUTTONUP, WM_NCMOUSELEAVE, WM_NCMOUSEMOVE, WM_SIZE,
        WNDCLASSEXW, WS_CHILD, WS_CLIPSIBLINGS, WS_OVERLAPPED, WS_VISIBLE,
    };

    /// Maximize-button rectangle in *physical* pixels relative to the client
    /// (webview) origin — which, on a frameless window, is the parent's
    /// top-left, so it doubles as the child overlay's position.
    #[derive(Clone, Copy)]
    struct MaxButtonRect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    /// The latest rectangle reported by the frontend on layout/DPI changes.
    /// Written from the IPC thread, read from the Win32 message thread, so the
    /// four fields move as one unit behind a `Mutex` — per-field atomics let a
    /// reader observe a torn rect (new left, old right) and misposition the
    /// overlay for a frame. `None` until the frontend reports a rect.
    static MAX_BUTTON_RECT: Mutex<Option<MaxButtonRect>> = Mutex::new(None);
    /// The transparent hit-test overlay child window.
    static OVERLAY_HWND: AtomicIsize = AtomicIsize::new(0);
    /// Pointer hover / press state, tracked so we emit only on transitions.
    static HOVERING: AtomicBool = AtomicBool::new(false);
    static PRESSING: AtomicBool = AtomicBool::new(false);
    static APP: OnceLock<AppHandle> = OnceLock::new();

    /// Stable subclass id ('HITC') for the parent-resize tracker.
    const SUBCLASS_ID: usize = 0x4849_5443;
    /// UTF-16, null-terminated window class name for the overlay.
    const CLASS_NAME: [u16; 13] = [
        b'H' as u16,
        b'i' as u16,
        b't' as u16,
        b'c' as u16,
        b'h' as u16,
        b'M' as u16,
        b'a' as u16,
        b'x' as u16,
        b'S' as u16,
        b'n' as u16,
        b'a' as u16,
        b'p' as u16,
        0,
    ];

    pub fn set_max_button_rect(left: i32, top: i32, right: i32, bottom: i32) {
        if let Ok(mut rect) = MAX_BUTTON_RECT.lock() {
            *rect = Some(MaxButtonRect {
                left,
                top,
                right,
                bottom,
            });
        }
        update_overlay_position();
    }

    fn emit(event: &str, hover: bool) {
        if let Some(app) = APP.get() {
            let _ = app.emit(event, hover);
        }
    }

    /// Move the overlay onto the latest reported rectangle. Uses
    /// `SWP_ASYNCWINDOWPOS` so it is safe to call from the IPC thread (the
    /// request is posted to the window's message queue, not run cross-thread).
    fn update_overlay_position() {
        // Copy the rect out and drop the guard before any Win32 call — this
        // runs inside the parent wndproc, so a `SetWindowPos` here could
        // re-enter it; holding the lock across that would risk a deadlock.
        let Some(rect) = MAX_BUTTON_RECT.lock().ok().and_then(|r| *r) else {
            return;
        };
        let overlay = OVERLAY_HWND.load(Ordering::Relaxed);
        if overlay == 0 {
            return;
        }
        let left = rect.left;
        let top = rect.top;
        let width = (rect.right - left).max(0);
        let height = (rect.bottom - top).max(0);
        // SAFETY: `overlay` is our live child HWND; flags are valid.
        unsafe {
            SetWindowPos(
                overlay as HWND,
                HWND_TOP,
                left,
                top,
                width,
                height,
                SWP_ASYNCWINDOWPOS | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
    }

    unsafe fn module_instance() -> HINSTANCE {
        GetModuleHandleW(null()) as HINSTANCE
    }

    unsafe fn register_class() {
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(overlay_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: module_instance(),
            hIcon: null_mut(),
            hCursor: null_mut(),
            // NULL_BRUSH: the overlay never erases its background, so the
            // WebView2-composited maximize button shows through untouched.
            hbrBackground: GetStockObject(NULL_BRUSH) as HBRUSH,
            lpszMenuName: null(),
            lpszClassName: CLASS_NAME.as_ptr(),
            hIconSm: null_mut(),
        };
        // Idempotent: a second registration just fails with "already exists".
        RegisterClassExW(&class);
    }

    /// The overlay's window procedure. Its whole area hit-tests as the caption
    /// maximize button, so the OS shows Snap Layouts on hover; the resulting
    /// non-client mouse messages are forwarded to the frontend.
    unsafe extern "system" fn overlay_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_NCHITTEST => HTMAXBUTTON as LRESULT,
            WM_NCMOUSEMOVE => {
                if !HOVERING.swap(true, Ordering::Relaxed) {
                    emit("hitch-max-button-hover", true);
                }
                // Ask for WM_NCMOUSELEAVE so we learn when the pointer exits.
                let mut track = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE | TME_NONCLIENT,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                TrackMouseEvent(&mut track);
                0
            }
            WM_NCMOUSELEAVE => {
                PRESSING.store(false, Ordering::Relaxed);
                if HOVERING.swap(false, Ordering::Relaxed) {
                    emit("hitch-max-button-hover", false);
                }
                0
            }
            // Swallow the press; act on release to match a native button click.
            WM_NCLBUTTONDOWN => {
                PRESSING.store(true, Ordering::Relaxed);
                0
            }
            WM_NCLBUTTONUP => {
                if PRESSING.swap(false, Ordering::Relaxed) {
                    emit("hitch-max-button-click", true);
                }
                0
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    /// Keep the overlay aligned to the maximize button across OS-driven resizes
    /// and DPI changes (the frontend re-reports its rectangle right after, which
    /// corrects any sub-frame drift).
    unsafe extern "system" fn parent_subclass_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _id: usize,
        _ref: usize,
    ) -> LRESULT {
        if msg == WM_SIZE || msg == WM_DPICHANGED {
            update_overlay_position();
        }
        DefSubclassProc(hwnd, msg, wparam, lparam)
    }

    /// Create the hit-test overlay as a child of the main window and start
    /// tracking its position. Must run on the main (UI) thread — `setup` does.
    pub fn install(window: &WebviewWindow) {
        let _ = APP.set(window.app_handle().clone());
        let Ok(handle) = window.window_handle() else {
            return;
        };
        let RawWindowHandle::Win32(win32) = handle.as_raw() else {
            return;
        };
        let parent = win32.hwnd.get() as *mut c_void as HWND;
        // SAFETY: `parent` is the live main-window handle on the UI thread;
        // `overlay_proc`/`parent_subclass_proc` are 'static with matching ABIs.
        unsafe {
            register_class();
            let overlay = CreateWindowExW(
                0,
                CLASS_NAME.as_ptr(),
                CLASS_NAME.as_ptr(),
                WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS | WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                parent,
                null_mut(),
                module_instance(),
                null_mut(),
            );
            if overlay.is_null() {
                return;
            }
            OVERLAY_HWND.store(overlay as isize, Ordering::Relaxed);
            SetWindowSubclass(parent, Some(parent_subclass_proc), SUBCLASS_ID, 0);
        }
        update_overlay_position();
    }
}

#[cfg(windows)]
pub use imp::{install, set_max_button_rect};

#[cfg(not(windows))]
pub fn set_max_button_rect(_left: i32, _top: i32, _right: i32, _bottom: i32) {}

#[cfg(not(windows))]
pub fn install(_window: &tauri::WebviewWindow) {}
