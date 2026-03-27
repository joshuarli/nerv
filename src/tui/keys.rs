/// Type-safe key identifier. e.g. "ctrl+enter", "shift+alt+f", "f1", "up".
pub type KeyId = &'static str;

/// Match raw terminal bytes against a named key.
pub fn matches_key(data: &[u8], key: KeyId) -> bool {
    parse_key(data).is_some_and(|k| k == key)
}

/// Parse raw terminal bytes into a key identifier.
/// Handles both VT100/xterm and Kitty CSI-u sequences.
pub fn parse_key(data: &[u8]) -> Option<KeyId> {
    if data.is_empty() {
        return None;
    }

    // Single ASCII bytes
    if data.len() == 1 {
        return match data[0] {
            0x0D => Some("enter"),
            0x0A => Some("ctrl+enter"), // ctrl flips bit 5: CR (0x0D) → LF (0x0A)
            0x09 => Some("tab"),
            0x7F => Some("backspace"),
            0x1B => Some("escape"),
            0x01 => Some("ctrl+a"),
            0x02 => Some("ctrl+b"),
            0x03 => Some("ctrl+c"),
            0x04 => Some("ctrl+d"),
            0x05 => Some("ctrl+e"),
            0x06 => Some("ctrl+f"),
            0x07 => Some("ctrl+g"),
            0x08 => Some("ctrl+h"),
            0x0B => Some("ctrl+k"),
            0x0C => Some("ctrl+l"),
            0x0E => Some("ctrl+n"),
            0x0F => Some("ctrl+o"),
            0x10 => Some("ctrl+p"),
            0x12 => Some("ctrl+r"),
            0x13 => Some("ctrl+s"),
            0x14 => Some("ctrl+t"),
            0x15 => Some("ctrl+u"),
            0x16 => Some("ctrl+v"),
            0x17 => Some("ctrl+w"),
            0x18 => Some("ctrl+x"),
            0x19 => Some("ctrl+y"),
            0x1A => Some("ctrl+z"),
            b' ' => Some("space"),
            _ => None,
        };
    }

    // CSI sequences: ESC [ ...
    if data.len() >= 3 && data[0] == 0x1B && data[1] == b'[' {
        let body = &data[2..];

        // Simple arrow/nav keys: ESC [ A/B/C/D/H/F
        if body.len() == 1 {
            return match body[0] {
                b'A' => Some("up"),
                b'B' => Some("down"),
                b'C' => Some("right"),
                b'D' => Some("left"),
                b'H' => Some("home"),
                b'F' => Some("end"),
                _ => None,
            };
        }

        // Extended keys: ESC [ 1;mod X (modified arrows, etc.)
        if body.len() >= 3 && body[0] == b'1' && body[1] == b';' {
            let modifier = body[2] - b'0';
            let final_byte = body.last().copied()?;
            let base = match final_byte {
                b'A' => "up",
                b'B' => "down",
                b'C' => "right",
                b'D' => "left",
                b'H' => "home",
                b'F' => "end",
                _ => return None,
            };
            return match modifier {
                2 => Some(match base {
                    "up" => "shift+up",
                    "down" => "shift+down",
                    "right" => "shift+right",
                    "left" => "shift+left",
                    _ => return None,
                }),
                5 => Some(match base {
                    "up" => "ctrl+up",
                    "down" => "ctrl+down",
                    "right" => "ctrl+right",
                    "left" => "ctrl+left",
                    _ => return None,
                }),
                6 => Some(match base {
                    "up" => "shift+ctrl+up",
                    "down" => "shift+ctrl+down",
                    "right" => "shift+ctrl+right",
                    "left" => "shift+ctrl+left",
                    "p" => "shift+ctrl+p",
                    _ => return None,
                }),
                _ => None,
            };
        }

        // Tilde keys: ESC [ N ~ (Insert, Delete, PgUp, PgDn)
        // Also handles modifyOtherKeys: ESC [ 27 ; mod ; code ~
        if body.len() >= 2 && body[body.len() - 1] == b'~' {
            let num_str = std::str::from_utf8(&body[..body.len() - 1]).ok()?;

            // modifyOtherKeys format: 27;modifier;keycode
            if let Some(rest) = num_str.strip_prefix("27;") {
                let parts: Vec<&str> = rest.split(';').collect();
                if parts.len() == 2 {
                    let modifier: u32 = parts[0].parse().ok()?;
                    let keycode: u32 = parts[1].parse().ok()?;
                    let shift = modifier == 2 || modifier == 6;
                    let ctrl = modifier == 5 || modifier == 6;
                    return match keycode {
                        13 if ctrl => Some("ctrl+enter"),
                        13 if shift => Some("shift+enter"),
                        _ => None,
                    };
                }
            }

            return match num_str {
                "2" => Some("insert"),
                "3" => Some("delete"),
                "5" => Some("page_up"),
                "6" => Some("page_down"),
                "15" => Some("f5"),
                "17" => Some("f6"),
                "18" => Some("f7"),
                "19" => Some("f8"),
                _ => None,
            };
        }

        // Kitty CSI-u: ESC [ codepoint ; modifiers u
        if body.len() >= 3 && body[body.len() - 1] == b'u' {
            let params = std::str::from_utf8(&body[..body.len() - 1]).ok()?;
            let parts: Vec<&str> = params.split(';').collect();
            if parts.len() >= 2 {
                let _codepoint: u32 = parts[0].parse().ok()?;
                let modifier: u32 = parts[1].split(':').next()?.parse().ok()?;
                let mods = modifier.saturating_sub(1);
                let _shift = mods & 1 != 0;
                let _alt = mods & 2 != 0;
                let ctrl = mods & 4 != 0;

                // Map codepoint to base key name
                let base = match _codepoint {
                    8 | 127 => Some("backspace"),
                    9 => Some("tab"),
                    13 => Some("enter"),
                    27 => Some("escape"),
                    32 => Some("space"),
                    // Printable ASCII: a-z mapped to ctrl+letter
                    cp @ 97..=122 if ctrl => {
                        return Some(match cp {
                            97 => "ctrl+a",
                            98 => "ctrl+b",
                            99 => "ctrl+c",
                            100 => "ctrl+d",
                            101 => "ctrl+e",
                            102 => "ctrl+f",
                            103 => "ctrl+g",
                            104 => "ctrl+h",
                            107 => "ctrl+k",
                            108 => "ctrl+l",
                            110 => "ctrl+n",
                            111 => "ctrl+o",
                            112 => "ctrl+p",
                            114 => "ctrl+r",
                            115 => "ctrl+s",
                            116 => "ctrl+t",
                            117 => "ctrl+u",
                            118 => "ctrl+v",
                            119 => "ctrl+w",
                            120 => "ctrl+x",
                            121 => "ctrl+y",
                            122 => "ctrl+z",
                            _ => return None,
                        });
                    }
                    _ => None,
                };

                if let Some(name) = base {
                    return match (name, ctrl, _shift, _alt) {
                        ("enter", false, false, false) => Some("enter"),
                        ("enter", false, true, false) => Some("shift+enter"),
                        ("enter", true, false, false) => Some("ctrl+enter"),
                        ("tab", false, false, false) => Some("tab"),
                        ("tab", false, true, false) => Some("shift+tab"),
                        ("backspace", false, false, false) => Some("backspace"),
                        ("escape", false, false, false) => Some("escape"),
                        ("space", false, false, false) => Some("space"),
                        _ => None,
                    };
                }

                return None;
            }
        }
    }

    // SS3 sequences: ESC O P/Q/R/S (F1-F4)
    if data.len() == 3 && data[0] == 0x1B && data[1] == b'O' {
        return match data[2] {
            b'P' => Some("f1"),
            b'Q' => Some("f2"),
            b'R' => Some("f3"),
            b'S' => Some("f4"),
            _ => None,
        };
    }

    // Alt combinations: ESC + char
    if data.len() == 2 && data[0] == 0x1B {
        return match data[1] {
            b'f' => Some("alt+f"),
            b'b' => Some("alt+b"),
            b'd' => Some("alt+d"),
            0x7F => Some("alt+backspace"), // ESC + DEL
            _ => None,
        };
    }

    // Alt+arrow: ESC [ 1;3 D/C/A/B (modifier=3 = Alt)
    if data.len() >= 6 && data[0] == 0x1B && data[1] == b'[' {
        let body = &data[2..];
        if body.len() >= 4 && body[0] == b'1' && body[1] == b';' && body[2] == b'3' {
            return match body[3] {
                b'D' => Some("alt+left"),
                b'C' => Some("alt+right"),
                b'A' => Some("alt+up"),
                b'B' => Some("alt+down"),
                _ => None,
            };
        }
    }

    None
}

