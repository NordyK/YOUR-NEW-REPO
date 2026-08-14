//! JSON-RPC request builder for `eth_sendRawTransaction` (and conditional variants).

use faster_hex::hex_encode;

/// Classification of a sequencer JSON-RPC response to `eth_sendRawTransaction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitOutcome {
    /// The response carried a `"result"` (a tx hash) — the sequencer ACCEPTED the tx.
    Accepted,
    /// The response carried a JSON-RPC `"error"` — the sequencer REJECTED the tx (reverted in
    /// validation, underpriced, insufficient funds, etc.). The HTTP call succeeded but the trade
    /// did not enter the mempool — an adverse signal for the breach monitor's loss-streak. Nonce
    /// drift and same-nonce pending/landed errors are classified separately below.
    Rejected,
    /// The error indicates the nonce may already be CONSUMED on-chain or a same-nonce tx is in
    /// flight: "nonce too low", "already known", "known transaction", "replacement transaction
    /// underpriced", "already imported". This is NOT a clean pre-inclusion reject — reclaiming the
    /// nonce would re-issue a consumed/in-flight nonce and spin in a nonce-too-low loop, so we
    /// RELEASE it (like `Malformed`) and let the next explicit nonce seed/reconcile advance the
    /// lane. Not an adverse condition either (a tx landed / is landing), so it records neither a
    /// loss nor a win. [Codex-F6]
    AmbiguousLanded,
    /// The error indicates this lane's local nonce is ahead of the chain or sequencer view
    /// ("nonce too high", future/gap wording). Re-issuing the same nonce would spin, but silently
    /// releasing it would push the lane farther ahead. The engine must stop this lane, reclaim the
    /// slot locally, and require a fresh chain nonce seed before further live submit.
    NonceSyncRequired,
    /// Neither `"result"` nor `"error"` present (malformed / unexpected) — treated as rejected
    /// (fail-closed: never count an unparseable body as an accepted submission).
    Malformed,
}

impl SubmitOutcome {
    /// Should the claimed nonce be RECLAIMED (returned to the lane for re-issue) rather than
    /// RELEASED (treated as consumed on-chain), given this ack?
    ///
    /// The submit path optimistically marks a nonce "sent" the moment the HTTP POST returns
    /// `Ok` (it reached the wire), then consults this to decide release-vs-reclaim. Only
    /// `Rejected` and `NonceSyncRequired` flip it back to reclaim:
    ///
    /// - `Rejected`: the sequencer refused the tx pre-inclusion (reverted in validation,
    ///   underpriced, insufficient funds). It was NOT sequenced, so the nonce was NOT consumed
    ///   on-chain — it MUST be reclaimed. Releasing it instead consumes the nonce in the lane's
    ///   bookkeeping while the chain never used it, leaving a permanent sequential-nonce gap; on
    ///   Arbitrum a missing nonce parks every higher nonce on the EOA, so the whole lane wedges.
    /// - `NonceSyncRequired`: the lane is ahead of the chain/sequencer view. Reclaim the local
    ///   in-flight slot, but do not re-use it until the submit path has disabled/reseeded the lane.
    /// - `Accepted`: sequenced → nonce consumed on-chain → release (do not reclaim).
    /// - `Malformed`: the ack did not parse, so we cannot tell whether it landed. Treat it as
    ///   possibly-landed and RELEASE (do not reclaim): a false gap is repaired by the background
    ///   reconciler's `force_nonce`, whereas a false reclaim would re-issue a possibly-consumed
    ///   nonce and spin in a tight nonce-too-low reject loop.
    #[inline]
    pub fn nonce_should_reclaim(self) -> bool {
        matches!(
            self,
            SubmitOutcome::Rejected | SubmitOutcome::NonceSyncRequired
        )
    }

    /// Does this outcome require the lane to stop until its next nonce is re-seeded from chain?
    #[inline]
    pub fn requires_nonce_sync(self) -> bool {
        matches!(self, SubmitOutcome::NonceSyncRequired)
    }
}

/// Classify a sequencer response body. A response with a top-level `"result"` is `Accepted`; one
/// with a top-level `"error"` is `Rejected`; anything else is `Malformed`. We scan for the keys
/// rather than full JSON-parse (the body is a tiny, fixed-shape `eth_sendRawTransaction` reply and
/// this is the cold submit path). `"error"` is checked first because a well-formed JSON-RPC error
/// response never also carries `"result"`.
pub fn classify_submit_response(body: &[u8]) -> SubmitOutcome {
    // Find the JSON body start (skip any HTTP headers the caller may have left in).
    let hay = body;
    let has_error = find_subslice(hay, b"\"error\"").is_some();
    if has_error {
        // Distinguish "the nonce is already consumed on-chain / a same-nonce tx is in flight"
        // (RELEASE the nonce — reclaiming would spin nonce-too-low) from a definite pre-inclusion
        // reject (RECLAIM the nonce — it was never sequenced). [Codex-F6]
        if response_is_ambiguous_landed(hay) {
            return SubmitOutcome::AmbiguousLanded;
        }
        if response_requires_nonce_sync(hay) {
            return SubmitOutcome::NonceSyncRequired;
        }
        return SubmitOutcome::Rejected;
    }
    if find_subslice(hay, b"\"result\"").is_some() {
        return SubmitOutcome::Accepted;
    }
    SubmitOutcome::Malformed
}

