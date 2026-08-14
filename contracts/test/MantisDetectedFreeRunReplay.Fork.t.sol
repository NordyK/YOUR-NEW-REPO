// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/MantisExecutor.sol";

/// Diagnostic replay for the two submitted picks preserved from the 2026-06-09 free runs.
///
/// This is intentionally not the full DET-1.1 artifact: it measures executor balance delta at
/// exact-prefix fork state, but does not add Arbitrum L1 data fee or produce the all-pick CSV.
interface IERC20FreeRunReplay {
    function balanceOf(address account) external view returns (uint256);
}

contract MantisDetectedFreeRunReplayForkTest is Test {
    MantisExecutor executor;

    address constant WETH = 0x82aF49447D8a07e3bd95BD0d56f35241523fBab1;
    address constant WBTC = 0x2f2a2543B76A4166549F7aaB2e75Bef0aefC5B0f;
    address constant USDC_NATIVE = 0xaf88d065e77c8cC2239327C5EDb3A432268e5831;

    address constant DODO_WETH_USDC = 0x1666EA6E83BCC3473111732Ef086BA47e6EDDeB7;
    address constant UNISWAP_V4_POOL_MANAGER = 0x360E68faCcca8cA495c1B759Fd9EEe466db9FB32;

    bytes32 constant ROUTE3003_TX = 0xb1c96f8453309db42f949c45e2ed213e5c39f3f32012919750d2ae2ceb2d5945;
    bytes32 constant ROUTE639_TX = 0x0cb6011ba71c8a6b58dc77d32f9a6c86fe3e864e7463509580a3b83eb98eba1c;

    bytes32 constant V4_WBTC_USDC_POOL_ID = 0x70bf44c3a9b6b047bf60e5a05968225dbf3d6a5b9e8a95a73727e48921e889c1;

    uint8 constant UNISWAP_V3_ID = 0;
    uint8 constant PANCAKE_V3_ID = 6;
    uint8 constant CAMELOT_V3_ID = 9;
    uint8 constant UNISWAP_V4_ID = 20;
    uint8 constant DODO_V2_ID = 23;

    error Loss(uint256);

    function setUp() public view {
        require(block.chainid == 42161, "fork Arbitrum One");
    }

    function _empty() internal pure returns (MantisExecutor.TradeHopV2 memory h) {}

    function _legacy(uint8 exchangeId, address tokenIn, address tokenOut, uint24 fee)
        internal
        pure
        returns (MantisExecutor.TradeHopV2 memory h)
    {
        h.exchangeId = exchangeId;
        h.tokenIn = tokenIn;
        h.tokenOut = tokenOut;
        h.fee = fee;
    }

    function _dodo(address tokenIn, address tokenOut, address pool)
        internal
        pure
        returns (MantisExecutor.TradeHopV2 memory h)
    {
        h.exchangeId = DODO_V2_ID;
        h.tokenIn = tokenIn;
        h.tokenOut = tokenOut;
        h.pool = pool;
    }

    function _v4(address tokenIn, address tokenOut, uint24 fee, int24 tickSpacing, bytes32 poolId)
        internal
        pure
        returns (MantisExecutor.TradeHopV2 memory h)
    {
        h.exchangeId = UNISWAP_V4_ID;
        h.tokenIn = tokenIn;
        h.tokenOut = tokenOut;
        h.fee = fee;
        h.tickSpacing = tickSpacing;
        h.poolManager = UNISWAP_V4_POOL_MANAGER;
        h.poolId = poolId;
    }

    function _probe(address profitToken, uint128 amountIn, MantisExecutor.TradeHopV2[3] memory route)
        internal
        returns (uint256 profit, bytes4 sel, uint256 gasUsed, uint256 shortfall)
    {
        uint256 snap = vm.snapshotState();
        uint256 beforeBalance = IERC20FreeRunReplay(profitToken).balanceOf(address(this));
        uint256 gasBefore = gasleft();

        try executor.flashLoanArbMixed(amountIn, route, 0) {
            gasUsed = gasBefore - gasleft();
            profit = IERC20FreeRunReplay(profitToken).balanceOf(address(this)) - beforeBalance;
        } catch (bytes memory data) {
            gasUsed = gasBefore - gasleft();
            assembly {
                sel := mload(add(data, 0x20))
            }
            if (sel == Loss.selector) {
                assembly {
                    shortfall := mload(add(data, 0x24))
                }
            }
        }

        vm.revertToState(snap);
    }

    function test_route3003_exact_prefix_balance_delta() public {
        vm.rollFork(ROUTE3003_TX);
        emit log_named_uint("route3003 observed block", block.number);
        assertEq(block.number, 471733538, "route3003 exact-prefix block mismatch");

        executor = new MantisExecutor(address(this));
        MantisExecutor.TradeHopV2[3] memory route;
        route[0] = _legacy(PANCAKE_V3_ID, WETH, WBTC, 100);
        route[1] = _v4(WBTC, USDC_NATIVE, 3000, 60, V4_WBTC_USDC_POOL_ID);
        route[2] = _legacy(UNISWAP_V3_ID, USDC_NATIVE, WETH, 500);

        (uint256 profit, bytes4 sel, uint256 gasUsed, uint256 shortfall) = _probe(WETH, 442_580_510_746_630_569, route);
        emit log_named_uint("route3003 amount_in WETH wei", 442_580_510_746_630_569);
        emit log_named_uint("route3003 balance_delta WETH wei before L1 fee", profit);
        emit log_named_uint("route3003 loss shortfall WETH wei", shortfall);
        emit log_named_uint("route3003 gasUsed", gasUsed);
        emit log_named_bytes32("route3003 revert selector", bytes32(sel));
    }

    function test_route639_submitted_usdc_direction_exact_prefix_balance_delta() public {
        vm.rollFork(ROUTE639_TX);
        emit log_named_uint("route639 observed block", block.number);
        assertEq(block.number, 471763559, "route639 exact-prefix block mismatch");

        executor = new MantisExecutor(address(this));
        MantisExecutor.TradeHopV2[3] memory route;
        route[0] = _dodo(USDC_NATIVE, WETH, DODO_WETH_USDC);
        route[1] = _legacy(CAMELOT_V3_ID, WETH, USDC_NATIVE, 0);
        route[2] = _empty();

        (uint256 profit, bytes4 sel, uint256 gasUsed, uint256 shortfall) = _probe(USDC_NATIVE, 762_320_410, route);
        emit log_named_uint("route639 submitted amount_in USDC raw", 762_320_410);
        emit log_named_uint("route639 balance_delta USDC raw before L1 fee", profit);
        emit log_named_uint("route639 loss shortfall USDC raw", shortfall);
        emit log_named_uint("route639 gasUsed", gasUsed);
        emit log_named_bytes32("route639 revert selector", bytes32(sel));
    }

    function test_route639_candidate_weth_direction_exact_prefix_balance_delta() public {
        vm.rollFork(ROUTE639_TX);
        emit log_named_uint("route639 candidate observed block", block.number);
        assertEq(block.number, 471763559, "route639 exact-prefix block mismatch");

        executor = new MantisExecutor(address(this));
        MantisExecutor.TradeHopV2[3] memory route;
        route[0] = _dodo(WETH, USDC_NATIVE, DODO_WETH_USDC);
        route[1] = _legacy(CAMELOT_V3_ID, USDC_NATIVE, WETH, 0);
        route[2] = _empty();

        uint128 candidateWethAmount = 464_647_301_179_757_900;
        (uint256 profit, bytes4 sel, uint256 gasUsed, uint256 shortfall) = _probe(WETH, candidateWethAmount, route);
        emit log_named_uint("route639 candidate amount_in WETH wei", candidateWethAmount);
        emit log_named_uint("route639 candidate balance_delta WETH wei before L1 fee", profit);
        emit log_named_uint("route639 candidate loss shortfall WETH wei", shortfall);
        emit log_named_uint("route639 candidate gasUsed", gasUsed);
        emit log_named_bytes32("route639 candidate revert selector", bytes32(sel));
    }
}
