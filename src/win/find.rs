//! Find/Replace dialog in the style of EmEditor's: a modeless native dialog built from an
//! in-memory template, the live preview it drives, and its regex helpers.
//!
//! The dialog procedure only forwards notifications to the main window as `WM_FIND`
//! messages; `App` reads the controls there and runs the search, so editor state is only
//! touched from the main window procedure. In regex mode, typing `\`, `(?`, `[:` or `$` opens
//! a completion list under the caret (a subclassed edit and a popup list box), and the ">"
//! buttons open menus of tokens.

use std::ffi::c_void;
use std::mem::{size_of, transmute, zeroed};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicPtr, Ordering};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::{
    CheckDlgButton, CheckRadioButton, GetComboBoxInfo, IsDlgButtonChecked, BST_CHECKED,
    BST_UNCHECKED, COMBOBOXINFO, EM_GETSEL, EM_POSFROMCHAR, EM_REPLACESEL, EM_SETSEL,
};
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetKeyState, SetFocus, VK_CONTROL, VK_SHIFT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CallWindowProcW, CreateDialogIndirectParamW, CreatePopupMenu, CreateWindowExW,
    DestroyMenu, GetClientRect, GetDlgItem, GetWindow, GetWindowLongPtrW, GetWindowRect,
    GetWindowTextLengthW, GetWindowTextW, IsChild, IsDialogMessageW, IsWindowVisible,
    MapDialogRect, PostMessageW, SendMessageW, SetWindowLongPtrW, SetWindowPos, SetWindowTextW,
    ShowWindow, TrackPopupMenu, BN_CLICKED, CBN_EDITCHANGE, CBN_SELCHANGE, CB_DELETESTRING,
    CB_FINDSTRINGEXACT, CB_GETCOUNT, CB_INSERTSTRING, CB_SETEDITSEL, DLGC_WANTALLKEYS, DLGTEMPLATE,
    DM_SETDEFID, GWLP_USERDATA, GWLP_WNDPROC, GW_OWNER, HWND_TOPMOST, IDCANCEL,
    LBS_NOINTEGRALHEIGHT, LBS_NOTIFY, LBS_USETABSTOPS, LB_ADDSTRING, LB_GETCOUNT, LB_GETCURSEL,
    LB_GETITEMHEIGHT, LB_ITEMFROMPOINT, LB_RESETCONTENT, LB_SETCURSEL, LB_SETTABSTOPS,
    MA_NOACTIVATE, MF_SEPARATOR, MF_STRING, MSG, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SWP_NOZORDER, SWP_SHOWWINDOW, SW_HIDE, SW_SHOW, TPM_NONOTIFY, TPM_RETURNCMD, WM_APP, WM_CHAR,
    WM_CLOSE, WM_COMMAND, WM_GETDLGCODE, WM_GETFONT, WM_INITDIALOG, WM_KEYDOWN, WM_KILLFOCUS,
    WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_MOUSEACTIVATE, WM_NCDESTROY, WM_SETFONT, WM_SYSKEYDOWN,
    WNDPROC, WS_BORDER, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VSCROLL,
};

use super::{redraw, wide, App};
use crate::config::{
    VK_DOWN, VK_END, VK_ESCAPE, VK_F1, VK_HOME, VK_LEFT, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT,
    VK_TAB, VK_UP,
};
use crate::regex::TooComplex;
use crate::search::{
    completions, Completion, Field, Hit, Mode, Query, QueryError, Scope, Searcher, Token,
    FIND_BUDGET, FIND_TOKENS, PREVIEW_BUDGET, REPLACE_TOKENS,
};

/// Dialog notifications forwarded to the main window. wParam is the WM_COMMAND wParam.
pub(super) const WM_FIND: u32 = WM_APP + 1;

const ID_FIND_LABEL: i32 = 1001;
const ID_FIND: i32 = 1002;
const ID_FIND_MENU: i32 = 1003;
const ID_REPLACE_LABEL: i32 = 1004;
const ID_REPLACE: i32 = 1005;
const ID_REPLACE_MENU: i32 = 1006;
const ID_CASE: i32 = 1010;
const ID_WORD: i32 = 1011;
const ID_SEP: i32 = 1012;
const ID_INCREMENTAL: i32 = 1013;
const ID_PREVIEW: i32 = 1014;
const ID_SELECTION: i32 = 1015;
const ID_WRAP: i32 = 1016;
const ID_CLOSE_DONE: i32 = 1017;
const ID_MODE_NONE: i32 = 1020;
const ID_MODE_REGEX: i32 = 1021;
const ID_MODE_ESCAPE: i32 = 1022;
const ID_MODE_NUMBER: i32 = 1023;
const ID_FUZZY: i32 = 1024;
const ID_PRESERVE: i32 = 1025;
const ID_FIND_NEXT: i32 = 1030;
const ID_FIND_PREV: i32 = 1031;
const ID_REPLACE_ONE: i32 = 1032;
const ID_REPLACE_ALL: i32 = 1033;
const ID_COUNT: i32 = 1034;
const ID_TOGGLE: i32 = 1035;
const ID_STATUS: i32 = 1040;
/// Sent by the message loop for Ctrl+F / Ctrl+H pressed inside the dialog.
const CMD_FIND_MODE: i32 = 1100;
const CMD_REPLACE_MODE: i32 = 1101;

// Template styles, written out so the template reads like a resource script.
const WS_CHILD: u32 = 0x4000_0000;
const WS_VISIBLE: u32 = 0x1000_0000;
const WS_TABSTOP: u32 = 0x0001_0000;
const WS_GROUP: u32 = 0x0002_0000;
const WS_CAPTION: u32 = 0x00C0_0000;
const WS_SYSMENU: u32 = 0x0008_0000;
const DS_SETFONT: u32 = 0x40;
const DS_MODALFRAME: u32 = 0x80;
const DS_3DLOOK: u32 = 0x4;
const BS_DEFPUSHBUTTON: u32 = 1;
const BS_AUTOCHECKBOX: u32 = 3;
const BS_AUTORADIOBUTTON: u32 = 9;
const CBS_DROPDOWN: u32 = 2;
const CBS_AUTOHSCROLL: u32 = 0x40;
const SS_ETCHEDHORZ: u32 = 0x10;
const SS_LEFTNOWORDWRAP: u32 = 0xC;
const SS_NOPREFIX: u32 = 0x80;
const SS_ENDELLIPSIS: u32 = 0x4000;
const BUTTON: u16 = 0x80;
const STATIC: u16 = 0x82;
const COMBOBOX: u16 = 0x85;

const CHECK: u32 = BS_AUTOCHECKBOX | WS_TABSTOP;
const COMBO: u32 = CBS_DROPDOWN | CBS_AUTOHSCROLL | WS_VSCROLL | WS_TABSTOP;

