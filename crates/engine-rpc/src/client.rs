//! Blocking JSON-RPC client. Used only from the background control plane.

use std::time::Duration;

use alloy_primitives::U256;
use faster_hex::{hex_decode, hex_encode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;

use engine_feed::Address;

/// RPC errors.
#[derive(Debug, Error)]
pub enum RpcError {
    /// Network / IO error.
    #[error("transport error: {0}")]
    Transport(String),
    /// JSON decode error.
    #[error("json error: {0}")]
    Json(String),
    /// RPC method returned an error envelope.
    #[error("rpc error {code}: {message}")]
    Server {
        /// Numeric error code from the RPC server.
        code: i64,
        /// Human-readable message.
        message: String,
        /// Optional ABI-encoded revert data (`error.data`) as a `0x…` hex string. Carries custom
        /// errors and revert-estimate payloads (e.g. Fluid DEX `estimateSwapIn`). `None` if absent.
        data: Option<String>,
    },
    /// Result payload could not be parsed into the expected shape.
    #[error("decode error: {0}")]
    Decode(String),
    /// [Stage 5 · WP0 · A6] The raw HTTP response body exceeded the caller's byte cap and was
    /// rejected BEFORE JSON parse / allocation. A distinct variant (not `Decode`) so a caller can
    /// classify it as an input-limit rejection without string-matching a message.
    #[error("response body exceeds cap of {limit} bytes")]
    BodyTooLarge {
        /// The byte cap that was exceeded.
        limit: usize,
    },
}

/// P0.4 (Codex 2026-07-10): strip credential-bearing segments from any provider-facing message
/// BEFORE it can reach a log line. One 4h soak's engine.log carried 24,975 full key-bearing
/// provider URLs — ureq's error `Display` embeds the complete request URL. Masks:
/// * Alchemy-style path keys: `/v2/<key>` → `/v2/<redacted>` (any scheme/host),
/// * key-ish query values: `key`/`apikey`/`api_key`/`token`/`auth` `=…` → `=<redacted>`,
/// * URL userinfo: `scheme://user:pass@host` → `scheme://<redacted>@host`.
/// 2026-07-10 RPC split: latency-sensitive consumers (the REVM validator's fork
/// fetches, gas refresher, Fluid poller, submitter background reads) use
/// `ARBITRUM_RPC_URL_HOT` when set, falling back to `ARBITRUM_RPC_URL`. Boot-time
/// bulk (discovery, snapshot, prewarm), resync batches, and the shadow replayer stay
/// on the base URL — the hot key's quota is reserved for the calls that decide money.
/// Pure resolver for testability; `hot_rpc_url` is the env wrapper.
pub fn resolve_hot_rpc_url(hot: Option<String>, base: Option<String>) -> Option<String> {
    hot.filter(|s| !s.trim().is_empty()).or(base)
}

/// See [`resolve_hot_rpc_url`]. `ARBITRUM_RPC_URL_HOT` accepts a COMMA-SEPARATED list
/// (2026-07-10 QuickNode integration): each candidate is probed once with `eth_chainId`
/// at boot and the fastest healthy endpoint wins (~20 credits/candidate, cached for the
/// process lifetime). A single entry short-circuits with no probe.
pub fn hot_rpc_url() -> Option<String> {
    static V: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    V.get_or_init(|| {
        let raw = resolve_hot_rpc_url(
            std::env::var("ARBITRUM_RPC_URL_HOT").ok(),
            std::env::var("ARBITRUM_RPC_URL").ok(),
        )?;
        let candidates: Vec<String> = raw
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        Some(select_hot_endpoint(candidates, probe_endpoint_rtt))
    })
    .clone()
}

/// A7 (2026-07-12) archive supplement for the S5 MPT cold-verify tier. `eth_getProof` (+ the block
/// `stateRoot` anchor + historical receipts) needs an ARCHIVE-capable node. Owner RPC policy: lava
/// (base) is a decentralized aggregator with spotty archive coverage and won't reliably serve
/// `eth_getProof`; Alchemy/QuickNode are latency-sensitive AND "supplement what lava can't". So the
/// proof reads must route to an archive endpoint, NOT lava-base. `ARBITRUM_RPC_URL_ARCHIVE` names a
/// dedicated archive node; when unset we fall back to the hot endpoint (Alchemy is archive-capable)
/// via [`resolve_hot_rpc_url`], then the base URL — default-safe: nothing breaks if it is not set.
/// Pure resolver for testability; `archive_rpc_url` is the env wrapper.
pub fn resolve_archive_rpc_url(
    archive: Option<String>,
    hot: Option<String>,
    base: Option<String>,
) -> Option<String> {
    archive
        .filter(|s| !s.trim().is_empty())
        .or_else(|| resolve_hot_rpc_url(hot, base))
}

/// See [`resolve_archive_rpc_url`]. Reads `ARBITRUM_RPC_URL_ARCHIVE`, else falls back to the
/// (boot-probed, cached) hot endpoint [`hot_rpc_url`] and finally the base URL. Cached for the
/// process lifetime. When a dedicated archive endpoint is set this resolves WITHOUT probing hot.
pub fn archive_rpc_url() -> Option<String> {
    static V: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    V.get_or_init(|| {
        match std::env::var("ARBITRUM_RPC_URL_ARCHIVE")
            .ok()
            .filter(|s| !s.trim().is_empty())
        {
            Some(archive) => Some(archive.trim().to_string()),
            // No dedicated archive node: reuse the hot endpoint (archive-capable), which itself
            // falls back to the base URL. Mirrors `resolve_archive_rpc_url(None, hot, base)`.
            None => hot_rpc_url(),
        }
    })
    .clone()
}

/// Probe one endpoint's request RTT with a single cheap `eth_chainId` (correctness-checked:
/// a reply that isn't chain 42161/0xa4b1-style hex is treated as unhealthy). 2s budget.
fn probe_endpoint_rtt(url: &str) -> Option<std::time::Duration> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(1))
        .timeout_read(Duration::from_secs(2))
        .build();
    let t0 = std::time::Instant::now();
    let resp = agent
        .post(url)
        .set("Content-Type", "application/json")
        .send_string(r#"{"jsonrpc":"2.0","id":1,"method":"eth_chainId","params":[]}"#)
        .ok()?;
    let body = resp.into_string().ok()?;
    body.contains("\"result\"").then(|| t0.elapsed())
}

