#![windows_subsystem = "windows"]

#[cfg(windows)]
fn main() {
    note::win::run();
}

#[cfg(not(windows))]
fn main() {
    eprintln!("note is a Win32 application; build with --target x86_64-pc-windows-msvc or -gnu.");
}