/// Controls in tab order: id, class, text, style.
const CONTROLS: &[(i32, u16, &str, u32)] = &[
    (ID_FIND_LABEL, STATIC, "&Find:", 0),
    (ID_FIND, COMBOBOX, "", COMBO),
    (ID_FIND_MENU, BUTTON, ">", WS_TABSTOP),
    (ID_REPLACE_LABEL, STATIC, "Re&place with:", 0),
    (ID_REPLACE, COMBOBOX, "", COMBO),
    (ID_REPLACE_MENU, BUTTON, ">", WS_TABSTOP),
    (ID_CASE, BUTTON, "Match &Case", CHECK),
    (ID_WORD, BUTTON, "Match &Whole Word", CHECK),
    (ID_SEP, STATIC, "", SS_ETCHEDHORZ),
    (ID_INCREMENTAL, BUTTON, "&Incremental Search", CHECK),
    (ID_PREVIEW, BUTTON, "&Live Preview", CHECK),
    (ID_SELECTION, BUTTON, "In the &Selection Only", CHECK),
    (ID_WRAP, BUTTON, "Wrap Ar&ound", CHECK),
    (ID_CLOSE_DONE, BUTTON, "Close w&hen Finished", CHECK),
    (
        ID_MODE_NONE,
        BUTTON,
        "(Non&e)",
        BS_AUTORADIOBUTTON | WS_GROUP | WS_TABSTOP,
    ),
    (
        ID_MODE_REGEX,
        BUTTON,
        "Regular E&xpressions",
        BS_AUTORADIOBUTTON,
    ),
    (
        ID_MODE_ESCAPE,
        BUTTON,
        "Escape Se&quence",
        BS_AUTORADIOBUTTON,
    ),
    (ID_MODE_NUMBER, BUTTON, "Number Ran&ge", BS_AUTORADIOBUTTON),
    (ID_FUZZY, BUTTON, "Fu&zzy Matching", CHECK | WS_GROUP),
    (ID_PRESERVE, BUTTON, "Preser&ve Case", CHECK),
    (
        ID_FIND_NEXT,
        BUTTON,
        "Find &Next",
        BS_DEFPUSHBUTTON | WS_GROUP | WS_TABSTOP,
    ),
    (ID_FIND_PREV, BUTTON, "Find Previo&us", WS_TABSTOP),
    (ID_REPLACE_ONE, BUTTON, "&Replace", WS_TABSTOP),
    (ID_REPLACE_ALL, BUTTON, "Replace &All", WS_TABSTOP),
    (ID_COUNT, BUTTON, "Coun&t", WS_TABSTOP),
    (ID_TOGGLE, BUTTON, "<< Find", WS_TABSTOP),
    (IDCANCEL, BUTTON, "Close", WS_TABSTOP),
    (
        ID_STATUS,
        STATIC,
        "",
        SS_LEFTNOWORDWRAP | SS_NOPREFIX | SS_ENDELLIPSIS,
    ),
];

const DLG_W: i16 = 330;

/// Top of the option rows, which move up when the Replace row is hidden.
fn options_top(replace: bool) -> i16 {
    if replace {
        67
    } else {
        38
    }
}

fn dialog_height(replace: bool) -> i16 {
    options_top(replace) + 110
}

/// A control's place in dialog units (x, y, width, height), or None when it is hidden in
/// this mode. Combo box heights include their drop-down list.
fn place(id: i32, replace: bool) -> Option<[i16; 4]> {
    let y = options_top(replace);
    let button = |top: i16| Some([263, top, 60, 14]);
    let r = |top: i16| if replace { top } else { top - 34 };
    Some(match id {
        ID_FIND_LABEL => [7, 7, 200, 8],
        ID_FIND => [7, 17, 230, 150],
        ID_FIND_MENU => [240, 16, 14, 14],
        ID_REPLACE_LABEL if replace => [7, 36, 200, 8],
        ID_REPLACE if replace => [7, 46, 230, 150],
        ID_REPLACE_MENU if replace => [240, 45, 14, 14],
        ID_CASE => [7, y, 124, 10],
        ID_WORD => [7, y + 12, 124, 10],
        ID_SEP => [7, y + 26, 122, 1],
        ID_INCREMENTAL => [7, y + 31, 124, 10],
        ID_PREVIEW => [7, y + 43, 124, 10],
        ID_SELECTION => [7, y + 55, 124, 10],
        ID_WRAP => [7, y + 67, 124, 10],
        ID_CLOSE_DONE => [7, y + 79, 124, 10],
        ID_MODE_NONE => [137, y, 120, 10],
        ID_MODE_REGEX => [137, y + 12, 120, 10],
        ID_MODE_ESCAPE => [137, y + 24, 120, 10],
        ID_MODE_NUMBER => [137, y + 36, 120, 10],
        ID_FUZZY => [137, y + 55, 120, 10],
        ID_PRESERVE if replace => [137, y + 67, 120, 10],
        ID_FIND_NEXT => return button(7),
        ID_FIND_PREV => return button(24),
        ID_REPLACE_ONE if replace => return button(41),
        ID_REPLACE_ALL if replace => return button(58),
        ID_COUNT => return button(r(75)),
        ID_TOGGLE => return button(r(97)),
        IDCANCEL => return button(r(114)),
        ID_STATUS => [7, y + 96, 316, 9],
        _ => return None,
    })
}

/// DLGTEMPLATEEX for the dialog, DWORD-aligned as CreateDialogIndirectParam requires.
fn template() -> Vec<u32> {
    let mut w: Vec<u16> = Vec::new();
    let u32_ = |w: &mut Vec<u16>, v: u32| {
        w.push(v as u16);
        w.push((v >> 16) as u16);
    };
    let text = |w: &mut Vec<u16>, s: &str| {
        w.extend(s.encode_utf16());
        w.push(0);
    };
    // Header: version 1, extended signature, help id, ex-style, style, item count, rect.
    w.extend([1, 0xFFFF]);
    u32_(&mut w, 0);
    u32_(&mut w, 0);
    u32_(
        &mut w,
        WS_POPUP | WS_CAPTION | WS_SYSMENU | DS_SETFONT | DS_MODALFRAME | DS_3DLOOK,
    );
    w.push(CONTROLS.len() as u16);
    w.extend([0, 0, DLG_W as u16, dialog_height(true) as u16]);
    w.extend([0, 0]); // no menu, default class
    text(&mut w, "Replace");
    // Font: 9 pt, regular weight, not italic, default charset.
    w.extend([9, 400, 0x0100]);
    text(&mut w, "Segoe UI");
    for &(id, class, label, style) in CONTROLS {
        if w.len() % 2 == 1 {
            w.push(0);
        }
        let rect = place(id, true).unwrap_or([0, 0, 0, 0]);
        u32_(&mut w, 0);
        u32_(&mut w, 0);
        u32_(&mut w, WS_CHILD | WS_VISIBLE | style);
        w.extend(rect.map(|v| v as u16));
        u32_(&mut w, id as u32);
        w.extend([0xFFFF, class]);
        text(&mut w, label);
        w.push(0); // no creation data
    }
    let mut out = vec![0u32; w.len().div_ceil(2)];
    for (i, v) in w.iter().enumerate() {
        out[i / 2] |= (*v as u32) << (16 * (i % 2));
    }
    out
}

