//! Helpers for editing Anvil `--dump-state` / `--load-state` JSON.

use anyhow::{anyhow, Context, Result};
use engine_feed::Address;
use serde_json::{Map, Value};
use tiny_keccak::{Hasher, Keccak};

use crate::{pack_v3_tick_liquidity_word, v3_tick_info_word0_slot, V3TickSnapshot};

/// Write one 32-byte storage word into an existing account in an Anvil load-state JSON value.
///
/// Missing `storage` is created, but the account itself must already exist.
pub fn write_storage_word_in_load_state(
    state: &mut Value,
    account: &Address,
    slot: [u8; 32],
    word: [u8; 32],
) -> Result<()> {
    let storage = storage_object_mut(state, account)?;
    storage.insert(format_word(slot), Value::String(format_word(word)));
    Ok(())
}

/// Return the Solidity storage key for `mapping(address => uint256)` ERC20 balances.
pub fn erc20_balance_slot(owner: &Address, mapping_slot: u64) -> [u8; 32] {
    let mut preimage = [0u8; 64];
    preimage[12..32].copy_from_slice(owner);
    preimage[56..64].copy_from_slice(&mapping_slot.to_be_bytes());

    let mut hasher = Keccak::v256();
    hasher.update(&preimage);
    let mut out = [0u8; 32];
    hasher.finalize(&mut out);
    out
}

/// Insert canonical V3 `ticks` word0 storage entries into an Anvil load-state JSON value.
///
/// The pool account must already exist in `state["accounts"]`. Missing `storage`
/// is created, but non-object storage is rejected because Anvil expects a slot map.
pub fn hydrate_v3_ticks_in_load_state(
    state: &mut Value,
    pool: &Address,
    mapping_slot: u64,
    ticks: &[V3TickSnapshot],
) -> Result<usize> {
    let storage_obj = storage_object_mut(state, pool)?;

    for tick in ticks {
        let slot = v3_tick_info_word0_slot(tick.tick, mapping_slot);
        let word = pack_v3_tick_liquidity_word(tick.liquidity_gross, tick.liquidity_net);
        storage_obj.insert(format_word(slot), Value::String(format_word(word)));
    }
    Ok(ticks.len())
}

fn storage_object_mut<'a>(
    state: &'a mut Value,
    account: &Address,
) -> Result<&'a mut Map<String, Value>> {
    let account_key = format_address(account);
    let accounts = state
        .get_mut("accounts")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| anyhow!("load-state JSON missing accounts object"))?;
    let account = accounts
        .get_mut(&account_key)
        .with_context(|| format!("missing account {account_key} in load-state"))?;
    let account_obj = account
        .as_object_mut()
        .with_context(|| format!("account {account_key} is not an object"))?;
    let storage = account_obj
        .entry("storage")
        .or_insert_with(|| Value::Object(Map::new()));
    let storage_obj = storage
        .as_object_mut()
        .with_context(|| format!("account {account_key} storage is not an object"))?;
    Ok(storage_obj)
}

fn format_address(address: &Address) -> String {
    format!("0x{}", faster_hex::hex_string(address))
}

fn format_word(word: [u8; 32]) -> String {
    format!("0x{}", faster_hex::hex_string(&word))
}
