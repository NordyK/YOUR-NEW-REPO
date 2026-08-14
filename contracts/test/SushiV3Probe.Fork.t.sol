// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

/// Isolate the SushiV3 failure: call Sushi's V3 SwapRouter exactInputSingle directly with
/// dealt funds, at varying sizes, to tell "router/ABI bug" from "thin pool at this block".
interface IERC20m { function approve(address,uint256) external returns (bool); function balanceOf(address) external view returns (uint256); }
interface IV3Pool { function token0() external view returns (address); function token1() external view returns (address); function fee() external view returns (uint24); function liquidity() external view returns (uint128); function slot0() external view returns (uint160,int24,uint16,uint16,uint16,uint8,bool); }
interface ISushiRouter {
    struct ExactInputSingleParams { address tokenIn; address tokenOut; uint24 fee; address recipient; uint256 amountIn; uint256 amountOutMinimum; uint160 sqrtPriceLimitX96; }
    function exactInputSingle(ExactInputSingleParams calldata p) external payable returns (uint256);
}

interface ISushiRouterDeadline {
    struct ExactInputSingleParams { address tokenIn; address tokenOut; uint24 fee; address recipient; uint256 deadline; uint256 amountIn; uint256 amountOutMinimum; uint160 sqrtPriceLimitX96; }
    function exactInputSingle(ExactInputSingleParams calldata p) external payable returns (uint256);
}

contract SushiV3ProbeForkTest is Test {
    address constant SUSHI_ROUTER = 0x8A21F6768C1f8075791D08546Dadf6daA0bE820c;
    address constant POOL = 0xf3Eb87C1F6020982173C908E7eB31aA66c1f0296; // SushiV3 WETH/USDC
    address constant WETH = 0x82aF49447D8a07e3bd95BD0d56f35241523fBab1;
    address constant USDC = 0xaf88d065e77c8cC2239327C5EDb3A432268e5831;
    uint160 constant MIN_SQRT = 4295128739;
    uint160 constant MAX_SQRT = 1461446703485210103287273052203988822378723970342;

    function test_SushiV3_DirectSwap_Diagnostics() public {
        emit log_named_address("pool token0", IV3Pool(POOL).token0());
        emit log_named_address("pool token1", IV3Pool(POOL).token1());
        emit log_named_uint("pool fee", IV3Pool(POOL).fee());
        emit log_named_uint("pool active liquidity", IV3Pool(POOL).liquidity());
        (,,,,,, bool unlocked) = IV3Pool(POOL).slot0();
        emit log_named_string("pool unlocked", unlocked ? "true" : "false");

        uint24 fee = IV3Pool(POOL).fee();
        // Try USDC->WETH at several sizes
        uint256[3] memory amts = [uint256(1e6), 10e6, 100e6]; // 1, 10, 100 USDC
        for (uint256 i = 0; i < 3; i++) {
            deal(USDC, address(this), amts[i]);
            IERC20m(USDC).approve(SUSHI_ROUTER, amts[i]);
            // USDC->WETH: USDC>WETH so zeroForOne=false (oneForZero) -> limit = MAX-1
            try ISushiRouter(SUSHI_ROUTER).exactInputSingle(ISushiRouter.ExactInputSingleParams({
                tokenIn: USDC, tokenOut: WETH, fee: fee, recipient: address(this),
                amountIn: amts[i], amountOutMinimum: 0, sqrtPriceLimitX96: MAX_SQRT - 1
            })) returns (uint256 out) {
                emit log_string(string.concat("USDC->WETH ", vm.toString(amts[i]), " => out=", vm.toString(out), " OK"));
            } catch (bytes memory d) {
                bytes4 s; assembly { s := mload(add(d,0x20)) }
                emit log_string(string.concat("no-deadline USDC->WETH ", vm.toString(amts[i]), " => REVERT len=", vm.toString(d.length), " sel=", vm.toString(s)));
            }
            // Try the DEADLINE variant (original UniV3 SwapRouter ABI)
            deal(USDC, address(this), amts[i]);
            IERC20m(USDC).approve(SUSHI_ROUTER, amts[i]);
            try ISushiRouterDeadline(SUSHI_ROUTER).exactInputSingle(ISushiRouterDeadline.ExactInputSingleParams({
                tokenIn: USDC, tokenOut: WETH, fee: fee, recipient: address(this),
                deadline: block.timestamp + 600, amountIn: amts[i], amountOutMinimum: 0, sqrtPriceLimitX96: MAX_SQRT - 1
            })) returns (uint256 out2) {
                emit log_string(string.concat("DEADLINE  USDC->WETH ", vm.toString(amts[i]), " => out=", vm.toString(out2), " OK"));
            } catch (bytes memory d2) {
                bytes4 s2; assembly { s2 := mload(add(d2,0x20)) }
                emit log_string(string.concat("DEADLINE  USDC->WETH ", vm.toString(amts[i]), " => REVERT len=", vm.toString(d2.length), " sel=", vm.toString(s2)));
            }
        }
    }
}
