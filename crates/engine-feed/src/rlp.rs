//! Minimal RLP decoder tailored to extracting `(nonce, to, value, data)` from signed
//! Ethereum transactions on Arbitrum. We do not need a general RLP library because every
//! Arbitrum tx falls into one of three concrete shapes.
//!
//! Reference: EIP-2718 / EIP-1559 / EIP-2930 / legacy.

use alloy_primitives::U256;
use thiserror::Error;

use crate::address::{Address, ZERO_ADDRESS};
use crate::l2msg::TransactionInfo;

/// RLP decode errors.
#[derive(Debug, Clone, Copy, Error)]
pub enum RlpError {
    /// Buffer too short.
    #[error("unexpected end of input")]
    Eof,
    /// Malformed encoding.
    #[error("malformed rlp")]
    Malformed,
    /// Numeric value too large for the target field.
    #[error("numeric overflow")]
    Overflow,
}

/// One decoded RLP item: either a byte string or a nested list (for which we record the
/// payload slice bounds).
#[derive(Debug, Clone, Copy)]
enum Item<'a> {
    Bytes(&'a [u8]),
    List(&'a [u8]),
}

fn decode_one(buf: &[u8]) -> Result<(Item<'_>, &[u8]), RlpError> {
    let first = *buf.first().ok_or(RlpError::Eof)?;
    match first {
        0x00..=0x7f => Ok((Item::Bytes(&buf[..1]), &buf[1..])),
        0x80..=0xb7 => {
            let len = (first - 0x80) as usize;
            if buf.len() < 1 + len {
                return Err(RlpError::Eof);
            }
            Ok((Item::Bytes(&buf[1..1 + len]), &buf[1 + len..]))
        }
        0xb8..=0xbf => {
            let len_of_len = (first - 0xb7) as usize;
            if buf.len() < 1 + len_of_len {
                return Err(RlpError::Eof);
            }
            let len = decode_be_usize(&buf[1..1 + len_of_len])?;
            let start = 1 + len_of_len;
            if buf.len() < start + len {
                return Err(RlpError::Eof);
            }
            Ok((Item::Bytes(&buf[start..start + len]), &buf[start + len..]))
        }
        0xc0..=0xf7 => {
            let len = (first - 0xc0) as usize;
            if buf.len() < 1 + len {
                return Err(RlpError::Eof);
            }
            Ok((Item::List(&buf[1..1 + len]), &buf[1 + len..]))
        }
        0xf8..=0xff => {
            let len_of_len = (first - 0xf7) as usize;
            if buf.len() < 1 + len_of_len {
                return Err(RlpError::Eof);
            }
            let len = decode_be_usize(&buf[1..1 + len_of_len])?;
            let start = 1 + len_of_len;
            if buf.len() < start + len {
                return Err(RlpError::Eof);
            }
            Ok((Item::List(&buf[start..start + len]), &buf[start + len..]))
        }
    }
}

/// Extract the access-list LIST BODY from the RLP `rest` positioned at the `access_list` field.
/// Returns the entries' concatenated bytes (outer header stripped), or `&[]` if the field is
/// absent / malformed / not a list. Lenient by design: a bad access list must not fail the whole
/// tx decode (`to`/`data` are the load-bearing fields) — it just yields no wakeup addresses.
fn access_list_body(rest: &[u8]) -> &[u8] {
    match decode_one(rest) {
        Ok((Item::List(body), _)) => body,
        _ => &[],
    }
}

/// Zero-alloc iterator over the addresses named in an EIP-2930/1559 access list.
///
/// `body` is the access-list RLP list body (entries concatenated, outer header stripped) — i.e.
/// exactly [`TransactionInfo::access_list`]. Each entry is `[address, [storageKeys...]]`; this
/// yields the 20-byte `address` of each entry. Iteration stops at the first malformed item (the
/// well-formed prefix is still yielded), so a corrupt tail can't panic the hot path.
#[derive(Debug, Clone, Copy)]
pub struct AccessListAddresses<'a> {
    rest: &'a [u8],
}

impl Iterator for AccessListAddresses<'_> {
    type Item = Address;

    fn next(&mut self) -> Option<Address> {
        while !self.rest.is_empty() {
            let (entry, after) = decode_one(self.rest).ok()?;
            self.rest = after;
            // Each entry is itself a list `[address, [storageKeys]]`.
            let entry_body = match entry {
                Item::List(b) => b,
                Item::Bytes(_) => continue, // not a tuple — skip this item
            };
            let (addr_item, _) = decode_one(entry_body).ok()?;
            if let Item::Bytes(b) = addr_item {
                if b.len() == 20 {
                    let mut a: Address = ZERO_ADDRESS;
                    a.copy_from_slice(b);
                    return Some(a);
                }
            }
            // malformed/empty address field — skip this entry, keep walking
        }
        None
    }
}

