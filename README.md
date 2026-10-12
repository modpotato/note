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
- **Small.** About 450 KB as a single exe with a static CRT. No runtime DLLs beyond the Windows system DLLs.

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

The exe is `target/<triple>/release/note.exe`. CI (`.github/workflows/build.yml`) runs the
tests and uploads the exe as the `note-x86_64-windows` artifact.

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
font = Consolas
font_size = 12
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
`hint_fg`, and the token colours `keyword`, `type`, `string`, `number`, `comment`,
`preproc`, `function`, `punct`, `heading`.

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
`Release vX.Y.Z` to the branch. It then creates an annotated tag, builds
`note-vX.Y.Z-x86_64-windows.exe`, and publishes it with `SHA256SUMS.txt` and a changelog of
the commits since the previous tag. If there is no tag yet, a patch release publishes the
version in `Cargo.toml` as it is, and a minor or major release bumps it.

## Known limitations

- Soft wrap is not implemented yet. Long lines scroll horizontally.
- CJK and other wide characters take one cell.
- One document per window, by design. Open another copy of the app for another file.
