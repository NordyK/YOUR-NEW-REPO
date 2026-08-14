use engine_rpc::{pack_v3_tick_liquidity_word, v3_tick_info_word0_slot};

fn hex32(s: &str) -> [u8; 32] {
    let s = s.strip_prefix("0x").unwrap_or(s);
    assert_eq!(s.len(), 64);
    let mut out = [0u8; 32];
    faster_hex::hex_decode(s.as_bytes(), &mut out).unwrap();
    out
}

#[test]
fn v3_tick_info_slot_matches_cast_index_for_positive_tick() {
    assert_eq!(
        v3_tick_info_word0_slot(266_270, 5),
        hex32("0x074f6b5509fb6b3da16b1fa5c8ff7ff187951012db709cf129ba741983ab3c6a")
    );
}

#[test]
fn v3_tick_info_slot_matches_cast_index_for_negative_tick() {
    assert_eq!(
        v3_tick_info_word0_slot(-10, 5),
        hex32("0x13d13548d6c2405b9165b5fc0c870cd4277990da2075be966f959be49eb41bb2")
    );
}

#[test]
fn v3_tick_liquidity_word_packs_gross_low_and_net_high() {
    assert_eq!(
        pack_v3_tick_liquidity_word(1, -2),
        hex32("0xfffffffffffffffffffffffffffffffe00000000000000000000000000000001")
    );
}

#[test]
fn v3_tick_liquidity_word_packs_positive_net() {
    assert_eq!(
        pack_v3_tick_liquidity_word(0x1122, 0x3344),
        hex32("0x0000000000000000000000000000334400000000000000000000000000001122")
    );
}
