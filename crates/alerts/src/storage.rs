//! Cooperative process locks and durable atomic alert-book writes.
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::Path,
};
use tr_core::price_alerts::Book;

pub struct Lock(File);
impl Lock {
    fn open(path: &Path) -> io::Result<File> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        OpenOptions::new().create(true).truncate(false).read(true).write(true).open(path)
    }
    pub fn acquire(path: &Path) -> io::Result<Self> {
        let file = Self::open(path)?;
        file.lock()?;
        Ok(Self(file))
    }
    /// App startup never waits out a daemon's network cycle.
    pub fn acquire_app(path: &Path) -> io::Result<Self> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(900);
        loop {
            if let Some(lock) = Self::try_acquire(path)? {
                return Ok(lock);
            }
            if std::time::Instant::now() >= deadline {
                return Err(io::Error::new(io::ErrorKind::WouldBlock, "Another application owns the alert gate"));
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    pub fn try_acquire(path: &Path) -> io::Result<Option<Self>> {
        let file = Self::open(path)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self(file))),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(e)) => Err(e),
        }
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}
pub fn load(path: &Path) -> Result<Book, String> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Book::default()),
        Err(e) => return Err(format!("Reading price alerts: {e}")),
    };
    if bytes.len() > 4_194_304 {
        return Err("Price-alert file exceeds 4 MiB".into());
    }
    let mut book: Book = serde_json::from_slice(&bytes).map_err(|e| format!("Reading price alerts: {e}"))?;
    let now = chrono::Utc::now().timestamp();
    book.sanitize_fired_through(now);
    book.validate_at(now)?;
    Ok(book)
}
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| io::Error::other("Missing parent directory"))?;
    fs::create_dir_all(parent)?;
    let now = std::time::SystemTime::now();
    for entry in fs::read_dir(parent)?.flatten() {
        let sibling = entry.path();
        if sibling.extension().is_some_and(|ext| ext == "tmp")
            && entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .is_some_and(|age| age > std::time::Duration::from_secs(3600))
        {
            let _ = fs::remove_file(sibling);
        }
    }
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = now.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
    let counter = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temp = path.with_extension(format!("{}.{nanos}.{counter}.tmp", std::process::id()));
    let result = (|| {
        let mut file = OpenOptions::new().write(true).create_new(true).open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        #[cfg(unix)]
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}
/// Caller holds the book lock for the complete read/modify/write transaction.
pub fn save_locked(path: &Path, book: &Book) -> Result<(), String> {
    book.validate()?;
    let bytes = serde_json::to_vec_pretty(book).map_err(|e| e.to_string())?;
    write_atomic(path, &bytes).map_err(|e| e.to_string())
}
pub fn save(path: &Path, book: &Book) -> Result<(), String> {
    let _lock = Lock::acquire(&path.with_extension("lock")).map_err(|e| e.to_string())?;
    save_locked(path, book)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn app_lock_excludes_daemon_until_released() {
        let dir = std::env::temp_dir().join(format!("tr-alert-lock-{}", std::process::id()));
        let path = dir.join("app.lock");
        let app = Lock::acquire(&path).unwrap();
        assert!(Lock::try_acquire(&path).unwrap().is_none());
        drop(app);
        assert!(Lock::try_acquire(&path).unwrap().is_some());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn write_atomic_overwrites_a_stale_temp_left_by_a_crash() {
        let dir = std::env::temp_dir().join(format!("tr-alert-stale-tmp-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("price-alerts.json");
        // The previous writer died between creating and renaming its temp file.
        let stale = path.with_extension(format!("{}.tmp", std::process::id()));
        fs::write(&stale, b"partial write from a crashed process").unwrap();
        File::options()
            .write(true)
            .open(&stale)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3601))
            .unwrap();
        let recent = path.with_extension("recent.tmp");
        fs::write(&recent, b"another active writer").unwrap();
        let payload = b"{\"version\":3,\"alerts\":[],\"events\":[],\"next_id\":1}";
        write_atomic(&path, payload).expect("save must recover from a leftover temp file");
        assert_eq!(fs::read(&path).unwrap(), payload.as_slice());
        assert!(!stale.exists());
        assert!(recent.exists(), "recent writers' temp files must be left alone");
        write_atomic(&path, payload).unwrap();
        assert_eq!(fs::read(&path).unwrap(), payload.as_slice());
        fs::remove_dir_all(dir).unwrap();
    }
}
