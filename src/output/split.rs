//! Walks a transcript and yields Markdown vs A2UI 1.0 envelopes.
//!
//! A2UI may arrive as JSONL, a JSON array, a single object, or a fenced
//! `a2ui`/`json` block. Everything else stays Markdown so GFM tables keep
//! their pipe syntax.

use serde_json::Value;

pub(super) enum Segment {
    Markdown(String),
    A2ui(Vec<Value>),
}

pub(super) fn segments(input: &str) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut markdown = String::new();
    let mut offset = 0;

    while offset < input.len() {
        if let Some((end, body)) = fence_at(input, offset) {
            if let Some(messages) = a2ui_payload(body) {
                push_markdown(&mut out, &mut markdown);
                out.push(Segment::A2ui(messages));
            } else {
                markdown.push_str(&input[offset..end]);
            }
            offset = end;
            continue;
        }

        if let Some((mut end, mut messages)) = a2ui_value_at(input, offset) {
            while let Some((next_end, more)) = a2ui_value_at(input, end) {
                messages.extend(more);
                end = next_end;
            }
            push_markdown(&mut out, &mut markdown);
            out.push(Segment::A2ui(messages));
            offset = end;
            continue;
        }

        let ch = input[offset..].chars().next().unwrap();
        markdown.push(ch);
        offset += ch.len_utf8();
    }

    push_markdown(&mut out, &mut markdown);
    out
}

fn push_markdown(out: &mut Vec<Segment>, markdown: &mut String) {
    if markdown.is_empty() {
        return;
    }
    out.push(Segment::Markdown(std::mem::take(markdown)));
}

fn at_line_start(input: &str, offset: usize) -> bool {
    offset == 0 || input.as_bytes().get(offset - 1) == Some(&b'\n')
}

fn fence_at(input: &str, offset: usize) -> Option<(usize, &str)> {
    if !at_line_start(input, offset) {
        return None;
    }
    let line_start = offset;
    let rest = &input[offset..];
    let indent = rest.bytes().take_while(|b| *b == b' ').count();
    if indent > 3 {
        return None;
    }
    let after_indent = &rest[indent..];
    let marker = if after_indent.starts_with("```") {
        '`'
    } else if after_indent.starts_with("~~~") {
        '~'
    } else {
        return None;
    };
    let mark_len = after_indent
        .bytes()
        .take_while(|b| *b == marker as u8)
        .count();
    if mark_len < 3 {
        return None;
    }
    let after_mark = &after_indent[mark_len..];
    let first_line_end = after_mark.find('\n')?;
    let info = after_mark[..first_line_end].trim();
    if info.contains(marker) {
        return None;
    }
    let content_start = offset + indent + mark_len + first_line_end + 1;
    let mut search = content_start;
    while search < input.len() {
        let slice = &input[search..];
        let nl = slice.find('\n').unwrap_or(slice.len());
        let line = &slice[..nl];
        let line_indent = line.bytes().take_while(|b| *b == b' ').count();
        if line_indent <= 3 {
            let trimmed = line[line_indent..].trim_end();
            let close_len = trimmed.bytes().take_while(|b| *b == marker as u8).count();
            if close_len >= mark_len && trimmed.bytes().all(|b| b == marker as u8) {
                let end = search + nl + usize::from(nl < slice.len());
                return Some((end, &input[content_start..search]));
            }
        }
        search += nl + usize::from(nl < slice.len());
    }
    let _ = line_start;
    None
}

fn a2ui_value_at(input: &str, offset: usize) -> Option<(usize, Vec<Value>)> {
    let rest = &input[offset..];
    let trimmed = rest.trim_start();
    let skip = rest.len() - trimmed.len();
    if trimmed.is_empty() {
        return None;
    }
    let first = trimmed.as_bytes()[0];
    if first != b'{' && first != b'[' {
        return None;
    }
    if !looks_like_a2ui_prefix(trimmed) && first != b'[' {
        return None;
    }
    let (value, consumed) = parse_json_value(trimmed)?;
    let messages = envelopes_from_value(&value)?;
    let end = offset + skip + consumed;
    Some((end, messages))
}

fn looks_like_a2ui_prefix(s: &str) -> bool {
    s.contains("\"createSurface\"")
        || s.contains("\"updateComponents\"")
        || s.contains("\"updateDataModel\"")
        || s.contains("\"deleteSurface\"")
        || s.contains("\"callRendererFunction\"")
        || s.contains("\"agentFunctionResponse\"")
}

pub(super) fn a2ui_payload(body: &str) -> Option<Vec<Value>> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return envelopes_from_value(&value);
    }
    let mut messages = Vec::new();
    let mut rest = trimmed;
    while !rest.is_empty() {
        let (value, consumed) = parse_json_value(rest)?;
        let batch = envelopes_from_value(&value)?;
        messages.extend(batch);
        rest = rest[consumed..].trim_start();
    }
    if messages.is_empty() {
        None
    } else {
        Some(messages)
    }
}

fn envelopes_from_value(value: &Value) -> Option<Vec<Value>> {
    match value {
        Value::Array(items) => {
            let messages: Vec<Value> = items
                .iter()
                .filter(|item| is_envelope(item))
                .cloned()
                .collect();
            if messages.is_empty() {
                None
            } else {
                Some(messages)
            }
        }
        value if is_envelope(value) => Some(vec![value.clone()]),
        _ => None,
    }
}

fn is_envelope(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    object.contains_key("createSurface")
        || object.contains_key("updateComponents")
        || object.contains_key("updateDataModel")
        || object.contains_key("deleteSurface")
        || object.contains_key("callRendererFunction")
        || object.contains_key("agentFunctionResponse")
}

fn parse_json_value(input: &str) -> Option<(Value, usize)> {
    let start = input.len() - input.trim_start().len();
    let slice = &input[start..];
    let end = json_span(slice)?;
    let raw = &slice[..end];
    let value = serde_json::from_str(raw).ok()?;
    Some((value, start + end))
}

fn json_span(input: &str) -> Option<usize> {
    let bytes = input.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    match bytes[0] {
        b'{' | b'[' => {
            let open = bytes[0];
            let close = if open == b'{' { b'}' } else { b']' };
            let mut depth = 0usize;
            let mut in_string = false;
            let mut escape = false;
            for (index, &byte) in bytes.iter().enumerate() {
                if in_string {
                    if escape {
                        escape = false;
                    } else if byte == b'\\' {
                        escape = true;
                    } else if byte == b'"' {
                        in_string = false;
                    }
                    continue;
                }
                match byte {
                    b'"' => in_string = true,
                    b if b == open => depth += 1,
                    b if b == close => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(index + 1);
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonl_envelopes() {
        let source = r#"{"version":"v1.0","createSurface":{"surfaceId":"a"}}
{"version":"v1.0","deleteSurface":{"surfaceId":"a"}}"#;
        let parts = segments(source);
        assert_eq!(parts.len(), 1);
        match &parts[0] {
            Segment::A2ui(messages) => assert_eq!(messages.len(), 2),
            Segment::Markdown(_) => panic!("expected a2ui"),
        }
    }

    #[test]
    fn markdown_table_stays_markdown() {
        let source = "| A | B |\n| --- | --- |\n| 1 | 2 |\n";
        let parts = segments(source);
        assert!(matches!(&parts[..], [Segment::Markdown(_)]));
    }
}
