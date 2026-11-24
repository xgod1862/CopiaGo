use std::env;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

static NEXT_DIRECTORY: AtomicU32 = AtomicU32::new(0);

pub(crate) struct TestDir {
    path: PathBuf,
    resolved: PathBuf,
}

impl TestDir {
    pub(crate) fn new() -> Self {
        let parent = env::temp_dir();
        let resolved_parent = fs::canonicalize(&parent).expect("resolve temporary root");
        loop {
            let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let name = format!("copiago-core-test-{}-{sequence}", std::process::id());
            let path = parent.join(&name);
            match fs::create_dir(&path) {
                Ok(()) => {
                    return Self {
                        path,
                        resolved: resolved_parent.join(name),
                    };
                }
                Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create temporary test directory: {error}"),
            }
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        if fs::canonicalize(&self.path).is_ok_and(|path| path == self.resolved) {
            let _ = fs::remove_dir_all(&self.resolved);
        }
    }
}