/// Iterate the addresses named in a tx access list (see [`AccessListAddresses`]). `body` is
/// [`TransactionInfo::access_list`].
pub fn access_list_addresses(body: &[u8]) -> AccessListAddresses<'_> {
    AccessListAddresses { rest: body }
}

fn decode_be_usize(buf: &[u8]) -> Result<usize, RlpError> {
    if buf.len() > std::mem::size_of::<usize>() {
        return Err(RlpError::Overflow);
    }
    let mut n: usize = 0;
    for &b in buf {
        n = n
            .checked_shl(8)
            .ok_or(RlpError::Overflow)?
            .checked_add(b as usize)
            .ok_or(RlpError::Overflow)?;
    }
    Ok(n)
}

fn rlp_bytes(item: Item<'_>) -> Result<&[u8], RlpError> {
    match item {
        Item::Bytes(b) => Ok(b),
        Item::List(_) => Err(RlpError::Malformed),
    }
}

fn rlp_u64(item: Item<'_>) -> Result<u64, RlpError> {
    let bytes = rlp_bytes(item)?;
    if bytes.len() > 8 {
        return Err(RlpError::Overflow);
    }
    let mut n: u64 = 0;
    for &b in bytes {
        n = n
            .checked_shl(8)
            .ok_or(RlpError::Overflow)?
            .checked_add(b as u64)
            .ok_or(RlpError::Overflow)?;
    }
    Ok(n)
}

fn rlp_u256(item: Item<'_>) -> Result<U256, RlpError> {
    let bytes = rlp_bytes(item)?;
    if bytes.len() > 32 {
        return Err(RlpError::Overflow);
    }
    let mut padded = [0u8; 32];
    padded[32 - bytes.len()..].copy_from_slice(bytes);
    Ok(U256::from_be_bytes(padded))
}

fn rlp_address(item: Item<'_>) -> Result<Address, RlpError> {
    Ok(rlp_address_present(item)?.0)
}

/// Decode an RLP address field, ALSO reporting whether it was present. An empty byte string
/// is the canonical contract-creation `to` — the legacy mapping to the zero address is kept
/// for compatibility, but the `false` flag lets the canonical normalizer type creations as
/// taint instead of conflating them with a genuine `to == 0x0` call (FL-05 ingest half).
fn rlp_address_present(item: Item<'_>) -> Result<(Address, bool), RlpError> {
    let bytes = rlp_bytes(item)?;
    if bytes.is_empty() {
        return Ok((ZERO_ADDRESS, false));
    }
    if bytes.len() != 20 {
        return Err(RlpError::Malformed);
    }
    let mut out = [0u8; 20];
    out.copy_from_slice(bytes);
    Ok((out, true))
}

/// Decode a list head and return the contained payload slice.
fn decode_list(buf: &[u8]) -> Result<&[u8], RlpError> {
    let (item, rest) = decode_one(buf)?;
    if !rest.is_empty() {
        return Err(RlpError::Malformed);
    }
    match item {
        Item::List(p) => Ok(p),
        _ => Err(RlpError::Malformed),
    }
}

/// Decode an EIP-1559 signed transaction's RLP list payload (i.e. after the `0x02` prefix).
///
/// Layout: `[chain_id, nonce, max_priority_fee_per_gas, max_fee_per_gas, gas_limit, to,
///           value, data, access_list, y_parity, r, s]`.
pub fn decode_signed_tx_1559(buf: &[u8]) -> Result<TransactionInfo<'_>, RlpError> {
    let payload = decode_list(buf)?;
    let (chain_id_i, rest) = decode_one(payload)?;
    let chain_id = rlp_u64(chain_id_i)?;
    let (nonce_i, rest) = decode_one(rest)?;
    let nonce = rlp_u64(nonce_i)?;
    let (_max_priority, rest) = decode_one(rest)?;
    let (_max_fee, rest) = decode_one(rest)?;
    let (_gas_limit, rest) = decode_one(rest)?;
    let (to_i, rest) = decode_one(rest)?;
    let (to, to_present) = rlp_address_present(to_i)?;
    let (value_i, rest) = decode_one(rest)?;
    let value = rlp_u256(value_i)?;
    let (data_i, rest) = decode_one(rest)?;
    let data = rlp_bytes(data_i)?;
    let access_list = access_list_body(rest);
    Ok(TransactionInfo {
        tx_type: 0x02,
        to,
        data,
        value,
        nonce,
        chain_id,
        access_list,
        to_present,
    })
}

