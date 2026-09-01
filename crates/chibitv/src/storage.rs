//! Where the server puts what it produces, such as recordings.
//!
//! Only local files are stored today, but an object is written as a stream and
//! is complete only once it has been finished, so that a remote store — an S3
//! bucket, say — fits the same interface.

use std::fs::{File, create_dir_all, rename};
use std::io::{BufWriter, Write};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, bail};
use tracing::info;

use crate::config::StorageConfig;

/// The suffix an object being written carries until it is finished, so that an
/// interrupted one is not taken for a complete recording.
const PARTIAL_SUFFIX: &str = ".part";

pub trait Storage: Send + Sync {
    /// Opens an object to write into, replacing any object of the same name.
    fn create(&self, name: &str) -> anyhow::Result<Box<dyn StorageObject>>;
}

/// An object being written to a [`Storage`].
///
/// The data is written through [`Write`], and the object counts as complete
/// only once [`StorageObject::finish`] has returned. Dropping one without
/// finishing it keeps what was written, marked as unfinished, because a
/// recording cut short is still worth more than nothing.
pub trait StorageObject: Write + Send {
    fn finish(&mut self) -> anyhow::Result<()>;
}

pub fn open(config: &StorageConfig) -> anyhow::Result<Box<dyn Storage>> {
    Ok(match config {
        StorageConfig::Directory { path } => Box::new(DirectoryStorage::new(path)),
    })
}

/// Keeps objects as files in one directory.
pub struct DirectoryStorage {
    path: PathBuf,
}

impl DirectoryStorage {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl Storage for DirectoryStorage {
    fn create(&self, name: &str) -> anyhow::Result<Box<dyn StorageObject>> {
        // A name is what an object is called, not where it goes, so anything
        // that would take the file out of the directory is refused rather than
        // quietly written elsewhere.
        let mut components = Path::new(name).components();
        if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
            bail!("`{name}` is not a valid object name");
        }

        create_dir_all(&self.path)
            .with_context(|| format!("Could not create `{}`", self.path.display()))?;

        let path = self.path.join(name);
        let partial_path = self.path.join(format!("{name}{PARTIAL_SUFFIX}"));
        let file = File::create(&partial_path)
            .with_context(|| format!("Could not create `{}`", partial_path.display()))?;

        info!(path = %path.display(), "Writing to a file");

        Ok(Box::new(FileObject {
            writer: Some(BufWriter::new(file)),
            partial_path,
            path,
        }))
    }
}

struct FileObject {
    /// Taken once the object is finished, so that finishing it twice, or
    /// writing to it afterwards, does not touch the file again.
    writer: Option<BufWriter<File>>,
    partial_path: PathBuf,
    path: PathBuf,
}

impl Write for FileObject {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match &mut self.writer {
            Some(writer) => writer.write(buf),
            None => Err(std::io::Error::other("the object is already finished")),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match &mut self.writer {
            Some(writer) => writer.flush(),
            None => Ok(()),
        }
    }
}

impl StorageObject for FileObject {
    fn finish(&mut self) -> anyhow::Result<()> {
        let Some(mut writer) = self.writer.take() else {
            return Ok(());
        };

        writer.flush()?;
        drop(writer);

        rename(&self.partial_path, &self.path).with_context(|| {
            format!(
                "Could not rename `{}` to `{}`",
                self.partial_path.display(),
                self.path.display()
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use std::fs::read_to_string;

    use super::*;

    #[test]
    fn keeps_a_finished_object_under_its_name() {
        let directory = tempfile::tempdir().unwrap();
        let storage = DirectoryStorage::new(directory.path());

        let mut object = storage.create("recording.m2ts").unwrap();
        object.write_all(b"stream").unwrap();
        object.finish().unwrap();

        let path = directory.path().join("recording.m2ts");
        assert_eq!(read_to_string(path).unwrap(), "stream");
    }

    #[test]
    fn leaves_an_unfinished_object_marked_as_partial() {
        let directory = tempfile::tempdir().unwrap();
        let storage = DirectoryStorage::new(directory.path());

        let mut object = storage.create("recording.m2ts").unwrap();
        object.write_all(b"stream").unwrap();
        drop(object);

        assert!(!directory.path().join("recording.m2ts").exists());
        let partial = directory.path().join("recording.m2ts.part");
        assert_eq!(read_to_string(partial).unwrap(), "stream");
    }

    #[test]
    fn creates_the_directory_it_stores_into() {
        let directory = tempfile::tempdir().unwrap();
        let storage = DirectoryStorage::new(directory.path().join("recordings"));

        storage.create("recording.m2ts").unwrap().finish().unwrap();

        assert!(directory.path().join("recordings/recording.m2ts").is_file());
    }

    #[test]
    fn refuses_a_name_that_is_a_path() {
        let directory = tempfile::tempdir().unwrap();
        let storage = DirectoryStorage::new(directory.path());

        for name in ["", "..", "sub/recording.m2ts", "/etc/passwd"] {
            assert!(storage.create(name).is_err(), "`{name}` was accepted");
        }
    }
}
