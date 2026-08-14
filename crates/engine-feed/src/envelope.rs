//! Extracts the JSON envelope fields `sequenceNumber`, `header.timestamp`, and `l2Msg`
//! using `memchr`-based byte scans. No JSON parser allocation; we treat the JSON envelope
//! as a flat byte stream and locate fields by their key tags.

use std::sync::LazyLock;

use memchr::memmem::Finder;
use thiserror::Error;

/// Envelope decode errors.
#[derive(Debug, Clone, Copy, Error, PartialEq, Eq)]
pub enum EnvelopeError {
    /// `sequenceNumber` field not found.
    #[error("sequenceNumber field missing")]
    SeqNotFound,
    /// `timestamp` field not found.
    #[error("header.timestamp field missing")]
    TimestampNotFound,
    /// `l2Msg` field not found.
    #[error("l2Msg field missing")]
    L2MsgNotFound,
    /// A numeric field could not be parsed.
    #[error("numeric parse error")]
    NumberParseError,
    /// The l2Msg quoted string is malformed.
    #[error("malformed l2Msg string")]
    L2MsgMalformed,
}

/// Output of the envelope decode: integer fields plus a slice referring to the base64 body
/// (still encoded, ready for in-place base64-simd decode).
#[derive(Debug, Clone, Copy)]
pub struct EnvelopeFields<'a> {
    /// `sequenceNumber` field.
    pub sequence_number: u64,
    /// `header.timestamp` field (unix seconds).
    pub timestamp: u64,
    /// `header.blockNumber` field (parent L1 block at the time of acceptance).
    pub block_number: u64,
    /// Base64-encoded `l2Msg` body (still encoded).
    pub l2msg_b64: &'a [u8],
}

static SEQ_FINDER: LazyLock<Finder<'static>> = LazyLock::new(|| Finder::new(b"\"sequenceNumber\""));
static TS_FINDER: LazyLock<Finder<'static>> = LazyLock::new(|| Finder::new(b"\"timestamp\""));
static L2_FINDER: LazyLock<Finder<'static>> = LazyLock::new(|| Finder::new(b"\"l2Msg\""));
static BLK_FINDER: LazyLock<Finder<'static>> = LazyLock::new(|| Finder::new(b"\"blockNumber\""));

/// Decode the JSON envelope's flat fields from `buf` without allocating.
pub fn decode_envelope(buf: &[u8]) -> Result<EnvelopeFields<'_>, EnvelopeError> {
    let sequence_number = parse_u64_after_key(buf, &SEQ_FINDER, EnvelopeError::SeqNotFound)?;
    let timestamp = parse_u64_after_key(buf, &TS_FINDER, EnvelopeError::TimestampNotFound)?;
    let block_number =
        parse_u64_after_key(buf, &BLK_FINDER, EnvelopeError::TimestampNotFound).unwrap_or(0);
    let l2msg_b64 = parse_quoted_string_after_key(buf, &L2_FINDER, EnvelopeError::L2MsgNotFound)?;

    Ok(EnvelopeFields {
        sequence_number,
        timestamp,
        block_number,
        l2msg_b64,
    })
}

/// Iterate every `BroadcastFeedMessage` in a relay frame. Each WebSocket payload from the
/// nitro relay batches many sequencer-accepted messages; this walker yields one
/// `EnvelopeFields` per `messages[i]` entry. Memory-friendly: returned slices reference
/// `buf`, no allocations.
pub fn iter_envelopes(buf: &[u8]) -> EnvelopeIter<'_> {
    EnvelopeIter { buf, cursor: 0 }
}

/// Iterator returned by [`iter_envelopes`].
pub struct EnvelopeIter<'a> {
    buf: &'a [u8],
    cursor: usize,
}

impl<'a> Iterator for EnvelopeIter<'a> {
    type Item = EnvelopeFields<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.cursor >= self.buf.len() {
            return None;
        }
        let rest = &self.buf[self.cursor..];
        let l2_rel = L2_FINDER.find(rest)?;
        let l2_abs = self.cursor + l2_rel;
        // Look backwards from l2_abs for the nearest "sequenceNumber" / "timestamp"
        // belonging to the same message envelope.
        let seq_pos = find_last_in_prefix(&self.buf[..l2_abs], SEQ_FINDER.needle())?;
        let ts_pos =
            find_last_in_prefix(&self.buf[..l2_abs], TS_FINDER.needle()).unwrap_or(seq_pos);
        let blk_pos = find_last_in_prefix(&self.buf[..l2_abs], BLK_FINDER.needle());
        let sequence_number = parse_u64_at(self.buf, seq_pos + SEQ_FINDER.needle().len()).ok()?;
        let timestamp = parse_u64_at(self.buf, ts_pos + TS_FINDER.needle().len()).unwrap_or(0);
        let block_number = blk_pos
            .and_then(|p| parse_u64_at(self.buf, p + BLK_FINDER.needle().len()).ok())
            .unwrap_or(0);
        let l2msg_b64 = parse_quoted_string_at(self.buf, l2_abs + L2_FINDER.needle().len())?;
        let consumed_end =
            l2msg_b64.as_ptr() as usize - self.buf.as_ptr() as usize + l2msg_b64.len();
        self.cursor = consumed_end + 1;
        Some(EnvelopeFields {
            sequence_number,
            timestamp,
            block_number,
            l2msg_b64,
        })
    }
}

fn find_last_in_prefix(buf: &[u8], needle: &[u8]) -> Option<usize> {
    let mut last = None;
    let mut start = 0;
    while let Some(p) = memchr::memmem::find(&buf[start..], needle) {
        let abs = start + p;
        last = Some(abs);
        start = abs + needle.len();
    }
    last
}

