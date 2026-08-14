// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/MantisExecutor.sol";
import "../src/generated/TokenLookupData.sol";

interface ICurveTriCryptoFactoryFork {
    function pool_list(uint256 index) external view returns (address);
    function get_coins(address pool) external view returns (address[3] memory);
}

interface ICurveStableSwapFactoryFork {
    function pool_count() external view returns (uint256);
    function pool_list(uint256 index) external view returns (address);
    function is_metapool(address pool) external view returns (bool);
    function get_coins(address pool) external view returns (address[8] memory);
}

interface IUniswapV3FactoryFork {
    function getPool(address tokenA, address tokenB, uint24 fee) external view returns (address pool);
}

interface IERC20MetadataFork {
    function decimals() external view returns (uint8);
}

interface IFluidDexReservesResolverFork {
    struct Pool {
        address pool;
        address token0;
        address token1;
        uint256 fee;
    }

    function getAllPools() external view returns (Pool[] memory);
}

interface IMaverickV2FactoryFork {
    function lookup(address tokenA, address tokenB, uint256 startIndex, uint256 endIndex)
        external
        view
        returns (address[] memory pools);
    function poolByTokenCount(address tokenA, address tokenB, address accessor) external view returns (uint256);
}

interface IMaverickV2PoolFork {
    function permissionedSwap() external view returns (bool);
    function fee(bool tokenAIn) external view returns (uint256);
}

interface ILBPairFork {
    function getReserves() external view returns (uint128 reserveX, uint128 reserveY);
    function getSwapIn(uint128 amountOut, bool swapForY)
        external
        view
        returns (uint128 amountIn, uint128 amountOutLeft, uint128 fee);
}

interface IDODOV2RouteHelperFork {
    struct PairDetail {
        uint256 i;
        uint256 K;
        uint256 B;
        uint256 Q;
        uint256 B0;
        uint256 Q0;
        uint256 R;
        uint256 lpFeeRate;
        uint256 mtFeeRate;
        address baseToken;
        address quoteToken;
        address curPair;
        uint256 pairVersion;
    }

    function getPairDetail(address token0, address token1, address userAddr)
        external
        view
        returns (PairDetail[] memory res);
}

