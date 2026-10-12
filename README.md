# note

An ultra-light Notepad replacement for Windows. Pure Win32 and GDI, dark by default, nano
key bindings, and no tabs, menus or frameworks. The release executable is about 450 KB, with
the C runtime linked statically.

- **Pure Win32 / GDI.** One window, one document, a double-buffered GDI renderer that paints
  only the visible lines.
- **Dark theme.** Every colour is configurable; a light palette is built in.
- **Nano key bindings.** `^O` saves, `^X` exits, `^G` shows the full key list.
- **SIMD.** Byte scans (newline counting, search, line endings, ASCII checks) use SSE2 and
  AVX2, with AVX2 chosen at runtime.
- **Syntax highlighting** for about 20 languages, computed incrementally.
- **Small.** About 680 KB as a single exe with a static CRT and Paper Mono embedded (about
  470 KB without it). No runtime DLLs beyond the Windows system DLLs.

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

Pass a file to open it. You can also drag a file onto the window. Without a file, the editor
opens an untitled document. Saving an untitled document asks for a name.

Files are decoded on load and written back in the same encoding and line ending they had:
UTF-8 (with or without BOM), UTF-16 LE/BE, or ANSI (Latin-1) for files that are not valid
UTF-8. CRLF and LF are both preserved.

## Key bindings

Chords use nano notation: `^` is Ctrl, `M-` is Alt, `S-` is Shift. The table lists every
action in the default keymap with all of its chords. Action names are the ones used in
`[keys]` in the config file.

| Action | Chords |
| --- | --- |
| `save` | `^O` `^S` |
| `save_as` | `^S-S` |
| `open` | `^R` |
| `new` | `^N` |
| `exit` | `^X` |
| `cut_line` | `^K` |
| `paste` | `^U` `S-Ins` |
| `copy` | `M-6` `^Ins` |
| `cut` | `S-Del` |
| `find` | `^W` `^F` |
| `find_next` | `M-W` `F3` |
| `find_prev` | `M-Q` `S-F3` |
| `replace` | `^\` `^H` |
| `goto_line` | `^_` `^L` |
| `show_position` | `^C` |
| `help` | `^G` `F1` |
| `undo` | `M-U` `^Z` |
| `redo` | `M-E` `^S-Z` |
| `select_all` | `^S-A` |
| `line_home` | `^A` |
| `line_end` | `^E` |
| `page_up` | `^Y` |
| `page_down` | `^V` |
| `doc_start` | `M-\` `^Home` |
| `doc_end` | `M-/` `^End` |
| `word_left` | `^Left` |
| `word_right` | `^Right` |
| `delete_word_left` | `^Backspace` |
| `delete_word_right` | `^Del` |
| `toggle_line_numbers` | `M-N` |
| `toggle_wrap` | `M-L` |
| `toggle_hints` | `M-X` |
| `zoom_in` | `^+` |
| `zoom_out` | `^-` |
| `zoom_reset` | `^0` |
| `indent` | `Tab` |
| `unindent` | `S-Tab` |
| `duplicate_line` | `^D` |
| `mark_toggle` | `^6` `M-A` |
| `toggle_comment` | `M-3` |
| `move_line_up` | `M-Up` |
| `move_line_down` | `M-Down` |

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
`scrollbar_track`, and the token colours `keyword`, `type`, `string`, `number`, `comment`,
`preproc`, `function`, `punct`, `heading`.

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

The module contracts and the reasoning behind them are in [DESIGN.md](DESIGN.md).

## Performance

Measure load, index and lex throughput on a real file:

```
cargo run --release --example bench -- <file>
```

It reports the ASCII check, a plain copy for comparison, decode, line indexing, a byte count
and a full lex pass.

## Releasing

Releases are cut by hand. Run the **Release** workflow (`.github/workflows/release.yml`)
from the Actions tab, on the branch to release from.

- `bump`: which part of the latest `vX.Y.Z` tag to increase: `patch`, `minor` or `major`.
- `prerelease`: marks the GitHub release as a pre-release.
- `draft`: creates the release as a draft, to review before publishing.

The workflow runs `cargo test --release`, sets `version` in `Cargo.toml`, and commits
`Release vX.Y.Z` to the branch. It then creates an annotated tag, builds the portable exe
`note-vX.Y.Z-x86_64-windows.exe` and the setup program `note-vX.Y.Z-setup-x86_64.exe`, and
publishes both with `SHA256SUMS.txt` and a changelog of the commits since the previous tag.
The release notes start with an Install section that names both files. If `makensis` is not
on the Windows runner, the workflow installs NSIS with Chocolatey first. If there is no tag yet, a patch release publishes the
version in `Cargo.toml` as it is, and a minor or major release bumps it.

## Known limitations

- Soft wrap (`M-L`) breaks lines at word boundaries and is visual only. The vertical
  scrollbar still counts logical lines.
- CJK and other wide characters take one cell.
- One document per window, by design. Open another copy of the app for another file.
