use serde_json::Value;

pub(super) const PROTOCOL_VERSION: u64 = 1;
pub(super) const SCHEMA_VERSION: u64 = 3;
pub(super) const SDK_VERSION: &str = "1.0.4";
pub(super) const MAX_FRAME_BYTES: usize = 64 * 1024;
pub(super) const MAX_PROMPT_BYTES: usize = 4096;

#[derive(Default)]
pub(super) struct FrameDecoder {
    pending: Vec<u8>,
}

impl FrameDecoder {
    pub(super) fn push(&mut self, chunk: &[u8]) -> Result<Vec<Value>, &'static str> {
        let mut frames = Vec::new();
        for byte in chunk {
            if *byte == b'\n' {
                if self.pending.is_empty() {
                    return Err("EMPTY_FRAME");
                }
                let frame = serde_json::from_slice::<Value>(&self.pending)
                    .map_err(|_| "INVALID_JSON_FRAME")?;
                if !frame.is_object() {
                    return Err("INVALID_FRAME_SHAPE");
                }
                frames.push(frame);
                self.pending.clear();
                continue;
            }
            if self.pending.len() >= MAX_FRAME_BYTES {
                return Err("FRAME_TOO_LARGE");
            }
            self.pending.push(*byte);
        }
        Ok(frames)
    }

    pub(super) fn finish(&self) -> Result<(), &'static str> {
        if self.pending.is_empty() {
            Ok(())
        } else {
            Err("TRUNCATED_FRAME")
        }
    }
}

pub(super) fn encode_frame(frame: &Value) -> Result<Vec<u8>, &'static str> {
    let mut bytes = serde_json::to_vec(frame).map_err(|_| "INVALID_OUTGOING_FRAME")?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err("FRAME_TOO_LARGE");
    }
    bytes.push(b'\n');
    Ok(bytes)
}

pub(super) fn valid_token(value: &str) -> bool {
    (16..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::{encode_frame, valid_token, FrameDecoder, MAX_FRAME_BYTES};
    use serde_json::json;

    #[test]
    fn frame_decoder_handles_fragmentation_and_multiple_frames() {
        let first = encode_frame(&json!({"type":"ready","protocol":1})).unwrap();
        let second = encode_frame(&json!({"type":"settled","protocol":1})).unwrap();
        let midpoint = first.len() - 1;
        let combined = [first, second].concat();
        let mut decoder = FrameDecoder::default();
        assert!(decoder.push(&combined[..midpoint]).unwrap().is_empty());
        let frames = decoder.push(&combined[midpoint..]).unwrap();
        decoder.finish().unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0]["type"], "ready");
        assert_eq!(frames[1]["type"], "settled");
    }

    #[test]
    fn frame_decoder_rejects_unbounded_and_malformed_frames() {
        let mut decoder = FrameDecoder::default();
        assert_eq!(
            decoder.push(&vec![b'x'; MAX_FRAME_BYTES + 1]),
            Err("FRAME_TOO_LARGE")
        );
        assert_eq!(FrameDecoder::default().push(b"{}\n\n"), Err("EMPTY_FRAME"));
        assert_eq!(
            FrameDecoder::default().push(b"{\xff}\n"),
            Err("INVALID_JSON_FRAME")
        );
        let mut truncated = FrameDecoder::default();
        truncated.push(b"{\"x\":").unwrap();
        assert_eq!(truncated.finish(), Err("TRUNCATED_FRAME"));
    }

    #[test]
    fn opaque_ids_are_bounded_ascii_tokens() {
        assert!(valid_token("conversation_0123456789abcdef"));
        assert!(!valid_token("short"));
        assert!(!valid_token("../../outside-path"));
        assert!(!valid_token("conversation_with_空格"));
        assert!(!valid_token(&"x".repeat(129)));
    }
}
