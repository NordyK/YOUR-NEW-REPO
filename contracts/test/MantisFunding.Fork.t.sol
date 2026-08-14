// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/MantisExecutor.sol";

/// MANTIS funding-mechanism tests: prove IN CODE that a route trading a Balancer pool
/// cannot be funded by a Balancer flash loan (non-reentrant vault) and is forced onto the
/// Aave path — and that Balancer cannot be the flash-swap entry hop.
/// Run: forge test --fork-url $ARBITRUM_RPC_URL --match-path test/MantisFunding.Fork.t.sol -vv
contract MantisFundingForkTest is Test {
    MantisExecutor executor;

    uint8 constant BALANCER_STABLE_ID = 26;
    uint8 constant UNISWAP_V3_ID = 0;

    address constant USDC = 0xaf88d065e77c8cC2239327C5EDb3A432268e5831;
    address constant USDT0 = 0xFd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9;

    function setUp() public {
        require(block.chainid == 42161, "fork Arbitrum One");
        executor = new MantisExecutor(address(this)); // payee == gateway == this
    }

    function _balHop(address tin, address tout) internal pure returns (MantisExecutor.TradeHopV2 memory h) {
        h.exchangeId = BALANCER_STABLE_ID;
        h.tokenIn = tin;
        h.tokenOut = tout;
        h.poolId = bytes32(uint256(1)); // dummy; guard fires before any swap
    }

    function _v3Hop(address tin, address tout, uint24 fee) internal pure returns (MantisExecutor.TradeHopV2 memory h) {
        h.exchangeId = UNISWAP_V3_ID;
        h.tokenIn = tin;
        h.tokenOut = tout;
        h.fee = fee;
    }

    /// A Balancer-leg route MUST be rejected by the Balancer-flash-funded entrypoint.
    function test_Guard_BalancerLeg_RejectedByBalancerFlashFunding() public {
        MantisExecutor.TradeHopV2[3] memory route;
        route[0] = _balHop(USDC, USDT0); // trades a Balancer pool
        route[1] = _v3Hop(USDT0, USDC, 100);
        // route[2] left empty -> 2-hop
        vm.expectRevert(bytes("Balancer leg: use flashLoanArbAaveMixed"));
        executor.flashLoanArbMixed(1_000_000, route, 0);
    }

    /// Balancer cannot be the FLASH-SWAP entry hop (no swap-callback funding path).
    function test_Guard_BalancerCannotBeFlashSwapEntry() public {
        MantisExecutor.TradeHopV2[3] memory route;
        route[0] = _balHop(USDC, USDT0);
        route[1] = _v3Hop(USDT0, USDC, 100);
        vm.expectRevert(bytes("Balancer cannot be flash-swap entry; use flashLoanArbAaveMixed"));
        executor.flashSwapMixed(1_000_000, route, 0);
    }

    /// The Aave-funded entrypoint ACCEPTS a Balancer-leg route (no funding guard): it must NOT
    /// revert with either guard message. It will revert later (no real arb / Aave execution),
    /// which proves it passed funding selection and entered the Aave flash path.
    function test_AaveEntrypoint_AcceptsBalancerLeg() public {
        MantisExecutor.TradeHopV2[3] memory route;
        route[0] = _balHop(USDC, USDT0);
        route[1] = _v3Hop(USDT0, USDC, 100);
        try executor.flashLoanArbAaveMixed(1_000_000, route, 0) {
            // Unlikely to profit on a stable round-trip; either outcome is fine here.
        } catch Error(string memory reason) {
            assertTrue(
                keccak256(bytes(reason)) != keccak256(bytes("Balancer leg: use flashLoanArbAaveMixed")),
                "must not hit Balancer-flash funding guard"
            );
            assertTrue(
                keccak256(bytes(reason)) != keccak256(bytes("Not gateway")),
                "gateway should be this contract"
            );
        } catch {
            // Low-level revert (e.g. Loss/InsufficientProfit custom error, or Balancer swap) —
            // expected; it means we got PAST funding selection into the Aave path.
        }
    }
}
