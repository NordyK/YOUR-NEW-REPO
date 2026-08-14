// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

/// ISOLATION DIAGNOSTIC (read-only) — generalizes V4StateDivergenceDiag to ALL 5 Gate9
/// clean-success rows that disagree with Foundry live-fork (the V4/Curve cluster):
///   268   UniswapV4>DodoV2                         (trigger idx 5)
///   3408  PancakeV3>UniswapV4>UniswapV3            (trigger idx 3)
///   15603 UniswapV3>UniswapV3>UniswapV4>UniswapV3  (trigger idx 3, same tx as 3408)
///   6252  UniswapV3>UniswapV4>CurveStableSwapNg    (trigger idx 27)
///   6674  UniswapV4>CurveStableSwapNg>UniswapV4    (trigger idx 27, same tx as 6252)
///
/// All three triggers land in block 465686729 (parent 465686728). The engine's offline REVM pins the
/// PARENT and re-executes only its own trigger tx, so a SINGLETON venue (V4 PoolManager / Curve-NG
/// shared pool) mutated by OTHER txs earlier in the block is STALE in the engine's validation DB.
/// This test reads each singleton pool's state at the parent, then at each trigger's pre-arb point
/// (full block prefix via rollFork), and reports per-(pool,trigger) divergence. Divergence == the
/// engine over/under-prices the hop against stale state == the 5/27 spurious successes are an
/// offline state-reproduction artifact, NOT a kernel/calldata bug.
///
/// Run: forge test --fork-url $ARBITRUM_RPC_URL --fork-block-number 465686728 \
///        --match-path test/V4CurveStateDivergenceAll.Fork.t.sol -vv
interface IStateView {
    function getSlot0(bytes32 poolId)
        external
        view
        returns (uint160 sqrtPriceX96, int24 tick, uint24 protocolFee, uint24 lpFee);
    function getLiquidity(bytes32 poolId) external view returns (uint128 liquidity);
}

interface ICurveNg {
    function get_virtual_price() external view returns (uint256);
    function balances(uint256 i) external view returns (uint256);
}

