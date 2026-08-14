// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/MantisExecutor.sol";

interface IERC20MetadataReplay {
    function balanceOf(address account) external view returns (uint256);
}

interface IV3MetadataReplay {
    function fee() external view returns (uint24);
}

/// Diagnostic replay for the six strict detector rows that were previously classified as
/// metadata-limited. This uses metadata recovered from engine/discovery_cache.json at Gate9
/// snapshot block 465686728. It does not mutate production logic and does not certify release by
/// itself: it classifies whether the rows become executable once V4/Curve metadata is supplied.
contract MantisStrictMetadataReplayForkTest is Test {
    MantisExecutor executor;

    address constant WETH = 0x82aF49447D8a07e3bd95BD0d56f35241523fBab1;
    address constant WBTC = 0x2f2a2543B76A4166549F7aaB2e75Bef0aefC5B0f;
    address constant USDC = 0xaf88d065e77c8cC2239327C5EDb3A432268e5831;
    address constant ARB = 0x912CE59144191C1204E64559FE8253a0e49E6548;
    address constant USDT0 = 0xFd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9;

    address constant PANCAKE_WETH_WBTC = 0x4bfc22A4dA7f31F8a912a79A7e44a822398b4390;
    address constant UNI_WETH_ARB = 0xC6F780497A95e246EB9449f5e4770916DCd6396A;
    address constant UNI_ARB_WBTC = 0x689C96ceAb93f5E131631D225D75DeA3fD37747E;
    address constant ALGEBRA_USDC_ARB = 0xfaE2AE0a9f87FD35b5b0E24B47BAC796A7EEfEa1;
    address constant V4_WBTC_USDC_500 = 0x80c735C5A0222241F211b3Edb8df2CcefAD94553;
    address constant V4_WBTC_USDC_3000 = 0x70Bf44C3a9B6b047BF60e5a05968225Dbf3D6A5b;
    address constant V4_WBTC_USDT0_500 = 0x41B1E6b28A39199365f5348A2BF71D0F785167e9;
    address constant UNI_USDC_WETH = 0xC6962004f452bE9203591991D15f6b388e09E8D0;
    address constant UNI_USDT0_WBTC = 0x5969EFddE3cF5C0D9a88aE51E47d721096A97203;
    address constant DODO_WBTC_USDC = 0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14;
    address constant CURVE_USDC_USDT0 = 0x49b720F1Aab26260BEAec93A7BeB5BF2925b2A8F;
    address constant UNI_WBTC_WETH = 0x2f5e87C9312fa29aed5c179E456625D79015299c;

    address constant UNISWAP_V4_POOL_MANAGER = 0x360E68faCcca8cA495c1B759Fd9EEe466db9FB32;

    bytes32 constant V4_WBTC_USDC_500_POOL_ID = 0x80c735c5a0222241f211b3edb8df2ccefad94553ec18f1c29143f0399c78f500;
    bytes32 constant V4_WBTC_USDC_3000_POOL_ID = 0x70bf44c3a9b6b047bf60e5a05968225dbf3d6a5b9e8a95a73727e48921e889c1;
    bytes32 constant V4_WBTC_USDT0_500_POOL_ID = 0x41b1e6b28a39199365f5348a2bf71d0f785167e920d05254b3f2efd1b9487db5;

    uint8 constant UNISWAP_V3_ID = 0;
    uint8 constant PANCAKE_V3_ID = 6;
    uint8 constant ALGEBRA_V19_ID = 9;
    uint8 constant CURVE_STABLESWAP_NG_ID = 18;
    uint8 constant UNISWAP_V4_ID = 20;
    uint8 constant DODO_V2_ID = 23;

    error Loss(uint256);

    struct Probe {
        string label;
        uint128 amountIn;
        MantisExecutor.TradeHopV2[4] route;
    }

    function setUp() public {
        require(block.chainid == 42161, "fork Arbitrum One");
        executor = new MantisExecutor(address(this));
    }

    function _v3(uint8 exchangeId, address tokenIn, address tokenOut, address pool)
        internal
        view
        returns (MantisExecutor.TradeHopV2 memory hop)
    {
        hop.exchangeId = exchangeId;
        hop.tokenIn = tokenIn;
        hop.tokenOut = tokenOut;
        hop.fee = IV3MetadataReplay(pool).fee();
        hop.pool = pool;
    }

    function _pool(uint8 exchangeId, address tokenIn, address tokenOut, address pool)
        internal
        pure
        returns (MantisExecutor.TradeHopV2 memory hop)
    {
        hop.exchangeId = exchangeId;
        hop.tokenIn = tokenIn;
        hop.tokenOut = tokenOut;
        hop.pool = pool;
    }

    function _v4(address tokenIn, address tokenOut, uint24 fee, int24 tickSpacing, bytes32 poolId)
        internal
        pure
        returns (MantisExecutor.TradeHopV2 memory hop)
    {
        hop.exchangeId = UNISWAP_V4_ID;
        hop.tokenIn = tokenIn;
        hop.tokenOut = tokenOut;
        hop.fee = fee;
        hop.tickSpacing = tickSpacing;
        hop.poolManager = UNISWAP_V4_POOL_MANAGER;
        hop.poolId = poolId;
    }

    function _curve(address tokenIn, address tokenOut, uint8 coinInIdx, uint8 coinOutIdx)
        internal
        pure
        returns (MantisExecutor.TradeHopV2 memory hop)
    {
        hop.exchangeId = CURVE_STABLESWAP_NG_ID;
        hop.tokenIn = tokenIn;
        hop.tokenOut = tokenOut;
        hop.pool = CURVE_USDC_USDT0;
        hop.coinInIdx = coinInIdx;
        hop.coinOutIdx = coinOutIdx;
    }

    function _route3408() internal view returns (MantisExecutor.TradeHopV2[4] memory route) {
        route[0] = _v3(PANCAKE_V3_ID, WETH, WBTC, PANCAKE_WETH_WBTC);
        route[1] = _v4(WBTC, USDC, 500, 10, V4_WBTC_USDC_500_POOL_ID);
        route[2] = _v3(UNISWAP_V3_ID, USDC, WETH, UNI_USDC_WETH);
    }

    function _route15603() internal view returns (MantisExecutor.TradeHopV2[4] memory route) {
        route[0] = _v3(UNISWAP_V3_ID, WETH, ARB, UNI_WETH_ARB);
        route[1] = _v3(UNISWAP_V3_ID, ARB, WBTC, UNI_ARB_WBTC);
        route[2] = _v4(WBTC, USDC, 500, 10, V4_WBTC_USDC_500_POOL_ID);
        route[3] = _v3(UNISWAP_V3_ID, USDC, WETH, UNI_USDC_WETH);
    }

    function _route16803() internal view returns (MantisExecutor.TradeHopV2[4] memory route) {
        route[0] = _v3(UNISWAP_V3_ID, WETH, ARB, UNI_WETH_ARB);
        route[1] = _pool(ALGEBRA_V19_ID, ARB, USDC, ALGEBRA_USDC_ARB);
        route[2] = _v4(USDC, WBTC, 3000, 60, V4_WBTC_USDC_3000_POOL_ID);
        route[3] = _v3(UNISWAP_V3_ID, WBTC, WETH, UNI_WBTC_WETH);
    }

    function _route268() internal pure returns (MantisExecutor.TradeHopV2[4] memory route) {
        route[0] = _v4(WBTC, USDC, 500, 10, V4_WBTC_USDC_500_POOL_ID);
        route[1] = _pool(DODO_V2_ID, USDC, WBTC, DODO_WBTC_USDC);
    }

    function _route6252() internal view returns (MantisExecutor.TradeHopV2[4] memory route) {
        route[0] = _v3(UNISWAP_V3_ID, USDT0, WBTC, UNI_USDT0_WBTC);
        route[1] = _v4(WBTC, USDC, 500, 10, V4_WBTC_USDC_500_POOL_ID);
        route[2] = _curve(USDC, USDT0, 0, 1);
    }

    function _route6674() internal pure returns (MantisExecutor.TradeHopV2[4] memory route) {
        route[0] = _v4(WBTC, USDC, 500, 10, V4_WBTC_USDC_500_POOL_ID);
        route[1] = _curve(USDC, USDT0, 0, 1);
        route[2] = _v4(USDT0, WBTC, 500, 10, V4_WBTC_USDT0_500_POOL_ID);
    }

    function _probe(Probe memory probe) internal returns (uint8 outcome, int256 net, bytes4 selector) {
        uint256 snapshot = vm.snapshotState();
        address measureToken = probe.route[0].tokenIn;
        uint256 beforeBalance = IERC20MetadataReplay(measureToken).balanceOf(address(this));

        try executor.flashLoanArbMixed4(probe.amountIn, probe.route, 0) {
            net = int256(IERC20MetadataReplay(measureToken).balanceOf(address(this))) - int256(beforeBalance);
            outcome = net > 0 ? 0 : 1;
        } catch (bytes memory data) {
            if (data.length >= 4) {
                assembly {
                    selector := mload(add(data, 0x20))
                }
                if (selector == Loss.selector && data.length >= 36) {
                    uint256 shortfall;
                    assembly {
                        shortfall := mload(add(data, 0x24))
                    }
                    net = -int256(shortfall);
                    outcome = 1;
                } else {
                    outcome = 2;
                }
            } else {
                outcome = 2;
            }
        }

        vm.revertToState(snapshot);
    }

    function _emitOutcome(string memory label, uint8 outcome, int256 net, bytes4 selector) internal {
        emit log_string(string.concat(label, outcome == 0 ? " positive" : outcome == 1 ? " loss" : " other_revert"));
        emit log_named_int(string.concat(label, " net"), net);
        emit log_named_bytes32(string.concat(label, " selector"), bytes32(selector));
    }

    function test_StrictMetadataRows_WithRecoveredMetadata_Diagnostic() public {
        Probe[6] memory probes = [
            Probe("route3408", 1_997_778_864_620_776_083, _route3408()),
            Probe("route15603", 1_286_101_898_339_319_941, _route15603()),
            Probe("route16803", 47_146_747_792_887_691, _route16803()),
            Probe("route268", 73_436_371, _route268()),
            Probe("route6252", 33_316_121_147, _route6252()),
            Probe("route6674", 14_936_120, _route6674())
        ];

        uint256 nPositive;
        uint256 nLoss;
        uint256 nOther;
        int256 sumNet;
        for (uint256 i = 0; i < probes.length; i++) {
            (uint8 outcome, int256 net, bytes4 selector) = _probe(probes[i]);
            _emitOutcome(probes[i].label, outcome, net, selector);
            sumNet += net;
            if (outcome == 0) nPositive++;
            else if (outcome == 1) nLoss++;
            else nOther++;
        }

        emit log_named_uint("metadata rows tested", probes.length);
        emit log_named_uint("metadata rows positive", nPositive);
        emit log_named_uint("metadata rows loss", nLoss);
        emit log_named_uint("metadata rows other", nOther);
        emit log_named_int("metadata rows sum net", sumNet);

        assertEq(nPositive + nLoss + nOther, probes.length, "metadata diagnostic did not classify every row");
    }
}
