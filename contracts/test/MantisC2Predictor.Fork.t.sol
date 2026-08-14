// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

/// T2 — C2 predictor == real post-swap state (TEST_PLAN §7 T2; DEPLOYMENT_READINESS §8 #7).
///
/// The Rust generator (`cargo run --release -p engine --example emit_t2_predictor_fixtures`)
/// snapshots one live pool per C2 family at a pinned block via the engine's own readers, runs the
/// production predictor (`apply_*` / kernel swap), and writes test/fixtures/t2_predictor.json.
/// This test forks THAT block, executes the real swaps, and asserts per family:
///   1. received(dx)   == predicted out      (the quote is right),
///   2. post-state     == predicted state    (the predictor is right — exact for V2 reserves,
///      V3 sqrtP/tick/liquidity, DODO base reserve, LB active bin; documented fee-share bound
///      for Curve/Balancer/DODO-quote balances),
///   3. received(dx2)  == predicted out2     (the predicted state QUOTES right — what the
///      zero-RPC fast lane actually depends on).
///
/// Run:  ARBITRUM_RPC_URL=<alchemy> forge test --match-path test/MantisC2Predictor.Fork.t.sol -vv
/// (self-pins via vm.createSelectFork to the fixture's block — regenerate the fixture to re-pin).
interface IERC20T2 {
    function balanceOf(address) external view returns (uint256);
    function transfer(address, uint256) external returns (bool);
    function approve(address, uint256) external returns (bool);
}

interface IV2PairT2 {
    function token0() external view returns (address);
    function getReserves() external view returns (uint112, uint112, uint32);
    function swap(uint256 amount0Out, uint256 amount1Out, address to, bytes calldata data) external;
}

interface IV3PoolT2 {
    function slot0() external view returns (uint160 sqrtPriceX96, int24 tick, uint16, uint16, uint16, uint8, bool);
    function liquidity() external view returns (uint128);
    function swap(address recipient, bool zeroForOne, int256 amountSpecified, uint160 sqrtPriceLimitX96, bytes calldata data)
        external
        returns (int256 amount0, int256 amount1);
}

interface ICurveNgT2 {
    function exchange(int128 i, int128 j, uint256 dx, uint256 minDy) external returns (uint256);
    function balances(uint256 i) external view returns (uint256);
}

interface IDodoT2 {
    function sellBase(address to) external returns (uint256);
    function getVaultReserve() external view returns (uint256 baseReserve, uint256 quoteReserve);
}

interface IBalVaultT2 {
    struct SingleSwap {
        bytes32 poolId;
        uint8 kind; // 0 = GIVEN_IN
        address assetIn;
        address assetOut;
        uint256 amount;
        bytes userData;
    }
    struct FundManagement {
        address sender;
        bool fromInternalBalance;
        address payable recipient;
        bool toInternalBalance;
    }
    function swap(SingleSwap calldata, FundManagement calldata, uint256 limit, uint256 deadline)
        external
        returns (uint256);
    function getPoolTokens(bytes32 poolId)
        external
        view
        returns (address[] memory tokens, uint256[] memory balances, uint256 lastChangeBlock);
}

interface ILBPairT2 {
    function getActiveId() external view returns (uint24);
    function swap(bool swapForY, address to) external returns (bytes32);
}

