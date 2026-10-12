//! note — ultra-light Win32 text editor. See DESIGN.md.
pub mod buffer;
pub mod config;
pub mod editor;
pub mod encoding;
pub mod simd;
pub mod syntax;
#[cfg(windows)]
pub mod win;
