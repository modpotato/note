# note

You know that moment when you just want to jot something down, and your editor takes a
few seconds to wake up first? note is built so that never happens. It's a tiny Notepad
replacement for Windows: pure Win32 and GDI, dark by default, with the Notepad keys you
already know plus a few line-editing extras. No menus, no frameworks. The whole thing is
about 870 KB with the font, icon and C runtime baked in, and the installer is about 520 KB.

Here's what you get:

- **Pure Win32 / GDI.** One window with tabs, painted by a double-buffered GDI renderer
  that only draws the lines you can actually see.
- **Dark theme.** Every colour is yours to change, and there's a light palette built in if
  you'd rather.
- **Notepad keys, plus extras.** `Ctrl+S`, `Ctrl+A`, `Ctrl+C`/`V`/`X`, `Ctrl+Z`/`Y`, `Ctrl+F`,
  `Ctrl+H` and `Ctrl+G` do what your fingers expect. On top of that, `Ctrl+K` cuts the line,
  `Ctrl+D` duplicates it, and `Alt+Up`/`Alt+Down` move it around. Forget one? `F1` lists them all.
- **Find and replace like EmEditor.** Regular expressions with lookaround, back-references,
  named groups and case conversion in the replacement. Also escape sequences, number ranges
  and fuzzy matching. A live preview shows every match and what each line will turn into, and
  regex syntax completes as you type. The regex engine is built in, so no dependencies.
- **SIMD.** Byte scans (newline counting, search, line endings, ASCII checks) use SSE2, and
  AVX2 when your CPU has it.
- **Syntax highlighting** for about 20 languages, updated incrementally as you type.
- **Small.** About 870 KB as a single exe with a static CRT, the icon and Paper Mono
  embedded (about 660 KB without the font). It needs nothing beyond the DLLs Windows
  already ships.

## Install

