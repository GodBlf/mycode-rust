use std::{
    fs, io,
    path::{Path, PathBuf},
};

const SKIP_DIRECTORIES: &[&str] = &[
    ".git",
    ".mycode",
    ".mypy_cache",
    ".tox",
    ".venv",
    "__pycache__",
    "node_modules",
];

pub(crate) fn walk_files(base: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut stack = vec![base.to_path_buf()];

    while let Some(directory) = stack.pop() {
        let mut entries = fs::read_dir(&directory)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|entry| entry.path());

        for entry in entries {
            let path = entry.path();
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                if should_skip_directory(&path) {
                    continue;
                }
                stack.push(path);
            } else if file_type.is_file() {
                files.push(path);
            }
        }
    }

    files.sort();
    Ok(files)
}

fn should_skip_directory(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| SKIP_DIRECTORIES.contains(&name))
}
