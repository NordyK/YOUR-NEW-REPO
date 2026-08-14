// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;
pragma abicoder v2;

// all the external interfaces and libs required by 'TiggerExecutor'

/// @title Provides functions for deriving a pool address from the factory, tokens, and the fee
library PoolAddress {
    bytes32 internal constant POOL_INIT_CODE_HASH = 0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54;

    /// @notice The identifying key of the pool
    struct PoolKey {
        address token0;
        address token1;
        uint24 fee;
    }

    /// @notice Returns PoolKey: the ordered tokens with the matched fee levels
    /// @param tokenA The first token of a pool, unsorted
    /// @param tokenB The second token of a pool, unsorted
    /// @param fee The fee level of the pool
    /// @return Poolkey The pool details with ordered token0 and token1 assignments
    function getPoolKey(address tokenA, address tokenB, uint24 fee) internal pure returns (PoolKey memory) {
        if (tokenA > tokenB) (tokenA, tokenB) = (tokenB, tokenA);
        return PoolKey({token0: tokenA, token1: tokenB, fee: fee});
    }

    /// @notice Deterministically computes the pool address given the factory and PoolKey
    /// @param factory The Uniswap V3 factory contract address
    /// @param key The PoolKey
    /// @return pool The contract address of the V3 pool
    function computeAddress(address factory, PoolKey memory key) internal pure returns (address pool) {
        require(key.token0 < key.token1);
        pool = address(
            uint160(
                uint256(
                    keccak256(
                        abi.encodePacked(
                            hex"ff",
                            factory,
                            keccak256(abi.encode(key.token0, key.token1, key.fee)),
                            POOL_INIT_CODE_HASH
                        )
                    )
                )
            )
        );
    }
}

library UniswapV2Math {
    // given an input amount of an asset and pair reserves, returns the maximum output amount of the other asset
    function getAmountOut(uint256 amountIn, uint256 reserveIn, uint256 reserveOut)
        internal
        pure
        returns (uint256 amountOut)
    {
        require(amountIn > 0, "UniswapV2Library: INSUFFICIENT_INPUT_AMOUNT");
        uint256 amountInWithFee = amountIn * 997;
        uint256 numerator = amountInWithFee * reserveOut;
        uint256 denominator = (reserveIn * 1000) + amountInWithFee;
        return amountOut = numerator / denominator;
    }
    // given an output amount of an asset and pair reserves, returns the minimum input amount of the other asset

    function getAmountIn(uint256 amountOut, uint256 reserveIn, uint256 reserveOut)
        internal
        pure
        returns (uint256 amountIn)
    {
        require(amountOut > 0, "UniswapV2Library: INSUFFICIENT_OUTPUT_AMOUNT");
        uint256 numerator = reserveIn * amountOut * 1000;
        uint256 denominator = (reserveOut - amountOut) * 997;
        return amountIn = (numerator / denominator) + 1;
    }
}

interface IUniswapV2Callee {
    function uniswapV2Call(address sender, uint256 amount0, uint256 amount1, bytes calldata data) external;
}

interface IUniswapV2Pair {
    function getReserves() external view returns (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast);
    function swap(uint256 amount0Out, uint256 amount1Out, address to, bytes calldata data) external;
}

interface IUniswapV2Factory {
    function getPair(address tokenA, address tokenB) external view returns (address pair);
}

interface IUniswapV3SwapCallback {
    /// @notice Called to `msg.sender` after executing a swap via IUniswapV3Pool#swap.
    /// @dev In the implementation you must pay the pool tokens owed for the swap.
    /// The caller of this method must be checked to be a UniswapV3Pool deployed by the canonical UniswapV3Factory.
    /// amount0Delta and amount1Delta can both be 0 if no tokens were swapped.
    /// @param amount0Delta The amount of token0 that was sent (negative) or must be received (positive) by the pool by
    /// the end of the swap. If positive, the callback must send that amount of token0 to the pool.
    /// @param amount1Delta The amount of token1 that was sent (negative) or must be received (positive) by the pool by
    /// the end of the swap. If positive, the callback must send that amount of token1 to the pool.
    /// @param data Any data passed through by the caller via the IUniswapV3PoolActions#swap call
    function uniswapV3SwapCallback(int256 amount0Delta, int256 amount1Delta, bytes calldata data) external;
}

interface IUnlockCallback {
    function unlockCallback(bytes calldata data) external returns (bytes memory);
}

interface IHooks {}

type Currency is address;
type BalanceDelta is int256;

