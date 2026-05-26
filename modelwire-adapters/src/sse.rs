//! SSE (Server-Sent Events) utilities.

use bytes::{Bytes, BytesMut};
use modelwire_core::CanonicalEvent;
use std::str;

/// SSE event types used by OpenAI Responses API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SseEventType {
    ResponseCreated,
    ResponseInProgress,
    ResponseOutputItemAdded,
    ResponseTextDelta,
    ResponseTextDone,
    ResponseFunctionCallArgumentsDelta,
    ResponseOutputItemDone,
    ResponseReasoningSummaryTextDelta,
    ResponseReasoningSummaryTextDone,
    ResponseCompleted,
    ResponseFailed,
    Error,
    Unknown,
}

impl SseEventType {
    /// Parse from event type string.
    pub fn parse(s: &str) -> Self {
        match s {
            "response.created" => SseEventType::ResponseCreated,
            "response.in_progress" => SseEventType::ResponseInProgress,
            "response.output_item.added" => SseEventType::ResponseOutputItemAdded,
            "response.output_text.delta" | "response.text.delta" => SseEventType::ResponseTextDelta,
            "response.output_text.done" | "response.text.done" => SseEventType::ResponseTextDone,
            "response.function_call_arguments.delta" => {
                SseEventType::ResponseFunctionCallArgumentsDelta
            }
            "response.output_item.done" => SseEventType::ResponseOutputItemDone,
            "response.reasoning_summary_text.delta" => {
                SseEventType::ResponseReasoningSummaryTextDelta
            }
            "response.reasoning_summary_text.done" => {
                SseEventType::ResponseReasoningSummaryTextDone
            }
            "response.completed" => SseEventType::ResponseCompleted,
            "response.failed" => SseEventType::ResponseFailed,
            "error" => SseEventType::Error,
            _ => SseEventType::Unknown,
        }
    }

    /// Convert to canonical event type string.
    pub fn as_str(&self) -> &'static str {
        match self {
            SseEventType::ResponseCreated => "response.created",
            SseEventType::ResponseInProgress => "response.in_progress",
            SseEventType::ResponseOutputItemAdded => "response.output_item.added",
            SseEventType::ResponseTextDelta => "response.output_text.delta",
            SseEventType::ResponseTextDone => "response.output_text.done",
            SseEventType::ResponseFunctionCallArgumentsDelta => {
                "response.function_call_arguments.delta"
            }
            SseEventType::ResponseOutputItemDone => "response.output_item.done",
            SseEventType::ResponseReasoningSummaryTextDelta => {
                "response.reasoning_summary_text.delta"
            }
            SseEventType::ResponseReasoningSummaryTextDone => {
                "response.reasoning_summary_text.done"
            }
            SseEventType::ResponseCompleted => "response.completed",
            SseEventType::ResponseFailed => "response.failed",
            SseEventType::Error => "error",
            SseEventType::Unknown => "unknown",
        }
    }
}

/// SSE message parsed from stream.
#[derive(Debug, Clone)]
pub struct SseMessage {
    pub event_type: SseEventType,
    pub data: Bytes,
}

/// Raw SSE frame extracted from streaming bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawSseFrame {
    pub event: Option<String>,
    pub data: Vec<u8>,
}

/// Parse SSE data line by line.
pub fn parse_sse_line(line: &str) -> Option<(&str, &str)> {
    if line.is_empty() {
        return None;
    }

    if let Some(pos) = line.find(':') {
        let key = &line[..pos];
        let value = line[pos + 1..].trim_start();
        Some((key, value))
    } else {
        None
    }
}

/// Parse SSE data field (data line may be split across multiple lines).
pub fn parse_sse_data(data: &str) -> Vec<Bytes> {
    let mut result = Vec::new();
    let mut current = BytesMut::new();

    for line in data.lines() {
        if line.is_empty() {
            if !current.is_empty() {
                result.push(current.freeze());
                current = BytesMut::new();
            }
            continue;
        }

        if let Some((key, value)) = parse_sse_line(line) {
            if key == "data" {
                current.extend_from_slice(value.as_bytes());
            }
        }
    }

    if !current.is_empty() {
        result.push(current.freeze());
    }

    result
}

