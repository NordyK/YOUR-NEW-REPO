// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/MantisExecutor.sol";

/// GAS-ORACLE-vs-ANVIL verification (offline, pinned to a CACHED block).
///
/// Measures the REAL on-chain gas of executing each route shape through MantisExecutor on an
/// Arbitrum fork served entirely from the local disk cache (anvil/block_cache, symlinked into
/// ~/.foundry/cache/rpc/arbitrum), and checks it against the gas the engine assumes:
///   - the on-wire gas_limit the submitter sets (700_000)  -> must never OOG
///   - the per-family profit-calc gas_units                -> must be a SAFE (>=) estimate, else
///     the engine would under-state cost and submit an arb that loses money after gas.
///
/// Engine gas_units constants (engine/crates/engine/src/pipeline.rs), per family:
///   DODO/PMM cycle        = 520_000      (line 4218)
///   V2/V3 mixed           = 20_000 + v2*120_000 + v3*110_000   (line 5155)
///   generic Mantis submit = 200_000 + 50_000*hops             (line 5361)
///
/// Run (offline, cache-served):
///   forge test --match-path test/MantisGasOracle.Fork.t.sol -vv
/// (ARBITRUM_RPC_URL must be set for the FIRST fill of any uncached slot; re-runs are 0-RPC.)
interface IV3g { function token0() external view returns (address); function token1() external view returns (address); function fee() external view returns (uint24); }
interface IDodoG { function _BASE_TOKEN_() external view returns (address); function _QUOTE_TOKEN_() external view returns (address); }

