use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::conversation::{ContentBlock, ConversationMessage, MessageRole, first_user_text};
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompactBoundary {
    pub summary: String,
    pub keep: Vec<ConversationMessage>,
}

#[derive(Debug, Clone, PartialEq)]
enum SessionRecord {
    Message(ConversationMessage),
    CompactBoundary(CompactBoundary),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CompactBoundaryRecord {
    #[serde(rename = "type")]
    record_type: String,
    #[serde(flatten)]
    boundary: CompactBoundary,
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

    pub fn append_compact_boundary(
        &self,
        session_id: &SessionId,
        boundary: &CompactBoundary,
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
        let record = CompactBoundaryRecord {
            record_type: "compact_boundary".into(),
            boundary: boundary.clone(),
        };
        let record = serde_json::to_string(&record).map_err(|source| SessionError::Serialize {
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
        let records = self.read_records(session_id, file)?;
        Ok(Some(Self::resume_messages(records)))
    }

    pub fn search(&self, query: &str) -> Result<Vec<SessionSummary>, SessionError> {
        let query = query.to_lowercase();
        Ok(self
            .list()?
            .into_iter()
            .filter(|summary| {
                query.is_empty()
                    || summary
                        .first_user_message
                        .as_deref()
                        .is_some_and(|message| message.to_lowercase().contains(&query))
                    || summary.id.as_str().to_lowercase().contains(&query)
            })
            .collect())
    }

    fn read_records(
        &self,
        session_id: &SessionId,
        file: File,
    ) -> Result<Vec<SessionRecord>, SessionError> {
        let reader = BufReader::new(file);
        let mut records = Vec::new();

        for (index, line) in reader.lines().enumerate() {
            let line = line.map_err(|source| SessionError::Read {
                session_id: session_id.as_str().to_string(),
                source,
            })?;
            let value = serde_json::from_str::<serde_json::Value>(&line).map_err(|source| {
                SessionError::InvalidRecord {
                    session_id: session_id.as_str().to_string(),
                    line_number: index + 1,
                    source,
                }
            })?;
            if value.get("type").and_then(serde_json::Value::as_str) == Some("compact_boundary") {
                let record = serde_json::from_value::<CompactBoundaryRecord>(value);
                if let Ok(record) = record {
                    records.push(SessionRecord::CompactBoundary(record.boundary));
                }
                continue;
            }
            let message =
                serde_json::from_value::<ConversationMessage>(value).map_err(|source| {
                    SessionError::InvalidRecord {
                        session_id: session_id.as_str().to_string(),
                        line_number: index + 1,
                        source,
                    }
                })?;
            records.push(SessionRecord::Message(message));
        }

        Ok(records)
    }

    fn resume_messages(records: Vec<SessionRecord>) -> Vec<ConversationMessage> {
        let last_boundary = records
            .iter()
            .rposition(|record| matches!(record, SessionRecord::CompactBoundary(_)));
        let Some(index) = last_boundary else {
            return records
                .into_iter()
                .filter_map(SessionRecord::into_message)
                .collect();
        };

        let SessionRecord::CompactBoundary(boundary) = &records[index] else {
            unreachable!("rposition returned a compact boundary");
        };
        let summary = ConversationMessage {
            role: MessageRole::User,
            content: vec![ContentBlock::Text {
                text: boundary.summary.clone(),
            }],
            timestamp_unix_seconds: current_timestamp(),
        };
        let mut messages = vec![summary];
        messages.extend(boundary.keep.iter().cloned());
        messages.extend(
            records
                .into_iter()
                .skip(index + 1)
                .filter_map(SessionRecord::into_message),
        );
        messages
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
            let messages = self.load_message_records(&id)?;
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

    fn load_message_records(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<ConversationMessage>, SessionError> {
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
            return Ok(Vec::new());
        };
        Ok(self
            .read_records(session_id, file)?
            .into_iter()
            .filter_map(SessionRecord::into_message)
            .collect())
    }
}

fn current_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

impl SessionRecord {
    fn into_message(self) -> Option<ConversationMessage> {
        match self {
            Self::Message(message) => Some(message),
            Self::CompactBoundary(_) => None,
        }
    }
}
