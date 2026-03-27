use nerv::tui::keys::*;

#[test]
fn kitty_csi_u_ctrl_c() {
    // ESC [ 99 ; 5 u = Ctrl+C in Kitty protocol
    assert!(matches_key(b"\x1b[99;5u", "ctrl+c"));
}

#[test]
fn kitty_csi_u_ctrl_z() {
    assert!(matches_key(b"\x1b[122;5u", "ctrl+z"));
}

#[test]
fn kitty_csi_u_enter() {
    assert!(matches_key(b"\x1b[13;1u", "enter"));
}

#[test]
fn kitty_csi_u_shift_enter() {
    assert!(matches_key(b"\x1b[13;2u", "shift+enter"));
}

#[test]
fn alt_f_recognized() {
    // ESC + f = Alt+F
    assert!(matches_key(b"\x1bf", "alt+f"));
}

#[test]
fn alt_b_recognized() {
    assert!(matches_key(b"\x1bb", "alt+b"));
}

#[test]
fn alt_d_recognized() {
    assert!(matches_key(b"\x1bd", "alt+d"));
}

#[test]
fn alt_backspace_recognized() {
    // ESC + DEL = Alt+Backspace
    assert!(matches_key(b"\x1b\x7f", "alt+backspace"));
}

#[test]
fn page_up_down() {
    assert!(matches_key(b"\x1b[5~", "page_up"));
    assert!(matches_key(b"\x1b[6~", "page_down"));
}

#[test]
fn delete_key() {
    assert!(matches_key(b"\x1b[3~", "delete"));
}

#[test]
fn home_end() {
    assert!(matches_key(b"\x1b[H", "home"));
    assert!(matches_key(b"\x1b[F", "end"));
}

#[test]
fn space_key() {
    assert!(matches_key(b" ", "space"));
}

#[test]
fn unrecognized_returns_none() {
    assert_eq!(parse_key(b"\x1b[999z"), None);
    assert_eq!(parse_key(b""), None);
}