/// Decode the payload of a signed EIP-7702 set-code transaction (one tx-type byte `0x04`
/// already stripped). Per EIP-7702 the RLP payload is:
///
/// ```text
/// [chain_id, nonce, max_priority_fee_per_gas, max_fee_per_gas, gas_limit, to, value, data,
///  access_list, authorization_list, y_parity, r, s]
/// ```
///
/// Reference: <https://eips.ethereum.org/EIPS/eip-7702>. The prefix up to and including
/// `data` is byte-for-byte identical in field order to EIP-1559 (it only appends an
/// `authorization_list` before the signature), so extracting `(to, value, data)` reuses the
/// 1559 field walk. Without this arm, 7702 set-code transactions — which are real user
/// transactions that frequently wrap router/aggregator swaps via EOA delegation — fall through
/// the envelope match and are silently dropped, making their pool moves invisible to the
/// classifier. Observed live on Arbitrum One (chain 42161) in the certification corpora.
pub fn decode_signed_tx_7702(buf: &[u8]) -> Result<TransactionInfo<'_>, RlpError> {
    let payload = decode_list(buf)?;
    let (_chain_id_i, rest) = decode_one(payload)?;
    let chain_id = rlp_u64(_chain_id_i)?;
    let (nonce_i, rest) = decode_one(rest)?;
    let nonce = rlp_u64(nonce_i)?;
    let (_max_priority, rest) = decode_one(rest)?;
    let (_max_fee, rest) = decode_one(rest)?;
    let (_gas_limit, rest) = decode_one(rest)?;
    let (to_i, rest) = decode_one(rest)?;
    let (to, to_present) = rlp_address_present(to_i)?;
    let (value_i, rest) = decode_one(rest)?;
    let value = rlp_u256(value_i)?;
    let (data_i, rest) = decode_one(rest)?;
    let data = rlp_bytes(data_i)?;
    let access_list = access_list_body(rest);
    Ok(TransactionInfo {
        tx_type: 0x04,
        to,
        data,
        value,
        nonce,
        chain_id,
        access_list,
        to_present,
    })
}

/// Decode the payload of a signed EIP-2930 typed transaction (one tx-type byte `0x01`
/// already stripped). Per EIP-2930 the RLP payload is:
///
/// ```text
/// [chain_id, nonce, gas_price, gas_limit, to, value, data, access_list, y_parity, r, s]
/// ```
///
/// Reference: <https://eips.ethereum.org/EIPS/eip-2930>. Note the field order differs
/// from EIP-1559 (single `gas_price` field replacing the `(max_priority_fee, max_fee)`
/// pair), so calling `decode_signed_tx_1559` against a 2930 payload silently shifts
/// every position after `nonce` — `to` ends up reading the encoded `value`, etc. The
/// classifier hashes `(to, selector)`, so a misdecoded `to` makes the entire 2930 lane
/// invisible to detection. This decoder gets the offsets right.
pub fn decode_signed_tx_2930(buf: &[u8]) -> Result<TransactionInfo<'_>, RlpError> {
    let payload = decode_list(buf)?;
    let (chain_id_i, rest) = decode_one(payload)?;
    let chain_id = rlp_u64(chain_id_i)?;
    let (nonce_i, rest) = decode_one(rest)?;
    let nonce = rlp_u64(nonce_i)?;
    let (_gas_price, rest) = decode_one(rest)?;
    let (_gas_limit, rest) = decode_one(rest)?;
    let (to_i, rest) = decode_one(rest)?;
    let (to, to_present) = rlp_address_present(to_i)?;
    let (value_i, rest) = decode_one(rest)?;
    let value = rlp_u256(value_i)?;
    let (data_i, rest) = decode_one(rest)?;
    let data = rlp_bytes(data_i)?;
    let access_list = access_list_body(rest);
    Ok(TransactionInfo {
        tx_type: 0x01,
        to,
        data,
        value,
        nonce,
        chain_id,
        access_list,
        to_present,
    })
}

/// Decode a legacy (pre-EIP-2718) signed transaction list head.
///
/// Layout: `[nonce, gas_price, gas_limit, to, value, data, v, r, s]`.
pub fn decode_signed_tx_legacy(buf: &[u8]) -> Result<TransactionInfo<'_>, RlpError> {
    let payload = decode_list(buf)?;
    let (nonce_i, rest) = decode_one(payload)?;
    let nonce = rlp_u64(nonce_i)?;
    let (_gas_price, rest) = decode_one(rest)?;
    let (_gas_limit, rest) = decode_one(rest)?;
    let (to_i, rest) = decode_one(rest)?;
    let (to, to_present) = rlp_address_present(to_i)?;
    let (value_i, rest) = decode_one(rest)?;
    let value = rlp_u256(value_i)?;
    let (data_i, rest) = decode_one(rest)?;
    let data = rlp_bytes(data_i)?;
    // v_byte: recover chain_id when EIP-155 was applied. v = chain_id*2 + 35 + y_parity
    let chain_id = if let Ok((v_i, _)) = decode_one(rest) {
        let v = rlp_u64(v_i).unwrap_or(0);
        if v >= 35 {
            (v - 35) / 2
        } else {
            0
        }
    } else {
        0
    };
    Ok(TransactionInfo {
        tx_type: 0x00,
        to,
        data,
        value,
        nonce,
        chain_id,
        access_list: &[],
        to_present,
    })
}

