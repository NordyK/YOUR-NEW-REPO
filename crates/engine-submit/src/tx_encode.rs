//! EIP-1559 transaction RLP encoder.
//!
//! Produces the unsigned and signed RLP encodings used by `eth_sendRawTransaction`. The
//! engine uses EIP-1559 exclusively (the sequencer accepts both, but priority fees do not
//! affect FCFS ordering — §10.11).

use alloy_primitives::U256;

/// EIP-1559 transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Eip1559Tx {
    /// Chain id (Arbitrum One: 42161).
    pub chain_id: u64,
    /// Nonce.
    pub nonce: u64,
    /// max_priority_fee_per_gas (FCFS — always 0).
    pub max_priority_fee_per_gas: U256,
    /// max_fee_per_gas (floored at the current ArbOS min base fee).
    pub max_fee_per_gas: U256,
    /// Gas limit.
    pub gas_limit: u64,
    /// To address.
    pub to: [u8; 20],
    /// Value.
    pub value: U256,
    /// Calldata.
    pub data: Vec<u8>,
}

/// Encode the unsigned EIP-1559 payload (`0x02 || rlp([chain_id, nonce, mpfg, mfg,
/// gas_limit, to, value, data, []])`).
pub fn encode_eip1559_unsigned(tx: &Eip1559Tx) -> Vec<u8> {
    let mut out = Vec::with_capacity(96 + tx.data.len());
    out.push(0x02);
    let payload = encode_payload(tx, None);
    encode_list_header(&payload, &mut out);
    out.extend_from_slice(&payload);
    out
}

/// Encode the signed EIP-1559 payload (`0x02 || rlp([..., y_parity, r, s])`).
pub fn encode_eip1559_signed(tx: &Eip1559Tx, sig: ([u8; 32], [u8; 32], u8)) -> Vec<u8> {
    let mut out = Vec::with_capacity(128 + tx.data.len());
    out.push(0x02);
    let payload = encode_payload(tx, Some(sig));
    encode_list_header(&payload, &mut out);
    out.extend_from_slice(&payload);
    out
}

fn encode_payload(tx: &Eip1559Tx, sig: Option<([u8; 32], [u8; 32], u8)>) -> Vec<u8> {
    let mut buf = Vec::with_capacity(96);
    encode_u64(tx.chain_id, &mut buf);
    encode_u64(tx.nonce, &mut buf);
    encode_u256(tx.max_priority_fee_per_gas, &mut buf);
    encode_u256(tx.max_fee_per_gas, &mut buf);
    encode_u64(tx.gas_limit, &mut buf);
    encode_address(&tx.to, &mut buf);
    encode_u256(tx.value, &mut buf);
    encode_bytes(&tx.data, &mut buf);
    // empty access list.
    buf.push(0xc0);
    if let Some((r, s, y_parity)) = sig {
        encode_u64(y_parity as u64, &mut buf);
        encode_u256_bytes(&r, &mut buf);
        encode_u256_bytes(&s, &mut buf);
    }
    buf
}

fn encode_u64(v: u64, out: &mut Vec<u8>) {
    if v == 0 {
        out.push(0x80);
        return;
    }
    let bytes = v.to_be_bytes();
    let lead = bytes
        .iter()
        .position(|&b| b != 0)
        .unwrap_or(bytes.len() - 1);
    encode_bytes(&bytes[lead..], out);
}

fn encode_u256(v: U256, out: &mut Vec<u8>) {
    if v.is_zero() {
        out.push(0x80);
        return;
    }
    let bytes = v.to_be_bytes::<32>();
    let lead = bytes
        .iter()
        .position(|&b| b != 0)
        .unwrap_or(bytes.len() - 1);
    encode_bytes(&bytes[lead..], out);
}

fn encode_u256_bytes(b: &[u8; 32], out: &mut Vec<u8>) {
    let lead = b.iter().position(|&x| x != 0).unwrap_or(b.len() - 1);
    encode_bytes(&b[lead..], out);
}

fn encode_address(a: &[u8; 20], out: &mut Vec<u8>) {
    encode_bytes(a, out);
}

fn encode_bytes(b: &[u8], out: &mut Vec<u8>) {
    if b.len() == 1 && b[0] < 0x80 {
        out.push(b[0]);
        return;
    }
    if b.len() < 56 {
        out.push(0x80 + b.len() as u8);
        out.extend_from_slice(b);
        return;
    }
    let len_bytes = (b.len() as u64).to_be_bytes();
    let lead = len_bytes.iter().position(|&x| x != 0).unwrap_or(7);
    out.push(0xb7 + (8 - lead) as u8);
    out.extend_from_slice(&len_bytes[lead..]);
    out.extend_from_slice(b);
}

fn encode_list_header(payload: &[u8], out: &mut Vec<u8>) {
    let len = payload.len();
    if len < 56 {
        out.push(0xc0 + len as u8);
        return;
    }
    let len_bytes = (len as u64).to_be_bytes();
    let lead = len_bytes.iter().position(|&x| x != 0).unwrap_or(7);
    out.push(0xf7 + (8 - lead) as u8);
    out.extend_from_slice(&len_bytes[lead..]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_data_encodes_minimal() {
        let tx = Eip1559Tx {
            chain_id: 42_161,
            nonce: 0,
            max_priority_fee_per_gas: U256::ZERO,
            max_fee_per_gas: U256::from(20_000_000u64),
            gas_limit: 21_000,
            to: [0u8; 20],
            value: U256::ZERO,
            data: vec![],
        };
        let unsigned = encode_eip1559_unsigned(&tx);
        assert_eq!(unsigned[0], 0x02);
    }
}
