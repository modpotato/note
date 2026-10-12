//! Win32 window layer: application state, window procedure, keyboard and mouse input,
//! nano-style prompts, file I/O and the clipboard. Painting lives in `render`.

mod render;

use std::ffi::c_void;
use std::io::ErrorKind;
use std::iter::once;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};

use windows_sys::core::{PCSTR, PCWSTR};
use windows_sys::Win32::Foundation::{GlobalFree, FARPROC, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, InvalidateRect, UpdateWindow, PAINTSTRUCT,
};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows_sys::Win32::System::Ole::CF_UNICODETEXT;
use windows_sys::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, GetSaveFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_OVERWRITEPROMPT,
    OFN_PATHMUSTEXIST, OPENFILENAMEW,
};
use windows_sys::Win32::UI::Controls::{SetScrollInfo, SetWindowTheme};
use windows_sys::Win32::UI::HiDpi::{
    GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, VK_CONTROL, VK_LWIN, VK_MENU, VK_RMENU, VK_RWIN,
    VK_SHIFT,
};
use windows_sys::Win32::UI::Shell::{DragFinish, DragQueryFileW, HDROP};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetMessageW,
    GetScrollInfo, GetWindowLongPtrW, KillTimer, LoadCursorW, LoadIconW, PostQuitMessage,
    RegisterClassExW, SetTimer, SetWindowLongPtrW, SetWindowPos, SetWindowTextW, ShowWindow,
    TranslateMessage, CREATESTRUCTW, CS_DBLCLKS, CW_USEDEFAULT, GWLP_USERDATA, IDC_IBEAM,
    IDI_APPLICATION, MSG, SB_BOTTOM, SB_HORZ, SB_LINEDOWN, SB_LINEUP, SB_PAGEDOWN, SB_PAGEUP,
    SB_THUMBPOSITION, SB_THUMBTRACK, SB_TOP, SB_VERT, SCROLLINFO, SIF_PAGE, SIF_POS, SIF_RANGE,
    SIF_TRACKPOS, SWP_NOACTIVATE, SWP_NOZORDER, SW_SHOW, WHEEL_DELTA, WM_CHAR, WM_CLOSE, WM_CREATE,
    WM_DESTROY, WM_DPICHANGED, WM_DROPFILES, WM_ERASEBKGND, WM_HSCROLL, WM_KEYDOWN, WM_KEYUP,
    WM_KILLFOCUS, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE,
    WM_MOUSEWHEEL, WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_SETFOCUS, WM_SIZE, WM_SYSCHAR,
    WM_SYSKEYDOWN, WM_SYSKEYUP, WM_TIMER, WM_VSCROLL, WNDCLASSEXW, WS_EX_ACCEPTFILES, WS_HSCROLL,
    WS_OVERLAPPEDWINDOW, WS_VSCROLL,
};

use crate::config::{
    Action, Chord, Config, VK_BACK, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME, VK_LEFT,
    VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_TAB, VK_UP,
};
use crate::editor::{Editor, Move};
use crate::simd;
use crate::syntax::Lang;
use render::Renderer;

const TIMER_BLINK: usize = 1;
const BLINK_MS: u32 = 530;
const MIN_PT: i32 = 6;
const MAX_PT: i32 = 72;
/// Capacity, in UTF-16 units, of the open/save dialog path buffer.
const PATH_CHARS: usize = 32768;
const WINDOW_CLASS: &str = "note.window";
const DIALOG_FILTER: &str = "All Files\0*.*\0Text\0*.txt\0\0";
/// Win32 MK_LBUTTON and MK_CONTROL key-state flags (WM_MOUSEMOVE / WM_MOUSEWHEEL wParam).
const MK_LBUTTON_FLAG: usize = 0x0001;
const MK_CONTROL_FLAG: usize = 0x0008;
/// Cells moved per horizontal wheel notch or scrollbar arrow.
const H_STEP: isize = 4;
/// Lines moved per vertical wheel notch.
const V_WHEEL_LINES: isize = 3;

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

/// Dark title bar, dark scrollbars and dark-mode allowance for one window.
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
        let theme = wide("DarkMode_Explorer");
        SetWindowTheme(hwnd, theme.as_ptr(), null());
    }
}