/// Pure selection (unit-tested with an injected probe): fastest healthy candidate wins;
/// all-unhealthy falls back to the FIRST candidate (fail-open — the consumer's own
/// retries handle a degraded endpoint better than having none).
pub fn select_hot_endpoint(
    candidates: Vec<String>,
    probe: impl Fn(&str) -> Option<std::time::Duration>,
) -> String {
    if candidates.len() == 1 {
        return candidates.into_iter().next().expect("len checked");
    }
    let mut best: Option<(std::time::Duration, &String)> = None;
    for c in &candidates {
        if let Some(rtt) = probe(c) {
            if best.map(|(b, _)| rtt < b).unwrap_or(true) {
                best = Some((rtt, c));
            }
        }
    }
    match best {
        Some((rtt, url)) => {
            tracing::info!(
                rtt_ms = rtt.as_millis() as u64,
                candidates = candidates.len(),
                "hot RPC endpoint selected by boot probe"
            );
            url.clone()
        }
        None => {
            tracing::warn!("all hot RPC candidates failed the boot probe; using the first");
            candidates.into_iter().next().expect("non-empty")
        }
    }
}

/// Mask API keys / credentials embedded in RPC endpoint URLs before a message reaches
/// logs: `/v2/<key>` path segments, `apikey=`-style query values, and URL userinfo.
pub fn redact_secrets(msg: &str) -> String {
    let mut out = String::with_capacity(msg.len());
    let bytes = msg.as_bytes();
    let mut i = 0usize;
    let is_delim = |c: u8| matches!(c, b'/' | b'?' | b'&' | b'"' | b'\'' | b' ' | b')' | b',');
    while i < bytes.len() {
        let rest = &msg[i..];
        // Path key: "/v2/<key>" (mask up to the next delimiter).
        if rest.starts_with("/v2/") {
            out.push_str("/v2/<redacted>");
            i += 4;
            while i < bytes.len() && !is_delim(bytes[i]) {
                i += 1;
            }
            continue;
        }
        // Bare hex path key (QuickNode/Infura style): a path segment that is PURE hex and
        // 32..=64 chars long is a credential, never a legitimate path component.
        if rest.starts_with('/') {
            let seg_end = rest[1..]
                .find(|c: char| is_delim(c as u8))
                .map(|e| e + 1)
                .unwrap_or(rest.len());
            let seg = &rest[1..seg_end];
            if (32..=64).contains(&seg.len()) && seg.bytes().all(|b| b.is_ascii_hexdigit()) {
                out.push_str("/<redacted>");
                i += seg_end;
                continue;
            }
        }
        // Userinfo: "://user:pass@host" (only when '@' precedes the next '/').
        if rest.starts_with("://") {
            let tail = &rest[3..];
            let at = tail.find('@');
            let slash = tail.find('/').unwrap_or(tail.len());
            if let Some(at) = at.filter(|&a| a < slash && a > 0) {
                out.push_str("://<redacted>@");
                i += 3 + at + 1;
                continue;
            }
        }
        // Query keys: "key=", "apikey=", "api_key=", "token=", "auth=" (case-insensitive).
        let lower = rest.to_ascii_lowercase();
        let hit = ["apikey=", "api_key=", "token=", "auth=", "key="]
            .iter()
            .find(|k| lower.starts_with(*k));
        if let Some(k) = hit {
            out.push_str(&msg[i..i + k.len()]);
            out.push_str("<redacted>");
            i += k.len();
            while i < bytes.len() && !matches!(bytes[i], b'&' | b'"' | b'\'' | b' ' | b')') {
                i += 1;
            }
            continue;
        }
        let ch = msg[i..].chars().next().expect("in-bounds char");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Arbitrum JSON-RPC client.
pub struct ArbitrumRpcClient {
    endpoint: String,
    agent: ureq::Agent,
    id_counter: std::sync::atomic::AtomicU64,
}

impl ArbitrumRpcClient {
    /// Construct a client targeting `endpoint`. Use the operator's Alchemy URL or the
    /// public Arbitrum One endpoint.
    ///
    /// Timeouts are env-tunable (2026-07-10): free-tier public RPCs are slow and flaky, and a
    /// hardcoded 10s read timeout turned hundreds of failing Curve-pool snapshot calls into an
    /// ~18-min boot on the box. `RPC_TIMEOUT_READ_SECS` (default 10) lets the operator fail-fast
    /// on a public endpoint (e.g. 4) or lengthen it for a congested private node. A paid archive
    /// node needs no change. `RPC_TIMEOUT_CONNECT_SECS` (default 5) tunes the dial timeout.
    pub fn new(endpoint: impl Into<String>) -> Self {
        let read_secs = std::env::var("RPC_TIMEOUT_READ_SECS")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(10);
        let connect_secs = std::env::var("RPC_TIMEOUT_CONNECT_SECS")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(5);
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(connect_secs))
            .timeout_read(Duration::from_secs(read_secs))
            .timeout_write(Duration::from_secs(5))
            .build();
        Self {
            endpoint: endpoint.into(),
            agent,
            id_counter: std::sync::atomic::AtomicU64::new(1),
        }
    }

    /// The RPC endpoint URL this client targets. Used as the archive-supplement fallback source
    /// (A7) when `ARBITRUM_RPC_URL_ARCHIVE`/hot/base are unset, so the proof fetcher can reuse the
    /// caller's own endpoint rather than fabricate one.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Generic JSON-RPC call.
    pub fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let id = self
            .id_counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let resp = self
            .agent
            .post(&self.endpoint)
            .set("Content-Type", "application/json")
            .send_json(body)
            .map_err(|e| RpcError::Transport(redact_secrets(&e.to_string())))?;
        let parsed: RpcResponse = resp
            .into_json()
            .map_err(|e| RpcError::Json(e.to_string()))?;
        if let Some(err) = parsed.error {
            return Err(RpcError::Server {
                code: err.code,
                message: err.message,
                data: err.data.and_then(|d| d.as_str().map(|s| s.to_string())),
            });
        }
        parsed
            .result
            .ok_or_else(|| RpcError::Decode("missing result".into()))
    }

    /// [Stage 5 · WP0 · A6] Like [`call`](Self::call) but reads the raw response body through a hard
    /// byte cap, rejecting an oversized body with [`RpcError::BodyTooLarge`] BEFORE it is parsed into
    /// a `serde_json::Value` (i.e. before the large allocation). This is the first-layer DoS guard for
    /// the S5 cold-proof tier: a malicious/broken provider cannot stream an unbounded proof body to
    /// OOM the parser. `max_body_bytes` bounds the transport payload; the caller still applies
    /// structural bounds (node counts, hex lengths) on the parsed value.
    pub fn call_capped(
        &self,
        method: &str,
        params: Value,
        max_body_bytes: usize,
    ) -> Result<Value, RpcError> {
        use std::io::Read as _;
        let id = self
            .id_counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let resp = self
            .agent
            .post(&self.endpoint)
            .set("Content-Type", "application/json")
            .send_json(body)
            .map_err(|e| RpcError::Transport(redact_secrets(&e.to_string())))?;
        // Read at most `max_body_bytes + 1` so an exactly-at-cap body still parses while anything
        // larger is caught. `take` bounds the allocation regardless of a lying Content-Length.
        let mut buf = Vec::new();
        resp.into_reader()
            .take(max_body_bytes as u64 + 1)
            .read_to_end(&mut buf)
            .map_err(|e| RpcError::Transport(e.to_string()))?;
        if buf.len() > max_body_bytes {
            return Err(RpcError::BodyTooLarge {
                limit: max_body_bytes,
            });
        }
        let parsed: RpcResponse =
            serde_json::from_slice(&buf).map_err(|e| RpcError::Json(e.to_string()))?;
        if let Some(err) = parsed.error {
            return Err(RpcError::Server {
                code: err.code,
                message: err.message,
                data: err.data.and_then(|d| d.as_str().map(|s| s.to_string())),
            });
        }
        parsed
            .result
            .ok_or_else(|| RpcError::Decode("missing result".into()))
    }

    /// `eth_blockNumber`.
    pub fn block_number(&self) -> Result<u64, RpcError> {
        let v = self.call("eth_blockNumber", json!([]))?;
        let s = v
            .as_str()
            .ok_or_else(|| RpcError::Decode("expected string".into()))?;
        parse_hex_u64(s)
    }

    /// `eth_chainId` — the chain id the endpoint actually serves.
    pub fn chain_id(&self) -> Result<u64, RpcError> {
        let v = self.call("eth_chainId", json!([]))?;
        let s = v
            .as_str()
            .ok_or_else(|| RpcError::Decode("expected string".into()))?;
        parse_hex_u64(s)
    }

    /// AUD-CHAINID: assert the endpoint serves `expected` (Arbitrum One = 42161). Every hardcoded
    /// `*_ARBITRUM` address (Vault, Multicall3, factories, precompiles) is silently WRONG if the
    /// operator points `ARBITRUM_RPC_URL` at another chain; verifying once at startup turns that
    /// misconfiguration into a hard, explanatory boot error instead of garbage reads.
    pub fn verify_chain_id(&self, expected: u64) -> Result<(), RpcError> {
        chain_id_matches(self.chain_id()?, expected)
    }

    /// `eth_gasPrice` in wei.
    pub fn gas_price_wei(&self) -> Result<u128, RpcError> {
        let v = self.call("eth_gasPrice", json!([]))?;
        let s = v
            .as_str()
            .ok_or_else(|| RpcError::Decode("expected string".into()))?;
        let n = parse_hex_u256(s)?;
        if n.bit_len() > 128 {
            return Err(RpcError::Decode("gas price > u128".into()));
        }
        let limbs = n.as_limbs();
        Ok((limbs[0] as u128) | ((limbs[1] as u128) << 64))
    }

    /// `eth_getBalance` for native ETH balance at `block`.
    pub fn balance(&self, address: &Address, block: BlockTag) -> Result<U256, RpcError> {
        let v = self.call(
            "eth_getBalance",
            json!([encode_address(address), block.to_json_param()]),
        )?;
        let s = v
            .as_str()
            .ok_or_else(|| RpcError::Decode("expected hex string".into()))?;
        parse_hex_u256(s)
    }

    /// Build the positional `eth_call` params for `(to, data, block)`. Split out so the
    /// block-parameter encoding (incl. the EIP-1898 hash object) is unit-testable without a
    /// live endpoint.
    pub fn eth_call_params(to: &Address, data: &[u8], block: BlockTag) -> Value {
        let data_hex = encode_hex_with_prefix(data);
        let to_hex = encode_address(to);
        json!([{"to": to_hex, "data": data_hex}, block.to_json_param()])
    }

    /// `eth_call`. Returns the raw response bytes (hex-decoded).
    pub fn eth_call(
        &self,
        to: &Address,
        data: &[u8],
        block: BlockTag,
    ) -> Result<Vec<u8>, RpcError> {
        let v = self.call("eth_call", Self::eth_call_params(to, data, block))?;
        let s = v
            .as_str()
            .ok_or_else(|| RpcError::Decode("expected hex string".into()))?;
        decode_hex_with_prefix(s)
    }

    /// `eth_getBlockByNumber(tag, false)` (or `eth_getBlockByHash` for a hash tag) → the
    /// canonical `(number, hash)` header pair used to anchor the WI-13 one-block
    /// gas-authority fetch.
    pub fn block_header(&self, block: BlockTag) -> Result<(u64, [u8; 32]), RpcError> {
        let v = match block {
            BlockTag::Hash(h) => self.call(
                "eth_getBlockByHash",
                json!([encode_hex_with_prefix(&h), false]),
            )?,
            other => self.call("eth_getBlockByNumber", json!([other.as_str(), false]))?,
        };
        parse_block_header(&v)
    }

    /// `eth_getStorageAt`. Returns 32 bytes.
    pub fn storage_at(
        &self,
        address: &Address,
        slot: &[u8; 32],
        block: BlockTag,
    ) -> Result<[u8; 32], RpcError> {
        let v = self.call(
            "eth_getStorageAt",
            json!([
                encode_address(address),
                encode_hex_with_prefix(slot),
                block.to_json_param()
            ]),
        )?;
        let s = v
            .as_str()
            .ok_or_else(|| RpcError::Decode("expected hex string".into()))?;
        let bytes = decode_hex_with_prefix(s)?;
        if bytes.len() != 32 {
            return Err(RpcError::Decode("storage value not 32 bytes".into()));
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(&bytes);
        Ok(out)
    }

    /// `eth_getTransactionCount` for nonce reconciliation.
    pub fn transaction_count(&self, address: &Address, block: BlockTag) -> Result<u64, RpcError> {
        let v = self.call(
            "eth_getTransactionCount",
            json!([encode_address(address), block.to_json_param()]),
        )?;
        let s = v
            .as_str()
            .ok_or_else(|| RpcError::Decode("expected hex string".into()))?;
        parse_hex_u64(s)
    }

    /// `eth_getCode` returning the raw bytecode.
    pub fn get_code(&self, address: &Address, block: BlockTag) -> Result<Vec<u8>, RpcError> {
        let v = self.call(
            "eth_getCode",
            json!([encode_address(address), block.to_json_param()]),
        )?;
        let s = v
            .as_str()
            .ok_or_else(|| RpcError::Decode("expected hex string".into()))?;
        decode_hex_with_prefix(s)
    }

    /// One JSON-RPC **batch** (single HTTP round-trip) of `calls`. Returns one per-item result
    /// in the SAME ORDER as `calls` (the server may reply out of order — items are matched back
    /// by id). Outer `Err` = transport / envelope failure (nothing usable came back); inner
    /// `Err` = that item's RPC error. This is the latency primitive that collapses the fork
    /// fetcher's N sequential round-trips into one (validation hot-path cold misses).
    pub fn call_batch(
        &self,
        calls: &[(&str, Value)],
    ) -> Result<Vec<Result<Value, RpcError>>, RpcError> {
        if calls.is_empty() {
            return Ok(Vec::new());
        }
        let first_id = self
            .id_counter
            .fetch_add(calls.len() as u64, std::sync::atomic::Ordering::Relaxed);
        let body: Vec<Value> = calls
            .iter()
            .enumerate()
            .map(|(i, (method, params))| {
                json!({
                    "jsonrpc": "2.0",
                    "id": first_id + i as u64,
                    "method": method,
                    "params": params,
                })
            })
            .collect();
        let resp = self
            .agent
            .post(&self.endpoint)
            .set("Content-Type", "application/json")
            .send_json(Value::Array(body))
            .map_err(|e| RpcError::Transport(redact_secrets(&e.to_string())))?;
        let parsed: Vec<RpcResponse> = resp
            .into_json()
            .map_err(|e| RpcError::Json(e.to_string()))?;
        // Servers may reply in any order — slot each item back by id; anything missing stays a
        // Decode error for that position (never silently defaulted).
        let mut out: Vec<Result<Value, RpcError>> = (0..calls.len())
            .map(|_| Err(RpcError::Decode("missing batch item".into())))
            .collect();
        for item in parsed {
            let Some(idx) = item
                .id
                .and_then(|id| id.checked_sub(first_id))
                .and_then(|d| usize::try_from(d).ok())
                .filter(|i| *i < out.len())
            else {
                continue;
            };
            out[idx] = if let Some(err) = item.error {
                Err(RpcError::Server {
                    code: err.code,
                    message: err.message,
                    data: err.data.and_then(|d| d.as_str().map(|s| s.to_string())),
                })
            } else {
                item.result
                    .ok_or_else(|| RpcError::Decode("missing result".into()))
            };
        }
        Ok(out)
    }

    /// Fetch an account's `(balance, code, nonce)` in ONE batched round-trip (was 3 sequential).
    /// Per-field `Result`s preserve the fork fetcher's fail-closed semantics: an errored field is
    /// counted and defaulted by the caller, exactly like the sequential path did.
    pub fn account_bundle(
        &self,
        address: &Address,
        block: BlockTag,
    ) -> Result<AccountBundle, RpcError> {
        let addr = encode_address(address);
        let blk = block.to_json_param();
        let mut items = self
            .call_batch(&[
                ("eth_getBalance", json!([addr, blk])),
                ("eth_getCode", json!([addr, blk])),
                ("eth_getTransactionCount", json!([addr, blk])),
            ])?
            .into_iter();
        let hex_str = |v: Value| -> Result<String, RpcError> {
            v.as_str()
                .map(|s| s.to_string())
                .ok_or_else(|| RpcError::Decode("expected hex string".into()))
        };
        let balance = items
            .next()
            .expect("3 batch items")
            .and_then(&hex_str)
            .and_then(|s| parse_hex_u256(&s));
        let code = items
            .next()
            .expect("3 batch items")
            .and_then(&hex_str)
            .and_then(|s| decode_hex_with_prefix(&s));
        let nonce = items
            .next()
            .expect("3 batch items")
            .and_then(&hex_str)
            .and_then(|s| parse_hex_u64(&s));
        Ok(AccountBundle {
            balance,
            code,
            nonce,
        })
    }

    /// Batched `eth_getStorageAt` over `(address, slot)` pairs in ONE round-trip, results in
    /// input order. Used by the boot/idle prewarm to bulk-load derived hot slots (tick data,
    /// fee growth, observations) without paying one round-trip per slot.
    pub fn storage_batch(
        &self,
        reads: &[(Address, [u8; 32])],
        block: BlockTag,
    ) -> Result<Vec<Result<[u8; 32], RpcError>>, RpcError> {
        let blk = block.to_json_param();
        let params: Vec<Value> = reads
            .iter()
            .map(|(addr, slot)| json!([encode_address(addr), encode_hex_with_prefix(slot), blk]))
            .collect();
        let calls: Vec<(&str, Value)> = params
            .into_iter()
            .map(|p| ("eth_getStorageAt", p))
            .collect();
        let items = self.call_batch(&calls)?;
        Ok(items
            .into_iter()
            .map(|r| {
                r.and_then(|v| {
                    let s = v
                        .as_str()
                        .ok_or_else(|| RpcError::Decode("expected hex string".into()))?;
                    let bytes = decode_hex_with_prefix(s)?;
                    if bytes.len() != 32 {
                        return Err(RpcError::Decode("storage value not 32 bytes".into()));
                    }
                    let mut w = [0u8; 32];
                    w.copy_from_slice(&bytes);
                    Ok(w)
                })
            })
            .collect())
    }
}

/// Per-field results of a batched account fetch ([`ArbitrumRpcClient::account_bundle`]).
#[derive(Debug)]
pub struct AccountBundle {
    /// `eth_getBalance`.
    pub balance: Result<U256, RpcError>,
    /// `eth_getCode` (raw bytes; empty = EOA).
    pub code: Result<Vec<u8>, RpcError>,
    /// `eth_getTransactionCount`.
    pub nonce: Result<u64, RpcError>,
}

/// Block tag for read calls.
#[derive(Debug, Clone, Copy)]
pub enum BlockTag {
    /// `latest`.
    Latest,
    /// `pending` — not generally useful on Arbitrum (no public mempool).
    Pending,
    /// Specific block number.
    Number(u64),
    /// Specific block HASH — encoded as the EIP-1898 `{"blockHash": "0x…"}` object so the
    /// node MUST resolve the exact block (a number can be re-orged/ambiguous across
    /// providers; a hash cannot). WI-13: the one-block gas-authority fetch anchors here.
    Hash([u8; 32]),
}

impl BlockTag {
    /// Display/string form. NOTE: for [`BlockTag::Hash`] this is the bare hash hex — it is
    /// NOT a valid positional JSON-RPC block parameter; param construction must go through
    /// [`BlockTag::to_json_param`].
    pub fn as_str(&self) -> String {
        match self {
            BlockTag::Latest => "latest".to_string(),
            BlockTag::Pending => "pending".to_string(),
            BlockTag::Number(n) => format!("0x{:x}", n),
            BlockTag::Hash(h) => encode_hex_with_prefix(h),
        }
    }

    /// JSON-RPC block parameter. String tags stay strings; [`BlockTag::Hash`] encodes as the
    /// EIP-1898 `{"blockHash": "0x…", "requireCanonical": true}` object so the node MUST resolve the
    /// exact block AND that block must be part of the CANONICAL chain.
    ///
    /// WI-06 T3 fix round 4 (codex round-4 item 9): `requireCanonical: true` is REQUIRED. Per
    /// EIP-1898 the default (`requireCanonical: false`) lets a node serve a RETAINED ORPHAN — a block
    /// that was reorged OUT of the canonical chain but is still in the node's store. The hash pin
    /// exists precisely to FAIL on a reorged-out block (both producers — the gas-authority
    /// `aggregate3_at_hash` and the resync poller's cycle re-pin — expect a reorged-out block to fail
    /// the fetch so their fail-safe skip fires). Without `requireCanonical` the pin would silently
    /// succeed against the orphan and orphan-A state could overwrite canonical-B.
    pub fn to_json_param(&self) -> Value {
        match self {
            BlockTag::Hash(h) => {
                json!({"blockHash": encode_hex_with_prefix(h), "requireCanonical": true})
            }
            other => json!(other.as_str()),
        }
    }
}

#[derive(Deserialize, Serialize)]
struct RpcResponse {
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<RpcErrorBody>,
}

#[derive(Deserialize, Serialize)]
struct RpcErrorBody {
    code: i64,
    message: String,
    /// Optional ABI-encoded revert data (`error.data`), e.g. a custom-error selector + payload.
    /// Some contracts (Fluid DEX `estimateSwapIn`) return their result via a revert with this data.
    #[serde(default)]
    data: Option<Value>,
}

/// Parse an `eth_getBlockByNumber` result object into `(number, hash)`.
pub fn parse_block_header(v: &Value) -> Result<(u64, [u8; 32]), RpcError> {
    let obj = v
        .as_object()
        .ok_or_else(|| RpcError::Decode("block header: result is not an object".into()))?;
    let number = obj
        .get("number")
        .and_then(|n| n.as_str())
        .ok_or_else(|| RpcError::Decode("block header: missing number".into()))?;
    let number = parse_hex_u64(number)?;
    let hash_hex = obj
        .get("hash")
        .and_then(|h| h.as_str())
        .ok_or_else(|| RpcError::Decode("block header: missing hash".into()))?;
    let bytes = decode_hex_with_prefix(hash_hex)?;
    if bytes.len() != 32 {
        return Err(RpcError::Decode("block header: hash is not 32 bytes".into()));
    }
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&bytes);
    Ok((number, hash))
}

