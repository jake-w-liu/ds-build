//! Shared utilities used by both `ds-shell` and its downstream clients
//! (e.g. `ds-pager-render`). This crate sits upstream of `ds-shell`
//! so it must never depend on it.

pub mod clipboard;
pub mod placeholder_images;
pub mod session;
pub mod stderr;
pub mod ui_config;
