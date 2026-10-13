# note

An ultra-light Notepad replacement for Windows. Pure Win32 and GDI, dark by default, the
usual Notepad keys plus a few line-editing extras, and no tabs, menus or frameworks. The
release executable is about 870 KB (font, icon and C runtime included); the installer is
about 520 KB.

- **Pure Win32 / GDI.** One window, one document, a double-buffered GDI renderer that paints
  only the visible lines.
- **Dark theme.** Every colour is configurable; a light palette is built in.
- **Notepad keys, plus extras.** `Ctrl+S`, `Ctrl+A`, `Ctrl+C`/`V`/`X`, `Ctrl+Z`/`Y`, `Ctrl+F`,
  `Ctrl+H` and `Ctrl+G` work as in Notepad. On top of that, `Ctrl+K` cuts the line, `Ctrl+D`
  duplicates it and `Alt+Up`/`Alt+Down` move it. `F1` lists every key.
- **Find and replace like EmEditor.** Regular expressions with lookaround, back-references,
  named groups and case conversion in the replacement, plus escape sequences, number ranges
  and fuzzy matching. A live preview shows every match and what each line becomes, and regex
  syntax completes as you type. The regex engine is built in, so there are no dependencies.
- **SIMD.** Byte scans (newline counting, search, line endings, ASCII checks) use SSE2 and
  AVX2, with AVX2 chosen at runtime.
- **Syntax highlighting** for about 20 languages, computed incrementally.
- **Small.** About 870 KB as a single exe with a static CRT, icon and Paper Mono embedded
  (about 660 KB without the font). No runtime DLLs beyond the Windows system DLLs.

## Install

