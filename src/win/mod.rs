//! Win32 window layer: application state, window procedure, keyboard and mouse input,
//! nano-style prompts, file I/O and the clipboard. Painting lives in `render`.

mod find;
#[cfg(feature = "embed-font")]
mod fonts;
mod render;

use std::ffi::c_void;
use std::io::ErrorKind;
use std::iter::once;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};

use windows_sys::core::{PCSTR, PCWSTR};
use windows_sys::Win32::Foundation::{
    GlobalFree, FARPROC, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows_sys::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, InvalidateRect, ScreenToClient, UpdateWindow, PAINTSTRUCT,
};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows_sys::Win32::System::Ole::CF_UNICODETEXT;
use windows_sys::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, GetSaveFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_PATHMUSTEXIST,
    OPENFILENAMEW,
};
use windows_sys::Win32::UI::Controls::WM_MOUSELEAVE;
use windows_sys::Win32::UI::HiDpi::{
    GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
    VK_CONTROL, VK_LWIN, VK_MENU, VK_RMENU, VK_RWIN, VK_SHIFT,
};
use windows_sys::Win32::UI::Shell::{DragFinish, DragQueryFileW, HDROP};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetCursorPos,
    GetMessageW, GetWindowLongPtrW, IsWindow, KillTimer, LoadCursorW, LoadIconW, PostQuitMessage,
    RegisterClassExW, SetCursor, SetTimer, SetWindowLongPtrW, SetWindowPos, SetWindowTextW,
    MessageBoxW, SystemParametersInfoW, SPI_GETWHEELSCROLLCHARS, SPI_GETWHEELSCROLLLINES,
    ShowWindow, TranslateMessage, CREATESTRUCTW, CS_DBLCLKS, CW_USEDEFAULT, GWLP_USERDATA,
    HTCLIENT, IDC_ARROW, IDC_IBEAM, IDI_APPLICATION, IDNO, IDYES, MB_DEFBUTTON2, MB_ICONWARNING,
    MB_YESNO, MB_YESNOCANCEL, MSG, SWP_NOACTIVATE, SWP_NOZORDER, SW_SHOW,
    WHEEL_DELTA, WM_CHAR, WM_CLOSE, WM_CREATE, WM_DESTROY, WM_DPICHANGED, WM_DROPFILES,
    WM_ERASEBKGND, WM_KEYDOWN, WM_KEYUP, WM_KILLFOCUS, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCCREATE,
    WM_NCDESTROY, WM_PAINT,
    WM_SETCURSOR, WM_SETFOCUS, WM_SIZE, WM_SYSCHAR, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_TIMER,
    WNDCLASSEXW, WS_EX_ACCEPTFILES, WS_OVERLAPPEDWINDOW,
};

use crate::config::{
    Action, Chord, Config, TabBar, VK_BACK, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME,
    VK_LEFT, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_TAB, VK_UP,
};
use crate::editor::{save_as_path, Editor, Move};
use crate::simd;
use crate::syntax::Lang;
use render::{rect, Renderer};

const TIMER_BLINK: usize = 1;
const BLINK_MS: u32 = 530;
const MIN_PT: i32 = 6;
const MAX_PT: i32 = 72;
/// Capacity, in UTF-16 units, of the open/save dialog path buffer.
const PATH_CHARS: usize = 32768;
const WINDOW_CLASS: &str = "note.window";
const OPEN_FILTER: &str = "All Files (*.*)\0*.*\0Text Documents (*.txt)\0*.txt\0\0";
const SAVE_FILTER: &str = "Text Documents (*.txt)\0*.txt\0All Files (*.*)\0*.*\0\0";
/// 1-based positions of the entries in SAVE_FILTER.
const SAVE_FILTER_TEXT: u32 = 1;
const SAVE_FILTER_ALL: u32 = 2;
/// Win32 MK_LBUTTON, MK_SHIFT and MK_CONTROL key-state flags (WM_MOUSEMOVE / WM_MOUSEWHEEL wParam).
const MK_LBUTTON_FLAG: usize = 0x0001;
const MK_SHIFT_FLAG: usize = 0x0004;
const MK_CONTROL_FLAG: usize = 0x0008;
/// Cells moved per horizontal wheel notch.
const H_STEP: isize = 4;
/// Custom scrollbars at 96 dpi: strip thickness, shortest thumb, and the gap between the
/// thumb and the strip edges.
const SB_THICK_PX: i32 = 10;
const SB_MIN_THUMB_PX: i32 = 24;
const SB_INSET_PX: i32 = 2;
/// Lines moved per vertical wheel notch.
const V_WHEEL_LINES: isize = 3;
/// Tab strip sizes at 96 dpi: strip height, tab width limits, the left margin, the room
/// kept at the right end for "+", the top of a tab and its corner radius, the title's left
/// padding, the room right of the title for the close box or dirty dot, the close box and
/// "+" button sizes, and the gaps around them.
const TAB_BAR_PX: i32 = 32;
const TAB_MAX_PX: i32 = 220;
const TAB_MIN_PX: i32 = 56;
const TAB_MARGIN_PX: i32 = 8;
const TAB_RESERVE_PX: i32 = 40;
const TAB_TOP_PX: i32 = 6;
const TAB_RADIUS_PX: i32 = 8;
const TAB_TEXT_PAD_PX: i32 = 12;
const TAB_CLOSE_SPACE_PX: i32 = 28;
const TAB_CLOSE_PX: i32 = 18;
const TAB_CLOSE_INSET_PX: i32 = 8;
const TAB_CLOSE_RADIUS_PX: i32 = 4;
const TAB_PLUS_PX: i32 = 24;
const TAB_PLUS_GAP_PX: i32 = 6;
const TAB_SEP_INSET_PX: i32 = 8;
/// Most closed documents kept for Ctrl+Shift+T.
const CLOSED_TABS_MAX: usize = 10;
/// Label of the goto-line prompt.
const GOTO_LABEL: &str = "Enter line number, column number: ";

/// Converts `s` to a NUL-terminated UTF-16 buffer for Win32 calls.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(once(0)).collect()
}

/// Reads a UTF-16 buffer up to its first NUL (or the whole buffer) as a `String`.
fn from_wide(buf: &[u16]) -> String {
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..n])
}

/// Resolves an ordinal export of uxtheme.dll. Undocumented, so callers treat failure as normal.
///
/// # Safety
/// Only resolves an address; callers must transmute it to the export's real signature.
unsafe fn uxtheme_export(ordinal: usize) -> FARPROC {
    let name = wide("uxtheme.dll");
    let module = LoadLibraryW(name.as_ptr());
    if module.is_null() {
        return None;
    }
    GetProcAddress(module, ordinal as PCSTR)
}

/// Lets the process use dark-mode menus and dialogs (uxtheme ordinal 135, AllowDark).
fn enable_dark_app_mode() {
    // SAFETY: ordinal 135 is SetPreferredAppMode(int) returning int.
    unsafe {
        if let Some(f) = uxtheme_export(135) {
            let set_mode: unsafe extern "system" fn(i32) -> i32 = std::mem::transmute(f);
            set_mode(1);
        }
    }
}

/// Dark title bar and dark-mode allowance for one window.
fn enable_dark_window(hwnd: HWND) {
    // SAFETY: the attribute value is a 4-byte BOOL that outlives the call. Ordinal 133 is
    // AllowDarkModeForWindow(HWND, BOOL) returning BOOL.
    unsafe {
        let on: i32 = 1;
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
            &on as *const i32 as *const c_void,
            size_of::<i32>() as u32,
        );
        if let Some(f) = uxtheme_export(133) {
            let allow: unsafe extern "system" fn(HWND, i32) -> i32 = std::mem::transmute(f);
            allow(hwnd, 1);
        }
    }
}

/// Starts the editor: DPI awareness, config, optional file argument, window, message loop.
pub fn run() {
    // SAFETY: plain call without pointer arguments.
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    // Before any font is created, so the embedded faces can be matched.
    #[cfg(feature = "embed-font")]
    fonts::register();
    let config = load_config();
    let bg = config.theme.bg;
    let dark = luminance(bg.0, bg.1, bg.2) < 128;
    if dark {
        enable_dark_app_mode();
    }
    let files: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    // Owned by the window from here on: freed in WM_NCDESTROY (or below on failure).
    let raw = Box::into_raw(Box::new(App::new(config, files, dark)));

    // SAFETY: standard class registration and creation. The App pointer travels through
    // lpCreateParams to WM_NCCREATE and is freed in WM_NCDESTROY.
    unsafe {
        let hinst = GetModuleHandleW(null());
        let mut icon = LoadIconW(hinst, 1usize as PCWSTR);
        if icon.is_null() {
            icon = LoadIconW(null_mut(), IDI_APPLICATION);
        }
        let class = wide(WINDOW_CLASS);
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinst,
            hIcon: icon,
            hCursor: LoadCursorW(null_mut(), IDC_IBEAM),
            hbrBackground: null_mut(),
            lpszMenuName: null(),
            lpszClassName: class.as_ptr(),
            hIconSm: icon,
        };
        if RegisterClassExW(&wc) == 0 {
            drop(Box::from_raw(raw));
            return;
        }
        let title = wide("note");
        let hwnd = CreateWindowExW(
            WS_EX_ACCEPTFILES,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            null_mut(),
            null_mut(),
            hinst,
            raw as *const c_void,
        );
        if hwnd.is_null() {
            drop(Box::from_raw(raw));
            return;
        }
        ShowWindow(hwnd, SW_SHOW);
        UpdateWindow(hwnd);

        // The process lives exactly as long as its window: the loop ends on WM_QUIT, on a
        // GetMessage error, or as soon as the window no longer exists (even if no quit
        // message was posted), and the process then exits immediately.
        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            if find::pre_translate(&msg) {
                continue;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
            if IsWindow(hwnd) == 0 {
                break;
            }
        }
    }
    std::process::exit(0);
}

/// `px` at 96 dpi, scaled to `dpi`, never below 1.
fn scaled(px: i32, dpi: u32) -> i32 {
    ((px as i64 * dpi as i64 + 48) / 96).max(1) as i32
}

/// Thumb length for a view of `visible` units in `total`, along a track of `track` pixels.
/// Never below `min` (or the track itself, when that is shorter).
fn bar_len(visible: usize, total: usize, track: i32, min: i32) -> i32 {
    let len = (visible as u64 * track as u64 / total.max(1) as u64) as i32;
    len.max(min.min(track)).min(track)
}