/// The dialog window, for the message loop. Null while none exists.
static DIALOG: AtomicPtr<c_void> = AtomicPtr::new(null_mut());

unsafe extern "system" fn dlg_proc(dlg: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> isize {
    match msg {
        WM_INITDIALOG => 1,
        WM_COMMAND => {
            SendMessageW(GetWindow(dlg, GW_OWNER), WM_FIND, wp, lp);
            1
        }
        WM_CLOSE => {
            SendMessageW(GetWindow(dlg, GW_OWNER), WM_FIND, IDCANCEL as usize, 0);
            1
        }
        _ => 0,
    }
}

/// Keyboard handling for the dialog, called by the message loop before dispatch. Returns
/// true when the message was used. F3 / Shift+F3 search again and Ctrl+F / Ctrl+H switch
/// between Find and Replace while the dialog has the focus; everything else goes through
/// the dialog manager (Tab, Enter, Esc, Alt+mnemonics).
pub(super) fn pre_translate(msg: &MSG) -> bool {
    let dlg = DIALOG.load(Ordering::Relaxed);
    if dlg.is_null() {
        return false;
    }
    // SAFETY: plain window queries and messages on windows of this thread.
    unsafe {
        if msg.hwnd != dlg && IsChild(dlg, msg.hwnd) == 0 {
            return false;
        }
        if msg.message == WM_KEYDOWN || msg.message == WM_SYSKEYDOWN {
            let vk = msg.wParam as u16;
            let ctrl = GetKeyState(VK_CONTROL as i32) < 0;
            let shift = GetKeyState(VK_SHIFT as i32) < 0;
            let cmd = match vk {
                v if v == VK_F1 + 2 => Some(if shift { ID_FIND_PREV } else { ID_FIND_NEXT }),
                0x46 if ctrl => Some(CMD_FIND_MODE),
                0x48 if ctrl => Some(CMD_REPLACE_MODE),
                _ => None,
            };
            if let Some(cmd) = cmd {
                SendMessageW(GetWindow(dlg, GW_OWNER), WM_FIND, cmd as usize, 0);
                return true;
            }
        }
        IsDialogMessageW(dlg, msg) != 0
    }
}

fn item(dlg: HWND, id: i32) -> HWND {
    // SAFETY: GetDlgItem only reads.
    unsafe { GetDlgItem(dlg, id) }
}

fn window_text(hwnd: HWND) -> String {
    // SAFETY: the buffer holds len + 1 units, as GetWindowTextW is told.
    unsafe {
        let len = GetWindowTextLengthW(hwnd).max(0) as usize;
        let mut buf = vec![0u16; len + 1];
        let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32).max(0) as usize;
        String::from_utf16_lossy(&buf[..n.min(len)])
    }
}

fn set_text(hwnd: HWND, s: &str) {
    let w = wide(s);
    // SAFETY: w is NUL-terminated and outlives the call.
    unsafe {
        SetWindowTextW(hwnd, w.as_ptr());
    }
}

fn checked(dlg: HWND, id: i32) -> bool {
    // SAFETY: plain query.
    unsafe { IsDlgButtonChecked(dlg, id) == BST_CHECKED }
}

fn set_check(dlg: HWND, id: i32, on: bool) {
    // SAFETY: plain message.
    unsafe {
        CheckDlgButton(dlg, id, if on { BST_CHECKED } else { BST_UNCHECKED });
    }
}

fn enable(dlg: HWND, id: i32, on: bool) {
    // SAFETY: plain message.
    unsafe {
        EnableWindow(item(dlg, id), on as i32);
    }
}

/// The edit control inside a drop-down combo box.
fn combo_edit(combo: HWND) -> HWND {
    // SAFETY: COMBOBOXINFO is plain data with its size set.
    unsafe {
        let mut info: COMBOBOXINFO = zeroed();
        info.cbSize = size_of::<COMBOBOXINFO>() as u32;
        if GetComboBoxInfo(combo, &mut info) == 0 {
            return null_mut();
        }
        info.hwndItem
    }
}

/// Selection of an edit control in UTF-16 units.
fn edit_sel(edit: HWND) -> (usize, usize) {
    let (mut s, mut e) = (0u32, 0u32);
    // SAFETY: EM_GETSEL writes two DWORDs through the pointers.
    unsafe {
        SendMessageW(
            edit,
            EM_GETSEL,
            &mut s as *mut u32 as usize,
            &mut e as *mut u32 as isize,
        );
    }
    (s as usize, e as usize)
}

/// Replaces the selection of `edit` with `s`, then moves the caret `back` units left.
fn edit_insert(edit: HWND, s: &str, back: usize) {
    let w = wide(s);
    // SAFETY: w is NUL-terminated and outlives the call.
    unsafe {
        SendMessageW(edit, EM_REPLACESEL, 1, w.as_ptr() as isize);
        if back > 0 {
            let (_, e) = edit_sel(edit);
            let at = e.saturating_sub(back);
            SendMessageW(edit, EM_SETSEL, at, at as isize);
        }
    }
}

// ---------------------------------------------------------------------------
// Completion popup
// ---------------------------------------------------------------------------

/// The completion list for one edit box. Boxed so the subclassed edit can keep a pointer
/// to it in GWLP_USERDATA.
pub(super) struct Complete {
    field: Field,
    edit: HWND,
    popup: HWND,
    edit_proc: isize,
    popup_proc: isize,
    /// Completions are only offered in regex mode.
    regex: bool,
    groups: usize,
    names: Vec<String>,
    items: Vec<Completion>,
    /// UTF-16 offset where the token being completed starts.
    start: usize,
    /// Swallow the WM_CHAR that follows a key the list used (Tab, Enter, Esc).
    swallow: bool,
}

impl Complete {
    fn new(field: Field) -> Box<Complete> {
        Box::new(Complete {
            field,
            edit: null_mut(),
            popup: null_mut(),
            edit_proc: 0,
            popup_proc: 0,
            regex: false,
            groups: 0,
            names: Vec::new(),
            items: Vec::new(),
            start: 0,
            swallow: false,
        })
    }

    /// Subclasses `edit`. `self` must stay at this address while the edit exists.
    fn attach(&mut self, edit: HWND) {
        if edit.is_null() {
            return;
        }
        self.edit = edit;
        // SAFETY: the edit keeps a pointer to this boxed struct, which outlives it (the
        // dialog is destroyed with the main window, before App is dropped).
        unsafe {
            SetWindowLongPtrW(edit, GWLP_USERDATA, self as *mut Complete as isize);
            self.edit_proc = SetWindowLongPtrW(edit, GWLP_WNDPROC, edit_proc as *const () as isize);
        }
    }