/// Does this error body indicate the nonce was already consumed on-chain, or a same-nonce tx is in
/// flight? Such acks must RELEASE (not reclaim) the lane nonce. Alloc-free, case-insensitive scan
/// over the small fixed-shape error body (geth / Nitro sequencer wording). [Codex-F6]
fn response_is_ambiguous_landed(body: &[u8]) -> bool {
    const PATTERNS: [&[u8]; 8] = [
        b"nonce too low",
        b"already known",
        b"already imported",
        b"transaction already imported",
        b"known transaction",
        b"replacement transaction underpriced",
        b"replacement underpriced",
        b"replacement fee too low",
    ];
    PATTERNS.iter().any(|p| find_subslice_ci(body, p))
}

/// Does this error body indicate that the locally claimed nonce is ahead of the chain/sequencer
/// view and the lane must be re-seeded before any further live submit?
fn response_requires_nonce_sync(body: &[u8]) -> bool {
    const PATTERNS: [&[u8]; 3] = [b"nonce too high", b"future transaction", b"nonce gap"];
    PATTERNS.iter().any(|p| find_subslice_ci(body, p))
}

/// Tiny substring search (no regex/alloc) for the response classifier.
fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Case-insensitive substring presence test (`needle_lower` must already be ASCII-lowercase).
/// Alloc-free — used only on the cold error-classification path. [Codex-F6]
fn find_subslice_ci(hay: &[u8], needle_lower: &[u8]) -> bool {
    let n = needle_lower.len();
    if n == 0 || hay.len() < n {
        return false;
    }
    hay.windows(n).any(|w| {
        w.iter()
            .zip(needle_lower)
            .all(|(a, b)| a.to_ascii_lowercase() == *b)
    })
}

/// Build a JSON-RPC envelope for `eth_sendRawTransaction(raw_tx)`. Returns the body bytes.
///
/// Preallocates a reasonable buffer and writes directly into it. The id is a monotonic u64
/// owned by the caller.
pub fn build_send_raw_tx_envelope(raw_tx: &[u8], id: u64, out: &mut Vec<u8>) {
    out.clear();
    out.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"id\":");
    write_u64_dec(id, out);
    out.extend_from_slice(b",\"method\":\"eth_sendRawTransaction\",\"params\":[\"0x");
    write_hex(raw_tx, out);
    out.extend_from_slice(b"\"]}");
}

/// Conditional-submit precondition (ERC-7796).
#[derive(Debug, Clone)]
pub struct ConditionalPreconditions {
    /// `blockNumberMin` — earliest acceptable inclusion block.
    pub block_number_min: Option<u64>,
    /// `blockNumberMax` — latest acceptable inclusion block.
    pub block_number_max: Option<u64>,
    /// `timestampMin` — earliest acceptable inclusion timestamp.
    pub timestamp_min: Option<u64>,
    /// `timestampMax` — latest acceptable inclusion timestamp.
    pub timestamp_max: Option<u64>,
    /// `knownAccounts[address].storage[slot] = value` for every touched pool.
    pub known_storage: Vec<KnownStorage>,
}

/// One `(address, slot, value)` storage precondition.
#[derive(Debug, Clone, Copy)]
pub struct KnownStorage {
    /// 20-byte pool / contract address.
    pub address: [u8; 20],
    /// 32-byte storage slot.
    pub slot: [u8; 32],
    /// 32-byte expected storage value.
    pub value: [u8; 32],
}