/// True when (x, y) is inside `r`, right and bottom edges excluded.
fn inside(r: &RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

/// Perceptual brightness on 0..=255, as used for the dark-mode decision.
fn luminance(r: u8, g: u8, b: u8) -> u32 {
    (r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000
}

/// `%APPDATA%\note`, which holds note.ini and session.txt.
fn app_dir() -> Option<std::path::PathBuf> {
    let appdata = std::env::var("APPDATA").ok()?;
    Some(std::path::PathBuf::from(appdata).join("note"))
}

/// Reads `%APPDATA%\note\note.ini`, creating it with the defaults when missing.
fn load_config() -> Config {
    let Some(dir) = app_dir() else {
        return Config::default();
    };
    let file = dir.join("note.ini");
    if !file.exists() {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(&file, Config::default_ini());
    }
    match std::fs::read(&file) {
        Ok(bytes) => Config::parse(&String::from_utf8_lossy(&bytes)),
        Err(_) => Config::default(),
    }
}

/// The paths and active index written by `App::save_session`, or None without a session file.
fn read_session() -> Option<(Vec<String>, usize)> {
    let text = std::fs::read_to_string(app_dir()?.join("session.txt")).ok()?;
    let mut paths = Vec::new();
    let mut active = 0;
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("active=") {
            active = v.trim().parse().unwrap_or(0);
        } else if !line.trim().is_empty() {
            paths.push(line.to_string());
        }
    }
    Some((paths, active))
}

/// File name of a document for titles and messages, or "Untitled" when it has no path.
fn doc_name(ed: &Editor) -> &str {
    match &ed.path {
        Some(p) => p.rsplit(['\\', '/']).next().unwrap_or(p.as_str()),
        None => "Untitled",
    }
}

/// Starts another copy of the program with no files. Errors are ignored.
fn spawn_window() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).spawn();
    }
}

/// Window procedure. Recovers the App from GWLP_USERDATA and forwards to `handle`.
unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = lp as *const CREATESTRUCTW;
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, (*cs).lpCreateParams as isize);
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    if msg == WM_NCDESTROY {
        let p = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) as *mut App;
        if !p.is_null() {
            drop(Box::from_raw(p));
        }
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
    if p.is_null() {
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    if msg == WM_DESTROY {
        // Always end the message loop, however the window came to be destroyed.
        PostQuitMessage(0);
        return 0;
    }
    let app = &mut *p;
    let res = handle(app, hwnd, msg, wp, lp);
    if app.quit && !app.destroying {
        // DestroyWindow re-enters this procedure (focus, activation, WM_DESTROY, ...) and
        // frees `app` in WM_NCDESTROY, so mark it first and never touch `app` afterwards.
        app.destroying = true;
        app.save_session();
        DestroyWindow(hwnd);
        return 0;
    }
    res.unwrap_or_else(|| DefWindowProcW(hwnd, msg, wp, lp))
}

/// Dispatches one message. `None` means "let DefWindowProc handle it".
fn handle(app: &mut App, hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    match msg {
        WM_CREATE => app.on_create(hwnd),
        WM_CLOSE => app.request_exit(hwnd),
        WM_ERASEBKGND => return Some(1),
        WM_PAINT => app.on_paint(hwnd),
        WM_SIZE => app.on_size(hwnd, lp),
        WM_DPICHANGED => app.on_dpi(hwnd, wp, lp),
        WM_SETFOCUS => app.on_focus(hwnd, true),
        WM_KILLFOCUS => app.on_focus(hwnd, false),
        WM_TIMER => {
            if wp == TIMER_BLINK {
                app.on_blink(hwnd);
            }
        }
        // Keys we handle return 0 so Alt does not open the (empty) system menu; the rest go
        // to DefWindowProc so Alt+F4 and friends keep working.
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            return if app.on_keydown(hwnd, wp as u16) {
                Some(0)
            } else {
                None
            };
        }
        WM_KEYUP | WM_SYSKEYUP => app.suppress_char = false,
        WM_CHAR => app.on_char(hwnd, wp as u16),
        WM_SYSCHAR => app.on_syschar(hwnd, wp as u16),
        WM_LBUTTONDOWN => app.on_mouse_down(hwnd, lp),
        WM_LBUTTONDBLCLK => app.on_double_click(hwnd, lp),
        WM_MOUSEMOVE => app.on_mouse_move(hwnd, lp, wp),
        WM_LBUTTONUP => app.on_mouse_up(hwnd),
        WM_MBUTTONUP => app.on_middle_up(hwnd, lp),
        WM_MOUSELEAVE => app.on_mouse_leave(hwnd),
        WM_SETCURSOR => {
            // Only the client area is ours; borders and the caption keep their cursors.
            if (lp & 0xFFFF) as u32 == HTCLIENT && app.on_set_cursor(hwnd) {
                return Some(1);
            }
            return None;
        }
        WM_MOUSEWHEEL => app.on_wheel(hwnd, wp),
        WM_MOUSEHWHEEL => app.on_hwheel(hwnd, wp),
        WM_DROPFILES => app.on_drop(hwnd, wp as HDROP),
        find::WM_FIND => app.on_find_msg(hwnd, wp),
        _ => return None,
    }
    Some(0)
}

/// Modifier state read at input time.
#[derive(Clone, Copy)]
struct Mods {
    ctrl: bool,
    alt: bool,
    shift: bool,
    /// Ctrl+RightAlt, the layout's AltGr. Its characters arrive as text, not as chords.
    altgr: bool,
}

impl Mods {
    fn read() -> Mods {
        let down = |vk: u16| -> bool { unsafe { GetKeyState(vk as i32) < 0 } };
        let ctrl = down(VK_CONTROL);
        Mods {
            ctrl,
            alt: down(VK_MENU),
            shift: down(VK_SHIFT),
            altgr: ctrl && down(VK_RMENU),
        }
    }
}

/// Signed client x/y from a mouse message's lParam.
fn point_of(lp: LPARAM) -> (i32, i32) {
    let x = (lp & 0xFFFF) as u16 as i16 as i32;
    let y = ((lp >> 16) & 0xFFFF) as u16 as i16 as i32;
    (x, y)
}

/// Reads a u32 system setting (e.g. wheel scroll lines), or `default` if the call fails.
fn system_param(action: u32, default: u32) -> u32 {
    let mut v: u32 = default;
    // SAFETY: the SPI_GETWHEELSCROLL* actions write one u32 through pvParam.
    let ok = unsafe { SystemParametersInfoW(action, 0, (&mut v as *mut u32).cast(), 0) };
    if ok == 0 {
        default
    } else {
        v
    }
}

/// Signed wheel delta from a wheel message's wParam.
fn wheel_delta(wp: WPARAM) -> i32 {
    ((wp >> 16) & 0xFFFF) as u16 as i16 as i32
}

/// Pixel and cell geometry for the current client size and font.
#[derive(Clone, Copy)]
struct Layout {
    w: i32,
    /// Height of the editor: the client height less the tab strip.
    h: i32,
    /// Height of the tab strip above the editor, 0 when hidden.
    tabbar_h: i32,
    lh: i32,
    cw: i32,
    /// Height of the text area, above the status row. The horizontal scrollbar strip, when
    /// shown, is its bottom `sb` pixels.
    text_h: i32,
    /// Height of the rows that show text: `text_h`, less the horizontal scrollbar strip.
    view_h: i32,
    /// Visible text rows (at least 1).
    rows: usize,
    /// Visible columns right of the gutter (at least 1), not counting the vertical
    /// scrollbar strip on the right edge.
    cols: usize,
    /// Gutter width in pixels, 0 when line numbers are off.
    gutter: i32,
    /// Scrollbar thickness in pixels for this dpi.
    sb: i32,
    /// True when the horizontal scrollbar is shown (no wrap, and a line runs past the view).
    hbar: bool,
    /// Widest visible line in cells, measured only without wrap.
    longest: usize,
}

/// Scrollbar axis. The vertical bar runs down the right edge of the text, the horizontal
/// one along the bottom of the text, just above the status row.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Axis {
    Vert,
    Horz,
}

/// Geometry of one custom scrollbar for this frame, in client pixels.
#[derive(Clone, Copy)]
struct Bar {
    axis: Axis,
    /// The whole strip. It is the hit area and the track painted on hover.
    track: RECT,
    /// The thumb, inset from the strip's edges.
    thumb: RECT,
    /// Along-axis pixel where the track starts, and where the thumb starts and how long
    /// it is.
    origin: i32,
    start: i32,
    len: i32,
    /// Pixels the thumb can travel: track length less thumb length.
    span: i32,
    /// Largest scroll position the thumb maps to: a logical line, or a column.
    max: usize,
}

impl Bar {
    /// The pointer's coordinate along this bar's axis.
    fn along(&self, x: i32, y: i32) -> i32 {
        match self.axis {
            Axis::Vert => y,
            Axis::Horz => x,
        }
    }
}

/// The scrollbars for one frame. A bar is None when it is not shown.
#[derive(Clone, Copy)]
struct Bars {
    v: Option<Bar>,
    h: Option<Bar>,
}

impl Bars {
    /// The shown bar whose strip contains the point.
    fn hit(&self, x: i32, y: i32) -> Option<Bar> {
        [self.v, self.h]
            .into_iter()
            .flatten()
            .find(|b| inside(&b.track, x, y))
    }
}

/// A thumb drag in progress. Positions are fixed when the drag starts, so the thumb keeps
/// tracking the pointer even though the line widths behind the horizontal bar change.
#[derive(Clone, Copy)]
struct SbDrag {
    axis: Axis,
    /// Pointer offset from the thumb start when the button went down.
    grab: i32,
    origin: i32,
    span: i32,
    max: usize,
}

/// What the pointer is over in the tab strip. Tab indices are absolute.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TabHit {
    /// The title of a tab.
    Tab(usize),
    /// The close box of a tab, or its dirty dot.
    Close(usize),
    /// The "+" button.
    New,
}

impl TabHit {
    /// The tab this item belongs to, or None for the "+" button.
    fn tab_index(self) -> Option<usize> {
        match self {
            TabHit::Tab(i) | TabHit::Close(i) => Some(i),
            TabHit::New => None,
        }
    }
}

/// Geometry of the tab strip for one frame, in client pixels. Slot `k` is the `k`-th tab
/// drawn, which is tab `first + k`.
#[derive(Clone, Copy)]
struct TabStrip {
    /// Client width and strip height.
    w: i32,
    h: i32,
    dpi: u32,
    /// Left margin before the first slot, and the width of one slot.
    margin: i32,
    tab_w: i32,
    /// Index of the first tab drawn, and how many tabs are drawn.
    first: usize,
    count: usize,
    /// The active tab.
    active: usize,
}

