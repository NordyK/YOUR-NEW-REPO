// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/MantisExecutor.sol";

/// GAP 3 — CURVE through the MIXED path (`flashLoanArbMixed`, TradeHopV2) with the engine's actual
/// id mapping. The engine emits Curve crypto pools as exchangeId 17 (CURVE_TWOCRYPTO_NG_ID) /
/// stable as 18 through the MIXED entrypoint, but the existing fork tests only exercise Curve via
/// `flashLoanArbV2` (TradeHop, ids 16/18). This test closes that: it drives a Curve leg as id 17
/// through `flashLoanArbMixed`.
///
/// In the executor the id-17 branch resolves to the Curve CRYPTO ABI
/// `exchange(uint256 i, uint256 j, uint256 dx, uint256 min_dy)` (it is NOT a StableSwap-NG id, so it
/// skips the int128 `exchange`/`exchange_underlying` variants — see MantisExecutor L2164-2179). That
/// is byte-identical to what a Curve Tricrypto/TwoCrypto-NG pool exposes. We therefore drive the
/// id-17 dispatch against a REAL Curve crypto pool discovered on-fork from the Tricrypto-NG factory.
///
/// Why this pool: TwoCrypto-NG (2-coin) is OPT-IN and has near-zero Arbitrum volume — its factory
/// isn't even pinned in the repo (engine.example.json marks it OFF). The Tricrypto-NG factory
/// 0xbC07…bEE8 (the same one `MantisExecutor.Fork.t.sol` and research/dex/curve_cryptoswap use) has
/// real, cached pools whose `exchange(uint256,...)` ABI is the EXACT call the id-17 branch makes.
/// `pool_list(0)` is resolved on-fork and its WETH/USDC.e coin indices read via `get_coins` — no
/// hard-coded pool address.
///
/// Route: WETH --Curve id17 exchange(WETH->USDC.e)--> USDC.e --UniV3 500(USDC.e->WETH)--> WETH,
/// Balancer-flash-funded (no Balancer leg present, so the Balancer-flash path is legal).
///
/// ACCEPTANCE: executes the full mixed path through the id-17 Curve dispatch and either nets profit
/// or reverts `Loss` (a Curve↔V3 round-trip with fees is normally lossy) — NOT a decode / unknown-id
/// / "Curve pool required" revert. Mechanical-path proof for id-17 over the MIXED entrypoint.
///
/// Run: forge test --fork-url $ARBITRUM_RPC_URL --fork-block-number 465686728 \
///        --match-path test/MantisCurveMixedId17.Fork.t.sol -vv
interface ICurveTriCryptoFactory17 {
    function pool_list(uint256 index) external view returns (address);
    function get_coins(address pool) external view returns (address[3] memory);
}

