// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/MantisExecutor.sol";

/// GAP 2 — BALANCER-WEIGHTED leg (exchangeId 27) has never been EXECUTED in any fork test.
///
/// The executor routes a weighted leg through the IDENTICAL Vault single-swap path as a stable leg
/// (`_swapExactInHopV2` dispatches both id 26 and id 27 to `_swapExactInBalancer`; the Vault then
/// dispatches to the pool's own weighted invariant math by poolId — see MantisExecutor L1996-2000).
/// This test executes a REAL Balancer V2 *weighted* pool leg (id 27) through the Aave-funded
/// entrypoint (weighted pools live in the same non-reentrant Vault, so Aave is the only legal
/// funding) and asserts it RUNS: success, or the executor's own profit gate (`Loss` /
/// `InsufficientProfit`) — i.e. NOT an "Unknown exchange ID" / decode / funding-guard revert.
///
/// Real pool: discovered on-fork. We probe a candidate list (from scratchpad real_pools.json @
/// head_block 465686728) with `getNormalizedWeights()` — a method ONLY weighted pools expose — and
/// pick the first that (a) is a weighted pool and (b) trades a {WETH, X} pair with a Uniswap-V3
/// companion, so we never hard-code a guessed weighted poolId. The 50WBTC-50WETH pool 0x6454…0595
/// is the expected primary hit. poolId is read on-fork via getPoolId(). Funding asset = WETH (Aave).
///
/// ACCEPTANCE: executes the full Aave→[WETH→X on Balancer-WEIGHTED, X→WETH on UniV3]→repay path and
/// either nets profit or reverts the profit gate (the 0.05% Aave premium makes a flat round-trip
/// lossy). Mechanical-path proof for the id-27 dispatch.
///
/// Run: forge test --fork-url $ARBITRUM_RPC_URL --fork-block-number 465686728 \
///        --match-path test/MantisBalancerWeighted.Fork.t.sol -vv
interface IBalWeightedPool {
    function getPoolId() external view returns (bytes32);
    function getNormalizedWeights() external view returns (uint256[] memory);
}

interface IBalVaultTokens2 {
    function getPoolTokens(bytes32 poolId)
        external
        view
        returns (address[] memory tokens, uint256[] memory balances, uint256 lastChangeBlock);
}

interface IUniV3FactoryW {
    function getPool(address tokenA, address tokenB, uint24 fee) external view returns (address pool);
}

