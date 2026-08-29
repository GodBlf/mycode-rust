#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

#[derive(Debug, Default)]
pub struct SseParser {
    buffer: Vec<u8>,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_bytes(&mut self, bytes: &[u8]) -> Result<Vec<SseEvent>, crate::events::LlmError> {
        self.buffer.extend_from_slice(bytes);
        self.take_complete_events()
    }

    pub fn finish(&mut self) -> Result<Vec<SseEvent>, crate::events::LlmError> {
        let events = self.take_complete_events()?;
        if self.buffer.is_empty() {
            return Ok(events);
        }

        let remainder = String::from_utf8(std::mem::take(&mut self.buffer)).map_err(|error| {
            crate::events::LlmError::InvalidResponse {
                message: format!("provider sent invalid UTF-8 SSE data: {error}"),
            }
        })?;
        let mut result = events;
        if let Some(event) = parse_sse_event(&remainder) {
            result.push(event);
        }
        Ok(result)
    }

    fn take_complete_events(&mut self) -> Result<Vec<SseEvent>, crate::events::LlmError> {
        let mut events = Vec::new();
        while let Some((start, length)) = find_event_separator(&self.buffer) {
            let event_bytes = self.buffer.drain(..start).collect::<Vec<_>>();
            self.buffer.drain(..length);
            let text = String::from_utf8(event_bytes).map_err(|error| {
                crate::events::LlmError::InvalidResponse {
                    message: format!("provider sent invalid UTF-8 SSE data: {error}"),
                }
            })?;
            if let Some(event) = parse_sse_event(&text) {
                events.push(event);
            }
        }
        Ok(events)
    }
}

fn find_event_separator(buffer: &[u8]) -> Option<(usize, usize)> {
    let candidates = [
        buffer
            .windows(b"\n\n".len())
            .position(|window| window == b"\n\n")
            .map(|position| (position, b"\n\n".len())),
        buffer
            .windows(b"\r\n\r\n".len())
            .position(|window| window == b"\r\n\r\n")
            .map(|position| (position, b"\r\n\r\n".len())),
        buffer
            .windows(b"\r\r".len())
            .position(|window| window == b"\r\r")
            .map(|position| (position, b"\r\r".len())),
    ];
    candidates
        .into_iter()
        .flatten()
        .min_by_key(|(start, _)| *start)
}

fn parse_sse_event(text: &str) -> Option<SseEvent> {
    let mut event = None;
    let mut data_lines = Vec::new();

    for line in text.lines() {
        if line.starts_with(':') {
            continue;
        }
        let (name, value) = match line.split_once(':') {
            Some((name, value)) => (name, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match name {
            "event" => event = Some(value.to_string()),
            "data" => data_lines.push(value.to_string()),
            _ => {}
        }
    }

    if event.is_none() && data_lines.is_empty() {
        return None;
    }

    Some(SseEvent {
        event,
        data: data_lines.join("\n"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_events_split_across_chunks() {
        let mut parser = SseParser::new();
        assert_eq!(
            parser
                .push_bytes(b"event: message_start\ndata: {\"value\":1}\n\nevent")
                .expect("valid UTF-8"),
            vec![SseEvent {
                event: Some("message_start".into()),
                data: "{\"value\":1}".into(),
            }]
        );
        assert_eq!(
            parser
                .push_bytes(b": delta\ndata: {\"value\":2}\r\n\r\n")
                .expect("valid UTF-8"),
            vec![SseEvent {
                event: Some("delta".into()),
                data: "{\"value\":2}".into(),
            }]
        );
    }

    #[test]
    fn finish_parses_a_final_event_without_a_separator() {
        let mut parser = SseParser::new();
        parser.push_bytes(b"data: final\n").expect("valid UTF-8");
        assert_eq!(
            parser.finish().expect("valid UTF-8"),
            vec![SseEvent {
                event: None,
                data: "final".into(),
            }]
        );
    }
}