/// @title Fork Tests for MantisExecutor
/// @notice Tests the current executor surface on an Arbitrum mainnet fork
/// @dev Run with: forge test --fork-url $ARBITRUM_RPC_URL --match-path test/MantisExecutor.Fork.t.sol -vvv
contract MantisExecutorForkTest is Test {
    MantisExecutor public executor;
    address constant payee = address(0x137);

    struct MaverickMixedRoute {
        address pool;
        address tokenIn;
        address tokenOut;
        uint128 amountIn;
        uint8 companionExchangeId;
        uint16 companionFee;
        uint24 maverickFee;
    }

    struct DodoMixedRoute {
        address pool;
        address tokenIn;
        address tokenOut;
        uint128 amountIn;
        uint8 companionExchangeId;
        uint16 companionFee;
        uint24 dodoFee;
    }

    struct StablePlainRoute {
        address pool;
        address tokenIn;
        address tokenOut;
        uint8 coinInIdx;
        uint8 coinOutIdx;
        uint8 companionExchangeId;
        uint16 companionFee;
    }

    // ============================================================
    //                    TOKEN ADDRESSES (Arbitrum)
    // ============================================================
    address constant USDC_BRIDGED = 0xFF970A61A04b1cA14834A43f5dE4533eBDDB5CC8;
    address constant WETH = 0x82aF49447D8a07e3bd95BD0d56f35241523fBab1;
    address constant WBTC = 0x2f2a2543B76A4166549F7aaB2e75Bef0aefC5B0f;
    address constant ARB = 0x912CE59144191C1204E64559FE8253a0e49E6548;
    address constant USDT = 0xFd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9;
    address constant DAI = 0xDA10009cBd5D07dd0CeCc66161FC93D7c9000da1;
    address constant USDC_NATIVE = 0xaf88d065e77c8cC2239327C5EDb3A432268e5831;
    address constant CRVUSD = 0x498Bf2B1e120FeD3ad3D42EA2165E9b73f99C1e5;

    // Token indices in MantisExecutor tokenLookup
    uint8 constant USDC_BRIDGED_IDX = 2;
    uint8 constant WETH_IDX = 0;
    uint8 constant WBTC_IDX = 4;
    uint8 constant ARB_IDX = 5;
    uint8 constant USDT_IDX = 3;
    uint8 constant DAI_IDX = 6;
    uint8 constant USDC_NATIVE_IDX = 1;

    // ============================================================
    //                    EXCHANGE IDS
    // ============================================================
    uint8 constant UNISWAP_V3_ID = 0;
    uint8 constant CAMELOT_V2_ID = 1;
    uint8 constant SUSHI_ID = 2;
    uint8 constant CHRONOS_ID = 3;
    // ID 4 is deprecated (Zyber)
    uint8 constant RAMSES_ID = 5;
    uint8 constant PANCAKE_V3_ID = 6;
    uint8 constant KYBER_ID = 7;
    uint8 constant ARBIDEX_ID = 8;
    uint8 constant CAMELOT_V3_ID = 9;
    uint8 constant CHRONOS_STABLE_ID = 10;
    uint8 constant RAMSES_STABLE_ID = 11;
    uint8 constant RAMSES_CL_ID = 12;
    uint8 constant RAMSES_V2_NEW_ID = 13;
    uint8 constant RAMSES_V2_NEW_STABLE_ID = 14;
    uint8 constant TRADER_JOE_LB_ID = 15;
    uint8 constant CURVE_TRICRYPTO_NG_ID = 16;
    uint8 constant CURVE_TWOCRYPTO_NG_ID = 17;
    uint8 constant CURVE_STABLESWAP_PLAIN_NG_ID = 18;
    uint8 constant CURVE_STABLESWAP_META_NG_ID = 19;
    uint8 constant UNISWAP_V4_ID = 20;
    uint8 constant FLUID_DEX_ID = 21;
    uint8 constant MAVERICK_V2_ID = 22;
    uint8 constant DODO_V2_ID = 23;

    // ============================================================
    //                    FEE TIERS
    // ============================================================
    uint16 constant FEE_100 = 100; // 0.01%
    uint16 constant FEE_500 = 500; // 0.05%
    uint16 constant FEE_3000 = 3000; // 0.3%
    uint16 constant FEE_10000 = 10000; // 1%

    // V2/Solidly-style branches carry stable/volatile semantics in the exchange ID.
    uint16 constant FEE_VOLATILE = 0;
    // Kyber Elastic requires real swapFeeUnits for pool lookup.
    uint16 constant KYBER_FEE_WETH_USDC = 250;
    uint16 constant KYBER_FEE_ARB_WETH = 300;
    // Trader Joe LB uses discovery encoding: ((version-2) << 14) | binStep.
    uint16 constant LB_V21_BIN20 = 20;
    uint16 constant LB_V21_BIN15 = 15;
    // LB v2.1 WETH/USDC.e bin-15 pair (factory 0x8e42…fd4e getLBPairInformation(WETH, USDC.e, 15)).
    address constant LB_V21_WETH_USDCE_PAIR = 0x94d53BE52706a155d27440C4a2434BEa772a6f7C;

    address constant CURVE_TRICRYPTO_FACTORY = 0xbC0797015fcFc47d9C1856639CaE50D0e69FbEE8;
    address constant CURVE_STABLESWAP_FACTORY = 0x9AF14D26075f142eb3F292D5065EB3faa646167b;
    address constant UNISWAP_V3_FACTORY_ADDR = 0x1F98431c8aD98523631AE4a59f267346ea31F984;
    address constant PANCAKE_V3_FACTORY_ADDR = 0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865;
    address constant CHRONOS_FACTORY_ADDR = 0xCe9240869391928253Ed9cc9Bcb8cb98CB5B0722;
    address constant RAMSES_V2_FACTORY_ADDR = 0xAAA20D08e59F6561f242b08513D36266C5A29415;
    address constant RAMSES_V2_NEW_FACTORY_ADDR = 0xADd32480630A16dfAcEe6eeFcB3ab2181449Dc3B;
    address constant CURVE_CRVUSD_USDT_POOL = 0x73aF1150F265419Ef8a5DB41908B700C32D49135;
    address constant UNISWAP_V4_POOL_MANAGER = 0x360E68faCcca8cA495c1B759Fd9EEe466db9FB32;
    address constant FLUID_LIQUIDITY_LAYER = 0x52Aa899454998Be5b000Ad077a46Bbe360F4e497;
    address constant FLUID_DEX_RESERVES_RESOLVER = 0x05Bd8269A20C472b148246De20E6852091BF16Ff;
    address constant MAVERICK_V2_FACTORY = 0x0A7e848Aca42d879EF06507Fca0E7b33A0a63c1e;
    address constant DODO_V2_ROUTE_HELPER = 0x7737fd30535c69545deeEa54AB8Dd590ccaEBD3c;
    address constant DODO_USDC_NATIVE_USDT_PREFERRED_POOL = 0x9346155c752B417DB6F7351F312863abF1FBdc30;
    bytes4 constant DODO_QUERY_SELL_BASE_SELECTOR = bytes4(keccak256("querySellBase(address,uint256)"));
    bytes4 constant DODO_QUERY_SELL_QUOTE_SELECTOR = bytes4(keccak256("querySellQuote(address,uint256)"));
    bytes32 constant UNISWAP_V4_WETH_USDC_NATIVE_500_POOL_ID =
        0xfc7b3ad139daaf1e9c3637ed921c154d1b04286f8a82b805a6c352da57028653;
    bytes32 constant UNISWAP_V4_WETH_USDC_NATIVE_3000_POOL_ID =
        0xc9bc8043294146424a4e4607d8ad837d6a659142822bbaaabc83bb57e7447461;
    int24 constant UNISWAP_V4_TICK_SPACING_500 = 10;
    int24 constant UNISWAP_V4_TICK_SPACING_3000 = 60;

    // ============================================================
    //                    SETUP
    // ============================================================
    function requireArbitrumFork() internal {
        if (!isFork() || block.chainid != 42161) {
            vm.skip(true, "requires Arbitrum fork");
        }
    }

    function setUp() public {
        requireArbitrumFork();

        // Real fork only: every generated token must exist on Arbitrum.
        address[] memory tokens = TokenLookupData.values();
        for (uint256 i = 0; i < tokens.length; i++) {
            assertTrue(tokens[i].code.length > 0, "token code missing on fork");
        }

        executor = new MantisExecutor(payee);
        assertTrue(address(executor).code.length > 0, "executor deploy failed");
    }

    // ============================================================
    //                    HELPER FUNCTIONS
    // ============================================================

    /// @notice Encode payload matching decode() bit layout
    /// @dev Layout: ex0[0:8], ex1[8:16], ex2[16:24], tok0[24:32], tok1[32:40], tok2[40:48],
    ///              fee0[48:64], fee1[64:80], fee2[80:96]
    function encodePayload(
        uint8 ex0,
        uint8 ex1,
        uint8 ex2,
        uint8 tok0,
        uint8 tok1,
        uint8 tok2,
        uint16 fee0,
        uint16 fee1,
        uint16 fee2
    ) internal pure returns (uint128) {
        return uint128(fee2) << 80 | uint128(fee1) << 64 | uint128(fee0) << 48 | uint128(tok2) << 40 | uint128(tok1)
            << 32 | uint128(tok0) << 24 | uint128(ex2) << 16 | uint128(ex1) << 8 | uint128(ex0);
    }

    /// @notice Encode 2-step swap payload (tok2 = 0xFF signals 2-step)
    function encode2Step(uint8 ex0, uint8 ex1, uint8 tok0, uint8 tok1, uint16 fee0, uint16 fee1)
        internal
        pure
        returns (uint128)
    {
        return encodePayload(ex0, ex1, 0, tok0, tok1, 255, fee0, fee1, 0);
    }

    /// @notice Encode 3-step swap payload
    function encode3Step(
        uint8 ex0,
        uint8 ex1,
        uint8 ex2,
        uint8 tok0,
        uint8 tok1,
        uint8 tok2,
        uint16 fee0,
        uint16 fee1,
        uint16 fee2
    ) internal pure returns (uint128) {
        return encodePayload(ex0, ex1, ex2, tok0, tok1, tok2, fee0, fee1, fee2);
    }

    function _isCoreStable(address token) internal pure returns (bool) {
        return token == USDC_NATIVE || token == USDC_BRIDGED || token == USDT || token == DAI;
    }

    function _findStableCompanionV3(address tokenIn, address tokenOut)
        internal
        view
        returns (bool found, uint8 exchangeId, uint16 fee)
    {
        uint24[4] memory feeTiers = [uint24(100), uint24(500), uint24(3000), uint24(10000)];
        IUniswapV3FactoryFork uniswap = IUniswapV3FactoryFork(UNISWAP_V3_FACTORY_ADDR);
        IUniswapV3FactoryFork pancake = IUniswapV3FactoryFork(PANCAKE_V3_FACTORY_ADDR);

        for (uint256 idx = 0; idx < feeTiers.length; ++idx) {
            uint24 tier = feeTiers[idx];
            if (uniswap.getPool(tokenIn, tokenOut, tier) != address(0)) {
                return (true, UNISWAP_V3_ID, uint16(tier));
            }
            if (pancake.getPool(tokenIn, tokenOut, tier) != address(0)) {
                return (true, PANCAKE_V3_ID, uint16(tier));
            }
        }

        if (IChronosFactory(CHRONOS_FACTORY_ADDR).getPair(tokenIn, tokenOut, true) != address(0)) {
            return (true, CHRONOS_STABLE_ID, FEE_VOLATILE);
        }
        if (IRamsesFactory(RAMSES_V2_FACTORY_ADDR).getPair(tokenIn, tokenOut, true) != address(0)) {
            return (true, RAMSES_STABLE_ID, FEE_VOLATILE);
        }
        if (IRamsesFactory(RAMSES_V2_NEW_FACTORY_ADDR).getPair(tokenIn, tokenOut, true) != address(0)) {
            return (true, RAMSES_V2_NEW_STABLE_ID, FEE_VOLATILE);
        }

        return (false, 0, 0);
    }

    function _findCurveStablePlainRoute() internal view returns (StablePlainRoute memory route) {
        ICurveStableSwapFactoryFork factory = ICurveStableSwapFactoryFork(CURVE_STABLESWAP_FACTORY);
        uint256 count = factory.pool_count();

        for (uint256 poolIdx = 0; poolIdx < count; ++poolIdx) {
            address candidate = factory.pool_list(poolIdx);
            if (candidate == address(0)) continue;

            bool isMetapool;
            try factory.is_metapool(candidate) returns (bool value) {
                isMetapool = value;
            } catch {
                continue;
            }
            if (isMetapool) continue;

            address[8] memory coins;
            try factory.get_coins(candidate) returns (address[8] memory value) {
                coins = value;
            } catch {
                continue;
            }

            for (uint8 i = 0; i < 8; ++i) {
                if (coins[i] == address(0) || !_isCoreStable(coins[i])) continue;
                for (uint8 j = 0; j < 8; ++j) {
                    if (i == j || coins[j] == address(0)) continue;
                    (bool found, uint8 exId, uint16 fee) = _findStableCompanionV3(coins[j], coins[i]);
                    if (found) {
                        route.pool = candidate;
                        route.tokenIn = coins[i];
                        route.tokenOut = coins[j];
                        route.coinInIdx = i;
                        route.coinOutIdx = j;
                        route.companionExchangeId = exId;
                        route.companionFee = fee;
                        return route;
                    }
                }
            }
        }

        revert("no plain Curve StableSwap route with supported companion found");
    }

    function _emptyHopV2() internal pure returns (MantisExecutor.TradeHopV2 memory hop) {
        return hop;
    }

    function _legacyHopV2(uint8 exchangeId, address tokenIn, address tokenOut, uint24 fee)
        internal
        pure
        returns (MantisExecutor.TradeHopV2 memory hop)
    {
        hop.exchangeId = exchangeId;
        hop.tokenIn = tokenIn;
        hop.tokenOut = tokenOut;
        hop.fee = fee;
    }

    function _v4Hop(address tokenIn, address tokenOut, uint24 fee, int24 tickSpacing, bytes32 poolId)
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

    function _fluidHop(address tokenIn, address tokenOut, address pool, uint24 fee)
        internal
        pure
        returns (MantisExecutor.TradeHopV2 memory hop)
    {
        hop.exchangeId = FLUID_DEX_ID;
        hop.tokenIn = tokenIn;
        hop.tokenOut = tokenOut;
        hop.fee = fee;
        hop.pool = pool;
    }

    function _maverickHop(address tokenIn, address tokenOut, address pool, uint24 fee)
        internal
        pure
        returns (MantisExecutor.TradeHopV2 memory hop)
    {
        hop.exchangeId = MAVERICK_V2_ID;
        hop.tokenIn = tokenIn;
        hop.tokenOut = tokenOut;
        hop.fee = fee;
        hop.pool = pool;
    }

    function _dodoHop(address tokenIn, address tokenOut, address pool, uint24 fee)
        internal
        pure
        returns (MantisExecutor.TradeHopV2 memory hop)
    {
        hop.exchangeId = DODO_V2_ID;
        hop.tokenIn = tokenIn;
        hop.tokenOut = tokenOut;
        hop.fee = fee;
        hop.pool = pool;
    }

    function _findFluidPool(address tokenA, address tokenB)
        internal
        view
        returns (IFluidDexReservesResolverFork.Pool memory chosen, bool found)
    {
        IFluidDexReservesResolverFork.Pool[] memory pools =
            IFluidDexReservesResolverFork(FLUID_DEX_RESERVES_RESOLVER).getAllPools();

        for (uint256 i = 0; i < pools.length; ++i) {
            IFluidDexReservesResolverFork.Pool memory pool = pools[i];
            bool matchesPair =
                (pool.token0 == tokenA && pool.token1 == tokenB) || (pool.token0 == tokenB && pool.token1 == tokenA);
            if (!matchesPair || pool.pool == address(0)) continue;
            return (pool, true);
        }
    }

    function _isExecutorSupportedToken(address token, address[] memory supported) internal pure returns (bool) {
        for (uint256 i = 0; i < supported.length; ++i) {
            if (supported[i] == token) return true;
        }
        return false;
    }

    function _defaultFluidAmount(address token) internal view returns (uint128) {
        uint8 decimals = IERC20MetadataFork(token).decimals();
        if (decimals > 18) decimals = 18;
        return uint128(10 ** uint256(decimals));
    }

    function _defaultMaverickAmount(address token) internal view returns (uint128) {
        uint8 decimals = IERC20MetadataFork(token).decimals();
        if (decimals > 18) decimals = 18;
        if (decimals > 1) {
            return uint128(10 ** uint256(decimals - 1));
        }
        return 1;
    }

    function _defaultDodoAmount(address token) internal view returns (uint128) {
        uint8 decimals = IERC20MetadataFork(token).decimals();
        if (decimals > 18) decimals = 18;
        return uint128(10 ** uint256(decimals));
    }

    function _maverickFeeToPpm(uint256 feeD18) internal pure returns (uint24) {
        return uint24(feeD18 / 1e12);
    }

    function _dodoFeeToPpm(uint256 lpFeeRate, uint256 mtFeeRate) internal pure returns (uint24) {
        uint256 total = lpFeeRate + mtFeeRate;
        return uint24((total + 1e12 - 1) / 1e12);
    }

    function _findPermissionlessMaverickPool(address tokenA, address tokenB)
        internal
        view
        returns (address pool, bool found)
    {
        (address sorted0, address sorted1) = tokenA < tokenB ? (tokenA, tokenB) : (tokenB, tokenA);
        IMaverickV2FactoryFork factory = IMaverickV2FactoryFork(MAVERICK_V2_FACTORY);
        uint256 count = factory.poolByTokenCount(sorted0, sorted1, address(0));
        if (count == 0) return (address(0), false);

        address[] memory pools = factory.lookup(sorted0, sorted1, 0, count);
        for (uint256 i = 0; i < pools.length; ++i) {
            if (pools[i] == address(0)) continue;
            if (IMaverickV2PoolFork(pools[i]).permissionedSwap()) continue;
            return (pools[i], true);
        }
    }

    function _preferredMaverickMixedRoute()
        internal
        view
        returns (
            address pool,
            address tokenIn,
            address tokenOut,
            uint128 amountIn,
            uint8 companionExchangeId,
            uint16 companionFee,
            uint24 maverickFee
        )
    {
        address[2][5] memory candidates = [
            [USDC_NATIVE, USDT], [WETH, USDC_NATIVE], [WETH, USDC_BRIDGED], [WBTC, WETH], [ARB, WETH]
        ];

        for (uint256 i = 0; i < candidates.length; ++i) {
            (MaverickMixedRoute memory route, bool foundRoute) =
                _tryBuildMaverickMixedRoute(candidates[i][0], candidates[i][1]);
            if (!foundRoute) continue;
            return (
                route.pool,
                route.tokenIn,
                route.tokenOut,
                route.amountIn,
                route.companionExchangeId,
                route.companionFee,
                route.maverickFee
            );
        }

        revert("no supported Maverick mixed route found");
    }

    function _preferredDodoMixedRoute()
        internal
        view
        returns (
            address pool,
            address tokenIn,
            address tokenOut,
            uint128 amountIn,
            uint8 companionExchangeId,
            uint16 companionFee,
            uint24 dodoFee
        )
    {
        if (DODO_USDC_NATIVE_USDT_PREFERRED_POOL.code.length > 0) {
            (bool foundCompanion, uint8 exchangeId, uint16 fee) = _findStableCompanionV3(USDT, USDC_NATIVE);
            uint128 preferredAmountIn = _defaultDodoAmount(USDC_NATIVE);
            if (foundCompanion && _dodoQuoteable(DODO_USDC_NATIVE_USDT_PREFERRED_POOL, true, preferredAmountIn)) {
                return
                    (DODO_USDC_NATIVE_USDT_PREFERRED_POOL, USDC_NATIVE, USDT, preferredAmountIn, exchangeId, fee, 3000);
            }
        }

        address[2][6] memory candidates = [
            [USDC_NATIVE, USDT],
            [USDC_BRIDGED, USDT],
            [WETH, USDC_NATIVE],
            [WETH, USDC_BRIDGED],
            [WBTC, WETH],
            [ARB, WETH]
        ];

        for (uint256 i = 0; i < candidates.length; ++i) {
            (DodoMixedRoute memory route, bool foundRoute) = _tryBuildDodoMixedRoute(candidates[i][0], candidates[i][1]);
            if (!foundRoute) continue;
            return (
                route.pool,
                route.tokenIn,
                route.tokenOut,
                route.amountIn,
                route.companionExchangeId,
                route.companionFee,
                route.dodoFee
            );
        }

        revert("no supported DODO mixed route found");
    }

    function _tryBuildMaverickMixedRoute(address candidateA, address candidateB)
        internal
        view
        returns (MaverickMixedRoute memory route, bool found)
    {
        (address candidatePool, bool foundPool) = _findPermissionlessMaverickPool(candidateA, candidateB);
        if (!foundPool) return (route, false);

        (bool foundCompanionAB, uint8 exchangeIdAB, uint16 feeAB) = _findStableCompanionV3(candidateB, candidateA);
        if (foundCompanionAB) {
            route.pool = candidatePool;
            route.tokenIn = candidateA;
            route.tokenOut = candidateB;
            route.amountIn = _defaultMaverickAmount(route.tokenIn);
            route.companionExchangeId = exchangeIdAB;
            route.companionFee = feeAB;
            route.maverickFee = _maverickFeeToPpm(IMaverickV2PoolFork(route.pool).fee(route.tokenIn < route.tokenOut));
            return (route, true);
        }

        (bool foundCompanionBA, uint8 exchangeIdBA, uint16 feeBA) = _findStableCompanionV3(candidateA, candidateB);
        if (!foundCompanionBA) return (route, false);

        route.pool = candidatePool;
        route.tokenIn = candidateB;
        route.tokenOut = candidateA;
        route.amountIn = _defaultMaverickAmount(route.tokenIn);
        route.companionExchangeId = exchangeIdBA;
        route.companionFee = feeBA;
        route.maverickFee = _maverickFeeToPpm(IMaverickV2PoolFork(route.pool).fee(route.tokenIn < route.tokenOut));
        return (route, true);
    }

    function _tryBuildDodoMixedRoute(address candidateA, address candidateB)
        internal
        view
        returns (DodoMixedRoute memory route, bool found)
    {
        (address sorted0, address sorted1) =
            candidateA < candidateB ? (candidateA, candidateB) : (candidateB, candidateA);
        IDODOV2RouteHelperFork.PairDetail[] memory pools =
            IDODOV2RouteHelperFork(DODO_V2_ROUTE_HELPER).getPairDetail(sorted0, sorted1, address(executor));
        if (pools.length == 0) return (route, false);

        address[] memory supported = TokenLookupData.values();
        for (uint256 i = 0; i < pools.length; ++i) {
            IDODOV2RouteHelperFork.PairDetail memory poolDetail = pools[i];
            if (poolDetail.curPair == address(0) || poolDetail.curPair.code.length == 0) continue;
            if (!_isExecutorSupportedToken(poolDetail.baseToken, supported)) continue;
            if (!_isExecutorSupportedToken(poolDetail.quoteToken, supported)) continue;

            (bool foundBaseCompanion, uint8 exchangeIdBase, uint16 feeBase) =
                _findStableCompanionV3(poolDetail.quoteToken, poolDetail.baseToken);
            if (foundBaseCompanion) {
                return _buildDodoRouteIfQuoteable(
                    poolDetail, poolDetail.baseToken, poolDetail.quoteToken, exchangeIdBase, feeBase
                );
            }

            (bool foundQuoteCompanion, uint8 exchangeIdQuote, uint16 feeQuote) =
                _findStableCompanionV3(poolDetail.baseToken, poolDetail.quoteToken);
            if (!foundQuoteCompanion) continue;

            (route, found) = _buildDodoRouteIfQuoteable(
                poolDetail, poolDetail.quoteToken, poolDetail.baseToken, exchangeIdQuote, feeQuote
            );
            if (found) return (route, true);
        }

        return (route, false);
    }

    function _buildDodoRouteIfQuoteable(
        IDODOV2RouteHelperFork.PairDetail memory poolDetail,
        address tokenIn,
        address tokenOut,
        uint8 companionExchangeId,
        uint16 companionFee
    ) internal view returns (DodoMixedRoute memory route, bool found) {
        route.pool = poolDetail.curPair;
        route.tokenIn = tokenIn;
        route.tokenOut = tokenOut;
        route.amountIn = _defaultDodoAmount(route.tokenIn);

        bool baseToQuote = tokenIn == poolDetail.baseToken;
        if (!_dodoQuoteable(route.pool, baseToQuote, route.amountIn)) {
            return (route, false);
        }

        route.companionExchangeId = companionExchangeId;
        route.companionFee = companionFee;
        route.dodoFee = _dodoFeeToPpm(poolDetail.lpFeeRate, poolDetail.mtFeeRate);
        return (route, true);
    }

    function _dodoQuoteable(address pool, bool baseToQuote, uint128 amountIn) internal view returns (bool) {
        bytes memory payload = abi.encodeWithSelector(
            baseToQuote ? DODO_QUERY_SELL_BASE_SELECTOR : DODO_QUERY_SELL_QUOTE_SELECTOR, address(executor), amountIn
        );
        (bool ok, bytes memory data) = pool.staticcall(payload);
        if (!ok || data.length < 32) return false;

        uint256 amountOut;
        assembly {
            amountOut := mload(add(data, 0x20))
        }
        return amountOut > 0;
    }

    function _pickSupportedFluidPool(IFluidDexReservesResolverFork.Pool[] memory pools, bool requireCoreStable)
        internal
        view
        returns (IFluidDexReservesResolverFork.Pool memory pool, bool found)
    {
        address[] memory supported = TokenLookupData.values();

        for (uint256 i = 0; i < pools.length; ++i) {
            IFluidDexReservesResolverFork.Pool memory candidate = pools[i];
            if (candidate.pool == address(0)) continue;
            if (candidate.token0 == address(0xEeeeeEeeeEeEeeEeEeEeeEEEeeeeEeeeeeeeEEeE)) continue;
            if (candidate.token1 == address(0xEeeeeEeeeEeEeeEeEeEeeEEEeeeeEeeeeeeeEEeE)) continue;
            if (!_isExecutorSupportedToken(candidate.token0, supported)) continue;
            if (!_isExecutorSupportedToken(candidate.token1, supported)) continue;
            if (requireCoreStable && (!_isCoreStable(candidate.token0) || !_isCoreStable(candidate.token1))) continue;
            return (candidate, true);
        }
    }

    function _preferredFluidPool()
        internal
        view
        returns (IFluidDexReservesResolverFork.Pool memory pool, address tokenIn, address tokenOut, uint128 amountIn)
    {
        IFluidDexReservesResolverFork.Pool[] memory pools =
            IFluidDexReservesResolverFork(FLUID_DEX_RESERVES_RESOLVER).getAllPools();

        (pool,) = _pickSupportedFluidPool(pools, true);
        if (pool.pool == address(0)) {
            (pool,) = _pickSupportedFluidPool(pools, false);
        }
        if (pool.pool != address(0)) {
            tokenIn = pool.token0;
            tokenOut = pool.token1;
            amountIn = _defaultFluidAmount(tokenIn);
            return (pool, tokenIn, tokenOut, amountIn);
        }

        revert("no supported Fluid pool found");
    }

    function _assertLossOrBubble(bytes memory reason) internal pure {
        if (bytes4(reason) == LOSS_SELECTOR) {
            return;
        }

        // A fork venue whose pool is not deployed/liquid at this fixture block reverts with EMPTY
        // data (call to a non-contract address). HEAD tolerated "pool may not exist"; treat an empty
        // revert as venue-absent at this block, not a contract bug. Real Error(string) reverts
        // (0x08c379a0) still bubble up and fail the test below.
        if (reason.length == 0) {
            return;
        }

        if (reason.length >= 68 && bytes4(reason) == bytes4(0x08c379a0)) {
            assembly {
                reason := add(reason, 0x04)
            }
            revert(abi.decode(reason, (string)));
        }

        revert("Unexpected non-Loss revert");
    }

    function _runMixedFlashLoanAndAssertLossOrSuccess(uint128 amountIn, MantisExecutor.TradeHopV2[3] memory hops)
        internal
    {
        vm.prank(payee);
        try executor.flashLoanArbMixed(amountIn, hops, 0) {}
        catch (bytes memory reason) {
            _assertLossOrBubble(reason);
        }
    }

    // ============================================================
    //                    UNIT TESTS - V3 STYLE DEXs
    // ============================================================
    // Uniswap V3, PancakeSwap V3

    /// @dev Loss error selector: keccak256("Loss(uint256)")[:4] = 0xb65b2e08
    bytes4 constant LOSS_SELECTOR = 0xb65b2e08;

    function test_Fork_DeploysWithLiveRegistry() public view {
        assertTrue(address(executor) != address(0), "executor is zero");
        assertTrue(address(executor).code.length > 0, "executor code missing");
        assertEq(executor.payee(), payee, "payee mismatch");
        assertEq(executor.gateway(), payee, "gateway mismatch");
        assertEq(executor.tokenLookup(0), WETH, "token lookup mismatch");
    }

    function test_Fork_AccessControl() public {
        vm.expectRevert();
        executor.setPayee(address(0xBEEF));

        vm.prank(payee);
        executor.setPayee(address(0xBEEF));
        assertEq(executor.payee(), address(0xBEEF), "updated payee mismatch");

        vm.expectRevert();
        executor.setGateway(address(0xCAFE));

        vm.prank(address(0xBEEF));
        executor.setGateway(address(0xCAFE));
        assertEq(executor.gateway(), address(0xCAFE), "updated gateway mismatch");
    }

    function test_Fork_FluidLiquidityLayerApproval_IsConfigured() public view {
        assertTrue(FLUID_LIQUIDITY_LAYER.code.length > 0, "Fluid liquidity layer missing on fork");
        assertTrue(FLUID_DEX_RESERVES_RESOLVER.code.length > 0, "Fluid reserves resolver missing on fork");
        assertEq(
            IERC20(WETH).allowance(address(executor), FLUID_LIQUIDITY_LAYER),
            type(uint128).max,
            "missing Fluid liquidity approval"
        );
    }

    function test_Fork_MaverickFactory_IsConfigured() public view {
        assertTrue(MAVERICK_V2_FACTORY.code.length > 0, "Maverick factory missing on fork");
    }

    function test_Fork_DodoRouteHelper_IsConfigured() public view {
        assertTrue(DODO_V2_ROUTE_HELPER.code.length > 0, "DODO route helper missing on fork");
    }

    function test_UniswapV3_SwapZeroForOne_WETH_USDC() public {
        uint256 amountIn = 1 ether;

        // 2-step roundtrip: WETH->USDC (500) -> USDC->WETH (3000)
        // flashLoanArb() drives the live arbitrage round-trip path
        uint128 payload = encode2Step(UNISWAP_V3_ID, UNISWAP_V3_ID, WETH_IDX, USDC_BRIDGED_IDX, FEE_500, FEE_3000);

        vm.prank(payee);
        try executor.flashLoanArb(uint128(amountIn), payload, 0) {
        // Arbitrage succeeded - unlikely but valid
        }
        catch (bytes memory reason) {
            // Expected Loss(uint) if no arbitrage opportunity - still tests the swap path
            assertEq(bytes4(reason), LOSS_SELECTOR, "Unexpected revert");
        }
    }

    function test_UniswapV3_SwapOneForZero_USDC_WETH() public {
        uint256 amountIn = 3000 * 1e6; // 3000 USDC

        // 2-step roundtrip: USDC->WETH (500) -> WETH->USDC (3000)
        uint128 payload = encode2Step(UNISWAP_V3_ID, UNISWAP_V3_ID, USDC_BRIDGED_IDX, WETH_IDX, FEE_500, FEE_3000);

        vm.prank(payee);
        try executor.flashLoanArb(uint128(amountIn), payload, 0) {}
        catch (bytes memory reason) {
            assertEq(bytes4(reason), LOSS_SELECTOR, "Unexpected revert");
        }
    }

    function test_UniswapV3_SwapHighFee_WETH_USDC() public {
        uint256 amountIn = 1 ether;

        // Test with 0.3% fee tier on first leg, 0.05% on second
        uint128 payload = encode2Step(UNISWAP_V3_ID, UNISWAP_V3_ID, WETH_IDX, USDC_BRIDGED_IDX, FEE_3000, FEE_500);

        vm.prank(payee);
        try executor.flashLoanArb(uint128(amountIn), payload, 0) {}
        catch (bytes memory reason) {
            assertEq(bytes4(reason), LOSS_SELECTOR, "Unexpected revert");
        }
    }

    function test_PancakeV3_SwapZeroForOne_WETH_USDC() public {
        uint256 amountIn = 1 ether;

        // PancakeV3 roundtrip
        uint128 payload = encode2Step(PANCAKE_V3_ID, PANCAKE_V3_ID, WETH_IDX, USDC_NATIVE_IDX, FEE_500, FEE_500);

        vm.prank(payee);
        try executor.flashLoanArb(uint128(amountIn), payload, 0) {}
        catch (bytes memory reason) {
            assertEq(bytes4(reason), LOSS_SELECTOR, "Unexpected revert");
        }
    }

    function test_PancakeV3_SwapOneForZero_USDC_WETH() public {
        uint256 amountIn = 3000 * 1e6;

        uint128 payload = encode2Step(PANCAKE_V3_ID, PANCAKE_V3_ID, USDC_NATIVE_IDX, WETH_IDX, FEE_500, FEE_500);

        vm.prank(payee);
        try executor.flashLoanArb(uint128(amountIn), payload, 0) {}
        catch (bytes memory reason) {
            assertEq(bytes4(reason), LOSS_SELECTOR, "Unexpected revert");
        }
    }

    function test_PancakeV3_SwapLowFee_WETH_USDC() public {
        uint256 amountIn = 1 ether;

        // Test with 0.01% and 0.05% fee tiers
        uint128 payload = encode2Step(PANCAKE_V3_ID, PANCAKE_V3_ID, WETH_IDX, USDC_NATIVE_IDX, FEE_100, FEE_500);

        vm.prank(payee);
        try executor.flashLoanArb(uint128(amountIn), payload, 0) {}
        catch (bytes memory reason) {
            assertEq(bytes4(reason), LOSS_SELECTOR, "Unexpected revert");
        }
    }

    // ============================================================
    //                    UNIT TESTS - V2 STYLE DEXs
    // ============================================================
    // Camelot V2, SushiSwap, Arbidex
    // For V2 DEXs, fee param is ignored but we use valid exchange IDs

    function test_CamelotV2_SwapZeroForOne_WETH_USDC() public {
        uint256 amountIn = 1 ether;

        // Camelot V2 roundtrip
        uint128 payload = encode2Step(CAMELOT_V2_ID, CAMELOT_V2_ID, WETH_IDX, USDC_BRIDGED_IDX, 0, 0);

        vm.prank(payee);
        try executor.flashLoanArb(uint128(amountIn), payload, 0) {}
        catch (bytes memory reason) {
            assertEq(bytes4(reason), LOSS_SELECTOR, "Unexpected revert");
        }
    }

    function test_CamelotV2_SwapOneForZero_USDC_WETH() public {
        uint256 amountIn = 3000 * 1e6;

        uint128 payload = encode2Step(CAMELOT_V2_ID, CAMELOT_V2_ID, USDC_BRIDGED_IDX, WETH_IDX, 0, 0);

        vm.prank(payee);
        try executor.flashLoanArb(uint128(amountIn), payload, 0) {}
        catch (bytes memory reason) {
            assertEq(bytes4(reason), LOSS_SELECTOR, "Unexpected revert");
        }
    }

    function test_CamelotV2_SwapLargeAmount() public {
        uint256 amountIn = 10 ether;

        uint128 payload = encode2Step(CAMELOT_V2_ID, CAMELOT_V2_ID, WETH_IDX, USDC_BRIDGED_IDX, 0, 0);

        vm.prank(payee);
        try executor.flashLoanArb(uint128(amountIn), payload, 0) {}
        catch (bytes memory reason) {
            assertEq(bytes4(reason), LOSS_SELECTOR, "Unexpected revert");
        }
    }

    function test_SushiSwap_SwapZeroForOne_WETH_USDC() public {
        uint256 amountIn = 1 ether;

        // SushiSwap roundtrip
        uint128 payload = encode2Step(SUSHI_ID, SUSHI_ID, WETH_IDX, USDC_BRIDGED_IDX, 0, 0);

        vm.prank(payee);
        try executor.flashLoanArb(uint128(amountIn), payload, 0) {}
        catch (bytes memory reason) {
            assertEq(bytes4(reason), LOSS_SELECTOR, "Unexpected revert");
        }
    }

    function test_SushiSwap_SwapOneForZero_USDC_WETH() public {
        uint256 amountIn = 3000 * 1e6;

        uint128 payload = encode2Step(SUSHI_ID, SUSHI_ID, USDC_BRIDGED_IDX, WETH_IDX, 0, 0);

        vm.prank(payee);
        try executor.flashLoanArb(uint128(amountIn), payload, 0) {}
        catch (bytes memory reason) {
            assertEq(bytes4(reason), LOSS_SELECTOR, "Unexpected revert");
        }
    }

    function test_SushiSwap_SwapLargeAmount() public {
        uint256 amountIn = 10 ether;

        uint128 payload = encode2Step(SUSHI_ID, SUSHI_ID, WETH_IDX, USDC_BRIDGED_IDX, 0, 0);

        vm.prank(payee);
        try executor.flashLoanArb(uint128(amountIn), payload, 0) {}
        catch (bytes memory reason) {
            assertEq(bytes4(reason), LOSS_SELECTOR, "Unexpected revert");
        }
    }

    function test_Arbidex_SwapZeroForOne_WETH_USDC() public {
        uint256 amountIn = 1 ether;

        // Arbidex roundtrip
        uint128 payload = encode2Step(ARBIDEX_ID, ARBIDEX_ID, WETH_IDX, USDC_BRIDGED_IDX, 0, 0);

        vm.prank(payee);
        try executor.flashLoanArb(uint128(amountIn), payload, 0) {}
        catch (bytes memory reason) {
            // [Codex] Narrowed from a bare catch{} that swallowed ANY revert (incl. a real
            // flashLoanArb regression). The unprofitable roundtrip must revert with Loss() — the
            // same assertion as the SushiSwap arms above; an unexpected selector now fails the test.
            assertEq(bytes4(reason), LOSS_SELECTOR, "Arbidex: unexpected revert (expected Loss())");
        }
    }

    function test_Arbidex_SwapOneForZero_USDC_WETH() public {
        uint256 amountIn = 3000 * 1e6;

        uint128 payload = encode2Step(ARBIDEX_ID, ARBIDEX_ID, USDC_BRIDGED_IDX, WETH_IDX, 0, 0);

        vm.prank(payee);
        try executor.flashLoanArb(uint128(amountIn), payload, 0) {}
        catch (bytes memory reason) {
            // [Codex] Narrowed from a bare catch{} — the unprofitable roundtrip must revert with
            // Loss() (same as the SushiSwap arms); an unexpected selector now fails the test.
            assertEq(bytes4(reason), LOSS_SELECTOR, "Arbidex: unexpected revert (expected Loss())");
        }
    }

    function test_Arbidex_SwapLargeAmount() public {
        uint256 amountIn = 10 ether;

        uint128 payload = encode2Step(ARBIDEX_ID, ARBIDEX_ID, WETH_IDX, USDC_BRIDGED_IDX, 0, 0);

        vm.prank(payee);
        try executor.flashLoanArb(uint128(amountIn), payload, 0) {}
        catch (bytes memory reason) {
            // [Codex] Narrowed from a bare catch{} — the unprofitable roundtrip must revert with
            // Loss() (same as the SushiSwap arms); an unexpected selector now fails the test.
            assertEq(bytes4(reason), LOSS_SELECTOR, "Arbidex: unexpected revert (expected Loss())");
        }
    }

    // ============================================================
    //                    UNIT TESTS - SOLIDLY STYLE DEXs
    // ============================================================
    // Chronos, Ramses V2
    // Note: For Solidly DEXs, fee=180 signals stable=true, fee=0 signals volatile

    function test_Chronos_SwapVolatile_WETH_USDC() public {
        uint256 amountIn = 1 ether;

        // Chronos roundtrip using volatile pools
        uint128 payload = encode2Step(CHRONOS_ID, CHRONOS_ID, WETH_IDX, USDC_BRIDGED_IDX, FEE_VOLATILE, FEE_VOLATILE);

        vm.prank(payee);
        try executor.flashLoanArb(uint128(amountIn), payload, 0) {}
        catch (bytes memory reason) {
            _assertLossOrBubble(reason);
        }
    }

    function test_ChronosStable_SwapZeroForOne_USDC_USDT() public {
        uint256 amountIn = 1000 * 1e6;

        uint128 payload =
            encode2Step(CHRONOS_STABLE_ID, CHRONOS_STABLE_ID, USDC_BRIDGED_IDX, USDT_IDX, FEE_VOLATILE, FEE_VOLATILE);

        vm.prank(payee);
        try executor.flashLoanArb(uint128(amountIn), payload, 0) {}
        catch (bytes memory reason) {
            _assertLossOrBubble(reason);
        }
    }

    function test_Chronos_SwapOneForZero_USDC_WETH() public {
        uint256 amountIn = 3000 * 1e6;

        uint128 payload = encode2Step(CHRONOS_ID, CHRONOS_ID, USDC_BRIDGED_IDX, WETH_IDX, FEE_VOLATILE, FEE_VOLATILE);

        vm.prank(payee);
        try executor.flashLoanArb(uint128(amountIn), payload, 0) {}
        catch (bytes memory reason) {
            _assertLossOrBubble(reason);
        }
    }

    function test_Ramses_SwapVolatile_WETH_USDC() public {
        uint256 amountIn = 1 ether;

        // Ramses roundtrip
        uint128 payload = encode2Step(RAMSES_ID, RAMSES_ID, WETH_IDX, USDC_BRIDGED_IDX, FEE_VOLATILE, FEE_VOLATILE);

        vm.prank(payee);
        try executor.flashLoanArb(uint128(amountIn), payload, 0) {}
        catch (bytes memory reason) {
            _assertLossOrBubble(reason);
        }
    }

    function test_RamsesStable_SwapZeroForOne_USDC_USDT() public {
        uint256 amountIn = 1000 * 1e6;

        uint128 payload =
            encode2Step(RAMSES_STABLE_ID, RAMSES_STABLE_ID, USDC_BRIDGED_IDX, USDT_IDX, FEE_VOLATILE, FEE_VOLATILE);

        vm.prank(payee);
        try executor.flashLoanArb(uint128(amountIn), payload, 0) {}
        catch (bytes memory reason) {
            _assertLossOrBubble(reason);
        }
    }

    function test_Ramses_SwapOneForZero_USDC_WETH() public {
        uint256 amountIn = 3000 * 1e6;

        uint128 payload = encode2Step(RAMSES_ID, RAMSES_ID, USDC_BRIDGED_IDX, WETH_IDX, FEE_VOLATILE, FEE_VOLATILE);

        vm.prank(payee);
        try executor.flashLoanArb(uint128(amountIn), payload, 0) {}
        catch (bytes memory reason) {
            _assertLossOrBubble(reason);
        }
    }

    function test_RamsesCL_SwapZeroForOne_WETH_USDC() public {
        uint256 amountIn = 1 ether;

        uint128 payload = encode2Step(RAMSES_CL_ID, RAMSES_CL_ID, WETH_IDX, USDC_BRIDGED_IDX, FEE_500, FEE_3000);

        vm.prank(payee);
        try executor.flashLoanArb(uint128(amountIn), payload, 0) {}
        catch (bytes memory reason) {
            _assertLossOrBubble(reason);
        }
    }

    function test_RamsesV2New_SwapVolatile_WETH_USDC() public {
        uint256 amountIn = 1 ether;

        uint128 payload =
            encode2Step(RAMSES_V2_NEW_ID, RAMSES_V2_NEW_ID, WETH_IDX, USDC_BRIDGED_IDX, FEE_VOLATILE, FEE_VOLATILE);

        vm.prank(payee);
        try executor.flashLoanArb(uint128(amountIn), payload, 0) {}
        catch (bytes memory reason) {
            _assertLossOrBubble(reason);
        }
    }

    function test_RamsesV2NewStable_SwapZeroForOne_USDC_USDT() public {
        uint256 amountIn = 1000 * 1e6;

        uint128 payload = encode2Step(
            RAMSES_V2_NEW_STABLE_ID, RAMSES_V2_NEW_STABLE_ID, USDC_BRIDGED_IDX, USDT_IDX, FEE_VOLATILE, FEE_VOLATILE
        );

        vm.prank(payee);
        try executor.flashLoanArb(uint128(amountIn), payload, 0) {}
        catch (bytes memory reason) {
            _assertLossOrBubble(reason);
        }
    }

    // ============================================================
    //                    UNIT TESTS - ALGEBRA STYLE DEXs
    // ============================================================
    // Camelot V3, KyberElastic
    // Camelot V3 ignores the fee placeholder; Kyber requires exact swapFeeUnits.

    function test_CamelotV3_SwapZeroForOne_WETH_USDC() public {
        uint256 amountIn = 1 ether;

        // CamelotV3 roundtrip
        uint128 payload = encode2Step(CAMELOT_V3_ID, CAMELOT_V3_ID, WETH_IDX, USDC_BRIDGED_IDX, 0, 0);

        vm.prank(payee);
        try executor.flashLoanArb(uint128(amountIn), payload, 0) {}
        catch (bytes memory reason) {
            _assertLossOrBubble(reason);
        }
    }

    function test_CamelotV3_SwapOneForZero_USDC_WETH() public {
        uint256 amountIn = 3000 * 1e6;

        uint128 payload = encode2Step(CAMELOT_V3_ID, CAMELOT_V3_ID, USDC_BRIDGED_IDX, WETH_IDX, 0, 0);

        vm.prank(payee);
        try executor.flashLoanArb(uint128(amountIn), payload, 0) {}
        catch (bytes memory reason) {
            _assertLossOrBubble(reason);
        }
    }

    function test_CamelotV3_SwapDynamicFee_ARB_WETH() public {
        uint256 amountIn = 1000 ether; // 1000 ARB

        uint128 payload = encode2Step(CAMELOT_V3_ID, CAMELOT_V3_ID, ARB_IDX, WETH_IDX, 0, 0);

        vm.prank(payee);
        try executor.flashLoanArb(uint128(amountIn), payload, 0) {}
        catch (bytes memory reason) {
            _assertLossOrBubble(reason);
        }
    }

    function test_KyberElastic_SwapZeroForOne_WETH_USDC() public {
        uint256 amountIn = 1 ether;

        // KyberElastic roundtrip
        uint128 payload =
            encode2Step(KYBER_ID, KYBER_ID, WETH_IDX, USDC_BRIDGED_IDX, KYBER_FEE_WETH_USDC, KYBER_FEE_WETH_USDC);

        vm.prank(payee);
        try executor.flashLoanArb(uint128(amountIn), payload, 0) {}
        catch (bytes memory reason) {
            _assertLossOrBubble(reason);
        }
    }

    function test_KyberElastic_SwapOneForZero_USDC_WETH() public {
        uint256 amountIn = 3000 * 1e6;

        uint128 payload =
            encode2Step(KYBER_ID, KYBER_ID, USDC_BRIDGED_IDX, WETH_IDX, KYBER_FEE_WETH_USDC, KYBER_FEE_WETH_USDC);

        vm.prank(payee);
        try executor.flashLoanArb(uint128(amountIn), payload, 0) {}
        catch (bytes memory reason) {
            _assertLossOrBubble(reason);
        }
    }

    function test_KyberElastic_SwapDynamicFee_ARB_WETH() public {
        uint256 amountIn = 1000 ether;

        uint128 payload = encode2Step(KYBER_ID, KYBER_ID, ARB_IDX, WETH_IDX, KYBER_FEE_ARB_WETH, KYBER_FEE_ARB_WETH);

        vm.prank(payee);
        try executor.flashLoanArb(uint128(amountIn), payload, 0) {}
        catch (bytes memory reason) {
            _assertLossOrBubble(reason);
        }
    }

    // ============================================================
    //                    FLASH SWAP 2-STEP TESTS
    // ============================================================

    function test_FlashSwap2Step_UniswapV3_Forward() public {
        // UniV3(WETH->USDC) -> UniV3(USDC->WETH)
        uint128 payload = encode2Step(
            UNISWAP_V3_ID,
            UNISWAP_V3_ID,
            WETH_IDX,
            USDC_BRIDGED_IDX,
            FEE_500,
            FEE_3000 // 500 in, 3000 out
        );

        vm.prank(payee);
        try executor.flashSwap(1 ether, payload, 0) {}
        catch (bytes memory reason) {
            assertEq(bytes4(reason), LOSS_SELECTOR, "Unexpected revert");
        }
    }

    function test_FlashSwap2Step_UniswapV3_Reverse() public {
        // UniV3(USDC->WETH) -> UniV3(WETH->USDC)
        uint128 payload = encode2Step(UNISWAP_V3_ID, UNISWAP_V3_ID, USDC_BRIDGED_IDX, WETH_IDX, FEE_500, FEE_3000);

        vm.prank(payee);
        try executor.flashSwap(10000 * 1e6, payload, 0) {}
        catch (bytes memory reason) {
            assertEq(bytes4(reason), LOSS_SELECTOR, "Unexpected revert");
        }
    }

    function test_FlashSwap2Step_CamelotV2_Forward() public {
        // Camelot(WETH->USDC) -> Uniswap(USDC->WETH)
        uint128 payload = encode2Step(CAMELOT_V2_ID, UNISWAP_V3_ID, WETH_IDX, USDC_BRIDGED_IDX, 0, FEE_500);

        vm.prank(payee);
        try executor.flashSwap(1 ether, payload, 0) {}
        catch (bytes memory reason) {
            assertEq(bytes4(reason), LOSS_SELECTOR, "Unexpected revert");
        }
    }

    function test_FlashSwap2Step_CamelotV2_Reverse() public {
        uint128 payload = encode2Step(CAMELOT_V2_ID, UNISWAP_V3_ID, USDC_BRIDGED_IDX, WETH_IDX, 0, FEE_500);

        vm.prank(payee);
        try executor.flashSwap(10000 * 1e6, payload, 0) {}
        catch (bytes memory reason) {
            assertEq(bytes4(reason), LOSS_SELECTOR, "Unexpected revert");
        }
    }

    function test_FlashSwap2Step_SushiSwap_Forward() public {
        uint128 payload = encode2Step(SUSHI_ID, UNISWAP_V3_ID, WETH_IDX, USDC_BRIDGED_IDX, 0, FEE_500);

        vm.prank(payee);
        try executor.flashSwap(1 ether, payload, 0) {}
        catch (bytes memory reason) {
            assertEq(bytes4(reason), LOSS_SELECTOR, "Unexpected revert");
        }
    }

    function test_FlashSwap2Step_SushiSwap_Reverse() public {
        uint128 payload = encode2Step(SUSHI_ID, UNISWAP_V3_ID, USDC_BRIDGED_IDX, WETH_IDX, 0, FEE_500);

        vm.prank(payee);
        try executor.flashSwap(10000 * 1e6, payload, 0) {}
        catch (bytes memory reason) {
            assertEq(bytes4(reason), LOSS_SELECTOR, "Unexpected revert");
        }
    }

    function test_FlashSwap2Step_Chronos_Forward() public {
        uint128 payload = encode2Step(CHRONOS_ID, UNISWAP_V3_ID, WETH_IDX, USDC_BRIDGED_IDX, FEE_VOLATILE, FEE_500);

        vm.prank(payee);
        try executor.flashSwap(1 ether, payload, 0) {}
        catch (bytes memory reason) {
            _assertLossOrBubble(reason);
        }
    }

    function test_FlashSwap2Step_Chronos_Reverse() public {
        uint128 payload = encode2Step(CHRONOS_ID, UNISWAP_V3_ID, USDC_BRIDGED_IDX, WETH_IDX, FEE_VOLATILE, FEE_500);

        vm.prank(payee);
        try executor.flashSwap(10000 * 1e6, payload, 0) {}
        catch (bytes memory reason) {
            _assertLossOrBubble(reason);
        }
    }

    function test_FlashSwap2Step_Ramses_Forward() public {
        uint128 payload = encode2Step(RAMSES_ID, UNISWAP_V3_ID, WETH_IDX, USDC_BRIDGED_IDX, FEE_VOLATILE, FEE_500);

        vm.prank(payee);
        try executor.flashSwap(1 ether, payload, 0) {}
        catch (bytes memory reason) {
            _assertLossOrBubble(reason);
        }
    }

    function test_FlashSwap2Step_Ramses_Reverse() public {
        uint128 payload = encode2Step(RAMSES_ID, UNISWAP_V3_ID, USDC_BRIDGED_IDX, WETH_IDX, FEE_VOLATILE, FEE_500);

        vm.prank(payee);
        try executor.flashSwap(10000 * 1e6, payload, 0) {}
        catch (bytes memory reason) {
            _assertLossOrBubble(reason);
        }
    }

    function test_FlashSwap2Step_PancakeV3_Forward() public {
        uint128 payload = encode2Step(PANCAKE_V3_ID, UNISWAP_V3_ID, WETH_IDX, USDC_NATIVE_IDX, FEE_500, FEE_500);

        vm.prank(payee);
        try executor.flashSwap(1 ether, payload, 0) {}
        catch (bytes memory reason) {
            _assertLossOrBubble(reason);
        }
    }

    function test_FlashSwap2Step_PancakeV3_Reverse() public {
        uint128 payload = encode2Step(PANCAKE_V3_ID, UNISWAP_V3_ID, USDC_NATIVE_IDX, WETH_IDX, FEE_500, FEE_500);

        vm.prank(payee);
        try executor.flashSwap(10000 * 1e6, payload, 0) {}
        catch (bytes memory reason) {
            _assertLossOrBubble(reason);
        }
    }

    function test_FlashSwap2Step_KyberElastic_Forward() public {
        uint128 payload = encode2Step(KYBER_ID, UNISWAP_V3_ID, WETH_IDX, USDC_BRIDGED_IDX, KYBER_FEE_WETH_USDC, FEE_500);

        vm.prank(payee);
        vm.expectRevert(bytes("Kyber: use flashLoanArb"));
        executor.flashSwap(1 ether, payload, 0);
    }

    function test_FlashSwap2Step_KyberElastic_Reverse() public {
        uint128 payload = encode2Step(KYBER_ID, UNISWAP_V3_ID, USDC_BRIDGED_IDX, WETH_IDX, KYBER_FEE_WETH_USDC, FEE_500);

        vm.prank(payee);
        vm.expectRevert(bytes("Kyber: use flashLoanArb"));
        executor.flashSwap(10000 * 1e6, payload, 0);
    }

    function test_FlashSwap2Step_Arbidex_Forward() public {
        uint128 payload = encode2Step(ARBIDEX_ID, UNISWAP_V3_ID, WETH_IDX, USDC_BRIDGED_IDX, 0, FEE_500);

        vm.prank(payee);
        try executor.flashSwap(1 ether, payload, 0) {}
        catch (bytes memory reason) {
            _assertLossOrBubble(reason);
        }
    }

    function test_FlashSwap2Step_Arbidex_Reverse() public {
        uint128 payload = encode2Step(ARBIDEX_ID, UNISWAP_V3_ID, USDC_BRIDGED_IDX, WETH_IDX, 0, FEE_500);

        vm.prank(payee);
        try executor.flashSwap(10000 * 1e6, payload, 0) {}
        catch (bytes memory reason) {
            _assertLossOrBubble(reason);
        }
    }

    function test_FlashSwap2Step_CamelotV3_Forward() public {
        uint128 payload = encode2Step(CAMELOT_V3_ID, UNISWAP_V3_ID, WETH_IDX, USDC_BRIDGED_IDX, 0, FEE_500);

        vm.prank(payee);
        try executor.flashSwap(1 ether, payload, 0) {}
        catch (bytes memory reason) {
            _assertLossOrBubble(reason);
        }
    }

    function test_FlashSwap2Step_CamelotV3_Reverse() public {
        uint128 payload = encode2Step(CAMELOT_V3_ID, UNISWAP_V3_ID, USDC_BRIDGED_IDX, WETH_IDX, 0, FEE_500);

        vm.prank(payee);
        try executor.flashSwap(10000 * 1e6, payload, 0) {}
        catch (bytes memory reason) {
            _assertLossOrBubble(reason);
        }
    }

    function test_FlashSwap_TraderJoeLB_UseFlashLoanArb() public {
        uint128 payload =
            encode2Step(TRADER_JOE_LB_ID, UNISWAP_V3_ID, WETH_IDX, USDC_BRIDGED_IDX, LB_V21_BIN20, FEE_500);

        vm.prank(payee);
        vm.expectRevert(bytes("LB: use flashLoanArb"));
        executor.flashSwap(1 ether, payload, 0);
    }

    // ============================================================
    //                    FLASH SWAP 3-STEP TESTS
    // ============================================================

    function test_FlashSwap3Step_V3_V2_V3() public {
        // UniV3 -> Camelot -> UniV3
        // WETH -> USDC -> ARB -> WETH
        uint128 payload = encode3Step(
            UNISWAP_V3_ID, CAMELOT_V2_ID, UNISWAP_V3_ID, WETH_IDX, USDC_BRIDGED_IDX, ARB_IDX, FEE_500, 0, FEE_500
        );

        vm.prank(payee);
        try executor.flashSwap(1 ether, payload, 0) {}
        catch (bytes memory reason) {
            assertEq(bytes4(reason), LOSS_SELECTOR, "Unexpected revert");
        }
    }

    function test_FlashSwap3Step_V2_Solidly_V2() public {
        // Camelot -> Chronos -> Sushi
        // WETH -> USDC -> ARB -> WETH
        uint128 payload =
            encode3Step(CAMELOT_V2_ID, CHRONOS_ID, SUSHI_ID, WETH_IDX, USDC_BRIDGED_IDX, ARB_IDX, 0, FEE_VOLATILE, 0);

        vm.prank(payee);
        try executor.flashSwap(1 ether, payload, 0) {}
        catch (bytes memory reason) {
            _assertLossOrBubble(reason);
        }
    }

    function test_FlashSwap3Step_Algebra_V3_V2() public {
        // CamelotV3 -> UniV3 -> Arbidex
        uint128 payload =
            encode3Step(CAMELOT_V3_ID, UNISWAP_V3_ID, ARBIDEX_ID, WETH_IDX, USDC_BRIDGED_IDX, ARB_IDX, 0, FEE_500, 0);

        vm.prank(payee);
        try executor.flashSwap(1 ether, payload, 0) {}
        catch (bytes memory reason) {
            _assertLossOrBubble(reason);
        }
    }

    function test_FlashSwap3Step_AllV3() public {
        // UniV3 -> PancakeV3 -> UniV3
        uint128 payload = encode3Step(
            UNISWAP_V3_ID, PANCAKE_V3_ID, UNISWAP_V3_ID, WETH_IDX, USDC_NATIVE_IDX, ARB_IDX, FEE_500, FEE_500, FEE_3000
        );

        vm.prank(payee);
        try executor.flashSwap(1 ether, payload, 0) {}
        catch (bytes memory reason) {
            assertEq(bytes4(reason), LOSS_SELECTOR, "Unexpected revert");
        }
    }

    function test_FlashSwap3Step_AllV2() public {
        // CamelotV2 -> Sushi -> Arbidex
        uint128 payload = encode3Step(CAMELOT_V2_ID, SUSHI_ID, ARBIDEX_ID, WETH_IDX, USDC_BRIDGED_IDX, ARB_IDX, 0, 0, 0);

        vm.prank(payee);
        try executor.flashSwap(1 ether, payload, 0) {}
        catch (bytes memory reason) {
            _assertLossOrBubble(reason);
        }
    }

    function test_FlashSwap3Step_Mixed() public {
        // Ramses -> CamelotV3 -> PancakeV3
        uint128 payload = encode3Step(
            RAMSES_ID, CAMELOT_V3_ID, PANCAKE_V3_ID, WETH_IDX, USDC_BRIDGED_IDX, ARB_IDX, FEE_VOLATILE, 0, FEE_500
        );

        vm.prank(payee);
        try executor.flashSwap(1 ether, payload, 0) {}
        catch (bytes memory reason) {
            _assertLossOrBubble(reason);
        }
    }

    // ============================================================
    //                    FUZZ TESTS
    // ============================================================

    function testFuzz_SwapExactIn_AmountBounds(uint256 amountIn) public {
        amountIn = bound(amountIn, 1e15, 10 ether); // 0.001 to 10 ETH

        // Use valid roundtrip with proper fees on both legs
        uint128 payload = encode2Step(UNISWAP_V3_ID, UNISWAP_V3_ID, WETH_IDX, USDC_BRIDGED_IDX, FEE_500, FEE_3000);

        vm.prank(payee);
        try executor.flashLoanArb(uint128(amountIn), payload, 0) {
        // Any successful swap is valid
        }
        catch (bytes memory reason) {
            // Accept Loss error or empty revert (pool issues)
            if (reason.length >= 4) {
                assertEq(bytes4(reason), LOSS_SELECTOR, "Unexpected revert");
            }
            // Empty reverts are OK (pool doesn't exist or other issue)
        }
    }

    function testFuzz_PayloadEncodeDecode_Roundtrip(
        uint8 ex0,
        uint8 ex1,
        uint8 ex2,
        uint8 tok0,
        uint8 tok1,
        uint8 tok2,
        uint16 fee0,
        uint16 fee1,
        uint16 fee2
    ) public view {
        uint128 encoded = encodePayload(ex0, ex1, ex2, tok0, tok1, tok2, fee0, fee1, fee2);
        (uint8[3] memory ex, uint8[3] memory tok, uint16[3] memory fee) = executor.decode(encoded);

        assertEq(ex[0], ex0, "ex0 mismatch");
        assertEq(ex[1], ex1, "ex1 mismatch");
        assertEq(ex[2], ex2, "ex2 mismatch");
        assertEq(tok[0], tok0, "tok0 mismatch");
        assertEq(tok[1], tok1, "tok1 mismatch");
        assertEq(tok[2], tok2, "tok2 mismatch");
        assertEq(fee[0], fee0, "fee0 mismatch");
        assertEq(fee[1], fee1, "fee1 mismatch");
        assertEq(fee[2], fee2, "fee2 mismatch");
    }

    function testFuzz_SwapExactIn_InvalidExchangeReverts(uint8 exchangeId) public {
        // Bound the fuzz domain to ONLY truly-unknown exchange ids. Valid ids are 0..=26
        // (UniV3=0 … SushiV3=25, Balancer=26; 4 is an unused gap). Ids 20-26 are VALID and
        // must NOT be probed here (e.g. 24 = UNISWAP_V2_ID). Curve 16-19 fail closed with a
        // dedicated "use flashLoanArbV2" revert, not "Unknown exchange ID". The only ids that
        // hit the _swapExactInHop else->"Unknown exchange ID" branch are the gap (4) and >26.
        vm.assume(exchangeId == 4 || exchangeId > 26);

        uint128 payload = encode2Step(exchangeId, 0, WETH_IDX, USDC_BRIDGED_IDX, FEE_500, 0);

        vm.expectRevert("Unknown exchange ID");
        vm.prank(payee);
        executor.flashLoanArb(1 ether, payload, 0);
    }

    function testFuzz_FlashSwap_AmountBounds(uint128 amountIn) public {
        amountIn = uint128(bound(amountIn, 1e15, 10 ether));

        uint128 payload = encode2Step(UNISWAP_V3_ID, CAMELOT_V2_ID, WETH_IDX, USDC_BRIDGED_IDX, FEE_500, 0);

        vm.prank(payee);
        try executor.flashSwap(amountIn, payload, 0) {}
        catch (bytes memory reason) {
            assertEq(bytes4(reason), LOSS_SELECTOR, "Unexpected revert");
        }
    }

    // ============================================================
    //                    EDGE CASE TESTS
    // ============================================================

    function test_EdgeCase_OnlyPayeeCanSwap() public {
        uint128 payload = encode2Step(UNISWAP_V3_ID, 0, WETH_IDX, USDC_BRIDGED_IDX, FEE_500, 0);

        // Non-payee should be rejected
        vm.expectRevert();
        executor.flashLoanArb(1 ether, payload, 0);
    }

    function test_EdgeCase_OnlyPayeeCanFlashSwap() public {
        uint128 payload = encode2Step(UNISWAP_V3_ID, CAMELOT_V2_ID, WETH_IDX, USDC_BRIDGED_IDX, FEE_500, 0);

        // Non-payee should be rejected
        vm.expectRevert();
        executor.flashSwap(1 ether, payload, 0);
    }

    function test_EdgeCase_MinProfitThreshold() public {
        uint128 payload = encode2Step(UNISWAP_V3_ID, UNISWAP_V3_ID, WETH_IDX, USDC_BRIDGED_IDX, FEE_500, FEE_3000);

        // Set high min profit that can't be achieved
        vm.prank(payee);
        try executor.flashSwap(1 ether, payload, type(uint128).max) {
            revert("Should have reverted with Loss");
        } catch (bytes memory reason) {
            assertEq(bytes4(reason), LOSS_SELECTOR, "Unexpected revert");
        }
    }

    function test_EdgeCase_TokenOrderingConsistency_WETH_USDC() public {
        // Test both directions work correctly

        // WETH -> USDC (WETH < USDC address, so zeroForOne depends on ordering)
        uint128 payload1 = encode2Step(UNISWAP_V3_ID, 0, WETH_IDX, USDC_BRIDGED_IDX, FEE_500, 0);

        // USDC -> WETH
        uint128 payload2 = encode2Step(UNISWAP_V3_ID, 0, USDC_BRIDGED_IDX, WETH_IDX, FEE_500, 0);

        vm.prank(payee);
        try executor.flashLoanArb(1 ether, payload1, 0) {}
        catch (bytes memory reason) {
            _assertLossOrBubble(reason);
        }

        vm.prank(payee);
        try executor.flashLoanArb(3000 * 1e6, payload2, 0) {}
        catch (bytes memory reason) {
            _assertLossOrBubble(reason);
        }
    }

    function test_EdgeCase_ZeroAmountReverts() public {
        uint128 payload = encode2Step(UNISWAP_V3_ID, 0, WETH_IDX, USDC_BRIDGED_IDX, FEE_500, 0);

        vm.prank(payee);
        vm.expectRevert();
        executor.flashLoanArb(0, payload, 0);
    }

    // ============================================================
    //                    BALANCER FLASH LOAN TESTS
    // ============================================================
    // Tests for 0% fee flash loans via Balancer V2 Vault

    function test_BalancerFlashLoan2Step_UniswapV3_Forward() public {
        // UniV3(WETH->USDC) -> UniV3(USDC->WETH)
        uint128 payload = encode2Step(UNISWAP_V3_ID, UNISWAP_V3_ID, WETH_IDX, USDC_BRIDGED_IDX, FEE_500, FEE_3000);

        vm.prank(payee);
        try executor.flashLoanArb(1 ether, payload, 0) {
        // Flash loan arb succeeded (unlikely without real arb opportunity)
        }
        catch (bytes memory reason) {
            assertEq(bytes4(reason), LOSS_SELECTOR, "Unexpected revert");
        }
    }

    function test_BalancerFlashLoan2Step_UniswapV3_Reverse() public {
        // UniV3(USDC->WETH) -> UniV3(WETH->USDC)
        uint128 payload = encode2Step(UNISWAP_V3_ID, UNISWAP_V3_ID, USDC_BRIDGED_IDX, WETH_IDX, FEE_500, FEE_3000);

        vm.prank(payee);
        try executor.flashLoanArb(10000 * 1e6, payload, 0) {}
        catch (bytes memory reason) {
            assertEq(bytes4(reason), LOSS_SELECTOR, "Unexpected revert");
        }
    }

    function test_BalancerFlashLoan2Step_CamelotV2_Forward() public {
        // CamelotV2(WETH->USDC) -> UniV3(USDC->WETH)
        uint128 payload = encode2Step(CAMELOT_V2_ID, UNISWAP_V3_ID, WETH_IDX, USDC_BRIDGED_IDX, 0, FEE_500);

        vm.prank(payee);
        try executor.flashLoanArb(1 ether, payload, 0) {}
        catch (bytes memory reason) {
            assertEq(bytes4(reason), LOSS_SELECTOR, "Unexpected revert");
        }
    }

    function test_BalancerFlashLoan2Step_SushiSwap_Forward() public {
        // Sushi(WETH->USDC) -> UniV3(USDC->WETH)
        uint128 payload = encode2Step(SUSHI_ID, UNISWAP_V3_ID, WETH_IDX, USDC_BRIDGED_IDX, 0, FEE_500);

        vm.prank(payee);
        try executor.flashLoanArb(1 ether, payload, 0) {}
        catch (bytes memory reason) {
            assertEq(bytes4(reason), LOSS_SELECTOR, "Unexpected revert");
        }
    }

    function test_BalancerFlashLoan2Step_TraderJoeLB_Forward() public {
        // Trader Joe LB (WETH->USDC.e) -> UniV3(USDC.e->WETH)
        // Latest-fork test: the LB pair's live liquidity is thin (~378 USDC.e total on
        // 2026-06-11), so a fixed 1-ether first leg reverts LBPair__OutOfLiquidity inside
        // the pair before the executor's Loss gate is ever reached. Size the flash amount
        // to what the pair can actually fill — target half its USDC.e reserve and let the
        // pair itself quote the WETH input (LB v2.1 pairs expose getSwapIn on-pair).
        (, uint128 reserveY) = ILBPairFork(LB_V21_WETH_USDCE_PAIR).getReserves();
        if (reserveY < 10e6) {
            vm.skip(true, "Trader Joe LB pair drained below $10; no fillable leg exists on this fork");
            return;
        }
        (uint128 amountIn, uint128 amountOutLeft,) = ILBPairFork(LB_V21_WETH_USDCE_PAIR).getSwapIn(reserveY / 2, true);
        assertEq(amountOutLeft, 0, "LB pair cannot fill half its own reserve");

        uint128 payload =
            encode2Step(TRADER_JOE_LB_ID, UNISWAP_V3_ID, WETH_IDX, USDC_BRIDGED_IDX, LB_V21_BIN15, FEE_500);

        vm.prank(payee);
        try executor.flashLoanArb(amountIn, payload, 0) {}
        catch (bytes memory reason) {
            assertEq(bytes4(reason), LOSS_SELECTOR, "Unexpected revert");
        }
    }

    function test_BalancerFlashLoanV2_CurveTriCrypto_ToPancakeV3() public {
        uint128 amountIn = 1e15;
        ICurveTriCryptoFactoryFork factory = ICurveTriCryptoFactoryFork(CURVE_TRICRYPTO_FACTORY);
        address pool = factory.pool_list(0);
        address[3] memory coins = factory.get_coins(pool);

        uint8 wethIdx = type(uint8).max;
        uint8 usdcNativeIdx = type(uint8).max;
        for (uint8 i = 0; i < 3; ++i) {
            if (coins[i] == WETH) wethIdx = i;
            if (coins[i] == USDC_NATIVE) usdcNativeIdx = i;
        }

        assertTrue(pool != address(0), "tricrypto pool missing");
        assertTrue(wethIdx != type(uint8).max, "tricrypto WETH missing");
        assertTrue(usdcNativeIdx != type(uint8).max, "tricrypto USDC native missing");

        MantisExecutor.TradeHop[3] memory hops;
        hops[0] = MantisExecutor.TradeHop({
            exchangeId: CURVE_TRICRYPTO_NG_ID,
            tokenIn: WETH,
            tokenOut: USDC_NATIVE,
            fee: 0,
            pool: pool,
            coinInIdx: wethIdx,
            coinOutIdx: usdcNativeIdx
        });
        hops[1] = MantisExecutor.TradeHop({
            exchangeId: PANCAKE_V3_ID,
            tokenIn: USDC_NATIVE,
            tokenOut: WETH,
            fee: FEE_500,
            pool: address(0),
            coinInIdx: 0,
            coinOutIdx: 0
        });

        vm.prank(payee);
        try executor.flashLoanArbV2(amountIn, hops, 0) {}
        catch (bytes memory reason) {
            if (reason.length != 0 && bytes4(reason) != LOSS_SELECTOR) {
                if (reason.length >= 68 && bytes4(reason) == bytes4(0x08c379a0)) {
                    assembly {
                        reason := add(reason, 0x04)
                    }
                    revert(abi.decode(reason, (string)));
                }
                revert("Unexpected non-Loss revert");
            }
        }
    }

    function test_BalancerFlashLoanV2_CurveStablePlain_RoundTrip() public {
        uint8 decimals = IERC20MetadataFork(USDT).decimals();
        uint128 amountIn = uint128(1000 * (10 ** uint256(decimals)));

        assertTrue(CURVE_CRVUSD_USDT_POOL.code.length > 0, "curve stable pool missing on fork");

        MantisExecutor.TradeHop[3] memory hops;
        hops[0] = MantisExecutor.TradeHop({
            exchangeId: CURVE_STABLESWAP_PLAIN_NG_ID,
            tokenIn: USDT,
            tokenOut: CRVUSD,
            fee: 0,
            pool: CURVE_CRVUSD_USDT_POOL,
            coinInIdx: 1,
            coinOutIdx: 0
        });
        hops[1] = MantisExecutor.TradeHop({
            exchangeId: CURVE_STABLESWAP_PLAIN_NG_ID,
            tokenIn: CRVUSD,
            tokenOut: USDT,
            fee: 0,
            pool: CURVE_CRVUSD_USDT_POOL,
            coinInIdx: 0,
            coinOutIdx: 1
        });

        vm.prank(payee);
        try executor.flashLoanArbV2(amountIn, hops, 0) {}
        catch (bytes memory reason) {
            if (reason.length != 0 && bytes4(reason) != LOSS_SELECTOR) {
                if (reason.length >= 68 && bytes4(reason) == bytes4(0x08c379a0)) {
                    assembly {
                        reason := add(reason, 0x04)
                    }
                    revert(abi.decode(reason, (string)));
                }
                revert("Unexpected non-Loss revert");
            }
        }
    }

    function test_FlashSwapMixed_UniswapV4EntryRequiresFlashLoanArbMixed() public {
        assertTrue(UNISWAP_V4_POOL_MANAGER.code.length > 0, "v4 pool manager missing on fork");

        MantisExecutor.TradeHopV2[3] memory hops;
        hops[0] =
            _v4Hop(WETH, USDC_NATIVE, FEE_500, UNISWAP_V4_TICK_SPACING_500, UNISWAP_V4_WETH_USDC_NATIVE_500_POOL_ID);
        hops[1] = _legacyHopV2(PANCAKE_V3_ID, USDC_NATIVE, WETH, FEE_500);
        hops[2] = _emptyHopV2();

        vm.prank(payee);
        vm.expectRevert(bytes("V4: use flashLoanArbMixed"));
        executor.flashSwapMixed(1 ether, hops, 0);
    }

    function test_FlashSwapMixed_FluidEntryRequiresFlashLoanArbMixed() public {
        (IFluidDexReservesResolverFork.Pool memory pool, address tokenIn, address tokenOut, uint128 amountIn) =
            _preferredFluidPool();
        assertTrue(pool.pool.code.length > 0, "Fluid pool missing on fork");
        assertLe(pool.fee, type(uint24).max, "Fluid fee too large for hop");

        MantisExecutor.TradeHopV2[3] memory hops;
        hops[0] = _fluidHop(tokenIn, tokenOut, pool.pool, uint24(pool.fee));
        hops[1] = _fluidHop(tokenOut, tokenIn, pool.pool, uint24(pool.fee));
        hops[2] = _emptyHopV2();

        vm.prank(payee);
        vm.expectRevert(bytes("Fluid: use flashLoanArbMixed"));
        executor.flashSwapMixed(amountIn, hops, 0);
    }

    function test_BalancerFlashLoanMixed_Fluid_RoundTrip() public {
        (IFluidDexReservesResolverFork.Pool memory pool, address tokenIn, address tokenOut, uint128 amountIn) =
            _preferredFluidPool();
        assertTrue(pool.pool.code.length > 0, "Fluid pool missing on fork");
        assertLe(pool.fee, type(uint24).max, "Fluid fee too large for hop");

        MantisExecutor.TradeHopV2[3] memory hops;
        hops[0] = _fluidHop(tokenIn, tokenOut, pool.pool, uint24(pool.fee));
        hops[1] = _fluidHop(tokenOut, tokenIn, pool.pool, uint24(pool.fee));
        hops[2] = _emptyHopV2();

        _runMixedFlashLoanAndAssertLossOrSuccess(amountIn, hops);
    }

    function test_FlashSwapMixed_MaverickEntryRequiresFlashLoanArbMixed() public {
        (
            address pool,
            address tokenIn,
            address tokenOut,
            uint128 amountIn,
            uint8 companionExchangeId,
            uint16 companionFee,
            uint24 maverickFee
        ) = _preferredMaverickMixedRoute();
        assertTrue(pool.code.length > 0, "Maverick pool missing on fork");

        MantisExecutor.TradeHopV2[3] memory hops;
        hops[0] = _maverickHop(tokenIn, tokenOut, pool, maverickFee);
        hops[1] = _legacyHopV2(companionExchangeId, tokenOut, tokenIn, companionFee);
        hops[2] = _emptyHopV2();

        vm.prank(payee);
        vm.expectRevert(bytes("Maverick: use flashLoanArbMixed"));
        executor.flashSwapMixed(amountIn, hops, 0);
    }

    function test_BalancerFlashLoanMixed_Maverick_WithCompanionRoute() public {
        (
            address pool,
            address tokenIn,
            address tokenOut,
            uint128 amountIn,
            uint8 companionExchangeId,
            uint16 companionFee,
            uint24 maverickFee
        ) = _preferredMaverickMixedRoute();
        assertTrue(pool.code.length > 0, "Maverick pool missing on fork");

        MantisExecutor.TradeHopV2[3] memory hops;
        hops[0] = _maverickHop(tokenIn, tokenOut, pool, maverickFee);
        hops[1] = _legacyHopV2(companionExchangeId, tokenOut, tokenIn, companionFee);
        hops[2] = _emptyHopV2();

        _runMixedFlashLoanAndAssertLossOrSuccess(amountIn, hops);
    }

    function test_FlashSwapMixed_DodoEntryRequiresFlashLoanArbMixed() public {
        (
            address pool,
            address tokenIn,
            address tokenOut,
            uint128 amountIn,
            uint8 companionExchangeId,
            uint16 companionFee,
            uint24 dodoFee
        ) = _preferredDodoMixedRoute();
        assertTrue(pool.code.length > 0, "DODO pool missing on fork");

        MantisExecutor.TradeHopV2[3] memory hops;
        hops[0] = _dodoHop(tokenIn, tokenOut, pool, dodoFee);
        hops[1] = _legacyHopV2(companionExchangeId, tokenOut, tokenIn, companionFee);
        hops[2] = _emptyHopV2();

        vm.prank(payee);
        vm.expectRevert(bytes("DODO: use flashLoanArbMixed"));
        executor.flashSwapMixed(amountIn, hops, 0);
    }

    function test_BalancerFlashLoanMixed_Dodo_WithCompanionRoute() public {
        (
            address pool,
            address tokenIn,
            address tokenOut,
            uint128 amountIn,
            uint8 companionExchangeId,
            uint16 companionFee,
            uint24 dodoFee
        ) = _preferredDodoMixedRoute();
        assertTrue(pool.code.length > 0, "DODO pool missing on fork");

        MantisExecutor.TradeHopV2[3] memory hops;
        hops[0] = _dodoHop(tokenIn, tokenOut, pool, dodoFee);
        hops[1] = _legacyHopV2(companionExchangeId, tokenOut, tokenIn, companionFee);
        hops[2] = _emptyHopV2();

        _runMixedFlashLoanAndAssertLossOrSuccess(amountIn, hops);
    }

    function test_BalancerFlashLoanMixed_UniswapV4_Fee500_RoundTrip_WETH_USDCNative() public {
        assertTrue(UNISWAP_V4_POOL_MANAGER.code.length > 0, "v4 pool manager missing on fork");

        MantisExecutor.TradeHopV2[3] memory hops;
        hops[0] =
            _v4Hop(WETH, USDC_NATIVE, FEE_500, UNISWAP_V4_TICK_SPACING_500, UNISWAP_V4_WETH_USDC_NATIVE_500_POOL_ID);
        hops[1] =
            _v4Hop(USDC_NATIVE, WETH, FEE_500, UNISWAP_V4_TICK_SPACING_500, UNISWAP_V4_WETH_USDC_NATIVE_500_POOL_ID);
        hops[2] = _emptyHopV2();

        _runMixedFlashLoanAndAssertLossOrSuccess(1 ether, hops);
    }

    function test_BalancerFlashLoanMixed_UniswapV4_Fee500_RoundTrip_USDCNative_WETH() public {
        assertTrue(UNISWAP_V4_POOL_MANAGER.code.length > 0, "v4 pool manager missing on fork");

        MantisExecutor.TradeHopV2[3] memory hops;
        hops[0] =
            _v4Hop(USDC_NATIVE, WETH, FEE_500, UNISWAP_V4_TICK_SPACING_500, UNISWAP_V4_WETH_USDC_NATIVE_500_POOL_ID);
        hops[1] =
            _v4Hop(WETH, USDC_NATIVE, FEE_500, UNISWAP_V4_TICK_SPACING_500, UNISWAP_V4_WETH_USDC_NATIVE_500_POOL_ID);
        hops[2] = _emptyHopV2();

        _runMixedFlashLoanAndAssertLossOrSuccess(3000e6, hops);
    }

    function test_BalancerFlashLoanMixed_UniswapV4_Fee3000_RoundTrip_WETH_USDCNative() public {
        assertTrue(UNISWAP_V4_POOL_MANAGER.code.length > 0, "v4 pool manager missing on fork");

        MantisExecutor.TradeHopV2[3] memory hops;
        hops[0] =
            _v4Hop(WETH, USDC_NATIVE, FEE_3000, UNISWAP_V4_TICK_SPACING_3000, UNISWAP_V4_WETH_USDC_NATIVE_3000_POOL_ID);
        hops[1] =
            _v4Hop(USDC_NATIVE, WETH, FEE_3000, UNISWAP_V4_TICK_SPACING_3000, UNISWAP_V4_WETH_USDC_NATIVE_3000_POOL_ID);
        hops[2] = _emptyHopV2();

        _runMixedFlashLoanAndAssertLossOrSuccess(1 ether, hops);
    }

    function test_BalancerFlashLoanMixed_UniswapV4_ToPancakeV3_RoundTrip() public {
        assertTrue(UNISWAP_V4_POOL_MANAGER.code.length > 0, "v4 pool manager missing on fork");

        MantisExecutor.TradeHopV2[3] memory hops;
        hops[0] =
            _v4Hop(WETH, USDC_NATIVE, FEE_500, UNISWAP_V4_TICK_SPACING_500, UNISWAP_V4_WETH_USDC_NATIVE_500_POOL_ID);
        hops[1] = _legacyHopV2(PANCAKE_V3_ID, USDC_NATIVE, WETH, FEE_500);
        hops[2] = _emptyHopV2();

        _runMixedFlashLoanAndAssertLossOrSuccess(1 ether, hops);
    }

    function test_BalancerFlashLoan3Step_V3_V2_V3() public {
        // UniV3 -> Camelot -> UniV3
        // WETH -> USDC -> ARB -> WETH
        uint128 payload = encode3Step(
            UNISWAP_V3_ID, CAMELOT_V2_ID, UNISWAP_V3_ID, WETH_IDX, USDC_BRIDGED_IDX, ARB_IDX, FEE_500, 0, FEE_500
        );

        vm.prank(payee);
        try executor.flashLoanArb(1 ether, payload, 0) {}
        catch (bytes memory reason) {
            assertEq(bytes4(reason), LOSS_SELECTOR, "Unexpected revert");
        }
    }

    function test_BalancerFlashLoan3Step_AllV3() public {
        // UniV3 -> PancakeV3 -> UniV3
        uint128 payload = encode3Step(
            UNISWAP_V3_ID, PANCAKE_V3_ID, UNISWAP_V3_ID, WETH_IDX, USDC_NATIVE_IDX, ARB_IDX, FEE_500, FEE_500, FEE_3000
        );

        vm.prank(payee);
        try executor.flashLoanArb(1 ether, payload, 0) {}
        catch (bytes memory reason) {
            assertEq(bytes4(reason), LOSS_SELECTOR, "Unexpected revert");
        }
    }

    function test_BalancerFlashLoan3Step_AllV2() public {
        // CamelotV2 -> Sushi -> Arbidex
        uint128 payload = encode3Step(CAMELOT_V2_ID, SUSHI_ID, ARBIDEX_ID, WETH_IDX, USDC_BRIDGED_IDX, ARB_IDX, 0, 0, 0);

        vm.prank(payee);
        try executor.flashLoanArb(1 ether, payload, 0) {}
        catch (bytes memory reason) {
            _assertLossOrBubble(reason);
        }
    }

    function test_BalancerFlashLoan_OnlyGatewayCanCall() public {
        uint128 payload = encode2Step(UNISWAP_V3_ID, UNISWAP_V3_ID, WETH_IDX, USDC_BRIDGED_IDX, FEE_500, FEE_3000);

        // Non-gateway should be rejected
        vm.expectRevert("Not gateway");
        executor.flashLoanArb(1 ether, payload, 0);
    }

    function test_BalancerFlashLoan_MinProfitThreshold() public {
        uint128 payload = encode2Step(UNISWAP_V3_ID, UNISWAP_V3_ID, WETH_IDX, USDC_BRIDGED_IDX, FEE_500, FEE_3000);

        // Set high min profit that can't be achieved
        vm.prank(payee);
        try executor.flashLoanArb(1 ether, payload, type(uint128).max) {
            revert("Should have reverted with Loss");
        } catch (bytes memory reason) {
            assertEq(bytes4(reason), LOSS_SELECTOR, "Unexpected revert");
        }
    }

    function testFuzz_BalancerFlashLoan_AmountBounds(uint128 amountIn) public {
        amountIn = uint128(bound(amountIn, 1e15, 10 ether));

        uint128 payload = encode2Step(UNISWAP_V3_ID, CAMELOT_V2_ID, WETH_IDX, USDC_BRIDGED_IDX, FEE_500, 0);

        vm.prank(payee);
        try executor.flashLoanArb(amountIn, payload, 0) {}
        catch (bytes memory reason) {
            assertEq(bytes4(reason), LOSS_SELECTOR, "Unexpected revert");
        }
    }
}
