// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/MantisExecutor.sol";

interface IERC20CatalogReplay {
    function balanceOf(address account) external view returns (uint256);
}

interface IV3CatalogReplay {
    function fee() external view returns (uint24);
}

/// Replays local `anvil/anvil_catalog.csv` seeds only after segment-level reconstruction.
///
/// The 3-hop seed below came from Dune query 7717101 against `dex.trades`:
///   tx 0x49fe...f6b0, block 462907520
///   WETH -> USDC  on Pancake V3 pool 0x7fcd...
///   USDC -> PENDLE on Uniswap V3 pool 0x3622...
///   PENDLE -> WETH on Uniswap V3 pool 0xb08a...
///
/// Pass rule: exact-prefix replay must succeed and increase this test contract's WETH balance.
contract MantisAnvilCatalogPositiveReplayForkTest is Test {
    MantisExecutor executor;

    address constant WETH = 0x82aF49447D8a07e3bd95BD0d56f35241523fBab1;
    address constant USDC = 0xaf88d065e77c8cC2239327C5EDb3A432268e5831;
    address constant PENDLE = 0x0c880f6761F1af8d9Aa9C466984b80DAb9a8c9e8;
    address constant WBTC = 0x2f2a2543B76A4166549F7aaB2e75Bef0aefC5B0f;
    address constant USDT0 = 0xFd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9;

    address constant PANCAKE_WETH_USDC = 0x7fCDC35463E3770c2fB992716Cd070B63540b947;
    address constant UNI_USDC_PENDLE = 0x3622D2E9ab0025621aB3C8cfEbDbD27bc02A8d1e;
    address constant UNI_PENDLE_WETH = 0xB08a8794A5D3cCCA3725D92964696858d3201909;
    address constant UNI_WETH_WBTC = 0x2f5e87C9312fa29aed5c179E456625D79015299c;
    address constant UNI_WBTC_USDT0 = 0x5969EFddE3cF5C0D9a88aE51E47d721096A97203;
    address constant UNI_USDT0_USDC = 0xbE3aD6a5669Dc0B8b12FeBC03608860C31E2eef6;
    address constant UNI_USDC_WETH = 0xC6962004f452bE9203591991D15f6b388e09E8D0;

    uint8 constant UNISWAP_V3_ID = 0;
    uint8 constant PANCAKE_V3_ID = 6;

    bytes32 constant TX_3HOP_PANCAKE_UNI = 0x49fe181b6dd857ca871976ea2f3619acc41dae20858764bdf6cdb91c5962f6b0;
    uint128 constant TX_3HOP_WETH_INPUT = 3_572_891_540_303_330;
    bytes32 constant TX_4HOP_UNI_V3 = 0x004d7ccb8f83cd09b76f591298a21e22006fca88e6e6318bcbf93b5e72d43d94;
    uint128 constant TX_4HOP_WETH_INPUT = 4_701_185_202_312_152_746;

    error Loss(uint256);

    function setUp() public {
        require(block.chainid == 42161, "fork Arbitrum One");
    }

    function _v3(uint8 exchangeId, address tokenIn, address tokenOut, address pool)
        internal
        view
        returns (MantisExecutor.TradeHopV2 memory hop)
    {
        hop.exchangeId = exchangeId;
        hop.tokenIn = tokenIn;
        hop.tokenOut = tokenOut;
        hop.fee = IV3CatalogReplay(pool).fee();
        hop.pool = pool;
    }

    function _threeHopPancakeUniRoute() internal view returns (MantisExecutor.TradeHopV2[3] memory route) {
        route[0] = _v3(PANCAKE_V3_ID, WETH, USDC, PANCAKE_WETH_USDC);
        route[1] = _v3(UNISWAP_V3_ID, USDC, PENDLE, UNI_USDC_PENDLE);
        route[2] = _v3(UNISWAP_V3_ID, PENDLE, WETH, UNI_PENDLE_WETH);
    }

    function _fourHopUniV3Route() internal view returns (MantisExecutor.TradeHopV2[4] memory route) {
        route[0] = _v3(UNISWAP_V3_ID, WETH, WBTC, UNI_WETH_WBTC);
        route[1] = _v3(UNISWAP_V3_ID, WBTC, USDT0, UNI_WBTC_USDT0);
        route[2] = _v3(UNISWAP_V3_ID, USDT0, USDC, UNI_USDT0_USDC);
        route[3] = _v3(UNISWAP_V3_ID, USDC, WETH, UNI_USDC_WETH);
    }

    function _probe3Hop(uint128 amountIn) internal returns (uint256 profit, bytes4 selector, uint256 shortfall) {
        MantisExecutor.TradeHopV2[3] memory route = _threeHopPancakeUniRoute();
        uint256 snap = vm.snapshotState();
        uint256 beforeBalance = IERC20CatalogReplay(WETH).balanceOf(address(this));

        try executor.flashLoanArbMixed(amountIn, route, 0) {
            profit = IERC20CatalogReplay(WETH).balanceOf(address(this)) - beforeBalance;
        } catch (bytes memory data) {
            if (data.length >= 4) {
                assembly {
                    selector := mload(add(data, 0x20))
                }
                if (selector == Loss.selector && data.length >= 36) {
                    assembly {
                        shortfall := mload(add(data, 0x24))
                    }
                }
            }
        }

        vm.revertToState(snap);
    }

    function _probe4Hop(uint128 amountIn) internal returns (uint256 profit, bytes4 selector, uint256 shortfall) {
        MantisExecutor.TradeHopV2[4] memory route = _fourHopUniV3Route();
        uint256 snap = vm.snapshotState();
        uint256 beforeBalance = IERC20CatalogReplay(WETH).balanceOf(address(this));

        try executor.flashLoanArbMixed4(amountIn, route, 0) {
            profit = IERC20CatalogReplay(WETH).balanceOf(address(this)) - beforeBalance;
        } catch (bytes memory data) {
            if (data.length >= 4) {
                assembly {
                    selector := mload(add(data, 0x20))
                }
                if (selector == Loss.selector && data.length >= 36) {
                    assembly {
                        shortfall := mload(add(data, 0x24))
                    }
                }
            }
        }

        vm.revertToState(snap);
    }

    function test_3hop_pancake_uniswap_catalog_seed_positive_realized() public {
        vm.rollFork(TX_3HOP_PANCAKE_UNI);
        assertEq(block.number, 462_907_520, "rollFork did not reach candidate block");

        executor = new MantisExecutor(address(this));
        (uint256 profit, bytes4 selector, uint256 shortfall) = _probe3Hop(TX_3HOP_WETH_INPUT);

        emit log_named_uint("3hop amount input WETH wei", TX_3HOP_WETH_INPUT);
        emit log_named_uint("3hop exact-prefix realized WETH profit wei", profit);
        emit log_named_bytes32("3hop exact-prefix revert selector", bytes32(selector));
        emit log_named_uint("3hop exact-prefix loss shortfall wei", shortfall);

        assertEq(selector, bytes4(0), "3-hop exact-prefix replay reverted");
        assertGt(profit, 0, "3-hop exact-prefix replay did not realize positive WETH");
    }

    function test_4hop_uniswap_v3_catalog_seed_positive_realized() public {
        vm.rollFork(TX_4HOP_UNI_V3);
        assertEq(block.number, 463_931_868, "rollFork did not reach candidate block");

        executor = new MantisExecutor(address(this));
        (uint256 profit, bytes4 selector, uint256 shortfall) = _probe4Hop(TX_4HOP_WETH_INPUT);

        emit log_named_uint("4hop amount input WETH wei", TX_4HOP_WETH_INPUT);
        emit log_named_uint("4hop exact-prefix realized WETH profit wei", profit);
        emit log_named_bytes32("4hop exact-prefix revert selector", bytes32(selector));
        emit log_named_uint("4hop exact-prefix loss shortfall wei", shortfall);

        assertEq(selector, bytes4(0), "4-hop exact-prefix replay reverted");
        assertGt(profit, 0, "4-hop exact-prefix replay did not realize positive WETH");
    }
}
