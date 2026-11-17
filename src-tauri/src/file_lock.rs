use std::fs::File;

/// Owns an already acquired OS lock and releases it before closing the file.
/// Closing alone can leave flock held by a child between process creation and exec.
#[derive(Debug)]
pub struct FileLock(pub File);

impl Drop for FileLock {
    fn drop(&mut self) {
        if let Err(error) = self.0.unlock() {
            eprintln!("Unable to release file lock: {error}");
        }
    }
}