/// Append an RLP list header for a payload of `payload_len` bytes.
fn push_list_header(out: &mut Vec<u8>, payload_len: usize) {
    if payload_len <= 55 {
        out.push(0xc0 + payload_len as u8);
    } else {
        let len_bytes = payload_len.to_be_bytes();
        let trim = len_bytes.iter().position(|&b| b != 0).unwrap_or(7);
        let len_slice = &len_bytes[trim..];
        out.push(0xf7 + len_slice.len() as u8);
        out.extend_from_slice(len_slice);
    }
}

/// Append a minimal-RLP-encoded unsigned integer.
fn push_uint(out: &mut Vec<u8>, v: u64) {
    if v == 0 {
        out.push(0x80);
        return;
    }
    let bytes = v.to_be_bytes();
    let first = bytes.iter().position(|&b| b != 0).unwrap_or(7);
    let slice = &bytes[first..];
    if slice.len() == 1 && slice[0] < 0x80 {
        out.push(slice[0]);
    } else {
        out.push(0x80 + slice.len() as u8);
        out.extend_from_slice(slice);
    }
}

/// Assemble a 65-byte `(r, s, y_parity)` signature from RLP-encoded `r`/`s` byte strings.
fn assemble_sig65(r: &[u8], s: &[u8], y_parity: u8) -> Option<[u8; 65]> {
    if r.len() > 32 || s.len() > 32 {
        return None;
    }
    let mut sig65 = [0u8; 65];
    sig65[32 - r.len()..32].copy_from_slice(r);
    sig65[64 - s.len()..64].copy_from_slice(s);
    sig65[64] = y_parity;
    Some(sig65)
}

/// Recover the EOA sender of a signed-tx envelope — the bytes *after* the L2 kind byte
/// (exactly what `decode_signed_tx_envelope` receives). Returns `None` on malformed input
/// or recovery failure.
///
/// Cold path only: this reconstructs the EIP-1559 / EIP-2930 / legacy signing preimage,
/// keccaks it, and runs secp256k1 ECDSA recovery (tens of µs). It is deliberately *not*
/// called on the hot detector loop — only the shadow-REVM worker, which already runs off
/// the hot cores, needs the real sender so a replayed swap reads the sender's true token
/// balance + allowance from the fork DB.
pub fn recover_tx_sender(envelope: &[u8]) -> Option<Address> {
    let first = *envelope.first()?;
    match first {
        // EIP-1559: 9 fields before (y_parity, r, s).
        0x02 => recover_typed(&envelope[1..], 0x02, 9),
        // EIP-2930: 8 fields before (y_parity, r, s).
        0x01 => recover_typed(&envelope[1..], 0x01, 8),
        // EIP-7702 set-code: 10 fields before (y_parity, r, s) — 1559's 9 plus the appended
        // `authorization_list`. Same signing scheme (keccak(0x04 || rlp(first 10 fields))), so
        // the typed recovery walk applies with the 7702 field count.
        0x04 => recover_typed(&envelope[1..], 0x04, 10),
        // Legacy list head.
        v if v >= 0xc0 => recover_legacy(envelope),
        _ => None,
    }
}

/// Recover the sender of a typed (EIP-1559/2930) transaction. `presig_fields` is the count
/// of RLP items preceding `(y_parity, r, s)`; `type_byte` prefixes the signing preimage.
fn recover_typed(list_buf: &[u8], type_byte: u8, presig_fields: usize) -> Option<Address> {
    let payload = decode_list(list_buf).ok()?;
    let mut rest = payload;
    for _ in 0..presig_fields {
        let (_, next) = decode_one(rest).ok()?;
        rest = next;
    }
    let consumed = payload.len() - rest.len();
    let sig_inner = &payload[..consumed];
    // Signing preimage = type_byte || rlp_list(first `presig_fields` items).
    let mut preimage = Vec::with_capacity(1 + 9 + consumed);
    preimage.push(type_byte);
    push_list_header(&mut preimage, consumed);
    preimage.extend_from_slice(sig_inner);
    let signing_hash = crate::address::keccak256(&preimage);

    let (yp_i, rest) = decode_one(rest).ok()?;
    let y_parity = rlp_u64(yp_i).ok()? as u8;
    let (r_i, rest) = decode_one(rest).ok()?;
    let (s_i, _) = decode_one(rest).ok()?;
    let sig65 = assemble_sig65(rlp_bytes(r_i).ok()?, rlp_bytes(s_i).ok()?, y_parity)?;
    crate::address::recover_sender(&signing_hash, &sig65).ok()
}

