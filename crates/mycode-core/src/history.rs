use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use crate::workspace::WorkspacePaths;

const MAX_HISTORY_ENTRIES: usize = 200;

#[derive(Debug, Error)]
pub enum HistoryError {
    #[error("failed to create the prompt history directory")]
    CreateDirectory(#[source] std::io::Error),
    #[error("failed to read prompt history")]
    Read(#[source] std::io::Error),
    #[error("failed to write prompt history")]
    Write(#[source] std::io::Error),
    #[error("failed to serialize prompt history")]
    Serialize(#[source] serde_json::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HistoryRecord {
    text: String,
    timestamp_unix_seconds: u64,
}

#[derive(Debug, Clone)]
pub struct PromptHistory {
    path: PathBuf,
    max_entries: usize,
}

impl PromptHistory {
    pub fn new(work_dir: impl AsRef<Path>) -> Self {
        Self::with_limit(work_dir, MAX_HISTORY_ENTRIES)
    }

    pub fn with_limit(work_dir: impl AsRef<Path>, max_entries: usize) -> Self {
        Self {
            path: history_path(work_dir.as_ref()),
            max_entries: max_entries.max(1),
        }
    }

    pub fn load(&self) -> Result<Vec<String>, HistoryError> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Vec::new());
            }
            Err(source) => return Err(HistoryError::Read(source)),
        };

        let mut entries = Vec::new();
        for line in BufReader::new(file).lines() {
            let line = line.map_err(HistoryError::Read)?;
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(record) = serde_json::from_str::<HistoryRecord>(&line)
                && !record.text.is_empty()
                && !entries.contains(&record.text)
            {
                entries.push(record.text);
            }
        }
        Ok(entries)
    }

    pub fn append(&self, text: &str) -> Result<(), HistoryError> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(());
        }

        let mut entries = self.load()?;
        if entries.last().is_some_and(|last| last == text) {
            return Ok(());
        }
        entries.push(text.to_string());
        if entries.len() > self.max_entries {
            let overflow = entries.len() - self.max_entries;
            entries.drain(0..overflow);
        }

        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(HistoryError::CreateDirectory)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&self.path)
            .map_err(HistoryError::Write)?;
        let mut writer = std::io::BufWriter::new(file);
        for entry in entries {
            let record = HistoryRecord {
                text: entry,
                timestamp_unix_seconds: crate::time::current_timestamp(),
            };
            serde_json::to_writer(&mut writer, &record).map_err(HistoryError::Serialize)?;
            writer.write_all(b"\n").map_err(HistoryError::Write)?;
        }
        writer.flush().map_err(HistoryError::Write)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn history_path(work_dir: &Path) -> PathBuf {
    WorkspacePaths::new(work_dir).prompt_history_file()
}