/// Starts the editor: DPI awareness, config, optional file argument, window, message loop.
pub fn run() {
    // SAFETY: plain call without pointer arguments.
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let config = load_config();
    let bg = config.theme.bg;
    let dark = luminance(bg.0, bg.1, bg.2) < 128;
    if dark {
        enable_dark_app_mode();
    }
    let file = std::env::args_os()
        .nth(1)
        .map(|a| a.to_string_lossy().into_owned());
    // Owned by the window from here on: freed in WM_NCDESTROY (or below on failure).
    let raw = Box::into_raw(Box::new(App::new(config, file, dark)));

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
            WS_OVERLAPPEDWINDOW | WS_VSCROLL | WS_HSCROLL,
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

        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Perceptual brightness on 0..=255, as used for the dark-mode decision.
fn luminance(r: u8, g: u8, b: u8) -> u32 {
    (r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000
}

/// Reads `%APPDATA%\note\note.ini`, creating it with the defaults when missing.
fn load_config() -> Config {
    let Ok(appdata) = std::env::var("APPDATA") else {
        return Config::default();
    };
    let dir = std::path::PathBuf::from(appdata).join("note");
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
    let app = &mut *p;
    let res = handle(app, hwnd, msg, wp, lp);
    if app.quit {
        // Last use of `app`; DestroyWindow frees it through WM_NCDESTROY.
        DestroyWindow(hwnd);
        return 0;
    }
    res.unwrap_or_else(|| DefWindowProcW(hwnd, msg, wp, lp))
}

/// Dispatches one message. `None` means "let DefWindowProc handle it".
fn handle(app: &mut App, hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    match msg {
        WM_CREATE => app.on_create(hwnd),
        WM_DESTROY => unsafe { PostQuitMessage(0) },
        WM_CLOSE => app.request(hwnd, Pending::Exit),
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
        WM_LBUTTONUP => app.on_mouse_up(),
        WM_MOUSEWHEEL => app.on_wheel(hwnd, wp),
        WM_MOUSEHWHEEL => app.on_hwheel(hwnd, wp),
        WM_VSCROLL => app.on_scroll(hwnd, wp, SB_VERT),
        WM_HSCROLL => app.on_scroll(hwnd, wp, SB_HORZ),
        WM_DROPFILES => app.on_drop(hwnd, wp as HDROP),
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

/// Signed wheel delta from a wheel message's wParam.
fn wheel_delta(wp: WPARAM) -> i32 {
    ((wp >> 16) & 0xFFFF) as u16 as i16 as i32
}

/// Pixel and cell geometry for the current client size and font.
#[derive(Clone, Copy)]
struct Layout {
    w: i32,
    h: i32,
    lh: i32,
    cw: i32,
    /// Height of the text area, above the status row.
    text_h: i32,
    /// Visible text rows (at least 1).
    rows: usize,
    /// Visible columns right of the gutter (at least 1).
    cols: usize,
    /// Gutter width in pixels, 0 when line numbers are off.
    gutter: i32,
}

/// Command that waits for the unsaved-changes answer, or runs right away.
#[derive(Clone)]
enum Pending {
    Exit,
    New,
    Open,
    OpenPath(String),
}

#[derive(Clone)]
enum PromptKind {
    Search,
    ReplaceSearch,
    ReplaceWith,
    GotoLine,
    ReplaceConfirm,
    Confirm(Pending),
}

impl PromptKind {
    /// Text prompts take a line of input; the others only take single keys.
    fn takes_text(&self) -> bool {
        !matches!(self, PromptKind::ReplaceConfirm | PromptKind::Confirm(_))
    }

    fn is_search(&self) -> bool {
        matches!(self, PromptKind::Search | PromptKind::ReplaceSearch)
    }
}

/// nano-style prompt shown in the status row.
struct Prompt {
    kind: PromptKind,
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

fn prompt_label(kind: &PromptKind, case_sensitive: bool) -> String {
    let tag = if case_sensitive { " [Case]" } else { "" };
    match kind {
        PromptKind::Search => format!("Search{tag}: "),
        PromptKind::ReplaceSearch => format!("Search (to replace){tag}: "),
        PromptKind::ReplaceWith => "Replace with: ".to_string(),
        PromptKind::GotoLine => "Enter line number, column number: ".to_string(),
        PromptKind::ReplaceConfirm => "Replace this instance? (Y)es (N)o (A)ll (Esc)".to_string(),
        PromptKind::Confirm(_) => "Save modified buffer? (Y)es (N)o (C)ancel".to_string(),
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
    editor: Editor,
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
    /// Left button is held after a click in the text area.
    drag: bool,
    show_help: bool,
    help_scroll: usize,
    /// Set while the WM_CHAR of a key we already handled must be swallowed.
    suppress_char: bool,
    high_surrogate: u16,
    wheel_v: i32,
    wheel_h: i32,
    last_title: String,
    dark: bool,
    /// Set when the window must be destroyed once the current message is done.
    quit: bool,
}

impl App {
    fn new(config: Config, file: Option<String>, dark: bool) -> App {
        let mut editor = Editor::new(&config);
        if let Some(path) = file {
            match std::fs::read(&path) {
                Ok(bytes) => editor.load_owned(&path, bytes),
                Err(e) if e.kind() == ErrorKind::NotFound => {
                    editor.load_owned(&path, Vec::new());
                    editor.message = Some("New file".to_string());
                }
                Err(e) => editor.message = Some(format!("Cannot open {path}: {e}")),
            }
        }
        let renderer = Renderer::new(&config);
        App {
            editor,
            config,
            renderer,
            zoom: 0,
            dpi: 96,
            client_w: 1,
            client_h: 1,
            caret_visible: true,
            focused: false,
            prompt: None,
            drag: false,
            show_help: false,
            help_scroll: 0,
            suppress_char: false,
            high_surrogate: 0,
            wheel_v: 0,
            wheel_h: 0,
            last_title: String::new(),
            dark,
            quit: false,
        }
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
        self.renderer.set_font(&self.config.font_face, pt, self.dpi);
    }

    fn change_zoom(&mut self, hwnd: HWND, delta: i32) {
        let pt = (self.pt() + delta).clamp(MIN_PT, MAX_PT);
        self.zoom = pt - self.config.font_size;
        self.apply_font();
        self.after_command(hwnd);
    }

    /// Current geometry. Rows and columns are at least 1 so scrolling math stays valid.
    fn layout(&self) -> Layout {
        let lh = self.renderer.line_h.max(1);
        let cw = self.renderer.char_w.max(1);
        let w = self.client_w.max(1);
        let h = self.client_h.max(1);
        let hint_rows = if self.config.show_hints { 2 } else { 0 };
        let text_h = (h - (1 + hint_rows) * lh).max(0);
        let rows = ((text_h / lh) as usize).max(1);
        let gutter = if self.config.line_numbers {
            let d = digits(self.editor.buf.line_count()).max(3) as i32;
            (d + 2) * cw
        } else {
            0
        };
        let cols = ((w - gutter) / cw).max(1) as usize;
        Layout {
            w,
            h,
            lh,
            cw,
            text_h,
            rows,
            cols,
            gutter,
        }
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

    /// Common tail of every command: keep the cursor visible, sync title and scrollbars,
    /// restart the caret blink and repaint.
    fn after_command(&mut self, hwnd: HWND) {
        let lay = self.layout();
        self.editor.ensure_cursor_visible(lay.rows, lay.cols);
        self.sync_title(hwnd);
        self.caret_visible = true;
        self.start_blink(hwnd);
        self.update_scrollbars(hwnd);
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

    fn update_scrollbars(&self, hwnd: HWND) {
        let lay = self.layout();
        let lines = self.editor.buf.line_count() as i32;
        set_scroll(
            hwnd,
            SB_VERT,
            lines - 1,
            lay.rows as u32,
            self.editor.scroll_line as i32,
        );
        let span = (self.editor.scroll_col + lay.cols * 2).max(256) as i32;
        set_scroll(
            hwnd,
            SB_HORZ,
            span - 1,
            lay.cols as u32,
            self.editor.scroll_col as i32,
        );
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
        let Some(kind) = self.prompt.as_ref().map(|p| p.kind.clone()) else {
            return false;
        };
        if !kind.takes_text() {
            self.confirm_key(hwnd, kind, vk);
            return true;
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
            0x43 if m.alt && kind.is_search() => {
                // Alt+C toggles case sensitivity for the search text.
                self.editor.search_case_sensitive = !self.editor.search_case_sensitive;
                let cs = self.editor.search_case_sensitive;
                if let Some(p) = self.prompt.as_mut() {
                    p.label = prompt_label(&p.kind, cs);
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

    /// Y / N / A / C / Esc answers for the single-key prompts.
    fn confirm_key(&mut self, hwnd: HWND, kind: PromptKind, vk: u16) {
        const KEY_Y: u16 = b'Y' as u16;
        const KEY_N: u16 = b'N' as u16;
        const KEY_A: u16 = b'A' as u16;
        const KEY_C: u16 = b'C' as u16;
        match kind {
            PromptKind::Confirm(pending) => match vk {
                KEY_Y => {
                    self.prompt = None;
                    if self.save(hwnd) {
                        self.perform(hwnd, pending);
                    }
                }
                KEY_N => {
                    self.prompt = None;
                    self.perform(hwnd, pending);
                }
                KEY_C | VK_ESCAPE => self.prompt = None,
                _ => {}
            },
            PromptKind::ReplaceConfirm => {
                let more = match vk {
                    KEY_Y => self.editor.replace_one(),
                    KEY_N => self.editor.find_next(true),
                    KEY_A => {
                        self.editor.replace_all();
                        false
                    }
                    VK_ESCAPE => false,
                    _ => true,
                };
                if !more {
                    self.prompt = None;
                }
            }
            _ => {}
        }
        self.after_command(hwnd);
    }

    /// Enter in a text prompt: does the prompt's job and usually closes it.
    fn prompt_confirm(&mut self, hwnd: HWND) {
        let Some(prompt) = self.prompt.take() else {
            return;
        };
        match prompt.kind {
            PromptKind::Search => {
                self.editor.search = prompt.text.into_bytes();
                self.editor.find_next(true);
            }
            PromptKind::ReplaceSearch => {
                if prompt.text.is_empty() {
                    self.editor.message = Some("No search text".to_string());
                } else {
                    self.editor.search = prompt.text.into_bytes();
                    let prefill = String::from_utf8_lossy(&self.editor.replace_with).into_owned();
                    self.open_prompt(PromptKind::ReplaceWith, prefill);
                }
            }
            PromptKind::ReplaceWith => {
                self.editor.replace_with = prompt.text.into_bytes();
                if self.editor.find_next(true) {
                    self.open_prompt(PromptKind::ReplaceConfirm, String::new());
                }
            }
            PromptKind::GotoLine => {
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
            }
            PromptKind::ReplaceConfirm | PromptKind::Confirm(_) => {}
        }
        self.after_command(hwnd);
    }

    fn open_prompt(&mut self, kind: PromptKind, text: String) {
        let label = prompt_label(&kind, self.editor.search_case_sensitive);
        let cursor = text.len();
        self.prompt = Some(Prompt {
            kind,
            label,
            text,
            cursor,
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
            if p.kind.takes_text() {
                let mut b = [0u8; 4];
                p.insert(ch.encode_utf8(&mut b));
                self.after_command(hwnd);
            }
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
            Action::Open => self.request(hwnd, Pending::Open),
            Action::New => self.request(hwnd, Pending::New),
            Action::Exit => self.request(hwnd, Pending::Exit),
            Action::CutLine => {
                let text = self.editor.cut_line();
                clipboard_set(hwnd, &text);
            }
            Action::Paste => self.paste(hwnd),
            Action::Copy => self.copy(hwnd),
            Action::Cut => self.cut(hwnd),
            Action::Find => {
                let text = String::from_utf8_lossy(&self.editor.search).into_owned();
                self.open_prompt(PromptKind::Search, text);
            }
            Action::FindNext => self.find_repeat(true),
            Action::FindPrev => self.find_repeat(false),
            Action::Replace => {
                let text = String::from_utf8_lossy(&self.editor.search).into_owned();
                self.open_prompt(PromptKind::ReplaceSearch, text);
            }
            Action::GotoLine => self.open_prompt(PromptKind::GotoLine, String::new()),
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
                self.editor.message = Some("Soft wrap not supported yet".to_string());
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

    /// Repeats the last search, or asks for one when there is none yet.
    fn find_repeat(&mut self, forward: bool) {
        if self.editor.search.is_empty() {
            self.open_prompt(PromptKind::Search, String::new());
        } else {
            self.editor.find_next(forward);
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

    /// Runs `p` now, or after the unsaved-changes prompt when the buffer is dirty.
    fn request(&mut self, hwnd: HWND, p: Pending) {
        if self.editor.is_dirty() {
            self.open_prompt(PromptKind::Confirm(p), String::new());
        } else {
            self.perform(hwnd, p);
        }
    }

    fn perform(&mut self, hwnd: HWND, p: Pending) {
        match p {
            Pending::Exit => self.quit = true,
            Pending::New => self.editor = Editor::new(&self.config),
            Pending::Open => {
                if let Some(path) = open_dialog(hwnd) {
                    self.load_path(&path);
                }
            }
            Pending::OpenPath(path) => self.load_path(&path),
        }
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
        let path = unsafe {
            let n = DragQueryFileW(hdrop, 0, null_mut(), 0);
            let path = if n > 0 {
                let mut buf = vec![0u16; n as usize + 1];
                DragQueryFileW(hdrop, 0, buf.as_mut_ptr(), n + 1);
                Some(from_wide(&buf))
            } else {
                None
            };
            DragFinish(hdrop);
            path
        };
        if let Some(path) = path {
            self.request(hwnd, Pending::OpenPath(path));
        }
    }

    // ----- mouse and wheel ---------------------------------------------------

    /// Byte offset under a client point, clamped to the visible text area.
    fn pos_at(&self, lay: Layout, x: i32, y: i32) -> usize {
        let row = ((y.max(0) / lay.lh) as usize).min(lay.rows - 1);
        let last = self.editor.buf.line_count() - 1;
        let line = (self.editor.scroll_line + row).min(last);
        let dx = x - lay.gutter;
        let col = if dx <= 0 {
            0
        } else {
            ((dx + lay.cw / 2) / lay.cw) as usize
        };
        self.editor
            .pos_from_visual(line, col + self.editor.scroll_col)
    }

    /// True when a mouse point lands in the text area and the mouse is free to use.
    fn text_click_ok(&self, y: i32) -> bool {
        !self.show_help && self.prompt.is_none() && y >= 0 && y < self.layout().text_h
    }

    fn on_mouse_down(&mut self, hwnd: HWND, lp: LPARAM) {
        let (x, y) = point_of(lp);
        if !self.text_click_ok(y) {
            return;
        }
        let pos = self.pos_at(self.layout(), x, y);
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
        if !self.drag {
            return;
        }
        if wp & MK_LBUTTON_FLAG == 0 {
            self.on_mouse_up();
            return;
        }
        let (x, mut y) = point_of(lp);
        let lay = self.layout();
        // Autoscroll while dragging above or below the text area.
        if y < 0 {
            self.editor.scroll_by(-1);
            y = 0;
        } else if y >= lay.text_h {
            self.editor.scroll_by(1);
            y = lay.text_h - 1;
        }
        let pos = self.pos_at(lay, x, y.max(0));
        self.editor.set_cursor(pos, true);
        self.after_command(hwnd);
    }

    fn on_mouse_up(&mut self) {
        if self.drag {
            self.drag = false;
            // SAFETY: the mouse was captured in on_mouse_down.
            unsafe {
                ReleaseCapture();
            }
        }
    }

    fn on_double_click(&mut self, hwnd: HWND, lp: LPARAM) {
        let (x, y) = point_of(lp);
        if !self.text_click_ok(y) {
            return;
        }
        let pos = self.pos_at(self.layout(), x, y);
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
        if wp & MK_CONTROL_FLAG != 0 {
            self.change_zoom(hwnd, if delta > 0 { 1 } else { -1 });
            return;
        }
        self.wheel_v += delta;
        let notches = self.wheel_v / WHEEL_DELTA as i32;
        self.wheel_v %= WHEEL_DELTA as i32;
        if notches != 0 {
            self.editor.scroll_by(-(notches as isize) * V_WHEEL_LINES);
            self.update_scrollbars(hwnd);
            redraw(hwnd);
        }
    }

    fn on_hwheel(&mut self, hwnd: HWND, wp: WPARAM) {
        self.wheel_h += wheel_delta(wp);
        let notches = self.wheel_h / WHEEL_DELTA as i32;
        self.wheel_h %= WHEEL_DELTA as i32;
        if notches != 0 {
            self.scroll_cols(notches as isize * H_STEP);
            self.update_scrollbars(hwnd);
            redraw(hwnd);
        }
    }

    fn scroll_cols(&mut self, n: isize) {
        self.editor.scroll_col = (self.editor.scroll_col as isize + n).max(0) as usize;
    }

    /// Scrollbar messages. Both bars share the same codes, so `bar` picks the axis.
    fn on_scroll(&mut self, hwnd: HWND, wp: WPARAM, bar: i32) {
        let code = (wp & 0xFFFF) as i32;
        let lay = self.layout();
        let vertical = bar == SB_VERT;
        let page = (if vertical { lay.rows } else { lay.cols }) as isize;
        let step: isize = if vertical { 1 } else { H_STEP };
        let last = self.editor.buf.line_count() - 1;
        match code {
            SB_LINEUP => self.scroll_axis(vertical, -step),
            SB_LINEDOWN => self.scroll_axis(vertical, step),
            SB_PAGEUP => self.scroll_axis(vertical, -page),
            SB_PAGEDOWN => self.scroll_axis(vertical, page),
            SB_TOP if vertical => self.editor.scroll_line = 0,
            SB_TOP => self.editor.scroll_col = 0,
            SB_BOTTOM if vertical => self.editor.scroll_line = last,
            SB_THUMBTRACK | SB_THUMBPOSITION => {
                let pos = track_position(hwnd, bar);
                if vertical {
                    self.editor.scroll_line = pos.min(last);
                } else {
                    self.editor.scroll_col = pos;
                }
            }
            _ => return,
        }
        self.update_scrollbars(hwnd);
        redraw(hwnd);
    }

    fn scroll_axis(&mut self, vertical: bool, n: isize) {
        if vertical {
            self.editor.scroll_by(n);
        } else {
            self.scroll_cols(n);
        }
    }
}

/// Requests a repaint of the whole client area.
fn redraw(hwnd: HWND) {
    // SAFETY: a null rect invalidates the whole client area of a valid window.
    unsafe {
        InvalidateRect(hwnd, null(), 0);
    }
}

/// 32-bit thumb position of a scrollbar being dragged.
fn track_position(hwnd: HWND, bar: i32) -> usize {
    // SAFETY: SCROLLINFO is plain data; SIF_TRACKPOS asks for the 32-bit thumb position.
    unsafe {
        let mut si: SCROLLINFO = zeroed();
        si.cbSize = size_of::<SCROLLINFO>() as u32;
        si.fMask = SIF_TRACKPOS;
        GetScrollInfo(hwnd, bar, &mut si);
        si.nTrackPos.max(0) as usize
    }
}

fn set_scroll(hwnd: HWND, bar: i32, max: i32, page: u32, pos: i32) {
    let si = SCROLLINFO {
        cbSize: size_of::<SCROLLINFO>() as u32,
        fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
        nMin: 0,
        nMax: max.max(0),
        nPage: page,
        nPos: pos,
        nTrackPos: 0,
    };
    // SAFETY: si is fully initialised and lives for the call.
    unsafe {
        SetScrollInfo(hwnd, bar, &si, 1);
    }
}

/// Client rectangle of the caret, or None when it is not visible.
fn caret_rect(app: &App) -> Option<RECT> {
    let lay = app.layout();
    let (x, y) = render::caret_spot(app, lay)?;
    Some(RECT {
        left: x,
        top: y,
        right: x + app.config.caret_width.max(1),
        bottom: y + lay.lh,
    })
}

/// Native open dialog. Returns the chosen path.
fn open_dialog(hwnd: HWND) -> Option<String> {
    let filter: Vec<u16> = DIALOG_FILTER.encode_utf16().collect();
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

/// Native save dialog, prefilled with `current`. Returns the chosen path.
fn save_dialog(hwnd: HWND, current: Option<&str>) -> Option<String> {
    let filter: Vec<u16> = DIALOG_FILTER.encode_utf16().collect();
    let mut buf = vec![0u16; PATH_CHARS];
    if let Some(c) = current {
        for (dst, src) in buf.iter_mut().take(PATH_CHARS - 1).zip(c.encode_utf16()) {
            *dst = src;
        }
    }
    // SAFETY: OPENFILENAMEW is plain data; the buffers outlive the call.
    unsafe {
        let mut ofn: OPENFILENAMEW = zeroed();
        ofn.lStructSize = size_of::<OPENFILENAMEW>() as u32;
        ofn.hwndOwner = hwnd;
        ofn.lpstrFilter = filter.as_ptr();
        ofn.nFilterIndex = 1;
        ofn.lpstrFile = buf.as_mut_ptr();
        ofn.nMaxFile = PATH_CHARS as u32;
        ofn.Flags = OFN_OVERWRITEPROMPT | OFN_PATHMUSTEXIST | OFN_EXPLORER;
        if GetSaveFileNameW(&mut ofn) == 0 {
            return None;
        }
    }
    let path = from_wide(&buf);
    (!path.is_empty()).then_some(path)
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
