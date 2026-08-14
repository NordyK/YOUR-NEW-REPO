// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

/// ISOLATION DIAGNOSTIC (read-only): is the V4 singleton PoolManager state the engine validates
/// against (parent/snapshot-pinned, only the trigger tx re-executed) the SAME as the real pre-arb
/// state (full block prefix via rollFork)? For route 268 (UniswapV4 0x80c735c5.. -> DodoV2), the
/// engine's REVM said SUCCESS (+0.0142 WETH) but exact calldata Loss-reverts on a full-prefix fork.
/// Hypothesis: V4 is a SINGLETON PoolManager touched by many block txs; parent-pinned + trigger-only
/// state is stale, so the engine over-prices the V4 hop. V2/V3 per-pair pools don't have this.
///
/// Run: forge test --fork-url $ARBITRUM_RPC_URL --fork-block-number 465686728 \
///        --match-path test/V4StateDivergenceDiag.Fork.t.sol -vv
interface IStateView {
    function getSlot0(bytes32 poolId)
        external
        view
        returns (uint160 sqrtPriceX96, int24 tick, uint24 protocolFee, uint24 lpFee);
    function getLiquidity(bytes32 poolId) external view returns (uint128 liquidity);
}

contract V4StateDivergenceDiag is Test {
    address constant STATE_VIEW = 0x76Fd297e2D437cd7f76d50F01AfE6160f86e9990;
    bytes32 constant POOL_ID = 0x80c735c5a0222241f211b3edb8df2ccefad94553ec18f1c29143f0399c78f500;
    bytes32 constant TRIGGER_TX = 0xc870b79186962e5a5612801b0f43798328a6531d75edab8eca33bdd198ba2cec;

    function test_v4_singleton_state_parent_vs_prearb() public {
        require(block.chainid == 42161, "fork Arbitrum One");
        emit log_named_uint("fork block at start (engine parent pin)", block.number);

        // (a) Parent / snapshot-pinned state — what the engine's REVM fork DB baseline sees.
        (uint160 pSqrt, int24 pTick,,) = IStateView(STATE_VIEW).getSlot0(POOL_ID);
        uint128 pLiq = IStateView(STATE_VIEW).getLiquidity(POOL_ID);

        // (b) Exact pre-arb state — full block-465686729 prefix up to the trigger tx.
        vm.rollFork(TRIGGER_TX);
        emit log_named_uint("fork block after rollFork(trigger)", block.number);
        (uint160 aSqrt, int24 aTick,,) = IStateView(STATE_VIEW).getSlot0(POOL_ID);
        uint128 aLiq = IStateView(STATE_VIEW).getLiquidity(POOL_ID);

        emit log_named_uint("parent  sqrtPriceX96", pSqrt);
        emit log_named_uint("pre-arb sqrtPriceX96", aSqrt);
        emit log_named_int("parent  tick", pTick);
        emit log_named_int("pre-arb tick", aTick);
        emit log_named_uint("parent  liquidity", pLiq);
        emit log_named_uint("pre-arb liquidity", aLiq);

        if (pSqrt != aSqrt || pLiq != aLiq || pTick != aTick) {
            emit log_string(
                "DIVERGENT: V4 singleton pool state changed within block 465686729 before the trigger "
                "-> the engine's parent-pinned/trigger-only REVM validates the V4 hop against STALE "
                "state -> over-prices it -> spurious success. Confirms the singleton-state root cause."
            );
        } else {
            emit log_string(
                "IDENTICAL: V4 pool state unchanged parent->pre-arb -> singleton-state staleness is NOT "
                "the cause for this route; look at default-zero fetch or calldata/metadata instead."
            );
        }
    }
}