/// Parse `0x...` u64.
pub fn parse_hex_u64(s: &str) -> Result<u64, RpcError> {
    let trimmed = s.trim_start_matches("0x");
    u64::from_str_radix(trimmed, 16).map_err(|e| RpcError::Decode(e.to_string()))
}

/// Parse `0x...` u256.
pub fn parse_hex_u256(s: &str) -> Result<U256, RpcError> {
    let trimmed = s.trim_start_matches("0x");
    if trimmed.is_empty() {
        return Ok(U256::ZERO);
    }
    let padded_owner;
    let input = if trimmed.len() % 2 == 1 {
        padded_owner = format!("0{}", trimmed);
        padded_owner.as_str()
    } else {
        trimmed
    };
    if input.len() > 64 {
        return Err(RpcError::Decode("value > 256 bits".into()));
    }
    let mut out = vec![0u8; input.len() / 2];
    hex_decode(input.as_bytes(), &mut out).map_err(|e| RpcError::Decode(e.to_string()))?;
    let mut padded = [0u8; 32];
    padded[32 - out.len()..].copy_from_slice(&out);
    Ok(U256::from_be_bytes(padded))
}

/// Encode a 20-byte address as `0x...`.
pub fn encode_address(a: &Address) -> String {
    let mut s = String::with_capacity(2 + 40);
    s.push_str("0x");
    let mut hex = [0u8; 40];
    hex_encode(&a[..], &mut hex).expect("hex encode");
    s.push_str(std::str::from_utf8(&hex).expect("ascii"));
    s
}

