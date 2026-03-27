//! Minimal syntax highlighting for code blocks.
//! Adapted from ~/d/e/src/highlight.rs — byte-by-byte highlighter.

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Hl {
    #[default]
    Normal,
    Keyword,
    Type,
    String,
    Comment,
    Number,
    Bracket,
    Operator,
    Function,
    Constant,
    Macro,
}

impl Hl {
    pub fn ansi(self) -> &'static str {
        match self {
            Hl::Normal => "",
            Hl::Comment => "\x1b[90m",
            Hl::Keyword => "\x1b[33m",
            Hl::Type => "\x1b[36m",
            Hl::String => "\x1b[32m",
            Hl::Number => "\x1b[31m",
            Hl::Bracket => "\x1b[35m",
            Hl::Operator => "\x1b[33m",
            Hl::Function => "\x1b[34m",
            Hl::Constant => "\x1b[31;1m",
            Hl::Macro => "\x1b[35;1m",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum HlState {
    #[default]
    Normal,
    BlockComment,
    MultiLineString(u8),
}

pub struct Rules {
    pub line_comment: &'static str,
    pub block_comment: (&'static str, &'static str),
    pub strings: &'static [(&'static str, &'static str, bool)], // (open, close, multiline)
    pub keywords: &'static [&'static str],
    pub types: &'static [&'static str],
    pub operators: &'static [&'static str],
    pub highlight_numbers: bool,
    pub highlight_fn_calls: bool,
    pub highlight_bang_macros: bool,
}

