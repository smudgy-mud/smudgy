//! Windows Restart Manager cooperation, so the installer can close a running
//! smudgy for an in-place upgrade without a manual prompt.
//!
//! Windows cannot overwrite a running `smudgy.exe`, so the installer
//! (`assets/installer.iss`) uses the Restart Manager to close smudgy before
//! replacing files. The Restart Manager shuts a GUI app down by ending its
//! "session" (`WM_QUERYENDSESSION` / `WM_ENDSESSION`), which winit does not
//! surface. We subclass each main window to catch those messages and exit.
//!
//! The exit must be **synchronous** here. During a session-end the winit/iced
//! event loop is no longer serviced, so routing the shutdown through a normal
//! `iced::exit` message would never run — the app would sit until the installer's
//! force-kill timeout (~5 s, felt as a hung installer) terminated it. Instead we
//! call `std::process::exit` straight from the window proc. smudgy persists its
//! state continuously, so an abrupt exit is safe; it matches what the installer's
//! `CloseApplications=force` would do anyway, only immediately instead of after
//! the timeout.
//!
//! Windows-only; the hook is a no-op elsewhere.

#[cfg(windows)]
pub use imp::hook_window;

#[cfg(not(windows))]
pub use stub::hook_window;

#[cfg(not(windows))]
mod stub {
    pub fn hook_window(_raw_id: u64) {}
}

#[cfg(windows)]
mod imp {
    use winapi::shared::basetsd::{DWORD_PTR, UINT_PTR};
    use winapi::shared::minwindef::{LPARAM, LRESULT, UINT, WPARAM};
    use winapi::shared::windef::HWND;
    use winapi::um::commctrl::{DefSubclassProc, SetWindowSubclass};
    use winapi::um::winuser::WM_ENDSESSION;

    /// A stable id identifying our subclass on a window (any nonzero value; only
    /// this module subclasses smudgy's windows).
    const SUBCLASS_ID: UINT_PTR = 1;

    /// Install the session-end watcher on a top-level window, given the raw HWND
    /// iced reported for it. Idempotent per window (the subclass id is fixed).
    pub fn hook_window(raw_id: u64) {
        let hwnd = raw_id as HWND;
        // SAFETY: `raw_id` is the HWND iced/winit reported for a live window.
        // SetWindowSubclass chains our proc ahead of winit's without disturbing
        // it — messages we do not handle fall through via DefSubclassProc.
        unsafe {
            let _ = SetWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID, 0);
        }
    }

    /// Window-proc subclass: when the Restart Manager ends the session to replace
    /// files, exit immediately (see the module docs on why this is synchronous).
    unsafe extern "system" fn subclass_proc(
        hwnd: HWND,
        msg: UINT,
        wparam: WPARAM,
        lparam: LPARAM,
        _id: UINT_PTR,
        _ref_data: DWORD_PTR,
    ) -> LRESULT {
        // WM_ENDSESSION with a nonzero wParam is the authoritative "the session is
        // really ending" signal. (WM_QUERYENDSESSION merely asks whether we may
        // close; letting it fall through to DefSubclassProc returns TRUE, which
        // permits the shutdown, and WM_ENDSESSION follows.)
        if msg == WM_ENDSESSION && wparam != 0 {
            std::process::exit(0);
        }
        // SAFETY: forwarding the same window-proc arguments to the next handler in
        // the subclass chain, as required for messages we do not consume.
        unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
    }
}