contract MantisCurveMixedId17ForkTest is Test {
    MantisExecutor executor;

    address constant WETH = 0x82aF49447D8a07e3bd95BD0d56f35241523fBab1;
    address constant USDC_NATIVE = 0xaf88d065e77c8cC2239327C5EDb3A432268e5831;
    address constant USDC_BRIDGED = 0xFF970A61A04b1cA14834A43f5dE4533eBDDB5CC8;
    address constant USDT0 = 0xFd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9;
    address constant CURVE_TRICRYPTO_FACTORY = 0xbC0797015fcFc47d9C1856639CaE50D0e69FbEE8;

    uint8 constant UNISWAP_V3_ID = 0;
    uint8 constant CURVE_TWOCRYPTO_NG_ID = 17; // engine's MIXED-path Curve crypto id

    error Loss(uint256);
    error InsufficientProfit(uint256 actual, uint256 minimum);

    function setUp() public {
        require(block.chainid == 42161, "fork Arbitrum One");
        executor = new MantisExecutor(address(this)); // payee == gateway == this
        assertTrue(CURVE_TRICRYPTO_FACTORY.code.length > 0, "Curve Tricrypto-NG factory missing on fork");
    }

    function _curveHop(address tin, address tout, address pool, uint8 ci, uint8 co)
        internal
        pure
        returns (MantisExecutor.TradeHopV2 memory h)
    {
        h.exchangeId = CURVE_TWOCRYPTO_NG_ID;
        h.tokenIn = tin;
        h.tokenOut = tout;
        h.pool = pool;
        h.coinInIdx = ci;
        h.coinOutIdx = co;
    }

    function _v3(address tin, address tout, uint24 fee) internal pure returns (MantisExecutor.TradeHopV2 memory h) {
        h.exchangeId = UNISWAP_V3_ID;
        h.tokenIn = tin;
        h.tokenOut = tout;
        h.fee = fee;
    }

    /// Find a real Curve crypto pool (from the Tricrypto-NG factory) that contains WETH plus a
    /// USDC/USDT-class coin with a UniV3 WETH companion to close the cycle. Scans the first pools.
    function _findCurveCryptoLeg()
        internal
        view
        returns (address pool, uint8 wethIdx, uint8 stableIdx, address stable)
    {
        ICurveTriCryptoFactory17 f = ICurveTriCryptoFactory17(CURVE_TRICRYPTO_FACTORY);
        for (uint256 p = 0; p < 12; p++) {
            address cand;
            try f.pool_list(p) returns (address a) {
                cand = a;
            } catch {
                break;
            }
            if (cand == address(0) || cand.code.length == 0) continue;
            address[3] memory coins;
            try f.get_coins(cand) returns (address[3] memory c) {
                coins = c;
            } catch {
                continue;
            }
            uint8 wi = type(uint8).max;
            uint8 si = type(uint8).max;
            address sTok;
            for (uint8 i = 0; i < 3; i++) {
                if (coins[i] == WETH) wi = i;
                else if (coins[i] == USDC_NATIVE || coins[i] == USDC_BRIDGED || coins[i] == USDT0) {
                    si = i;
                    sTok = coins[i];
                }
            }
            if (wi != type(uint8).max && si != type(uint8).max) {
                return (cand, wi, si, sTok);
            }
        }
        revert("no real Curve crypto pool with WETH + a USDC/USDT coin found");
    }

    function _v3FeeForWethStable(address stable) internal pure returns (uint24) {
        // UniV3 0.05% WETH pools exist for native USDC, bridged USDC.e, and USDT0 at 465686728.
        if (stable == USDC_NATIVE || stable == USDC_BRIDGED || stable == USDT0) return 500;
        return 3000;
    }

    function test_curve_id17_executes_via_flashLoanArbMixed() public {
        (address pool, uint8 wethIdx, uint8 stableIdx, address stable) = _findCurveCryptoLeg();
        emit log_named_address("curve crypto pool", pool);
        emit log_named_address("curve stable-side coin", stable);

        // WETH --Curve id17(WETH->stable)--> stable --UniV3(stable->WETH)--> WETH.
        MantisExecutor.TradeHopV2[3] memory route;
        route[0] = _curveHop(WETH, stable, pool, wethIdx, stableIdx);
        route[1] = _v3(stable, WETH, _v3FeeForWethStable(stable));

        bool ran;
        try executor.flashLoanArbMixed(1e15, route, 0) {
            ran = true; // a real edge existed -> profit landed on payee
            emit log_string("Curve id-17 MIXED route SUCCEEDED (net-positive)");
        } catch (bytes memory reason) {
            bytes4 sel;
            assembly {
                sel := mload(add(reason, 0x20))
            }
            if (sel == Loss.selector || sel == InsufficientProfit.selector) {
                ran = true; // the id-17 Curve exchange RAN; profit gate fired
                emit log_string("Curve id-17 MIXED route executed; profit gate fired (gas charged)");
            } else if (reason.length >= 68 && bytes4(reason) == bytes4(0x08c379a0)) {
                assembly {
                    reason := add(reason, 0x04)
                }
                revert(abi.decode(reason, (string)));
            } else {
                revert("Curve id-17 mixed route hit an unexpected (non-profit-gate) revert");
            }
        }
        assertTrue(ran, "Curve id-17 leg must execute through flashLoanArbMixed (no unknown-id / decode revert)");
    }
}