/// Highlight a line of code. Returns ANSI-colored string.
pub fn highlight_line(line: &str, state: &mut HlState, rules: &Rules) -> String {
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut hl = vec![Hl::Normal; len];

    let block_open = rules.block_comment.0.as_bytes();
    let block_close = rules.block_comment.1.as_bytes();
    let line_com = rules.line_comment.as_bytes();

    let mut i = 0;
    let mut prev_sep = true;

    // Resume from multiline state
    match *state {
        HlState::BlockComment => {
            while i < len {
                if starts_with(bytes, block_close, i) {
                    let end = i + block_close.len();
                    for b in &mut hl[i..end] {
                        *b = Hl::Comment;
                    }
                    i = end;
                    *state = HlState::Normal;
                    prev_sep = true;
                    break;
                }
                hl[i] = Hl::Comment;
                i += 1;
            }
            if *state == HlState::BlockComment {
                return apply_hl(line, &hl);
            }
        }
        HlState::MultiLineString(idx) => {
            let (_, close, _) = rules.strings[idx as usize];
            let cb = close.as_bytes();
            while i < len {
                if bytes[i] == b'\\' && i + 1 < len {
                    hl[i] = Hl::String;
                    hl[i + 1] = Hl::String;
                    i += 2;
                    continue;
                }
                if starts_with(bytes, cb, i) {
                    let end = i + cb.len();
                    for b in &mut hl[i..end] {
                        *b = Hl::String;
                    }
                    i = end;
                    *state = HlState::Normal;
                    prev_sep = true;
                    break;
                }
                hl[i] = Hl::String;
                i += 1;
            }
            if matches!(*state, HlState::MultiLineString(_)) {
                return apply_hl(line, &hl);
            }
        }
        HlState::Normal => {}
    }

    while i < len {
        // Line comment
        if !line_com.is_empty() && starts_with(bytes, line_com, i) {
            for b in &mut hl[i..len] {
                *b = Hl::Comment;
            }
            *state = HlState::Normal;
            return apply_hl(line, &hl);
        }
        // Block comment
        if !block_open.is_empty() && starts_with(bytes, block_open, i) {
            let start = i;
            i += block_open.len();
            let mut found = false;
            while i < len {
                if starts_with(bytes, block_close, i) {
                    let end = i + block_close.len();
                    for b in &mut hl[start..end] {
                        *b = Hl::Comment;
                    }
                    i = end;
                    prev_sep = true;
                    found = true;
                    break;
                }
                i += 1;
            }
            if !found {
                for b in &mut hl[start..len] {
                    *b = Hl::Comment;
                }
                *state = HlState::BlockComment;
                return apply_hl(line, &hl);
            }
            continue;
        }
        // Strings
        let mut matched = false;
        for (di, &(open, close, multiline)) in rules.strings.iter().enumerate() {
            let ob = open.as_bytes();
            let cb = close.as_bytes();
            if starts_with(bytes, ob, i) {
                let start = i;
                i += ob.len();
                let mut found = false;
                while i < len {
                    if bytes[i] == b'\\' && i + 1 < len {
                        hl[i] = Hl::String;
                        hl[i + 1] = Hl::String;
                        i += 2;
                        continue;
                    }
                    if starts_with(bytes, cb, i) {
                        let end = i + cb.len();
                        for b in &mut hl[start..end] {
                            *b = Hl::String;
                        }
                        i = end;
                        prev_sep = true;
                        found = true;
                        break;
                    }
                    i += 1;
                }
                if !found {
                    for b in &mut hl[start..len] {
                        *b = Hl::String;
                    }
                    if multiline {
                        *state = HlState::MultiLineString(di as u8);
                    }
                    return apply_hl(line, &hl);
                }
                matched = true;
                break;
            }
        }
        if matched {
            continue;
        }
        // Numbers
        if rules.highlight_numbers
            && prev_sep
            && i < len
            && (bytes[i].is_ascii_digit()
                || (bytes[i] == b'.' && i + 1 < len && bytes[i + 1].is_ascii_digit()))
        {
            let start = i;
            i += 1;
            while i < len
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'.')
            {
                i += 1;
            }
            for b in &mut hl[start..i] {
                *b = Hl::Number;
            }
            prev_sep = false;
            continue;
        }
        // Keywords / types / identifiers
        if prev_sep && i < len && (bytes[i].is_ascii_alphabetic() || bytes[i] == b'_') {
            let start = i;
            i += 1;
            while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let id = &bytes[start..i];
            let hl_type = if kw_search(id, rules.keywords) {
                Some(Hl::Keyword)
            } else if kw_search(id, rules.types) {
                Some(Hl::Type)
            } else {
                None
            };
            if let Some(t) = hl_type {
                for b in &mut hl[start..i] {
                    *b = t;
                }
                prev_sep = false;
                continue;
            }
            if rules.highlight_bang_macros
                && i < len
                && bytes[i] == b'!'
                && (i + 1 >= len || bytes[i + 1] != b'=')
            {
                for b in &mut hl[start..=i] {
                    *b = Hl::Macro;
                }
                i += 1;
                prev_sep = true;
                continue;
            }
            if rules.highlight_fn_calls && i < len && bytes[i] == b'(' {
                for b in &mut hl[start..i] {
                    *b = Hl::Function;
                }
                prev_sep = true;
                continue;
            }
            // UPPER_SNAKE_CASE constants
            if i - start >= 2
                && id
                    .iter()
                    .all(|&b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
                && id.iter().any(|&b| b.is_ascii_uppercase())
            {
                for b in &mut hl[start..i] {
                    *b = Hl::Constant;
                }
            }
            prev_sep = false;
            continue;
        }
        // Operators
        for &op in rules.operators {
            let ob = op.as_bytes();
            if starts_with(bytes, ob, i) {
                for b in &mut hl[i..i + ob.len()] {
                    *b = Hl::Operator;
                }
                i += ob.len();
                prev_sep = true;
                matched = true;
                break;
            }
        }
        if matched {
            continue;
        }
        if matches!(bytes[i], b'(' | b')' | b'[' | b']' | b'{' | b'}') {
            hl[i] = Hl::Bracket;
        }
        prev_sep = is_sep(bytes[i]);
        i += 1;
    }
    *state = HlState::Normal;
    apply_hl(line, &hl)
}

fn starts_with(hay: &[u8], needle: &[u8], pos: usize) -> bool {
    !needle.is_empty() && pos + needle.len() <= hay.len() && &hay[pos..pos + needle.len()] == needle
}

fn is_sep(c: u8) -> bool {
    c.is_ascii_whitespace()
        || matches!(
            c,
            b',' | b'.'
                | b'('
                | b')'
                | b'+'
                | b'-'
                | b'/'
                | b'*'
                | b'='
                | b'~'
                | b'%'
                | b'<'
                | b'>'
                | b'['
                | b']'
                | b'{'
                | b'}'
                | b';'
                | b':'
                | b'&'
                | b'|'
                | b'!'
                | b'^'
                | b'@'
                | b'#'
                | b'?'
        )
}

fn kw_search(id: &[u8], words: &[&str]) -> bool {
    words.binary_search_by(|w| w.as_bytes().cmp(id)).is_ok()
}

