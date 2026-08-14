// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/MantisExecutor.sol";

/// Validates the NEW 4-hop executor path (`flashLoanArbMixed4`, `TradeHopV2[4]`). Builds a valid
/// 4-hop closed cycle WETH→USDC→WBTC→USDC→WETH (V3 + DODO legs) and executes it over a fork. The
/// cycle is intentionally lossy (a DODO round-trip), so the PASS criterion is: it executes the full
/// 4-swap path and reverts `Loss(...)` (or, if a real edge exists, succeeds) — NOT a decode/
/// malformed/mechanical revert. This proves the [4] decode → 4 sequential swaps → repay-or-Loss
/// path works end-to-end, complementing the untouched [3] suite.
///
/// Run: forge test --fork-url $ARBITRUM_RPC_URL --fork-block-number 467233189 \
///        --match-path test/MantisExecutor4Hop.Fork.t.sol -vv
contract MantisExecutor4HopForkTest is Test {
    MantisExecutor executor;
    address constant WETH = 0x82aF49447D8a07e3bd95BD0d56f35241523fBab1;
    address constant USDC = 0xaf88d065e77c8cC2239327C5EDb3A432268e5831;
    address constant WBTC = 0x2f2a2543B76A4166549F7aaB2e75Bef0aefC5B0f;
    address constant V3_WETH_USDC = 0xC6962004f452bE9203591991D15f6b388e09E8D0; // UniV3 WETH/USDC 500
    address constant DODO_WBTC_USDC = 0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14; // DODO DPP WBTC/USDC
    uint8 constant V3 = 0;
    uint8 constant DODO = 23;
    error Loss(uint256);

    function setUp() public {
        require(block.chainid == 42161, "fork Arbitrum One");
        executor = new MantisExecutor(address(this)); // payee = this
    }

    function _v3(address tin, address tout, uint24 fee) internal pure returns (MantisExecutor.TradeHopV2 memory h) {
        h.exchangeId = V3; h.tokenIn = tin; h.tokenOut = tout; h.fee = fee;
    }
    function _dodo(address tin, address tout, address pool) internal pure returns (MantisExecutor.TradeHopV2 memory h) {
        h.exchangeId = DODO; h.tokenIn = tin; h.tokenOut = tout; h.pool = pool;
    }

    function test_4hop_executes_full_path() public {
        // Valid 4-hop cycle: WETH→USDC (V3) → WBTC (DODO) → USDC (DODO) → WETH (V3).
        MantisExecutor.TradeHopV2[4] memory route;
        route[0] = _v3(WETH, USDC, 500);
        route[1] = _dodo(USDC, WBTC, DODO_WBTC_USDC);
        route[2] = _dodo(WBTC, USDC, DODO_WBTC_USDC);
        route[3] = _v3(USDC, WETH, 500);

        bool executed = false;
        try executor.flashLoanArbMixed4(1e16, route, 0) {
            executed = true; // a real edge existed → profit landed on payee
            emit log_string("4-hop route SUCCEEDED (net-positive)");
        } catch (bytes memory data) {
            bytes4 sel;
            assembly { sel := mload(add(data, 0x20)) }
            // Loss(uint256) selector = the executor's own profit-gate revert => the 4 swaps RAN.
            if (sel == Loss.selector) {
                executed = true;
                uint256 shortfall;
                assembly { shortfall := mload(add(data, 0x24)) }
                emit log_named_uint("4-hop route executed; reverted Loss shortfall (wei)", shortfall);
            } else {
                emit log_named_bytes32("UNEXPECTED 4-hop revert selector", bytes4(sel));
            }
        }
        assertTrue(executed, "flashLoanArbMixed4 must decode [4] + run 4 swaps (success or Loss), not a mechanical revert");
    }
}
