//! Wire framing helpers for control JSON and raw PTY payloads.

use std::collections::VecDeque;
use std::fmt;

use serde::Serialize;

use crate::message::ControlMessage;

/// Default safety cap for one PTY payload frame: 16 MiB.
pub const MAX_PTY_FRAME_LEN: usize = 16 * 1024 * 1024;
const LEN_PREFIX_BYTES: usize = 4;

/// Safety cap for one newline-delimited control line: 512 KiB.
///
/// Control frames are JSON `ControlMessage`s — small handshakes, requests, and
/// responses, never bulk data (PTY output and ssh-agent payloads ride their own
/// length-prefixed planes). 512 KiB is orders of magnitude above any legitimate
/// control line yet bounds the buffer a peer can force us to accumulate.
///
/// Without this cap, [`ControlLineDecoder::push`] would buffer bytes until it saw
/// a `\n`: a compromised or MITM'd remote daemon could stream gigabytes with no
/// newline and drive the process to OOM. The bound is enforced *incrementally*
/// (mirroring [`PtyFrameDecoder`]'s `MAX_PTY_FRAME_LEN` check) so the offending
/// peer is cut off long before the allocation grows large.
pub const MAX_CONTROL_LINE_LEN: usize = 512 * 1024;

/// Errors produced by framing encoders/decoders.
#[derive(Debug)]
pub enum FrameError {
    /// The frame length exceeds the configured maximum.
    FrameTooLarge { len: usize, max: usize },
    /// An incomplete control line (no newline yet) exceeds the configured
    /// maximum, indicating a peer streaming an unbounded newline-less line.
    ControlLineTooLarge { len: usize, max: usize },
    /// The length prefix would not fit into the on-wire u32.
    LengthOverflow { len: usize },
    /// A JSON control frame failed to serialize or deserialize.
    Json(serde_json::Error),
    /// Control frames must be newline-delimited JSON; raw newlines indicate a
    /// caller tried to encode multiple frames at once.
    ControlPayloadContainsNewline,
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FrameTooLarge { len, max } => {
                write!(f, "PTY frame length {len} exceeds maximum {max}")
            }
            Self::ControlLineTooLarge { len, max } => {
                write!(f, "control line length {len} exceeds maximum {max}")
            }
            Self::LengthOverflow { len } => write!(f, "PTY frame length {len} exceeds u32::MAX"),
            Self::Json(err) => fmt::Display::fmt(err, f),
            Self::ControlPayloadContainsNewline => {
                write!(f, "control payload unexpectedly contained a newline")
            }
        }
    }
}

impl std::error::Error for FrameError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(err) => Some(err),
            _ => None,
        }
    }
}

impl From<serde_json::Error> for FrameError {
    fn from(err: serde_json::Error) -> Self {
        Self::Json(err)
    }
}

/// Encode a control message as newline-delimited JSON.
pub fn encode_control_message(message: &ControlMessage) -> Result<Vec<u8>, FrameError> {
    let mut encoded = serde_json::to_vec(message)?;
    if encoded.contains(&b'\n') {
        return Err(FrameError::ControlPayloadContainsNewline);
    }
    encoded.push(b'\n');
    Ok(encoded)
}

/// Incremental decoder for newline-delimited JSON control messages.
#[derive(Debug, Default)]
pub struct ControlLineDecoder {
    buffer: Vec<u8>,
}

impl ControlLineDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push bytes from the socket and return every complete decoded message.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<ControlMessage>, FrameError> {
        self.buffer.extend_from_slice(bytes);
        let mut messages = Vec::new();

        while let Some(pos) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=pos).collect();
            let line = &line[..line.len() - 1];
            if line.is_empty() {
                continue;
            }
            messages.push(serde_json::from_slice(line)?);
        }

        // Whatever remains is an incomplete line (no newline yet). Bound it so a
        // peer cannot stream an unbounded newline-less line and OOM us — the
        // incremental analog of `PtyFrameDecoder`'s `MAX_PTY_FRAME_LEN` check.
        // Completed lines drained above, so this only trips on a single oversized
        // partial line; the counter resets naturally as the buffer drains.
        if self.buffer.len() > MAX_CONTROL_LINE_LEN {
            return Err(FrameError::ControlLineTooLarge {
                len: self.buffer.len(),
                max: MAX_CONTROL_LINE_LEN,
            });
        }

        Ok(messages)
    }

    /// Bytes buffered for an incomplete line.
    pub fn buffered_len(&self) -> usize {
        self.buffer.len()
    }
}

/// Encode one raw PTY payload as a four-byte big-endian length prefix plus bytes.
pub fn encode_pty_frame(payload: &[u8]) -> Result<Vec<u8>, FrameError> {
    if payload.len() > MAX_PTY_FRAME_LEN {
        return Err(FrameError::FrameTooLarge {
            len: payload.len(),
            max: MAX_PTY_FRAME_LEN,
        });
    }
    let len = u32::try_from(payload.len())
        .map_err(|_| FrameError::LengthOverflow { len: payload.len() })?;
    let mut frame = Vec::with_capacity(LEN_PREFIX_BYTES + payload.len());
    frame.extend_from_slice(&len.to_be_bytes());
    frame.extend_from_slice(payload);
    Ok(frame)
}

/// Decode a single complete PTY frame.
pub fn decode_pty_frame(frame: &[u8]) -> Result<Option<Vec<u8>>, FrameError> {
    let mut decoder = PtyFrameDecoder::new();
    let frames = decoder.push(frame)?;
    if frames.is_empty() || decoder.buffered_len() > 0 {
        Ok(None)
    } else {
        Ok(frames.into_iter().next())
    }
}

