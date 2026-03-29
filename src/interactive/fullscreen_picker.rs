/// Full-screen alt-screen picker.
///
/// Callers implement [`FullscreenList`] and pass it to
/// [`run_fullscreen_picker`], which enters the alt screen, drives its own
/// blocking stdin read loop, and returns the selected ID (or `None` if the user
/// cancelled).
///
/// The main TUI is not touched during the picker's lifetime; the alt screen
/// provides a completely separate canvas. When `run_fullscreen_picker` returns,
/// the main TUI is right where it was.
use std::io::{self, Write};
use std::mem::MaybeUninit;

use crate::tui::keys;
use crate::tui::stdin_buffer::{StdinBuffer, StdinEvent};

// ─────────────────────────────── trait ──────────────────────────────────────

/// A list that can be rendered full-screen and navigated.
pub trait FullscreenList {
    /// Draw the complete UI into `out`.
    /// `cols` and `rows` are the current terminal dimensions.
    fn render(&self, out: &mut dyn Write, cols: u16, rows: u16);

    fn move_up(&mut self);
    fn move_down(&mut self);
    fn move_page_up(&mut self) {}
    fn move_page_down(&mut self) {}
    fn push_char(&mut self, ch: char);
    fn pop_char(&mut self);
    fn clear_query(&mut self);

    /// Called when Enter is pressed.  Returns the selected ID string, or `None`
    /// if nothing is selected (list is empty, etc.).
    fn enter(&self) -> Option<String>;

    /// Handle a raw key sequence that wasn't handled by the generic loop.
    /// Returns `true` if the key was consumed and a redraw is needed.
    fn handle_extra_key(&mut self, _seq: &[u8]) -> bool {
        false
    }
}

// ─────────────────────────── runner ─────────────────────────────────────────

/// Enter the alt screen, run the picker loop, return when the user selects or
/// cancels.
///
/// Returns `Some(id)` on a confirmed selection, `None` on Escape / Ctrl-C.
pub fn run_fullscreen_picker(list: &mut dyn FullscreenList) -> Option<String> {
    let mut out = io::stdout();

    // ── drain stale stdin ──────────────────────────────────────────────────
    // The caller paused the stdin reader thread, but bytes may have been
    // buffered in the kernel.  Drain them so the picker starts clean.
    drain_stdin();

    // ── enter alt screen ───────────────────────────────────────────────────
    // Switch to alt screen buffer, hide cursor.
    let _ = out.write_all(b"\x1b[?1049h\x1b[?25l");
    let _ = out.flush();
    render_frame(&mut out, list);

    // ── input loop ─────────────────────────────────────────────────────────
    let mut stdin_buf = StdinBuffer::new();
    let mut result: Option<String> = None;

    // We read stdin in blocking mode.  Raw mode is already enabled by
    // Terminal::start(), so bytes arrive one keystroke at a time.
    'outer: loop {
        let mut buf = [0u8; 256];
        let n = unsafe {
            libc::read(libc::STDIN_FILENO, buf.as_mut_ptr() as *mut libc::c_void, buf.len())
        };
        if n == 0 {
            break; // EOF
        }
        if n < 0 {
            // EINTR (signal interrupted read) — just retry.
            let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            if errno == libc::EINTR {
                continue;
            }
            break; // real error
        }

        let events = stdin_buf.process(&buf[..n as usize]);
        let mut needs_redraw = false;

        for event in events {
            match event {
                StdinEvent::Sequence(seq) => {
                    if keys::matches_key(&seq, "escape") || keys::matches_key(&seq, "ctrl+c") {
                        break 'outer;
                    } else if keys::matches_key(&seq, "enter") {
                        result = list.enter();
                        if result.is_some() {
                            break 'outer;
                        }
                    } else if keys::matches_key(&seq, "up") {
                        list.move_up();
                        needs_redraw = true;
                    } else if keys::matches_key(&seq, "down") {
                        list.move_down();
                        needs_redraw = true;
                    } else if keys::matches_key(&seq, "left") || keys::matches_key(&seq, "page_up")
                    {
                        list.move_page_up();
                        needs_redraw = true;
                    } else if keys::matches_key(&seq, "right")
                        || keys::matches_key(&seq, "page_down")
                    {
                        list.move_page_down();
                        needs_redraw = true;
                    } else if list.handle_extra_key(&seq) {
                        // handle_extra_key has priority (e.g. Ctrl+U for tree filter vs.
                        // clear_query)
                        needs_redraw = true;
                    } else if keys::matches_key(&seq, "ctrl+u") {
                        list.clear_query();
                        needs_redraw = true;
                    } else if keys::matches_key(&seq, "backspace") {
                        list.pop_char();
                        needs_redraw = true;
                    } else {
                        // Printable chars (including multi-byte UTF-8).
                        if let Ok(s) = std::str::from_utf8(&seq)
                            && let Some(ch) = single_printable(s)
                        {
                            list.push_char(ch);
                            needs_redraw = true;
                        }
                    }
                }
                StdinEvent::Paste(text) => {
                    for ch in text.chars() {
                        if !ch.is_control() {
                            list.push_char(ch);
                        }
                    }
                    needs_redraw = true;
                }
            }
        }

        if needs_redraw {
            render_frame(&mut out, list);
        }
    }

    // ── exit alt screen ────────────────────────────────────────────────────
    let _ = write!(out, "\x1b[?25h\x1b[?1049l");
    let _ = out.flush();

    result
}

/// Clear and repaint the entire alt screen inside a synchronized output block.
fn render_frame(out: &mut io::Stdout, list: &mut dyn FullscreenList) {
    let (cols, rows) = term_size();
    // Begin synchronized output so the terminal batches clear + content.
    let _ = out.write_all(b"\x1b[?2026h\x1b[H\x1b[2J");
    list.render(out, cols, rows);
    // End synchronized output — terminal flushes in one
    let _ = out.write_all(b"\x1b[?2026l");
    let _ = out.flush();
}

// ─────────────────────────── helpers ────────────────────────────────────────

/// Non-blocking drain of any bytes sitting in stdin.
fn drain_stdin() {
    let mut buf = [0u8; 256];
    let mut pfd = libc::pollfd { fd: libc::STDIN_FILENO, events: libc::POLLIN, revents: 0 };
    loop {
        let ready = unsafe { libc::poll(&mut pfd, 1, 0) }; // timeout=0: instant
        if ready <= 0 {
            break;
        }
        let n = unsafe {
            libc::read(libc::STDIN_FILENO, buf.as_mut_ptr() as *mut libc::c_void, buf.len())
        };
        if n <= 0 {
            break;
        }
    }
}

/// Get current terminal dimensions via ioctl TIOCGWINSZ.
fn term_size() -> (u16, u16) {
    unsafe {
        let mut ws = MaybeUninit::<libc::winsize>::uninit();
        if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, ws.as_mut_ptr()) == 0 {
            let ws = ws.assume_init();
            (ws.ws_col, ws.ws_row)
        } else {
            (80, 24)
        }
    }
}

/// If `s` is a single printable (non-control) Unicode character, return it.
fn single_printable(s: &str) -> Option<char> {
    let mut chars = s.chars();
    let ch = chars.next()?;
    if chars.next().is_none() && !ch.is_control() { Some(ch) } else { None }
}
