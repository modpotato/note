//! Paper Mono, embedded in the exe and registered with GDI for this process only.
//!
//! The faces are added with `AddFontMemResourceEx`, so nothing is installed system-wide and
//! Windows removes them when the process exits. Paper Mono is OFL 1.1, embedded unmodified;
//! see `assets/fonts/OFL.txt`.

use std::ptr::null;
use std::sync::OnceLock;

use windows_sys::Win32::Graphics::Gdi::AddFontMemResourceEx;

const REGULAR: &[u8] = include_bytes!("../../assets/fonts/PaperMono-Regular.ttf");
const BOLD: &[u8] = include_bytes!("../../assets/fonts/PaperMono-Bold.ttf");

/// Handles from `AddFontMemResourceEx`, kept for the life of the process.
static HANDLES: OnceLock<Vec<usize>> = OnceLock::new();

/// Registers the embedded faces. Call once, before the first font is created. A failure is
/// not fatal: the renderer then uses whichever installed face matches.
pub fn register() {
    let mut handles = Vec::with_capacity(2);
    for data in [REGULAR, BOLD] {
        // Receives the number of faces added; each file has one, so it is not checked.
        let mut added = 0u32;
        // SAFETY: `data` is a static slice that stays valid for the whole process, and GDI
        // copies the font data during the call. GDI writes the count through the pointer,
        // so it is derived from a mutable reference.
        let h = unsafe {
            AddFontMemResourceEx(
                data.as_ptr().cast(),
                data.len() as u32,
                null(),
                &mut added as *mut u32 as *const u32,
            )
        };
        if !h.is_null() {
            handles.push(h as usize);
        }
    }
    let _ = HANDLES.set(handles);
}