impl TabStrip {
    /// `v` at 96 dpi, scaled to this strip's dpi.
    fn px(&self, v: i32) -> i32 {
        scaled(v, self.dpi)
    }

    /// Left edge of slot `k`.
    fn slot_x(&self, k: usize) -> i32 {
        self.margin + k as i32 * self.tab_w
    }

    /// Area of slot `k`: from the top of the tabs down to the bottom of the strip.
    fn slot(&self, k: usize) -> RECT {
        let x = self.slot_x(k);
        rect(x, self.px(TAB_TOP_PX), x + self.tab_w, self.h)
    }

    /// Close box of slot `k`: a square at the right of the tab, centred vertically.
    fn close_box(&self, k: usize) -> RECT {
        let s = self.slot(k);
        let size = self.px(TAB_CLOSE_PX);
        let right = s.right - self.px(TAB_CLOSE_INSET_PX);
        let top = (s.top + self.h) / 2 - size / 2;
        rect(right - size, top, right, top + size)
    }

    /// The "+" button, just after the last drawn tab.
    fn plus_box(&self) -> RECT {
        let size = self.px(TAB_PLUS_PX);
        let x = self.slot_x(self.count) + self.px(TAB_PLUS_GAP_PX);
        let y = (self.h - size) / 2;
        rect(x, y, x + size, y + size)
    }

    /// The strip item at a point in client pixels, or None.
    fn hit(&self, x: i32, y: i32) -> Option<TabHit> {
        if y < 0 || y >= self.h {
            return None;
        }
        if inside(&self.plus_box(), x, y) {
            return Some(TabHit::New);
        }
        (0..self.count).find_map(|k| {
            inside(&self.slot(k), x, y).then(|| {
                let i = self.first + k;
                if inside(&self.close_box(k), x, y) {
                    TabHit::Close(i)
                } else {
                    TabHit::Tab(i)
                }
            })
        })
    }
}

/// nano-style prompt shown in the status row (Ctrl+G only).
struct Prompt {
    label: String,
    text: String,
    /// Byte offset of the input caret, always on a char boundary.
    cursor: usize,
}

impl Prompt {
    fn insert(&mut self, s: &str) {
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    fn backspace(&mut self) {
        if let Some((i, _)) = self.text[..self.cursor].char_indices().next_back() {
            self.text.replace_range(i..self.cursor, "");
            self.cursor = i;
        }
    }

    fn delete(&mut self) {
        if let Some(c) = self.text[self.cursor..].chars().next() {
            self.text
                .replace_range(self.cursor..self.cursor + c.len_utf8(), "");
        }
    }

    fn left(&mut self) {
        if let Some((i, _)) = self.text[..self.cursor].char_indices().next_back() {
            self.cursor = i;
        }
    }

    fn right(&mut self) {
        if let Some(c) = self.text[self.cursor..].chars().next() {
            self.cursor += c.len_utf8();
        }
    }
}

/// Characters that make up a "word" for double-click and word motion. Non-ASCII bytes count
/// as word bytes so multi-byte characters stay together.
fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// Decimal digit count of `n` (at least 1), without allocating.
fn digits(mut n: usize) -> usize {
    let mut d = 1;
    while n >= 10 {
        n /= 10;
        d += 1;
    }
    d
}

/// Window state. Owned by the window through a Box pointer in GWLP_USERDATA.
struct App {
    /// The active tab's document. The other tabs' documents are in `tabs`.
    editor: Editor,
    /// One entry per tab. `tabs[active]` is an empty placeholder: the live document is `editor`.
    tabs: Vec<Editor>,
    active: usize,
    /// Documents closed this session, most recent last, for Ctrl+Shift+T.
    closed: Vec<Editor>,
    /// First tab slot drawn in the tab strip.
    tab_first: usize,
    /// Item under the pointer in the tab strip.
    tab_hot: Option<TabHit>,
    /// The left button is held on a tab: moving the pointer reorders the tabs.
    tab_drag: bool,
    /// Wheel movement over the tab strip not yet turned into a tab switch.
    wheel_t: i32,
    config: Config,
    renderer: Renderer,
    /// Font size delta in points, applied on top of `config.font_size`.
    zoom: i32,
    dpi: u32,
    client_w: i32,
    client_h: i32,
    caret_visible: bool,
    focused: bool,
    prompt: Option<Prompt>,
    /// Find/Replace dialog state and its live preview.
    find: find::FindUi,
    /// Left button is held after a click in the text area.
    drag: bool,
    show_help: bool,
    help_scroll: usize,
    /// Set while the WM_CHAR of a key we already handled must be swallowed.
    suppress_char: bool,
    high_surrogate: u16,
    wheel_v: i32,
    wheel_h: i32,
    wheel_z: i32,
    /// The pointer is inside the window, so WM_MOUSELEAVE has been requested.
    mouse_in: bool,
    /// Scrollbar the pointer is over, if any.
    sb_hover: Option<Axis>,
    /// Thumb drag in progress.
    sb_drag: Option<SbDrag>,
    last_title: String,
    dark: bool,
    /// Set when the window must be destroyed once the current message is done.
    quit: bool,
    /// Set once DestroyWindow has been called, so re-entrant messages don't call it again.
    destroying: bool,
}

impl App {
    fn new(config: Config, files: Vec<String>, dark: bool) -> App {
        let renderer = Renderer::new(&config);
        let editor = Editor::new(&config);
        let tabs = vec![Editor::new(&config)];
        let mut app = App {
            editor,
            tabs,
            active: 0,
            closed: Vec::new(),
            tab_first: 0,
            tab_hot: None,
            tab_drag: false,
            wheel_t: 0,
            config,
            renderer,
            zoom: 0,
            dpi: 96,
            client_w: 1,
            client_h: 1,
            caret_visible: true,
            focused: false,
            prompt: None,
            find: find::FindUi::new(),
            drag: false,
            show_help: false,
            help_scroll: 0,
            suppress_char: false,
            high_surrogate: 0,
            wheel_v: 0,
            wheel_h: 0,
            wheel_z: 0,
            mouse_in: false,
            sb_hover: None,
            sb_drag: None,
            last_title: String::new(),
            dark,
            quit: false,
            destroying: false,
        };
        if files.is_empty() {
            if app.config.restore_tabs {
                app.restore_session();
            }
        } else {
            for path in &files {
                app.place_path(path);
            }
        }
        app
    }

    /// Effective font size in points, kept within MIN_PT..=MAX_PT.
    fn pt(&self) -> i32 {
        (self.config.font_size + self.zoom).clamp(MIN_PT, MAX_PT)
    }

    /// Current size as a percentage of the configured size.
    fn zoom_pct(&self) -> i32 {
        self.pt() * 100 / self.config.font_size.max(1)
    }

    fn apply_font(&mut self) {
        let pt = self.pt();
        self.renderer.set_font(&self.config, pt, self.dpi);
    }

    fn change_zoom(&mut self, hwnd: HWND, delta: i32) {
        let pt = (self.pt() + delta).clamp(MIN_PT, MAX_PT);
        self.zoom = pt - self.config.font_size;
        self.apply_font();
        self.after_command(hwnd);
    }

    /// Current geometry. Rows and columns are at least 1 so scrolling math stays valid.
    /// The vertical scrollbar always takes its strip from the columns, so the caret never
    /// sits under it. The horizontal one takes the bottom row strip, but only when a line
    /// runs past the view.
    fn layout(&self) -> Layout {
        let lh = self.renderer.line_h.max(1);
        let cw = self.renderer.char_w.max(1);
        let w = self.client_w.max(1);
        let tabbar_h = self.tabbar_h();
        let h = (self.client_h - tabbar_h).max(1);
        let sb = scaled(SB_THICK_PX, self.dpi);
        let hint_rows = if self.config.show_hints { 2 } else { 0 };
        let text_h = (h - (1 + hint_rows) * lh).max(0);
        let gutter = if self.config.line_numbers {
            let d = digits(self.editor.buf.line_count()).max(3) as i32;
            (d + 2) * cw
        } else {
            0
        };
        let cols = ((w - gutter - sb) / cw).max(1) as usize;
        let rows_all = ((text_h / lh) as usize).max(1);
        let longest = if self.editor.wrap {
            0
        } else {
            self.editor.longest_visible(rows_all)
        };
        let hbar = !self.editor.wrap && (longest > cols || self.editor.scroll_col > 0);
        let view_h = if hbar { (text_h - sb).max(0) } else { text_h };
        let rows = ((view_h / lh) as usize).max(1);
        Layout {
            w,
            h,
            tabbar_h,
            lh,
            cw,
            text_h,
            view_h,
            rows,
            cols,
            gutter,
            sb,
            hbar,
            longest,
        }
    }

    /// Geometry of both custom scrollbars for this frame. Nothing is shown while the help
    /// overlay covers the text.
    fn scroll_bars(&self, lay: Layout) -> Bars {
        let mut bars = Bars { v: None, h: None };
        if self.show_help {
            return bars;
        }
        let ins = scaled(SB_INSET_PX, self.dpi);
        let min_len = scaled(SB_MIN_THUMB_PX, self.dpi);
        let ed = &self.editor;

        let track_len = lay.view_h;
        if track_len > 0 && ed.overflows_view(lay.rows) {
            let lines = ed.buf.line_count();
            // Total in logical lines: the last line can scroll to the top of the view.
            let total = lines + lay.rows - 1;
            let len = bar_len(lay.rows, total, track_len, min_len);
            let max = lines - 1;
            let span = track_len - len;
            let start = (span as usize * ed.scroll_line.min(max))
                .checked_div(max)
                .unwrap_or(0) as i32;
            let track = rect(lay.w - lay.sb, 0, lay.w, track_len);
            bars.v = Some(Bar {
                axis: Axis::Vert,
                track,
                thumb: rect(
                    track.left + ins,
                    start + ins,
                    track.right - ins,
                    start + len - ins,
                ),
                origin: 0,
                start,
                len,
                span,
                max,
            });
        }

        let track_w = lay.w - lay.sb - lay.gutter;
        if lay.hbar && track_w > 0 {
            let track = rect(lay.gutter, lay.view_h, lay.w - lay.sb, lay.text_h);
            // The content is at least as wide as the widest visible line, and at least as
            // wide as the view scrolled to its current column.
            let total = lay.longest.max(ed.scroll_col + lay.cols);
            let max = (total - lay.cols).max(1);
            let len = bar_len(lay.cols, total, track_w, min_len);
            let span = track_w - len;
            let start = track.left + (span as usize * ed.scroll_col / max) as i32;
            bars.h = Some(Bar {
                axis: Axis::Horz,
                track,
                thumb: rect(
                    start + ins,
                    track.top + ins,
                    start + len - ins,
                    track.bottom - ins,
                ),
                origin: track.left,
                start,
                len,
                span,
                max,
            });
        }
        bars
    }

