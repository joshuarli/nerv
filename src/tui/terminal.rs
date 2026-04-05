use std::io::{self, Stdout, Write};
use std::mem::MaybeUninit;
use std::os::unix::io::RawFd;

pub trait Terminal: Send {
    fn start(&mut self);
    fn stop(&mut self);
    fn restart(&mut self);
    fn write_bytes(&mut self, data: &[u8]);
    /// Write text to the main screen scrollback (exits alt screen, prints,
    /// re-enters).
    fn dump_scrollback(&mut self, text: &str);
    fn columns(&self) -> u16;
    fn rows(&self) -> u16;
    fn hide_cursor(&mut self);
    fn show_cursor(&mut self);
    fn kitty_protocol_active(&self) -> bool;
}

pub struct ProcessTerminal {
    stdout: Stdout,
    original_termios: Option<libc::termios>,
    kitty_active: bool,
}

impl Default for ProcessTerminal {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessTerminal {
    pub fn new() -> Self {
        Self { stdout: io::stdout(), original_termios: None, kitty_active: false }
    }
}

impl Terminal for ProcessTerminal {
    fn start(&mut self) {
        // Save original termios and enable raw mode
        unsafe {
            let mut termios = MaybeUninit::<libc::termios>::uninit();
            if libc::tcgetattr(libc::STDIN_FILENO, termios.as_mut_ptr()) == 0 {
                let original = termios.assume_init();
                self.original_termios = Some(original);

                let mut raw = original;
                libc::cfmakeraw(&mut raw);
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw);
            }
        }

        // Stay on primary screen for scrollback persistence
        // Hide cursor, enable bracketed paste, disable line wrap
        let _ = self.stdout.write_all(b"\x1b[?25l\x1b[?2004h\x1b[?7l");
        // xterm modifyOtherKeys mode 2.
        // In tmux we must wrap the escape in a DCS passthrough so it reaches the
        // outer terminal; tmux then re-encodes the extended key sequences it
        // receives and forwards them to us (requires `extended-keys on` in
        // tmux.conf, or tmux ≥ 3.3 with the default).
        if std::env::var_os("TMUX").is_some() {
            // DCS passthrough: ESC P tmux ; ESC <payload> ESC backslash
            let _ = self.stdout.write_all(b"\x1bPtmux;\x1b\x1b[>4;2m\x1b\\");
        } else {
            let _ = self.stdout.write_all(b"\x1b[>4;2m");
        }
        let _ = self.stdout.flush();
        self.kitty_active = false;
    }

    fn stop(&mut self) {
        // Disable modifyOtherKeys (wrapped in DCS passthrough when in tmux)
        if std::env::var_os("TMUX").is_some() {
            let _ = self.stdout.write_all(b"\x1bPtmux;\x1b\x1b[>4;0m\x1b\\");
        } else {
            let _ = self.stdout.write_all(b"\x1b[>4;0m");
        }
        // Leave primary screen mode (no ?1049l needed), show cursor,
        // disable bracketed paste, enable line wrap
        let _ = self.stdout.write_all(b"\x1b[0 q\x1b[?25h\x1b[?2004l\x1b[?7h");
        let _ = self.stdout.flush();

        // Restore original termios, flushing pending input to avoid ^C echo
        if let Some(ref original) = self.original_termios {
            unsafe {
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, original);
            }
        }
    }

    fn restart(&mut self) {
        // Re-enter raw mode using TCSAFLUSH to discard any bytes that
        // accumulated in the input queue while the external editor was running
        // (echoed text, editor status output, etc.). Without the flush those
        // bytes would be processed as key events and corrupt the editor buffer.
        if let Some(ref original) = self.original_termios {
            enter_raw_mode(libc::STDIN_FILENO, original);
        }
        // Re-enable TUI escape sequences (cursor hide, bracketed paste, no wrap,
        // modifyOtherKeys).
        let _ = self.stdout.write_all(b"\x1b[?25l\x1b[?2004h\x1b[?7l");
        if std::env::var_os("TMUX").is_some() {
            let _ = self.stdout.write_all(b"\x1bPtmux;\x1b\x1b[>4;2m\x1b\\");
        } else {
            let _ = self.stdout.write_all(b"\x1b[>4;2m");
        }
        let _ = self.stdout.flush();
    }

    fn write_bytes(&mut self, data: &[u8]) {
        let _ = self.stdout.write_all(data);
        let _ = self.stdout.flush();
    }

    fn dump_scrollback(&mut self, text: &str) {
        // Temporarily restore cooked mode for proper \n → \r\n handling
        if let Some(ref original) = self.original_termios {
            unsafe {
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, original);
            }
        }
        let _ = self.stdout.write_all(b"\x1b[?7h\x1b[?25h");
        let _ = self.stdout.write_all(text.as_bytes());
        let _ = self.stdout.write_all(b"\n");
        let _ = self.stdout.flush();
        // Re-enter raw mode
        unsafe {
            let mut termios = MaybeUninit::<libc::termios>::uninit();
            if libc::tcgetattr(libc::STDIN_FILENO, termios.as_mut_ptr()) == 0 {
                let mut raw = termios.assume_init();
                libc::cfmakeraw(&mut raw);
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw);
            }
        }
        let _ = self.stdout.write_all(b"\x1b[?7l\x1b[?25l");
        let _ = self.stdout.flush();
    }

    fn columns(&self) -> u16 {
        terminal_size().0
    }

    fn rows(&self) -> u16 {
        terminal_size().1
    }

    fn hide_cursor(&mut self) {
        let _ = self.stdout.write_all(b"\x1b[?25l");
        let _ = self.stdout.flush();
    }

    fn show_cursor(&mut self) {
        let _ = self.stdout.write_all(b"\x1b[?25h");
        let _ = self.stdout.flush();
    }

    fn kitty_protocol_active(&self) -> bool {
        self.kitty_active
    }
}

impl Drop for ProcessTerminal {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Enter raw mode on `fd`, discarding any bytes already in the input queue
/// (`TCSAFLUSH`). Used by `restart()` to prevent bytes that arrived while an
/// external editor was running from leaking into the TUI as key events.
pub fn enter_raw_mode(fd: RawFd, original: &libc::termios) {
    unsafe {
        let mut raw = *original;
        libc::cfmakeraw(&mut raw);
        libc::tcsetattr(fd, libc::TCSAFLUSH, &raw);
    }
}

fn terminal_size() -> (u16, u16) {
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


