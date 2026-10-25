//! File loading and explicit save policies without memory mapping or FFI.
//!
//! Auto saves existing files in place to retain links, ownership, ACLs and extended
//! attributes. Like the C in-place policy, a failed write can leave partial data.
//! Atomic replacement is opt-in for existing files: Unix ownership and mode are
//! preserved, but ACLs and extended attributes are not copied by std. Conflict
//! detection is best effort, not a lock against concurrent writers.

use crate::text::{Text, TextSnapshot};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SaveMethod {
    #[default]
    Auto,
    Atomic,
    InPlace,
}

/// A text buffer and the on-disk contents last observed by this instance.
pub struct Document {
    pub text: Text,
    path: PathBuf,
    baseline: Option<TextSnapshot>,
}

impl Document {
    /// Open a regular file, or prepare a new buffer if the path does not exist.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        // Reject ordinary FIFO/device paths before open, which could block on a
        // FIFO indefinitely. Validate the opened handle as well for replacements.
        match fs::metadata(&path) {
            Ok(metadata) if !metadata.is_file() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "not a regular file",
                ));
            }
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
        let contents = match File::open(&path) {
            Ok(mut file) => {
                if !file.metadata()?.is_file() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "not a regular file",
                    ));
                }
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes)?;
                Some(bytes)
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                // A dangling symlink must not be mistaken for a new file.
                if fs::symlink_metadata(&path).is_ok() {
                    return Err(err);
                }
                None
            }
            Err(err) => return Err(err),
        };
        let text = Text::from_bytes(contents.as_deref().unwrap_or_default());
        let baseline = contents.as_ref().map(|_| text.read_snapshot());
        Ok(Self {
            text,
            path,
            baseline,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Save and mark the revision saved only after all writes succeed.
    /// `force` permits overwriting contents changed since opening.
    pub fn save(&mut self, method: SaveMethod, force: bool) -> io::Result<()> {
        let saved = self.text.read_snapshot();
        self.save_snapshot(&saved, method, force)
    }

    /// Write exactly the supplied immutable snapshot without replacing this
    /// document's in-memory text or undo history. The snapshot can concatenate
    /// selected ranges or originate from a different text.
    ///
    /// As in C TextSave commit, success marks the current in-memory revision
    /// saved even for a projected write. Conflict checks subsequently compare
    /// against the actual written snapshot, rather than the full document.
    /// Failure does not update the saved revision or the observed disk baseline.
    pub fn save_snapshot(
        &mut self,
        saved: &TextSnapshot,
        method: SaveMethod,
        force: bool,
    ) -> io::Result<()> {
        let metadata = match fs::metadata(&self.path) {
            Ok(meta) => {
                if !meta.is_file() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "not a regular file",
                    ));
                }
                Some(meta)
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => None,
            Err(err) => return Err(err),
        };
        if !force {
            let unchanged = match (&metadata, &self.baseline) {
                (Some(_), Some(baseline)) => matches_snapshot(File::open(&self.path)?, baseline)?,
                (None, None) => true,
                _ => false,
            };
            if !unchanged {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "file changed on disk",
                ));
            }
        }
        let atomic =
            method == SaveMethod::Atomic || (method == SaveMethod::Auto && metadata.is_none());
        if atomic {
            if fs::symlink_metadata(&self.path).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "atomic save would replace a symlink",
                ));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if metadata.as_ref().is_some_and(|m| m.nlink() > 1) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "atomic save would break hard links",
                    ));
                }
            }
            atomic_write(&self.path, saved, metadata.as_ref())?;
        } else {
            let mut options = OpenOptions::new();
            options.write(true).read(!force);
            if metadata.is_none() {
                options.create_new(true);
            }
            let mut file = options.open(&self.path)?;
            if !file.metadata()?.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "not a regular file",
                ));
            }
            // Check the opened handle too, before altering any bytes.
            if !force && metadata.is_some() {
                let unchanged = match &self.baseline {
                    Some(baseline) => matches_snapshot(&mut file, baseline)?,
                    None => false,
                };
                if !unchanged {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "file changed on disk",
                    ));
                }
            }
            file.seek(SeekFrom::Start(0))?;
            write_snapshot(&mut file, saved)?;
            file.set_len(saved.len() as u64)?;
            file.sync_all()?;
        }
        self.baseline = Some(saved.clone());
        self.text.mark_saved();
        Ok(())
    }
}

struct Temporary(PathBuf);

impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn matches_snapshot(mut reader: impl Read, snapshot: &TextSnapshot) -> io::Result<bool> {
    let mut buffer = [0; 8192];
    for piece in snapshot.chunks() {
        for expected in piece.chunks(buffer.len()) {
            match reader.read_exact(&mut buffer[..expected.len()]) {
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(false),
                result => result?,
            }
            if &buffer[..expected.len()] != expected {
                return Ok(false);
            }
        }
    }
    loop {
        match reader.read(&mut buffer[..1]) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Ok(0) => return Ok(true),
            Ok(_) => return Ok(false),
            Err(error) => return Err(error),
        }
    }
}

