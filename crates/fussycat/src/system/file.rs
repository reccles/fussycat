use std::path::PathBuf;

use fussycat_core::quilt::{Source, SourceError};

/// A block's content read from a file. Process substitution (`<(cmd)`)
/// works because the run reads a stable source exactly once.
pub struct FileSource {
    path: PathBuf,
}

impl FileSource {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl Source for FileSource {
    /// The error leaves the path out: the block is named after it, and the
    /// run's error already says which block failed.
    fn fetch(&self) -> Result<String, SourceError> {
        std::fs::read_to_string(&self.path).map_err(|error| SourceError(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_file() {
        let path =
            std::env::temp_dir().join(format!("fussycat-file-source-{}.md", std::process::id()));
        std::fs::write(&path, "be terse").unwrap();
        let content = FileSource::new(&path).fetch();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(content, Ok("be terse".into()));
    }

    #[test]
    fn a_missing_file_says_why_without_repeating_the_path() {
        let error = FileSource::new("/nonexistent/core.md").fetch().unwrap_err();
        assert!(error.0.starts_with("No such file or directory"), "{error}");
    }
}