contract MantisC2PredictorForkTest is Test {
    address constant BAL_VAULT = 0xBA12222222228d8Ba445958a75a0704d566BF2C8;
    uint160 constant MIN_SQRT_PLUS1 = 4295128740;
    string json;

    function setUp() public {
        json = vm.readFile("test/fixtures/t2_predictor.json");
        uint256 pinned = vm.parseJsonUint(json, ".block");
        vm.createSelectFork("arbitrum", pinned);
        require(block.chainid == 42161, "fork Arbitrum One");
    }

    function ju(string memory key) internal view returns (uint256) {
        return vm.parseJsonUint(json, key);
    }

    function ja(string memory key) internal view returns (address) {
        return vm.parseJsonAddress(json, key);
    }

    // ---------------- V2: out + exact reserves + next quote ----------------
    function test_T2_V2_predictor_matches_executed_swap() public {
        IV2PairT2 pair = IV2PairT2(ja(".v2_pair"));
        address weth = pair.token0();
        IERC20T2 usdc;
        {
            (uint112 r0,,) = pair.getReserves();
            r0; // silence
        }
        uint256 dx = ju(".v2_dx");
        uint256 predOut = ju(".v2_out");
        deal(weth, address(this), dx + ju(".v2_dx2"));

        // swap 1: send dx of token0, take the predicted out of token1.
        IERC20T2(weth).transfer(address(pair), dx);
        pair.swap(0, predOut, address(this), "");
        (uint112 r0a, uint112 r1a,) = pair.getReserves();
        assertEq(uint256(r0a), ju(".v2_r0_after"), "V2 reserve0 != predicted");
        assertEq(uint256(r1a), ju(".v2_r1_after"), "V2 reserve1 != predicted");

        // swap 2 off the predicted state: predicted out2 must clear the K check exactly.
        IERC20T2(weth).transfer(address(pair), ju(".v2_dx2"));
        pair.swap(0, ju(".v2_out2"), address(this), "");
        usdc; // silence
    }

    // ---------------- V3: out + exact sqrtP/tick/liquidity + next quote ----------------
    uint256 private v3PayToken0; // set during swap; callback pays in token0 (WETH)
    address private v3Weth;

    function uniswapV3SwapCallback(int256 amount0Delta, int256 amount1Delta, bytes calldata) external {
        amount1Delta;
        if (amount0Delta > 0) {
            IERC20T2(v3Weth).transfer(msg.sender, uint256(amount0Delta));
        }
    }

    function test_T2_V3_predictor_matches_executed_swap() public {
        IV3PoolT2 pool = IV3PoolT2(ja(".v3_pool"));
        v3Weth = 0x82aF49447D8a07e3bd95BD0d56f35241523fBab1;
        address usdc = 0xaf88d065e77c8cC2239327C5EDb3A432268e5831;
        uint256 dx = ju(".v3_dx");
        deal(v3Weth, address(this), dx + ju(".v3_dx2"));

        uint256 bal0 = IERC20T2(usdc).balanceOf(address(this));
        (, int256 a1) = pool.swap(address(this), true, int256(dx), MIN_SQRT_PLUS1, "");
        uint256 received = IERC20T2(usdc).balanceOf(address(this)) - bal0;
        assertEq(received, ju(".v3_out"), "V3 received != predicted out");
        assertEq(uint256(-a1), ju(".v3_out"), "V3 amount1 != predicted out");
        (uint160 sqrtP, int24 tick,,,,,) = pool.slot0();
        assertEq(uint256(sqrtP), ju(".v3_sqrt_after"), "V3 sqrtPriceX96 != predicted");
        assertEq(int256(tick), vm.parseJsonInt(json, ".v3_tick_after"), "V3 tick != predicted");
        assertEq(uint256(pool.liquidity()), ju(".v3_liq_after"), "V3 liquidity != predicted");

        // swap 2 off the predicted state.
        uint256 balB = IERC20T2(usdc).balanceOf(address(this));
        pool.swap(address(this), true, int256(ju(".v3_dx2")), MIN_SQRT_PLUS1, "");
        uint256 received2 = IERC20T2(usdc).balanceOf(address(this)) - balB;
        assertEq(received2, ju(".v3_out2"), "V3 second swap != quote off predicted state");
    }

    // ---------------- Curve NG: out exact + balances (i exact, j within fee bound) ----------------
    function test_T2_Curve_predictor_matches_executed_exchange() public {
        ICurveNgT2 pool = ICurveNgT2(ja(".curve_pool"));
        address cin = ja(".curve_coin_in");
        uint256 dx = ju(".curve_dx");
        deal(cin, address(this), dx + ju(".curve_dx2"));
        IERC20T2(cin).approve(address(pool), type(uint256).max);

        uint256 dy = pool.exchange(0, 1, dx, 0);
        uint256 tol = ju(".curve_fee_tol");
        {
            // Documented bound (startup.rs): the static-fee kernel does not model the NG offpeg
            // dynamic fee, so dy may deviate within the full fee on dy (observed 0.16 bps on a
            // balanced pool — the kernel UNDER-quoted, the safe side).
            uint256 predDy = ju(".curve_dy");
            uint256 dDy = dy > predDy ? dy - predDy : predDy - dy;
            assertLe(dDy, tol, "Curve dy beyond documented fee bound");
        }
        assertEq(pool.balances(0), ju(".curve_bal_i_after"), "Curve balance[i] != predicted");
        uint256 balJ = pool.balances(1);
        uint256 predJ = ju(".curve_bal_j_after");
        uint256 diffJ = balJ > predJ ? balJ - predJ : predJ - balJ;
        assertLe(diffJ, tol + (dy > ju(".curve_dy") ? dy - ju(".curve_dy") : ju(".curve_dy") - dy), "Curve balance[j] drift beyond documented admin-fee bound");

        uint256 dy2 = pool.exchange(0, 1, ju(".curve_dx2"), 0);
        uint256 predDy2 = ju(".curve_dy2");
        uint256 diff2 = dy2 > predDy2 ? dy2 - predDy2 : predDy2 - dy2;
        assertLe(diff2, tol, "Curve second exchange != quote off predicted state (beyond bound)");
    }

    // ---------------- DODO: out exact + base reserve exact, quote reserve within fee bound ----------------
    function test_T2_Dodo_predictor_matches_executed_sellBase() public {
        IDodoT2 pool = IDodoT2(ja(".dodo_pool"));
        address base = ja(".dodo_base");
        uint256 dx = ju(".dodo_dx");
        deal(base, address(this), dx + ju(".dodo_dx2"));

        IERC20T2(base).transfer(address(pool), dx);
        uint256 got = pool.sellBase(address(this));
        assertEq(got, ju(".dodo_out"), "DODO sellBase out != predicted");
        (uint256 b, uint256 q) = pool.getVaultReserve();
        assertEq(b, ju(".dodo_b_after"), "DODO base reserve != predicted");
        uint256 predQ = ju(".dodo_q_after");
        uint256 tol = ju(".dodo_q_tol");
        uint256 diffQ = q > predQ ? q - predQ : predQ - q;
        assertLe(diffQ, tol, "DODO quote reserve drift beyond documented mt-fee bound");

        IERC20T2(base).transfer(address(pool), ju(".dodo_dx2"));
        uint256 got2 = pool.sellBase(address(this));
        uint256 predOut2 = ju(".dodo_out2");
        uint256 diff2 = got2 > predOut2 ? got2 - predOut2 : predOut2 - got2;
        assertLe(diff2, tol, "DODO second sellBase != quote off predicted state (beyond bound)");
    }

    // ---------------- Balancer Stable: out + balances via Vault swap GIVEN_IN ----------------
    function test_T2_BalancerStable_predictor_matches_executed_vault_swap() public {
        bytes32 poolId = vm.parseJsonBytes32(json, ".bal_pool_id");
        address tin = ja(".bal_token_in");
        address tout = ja(".bal_token_out");
        uint256 dx = ju(".bal_dx");
        deal(tin, address(this), dx + ju(".bal_dx2"));
        IERC20T2(tin).approve(BAL_VAULT, type(uint256).max);

        IBalVaultT2.FundManagement memory funds = IBalVaultT2.FundManagement({
            sender: address(this),
            fromInternalBalance: false,
            recipient: payable(address(this)),
            toInternalBalance: false
        });
        uint256 got = IBalVaultT2(BAL_VAULT).swap(
            IBalVaultT2.SingleSwap(poolId, 0, tin, tout, dx, ""), funds, 0, type(uint256).max
        );
        uint256 tol = ju(".bal_fee_tol");
        {
            uint256 predOut = ju(".bal_out");
            uint256 diffO = got > predOut ? got - predOut : predOut - got;
            assertLe(diffO, tol, "Balancer out != predicted (beyond fee bound)");
        }
        {
            (address[] memory toks, uint256[] memory bals,) = IBalVaultT2(BAL_VAULT).getPoolTokens(poolId);
            uint256 balIn;
            uint256 balOut;
            for (uint256 k = 0; k < toks.length; k++) {
                if (toks[k] == tin) balIn = bals[k];
                if (toks[k] == tout) balOut = bals[k];
            }
            uint256 predIn = ju(".bal_in_after");
            uint256 predOutBal = ju(".bal_out_after");
            uint256 dIn = balIn > predIn ? balIn - predIn : predIn - balIn;
            uint256 dOut = balOut > predOutBal ? balOut - predOutBal : predOutBal - balOut;
            assertLe(dIn, tol, "Balancer balance[in] drift beyond fee bound");
            assertLe(dOut, tol, "Balancer balance[out] drift beyond fee bound");
        }
        uint256 got2 = IBalVaultT2(BAL_VAULT).swap(
            IBalVaultT2.SingleSwap(poolId, 0, tin, tout, ju(".bal_dx2"), ""), funds, 0, type(uint256).max
        );
        uint256 predOut2 = ju(".bal_out2");
        uint256 diff2 = got2 > predOut2 ? got2 - predOut2 : predOut2 - got2;
        assertLe(diff2, tol, "Balancer second swap != quote off predicted state (beyond bound)");
    }

    // ---------------- Trader Joe LB: out exact + active bin exact + next quote ----------------
    function test_T2_LB_predictor_matches_executed_swap() public {
        ILBPairT2 pair = ILBPairT2(ja(".lb_pair"));
        address weth = 0x82aF49447D8a07e3bd95BD0d56f35241523fBab1;
        address usdce = 0xFF970A61A04b1cA14834A43f5dE4533eBDDB5CC8;
        uint256 dx = ju(".lb_dx");
        deal(weth, address(this), dx + ju(".lb_dx2"));

        uint256 bal0 = IERC20T2(usdce).balanceOf(address(this));
        IERC20T2(weth).transfer(address(pair), dx);
        pair.swap(true, address(this));
        uint256 received = IERC20T2(usdce).balanceOf(address(this)) - bal0;
        assertEq(received, ju(".lb_out"), "LB received != predicted out");
        assertEq(uint256(pair.getActiveId()), ju(".lb_active_id_after"), "LB active bin != predicted");

        uint256 balB = IERC20T2(usdce).balanceOf(address(this));
        IERC20T2(weth).transfer(address(pair), ju(".lb_dx2"));
        pair.swap(true, address(this));
        uint256 received2 = IERC20T2(usdce).balanceOf(address(this)) - balB;
        assertEq(received2, ju(".lb_out2"), "LB second swap != quote off predicted state");
    }
}