    fn shown(&self) -> bool {
        // SAFETY: plain query.
        !self.items.is_empty()
            && !self.popup.is_null()
            && unsafe { IsWindowVisible(self.popup) } != 0
    }

    fn hide(&mut self) {
        self.items.clear();
        if !self.popup.is_null() {
            // SAFETY: plain call.
            unsafe {
                ShowWindow(self.popup, SW_HIDE);
            }
        }
    }

    /// Re-reads the text before the caret and shows, filters or hides the list.
    fn refresh(&mut self) {
        if !self.regex {
            self.hide();
            return;
        }
        let text: Vec<u16> = window_text(self.edit).encode_utf16().collect();
        let caret = edit_sel(self.edit).1.min(text.len());
        let before = String::from_utf16_lossy(&text[..caret]);
        match completions(&before, self.field, self.groups, &self.names) {
            Some((at, list)) => {
                self.start = before[..at].encode_utf16().count();
                self.items = list;
                self.show();
            }
            None => self.hide(),
        }
    }

    fn show(&mut self) {
        // SAFETY: standard window creation and messages; strings outlive their calls.
        unsafe {
            if self.popup.is_null() {
                let class = wide("LISTBOX");
                let dlg = DIALOG.load(Ordering::Relaxed);
                self.popup = CreateWindowExW(
                    WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                    class.as_ptr(),
                    null(),
                    WS_POPUP
                        | WS_BORDER
                        | WS_VSCROLL
                        | LBS_NOTIFY as u32
                        | LBS_USETABSTOPS as u32
                        | LBS_NOINTEGRALHEIGHT as u32,
                    0,
                    0,
                    0,
                    0,
                    dlg,
                    null_mut(),
                    GetModuleHandleW(null()),
                    null(),
                );
                if self.popup.is_null() {
                    return;
                }
                SetWindowLongPtrW(self.popup, GWLP_USERDATA, self as *mut Complete as isize);
                self.popup_proc =
                    SetWindowLongPtrW(self.popup, GWLP_WNDPROC, popup_proc as *const () as isize);
                let font = SendMessageW(self.edit, WM_GETFONT, 0, 0);
                SendMessageW(self.popup, WM_SETFONT, font as usize, 0);
                let stops = [64i32];
                SendMessageW(self.popup, LB_SETTABSTOPS, 1, stops.as_ptr() as isize);
            }
            SendMessageW(self.popup, LB_RESETCONTENT, 0, 0);
            for c in &self.items {
                let w = wide(&format!("{}\t{}", c.insert, c.label));
                SendMessageW(self.popup, LB_ADDSTRING, 0, w.as_ptr() as isize);
            }
            SendMessageW(self.popup, LB_SETCURSEL, 0, 0);

            let pos = SendMessageW(self.edit, EM_POSFROMCHAR, self.start, 0);
            let mut pt = POINT {
                x: (pos & 0xFFFF) as u16 as i16 as i32,
                y: 0,
            };
            let mut rc: RECT = zeroed();
            GetClientRect(self.edit, &mut rc);
            pt.y = rc.bottom + 2;
            ClientToScreen(self.edit, &mut pt);
            let dpi = GetDpiForWindow(self.edit).max(96) as i32;
            let ih = SendMessageW(self.popup, LB_GETITEMHEIGHT, 0, 0).max(8) as i32;
            let rows = self.items.len().min(9) as i32;
            let w = 300 * dpi / 96;
            let h = rows * ih + 2 * dpi / 96 + 2;
            SetWindowPos(
                self.popup,
                HWND_TOPMOST,
                pt.x,
                pt.y,
                w,
                h,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
    }

    fn move_sel(&mut self, by: isize) {
        // SAFETY: plain list box messages.
        unsafe {
            let n = SendMessageW(self.popup, LB_GETCOUNT, 0, 0);
            let cur = SendMessageW(self.popup, LB_GETCURSEL, 0, 0).max(0);
            let next = (cur + by).clamp(0, (n - 1).max(0));
            SendMessageW(self.popup, LB_SETCURSEL, next as usize, 0);
        }
    }

    /// Replaces the typed part of the token with the chosen completion.
    fn accept(&mut self) {
        // SAFETY: plain list box message.
        let sel = unsafe { SendMessageW(self.popup, LB_GETCURSEL, 0, 0) };
        let Some(c) = usize::try_from(sel)
            .ok()
            .and_then(|i| self.items.get(i))
            .cloned()
        else {
            self.hide();
            return;
        };
        self.hide();
        let caret = edit_sel(self.edit).1;
        // SAFETY: plain edit message.
        unsafe {
            SendMessageW(self.edit, EM_SETSEL, self.start, caret as isize);
        }
        edit_insert(self.edit, &c.insert, 0);
    }
}

unsafe extern "system" fn edit_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let c = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Complete;
    if c.is_null() {
        return 0;
    }
    let c = &mut *c;
    let old: WNDPROC = transmute::<isize, WNDPROC>(c.edit_proc);
    match msg {
        WM_GETDLGCODE if c.shown() => {
            return CallWindowProcW(old, hwnd, msg, wp, lp) | DLGC_WANTALLKEYS as isize;
        }
        WM_KEYDOWN if c.shown() => match wp as u16 {
            VK_DOWN => {
                c.move_sel(1);
                return 0;
            }
            VK_UP => {
                c.move_sel(-1);
                return 0;
            }
            VK_NEXT => {
                c.move_sel(8);
                return 0;
            }
            VK_PRIOR => {
                c.move_sel(-8);
                return 0;
            }
            VK_TAB | VK_RETURN => {
                c.accept();
                c.swallow = true;
                return 0;
            }
            VK_ESCAPE => {
                c.hide();
                c.swallow = true;
                return 0;
            }
            VK_LEFT | VK_RIGHT | VK_HOME | VK_END => c.hide(),
            _ => {}
        },
        WM_CHAR if c.swallow => {
            c.swallow = false;
            return 0;
        }
        WM_CHAR => {
            let r = CallWindowProcW(old, hwnd, msg, wp, lp);
            c.refresh();
            return r;
        }
        WM_KILLFOCUS => c.hide(),
        WM_NCDESTROY => {
            SetWindowLongPtrW(hwnd, GWLP_WNDPROC, c.edit_proc);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            return CallWindowProcW(old, hwnd, msg, wp, lp);
        }
        _ => {}
    }
    CallWindowProcW(old, hwnd, msg, wp, lp)
}

unsafe extern "system" fn popup_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let c = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Complete;
    if c.is_null() {
        return 0;
    }
    let c = &mut *c;
    let old: WNDPROC = transmute::<isize, WNDPROC>(c.popup_proc);
    match msg {
        // The list never takes the focus, so typing stays in the edit box.
        WM_MOUSEACTIVATE => return MA_NOACTIVATE as isize,
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            let r = SendMessageW(hwnd, LB_ITEMFROMPOINT, 0, lp);
            if (r >> 16) & 0xFFFF == 0 {
                SendMessageW(hwnd, LB_SETCURSEL, (r & 0xFFFF) as usize, 0);
                c.accept();
            }
            return 0;
        }
        WM_NCDESTROY => {
            SetWindowLongPtrW(hwnd, GWLP_WNDPROC, c.popup_proc);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            c.popup = null_mut();
        }
        _ => {}
    }
    CallWindowProcW(old, hwnd, msg, wp, lp)
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// Matches and line previews for the visible lines, for the renderer.
pub(super) struct Preview {
    /// Byte ranges of the matches, in order.
    pub(super) hits: Vec<(usize, usize)>,
    /// What each line becomes after Replace, as UTF-16 with a leading arrow.
    pub(super) lines: Vec<(usize, Vec<u16>)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct PreviewKey {
    version: u64,
    generation: u64,
    first: usize,
    last: usize,
    replace: bool,
    scope: Option<(usize, usize)>,
}

/// Find/Replace state. Options and history live here, so they survive closing the dialog.
pub(super) struct FindUi {
    dlg: HWND,
    replace_mode: bool,
    query: Query,
    incremental: bool,
    preview: bool,
    selection_only: bool,
    wrap: bool,
    close_when_done: bool,
    history: [Vec<String>; 2],
    compiled: Option<Result<Searcher, QueryError>>,
    /// Bumped whenever the query changes, to invalidate the caches below.
    generation: u64,
    /// "In the Selection Only" range.
    scope: Option<(usize, usize)>,
    /// Where incremental search starts.
    inc_origin: usize,
    /// Result of the last command, shown in the status line until the document or the
    /// query changes.
    message: String,
    /// Document version right after that command.
    message_version: u64,
    /// Live match count, shown after the message.
    count_text: String,
    count_key: Option<(u64, u64, Scope)>,
    preview_key: Option<PreviewKey>,
    preview_data: Option<Preview>,
    complete: [Box<Complete>; 2],
}

impl FindUi {
    pub(super) fn new() -> FindUi {
        FindUi {
            dlg: null_mut(),
            replace_mode: false,
            query: Query::default(),
            incremental: false,
            preview: true,
            selection_only: false,
            wrap: true,
            close_when_done: false,
            history: [Vec::new(), Vec::new()],
            compiled: None,
            generation: 0,
            scope: None,
            inc_origin: 0,
            message: String::new(),
            message_version: 0,
            count_text: String::new(),
            count_key: None,
            preview_key: None,
            preview_data: None,
            complete: [Complete::new(Field::Find), Complete::new(Field::Replace)],
        }
    }

