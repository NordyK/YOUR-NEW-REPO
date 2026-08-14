// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/MantisExecutor.sol";

/// END-TO-END proof: detect a PROFITABLE arb and execute it for realized profit.
///
/// Seeded with a REAL clean 2-hop linear arb from Dune (block 465686729, $58k notional):
///   WETH --Pancake V3 (fee 100)--> WBTC --Uniswap V3 (fee 500)--> WETH.
/// Both pools are WBTC/WETH; the cross-venue price gap is the edge a real bot captured in this block.
///
/// "DETECT": sweep input sizes (what the engine's golden-section sizer does) and find the size that
/// maximises realized profit — i.e. confirm a profitable size EXISTS and is findable.
/// "EXECUTE + PROFIT": run `flashLoanArbMixed` at that size and measure the WETH delta paid to the
/// payee. PASS iff realized profit > 0.
///
/// Run: forge test --fork-url $ARBITRUM_RPC_URL --fork-block-number 465686728 \
///        --match-path test/MantisDetectExecuteProfit.Fork.t.sol -vv
/// (465686728 = the PARENT pin: the arb was captured IN block 465686729, so the edge only exists
///  in the parent's end-state — forking at 465686729 yields post-arb state with no edge. Same
///  pre-state convention as MantisGate3PreState / the FINAL_GREEN_REVIEW 93/93 run.)
interface IWETHd { function balanceOf(address) external view returns (uint256); }

contract MantisDetectExecuteProfitForkTest is Test {
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

    function _v3(uint8 ex, address tin, address tout, uint24 fee) internal pure returns (MantisExecutor.TradeHopV2 memory h) {
        h.exchangeId = ex; h.tokenIn = tin; h.tokenOut = tout; h.fee = fee;
    }

    function test_detect_then_execute_profit() public {
        // The cycle (matches the real arb's direction): WETH->WBTC on Pancake(100), WBTC->WETH on Uniswap(500).
        MantisExecutor.TradeHopV2[3] memory route;
        route[0] = _v3(PANCAKE_V3, WETH, WBTC, 100);
        route[1] = _v3(UNIV3, WBTC, WETH, 500);
        // route[2] left inactive -> 2-hop cycle.

        // --- DETECT: sweep sizes, find the profit-maximising one (golden-section's job) ---
        uint128[12] memory sizes = [
            uint128(0.02 ether), 0.05 ether, 0.1 ether, 0.25 ether, 0.5 ether, 1 ether,
            2 ether, 4 ether, 8 ether, 16 ether, 32 ether, 64 ether
        ];
        uint256 bestProfit = 0;
        uint128 bestSize = 0;
        for (uint256 i = 0; i < sizes.length; i++) {
            // Identical-fresh-state sweep (same pattern as MantisSlippageCurve): a successful
            // execution at one size would otherwise CONSUME the edge and starve every later
            // size — and the final detected-size execution below — of the dislocation.
            uint256 snap = vm.snapshotState();
            uint256 bal0 = IWETHd(WETH).balanceOf(address(this));
            try executor.flashLoanArbMixed(sizes[i], route, 0) {
                uint256 profit = IWETHd(WETH).balanceOf(address(this)) - bal0;
                if (profit > bestProfit) { bestProfit = profit; bestSize = sizes[i]; }
                emit log_named_uint(string.concat("size ", vm.toString(sizes[i]), " -> profit wei"), profit);
            } catch {
                // Loss / not profitable at this size — keep sweeping.
            }
            vm.revertToState(snap);
        }

        // --- ASSERT: a profitable size was DETECTED ---
        assertGt(bestProfit, 0, "no profitable size found for the real arb cycle");
        emit log_named_uint("DETECTED best size (wei)", bestSize);
        emit log_named_uint("DETECTED+EXECUTED realized profit (wei)", bestProfit);

        // --- EXECUTE at the detected best size and confirm realized profit lands on payee ---
        uint256 before = IWETHd(WETH).balanceOf(address(this));
        executor.flashLoanArbMixed(bestSize, route, 0);
        uint256 realized = IWETHd(WETH).balanceOf(address(this)) - before;
        assertGt(realized, 0, "execution at detected size must net positive WETH to payee");
        emit log_named_uint("FINAL realized profit at detected size (wei)", realized);
        // ETH terms for readability (1e14 wei = 0.0001 ETH).
        emit log_named_uint("FINAL realized profit (x1e-4 ETH)", realized / 1e14);
    }
}
