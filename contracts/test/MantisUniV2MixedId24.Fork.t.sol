// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/MantisExecutor.sol";

/// GAP 4 — UNISWAP V2 (exchangeId 24) is engine-reachable but UNTESTED in any executor fork test.
///
/// The executor supports a true Uniswap V2 deployment on Arbitrum (factory 0xf1D7…bcf9, router
/// 0x4752…aD24), distinct from SushiV2/CamelotV2. Its leg is dispatched in `_swapExactInHop` via the
/// UniV2 router `swapExactTokensForTokens` (MantisExecutor L2065-2071), and as a flash-swap ENTRY hop
/// the pair is resolved through `UNISWAP_V2_FACTORY.getPair` and callback-verified against the same
/// (MantisExecutor L862-864, L889-892). This test drives a UniV2 (id 24) leg through
/// `flashLoanArbMixed` and asserts the path RUNS.
///
/// Real pool: Uniswap V2 WETH/USDC pair 0xf64d…366a (from scratchpad real_pools.json UniswapV2[0] @
/// head_block 465686728). Its WETH membership and on-chain reserves are asserted on-fork, so the
/// test fails loudly if the pair ever drifts. We DON'T pin the pool in the hop (id-24 is router-/
/// factory-resolved by token pair); the address is only used to read token0/token1 + sanity reserves.
///
/// Route: WETH --UniV2 id24(WETH->USDC)--> USDC --UniV3 500(USDC->WETH)--> WETH, Balancer-flash-
/// funded (no Balancer leg, so the Balancer-flash path is legal). The opposite leg is UniV3 so the
/// edge, if any, is a genuine cross-venue UniV2↔UniV3 dislocation rather than a self-trade.
///
/// ACCEPTANCE: executes the full mixed path through the id-24 UniV2 swap and either nets profit or
/// reverts `Loss` (a UniV2↔V3 WETH/USDC round-trip with two fees is normally lossy) — NOT a decode /
/// unknown-id / "Invalid V2 callback" revert. Mechanical-path proof for id-24 over the MIXED path.
///
/// Run: forge test --fork-url $ARBITRUM_RPC_URL --fork-block-number 465686728 \
///        --match-path test/MantisUniV2MixedId24.Fork.t.sol -vv
interface IUniV2PairR {
    function token0() external view returns (address);
    function token1() external view returns (address);
    function getReserves() external view returns (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast);
}

interface IWETHd24 {
    function balanceOf(address) external view returns (uint256);
}

contract MantisUniV2MixedId24ForkTest is Test {
    MantisExecutor executor;

    address constant WETH = 0x82aF49447D8a07e3bd95BD0d56f35241523fBab1;
    address constant USDC_NATIVE = 0xaf88d065e77c8cC2239327C5EDb3A432268e5831;
    // Uniswap V2 WETH/USDC pair (real, from real_pools.json UniswapV2[0] @ 465686728).
    address constant UNIV2_WETH_USDC = 0xF64Dfe17C8b87F012FCf50FbDA1D62bfA148366a;

    uint8 constant UNISWAP_V3_ID = 0;
    uint8 constant UNISWAP_V2_ID = 24;

    error Loss(uint256);
    error InsufficientProfit(uint256 actual, uint256 minimum);

    function setUp() public {
        require(block.chainid == 42161, "fork Arbitrum One");
        executor = new MantisExecutor(address(this)); // payee == gateway == this
        assertTrue(UNIV2_WETH_USDC.code.length > 0, "UniV2 WETH/USDC pair missing on fork");
    }

    function _univ2(address tin, address tout) internal pure returns (MantisExecutor.TradeHopV2 memory h) {
        h.exchangeId = UNISWAP_V2_ID; // pool/fee resolved by router + factory.getPair(tokenIn, tokenOut)
        h.tokenIn = tin;
        h.tokenOut = tout;
    }

    function _v3(address tin, address tout, uint24 fee) internal pure returns (MantisExecutor.TradeHopV2 memory h) {
        h.exchangeId = UNISWAP_V3_ID;
        h.tokenIn = tin;
        h.tokenOut = tout;
        h.fee = fee;
    }

    function test_univ2_id24_executes_via_flashLoanArbMixed() public {
        // Assert the real UniV2 pair holds WETH/USDC with live reserves before routing through it.
        address t0 = IUniV2PairR(UNIV2_WETH_USDC).token0();
        address t1 = IUniV2PairR(UNIV2_WETH_USDC).token1();
        assertTrue(
            (t0 == WETH && t1 == USDC_NATIVE) || (t0 == USDC_NATIVE && t1 == WETH),
            "pair is not the UniV2 WETH/USDC pool"
        );
        (uint112 r0, uint112 r1,) = IUniV2PairR(UNIV2_WETH_USDC).getReserves();
        assertTrue(r0 > 0 && r1 > 0, "UniV2 pair has no liquidity at this block");
        emit log_named_uint("UniV2 reserve0", uint256(r0));
        emit log_named_uint("UniV2 reserve1", uint256(r1));

        // WETH --UniV2 id24(WETH->USDC)--> USDC --UniV3 500(USDC->WETH)--> WETH.
        MantisExecutor.TradeHopV2[3] memory route;
        route[0] = _univ2(WETH, USDC_NATIVE);
        route[1] = _v3(USDC_NATIVE, WETH, 500);

        bool ran;
        uint256 before = IWETHd24(WETH).balanceOf(address(this));
        try executor.flashLoanArbMixed(0.1 ether, route, 0) {
            ran = true; // a real edge existed -> profit landed on payee
            uint256 realized = IWETHd24(WETH).balanceOf(address(this)) - before;
            emit log_named_uint("UniV2 id-24 MIXED route SUCCEEDED, realized WETH", realized);
            assertGt(realized, 0, "successful id-24 route must net positive WETH to payee");
        } catch (bytes memory reason) {
            bytes4 sel;
            assembly {
                sel := mload(add(reason, 0x20))
            }
            if (sel == Loss.selector || sel == InsufficientProfit.selector) {
                ran = true; // the id-24 UniV2 swap RAN; profit gate fired
                emit log_string("UniV2 id-24 MIXED route executed; profit gate fired (gas charged)");
            } else if (reason.length >= 68 && bytes4(reason) == bytes4(0x08c379a0)) {
                assembly {
                    reason := add(reason, 0x04)
                }
                revert(abi.decode(reason, (string)));
            } else {
                revert("UniV2 id-24 mixed route hit an unexpected (non-profit-gate) revert");
            }
        }
        assertTrue(ran, "UniV2 id-24 leg must execute through flashLoanArbMixed (no unknown-id / callback revert)");
    }
}