/// Incrementally parse raw SSE frames from byte chunks.
///
/// `buffer` stores trailing incomplete bytes between chunks.
pub fn extract_sse_frames(buffer: &mut BytesMut, chunk: &[u8]) -> Vec<RawSseFrame> {
    buffer.extend_from_slice(chunk);
    let mut frames = Vec::new();

    while let Some((split_at, delimiter_len)) = find_frame_delimiter(buffer) {
        let mut frame_bytes = buffer.split_to(split_at + delimiter_len).to_vec();
        while frame_bytes.ends_with(b"\n") || frame_bytes.ends_with(b"\r") {
            frame_bytes.pop();
        }
        if frame_bytes.is_empty() {
            continue;
        }

        let mut event_name: Option<String> = None;
        let mut data_lines: Vec<Vec<u8>> = Vec::new();

        for raw_line in frame_bytes.split(|b| *b == b'\n') {
            let line = if raw_line.ends_with(b"\r") {
                &raw_line[..raw_line.len().saturating_sub(1)]
            } else {
                raw_line
            };
            if line.is_empty() || line.first() == Some(&b':') {
                continue;
            }

            if let Some(rest) = line.strip_prefix(b"event:") {
                event_name = Some(String::from_utf8_lossy(trim_left_space(rest)).to_string());
                continue;
            }

            if let Some(rest) = line.strip_prefix(b"data:") {
                data_lines.push(trim_left_space(rest).to_vec());
            }
        }

        if event_name.is_none() && data_lines.is_empty() {
            continue;
        }

        let mut data = Vec::new();
        for (index, line) in data_lines.into_iter().enumerate() {
            if index > 0 {
                data.push(b'\n');
            }
            data.extend_from_slice(&line);
        }

        frames.push(RawSseFrame {
            event: event_name,
            data,
        });
    }

    frames
}

fn trim_left_space(input: &[u8]) -> &[u8] {
    let mut start = 0;
    while start < input.len() && input[start] == b' ' {
        start += 1;
    }
    &input[start..]
}

fn find_frame_delimiter(buffer: &BytesMut) -> Option<(usize, usize)> {
    let bytes = buffer.as_ref();
    let lf = bytes.windows(2).position(|window| window == b"\n\n");
    let crlf = bytes.windows(4).position(|window| window == b"\r\n\r\n");
    match (lf, crlf) {
        (Some(a), Some(b)) if a <= b => Some((a, 2)),
        (Some(_), Some(b)) => Some((b, 4)),
        (Some(a), None) => Some((a, 2)),
        (None, Some(b)) => Some((b, 4)),
        (None, None) => None,
    }
}

/// SSE writer for sending events downstream.
pub struct SseWriter {
    buffer: BytesMut,
}

impl SseWriter {
    pub fn new() -> Self {
        Self {
            buffer: BytesMut::new(),
        }
    }

    /// Write an SSE event.
    pub fn write_event(&mut self, event_type: SseEventType, data: &serde_json::Value) {
        let event_str = event_type.as_str();
        let data_str = serde_json::to_string(data).unwrap_or_default();

        self.buffer.extend_from_slice(b"event: ");
        self.buffer.extend_from_slice(event_str.as_bytes());
        self.buffer.extend_from_slice(b"\n");

        self.buffer.extend_from_slice(b"data: ");
        self.buffer.extend_from_slice(data_str.as_bytes());
        self.buffer.extend_from_slice(b"\n\n");
    }

    /// Write a comment (keep-alive).
    pub fn write_comment(&mut self, comment: &str) {
        self.buffer.extend_from_slice(b": ");
        self.buffer.extend_from_slice(comment.as_bytes());
        self.buffer.extend_from_slice(b"\n\n");
    }

    /// Flush the buffer.
    pub fn flush(&mut self) -> Bytes {
        self.buffer.split().freeze()
    }
}

impl Default for SseWriter {
    fn default() -> Self {
        Self::new()
    }
}

