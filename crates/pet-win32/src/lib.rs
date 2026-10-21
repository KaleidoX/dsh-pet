//! Global pointer state on Windows, for the click-through logic.
//!
//! The pet window is a single transparent rectangle, but only the hit box may
//! take clicks; everything else must fall through to whatever is underneath.
//! On Linux that is the X11 SHAPE input region (see `pet-ui-egui::x11`). The
//! Windows equivalent is the `WS_EX_TRANSPARENT | WS_EX_LAYERED` pair, which
//! applies to the whole window, so it has to be toggled as the pointer enters
//! and leaves the hit box. `WM_NCHITTEST` is not an option: `HTTRANSPARENT` is
//! only honoured for windows owned by the same thread, so it cannot hand a
//! click to another process.
//!
//! One Win32 call besides the pointer read: turning off DWM's non-client
//! rendering for the pet window.
//!
//! This crate is the only place allowed to call Win32 directly: it does not
//! inherit the workspace's `unsafe_code = "forbid"`.

/// Pointer position in physical screen pixels and whether the left button is down.
#[cfg(windows)]
pub fn pointer() -> Option<(i32, i32, bool)> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;

    let mut point = POINT { x: 0, y: 0 };
    // SAFETY: `GetCursorPos` only writes to the POINT we pass, and we check the
    // BOOL it returns before reading it.
    let ok = unsafe { GetCursorPos(&mut point) };
    if ok == 0 {
        return None;
    }
    // SAFETY: `GetAsyncKeyState` has no preconditions. Only the high bit is
    // documented as reliable, which is all we read.
    let down = unsafe { GetAsyncKeyState(VK_LBUTTON as i32) } < 0;
    Some((point.x, point.y, down))
}

/// The pointer is not readable on other platforms; they use their own backends.
#[cfg(not(windows))]
pub fn pointer() -> Option<(i32, i32, bool)> {
    None
}

/// Stops DWM from drawing a non-client frame around `hwnd`.
///
/// A borderless window still gets a thin DWM-drawn border and shadow, which
/// shows up as a grey outline around the pet's transparent canvas. The window
/// has no redirection surface, so there is no non-client area to render: with
/// `DWMNCRP_DISABLED` DWM skips it entirely and the outline is gone.
/// `DWMWA_BORDER_COLOR` does not help here — it only recolours the same frame.
///
/// Returns `false` when DWM rejects the attribute, in which case the frame stays.
#[cfg(windows)]
pub fn disable_dwm_frame(hwnd: isize) -> bool {
    use windows_sys::Win32::Graphics::Dwm::{DWMNCRP_DISABLED, DWMWA_NCRENDERING_POLICY, DwmSetWindowAttribute};

    let policy: i32 = DWMNCRP_DISABLED;
    // `hwnd` comes from a live window; `policy` is a plain i32 the API only reads.
    let hr = unsafe {
        DwmSetWindowAttribute(
            hwnd as windows_sys::Win32::Foundation::HWND,
            DWMWA_NCRENDERING_POLICY as u32,
            (&raw const policy).cast(),
            size_of::<i32>() as u32,
        )
    };
    hr >= 0
}

#[cfg(not(windows))]
pub fn disable_dwm_frame(_hwnd: isize) -> bool {
    false
}