    pub(super) fn visible(&self) -> bool {
        // SAFETY: plain query.
        !self.dlg.is_null() && unsafe { IsWindowVisible(self.dlg) } != 0
    }

    /// The live preview to paint, if one is on.
    pub(super) fn preview(&self) -> Option<&Preview> {
        self.preview_data.as_ref()
    }

    /// Drops everything cached for the document on screen. Called when another tab is shown,
    /// since the cached counts, previews and "In the Selection Only" range belong to one tab.
    pub(super) fn reset_caches(&mut self) {
        self.preview_key = None;
        self.preview_data = None;
        self.count_key = None;
        self.scope = None;
    }

    fn scope(&self) -> Option<(usize, usize)> {
        if self.selection_only {
            self.scope
        } else {
            None
        }
    }

    fn searcher(&mut self) -> Result<&mut Searcher, String> {
        let compiled = self
            .compiled
            .get_or_insert_with(|| Searcher::new(&self.query));
        match compiled {
            Ok(s) => Ok(s),
            Err(e) if e.msg == "Nothing to find" => Err(String::new()),
            Err(e) => {
                let field = match e.field {
                    Field::Find => "Find",
                    Field::Replace => "Replace with",
                };
                Err(format!("{field}: {} (at {})", e.msg, e.pos + 1))
            }
        }
    }