fn apply_hl(line: &str, hl: &[Hl]) -> String {
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len() + 64);
    let mut current = Hl::Normal;
    for (i, &b) in bytes.iter().enumerate() {
        let h = if i < hl.len() { hl[i] } else { Hl::Normal };
        if h != current {
            if current != Hl::Normal {
                out.push_str("\x1b[0m");
            }
            if h != Hl::Normal {
                out.push_str(h.ansi());
            }
            current = h;
        }
        out.push(b as char);
    }
    if current != Hl::Normal {
        out.push_str("\x1b[0m");
    }
    out
}

// -- Language rules ----------------------------------------------------------

macro_rules! s {
    ($o:expr, $c:expr, $m:expr) => {
        ($o, $c, $m)
    };
}

static RUST: Rules = Rules {
    line_comment: "//",
    block_comment: ("/*", "*/"),
    strings: &[s!("\"", "\"", false), s!("'", "'", false)],
    keywords: &[
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
        "extern", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut",
        "pub", "ref", "return", "self", "static", "struct", "super", "trait", "type", "unsafe",
        "use", "where", "while", "yield",
    ],
    types: &[
        "Box", "Err", "None", "Ok", "Option", "Result", "Self", "Some", "String", "Vec", "bool",
        "char", "f32", "f64", "false", "i128", "i16", "i32", "i64", "i8", "isize", "str", "true",
        "u128", "u16", "u32", "u64", "u8", "usize",
    ],
    operators: &["&&", "->", "!=", "==", "<=", ">=", "=>", "||"],
    highlight_numbers: true,
    highlight_fn_calls: true,
    highlight_bang_macros: true,
};

static PYTHON: Rules = Rules {
    line_comment: "#",
    block_comment: ("", ""),
    strings: &[
        s!("\"\"\"", "\"\"\"", true),
        s!("'''", "'''", true),
        s!("\"", "\"", false),
        s!("'", "'", false),
    ],
    keywords: &[
        "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del",
        "elif", "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is",
        "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while", "with",
        "yield",
    ],
    types: &[
        "False", "None", "True", "bool", "bytes", "dict", "float", "int", "list", "self", "set",
        "str", "tuple",
    ],
    operators: &["!=", "==", "<=", ">="],
    highlight_numbers: true,
    highlight_fn_calls: true,
    highlight_bang_macros: false,
};

static GO: Rules = Rules {
    line_comment: "//",
    block_comment: ("/*", "*/"),
    strings: &[
        s!("`", "`", true),
        s!("\"", "\"", false),
        s!("'", "'", false),
    ],
    keywords: &[
        "break",
        "case",
        "chan",
        "const",
        "continue",
        "default",
        "defer",
        "else",
        "fallthrough",
        "for",
        "func",
        "go",
        "goto",
        "if",
        "import",
        "interface",
        "map",
        "package",
        "range",
        "return",
        "select",
        "struct",
        "switch",
        "type",
        "var",
    ],
    types: &[
        "bool",
        "byte",
        "complex128",
        "complex64",
        "error",
        "false",
        "float32",
        "float64",
        "int",
        "int16",
        "int32",
        "int64",
        "int8",
        "iota",
        "nil",
        "rune",
        "string",
        "true",
        "uint",
        "uint16",
        "uint32",
        "uint64",
        "uint8",
        "uintptr",
    ],
    operators: &["&&", ":=", "!=", "==", "<=", ">=", "||"],
    highlight_numbers: true,
    highlight_fn_calls: true,
    highlight_bang_macros: false,
};

static TS: Rules = Rules {
    line_comment: "//",
    block_comment: ("/*", "*/"),
    strings: &[
        s!("`", "`", true),
        s!("\"", "\"", false),
        s!("'", "'", false),
    ],
    keywords: &[
        "abstract",
        "as",
        "async",
        "await",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "debugger",
        "default",
        "delete",
        "do",
        "else",
        "enum",
        "export",
        "extends",
        "finally",
        "for",
        "from",
        "function",
        "if",
        "implements",
        "import",
        "in",
        "instanceof",
        "interface",
        "let",
        "new",
        "of",
        "package",
        "private",
        "protected",
        "public",
        "return",
        "static",
        "super",
        "switch",
        "this",
        "throw",
        "try",
        "typeof",
        "var",
        "void",
        "while",
        "with",
        "yield",
    ],
    types: &[
        "Array",
        "Map",
        "Promise",
        "Set",
        "any",
        "bigint",
        "boolean",
        "false",
        "never",
        "null",
        "number",
        "object",
        "string",
        "symbol",
        "true",
        "undefined",
        "unknown",
        "void",
    ],
    operators: &["&&", "!==", "===", "!=", "==", "<=", ">=", "=>", "||"],
    highlight_numbers: true,
    highlight_fn_calls: true,
    highlight_bang_macros: false,
};