/// Build a JSON-RPC envelope for `eth_sendRawTransactionConditional(raw_tx, preconditions)`
/// per ERC-7796.
///
/// Production rule (§2.2): a successful response is NOT inclusion finality; the sequencer
/// rejects cheaply on precondition failure but may still reject post-acceptance.
pub fn build_send_raw_tx_conditional_envelope(
    raw_tx: &[u8],
    pre: &ConditionalPreconditions,
    id: u64,
    out: &mut Vec<u8>,
) {
    out.clear();
    out.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"id\":");
    write_u64_dec(id, out);
    out.extend_from_slice(b",\"method\":\"eth_sendRawTransactionConditional\",\"params\":[\"0x");
    write_hex(raw_tx, out);
    out.extend_from_slice(b"\",{");
    let mut first = true;
    if let Some(v) = pre.block_number_min {
        write_field_u64(out, "blockNumberMin", v, &mut first);
    }
    if let Some(v) = pre.block_number_max {
        write_field_u64(out, "blockNumberMax", v, &mut first);
    }
    if let Some(v) = pre.timestamp_min {
        write_field_u64(out, "timestampMin", v, &mut first);
    }
    if let Some(v) = pre.timestamp_max {
        write_field_u64(out, "timestampMax", v, &mut first);
    }
    if !pre.known_storage.is_empty() {
        if !first {
            out.push(b',');
        }
        out.extend_from_slice(b"\"knownAccounts\":{");
        // Group storage entries by address.
        let mut emitted_addrs: std::collections::BTreeMap<&[u8; 20], Vec<&KnownStorage>> =
            std::collections::BTreeMap::new();
        for ks in &pre.known_storage {
            emitted_addrs.entry(&ks.address).or_default().push(ks);
        }
        let mut addr_first = true;
        for (addr, slots) in emitted_addrs.iter() {
            if !addr_first {
                out.push(b',');
            }
            addr_first = false;
            out.push(b'"');
            out.extend_from_slice(b"0x");
            let mut addr_hex = [0u8; 40];
            hex_encode(&addr[..], &mut addr_hex).unwrap();
            out.extend_from_slice(&addr_hex);
            out.extend_from_slice(b"\":{\"storage\":{");
            let mut slot_first = true;
            for ks in slots {
                if !slot_first {
                    out.push(b',');
                }
                slot_first = false;
                out.extend_from_slice(b"\"0x");
                let mut s_hex = [0u8; 64];
                hex_encode(&ks.slot[..], &mut s_hex).unwrap();
                out.extend_from_slice(&s_hex);
                out.extend_from_slice(b"\":\"0x");
                let mut v_hex = [0u8; 64];
                hex_encode(&ks.value[..], &mut v_hex).unwrap();
                out.extend_from_slice(&v_hex);
                out.push(b'"');
            }
            out.extend_from_slice(b"}}");
        }
        out.push(b'}');
        first = false;
    }
    let _ = first;
    out.extend_from_slice(b"}]}");
}

fn write_field_u64(out: &mut Vec<u8>, key: &str, value: u64, first: &mut bool) {
    if !*first {
        out.push(b',');
    }
    *first = false;
    out.push(b'"');
    out.extend_from_slice(key.as_bytes());
    out.extend_from_slice(b"\":");
    write_u64_dec(value, out);
}

fn write_u64_dec(mut n: u64, out: &mut Vec<u8>) {
    if n == 0 {
        out.push(b'0');
        return;
    }
    let mut buf = [0u8; 20];
    let mut i = 20;
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    out.extend_from_slice(&buf[i..]);
}

