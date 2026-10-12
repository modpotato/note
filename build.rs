//! Embeds the application icon (resource ID 1, which `src/win/mod.rs` loads) and a
//! VERSIONINFO block in the Windows exe. Only Windows targets do anything; every other
//! target returns early, so Linux builds and tests are unaffected.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=assets/icon/note.ico");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    // Forward slashes work for rc.exe, llvm-rc and windres alike, and need no escaping.
    let icon = manifest_dir.join("assets/icon/note.ico");
    let icon = icon.to_string_lossy().replace('\\', "/");

    let version = env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION");
    // FILEVERSION and PRODUCTVERSION need four numbers. Pre-release tags are not numeric,
    // so they are dropped from the binary fields (the string fields keep them).
    let mut parts = version
        .split(['.', '-', '+'])
        .take(3)
        .map(|p| p.parse::<u16>().unwrap_or(0));
    let (major, minor, patch) = (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    );

    // Generated here rather than kept in the tree, so the version can never drift from Cargo.toml.
    let rc = format!(
        r#"1 ICON "{icon}"

1 VERSIONINFO
FILEVERSION {major},{minor},{patch},0
PRODUCTVERSION {major},{minor},{patch},0
FILEFLAGSMASK 0x3fL
FILEFLAGS 0x0L
FILEOS 0x40004L
FILETYPE 0x1L
FILESUBTYPE 0x0L
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904b0"
        BEGIN
            VALUE "FileDescription", "note"
            VALUE "FileVersion", "{version}"
            VALUE "InternalName", "note"
            VALUE "LegalCopyright", "MIT"
            VALUE "OriginalFilename", "note.exe"
            VALUE "ProductName", "note"
            VALUE "ProductVersion", "{version}"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x409, 1200
    END
END
"#
    );
    let rc_path = out_dir.join("note-resources.rc");
    fs::write(&rc_path, rc).expect("write generated .rc");

    // Required, not optional: a Windows build without its icon and version info should fail,
    // not quietly ship an exe that lacks them.
    embed_resource::compile(&rc_path, embed_resource::NONE)
        .manifest_required()
        .expect("embedding the icon and version resource");
}