fn write_snapshot(mut writer: impl Write, snapshot: &TextSnapshot) -> io::Result<()> {
    for chunk in snapshot.chunks() {
        writer.write_all(chunk)?;
    }
    Ok(())
}

fn atomic_write(
    path: &Path,
    snapshot: &TextSnapshot,
    metadata: Option<&fs::Metadata>,
) -> io::Result<()> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    // Acquire resources before publishing. Opening an unreadable parent after
    // rename/link would report a failed save even though the file was installed.
    #[cfg(unix)]
    let directory = File::open(parent)?;
    let mut candidate = None;
    for _ in 0..128 {
        let name = format!(
            ".vis-{}-{}.tmp",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let temporary = parent.join(name);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(if metadata.is_some() { 0o600 } else { 0o666 });
        }
        match options.open(&temporary) {
            Ok(file) => {
                candidate = Some((Temporary(temporary), file));
                break;
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    let (temporary, mut file) = candidate.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "cannot allocate save temporary",
        )
    })?;
    write_snapshot(&mut file, snapshot)?;
    if let Some(metadata) = metadata {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            std::os::unix::fs::chown(&temporary.0, Some(metadata.uid()), Some(metadata.gid()))?;
        }
        file.set_permissions(metadata.permissions())?;
    }
    file.sync_all()?;
    if metadata.is_some() {
        fs::rename(&temporary.0, path)?;
    } else {
        // Atomic creation without overwriting a file created by another writer.
        fs::hard_link(&temporary.0, path)?;
        fs::remove_file(&temporary.0)?;
    }
    #[cfg(unix)]
    directory.sync_all()?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)] // Projected writes deliberately use containers of byte ranges.
