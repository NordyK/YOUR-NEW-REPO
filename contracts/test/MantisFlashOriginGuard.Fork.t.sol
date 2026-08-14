// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/MantisExecutor.sol";

/// SEC-FLASH-ORIGIN regression: an attacker can call `BalancerVault.flashLoan(recipient=Mantis,
/// userData=craftedRoute)`, which makes the Vault invoke `receiveFlashLoan` with
/// `msg.sender == VAULT` — passing the only prior auth check. Before the transient flash-origin
/// guard, the crafted route (with attacker-controlled exotic-venue `pool` addresses) would execute
/// against the contract's blanket token approvals and could drain held balances. The guard requires
/// a gateway-authorized flash to be in progress (a transient flag only a `msg.sender == gateway`
/// entrypoint sets), so an unsolicited callback now reverts `UnsolicitedFlashCallback`.
///
/// Fork test (deploying MantisExecutor runs its constructor's real token→router approvals, which
/// need live token code). Run: `forge test --fork-url $ARBITRUM_RPC_URL --match-path
/// test/MantisFlashOriginGuard.Fork.t.sol -vv`.
contract MantisFlashOriginGuardForkTest is Test {
    address private constant BALANCER_VAULT = 0xBA12222222228d8Ba445958a75a0704d566BF2C8;
    address private constant WETH = 0x82aF49447D8a07e3bd95BD0d56f35241523fBab1;

    MantisExecutor private mantis;
    address private payee = address(0xBEEF);

    function setUp() public {
        mantis = new MantisExecutor(payee);
    }

    /// A direct `receiveFlashLoan` from the Vault, with NO gateway-initiated flash in progress,
    /// must revert via the transient guard — not execute the attacker's userData.
    function test_UnsolicitedReceiveFlashLoanReverts() public {
        IERC20[] memory tokens = new IERC20[](1);
        tokens[0] = IERC20(WETH);
        uint256[] memory amounts = new uint256[](1);
        amounts[0] = 1 ether;
        uint256[] memory fees = new uint256[](1);
        fees[0] = 0;
        // Any userData — the guard reverts before it is decoded.
        bytes memory crafted = abi.encode(uint256(0xdead), uint128(1 ether));

        vm.prank(BALANCER_VAULT);
        vm.expectRevert(MantisExecutor.UnsolicitedFlashCallback.selector);
        mantis.receiveFlashLoan(tokens, amounts, fees, crafted);
    }

    /// Even the correct Vault caller cannot drive the callback without the guard set — the auth
    /// (`msg.sender == VAULT`) is necessary but no longer sufficient.
    function test_VaultCallerStillBlockedWithoutGatewayFlash() public {
        IERC20[] memory tokens = new IERC20[](1);
        tokens[0] = IERC20(WETH);
        uint256[] memory amounts = new uint256[](1);
        amounts[0] = 1 ether;
        uint256[] memory fees = new uint256[](1);
        fees[0] = 0;

        vm.prank(BALANCER_VAULT);
        vm.expectRevert(MantisExecutor.UnsolicitedFlashCallback.selector);
        mantis.receiveFlashLoan(tokens, amounts, fees, bytes(""));
    }

    /// The DEX SWAP callbacks now also require the flash-origin guard. The sharpest vector is
    /// `uniswapV2Call`: a V2 pair calls back the `to` RECIPIENT, which an attacker can set to this
    /// contract via `pair.swap(amountOut, 0, to=mantis, data=craftedRoute)` (passing the real-pair
    /// `_verifyV2Callback`). With no gateway-initiated swap in progress the transient guard reverts
    /// FIRST — before any route decode — so a crafted route can never run. The V3-family callbacks
    /// (called back the swap initiator, not the recipient) carry the same guard as defense-in-depth.
    function test_UnsolicitedSwapCallbacksRevert() public {
        bytes memory crafted = abi.encode(uint128(0xdead), uint128(1 ether));
        vm.expectRevert(MantisExecutor.UnsolicitedFlashCallback.selector);
        mantis.uniswapV2Call(address(0xA77AC), 1 ether, 0, crafted);
        vm.expectRevert(MantisExecutor.UnsolicitedFlashCallback.selector);
        mantis.uniswapV3SwapCallback(int256(1 ether), int256(-1 ether), crafted);
        vm.expectRevert(MantisExecutor.UnsolicitedFlashCallback.selector);
        mantis.pancakeV3SwapCallback(int256(1 ether), int256(-1 ether), crafted);
        vm.expectRevert(MantisExecutor.UnsolicitedFlashCallback.selector);
        mantis.algebraSwapCallback(int256(1 ether), int256(-1 ether), crafted);
    }
}
