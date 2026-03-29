use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Serializes concurrent mutations to the same file.
pub struct FileMutationQueue {
    locks: Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>,
}

impl Default for FileMutationQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl FileMutationQueue {
    pub fn new() -> Self {
        Self { locks: Mutex::new(HashMap::new()) }
    }

    /// Execute `f` while holding the per-file lock.
    pub fn with<F, T>(&self, path: &Path, f: F) -> T
    where
        F: FnOnce() -> T,
    {
        let lock = {
            let mut locks = self.locks.lock().unwrap();
            locks.entry(path.to_path_buf()).or_insert_with(|| Arc::new(Mutex::new(()))).clone()
        };
        let _guard = lock.lock().unwrap();
        f()
    }
}