    fn show_status(&self) {
        if self.dlg.is_null() {
            return;
        }
        let parts: Vec<&str> = [self.message.as_str(), self.count_text.as_str()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect();
        set_text(item(self.dlg, ID_STATUS), &parts.join("   \u{00B7}   "));
    }

    /// Reads every control into the query and options. Returns true when the query changed.
    fn read_controls(&mut self) -> bool {
        let dlg = self.dlg;
        if dlg.is_null() {
            return false;
        }
        let mode = if checked(dlg, ID_MODE_REGEX) {
            Mode::Regex
        } else if checked(dlg, ID_MODE_ESCAPE) {
            Mode::Escapes
        } else if checked(dlg, ID_MODE_NUMBER) {
            Mode::NumberRange
        } else {
            Mode::Plain
        };
        let q = Query {
            find: window_text(item(dlg, ID_FIND)),
            replace: window_text(item(dlg, ID_REPLACE)),
            mode,
            match_case: checked(dlg, ID_CASE),
            whole_word: checked(dlg, ID_WORD),
            fuzzy: checked(dlg, ID_FUZZY),
            preserve_case: checked(dlg, ID_PRESERVE),
        };
        self.incremental = checked(dlg, ID_INCREMENTAL);
        self.preview = checked(dlg, ID_PREVIEW);
        self.wrap = checked(dlg, ID_WRAP);
        self.close_when_done = checked(dlg, ID_CLOSE_DONE);
        let literal = matches!(mode, Mode::Plain | Mode::Escapes);
        enable(dlg, ID_FUZZY, literal);
        enable(dlg, ID_CASE, mode != Mode::NumberRange);
        enable(dlg, ID_WORD, mode != Mode::NumberRange);
        if q == self.query {
            return false;
        }
        self.query = q;
        self.generation += 1;
        self.compiled = None;
        self.message.clear();
        self.count_key = None;
        // Feed the completion lists the group numbers and names of the pattern.
        let regex = mode == Mode::Regex;
        let (groups, names) = match self.searcher().ok().and_then(|s| s.regex()) {
            Some(r) => (
                r.group_count(),
                r.group_names().map(|(n, _)| n.to_string()).collect(),
            ),
            None => (self.complete[1].groups, self.complete[1].names.clone()),
        };
        for c in self.complete.iter_mut() {
            c.regex = regex;
            c.groups = groups;
            c.names = names.clone();
        }
        true
    }

    /// Adds `text` to the top of a history list and of its combo box.
    fn remember(&mut self, which: usize, text: &str) {
        if text.is_empty() {
            return;
        }
        let h = &mut self.history[which];
        h.retain(|t| t != text);
        h.insert(0, text.to_string());
        h.truncate(20);
        if self.dlg.is_null() {
            return;
        }
        let combo = item(self.dlg, if which == 0 { ID_FIND } else { ID_REPLACE });
        let w = wide(text);
        // SAFETY: w outlives the calls. The edit text is left as it is.
        unsafe {
            let at = SendMessageW(combo, CB_FINDSTRINGEXACT, usize::MAX, w.as_ptr() as isize);
            if at >= 0 {
                SendMessageW(combo, CB_DELETESTRING, at as usize, 0);
            }
            SendMessageW(combo, CB_INSERTSTRING, 0, w.as_ptr() as isize);
            while SendMessageW(combo, CB_GETCOUNT, 0, 0) > 20 {
                SendMessageW(combo, CB_DELETESTRING, 20, 0);
            }
        }
    }

    /// Shows the controls of Find or Replace mode and fits the dialog around them.
    fn layout(&mut self) {
        let dlg = self.dlg;
        let replace = self.replace_mode;
        // SAFETY: rectangles are plain data; every handle is a control of the dialog.
        unsafe {
            for &(id, ..) in CONTROLS {
                let ctl = item(dlg, id);
                match place(id, replace) {
                    Some([x, y, w, h]) => {
                        let mut rc = RECT {
                            left: x as i32,
                            top: y as i32,
                            right: (x + w) as i32,
                            bottom: (y + h) as i32,
                        };
                        MapDialogRect(dlg, &mut rc);
                        SetWindowPos(
                            ctl,
                            null_mut(),
                            rc.left,
                            rc.top,
                            rc.right - rc.left,
                            rc.bottom - rc.top,
                            SWP_NOZORDER | SWP_NOACTIVATE,
                        );
                        ShowWindow(ctl, SW_SHOW);
                    }
                    None => {
                        ShowWindow(ctl, SW_HIDE);
                    }
                }
            }
            let mut want = RECT {
                left: 0,
                top: 0,
                right: DLG_W as i32,
                bottom: dialog_height(replace) as i32,
            };
            MapDialogRect(dlg, &mut want);
            let (mut win, mut client): (RECT, RECT) = (zeroed(), zeroed());
            GetWindowRect(dlg, &mut win);
            GetClientRect(dlg, &mut client);
            let w = want.right + (win.right - win.left) - client.right;
            let h = want.bottom + (win.bottom - win.top) - client.bottom;
            SetWindowPos(
                dlg,
                null_mut(),
                0,
                0,
                w,
                h,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
            set_text(dlg, if replace { "Replace" } else { "Find" });
            set_text(
                item(dlg, ID_TOGGLE),
                if replace { "<< Find" } else { "Replace >>" },
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Status text for a failed or stopped search.
fn not_found(q: &Query) -> String {
    let mut shown: String = q.find.chars().take(40).collect();
    if q.find.chars().count() > 40 {
        shown.push('\u{2026}');
    }
    format!("Cannot find \"{shown}\"")
}

const TOO_COMPLEX: &str = "The search stopped: the pattern is too complex for this text";

impl App {
    fn create_find_dialog(&mut self, hwnd: HWND) {
        let tpl = template();
        // SAFETY: the template is a valid, DWORD-aligned DLGTEMPLATEEX that lives across the
        // call; the dialog procedure only forwards messages.
        let dlg = unsafe {
            CreateDialogIndirectParamW(
                GetModuleHandleW(null()),
                tpl.as_ptr() as *const DLGTEMPLATE,
                hwnd,
                Some(dlg_proc),
                0,
            )
        };
        if dlg.is_null() {
            return;
        }
        DIALOG.store(dlg, Ordering::Relaxed);
        let f = &mut self.find;
        f.dlg = dlg;
        set_check(dlg, ID_INCREMENTAL, f.incremental);
        set_check(dlg, ID_PREVIEW, f.preview);
        set_check(dlg, ID_WRAP, f.wrap);
        set_check(dlg, ID_CLOSE_DONE, f.close_when_done);
        set_check(dlg, ID_CASE, f.query.match_case);
        set_check(dlg, ID_WORD, f.query.whole_word);
        set_check(dlg, ID_FUZZY, f.query.fuzzy);
        set_check(dlg, ID_PRESERVE, f.query.preserve_case);
        let mode = match f.query.mode {
            Mode::Plain => ID_MODE_NONE,
            Mode::Regex => ID_MODE_REGEX,
            Mode::Escapes => ID_MODE_ESCAPE,
            Mode::NumberRange => ID_MODE_NUMBER,
        };
        // SAFETY: plain dialog messages.
        unsafe {
            CheckRadioButton(dlg, ID_MODE_NONE, ID_MODE_NUMBER, mode);
            SendMessageW(dlg, DM_SETDEFID, ID_FIND_NEXT as usize, 0);
        }
        f.complete[0].attach(combo_edit(item(dlg, ID_FIND)));
        f.complete[1].attach(combo_edit(item(dlg, ID_REPLACE)));
        // Top right of the editor window, clear of most text.
        // SAFETY: rectangles are plain data.
        unsafe {
            let (mut owner, mut me): (RECT, RECT) = (zeroed(), zeroed());
            GetWindowRect(hwnd, &mut owner);
            GetWindowRect(dlg, &mut me);
            let x = (owner.right - (me.right - me.left) - 40).max(owner.left);
            SetWindowPos(
                dlg,
                null_mut(),
                x,
                owner.top + 80,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }

    /// Ctrl+F / Ctrl+H: shows the dialog in Find or Replace mode. A selection on one line
    /// becomes the Find text; a selection over several lines turns on "In the Selection Only".
    pub(super) fn open_find(&mut self, hwnd: HWND, replace: bool) {
        if self.find.dlg.is_null() {
            self.create_find_dialog(hwnd);
        }
        let dlg = self.find.dlg;
        if dlg.is_null() {
            return;
        }
        if let Some((s, e)) = self.editor.selection() {
            let text = self.editor.buf.slice(s, e);
            if text.contains(&b'\n') || text.len() > 1000 {
                self.find.selection_only = true;
                self.find.scope = Some((s, e));
            } else {
                self.find.selection_only = false;
                self.find.scope = None;
                set_text(item(dlg, ID_FIND), &String::from_utf8_lossy(&text));
            }
            set_check(dlg, ID_SELECTION, self.find.selection_only);
        }
        self.find.inc_origin = self
            .editor
            .selection()
            .map_or(self.editor.cursor, |(s, _)| s);
        self.find.replace_mode = replace;
        self.find.layout();
        self.find.read_controls();
        // SAFETY: plain window calls.
        unsafe {
            ShowWindow(dlg, SW_SHOW);
            let combo = item(dlg, ID_FIND);
            SetFocus(combo);
            SendMessageW(combo, CB_SETEDITSEL, 0, 0xFFFF_0000u32 as isize);
        }
        self.find.count_key = None;
        self.find.show_status();
        redraw(hwnd);
    }

    fn hide_find(&mut self, hwnd: HWND) {
        if self.find.dlg.is_null() {
            return;
        }
        self.find.complete.iter_mut().for_each(|c| c.hide());
        // SAFETY: plain window calls.
        unsafe {
            ShowWindow(self.find.dlg, SW_HIDE);
            SetFocus(hwnd);
        }
        self.find.preview_data = None;
        self.find.preview_key = None;
        redraw(hwnd);
    }

    /// F3 / Shift+F3 in the editor: repeats the last search, or opens the dialog.
    pub(super) fn find_again(&mut self, hwnd: HWND, forward: bool) {
        if self.find.dlg.is_null() || self.find.query.find.is_empty() {
            self.open_find(hwnd, false);
            return;
        }
        self.find_step(hwnd, forward);
    }

    /// A WM_FIND message from the dialog.
    pub(super) fn on_find_msg(&mut self, hwnd: HWND, wp: WPARAM) {
        let id = (wp & 0xFFFF) as i32;
        let code = ((wp >> 16) & 0xFFFF) as u32;
        let dlg = self.find.dlg;
        if dlg.is_null() {
            return;
        }
        match id {
            ID_FIND | ID_REPLACE => match code {
                CBN_EDITCHANGE => self.find_changed(hwnd, id == ID_FIND),
                // The edit text changes only after this notification, so read it later.
                CBN_SELCHANGE => {
                    let later = (CBN_EDITCHANGE as usize) << 16 | id as usize;
                    // SAFETY: plain message.
                    unsafe {
                        PostMessageW(hwnd, WM_FIND, later, 0);
                    }
                }
                _ => {}
            },
            _ if code != BN_CLICKED => {}
            ID_FIND_MENU | ID_REPLACE_MENU => self.token_menu(hwnd, id == ID_REPLACE_MENU),
            ID_CASE | ID_WORD | ID_MODE_NONE | ID_MODE_REGEX | ID_MODE_ESCAPE | ID_MODE_NUMBER
            | ID_FUZZY | ID_PRESERVE | ID_PREVIEW => self.find_changed(hwnd, false),
            ID_INCREMENTAL | ID_WRAP | ID_CLOSE_DONE => {
                self.find.read_controls();
            }
            ID_SELECTION => {
                let on = checked(dlg, ID_SELECTION);
                self.find.selection_only = on;
                self.find.scope = None;
                if on {
                    match self.editor.selection() {
                        Some(sel) => self.find.scope = Some(sel),
                        None => {
                            set_check(dlg, ID_SELECTION, false);
                            self.find.selection_only = false;
                            self.find.message = "Select the text to search in first".to_string();
                            self.find.message_version = self.editor.buf.version();
                        }
                    }
                }
                self.find.count_key = None;
                self.find.show_status();
                redraw(hwnd);
            }
            ID_FIND_NEXT => self.find_step(hwnd, true),
            ID_FIND_PREV => self.find_step(hwnd, false),
            ID_REPLACE_ONE => self.replace_step(hwnd),
            ID_REPLACE_ALL => self.replace_everything(hwnd),
            ID_COUNT => self.count_all(hwnd),
            ID_TOGGLE => {
                self.find.replace_mode = !self.find.replace_mode;
                self.find.layout();
                self.find.preview_key = None;
                redraw(hwnd);
            }
            CMD_FIND_MODE => self.open_find(hwnd, false),
            CMD_REPLACE_MODE => self.open_find(hwnd, true),
            IDCANCEL => self.hide_find(hwnd),
            _ => {}
        }
    }

    /// A box or option changed: recompile, run incremental search, refresh the preview.
    fn find_changed(&mut self, hwnd: HWND, typed_find: bool) {
        if !self.find.read_controls() {
            return;
        }
        if let Err(msg) = self.find.searcher() {
            self.find.message = msg;
        }
        if typed_find && self.find.incremental && self.find.searcher().is_ok() {
            let origin = self.find.inc_origin.min(self.editor.buf.len());
            self.editor.anchor = None;
            self.editor.cursor = origin;
            let scope = self.find.scope();
            let wrap = self.find.wrap;
            let mut budget = FIND_BUDGET;
            let found = match self.find.searcher() {
                Ok(s) => self.editor.find(s, true, wrap, scope, &mut budget),
                Err(_) => Ok(None),
            };
            self.find.message = match found {
                Ok(Some(_)) => String::new(),
                Ok(None) => not_found(&self.find.query),
                Err(TooComplex) => TOO_COMPLEX.to_string(),
            };
            self.after_command(hwnd);
        }
        self.find.message_version = self.editor.buf.version();
        self.find.show_status();
        redraw(hwnd);
    }

    /// Sets the status line (and the main status bar) after a command.
    fn find_result(&mut self, hwnd: HWND, msg: String) {
        self.editor.message = (!msg.is_empty()).then(|| msg.clone());
        self.find.message = msg;
        self.find.message_version = self.editor.buf.version();
        self.find.show_status();
        self.after_command(hwnd);
    }

    fn find_step(&mut self, hwnd: HWND, forward: bool) {
        self.find.read_controls();
        let scope = self.find.scope();
        let wrap = self.find.wrap;
        let in_sel = scope.is_some();
        let mut budget = FIND_BUDGET;
        let res = match self.find.searcher() {
            Ok(s) => self.editor.find(s, forward, wrap, scope, &mut budget),
            Err(msg) => return self.find_result(hwnd, msg),
        };
        let find_text = self.find.query.find.clone();
        self.find.remember(0, &find_text);
        let msg = match res {
            Ok(Some(f)) => {
                self.find.inc_origin = f.start;
                let what = if in_sel { "selection" } else { "file" };
                if !f.wrapped {
                    String::new()
                } else if forward {
                    format!("Passed the end of the {what}")
                } else {
                    format!("Passed the start of the {what}")
                }
            }
            Ok(None) => not_found(&self.find.query),
            Err(TooComplex) => TOO_COMPLEX.to_string(),
        };
        let found = matches!(res, Ok(Some(_)));
        self.find_result(hwnd, msg);
        if found && self.find.close_when_done {
            self.hide_find(hwnd);
        }
    }

    /// Checks the Replace text before a replace command.
    fn replace_ready(&mut self, hwnd: HWND) -> bool {
        self.find.read_controls();
        let err = match self.find.searcher() {
            Ok(s) => s
                .replace_error()
                .map(|e| format!("Replace with: {} (at {})", e.msg, e.pos + 1)),
            Err(msg) => Some(msg),
        };
        if let Some(msg) = err {
            self.find_result(hwnd, msg);
            return false;
        }
        let (f, r) = (
            self.find.query.find.clone(),
            self.find.query.replace.clone(),
        );
        self.find.remember(0, &f);
        self.find.remember(1, &r);
        true
    }

    fn replace_step(&mut self, hwnd: HWND) {
        if !self.replace_ready(hwnd) {
            return;
        }
        let scope = self.find.scope();
        let wrap = self.find.wrap;
        let mut budget = FIND_BUDGET;
        let res = match self.find.searcher() {
            Ok(s) => self.editor.replace_next(s, wrap, scope, &mut budget),
            Err(_) => return,
        };
        let msg = match res {
            Ok((replaced, found, new_scope)) => {
                if self.find.selection_only {
                    self.find.scope = new_scope;
                }
                match (replaced, found) {
                    (_, Some(_)) => String::new(),
                    (true, None) => "Replaced the last match".to_string(),
                    (false, None) => not_found(&self.find.query),
                }
            }
            Err(TooComplex) => TOO_COMPLEX.to_string(),
        };
        self.find_result(hwnd, msg);
    }

    fn replace_everything(&mut self, hwnd: HWND) {
        if !self.replace_ready(hwnd) {
            return;
        }
        let scope = self.find.scope();
        let mut budget = FIND_BUDGET;
        let res = match self.find.searcher() {
            Ok(s) => self.editor.replace_all(s, scope, &mut budget),
            Err(_) => return,
        };
        let (msg, done) = match res {
            Ok((0, _)) => (not_found(&self.find.query), false),
            Ok((n, new_scope)) => {
                if self.find.selection_only {
                    self.find.scope = new_scope;
                }
                let s = if n == 1 { "" } else { "s" };
                (format!("Replaced {n} occurrence{s}"), true)
            }
            Err(TooComplex) => (TOO_COMPLEX.to_string(), false),
        };
        self.find_result(hwnd, msg);
        if done && self.find.close_when_done {
            self.hide_find(hwnd);
        }
    }

    fn count_all(&mut self, hwnd: HWND) {
        self.find.read_controls();
        let scope = self.find.scope();
        let mut budget = FIND_BUDGET;
        let res = match self.find.searcher() {
            Ok(s) => self.editor.count_matches(s, scope, &mut budget),
            Err(msg) => return self.find_result(hwnd, msg),
        };
        let msg = match res {
            Ok(n) => count_text(n, scope.is_some()),
            Err(TooComplex) => TOO_COMPLEX.to_string(),
        };
        self.find_result(hwnd, msg);
    }

    /// The ">" button: a menu of tokens to insert at the caret. Picking one turns on
    /// regex mode.
    fn token_menu(&mut self, hwnd: HWND, replace_box: bool) {
        let dlg = self.find.dlg;
        let groups: &[&[Token]] = if replace_box {
            REPLACE_TOKENS
        } else {
            FIND_TOKENS
        };
        let mut flat: Vec<Token> = Vec::new();
        // SAFETY: the menu is created, shown modally and destroyed here; strings outlive
        // their calls.
        let cmd = unsafe {
            let menu = CreatePopupMenu();
            if menu.is_null() {
                return;
            }
            for (g, group) in groups.iter().enumerate() {
                if g > 0 {
                    AppendMenuW(menu, MF_SEPARATOR, 0, null());
                }
                for t in group.iter() {
                    flat.push(*t);
                    let label = wide(&format!("{}\t{}", t.label, t.text.replace('&', "&&")));
                    AppendMenuW(menu, MF_STRING, flat.len(), label.as_ptr());
                }
            }
            let button = item(
                dlg,
                if replace_box {
                    ID_REPLACE_MENU
                } else {
                    ID_FIND_MENU
                },
            );
            let mut rc: RECT = zeroed();
            GetWindowRect(button, &mut rc);
            let cmd = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_NONOTIFY,
                rc.right,
                rc.top,
                0,
                dlg,
                null(),
            );
            DestroyMenu(menu);
            cmd
        };
        let Some(t) = usize::try_from(cmd)
            .ok()
            .and_then(|c| c.checked_sub(1))
            .and_then(|i| flat.get(i))
        else {
            return;
        };
        let combo = item(dlg, if replace_box { ID_REPLACE } else { ID_FIND });
        let edit = combo_edit(combo);
        // SAFETY: plain calls on the dialog's controls.
        unsafe {
            SetFocus(edit);
            CheckRadioButton(dlg, ID_MODE_NONE, ID_MODE_NUMBER, ID_MODE_REGEX);
        }
        edit_insert(edit, t.text, t.back);
        self.find_changed(hwnd, !replace_box);
    }

    /// Recomputes the live preview and the live count for the visible lines, when the
    /// dialog is open with Live Preview on. Called before each paint; cached by document
    /// version, query and view, so an unchanged frame costs nothing.
    pub(super) fn update_find_preview(&mut self, rows: usize) {
        if !self.find.visible() || !self.find.preview {
            self.find.preview_data = None;
            self.find.preview_key = None;
            if !self.find.count_text.is_empty() {
                self.find.count_text.clear();
                self.find.show_status();
            }
            return;
        }
        let scope = self.find.scope();
        let version = self.editor.buf.version();
        let generation = self.find.generation;

        let count_key = (version, generation, scope);
        if self.find.count_key != Some(count_key) {
            self.find.count_key = Some(count_key);
            if version != self.find.message_version {
                self.find.message.clear();
            }
            // Small budget: this runs on every edit while the dialog is open.
            let mut budget = PREVIEW_BUDGET;
            let text = match self.find.searcher() {
                Ok(s) => match self.editor.count_matches(s, scope, &mut budget) {
                    Ok(n) => count_text(n, scope.is_some()),
                    Err(TooComplex) => "Many matches (press Count)".to_string(),
                },
                Err(_) => String::new(),
            };
            self.find.count_text = text;
            self.find.show_status();
        }

        let first = if self.editor.wrap {
            self.editor.top_row(self.editor.wrap_width()).0
        } else {
            self.editor.scroll_line
        };
        let last = first + rows + 1;
        let key = PreviewKey {
            version,
            generation,
            first,
            last,
            replace: self.find.replace_mode,
            scope,
        };
        if self.find.preview_key == Some(key) {
            return;
        }
        self.find.preview_key = Some(key);
        self.find.preview_data = None;
        let replace = self.find.replace_mode;
        let mut budget = PREVIEW_BUDGET;
        let hits = match self.find.searcher() {
            Ok(s) => {
                let with_rep = replace && s.replace_error().is_none();
                self.editor
                    .hits(s, first, last, 4000, with_rep, &mut budget)
            }
            Err(_) => return,
        };
        let Ok(mut hits) = hits else {
            return;
        };
        if let Some((lo, hi)) = scope {
            hits.retain(|h| h.start >= lo && h.end <= hi);
        }
        let lines = if replace {
            self.editor
                .preview_lines(&hits, 400)
                .into_iter()
                .map(|(line, s)| (line, format!("\u{2192} {s}").encode_utf16().collect()))
                .collect()
        } else {
            Vec::new()
        };
        self.find.preview_data = Some(Preview {
            hits: hits.iter().map(|h: &Hit| (h.start, h.end)).collect(),
            lines,
        });
    }
}

fn count_text(n: usize, in_selection: bool) -> String {
    let s = if n == 1 { "" } else { "es" };
    let where_ = if in_selection {
        " in the selection"
    } else {
        ""
    };
    format!("{n} match{s}{where_}")
}