/// Incremental decoder for length-prefixed raw PTY payloads.
#[derive(Debug, Clone)]
pub struct PtyFrameDecoder {
    buffer: VecDeque<u8>,
    max_frame_len: usize,
}

impl PtyFrameDecoder {
    pub fn new() -> Self {
        Self::with_max_frame_len(MAX_PTY_FRAME_LEN)
    }

    pub fn with_max_frame_len(max_frame_len: usize) -> Self {
        Self {
            buffer: VecDeque::new(),
            max_frame_len,
        }
    }

    /// Push bytes and return every complete payload assembled so far.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>, FrameError> {
        self.buffer.extend(bytes.iter().copied());
        let mut frames = Vec::new();

        loop {
            if self.buffer.len() < LEN_PREFIX_BYTES {
                break;
            }

            let len = self.peek_len();
            if len > self.max_frame_len {
                return Err(FrameError::FrameTooLarge {
                    len,
                    max: self.max_frame_len,
                });
            }

            let total_len = LEN_PREFIX_BYTES + len;
            if self.buffer.len() < total_len {
                break;
            }

            for _ in 0..LEN_PREFIX_BYTES {
                self.buffer.pop_front();
            }
            let mut payload = Vec::with_capacity(len);
            for _ in 0..len {
                // Safe: total_len was checked above.
                payload.push(self.buffer.pop_front().expect("checked payload length"));
            }
            frames.push(payload);
        }

        Ok(frames)
    }

    /// Bytes buffered for an incomplete frame.
    pub fn buffered_len(&self) -> usize {
        self.buffer.len()
    }

    fn peek_len(&self) -> usize {
        let bytes = [
            self.buffer[0],
            self.buffer[1],
            self.buffer[2],
            self.buffer[3],
        ];
        u32::from_be_bytes(bytes) as usize
    }
}

impl Default for PtyFrameDecoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Helper for tests/diagnostics: serialize any JSON control-plane value to a string.
pub fn to_json_string<T: Serialize>(value: &T) -> Result<String, FrameError> {
    Ok(serde_json::to_string(value)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{ControlMessage, Request, PROTOCOL_VERSION};

    #[test]
    fn control_messages_are_newline_delimited_json() {
        let message = ControlMessage::request(
            7,
            Request::Hello {
                client_name: "test".into(),
                protocol_version: PROTOCOL_VERSION,
            },
        );
        let encoded = encode_control_message(&message).unwrap();
        assert_eq!(encoded.last(), Some(&b'\n'));

        let mut decoder = ControlLineDecoder::new();
        assert!(decoder.push(&encoded[..5]).unwrap().is_empty());
        let messages = decoder.push(&encoded[5..]).unwrap();
        assert_eq!(messages, vec![message]);
        assert_eq!(decoder.buffered_len(), 0);
    }

    #[test]
    fn pty_frame_round_trips() {
        let payload = b"hello pty";
        let frame = encode_pty_frame(payload).unwrap();
        assert_eq!(decode_pty_frame(&frame).unwrap(), Some(payload.to_vec()));
    }

    #[test]
    fn pty_frame_reassembles_across_split_reads() {
        let first = encode_pty_frame(b"abc").unwrap();
        let second = encode_pty_frame(b"defgh").unwrap();
        let mut combined = Vec::new();
        combined.extend_from_slice(&first);
        combined.extend_from_slice(&second);

        let mut decoder = PtyFrameDecoder::new();
        assert!(decoder.push(&combined[..2]).unwrap().is_empty());
        assert_eq!(decoder.buffered_len(), 2);
        assert!(decoder.push(&combined[2..6]).unwrap().is_empty());
        assert_eq!(decoder.buffered_len(), 6);

        let frames = decoder.push(&combined[6..]).unwrap();
        assert_eq!(frames, vec![b"abc".to_vec(), b"defgh".to_vec()]);
        assert_eq!(decoder.buffered_len(), 0);
    }

    #[test]
    fn control_decoder_rejects_unbounded_newline_less_line() {
        // A peer streaming a newline-less line must be cut off once the buffered
        // partial line exceeds the cap, instead of growing the buffer unboundedly
        // (OOM DoS). Feed just over the cap with no `\n`.
        let mut decoder = ControlLineDecoder::new();
        let flood = vec![b'x'; MAX_CONTROL_LINE_LEN + 1];
        let err = decoder.push(&flood).unwrap_err();
        assert!(
            matches!(
                err,
                FrameError::ControlLineTooLarge { len, max }
                    if len == MAX_CONTROL_LINE_LEN + 1 && max == MAX_CONTROL_LINE_LEN
            ),
            "expected ControlLineTooLarge, got {err:?}",
        );
    }

    #[test]
    fn control_decoder_accepts_a_normal_line_after_the_cap_exists() {
        // The cap must not regress ordinary decoding: a well-formed line (well
        // under the cap) still decodes, and the buffer drains back to empty so the
        // length counter resets between lines.
        let message = ControlMessage::request(
            1,
            Request::Hello {
                client_name: "ok".into(),
                protocol_version: PROTOCOL_VERSION,
            },
        );
        let encoded = encode_control_message(&message).unwrap();
        let mut decoder = ControlLineDecoder::new();
        let messages = decoder.push(&encoded).unwrap();
        assert_eq!(messages, vec![message]);
        assert_eq!(decoder.buffered_len(), 0);
    }

    #[test]
    fn pty_decoder_rejects_oversized_frames_before_payload_arrives() {
        let mut decoder = PtyFrameDecoder::with_max_frame_len(8);
        let too_large_prefix = 9_u32.to_be_bytes();
        let err = decoder.push(&too_large_prefix).unwrap_err();
        assert!(matches!(err, FrameError::FrameTooLarge { len: 9, max: 8 }));
    }
}
