use engine_feed::Address;
use engine_rpc::{
    erc20_balance_slot, hydrate_v3_ticks_in_load_state, pack_v3_tick_liquidity_word,
    v3_tick_info_word0_slot, write_storage_word_in_load_state, V3TickSnapshot,
};
use serde_json::json;

fn hex32(bytes: [u8; 32]) -> String {
    let mut out = String::with_capacity(66);
    out.push_str("0x");
    out.push_str(&faster_hex::hex_string(&bytes));
    out
}

fn word_from_hex(s: &str) -> [u8; 32] {
    let s = s.strip_prefix("0x").unwrap_or(s);
    let mut out = [0u8; 32];
    faster_hex::hex_decode(s.as_bytes(), &mut out).unwrap();
    out
}

#[test]
fn hydrate_v3_ticks_writes_word0_slots_into_existing_pool_storage() {
    let pool: Address = [0x11; 20];
    let mut state = json!({
        "accounts": {
            "0x1111111111111111111111111111111111111111": {
                "nonce": 1,
                "balance": "0x0",
                "code": "0x6000",
                "storage": {
                    "0x0": "0x1234"
                }
            }
        }
    });
    let ticks = vec![
        V3TickSnapshot {
            tick: 266_270,
            liquidity_gross: 0x1122,
            liquidity_net: 0x3344,
        },
        V3TickSnapshot {
            tick: -10,
            liquidity_gross: 1,
            liquidity_net: -2,
        },
    ];

    let written = hydrate_v3_ticks_in_load_state(&mut state, &pool, 5, &ticks).unwrap();

    assert_eq!(written, 2);
    let storage = &state["accounts"]["0x1111111111111111111111111111111111111111"]["storage"];
    assert_eq!(storage["0x0"], "0x1234");
    assert_eq!(
        storage[hex32(v3_tick_info_word0_slot(266_270, 5)).as_str()],
        hex32(pack_v3_tick_liquidity_word(0x1122, 0x3344))
    );
    assert_eq!(
        storage[hex32(v3_tick_info_word0_slot(-10, 5)).as_str()],
        hex32(pack_v3_tick_liquidity_word(1, -2))
    );
}

#[test]
fn hydrate_v3_ticks_fails_if_pool_account_is_missing() {
    let pool: Address = [0x22; 20];
    let mut state = json!({ "accounts": {} });
    let ticks = [V3TickSnapshot {
        tick: 0,
        liquidity_gross: 1,
        liquidity_net: 1,
    }];

    let err = hydrate_v3_ticks_in_load_state(&mut state, &pool, 5, &ticks).unwrap_err();

    assert!(err.to_string().contains("missing account"));
}

#[test]
fn write_storage_word_updates_existing_account_storage() {
    let account: Address = [0xaa; 20];
    let slot = [0x11; 32];
    let word = [0x22; 32];
    let mut state = json!({
        "accounts": {
            "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa": {
                "nonce": 1,
                "balance": "0x0",
                "code": "0x6000",
                "storage": {
                    "0x0": "0x1234"
                }
            }
        }
    });

    write_storage_word_in_load_state(&mut state, &account, slot, word).unwrap();

    let storage = &state["accounts"]["0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"]["storage"];
    assert_eq!(storage["0x0"], "0x1234");
    assert_eq!(storage[hex32(slot).as_str()], hex32(word));
}

#[test]
fn erc20_balance_slot_matches_wbtc_balancer_vault_cast_vector() {
    let vault: Address = [
        0xba, 0x12, 0x22, 0x22, 0x22, 0x22, 0x8d, 0x8b, 0xa4, 0x45, 0x95, 0x8a, 0x75, 0xa0, 0x70,
        0x4d, 0x56, 0x6b, 0xf2, 0xc8,
    ];

    assert_eq!(
        erc20_balance_slot(&vault, 51),
        word_from_hex("0x8e136277087394ae84bd88b3538db688c3763bf01dc7ed78e5235dc311d5959c")
    );
}