/// Encode bytes as `0x...`.
pub fn encode_hex_with_prefix(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(2 + bytes.len() * 2);
    s.push_str("0x");
    if bytes.is_empty() {
        return s;
    }
    let mut hex = vec![0u8; bytes.len() * 2];
    hex_encode(bytes, &mut hex).expect("hex encode");
    s.push_str(std::str::from_utf8(&hex).expect("ascii"));
    s
}

/// Decode `0x...` into a Vec<u8>.
pub fn decode_hex_with_prefix(s: &str) -> Result<Vec<u8>, RpcError> {
    let trimmed = s.trim_start_matches("0x");
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let mut buf = vec![0u8; trimmed.len() / 2];
    hex_decode(trimmed.as_bytes(), &mut buf).map_err(|e| RpcError::Decode(e.to_string()))?;
    Ok(buf)
}

/// Pure core of [`ArbitrumRpcClient::verify_chain_id`] (unit-tested without a network round-trip):
/// `Ok(())` iff the served id equals the expected one, else an explanatory `Decode` error.
fn chain_id_matches(got: u64, expected: u64) -> Result<(), RpcError> {
    if got != expected {
        return Err(RpcError::Decode(format!(
            "endpoint serves chain id {got}, expected {expected} (Arbitrum One) — refusing to \
             trust hardcoded Arbitrum addresses against the wrong chain"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod chain_id_tests {
    use super::chain_id_matches;
    #[test]
    fn chain_id_matches_accepts_expected_rejects_other() {
        assert!(chain_id_matches(42161, 42161).is_ok());
        assert!(chain_id_matches(1, 42161).is_err(), "mainnet id must be rejected");
        assert!(chain_id_matches(10, 42161).is_err(), "optimism id must be rejected");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hex_u64_basic() {
        assert_eq!(parse_hex_u64("0x1f").unwrap(), 31);
        assert_eq!(parse_hex_u64("0x0").unwrap(), 0);
    }

    /// WI-13: the gas-authority fetch anchors every fee-critical read at ONE explicit block
    /// hash. Per EIP-1898 a hash-anchored call MUST send `{"blockHash": "0x…"}` — a bare
    /// hash string is not a valid positional block parameter and providers may silently
    /// coerce it (exactly the ambiguity the one-block fetch exists to kill).
    ///
    /// WI-06 T3 fix round 4 (codex round-4 item 9): the object MUST also carry
    /// `"requireCanonical": true`. Per EIP-1898 the default (`false`) lets a node serve a RETAINED
    /// ORPHAN (a reorged-out block still in its store) — so the reorg-detection pin would silently
    /// succeed against orphan-A and orphan-A state could overwrite canonical-B. With
    /// `requireCanonical: true` a reorged-out pin FAILS the fetch (the caller's fail-safe skip fires).
    #[test]
    fn eth_call_block_hash_param_uses_eip1898_object() {
        let h = [0x11u8; 32];
        let p = BlockTag::Hash(h).to_json_param();
        assert!(
            p.is_object(),
            "EIP-1898 hash anchor must encode as an object, got {p}"
        );
        assert_eq!(p["blockHash"], json!(format!("0x{}", "11".repeat(32))));
        assert_eq!(
            p["requireCanonical"],
            json!(true),
            "the hash pin MUST require canonicality — otherwise a node may serve a retained orphan \
             and orphan-A state can overwrite canonical-B (codex round-4 item 9)"
        );

        // And eth_call's positional params must go through the same encoding.
        let to = [0x6Cu8; 20];
        let params = ArbitrumRpcClient::eth_call_params(&to, &[0xde, 0xad], BlockTag::Hash(h));
        assert!(
            params[1].is_object(),
            "eth_call block param for a hash anchor must be the EIP-1898 object, got {}",
            params[1]
        );
        assert_eq!(params[1]["blockHash"], json!(format!("0x{}", "11".repeat(32))));
        assert_eq!(
            params[1]["requireCanonical"],
            json!(true),
            "eth_call's hash block param must also require canonicality"
        );
        // String tags stay strings (no regression for latest/number).
        let params = ArbitrumRpcClient::eth_call_params(&to, &[], BlockTag::Number(0x1f));
        assert_eq!(params[1], json!("0x1f"));
        let params = ArbitrumRpcClient::eth_call_params(&to, &[], BlockTag::Latest);
        assert_eq!(params[1], json!("latest"));
    }

    /// WI-13: canonical-header selection parses `(number, hash)` from the standard
    /// `eth_getBlockByNumber` result shape.
    #[test]
    fn block_header_parses_number_and_hash() {
        let v = json!({
            "number": "0x158d2f01",
            "hash": format!("0x{}", "ab".repeat(32)),
            "parentHash": format!("0x{}", "00".repeat(32)),
        });
        let (number, hash) = parse_block_header(&v).expect("standard header shape must parse");
        assert_eq!(number, 0x158d_2f01);
        assert_eq!(hash, [0xAB; 32]);
        // Malformed shapes must be typed decode errors, not panics.
        assert!(parse_block_header(&json!({"number": "0x1"})).is_err(), "missing hash");
        assert!(parse_block_header(&json!(null)).is_err(), "null result (unknown block)");
    }

    #[test]
    fn parse_hex_u256_basic() {
        let v = parse_hex_u256("0x100").unwrap();
        assert_eq!(v, U256::from(256u64));
    }

    #[test]
    fn encode_decode_hex_roundtrip() {
        let data = vec![0u8, 1, 2, 0xff, 0xab];
        let encoded = encode_hex_with_prefix(&data);
        let decoded = decode_hex_with_prefix(&encoded).unwrap();
        assert_eq!(data, decoded);
    }

    #[test]
    fn encode_address_roundtrip() {
        let mut a = [0u8; 20];
        a[..4].copy_from_slice(&[0xaa, 0xbb, 0xcc, 0xdd]);
        let s = encode_address(&a);
        assert!(s.starts_with("0xaabbccdd"));
        assert_eq!(s.len(), 2 + 40);
    }

    /// Serve exactly ONE HTTP request on a fresh loopback port: parse the JSON-RPC batch body,
    /// assert the expected method names (in order) with distinct ids, and reply with
    /// `build(&[(method, id)])` (a JSON array string, arrival order). Returns
    /// (endpoint, join-handle yielding the number of requests served).
    fn serve_batch_once(
        expect_methods: &'static [&'static str],
        build: fn(&[(String, u64)]) -> String,
    ) -> (String, std::thread::JoinHandle<usize>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            let body_start;
            loop {
                let n = sock.read(&mut tmp).unwrap();
                buf.extend_from_slice(&tmp[..n]);
                if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    body_start = p + 4;
                    break;
                }
            }
            let headers = String::from_utf8_lossy(&buf[..body_start]).to_string();
            let clen: usize = headers
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse().unwrap())
                })
                .expect("content-length");
            while buf.len() < body_start + clen {
                let n = sock.read(&mut tmp).unwrap();
                buf.extend_from_slice(&tmp[..n]);
            }
            let body: Value = serde_json::from_slice(&buf[body_start..body_start + clen]).unwrap();
            let arr = body.as_array().expect("batch body must be a JSON array");
            assert_eq!(arr.len(), expect_methods.len(), "one item per call");
            let mut ids: Vec<(String, u64)> = Vec::new();
            for (item, expect) in arr.iter().zip(expect_methods) {
                let m = item["method"].as_str().unwrap().to_string();
                assert_eq!(&m, expect, "method order must match call order");
                ids.push((m, item["id"].as_u64().expect("numeric id")));
            }
            let distinct: std::collections::HashSet<u64> = ids.iter().map(|(_, i)| *i).collect();
            assert_eq!(distinct.len(), ids.len(), "ids must be distinct");
            let reply = build(&ids);
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                reply.len(),
                reply
            );
            sock.write_all(resp.as_bytes()).unwrap();
            1usize
        });
        (format!("http://127.0.0.1:{port}"), handle)
    }

    #[test]
    fn call_batch_single_round_trip_out_of_order_ids_and_item_errors() {
        let (endpoint, server) = serve_batch_once(
            &["eth_getBalance", "eth_getCode", "eth_getTransactionCount"],
            |ids| {
                // Deliberately OUT OF ORDER, with the balance item an error envelope.
                format!(
                    r#"[{{"jsonrpc":"2.0","id":{},"result":"0x60ff"}},{{"jsonrpc":"2.0","id":{},"error":{{"code":-32000,"message":"boom"}}}},{{"jsonrpc":"2.0","id":{},"result":"0x2a"}}]"#,
                    ids[1].1, ids[0].1, ids[2].1
                )
            },
        );
        let c = ArbitrumRpcClient::new(endpoint);
        let out = c
            .call_batch(&[
                ("eth_getBalance", json!(["0x00", "latest"])),
                ("eth_getCode", json!(["0x00", "latest"])),
                ("eth_getTransactionCount", json!(["0x00", "latest"])),
            ])
            .expect("batch transport ok");
        assert_eq!(out.len(), 3);
        match &out[0] {
            Err(RpcError::Server { code, message, .. }) => {
                assert_eq!(*code, -32000);
                assert_eq!(message, "boom");
            }
            other => panic!("balance item must be the server error, got {other:?}"),
        }
        assert_eq!(out[1].as_ref().unwrap().as_str(), Some("0x60ff"));
        assert_eq!(out[2].as_ref().unwrap().as_str(), Some("0x2a"));
        assert_eq!(server.join().unwrap(), 1, "exactly one HTTP round trip");
    }

    #[test]
    fn account_bundle_one_round_trip_three_fields() {
        let (endpoint, server) = serve_batch_once(
            &["eth_getBalance", "eth_getCode", "eth_getTransactionCount"],
            |ids| {
                format!(
                    r#"[{{"jsonrpc":"2.0","id":{},"result":"0x10"}},{{"jsonrpc":"2.0","id":{},"result":"0x60ff"}},{{"jsonrpc":"2.0","id":{},"result":"0x2a"}}]"#,
                    ids[0].1, ids[1].1, ids[2].1
                )
            },
        );
        let c = ArbitrumRpcClient::new(endpoint);
        let b = c
            .account_bundle(&[0x11; 20], BlockTag::Number(465686728))
            .expect("bundle transport ok");
        assert_eq!(b.balance.unwrap(), U256::from(16u64));
        assert_eq!(b.code.unwrap(), vec![0x60, 0xff]);
        assert_eq!(b.nonce.unwrap(), 42);
        assert_eq!(server.join().unwrap(), 1, "exactly one HTTP round trip");
    }

    #[test]
    fn storage_batch_orders_results_and_decodes_words() {
        let (endpoint, server) = serve_batch_once(
            &["eth_getStorageAt", "eth_getStorageAt"],
            |ids| {
                // Reply OUT OF ORDER (second item first) to force id-based matching.
                format!(
                    r#"[{{"jsonrpc":"2.0","id":{},"result":"0x{}"}},{{"jsonrpc":"2.0","id":{},"result":"0x{}"}}]"#,
                    ids[1].1,
                    "22".repeat(32),
                    ids[0].1,
                    "11".repeat(32),
                )
            },
        );
        let c = ArbitrumRpcClient::new(endpoint);
        let out = c
            .storage_batch(
                &[([0xAA; 20], [0u8; 32]), ([0xBB; 20], [1u8; 32])],
                BlockTag::Number(7),
            )
            .expect("storage batch ok");
        assert_eq!(out[0].as_ref().unwrap(), &[0x11u8; 32]);
        assert_eq!(out[1].as_ref().unwrap(), &[0x22u8; 32]);
        assert_eq!(server.join().unwrap(), 1);
    }

    /// P0.4: the exact leak shape from the 07-09 soak — a ureq transport error embedding the
    /// full Alchemy URL — must never survive into an error string; nor query keys or userinfo.
    #[test]
    fn redact_secrets_masks_provider_credentials() {
        let ureq_style =
            "https://arb-mainnet.g.alchemy.com/v2/aBcD1234efGh5678IkkK69: status code 429";
        let red = redact_secrets(ureq_style);
        assert!(!red.contains("aBcD1234efGh5678IkkK69"), "{red}");
        assert!(red.contains("/v2/<redacted>"), "{red}");
        assert!(red.contains("status code 429"), "context preserved: {red}");

        let query = "wss://node.example.com/ws?apikey=SECRET123&x=1 failed";
        let red = redact_secrets(query);
        assert!(!red.contains("SECRET123"), "{red}");
        assert!(red.contains("apikey=<redacted>"), "{red}");
        assert!(red.contains("&x=1 failed"), "{red}");

        let userinfo = "https://user:hunter2@rpc.example.com/path timed out";
        let red = redact_secrets(userinfo);
        assert!(!red.contains("hunter2"), "{red}");
        assert!(red.contains("://<redacted>@rpc.example.com/path"), "{red}");

        // No secrets → byte-identical (no accidental mangling of ordinary messages).
        let plain = "transport error: connection refused (os error 111)";
        assert_eq!(redact_secrets(plain), plain);
    }

    /// Hot-endpoint selection: fastest healthy wins; unhealthy skipped; all-fail
    /// falls back to the first; single candidate short-circuits without probing.
    #[test]
    fn hot_endpoint_selection() {
        use std::time::Duration;
        let c = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // fastest healthy wins
        let pick = super::select_hot_endpoint(c(&["a", "b", "c"]), |u| match u {
            "a" => Some(Duration::from_millis(30)),
            "b" => Some(Duration::from_millis(10)),
            _ => None, // c unhealthy
        });
        assert_eq!(pick, "b");
        // all-fail -> first (fail-open)
        let pick = super::select_hot_endpoint(c(&["x", "y"]), |_| None);
        assert_eq!(pick, "x");
        // single candidate: probe must not even run
        let pick = super::select_hot_endpoint(c(&["only"]), |_| panic!("no probe for single"));
        assert_eq!(pick, "only");
    }

    /// QuickNode-style path keys (`https://name.chain.quiknode.pro/<40-hex>/`) must be
    /// masked: any pure-hex path segment of 32..=64 chars is a credential, not a path.
    #[test]
    fn redacts_quicknode_path_key() {
        let msg = "https://example-x.arbitrum-mainnet.quiknode.pro/deadbeefdeadbeefdeadbeefdeadbeefdeadbeef/: status 429";
        let red = super::redact_secrets(msg);
        assert!(!red.contains("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"), "{red}");
        assert!(red.contains("/<redacted>/"), "{red}");
        // Short hex path segments (route ids etc.) must NOT be masked.
        let ok = super::redact_secrets("https://host/abc123/x");
        assert!(ok.contains("/abc123/"), "{ok}");
    }

    /// RPC split resolver: HOT wins when non-empty; empty/absent falls back to base.
    #[test]
    fn hot_rpc_url_resolution() {
        let h = |s: &str| Some(s.to_string());
        assert_eq!(super::resolve_hot_rpc_url(h("https://hot"), h("https://base")), h("https://hot"));
        assert_eq!(super::resolve_hot_rpc_url(None, h("https://base")), h("https://base"));
        assert_eq!(super::resolve_hot_rpc_url(h("  "), h("https://base")), h("https://base"));
        assert_eq!(super::resolve_hot_rpc_url(None, None), None);
    }

    /// A7 archive-supplement resolver: `ARBITRUM_RPC_URL_ARCHIVE` wins when non-empty; when unset it
    /// falls back to the hot endpoint (Alchemy is archive-capable), then the base URL. This enforces
    /// the owner's RPC policy — lava (base) won't reliably serve `eth_getProof`, so the MPT cold tier
    /// must reach an archive-capable endpoint — while staying default-safe: unset archive+hot+base ⇒
    /// None (caller fails closed), and an unset archive alone still yields a working (hot/base) URL.
    #[test]
    fn archive_rpc_url_resolution() {
        let h = |s: &str| Some(s.to_string());
        // explicit archive endpoint wins over hot and base
        assert_eq!(
            super::resolve_archive_rpc_url(h("https://arch"), h("https://hot"), h("https://base")),
            h("https://arch")
        );
        // archive unset ⇒ hot (archive-capable) supplements
        assert_eq!(
            super::resolve_archive_rpc_url(None, h("https://hot"), h("https://base")),
            h("https://hot")
        );
        // archive + hot unset ⇒ base
        assert_eq!(super::resolve_archive_rpc_url(None, None, h("https://base")), h("https://base"));
        // whitespace-only archive is treated as unset ⇒ hot fallback
        assert_eq!(
            super::resolve_archive_rpc_url(h("   "), h("https://hot"), h("https://base")),
            h("https://hot")
        );
        // nothing configured ⇒ None (fail-closed)
        assert_eq!(super::resolve_archive_rpc_url(None, None, None), None);
    }
}
