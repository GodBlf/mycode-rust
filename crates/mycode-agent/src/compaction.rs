use mycode_core::{
    conversation::{ContentBlock, Conversation, ConversationMessage, MessageRole},
    session::{CompactBoundary, SessionError, SessionId, SessionStore},
    time::current_timestamp,
};
use mycode_llm::{LlmClient, ProviderError, ProviderEvent, ProviderRequest, ToolDefinition, Usage};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::tool_result::is_tool_result;

const SUMMARY_OUTPUT_RESERVE_TOKENS: usize = 20_000;
const AUTO_COMPACT_SAFETY_MARGIN_TOKENS: usize = 13_000;
const MANUAL_COMPACT_SAFETY_MARGIN_TOKENS: usize = 3_000;
const MAX_CONSECUTIVE_AUTO_COMPACT_FAILURES: usize = 3;
const RECENT_FILE_READ_LIMIT: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionConfig {
    pub context_window_tokens: usize,
    pub max_output_tokens: usize,
    pub summary_output_reserve_tokens: usize,
    pub auto_safety_margin_tokens: usize,
    pub manual_safety_margin_tokens: usize,
    pub keep_recent_tokens: usize,
    pub min_keep_messages: usize,
    pub max_keep_tokens: usize,
    pub recovery_token_budget: usize,
    pub max_consecutive_auto_failures: usize,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            context_window_tokens: 200_000,
            max_output_tokens: 8_192,
            summary_output_reserve_tokens: SUMMARY_OUTPUT_RESERVE_TOKENS,
            auto_safety_margin_tokens: AUTO_COMPACT_SAFETY_MARGIN_TOKENS,
            manual_safety_margin_tokens: MANUAL_COMPACT_SAFETY_MARGIN_TOKENS,
            keep_recent_tokens: 10_000,
            min_keep_messages: 5,
            max_keep_tokens: 40_000,
            recovery_token_budget: 4_000,
            max_consecutive_auto_failures: MAX_CONSECUTIVE_AUTO_COMPACT_FAILURES,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompactionOutcome {
    pub summary: String,
    pub estimated_tokens_before: usize,
    pub estimated_tokens_after: usize,
    pub compacted_conversation: Conversation,
}

#[derive(Debug, Error)]
pub enum CompactionError {
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error("Session was not found")]
    SessionMissing,
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error("compaction summary was empty")]
    EmptySummary,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageAnchor {
    pub baseline_tokens: usize,
    pub anchor_count: usize,
    pub has_usage: bool,
}

impl UsageAnchor {
    pub fn from_usage(usage: Usage, conversation_len: usize) -> Self {
        Self {
            baseline_tokens: (usage.input_tokens
                + usage.output_tokens
                + usage.cache_read_tokens
                + usage.cache_creation_tokens) as usize,
            anchor_count: conversation_len,
            has_usage: true,
        }
    }
}

#[derive(Debug, Default)]
pub struct CompactionTracker {
    usage_anchor: UsageAnchor,
    consecutive_auto_failures: usize,
}

impl CompactionTracker {
    pub fn record_usage(&mut self, usage: Usage, conversation_len: usize) {
        self.usage_anchor = UsageAnchor::from_usage(usage, conversation_len);
    }

    pub fn reset_after_compaction(&mut self) {
        self.usage_anchor = UsageAnchor::default();
    }

    pub fn record_auto_failure(&mut self, config: &CompactionConfig) -> bool {
        self.consecutive_auto_failures += 1;
        self.consecutive_auto_failures < config.max_consecutive_auto_failures
    }

    pub fn record_auto_success(&mut self) {
        self.consecutive_auto_failures = 0;
    }

    pub fn should_compact(
        &self,
        messages: &[ConversationMessage],
        config: &CompactionConfig,
    ) -> CompactionTrigger {
        let used_tokens = self.used_tokens(messages);
        let output_reserve = if config.max_output_tokens == 0 {
            config.summary_output_reserve_tokens
        } else {
            config
                .max_output_tokens
                .min(config.summary_output_reserve_tokens)
        };
        let effective_window = config.context_window_tokens.saturating_sub(output_reserve);
        let manual_threshold = effective_window.saturating_sub(config.manual_safety_margin_tokens);
        if used_tokens >= manual_threshold {
            return CompactionTrigger::Hard;
        }
        let auto_threshold = effective_window.saturating_sub(config.auto_safety_margin_tokens);
        if used_tokens >= auto_threshold
            && self.consecutive_auto_failures < config.max_consecutive_auto_failures
        {
            return CompactionTrigger::Soft;
        }
        CompactionTrigger::None
    }