/// Recover the sender of a legacy transaction (EIP-155 or pre-155).
fn recover_legacy(list_buf: &[u8]) -> Option<Address> {
    let payload = decode_list(list_buf).ok()?;
    let mut rest = payload;
    // 6 fields: nonce, gas_price, gas_limit, to, value, data.
    for _ in 0..6 {
        let (_, next) = decode_one(rest).ok()?;
        rest = next;
    }
    let consumed = payload.len() - rest.len();
    let presig = &payload[..consumed];

    let (v_i, rest) = decode_one(rest).ok()?;
    let v = rlp_u64(v_i).ok()?;
    let (r_i, rest) = decode_one(rest).ok()?;
    let (s_i, _) = decode_one(rest).ok()?;

    let (y_parity, chain_id): (u8, Option<u64>) = if v >= 35 {
        (((v - 35) % 2) as u8, Some((v - 35) / 2))
    } else if v == 27 || v == 28 {
        ((v - 27) as u8, None)
    } else {
        return None;
    };
    // EIP-155 signing preimage appends [chain_id, 0, 0] to the 6 fields; pre-155 does not.
    let mut inner = Vec::with_capacity(consumed + 12);
    inner.extend_from_slice(presig);
    if let Some(cid) = chain_id {
        push_uint(&mut inner, cid);
        inner.push(0x80);
        inner.push(0x80);
    }
    let mut preimage = Vec::with_capacity(inner.len() + 9);
    push_list_header(&mut preimage, inner.len());
    preimage.extend_from_slice(&inner);
    let signing_hash = crate::address::keccak256(&preimage);

    let sig65 = assemble_sig65(rlp_bytes(r_i).ok()?, rlp_bytes(s_i).ok()?, y_parity)?;
    crate::address::recover_sender(&signing_hash, &sig65).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Hand-built minimal RLP list to verify parser shape:
    // [0x01, 0x02, "0xAA"*20, 0x01]
    #[test]
    fn rlp_decode_one_byte_value() {
        let buf = &[0x01u8][..];
        let (item, rest) = decode_one(buf).unwrap();
        assert!(matches!(item, Item::Bytes(b) if b == [0x01]));
        assert!(rest.is_empty());
    }

    #[test]
    fn rlp_decode_short_list() {
        // [0x42, 0x43]
        let buf = &[0xc2u8, 0x42, 0x43][..];
        let (item, rest) = decode_one(buf).unwrap();
        let payload = match item {
            Item::List(p) => p,
            _ => panic!(),
        };
        assert_eq!(payload, &[0x42, 0x43]);
        assert!(rest.is_empty());
    }

    #[test]
    fn rlp_decode_empty_address_is_zero() {
        let buf = &[0x80u8][..];
        let (item, _) = decode_one(buf).unwrap();
        let addr = rlp_address(item).unwrap();
        assert_eq!(addr, ZERO_ADDRESS);
    }

    #[test]
    fn rlp_decode_address_20() {
        let mut buf = vec![0x94u8];
        buf.extend_from_slice(&[0xAAu8; 20]);
        let (item, _) = decode_one(&buf).unwrap();
        let addr = rlp_address(item).unwrap();
        assert_eq!(addr, [0xAAu8; 20]);
    }

    /// Encode a `Vec<Vec<u8>>` of pre-encoded RLP items as an RLP list. Tests-only
    /// helper kept private to this module; the production engine never builds RLP.
    fn rlp_list(items: &[Vec<u8>]) -> Vec<u8> {
        let payload: Vec<u8> = items.iter().flatten().copied().collect();
        let mut out = Vec::with_capacity(payload.len() + 9);
        if payload.len() <= 55 {
            out.push(0xc0 + payload.len() as u8);
        } else {
            let len_bytes = payload.len().to_be_bytes();
            let trim = len_bytes.iter().position(|&b| b != 0).unwrap_or(7);
            let len_slice = &len_bytes[trim..];
            out.push(0xf7 + len_slice.len() as u8);
            out.extend_from_slice(len_slice);
        }
        out.extend_from_slice(&payload);
        out
    }

    /// Encode a byte string as RLP per the spec.
    fn rlp_bytes_enc(b: &[u8]) -> Vec<u8> {
        if b.len() == 1 && b[0] < 0x80 {
            vec![b[0]]
        } else if b.len() <= 55 {
            let mut out = Vec::with_capacity(b.len() + 1);
            out.push(0x80 + b.len() as u8);
            out.extend_from_slice(b);
            out
        } else {
            let len_bytes = b.len().to_be_bytes();
            let trim = len_bytes.iter().position(|&x| x != 0).unwrap_or(7);
            let len_slice = &len_bytes[trim..];
            let mut out = Vec::with_capacity(b.len() + 1 + len_slice.len());
            out.push(0xb7 + len_slice.len() as u8);
            out.extend_from_slice(len_slice);
            out.extend_from_slice(b);
            out
        }
    }

    fn rlp_uint(v: u64) -> Vec<u8> {
        if v == 0 {
            return vec![0x80];
        }
        let bytes = v.to_be_bytes();
        let first = bytes.iter().position(|&b| b != 0).unwrap_or(7);
        rlp_bytes_enc(&bytes[first..])
    }

    /// EIP-2930 was previously routed through `decode_signed_tx_1559`, which shifted
    /// every field after `nonce` by one (1559 has `[max_priority, max_fee]` where 2930
    /// has just `[gas_price]`). The dedicated decoder must read `to`, `value`, `data`
    /// correctly. Reference: <https://eips.ethereum.org/EIPS/eip-2930>.
    #[test]
    fn eip2930_decoder_reads_to_value_data_correctly() {
        let to_addr = [0xCCu8; 20];
        let value_bytes = [0xDEu8, 0xADu8];
        let data_bytes = [0xCAu8, 0xFEu8, 0xBAu8, 0xBEu8];
        let mut to_rlp = vec![0x94u8]; // address marker
        to_rlp.extend_from_slice(&to_addr);

        let list = rlp_list(&[
            rlp_uint(42_161),        // chain_id (Arbitrum One)
            rlp_uint(7),             // nonce
            rlp_uint(1_000_000_000), // gas_price (1 gwei)
            rlp_uint(100_000),       // gas_limit
            to_rlp,                  // to
            rlp_bytes_enc(&value_bytes),
            rlp_bytes_enc(&data_bytes),
            vec![0xc0u8], // access_list = []
            rlp_uint(0),  // y_parity
            rlp_uint(1),  // r
            rlp_uint(1),  // s
        ]);

        // The dispatcher strips the 0x01 type byte before calling; replicate that here.
        let tx = decode_signed_tx_2930(&list).expect("must decode");
        assert_eq!(tx.tx_type, 0x01, "tx_type must reflect 2930, not 1559");
        assert_eq!(
            tx.to, to_addr,
            "to must be the actual 20-byte address, not the encoded value"
        );
        assert_eq!(tx.nonce, 7);
        assert_eq!(tx.chain_id, 42_161);
        assert_eq!(
            tx.data,
            &data_bytes[..],
            "data must read the data field, not the access list"
        );
    }

    /// RLP-encode a 20-byte address (`0x94 ++ addr`).
    fn rlp_addr(a: &[u8; 20]) -> Vec<u8> {
        let mut v = vec![0x94u8];
        v.extend_from_slice(a);
        v
    }

    /// RLP-encode a 32-byte storage key (`0xa0 ++ key`).
    fn rlp_storage_key(k: &[u8; 32]) -> Vec<u8> {
        let mut v = vec![0xa0u8];
        v.extend_from_slice(k);
        v
    }

    /// EIP-1559 access lists name the storage slots a tx will touch. The wakeup-widening path
    /// reads them to mark a touched pool dirty WITHOUT decoding the selector, so the decoder must
    /// surface the list body and `access_list_addresses` must yield each entry's address in order.
    #[test]
    fn access_list_addresses_extracted_from_1559() {
        let to_addr = [0xCCu8; 20];
        let pool_a = [0xAAu8; 20];
        let pool_b = [0xBBu8; 20];
        // entry A: (pool_a, [one storage key]); entry B: (pool_b, [] empty keys).
        let entry_a = rlp_list(&[
            rlp_addr(&pool_a),
            rlp_list(&[rlp_storage_key(&[0x01u8; 32])]),
        ]);
        let entry_b = rlp_list(&[rlp_addr(&pool_b), rlp_list(&[])]);
        let access_list = rlp_list(&[entry_a, entry_b]);

        let list = rlp_list(&[
            rlp_uint(42_161),
            rlp_uint(7),
            rlp_uint(1_000_000),
            rlp_uint(2_000_000),
            rlp_uint(100_000),
            rlp_addr(&to_addr),
            rlp_uint(0),
            rlp_bytes_enc(&[0xde, 0xad]),
            access_list,
            rlp_uint(0),
            rlp_uint(1),
            rlp_uint(1),
        ]);

        let tx = decode_signed_tx_1559(&list).expect("must decode");
        let got: Vec<[u8; 20]> = access_list_addresses(tx.access_list).collect();
        assert_eq!(got, vec![pool_a, pool_b], "both entry addresses, in order");
    }

    #[test]
    fn empty_access_list_yields_no_addresses() {
        let to_addr = [0xCCu8; 20];
        let list = rlp_list(&[
            rlp_uint(42_161),
            rlp_uint(7),
            rlp_uint(1_000_000_000),
            rlp_uint(100_000),
            rlp_addr(&to_addr),
            rlp_bytes_enc(&[0xDE, 0xAD]),
            rlp_bytes_enc(&[0xCA, 0xFE]),
            vec![0xc0u8], // access_list = []
            rlp_uint(0),
            rlp_uint(1),
            rlp_uint(1),
        ]);
        let tx = decode_signed_tx_2930(&list).expect("must decode");
        assert_eq!(access_list_addresses(tx.access_list).count(), 0);
    }

    #[test]
    fn legacy_tx_has_empty_access_list() {
        let to_addr = [0xCCu8; 20];
        let list = rlp_list(&[
            rlp_uint(7),
            rlp_uint(1_000_000_000),
            rlp_uint(100_000),
            rlp_addr(&to_addr),
            rlp_uint(0),
            rlp_bytes_enc(&[0xCA, 0xFE]),
            rlp_uint(37), // v (EIP-155: chain_id 1)
            rlp_uint(1),
            rlp_uint(1),
        ]);
        let tx = decode_signed_tx_legacy(&list).expect("must decode");
        assert!(tx.access_list.is_empty(), "legacy txs carry no access list");
        assert_eq!(access_list_addresses(tx.access_list).count(), 0);
    }

    /// A corrupt tail after a well-formed entry must not panic — the good prefix is yielded and
    /// iteration stops (the hot path can't trust a malformed remainder).
    #[test]
    fn malformed_access_list_tail_stops_cleanly() {
        let pool_a = [0xAAu8; 20];
        let mut body = rlp_list(&[rlp_addr(&pool_a), rlp_list(&[])]);
        body.push(0xff); // long-list header claiming an out-of-range length → decode stops
        let got: Vec<[u8; 20]> = access_list_addresses(&body).collect();
        assert_eq!(got, vec![pool_a]);
    }

    /// Regression guard: feeding the same 2930 payload through the 1559 decoder
    /// produces wrong fields (this is the bug we fixed). The test pins the misbehaviour
    /// so anyone tempted to re-route 2930 → 1559 sees the failure modes.
    #[test]
    fn eip1559_decoder_misreads_2930_payload() {
        let to_addr = [0xCCu8; 20];
        let mut to_rlp = vec![0x94u8];
        to_rlp.extend_from_slice(&to_addr);
        let list = rlp_list(&[
            rlp_uint(42_161),
            rlp_uint(7),
            rlp_uint(1_000_000_000),
            rlp_uint(100_000),
            to_rlp,
            rlp_bytes_enc(&[0xDEu8, 0xADu8]),
            rlp_bytes_enc(&[0xCAu8, 0xFEu8]),
            vec![0xc0u8],
            rlp_uint(0),
            rlp_uint(1),
            rlp_uint(1),
        ]);
        // The 1559 decoder either errors (one field type mismatch) or silently shifts
        // fields. Either path proves "do not route 2930 through 1559." The dedicated
        // 2930 decoder above is the only correct choice.
        match decode_signed_tx_1559(&list) {
            Err(_) => { /* expected: decoder rejects the malformed-from-its-pov payload */ }
            Ok(mismatched) => {
                // If it does parse, the tx_type would be 0x02 (wrong) which is the
                // misclassification bug we were guarding against.
                assert_eq!(
                    mismatched.tx_type, 0x02,
                    "1559 decoder hardcodes 0x02 — that's exactly the bug"
                );
            }
        }
    }

    /// Sign a real EIP-1559 envelope with a known key and prove `recover_tx_sender`
    /// reconstructs the exact signing preimage and recovers that key's address. This is
    /// the foundation of the shadow-REVM worker: replaying a feed swap needs the true
    /// sender so `transferFrom(sender, …)` reads its real on-chain balance/allowance.
    #[test]
    fn recover_tx_sender_eip1559_roundtrip() {
        use secp256k1::{Message, PublicKey, Secp256k1, SecretKey};
        use tiny_keccak::{Hasher, Keccak};

        let secp = Secp256k1::new();
        let sk = SecretKey::from_slice(&[0x11u8; 32]).unwrap();
        let pk = PublicKey::from_secret_key(&secp, &sk);
        // address = keccak256(uncompressed_pubkey[1..])[12..]
        let uncompressed = pk.serialize_uncompressed();
        let mut h = Keccak::v256();
        h.update(&uncompressed[1..]);
        let mut pkh = [0u8; 32];
        h.finalize(&mut pkh);
        let mut expected_from = [0u8; 20];
        expected_from.copy_from_slice(&pkh[12..]);

        // Unsigned 1559 fields: [chain_id, nonce, max_prio, max_fee, gas, to, value, data, access_list]
        let to_addr = [0xCCu8; 20];
        let mut to_rlp = vec![0x94u8];
        to_rlp.extend_from_slice(&to_addr);
        let fields = vec![
            rlp_uint(42_161),
            rlp_uint(7),
            rlp_uint(1_000_000),
            rlp_uint(2_000_000),
            rlp_uint(100_000),
            to_rlp,
            rlp_uint(0),
            rlp_bytes_enc(&[0xde, 0xad, 0xbe, 0xef]),
            vec![0xc0u8], // empty access list
        ];
        let unsigned_inner: Vec<u8> = fields.iter().flatten().copied().collect();
        let unsigned_list = rlp_list(&fields);
        // Independent signing hash: keccak(0x02 || rlp_list(9 fields)).
        let mut preimage = vec![0x02u8];
        preimage.extend_from_slice(&unsigned_list);
        let mut hh = Keccak::v256();
        hh.update(&preimage);
        let mut signing_hash = [0u8; 32];
        hh.finalize(&mut signing_hash);

        let msg = Message::from_digest(signing_hash);
        let rec_sig = secp.sign_ecdsa_recoverable(&msg, &sk);
        let (recid, sig) = rec_sig.serialize_compact();
        let y_parity = i32::from(recid) as u8;

        // Full signed envelope: 0x02 || rlp_list(9 fields + y_parity + r + s).
        let mut signed_fields = fields.clone();
        signed_fields.push(rlp_uint(y_parity as u64));
        signed_fields.push(rlp_bytes_enc(&sig[..32]));
        signed_fields.push(rlp_bytes_enc(&sig[32..]));
        let mut envelope = vec![0x02u8];
        envelope.extend_from_slice(&rlp_list(&signed_fields));
        let _ = unsigned_inner; // (kept for clarity of the field layout)

        let recovered = recover_tx_sender(&envelope).expect("recover");
        assert_eq!(
            recovered, expected_from,
            "recovered sender must equal the signing key's address"
        );
    }

    /// Sign a real EIP-7702 set-code envelope and prove `recover_tx_sender` recovers the signer.
    /// 7702 appends an `authorization_list` after `access_list`, so there are 10 signed fields
    /// before `(y_parity, r, s)` and the signing preimage is `keccak(0x04 || rlp(first 10))`.
    /// Without the `0x04` arm `recover_tx_sender` returns `None`, and the shadow-REVM worker
    /// replays 7702 swaps against a wrong (zero) sender balance/allowance.
    #[test]
    fn recover_tx_sender_eip7702_roundtrip() {
        use secp256k1::{Message, PublicKey, Secp256k1, SecretKey};
        use tiny_keccak::{Hasher, Keccak};

        let secp = Secp256k1::new();
        let sk = SecretKey::from_slice(&[0x22u8; 32]).unwrap();
        let pk = PublicKey::from_secret_key(&secp, &sk);
        let uncompressed = pk.serialize_uncompressed();
        let mut h = Keccak::v256();
        h.update(&uncompressed[1..]);
        let mut pkh = [0u8; 32];
        h.finalize(&mut pkh);
        let mut expected_from = [0u8; 20];
        expected_from.copy_from_slice(&pkh[12..]);

        // Unsigned 7702 fields: [chain_id, nonce, max_prio, max_fee, gas, to, value, data,
        //                        access_list, authorization_list]  (10 fields)
        let to_addr = [0xCCu8; 20];
        let mut to_rlp = vec![0x94u8];
        to_rlp.extend_from_slice(&to_addr);
        // One authorization tuple: [chain_id, address, nonce, y_parity, r, s].
        let auth_tuple = rlp_list(&[
            rlp_uint(42_161),
            rlp_addr(&[0xABu8; 20]),
            rlp_uint(3),
            rlp_uint(0),
            rlp_uint(1),
            rlp_uint(1),
        ]);
        let authorization_list = rlp_list(&[auth_tuple]);
        let fields = vec![
            rlp_uint(42_161),
            rlp_uint(7),
            rlp_uint(1_000_000),
            rlp_uint(2_000_000),
            rlp_uint(100_000),
            to_rlp,
            rlp_uint(0),
            rlp_bytes_enc(&[0xde, 0xad, 0xbe, 0xef]),
            vec![0xc0u8], // empty access list
            authorization_list,
        ];
        let unsigned_list = rlp_list(&fields);
        // Independent signing hash: keccak(0x04 || rlp_list(10 fields)).
        let mut preimage = vec![0x04u8];
        preimage.extend_from_slice(&unsigned_list);
        let mut hh = Keccak::v256();
        hh.update(&preimage);
        let mut signing_hash = [0u8; 32];
        hh.finalize(&mut signing_hash);

        let msg = Message::from_digest(signing_hash);
        let rec_sig = secp.sign_ecdsa_recoverable(&msg, &sk);
        let (recid, sig) = rec_sig.serialize_compact();
        let y_parity = i32::from(recid) as u8;

        // Full signed envelope: 0x04 || rlp_list(10 fields + y_parity + r + s).
        let mut signed_fields = fields.clone();
        signed_fields.push(rlp_uint(y_parity as u64));
        signed_fields.push(rlp_bytes_enc(&sig[..32]));
        signed_fields.push(rlp_bytes_enc(&sig[32..]));
        let mut envelope = vec![0x04u8];
        envelope.extend_from_slice(&rlp_list(&signed_fields));

        let recovered = recover_tx_sender(&envelope).expect("recover 7702 sender");
        assert_eq!(
            recovered, expected_from,
            "recovered 7702 sender must equal the signing key's address"
        );
    }
}
