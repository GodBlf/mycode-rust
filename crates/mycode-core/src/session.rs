use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::conversation::{ConversationMessage, first_user_text};
use crate::workspace::{WorkspacePaths, unique_slug};

#[derive(Debug, Error)]
pub enum SessionError {
    #[error(
        "invalid session ID: session IDs may contain ASCII letters, numbers, hyphens, and underscores"
    )]
    InvalidSessionId,
    #[error("failed to create the Session directory")]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to append to Session {session_id}")]
    Append {
        session_id: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to serialize a Session {session_id} message")]
    Serialize {
        session_id: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to read Session {session_id}")]
    Read {
        session_id: String,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid Session {session_id} record at line {line_number}")]
    InvalidRecord {
        session_id: String,
        line_number: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to list Sessions")]
    List {
        #[source]
        source: std::io::Error,
    },
    #[error("failed to read Session metadata for {session_id}")]
    Metadata {
        session_id: String,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(String);

impl SessionId {
    pub fn new(value: impl Into<String>) -> Result<Self, SessionError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.chars().all(|character| {
                character.is_ascii_alphanumeric() || character == '-' || character == '_'
            });
        if valid {
            Ok(Self(value))
        } else {
            Err(SessionError::InvalidSessionId)
        }
    }

    pub fn generate() -> Self {
        Self(unique_slug())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: SessionId,
    pub first_user_message: Option<String>,
    pub message_count: usize,
    pub file_size_bytes: u64,
    pub modified_at: SystemTime,
}

#[derive(Debug, Clone)]
pub struct SessionStore {
    work_dir: PathBuf,
}

impl SessionStore {
    pub fn new(work_dir: impl Into<PathBuf>) -> Self {
        Self {
            work_dir: work_dir.into(),
        }
    }

    pub fn append(
        &self,
        session_id: &SessionId,
        message: &ConversationMessage,
    ) -> Result<(), SessionError> {
        let path = self.session_path(session_id);
        fs::create_dir_all(path.parent().expect("session path has a parent")).map_err(
            |source| SessionError::CreateDirectory {
                path: path
                    .parent()
                    .expect("session path has a parent")
                    .to_path_buf(),
                source,
            },
        )?;
        let record = serde_json::to_string(message).map_err(|source| SessionError::Serialize {
            session_id: session_id.as_str().to_string(),
            source,
        })?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|source| SessionError::Append {
                session_id: session_id.as_str().to_string(),
                source,
            })?;
        file.write_all(record.as_bytes())
            .and_then(|()| file.write_all(b"\n"))
            .map_err(|source| SessionError::Append {
                session_id: session_id.as_str().to_string(),
                source,
            })
    }

    pub fn load(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<Vec<ConversationMessage>>, SessionError> {
        let path = self.session_path(session_id);
        let file = File::open(&path).map_or_else(
            |source| {
                if source.kind() == std::io::ErrorKind::NotFound {
                    Ok(None)
                } else {
                    Err(SessionError::Read {
                        session_id: session_id.as_str().to_string(),
                        source,
                    })
                }
            },
            |file| Ok(Some(file)),
        )?;
        let Some(file) = file else {
            return Ok(None);
        };
        let reader = BufReader::new(file);
        let mut messages = Vec::new();

        for (index, line) in reader.lines().enumerate() {
            let line = line.map_err(|source| SessionError::Read {
                session_id: session_id.as_str().to_string(),
                source,
            })?;
            let message =
                serde_json::from_str(&line).map_err(|source| SessionError::InvalidRecord {
                    session_id: session_id.as_str().to_string(),
                    line_number: index + 1,
                    source,
                })?;
            messages.push(message);
        }

        Ok(Some(messages))
    }

    pub fn list(&self) -> Result<Vec<SessionSummary>, SessionError> {
        let directory = self.sessions_dir();
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => return Err(SessionError::List { source }),
        };

        let mut summaries = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| SessionError::List { source })?;
            let path = entry.path();
            if !path.is_file()
                || path
                    .extension()
                    .is_none_or(|extension| extension != "jsonl")
            {
                continue;
            }
            let file_stem = path
                .file_stem()
                .and_then(|value| value.to_str())
                .ok_or(SessionError::InvalidSessionId)?;
            let id = SessionId::new(file_stem)?;
            let messages = self.load(&id)?.unwrap_or_default();
            let metadata = entry.metadata().map_err(|source| SessionError::Metadata {
                session_id: id.as_str().to_string(),
                source,
            })?;
            let first_user_message = first_user_text(&messages).map(str::to_string);

            let modified_at = metadata
                .modified()
                .map_err(|source| SessionError::Metadata {
                    session_id: id.as_str().to_string(),
                    source,
                })?;

            summaries.push(SessionSummary {
                id,
                first_user_message,
                message_count: messages.len(),
                file_size_bytes: metadata.len(),
                modified_at,
            });
        }

        summaries.sort_by(|left, right| {
            right
                .modified_at
                .cmp(&left.modified_at)
                .then_with(|| right.id.as_str().cmp(left.id.as_str()))
        });
        Ok(summaries)
    }

    fn sessions_dir(&self) -> PathBuf {
        WorkspacePaths::new(&self.work_dir).sessions_dir()
    }

    fn session_path(&self, session_id: &SessionId) -> PathBuf {
        Path::new(&self.sessions_dir()).join(format!("{}.jsonl", session_id.as_str()))
    }
}