    pub fn used_tokens(&self, messages: &[ConversationMessage]) -> usize {
        if !self.usage_anchor.has_usage {
            return estimate_tokens(messages);
        }
        if self.usage_anchor.anchor_count > messages.len() {
            return estimate_tokens(messages);
        }
        self.usage_anchor.baseline_tokens
            + estimate_tokens(&messages[self.usage_anchor.anchor_count..])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionTrigger {
    None,
    Soft,
    Hard,
}

#[derive(Debug, Default)]
pub struct RecoveryState {
    file_reads: Vec<FileReadRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileReadRecord {
    path: String,
    output: String,
}

pub(crate) struct CompactionTarget<'a> {
    pub(crate) session_store: &'a SessionStore,
    pub(crate) session_id: &'a SessionId,
}

pub(crate) struct CompactionAttachments<'a> {
    pub(crate) cancellation: CancellationToken,
    pub(crate) recovery: Option<&'a RecoveryState>,
    pub(crate) tools: &'a [ToolDefinition],
}

impl RecoveryState {
    pub fn record_file_read(&mut self, path: &str, output: &str) {
        self.file_reads.retain(|record| record.path != path);
        self.file_reads.push(FileReadRecord {
            path: path.to_string(),
            output: output.to_string(),
        });
        if self.file_reads.len() > RECENT_FILE_READ_LIMIT {
            let overflow = self.file_reads.len() - RECENT_FILE_READ_LIMIT;
            self.file_reads.drain(0..overflow);
        }
    }

