// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/MantisExecutor.sol";

/// GAP 1 — AAVE-FUNDED EXECUTION through a REAL Balancer-pool leg.
///
/// Until now no fork test drove the Aave funding entrypoint (`flashLoanArbAaveMixed`) through a
/// REAL venue swap to a realized result. A Balancer-leg route is the natural fit: Aave is its ONLY
/// legal funding (the Balancer Vault is non-reentrant, so a Balancer pool cannot be both the flash
/// source AND a swap venue — see `flashLoanArbMixed`'s `_routeTradesBalancer` guard). This test:
///   1. discovers the REAL Balancer poolId on-fork via `getPoolId()` (no hard-coded poolId),
///   2. confirms the pool trades the WETH/WBTC pair we route through,
///   3. drives Aave→ WETH flash → [WETH→WBTC on Balancer (Vault GIVEN_IN swap), WBTC→WETH on
///      Uniswap V3] → repay Aave (amount + 0.05% premium) → Loss / profit gate.
///
/// Real pool: Balancer V2 50WBTC-50WETH pool 0x6454…0595 (from scratchpad real_pools.json @
/// head_block 465686728; its membership of {WBTC,WETH} is asserted on-fork via the Vault's
/// getPoolTokens, so the test fails loudly if the address ever drifts). This pool is in fact a
/// WEIGHTED pool, which is fine: the executor dispatches BOTH the stable id (26) and weighted id
/// (27) to the IDENTICAL Vault single-swap `_swapExactInBalancer` (the Vault then routes to the
/// pool's own invariant math by poolId — MantisExecutor L1996-2000). We use id 26 here; the Aave
/// funding + Vault single-swap + repay is exactly what this gap proves.
/// Funding: Aave V3 Pool 0x794a…14aD on Arbitrum One, asset = WETH.
///
/// ACCEPTANCE (per the execution-gap brief): EXECUTE the full Aave-funded path; if a real
/// cross-venue edge exists in the cached state, assert realized WETH profit > 0 on the payee, else
/// the path is proven by reverting the executor's own `Loss`/`InsufficientProfit` gate (gas
/// charged). The Balancer leg's input is capped below the Vault MAX_IN_RATIO (~30%; BAL#304) by
/// reading the pool's live WETH balance on-fork — that is the Balancer pool's limit, not the
/// executor's. The path must NOT fail with a decode / unknown-id / funding-guard / Vault revert.
///
/// Run: forge test --fork-url $ARBITRUM_RPC_URL --fork-block-number 465686728 \
///        --match-path test/MantisAaveFundedProfit.Fork.t.sol -vv
/// (465686728 = the flagship PARENT pin used across the suite — definitely in the warm cache.)
interface IWETHd1 {
    function balanceOf(address) external view returns (uint256);
}

interface IBalPoolId {
    function getPoolId() external view returns (bytes32);
}

interface IBalVaultTokens {
    function getPoolTokens(bytes32 poolId)
        external
        view
        returns (address[] memory tokens, uint256[] memory balances, uint256 lastChangeBlock);
}

