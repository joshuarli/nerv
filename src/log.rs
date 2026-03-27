use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::Mutex;

static LOG_FILE: Mutex<Option<File>> = Mutex::new(None);
static LOG_LEVEL: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(Level::Warn as u8);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Level {
    Debug = 0,
    Info = 1,
    Warn = 2,
    Error = 3,
}

impl std::str::FromStr for Level {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        match s.to_lowercase().as_str() {
            "debug" => Ok(Self::Debug),
            "info" => Ok(Self::Info),
            "warn" | "warning" => Ok(Self::Warn),
            "error" => Ok(Self::Error),
            _ => Err(()),
        }
    }
}

pub fn init(path: &std::path::Path) {
    if let Ok(file) = OpenOptions::new().create(true).append(true).open(path) {
        *LOG_FILE.lock().unwrap() = Some(file);
    }
}

pub fn set_level(level: Level) {
    LOG_LEVEL.store(level as u8, std::sync::atomic::Ordering::Relaxed);
}

pub fn debug(msg: &str) {
    write_log(Level::Debug, "DEBUG", msg);
}

pub fn info(msg: &str) {
    write_log(Level::Info, "INFO", msg);
}

pub fn warn(msg: &str) {
    write_log(Level::Warn, "WARN", msg);
}

pub fn error(msg: &str) {
    write_log(Level::Error, "ERROR", msg);
}

fn write_log(level: Level, label: &str, msg: &str) {
    let min = LOG_LEVEL.load(std::sync::atomic::Ordering::Relaxed);
    if (level as u8) < min {
        return;
    }
    if let Some(ref mut file) = *LOG_FILE.lock().unwrap() {
        let _ = writeln!(
            file,
            "{} [{}] {}",
            crate::session::types::now_iso(),
            label,
            msg
        );
    }
}