    // ----- window lifecycle --------------------------------------------------

    fn on_create(&mut self, hwnd: HWND) {
        // SAFETY: hwnd is the window being created.
        unsafe {
            self.dpi = GetDpiForWindow(hwnd).max(96);
            let mut rc: RECT = zeroed();
            GetClientRect(hwnd, &mut rc);
            self.client_w = rc.right;
            self.client_h = rc.bottom;
        }
        self.apply_font();
        if self.dark {
            enable_dark_window(hwnd);
        }
        self.focused = true;
        self.sync_title(hwnd);
        self.after_command(hwnd);
    }

    fn on_paint(&mut self, hwnd: HWND) {
        // SAFETY: BeginPaint/EndPaint bracket the paint with a valid PAINTSTRUCT.
        unsafe {
            let mut ps: PAINTSTRUCT = zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);
            render::paint(self, hdc, ps.rcPaint);
            EndPaint(hwnd, &ps);
        }
    }

    fn on_size(&mut self, hwnd: HWND, lp: LPARAM) {
        self.client_w = (lp & 0xFFFF) as i32;
        self.client_h = ((lp >> 16) & 0xFFFF) as i32;
        self.after_command(hwnd);
    }

    fn on_dpi(&mut self, hwnd: HWND, wp: WPARAM, lp: LPARAM) {
        self.dpi = ((wp & 0xFFFF) as u32).max(48);
        self.apply_font();
        // SAFETY: Windows passes a valid suggested RECT in lParam for WM_DPICHANGED.
        unsafe {
            let rc = &*(lp as *const RECT);
            SetWindowPos(
                hwnd,
                null_mut(),
                rc.left,
                rc.top,
                rc.right - rc.left,
                rc.bottom - rc.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        self.after_command(hwnd);
    }

    fn on_focus(&mut self, hwnd: HWND, gained: bool) {
        self.focused = gained;
        if gained {
            self.caret_visible = true;
            self.start_blink(hwnd);
        } else {
            // SAFETY: the id matches the one used by start_blink.
            unsafe {
                KillTimer(hwnd, TIMER_BLINK);
            }
            self.caret_visible = false;
        }
        redraw(hwnd);
    }

    /// (Re)arms the blink timer. Restarting resets the blink phase.
    fn start_blink(&self, hwnd: HWND) {
        if self.focused {
            // SAFETY: a null timer procedure makes the timer post WM_TIMER to the window.
            unsafe {
                SetTimer(hwnd, TIMER_BLINK, BLINK_MS, None);
            }
        }
    }

    fn on_blink(&mut self, hwnd: HWND) {
        self.caret_visible = !self.caret_visible;
        if let Some(rc) = caret_rect(self) {
            // SAFETY: rc lives for the duration of the call.
            unsafe {
                InvalidateRect(hwnd, &rc, 0);
            }
        }
    }

    /// Common tail of every command: keep the cursor visible, sync title,
    /// restart the caret blink and repaint.
    fn after_command(&mut self, hwnd: HWND) {
        let lay = self.layout();
        // Wrapping width follows the window and the font, so it is refreshed on every command.
        self.editor.wrap_cols = lay.cols;
        self.editor.ensure_cursor_visible(lay.rows, lay.cols);
        self.tab_first = self.tab_strip().map_or(0, |s| s.first);
        self.sync_title(hwnd);
        self.caret_visible = true;
        self.start_blink(hwnd);
        redraw(hwnd);
    }

    fn sync_title(&mut self, hwnd: HWND) {
        let title = self.editor.title();
        if title != self.last_title {
            let w = wide(&title);
            // SAFETY: w is NUL-terminated and lives across the call.
            unsafe {
                SetWindowTextW(hwnd, w.as_ptr());
            }
            self.last_title = title;
        }
    }

    // ----- keyboard ----------------------------------------------------------

    /// Returns true when the key was handled. Its WM_CHAR is then suppressed.
    fn on_keydown(&mut self, hwnd: HWND, vk: u16) -> bool {
        self.suppress_char = false;
        if matches!(vk, VK_SHIFT | VK_CONTROL | VK_MENU | VK_LWIN | VK_RWIN) {
            return false;
        }
        let m = Mods::read();
        self.editor.message = None;

        if self.prompt.is_some() {
            let handled = self.prompt_key(hwnd, vk, m);
            self.suppress_char = handled;
            return handled;
        }
        if self.show_help && self.help_key(hwnd, vk) {
            self.suppress_char = true;
            return true;
        }

        let chord = Chord {
            vk,
            ctrl: m.ctrl,
            alt: m.alt,
            shift: m.shift,
        };
        let action = if m.altgr {
            None
        } else {
            self.config.keymap.lookup(chord)
        };
        // Consecutive cut-line calls accumulate. Every other key breaks the chain.
        if action != Some(Action::CutLine) {
            self.editor.reset_cut_chain();
        }
        if let Some(a) = action {
            self.dispatch(hwnd, a);
            self.suppress_char = true;
            self.after_command(hwnd);
            return true;
        }
        if m.altgr {
            return false;
        }
        if self.show_help {
            // The overlay is modal for editing keys: they are swallowed, not applied.
            self.suppress_char = true;
            return true;
        }
        if self.nav_key(vk, m) {
            self.suppress_char = true;
            self.after_command(hwnd);
            return true;
        }
        false
    }

    /// Unbound navigation and editing keys. Returns true when handled.
    fn nav_key(&mut self, vk: u16, m: Mods) -> bool {
        let rows = self.layout().rows;
        let ed = &mut self.editor;
        match vk {
            VK_LEFT => ed.move_cursor(if m.ctrl { Move::WordLeft } else { Move::Left }, m.shift),
            VK_RIGHT => ed.move_cursor(if m.ctrl { Move::WordRight } else { Move::Right }, m.shift),
            VK_UP => ed.move_cursor(Move::Up, m.shift),
            VK_DOWN => ed.move_cursor(Move::Down, m.shift),
            VK_HOME => ed.move_cursor(
                if m.ctrl {
                    Move::DocStart
                } else {
                    Move::LineHome
                },
                m.shift,
            ),
            VK_END => ed.move_cursor(if m.ctrl { Move::DocEnd } else { Move::LineEnd }, m.shift),
            VK_PRIOR => ed.move_cursor(Move::PageUp(rows), m.shift),
            VK_NEXT => ed.move_cursor(Move::PageDown(rows), m.shift),
            VK_DELETE => {
                if m.ctrl {
                    ed.delete_word_right()
                } else {
                    ed.delete_forward()
                }
            }
            VK_BACK => {
                if m.ctrl {
                    ed.delete_word_left()
                } else {
                    ed.backspace()
                }
            }
            VK_RETURN => ed.insert_char('\n'),
            VK_TAB => {
                if m.shift {
                    ed.unindent()
                } else if ed.selection().is_some() {
                    ed.indent()
                } else {
                    ed.insert_char('\t')
                }
            }
            VK_ESCAPE => {
                ed.mark_mode = false;
                ed.anchor = None;
            }
            _ => return false,
        }
        true
    }

    /// Keys while the help overlay is open. Returns true when consumed.
    fn help_key(&mut self, hwnd: HWND, vk: u16) -> bool {
        let page = self.layout().rows.saturating_sub(1).max(1);
        match vk {
            VK_ESCAPE => self.show_help = false,
            VK_UP => self.help_scroll = self.help_scroll.saturating_sub(1),
            VK_DOWN => self.help_scroll += 1,
            VK_PRIOR => self.help_scroll = self.help_scroll.saturating_sub(page),
            VK_NEXT => self.help_scroll += page,
            VK_HOME => self.help_scroll = 0,
            VK_END => self.help_scroll = usize::MAX / 2,
            _ => return false,
        }
        redraw(hwnd);
        true
    }

    /// Keys while a prompt is open. Returns true when consumed. Plain printable keys return
    /// false so their WM_CHAR can type into the prompt.
    fn prompt_key(&mut self, hwnd: HWND, vk: u16, m: Mods) -> bool {
        if m.altgr {
            return false;
        }
        if self.prompt.is_none() {
            return false;
        }
        match vk {
            VK_RETURN => self.prompt_confirm(hwnd),
            VK_ESCAPE => self.prompt = None,
            0x56 if m.ctrl => {
                // Ctrl+V pastes the first line of the clipboard.
                if let Some(text) = clipboard_get(hwnd) {
                    let line = text.split(['\r', '\n']).next().unwrap_or("");
                    if let Some(p) = self.prompt.as_mut() {
                        p.insert(line);
                    }
                }
            }
            _ => {
                let Some(p) = self.prompt.as_mut() else {
                    return false;
                };
                match vk {
                    VK_BACK => p.backspace(),
                    VK_DELETE => p.delete(),
                    VK_LEFT => p.left(),
                    VK_RIGHT => p.right(),
                    VK_HOME => p.cursor = 0,
                    VK_END => p.cursor = p.text.len(),
                    // Chords are swallowed here; plain keys go on to WM_CHAR.
                    _ => return m.ctrl || m.alt,
                }
            }
        }
        self.after_command(hwnd);
        true
    }

    /// Enter in the goto prompt: goes to the line and column it names, then closes.
    fn prompt_confirm(&mut self, hwnd: HWND) {
        let Some(prompt) = self.prompt.take() else {
            return;
        };
        let mut parts = prompt.text.split(',');
        let line = parts.next().and_then(|s| s.trim().parse::<usize>().ok());
        let col = parts
            .next()
            .and_then(|s| s.trim().parse::<usize>().ok())
            .unwrap_or(1);
        match line {
            Some(l) => self.editor.goto_line(l, col),
            None => self.editor.message = Some("Invalid line number".to_string()),
        }
        self.after_command(hwnd);
    }

    /// Ctrl+G: opens the goto prompt, empty.
    fn open_goto_prompt(&mut self) {
        self.prompt = Some(Prompt {
            label: GOTO_LABEL.to_string(),
            text: String::new(),
            cursor: 0,
        });
    }

    /// Turns a UTF-16 unit from WM_CHAR into a char, joining surrogate pairs.
    fn decode_unit(&mut self, unit: u16) -> Option<char> {
        if (0xD800..0xDC00).contains(&unit) {
            self.high_surrogate = unit;
            return None;
        }
        let high = std::mem::take(&mut self.high_surrogate);
        if (0xDC00..0xE000).contains(&unit) {
            if high == 0 {
                return None;
            }
            return char::decode_utf16([high, unit]).next().and_then(Result::ok);
        }
        char::from_u32(unit as u32)
    }

    fn on_char(&mut self, hwnd: HWND, unit: u16) {
        if self.suppress_char {
            self.suppress_char = false;
            self.high_surrogate = 0;
            return;
        }
        let Some(ch) = self.decode_unit(unit) else {
            return;
        };
        if ch < ' ' || ch == '\u{7f}' || self.show_help {
            return;
        }
        if let Some(p) = self.prompt.as_mut() {
            let mut b = [0u8; 4];
            p.insert(ch.encode_utf8(&mut b));
            self.after_command(hwnd);
            return;
        }
        self.editor.message = None;
        self.editor.reset_cut_chain();
        self.editor.insert_char(ch);
        self.after_command(hwnd);
    }

    fn on_syschar(&mut self, hwnd: HWND, unit: u16) {
        // Alt+letter is a command, never text. AltGr can arrive as a system char on some
        // layouts, so it is let through as text.
        if Mods::read().altgr {
            self.on_char(hwnd, unit);
        } else {
            self.suppress_char = false;
            self.high_surrogate = 0;
        }
    }

    // ----- commands ----------------------------------------------------------

    fn dispatch(&mut self, hwnd: HWND, action: Action) {
        let rows = self.layout().rows;
        match action {
            Action::Save => {
                self.save(hwnd);
            }
            Action::SaveAs => {
                self.save_as(hwnd);
            }
            Action::Open => {
                if let Some(path) = open_dialog(hwnd) {
                    self.open_paths(hwnd, &[path]);
                }
            }
            Action::New => self.new_tab(hwnd),
            Action::Exit => self.request_exit(hwnd),
            Action::CloseTab => self.close_tab_checked(hwnd, self.active),
            Action::ReopenTab => self.reopen_tab(hwnd),
            Action::NextTab => self.activate(hwnd, (self.active + 1) % self.tab_count()),
            Action::PrevTab => {
                let n = self.tab_count();
                self.activate(hwnd, (self.active + n - 1) % n);
            }
            Action::Tab1 => self.activate(hwnd, 0),
            Action::Tab2 => self.activate(hwnd, 1),
            Action::Tab3 => self.activate(hwnd, 2),
            Action::Tab4 => self.activate(hwnd, 3),
            Action::Tab5 => self.activate(hwnd, 4),
            Action::Tab6 => self.activate(hwnd, 5),
            Action::Tab7 => self.activate(hwnd, 6),
            Action::Tab8 => self.activate(hwnd, 7),
            Action::LastTab => {
                let last = self.tab_count() - 1;
                self.activate(hwnd, last);
            }
            Action::MoveTabLeft => {
                if self.active > 0 {
                    self.move_tab(self.active, self.active - 1);
                }
            }
            Action::MoveTabRight => {
                if self.active + 1 < self.tab_count() {
                    self.move_tab(self.active, self.active + 1);
                }
            }
            Action::NewWindow => spawn_window(),
            Action::CutLine => {
                let text = self.editor.cut_line();
                clipboard_set(hwnd, &text);
            }
            Action::Paste => self.paste(hwnd),
            Action::Copy => self.copy(hwnd),
            Action::Cut => self.cut(hwnd),
            Action::Find => self.open_find(hwnd, false),
            Action::FindNext => self.find_again(hwnd, true),
            Action::FindPrev => self.find_again(hwnd, false),
            Action::Replace => self.open_find(hwnd, true),
            Action::GotoLine => self.open_goto_prompt(),
            Action::ShowPosition => self.show_position(),
            Action::Help => {
                self.show_help = !self.show_help;
                self.help_scroll = 0;
            }
            Action::Undo => self.editor.undo(),
            Action::Redo => self.editor.redo(),
            Action::SelectAll => self.editor.select_all(),
            Action::LineHome => self.editor.move_cursor(Move::LineHome, false),
            Action::LineEnd => self.editor.move_cursor(Move::LineEnd, false),
            Action::PageUp => self.editor.move_cursor(Move::PageUp(rows), false),
            Action::PageDown => self.editor.move_cursor(Move::PageDown(rows), false),
            Action::DocStart => self.editor.move_cursor(Move::DocStart, false),
            Action::DocEnd => self.editor.move_cursor(Move::DocEnd, false),
            Action::WordLeft => self.editor.move_cursor(Move::WordLeft, false),
            Action::WordRight => self.editor.move_cursor(Move::WordRight, false),
            Action::DeleteWordLeft => self.editor.delete_word_left(),
            Action::DeleteWordRight => self.editor.delete_word_right(),
            Action::ToggleLineNumbers => self.config.line_numbers = !self.config.line_numbers,
            Action::ToggleWrap => {
                let on = !self.editor.wrap;
                self.editor.wrap = on;
                self.config.word_wrap = on;
                self.editor.scroll_subrow = 0;
                self.editor.scroll_col = 0;
                self.editor.message = Some(if on {
                    "Soft wrap enabled"
                } else {
                    "Soft wrap disabled"
                }
                .to_string());
            }
            Action::ToggleHints => self.config.show_hints = !self.config.show_hints,
            Action::ZoomIn => self.change_zoom(hwnd, 1),
            Action::ZoomOut => self.change_zoom(hwnd, -1),
            Action::ZoomReset => {
                self.zoom = 0;
                self.apply_font();
            }
            Action::Indent => self.editor.indent(),
            Action::Unindent => self.editor.unindent(),
            Action::DuplicateLine => self.editor.duplicate_line(),
            Action::MoveLineUp => self.editor.move_line(true),
            Action::MoveLineDown => self.editor.move_line(false),
            Action::ToggleComment => self.editor.toggle_comment(),
            Action::MarkToggle => {
                let ed = &mut self.editor;
                ed.mark_mode = !ed.mark_mode;
                if ed.mark_mode {
                    ed.anchor = Some(ed.cursor);
                    ed.message = Some("Mark Set".to_string());
                } else {
                    ed.anchor = None;
                    ed.message = Some("Mark Unset".to_string());
                }
            }
        }
    }

    fn show_position(&mut self) {
        let (line, col) = self.editor.cursor_line_col();
        let lines = self.editor.buf.line_count();
        let pct = (line + 1) * 100 / lines.max(1);
        let cur = self.editor.cursor;
        let bytes = self.editor.buf.make_contiguous();
        let before = bytes[..cur].iter().filter(|&&b| b & 0xC0 != 0x80).count();
        let total = bytes.iter().filter(|&&b| b & 0xC0 != 0x80).count();
        self.editor.message = Some(format!(
            "line {}/{} ({}%), col {}, char {}/{}",
            line + 1,
            lines,
            pct,
            col + 1,
            before + 1,
            total
        ));
    }

    fn paste(&mut self, hwnd: HWND) {
        match clipboard_get(hwnd).filter(|s| !s.is_empty()) {
            Some(text) => self.editor.insert_text(text.as_bytes()),
            None => {
                let text = self.editor.cut_buffer.clone();
                if !text.is_empty() {
                    self.editor.insert_text(&text);
                }
            }
        }
    }

    /// Selection text, or the current line with its newline when nothing is selected.
    fn copy_text(&self) -> Vec<u8> {
        match self.editor.copy_selection() {
            Some(t) => t,
            None => {
                let buf = &self.editor.buf;
                let line = buf.line_of(self.editor.cursor);
                let mut t = buf.slice(buf.line_start(line), buf.line_end(line));
                t.push(b'\n');
                t
            }
        }
    }

    fn copy(&mut self, hwnd: HWND) {
        let text = self.copy_text();
        clipboard_set(hwnd, &text);
    }

    /// Cuts the selection, or the current line when nothing is selected.
    fn cut(&mut self, hwnd: HWND) {
        let text = match self.editor.cut_selection() {
            Some(t) => t,
            None => self.editor.cut_line(),
        };
        clipboard_set(hwnd, &text);
    }

    // ----- confirm and file I/O ----------------------------------------------

    // ----- tabs ---------------------------------------------------------------
    //
    // `editor` is the active document. `tabs[i]` holds the document of tab i for every
    // i != `active`, and `tabs[active]` is an empty placeholder, so switching tabs is two
    // swaps and the rest of the code only ever sees `editor`.

    /// Number of tabs.
    fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    /// Document of tab `i`.
    fn tab_editor(&self, i: usize) -> &Editor {
        if i == self.active {
            &self.editor
        } else {
            self.tabs.get(i).unwrap_or(&self.editor)
        }
    }

    /// True when the tab strip is shown: always, never, or (Auto) with two or more tabs.
    fn tab_bar_shown(&self) -> bool {
        match self.config.tab_bar {
            TabBar::Always => true,
            TabBar::Auto => self.tab_count() >= 2,
            TabBar::Never => false,
        }
    }

    /// Height of the tab strip in pixels, 0 when hidden.
    fn tabbar_h(&self) -> i32 {
        if self.tab_bar_shown() {
            scaled(TAB_BAR_PX, self.dpi)
        } else {
            0
        }
    }

    /// Geometry of the tab strip, or None when hidden. The first slot is `tab_first`, moved
    /// only as far as needed to keep the active tab in view. Slots are as wide as the room
    /// divided by the tab count, within TAB_MIN_PX..=TAB_MAX_PX.
    fn tab_strip(&self) -> Option<TabStrip> {
        if !self.tab_bar_shown() {
            return None;
        }
        let dpi = self.dpi;
        let margin = scaled(TAB_MARGIN_PX, dpi);
        let total = self.tab_count();
        let avail = (self.client_w - margin - scaled(TAB_RESERVE_PX, dpi)).max(0);
        let tab_w = (avail / total as i32).clamp(scaled(TAB_MIN_PX, dpi), scaled(TAB_MAX_PX, dpi));
        let fit = ((avail / tab_w) as usize).clamp(1, total);
        let mut first = self.tab_first.min(total - 1);
        if self.active < first {
            first = self.active;
        }
        if self.active >= first + fit {
            first = self.active + 1 - fit;
        }
        let first = first.min(total - fit);
        Some(TabStrip {
            w: self.client_w,
            h: scaled(TAB_BAR_PX, dpi),
            dpi,
            margin,
            tab_w,
            first,
            count: fit,
            active: self.active,
        })
    }

    /// Makes tab `b` the active one. Only the swap is done; see `activate` for the rest.
    fn swap_to(&mut self, b: usize) {
        let a = self.active;
        if a == b || b >= self.tabs.len() {
            return;
        }
        std::mem::swap(&mut self.editor, &mut self.tabs[a]);
        std::mem::swap(&mut self.editor, &mut self.tabs[b]);
        self.active = b;
    }

    /// Work after the active tab changed: no prompt stays open, the find caches belong to
    /// the old document, and the window follows the new one.
    fn switched(&mut self, hwnd: HWND) {
        self.prompt = None;
        self.find.reset_caches();
        self.after_command(hwnd);
    }

    /// Shows tab `i`.
    fn activate(&mut self, hwnd: HWND, i: usize) {
        if i == self.active || i >= self.tab_count() {
            return;
        }
        self.swap_to(i);
        self.switched(hwnd);
    }

    /// Puts `doc` in a new tab right after the active one, without showing it. Returns its index.
    fn insert_after_active(&mut self, doc: Editor) -> usize {
        let i = self.active + 1;
        self.tabs.insert(i, doc);
        i
    }

    /// Opens an empty tab after the active one and shows it.
    fn new_tab(&mut self, hwnd: HWND) {
        let doc = Editor::new(&self.config);
        let i = self.insert_after_active(doc);
        self.activate(hwnd, i);
    }

    /// Shows the most recently closed document, in a new tab after the active one.
    fn reopen_tab(&mut self, hwnd: HWND) {
        if let Some(doc) = self.closed.pop() {
            let i = self.insert_after_active(doc);
            self.activate(hwnd, i);
        }
    }

    /// Closes tab `i` without asking. Its document goes to the reopen list. Closing the
    /// last tab ends the program.
    fn close_tab(&mut self, hwnd: HWND, i: usize) {
        if i >= self.tab_count() {
            return;
        }
        if self.tab_count() == 1 {
            self.quit = true;
            return;
        }
        let was_active = i == self.active;
        if was_active {
            // The tab on the right takes over, or the one on the left at the end.
            let next = if i + 1 < self.tab_count() { i + 1 } else { i - 1 };
            self.swap_to(next);
        }
        // The closed document now sits at `i`, whichever tab was active.
        let doc = self.tabs.remove(i);
        if i < self.active {
            self.active -= 1;
        }
        if self.closed.len() == CLOSED_TABS_MAX {
            self.closed.remove(0);
        }
        self.closed.push(doc);
        self.tab_hot = None;
        if was_active {
            self.switched(hwnd);
        } else {
            self.after_command(hwnd);
        }
    }

    /// Closes tab `i` after asking about its unsaved changes. The tab is shown while it is
    /// asked about, and stays open on Cancel or when the save does not happen.
    fn close_tab_checked(&mut self, hwnd: HWND, i: usize) {
        if i >= self.tab_count() {
            return;
        }
        if self.tab_editor(i).is_dirty() {
            self.activate(hwnd, i);
            if !self.confirm_close_active(hwnd) {
                self.after_command(hwnd);
                return;
            }
        }
        self.close_tab(hwnd, i);
    }

    /// Moves tab `from` to position `to`. The active tab stays the active one.
    fn move_tab(&mut self, from: usize, to: usize) {
        let n = self.tab_count();
        if from == to || from >= n || to >= n {
            return;
        }
        let doc = self.tabs.remove(from);
        self.tabs.insert(to, doc);
        let a = self.active;
        self.active = if a == from {
            to
        } else if from < a && a <= to {
            a - 1
        } else if to <= a && a < from {
            a + 1
        } else {
            a
        };
    }

    /// Index of the open tab that shows `path` (case-insensitive).
    fn tab_for_path(&self, path: &str) -> Option<usize> {
        let want = path.to_lowercase();
        (0..self.tab_count()).find(|&i| {
            self.tab_editor(i)
                .path
                .as_deref()
                .is_some_and(|p| p.to_lowercase() == want)
        })
    }

    /// Loads the file at `path` into a tab, without redrawing. A tab that already shows it
    /// is shown again. The active tab is reused when it is an empty untitled document;
    /// otherwise the file gets a new tab after the active one.
    fn place_path(&mut self, path: &str) {
        if let Some(i) = self.tab_for_path(path) {
            self.swap_to(i);
            return;
        }
        let untitled = self.editor.path.is_none()
            && self.editor.buf.is_empty()
            && !self.editor.is_dirty();
        if !untitled {
            let doc = Editor::new(&self.config);
            let i = self.insert_after_active(doc);
            self.swap_to(i);
        }
        self.load_path(path);
    }

    /// Opens each file in a tab and shows the last one.
    fn open_paths(&mut self, hwnd: HWND, paths: &[String]) {
        for path in paths {
            self.place_path(path);
        }
        self.switched(hwnd);
    }

    /// Reorder drag: while the left button is held on a tab, the active tab moves past a
    /// visible neighbour once the pointer is beyond that neighbour's centre. The hover
    /// follows the dragged tab.
    fn tab_drag_move(&mut self, hwnd: HWND, strip: TabStrip, x: i32, y: i32) {
        // Horizontal centre of tab `j` while it is drawn in the strip.
        let centre = |j: usize| -> Option<i32> {
            (j >= strip.first && j < strip.first + strip.count)
                .then(|| strip.slot_x(j - strip.first) + strip.tab_w / 2)
        };
        loop {
            let i = self.active;
            let to = if i > 0 && centre(i - 1).is_some_and(|c| x < c) {
                i - 1
            } else if centre(i + 1).is_some_and(|c| x > c) {
                i + 1
            } else {
                break;
            };
            self.move_tab(i, to);
        }
        let hot = self.tab_strip().and_then(|s| s.hit(x, y));
        self.set_tab_hot(hwnd, hot);
        self.after_command(hwnd);
    }

    // ----- unsaved changes, exit and session ----------------------------------

    /// Asks whether the active tab's changes should be saved before the tab goes. Returns
    /// true when it may go: the answer was Yes and the save happened, or the answer was No.
    fn confirm_close_active(&mut self, hwnd: HWND) -> bool {
        let text = wide(&format!(
            "Do you want to save changes to {}?",
            doc_name(&self.editor)
        ));
        let caption = wide("note");
        // SAFETY: both strings are NUL-terminated and outlive the call.
        let answer = unsafe {
            MessageBoxW(
                hwnd,
                text.as_ptr(),
                caption.as_ptr(),
                MB_YESNOCANCEL | MB_ICONWARNING,
            )
        };
        match answer {
            IDYES => self.save(hwnd),
            IDNO => true,
            _ => false,
        }
    }

    /// Closes the window once every unsaved tab is settled. Each dirty tab is shown and asked
    /// about in turn. Cancel, or a save that does not happen, keeps the window open.
    fn request_exit(&mut self, hwnd: HWND) {
        for i in 0..self.tab_count() {
            if self.tab_editor(i).is_dirty() {
                self.activate(hwnd, i);
                if !self.confirm_close_active(hwnd) {
                    self.after_command(hwnd);
                    return;
                }
            }
        }
        self.quit = true;
    }

    /// Writes the paths of the tabs that have one, and the active index, to session.txt. Only
    /// when restore_tabs is on. Called as the window is about to be destroyed.
    fn save_session(&self) {
        use std::fmt::Write as _;
        if !self.config.restore_tabs {
            return;
        }
        let Some(dir) = app_dir() else {
            return;
        };
        let mut text = String::new();
        let mut kept = 0usize;
        let mut active = 0usize;
        for i in 0..self.tab_count() {
            if i == self.active {
                active = kept;
            }
            if let Some(path) = &self.tab_editor(i).path {
                let _ = writeln!(text, "{path}");
                kept += 1;
            }
        }
        let _ = writeln!(text, "active={}", active.min(kept.saturating_sub(1)));
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("session.txt"), text);
    }

    /// Replaces the untitled startup tab with the files of the last session. Files that can
    /// no longer be read are skipped. Only called when no file was given on the command line.
    fn restore_session(&mut self) {
        let Some((paths, saved)) = read_session() else {
            return;
        };
        let mut docs = Vec::new();
        let mut want = None;
        for (k, path) in paths.iter().enumerate() {
            if k == saved {
                want = Some(docs.len());
            }
            if let Ok(bytes) = std::fs::read(path) {
                let mut doc = Editor::new(&self.config);
                doc.load_owned(path, bytes);
                docs.push(doc);
            }
        }
        if docs.is_empty() {
            return;
        }
        let idx = want.unwrap_or(0).min(docs.len() - 1);
        self.editor = std::mem::replace(&mut docs[idx], Editor::new(&self.config));
        self.tabs = docs;
        self.active = idx;
    }

    fn load_path(&mut self, path: &str) {
        match std::fs::read(path) {
            Ok(bytes) => self.editor.load_owned(path, bytes),
            Err(e) if e.kind() == ErrorKind::NotFound => {
                self.editor.load_owned(path, Vec::new());
                self.editor.message = Some("New file".to_string());
            }
            Err(e) => self.editor.message = Some(format!("Error reading {path}: {e}")),
        }
    }

    /// Saves to the current path, or asks for one. Returns true when the file was written.
    fn save(&mut self, hwnd: HWND) -> bool {
        match self.editor.path.clone() {
            Some(p) => self.write_to(&p),
            None => self.save_as(hwnd),
        }
    }

    fn save_as(&mut self, hwnd: HWND) -> bool {
        match save_dialog(hwnd, self.editor.path.as_deref()) {
            Some(path) => self.write_to(&path),
            None => false,
        }
    }

    /// Encodes and writes the buffer. It is marked clean only after a successful write.
    fn write_to(&mut self, path: &str) -> bool {
        let e = &mut self.editor;
        let bytes = e.save_bytes();
        match std::fs::write(path, &bytes) {
            Ok(()) => {
                let lines = e.buf.nano_line_count();
                e.mark_saved();
                e.path = Some(path.to_string());
                e.highlighter.set_lang(Lang::from_path(path));
                e.message = Some(format!("Wrote {lines} lines"));
                true
            }
            Err(err) => {
                e.message = Some(format!("Error writing {path}: {err}"));
                false
            }
        }
    }

    fn on_drop(&mut self, hwnd: HWND, hdrop: HDROP) {
        // SAFETY: hdrop comes from WM_DROPFILES and is finished exactly once below.
        let paths = unsafe {
            let n = DragQueryFileW(hdrop, u32::MAX, null_mut(), 0);
            let mut paths = Vec::with_capacity(n as usize);
            for i in 0..n {
                let len = DragQueryFileW(hdrop, i, null_mut(), 0);
                let mut buf = vec![0u16; len as usize + 1];
                DragQueryFileW(hdrop, i, buf.as_mut_ptr(), len + 1);
                paths.push(from_wide(&buf));
            }
            DragFinish(hdrop);
            paths
        };
        if !paths.is_empty() {
            self.open_paths(hwnd, &paths);
        }
    }

    // ----- mouse and wheel ---------------------------------------------------

    /// Byte offset under a client point, clamped to the visible text area.
    fn pos_at(&self, lay: Layout, x: i32, y: i32) -> usize {
        let row = (y.max(0) / lay.lh) as usize;
        let dx = x - lay.gutter;
        let col = if dx <= 0 {
            0
        } else {
            ((dx + lay.cw / 2) / lay.cw) as usize
        };
        self.editor.pos_from_view(row, col, lay.rows)
    }

    /// True when a mouse point lands in the text area and the mouse is free to use.
    fn text_click_ok(&self, y: i32) -> bool {
        !self.show_help && self.prompt.is_none() && y >= 0 && y < self.layout().view_h
    }

    /// True when a point lies in a scrollbar strip, shown or not. The strips are reserved
    /// for the bars, so a click there never moves the caret or starts a selection.
    fn in_bar_strip(lay: Layout, x: i32, y: i32) -> bool {
        (x >= lay.w - lay.sb && y >= 0 && y < lay.view_h) || (y >= lay.view_h && y < lay.text_h)
    }

    /// Press in a bar strip. On the thumb it starts a drag. On the track it pages one
    /// screen toward the click.
    fn bar_press(&mut self, hwnd: HWND, lay: Layout, x: i32, y: i32) {
        let Some(bar) = self.scroll_bars(lay).hit(x, y) else {
            return;
        };
        let along = bar.along(x, y);
        if along >= bar.start && along < bar.start + bar.len {
            self.sb_drag = Some(SbDrag {
                axis: bar.axis,
                grab: along - bar.start,
                origin: bar.origin,
                span: bar.span,
                max: bar.max,
            });
            self.sb_hover = Some(bar.axis);
            // SAFETY: released in on_mouse_up once the drag ends.
            unsafe {
                SetCapture(hwnd);
            }
        } else {
            let dir: isize = if along < bar.start { -1 } else { 1 };
            match bar.axis {
                Axis::Vert => self.editor.scroll_by(dir * lay.rows as isize),
                Axis::Horz => self.scroll_cols(dir * lay.cols as isize),
            }
        }
        redraw(hwnd);
    }

    /// Moves the view to follow a thumb drag. The pointer offset maps linearly onto the
    /// scroll range.
    fn bar_drag(&mut self, hwnd: HWND, lp: LPARAM) {
        let Some(d) = self.sb_drag else {
            return;
        };
        let (x, cy) = point_of(lp);
        let y = cy - self.tabbar_h();
        let along = if d.axis == Axis::Vert { y } else { x };
        let rel = (along - d.grab - d.origin).clamp(0, d.span) as usize;
        let pos = if d.span > 0 {
            rel * d.max / d.span as usize
        } else {
            0
        };
        match d.axis {
            Axis::Vert => {
                self.editor.scroll_line = pos;
                self.editor.scroll_subrow = 0;
            }
            Axis::Horz => self.editor.scroll_col = pos,
        }
        redraw(hwnd);
    }

    /// Asks Windows for WM_MOUSELEAVE the first time the pointer is seen in the window.
    fn track_mouse(&mut self, hwnd: HWND) {
        if self.mouse_in {
            return;
        }
        let mut tme = TRACKMOUSEEVENT {
            cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: hwnd,
            dwHoverTime: 0,
        };
        // SAFETY: tme is fully initialised and lives for the call.
        unsafe {
            TrackMouseEvent(&mut tme);
        }
        self.mouse_in = true;
    }

    fn on_mouse_leave(&mut self, hwnd: HWND) {
        self.mouse_in = false;
        let hot = self.tab_hot.take().is_some();
        let bar = self.sb_hover.take().is_some();
        if hot || bar {
            redraw(hwnd);
        }
    }

    /// Pointer position in client pixels, for messages that carry screen coordinates.
    fn cursor_in_client(hwnd: HWND) -> (i32, i32) {
        let mut pt = POINT { x: 0, y: 0 };
        // SAFETY: pt is a valid out-parameter and hwnd is a live window.
        unsafe {
            GetCursorPos(&mut pt);
            ScreenToClient(hwnd, &mut pt);
        }
        (pt.x, pt.y)
    }

    /// Records the strip item under the pointer and repaints when it changes.
    fn set_tab_hot(&mut self, hwnd: HWND, hot: Option<TabHit>) {
        if self.tab_hot != hot {
            self.tab_hot = hot;
            redraw(hwnd);
        }
    }

    /// Press on the tab strip: shows a tab and starts a reorder drag on it, closes a tab
    /// through its close box, or opens a tab on "+".
    fn tab_press(&mut self, hwnd: HWND, x: i32, y: i32) {
        let Some(strip) = self.tab_strip() else {
            return;
        };
        match strip.hit(x, y) {
            Some(TabHit::Tab(i)) => {
                self.activate(hwnd, i);
                self.tab_drag = true;
                // SAFETY: released in on_mouse_up.
                unsafe {
                    SetCapture(hwnd);
                }
            }
            Some(TabHit::Close(i)) => self.close_tab_checked(hwnd, i),
            Some(TabHit::New) => self.new_tab(hwnd),
            None => {}
        }
    }

    /// Middle click on a tab closes it.
    fn on_middle_up(&mut self, hwnd: HWND, lp: LPARAM) {
        let (x, y) = point_of(lp);
        let hit = self.tab_strip().and_then(|s| s.hit(x, y));
        if let Some(i) = hit.and_then(TabHit::tab_index) {
            self.close_tab_checked(hwnd, i);
        }
    }

    /// Arrow over the tab strip and the bar strips, the I-beam (class cursor) over the text.
    /// Returns true when the arrow was set.
    fn on_set_cursor(&self, hwnd: HWND) -> bool {
        let (x, cy) = Self::cursor_in_client(hwnd);
        let hb = self.tabbar_h();
        let arrow = if cy < hb {
            true
        } else {
            !self.show_help && Self::in_bar_strip(self.layout(), x, cy - hb)
        };
        if !arrow {
            return false;
        }
        // SAFETY: IDC_ARROW is a system cursor, so no module handle is needed.
        unsafe {
            SetCursor(LoadCursorW(null_mut(), IDC_ARROW));
        }
        true
    }

    fn on_mouse_down(&mut self, hwnd: HWND, lp: LPARAM) {
        let (x, cy) = point_of(lp);
        let hb = self.tabbar_h();
        if cy < hb {
            self.tab_press(hwnd, x, cy);
            return;
        }
        let y = cy - hb;
        let lay = self.layout();
        if Self::in_bar_strip(lay, x, y) {
            self.bar_press(hwnd, lay, x, y);
            return;
        }
        if !self.text_click_ok(y) {
            return;
        }
        let pos = self.pos_at(lay, x, y);
        // SAFETY: plain state query.
        let shift = unsafe { GetKeyState(VK_SHIFT as i32) < 0 };
        self.editor.reset_cut_chain();
        self.editor.set_cursor(pos, shift);
        self.drag = true;
        // SAFETY: released in on_mouse_up (or when a move arrives without the button).
        unsafe {
            SetCapture(hwnd);
        }
        self.after_command(hwnd);
    }

    fn on_mouse_move(&mut self, hwnd: HWND, lp: LPARAM, wp: WPARAM) {
        self.track_mouse(hwnd);
        if wp & MK_LBUTTON_FLAG == 0 {
            // The button went up outside the window, or was never down: end any drag.
            self.on_mouse_up(hwnd);
        }
        let (x, cy) = point_of(lp);
        let hb = self.tabbar_h();
        if self.tab_drag {
            if let Some(strip) = self.tab_strip() {
                self.tab_drag_move(hwnd, strip, x, cy);
            }
            return;
        }
        // Over the strip, with no text or bar drag under way, the pointer is on the tabs.
        if cy < hb && !self.drag && self.sb_drag.is_none() {
            let hot = self.tab_strip().and_then(|s| s.hit(x, cy));
            self.set_tab_hot(hwnd, hot);
            if self.sb_hover.take().is_some() {
                redraw(hwnd);
            }
            return;
        }
        self.set_tab_hot(hwnd, None);
        let mut y = cy - hb;
        let lay = self.layout();
        if self.sb_drag.is_some() {
            self.bar_drag(hwnd, lp);
            return;
        }
        let hover = self.scroll_bars(lay).hit(x, y).map(|b| b.axis);
        if hover != self.sb_hover {
            self.sb_hover = hover;
            redraw(hwnd);
        }
        if !self.drag {
            return;
        }
        // Autoscroll while dragging above or below the text rows.
        if y < 0 {
            self.editor.scroll_by(-1);
            y = 0;
        } else if y >= lay.view_h {
            self.editor.scroll_by(1);
            y = lay.view_h - 1;
        }
        let pos = self.pos_at(lay, x, y.max(0));
        self.editor.set_cursor(pos, true);
        self.after_command(hwnd);
    }

    fn on_mouse_up(&mut self, hwnd: HWND) {
        if self.tab_drag {
            self.tab_drag = false;
            // SAFETY: the mouse was captured in tab_press.
            unsafe {
                ReleaseCapture();
            }
        }
        if self.sb_drag.take().is_some() {
            // SAFETY: the mouse was captured in bar_press.
            unsafe {
                ReleaseCapture();
            }
            redraw(hwnd);
        }
        if self.drag {
            self.drag = false;
            // SAFETY: the mouse was captured in on_mouse_down.
            unsafe {
                ReleaseCapture();
            }
        }
    }

    fn on_double_click(&mut self, hwnd: HWND, lp: LPARAM) {
        let (x, cy) = point_of(lp);
        let hb = self.tabbar_h();
        if cy < hb {
            // Empty strip space opens a tab. On a tab or on "+" the press already did the work.
            if self.tab_strip().and_then(|s| s.hit(x, cy)).is_none() {
                self.new_tab(hwnd);
            }
            return;
        }
        let y = cy - hb;
        let lay = self.layout();
        // The second press of a double click arrives as this message, so the bars page
        // again on it, as a second click would.
        if Self::in_bar_strip(lay, x, y) {
            self.bar_press(hwnd, lay, x, y);
            return;
        }
        if !self.text_click_ok(y) {
            return;
        }
        let pos = self.pos_at(lay, x, y);
        let buf = &self.editor.buf;
        let line = buf.line_of(pos);
        let (ls, le) = (buf.line_start(line), buf.line_end(line));
        let mut start = pos.min(le);
        while start > ls && is_word_byte(buf.byte_at(start - 1)) {
            start -= 1;
        }
        let mut end = pos.min(le);
        while end < le && is_word_byte(buf.byte_at(end)) {
            end += 1;
        }
        self.editor.set_cursor(start, false);
        self.editor.set_cursor(end, true);
        self.after_command(hwnd);
    }

    fn on_wheel(&mut self, hwnd: HWND, wp: WPARAM) {
        let delta = wheel_delta(wp);
        // Over the tab strip the wheel steps through the tabs: up is the previous tab.
        let (_, cy) = Self::cursor_in_client(hwnd);
        if (0..self.tabbar_h()).contains(&cy) {
            self.wheel_t += delta;
            let notches = self.wheel_t / WHEEL_DELTA as i32;
            self.wheel_t %= WHEEL_DELTA as i32;
            if notches != 0 {
                let n = self.tab_count() as isize;
                let i = (self.active as isize - notches as isize).rem_euclid(n) as usize;
                self.activate(hwnd, i);
            }
            return;
        }
        // Ctrl+wheel zooms, one step per full notch (precision touchpads send small deltas).
        if wp & MK_CONTROL_FLAG != 0 {
            self.wheel_z += delta;
            let notches = self.wheel_z / WHEEL_DELTA as i32;
            self.wheel_z %= WHEEL_DELTA as i32;
            if notches != 0 {
                self.change_zoom(hwnd, notches);
            }
            return;
        }
        // Shift+wheel scrolls sideways, like most editors; wheel up = left.
        if wp & MK_SHIFT_FLAG != 0 {
            self.scroll_h_wheel(hwnd, -delta);
            return;
        }
        self.wheel_v += delta;
        let notches = self.wheel_v / WHEEL_DELTA as i32;
        self.wheel_v %= WHEEL_DELTA as i32;
        if notches != 0 {
            // Honour the user's "lines per notch" setting, including "one screen".
            let per = match system_param(SPI_GETWHEELSCROLLLINES, V_WHEEL_LINES as u32) {
                u32::MAX => self.layout().rows.max(1) as isize,
                n => n as isize,
            };
            if per > 0 {
                self.editor.scroll_by(-(notches as isize) * per);
                redraw(hwnd);
            }
        }
    }

    fn on_hwheel(&mut self, hwnd: HWND, wp: WPARAM) {
        self.scroll_h_wheel(hwnd, wheel_delta(wp));
    }

    /// Horizontal wheel motion; positive `delta` scrolls right.
    fn scroll_h_wheel(&mut self, hwnd: HWND, delta: i32) {
        self.wheel_h += delta;
        let notches = self.wheel_h / WHEEL_DELTA as i32;
        self.wheel_h %= WHEEL_DELTA as i32;
        if notches != 0 {
            let per = system_param(SPI_GETWHEELSCROLLCHARS, H_STEP as u32) as isize;
            self.scroll_cols(notches as isize * per);
            redraw(hwnd);
        }
    }

    fn scroll_cols(&mut self, n: isize) {
        if self.editor.wrap {
            return;
        }
        self.editor.scroll_col = (self.editor.scroll_col as isize + n).max(0) as usize;
    }
}

/// Requests a repaint of the whole client area.
fn redraw(hwnd: HWND) {
    // SAFETY: a null rect invalidates the whole client area of a valid window.
    unsafe {
        InvalidateRect(hwnd, null(), 0);
    }
}

/// Client rectangle of the caret, or None when it is not visible.
fn caret_rect(app: &App) -> Option<RECT> {
    let lay = app.layout();
    let (x, y) = render::caret_spot(app, lay)?;
    // The spot is in editor coordinates; the rectangle goes to the window, below the strip.
    let top = y + lay.tabbar_h;
    Some(RECT {
        left: x,
        top,
        right: x + app.config.caret_width.max(1),
        bottom: top + lay.lh,
    })
}

/// Native open dialog. Returns the chosen path.
fn open_dialog(hwnd: HWND) -> Option<String> {
    let filter: Vec<u16> = OPEN_FILTER.encode_utf16().collect();
    let mut buf = vec![0u16; PATH_CHARS];
    // SAFETY: OPENFILENAMEW is plain data; the buffers outlive the call.
    unsafe {
        let mut ofn: OPENFILENAMEW = zeroed();
        ofn.lStructSize = size_of::<OPENFILENAMEW>() as u32;
        ofn.hwndOwner = hwnd;
        ofn.lpstrFilter = filter.as_ptr();
        ofn.nFilterIndex = 1;
        ofn.lpstrFile = buf.as_mut_ptr();
        ofn.nMaxFile = PATH_CHARS as u32;
        ofn.Flags = OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_EXPLORER;
        if GetOpenFileNameW(&mut ofn) == 0 {
            return None;
        }
    }
    Some(from_wide(&buf))
}

/// Save As, prefilled with `current`. Returns the path to write.
///
/// With "Text Documents" chosen in the type list, a name typed without an extension gets
/// .txt ("notes" saves as notes.txt, "notes.md" as notes.md). "All Files" keeps the name
/// exactly as typed. New documents and .txt files start on "Text Documents"; other files
/// start on "All Files" so a name like LICENSE is not changed.
fn save_dialog(hwnd: HWND, current: Option<&str>) -> Option<String> {
    let mut filter_index = match current {
        Some(p) if !p.to_ascii_lowercase().ends_with(".txt") => SAVE_FILTER_ALL,
        _ => SAVE_FILTER_TEXT,
    };
    let mut name = current.map(str::to_owned);
    loop {
        let (chosen, index) = save_dialog_once(hwnd, name.as_deref(), filter_index)?;
        let ext = (index == SAVE_FILTER_TEXT).then_some("txt");
        let path = save_as_path(&chosen, ext);
        // The extension is added after the dialog closes, so the overwrite check is done
        // here, on the final name, instead of by the dialog. "No" goes back to the dialog.
        if !std::path::Path::new(&path).exists() || confirm_overwrite(hwnd, &path) {
            return Some(path);
        }
        name = Some(path);
        filter_index = index;
    }
}

/// Native save dialog, prefilled with `current`, opened on the `filter_index` entry of
/// SAVE_FILTER. Returns the chosen path and the type-list entry selected when it closed.
fn save_dialog_once(hwnd: HWND, current: Option<&str>, filter_index: u32) -> Option<(String, u32)> {
    let filter: Vec<u16> = SAVE_FILTER.encode_utf16().collect();
    let mut buf = vec![0u16; PATH_CHARS];
    if let Some(c) = current {
        for (dst, src) in buf.iter_mut().take(PATH_CHARS - 1).zip(c.encode_utf16()) {
            *dst = src;
        }
    }
    // SAFETY: OPENFILENAMEW is plain data; the buffers outlive the call.
    let index = unsafe {
        let mut ofn: OPENFILENAMEW = zeroed();
        ofn.lStructSize = size_of::<OPENFILENAMEW>() as u32;
        ofn.hwndOwner = hwnd;
        ofn.lpstrFilter = filter.as_ptr();
        ofn.nFilterIndex = filter_index;
        ofn.lpstrFile = buf.as_mut_ptr();
        ofn.nMaxFile = PATH_CHARS as u32;
        ofn.Flags = OFN_PATHMUSTEXIST | OFN_EXPLORER;
        if GetSaveFileNameW(&mut ofn) == 0 {
            return None;
        }
        ofn.nFilterIndex
    };
    let path = from_wide(&buf);
    (!path.is_empty()).then_some((path, index))
}

/// Asks before replacing an existing file. True means replace.
fn confirm_overwrite(hwnd: HWND, path: &str) -> bool {
    let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
    let text = wide(&format!(
        "{name} already exists.\nDo you want to replace it?"
    ));
    let caption = wide("Confirm Save As");
    // SAFETY: both strings are NUL-terminated and outlive the call.
    unsafe {
        MessageBoxW(
            hwnd,
            text.as_ptr(),
            caption.as_ptr(),
            MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
        ) == IDYES
    }
}

/// Reads CF_UNICODETEXT from the clipboard.
fn clipboard_get(hwnd: HWND) -> Option<String> {
    // SAFETY: the handle is locked while read and unlocked after; the clipboard is closed
    // on every path.
    unsafe {
        if OpenClipboard(hwnd) == 0 {
            return None;
        }
        let mut out = None;
        let h = GetClipboardData(CF_UNICODETEXT as u32);
        if !h.is_null() {
            let p = GlobalLock(h) as *const u16;
            if !p.is_null() {
                let mut len = 0usize;
                while *p.add(len) != 0 {
                    len += 1;
                }
                out = Some(String::from_utf16_lossy(std::slice::from_raw_parts(p, len)));
                GlobalUnlock(h);
            }
        }
        CloseClipboard();
        out
    }
}

/// Puts `text` (LF line endings) on the clipboard as CRLF CF_UNICODETEXT.
fn clipboard_set(hwnd: HWND, text: &[u8]) -> bool {
    let crlf = simd::lf_to_crlf(text);
    let w = wide(&String::from_utf8_lossy(&crlf));
    // SAFETY: the global block is sized for `w`, filled while locked, and given to the
    // clipboard only once the clipboard is open. It is freed on every failure path.
    unsafe {
        let h = GlobalAlloc(GMEM_MOVEABLE, w.len() * size_of::<u16>());
        if h.is_null() {
            return false;
        }
        let p = GlobalLock(h) as *mut u16;
        if p.is_null() {
            GlobalFree(h);
            return false;
        }
        std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
        GlobalUnlock(h);
        if OpenClipboard(hwnd) == 0 {
            GlobalFree(h);
            return false;
        }
        EmptyClipboard();
        let ok = !SetClipboardData(CF_UNICODETEXT as u32, h).is_null();
        CloseClipboard();
        if !ok {
            GlobalFree(h);
        }
        ok
    }
}