contract MantisAaveFundedProfitForkTest is Test {
    MantisExecutor executor;

    address constant WETH = 0x82aF49447D8a07e3bd95BD0d56f35241523fBab1;
    address constant WBTC = 0x2f2a2543B76A4166549F7aaB2e75Bef0aefC5B0f;
    address constant BAL_VAULT = 0xBA12222222228d8Ba445958a75a0704d566BF2C8;
    address constant AAVE_V3_POOL = 0x794a61358D6845594F94dc1DB02A252b5b4814aD;
    // Balancer V2 WBTC/WETH pool (real, from real_pools.json BalancerStable[0] @ 465686728).
    address constant BAL_WBTC_WETH = 0x64541216bAFFFEec8ea535BB71Fbc927831d0595;

    uint8 constant UNISWAP_V3_ID = 0;
    uint8 constant BALANCER_STABLE_ID = 26;

    error Loss(uint256);
    error InsufficientProfit(uint256 actual, uint256 minimum);

    function setUp() public {
        require(block.chainid == 42161, "fork Arbitrum One");
        executor = new MantisExecutor(address(this)); // payee == gateway == this -> profit lands here
        assertTrue(AAVE_V3_POOL.code.length > 0, "Aave V3 Pool missing on fork");
        assertTrue(BAL_VAULT.code.length > 0, "Balancer Vault missing on fork");
        assertTrue(BAL_WBTC_WETH.code.length > 0, "Balancer WBTC/WETH pool missing on fork");
    }

    function _balHop(address tin, address tout, bytes32 poolId)
        internal
        pure
        returns (MantisExecutor.TradeHopV2 memory h)
    {
        h.exchangeId = BALANCER_STABLE_ID;
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

    /// Resolve the REAL Balancer poolId on-fork, assert the pool holds {WETH, WBTC}, and return the
    /// live WETH balance (used to cap the swept input below the pool's MAX_IN_RATIO, ~30%).
    function _resolveBalancerPool() internal view returns (bytes32 poolId, uint256 wethBalance) {
        poolId = IBalPoolId(BAL_WBTC_WETH).getPoolId();
        // The poolId's top 20 bytes MUST equal the pool address (Balancer V2 invariant).
        assertEq(address(uint160(uint256(poolId) >> 96)), BAL_WBTC_WETH, "poolId/address mismatch");
        (address[] memory toks, uint256[] memory bals,) = IBalVaultTokens(BAL_VAULT).getPoolTokens(poolId);
        bool hasWeth;
        bool hasWbtc;
        for (uint256 i = 0; i < toks.length; i++) {
            if (toks[i] == WETH) {
                hasWeth = true;
                wethBalance = bals[i];
            }
            if (toks[i] == WBTC) hasWbtc = true;
        }
        assertTrue(hasWeth && hasWbtc, "Balancer pool does not trade WETH/WBTC");
        assertGt(wethBalance, 0, "Balancer pool has no WETH balance at this block");
    }

    /// True iff the revert is the executor's own profit gate (the Aave path RAN end-to-end), or a
    /// bubbled string revert which we surface. Returns whether it was a clean profit-gate Loss.
    function _assertRanOrBubble(bytes memory reason) internal pure returns (bool ranButLossy) {
        bytes4 sel;
        assembly {
            sel := mload(add(reason, 0x20))
        }
        if (sel == Loss.selector || sel == InsufficientProfit.selector) {
            return true; // executor executed all legs, then hit its own profit gate
        }
        // A funding-guard / decode / unknown-id failure would be a bare string revert — surface it.
        if (reason.length >= 68 && bytes4(reason) == bytes4(0x08c379a0)) {
            assembly {
                reason := add(reason, 0x04)
            }
            revert(abi.decode(reason, (string)));
        }
        revert("Aave-funded Balancer route hit an unexpected (non-profit-gate) revert");
    }

    function test_aaveFunded_balancerLeg_executes_to_profit_or_loss() public {
        (bytes32 poolId, uint256 poolWeth) = _resolveBalancerPool();

        // Cycle: WETH --Balancer(WETH->WBTC)--> WBTC --UniV3 500(WBTC->WETH)--> WETH. Aave funds WETH.
        MantisExecutor.TradeHopV2[3] memory route;
        route[0] = _balHop(WETH, WBTC, poolId);
        route[1] = _v3(WBTC, WETH, 500);
        // route[2] inactive -> 2-hop closed cycle (roundtrips to WETH).

        // CAP the WETH input below Balancer's MAX_IN_RATIO. The Vault rejects any single-swap input
        // exceeding ~30% of the in-token balance (error BAL#304); we cap at 25% to stay clear. This
        // is the Balancer pool's constraint, NOT the executor's — the path itself runs fine (the
        // smallest sizes already realize positive WETH on the cached state).
        uint256 maxIn = (poolWeth * 25) / 100;
        emit log_named_uint("Balancer pool WETH balance", poolWeth);
        emit log_named_uint("max viable WETH input (25% of balance)", maxIn);

        // Sweep realistic WETH sizes (the engine's sizer job). Only sizes within the MAX_IN_RATIO cap
        // are viable through the Balancer leg; larger ones are skipped (Balancer-side limit, not a
        // path failure). Snapshot per size so a winning leg can't consume the edge for later sizes.
        uint128[10] memory sizes = [
            uint128(0.001 ether), 0.005 ether, 0.01 ether, 0.05 ether, 0.1 ether, 0.25 ether, 0.5 ether, 1 ether, 2 ether, 5 ether
        ];

        uint256 viable;
        uint256 ranCount;
        uint256 bestProfit;
        uint128 bestSize;
        for (uint256 i = 0; i < sizes.length; i++) {
            if (uint256(sizes[i]) > maxIn) continue; // would trip Balancer MAX_IN_RATIO — skip
            viable++;
            uint256 snap = vm.snapshotState();
            uint256 bal0 = IWETHd1(WETH).balanceOf(address(this));
            try executor.flashLoanArbAaveMixed(sizes[i], route, 0) {
                uint256 profit = IWETHd1(WETH).balanceOf(address(this)) - bal0;
                ranCount++;
                if (profit > bestProfit) {
                    bestProfit = profit;
                    bestSize = sizes[i];
                }
                emit log_named_uint(string.concat("size ", vm.toString(sizes[i]), " -> realized WETH"), profit);
            } catch (bytes memory reason) {
                if (_assertRanOrBubble(reason)) ranCount++; // executed full path, profit gate fired
            }
            vm.revertToState(snap);
        }

        // At least one in-cap size must exist, and EVERY in-cap size must execute the full
        // Aave-funded Balancer path (success or the executor's own profit gate).
        assertGt(viable, 0, "no swept size fits under the Balancer MAX_IN_RATIO cap");
        assertEq(ranCount, viable, "every viable size must execute the full Aave-funded Balancer path");
        emit log_named_uint("Aave-funded sizes that ran end-to-end", ranCount);

        if (bestProfit > 0) {
            // A real edge exists in cached state — execute it for keeps and confirm WETH lands on payee.
            uint256 before = IWETHd1(WETH).balanceOf(address(this));
            executor.flashLoanArbAaveMixed(bestSize, route, 0);
            uint256 realized = IWETHd1(WETH).balanceOf(address(this)) - before;
            assertGt(realized, 0, "detected edge must net positive WETH via the Aave path");
            emit log_named_uint("AAVE-FUNDED realized profit (wei)", realized);
        } else {
            emit log_string("No cross-venue edge at this block: Aave path proven via clean Loss/InsufficientProfit (gas charged)");
        }
    }
}
