//! harness-tui: the harness terminal UI library, built from scratch.
//!
//! Line-based rendering: components produce lines for a width, the diff
//! computes minimal row updates, and the terminal layer writes them
//! atomically. Design spec:
//! docs/superpowers/specs/2026-07-11-harness-tui-design.md

pub mod diff;
pub mod headless;
pub mod terminal;
pub mod text;