library BalanceDeltaLibrary {
    function amount0(BalanceDelta delta) internal pure returns (int128 amount) {
        amount = int128(BalanceDelta.unwrap(delta) >> 128);
    }

    function amount1(BalanceDelta delta) internal pure returns (int128 amount) {
        amount = int128(BalanceDelta.unwrap(delta));
    }
}

struct V4PoolKey {
    Currency currency0;
    Currency currency1;
    uint24 fee;
    int24 tickSpacing;
    IHooks hooks;
}

struct V4SwapParams {
    bool zeroForOne;
    int256 amountSpecified;
    uint160 sqrtPriceLimitX96;
}

interface IPoolManagerV4 {
    function unlock(bytes calldata data) external returns (bytes memory result);
    function swap(V4PoolKey memory key, V4SwapParams memory params, bytes memory hookData)
        external
        returns (BalanceDelta delta);
    function sync(Currency currency) external;
    function settle() external payable returns (uint256 paid);
    function take(Currency currency, address to, uint256 amount) external;
}

interface IUniswapV2Router02 {
    function factory() external pure returns (address);

    function swapExactTokensForTokens(
        uint256 amountIn,
        uint256 amountOutMin,
        address[] calldata path,
        address to,
        uint256 deadline
    ) external returns (uint256[] memory amounts);
}

interface IERC20 {
    function balanceOf(address account) external view returns (uint256);
    function transfer(address recipient, uint256 amount) external returns (bool);
    function allowance(address owner, address spender) external view returns (uint256);
    function approve(address spender, uint256 amount) external returns (bool);
    function transferFrom(address sender, address recipient, uint256 amount) external returns (bool);
}

interface IUniswapV3Pool {
    function liquidity() external view returns (uint128);
    function slot0()
        external
        view
        returns (
            uint160 sqrtPriceX96,
            int24 tick,
            uint16 observationIndex,
            uint16 observationCardinality,
            uint16 observationCardinalityNext,
            uint8 feeProtocol,
            bool unlocked
        );
    function swap(
        address recipient,
        bool zeroForOne,
        int256 amountSpecified,
        uint160 sqrtPriceLimitX96,
        bytes calldata data
    ) external returns (int256 amount0, int256 amount1);
    function flash(address recipient, uint256 amount0, uint256 amount1, bytes calldata data) external;
}

interface ISwapRouter {
    struct ExactInputSingleParams {
        address tokenIn;
        address tokenOut;
        uint24 fee;
        address recipient;
        uint256 amountIn;
        uint256 amountOutMinimum;
        uint160 sqrtPriceLimitX96;
    }

    function exactInputSingle(ExactInputSingleParams calldata params) external payable returns (uint256 amountOut);
}

interface ICamelotRouter {
    function swapExactTokensForTokensSupportingFeeOnTransferTokens(
        uint256 amountIn,
        uint256 amountOutMin,
        address[] calldata path,
        address to,
        address referrer,
        uint256 deadline
    ) external;
}

interface IChronosRouter {
    function swapExactTokensForTokensSimple(
        uint256 amountIn,
        uint256 amountOutMin,
        address tokenFrom,
        address tokenTo,
        bool stable,
        address to,
        uint256 deadline
    ) external returns (uint256[] memory amounts);
}

interface IChronosFactory {
    function getPair(address tokenA, address tokenB, bool stable) external view returns (address);
}

// === NEW DEX INTERFACES (Phase 3) ===

/// @title PancakeSwap V3 callback interface
/// @notice PancakeSwap V3 uses a different callback function name than Uniswap V3
interface IPancakeV3SwapCallback {
    function pancakeV3SwapCallback(int256 amount0Delta, int256 amount1Delta, bytes calldata data) external;
}

/// @title Algebra swap callback interface
/// @notice Used by Camelot V3 and Kyber Elastic (both are Algebra-based)
interface IAlgebraSwapCallback {
    function algebraSwapCallback(int256 amount0Delta, int256 amount1Delta, bytes calldata data) external;
}

/// @title Algebra pool interface
/// @notice Algebra pools don't use fee tier in CREATE2 salt (dynamic fee model)
interface IAlgebraPool {
    function swap(address recipient, bool zeroToOne, int256 amountRequired, uint160 limitSqrtPrice, bytes calldata data)
        external
        returns (int256 amount0, int256 amount1);

    function liquidity() external view returns (uint128);

    function globalState()
        external
        view
        returns (
            uint160 price,
            int24 tick,
            uint16 fee,
            uint16 timepointIndex,
            uint8 communityFeeToken0,
            uint8 communityFeeToken1,
            bool unlocked
        );
}