    fn attachment(&self, tools: &[ToolDefinition], token_budget: usize) -> String {
        if self.file_reads.is_empty() && tools.is_empty() {
            return String::new();
        }
        let mut attachment = String::from("Recovery snapshot:\n");
        let file_budget = token_budget / 2;
        let tool_budget = token_budget.saturating_sub(file_budget);
        if !self.file_reads.is_empty() {
            let mut files = String::from("Recently read files:\n");
            for record in self.file_reads.iter().rev() {
                files.push_str(&format!("--- {} ---\n{}\n", record.path, record.output));
            }
            truncate_to_tokens(&mut files, file_budget);
            attachment.push_str(&files);
        }
        if !tools.is_empty() {
            let mut tool_listing = String::from("Available tools:\n");
            for tool in tools {
                tool_listing.push_str(&format!("- {}: {}\n", tool.name, tool.description));
            }
            truncate_to_tokens(&mut tool_listing, tool_budget);
            attachment.push_str(&tool_listing);
        }
        attachment
    }
}

pub(crate) async fn compact_conversation(
    provider: &dyn LlmClient,
    target: CompactionTarget<'_>,
    conversation: &Conversation,
    config: &CompactionConfig,
    attachments: CompactionAttachments<'_>,
) -> Result<CompactionOutcome, CompactionError> {
    let messages = conversation.messages();
    let estimated_tokens_before = estimate_tokens(messages);
    let keep_start = choose_keep_start(messages, config);
    if keep_start == 0 {
        return Ok(CompactionOutcome {
            summary: String::new(),
            estimated_tokens_before,
            estimated_tokens_after: estimated_tokens_before,
            compacted_conversation: conversation.clone(),
        });
    }

    let summary =
        request_summary(provider, &messages[..keep_start], attachments.cancellation).await?;
    if summary.trim().is_empty() {
        return Err(CompactionError::EmptySummary);
    }
    let keep = messages[keep_start..].to_vec();
    target.session_store.append_compact_boundary(
        target.session_id,
        &CompactBoundary {
            summary: summary.clone(),
            keep: keep.clone(),
        },
    )?;

    let mut continuation = String::from(
        "This session continues from an earlier conversation that was compacted. Earlier summary:\n\n",
    );
    continuation.push_str(&summary);
    if !keep.is_empty() {
        continuation.push_str("\n\nRecent messages are preserved below.");
    }
    if let Some(recovery) = attachments.recovery {
        let attachment = recovery.attachment(attachments.tools, config.recovery_token_budget);
        if !attachment.is_empty() {
            continuation.push_str("\n\n---\n\n");
            continuation.push_str(&attachment);
        }
    }

    let mut compacted = Conversation::new();
    compacted.push(ConversationMessage {
        role: MessageRole::User,
        content: vec![ContentBlock::Text { text: continuation }],
        timestamp_unix_seconds: current_timestamp(),
    });
    for message in keep {
        compacted.push(message);
    }
    let estimated_tokens_after = estimate_tokens(compacted.messages());

    Ok(CompactionOutcome {
        summary,
        estimated_tokens_before,
        estimated_tokens_after,
        compacted_conversation: compacted,
    })
}

async fn request_summary(
    provider: &dyn LlmClient,
    messages: &[ConversationMessage],
    cancellation: CancellationToken,
) -> Result<String, CompactionError> {
    let mut transcript = String::new();
    for message in messages {
        transcript.push_str(&format!(
            "[{}]: {}\n",
            role_name(message.role),
            message_text(message)
        ));
    }
    let mut summary_conversation = Conversation::new();
    summary_conversation.push(ConversationMessage {
        role: MessageRole::User,
        content: vec![ContentBlock::Text {
            text: format!(
                "Summarize the conversation so it can continue safely. Capture requests, decisions, and unresolved work.\n\n{transcript}"
            ),
        }],
        timestamp_unix_seconds: current_timestamp(),
    });
    let request = ProviderRequest {
        system_prompt: "You create detailed, actionable conversation summaries.".into(),
        conversation: summary_conversation,
        tools: Vec::new(),
    };
    let mut stream = provider.stream(request, cancellation).await?;
    let mut summary = String::new();
    while let Some(result) = stream.recv().await {
        match result? {
            ProviderEvent::TextDelta { text } => summary.push_str(&text),
            ProviderEvent::StreamEnd { .. } => break,
            _ => {}
        }
    }
    Ok(format_summary(&summary))
}

fn format_summary(raw: &str) -> String {
    if let Some(start) = raw.find("<summary>") {
        let body = &raw[start + "<summary>".len()..];
        let body = body.split("</summary>").next().unwrap_or(body);
        return body.trim().to_string();
    }
    if let (Some(start), Some(end)) = (raw.find("<analysis>"), raw.find("</analysis>")) {
        let mut summary = String::new();
        summary.push_str(&raw[..start]);
        summary.push_str(&raw[end + "</analysis>".len()..]);
        return summary.trim().to_string();
    }
    raw.trim().to_string()
}

fn choose_keep_start(messages: &[ConversationMessage], config: &CompactionConfig) -> usize {
    let mut start = messages.len();
    let mut tokens = 0;
    for (count, index) in (0..messages.len()).rev().enumerate() {
        let message_tokens = estimate_message_tokens(&messages[index]);
        if count >= config.min_keep_messages && tokens >= config.keep_recent_tokens {
            break;
        }
        if tokens + message_tokens > config.max_keep_tokens {
            break;
        }
        tokens += message_tokens;
        start = index;
    }

    while start > 0
        && messages[start].content.iter().any(is_tool_result)
        && messages[start - 1].content.iter().any(is_tool_use)
    {
        start -= 1;
    }
    start
}

pub fn estimate_tokens(messages: &[ConversationMessage]) -> usize {
    messages.iter().map(estimate_message_tokens).sum()
}

fn estimate_message_tokens(message: &ConversationMessage) -> usize {
    let mut tokens = 4;
    for block in &message.content {
        match block {
            ContentBlock::Text { text } => tokens += approximate_tokens(text),
            ContentBlock::Thinking { thinking, .. } => tokens += approximate_tokens(thinking),
            ContentBlock::ToolUse { arguments, .. } => {
                tokens += 50 + approximate_tokens(&arguments.to_string());
            }
            ContentBlock::ToolResult { content, .. } => {
                tokens += 10 + approximate_tokens(content);
            }
        }
    }
    tokens
}

fn approximate_tokens(text: &str) -> usize {
    (text.len() as f64 / 3.5).ceil() as usize
}

fn truncate_to_tokens(text: &mut String, token_budget: usize) {
    let max_chars = (token_budget as f64 * 3.5).floor() as usize;
    if text.chars().count() > max_chars {
        let truncated = text.chars().take(max_chars).collect::<String>();
        *text = truncated;
    }
}

fn message_text(message: &ConversationMessage) -> String {
    let mut text = String::new();
    for block in &message.content {
        match block {
            ContentBlock::Text { text: value } => text.push_str(value),
            ContentBlock::Thinking { thinking, .. } => text.push_str(thinking),
            ContentBlock::ToolUse {
                tool_name,
                tool_use_id,
                ..
            } => text.push_str(&format!("[tool use {tool_name} {tool_use_id}]")),
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                ..
            } => text.push_str(&format!("[tool result {tool_use_id}]: {content}")),
        }
    }
    text
}

fn role_name(role: MessageRole) -> &'static str {
    match role {
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::System => "system",
    }
}

fn is_tool_use(block: &ContentBlock) -> bool {
    matches!(block, ContentBlock::ToolUse { .. })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_state_keeps_only_the_most_recent_file_reads() {
        let mut recovery = RecoveryState::default();
        for index in 0..12 {
            recovery.record_file_read(&format!("file-{index}"), "content");
        }
        recovery.record_file_read("file-2", "updated");

        let attachment = recovery.attachment(&[], 1_000);
        assert!(attachment.contains("--- file-11 ---"));
        assert!(attachment.contains("--- file-2 ---\nupdated"));
        assert!(!attachment.contains("--- file-0 ---"));
        assert!(!attachment.contains("--- file-1 ---"));
    }
}
