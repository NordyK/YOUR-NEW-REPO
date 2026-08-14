//! Chainlink aggregator (price-feed) snapshot, taken once at startup.
//!
//! Batches `latestRoundData()` + `decimals()` for every configured feed into a single
//! Multicall3 `aggregate3`, so all feeds resolve in one round-trip. The result is an
//! authoritative, manipulation-resistant reference mark used to anchor ETH/USD and to
//! cross-check (or override) the pool-derived token marks. Feeds that revert or return
//! implausible data resolve to `None` and the caller falls back to the pool oracle.

use engine_feed::Address;

use crate::client::{ArbitrumRpcClient, BlockTag, RpcError};
use crate::multicall::{aggregate3, Call3};

/// `latestRoundData()` selector — returns (roundId, answer, startedAt, updatedAt,
/// answeredInRound). `answer` is the second 32-byte word.
const LATEST_ROUND_DATA: [u8; 4] = [0xfe, 0xaf, 0x96, 0x8c];
/// `decimals()` selector (uint8).
const DECIMALS: [u8; 4] = [0x31, 0x3c, 0xe5, 0x67];

/// A resolved Chainlink mark: the raw integer answer plus the feed's decimals.
#[derive(Clone, Copy, Debug)]
pub struct ChainlinkMark {
    /// Raw `answer` from `latestRoundData` (positive; we reject <= 0).
    pub answer: u128,
    /// Feed decimals (USD feeds are typically 8; ETH-denominated feeds 18).
    pub decimals: u8,
    /// `updatedAt` unix seconds — lets the caller reject a stale feed.
    pub updated_at: u64,
}

/// Max age (seconds) a Chainlink answer may have before we treat it as stale and refuse to
/// anchor pricing on it. Arbitrum's ETH/USD feed has a ~24h heartbeat (a fresh round is forced
/// at least daily even with no 0.5% deviation trigger), and the per-token feeds we consume are
/// no faster, so 24h is the most generous bound that still rejects a feed the sequencer/oracle
/// has clearly stopped updating. Fail-closed: a feed older than this resolves to `None` and the
/// caller falls back to the pool oracle rather than pricing on a frozen answer.
pub const CHAINLINK_MAX_AGE_SECS: u64 = 86_400;

impl ChainlinkMark {
    /// Answer in human units (`answer / 10^decimals`).
    #[inline]
    pub fn value(&self) -> f64 {
        self.answer as f64 / 10f64.powi(self.decimals as i32)
    }

    /// Staleness-guarded [`value`](Self::value). Returns `Some(value)` only when the feed's
    /// `updated_at` is within `max_age` seconds of `now_secs` (both unix seconds); otherwise
    /// `None` so the caller fail-closes onto the pool oracle. `now_secs < updated_at` (clock skew
    /// / feed updated this second) is treated as fresh.
    #[inline]
    pub fn value_if_fresh(&self, now_secs: u64, max_age: u64) -> Option<f64> {
        if now_secs.saturating_sub(self.updated_at) > max_age {
            return None;
        }
        Some(self.value())
    }
}

/// Batch-fetch a Chainlink mark for each feed. Returns a `Vec` aligned 1:1 with `feeds`;
/// an element is `None` when the feed reverted or returned implausible/negative data.
pub fn fetch_chainlink_marks(
    client: &ArbitrumRpcClient,
    multicall3: &Address,
    feeds: &[Address],
    block: BlockTag,
) -> Result<Vec<Option<ChainlinkMark>>, RpcError> {
    if feeds.is_empty() {
        return Ok(Vec::new());
    }
    let mut calls: Vec<Call3> = Vec::with_capacity(feeds.len() * 2);
    for f in feeds {
        calls.push(Call3 {
            target: *f,
            allow_failure: true,
            call_data: LATEST_ROUND_DATA.to_vec(),
        });
        calls.push(Call3 {
            target: *f,
            allow_failure: true,
            call_data: DECIMALS.to_vec(),
        });
    }
    let results = aggregate3(client, multicall3, &calls, block)?;
    let mut out = Vec::with_capacity(feeds.len());
    for i in 0..feeds.len() {
        out.push(decode_mark(&results[i * 2], &results[i * 2 + 1]));
    }
    Ok(out)
}