Every [release](https://github.com/modpotato/note/releases) comes with two Windows files.
Pick one:

- **`note-vX.Y.Z-setup-x86_64.exe`** (recommended). A setup program that installs just for
  you, no admin rights needed. By default it adds a Start menu entry, an "Open with note"
  item to the Explorer right-click menu, and an "Open with" entry for text and code files.
  Don't worry, those "Open with" entries won't touch your default programs. A desktop
  shortcut and adding note to PATH are both there, just off by default. Want it for everyone
  on the machine? Choose "Install for anyone using this computer" (that one needs admin).
- **`note-vX.Y.Z-x86_64-windows.exe`** (portable). Just the exe. Nothing gets installed or
  registered, so drop it anywhere and run it.

The setup program shows up in Apps and Features and uninstalls cleanly, removing everything
it added. Your settings in `%APPDATA%\note` stay put unless you tell it to delete them.
One heads-up: setup asks Windows for the highest rights your account has, so on an admin
account you'll see a UAC prompt when it starts. On a standard account you won't.

Scripting an install? Run `setup.exe /S`. Add `/AllUsers` to install for everyone (admin),
or `/D=C:\path` to pick the folder. To uninstall silently, run
`"%LOCALAPPDATA%\Programs\note\uninstall.exe" /S`. It keeps your settings.

## Build

On Windows with the MSVC toolchain:

```
cargo build --release
```

Or cross-compile from Linux with mingw-w64:

```
rustup target add x86_64-pc-windows-gnu
cargo build --release --target x86_64-pc-windows-gnu
```

You'll find the exe at `target/<triple>/release/note.exe`. (If you just want a build,
every GitHub release has prebuilt files. See [Releasing](#releasing).) The icon lives in
`assets/icon/note.ico`, and `assets/icon/make-icon.sh` regenerates it with ImageMagick.
`build.rs` embeds the icon and a version resource into the Windows exe.

To build the setup program, install NSIS 3 and point `makensis` at `installer/note.nsi`. The
`-D` options are required, and `-WX` turns warnings into errors:

```
makensis -WX -DVERSION=0.1.0 -DEXE=C:\full\path\to\note.exe -DOUTFILE=C:\full\path\to\note-v0.1.0-setup-x86_64.exe installer\note.nsi
```

On Linux it's the same `-D` options with forward-slash paths, like
`target/x86_64-pc-windows-gnu/release/note.exe`.

Tests run with plain `cargo test`.

## Usage

```
note [file...]
```

Pass one or more files and each one opens in its own tab. Dragging files onto the window
works too. With no files, you get a blank untitled document (or last session's files, if
`restore_tabs` is on). When you save an untitled document, note asks for a name.

Save As tries to do the obvious thing with extensions. With "Text Documents (*.txt)"
selected, a name without an extension gets `.txt`: `notes` saves as `notes.txt`, but
`notes.md` stays `notes.md`. Want the name exactly as typed? Switch to "All Files (*.*)",
or end the name with a dot (`notes.`) for no extension at all. Untitled documents and `.txt`
files start on "Text Documents", and everything else starts on "All Files", so Save As on
`LICENSE` doesn't sneak a `.txt` onto it.

And your files come back out the way they went in. note writes each file in the encoding
and line ending it was loaded with: UTF-8 (with or without BOM), UTF-16 LE/BE, or ANSI
(Latin-1) for files that aren't valid UTF-8. CRLF stays CRLF, LF stays LF.

## Key bindings

Chords are written the short way: `^` is Ctrl, `M-` is Alt, `S-` is Shift. Here's every
action in the default keymap with all its chords. The action names are the same ones you
use under `[keys]` in the config file, so you can move or remove any of them.

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

Hold Shift with the arrows, `Home`, `End`, `PgUp`, `PgDn`, `^Home`, `^End`, `^Left` or
`^Right` to extend the selection.

`^K` is the one worth learning. With nothing selected it cuts the whole line, and pressing
it again tacks the next line onto the same clipboard text. So `^K ^K ^K` then `^V` moves
three lines. Honestly, once it's in your fingers you'll miss it everywhere else.

`M-A` drops a mark: from then on every movement extends the selection, until you press it
again or hit `Esc`.

## Find and replace

`^F` opens Find and `^H` opens Replace. If you've selected text on one line, it becomes
the search text. Select across several lines and "In the Selection Only" switches on
instead. Inside the dialog, `Enter` is Find Next, `F3` and `S-F3` search forward and back,
`^F` and `^H` flip between Find and Replace, and `Esc` closes it. Once it's closed, `F3` and
`S-F3` still repeat your last search. Each box remembers its last 20 entries in its drop-down.

Options:

- **Match Case**, **Match Whole Word**. A whole-word match can't start or end inside a word.
- **Incremental Search** selects the first match while you type.
- **Live Preview** highlights every match on screen. In Replace mode it also shows, after
  the end of each line, what that line will become (`→ ...`). The status line keeps a running
  match count as you type.
- **In the Selection Only** searches and replaces inside the selection the dialog was opened
  with.
- **Wrap Around** continues from the other end of the file (or selection).
- **Close when Finished** closes the dialog once a match is found or Replace All runs.
- **Preserve Case** re-cases the replacement like the text it replaces: `cat` → `dog` turns
  `Cat` into `Dog` and `CAT` into `DOG`.

Modes:

- **(None)**: plain text.
- **Regular Expressions**: see below. The `>` buttons open a menu of tokens, so you don't
  have to remember the syntax. Typing `\`, `(?`, `[:` or `$` pops up a completion list
  (`↑`/`↓` to choose, `Tab` or `Enter` to insert). Back-reference and group-name suggestions
  come from the groups in your Find pattern.
- **Escape Sequence**: plain text with `\n`, `\t`, `\\`, `\xHH` and `\x{HHHH}`.
- **Number Range**: finds numbers in a range. Enter `10-20`, `1.5..3`, `>=100`, `<0`, `..50`
  or a single number. Decimals and negative numbers are understood.
- **Fuzzy Matching** (with (None) or Escape Sequence) forgives about one typo per four
  characters: a changed, missing, extra or swapped letter. So `receive` finds `recieve`.

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

In the replacement text (regex mode): `$0` or `$&` for the whole match, `$1` or `\1` for a group,
`${name}` for a named group, `$$` for a dollar sign, `\n` and `\t`, and the case conversions
`\U` (upper from here), `\L` (lower from here), `\E` (end), `\u` and `\l` (next char only).
For example, find `(\w+)@(\w+)` and replace with `\U$2\E: $1`.

Every search runs on a step budget. So if you write a pattern that would take forever (we've
all done it), it stops with a message instead of freezing the editor. And Replace All is a
single undo step, so one `^Z` takes it all back.

## Tabs

Each open file gets its own tab. Open a file that's already open and note just jumps to
its tab. Opening a file (`^O`, drag and drop, or the command line) reuses the active tab if
it's an empty untitled document. Otherwise the file opens in a new tab right after it.

The tab bar stays hidden while only one tab is open, so a quick note looks just like
Notepad. (`tab_bar` in `[editor]` changes that, see Configuration.) The active tab has a
close box (`×`). A tab with unsaved changes shows a dot (`●`) in the same spot, unless it's
active or under the pointer.

Keys: `^T` new tab, `^W` or `^F4` close tab, `^S-T` reopen the last closed tab (the last ten
are kept), `^Tab` or `^PgDn` next tab, `^S-Tab` or `^PgUp` previous tab, `^1` to `^8` tab 1 to
8, `^9` last tab, `^S-PgUp` and `^S-PgDn` move the tab left or right, `^S-N` opens another
window.

Mouse: drag a tab sideways to reorder it, middle-click to close it, and double-click empty
space in the bar (or click `+`) for a new tab. Scrolling the wheel over the bar flips
through tabs.

You won't lose work by accident. Closing a tab, or the whole window (`^Q` or the close
button), asks about every tab with unsaved changes. Yes saves it (an untitled document
opens Save As, and if you cancel that, the tab stays open). No throws the changes away.
Cancel keeps everything open, and when you're closing the window, it stops right there.

With `restore_tabs = true`, note remembers your open files in `%APPDATA%\note\session.txt`
when the window closes and reopens them next time you start it without any files. Anything
that's been deleted in the meantime is quietly skipped.

## Configuration

Settings live in `%APPDATA%\note\note.ini`, which note creates with the defaults the first
time it runs. Comments start with `;` or `#`. Don't worry about typos breaking anything:
unknown keys are ignored, and a bad value just falls back to its default.

```ini
[editor]
font = Paper Mono, Cascadia Mono, Consolas
font_size = 11
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

Write chords in nano notation (`^S-Z`) or spell them out (`ctrl+shift+z`, `alt+up`, `f3`),
whichever you like. The colour keys are the theme names: `bg`, `fg`, `caret`, `selection_bg`,
`current_line_bg`, `gutter_bg`, `gutter_fg`, `status_bg`, `status_fg`, `hint_key_fg`,
`hint_fg`, the scrollbar colours `scrollbar_thumb`, `scrollbar_thumb_hover` and
`scrollbar_track`, the live preview colours `match_bg` and `preview_fg`, the tab colours
`tabbar_bg`, `tab_hover_bg` and `tab_fg`, and the token colours `keyword`, `type`, `string`, `number`, `comment`, `preproc`, `function`, `punct`,
`heading`.

Font keys in `[editor]`:

- `font` takes a comma-separated list of typefaces, favourite first, each up to 31
  characters. note uses the first one Windows can find, and falls back to Consolas. The
  default is `Paper Mono`.
- `font_size` is in points (6 to 72). `Ctrl+plus` and `Ctrl+minus` zoom from here. The
  default is 11.
- `font_weight` is 100 to 900, or a name: `thin`, `extralight`, `light`, `regular`
  (`normal`), `medium`, `semibold`, `bold`, `extrabold`, `black`. The default is 400.
- `bold_keywords` draws keywords and types in bold (weight 700). Every cell keeps the same
  width, so your columns still line up. The default is `false`.
- `line_height` is the line pitch as a multiple of the font height, clamped to 1.0 to 2.5.
  Text sits centred in each line. The default is 1.2.
- `font_smoothing` is `cleartype`, `grayscale` or `none`. The default is `cleartype`.

Tab keys in `[editor]`:

- `tab_bar` is `auto`, `always` or `never`. `auto` shows the tab bar only when two or more
  tabs are open. The default is `auto`.
- `restore_tabs` reopens the files that were open when the editor last closed. The default
  is `false`.

## Fonts

The exe carries **Paper Mono** Regular and Bold inside it and registers them with GDI for
note's own process only. Nothing gets installed system-wide. Paper Mono is by Paper Design (Lost Coast Labs),
based on Geist Mono by Vercel. See <https://paper.design/mono> and
<https://github.com/paper-design/paper-mono>.

The font files are embedded unmodified under the SIL Open Font License 1.1. The license is
in [assets/fonts/OFL.txt](assets/fonts/OFL.txt).

Rather leave the font out? Build without default features:

```
cargo build --release --no-default-features
```

That saves about 210 KB. Then set `font` in `note.ini` to any typeface you have installed.
If you don't, the default `Paper Mono` won't be found and you'll get Consolas.

## Architecture

The editor is a library crate with a thin Windows front end on top. That split is the
whole trick: everything except `win/` runs and gets tested on any host, Linux included.

- **Gap buffer** of UTF-8 bytes with an **incremental line index**, so an edit only touches
  the line starts it affects. Undo and redo live in the buffer too.
- **Incremental highlighter** with a per-line state cache. An edit throws away the cached
  states from the edited line down, and the next lookup lexes forward from the last good
  one.
- **Renderer** paints into a memory bitmap and copies it to the window once per paint.
  Only the visible lines get drawn.
- **Regex engine** compiles a pattern to a small instruction list and runs it on a
  backtracking VM over the UTF-8 text. When the pattern starts with literal text, it skips
  ahead with SIMD. The live preview only searches the lines on screen.

For the module contracts and the thinking behind them, see [DESIGN.md](DESIGN.md).

## Performance

Curious how fast it really is? Point the bench at a real file:

```
cargo run --release --example bench -- <file>
```

It times the ASCII check, a plain copy (as a baseline), decoding, line indexing, a byte
count, a full lex pass, and find-all for a few plain and regex queries.

## Releasing

Releases are cut by hand: run the **Release** workflow (`.github/workflows/release.yml`)
from the Actions tab, on the branch you want to release from. It takes three inputs:

- `bump`: which part of the latest `vX.Y.Z` tag to increase: `patch`, `minor` or `major`.
- `prerelease`: marks the GitHub release as a pre-release.
- `draft`: creates the release as a draft, so you can look it over before publishing.

First the workflow runs `cargo test --release`. Then it builds the portable exe
`note-vX.Y.Z-x86_64-windows.exe` and the setup program `note-vX.Y.Z-setup-x86_64.exe` with
the new version applied (`scripts/set-version.sh`).

Only once the build succeeds does it commit `Release vX.Y.Z` to the branch, create the
annotated tag, and publish both files along with `SHA256SUMS.txt` and a changelog of the
commits since the previous tag. That ordering matters: a failed build leaves no stray tag or
release commit behind. And if someone pushed to the branch while the workflow ran, the push
is rejected and nothing gets tagged.

A few smaller details:

- The release notes open with an Install section that names both files.
- If `makensis` isn't on the Windows runner, the workflow installs NSIS with Chocolatey first.
- The setup program is a 32-bit NSIS installer (official NSIS builds only ship x86 stubs)
  that installs the 64-bit exe.
- If there's no tag yet, a patch release publishes the version in `Cargo.toml` as is, and a
  minor or major release bumps it.

## Known limitations

note is small on purpose, and that means a few rough edges. Here's what you might bump into:

- Soft wrap (`M-Z`) breaks lines at word boundaries, but it's visual only. The vertical
  scrollbar still counts logical lines.
- CJK and other wide characters take up one cell, not two.
- Tabs belong to one window. Open the same file in two windows and you've got two separate
  documents, and whichever one you save last wins.
- Regex: no Unicode properties (`\p{...}`), recursion or conditionals. Line breaks in the
  text are always `\n`, whatever the file uses, so `\r` only matches a stray carriage return.
- The Find/Replace dialog uses the light system style, even with the dark theme. Yes, it's a
  bit jarring. Its options and history last until the editor closes.