contract MantisGasOracleForkTest is Test {
    MantisExecutor executor;
    address constant WETH = 0x82aF49447D8a07e3bd95BD0d56f35241523fBab1;
    uint8 constant DODO = 23;
    uint128 constant AMOUNT_IN = 2e16; // 0.02 WETH
    uint256 constant CACHED_BLOCK = 460179231; // most-cached fork block on disk (27 MB)
    // Submitter's on-wire tx.gas_limit = 400_000 + 300_000*hop_count (pipeline.rs); 3-hop = 1.3M.
    uint64 constant WIRE_GAS_LIMIT_3HOP = 1_300_000;
    error Loss(uint256);

    function setUp() public {
        // Pin to the CACHED block so reads hit disk; offline after first fill.
        vm.createSelectFork(vm.envString("ARBITRUM_RPC_URL"), CACHED_BLOCK);
        require(block.chainid == 42161, "fork Arbitrum One");
        executor = new MantisExecutor(address(this));
    }

    function _tok(uint8 ex, address pool) internal view returns (address a, address b) {
        if (ex == DODO) return (IDodoG(pool)._BASE_TOKEN_(), IDodoG(pool)._QUOTE_TOKEN_());
        return (IV3g(pool).token0(), IV3g(pool).token1());
    }

    function _hop(uint8 ex, address tin, address tout, address pool)
        internal view returns (MantisExecutor.TradeHopV2 memory h)
    {
        h.exchangeId = ex; h.tokenIn = tin; h.tokenOut = tout;
        if (ex == DODO) h.pool = pool;
        else if (ex == 0 || ex == 6 || ex == 25) h.fee = IV3g(pool).fee();
    }

    /// Execute one route and return the gas consumed by the WHOLE flashLoanArbMixed call —
    /// including flash borrow, every swap leg, and the repay/minNetProfit check (the call reverts
    /// with Loss at the final profit gate, but all execution gas up to that point is charged, so
    /// this is the faithful on-chain execution cost of the arb).
    function _measure(MantisExecutor.TradeHopV2[3] memory route) internal returns (uint256 used, bool ranAllLegs) {
        uint256 g0 = gasleft();
        try executor.flashLoanArbMixed(AMOUNT_IN, route, 0) {
            used = g0 - gasleft(); ranAllLegs = true;
        } catch (bytes memory data) {
            used = g0 - gasleft();
            bytes4 sel; assembly { sel := mload(add(data, 0x20)) }
            ranAllLegs = (sel == Loss.selector); // Loss == executed through all legs, arb just closed
        }
    }

    function _triangular(uint8 x0, uint8 x1, uint8 x2, address p0, address p1, address p2)
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

    function test_GasOracleVsAnvil() public {
        // Representative 3-hop DODO-triangular routes (the dominant detected shape).
        uint16[5] memory rid   = [uint16(2665), 2967, 2664, 2668, 2972];
        uint8[5]  memory rx0   = [uint8(0), 6, 0, 0, 6];
        uint8[5]  memory rx2   = [uint8(0), 0, 23, 6, 9];
        address[5] memory rp0  = [
            0x2f5e87C9312fa29aed5c179E456625D79015299c,
            0x4bfc22A4dA7f31F8a912a79A7e44a822398b4390,
            0x2f5e87C9312fa29aed5c179E456625D79015299c,
            0x2f5e87C9312fa29aed5c179E456625D79015299c,
            0x4bfc22A4dA7f31F8a912a79A7e44a822398b4390
        ];
        address constant_p1 = 0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14; // DODO middle
        address[5] memory rp2  = [
            0xC6962004f452bE9203591991D15f6b388e09E8D0,
            0xC6962004f452bE9203591991D15f6b388e09E8D0,
            0x1666EA6E83BCC3473111732Ef086BA47e6EDDeB7,
            0x7fCDC35463E3770c2fB992716Cd070B63540b947,
            0xB1026b8e7276e7AC75410F1fcbbe21796e8f7526
        ];

        uint256 maxUsed;
        uint256 ranCount;
        emit log_string("=== DODO/PMM 3-hop cycles: engine assumes 520_000 gas; on-wire limit 700_000 ===");
        for (uint256 i = 0; i < rid.length; i++) {
            MantisExecutor.TradeHopV2[3] memory route =
                _triangular(rx0[i], 23, rx2[i], rp0[i], constant_p1, rp2[i]);
            (uint256 used, bool ran) = _measure(route);
            emit log_named_uint(string.concat("route ", vm.toString(rid[i]), " gasUsed"), used);
            if (ran) {
                ranCount++;
                if (used > maxUsed) maxUsed = used;
            }
        }
        emit log_named_uint("routes that ran through all legs", ranCount);
        emit log_named_uint("MAX gasUsed across DODO cycles", maxUsed);
        emit log_named_uint("engine DODO/PMM detect assumption (was 520k -> now)", 960_000);
        emit log_named_uint("engine 3-hop submit-floor gas_units (was 350k -> now)", 960_000);
        emit log_named_uint("on-wire gas_limit 3-hop (was 750k -> now)", WIRE_GAS_LIMIT_3HOP);
        // Economic-safety verdict surfaced for the report: the detector gas (960k) now EQUALS the
        // submit floor and must cover the measured worst case so detection is never more optimistic
        // than submission (the fast lane skips REVM, making the detector EV gate the last check).
        if (maxUsed > 960_000) {
            emit log_string("NOTE: max DODO gasUsed EXCEEDS the 960k detect/submit constant -> engine under-budgets gas for the heaviest DODO cycles; raise the constant.");
        } else {
            emit log_string("OK: 960k DODO detect/submit constant is a safe (>=) estimate vs measured on-chain gas.");
        }
        require(ranCount > 0, "at least one route must execute for a valid gas measurement");
        // The submitter's on-wire gas_limit MUST cover the worst measured cycle (else OOG on-chain).
        // After raising it to 400k+300k*hops (=1.3M for 3-hop) in pipeline.rs, this holds with margin.
        assertLt(maxUsed, uint256(WIRE_GAS_LIMIT_3HOP), "on-wire 3-hop gas_limit (1.3M) must cover worst DODO cycle");
        // And the new 960k submit-floor gas_units must be a SAFE (>=) estimate of the measured worst.
        assertLe(maxUsed, uint256(960_000), "new 3-hop submit-floor gas_units (960k) must cover measured worst-case");
    }
}
