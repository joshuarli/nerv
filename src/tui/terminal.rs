use std::io::{self, Stdout, Write};
use std::mem::MaybeUninit;

pub trait Terminal: Send {
    fn start(&mut self);
    fn stop(&mut self);
    fn restart(&mut self);
    fn write_bytes(&mut self, data: &[u8]);
    /// Write text to the main screen scrollback (exits alt screen, prints, re-enters).
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
        Self {
            stdout: io::stdout(),
            original_termios: None,
            kitty_active: false,
        }
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

        // Hide cursor, enable bracketed paste, disable line wrap
        let _ = self.stdout.write_all(b"\x1b[?25l\x1b[?2004h\x1b[?7l");
        // xterm modifyOtherKeys mode 2
        let _ = self.stdout.write_all(b"\x1b[>4;2m");
        let _ = self.stdout.flush();
        self.kitty_active = false;
    }

    fn stop(&mut self) {
        // Disable modifyOtherKeys
        let _ = self.stdout.write_all(b"\x1b[>4;0m");
        // Show cursor, disable bracketed paste, enable line wrap
        // Position cursor at bottom-left so shell prompt appears cleanly
        let rows = terminal_size().1;
        let _ = write!(self.stdout, "\x1b[{};1H\x1b[J", rows);
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
        self.start();
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
