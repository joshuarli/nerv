/// Integration test: enter_raw_mode uses TCSAFLUSH to discard queued input.
///
/// Regression test for the bug where ^G (open external editor) would leave
/// bytes in the kernel input queue that got injected into the TUI editor buffer
/// on return. `enter_raw_mode` must use TCSAFLUSH not TCSANOW.
///
/// Verified manually that the test FAILS if TCSAFLUSH is replaced with TCSANOW
/// in enter_raw_mode (TCSANOW: got b"poison", TCSAFLUSH: got nothing).
use std::os::unix::io::RawFd;

use nerv::tui::terminal::enter_raw_mode;

/// Open a PTY pair. Returns (master_fd, slave_fd).
fn open_pty() -> (RawFd, RawFd) {
    unsafe {
        let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
        assert!(master >= 0, "posix_openpt failed");
        assert_eq!(libc::grantpt(master), 0);
        assert_eq!(libc::unlockpt(master), 0);

        let name = libc::ptsname(master);
        assert!(!name.is_null());

        let slave = libc::open(name, libc::O_RDWR | libc::O_NOCTTY);
        assert!(slave >= 0, "open slave pty failed");
        (master, slave)
    }
}

fn close_fd(fd: RawFd) {
    unsafe { libc::close(fd); }
}

fn set_nonblocking(fd: RawFd) {
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
    }
}

/// Read up to `n` bytes from `fd` in non-blocking mode.
/// Returns the bytes read, or an empty vec if none are available.
fn try_read(fd: RawFd, n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    let ret = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, n) };
    if ret <= 0 { vec![] } else { buf[..ret as usize].to_vec() }
}

#[test]
fn enter_raw_mode_flushes_queued_input() {
    let (master, slave) = open_pty();

    // Put the slave into raw mode initially (like terminal::start() does).
    let original = unsafe {
        let mut t = std::mem::MaybeUninit::<libc::termios>::uninit();
        assert_eq!(libc::tcgetattr(slave, t.as_mut_ptr()), 0);
        let orig = t.assume_init();
        let mut raw = orig;
        libc::cfmakeraw(&mut raw);
        libc::tcsetattr(slave, libc::TCSANOW, &raw);
        orig
    };

    // Write bytes to the master — they arrive in the slave's input queue,
    // simulating an editor process echoing text while the terminal was in
    // cooked mode.
    let poison = b"should-be-flushed";
    unsafe { libc::write(master, poison.as_ptr() as *const libc::c_void, poison.len()); }

    // Give the kernel a moment to move the bytes into the slave's queue.
    std::thread::sleep(std::time::Duration::from_millis(50));

    // Now simulate restart(): re-enter raw mode. This is the call under test.
    // If it uses TCSANOW the queued bytes survive; TCSAFLUSH discards them.
    enter_raw_mode(slave, &original);

    // Read from slave in non-blocking mode — should get nothing if TCSAFLUSH
    // correctly discarded the queued input.
    set_nonblocking(slave);
    let got = try_read(slave, 64);

    close_fd(master);
    close_fd(slave);

    assert!(
        got.is_empty(),
        "enter_raw_mode left queued bytes in the input buffer: {:?}\n\
         This means TCSAFLUSH is not being used — bytes written by an external\n\
         editor would leak into the TUI as spurious key events.",
        String::from_utf8_lossy(&got)
    );
}