Each [release](https://github.com/modpotato/note/releases) has two Windows files:

- **`note-vX.Y.Z-setup-x86_64.exe`** (recommended). A setup program that installs for your
  own account, with no admin rights needed. By default it adds a Start menu entry, an
  "Open with note" item in the Explorer right-click menu, and an "Open with" entry for text
  and code files. Those "Open with" entries do not change your default programs. A desktop
  shortcut and adding the install folder to PATH are off by default. Choose "Install for
  anyone using this computer" to install for all users instead (this needs admin rights).
- **`note-vX.Y.Z-x86_64-windows.exe`** (portable). A single exe. Nothing is installed or
  registered, so copy it anywhere and run it.

The setup program appears in Apps and Features and uninstalls cleanly: it removes
everything it added. Your settings in `%APPDATA%\note` stay unless you choose to delete
them. Setup asks Windows for the highest rights the account has, so an admin account sees a
UAC prompt when setup starts. A standard account sees none.

For scripted installs, run `setup.exe /S`. Add `/AllUsers` for all users (admin) or
`/D=C:\path` for another folder. Uninstall silently with
`"%LOCALAPPDATA%\Programs\note\uninstall.exe" /S`. The uninstaller keeps your settings.

## Build

Windows with the MSVC toolchain:

```
cargo build --release
```

Cross-compile from Linux with mingw-w64:

```
rustup target add x86_64-pc-windows-gnu
cargo build --release --target x86_64-pc-windows-gnu
```

The exe is `target/<triple>/release/note.exe`. Prebuilt files are attached to each GitHub
release (see [Releasing](#releasing)). The icon is `assets/icon/note.ico`, which
`assets/icon/make-icon.sh` regenerates with ImageMagick. `build.rs` embeds it and a version
resource into the Windows exe.

To build the setup program, install NSIS 3 and run `makensis` on `installer/note.nsi`. The
`-D` options are required, and `-WX` makes warnings fail the build:

```
makensis -WX -DVERSION=0.1.0 -DEXE=C:\full\path\to\note.exe -DOUTFILE=C:\full\path\to\note-v0.1.0-setup-x86_64.exe installer\note.nsi
```

On Linux, pass the same `-D` options with forward-slash paths, for example
`target/x86_64-pc-windows-gnu/release/note.exe`.

Run the tests with `cargo test`.

## Usage

```
note [file]
```

Pass one or more files to open them, each in its own tab. You can also drag files onto the
window. Without a file, the editor opens an untitled document, or the files of the last
session when `restore_tabs` is on. Saving an untitled document asks for a name.

In the Save As dialog, "Text Documents (*.txt)" adds `.txt` to a name typed without an
extension: `notes` saves as `notes.txt`, while `notes.md` saves as `notes.md`. Switch the
type to "All Files (*.*)" to save the name exactly as typed, or end the name with a dot
(`notes.`) to save it with no extension. Untitled documents and `.txt` files start on "Text
Documents"; other files start on "All Files", so Save As on `LICENSE` keeps that name.

Files are decoded on load and written back in the same encoding and line ending they had:
UTF-8 (with or without BOM), UTF-16 LE/BE, or ANSI (Latin-1) for files that are not valid
UTF-8. CRLF and LF are both preserved.

## Key bindings

Chords are written short: `^` is Ctrl, `M-` is Alt, `S-` is Shift. The table lists every
action in the default keymap with all of its chords. Action names are the ones used in
`[keys]` in the config file, so any of them can be moved or removed there.

| Action | Chords |
| --- | --- |
| `save` | `^S` |
| `save_as` | `^S-S` |
| `open` | `^O` |
| `new` | `^T` `^N` |
| `exit` | `^Q` (and `Alt+F4`) |
| `undo` | `^Z` |
| `redo` | `^Y` `^S-Z` |
| `cut` | `^X` `S-Del` |
| `copy` | `^C` `^Ins` |
| `paste` | `^V` `S-Ins` |
| `select_all` | `^A` |
| `cut_line` | `^K` |
| `duplicate_line` | `^D` |
| `toggle_comment` | `^/` |
| `move_line_up` | `M-Up` |
| `move_line_down` | `M-Down` |
| `indent` | `Tab` |
| `unindent` | `S-Tab` |
| `delete_word_left` | `^Backspace` |
| `delete_word_right` | `^Del` |
| `mark_toggle` | `M-A` |
| `find` | `^F` |
| `find_next` | `F3` |
| `find_prev` | `S-F3` |
| `replace` | `^H` |
| `goto_line` | `^G` |
| `line_home` | `Home` |
| `line_end` | `End` |
| `page_up` | `PgUp` |
| `page_down` | `PgDn` |
| `doc_start` | `^Home` |
| `doc_end` | `^End` |
| `word_left` | `^Left` |
| `word_right` | `^Right` |
| `zoom_in` | `^+` |
| `zoom_out` | `^-` |
| `zoom_reset` | `^0` |
| `toggle_wrap` | `M-Z` |
| `toggle_line_numbers` | `M-N` |
| `toggle_hints` | `M-X` |
| `show_position` | `M-C` |
| `help` | `F1` |
| `close_tab` | `^W` `^F4` |
| `reopen_tab` | `^S-T` |
| `next_tab` | `^Tab` `^PgDn` |
| `prev_tab` | `^S-Tab` `^PgUp` |
| `tab_1` | `^1` |
| `tab_2` | `^2` |
| `tab_3` | `^3` |
| `tab_4` | `^4` |
| `tab_5` | `^5` |
| `tab_6` | `^6` |
| `tab_7` | `^7` |
| `tab_8` | `^8` |
| `last_tab` | `^9` |
| `move_tab_left` | `^S-PgUp` |
| `move_tab_right` | `^S-PgDn` |
| `new_window` | `^S-N` |

Shift with the arrows, `Home`, `End`, `PgUp`, `PgDn`, `^Home`, `^End`, `^Left` and `^Right`
extends the selection. `^K` without a selection cuts the whole line; pressing it again
appends the next line to the same clipboard text, so `^K ^K ^K` then `^V` moves three lines.
`M-A` starts a mark: every movement extends the selection until you press it again or `Esc`.

## Find and replace

`^F` opens the Find dialog and `^H` opens Replace. Selected text on one line becomes the Find
text; a selection over several lines turns on "In the Selection Only". In the dialog, `Enter`
is Find Next, `F3` and `S-F3` search forward and back, `^F` and `^H` switch between Find and
Replace, and `Esc` closes it. With the dialog closed, `F3` and `S-F3` repeat the last search.
Each box keeps its last 20 entries in its drop-down list.

Options:

- **Match Case**, **Match Whole Word**. A whole-word match cannot start or end inside a word.
- **Incremental Search** selects the first match while you type.
- **Live Preview** highlights every match in view. In Replace mode it also shows, after the end
  of each line, what that line becomes (`→ ...`). The status line counts the matches as you type.
- **In the Selection Only** searches and replaces inside the selection the dialog was opened
  with.
- **Wrap Around** continues from the other end of the file (or selection).
- **Close when Finished** closes the dialog after a match is found or Replace All runs.
- **Preserve Case** re-cases the replacement like the text it replaces: `cat` → `dog` turns
  `Cat` into `Dog` and `CAT` into `DOG`.

Modes:

- **(None)**: plain text.
- **Regular Expressions**: see below. The `>` buttons open a menu of tokens, and typing `\`,
  `(?`, `[:` or `$` opens a completion list (`↑`/`↓` to choose, `Tab` or `Enter` to insert).
  Back-reference and group-name completions come from the groups in the Find pattern.
- **Escape Sequence**: plain text with `\n`, `\t`, `\\`, `\xHH` and `\x{HHHH}`.
- **Number Range**: finds numbers in a range. Enter `10-20`, `1.5..3`, `>=100`, `<0`, `..50`
  or a single number. Decimals and negative numbers are understood.
- **Fuzzy Matching** (with (None) or Escape Sequence) allows about one typo per four
  characters: a changed, missing, extra or swapped letter. `receive` finds `recieve`.

Regular expressions:

| Syntax | Meaning |
| --- | --- |
| `.` `\d` `\w` `\s` `\h` | any char (not a line break), digit, word char, whitespace, space or tab; `\D` `\W` `\S` `\H` negate |
| `[abc]` `[^a-z]` `[[:alpha:]]` | character classes, with ranges and POSIX names |
| `^` `$` `\b` `\<` `\>` `\A` `\z` | line start and end, word boundary, word start and end, text start and end |
| `*` `+` `?` `{n,m}` | repeats; add `?` for as few as possible, `+` to never give back |
| `(...)` `(?:...)` `(?<name>...)` `\|` | groups and alternatives |
| `\1` `\k<name>` | text of a group |
| `(?=...)` `(?!...)` `(?<=...)` `(?<!...)` | followed by, not followed by, preceded by, not preceded by (any length) |
| `(?>...)` | atomic group |
| `(?i)` `(?s)` `(?m)` `(?x)` | ignore case, `.` matches line breaks, `^`/`$` per line (on by default), ignore spaces and `#` comments |
| `\n` `\t` `\r\n` `\R` `\xHH` `\x{HHHH}` `\Q...\E` | line break, tab, any line break, char by hex code, literal text |

Replacement text in regex mode: `$0` or `$&` for the whole match, `$1` or `\1` for a group,
`${name}` for a named group, `$$` for a dollar sign, `\n` and `\t`, and the case conversions
`\U` (upper from here), `\L` (lower from here), `\E` (end), `\u` and `\l` (next char only).
For example, find `(\w+)@(\w+)` and replace with `\U$2\E: $1`.

Every search has a step budget, so a pattern that would take too long stops with a message
instead of freezing the editor. Replace All is one undo step.

## Tabs

Each open file has its own tab, and a file that is already open is shown in its existing
tab. Opening a file (`^O`, drag and drop, or the command line) fills the active tab when it
is an empty untitled document, and otherwise opens a new tab after the active one.

The tab bar is hidden while only one tab is open. `tab_bar` in `[editor]` changes that (see
Configuration). The active tab shows a close box (`×`). A tab with unsaved changes shows a
dot (`●`) in that place when it is neither active nor under the pointer.

Keys: `^T` new tab, `^W` or `^F4` close tab, `^S-T` reopen the last closed tab (the last ten
are kept), `^Tab` or `^PgDn` next tab, `^S-Tab` or `^PgUp` previous tab, `^1` to `^8` tab 1 to
8, `^9` last tab, `^S-PgUp` and `^S-PgDn` move the tab left or right, `^S-N` opens another
window.

Mouse: drag a tab sideways to reorder it, middle click closes it, and double click on empty
space in the bar, or click `+`, opens a new tab. The mouse wheel over the bar switches tabs.

Closing a tab, or closing the window with `^Q` or the window's close button, asks about each
tab with unsaved changes. Yes saves it (an untitled document opens Save As, and if that is
cancelled the tab stays open), No discards the changes, and Cancel keeps everything open.
Closing the window stops at the first Cancel.

With `restore_tabs = true`, the files of the open tabs are saved to
`%APPDATA%\note\session.txt` when the window closes, and the next start without files
opens them again. Files that no longer exist are skipped.

## Configuration

Settings live in `%APPDATA%\note\note.ini`. The file is created with the defaults on first
run. Comments start with `;` or `#`. Unknown keys are ignored, and invalid values keep their
default.

```ini
[editor]
font = Paper Mono, Cascadia Mono, Consolas
font_size = 12
font_weight = 400
bold_keywords = false
line_height = 1.2
font_smoothing = cleartype
tab_width = 4
word_wrap = false

[theme]
base = dark            ; dark | light; put before colour overrides
bg = #1e1e1e
fg = #d4d4d4
selection_bg = #264f78
keyword = #569cd6

[keys]
ctrl+q = none          ; remove a binding
ctrl+s = save          ; add or move a binding
```

Chords can be written in nano notation (`^S-Z`) or by name (`ctrl+shift+z`, `alt+up`,
`f3`). Colour keys are the theme names: `bg`, `fg`, `caret`, `selection_bg`,
`current_line_bg`, `gutter_bg`, `gutter_fg`, `status_bg`, `status_fg`, `hint_key_fg`,
`hint_fg`, the scrollbar colours `scrollbar_thumb`, `scrollbar_thumb_hover` and
`scrollbar_track`, the live preview colours `match_bg` and `preview_fg`, the tab colours
`tabbar_bg`, `tab_hover_bg` and `tab_fg`, and the token
colours `keyword`, `type`, `string`, `number`, `comment`, `preproc`, `function`, `punct`,
`heading`.

Font keys in `[editor]`:

- `font` takes a comma-separated list of typefaces in order of preference, each at most 31
  characters. The first one Windows matches is used, then Consolas. The default is
  `Paper Mono`.
- `font_size` is in points (6 to 72). `Ctrl+plus` and `Ctrl+minus` zoom from this size.
- `font_weight` is 100 to 900, or a name: `thin`, `extralight`, `light`, `regular`
  (`normal`), `medium`, `semibold`, `bold`, `extrabold`, `black`. The default is 400.
- `bold_keywords` draws keywords and types in a bold face (weight 700). Cells keep the same
  width, so columns still line up. The default is `false`.
- `line_height` is the line pitch as a multiple of the font height, clamped to 1.0 to 2.5.
  Text is centred in each line. The default is 1.2.
- `font_smoothing` is `cleartype`, `grayscale` or `none`. The default is `cleartype`.

Tab keys in `[editor]`:

- `tab_bar` is `auto`, `always` or `never`. `auto` shows the tab bar only when two or more
  tabs are open. The default is `auto`.
- `restore_tabs` reopens the files that were open when the editor last closed. The default
  is `false`.

## Fonts

The exe embeds **Paper Mono** Regular and Bold and registers them with GDI for this process
only, so nothing is installed system-wide. Paper Mono is by Paper Design (Lost Coast Labs),
based on Geist Mono by Vercel. See <https://paper.design/mono> and
<https://github.com/paper-design/paper-mono>.

The font files are embedded unmodified under the SIL Open Font License 1.1. The license is
in [assets/fonts/OFL.txt](assets/fonts/OFL.txt).

To leave the font out, build without default features:

```
cargo build --release --no-default-features
```

That exe is about 210 KB smaller. Set `font` in `note.ini` to any installed typeface instead.
Without the embedded font, the default `Paper Mono` falls back to Consolas.

## Architecture

The editor is a library crate with a thin Windows front end, so everything except `win/`
runs and is tested on any host.

- **Gap buffer** of UTF-8 bytes with an **incremental line index**: an edit updates only the
  affected line starts. Undo and redo are part of the buffer.
- **Incremental highlighter** with a per-line state cache. An edit invalidates the cached
  states from the edited line down, and the next query lexes forward from the last valid
  line.
- **Renderer** paints into a memory bitmap and copies it to the window once per paint. Only
  visible lines are drawn.
- **Regex engine** compiles a pattern to a small instruction list and runs it with a
  backtracking VM over the UTF-8 text, skipping ahead with SIMD when the pattern starts with
  literal text. The live preview only searches the lines in view.

The module contracts and the reasoning behind them are in [DESIGN.md](DESIGN.md).

## Performance

Measure load, index and lex throughput on a real file:

```
cargo run --release --example bench -- <file>
```

It reports the ASCII check, a plain copy for comparison, decode, line indexing, a byte count,
a full lex pass and find-all times for a few plain and regex queries.

## Releasing

Releases are cut by hand. Run the **Release** workflow (`.github/workflows/release.yml`)
from the Actions tab, on the branch to release from.

- `bump`: which part of the latest `vX.Y.Z` tag to increase: `patch`, `minor` or `major`.
- `prerelease`: marks the GitHub release as a pre-release.
- `draft`: creates the release as a draft, to review before publishing.

The workflow runs `cargo test --release`, then builds the portable exe
`note-vX.Y.Z-x86_64-windows.exe` and the setup program `note-vX.Y.Z-setup-x86_64.exe` with
the new version applied (`scripts/set-version.sh`). Only after the build succeeds does it
commit `Release vX.Y.Z` to the branch, create the annotated tag and publish both files with
`SHA256SUMS.txt` and a changelog of the commits since the previous tag, so a failed build
leaves no tag or release commit behind. If the branch moved while the workflow ran, the push
is rejected and nothing is tagged. The release notes start with an Install section that
names both files. If `makensis` is not on the Windows runner, the workflow installs NSIS with
Chocolatey first. The setup program is a 32-bit NSIS installer (official NSIS builds ship
only x86 stubs) that installs the 64-bit exe. If there is no tag yet, a patch release
publishes the version in `Cargo.toml` as it is, and a minor or major release bumps it.

## Known limitations

- Soft wrap (`M-Z`) breaks lines at word boundaries and is visual only. The vertical
  scrollbar still counts logical lines.
- CJK and other wide characters take one cell.
- Tabs belong to one window. A file open in two windows is two separate documents, and
  whichever copy is saved last wins.
- Regex: no Unicode properties (`\p{...}`), recursion or conditionals. Line breaks in the
  text are `\n` whatever the file uses, so `\r` matches only a stray carriage return.
- The Find/Replace dialog uses the light system style, even with the dark theme. Its options
  and history last until the editor closes.