/// Decode one feed's `(latestRoundData, decimals)` result pair into a `ChainlinkMark`.
fn decode_mark(
    round: &crate::multicall::CallResult,
    dec: &crate::multicall::CallResult,
) -> Option<ChainlinkMark> {
    if !round.success || !dec.success {
        return None;
    }
    // latestRoundData: 5 words. answer = word[1] (int256), updatedAt = word[3] (uint256).
    if round.return_data.len() < 160 || dec.return_data.len() < 32 {
        return None;
    }
    let answer = positive_u128_from_word(&round.return_data[32..64])?;
    if answer == 0 {
        return None;
    }
    let updated_at = u64_from_word(&round.return_data[96..128]);
    let decimals = dec.return_data[31];
    Some(ChainlinkMark {
        answer,
        decimals,
        updated_at,
    })
}

/// Interpret a 32-byte big-endian int256 word as a positive `u128`. Returns `None` if
/// the high 16 bytes are non-zero (negative or out of u128 range — never valid for a
/// price answer).
#[inline]
fn positive_u128_from_word(word: &[u8]) -> Option<u128> {
    if word.len() < 32 || word[..16].iter().any(|&b| b != 0) {
        return None;
    }
    let mut buf = [0u8; 16];
    buf.copy_from_slice(&word[16..32]);
    Some(u128::from_be_bytes(buf))
}

/// Interpret the low 8 bytes of a 32-byte word as a `u64` (for unix timestamps).
#[inline]
fn u64_from_word(word: &[u8]) -> u64 {
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&word[24..32]);
    u64::from_be_bytes(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::multicall::CallResult;

    fn word_u128(v: u128) -> Vec<u8> {
        let mut w = [0u8; 32];
        w[16..32].copy_from_slice(&v.to_be_bytes());
        w.to_vec()
    }

    #[test]
    fn decodes_eth_usd_mark() {
        // latestRoundData: roundId, answer=2100_0000_0000 (8 dec => $2100), startedAt,
        // updatedAt=1_700_000_000, answeredInRound.
        let mut rd = Vec::new();
        rd.extend_from_slice(&word_u128(1)); // roundId
        rd.extend_from_slice(&word_u128(210_000_000_000)); // answer ($2100 @ 8dec)
        rd.extend_from_slice(&word_u128(0)); // startedAt
        rd.extend_from_slice(&word_u128(1_700_000_000)); // updatedAt
        rd.extend_from_slice(&word_u128(1)); // answeredInRound
        let round = CallResult {
            success: true,
            return_data: rd,
        };
        let mut dword = [0u8; 32];
        dword[31] = 8;
        let dec = CallResult {
            success: true,
            return_data: dword.to_vec(),
        };
        let m = decode_mark(&round, &dec).expect("decodes");
        assert_eq!(m.decimals, 8);
        assert_eq!(m.updated_at, 1_700_000_000);
        assert!((m.value() - 2100.0).abs() < 1e-6);
    }

    #[test]
    fn value_if_fresh_rejects_stale_accepts_fresh() {
        // A mark updated at t=1_700_000_000 with a 24h max-age.
        let m = ChainlinkMark {
            answer: 210_000_000_000,
            decimals: 8,
            updated_at: 1_700_000_000,
        };
        // Fresh: now is 1h after update → accepted, value preserved.
        let fresh = m
            .value_if_fresh(1_700_000_000 + 3_600, CHAINLINK_MAX_AGE_SECS)
            .expect("1h-old feed is fresh");
        assert!((fresh - 2100.0).abs() < 1e-6);
        // Exactly at the boundary (24h) → still fresh.
        assert!(m
            .value_if_fresh(
                1_700_000_000 + CHAINLINK_MAX_AGE_SECS,
                CHAINLINK_MAX_AGE_SECS
            )
            .is_some());
        // Stale: one second past 24h → rejected (fail-closed).
        assert!(m
            .value_if_fresh(
                1_700_000_000 + CHAINLINK_MAX_AGE_SECS + 1,
                CHAINLINK_MAX_AGE_SECS
            )
            .is_none());
        // Clock skew (now < updated_at) is treated as fresh.
        assert!(m
            .value_if_fresh(1_700_000_000 - 5, CHAINLINK_MAX_AGE_SECS)
            .is_some());
    }

    #[test]
    fn rejects_negative_and_failed() {
        // High bytes set => negative/oversized int256 => rejected.
        let mut neg = vec![0xffu8; 32];
        neg.resize(160, 0);
        let round = CallResult {
            success: true,
            return_data: neg,
        };
        let mut dword = [0u8; 32];
        dword[31] = 8;
        let dec = CallResult {
            success: true,
            return_data: dword.to_vec(),
        };
        assert!(decode_mark(&round, &dec).is_none());

        // Failed call => None.
        let failed = CallResult {
            success: false,
            return_data: Vec::new(),
        };
        assert!(decode_mark(&failed, &dec).is_none());
    }
}
