#![allow(clippy::module_inception)]
pub mod components;
pub mod highlight;
pub mod keys;
pub mod stdin_buffer;
pub mod terminal;
pub mod tui;
pub mod utils;

pub use self::components::*;
pub use self::keys::{KeyId, is_key_release, matches_key, parse_key};
pub use self::stdin_buffer::{StdinBuffer, StdinEvent};
pub use self::terminal::{ProcessTerminal, Terminal};
pub use self::tui::{CURSOR_MARKER, Component, Container, TUI};
pub use self::utils::{slice_by_column, truncate_to_width, visible_width, wrap_text_with_ansi};