/// Convert canonical event to SSE.
pub fn canonical_to_sse(event: &CanonicalEvent) -> (SseEventType, serde_json::Value) {
    match event {
        CanonicalEvent::ResponseCreated {
            response_id,
            model,
            created_at,
        } => (
            SseEventType::ResponseCreated,
            serde_json::json!({
                "response": {
                    "id": response_id,
                    "model": model,
                    "created_at": created_at,
                }
            }),
        ),
        CanonicalEvent::OutputItemAdded { response_id, item } => (
            SseEventType::ResponseOutputItemAdded,
            serde_json::json!({
                "response_id": response_id,
                "item": item,
            }),
        ),
        CanonicalEvent::OutputTextDelta { item_id, delta } => (
            SseEventType::ResponseTextDelta,
            serde_json::json!({
                "item_id": item_id,
                "delta": { "text": delta },
            }),
        ),
        CanonicalEvent::FunctionCallArgumentsDelta { item_id, delta } => (
            SseEventType::ResponseFunctionCallArgumentsDelta,
            serde_json::json!({
                "item_id": item_id,
                "delta": { "arguments": delta },
            }),
        ),
        CanonicalEvent::OutputItemDone { response_id, item } => (
            SseEventType::ResponseOutputItemDone,
            serde_json::json!({
                "response_id": response_id,
                "item": item,
            }),
        ),
        CanonicalEvent::ReasoningSummaryDelta { item_id, delta } => (
            SseEventType::ResponseReasoningSummaryTextDelta,
            serde_json::json!({
                "item_id": item_id,
                "delta": { "summary": delta },
            }),
        ),
        CanonicalEvent::ResponseCompleted {
            response_id,
            output,
            usage,
        } => {
            let mut obj = serde_json::json!({
                "response": {
                    "id": response_id,
                    "output": output,
                }
            });
            if let Some(u) = usage {
                obj["response"]["usage"] = serde_json::json!({
                    "input_tokens": u.input_tokens,
                    "output_tokens": u.output_tokens,
                    "total_tokens": u.total_tokens,
                });
            }
            (SseEventType::ResponseCompleted, obj)
        }
        CanonicalEvent::ResponseFailed { response_id, error } => (
            SseEventType::ResponseFailed,
            serde_json::json!({
                "response_id": response_id,
                "error": error,
            }),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sse_event_type_parse() {
        assert_eq!(
            SseEventType::parse("response.created"),
            SseEventType::ResponseCreated
        );
        assert_eq!(
            SseEventType::parse("response.completed"),
            SseEventType::ResponseCompleted
        );
        assert_eq!(
            SseEventType::parse("response.output_item.added"),
            SseEventType::ResponseOutputItemAdded
        );
    }

    #[test]
    fn test_sse_event_type_as_str() {
        assert_eq!(SseEventType::ResponseCreated.as_str(), "response.created");
        assert_eq!(
            SseEventType::ResponseCompleted.as_str(),
            "response.completed"
        );
    }

    #[test]
    fn test_sse_writer() {
        let mut writer = SseWriter::new();
        writer.write_event(
            SseEventType::ResponseCreated,
            &serde_json::json!({"id": "test"}),
        );
        let output = writer.flush();
        let s = String::from_utf8_lossy(&output);
        assert!(s.contains("event: response.created"));
        assert!(s.contains("data: {\"id\":\"test\"}"));
    }

    #[test]
    fn test_parse_sse_line() {
        assert_eq!(parse_sse_line("event: test"), Some(("event", "test")));
        assert_eq!(
            parse_sse_line("data: {\"id\":\"test\"}"),
            Some(("data", "{\"id\":\"test\"}"))
        );
        assert_eq!(parse_sse_line(""), None);
    }

    #[test]
    fn test_extract_sse_frames_handles_utf8_split_chunks() {
        let mut buffer = BytesMut::new();
        let first = b"event: response.output_text.delta\ndata: {\"delta\":{\"text\":\"\xE4\xBD";
        let second = b"\xA0\xE5\xA5\xBD\"}}\n\n";

        let frames_first = extract_sse_frames(&mut buffer, first);
        assert!(frames_first.is_empty());

        let frames_second = extract_sse_frames(&mut buffer, second);
        assert_eq!(frames_second.len(), 1);
        assert_eq!(
            frames_second[0].event.as_deref(),
            Some("response.output_text.delta")
        );
        let json = String::from_utf8(frames_second[0].data.clone()).unwrap();
        assert!(json.contains("你好"));
    }
}