static JS: Rules = Rules {
    line_comment: "//",
    block_comment: ("/*", "*/"),
    strings: &[
        s!("`", "`", true),
        s!("\"", "\"", false),
        s!("'", "'", false),
    ],
    keywords: &[
        "async",
        "await",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "debugger",
        "default",
        "delete",
        "do",
        "else",
        "export",
        "extends",
        "finally",
        "for",
        "from",
        "function",
        "if",
        "import",
        "in",
        "instanceof",
        "let",
        "new",
        "of",
        "return",
        "static",
        "super",
        "switch",
        "this",
        "throw",
        "try",
        "typeof",
        "var",
        "void",
        "while",
        "with",
        "yield",
    ],
    types: &[
        "Array",
        "Boolean",
        "Infinity",
        "Map",
        "NaN",
        "Number",
        "Object",
        "Promise",
        "Set",
        "String",
        "false",
        "null",
        "true",
        "undefined",
    ],
    operators: &["&&", "!==", "===", "!=", "==", "<=", ">=", "=>", "||"],
    highlight_numbers: true,
    highlight_fn_calls: true,
    highlight_bang_macros: false,
};

static BASH: Rules = Rules {
    line_comment: "#",
    block_comment: ("", ""),
    strings: &[s!("\"", "\"", false), s!("'", "'", false)],
    keywords: &[
        "break", "case", "continue", "declare", "do", "done", "elif", "else", "esac", "eval",
        "exec", "exit", "export", "fi", "for", "function", "if", "in", "local", "readonly",
        "return", "set", "shift", "source", "then", "trap", "unset", "while",
    ],
    types: &["false", "true"],
    operators: &["&&", "||"],
    highlight_numbers: true,
    highlight_fn_calls: false,
    highlight_bang_macros: false,
};

static C: Rules = Rules {
    line_comment: "//",
    block_comment: ("/*", "*/"),
    strings: &[s!("\"", "\"", false), s!("'", "'", false)],
    keywords: &[
        "auto", "break", "case", "const", "continue", "default", "do", "else", "enum", "extern",
        "for", "goto", "if", "inline", "register", "restrict", "return", "sizeof", "static",
        "struct", "switch", "typedef", "union", "volatile", "while",
    ],
    types: &[
        "NULL", "bool", "char", "double", "false", "float", "int", "int16_t", "int32_t", "int64_t",
        "int8_t", "long", "short", "signed", "size_t", "true", "uint16_t", "uint32_t", "uint64_t",
        "uint8_t", "unsigned", "void",
    ],
    operators: &["&&", "->", "!=", "==", "<=", ">=", "||"],
    highlight_numbers: true,
    highlight_fn_calls: true,
    highlight_bang_macros: false,
};

static TOML: Rules = Rules {
    line_comment: "#",
    block_comment: ("", ""),
    strings: &[
        s!("\"\"\"", "\"\"\"", true),
        s!("'''", "'''", true),
        s!("\"", "\"", false),
        s!("'", "'", false),
    ],
    keywords: &[],
    types: &["false", "true"],
    operators: &[],
    highlight_numbers: true,
    highlight_fn_calls: false,
    highlight_bang_macros: false,
};

/// Look up rules by language tag (as used in markdown code fences).
pub fn rules_for_lang(tag: &str) -> Option<&'static Rules> {
    match tag.to_lowercase().as_str() {
        "rust" | "rs" => Some(&RUST),
        "python" | "py" => Some(&PYTHON),
        "go" | "golang" => Some(&GO),
        "typescript" | "ts" => Some(&TS),
        "javascript" | "js" | "jsx" | "tsx" => Some(&JS),
        "bash" | "sh" | "shell" | "zsh" => Some(&BASH),
        "c" | "cpp" | "c++" | "h" | "cc" | "cxx" => Some(&C),
        "toml" => Some(&TOML),
        _ => None,
    }
}
