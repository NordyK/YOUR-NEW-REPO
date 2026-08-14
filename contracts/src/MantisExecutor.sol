// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;
pragma abicoder v2;

import "./External.sol";
import "./generated/TokenLookupData.sol";
import "@openzeppelin/contracts/security/ReentrancyGuard.sol";
import {IERC20 as OZIERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";

interface ICurveCryptoSwap {
    function exchange(uint256 i, uint256 j, uint256 dx, uint256 min_dy) external returns (uint256);
}

interface ICurveStableSwapPlainNg {
    function exchange(int128 i, int128 j, uint256 dx, uint256 min_dy) external returns (uint256);
}

interface ICurveStableSwapMetaNg {
    function exchange_underlying(int128 i, int128 j, uint256 dx, uint256 min_dy) external returns (uint256);
}

interface IFluidDexT1 {
    function swapIn(bool swap0to1_, uint256 amountIn_, uint256 amountOutMin_, address to_)
        external
        payable
        returns (uint256 amountOut_);
}

interface IMaverickV2Pool {
    struct SwapParams {
        uint256 amount;
        bool tokenAIn;
        bool exactOutput;
        int32 tickLimit;
    }

    function swap(address recipient, SwapParams memory params, bytes calldata data)
        external
        returns (uint256 amountIn, uint256 amountOut);
}

interface IDODOV2Pool {
    function sellBase(address to) external returns (uint256 receiveQuoteAmount);
    function sellQuote(address to) external returns (uint256 receiveBaseAmount);
    function _BASE_TOKEN_() external view returns (address);
    function _QUOTE_TOKEN_() external view returns (address);
}

/// MANTIS: Aave V3 Pool — alternate flash-loan funding used when the route trades a Balancer
/// pool (the Balancer Vault is non-reentrant, so it cannot be both flash source and swap venue
/// in one tx). Aave pulls repayment via allowance in executeOperation.
interface IAaveV3Pool {
    function flashLoanSimple(
        address receiverAddress,
        address asset,
        uint256 amount,
        bytes calldata params,
        uint16 referralCode
    ) external;
    function FLASHLOAN_PREMIUM_TOTAL() external view returns (uint128);
}

/// MANTIS: Sushi's V3 SwapRouter on Arbitrum (0x8A21F676…) is the ORIGINAL Uniswap V3
/// SwapRouter, whose exactInputSingle carries a `deadline` field (unlike SwapRouter02).
/// Calling the no-deadline variant empty-reverts (verified on-fork). Use this ABI for SushiV3.
interface ISushiV3SwapRouter {
    struct ExactInputSingleParams {
        address tokenIn;
        address tokenOut;
        uint24 fee;
        address recipient;
        uint256 deadline;
        uint256 amountIn;
        uint256 amountOutMinimum;
        uint160 sqrtPriceLimitX96;
    }

    function exactInputSingle(ExactInputSingleParams calldata params) external payable returns (uint256 amountOut);
}

/// MANTIS: Balancer V2 Vault single-swap, for trading a Balancer pool as a route leg
/// (only legal when funding is NOT a Balancer flash loan — see funding guards).
interface IBalancerVaultSingleSwap {
    enum SwapKind {
        GIVEN_IN,
        GIVEN_OUT
    }

    struct SingleSwap {
        bytes32 poolId;
        SwapKind kind;
        address assetIn;
        address assetOut;
        uint256 amount;
        bytes userData;
    }

    struct FundManagement {
        address sender;
        bool fromInternalBalance;
        address payable recipient;
        bool toInternalBalance;
    }

    function swap(SingleSwap memory singleSwap, FundManagement memory funds, uint256 limit, uint256 deadline)
        external
        payable
        returns (uint256 amountCalculated);
}

/// Prioritizes a lightweight ABI to make offchain component as fast as possible vs. gas cost savings
/// FIX (SEC-002): Added ReentrancyGuard to protect flash callback functions from reentrancy attacks
/// FIX (SEC-003): Added minNetProfit slippage and adverse-move protection
/// FIX (C1): Using SafeERC20.safeTransfer for all ERC20 transfers to handle non-standard return values
contract MantisExecutor is IUniswapV3SwapCallback, IUnlockCallback, ReentrancyGuard {
    using SafeERC20 for OZIERC20;
    using BalanceDeltaLibrary for BalanceDelta;

    struct TradeHop {
        uint8 exchangeId;
        address tokenIn;
        address tokenOut;
        uint16 fee;
        address pool;
        uint8 coinInIdx;
        uint8 coinOutIdx;
    }

    struct TradeHopV2 {
        uint8 exchangeId;
        address tokenIn;
        address tokenOut;
        uint24 fee;
        address pool;
        uint8 coinInIdx;
        uint8 coinOutIdx;
        int24 tickSpacing;
        address hooks;
        address poolManager;
        bytes32 poolId;
    }

    // uniV3 constants
    uint160 internal constant MIN_SQRT_RATIO = 4295128739;
    uint160 internal constant MAX_SQRT_RATIO = 1461446703485210103287273052203988822378723970342;
    // where to send profits
    address public payee;
    // gateway contract
    address public gateway;
    address[] public tokenLookup;

    /// @notice Minimum net profit required for current flash swap (SEC-003)
    /// @dev Set at entry, checked in payback(), cleared after use
    uint256 private _minNetProfit;

    error Loss(uint256);
    /// @notice Trade profit below minimum threshold
    error InsufficientProfit(uint256 actual, uint256 minimum);
    /// @notice A flash callback fired without a gateway-authorized flash in progress (SEC-FLASH-ORIGIN).
    error UnsolicitedFlashCallback();

    // === SEC-FLASH-ORIGIN: transient flash-origin guard (EIP-1153; Arbitrum ArbOS >= 20) ===
    // The Balancer Vault's `receiveFlashLoan` callback only authenticates `msg.sender == VAULT`,
    // and ANYONE can call `Vault.flashLoan(recipient = thisContract, userData = craftedRoute)` — so
    // without this guard an attacker could drive the contract's exotic-venue hops (whose `pool`
    // address is attacker-controlled and NOT factory-verified) and drain any token the contract
    // holds. This transient flag is set ONLY inside a gateway-authorized flash entrypoint
    // (`msg.sender == gateway`) immediately before the external flash call, and REQUIRED by the
    // flash callbacks. An unsolicited callback therefore reverts (the flag is 0). Transient storage
    // auto-clears at end of transaction, so a forgotten clear cannot leak across txs.
    uint256 private constant _FLASH_GUARD_SLOT =
        0x9f204e7a5b7ccf76302e5d461789e75c1dc2cd75282f307e8e69ecbc4496b1eb;

    function _setFlashGuard() private {
        assembly { tstore(_FLASH_GUARD_SLOT, 1) }
    }

    function _clearFlashGuard() private {
        assembly { tstore(_FLASH_GUARD_SLOT, 0) }
    }

    function _requireFlashGuard() private {
        uint256 v;
        assembly { v := tload(_FLASH_GUARD_SLOT) }
        if (v != 1) revert UnsolicitedFlashCallback();
    }

    address private constant UNISWAP_V3_ROUTER = 0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45;
    address private constant UNISWAP_V2_ROUTER = 0x4752ba5DBc23f44D87826276BF6Fd6b1C372aD24;
    address private constant CAMELOT_V2_ROUTER = 0xc873fEcbd354f5A56E00E710B90EF4201db2448d;
    address private constant SUSHI_ROUTER = 0x1b02dA8Cb0d097eB8D57A175b88c7D8b47997506;
    address private constant CHRONOS_ROUTER = 0xE708aA9E887980750C040a6A2Cb901c37Aa34f3b;

    address private constant CHRONOS_FACTORY = 0xCe9240869391928253Ed9cc9Bcb8cb98CB5B0722;
    address private constant SUSHI_FACTORY = 0xc35DADB65012eC5796536bD9864eD8773aBc74C4;
    address private constant UNISWAP_V2_FACTORY = 0xf1D7CC64Fb4452F05c498126312eBE29f30Fbcf9;
    address private constant CAMELOT_V2_FACTORY = 0x6EcCab422D763aC031210895C81787E87B43A652;
    address private constant UNISWAP_V3_FACTORY = 0x1F98431c8aD98523631AE4a59f267346ea31F984;
    address private constant SUSHISWAP_V3_FACTORY = 0x1af415a1EbA07a4986a52B6f2e7dE7003D82231e;

    uint8 private constant UNISWAP_V3_ID = 0;
    uint8 private constant CAMELOT_ID = 1;
    uint8 private constant SUSHI_ID = 2;
    uint8 private constant CHRONOS_ID = 3;

    // === FIX (SEC-007): Init code hashes for callback verification ===
    // Uniswap V3 pool init code hash (from PoolAddress library)
    bytes32 private constant UNISWAP_V3_INIT_HASH = 0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54;
    // Camelot V2 pair init code hash
    bytes32 private constant CAMELOT_V2_INIT_HASH = 0xa856464ae65f7619087bc369daaf7e387dae1e5af69cfa7935850ebf754b04c1;
    // SushiSwap pair init code hash
    bytes32 private constant SUSHI_INIT_HASH = 0xe18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303;
    bytes32 private constant UNISWAP_V2_INIT_HASH = 0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f;
    bytes32 private constant SUSHISWAP_V3_INIT_HASH = 0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54;

    // === NEW DEX SUPPORT (Phase 3) ===
    // New Exchange IDs
    uint8 private constant RAMSES_ID = 5;
    uint8 private constant PANCAKE_V3_ID = 6;
    uint8 private constant KYBER_ID = 7;
    uint8 private constant ARBIDEX_ID = 8;
    uint8 private constant CAMELOT_V3_ID = 9;
    // ALG-001: Solidly stable pool exchange IDs (stable=true instead of false)
    uint8 private constant CHRONOS_STABLE_ID = 10;
    uint8 private constant RAMSES_STABLE_ID = 11;
    // M2: Ramses V2 new factory + Ramses CL (V3) exchange IDs
    uint8 private constant RAMSES_CL_ID = 12;
    uint8 private constant RAMSES_V2_NEW_ID = 13;
    uint8 private constant RAMSES_V2_NEW_STABLE_ID = 14;
    // M3: Trader Joe Liquidity Book V2.1 (bin-based AMM)
    uint8 private constant TRADER_JOE_LB_ID = 15;
    uint8 private constant CURVE_TRICRYPTO_NG_ID = 16;
    uint8 private constant CURVE_TWOCRYPTO_NG_ID = 17;
    uint8 private constant CURVE_STABLESWAP_PLAIN_NG_ID = 18;
    uint8 private constant CURVE_STABLESWAP_META_NG_ID = 19;
    uint8 private constant UNISWAP_V4_ID = 20;
    uint8 private constant FLUID_DEX_ID = 21;
    uint8 private constant MAVERICK_V2_ID = 22;
    uint8 private constant DODO_V2_ID = 23;
    uint8 private constant UNISWAP_V2_ID = 24;
    uint8 private constant SUSHISWAP_V3_ID = 25;
    // === MANTIS additions ===
    // Balancer V2 stable pool as a TRADED leg (poolId carried in TradeHopV2.poolId).
    uint8 private constant BALANCER_STABLE_ID = 26;
    // Balancer V2 weighted pool as a TRADED leg. Executes through the IDENTICAL Vault
    // single-swap path as stable (the Vault routes to the pool's own weighted math); the
    // distinct id lets the off-chain engine pick the weighted quote kernel. poolId in TradeHopV2.
    uint8 private constant BALANCER_WEIGHTED_ID = 27;
    // Aave V3 Pool (Arbitrum One). Used by flashLoanArbAaveMixed to fund routes that trade a
    // Balancer pool, where a Balancer flash loan is impossible (non-reentrant vault).
    address private constant AAVE_V3_POOL = 0x794a61358D6845594F94dc1DB02A252b5b4814aD;

    bytes4 private constant MIXED_CALLBACK_MAGIC = 0x4d495834; // "MIX4"
    // 4-hop mixed path: distinct magic so the shared flash callbacks dispatch a TradeHopV2[4]
    // route without touching the battle-tested [3] path. Funding model identical (Balancer/Aave
    // flash → sequential exact-in swaps → repay); only the hop array length differs.
    bytes4 private constant MIXED_CALLBACK_MAGIC_4 = 0x4d495835; // "MIX5"

    // New Factory Addresses
    address private constant RAMSES_V2_FACTORY = 0xAAA20D08e59F6561f242b08513D36266C5A29415;
    address private constant PANCAKE_V3_FACTORY = 0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865;
    // PancakeSwap V3 deploys pools from a SEPARATE PoolDeployer (the factory holds it as an
    // immutable and delegates CREATE2 to it), so the pool address must be derived with the
    // DEPLOYER in the CREATE2 prefix, NOT the factory. Verified on-chain (Arbitrum One):
    // factory 0x0BFbCF9f… → poolDeployer 0x41ff9AA7…071c9 (arbiscan "PancakeSwap V3: Pool Deployer").
    address private constant PANCAKE_V3_DEPLOYER = 0x41ff9AA7e16B8B1a8a8dc4f0eFacd93D02d071c9;
    address private constant CAMELOT_V3_FACTORY = 0x1a3c9B1d2F0529D97f2afC5136Cc23e58f1FD35B;
    address private constant KYBER_ELASTIC_FACTORY = 0xC7a590291e07B9fe9E64b86c58fD8fC764308C4A;
    address private constant ARBIDEX_FACTORY = 0x1C6E968f2E6c9DEC61DB874E28589fd5CE3E1f2c;
    // M2: Ramses V2 new factory + Ramses CL factory
    address private constant RAMSES_V2_NEW_FACTORY = 0xADd32480630A16dfAcEe6eeFcB3ab2181449Dc3B;
    address private constant RAMSES_CL_FACTORY = 0xd0019e86edB35E1fedaaB03aED5c3c60f115d28b;

    // New Router Addresses
    address private constant RAMSES_V2_ROUTER = 0xAAA87963EFeB6f7E0a2711F397663105Acb1805e;
    address private constant PANCAKE_V3_ROUTER = 0x32226588378236Fd0c7c4053999F88aC0e5cAc77;
    address private constant CAMELOT_V3_ROUTER = 0x1F721E2E82F6676FCE4eA07A5958cF098D339e18;
    address private constant KYBER_ROUTER = 0xC1e7dFE73E1598E3910EF4C7845B68A9Ab6F4c83;
    address private constant ARBIDEX_ROUTER = 0x3E48298A5Fe88E4d62985DFf65Dee39a25914975;
    // M2: Ramses V2 new router + Ramses CL router
    address private constant RAMSES_V2_NEW_ROUTER = 0x1614a7e1fe63960B4684867a62080acd2404757f;
    address private constant RAMSES_CL_ROUTER = 0x4730e03EB4a58A5e20244062D5f9A99bCf5770a6;
    address private constant SUSHISWAP_V3_ROUTER = 0x8A21F6768C1f8075791D08546Dadf6daA0bE820c;
    // M3: Trader Joe LB V2.1 + V2.2 routers
    address private constant TRADER_JOE_LB_ROUTER = 0xb4315e873dBcf96Ffd0acd8EA43f689D8c20fB30;
    address private constant TRADER_JOE_LB_V22_ROUTER = 0x18556DA13313f3532c54711497A8FedAC273220E;
    address private constant UNISWAP_V4_POOL_MANAGER = 0x360E68faCcca8cA495c1B759Fd9EEe466db9FB32;

    // New Init Code Hashes
    bytes32 private constant ARBIDEX_INIT_HASH = 0x724c966696ee786bca53d9ebb15f34f4961a6a2d55b17599d6dd4681a335275a;
    bytes32 private constant PANCAKE_V3_INIT_HASH = 0x6ce8eb472fa82df5469c6ab6d485f17c3ad13c8cd7af59b3d4a8026c5ce0f7e2;
    bytes32 private constant CAMELOT_V3_INIT_HASH = 0x6c1bebd370ba84753516bc1393c0d0a6c645856da55f5393ac8ab3d6dbc861d3;
    // Note: Kyber and Ramses use factory lookup for verification (no init hash needed)

    // === BALANCER V2 FLASH LOAN SUPPORT ===
    /// @notice Balancer V2 Vault address on Arbitrum (0% flash loan fee)
    address private constant BALANCER_VAULT = 0xBA12222222228d8Ba445958a75a0704d566BF2C8;
    /// @notice Fluid Liquidity Layer on Arbitrum, the spender used by direct pool swap flows.
    address private constant FLUID_LIQUIDITY_LAYER = 0x52Aa899454998Be5b000Ad077a46Bbe360F4e497;

    constructor(address _payee) {
        require(_payee != address(0), "zero payee");
        payee = _payee;
        gateway = _payee;
        // Generated from the canonical Rust token registry; indices must stay 1:1
        // with the engine Token enum and trade payload encoding.
        tokenLookup = TokenLookupData.values();

        // Approve all tokens to all routers and protocol spenders (SEC-006)
        // Using type(uint128).max as approval limit for gas efficiency
        uint256 approvalLimit = type(uint128).max;
        address[17] memory routers = [
            UNISWAP_V3_ROUTER,
            UNISWAP_V2_ROUTER,
            CAMELOT_V2_ROUTER,
            SUSHI_ROUTER,
            CHRONOS_ROUTER,
            RAMSES_V2_ROUTER,
            PANCAKE_V3_ROUTER,
            CAMELOT_V3_ROUTER,
            KYBER_ROUTER,
            ARBIDEX_ROUTER,
            RAMSES_V2_NEW_ROUTER, // M2: Ramses V2 new factory router
            RAMSES_CL_ROUTER, // M2: Ramses CL (V3) router
            SUSHISWAP_V3_ROUTER,
            TRADER_JOE_LB_ROUTER, // M3: Trader Joe LB V2.1 router
            TRADER_JOE_LB_V22_ROUTER, // M3: Trader Joe LB V2.2 router
            BALANCER_VAULT, // For flash loan repayment
            FLUID_LIQUIDITY_LAYER // Fluid direct swaps settle token pulls via the shared liquidity layer
        ];

        // Approve all tokens to all routers + protocol spenders
        for (uint256 i = 0; i < tokenLookup.length; i++) {
            address token = tokenLookup[i];
            for (uint256 j = 0; j < routers.length; j++) {
                OZIERC20(token).forceApprove(routers[j], approvalLimit);
            }
        }
    }

    receive() external payable {}

    // ============================================================
    // === FIX (SEC-007): Callback Verification Functions =========
    // ============================================================

    /// @dev Verify V3-style callback caller using CREATE2 address computation
    /// @param factory The V3 factory address
    /// @param initCodeHash The pool init code hash for this factory
    /// @param tokenA First token of the pair
    /// @param tokenB Second token of the pair
    /// @param fee The fee tier
    function _verifyV3Callback(address factory, bytes32 initCodeHash, address tokenA, address tokenB, uint24 fee)
        internal
        view
    {
        // Sort tokens
        (address token0, address token1) = tokenA < tokenB ? (tokenA, tokenB) : (tokenB, tokenA);
        // Compute expected pool address using CREATE2
        address expectedPool = address(
            uint160(
                uint256(
                    keccak256(
                        abi.encodePacked(hex"ff", factory, keccak256(abi.encode(token0, token1, fee)), initCodeHash)
                    )
                )
            )
        );
        require(msg.sender == expectedPool, "Invalid V3 callback");
    }

    /// @dev Verify V2-style callback caller using CREATE2 address computation
    /// @param factory The V2 factory address
    /// @param initCodeHash The pair init code hash for this factory
    /// @param tokenA First token of the pair
    /// @param tokenB Second token of the pair
    function _verifyV2Callback(address factory, bytes32 initCodeHash, address tokenA, address tokenB) internal view {
        // Sort tokens
        (address token0, address token1) = tokenA < tokenB ? (tokenA, tokenB) : (tokenB, tokenA);
        // Compute expected pair address using CREATE2
        address expectedPair = address(
            uint160(
                uint256(
                    keccak256(
                        abi.encodePacked(hex"ff", factory, keccak256(abi.encodePacked(token0, token1)), initCodeHash)
                    )
                )
            )
        );
        require(msg.sender == expectedPair, "Invalid V2 callback");
    }

    /// @dev Verify Solidly-style callback caller using factory lookup
    /// @notice Uses factory.getPair() because Solidly forks may have varying init code hashes
    /// @param factory The Solidly factory address
    /// @param tokenA First token of the pair
    /// @param tokenB Second token of the pair
    /// @param stable Whether the pair is a stable pool (x³y+xy³=k) or volatile (x*y=k)
    function _verifySolidlyCallback(address factory, address tokenA, address tokenB, bool stable) internal view {
        address expectedPair = IChronosFactory(factory).getPair(tokenA, tokenB, stable);
        require(msg.sender == expectedPair, "Invalid Solidly callback");
    }

    /// @dev Verify Algebra-style callback caller using factory lookup
    /// @notice Uses factory.poolByPair() because Algebra pools don't include fee in CREATE2 salt
    /// @param factory The Algebra factory address (Camelot V3, Kyber Elastic)
    /// @param tokenA First token of the pair
    /// @param tokenB Second token of the pair
    function _verifyAlgebraCallback(address factory, address tokenA, address tokenB) internal view {
        address expectedPool = IAlgebraFactory(factory).poolByPair(tokenA, tokenB);
        require(msg.sender == expectedPool, "Invalid Algebra callback");
    }

    /// @dev Verify Kyber Elastic callback caller using factory getPool with fee
    /// @notice Kyber factory requires swapFeeUnits to disambiguate pools per token pair
    /// @param factory The Kyber Elastic factory address
    /// @param tokenA First token of the pair
    /// @param tokenB Second token of the pair
    /// @param swapFeeUnits The fee tier in ppm (from encoded payload)
    function _verifyKyberCallback(address factory, address tokenA, address tokenB, uint24 swapFeeUnits) internal view {
        address expectedPool = IKyberElasticFactory(factory).getPool(tokenA, tokenB, swapFeeUnits);
        require(msg.sender == expectedPool, "Invalid Kyber callback");
    }

    /// @dev Map Ramses CL fee tier to tick spacing
    /// @param fee The fee tier in ppm (100, 250, 500, 3000, 10000, 20000)
    /// @return The corresponding tick spacing
    function _ramsesTickSpacing(uint24 fee) internal pure returns (int24) {
        if (fee == 100) return 1;
        if (fee == 250) return 5;
        if (fee == 500) return 10;
        if (fee == 3000) return 50;
        if (fee == 10000) return 100;
        if (fee == 20000) return 200;
        revert("Unknown Ramses CL fee");
    }

    /// @dev Verify Ramses CL callback caller using factory lookup
    /// @notice Uses factory.getPool() instead of CREATE2 for maintainability
    /// @param tokenA First token
    /// @param tokenB Second token
    /// @param fee The fee tier
    function _verifyRamsesCLCallback(address tokenA, address tokenB, uint24 fee) internal view {
        (address token0, address token1) = tokenA < tokenB ? (tokenA, tokenB) : (tokenB, tokenA);
        int24 tickSpacing = _ramsesTickSpacing(fee);
        address expectedPool = IRamsesV3Factory(RAMSES_CL_FACTORY).getPool(token0, token1, tickSpacing);
        require(msg.sender == expectedPool && expectedPool != address(0), "Invalid Ramses CL callback");
    }

    /// @dev Compute PancakeSwap V3 pool address using CREATE2
    /// @param tokenA First token
    /// @param tokenB Second token
    /// @param fee Fee tier
    /// @return pool The computed pool address
    function _computePancakeV3Pool(address tokenA, address tokenB, uint24 fee) internal pure returns (address pool) {
        (address token0, address token1) = tokenA < tokenB ? (tokenA, tokenB) : (tokenB, tokenA);
        pool = address(
            uint160(
                uint256(
                    keccak256(
                        abi.encodePacked(
                            hex"ff",
                            PANCAKE_V3_DEPLOYER,
                            keccak256(abi.encode(token0, token1, fee)),
                            PANCAKE_V3_INIT_HASH
                        )
                    )
                )
            )
        );
    }

    // ============================================================

    /// decodes tigger trade data
    function decode(uint128 payload)
        public
        pure
        returns (uint8[3] memory exchanges, uint8[3] memory tokens, uint16[3] memory fees)
    {
        uint8 exchange0Id = uint8(payload);
        uint8 exchange1Id = uint8(payload >> 8);
        uint8 exchange2Id = uint8(payload >> 16);
        uint8 token0Id = uint8(payload >> 24);
        uint8 token1Id = uint8(payload >> 32);
        uint8 token2Id = uint8(payload >> 40);
        uint16 fee0 = uint16(payload >> 48);
        uint16 fee1 = uint16(payload >> 64);
        uint16 fee2 = uint16(payload >> 80);

        return ([exchange0Id, exchange1Id, exchange2Id], [token0Id, token1Id, token2Id], [fee0, fee1, fee2]);
    }

    function _isActiveHop(TradeHop memory hop) internal pure returns (bool) {
        return hop.tokenIn != address(0) && hop.tokenOut != address(0);
    }

    function _isActiveHopV2(TradeHopV2 memory hop) internal pure returns (bool) {
        return hop.tokenIn != address(0) && hop.tokenOut != address(0);
    }

    function _isUniswapV4Hop(TradeHopV2 memory hop) internal pure returns (bool) {
        return hop.exchangeId == UNISWAP_V4_ID;
    }

    function _hasMixedCallbackMagic(bytes calldata data) internal pure returns (bool) {
        if (data.length < 4) return false;
        bytes4 magic;
        assembly {
            magic := calldataload(data.offset)
        }
        return magic == MIXED_CALLBACK_MAGIC;
    }

    function _hasMixedCallbackMagicBytes(bytes memory data) internal pure returns (bool) {
        if (data.length < 4) return false;
        bytes4 magic;
        assembly {
            magic := mload(add(data, 32))
        }
        return magic == MIXED_CALLBACK_MAGIC;
    }

    function _stripMixedCallbackMagic(bytes calldata data) internal pure returns (bytes memory payload) {
        require(data.length >= 4, "Bad mixed callback");
        payload = new bytes(data.length - 4);
        for (uint256 i = 4; i < data.length; i++) {
            payload[i - 4] = data[i];
        }
    }

    function _stripMixedCallbackMagicBytes(bytes memory data) internal pure returns (bytes memory payload) {
        require(data.length >= 4, "Bad mixed callback");
        payload = new bytes(data.length - 4);
        for (uint256 i = 4; i < data.length; i++) {
            payload[i - 4] = data[i];
        }
    }

    function _decodeMixedCallback(bytes calldata data)
        internal
        pure
        returns (TradeHopV2[3] memory hops, uint128 amountIn)
    {
        return abi.decode(_stripMixedCallbackMagic(data), (TradeHopV2[3], uint128));
    }

    function _decodeMixedCallbackBytes(bytes memory data)
        internal
        pure
        returns (TradeHopV2[3] memory hops, uint128 amountIn)
    {
        return abi.decode(_stripMixedCallbackMagicBytes(data), (TradeHopV2[3], uint128));
    }

    function _encodeMixedCallback(TradeHopV2[3] memory hops, uint128 amountIn) internal pure returns (bytes memory) {
        return abi.encodePacked(MIXED_CALLBACK_MAGIC, abi.encode(hops, amountIn));
    }

    // ============================ 4-HOP MIXED PATH (additive) ============================
    // Mirrors the [3] mixed path for TradeHopV2[4] cycles. The [3] path above is untouched.

    function _hasMixedCallbackMagic4Bytes(bytes memory data) internal pure returns (bool) {
        if (data.length < 4) return false;
        bytes4 magic;
        assembly {
            magic := mload(add(data, 32))
        }
        return magic == MIXED_CALLBACK_MAGIC_4;
    }

    function _encodeMixedCallback4(TradeHopV2[4] memory hops, uint128 amountIn) internal pure returns (bytes memory) {
        return abi.encodePacked(MIXED_CALLBACK_MAGIC_4, abi.encode(hops, amountIn));
    }

    function _decodeMixedCallback4Bytes(bytes memory data)
        internal
        pure
        returns (TradeHopV2[4] memory hops, uint128 amountIn)
    {
        return abi.decode(_stripMixedCallbackMagicBytes(data), (TradeHopV2[4], uint128));
    }

    /// Validate a 2..4-hop closed cycle. Inactive trailing hops are allowed (so a 3-hop route can
    /// ride the [4] path with hops[3] empty), but the active prefix must chain and roundtrip.
    function _validateTradeHopsV2_4(TradeHopV2[4] memory hops) internal pure {
        require(_isActiveHopV2(hops[0]) && _isActiveHopV2(hops[1]), "Need 2 hops");
        require(hops[0].tokenIn != hops[0].tokenOut, "Bad hop 0");
        // Walk active hops: each must chain to the previous; once an inactive hop is seen, all
        // following must be inactive (no holes). The last active hop must roundtrip to hops[0].tokenIn.
        address prevOut = hops[0].tokenOut;
        uint256 lastActive = 0;
        bool ended = false;
        for (uint256 i = 1; i < 4; i++) {
            if (_isActiveHopV2(hops[i])) {
                require(!ended, "Hole in path");
                require(hops[i].tokenIn != hops[i].tokenOut, "Bad hop");
                require(hops[i].tokenIn == prevOut, "Broken path");
                prevOut = hops[i].tokenOut;
                lastActive = i;
            } else {
                ended = true;
            }
        }
        require(lastActive >= 1, "Need >=2 hops");
        require(prevOut == hops[0].tokenIn, "Must roundtrip");
        // Per-hop venue sanity (same checks as the [3] path).
        for (uint256 i = 0; i < 4; i++) {
            if (!_isActiveHopV2(hops[i])) continue;
            if (_isCurveExchange(hops[i].exchangeId)) {
                require(hops[i].pool != address(0), "Curve pool required");
                require(hops[i].coinInIdx != hops[i].coinOutIdx, "Bad Curve coin idx");
            }
            if (hops[i].exchangeId == FLUID_DEX_ID) require(hops[i].pool != address(0), "Fluid pool required");
            if (hops[i].exchangeId == MAVERICK_V2_ID) require(hops[i].pool != address(0), "Maverick pool required");
            if (hops[i].exchangeId == DODO_V2_ID) require(hops[i].pool != address(0), "DODO pool required");
            if (_isUniswapV4Hop(hops[i])) {
                require(hops[i].hooks == address(0), "Hooks unsupported");
                require(hops[i].poolManager == UNISWAP_V4_POOL_MANAGER, "Bad pool manager");
                require(hops[i].poolId != bytes32(0), "PoolId required");
                require(hops[i].tickSpacing != 0, "Tick spacing required");
            }
        }
    }

    function _executeTradeHopsV2_4(TradeHopV2[4] memory hops, uint256 amountIn) internal returns (uint256 amountOut) {
        amountOut = _swapExactInHopV2(hops[0], amountIn);
        for (uint256 i = 1; i < 4; i++) {
            if (!_isActiveHopV2(hops[i])) break; // contiguous active prefix (validated)
            amountOut = _swapExactInHopV2(hops[i], amountOut);
        }
    }

    /// True if `id` trades through the Balancer V2 Vault (stable OR weighted) — both share the
    /// non-reentrant Vault, so both are subject to the "can't be flash source AND swap venue"
    /// constraint.
    function _isBalancerVaultId(uint8 id) private pure returns (bool) {
        return id == BALANCER_STABLE_ID || id == BALANCER_WEIGHTED_ID;
    }

    function _routeTradesBalancer4(TradeHopV2[4] memory hops) internal pure returns (bool) {
        for (uint256 i = 0; i < 4; i++) {
            if (_isBalancerVaultId(hops[i].exchangeId)) return true;
        }
        return false;
    }

    function _settleFlashLoanMixed4(
        TradeHopV2[4] memory hops,
        IERC20[] memory loanTokens,
        uint256[] memory amounts,
        uint256[] memory feeAmounts
    ) internal {
        uint256 netOut = _executeTradeHopsV2_4(hops, amounts[0]);
        uint256 repay = amounts[0] + feeAmounts[0];
        if (netOut <= repay) revert Loss(repay - netOut);
        uint256 profit = netOut - repay;
        uint256 minP = _minNetProfit;
        _minNetProfit = 0;
        if (profit < minP) revert InsufficientProfit(profit, minP);
        OZIERC20(address(loanTokens[0])).safeTransfer(BALANCER_VAULT, repay);
        OZIERC20(address(loanTokens[0])).safeTransfer(payee, profit);
    }
    // ========================== END 4-HOP MIXED PATH ==========================

    function _isCurveExchange(uint8 exchangeId) internal pure returns (bool) {
        return exchangeId == CURVE_TRICRYPTO_NG_ID || exchangeId == CURVE_TWOCRYPTO_NG_ID
            || exchangeId == CURVE_STABLESWAP_PLAIN_NG_ID || exchangeId == CURVE_STABLESWAP_META_NG_ID;
    }

    function _decodePayloadToHops(uint128 payload) internal view returns (TradeHop[3] memory hops) {
        (uint8[3] memory exchanges, uint8[3] memory tokens, uint16[3] memory fees) = decode(payload);

        address token0 = tokenLookup[tokens[0]];
        address token1 = tokenLookup[tokens[1]];
        hops[0] = TradeHop({
            exchangeId: exchanges[0],
            tokenIn: token0,
            tokenOut: token1,
            fee: fees[0],
            pool: address(0),
            coinInIdx: 0,
            coinOutIdx: 1
        });

        if (tokens[2] == 255) {
            hops[1] = TradeHop({
                exchangeId: exchanges[1],
                tokenIn: token1,
                tokenOut: token0,
                fee: fees[1],
                pool: address(0),
                coinInIdx: 0,
                coinOutIdx: 0
            });
            return hops;
        }

        address token2 = tokenLookup[tokens[2]];
        hops[1] = TradeHop({
            exchangeId: exchanges[1],
            tokenIn: token1,
            tokenOut: token2,
            fee: fees[1],
            pool: address(0),
            coinInIdx: 0,
            coinOutIdx: 0
        });
        hops[2] = TradeHop({
            exchangeId: exchanges[2],
            tokenIn: token2,
            tokenOut: token0,
            fee: fees[2],
            pool: address(0),
            coinInIdx: 0,
            coinOutIdx: 0
        });
    }

    function _validateTradeHops(TradeHop[3] memory hops) internal pure {
        require(_isActiveHop(hops[0]) && _isActiveHop(hops[1]), "Need 2 hops");
        require(hops[0].tokenIn != hops[0].tokenOut, "Bad hop 0");
        require(hops[1].tokenIn != hops[1].tokenOut, "Bad hop 1");
        require(hops[1].tokenIn == hops[0].tokenOut, "Broken path");

        if (_isActiveHop(hops[2])) {
            require(hops[2].tokenIn != hops[2].tokenOut, "Bad hop 2");
            require(hops[2].tokenIn == hops[1].tokenOut, "Broken path");
            require(hops[2].tokenOut == hops[0].tokenIn, "Must roundtrip");
        } else {
            require(hops[1].tokenOut == hops[0].tokenIn, "Must roundtrip");
        }

        for (uint256 i = 0; i < 3; i++) {
            if (!_isActiveHop(hops[i])) continue;
            if (_isCurveExchange(hops[i].exchangeId)) {
                require(hops[i].pool != address(0), "Curve pool required");
                require(hops[i].coinInIdx != hops[i].coinOutIdx, "Bad Curve coin idx");
            }
            if (hops[i].exchangeId == FLUID_DEX_ID) {
                require(hops[i].pool != address(0), "Fluid pool required");
            }
            if (hops[i].exchangeId == MAVERICK_V2_ID) {
                revert("Maverick: use mixed route");
            }
            if (hops[i].exchangeId == DODO_V2_ID) {
                revert("DODO: use mixed route");
            }
        }
    }

    function _validateTradeHopsV2(TradeHopV2[3] memory hops) internal pure {
        require(_isActiveHopV2(hops[0]) && _isActiveHopV2(hops[1]), "Need 2 hops");
        require(hops[0].tokenIn != hops[0].tokenOut, "Bad hop 0");
        require(hops[1].tokenIn != hops[1].tokenOut, "Bad hop 1");
        require(hops[1].tokenIn == hops[0].tokenOut, "Broken path");

        if (_isActiveHopV2(hops[2])) {
            require(hops[2].tokenIn != hops[2].tokenOut, "Bad hop 2");
            require(hops[2].tokenIn == hops[1].tokenOut, "Broken path");
            require(hops[2].tokenOut == hops[0].tokenIn, "Must roundtrip");
        } else {
            require(hops[1].tokenOut == hops[0].tokenIn, "Must roundtrip");
        }

        for (uint256 i = 0; i < 3; i++) {
            if (!_isActiveHopV2(hops[i])) continue;
            if (_isCurveExchange(hops[i].exchangeId)) {
                require(hops[i].pool != address(0), "Curve pool required");
                require(hops[i].coinInIdx != hops[i].coinOutIdx, "Bad Curve coin idx");
            }
            if (hops[i].exchangeId == FLUID_DEX_ID) {
                require(hops[i].pool != address(0), "Fluid pool required");
            }
            if (hops[i].exchangeId == MAVERICK_V2_ID) {
                require(hops[i].pool != address(0), "Maverick pool required");
            }
            if (hops[i].exchangeId == DODO_V2_ID) {
                require(hops[i].pool != address(0), "DODO pool required");
            }
            if (_isUniswapV4Hop(hops[i])) {
                require(hops[i].hooks == address(0), "Hooks unsupported");
                require(hops[i].poolManager == UNISWAP_V4_POOL_MANAGER, "Bad pool manager");
                require(hops[i].poolId != bytes32(0), "PoolId required");
                require(hops[i].tickSpacing != 0, "Tick spacing required");
            }
        }
    }

    function _toLegacyTradeHop(TradeHopV2 memory hop) internal pure returns (TradeHop memory) {
        require(hop.exchangeId != UNISWAP_V4_ID, "V4 not legacy");
        require(hop.fee <= type(uint16).max, "Fee overflow");
        return TradeHop({
            exchangeId: hop.exchangeId,
            tokenIn: hop.tokenIn,
            tokenOut: hop.tokenOut,
            fee: uint16(hop.fee),
            pool: hop.pool,
            coinInIdx: hop.coinInIdx,
            coinOutIdx: hop.coinOutIdx
        });
    }

    function _resolvedPool(TradeHop memory hop) internal view returns (address pool) {
        if (hop.pool != address(0)) {
            return hop.pool;
        }
        if (hop.exchangeId == FLUID_DEX_ID) {
            revert("Fluid pool required");
        }
        if (hop.exchangeId == MAVERICK_V2_ID) {
            revert("Maverick: use mixed route");
        }
        if (hop.exchangeId == DODO_V2_ID) {
            revert("DODO: use mixed route");
        }

        if (hop.exchangeId == UNISWAP_V3_ID) {
            return
                PoolAddress.computeAddress(
                    UNISWAP_V3_FACTORY, PoolAddress.getPoolKey(hop.tokenIn, hop.tokenOut, hop.fee)
                );
        }
        if (hop.exchangeId == SUSHISWAP_V3_ID) {
            return PoolAddress.computeAddress(
                SUSHISWAP_V3_FACTORY, PoolAddress.getPoolKey(hop.tokenIn, hop.tokenOut, hop.fee)
            );
        }
        if (hop.exchangeId == RAMSES_CL_ID) {
            return IRamsesV3Factory(RAMSES_CL_FACTORY)
                .getPool(
                    hop.tokenIn < hop.tokenOut ? hop.tokenIn : hop.tokenOut,
                    hop.tokenIn < hop.tokenOut ? hop.tokenOut : hop.tokenIn,
                    _ramsesTickSpacing(hop.fee)
                );
        }
        if (hop.exchangeId == PANCAKE_V3_ID) {
            return _computePancakeV3Pool(hop.tokenIn, hop.tokenOut, hop.fee);
        }
        if (hop.exchangeId == CAMELOT_V3_ID) {
            return IAlgebraFactory(CAMELOT_V3_FACTORY).poolByPair(hop.tokenIn, hop.tokenOut);
        }
        if (hop.exchangeId == KYBER_ID) {
            return IKyberElasticFactory(KYBER_ELASTIC_FACTORY).getPool(hop.tokenIn, hop.tokenOut, uint24(hop.fee));
        }
        if (hop.exchangeId == CHRONOS_ID || hop.exchangeId == CHRONOS_STABLE_ID) {
            return
                IChronosFactory(CHRONOS_FACTORY).getPair(hop.tokenIn, hop.tokenOut, hop.exchangeId == CHRONOS_STABLE_ID);
        }
        if (hop.exchangeId == RAMSES_ID || hop.exchangeId == RAMSES_STABLE_ID) {
            return
                IRamsesFactory(RAMSES_V2_FACTORY).getPair(hop.tokenIn, hop.tokenOut, hop.exchangeId == RAMSES_STABLE_ID);
        }
        if (hop.exchangeId == RAMSES_V2_NEW_ID || hop.exchangeId == RAMSES_V2_NEW_STABLE_ID) {
            return IRamsesFactory(RAMSES_V2_NEW_FACTORY)
                .getPair(hop.tokenIn, hop.tokenOut, hop.exchangeId == RAMSES_V2_NEW_STABLE_ID);
        }
        if (hop.exchangeId == CAMELOT_ID) {
            return IUniswapV2Factory(CAMELOT_V2_FACTORY).getPair(hop.tokenIn, hop.tokenOut);
        }
        if (hop.exchangeId == UNISWAP_V2_ID) {
            return IUniswapV2Factory(UNISWAP_V2_FACTORY).getPair(hop.tokenIn, hop.tokenOut);
        }
        if (hop.exchangeId == SUSHI_ID) {
            return IUniswapV2Factory(SUSHI_FACTORY).getPair(hop.tokenIn, hop.tokenOut);
        }
        if (hop.exchangeId == ARBIDEX_ID) {
            return IUniswapV2Factory(ARBIDEX_FACTORY).getPair(hop.tokenIn, hop.tokenOut);
        }
        if (_isCurveExchange(hop.exchangeId)) {
            revert("Curve pool required");
        }

        revert("Unknown exchange ID");
    }

    function _requirePoolMatch(address expectedPool, address encodedPool) internal pure {
        if (encodedPool != address(0)) {
            require(encodedPool == expectedPool, "Pool mismatch");
        }
    }

    function _verifyTradeHopCallback(TradeHop memory hop) internal view {
        if (hop.exchangeId == CAMELOT_ID) {
            address expectedPair = IUniswapV2Factory(CAMELOT_V2_FACTORY).getPair(hop.tokenIn, hop.tokenOut);
            _requirePoolMatch(expectedPair, hop.pool);
            require(msg.sender == expectedPair, "Invalid V2 callback");
        } else if (hop.exchangeId == UNISWAP_V2_ID) {
            address expectedPair = IUniswapV2Factory(UNISWAP_V2_FACTORY).getPair(hop.tokenIn, hop.tokenOut);
            _requirePoolMatch(expectedPair, hop.pool);
            require(msg.sender == expectedPair, "Invalid V2 callback");
        } else if (hop.exchangeId == SUSHI_ID) {
            address expectedPair = IUniswapV2Factory(SUSHI_FACTORY).getPair(hop.tokenIn, hop.tokenOut);
            _requirePoolMatch(expectedPair, hop.pool);
            require(msg.sender == expectedPair, "Invalid V2 callback");
        } else if (hop.exchangeId == CHRONOS_ID || hop.exchangeId == CHRONOS_STABLE_ID) {
            address expectedPair = IChronosFactory(CHRONOS_FACTORY)
                .getPair(hop.tokenIn, hop.tokenOut, hop.exchangeId == CHRONOS_STABLE_ID);
            _requirePoolMatch(expectedPair, hop.pool);
            require(msg.sender == expectedPair, "Invalid Solidly callback");
        } else if (hop.exchangeId == RAMSES_ID || hop.exchangeId == RAMSES_STABLE_ID) {
            address expectedPair = IRamsesFactory(RAMSES_V2_FACTORY)
                .getPair(hop.tokenIn, hop.tokenOut, hop.exchangeId == RAMSES_STABLE_ID);
            _requirePoolMatch(expectedPair, hop.pool);
            require(msg.sender == expectedPair, "Invalid Solidly callback");
        } else if (hop.exchangeId == RAMSES_V2_NEW_ID || hop.exchangeId == RAMSES_V2_NEW_STABLE_ID) {
            address expectedPair = IRamsesFactory(RAMSES_V2_NEW_FACTORY)
                .getPair(hop.tokenIn, hop.tokenOut, hop.exchangeId == RAMSES_V2_NEW_STABLE_ID);
            _requirePoolMatch(expectedPair, hop.pool);
            require(msg.sender == expectedPair, "Invalid Solidly callback");
        } else if (hop.exchangeId == ARBIDEX_ID) {
            address expectedPair = IUniswapV2Factory(ARBIDEX_FACTORY).getPair(hop.tokenIn, hop.tokenOut);
            _requirePoolMatch(expectedPair, hop.pool);
            require(msg.sender == expectedPair, "Invalid V2 callback");
        } else if (hop.exchangeId == UNISWAP_V3_ID) {
            address expectedPool = PoolAddress.computeAddress(
                UNISWAP_V3_FACTORY, PoolAddress.getPoolKey(hop.tokenIn, hop.tokenOut, hop.fee)
            );
            _requirePoolMatch(expectedPool, hop.pool);
            require(msg.sender == expectedPool, "Invalid V3 callback");
        } else if (hop.exchangeId == SUSHISWAP_V3_ID) {
            address expectedPool = PoolAddress.computeAddress(
                SUSHISWAP_V3_FACTORY, PoolAddress.getPoolKey(hop.tokenIn, hop.tokenOut, hop.fee)
            );
            _requirePoolMatch(expectedPool, hop.pool);
            require(msg.sender == expectedPool, "Invalid V3 callback");
        } else if (hop.exchangeId == RAMSES_CL_ID) {
            address expectedPool = IRamsesV3Factory(RAMSES_CL_FACTORY)
                .getPool(
                    hop.tokenIn < hop.tokenOut ? hop.tokenIn : hop.tokenOut,
                    hop.tokenIn < hop.tokenOut ? hop.tokenOut : hop.tokenIn,
                    _ramsesTickSpacing(hop.fee)
                );
            _requirePoolMatch(expectedPool, hop.pool);
            require(msg.sender == expectedPool && expectedPool != address(0), "Invalid Ramses CL callback");
        } else if (hop.exchangeId == PANCAKE_V3_ID) {
            address expectedPool = _computePancakeV3Pool(hop.tokenIn, hop.tokenOut, hop.fee);
            _requirePoolMatch(expectedPool, hop.pool);
            require(msg.sender == expectedPool, "Invalid V3 callback");
        } else if (hop.exchangeId == CAMELOT_V3_ID) {
            address expectedPool = IAlgebraFactory(CAMELOT_V3_FACTORY).poolByPair(hop.tokenIn, hop.tokenOut);
            _requirePoolMatch(expectedPool, hop.pool);
            require(msg.sender == expectedPool, "Invalid Algebra callback");
        } else if (hop.exchangeId == KYBER_ID) {
            address expectedPool =
                IKyberElasticFactory(KYBER_ELASTIC_FACTORY).getPool(hop.tokenIn, hop.tokenOut, uint24(hop.fee));
            _requirePoolMatch(expectedPool, hop.pool);
            require(msg.sender == expectedPool, "Invalid Kyber callback");
        } else {
            revert("Unknown flash swap exchange");
        }
    }

    function _verifyTradeHopCallbackV2(TradeHopV2 memory hop) internal view {
        require(hop.exchangeId != UNISWAP_V4_ID, "V4 cannot flash swap");
        _verifyTradeHopCallback(_toLegacyTradeHop(hop));
    }

    function _executeRemainingHops(TradeHop[3] memory hops, uint256 amountIn) internal returns (uint256 amountOut) {
        amountOut = amountIn;
        amountOut = _swapExactInHop(hops[1], amountOut);
        if (_isActiveHop(hops[2])) {
            amountOut = _swapExactInHop(hops[2], amountOut);
        }
    }

    function _executeRemainingHopsV2(TradeHopV2[3] memory hops, uint256 amountIn) internal returns (uint256 amountOut) {
        amountOut = amountIn;
        amountOut = _swapExactInHopV2(hops[1], amountOut);
        if (_isActiveHopV2(hops[2])) {
            amountOut = _swapExactInHopV2(hops[2], amountOut);
        }
    }

    function _executeTradeHops(TradeHop[3] memory hops, uint256 amountIn) internal returns (uint256 amountOut) {
        amountOut = _swapExactInHop(hops[0], amountIn);
        amountOut = _executeRemainingHops(hops, amountOut);
    }

    function _executeTradeHopsV2(TradeHopV2[3] memory hops, uint256 amountIn) internal returns (uint256 amountOut) {
        amountOut = _swapExactInHopV2(hops[0], amountIn);
        amountOut = _executeRemainingHopsV2(hops, amountOut);
    }

    function _settleFlashSwapV2WithDeltas(TradeHop[3] memory hops, int256 amount0Delta, int256 amount1Delta) internal {
        uint256 callbackOwed;
        uint256 callbackEarned;
        if (hops[0].tokenIn < hops[0].tokenOut) {
            callbackOwed = uint256(amount0Delta);
            callbackEarned = uint256(-amount1Delta);
        } else {
            callbackOwed = uint256(amount1Delta);
            callbackEarned = uint256(-amount0Delta);
        }

        callbackEarned = _executeRemainingHops(hops, callbackEarned);
        payback(callbackOwed, callbackEarned, hops[0].tokenIn);
    }

    function _settleFlashSwapMixedWithDeltas(TradeHopV2[3] memory hops, int256 amount0Delta, int256 amount1Delta)
        internal
    {
        uint256 callbackOwed;
        uint256 callbackEarned;
        if (hops[0].tokenIn < hops[0].tokenOut) {
            callbackOwed = uint256(amount0Delta);
            callbackEarned = uint256(-amount1Delta);
        } else {
            callbackOwed = uint256(amount1Delta);
            callbackEarned = uint256(-amount0Delta);
        }

        callbackEarned = _executeRemainingHopsV2(hops, callbackEarned);
        payback(callbackOwed, callbackEarned, hops[0].tokenIn);
    }

    function _settleFlashSwapMixed(
        TradeHopV2[3] memory hops,
        uint128 callbackAmountInOwed,
        uint256 amount0,
        uint256 amount1
    ) internal {
        uint256 callbackNetAmountOut = amount0 > 0 ? amount0 : amount1;
        callbackNetAmountOut = _executeRemainingHopsV2(hops, callbackNetAmountOut);
        payback(callbackAmountInOwed, callbackNetAmountOut, hops[0].tokenIn);
    }

    function _settleFlashLoanV2(
        TradeHop[3] memory hops,
        IERC20[] memory loanTokens,
        uint256[] memory amounts,
        uint256[] memory feeAmounts
    ) internal {
        uint256 callbackNetAmountOut = _executeTradeHops(hops, amounts[0]);
        uint256 callbackRepayAmount = amounts[0] + feeAmounts[0];

        if (callbackNetAmountOut <= callbackRepayAmount) {
            revert Loss(callbackRepayAmount - callbackNetAmountOut);
        }

        uint256 callbackProfit = callbackNetAmountOut - callbackRepayAmount;
        uint256 callbackMinProfit = _minNetProfit;
        _minNetProfit = 0;
        if (callbackProfit < callbackMinProfit) revert InsufficientProfit(callbackProfit, callbackMinProfit);

        OZIERC20(address(loanTokens[0])).safeTransfer(BALANCER_VAULT, callbackRepayAmount);
        OZIERC20(address(loanTokens[0])).safeTransfer(payee, callbackProfit);
    }

    function _settleFlashLoanMixed(
        TradeHopV2[3] memory hops,
        IERC20[] memory loanTokens,
        uint256[] memory amounts,
        uint256[] memory feeAmounts
    ) internal {
        uint256 callbackNetAmountOut = _executeTradeHopsV2(hops, amounts[0]);
        uint256 callbackRepayAmount = amounts[0] + feeAmounts[0];

        if (callbackNetAmountOut <= callbackRepayAmount) {
            revert Loss(callbackRepayAmount - callbackNetAmountOut);
        }

        uint256 callbackProfit = callbackNetAmountOut - callbackRepayAmount;
        uint256 callbackMinProfit = _minNetProfit;
        _minNetProfit = 0;
        if (callbackProfit < callbackMinProfit) revert InsufficientProfit(callbackProfit, callbackMinProfit);

        OZIERC20(address(loanTokens[0])).safeTransfer(BALANCER_VAULT, callbackRepayAmount);
        OZIERC20(address(loanTokens[0])).safeTransfer(payee, callbackProfit);
    }

    function _initiateFlashSwapV2(TradeHop memory hop, uint128 amountIn, bytes memory callbackData) internal {
        if (hop.exchangeId == FLUID_DEX_ID) {
            revert("Fluid: use flashLoanArbMixed");
        }
        if (hop.exchangeId == MAVERICK_V2_ID) {
            revert("Maverick: use flashLoanArbMixed");
        }
        if (hop.exchangeId == DODO_V2_ID) {
            revert("DODO: use flashLoanArbMixed");
        }
        address pool0 = _resolvedPool(hop);

        if (hop.exchangeId == UNISWAP_V3_ID || hop.exchangeId == SUSHISWAP_V3_ID) {
            IUniswapV3Pool(pool0)
                .swap(
                    address(this),
                    hop.tokenIn < hop.tokenOut,
                    int256(uint256(amountIn)),
                    hop.tokenIn < hop.tokenOut ? MIN_SQRT_RATIO + 1 : MAX_SQRT_RATIO - 1,
                    callbackData
                );
            return;
        }
        if (hop.exchangeId == RAMSES_CL_ID) {
            IUniswapV3Pool(pool0)
                .swap(
                    address(this),
                    hop.tokenIn < hop.tokenOut,
                    int256(uint256(amountIn)),
                    hop.tokenIn < hop.tokenOut ? MIN_SQRT_RATIO + 1 : MAX_SQRT_RATIO - 1,
                    callbackData
                );
            return;
        }
        if (hop.exchangeId == PANCAKE_V3_ID) {
            IPancakeV3Pool(pool0)
                .swap(
                    address(this),
                    hop.tokenIn < hop.tokenOut,
                    int256(uint256(amountIn)),
                    hop.tokenIn < hop.tokenOut ? MIN_SQRT_RATIO + 1 : MAX_SQRT_RATIO - 1,
                    callbackData
                );
            return;
        }
        if (hop.exchangeId == CAMELOT_V3_ID) {
            IAlgebraPool(pool0)
                .swap(
                    address(this),
                    hop.tokenIn < hop.tokenOut,
                    int256(uint256(amountIn)),
                    hop.tokenIn < hop.tokenOut ? MIN_SQRT_RATIO + 1 : MAX_SQRT_RATIO - 1,
                    callbackData
                );
            return;
        }
        if (hop.exchangeId == KYBER_ID) {
            revert("Kyber: use flashLoanArbV2");
        }
        if (hop.exchangeId == TRADER_JOE_LB_ID) {
            revert("LB: use flashLoanArbV2");
        }
        if (_isCurveExchange(hop.exchangeId)) {
            revert("Curve: use flashLoanArbV2");
        }

        (uint256 reserve0, uint256 reserve1,) = IUniswapV2Pair(pool0).getReserves();
        if (hop.tokenIn < hop.tokenOut) {
            uint256 amountOut = UniswapV2Math.getAmountOut(uint256(amountIn), reserve0, reserve1);
            IUniswapV2Pair(pool0).swap(0, amountOut, address(this), callbackData);
        } else {
            uint256 amountOut = UniswapV2Math.getAmountOut(uint256(amountIn), reserve1, reserve0);
            IUniswapV2Pair(pool0).swap(amountOut, 0, address(this), callbackData);
        }
    }

    // delegate approval for 'who' to spend tokens from this contract e.g. to a router contract
    function setPayee(address who) external {
        require(msg.sender == payee);
        require(who != address(0), "zero payee");
        payee = who;
    }

    function setGateway(address who) external {
        require(msg.sender == payee);
        require(who != address(0), "zero gateway");
        gateway = who;
    }

    // delegate approval for 'who' to spend tokens from this contract e.g. to a router contract
    function setApproval(address who, address token, uint256 amount) external {
        require(msg.sender == payee);
        OZIERC20(token).forceApprove(who, amount);
    }

    // Withdraw erc20 token from the contract
    function withdrawToken(address token) external {
        require(msg.sender == payee);
        OZIERC20(token).safeTransfer(payee, IERC20(token).balanceOf(address(this)));
    }

    // Withdraw native token from the contract
    function withdrawNative() external nonReentrant {
        require(msg.sender == payee);
        (bool ok,) = payable(msg.sender).call{value: address(this).balance}("");
        require(ok, "native withdraw failed");
    }

    /// @dev Execute a flash swap across up to 3 exchanges and 3 pools
    /// @param amountIn Input amount in wei (borrowed from first pool)
    /// @param payload Encoded trade path (exchanges, tokens, fees)
    /// @param minNetProfit Minimum acceptable profit (SEC-003 slippage protection)
    /// @notice Reverts if profit < minNetProfit after modeled costs and slippage
    function flashSwap(uint128 amountIn, uint128 payload, uint128 minNetProfit) external {
        require(msg.sender == gateway);
        // SEC-FLASH-ORIGIN: mark this tx as one WE initiated so the V2/V3 swap callbacks invoked
        // synchronously by the pool.swap calls below pass _requireFlashGuard. Transient; auto-clears
        // at tx end.
        _setFlashGuard();
        // Store minNetProfit for callback to check (SEC-003)
        // NOTE: `minNetProfit == 0` is a legal contract input — it means "execute and let the
        // payback Loss/InsufficientProfit gate be the only floor" (the convention used by fork tests
        // and REVM dry-runs). The POSITIVE economic floor that must cover L2+L1 gas is enforced
        // UPSTREAM in the engine submission path (engine `mantis_submit::build_mantis_calldata`,
        // which refuses min_net_profit == 0), so live submissions always carry a gas-covering floor.
        _minNetProfit = minNetProfit;

        (uint8[3] memory exchanges, uint8[3] memory tokens, uint16[3] memory fees) = decode(payload);
        address token0 = tokenLookup[tokens[0]];
        address token1 = tokenLookup[tokens[1]];
        uint8 exchangeId0 = exchanges[0];

        // === V3-style DEXs (callbacks: uniswapV3SwapCallback, pancakeV3SwapCallback) ===
        if (exchangeId0 == UNISWAP_V3_ID) {
            IUniswapV3Pool pool0 = IUniswapV3Pool(
                PoolAddress.computeAddress(UNISWAP_V3_FACTORY, PoolAddress.getPoolKey(token0, token1, fees[0]))
            );
            // convert payload to bytes for callback abi
            bytes memory callbackData = new bytes(32);
            assembly {
                mstore(add(callbackData, 32), payload)
            }
            if (token0 < token1) {
                pool0.swap(address(this), true, int128(amountIn), MIN_SQRT_RATIO + 1, callbackData);
            } else {
                pool0.swap(address(this), false, int128(amountIn), MAX_SQRT_RATIO - 1, callbackData);
            }
        } else if (exchangeId0 == SUSHISWAP_V3_ID) {
            IUniswapV3Pool pool0 = IUniswapV3Pool(
                PoolAddress.computeAddress(SUSHISWAP_V3_FACTORY, PoolAddress.getPoolKey(token0, token1, fees[0]))
            );
            bytes memory callbackData = new bytes(32);
            assembly {
                mstore(add(callbackData, 32), payload)
            }
            if (token0 < token1) {
                pool0.swap(address(this), true, int128(amountIn), MIN_SQRT_RATIO + 1, callbackData);
            } else {
                pool0.swap(address(this), false, int128(amountIn), MAX_SQRT_RATIO - 1, callbackData);
            }
        } else if (exchangeId0 == RAMSES_CL_ID) {
            // M2: Ramses CL (V3 fork) — uses uniswapV3SwapCallback, pool from factory lookup
            int24 tickSpacing = _ramsesTickSpacing(fees[0]);
            IUniswapV3Pool pool0 =
                IUniswapV3Pool(IRamsesV3Factory(RAMSES_CL_FACTORY).getPool(token0, token1, tickSpacing));
            bytes memory callbackData = new bytes(32);
            assembly {
                mstore(add(callbackData, 32), payload)
            }
            if (token0 < token1) {
                pool0.swap(address(this), true, int128(amountIn), MIN_SQRT_RATIO + 1, callbackData);
            } else {
                pool0.swap(address(this), false, int128(amountIn), MAX_SQRT_RATIO - 1, callbackData);
            }
        } else if (exchangeId0 == PANCAKE_V3_ID) {
            // PancakeSwap V3 - same interface as Uniswap V3 but different callback name
            IPancakeV3Pool pool0 = IPancakeV3Pool(_computePancakeV3Pool(token0, token1, fees[0]));
            bytes memory callbackData = new bytes(32);
            assembly {
                mstore(add(callbackData, 32), payload)
            }
            if (token0 < token1) {
                pool0.swap(address(this), true, int128(amountIn), MIN_SQRT_RATIO + 1, callbackData);
            } else {
                pool0.swap(address(this), false, int128(amountIn), MAX_SQRT_RATIO - 1, callbackData);
            }
        }
        // === Camelot V3 (Algebra) — callback: algebraSwapCallback ===
        else if (exchangeId0 == CAMELOT_V3_ID) {
            IAlgebraPool pool0 = IAlgebraPool(IAlgebraFactory(CAMELOT_V3_FACTORY).poolByPair(token0, token1));
            bytes memory callbackData = new bytes(32);
            assembly {
                mstore(add(callbackData, 32), payload)
            }
            if (token0 < token1) {
                pool0.swap(address(this), true, int128(amountIn), MIN_SQRT_RATIO + 1, callbackData);
            } else {
                pool0.swap(address(this), false, int128(amountIn), MAX_SQRT_RATIO - 1, callbackData);
            }
        }
        // === Kyber Elastic — direct flash entry disabled; use Balancer fast path instead ===
        else if (exchangeId0 == KYBER_ID) {
            revert("Kyber: use flashLoanArb");
        }
        // === Trader Joe LB — no flash swap support, use flashLoanArb instead ===
        else if (exchangeId0 == TRADER_JOE_LB_ID) {
            revert("LB: use flashLoanArb");
        }
        // === V2-style DEXs (callback: uniswapV2Call) ===
        else {
            // stack too deep
            {
                address pool0;
                if (exchangeId0 == CHRONOS_ID || exchangeId0 == CHRONOS_STABLE_ID) {
                    bool stable = (exchangeId0 == CHRONOS_STABLE_ID);
                    pool0 = IChronosFactory(CHRONOS_FACTORY).getPair(token0, token1, stable);
                } else if (exchangeId0 == RAMSES_ID || exchangeId0 == RAMSES_STABLE_ID) {
                    bool stable = (exchangeId0 == RAMSES_STABLE_ID);
                    pool0 = IRamsesFactory(RAMSES_V2_FACTORY).getPair(token0, token1, stable);
                } else if (exchangeId0 == RAMSES_V2_NEW_ID || exchangeId0 == RAMSES_V2_NEW_STABLE_ID) {
                    bool stable = (exchangeId0 == RAMSES_V2_NEW_STABLE_ID);
                    pool0 = IRamsesFactory(RAMSES_V2_NEW_FACTORY).getPair(token0, token1, stable);
                } else if (exchangeId0 == CAMELOT_ID) {
                    pool0 = IUniswapV2Factory(CAMELOT_V2_FACTORY).getPair(token0, token1);
                } else if (exchangeId0 == UNISWAP_V2_ID) {
                    pool0 = IUniswapV2Factory(UNISWAP_V2_FACTORY).getPair(token0, token1);
                } else if (exchangeId0 == SUSHI_ID) {
                    pool0 = IUniswapV2Factory(SUSHI_FACTORY).getPair(token0, token1);
                } else if (exchangeId0 == ARBIDEX_ID) {
                    pool0 = IUniswapV2Factory(ARBIDEX_FACTORY).getPair(token0, token1);
                } else {
                    revert("Unknown flash swap exchange");
                }
                (uint256 reserve0, uint256 reserve1,) = IUniswapV2Pair(pool0).getReserves();

                // if we are given a trade usdc/weth -> weth/usdc
                // cannot borrow usdc then swap from the same pool...
                // but we know there's a profitable trade so we skip the first trade, borrow the output then trade and
                // return different token by swapping back to the pool
                // lock modifier prevents retrading on this pool...
                bytes memory callbackData = abi.encode(payload, amountIn);
                // a,b b,a
                // pull b, must paybcak
                if (token0 < token1) {
                    uint256 amountOut = UniswapV2Math.getAmountOut(uint256(amountIn), reserve0, reserve1);
                    IUniswapV2Pair(pool0).swap(0, amountOut, address(this), callbackData);
                } else {
                    uint256 amountOut = UniswapV2Math.getAmountOut(uint256(amountIn), reserve1, reserve0);
                    IUniswapV2Pair(pool0).swap(amountOut, 0, address(this), callbackData);
                }
            }
        }
    }

    /// @dev Execute a flash swap across up to 3 explicit hops. Curve and LB cannot be the flash-entry venue.
    function flashSwapV2(uint128 amountIn, TradeHop[3] calldata hops, uint128 minNetProfit) external {
        require(msg.sender == gateway, "Not gateway");
        TradeHop[3] memory route = hops;
        _validateTradeHops(route);
        // NOTE: `minNetProfit == 0` is a legal contract input — it means "execute and let the
        // payback Loss/InsufficientProfit gate be the only floor" (the convention used by fork tests
        // and REVM dry-runs). The POSITIVE economic floor that must cover L2+L1 gas is enforced
        // UPSTREAM in the engine submission path (engine `mantis_submit::build_mantis_calldata`,
        // which refuses min_net_profit == 0), so live submissions always carry a gas-covering floor.
        _minNetProfit = minNetProfit;
        _setFlashGuard(); // SEC-FLASH-ORIGIN: our own swap callbacks (via _initiateFlashSwapV2 -> pool.swap) require this
        _initiateFlashSwapV2(route[0], amountIn, abi.encode(route, amountIn));
    }

    /// @dev Execute a flash swap with mixed-hop routing. Uniswap v4 is allowed only on non-entry hops.
    function flashSwapMixed(uint128 amountIn, TradeHopV2[3] calldata hops, uint128 minNetProfit) external {
        require(msg.sender == gateway, "Not gateway");
        TradeHopV2[3] memory route = hops;
        _validateTradeHopsV2(route);
        require(route[0].exchangeId != UNISWAP_V4_ID, "V4: use flashLoanArbMixed");
        // MANTIS: a Balancer pool cannot be the flash-swap entry (no swap-callback funding path).
        // Balancer in a non-entry hop IS allowed here: a DEX flash-swap does not lock the Balancer Vault.
        require(!_isBalancerVaultId(route[0].exchangeId), "Balancer cannot be flash-swap entry; use flashLoanArbAaveMixed");
        // NOTE: `minNetProfit == 0` is a legal contract input — it means "execute and let the
        // payback Loss/InsufficientProfit gate be the only floor" (the convention used by fork tests
        // and REVM dry-runs). The POSITIVE economic floor that must cover L2+L1 gas is enforced
        // UPSTREAM in the engine submission path (engine `mantis_submit::build_mantis_calldata`,
        // which refuses min_net_profit == 0), so live submissions always carry a gas-covering floor.
        _minNetProfit = minNetProfit;
        _setFlashGuard(); // SEC-FLASH-ORIGIN: our own swap callbacks (via _initiateFlashSwapV2 -> pool.swap) require this
        _initiateFlashSwapV2(_toLegacyTradeHop(route[0]), amountIn, _encodeMixedCallback(route, amountIn));
    }

    /// @dev Execute arbitrage using Balancer V2 flash loan (0% fee)
    /// @param amountIn Amount to borrow from Balancer Vault
    /// @param payload Encoded trade path (exchanges, tokens, fees)
    /// @param minNetProfit Minimum acceptable profit (SEC-003 slippage protection)
    /// @notice Uses Balancer V2 flash loan which has 0% fee vs DEX flash swaps (0.01-1% fee)
    function flashLoanArb(uint128 amountIn, uint128 payload, uint128 minNetProfit) external {
        require(msg.sender == gateway, "Not gateway");
        // Store minNetProfit for callback to check (SEC-003)
        // NOTE: `minNetProfit == 0` is a legal contract input — it means "execute and let the
        // payback Loss/InsufficientProfit gate be the only floor" (the convention used by fork tests
        // and REVM dry-runs). The POSITIVE economic floor that must cover L2+L1 gas is enforced
        // UPSTREAM in the engine submission path (engine `mantis_submit::build_mantis_calldata`,
        // which refuses min_net_profit == 0), so live submissions always carry a gas-covering floor.
        _minNetProfit = minNetProfit;

        // Only need first token ID to determine borrow token
        uint8 token0Id = uint8(payload >> 24);
        address borrowToken = tokenLookup[token0Id];

        // Prepare flash loan parameters
        IERC20[] memory tokensToBorrow = new IERC20[](1);
        tokensToBorrow[0] = IERC20(borrowToken);

        uint256[] memory amounts = new uint256[](1);
        amounts[0] = amountIn;

        // Encode payload and amountIn for callback
        bytes memory userData = abi.encode(payload, amountIn);

        // Execute flash loan - Balancer will call receiveFlashLoan() callback
        _setFlashGuard();
        IBalancerVault(BALANCER_VAULT).flashLoan(IFlashLoanRecipient(address(this)), tokensToBorrow, amounts, userData);
        _clearFlashGuard();
    }

    /// @dev Execute arbitrage using explicit per-hop routing data, including direct Curve pool legs.
    function flashLoanArbV2(uint128 amountIn, TradeHop[3] calldata hops, uint128 minNetProfit) external {
        require(msg.sender == gateway, "Not gateway");
        TradeHop[3] memory route = hops;
        _validateTradeHops(route);
        // NOTE: `minNetProfit == 0` is a legal contract input — it means "execute and let the
        // payback Loss/InsufficientProfit gate be the only floor" (the convention used by fork tests
        // and REVM dry-runs). The POSITIVE economic floor that must cover L2+L1 gas is enforced
        // UPSTREAM in the engine submission path (engine `mantis_submit::build_mantis_calldata`,
        // which refuses min_net_profit == 0), so live submissions always carry a gas-covering floor.
        _minNetProfit = minNetProfit;

        IERC20[] memory tokensToBorrow = new IERC20[](1);
        tokensToBorrow[0] = IERC20(route[0].tokenIn);

        uint256[] memory amounts = new uint256[](1);
        amounts[0] = amountIn;

        bytes memory userData = abi.encode(route, amountIn);
        _setFlashGuard();
        IBalancerVault(BALANCER_VAULT).flashLoan(IFlashLoanRecipient(address(this)), tokensToBorrow, amounts, userData);
        _clearFlashGuard();
    }

    /// @dev Execute Balancer flash-loan arbitrage with mixed-hop routing, including Uniswap v4 vanilla legs.
    function flashLoanArbMixed(uint128 amountIn, TradeHopV2[3] calldata hops, uint128 minNetProfit) external {
        require(msg.sender == gateway, "Not gateway");
        TradeHopV2[3] memory route = hops;
        _validateTradeHopsV2(route);
        // MANTIS: cannot Balancer-flash-fund a route that trades a Balancer pool (vault is
        // non-reentrant). Route it to the Aave-funded path instead.
        require(!_routeTradesBalancer(route), "Balancer leg: use flashLoanArbAaveMixed");
        // NOTE: `minNetProfit == 0` is a legal contract input — it means "execute and let the
        // payback Loss/InsufficientProfit gate be the only floor" (the convention used by fork tests
        // and REVM dry-runs). The POSITIVE economic floor that must cover L2+L1 gas is enforced
        // UPSTREAM in the engine submission path (engine `mantis_submit::build_mantis_calldata`,
        // which refuses min_net_profit == 0), so live submissions always carry a gas-covering floor.
        _minNetProfit = minNetProfit;

        IERC20[] memory tokensToBorrow = new IERC20[](1);
        tokensToBorrow[0] = IERC20(route[0].tokenIn);

        uint256[] memory amounts = new uint256[](1);
        amounts[0] = amountIn;

        bytes memory userData = _encodeMixedCallback(route, amountIn);
        _setFlashGuard();
        IBalancerVault(BALANCER_VAULT).flashLoan(IFlashLoanRecipient(address(this)), tokensToBorrow, amounts, userData);
        _clearFlashGuard();
    }

    /// MANTIS: Aave V3 flash-loan funded arbitrage (0.05% premium). REQUIRED for any route that
    /// trades a Balancer pool (Balancer's non-reentrant vault forbids a Balancer flash loan there),
    /// and usable as a fallback whenever the Balancer Vault lacks inventory for the borrow token.
    /// Balancer pools may appear at ANY hop here (first/middle/last) — Aave is the flash source,
    /// so the Balancer Vault is not locked.
    function flashLoanArbAaveMixed(uint128 amountIn, TradeHopV2[3] calldata hops, uint128 minNetProfit) external {
        require(msg.sender == gateway, "Not gateway");
        TradeHopV2[3] memory route = hops;
        _validateTradeHopsV2(route);
        // NOTE: `minNetProfit == 0` is a legal contract input — it means "execute and let the
        // payback Loss/InsufficientProfit gate be the only floor" (the convention used by fork tests
        // and REVM dry-runs). The POSITIVE economic floor that must cover L2+L1 gas is enforced
        // UPSTREAM in the engine submission path (engine `mantis_submit::build_mantis_calldata`,
        // which refuses min_net_profit == 0), so live submissions always carry a gas-covering floor.
        _minNetProfit = minNetProfit;
        bytes memory params = _encodeMixedCallback(route, amountIn);
        // Aave calls executeOperation() with the borrowed asset; repayment is pulled via allowance.
        _setFlashGuard();
        IAaveV3Pool(AAVE_V3_POOL).flashLoanSimple(address(this), route[0].tokenIn, amountIn, params, 0);
        _clearFlashGuard();
    }

    /// MANTIS: 4-hop Balancer-flash-funded arbitrage. Same model as flashLoanArbMixed but a
    /// TradeHopV2[4] route (3-hop routes can also use this with hops[3] empty). Captures the deeper
    /// cycles the n-hop detector finds but the [3] executor couldn't carry.
    function flashLoanArbMixed4(uint128 amountIn, TradeHopV2[4] calldata hops, uint128 minNetProfit) external {
        require(msg.sender == gateway, "Not gateway");
        TradeHopV2[4] memory route = hops;
        _validateTradeHopsV2_4(route);
        require(!_routeTradesBalancer4(route), "Balancer leg: use flashLoanArbAaveMixed4");
        // NOTE: `minNetProfit == 0` is a legal contract input — it means "execute and let the
        // payback Loss/InsufficientProfit gate be the only floor" (the convention used by fork tests
        // and REVM dry-runs). The POSITIVE economic floor that must cover L2+L1 gas is enforced
        // UPSTREAM in the engine submission path (engine `mantis_submit::build_mantis_calldata`,
        // which refuses min_net_profit == 0), so live submissions always carry a gas-covering floor.
        _minNetProfit = minNetProfit;
        IERC20[] memory tokensToBorrow = new IERC20[](1);
        tokensToBorrow[0] = IERC20(route[0].tokenIn);
        uint256[] memory amounts = new uint256[](1);
        amounts[0] = amountIn;
        bytes memory userData = _encodeMixedCallback4(route, amountIn);
        _setFlashGuard();
        IBalancerVault(BALANCER_VAULT).flashLoan(IFlashLoanRecipient(address(this)), tokensToBorrow, amounts, userData);
        _clearFlashGuard();
    }

    /// MANTIS: 4-hop Aave-flash-funded arbitrage (0.05% premium). Use for 4-hop routes that trade a
    /// Balancer pool, or as a Balancer-inventory fallback.
    function flashLoanArbAaveMixed4(uint128 amountIn, TradeHopV2[4] calldata hops, uint128 minNetProfit) external {
        require(msg.sender == gateway, "Not gateway");
        TradeHopV2[4] memory route = hops;
        _validateTradeHopsV2_4(route);
        // NOTE: `minNetProfit == 0` is a legal contract input — it means "execute and let the
        // payback Loss/InsufficientProfit gate be the only floor" (the convention used by fork tests
        // and REVM dry-runs). The POSITIVE economic floor that must cover L2+L1 gas is enforced
        // UPSTREAM in the engine submission path (engine `mantis_submit::build_mantis_calldata`,
        // which refuses min_net_profit == 0), so live submissions always carry a gas-covering floor.
        _minNetProfit = minNetProfit;
        bytes memory params = _encodeMixedCallback4(route, amountIn);
        _setFlashGuard();
        IAaveV3Pool(AAVE_V3_POOL).flashLoanSimple(address(this), route[0].tokenIn, amountIn, params, 0);
        _clearFlashGuard();
    }

    /// MANTIS: Aave V3 flashLoanSimple callback. Executes the route then approves amount+premium
    /// for the pool to pull (Aave pull-repayment model, unlike Balancer's transfer-back).
    function executeOperation(
        address asset,
        uint256 amount,
        uint256 premium,
        address initiator,
        bytes calldata params
    ) external nonReentrant returns (bool) {
        require(msg.sender == AAVE_V3_POOL, "Invalid Aave callback");
        require(initiator == address(this), "Bad initiator");
        // Defense in depth (the initiator check above already blocks third-party Aave flashes).
        _requireFlashGuard();

        bytes memory data = params;
        // 4-hop path dispatch (additive): execute the TradeHopV2[4] route, same repay model.
        if (_hasMixedCallbackMagic4Bytes(data)) {
            (TradeHopV2[4] memory hops4,) = _decodeMixedCallback4Bytes(data);
            _validateTradeHopsV2_4(hops4);
            uint256 netOut4 = _executeTradeHopsV2_4(hops4, amount);
            uint256 repay4 = amount + premium;
            if (netOut4 <= repay4) revert Loss(repay4 - netOut4);
            uint256 profit4 = netOut4 - repay4;
            uint256 minP4 = _minNetProfit;
            _minNetProfit = 0;
            if (profit4 < minP4) revert InsufficientProfit(profit4, minP4);
            OZIERC20(asset).forceApprove(AAVE_V3_POOL, repay4);
            OZIERC20(asset).safeTransfer(payee, profit4);
            return true;
        }
        (TradeHopV2[3] memory hops,) = _decodeMixedCallbackBytes(data);
        _validateTradeHopsV2(hops);

        uint256 netAmountOut = _executeTradeHopsV2(hops, amount);
        uint256 repayAmount = amount + premium;
        if (netAmountOut <= repayAmount) revert Loss(repayAmount - netAmountOut);

        uint256 profit = netAmountOut - repayAmount;
        uint256 minProfit = _minNetProfit;
        _minNetProfit = 0;
        if (profit < minProfit) revert InsufficientProfit(profit, minProfit);

        // Aave pulls `repayAmount` via transferFrom — approve exactly that.
        OZIERC20(asset).forceApprove(AAVE_V3_POOL, repayAmount);
        OZIERC20(asset).safeTransfer(payee, profit);
        return true;
    }

    /// MANTIS: true iff any hop trades a Balancer pool (then Balancer-flash funding is illegal).
    function _routeTradesBalancer(TradeHopV2[3] memory hops) internal pure returns (bool) {
        return _isBalancerVaultId(hops[0].exchangeId) || _isBalancerVaultId(hops[1].exchangeId)
            || _isBalancerVaultId(hops[2].exchangeId);
    }

    /// MANTIS: swap one Balancer V2 stable-pool leg via Vault.swap (GIVEN_IN). Legal only when the
    /// Vault is not locked by a Balancer flash loan (i.e. Aave-funded or DEX-flash-swap-funded route).
    function _swapExactInBalancer(TradeHopV2 memory hop, uint256 amountIn) private returns (uint256 amountOut) {
        require(hop.poolId != bytes32(0), "Balancer poolId required");
        OZIERC20(hop.tokenIn).forceApprove(BALANCER_VAULT, amountIn);
        IBalancerVaultSingleSwap.SingleSwap memory singleSwap = IBalancerVaultSingleSwap.SingleSwap({
            poolId: hop.poolId,
            kind: IBalancerVaultSingleSwap.SwapKind.GIVEN_IN,
            assetIn: hop.tokenIn,
            assetOut: hop.tokenOut,
            amount: amountIn,
            userData: ""
        });
        IBalancerVaultSingleSwap.FundManagement memory funds = IBalancerVaultSingleSwap.FundManagement({
            sender: address(this),
            fromInternalBalance: false,
            recipient: payable(address(this)),
            toInternalBalance: false
        });
        amountOut = IBalancerVaultSingleSwap(BALANCER_VAULT).swap(singleSwap, funds, 0, block.timestamp);
        require(amountOut > 0, "Balancer zero output");
    }

    /// FIX (SEC-002): Added nonReentrant modifier to prevent reentrancy attacks
    /// FIX (SEC-007): Added callback verification to prevent unauthorized calls
    // SEC-FLASH-ORIGIN aliases: V2-fork pairs invoke their swap callback under different NAMES —
    // Uniswap/Sushi/Camelot = `uniswapV2Call`, Arbidex = `arbdexCall`, Solidly forks
    // (Chronos/Ramses/RamsesV2New) = `hook`. All route to the same flash-origin-guarded handler
    // `_onV2Callback`, which still `_requireFlashGuard()`s and verifies the calling pair by
    // exchangeId, so an unsolicited callback under ANY of these names reverts.
    function uniswapV2Call(address, uint256 amount0, uint256 amount1, bytes calldata data) external nonReentrant {
        _onV2Callback(amount0, amount1, data);
    }

    function arbdexCall(address, uint256 amount0, uint256 amount1, bytes calldata data) external nonReentrant {
        _onV2Callback(amount0, amount1, data);
    }

    function hook(address, uint256 amount0, uint256 amount1, bytes calldata data) external nonReentrant {
        _onV2Callback(amount0, amount1, data);
    }

    function _onV2Callback(uint256 amount0, uint256 amount1, bytes calldata data) internal {
        // SEC-FLASH-ORIGIN: a V2 pair calls `uniswapV2Call` on the `to` RECIPIENT, which an attacker
        // can set to THIS contract via a crafted `pair.swap(amountOut,0,to=executor,data=route)` —
        // passing `_verifyV2Callback` (a real pair) and driving our funds/approvals through an
        // attacker route. The transient flash-origin guard is set ONLY when WE initiate a swap
        // (flashSwap*/flashLoanArb*), so an unsolicited callback reverts here.
        _requireFlashGuard();
        if (_hasMixedCallbackMagic(data)) {
            (TradeHopV2[3] memory hops, uint128 callbackAmountInOwed) = _decodeMixedCallback(data);
            _validateTradeHopsV2(hops);
            _verifyTradeHopCallbackV2(hops[0]);
            _settleFlashSwapMixed(hops, callbackAmountInOwed, amount0, amount1);
            return;
        }

        if (data.length != 64) {
            (TradeHop[3] memory hops, uint128 callbackAmountInOwed) = abi.decode(data, (TradeHop[3], uint128));
            _validateTradeHops(hops);
            _verifyTradeHopCallback(hops[0]);
            uint256 callbackNetAmountOut = amount0 > 0 ? amount0 : amount1;
            callbackNetAmountOut = _executeRemainingHops(hops, callbackNetAmountOut);
            payback(callbackAmountInOwed, callbackNetAmountOut, hops[0].tokenIn);
            return;
        }

        // we have entered a flash position, now finish the arb
        (uint128 payload, uint128 amountInOwed) = abi.decode(data, (uint128, uint128));

        (uint8[3] memory exchanges, uint8[3] memory tokens, uint16[3] memory fees) = decode(payload);
        address token0 = tokenLookup[tokens[0]];
        address token1 = tokenLookup[tokens[1]];
        uint8 token2Id = tokens[2];
        uint8 exchangeId0 = exchanges[0];

        // FIX (SEC-007): Verify caller is legitimate pool based on exchange
        if (exchangeId0 == CAMELOT_ID) {
            _verifyV2Callback(CAMELOT_V2_FACTORY, CAMELOT_V2_INIT_HASH, token0, token1);
        } else if (exchangeId0 == UNISWAP_V2_ID) {
            _verifyV2Callback(UNISWAP_V2_FACTORY, UNISWAP_V2_INIT_HASH, token0, token1);
        } else if (exchangeId0 == SUSHI_ID) {
            _verifyV2Callback(SUSHI_FACTORY, SUSHI_INIT_HASH, token0, token1);
        } else if (exchangeId0 == CHRONOS_ID || exchangeId0 == CHRONOS_STABLE_ID) {
            _verifySolidlyCallback(CHRONOS_FACTORY, token0, token1, exchangeId0 == CHRONOS_STABLE_ID);
        } else if (exchangeId0 == RAMSES_ID || exchangeId0 == RAMSES_STABLE_ID) {
            _verifySolidlyCallback(RAMSES_V2_FACTORY, token0, token1, exchangeId0 == RAMSES_STABLE_ID);
        } else if (exchangeId0 == RAMSES_V2_NEW_ID || exchangeId0 == RAMSES_V2_NEW_STABLE_ID) {
            _verifySolidlyCallback(RAMSES_V2_NEW_FACTORY, token0, token1, exchangeId0 == RAMSES_V2_NEW_STABLE_ID);
        } else if (exchangeId0 == ARBIDEX_ID) {
            _verifyV2Callback(ARBIDEX_FACTORY, ARBIDEX_INIT_HASH, token0, token1);
        } else {
            revert("Unknown V2 exchange");
        }

        uint256 loanAmountOut = amount0 > 0 ? amount0 : amount1;
        uint256 netAmountOut = loanAmountOut;
        if (token2Id == 255) {
            // 2 step
            // we are holding token 1 at this point
            netAmountOut = swapExactIn(exchanges[1], netAmountOut, token1, token0, fees[1]);
        } else {
            // triangle
            address token2 = tokenLookup[token2Id];
            netAmountOut = swapExactIn(exchanges[1], netAmountOut, token1, token2, fees[1]);
            netAmountOut = swapExactIn(exchanges[2], netAmountOut, token2, token0, fees[2]);
        }

        // always payback in the starting token(0)
        payback(amountInOwed, netAmountOut, token0);
    }

    /// FIX (SEC-002): Added nonReentrant modifier to prevent reentrancy attacks
    /// FIX (SEC-007): Added callback verification to prevent unauthorized calls
    function uniswapV3SwapCallback(int256 amount0Delta, int256 amount1Delta, bytes calldata data)
        external
        nonReentrant
    {
        // SEC-FLASH-ORIGIN: only execute a swap callback we ourselves initiated (flash guard set by
        // flashSwap*/flashLoanArb*); reject any unsolicited pool callback. Defense-in-depth on the V3
        // path (the pool calls back the swap INITIATOR, but we still require our own origin).
        _requireFlashGuard();
        if (_hasMixedCallbackMagic(data)) {
            (TradeHopV2[3] memory hops,) = _decodeMixedCallback(data);
            _validateTradeHopsV2(hops);
            _verifyTradeHopCallbackV2(hops[0]);
            _settleFlashSwapMixedWithDeltas(hops, amount0Delta, amount1Delta);
            return;
        }

        if (data.length != 32) {
            (TradeHop[3] memory hops,) = abi.decode(data, (TradeHop[3], uint128));
            _validateTradeHops(hops);
            _verifyTradeHopCallback(hops[0]);
            _settleFlashSwapV2WithDeltas(hops, amount0Delta, amount1Delta);
            return;
        }

        // we have entered a flash position, now finish the arb
        uint128 payload = uint128(uint256(bytes32(data)));
        (uint8[3] memory exchanges, uint8[3] memory tokens, uint16[3] memory fees) = decode(payload);

        // scoping to fix stack depth
        address token0 = tokenLookup[tokens[0]];
        address token1 = tokenLookup[tokens[1]];
        uint8 token2Id = tokens[2];

        // FIX (SEC-007): Verify caller is legitimate V3 pool (Uniswap V3 or Ramses CL)
        uint8 exchangeId0 = exchanges[0];
        if (exchangeId0 == RAMSES_CL_ID) {
            _verifyRamsesCLCallback(token0, token1, fees[0]);
        } else if (exchangeId0 == SUSHISWAP_V3_ID) {
            _verifyV3Callback(SUSHISWAP_V3_FACTORY, SUSHISWAP_V3_INIT_HASH, token0, token1, fees[0]);
        } else {
            _verifyV3Callback(UNISWAP_V3_FACTORY, UNISWAP_V3_INIT_HASH, token0, token1, fees[0]);
        }

        // run trades
        uint256 amountInputOwed;
        uint256 amountInputEarned;

        // swap is loaning the output amount/token  (need to payback input by the end)
        // by this point we are paid in token1 by the initial swap
        if (token0 < token1) {
            // is zeroForOne
            amountInputOwed = uint256(amount0Delta);
            amountInputEarned = uint256(-amount1Delta);
        } else {
            amountInputOwed = uint256(amount1Delta);
            amountInputEarned = uint256(-amount0Delta);
        }

        // Execute remaining swaps (2-step or triangle)
        if (token2Id == 255) {
            // 2 step: swap back token1 → token0
            amountInputEarned = swapExactIn(exchanges[1], amountInputEarned, token1, token0, fees[1]);
        } else {
            // Triangle: token1 → token2 → token0
            address token2 = tokenLookup[token2Id];
            amountInputEarned = swapExactIn(exchanges[1], amountInputEarned, token1, token2, fees[1]);
            amountInputEarned = swapExactIn(exchanges[2], amountInputEarned, token2, token0, fees[2]);
        }
        payback(amountInputOwed, amountInputEarned, token0);
    }

    /// @notice PancakeSwap V3 callback - same logic as Uniswap V3 but different callback name
    /// FIX (SEC-007): Callback verification ensures only legitimate PancakeSwap V3 pools can call
    function pancakeV3SwapCallback(int256 amount0Delta, int256 amount1Delta, bytes calldata data)
        external
        nonReentrant
    {
        // SEC-FLASH-ORIGIN: reject any pool callback we did not ourselves initiate.
        _requireFlashGuard();
        if (_hasMixedCallbackMagic(data)) {
            (TradeHopV2[3] memory hops,) = _decodeMixedCallback(data);
            _validateTradeHopsV2(hops);
            _verifyTradeHopCallbackV2(hops[0]);
            _settleFlashSwapMixedWithDeltas(hops, amount0Delta, amount1Delta);
            return;
        }

        if (data.length != 32) {
            (TradeHop[3] memory hops,) = abi.decode(data, (TradeHop[3], uint128));
            _validateTradeHops(hops);
            _verifyTradeHopCallback(hops[0]);
            _settleFlashSwapV2WithDeltas(hops, amount0Delta, amount1Delta);
            return;
        }

        uint128 payload = uint128(uint256(bytes32(data)));
        (uint8[3] memory exchanges, uint8[3] memory tokens, uint16[3] memory fees) = decode(payload);

        address token0 = tokenLookup[tokens[0]];
        address token1 = tokenLookup[tokens[1]];
        uint8 token2Id = tokens[2];

        // FIX (SEC-007): Verify caller is legitimate PancakeSwap V3 pool.
        // PancakeV3 CREATE2 uses the PoolDeployer (not the factory) — see PANCAKE_V3_DEPLOYER.
        _verifyV3Callback(PANCAKE_V3_DEPLOYER, PANCAKE_V3_INIT_HASH, token0, token1, fees[0]);

        uint256 amountInputOwed;
        uint256 amountInputEarned;

        if (token0 < token1) {
            amountInputOwed = uint256(amount0Delta);
            amountInputEarned = uint256(-amount1Delta);
        } else {
            amountInputOwed = uint256(amount1Delta);
            amountInputEarned = uint256(-amount0Delta);
        }

        if (token2Id == 255) {
            amountInputEarned = swapExactIn(exchanges[1], amountInputEarned, token1, token0, fees[1]);
        } else {
            address token2 = tokenLookup[token2Id];
            amountInputEarned = swapExactIn(exchanges[1], amountInputEarned, token1, token2, fees[1]);
            amountInputEarned = swapExactIn(exchanges[2], amountInputEarned, token2, token0, fees[2]);
        }
        payback(amountInputOwed, amountInputEarned, token0);
    }

    /// @notice Algebra callback for Camelot V3 flash-entry swaps
    /// FIX (SEC-007): Callback verification ensures only legitimate Camelot Algebra pools can call
    function algebraSwapCallback(int256 amount0Delta, int256 amount1Delta, bytes calldata data) external nonReentrant {
        // SEC-FLASH-ORIGIN: reject any pool callback we did not ourselves initiate.
        _requireFlashGuard();
        if (_hasMixedCallbackMagic(data)) {
            (TradeHopV2[3] memory hops,) = _decodeMixedCallback(data);
            _validateTradeHopsV2(hops);
            _verifyTradeHopCallbackV2(hops[0]);
            _settleFlashSwapMixedWithDeltas(hops, amount0Delta, amount1Delta);
            return;
        }

        if (data.length != 32) {
            (TradeHop[3] memory hops,) = abi.decode(data, (TradeHop[3], uint128));
            _validateTradeHops(hops);
            _verifyTradeHopCallback(hops[0]);
            _settleFlashSwapV2WithDeltas(hops, amount0Delta, amount1Delta);
            return;
        }

        uint128 payload = uint128(uint256(bytes32(data)));
        (uint8[3] memory exchanges, uint8[3] memory tokens, uint16[3] memory fees) = decode(payload);

        address token0 = tokenLookup[tokens[0]];
        address token1 = tokenLookup[tokens[1]];
        uint8 token2Id = tokens[2];
        uint8 exchangeId0 = exchanges[0];

        // FIX (SEC-007): Verify caller is the legitimate Camelot V3 Algebra pool.
        if (exchangeId0 == CAMELOT_V3_ID) {
            _verifyAlgebraCallback(CAMELOT_V3_FACTORY, token0, token1);
        } else {
            revert("Unknown Algebra exchange");
        }

        uint256 amountInputOwed;
        uint256 amountInputEarned;

        if (token0 < token1) {
            amountInputOwed = uint256(amount0Delta);
            amountInputEarned = uint256(-amount1Delta);
        } else {
            amountInputOwed = uint256(amount1Delta);
            amountInputEarned = uint256(-amount0Delta);
        }

        if (token2Id == 255) {
            amountInputEarned = swapExactIn(exchanges[1], amountInputEarned, token1, token0, fees[1]);
        } else {
            address token2 = tokenLookup[token2Id];
            amountInputEarned = swapExactIn(exchanges[1], amountInputEarned, token1, token2, fees[1]);
            amountInputEarned = swapExactIn(exchanges[2], amountInputEarned, token2, token0, fees[2]);
        }
        payback(amountInputOwed, amountInputEarned, token0);
    }

    /// @notice Balancer V2 flash loan callback - receives borrowed tokens and executes arbitrage
    /// @dev Called by Balancer Vault after flash loan is initiated
    /// @param tokens Array of borrowed token addresses
    /// @param amounts Array of borrowed amounts
    /// @param feeAmounts Array of fees owed (0% for Balancer V2)
    /// @param userData Encoded (payload, amountIn) from flashLoanArb()
    function receiveFlashLoan(
        IERC20[] memory tokens,
        uint256[] memory amounts,
        uint256[] memory feeAmounts,
        bytes memory userData
    ) external nonReentrant {
        // FIX (SEC-007): Verify caller is Balancer Vault
        require(msg.sender == BALANCER_VAULT, "Invalid flash loan callback");
        // FIX (SEC-FLASH-ORIGIN): require a gateway-authorized flash to be in progress. Anyone can
        // call Vault.flashLoan(recipient=this, userData=craftedRoute); without this an unsolicited
        // callback would execute attacker-controlled hops against the contract's blanket approvals.
        _requireFlashGuard();

        // 4-hop path dispatch (additive): settle the TradeHopV2[4] route via Balancer flash.
        if (_hasMixedCallbackMagic4Bytes(userData)) {
            (TradeHopV2[4] memory hops4,) = _decodeMixedCallback4Bytes(userData);
            _validateTradeHopsV2_4(hops4);
            _settleFlashLoanMixed4(hops4, tokens, amounts, feeAmounts);
            return;
        }
        if (_hasMixedCallbackMagicBytes(userData)) {
            (TradeHopV2[3] memory hops,) = _decodeMixedCallbackBytes(userData);
            _validateTradeHopsV2(hops);
            _settleFlashLoanMixed(hops, tokens, amounts, feeAmounts);
            return;
        }

        if (userData.length != 64) {
            (TradeHop[3] memory hops,) = abi.decode(userData, (TradeHop[3], uint128));
            _validateTradeHops(hops);
            _settleFlashLoanV2(hops, tokens, amounts, feeAmounts);
            return;
        }

        // Decode trade data
        (uint128 payload,) = abi.decode(userData, (uint128, uint128));
        (uint8[3] memory exchanges, uint8[3] memory tokensPath, uint16[3] memory fees) = decode(payload);

        address token0 = tokenLookup[tokensPath[0]];
        address token1 = tokenLookup[tokensPath[1]];
        uint8 token2Id = tokensPath[2];

        // Start with borrowed amount
        uint256 netAmountOut = amounts[0];

        // Execute first swap: token0 → token1
        netAmountOut = swapExactIn(exchanges[0], netAmountOut, token0, token1, fees[0]);

        // Execute remaining swaps (2-step or 3-step triangle)
        if (token2Id == 255) {
            // 2-step: token1 → token0
            netAmountOut = swapExactIn(exchanges[1], netAmountOut, token1, token0, fees[1]);
        } else {
            // 3-step triangle: token1 → token2 → token0
            address token2 = tokenLookup[token2Id];
            netAmountOut = swapExactIn(exchanges[1], netAmountOut, token1, token2, fees[1]);
            netAmountOut = swapExactIn(exchanges[2], netAmountOut, token2, token0, fees[2]);
        }

        // Calculate repayment (borrowed + fee, fee is 0 for Balancer V2)
        uint256 repayAmount = amounts[0] + feeAmounts[0];

        // Check profitability
        if (netAmountOut <= repayAmount) revert Loss(repayAmount - netAmountOut);

        uint256 profit = netAmountOut - repayAmount;

        // SEC-003: Check minimum profit threshold after modeled costs.
        uint256 minProfit = _minNetProfit;
        _minNetProfit = 0; // Clear for next trade (fail-safe)
        if (profit < minProfit) revert InsufficientProfit(profit, minProfit);

        // Repay flash loan to Balancer Vault
        OZIERC20(token0).safeTransfer(BALANCER_VAULT, repayAmount);
        // Send profit to payee
        OZIERC20(token0).safeTransfer(payee, profit);
    }

    function swapExactIn(uint8 exchangeId, uint256 amountIn, address tokenIn, address tokenOut, uint16 fee)
        private
        returns (uint256 amountOut)
    {
        if (_isCurveExchange(exchangeId)) {
            revert("Curve: use flashLoanArbV2");
        }

        return _swapExactInHop(
            TradeHop({
                exchangeId: exchangeId,
                tokenIn: tokenIn,
                tokenOut: tokenOut,
                fee: fee,
                pool: address(0),
                coinInIdx: 0,
                coinOutIdx: 0
            }),
            amountIn
        );
    }

    function _v4PoolKey(TradeHopV2 memory hop) internal pure returns (V4PoolKey memory key) {
        (address currency0, address currency1) =
            hop.tokenIn < hop.tokenOut ? (hop.tokenIn, hop.tokenOut) : (hop.tokenOut, hop.tokenIn);
        key = V4PoolKey({
            currency0: Currency.wrap(currency0),
            currency1: Currency.wrap(currency1),
            fee: hop.fee,
            tickSpacing: hop.tickSpacing,
            hooks: IHooks(hop.hooks)
        });
    }

    function _v4PoolId(V4PoolKey memory key) internal pure returns (bytes32) {
        return keccak256(abi.encode(key));
    }

    function _settleV4Debt(IPoolManagerV4 manager, Currency currency, uint256 amount) internal {
        manager.sync(currency);
        OZIERC20(Currency.unwrap(currency)).safeTransfer(address(manager), amount);
        manager.settle();
    }

    function _swapExactInV4(TradeHopV2 memory hop, uint256 amountIn) private returns (uint256 amountOut) {
        require(hop.exchangeId == UNISWAP_V4_ID, "Not v4");
        require(hop.hooks == address(0), "Hooks unsupported");
        require(hop.poolManager == UNISWAP_V4_POOL_MANAGER, "Bad pool manager");
        require(amountIn <= uint256(type(int256).max), "Amount overflow");

        bytes memory result = IPoolManagerV4(hop.poolManager).unlock(abi.encode(hop, amountIn));
        amountOut = abi.decode(result, (uint256));
    }

    function _swapExactInFluid(address tokenIn, address tokenOut, address pool, uint256 amountIn)
        private
        returns (uint256 amountOut)
    {
        require(pool != address(0), "Fluid pool required");
        // Fluid pools collect tokenIn via pool-driven callback transferFrom during settlement.
        OZIERC20(tokenIn).forceApprove(pool, amountIn);
        // Fluid swapIn uses swap0to1 against the pool's canonical token ordering from discovery/resolver.
        bool swap0to1 = tokenIn < tokenOut;
        amountOut = IFluidDexT1(pool).swapIn(swap0to1, amountIn, 0, address(this));
    }

    function _swapExactInMaverick(address tokenIn, address tokenOut, address pool, uint256 amountIn)
        private
        returns (uint256 amountOut)
    {
        require(pool != address(0), "Maverick pool required");

        // Maverick's push-funded exact-in path expects the input tokens to be
        // transferred to the pool before swap(..., data=""), which avoids the
        // callback execution path entirely.
        OZIERC20(tokenIn).safeTransfer(pool, amountIn);

        bool tokenAIn = tokenIn < tokenOut;
        IMaverickV2Pool.SwapParams memory params = IMaverickV2Pool.SwapParams({
            amount: amountIn,
            tokenAIn: tokenAIn,
            exactOutput: false,
            tickLimit: tokenAIn ? type(int32).max : type(int32).min
        });

        (, amountOut) = IMaverickV2Pool(pool).swap(address(this), params, new bytes(0));
    }

    function _swapExactInDodo(address tokenIn, address tokenOut, address pool, uint256 amountIn)
        private
        returns (uint256 amountOut)
    {
        require(pool != address(0), "DODO pool required");

        address baseToken = IDODOV2Pool(pool)._BASE_TOKEN_();
        address quoteToken = IDODOV2Pool(pool)._QUOTE_TOKEN_();
        require(
            (tokenIn == baseToken && tokenOut == quoteToken) || (tokenIn == quoteToken && tokenOut == baseToken),
            "DODO token mismatch"
        );

        uint256 balanceBefore = IERC20(tokenOut).balanceOf(address(this));
        OZIERC20(tokenIn).safeTransfer(pool, amountIn);

        uint256 reportedAmountOut;
        if (tokenIn == baseToken) {
            reportedAmountOut = IDODOV2Pool(pool).sellBase(address(this));
        } else {
            reportedAmountOut = IDODOV2Pool(pool).sellQuote(address(this));
        }

        amountOut = IERC20(tokenOut).balanceOf(address(this)) - balanceBefore;
        require(amountOut > 0, "DODO zero output");
        if (reportedAmountOut > 0) {
            // `>=` not `==`: the measured received balance must be AT LEAST what DODO reported.
            // Strict equality would revert a legitimate swap whose OUTPUT token is fee-on-transfer
            // (received < reported). Receiving MORE than reported is never a loss (the downstream
            // payback/minNetProfit gate still bounds economics); receiving LESS is the real fault.
            require(amountOut >= reportedAmountOut, "DODO output short");
        }
    }

    function _swapExactInHopV2(TradeHopV2 memory hop, uint256 amountIn) private returns (uint256 amountOut) {
        if (_isUniswapV4Hop(hop)) {
            return _swapExactInV4(hop, amountIn);
        }
        if (hop.exchangeId == FLUID_DEX_ID) {
            return _swapExactInFluid(hop.tokenIn, hop.tokenOut, hop.pool, amountIn);
        }
        if (hop.exchangeId == MAVERICK_V2_ID) {
            return _swapExactInMaverick(hop.tokenIn, hop.tokenOut, hop.pool, amountIn);
        }
        if (hop.exchangeId == DODO_V2_ID) {
            return _swapExactInDodo(hop.tokenIn, hop.tokenOut, hop.pool, amountIn);
        }
        if (hop.exchangeId == BALANCER_STABLE_ID || hop.exchangeId == BALANCER_WEIGHTED_ID) {
            // Stable + weighted share the Vault single-swap entrypoint; the Vault dispatches
            // to the pool's own invariant math by poolId.
            return _swapExactInBalancer(hop, amountIn);
        }
        return _swapExactInHop(_toLegacyTradeHop(hop), amountIn);
    }

    function _swapExactInHop(TradeHop memory hop, uint256 amountIn) private returns (uint256 amountOut) {
        address recipient = address(this);
        if (hop.exchangeId == UNISWAP_V3_ID) {
            bool zeroForOne = hop.tokenIn < hop.tokenOut;
            uint160 sqrtPriceLimitX96 = zeroForOne ? MIN_SQRT_RATIO + 1 : MAX_SQRT_RATIO - 1;
            ISwapRouter.ExactInputSingleParams memory params = ISwapRouter.ExactInputSingleParams({
                tokenIn: hop.tokenIn,
                tokenOut: hop.tokenOut,
                fee: hop.fee,
                recipient: recipient,
                amountIn: amountIn,
                amountOutMinimum: 0,
                sqrtPriceLimitX96: sqrtPriceLimitX96
            });
            ISwapRouter uniswapV3Router = ISwapRouter(UNISWAP_V3_ROUTER);
            return uniswapV3Router.exactInputSingle(params);
        }

        if (hop.exchangeId == SUSHISWAP_V3_ID) {
            // MANTIS FIX: Sushi's V3 SwapRouter is the ORIGINAL UniV3 SwapRouter (deadline-bearing
            // ABI). The inherited code called the SwapRouter02 (no-deadline) variant → empty revert.
            bool zeroForOne = hop.tokenIn < hop.tokenOut;
            uint160 sqrtPriceLimitX96 = zeroForOne ? MIN_SQRT_RATIO + 1 : MAX_SQRT_RATIO - 1;
            ISushiV3SwapRouter.ExactInputSingleParams memory params = ISushiV3SwapRouter.ExactInputSingleParams({
                tokenIn: hop.tokenIn,
                tokenOut: hop.tokenOut,
                fee: hop.fee,
                recipient: recipient,
                deadline: block.timestamp,
                amountIn: amountIn,
                amountOutMinimum: 0,
                sqrtPriceLimitX96: sqrtPriceLimitX96
            });
            return ISushiV3SwapRouter(SUSHISWAP_V3_ROUTER).exactInputSingle(params);
        }

        if (hop.exchangeId == CAMELOT_ID) {
            address[] memory path = new address[](2);
            path[0] = hop.tokenIn;
            path[1] = hop.tokenOut;
            ICamelotRouter camelotRouter = ICamelotRouter(CAMELOT_V2_ROUTER);
            uint256 beforeBalance = IERC20(hop.tokenOut).balanceOf(address(this));
            // euck, this interface doesn't return amountOut
            camelotRouter.swapExactTokensForTokensSupportingFeeOnTransferTokens(
                amountIn,
                0,
                path,
                recipient,
                address(0), // referrer
                block.timestamp
            );
            return IERC20(hop.tokenOut).balanceOf(address(this)) - beforeBalance;
        } else if (hop.exchangeId == CHRONOS_ID || hop.exchangeId == CHRONOS_STABLE_ID) {
            // ALG-001: Chronos V2 - Solidly-style volatile (false) or stable (true) pools
            bool stable = (hop.exchangeId == CHRONOS_STABLE_ID);
            IChronosRouter router = IChronosRouter(CHRONOS_ROUTER);
            uint256[] memory amounts = router.swapExactTokensForTokensSimple(
                amountIn, 0, hop.tokenIn, hop.tokenOut, stable, recipient, block.timestamp
            );
            // amounts [[in][out]]
            return amounts[1];
        } else if (hop.exchangeId == UNISWAP_V2_ID) {
            address[] memory path = new address[](2);
            path[0] = hop.tokenIn;
            path[1] = hop.tokenOut;
            IUniswapV2Router02 router = IUniswapV2Router02(UNISWAP_V2_ROUTER);
            uint256[] memory amounts = router.swapExactTokensForTokens(amountIn, 0, path, recipient, block.timestamp);
            return amounts[1];
        } else if (hop.exchangeId == SUSHI_ID) {
            address[] memory path = new address[](2);
            path[0] = hop.tokenIn;
            path[1] = hop.tokenOut;
            // uniswap v2 ABI
            IUniswapV2Router02 router = IUniswapV2Router02(SUSHI_ROUTER);
            uint256[] memory amounts = router.swapExactTokensForTokens(amountIn, 0, path, recipient, block.timestamp);
            // amounts [[in][out]]
            return amounts[1];
        }
        // === NEW DEX SUPPORT (Phase 3) ===
        else if (hop.exchangeId == RAMSES_ID || hop.exchangeId == RAMSES_STABLE_ID) {
            // ALG-001: Ramses V2 - Solidly-style volatile (false) or stable (true) pools
            bool stable = (hop.exchangeId == RAMSES_STABLE_ID);
            IRamsesRouter router = IRamsesRouter(RAMSES_V2_ROUTER);
            uint256[] memory amounts = router.swapExactTokensForTokensSimple(
                amountIn, 0, hop.tokenIn, hop.tokenOut, stable, recipient, block.timestamp
            );
            return amounts[1];
        } else if (hop.exchangeId == RAMSES_V2_NEW_ID || hop.exchangeId == RAMSES_V2_NEW_STABLE_ID) {
            // M2: Ramses V2 new factory - same Solidly interface, different router
            bool stable = (hop.exchangeId == RAMSES_V2_NEW_STABLE_ID);
            IRamsesRouter router = IRamsesRouter(RAMSES_V2_NEW_ROUTER);
            uint256[] memory amounts = router.swapExactTokensForTokensSimple(
                amountIn, 0, hop.tokenIn, hop.tokenOut, stable, recipient, block.timestamp
            );
            return amounts[1];
        } else if (hop.exchangeId == RAMSES_CL_ID) {
            // M2: Ramses CL (V3 fork) - same exactInputSingle interface as Uniswap V3
            bool zeroForOne = hop.tokenIn < hop.tokenOut;
            uint160 sqrtPriceLimitX96 = zeroForOne ? MIN_SQRT_RATIO + 1 : MAX_SQRT_RATIO - 1;
            ISwapRouter.ExactInputSingleParams memory params = ISwapRouter.ExactInputSingleParams({
                tokenIn: hop.tokenIn,
                tokenOut: hop.tokenOut,
                fee: hop.fee,
                recipient: recipient,
                amountIn: amountIn,
                amountOutMinimum: 0,
                sqrtPriceLimitX96: sqrtPriceLimitX96
            });
            return ISwapRouter(RAMSES_CL_ROUTER).exactInputSingle(params);
        } else if (hop.exchangeId == PANCAKE_V3_ID) {
            // PancakeSwap V3 - same as Uniswap V3
            bool zeroForOne = hop.tokenIn < hop.tokenOut;
            uint160 sqrtPriceLimitX96 = zeroForOne ? MIN_SQRT_RATIO + 1 : MAX_SQRT_RATIO - 1;
            ISwapRouter.ExactInputSingleParams memory params = ISwapRouter.ExactInputSingleParams({
                tokenIn: hop.tokenIn,
                tokenOut: hop.tokenOut,
                fee: hop.fee,
                recipient: recipient,
                amountIn: amountIn,
                amountOutMinimum: 0,
                sqrtPriceLimitX96: sqrtPriceLimitX96
            });
            ISwapRouter pancakeV3Router = ISwapRouter(PANCAKE_V3_ROUTER);
            return pancakeV3Router.exactInputSingle(params);
        } else if (hop.exchangeId == ARBIDEX_ID) {
            // Arbidex - V2-style router (Uniswap V2 ABI)
            address[] memory path = new address[](2);
            path[0] = hop.tokenIn;
            path[1] = hop.tokenOut;
            IUniswapV2Router02 router = IUniswapV2Router02(ARBIDEX_ROUTER);
            uint256[] memory amounts = router.swapExactTokensForTokens(amountIn, 0, path, recipient, block.timestamp);
            return amounts[1];
        } else if (hop.exchangeId == CAMELOT_V3_ID) {
            // ALG-001: Camelot V3 (Algebra) - Use router for mid-leg swaps to avoid callback complexity
            // Direct pool.swap() with empty data causes callback failures for non-flash-swap legs
            IAlgebraSwapRouter.ExactInputSingleParams memory params = IAlgebraSwapRouter.ExactInputSingleParams({
                tokenIn: hop.tokenIn,
                tokenOut: hop.tokenOut,
                recipient: recipient,
                deadline: block.timestamp,
                amountIn: amountIn,
                amountOutMinimum: 0,
                limitSqrtPrice: hop.tokenIn < hop.tokenOut ? MIN_SQRT_RATIO + 1 : MAX_SQRT_RATIO - 1
            });
            return IAlgebraSwapRouter(CAMELOT_V3_ROUTER).exactInputSingle(params);
        } else if (hop.exchangeId == KYBER_ID) {
            // Kyber Elastic — fee from encoded path identifies the specific pool
            IKyberRouter.ExactInputSingleParams memory params = IKyberRouter.ExactInputSingleParams({
                tokenIn: hop.tokenIn,
                tokenOut: hop.tokenOut,
                fee: uint24(hop.fee), // Kyber swapFeeUnits (ppm) from encoded path
                recipient: recipient,
                deadline: block.timestamp,
                amountIn: amountIn,
                minAmountOut: 0,
                limitSqrtP: hop.tokenIn < hop.tokenOut ? MIN_SQRT_RATIO + 1 : MAX_SQRT_RATIO - 1
            });
            return IKyberRouter(KYBER_ROUTER).swapExactInputSingle(params);
        } else if (hop.exchangeId == FLUID_DEX_ID) {
            return _swapExactInFluid(hop.tokenIn, hop.tokenOut, hop.pool, amountIn);
        } else if (_isCurveExchange(hop.exchangeId)) {
            uint256 beforeBalance = IERC20(hop.tokenOut).balanceOf(address(this));
            OZIERC20(hop.tokenIn).forceApprove(hop.pool, amountIn);
            uint256 reportedOut;
            if (hop.exchangeId == CURVE_STABLESWAP_PLAIN_NG_ID) {
                reportedOut = ICurveStableSwapPlainNg(hop.pool)
                    .exchange(int128(uint128(hop.coinInIdx)), int128(uint128(hop.coinOutIdx)), amountIn, 0);
            } else if (hop.exchangeId == CURVE_STABLESWAP_META_NG_ID) {
                reportedOut = ICurveStableSwapMetaNg(hop.pool)
                    .exchange_underlying(int128(uint128(hop.coinInIdx)), int128(uint128(hop.coinOutIdx)), amountIn, 0);
            } else {
                reportedOut =
                    ICurveCryptoSwap(hop.pool).exchange(uint256(hop.coinInIdx), uint256(hop.coinOutIdx), amountIn, 0);
            }
            uint256 actualOut = IERC20(hop.tokenOut).balanceOf(address(this)) - beforeBalance;
            return actualOut > 0 ? actualOut : reportedOut;
        } else if (hop.exchangeId == TRADER_JOE_LB_ID) {
            // Trader Joe LB — bin-based AMM using Path struct
            // fee field uses discovery encoding: ((version-2) << 14) | binStep
            // Supports binStep 0–16383 and versions 2–5.
            uint256 binStep = uint256(hop.fee & 0x3FFF);
            uint8 lbVersion = uint8(((hop.fee >> 14) & 0x3) + 2);
            IERC20[] memory tokenPath = new IERC20[](2);
            tokenPath[0] = IERC20(hop.tokenIn);
            tokenPath[1] = IERC20(hop.tokenOut);
            uint256[] memory pairBinSteps = new uint256[](1);
            pairBinSteps[0] = binStep;
            ILBRouter.Version[] memory versions = new ILBRouter.Version[](1);
            versions[0] = ILBRouter.Version(lbVersion);
            ILBRouter.Path memory path =
                ILBRouter.Path({pairBinSteps: pairBinSteps, versions: versions, tokenPath: tokenPath});
            address lbRouter = lbVersion >= 3 ? TRADER_JOE_LB_V22_ROUTER : TRADER_JOE_LB_ROUTER;
            return ILBRouter(lbRouter).swapExactTokensForTokens(amountIn, 0, path, recipient, block.timestamp);
        } else {
            revert("Unknown exchange ID");
        }
    }

    function unlockCallback(bytes calldata data) external returns (bytes memory) {
        require(msg.sender == UNISWAP_V4_POOL_MANAGER, "Invalid v4 callback");

        (TradeHopV2 memory hop, uint256 amountIn) = abi.decode(data, (TradeHopV2, uint256));
        require(hop.exchangeId == UNISWAP_V4_ID, "Bad v4 hop");
        require(hop.poolManager == msg.sender, "Pool manager mismatch");
        require(hop.hooks == address(0), "Hooks unsupported");

        V4PoolKey memory key = _v4PoolKey(hop);
        require(_v4PoolId(key) == hop.poolId, "PoolId mismatch");

        bool zeroForOne = hop.tokenIn < hop.tokenOut;
        V4SwapParams memory params = V4SwapParams({
            zeroForOne: zeroForOne,
            amountSpecified: -int256(amountIn),
            sqrtPriceLimitX96: zeroForOne ? MIN_SQRT_RATIO + 1 : MAX_SQRT_RATIO - 1
        });

        BalanceDelta delta = IPoolManagerV4(msg.sender).swap(key, params, new bytes(0));
        int128 delta0 = delta.amount0();
        int128 delta1 = delta.amount1();

        if (delta0 < 0) {
            _settleV4Debt(IPoolManagerV4(msg.sender), key.currency0, uint256(uint128(-delta0)));
        } else if (delta0 > 0) {
            IPoolManagerV4(msg.sender).take(key.currency0, address(this), uint256(uint128(delta0)));
        }

        if (delta1 < 0) {
            _settleV4Debt(IPoolManagerV4(msg.sender), key.currency1, uint256(uint128(-delta1)));
        } else if (delta1 > 0) {
            IPoolManagerV4(msg.sender).take(key.currency1, address(this), uint256(uint128(delta1)));
        }

        uint256 outputAmount = zeroForOne ? uint256(uint128(delta1)) : uint256(uint128(delta0));
        return abi.encode(outputAmount);
    }

    /// @dev Payback the flash swap loan and send profits to 'payee'
    /// @notice Includes SEC-003 slippage protection check against _minNetProfit
    function payback(uint256 loanAmount, uint256 earnedAmount, address loanToken) private {
        if (earnedAmount <= loanAmount) revert Loss(loanAmount - earnedAmount);

        uint256 profit = earnedAmount - loanAmount;

        // SEC-003: Check minimum profit threshold after modeled costs.
        uint256 minProfit = _minNetProfit;
        _minNetProfit = 0; // Clear for next trade (fail-safe)
        if (profit < minProfit) revert InsufficientProfit(profit, minProfit);

        OZIERC20(loanToken).safeTransfer(msg.sender, loanAmount);
        OZIERC20(loanToken).safeTransfer(payee, profit);
    }
}