mod tests {
    use super::*;
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "vis-file-test-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn binary_save_and_external_change() {
        let dir = Directory::new();
        let path = dir.0.join("file");
        let mut doc = Document::open(&path).unwrap();
        doc.text.insert(0, b"\0\xffhello").unwrap();
        doc.save(SaveMethod::Auto, false).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"\0\xffhello");
        assert!(!doc.text.modified());
        doc.text.delete(2..7).unwrap();
        fs::write(&path, b"external").unwrap();
        assert_eq!(
            doc.save(SaveMethod::Auto, false).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(&path).unwrap(), b"external");
        assert!(doc.text.modified());
        doc.save(SaveMethod::Auto, true).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"\0\xff");
    }

    #[test]
    fn atomic_save_and_directory_rejection() {
        let dir = Directory::new();
        assert!(Document::open(&dir.0).is_err());
        let path = dir.0.join("file");
        fs::write(&path, b"before").unwrap();
        let mut doc = Document::open(&path).unwrap();
        doc.text.replace(0..6, b"after").unwrap();
        doc.save(SaveMethod::Atomic, false).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"after");
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
    }

    #[test]
    fn streamed_fragmented_saves_keep_an_independent_disk_baseline() {
        let dir = Directory::new();
        let path = dir.0.join("file");
        let original = vec![b'x'; 32_769];
        fs::write(&path, &original).unwrap();
        let mut doc = Document::open(&path).unwrap();
        for position in [0, 4000, 9000, 17_000, 32_000] {
            doc.text.insert(position, b"\0\xff").unwrap();
        }
        let expected = doc.text.to_vec();
        doc.save(SaveMethod::InPlace, false).unwrap();
        assert_eq!(fs::read(&path).unwrap(), expected);
        doc.text.insert(8192, b"insert").unwrap();
        let expected = doc.text.to_vec();
        doc.save(SaveMethod::Atomic, false).unwrap();
        assert_eq!(fs::read(&path).unwrap(), expected);
        for changed in [
            &expected[..expected.len() - 1],
            &[expected.as_slice(), b"extra"].concat(),
        ] {
            fs::write(&path, changed).unwrap();
            assert_eq!(
                doc.save(SaveMethod::Auto, false).unwrap_err().kind(),
                io::ErrorKind::AlreadyExists
            );
            assert_eq!(fs::read(&path).unwrap(), changed);
        }
    }

    #[test]
    fn projected_binary_ranges_preserve_full_text_history_and_written_baseline() {
        for method in [SaveMethod::Auto, SaveMethod::Atomic] {
            let dir = Directory::new();
            let path = dir.0.join("file");
            fs::write(&path, b"0123456789").unwrap();
            let mut doc = Document::open(&path).unwrap();
            doc.text.insert(3, b"\0\xff").unwrap();
            let full = doc.text.to_vec();
            let mark = doc.text.mark(6).unwrap();
            let projected = doc.text.read_ranges(&[3..5, 8..11, 0..2]).unwrap();
            doc.save_snapshot(&projected, method, false).unwrap();
            assert_eq!(fs::read(&path).unwrap(), b"\0\xff67801");
            assert_eq!(doc.text.to_vec(), full);
            assert_eq!(doc.text.mark_get(mark), Some(6));
            assert!(!doc.text.modified());
            doc.text.undo().unwrap();
            assert_eq!(doc.text.to_vec(), b"0123456789");
            assert!(doc.text.modified());
            doc.text.redo().unwrap();
            assert_eq!(doc.text.to_vec(), full);
            assert!(!doc.text.modified());
            // The disk baseline must be the projection just written. Using the
            // full text here would incorrectly reject this subsequent save.
            doc.save(method, false).unwrap();
            assert_eq!(fs::read(&path).unwrap(), full);
        }
    }

    #[test]
    fn failed_projected_save_preserves_revision_and_disk_baseline() {
        let dir = Directory::new();
        let path = dir.0.join("file");
        fs::write(&path, b"before").unwrap();
        let mut doc = Document::open(&path).unwrap();
        doc.text.insert(0, b"after ").unwrap();
        let full = doc.text.to_vec();
        let state = doc.text.state();
        let projected = doc.text.read_ranges(&[0..5]).unwrap();
        fs::write(&path, b"external").unwrap();
        assert_eq!(
            doc.save_snapshot(&projected, SaveMethod::Auto, false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(&path).unwrap(), b"external");
        assert_eq!(doc.text.to_vec(), full);
        assert_eq!(doc.text.state(), state);
        assert!(doc.text.modified());
        // Failure must retain the previous observed contents as the baseline.
        fs::write(&path, b"before").unwrap();
        doc.save_snapshot(&projected, SaveMethod::Auto, false)
            .unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"after");
        doc.text.undo().unwrap();
        assert_eq!(doc.text.to_vec(), b"before");
    }

    #[test]
    fn empty_projection_truncates_disk_without_emptying_buffer() {
        let dir = Directory::new();
        let path = dir.0.join("file");
        fs::write(&path, b"before").unwrap();
        let mut doc = Document::open(&path).unwrap();
        let projected = doc.text.read_ranges(&[2..2]).unwrap();
        doc.save_snapshot(&projected, SaveMethod::Auto, false)
            .unwrap();
        assert!(fs::read(&path).unwrap().is_empty());
        assert_eq!(doc.text.to_vec(), b"before");
        doc.save(SaveMethod::Auto, false).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"before");
    }

    #[cfg(unix)]
    #[test]
    fn projected_save_preserves_link_targets_and_mode() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = Directory::new();
        let path = dir.0.join("file");
        let link = dir.0.join("link");
        fs::write(&path, b"before").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        symlink(&path, &link).unwrap();
        let mut doc = Document::open(&link).unwrap();
        let projected = doc.text.read_ranges(&[2..5]).unwrap();
        assert!(
            doc.save_snapshot(&projected, SaveMethod::Atomic, false)
                .is_err()
        );
        doc.save_snapshot(&projected, SaveMethod::Auto, false)
            .unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"for");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[cfg(unix)]
    #[test]
    fn forced_save_only_requires_write_permission() {
        use std::os::unix::fs::PermissionsExt;
        let dir = Directory::new();
        let path = dir.0.join("file");
        fs::write(&path, b"before").unwrap();
        let mut doc = Document::open(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o200)).unwrap();
        doc.text.replace(0..6, b"after").unwrap();
        doc.save(SaveMethod::InPlace, true).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"after");
    }

    #[cfg(unix)]
    #[test]
    fn atomic_save_acquires_directory_access_before_publishing() {
        use std::os::unix::fs::PermissionsExt;
        let dir = Directory::new();
        let path = dir.0.join("file");
        let mut doc = Document::open(&path).unwrap();
        doc.text.insert(0, b"new contents").unwrap();
        fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o300)).unwrap();
        let can_open_directory = File::open(&dir.0).is_ok();
        let result = doc.save(SaveMethod::Auto, false);
        fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o700)).unwrap();
        if can_open_directory {
            // Elevated test users may bypass permission bits.
            result.unwrap();
            assert_eq!(fs::read(&path).unwrap(), b"new contents");
        } else {
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
            assert!(!path.exists(), "failed save published a new file");
            assert!(doc.text.modified());
            doc.save(SaveMethod::Auto, false).unwrap();
            assert_eq!(fs::read(&path).unwrap(), b"new contents");
        }
    }

    #[cfg(unix)]
    #[test]
    fn auto_preserves_links_and_permissions() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = Directory::new();
        let path = dir.0.join("file");
        let hard = dir.0.join("hard");
        let sym = dir.0.join("sym");
        fs::write(&path, b"before").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        fs::hard_link(&path, &hard).unwrap();
        symlink(&path, &sym).unwrap();
        for link in [&hard, &sym] {
            let mut doc = Document::open(link).unwrap();
            doc.text.replace(0..doc.text.len(), b"after").unwrap();
            assert!(doc.save(SaveMethod::Atomic, false).is_err());
            doc.save(SaveMethod::Auto, false).unwrap();
            assert_eq!(fs::read(&path).unwrap(), b"after");
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o640
            );
        }
        assert!(fs::symlink_metadata(sym).unwrap().file_type().is_symlink());
    }
}