fn write_hex(bytes: &[u8], out: &mut Vec<u8>) {
    // Hex-encode straight into `out` — no per-submit temp Vec on the core-7 critical path
    // (this runs between the submit decision and the wire for every eth_sendRawTransaction).
    // Resize-in-place keeps faster-hex's SIMD encoder; a scalar push loop measured ~8× slower.
    let start = out.len();
    out.resize(start + bytes.len() * 2, 0);
    hex_encode(bytes, &mut out[start..]).expect("hex buffer sized");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_accepted_on_result_hash() {
        let body = br#"{"jsonrpc":"2.0","id":7,"result":"0xabc123def456"}"#;
        assert_eq!(classify_submit_response(body), SubmitOutcome::Accepted);
    }

    #[test]
    fn classify_rejected_on_error_object() {
        // Sequencer rejected (e.g. reverted / underpriced / nonce). The HTTP call succeeded but
        // the trade did not enter the mempool — a loss-streak signal.
        let body =
            br#"{"jsonrpc":"2.0","id":7,"error":{"code":-32000,"message":"execution reverted"}}"#;
        assert_eq!(classify_submit_response(body), SubmitOutcome::Rejected);
    }

    #[test]
    fn classify_error_takes_precedence_and_malformed_is_rejected_safe() {
        // A body with both (shouldn't happen) → Rejected (fail-closed).
        let both = br#"{"error":{"code":-1},"result":"0x00"}"#;
        assert_eq!(classify_submit_response(both), SubmitOutcome::Rejected);
        // Garbage / empty → Malformed (never counted as accepted).
        assert_eq!(
            classify_submit_response(b"not json"),
            SubmitOutcome::Malformed
        );
        assert_eq!(classify_submit_response(b""), SubmitOutcome::Malformed);
    }

    #[test]
    fn rejected_ack_reclaims_nonce_accepted_and_malformed_do_not() {
        // E1: on a sequencer REJECT the tx was NOT sequenced, so the nonce was NOT consumed —
        // it must be reclaimed (else a permanent gap parks every higher nonce = lane wedge).
        // Accepted = consumed on-chain → release. Malformed = ambiguous → release (a false gap
        // self-heals via the reconciler; a false reclaim would nonce-too-low spin).
        assert!(
            SubmitOutcome::Rejected.nonce_should_reclaim(),
            "reject did not consume the nonce — must reclaim"
        );
        assert!(
            !SubmitOutcome::Accepted.nonce_should_reclaim(),
            "accepted consumed the nonce — must release"
        );
        assert!(
            !SubmitOutcome::Malformed.nonce_should_reclaim(),
            "ambiguous ack — release (reconciler repairs any gap)"
        );
    }

    #[test]
    fn nonce_too_low_and_already_known_release_not_reclaim() {
        // [Codex-F6] "nonce too low": a tx with this nonce already landed → nonce CONSUMED.
        let nonce_low =
            br#"{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"nonce too low"}}"#;
        assert_eq!(
            classify_submit_response(nonce_low),
            SubmitOutcome::AmbiguousLanded
        );
        // "already known" / "replacement underpriced": a same-nonce tx is in flight.
        let known = br#"{"error":{"code":-32000,"message":"already known"}}"#;
        assert_eq!(
            classify_submit_response(known),
            SubmitOutcome::AmbiguousLanded
        );
        let repl = br#"{"error":{"code":-32000,"message":"replacement transaction underpriced"}}"#;
        assert_eq!(
            classify_submit_response(repl),
            SubmitOutcome::AmbiguousLanded
        );
        // Case-insensitive (different clients vary the casing).
        let upper = br#"{"error":{"message":"Nonce Too Low"}}"#;
        assert_eq!(
            classify_submit_response(upper),
            SubmitOutcome::AmbiguousLanded
        );
        // Ambiguous-landed RELEASES the nonce (reclaiming would spin nonce-too-low).
        assert!(!SubmitOutcome::AmbiguousLanded.nonce_should_reclaim());
        // A genuine pre-inclusion reject still RECLAIMS the nonce.
        let reverted = br#"{"error":{"code":-32000,"message":"execution reverted"}}"#;
        assert_eq!(classify_submit_response(reverted), SubmitOutcome::Rejected);
        assert!(SubmitOutcome::Rejected.nonce_should_reclaim());
        let funds = br#"{"error":{"message":"insufficient funds for gas * price + value"}}"#;
        assert_eq!(classify_submit_response(funds), SubmitOutcome::Rejected);
    }

    #[test]
    fn nonce_pending_variants_release_and_nonce_too_high_requires_sync() {
        for body in [
            br#"{"error":{"message":"already imported"}}"#.as_slice(),
            br#"{"error":{"message":"transaction already imported"}}"#.as_slice(),
            br#"{"error":{"message":"replacement fee too low"}}"#.as_slice(),
        ] {
            let outcome = classify_submit_response(body);
            assert_eq!(outcome, SubmitOutcome::AmbiguousLanded);
            assert!(
                !outcome.nonce_should_reclaim(),
                "same-nonce pending/imported variants must release, not reclaim"
            );
        }

        let nonce_high = br#"{"error":{"code":-32000,"message":"nonce too high"}}"#;
        let outcome = classify_submit_response(nonce_high);
        assert_eq!(outcome, SubmitOutcome::NonceSyncRequired);
        assert!(outcome.nonce_should_reclaim());
        assert!(outcome.requires_nonce_sync());
    }

    #[test]
    fn envelope_shape() {
        let mut buf = Vec::new();
        build_send_raw_tx_envelope(&[0x01, 0xab], 42, &mut buf);
        let s = std::str::from_utf8(&buf).unwrap();
        assert!(s.contains("\"method\":\"eth_sendRawTransaction\""));
        assert!(s.contains("\"id\":42"));
        assert!(s.contains("0x01ab"));
    }

    #[test]
    fn conditional_envelope_known_storage() {
        let mut buf = Vec::new();
        let pre = ConditionalPreconditions {
            block_number_min: None,
            block_number_max: Some(123),
            timestamp_min: None,
            timestamp_max: None,
            known_storage: vec![KnownStorage {
                address: [0xa1u8; 20],
                slot: [0u8; 32],
                value: [0xffu8; 32],
            }],
        };
        build_send_raw_tx_conditional_envelope(&[0x01, 0xab], &pre, 7, &mut buf);
        let s = std::str::from_utf8(&buf).unwrap();
        assert!(s.contains("eth_sendRawTransactionConditional"));
        assert!(s.contains("blockNumberMax"));
        assert!(s.contains("knownAccounts"));
        assert!(s.contains("0xa1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1"));
    }
}
