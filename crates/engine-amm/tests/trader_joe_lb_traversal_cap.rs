use alloy_primitives::U256;
use engine_amm::trader_joe_lb::{
    simulate_lb_swap, simulate_lb_swap_detailed, BinData, LBState, MAX_TRAVERSAL_BINS,
};
use std::{collections::BTreeMap, sync::Arc};

const ACTIVE_ID: u32 = 8_388_608;
const BIN_LIQUIDITY: u128 = 1_000;

fn state_with_directional_bins(zero_for_one: bool, directional_bins: u32) -> LBState {
    let mut bin_map = BTreeMap::new();
    for offset in 1..=directional_bins {
        let id = if zero_for_one {
            ACTIVE_ID - offset
        } else {
            ACTIVE_ID + offset
        };
        let bin = if zero_for_one {
            BinData {
                reserve_x: 0,
                reserve_y: BIN_LIQUIDITY,
            }
        } else {
            BinData {
                reserve_x: BIN_LIQUIDITY,
                reserve_y: 0,
            }
        };
        bin_map.insert(id, bin);
    }

    LBState {
        active_id: ACTIVE_ID,
        bin_step: 0,
        reserve_x: if zero_for_one { 0 } else { BIN_LIQUIDITY },
        reserve_y: if zero_for_one { BIN_LIQUIDITY } else { 0 },
        base_factor: 0,
        filter_period: 0,
        decay_period: 0,
        reduction_factor: 0,
        variable_fee_control: 0,
        protocol_share: 0,
        max_volatility_accumulator: 0,
        volatility_accumulator: 0,
        volatility_reference: 0,
        id_reference: ACTIVE_ID,
        time_of_last_update: 0,
        block_number: 0,
        log_index: 0,
        stale: false,
        bin_map: Arc::new(bin_map),
    }
}

#[test]
fn trader_joe_lb_accepts_swap_that_fits_inside_traversal_cap() {
    for zero_for_one in [true, false] {
        let state = state_with_directional_bins(zero_for_one, MAX_TRAVERSAL_BINS);
        let amount_that_fits =
            U256::from((MAX_TRAVERSAL_BINS as u128).saturating_mul(BIN_LIQUIDITY));

        let detailed = simulate_lb_swap_detailed(&state, amount_that_fits, zero_for_one)
            .expect("detailed simulation should produce output");
        assert_eq!(detailed.remaining_in, 0);
        assert_eq!(detailed.amount_out, amount_that_fits);
        assert_eq!(detailed.traversed_bins.len(), MAX_TRAVERSAL_BINS as usize);

        let public_quote = simulate_lb_swap(&state, amount_that_fits, zero_for_one)
            .expect("public quote should accept complete fills inside the cap");
        assert_eq!(public_quote.0, amount_that_fits);
        assert_eq!(public_quote.1, detailed.final_active_id);
    }
}

#[test]
fn trader_joe_lb_rejects_cap_exhausted_partial_fill_instead_of_overquoting() {
    for zero_for_one in [true, false] {
        let state = state_with_directional_bins(zero_for_one, MAX_TRAVERSAL_BINS + 1);
        let amount_requiring_one_more_bin =
            U256::from(((MAX_TRAVERSAL_BINS + 1) as u128).saturating_mul(BIN_LIQUIDITY));

        let detailed =
            simulate_lb_swap_detailed(&state, amount_requiring_one_more_bin, zero_for_one)
                .expect("detailed simulation should expose cap-exhausted partial fill");
        assert_eq!(detailed.traversed_bins.len(), MAX_TRAVERSAL_BINS as usize);
        assert_eq!(detailed.remaining_in, BIN_LIQUIDITY);
        assert_eq!(
            detailed.amount_out,
            U256::from((MAX_TRAVERSAL_BINS as u128).saturating_mul(BIN_LIQUIDITY))
        );

        let next_id = if zero_for_one {
            detailed.final_active_id - 1
        } else {
            detailed.final_active_id + 1
        };
        let next_bin = state
            .bin_map
            .get(&next_id)
            .expect("test state intentionally includes one more liquid bin after the cap");
        let output_reserve_after_cap = if zero_for_one {
            next_bin.reserve_y
        } else {
            next_bin.reserve_x
        };
        assert_eq!(output_reserve_after_cap, BIN_LIQUIDITY);

        assert!(
            simulate_lb_swap(&state, amount_requiring_one_more_bin, zero_for_one).is_none(),
            "public LB quote must reject cap-exhausted partial fills instead of accepting an over-optimistic quote"
        );
    }
}
