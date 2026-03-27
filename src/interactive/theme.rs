// Centralized ANSI color constants for the TUI.

pub const RESET: &str = "\x1b[0m";
pub const DIM: &str = "\x1b[38;5;242m";
pub const ACCENT: &str = "\x1b[38;5;75m";
pub const ACCENT_BOLD: &str = "\x1b[1;38;5;75m";
pub const MUTED: &str = "\x1b[38;5;245m";
pub const THINKING: &str = "\x1b[38;5;141m";
pub const THINKING_LOW: &str = "\x1b[38;5;98m";
pub const THINKING_HIGH: &str = "\x1b[1;38;5;141m";
pub const BOLD: &str = "\x1b[1m";
pub const REVERSE: &str = "\x1b[7m";

// Semantic colors
pub const ERROR: &str = "\x1b[38;5;203m";
pub const WARN: &str = "\x1b[38;5;214m";
pub const CAUTION: &str = "\x1b[38;5;178m";
pub const SUCCESS: &str = "\x1b[38;5;114m";

// Tool colors
pub const TOOL_NAME: &str = "\x1b[38;5;180m";
pub const TOOL_PATH: &str = "\x1b[38;5;109m";

// Diff colors
pub const DIFF_ADD: &str = "\x1b[38;5;114m";
pub const DIFF_DEL: &str = "\x1b[38;5;203m";
pub const DIFF_HUNK: &str = "\x1b[38;5;67m";

// Footer / status
pub const FOOTER_DIM: &str = "\x1b[38;5;240m";
pub const FOOTER_LABEL: &str = "\x1b[38;5;248m";
pub const COST: &str = "\x1b[38;5;180m";
