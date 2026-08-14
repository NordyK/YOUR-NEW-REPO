// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/MantisExecutor.sol";

interface IERC20Gate6Diag {
    function balanceOf(address account) external view returns (uint256);
}

contract MantisGate6ExactAmountDiagnosticForkTest is Test {
    MantisExecutor executor;

    address constant USDC = 0xaf88d065e77c8cC2239327C5EDb3A432268e5831;
    address constant USDT0 = 0xFd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9;
    address constant FLUID_POOL = 0x3C0441B42195F4aD6aa9a0978E06096ea616CDa7;
    address constant UNISWAP_V4_POOL_MANAGER = 0x360E68faCcca8cA495c1B759Fd9EEe466db9FB32;
    bytes32 constant V4_USDC_USDT0_POOL_ID =
        0xab05003a63d2f34ac7eec4670bca3319f0e3d2f62af5c2b9cbd69d03fd804fd2;

    uint8 constant UNISWAP_V4_ID = 20;
    uint8 constant FLUID_DEX_ID = 21;

    bytes32 constant TX_464734243 =
        0x30262b91af714c48be7087114d46b145f154ba8253c47a7f75655bb30b989ec7;

    error Loss(uint256);

    function setUp() public {
        require(block.chainid == 42161, "fork Arbitrum One");
    }

    function _fluid(address tIn, address tOut) internal pure returns (MantisExecutor.TradeHopV2 memory h) {
        h.exchangeId = FLUID_DEX_ID;
        h.tokenIn = tIn;
        h.tokenOut = tOut;
        h.pool = FLUID_POOL;
    }

    function _v4(address tIn, address tOut) internal pure returns (MantisExecutor.TradeHopV2 memory h) {
        h.exchangeId = UNISWAP_V4_ID;
        h.tokenIn = tIn;
        h.tokenOut = tOut;
        h.fee = 8;
        h.tickSpacing = 1;
        h.poolManager = UNISWAP_V4_POOL_MANAGER;
        h.poolId = V4_USDC_USDT0_POOL_ID;
    }

    function _route685() internal pure returns (MantisExecutor.TradeHopV2[3] memory r) {
        r[0] = _v4(USDT0, USDC);
        r[1] = _fluid(USDC, USDT0);
    }

    function _probe(uint128 size) internal returns (uint256 profit, bytes4 sel) {
        MantisExecutor.TradeHopV2[3] memory route = _route685();
        uint256 snap = vm.snapshotState();
        uint256 beforeBal = IERC20Gate6Diag(USDT0).balanceOf(address(this));
        try executor.flashLoanArbMixed(size, route, 0) {
            profit = IERC20Gate6Diag(USDT0).balanceOf(address(this)) - beforeBal;
        } catch (bytes memory data) {
            if (data.length >= 4) {
                assembly {
                    sel := mload(add(data, 0x20))
                }
            }
        }
        vm.revertToState(snap);
    }

    function test_route685_engine_amount_and_gate5_sizes() public {
        vm.rollFork(TX_464734243);
        executor = new MantisExecutor(address(this));

        uint128 engineAmount = 23_460_465_395;
        (uint256 engineProfit, bytes4 engineSel) = _probe(engineAmount);
        emit log_named_uint("route685 engine amount raw", engineAmount);
        emit log_named_uint("route685 engine amount profit raw USDT0", engineProfit);
        emit log_named_bytes32("route685 engine amount revert selector", bytes32(engineSel));

        (uint256 profit25k, bytes4 sel25k) = _probe(25_000e6);
        emit log_named_uint("route685 25k profit raw USDT0", profit25k);
        emit log_named_bytes32("route685 25k revert selector", bytes32(sel25k));

        (uint256 profit50k, bytes4 sel50k) = _probe(50_000e6);
        emit log_named_uint("route685 50k profit raw USDT0", profit50k);
        emit log_named_bytes32("route685 50k revert selector", bytes32(sel50k));
    }
}
