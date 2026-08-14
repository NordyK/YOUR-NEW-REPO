// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/MantisExecutor.sol";

/// GATE 3 — exact pre-transaction state forced-route proof + brute-force sizing oracle.
///
/// Known historical arb (Dune): block 465686729, tx 0xbd25...:
///   Pancake V3 WETH -> WBTC (fee 100), then Uniswap V3 WBTC -> WETH (fee 500).
///   Dune: bot sold 28.0739 WETH, bought 0.768 WBTC, sold back for 28.0909 WETH.
///   Gross WETH delta ~0.016937 WETH.
///
/// CORRECT STATE POINT (runbook ISSUE-002): you must fork the PARENT block 465686728, not
/// 465686729. Forking at 465686729 gives POST-arb state (the opportunity was already consumed
/// inside the block) and shows zero profit. Forking at the parent gives the pre-arb state.
/// Same-block receipts (Codex) confirm txs 0..3 in 465686729 do not touch either target pool,
/// so the parent-block state equals the exact pre-target-tx state for these two pools.
///
/// This harness fixes the previous bug where probe sizes were executed sequentially WITHOUT
/// reverting state — each probe consumed the dislocation, corrupting the oracle and making the
/// final re-execution revert with Loss. Here every probe runs against a pristine snapshot.
///
/// Run (CORRECT pre-state):
///   forge test --fork-url $ARBITRUM_RPC_URL --fork-block-number 465686728 \
///     --match-path test/MantisGate3PreState.Fork.t.sol -vv
interface IWETHg { function balanceOf(address) external view returns (uint256); }

contract MantisGate3PreStateForkTest is Test {
    MantisExecutor executor;
    address constant WETH = 0x82aF49447D8a07e3bd95BD0d56f35241523fBab1;
    address constant WBTC = 0x2f2a2543B76A4166549F7aaB2e75Bef0aefC5B0f;
    uint8 constant UNIV3 = 0;
    uint8 constant PANCAKE_V3 = 6;
    error Loss(uint256);

    function setUp() public {
        require(block.chainid == 42161, "fork Arbitrum One");
        executor = new MantisExecutor(address(this)); // payee = this -> realized profit lands here
    }

    function _v3(uint8 ex, address tin, address tout, uint24 fee)
        internal pure returns (MantisExecutor.TradeHopV2 memory h)
    { h.exchangeId = ex; h.tokenIn = tin; h.tokenOut = tout; h.fee = fee; }

    /// Probe one input size against a PRISTINE snapshot; returns realized WETH profit (0 on revert/loss).
    function _probe(MantisExecutor.TradeHopV2[3] memory route, uint128 size) internal returns (uint256 profit) {
        uint256 snap = vm.snapshotState();
        uint256 bal0 = IWETHg(WETH).balanceOf(address(this));
        try executor.flashLoanArbMixed(size, route, 0) {
            profit = IWETHg(WETH).balanceOf(address(this)) - bal0;
        } catch {
            profit = 0;
        }
        vm.revertToState(snap);
    }

    function test_gate3_prestate_forced_route_and_oracle() public {
        // Real arb direction: WETH->WBTC on Pancake(100), WBTC->WETH on Uniswap(500).
        MantisExecutor.TradeHopV2[3] memory route;
        route[0] = _v3(PANCAKE_V3, WETH, WBTC, 100);
        route[1] = _v3(UNIV3, WBTC, WETH, 500);
        // route[2] inactive -> 2-hop cycle.

        // --- BRUTE-FORCE SIZING ORACLE: each probe sees identical pristine pre-state ---
        // Wide ladder spanning the Dune bot size (~28 WETH) to find the TRUE optimum.
        uint128[20] memory sizes = [
            uint128(0.05 ether), 0.1 ether, 0.25 ether, 0.5 ether, 1 ether, 2 ether,
            4 ether, 6 ether, 8 ether, 12 ether, 16 ether, 20 ether, 24 ether, 28 ether,
            32 ether, 40 ether, 48 ether, 56 ether, 64 ether, 96 ether
        ];
        uint256 bestProfit = 0;
        uint128 bestSize = 0;
        for (uint256 i = 0; i < sizes.length; i++) {
            uint256 p = _probe(route, sizes[i]);
            emit log_named_uint(string.concat("ORACLE size ", vm.toString(sizes[i]), " -> profit wei"), p);
            if (p > bestProfit) { bestProfit = p; bestSize = sizes[i]; }
        }

        // --- ASSERT: a profitable size was DETECTED at the exact pre-state ---
        assertGt(bestProfit, 0, "GATE3 FAIL: no profitable size at exact pre-state (wrong state point?)");
        emit log_named_uint("GATE3 oracle best size (wei)", bestSize);
        emit log_named_uint("GATE3 oracle best profit (wei)", bestProfit);
        emit log_named_uint("GATE3 oracle best profit (x1e-4 ETH)", bestProfit / 1e14);

        // --- EXECUTE at the detected best size against a PRISTINE snapshot; confirm realized > 0 ---
        uint256 snap = vm.snapshotState();
        uint256 before = IWETHg(WETH).balanceOf(address(this));
        executor.flashLoanArbMixed(bestSize, route, 0);
        uint256 realized = IWETHg(WETH).balanceOf(address(this)) - before;
        vm.revertToState(snap);
        assertGt(realized, 0, "GATE3 FAIL: execution at detected size must net positive WETH to payee");
        emit log_named_uint("GATE3 FINAL realized profit at best size (wei)", realized);
        emit log_named_uint("GATE3 FINAL realized profit (x1e-4 ETH)", realized / 1e14);
    }
}
