use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

use mycode_core::{
    conversation::{ContentBlock, Conversation, ConversationMessage},
    session::SessionId,
    workspace::WorkspacePaths,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolResultBudgetConfig {
    pub single_result_limit_chars: usize,
    pub message_aggregate_limit_chars: usize,
    pub old_result_snip_chars: usize,
    pub keep_recent_turns: usize,
}

impl Default for ToolResultBudgetConfig {
    fn default() -> Self {
        Self {
            single_result_limit_chars: 50_000,
            message_aggregate_limit_chars: 200_000,
            old_result_snip_chars: 2_000,
            keep_recent_turns: 10,
        }
    }
}

#[derive(Debug, Error)]
pub enum ToolResultBudgetError {
    #[error("failed to read Tool Result replacement records")]
    ReadRecords(#[source] std::io::Error),
    #[error("invalid Tool Result replacement record at line {line_number}")]
    InvalidRecord {
        line_number: usize,
        #[source]
        source: serde_json::Error,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ReplacementRecordKind {
    ToolResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ReplacementRecord {
    kind: ReplacementRecordKind,
    tool_use_id: String,
    replacement: String,
}

#[derive(Debug)]
pub struct ToolResultBudget {
    workspace_root: PathBuf,
    spill_dir: PathBuf,
    records_path: PathBuf,
    config: ToolResultBudgetConfig,
    seen_ids: HashSet<String>,
    replacements: HashMap<String, String>,
    persisted_replacements: HashSet<String>,
}

impl ToolResultBudget {
    pub fn resume(
        work_dir: impl AsRef<Path>,
        session_id: &SessionId,
        config: ToolResultBudgetConfig,
    ) -> Result<Self, ToolResultBudgetError> {
        let workspace_root = work_dir.as_ref().to_path_buf();
        let paths = WorkspacePaths::new(&workspace_root);
        let records_path = paths
            .sessions_dir()
            .join(session_id.as_str())
            .join("replacement_records.jsonl");
        let spill_dir = workspace_root.join(".mycode/tool_results");
        let mut replacements = HashMap::new();
        let mut persisted_replacements = HashSet::new();

        if let Some(file) = open_if_exists(&records_path)? {
            let reader = BufReader::new(file);
            for (index, line) in reader.lines().enumerate() {
                let line = line.map_err(ToolResultBudgetError::ReadRecords)?;
                if line.trim().is_empty() {
                    continue;
                }
                let record =
                    serde_json::from_str::<ReplacementRecord>(&line).map_err(|source| {
                        ToolResultBudgetError::InvalidRecord {
                            line_number: index + 1,
                            source,
                        }
                    })?;
                if record.kind == ReplacementRecordKind::ToolResult {
                    persisted_replacements.insert(record.tool_use_id.clone());
                    replacements.insert(record.tool_use_id, record.replacement);
                }
            }
        }

        Ok(Self {
            workspace_root,
            spill_dir,
            records_path,
            config,
            seen_ids: HashSet::new(),
            replacements,
            persisted_replacements,
        })
    }

    pub fn apply(&mut self, conversation: &Conversation) -> Conversation {
        let messages = conversation.clone().into_messages();
        let tool_uses = tool_uses_by_id(&messages);
        let mut new_messages = Vec::with_capacity(messages.len());

        for message in messages {
            if !message.content.iter().any(is_tool_result) {
                new_messages.push(message);
                continue;
            }
            new_messages.push(self.apply_message(message, &tool_uses));
        }

        self.snip_stale_results(&mut new_messages);
        let records = self.new_replacement_records();
        let _ = self.append_records(records);

        let mut result = Conversation::new();
        for message in new_messages {
            result.push(message);
        }
        result
    }

    pub fn reconstruct(&mut self, conversation: &Conversation) {
        for message in conversation.messages() {
            for block in &message.content {
                if let ContentBlock::ToolResult { tool_use_id, .. } = block {
                    self.seen_ids.insert(tool_use_id.clone());
                }
            }
        }
    }

    fn apply_message(
        &mut self,
        message: ConversationMessage,
        tool_uses: &HashMap<String, (String, serde_json::Value)>,
    ) -> ConversationMessage {
        let original_content = message.content.clone();
        let mut decisions: HashMap<String, String> = HashMap::new();
        let mut fresh: Vec<(usize, String)> = Vec::new();

        for (index, block) in original_content.iter().enumerate() {
            let ContentBlock::ToolResult {
                tool_use_id,
                content,
                ..
            } = block
            else {
                continue;
            };
            if let Some(replacement) = self.replacements.get(tool_use_id) {
                decisions.insert(tool_use_id.clone(), replacement.clone());
            } else if self.seen_ids.contains(tool_use_id) {
                decisions.insert(tool_use_id.clone(), content.clone());
            } else if is_replacement_preview(content) {
                self.seen_ids.insert(tool_use_id.clone());
                self.replacements
                    .insert(tool_use_id.clone(), content.clone());
                decisions.insert(tool_use_id.clone(), content.clone());
            } else {
                fresh.push((index, content.clone()));
            }
        }

        for (index, content) in &fresh {
            let tool_use_id = tool_result_id(&original_content[*index]);
            if content.len() <= self.config.single_result_limit_chars {
                continue;
            }
            if self.is_spill_readback(tool_use_id, tool_uses) {
                self.seen_ids.insert(tool_use_id.to_string());
                decisions.insert(tool_use_id.to_string(), content.clone());
            } else if let Some(preview) = self.write_spill(tool_use_id, content) {
                decisions.insert(tool_use_id.to_string(), preview);
            } else {
                self.seen_ids.insert(tool_use_id.to_string());
                decisions.insert(tool_use_id.to_string(), content.clone());
            }
        }

        let mut total = decisions.values().map(String::len).sum::<usize>();
        total += fresh
            .iter()
            .filter(|(index, _)| !decisions.contains_key(tool_result_id(&original_content[*index])))
            .map(|(_, content)| content.len())
            .sum::<usize>();
        let mut remaining = fresh
            .iter()
            .filter(|(index, _)| !decisions.contains_key(tool_result_id(&original_content[*index])))
            .map(|(index, content)| (*index, content.clone()))
            .collect::<Vec<_>>();
        remaining.sort_by_key(|result| std::cmp::Reverse(result.1.len()));

        for (index, content) in remaining {
            if total <= self.config.message_aggregate_limit_chars {
                break;
            }
            let tool_use_id = tool_result_id(&original_content[index]);
            if self.is_spill_readback(tool_use_id, tool_uses) {
                self.seen_ids.insert(tool_use_id.to_string());
                decisions.insert(tool_use_id.to_string(), content.clone());
                continue;
            }
            if let Some(preview) = self.write_spill(tool_use_id, &content) {
                total = total.saturating_sub(content.len().saturating_sub(preview.len()));
                decisions.insert(tool_use_id.to_string(), preview);
            } else {
                self.seen_ids.insert(tool_use_id.to_string());
                decisions.insert(tool_use_id.to_string(), content.clone());
            }
        }

        for (index, content) in fresh {
            let tool_use_id = tool_result_id(&original_content[index]);
            self.seen_ids.insert(tool_use_id.to_string());
            decisions
                .entry(tool_use_id.to_string())
                .or_insert_with(|| content.clone());
        }

        ConversationMessage {
            content: original_content
                .into_iter()
                .map(|block| match &block {
                    ContentBlock::ToolResult { tool_use_id, .. } => ContentBlock::ToolResult {
                        tool_use_id: tool_use_id.clone(),
                        content: decisions
                            .get(tool_use_id)
                            .cloned()
                            .expect("every Tool Result receives a decision"),
                        is_error: match &block {
                            ContentBlock::ToolResult { is_error, .. } => *is_error,
                            _ => false,
                        },
                    },
                    other => other.clone(),
                })
                .collect(),
            ..message
        }
    }

    fn snip_stale_results(&mut self, messages: &mut Vec<ConversationMessage>) {
        let total_turns = messages
            .iter()
            .filter(|message| {
                message.role == mycode_core::conversation::MessageRole::Assistant
                    && message
                        .content
                        .iter()
                        .all(|block| !matches!(block, ContentBlock::ToolUse { .. }))
            })
            .count();
        if total_turns <= self.config.keep_recent_turns {
            return;
        }
        let old_boundary = total_turns - self.config.keep_recent_turns;
        let mut turns_seen = 0;

        for message in messages {
            if message.role == mycode_core::conversation::MessageRole::Assistant
                && message
                    .content
                    .iter()
                    .all(|block| !matches!(block, ContentBlock::ToolUse { .. }))
            {
                turns_seen += 1;
            }
            if turns_seen > old_boundary || !message.content.iter().any(is_tool_result) {
                continue;
            }
            message.content = message
                .content
                .clone()
                .into_iter()
                .map(|block| match block {
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => {
                        let replacement = if content.len() > self.config.old_result_snip_chars
                            && !self.replacements.contains_key(&tool_use_id)
                        {
                            format!("[Stale output snipped: {} chars]", content.len())
                        } else {
                            content.clone()
                        };
                        if replacement != content {
                            self.seen_ids.insert(tool_use_id.clone());
                            self.replacements
                                .insert(tool_use_id.clone(), replacement.clone());
                        }
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content: replacement,
                            is_error,
                        }
                    }
                    other => other,
                })
                .collect();
        }
    }

    fn is_spill_readback(
        &self,
        tool_use_id: &str,
        tool_uses: &HashMap<String, (String, serde_json::Value)>,
    ) -> bool {
        let Some((tool_name, arguments)) = tool_uses.get(tool_use_id) else {
            return false;
        };
        if tool_name != "ReadFile" {
            return false;
        }
        let Some(path) = arguments
            .get("file_path")
            .and_then(serde_json::Value::as_str)
        else {
            return false;
        };
        let path = Path::new(path);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.workspace_root.join(path)
        };
        path.starts_with(&self.spill_dir)
    }

    fn write_spill(&mut self, tool_use_id: &str, content: &str) -> Option<String> {
        if tool_use_id.is_empty() {
            return None;
        }
        if fs::create_dir_all(&self.spill_dir).is_err() {
            return None;
        }
        let path = self.spill_dir.join(tool_use_id);
        if path.exists() {
            if !path.is_file() {
                return None;
            }
        } else if fs::write(&path, content).is_err() {
            return None;
        }
        let preview = format!(
            "[Result of {} chars saved to {} — read with ReadFile if needed]",
            content.len(),
            path.display()
        );
        self.seen_ids.insert(tool_use_id.to_string());
        self.replacements
            .insert(tool_use_id.to_string(), preview.clone());
        Some(preview)
    }

    fn new_replacement_records(&self) -> Vec<ReplacementRecord> {
        self.replacements
            .iter()
            .filter(|(tool_use_id, _)| !self.persisted_replacements.contains(*tool_use_id))
            .map(|(tool_use_id, replacement)| ReplacementRecord {
                kind: ReplacementRecordKind::ToolResult,
                tool_use_id: tool_use_id.clone(),
                replacement: replacement.clone(),
            })
            .collect()
    }

    fn append_records(&mut self, records: Vec<ReplacementRecord>) -> std::io::Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        fs::create_dir_all(
            self.records_path
                .parent()
                .expect("records path has a parent"),
        )?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.records_path)?;
        for record in records {
            serde_json::to_writer(&mut file, &record).map_err(std::io::Error::other)?;
            file.write_all(b"\n")?;
        }
        for tool_use_id in self.replacements.keys() {
            self.persisted_replacements.insert(tool_use_id.clone());
        }
        Ok(())
    }
}

fn open_if_exists(path: &Path) -> Result<Option<File>, ToolResultBudgetError> {
    match File::open(path) {
        Ok(file) => Ok(Some(file)),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ToolResultBudgetError::ReadRecords(source)),
    }
}

pub(crate) fn is_tool_result(block: &ContentBlock) -> bool {
    matches!(block, ContentBlock::ToolResult { .. })
}

fn tool_result_id(block: &ContentBlock) -> &str {
    match block {
        ContentBlock::ToolResult { tool_use_id, .. } => tool_use_id,
        _ => "",
    }
}

fn tool_uses_by_id(
    messages: &[ConversationMessage],
) -> HashMap<String, (String, serde_json::Value)> {
    let mut tool_uses = HashMap::new();
    for message in messages {
        for block in &message.content {
            if let ContentBlock::ToolUse {
                tool_use_id,
                tool_name,
                arguments,
            } = block
            {
                tool_uses.insert(tool_use_id.clone(), (tool_name.clone(), arguments.clone()));
            }
        }
    }
    tool_uses
}

fn is_replacement_preview(content: &str) -> bool {
    content.starts_with("[Result of ") || content.starts_with("[Stale output snipped:")
}