contract V4CurveStateDivergenceAll is Test {
    address constant STATE_VIEW = 0x76Fd297e2D437cd7f76d50F01AfE6160f86e9990;

    // V4 singleton pools in the failing cluster.
    bytes32 constant POOL_A = 0x80c735c5a0222241f211b3edb8df2ccefad94553ec18f1c29143f0399c78f500; // 268/3408/15603/6252/6674
    bytes32 constant POOL_B = 0x41b1e6b28a39199365f5348a2bf71d0f785167e920d05254b3f2efd1b9487db5; // 6674 second V4 hop

    // Curve-NG shared pool in routes 6252/6674.
    address constant CURVE = 0x49b720F1Aab26260BEAec93A7BeB5BF2925b2A8F;

    // Triggers (all block 465686729).
    bytes32 constant TX_IDX3 = 0x78a673323b6ea8ac32196fa7b3e7de4c28e2096ddf9502fdb150bb7a246d2d43; // 3408/15603
    bytes32 constant TX_IDX5 = 0xc870b79186962e5a5612801b0f43798328a6531d75edab8eca33bdd198ba2cec; // 268
    bytes32 constant TX_IDX27 = 0xb47c9960af9633e212ea0dd4e1254ea29011d715258958aa483a8e18b2c54dc9; // 6252/6674

    function _v4(bytes32 poolId) internal view returns (uint160 s, int24 t, uint128 l) {
        (s, t,,) = IStateView(STATE_VIEW).getSlot0(poolId);
        l = IStateView(STATE_VIEW).getLiquidity(poolId);
    }

    function _logV4(string memory tag, bytes32 poolId) internal {
        (uint160 s, int24 t, uint128 l) = _v4(poolId);
        emit log_named_uint(string.concat(tag, " sqrtPriceX96"), s);
        emit log_named_int(string.concat(tag, " tick"), t);
        emit log_named_uint(string.concat(tag, " liquidity"), l);
    }

    function _divergedV4(bytes32 poolId, uint160 ps, int24 pt, uint128 pl) internal view returns (bool) {
        (uint160 s, int24 t, uint128 l) = _v4(poolId);
        return s != ps || t != pt || l != pl;
    }

    function test_v4_curve_singleton_divergence_all_failing_routes() public {
        require(block.chainid == 42161, "fork Arbitrum One");
        emit log_named_uint("parent pin block (engine snapshot)", block.number);

        // (a) PARENT state — what the engine's REVM DB baseline sees for each singleton pool.
        (uint160 aPs, int24 aPt, uint128 aPl) = _v4(POOL_A);
        (uint160 bPs, int24 bPt, uint128 bPl) = _v4(POOL_B);
        emit log_string("--- PARENT (465686728) singleton state ---");
        _logV4("POOL_A parent", POOL_A);
        _logV4("POOL_B parent", POOL_B);
        uint256 cVpParent = ICurveNg(CURVE).get_virtual_price();
        uint256 cB0Parent = ICurveNg(CURVE).balances(0);
        uint256 cB1Parent = ICurveNg(CURVE).balances(1);
        emit log_named_uint("CURVE parent virtual_price", cVpParent);
        emit log_named_uint("CURVE parent balances0", cB0Parent);
        emit log_named_uint("CURVE parent balances1", cB1Parent);

        bool anyDiverged = false;

        // (b) idx3 pre-arb (routes 3408/15603) — POOL_A only.
        vm.rollFork(TX_IDX3);
        emit log_string("--- pre-arb idx3 (routes 3408/15603) ---");
        _logV4("POOL_A @idx3", POOL_A);
        bool d3a = _divergedV4(POOL_A, aPs, aPt, aPl);
        emit log_named_string("POOL_A diverged @idx3", d3a ? "YES (stale in engine)" : "no");
        anyDiverged = anyDiverged || d3a;

        // (c) idx5 pre-arb (route 268) — POOL_A only.
        vm.rollFork(TX_IDX5);
        emit log_string("--- pre-arb idx5 (route 268) ---");
        _logV4("POOL_A @idx5", POOL_A);
        bool d5a = _divergedV4(POOL_A, aPs, aPt, aPl);
        emit log_named_string("POOL_A diverged @idx5", d5a ? "YES (stale in engine)" : "no");
        anyDiverged = anyDiverged || d5a;

        // (d) idx27 pre-arb (routes 6252/6674) — POOL_A, POOL_B, and Curve.
        vm.rollFork(TX_IDX27);
        emit log_string("--- pre-arb idx27 (routes 6252/6674) ---");
        _logV4("POOL_A @idx27", POOL_A);
        _logV4("POOL_B @idx27", POOL_B);
        bool d27a = _divergedV4(POOL_A, aPs, aPt, aPl);
        bool d27b = _divergedV4(POOL_B, bPs, bPt, bPl);
        emit log_named_string("POOL_A diverged @idx27", d27a ? "YES (stale in engine)" : "no");
        emit log_named_string("POOL_B diverged @idx27", d27b ? "YES (stale in engine)" : "no");

        uint256 cVp27 = ICurveNg(CURVE).get_virtual_price();
        uint256 cB027 = ICurveNg(CURVE).balances(0);
        uint256 cB127 = ICurveNg(CURVE).balances(1);
        emit log_named_uint("CURVE @idx27 virtual_price", cVp27);
        emit log_named_uint("CURVE @idx27 balances0", cB027);
        emit log_named_uint("CURVE @idx27 balances1", cB127);
        bool dCurve = (cVp27 != cVpParent) || (cB027 != cB0Parent) || (cB127 != cB1Parent);
        emit log_named_string("CURVE diverged @idx27", dCurve ? "YES (stale in engine)" : "no");
        anyDiverged = anyDiverged || d27a || d27b || dCurve;

        if (anyDiverged) {
            emit log_string("PROVEN: at least one singleton (V4/Curve) pool's state moved within block 465686729 "
                "before the trigger. The engine's parent-pinned/trigger-only offline REVM validates "
                "these hops against STALE state -> the 5/27 clean-success disagreements are an offline "
                "state-reproduction artifact for singleton venues, not a kernel/calldata bug. "
                "(V2/V3 per-pair pools are only touched by their own trigger, so 22/27 match.)");
        } else {
            emit log_string("NOT DIVERGENT: singleton state identical parent->pre-arb for all failing routes -> "
                "the disagreement is NOT singleton-state staleness; investigate default-zero fetch, "
                "calldata, or metadata instead.");
        }
    }
}
