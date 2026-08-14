// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/MantisExecutor.sol";

/// DET-3.1 SLIPPAGE-CURVE VALIDATION (anti-phantom). Each input size is measured from the IDENTICAL
/// pre-arb fork state via snapshot/revert, so the curve is a true f(size) with no path-dependence
/// (unlike the in-sequence sweep in MantisDetectExecuteProfit). The phantom signature is FLAT profit
/// at large size (slippage not biting); a REAL arb's realized profit must be concave with a single
/// interior maximum, then degrade/revert as size grows past pool depth.
///
/// Seeded with the flagship clean 2-hop arb at its PRE-state (parent block 465686728):
///   WETH --Pancake V3(100)--> WBTC --Uniswap V3(500)--> WETH.
///
/// Run: forge test --fork-url $ARBITRUM_RPC_URL --fork-block-number 465686728 \
///        --match-path test/MantisSlippageCurve.Fork.t.sol -vv
interface IWETHs { function balanceOf(address) external view returns (uint256); }

contract MantisSlippageCurveForkTest is Test {
    MantisExecutor executor;
    address constant WETH = 0x82aF49447D8a07e3bd95BD0d56f35241523fBab1;
    address constant WBTC = 0x2f2a2543B76A4166549F7aaB2e75Bef0aefC5B0f;
    uint8 constant UNIV3 = 0;
    uint8 constant PANCAKE_V3 = 6;
    error Loss(uint256);

    function setUp() public {
        require(block.chainid == 42161, "fork Arbitrum One");
        executor = new MantisExecutor(address(this));
    }

    function _v3(uint8 ex, address tin, address tout, uint24 fee) internal pure returns (MantisExecutor.TradeHopV2 memory h) {
        h.exchangeId = ex; h.tokenIn = tin; h.tokenOut = tout; h.fee = fee;
    }

    function test_SlippageCurve_Isolated() public {
        MantisExecutor.TradeHopV2[3] memory route;
        route[0] = _v3(PANCAKE_V3, WETH, WBTC, 100);
        route[1] = _v3(UNIV3, WBTC, WETH, 500);

        // Spec DET-3.1 sizes {0.1,0.5,1,5,10,20} plus large sizes to find the gross peak/revert
        // from identical fresh state (a phantom edge would never peak; a real edge must).
        uint128[16] memory sizes = [
            uint128(0.1 ether), 0.5 ether, 1 ether, 2 ether, 5 ether,
            10 ether, 16 ether, 20 ether, 24 ether, 28 ether,
            32 ether, 40 ether, 80 ether, 160 ether, 320 ether,
            640 ether
        ];
        int256 prevProfit = type(int256).min;
        int256 prevMarginalSign = 1; // start positive
        uint256 peaks = 0;
        bool sawLossAfterPeak = false;
        emit log_string("size_wei,profit_wei,profit_is_loss");
        for (uint256 i = 0; i < sizes.length; i++) {
            uint256 snap = vm.snapshotState();
            uint256 bal0 = IWETHs(WETH).balanceOf(address(this));
            int256 profit;
            bool isLoss = false;
            try executor.flashLoanArbMixed(sizes[i], route, 0) {
                profit = int256(IWETHs(WETH).balanceOf(address(this)) - bal0);
            } catch {
                profit = 0; isLoss = true; // Loss/revert at this size (edge exhausted)
            }
            emit log_named_int(string.concat("profit@", vm.toString(uint256(sizes[i]))), profit);
            // Detect sign change of the marginal (peak) on the successful region.
            if (i > 0 && !isLoss) {
                int256 marginal = profit - prevProfit;
                int256 sign = marginal >= 0 ? int256(1) : int256(-1);
                if (sign != prevMarginalSign && prevMarginalSign == 1) { peaks++; }
                prevMarginalSign = sign;
            }
            if (isLoss && peaks == 1) {
                sawLossAfterPeak = true;
            }
            prevProfit = profit;
            vm.revertToState(snap);
        }
        emit log_named_uint("interior maxima detected (expect exactly 1)", peaks);
        assertEq(peaks, 1, "slippage curve must have one successful-region peak");
        assertTrue(sawLossAfterPeak, "large sizes must decay/revert, not remain flat-profitable");
    }
}
