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
    let temp = path.with_extension(format!("{}.tmp", std::process::id()));
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
}