contract MantisBalancerWeightedForkTest is Test {
    MantisExecutor executor;

    address constant WETH = 0x82aF49447D8a07e3bd95BD0d56f35241523fBab1;
    address constant BAL_VAULT = 0xBA12222222228d8Ba445958a75a0704d566BF2C8;
    address constant AAVE_V3_POOL = 0x794a61358D6845594F94dc1DB02A252b5b4814aD;
    address constant UNISWAP_V3_FACTORY = 0x1F98431c8aD98523631AE4a59f267346ea31F984;

    uint8 constant UNISWAP_V3_ID = 0;
    uint8 constant BALANCER_WEIGHTED_ID = 27;

    error Loss(uint256);
    error InsufficientProfit(uint256 actual, uint256 minimum);

    // Candidate Balancer pools (real, from real_pools.json @ 465686728). Several "BalancerStable"-
    // labelled rows are in fact WEIGHTED pools (the catalog label is imperfect); we filter on-fork
    // by getNormalizedWeights(). Order: WBTC/WETH 50/50 first (the canonical weighted pool).
    function _candidates() internal pure returns (address[5] memory c) {
        c[0] = 0x64541216bAFFFEec8ea535BB71Fbc927831d0595; // WBTC/WETH
        c[1] = 0x651e00FfD5eCfA7F3d4F33d62eDe0a97Cf62EdE2; // WETH/LINK
        c[2] = 0x26D23e37514d27566aBbfd7EDb3AFb1Fc12BD0bB; // (candidate)
        c[3] = 0xee98571dB53e1a68D50A1a016450A0808dB0e4C5; // WETH/USDC.e
        c[4] = 0xE695B4a75F7DDbE8ca57468ABDE97BEbA535F362; // WETH/USDC.e
    }

    function setUp() public {
        require(block.chainid == 42161, "fork Arbitrum One");
        executor = new MantisExecutor(address(this)); // payee == gateway == this
        assertTrue(AAVE_V3_POOL.code.length > 0, "Aave V3 Pool missing on fork");
        assertTrue(BAL_VAULT.code.length > 0, "Balancer Vault missing on fork");
    }

    /// True iff `pool` is a Balancer WEIGHTED pool (exposes getNormalizedWeights with >=2 weights).
    function _isWeighted(address pool) internal view returns (bool) {
        if (pool.code.length == 0) return false;
        try IBalWeightedPool(pool).getNormalizedWeights() returns (uint256[] memory w) {
            return w.length >= 2;
        } catch {
            return false;
        }
    }

    /// Find a real weighted pool that trades {WETH, X} with a Uniswap-V3 WETH/X companion pool.
    function _findWeightedRoute()
        internal
        view
        returns (bytes32 poolId, address other, uint24 v3Fee)
    {
        address[5] memory cands = _candidates();
        uint24[4] memory feeTiers = [uint24(500), uint24(3000), uint24(100), uint24(10000)];
        for (uint256 i = 0; i < cands.length; i++) {
            address pool = cands[i];
            if (!_isWeighted(pool)) continue;
            bytes32 pid = IBalWeightedPool(pool).getPoolId();
            if (address(uint160(uint256(pid) >> 96)) != pool) continue; // Balancer V2 poolId invariant
            (address[] memory toks,,) = IBalVaultTokens2(BAL_VAULT).getPoolTokens(pid);
            // Require WETH membership and pick the partner token.
            address partner;
            bool hasWeth;
            for (uint256 j = 0; j < toks.length; j++) {
                if (toks[j] == WETH) hasWeth = true;
                else if (partner == address(0)) partner = toks[j];
            }
            if (!hasWeth || partner == address(0)) continue;
            // Need a UniV3 WETH/partner companion to close the cycle back to WETH.
            for (uint256 f = 0; f < feeTiers.length; f++) {
                if (IUniV3FactoryW(UNISWAP_V3_FACTORY).getPool(WETH, partner, feeTiers[f]) != address(0)) {
                    return (pid, partner, feeTiers[f]);
                }
            }
        }
        revert("no real Balancer WEIGHTED pool with a UniV3 WETH companion found");
    }

    function _weightedHop(address tin, address tout, bytes32 poolId)
        internal
        pure
        returns (MantisExecutor.TradeHopV2 memory h)
    {
        h.exchangeId = BALANCER_WEIGHTED_ID;
        h.tokenIn = tin;
        h.tokenOut = tout;
        h.poolId = poolId;
    }

    function _v3(address tin, address tout, uint24 fee) internal pure returns (MantisExecutor.TradeHopV2 memory h) {
        h.exchangeId = UNISWAP_V3_ID;
        h.tokenIn = tin;
        h.tokenOut = tout;
        h.fee = fee;
    }

    function _assertRanOrBubble(bytes memory reason) internal pure {
        bytes4 sel;
        assembly {
            sel := mload(add(reason, 0x20))
        }
        if (sel == Loss.selector || sel == InsufficientProfit.selector) return; // weighted leg RAN
        if (reason.length >= 68 && bytes4(reason) == bytes4(0x08c379a0)) {
            assembly {
                reason := add(reason, 0x04)
            }
            revert(abi.decode(reason, (string)));
        }
        revert("Balancer-weighted (id 27) route hit an unexpected (non-profit-gate) revert");
    }

    function test_balancerWeighted_id27_executes_via_aave() public {
        (bytes32 poolId, address other, uint24 v3Fee) = _findWeightedRoute();
        emit log_named_address("weighted partner token", other);
        emit log_named_uint("companion UniV3 fee", v3Fee);

        // WETH --Balancer WEIGHTED(WETH->other)--> other --UniV3(other->WETH)--> WETH. Aave funds WETH.
        MantisExecutor.TradeHopV2[3] memory route;
        route[0] = _weightedHop(WETH, other, poolId);
        route[1] = _v3(other, WETH, v3Fee);

        bool ran;
        try executor.flashLoanArbAaveMixed(0.1 ether, route, 0) {
            ran = true; // a real edge existed -> profit landed on payee
            emit log_string("Balancer-WEIGHTED id-27 route SUCCEEDED (net-positive)");
        } catch (bytes memory reason) {
            _assertRanOrBubble(reason);
            ran = true; // executed the weighted leg then hit the profit gate
            emit log_string("Balancer-WEIGHTED id-27 route executed; profit gate fired (gas charged)");
        }
        assertTrue(ran, "id-27 weighted leg must dispatch to the Vault and run (no unknown-id revert)");
    }
}
