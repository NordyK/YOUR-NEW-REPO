// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/MantisExecutor.sol";

/// REALIZED-PnL test — the test the prior report did NOT run. For each detected DODO-family route,
/// execute Mantis's OWN route at Mantis's OWN detected size (amount_in_eth_min from
/// arb_routes_for_anvil.csv) with minNetProfit=0, and measure the **realized net** = the WETH the
/// executor transfers to `payee` on success (callbackProfit), or a `Loss` revert (net < 0). This is
/// realized profit, NOT "executed mechanically". Distinguishes "executor works" from "detection was a
/// real profitable arb".
///
/// CAVEAT (honest): forks at the single cached block 460179231, which is NOT each route's detection
/// block. Arbs are fleeting, so a `Loss`/breakeven here means "the arb had closed by this block OR
/// the detection was phantom" — it cannot distinguish the two. The DEFINITIVE test forks at each
/// route's own detection block (needs per-candidate L2-block capture from a dry-run). This run proves
/// the realized-net *measurement* + reports the success/Loss split at detected size.
///
/// Run: forge test --fork-url $ARBITRUM_RPC_URL --fork-block-number 460179231 \
///        --match-path test/MantisRealizedPnL.Fork.t.sol -vv
interface IV3p { function token0() external view returns (address); function token1() external view returns (address); function fee() external view returns (uint24); }
interface IDodoPnl { function _BASE_TOKEN_() external view returns (address); function _QUOTE_TOKEN_() external view returns (address); }
interface IWETHp { function balanceOf(address) external view returns (uint256); }

contract MantisRealizedPnLForkTest is Test {
    MantisExecutor executor;
    address constant WETH = 0x82aF49447D8a07e3bd95BD0d56f35241523fBab1;
    uint8 constant DODO = 23;
    error Loss(uint256);

    function setUp() public {
        require(block.chainid == 42161, "fork Arbitrum One");
        executor = new MantisExecutor(address(this)); // payee = this → realized profit lands here
    }

    function _tok(uint8 ex, address pool) internal view returns (address a, address b) {
        if (ex == DODO) return (IDodoPnl(pool)._BASE_TOKEN_(), IDodoPnl(pool)._QUOTE_TOKEN_());
        return (IV3p(pool).token0(), IV3p(pool).token1());
    }
    function _hop(uint8 ex, address tin, address tout, address pool) internal view returns (MantisExecutor.TradeHopV2 memory h) {
        h.exchangeId = ex; h.tokenIn = tin; h.tokenOut = tout;
        if (ex == DODO) h.pool = pool;
        else if (ex == 0 || ex == 6 || ex == 25) h.fee = IV3p(pool).fee();
    }
    function _tri(uint8 x0, uint8 x1, uint8 x2, address p0, address p1, address p2)
        internal view returns (MantisExecutor.TradeHopV2[3] memory route)
    {
        (address q0a, address q0b) = _tok(x0, p0);
        address A = q0a == WETH ? q0b : q0a;
        (address m0, address m1) = _tok(x1, p1);
        address B = (m0 == A) ? m1 : m0;
        route[0] = _hop(x0, WETH, A, p0);
        route[1] = _hop(x1, A, B, p1);
        route[2] = _hop(x2, B, WETH, p2);
    }

    function _run(uint16 id, uint8 x0, uint8 x2, address p0, address p2, uint128 amount) internal {
        MantisExecutor.TradeHopV2[3] memory route =
            _tri(x0, DODO, x2, p0, 0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14, p2);
        uint256 balBefore = IWETHp(WETH).balanceOf(address(this));
        uint256 g0 = gasleft();
        try executor.flashLoanArbMixed(amount, route, 0) {
            uint256 used = g0 - gasleft();
            int256 net = int256(IWETHp(WETH).balanceOf(address(this))) - int256(balBefore);
            emit log_named_int(string.concat("route ", vm.toString(id), " REALIZED net (wei, +=profit)"), net);
            emit log_named_uint(string.concat("route ", vm.toString(id), " gasUsed"), used);
        } catch (bytes memory data) {
            bytes4 sel; assembly { sel := mload(add(data, 0x20)) }
            if (sel == Loss.selector) {
                uint256 shortfall; assembly { shortfall := mload(add(data, 0x24)) }
                emit log_named_uint(string.concat("route ", vm.toString(id), " LOSS shortfall (wei, net<0)"), shortfall);
            } else {
                emit log_named_bytes32(string.concat("route ", vm.toString(id), " UNEXPECTED revert sel"), bytes4(sel));
            }
        }
    }

    /// DODO-family routes at their DETECTED amount_in_eth_min (the phantom family the review flagged).
    function test_RealizedPnL_DodoFamily() public {
        address U = 0x2f5e87C9312fa29aed5c179E456625D79015299c; // UniV3 USDC/WBTC
        address P = 0x4bfc22A4dA7f31F8a912a79A7e44a822398b4390; // PancakeV3 USDC/WBTC
        address V = 0xC6962004f452bE9203591991D15f6b388e09E8D0; // UniV3 WBTC/WETH
        // id, e0, e2, pool0, pool2, amount_in (wei = amount_in_eth_min × 1e18)
        _run(2665, 0, 0, U, V, 3198000000000000000);
        _run(2615, 0, 0, U, V, 4849000000000000000);
        _run(2967, 6, 0, P, V, 1672000000000000000);
        _run(2664, 0, 23, U, 0x1666EA6E83BCC3473111732Ef086BA47e6EDDeB7, 594000000000000000);
        _run(2668, 0, 6, U, 0x7fCDC35463E3770c2fB992716Cd070B63540b947, 1200000000000000000);
        _run(2972, 6, 9, P, 0xB1026b8e7276e7AC75410F1fcbbe21796e8f7526, 599000000000000000);
        _run(2670, 0, 9, U, 0xB1026b8e7276e7AC75410F1fcbbe21796e8f7526, 498000000000000000);
        emit log_string("REALIZED net per route above (+ = profit lands on payee; LOSS = net<0). At block 460179231 (NOT the detection block).");
    }
}
