use std::{fs, io, path::Path};

pub(crate) enum FileReadError {
    NotFound,
    Io(io::Error),
    InvalidUtf8,
}

pub(crate) fn read_utf8(path: &Path) -> Result<String, FileReadError> {
    let contents = fs::read(path).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            FileReadError::NotFound
        } else {
            FileReadError::Io(source)
        }
    })?;
    String::from_utf8(contents).map_err(|_| FileReadError::InvalidUtf8)
}