fn parse_u64_at(buf: &[u8], after_key: usize) -> Result<u64, EnvelopeError> {
    let mut cursor = after_key;
    let rest = &buf[cursor..];
    let colon = memchr::memchr(b':', rest).ok_or(EnvelopeError::NumberParseError)?;
    cursor += colon + 1;
    while cursor < buf.len() && (buf[cursor] == b' ' || buf[cursor] == b'\t') {
        cursor += 1;
    }
    let quoted = buf.get(cursor) == Some(&b'"');
    if quoted {
        cursor += 1;
    }
    let value_start = cursor;
    while cursor < buf.len() && buf[cursor].is_ascii_digit() {
        cursor += 1;
    }
    let value = &buf[value_start..cursor];
    if value.is_empty() {
        return Err(EnvelopeError::NumberParseError);
    }
    let mut n: u64 = 0;
    for &b in value {
        n = n
            .checked_mul(10)
            .ok_or(EnvelopeError::NumberParseError)?
            .checked_add((b - b'0') as u64)
            .ok_or(EnvelopeError::NumberParseError)?;
    }
    Ok(n)
}

fn parse_quoted_string_at(buf: &[u8], after_key: usize) -> Option<&[u8]> {
    let mut cursor = after_key;
    let colon = memchr::memchr(b':', &buf[cursor..])?;
    cursor += colon + 1;
    while cursor < buf.len() && (buf[cursor] == b' ' || buf[cursor] == b'\t') {
        cursor += 1;
    }
    if buf.get(cursor) != Some(&b'"') {
        return None;
    }
    cursor += 1;
    let start = cursor;
    while cursor < buf.len() {
        if buf[cursor] == b'"' {
            return Some(&buf[start..cursor]);
        }
        cursor += 1;
    }
    None
}

fn parse_u64_after_key(
    buf: &[u8],
    finder: &Finder<'_>,
    miss: EnvelopeError,
) -> Result<u64, EnvelopeError> {
    let key_pos = finder.find(buf).ok_or(miss)?;
    let after_key = key_pos + finder.needle().len();
    let colon = memchr::memchr(b':', &buf[after_key..]).ok_or(miss)?;
    let mut cursor = after_key + colon + 1;
    while cursor < buf.len() && (buf[cursor] == b' ' || buf[cursor] == b'\t') {
        cursor += 1;
    }
    let quoted = buf.get(cursor) == Some(&b'"');
    if quoted {
        cursor += 1;
    }
    let value_start = cursor;
    while cursor < buf.len() {
        let b = buf[cursor];
        if !b.is_ascii_digit() {
            break;
        }
        cursor += 1;
    }
    let value = &buf[value_start..cursor];
    if value.is_empty() {
        return Err(EnvelopeError::NumberParseError);
    }
    let mut n: u64 = 0;
    for &b in value {
        n = n
            .checked_mul(10)
            .ok_or(EnvelopeError::NumberParseError)?
            .checked_add((b - b'0') as u64)
            .ok_or(EnvelopeError::NumberParseError)?;
    }
    if quoted && buf.get(cursor) != Some(&b'"') {
        return Err(EnvelopeError::NumberParseError);
    }
    Ok(n)
}

fn parse_quoted_string_after_key<'a>(
    buf: &'a [u8],
    finder: &Finder<'_>,
    miss: EnvelopeError,
) -> Result<&'a [u8], EnvelopeError> {
    let key_pos = finder.find(buf).ok_or(miss)?;
    let after_key = key_pos + finder.needle().len();
    let colon = memchr::memchr(b':', &buf[after_key..]).ok_or(miss)?;
    let mut cursor = after_key + colon + 1;
    while cursor < buf.len() && (buf[cursor] == b' ' || buf[cursor] == b'\t') {
        cursor += 1;
    }
    if buf.get(cursor) != Some(&b'"') {
        return Err(EnvelopeError::L2MsgMalformed);
    }
    cursor += 1;
    let start = cursor;
    while cursor < buf.len() {
        if buf[cursor] == b'"' {
            return Ok(&buf[start..cursor]);
        }
        cursor += 1;
    }
    Err(EnvelopeError::L2MsgMalformed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_basic_envelope() {
        let raw = br#"{"version":1,"messages":[{"sequenceNumber":12345,"message":{"message":{"header":{"kind":3,"sender":"0x","blockNumber":22300000,"timestamp":1715000000,"requestId":null,"baseFeeL1":null},"l2Msg":"BAAA"}}}]}"#;
        let f = decode_envelope(raw).unwrap();
        assert_eq!(f.sequence_number, 12345);
        assert_eq!(f.timestamp, 1_715_000_000);
        assert_eq!(f.block_number, 22_300_000);
        assert_eq!(f.l2msg_b64, b"BAAA");
    }

    #[test]
    fn decode_quoted_numeric() {
        let raw = br#"{"sequenceNumber":"7","timestamp":"100","blockNumber":"50","l2Msg":"AAAA"}"#;
        let f = decode_envelope(raw).unwrap();
        assert_eq!(f.sequence_number, 7);
        assert_eq!(f.timestamp, 100);
        assert_eq!(f.block_number, 50);
    }

    #[test]
    fn missing_seq_errors() {
        let raw = br#"{"timestamp":1,"l2Msg":"AAAA"}"#;
        assert!(matches!(
            decode_envelope(raw),
            Err(EnvelopeError::SeqNotFound)
        ));
    }
}
