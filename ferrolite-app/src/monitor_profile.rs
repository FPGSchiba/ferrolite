//! Per-monitor ICC profile detection for the window's current monitor.
//! FFI is cfg-gated (Windows GDI ICM, macOS ColorSync); other OSes return None.
//! The heavy work (file read, parse, bake) happens on a job thread, not here.
//!
//! `detect`/`source_to_bytes` are called from the window-move / startup
//! detect flow wired up in Unit 5; `ProfileSource` is already consumed by
//! `settings::dto::resolve` (Unit 4 Task 10).
#![allow(dead_code)]

use std::path::PathBuf;

/// Where a detected/selected profile's bytes come from. `Path` is read on the
/// job thread (keeps file I/O off the UI thread).
pub enum ProfileSource {
    Path(PathBuf),
    Bytes(Vec<u8>),
}

/// Read a source to ICC bytes (job thread).
pub fn source_to_bytes(src: ProfileSource) -> std::io::Result<Vec<u8>> {
    match src {
        ProfileSource::Path(p) => std::fs::read(p),
        ProfileSource::Bytes(b) => Ok(b),
    }
}

#[cfg(windows)]
pub fn detect(raw: raw_window_handle::RawWindowHandle) -> (Option<ProfileSource>, u64) {
    use raw_window_handle::RawWindowHandle;
    use windows::core::PWSTR;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::{
        CreateDCW, DeleteDC, GetMonitorInfoW, MonitorFromWindow, MONITORINFOEXW,
        MONITOR_DEFAULTTONEAREST,
    };
    use windows::Win32::UI::ColorSystem::GetICMProfileW;

    let RawWindowHandle::Win32(h) = raw else {
        return (None, 0);
    };
    let hwnd = HWND(h.hwnd.get() as *mut _);
    // SAFETY: hwnd is a live top-level window handle from eframe for this frame.
    unsafe {
        let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let key = hmon.0 as u64;
        let mut mi = MONITORINFOEXW::default();
        mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if !GetMonitorInfoW(hmon, &mut mi.monitorInfo as *mut _ as *mut _).as_bool() {
            return (None, key);
        }
        let dc = CreateDCW(
            PWSTR(mi.szDevice.as_ptr() as *mut _),
            PWSTR::null(),
            PWSTR::null(),
            None,
        );
        if dc.is_invalid() {
            return (None, key);
        }
        let mut len: u32 = 260;
        let mut buf = vec![0u16; len as usize];
        let ok = GetICMProfileW(dc, &mut len, PWSTR(buf.as_mut_ptr())).as_bool();
        let _ = DeleteDC(dc);
        if !ok {
            return (None, key);
        }
        buf.truncate(len.saturating_sub(1) as usize);
        let path = String::from_utf16_lossy(&buf);
        if path.is_empty() {
            return (None, key);
        }
        (Some(ProfileSource::Path(PathBuf::from(path))), key)
    }
}

/// macOS: follow the PRIMARY (main) display's ICC profile, not the window's
/// actual monitor. Pure `core-graphics` (no AppKit/objc2) — accepted, documented
/// limitation: a window dragged onto a secondary display will still resolve
/// against the main display's profile until the app restarts / the main
/// display changes. The `Custom` file picker remains the cross-OS safety net
/// for anyone who needs per-monitor accuracy today.
///
/// `core-graphics` 0.24's safe wrapper does not expose
/// `CGDisplayCopyColorSpace`/`CGColorSpaceCopyICCData`, so they are bound
/// directly here (both are long-stable public CoreGraphics C APIs).
#[cfg(target_os = "macos")]
mod macos_ffi {
    use core_foundation::base::TCFType;
    use core_foundation::data::{CFData, CFDataRef};
    use core_graphics::color_space::CGColorSpace;
    use core_graphics::display::CGDirectDisplayID;
    use core_graphics::sys::CGColorSpaceRef;

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGDisplayCopyColorSpace(display: CGDirectDisplayID) -> CGColorSpaceRef;
        fn CGColorSpaceCopyICCData(space: CGColorSpaceRef) -> CFDataRef;
    }

    /// Copy the display's color space, if any. `None` on a null return (no
    /// display, or the system could not resolve one).
    pub fn display_color_space(display: CGDirectDisplayID) -> Option<CGColorSpace> {
        unsafe {
            let space = CGDisplayCopyColorSpace(display);
            if space.is_null() {
                None
            } else {
                // SAFETY: `CGDisplayCopyColorSpace` follows the Core
                // Foundation "Copy" naming rule — it returns an
                // already-retained reference we own.
                Some(CGColorSpace::from_ptr(space))
            }
        }
    }

    /// Copy the color space's embedded ICC profile bytes, if any.
    pub fn icc_data(space: &CGColorSpace) -> Option<Vec<u8>> {
        unsafe {
            let data_ref = CGColorSpaceCopyICCData(space.as_ptr());
            if data_ref.is_null() {
                None
            } else {
                // SAFETY: `CGColorSpaceCopyICCData` follows the "Copy"
                // naming rule — the returned CFData is already retained.
                let data: CFData = TCFType::wrap_under_create_rule(data_ref);
                Some(data.bytes().to_vec())
            }
        }
    }
}

#[cfg(target_os = "macos")]
pub fn detect(_raw: raw_window_handle::RawWindowHandle) -> (Option<ProfileSource>, u64) {
    use core_graphics::display::CGDisplay;

    let display = CGDisplay::main();
    let key = display.id as u64;
    let Some(color_space) = macos_ffi::display_color_space(display.id) else {
        return (None, key);
    };
    match macos_ffi::icc_data(&color_space) {
        Some(bytes) => (Some(ProfileSource::Bytes(bytes)), key),
        None => (None, key),
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn detect(_raw: raw_window_handle::RawWindowHandle) -> (Option<ProfileSource>, u64) {
    (None, 0)
}
