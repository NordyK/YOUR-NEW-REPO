// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/MantisExecutor.sol";

/// Gate 5 diagnostic for the Dune `461623756 / 0xe066...` Fluid v1 -> Uniswap v4 candidate.
///
/// This contains both a parent-block diagnostic and an exact-prefix forced-route oracle. It is not
/// a full certification close by itself: Gate 5 still requires the production bot size/profit to be
/// compared against the exact-prefix oracle from the runbook.
///
/// Run:
///   forge test --fork-url $ARBITRUM_RPC_URL --fork-block-number 461623755 \
///     --match-path test/MantisGate5FluidV4Parent.Fork.t.sol -vv
interface IERC20Gate5 {
    function balanceOf(address account) external view returns (uint256);
}

contract MantisGate5FluidV4ParentForkTest is Test {
    MantisExecutor executor;

    address constant USDC = 0xaf88d065e77c8cC2239327C5EDb3A432268e5831;
    address constant USDT0 = 0xFd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9;
    address constant FLUID_POOL = 0x3C0441B42195F4aD6aa9a0978E06096ea616CDa7;
    address constant UNISWAP_V3_USDC_USDT0_500 = 0xbcE73c2e5A623054B0e8e2428E956f4b9d0412a5;
    address constant UNISWAP_V4_POOL_MANAGER = 0x360E68faCcca8cA495c1B759Fd9EEe466db9FB32;
    address constant BALANCER_VAULT = 0xBA12222222228d8Ba445958a75a0704d566BF2C8;
    address constant AAVE_V3_POOL = 0x794a61358D6845594F94dc1DB02A252b5b4814aD;
    bytes32 constant V4_USDC_USDT0_POOL_ID = 0xab05003a63d2f34ac7eec4670bca3319f0e3d2f62af5c2b9cbd69d03fd804fd2;
    bytes32 constant E066_TX_HASH = 0xe066bc75ef21eac18846a3b2cdbe2409207abb5e1e48d19de822abd5ccc9bb23;

    uint8 constant UNISWAP_V3_ID = 0;
    uint8 constant UNISWAP_V4_ID = 20;
    uint8 constant FLUID_DEX_ID = 21;

    error Loss(uint256);

    uint128 constant BOT_ROUTE_723_INPUT_USDT0_RAW = 23_301_642_811;
    uint256 constant BOT_ROUTE_723_GROSS_USDT0_RAW = 7_738_889;
    uint256 constant BOT_ROUTE_723_REVM_DELTA_USDT0_RAW = 825_133;
    uint128 constant BOT_ROUTE_877_INPUT_USDT0_RAW = 2_035_173_711;
    uint256 constant BOT_ROUTE_877_GROSS_USDT0_RAW = 419_707;
    uint256 constant BOT_ROUTE_877_REVM_DELTA_USDT0_RAW = 419_707;
    uint128 constant BOT_ROUTE_712_INPUT_USDC_RAW = 2_468_202_899;
    uint256 constant BOT_ROUTE_712_GROSS_USDC_RAW = 648_997;
    uint256 constant BOT_ROUTE_712_REVM_DELTA_USDC_RAW = 233_177;

    function setUp() public {
        require(block.chainid == 42161, "fork Arbitrum One");
    }

    function _assertRequiredContractsPresent() internal view {
        assertGt(USDC.code.length, 0, "USDC missing on fork/load-state");
        assertGt(USDT0.code.length, 0, "USDT0 missing on fork/load-state");
        assertGt(FLUID_POOL.code.length, 0, "Fluid pool missing on fork/load-state");
        assertGt(UNISWAP_V3_USDC_USDT0_500.code.length, 0, "Uniswap V3 USDC/USDt0 pool missing");
        assertGt(UNISWAP_V4_POOL_MANAGER.code.length, 0, "V4 PoolManager missing on fork/load-state");
        assertGt(BALANCER_VAULT.code.length, 0, "Balancer Vault missing on fork/load-state");
        assertGt(AAVE_V3_POOL.code.length, 0, "Aave V3 Pool missing on fork/load-state");
    }

    function test_gate5_e066_parent_state_required_contracts_present() public view {
        _assertRequiredContractsPresent();
    }

    function _fluid(address tokenIn, address tokenOut) internal pure returns (MantisExecutor.TradeHopV2 memory hop) {
        hop.exchangeId = FLUID_DEX_ID;
        hop.tokenIn = tokenIn;
        hop.tokenOut = tokenOut;
        hop.pool = FLUID_POOL;
    }

    function _v3(address tokenIn, address tokenOut) internal pure returns (MantisExecutor.TradeHopV2 memory hop) {
        hop.exchangeId = UNISWAP_V3_ID;
        hop.tokenIn = tokenIn;
        hop.tokenOut = tokenOut;
        hop.fee = 500;
        hop.tickSpacing = 10;
        hop.pool = UNISWAP_V3_USDC_USDT0_500;
    }

    function _v4(address tokenIn, address tokenOut) internal pure returns (MantisExecutor.TradeHopV2 memory hop) {
        hop.exchangeId = UNISWAP_V4_ID;
        hop.tokenIn = tokenIn;
        hop.tokenOut = tokenOut;
        hop.fee = 8;
        hop.tickSpacing = 1;
        hop.poolManager = UNISWAP_V4_POOL_MANAGER;
        hop.poolId = V4_USDC_USDT0_POOL_ID;
    }

    function _routeUsdcToUsdt0() internal pure returns (MantisExecutor.TradeHopV2[3] memory route) {
        route[0] = _fluid(USDC, USDT0);
        route[1] = _v4(USDT0, USDC);
    }

    function _routeUsdt0ToUsdc() internal pure returns (MantisExecutor.TradeHopV2[3] memory route) {
        route[0] = _v4(USDT0, USDC);
        route[1] = _fluid(USDC, USDT0);
    }

    function _route877Usdt0ToUsdcToUsdt0() internal pure returns (MantisExecutor.TradeHopV2[3] memory route) {
        route[0] = _v3(USDT0, USDC);
        route[1] = _fluid(USDC, USDT0);
    }

    function _route712UsdcToUsdt0ToUsdc() internal pure returns (MantisExecutor.TradeHopV2[3] memory route) {
        route[0] = _v4(USDC, USDT0);
        route[1] = _v3(USDT0, USDC);
    }

    function _probe(MantisExecutor.TradeHopV2[3] memory route, address profitToken, uint128 size)
        internal
        returns (uint256 profit, bytes4 revertSelector)
    {
        uint256 snap = vm.snapshotState();
        uint256 beforeBalance = IERC20Gate5(profitToken).balanceOf(address(this));
        try executor.flashLoanArbMixed(size, route, 0) {
            profit = IERC20Gate5(profitToken).balanceOf(address(this)) - beforeBalance;
        } catch (bytes memory data) {
            if (data.length >= 4) {
                assembly {
                    revertSelector := mload(add(data, 0x20))
                }
            }
            profit = 0;
        }
        vm.revertToState(snap);
    }

    function _runSizingOracle(
        string memory label,
        MantisExecutor.TradeHopV2[3] memory route,
        address profitToken,
        string memory tokenLabel
    ) internal returns (uint128 bestSize, uint256 bestProfit, uint256 nonLossReverts) {
        uint128[20] memory sizes = [
            uint128(1_000e6),
            5_000e6,
            10_000e6,
            25_000e6,
            50_000e6,
            75_000e6,
            100_000e6,
            150_000e6,
            200_000e6,
            300_000e6,
            400_000e6,
            500_000e6,
            650_000e6,
            800_000e6,
            900_000e6,
            1_000_000e6,
            1_100_000e6,
            1_250_000e6,
            1_500_000e6,
            2_000_000e6
        ];

        for (uint256 i = 0; i < sizes.length; i++) {
            (uint256 profit, bytes4 sel) = _probe(route, profitToken, sizes[i]);
            emit log_named_uint(
                string.concat(label, " probe size ", tokenLabel, " raw ", vm.toString(sizes[i])), profit
            );
            if (sel != bytes4(0) && sel != Loss.selector) {
                nonLossReverts++;
                emit log_named_bytes32(string.concat(label, " unexpected revert selector"), bytes32(sel));
            }
            if (profit > bestProfit) {
                bestProfit = profit;
                bestSize = sizes[i];
            }
        }

        emit log_named_uint(string.concat(label, " oracle best size ", tokenLabel, " raw"), bestSize);
        emit log_named_uint(string.concat(label, " oracle best profit ", tokenLabel, " raw"), bestProfit);
        emit log_named_uint(string.concat(label, " unexpected non-Loss reverts"), nonLossReverts);
    }

    function test_gate5_e066_parent_state_fluid_v4_sizing_oracle_diagnostic() public {
        emit log_named_uint("GATE5 e066 observed block number", block.number);
        // On Arbitrum, Solidity block.number is not a reliable assertion for the L2 RPC fork
        // block used by Foundry. The command line pins --fork-block-number; this test verifies
        // the route-critical contracts/state are present before probing the size ladder.
        _assertRequiredContractsPresent();
        executor = new MantisExecutor(address(this));
        (, uint256 bestProfit,) = _runSizingOracle("GATE5 e066 parent", _routeUsdcToUsdt0(), USDC, "USDC");

        assertGt(bestProfit, 0, "parent-state diagnostic found no profitable Fluid/V4 size");
    }

    function test_gate5_e066_exact_prefix_state_fluid_v4_sizing_oracle() public {
        vm.rollFork(E066_TX_HASH);
        emit log_named_uint("GATE5 e066 exact-prefix observed block number", block.number);
        _assertRequiredContractsPresent();
        executor = new MantisExecutor(address(this));
        (, uint256 bestProfit,) = _runSizingOracle("GATE5 e066 exact-prefix", _routeUsdcToUsdt0(), USDC, "USDC");

        assertGt(bestProfit, 0, "exact-prefix oracle found no profitable Fluid/V4 size");
    }

    function test_gate5_e066_exact_prefix_state_reverse_fluid_v4_sizing_oracle() public {
        vm.rollFork(E066_TX_HASH);
        emit log_named_uint("GATE5 e066 exact-prefix reverse observed block number", block.number);
        _assertRequiredContractsPresent();
        executor = new MantisExecutor(address(this));
        (, uint256 bestProfit,) =
            _runSizingOracle("GATE5 e066 exact-prefix reverse", _routeUsdt0ToUsdc(), USDT0, "USDT0");

        assertGt(bestProfit, 0, "exact-prefix reverse oracle found no profitable Fluid/V4 size");
    }

    function test_gate5_e066_exact_prefix_reverse_bot_size_profit() public {
        vm.rollFork(E066_TX_HASH);
        emit log_named_uint("GATE5 e066 exact-prefix reverse bot-size observed block number", block.number);
        _assertRequiredContractsPresent();
        executor = new MantisExecutor(address(this));

        (uint256 profit, bytes4 sel) = _probe(_routeUsdt0ToUsdc(), USDT0, BOT_ROUTE_723_INPUT_USDT0_RAW);

        emit log_named_uint("GATE5 e066 bot route 723 input USDT0 raw", BOT_ROUTE_723_INPUT_USDT0_RAW);
        emit log_named_uint("GATE5 e066 bot route 723 gross USDT0 raw", BOT_ROUTE_723_GROSS_USDT0_RAW);
        emit log_named_uint("GATE5 e066 bot route 723 REVM delta USDT0 raw", BOT_ROUTE_723_REVM_DELTA_USDT0_RAW);
        emit log_named_uint("GATE5 e066 exact-prefix reverse bot-size oracle profit USDT0 raw", profit);
        emit log_named_uint(
            "GATE5 e066 exact-prefix reverse bot-size oracle minus bot gross raw",
            profit > BOT_ROUTE_723_GROSS_USDT0_RAW ? profit - BOT_ROUTE_723_GROSS_USDT0_RAW : 0
        );
        emit log_named_uint(
            "GATE5 e066 bot gross minus exact-prefix reverse bot-size oracle raw",
            BOT_ROUTE_723_GROSS_USDT0_RAW > profit ? BOT_ROUTE_723_GROSS_USDT0_RAW - profit : 0
        );
        emit log_named_bytes32("GATE5 e066 exact-prefix reverse bot-size revert selector", bytes32(sel));

        assertEq(sel, bytes4(0), "exact-prefix reverse bot-size oracle reverted");
        assertGt(profit, 0, "exact-prefix reverse bot-size oracle found no profit");
    }

    function test_gate5_e066_exact_prefix_route877_v3_fluid_sizing_oracle() public {
        vm.rollFork(E066_TX_HASH);
        emit log_named_uint("GATE5 e066 route 877 exact-prefix observed block number", block.number);
        _assertRequiredContractsPresent();
        executor = new MantisExecutor(address(this));
        (, uint256 bestProfit,) =
            _runSizingOracle("GATE5 e066 route 877 exact-prefix", _route877Usdt0ToUsdcToUsdt0(), USDT0, "USDT0");

        assertGt(bestProfit, 0, "route 877 exact-prefix oracle found no profit");
    }

    function test_gate5_e066_exact_prefix_route877_bot_size_profit() public {
        vm.rollFork(E066_TX_HASH);
        emit log_named_uint("GATE5 e066 route 877 bot-size observed block number", block.number);
        _assertRequiredContractsPresent();
        executor = new MantisExecutor(address(this));

        (uint256 profit, bytes4 sel) = _probe(_route877Usdt0ToUsdcToUsdt0(), USDT0, BOT_ROUTE_877_INPUT_USDT0_RAW);

        emit log_named_uint("GATE5 e066 route 877 bot input USDT0 raw", BOT_ROUTE_877_INPUT_USDT0_RAW);
        emit log_named_uint("GATE5 e066 route 877 bot gross USDT0 raw", BOT_ROUTE_877_GROSS_USDT0_RAW);
        emit log_named_uint("GATE5 e066 route 877 REVM delta USDT0 raw", BOT_ROUTE_877_REVM_DELTA_USDT0_RAW);
        emit log_named_uint("GATE5 e066 route 877 bot-size oracle profit USDT0 raw", profit);
        emit log_named_bytes32("GATE5 e066 route 877 bot-size revert selector", bytes32(sel));

        assertEq(sel, bytes4(0), "route 877 exact-prefix bot-size oracle reverted");
        assertGt(profit, 0, "route 877 exact-prefix bot-size oracle found no profit");
    }

    function test_gate5_e066_exact_prefix_route712_v4_v3_has_no_positive_oracle_size() public {
        vm.rollFork(E066_TX_HASH);
        emit log_named_uint("GATE5 e066 route 712 exact-prefix observed block number", block.number);
        _assertRequiredContractsPresent();
        executor = new MantisExecutor(address(this));
        (, uint256 bestProfit, uint256 nonLossReverts) =
            _runSizingOracle("GATE5 e066 route 712 exact-prefix", _route712UsdcToUsdt0ToUsdc(), USDC, "USDC");
        emit log_named_uint("GATE5 e066 route 712 exact-prefix oracle non-Loss reverts", nonLossReverts);

        assertEq(bestProfit, 0, "route 712 exact-prefix oracle unexpectedly found profit");
    }

    function test_gate5_e066_exact_prefix_route712_bot_size_profit() public {
        vm.rollFork(E066_TX_HASH);
        emit log_named_uint("GATE5 e066 route 712 bot-size observed block number", block.number);
        _assertRequiredContractsPresent();
        executor = new MantisExecutor(address(this));

        (uint256 profit, bytes4 sel) = _probe(_route712UsdcToUsdt0ToUsdc(), USDC, BOT_ROUTE_712_INPUT_USDC_RAW);

        emit log_named_uint("GATE5 e066 route 712 bot input USDC raw", BOT_ROUTE_712_INPUT_USDC_RAW);
        emit log_named_uint("GATE5 e066 route 712 bot gross USDC raw", BOT_ROUTE_712_GROSS_USDC_RAW);
        emit log_named_uint("GATE5 e066 route 712 REVM delta USDC raw", BOT_ROUTE_712_REVM_DELTA_USDC_RAW);
        emit log_named_uint("GATE5 e066 route 712 bot-size oracle profit USDC raw", profit);
        emit log_named_bytes32("GATE5 e066 route 712 bot-size revert selector", bytes32(sel));

        assertEq(sel, Loss.selector, "route 712 exact-prefix bot-size should be a Loss revert");
        assertEq(profit, 0, "route 712 exact-prefix bot-size should have zero realized profit");
    }
}