/// Check if a sequence is a Kitty key release event.
pub fn is_key_release(data: &[u8]) -> bool {
    // Kitty release events have :3 in the modifier field
    // e.g., ESC [ 97;1:3u
    if data.len() >= 4
        && data[0] == 0x1B
        && data[1] == b'['
        && let Ok(s) = std::str::from_utf8(&data[2..])
        && s.ends_with('u')
        && s.contains(":3")
    {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_keys() {
        assert!(matches_key(b"\r", "enter"));
        assert!(matches_key(b"\x1b", "escape"));
        assert!(matches_key(b"\t", "tab"));
        assert!(matches_key(b"\x7f", "backspace"));
        assert!(matches_key(b"\x03", "ctrl+c"));
    }

    #[test]
    fn arrow_keys() {
        assert!(matches_key(b"\x1b[A", "up"));
        assert!(matches_key(b"\x1b[B", "down"));
        assert!(matches_key(b"\x1b[C", "right"));
        assert!(matches_key(b"\x1b[D", "left"));
    }

    #[test]
    fn function_keys() {
        assert!(matches_key(b"\x1bOP", "f1"));
        assert!(matches_key(b"\x1bOQ", "f2"));
    }

    #[test]
    fn ctrl_keys() {
        assert!(matches_key(b"\x0e", "ctrl+n"));
        assert!(matches_key(b"\x10", "ctrl+p"));
        assert!(matches_key(b"\x14", "ctrl+t"));
    }

    #[test]
    fn key_release_detection() {
        assert!(is_key_release(b"\x1b[97;1:3u"));
        assert!(!is_key_release(b"\x1b[97;1u"));
    }
}
