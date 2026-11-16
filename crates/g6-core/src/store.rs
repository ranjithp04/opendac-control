//! On-disk locations and crash-safe JSON files.
//!
//! Presets live in the OS config dir, runtime state (last known settings,
//! Direct Mode saved state) in the OS data dir; both via the `directories`
//! crate. Files are written atomically (temp file in the same directory,
//! `fsync`, rename) so a crash never leaves a half-written file behind.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use directories::ProjectDirs;
use serde::de::DeserializeOwned;
use serde::Serialize;

/// Application name used for the OS config / data directories.
pub const APP_NAME: &str = "opendac-control";

/// Where files live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
}

impl Paths {
    /// OS config / data dirs (`None` if the home directory cannot be found).
    pub fn system() -> Option<Paths> {
        let dirs = ProjectDirs::from("", "", APP_NAME)?;
        Some(Paths {
            config_dir: dirs.config_dir().to_path_buf(),
            data_dir: dirs.data_dir().to_path_buf(),
        })
    }

    /// Everything below `root` (`root/config`, `root/data`); for tests and
    /// for a user-chosen directory.
    pub fn under(root: &Path) -> Paths {
        Paths {
            config_dir: root.join("config"),
            data_dir: root.join("data"),
        }
    }

    /// Directory holding `NAME.json` presets.
    pub fn presets_dir(&self) -> PathBuf {
        self.config_dir.join("presets")
    }

    /// Last known device settings (re-applied on reconnect).
    pub fn last_known_file(&self) -> PathBuf {
        self.data_dir.join("last_known.json")
    }

    /// Direct Mode saved state.
    pub fn direct_file(&self) -> PathBuf {
        self.data_dir.join("direct_mode.json")
    }
}

/// A file could not be read, written or parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    Io { path: PathBuf, message: String },
    Parse { path: PathBuf, message: String },
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Io { path, message } => write!(f, "{}: {message}", path.display()),
            StoreError::Parse { path, message } => {
                write!(f, "{}: invalid JSON: {message}", path.display())
            }
        }
    }
}

impl std::error::Error for StoreError {}

fn io_err(path: &Path, e: io::Error) -> StoreError {
    StoreError::Io {
        path: path.to_path_buf(),
        message: e.to_string(),
    }
}

/// Write `bytes` to `path` atomically, creating parent directories.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    // Unique per call: writers of one file in one process (the session thread
    // and a refreshing caller) must not share a temp file.
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".{name}.{}.{n}.tmp", std::process::id()));
    let result = (|| {
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(io_err(path, e));
    }
    // Make the rename durable where the platform allows it.
    #[cfg(unix)]
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

/// Serialize `value` as pretty JSON and write it atomically.
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), StoreError> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|e| StoreError::Parse {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    bytes.push(b'\n');
    write_atomic(path, &bytes)
}

/// Read a JSON file; `Ok(None)` if it does not exist.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, StoreError> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_err(path, e)),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| StoreError::Parse {
            path: path.to_path_buf(),
            message: e.to_string(),
        })
}

/// Delete a file; a missing file is not an error.
pub fn remove_file(path: &Path) -> Result<(), StoreError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_err(path, e)),
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Unique temp directory, removed on drop.
    pub struct TempDir(PathBuf);

    impl TempDir {
        pub fn new() -> Self {
            static N: AtomicU32 = AtomicU32::new(0);
            let p = std::env::temp_dir().join(format!(
                "g6-core-test-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            std::fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::TempDir;
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn atomic_write_roundtrip_and_overwrite() {
        let t = TempDir::new();
        let p = t.path().join("a/b/x.json");
        let mut m = BTreeMap::new();
        m.insert("k".to_string(), 1.5f32);
        write_json(&p, &m).unwrap();
        assert_eq!(read_json::<BTreeMap<String, f32>>(&p).unwrap(), Some(m));
        write_json(&p, &BTreeMap::<String, f32>::new()).unwrap();
        assert_eq!(
            read_json::<BTreeMap<String, f32>>(&p).unwrap(),
            Some(BTreeMap::new())
        );
        // No temp files left behind.
        let left: Vec<_> = fs::read_dir(p.parent().unwrap()).unwrap().collect();
        assert_eq!(left.len(), 1);
    }

    #[test]
    fn concurrent_writers_of_one_file_do_not_clash() {
        let t = TempDir::new();
        let p = t.path().join("x.json");
        let threads: Vec<_> = (0..8u32)
            .map(|i| {
                let p = p.clone();
                std::thread::spawn(move || {
                    for j in 0..50u32 {
                        write_json(&p, &(i * 100 + j)).unwrap();
                    }
                })
            })
            .collect();
        for h in threads {
            h.join().unwrap();
        }
        assert!(read_json::<u32>(&p).unwrap().is_some());
        let left: Vec<_> = fs::read_dir(t.path()).unwrap().collect();
        assert_eq!(left.len(), 1, "temp files left behind");
    }

    #[test]
    fn missing_file_is_none_and_garbage_is_parse_error() {
        let t = TempDir::new();
        let p = t.path().join("x.json");
        assert_eq!(read_json::<u32>(&p).unwrap(), None);
        fs::write(&p, "nope").unwrap();
        assert!(matches!(
            read_json::<u32>(&p),
            Err(StoreError::Parse { .. })
        ));
        remove_file(&p).unwrap();
        remove_file(&p).unwrap();
    }

    #[test]
    fn paths_layout() {
        let p = Paths::under(Path::new("/r"));
        assert_eq!(p.presets_dir(), Path::new("/r/config/presets"));
        assert_eq!(p.direct_file(), Path::new("/r/data/direct_mode.json"));
        assert_eq!(p.last_known_file(), Path::new("/r/data/last_known.json"));
    }
}