/// @title Algebra factory interface
/// @notice Used for callback verification via poolByPair lookup
interface IAlgebraFactory {
    function poolByPair(address tokenA, address tokenB) external view returns (address pool);
}

/// @title Kyber Elastic factory interface
/// @notice Kyber uses getPool(tokenA, tokenB, swapFeeUnits) — NOT poolByPair
interface IKyberElasticFactory {
    function getPool(address tokenA, address tokenB, uint24 swapFeeUnits) external view returns (address pool);
}

/// @title Algebra SwapRouter interface (Camelot V3, Kyber Elastic)
/// @notice ALG-001: Used for mid-leg swaps to avoid callback complexity
/// @dev Unlike Uniswap V3, Algebra uses dynamic fees so no fee parameter is needed
interface IAlgebraSwapRouter {
    struct ExactInputSingleParams {
        address tokenIn;
        address tokenOut;
        address recipient;
        uint256 deadline;
        uint256 amountIn;
        uint256 amountOutMinimum;
        uint160 limitSqrtPrice;
    }

    function exactInputSingle(ExactInputSingleParams calldata params) external payable returns (uint256 amountOut);
}

/// @title Ramses V2 factory interface (Solidly-style, same as Chronos)
interface IRamsesFactory {
    function getPair(address tokenA, address tokenB, bool stable) external view returns (address);
}

/// @title Ramses V3 (CL) factory interface — uses tickSpacing instead of fee
interface IRamsesV3Factory {
    function getPool(address tokenA, address tokenB, int24 tickSpacing) external view returns (address);
}

/// @title Ramses V2 router interface
interface IRamsesRouter {
    function swapExactTokensForTokensSimple(
        uint256 amountIn,
        uint256 amountOutMin,
        address tokenFrom,
        address tokenTo,
        bool stable,
        address to,
        uint256 deadline
    ) external returns (uint256[] memory amounts);
}

/// @title PancakeSwap V3 pool interface (same as Uniswap V3)
interface IPancakeV3Pool {
    function swap(
        address recipient,
        bool zeroForOne,
        int256 amountSpecified,
        uint160 sqrtPriceLimitX96,
        bytes calldata data
    ) external returns (int256 amount0, int256 amount1);
}

/// @title Kyber Elastic router interface
interface IKyberRouter {
    struct ExactInputSingleParams {
        address tokenIn;
        address tokenOut;
        uint24 fee;
        address recipient;
        uint256 deadline;
        uint256 amountIn;
        uint256 minAmountOut;
        uint160 limitSqrtP;
    }

    function swapExactInputSingle(ExactInputSingleParams calldata params) external payable returns (uint256 amountOut);
}

// === BALANCER V2 FLASH LOAN INTERFACES ===

/// @title Balancer V2 Vault interface for flash loans
/// @notice The Vault holds all tokens for all Balancer pools, enabling 0% fee flash loans
interface IBalancerVault {
    /// @notice Execute a flash loan from the Balancer Vault
    /// @param recipient Contract that will receive the tokens and handle the callback
    /// @param tokens Array of token addresses to borrow
    /// @param amounts Array of amounts to borrow for each token
    /// @param userData Arbitrary data passed to the recipient's receiveFlashLoan callback
    function flashLoan(
        IFlashLoanRecipient recipient,
        IERC20[] memory tokens,
        uint256[] memory amounts,
        bytes memory userData
    ) external;
}

/// @title Interface for contracts receiving Balancer flash loans
/// @notice Implement this interface to receive flash loans from Balancer V2 Vault
interface IFlashLoanRecipient {
    /// @notice Called by the Vault when flash loan is initiated
    /// @dev Must repay borrowed amounts + fees before returning
    /// @param tokens Array of borrowed token addresses
    /// @param amounts Array of borrowed amounts
    /// @param feeAmounts Array of fees owed (0% for Balancer V2)
    /// @param userData Arbitrary data from flashLoan call
    function receiveFlashLoan(
        IERC20[] memory tokens,
        uint256[] memory amounts,
        uint256[] memory feeAmounts,
        bytes memory userData
    ) external;
}

/// @title Trader Joe LB V2.1 Router interface
/// @notice Bin-based AMM with Path struct for multi-hop swaps
interface ILBRouter {
    enum Version {
        V1,
        V2,
        V2_1,
        V2_2
    }

    struct Path {
        uint256[] pairBinSteps;
        Version[] versions;
        IERC20[] tokenPath;
    }

    function swapExactTokensForTokens(
        uint256 amountIn,
        uint256 amountOutMin,
        Path memory path,
        address to,
        uint256 deadline
    ) external returns (uint256 amountOut);
}
