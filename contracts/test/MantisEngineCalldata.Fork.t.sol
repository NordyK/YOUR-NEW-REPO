// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/MantisExecutor.sol";

/// ACCEPTANCE CRITERION 4: replay ENGINE-BUILT calldata on the Arbitrum fork.
///
/// The bytes in test/fixtures/engine_calldata.json are produced ONLY by the engine's Rust
/// builder (`engine_routes::calldata::encode_mantis_mixed`, via the `emit_mantis_calldata`
/// example) — never hand-written here. This test deploys Mantis, calls it (as the gateway)
/// with each engine-built calldata blob, and asserts each route either lands (profit) or
/// reverts ONLY on no-profit (Loss/InsufficientProfit) — i.e. the engine's ABI encoding
/// decoded correctly and executed through every leg. It also asserts the funding-entrypoint
/// selector matches the route's touches_balancer (Balancer-leg -> Aave; else Balancer/DEX-flash).
///
/// Regenerate the fixture: `cargo run -p engine-routes --example emit_mantis_calldata`.
/// Run: forge test --fork-url $ARBITRUM_RPC_URL --match-path test/MantisEngineCalldata.Fork.t.sol -vv
contract MantisEngineCalldataForkTest is Test {
    MantisExecutor executor;

    // Mirror the executor's canonical reverts so we can classify "no-profit" outcomes.
    error Loss(uint256);
    error InsufficientProfit(uint256 actual, uint256 minimum);

    bytes4 constant MIXED_SEL = 0xfe95fbbd; // flashLoanArbMixed
    bytes4 constant AAVE_SEL = 0x28747b75; // flashLoanArbAaveMixed

    function setUp() public {
        require(block.chainid == 42161, "fork Arbitrum One");
        executor = new MantisExecutor(address(this)); // payee == gateway == this
    }

    function _sel(bytes memory b) internal pure returns (bytes4 s) {
        require(b.length >= 4, "short");
        assembly {
            s := mload(add(b, 0x20))
        }
    }

    function test_EngineBuiltCalldata_ReplaysOnFork() public {
        string memory json = vm.readFile("./test/fixtures/engine_calldata.json");
        string[] memory ids = vm.parseJsonStringArray(json, ".route_ids");
        string[] memory selectors = vm.parseJsonStringArray(json, ".selectors");
        string[] memory fundings = vm.parseJsonStringArray(json, ".fundings");
        bytes[] memory calldatas = vm.parseJsonBytesArray(json, ".calldatas");

        assertEq(ids.length, calldatas.length, "array length mismatch");
        assertGe(ids.length, 5, "need >=5 engine-built routes");

        uint256 mixedCount;
        uint256 aaveCount;
        uint256 executedOk;

        for (uint256 i = 0; i < calldatas.length; i++) {
            bytes memory cd = calldatas[i];
            bytes4 sel = _sel(cd);

            // (1) The funding entrypoint selector must match touches_balancer.
            bool isAave = keccak256(bytes(fundings[i])) == keccak256("aave");
            if (isAave) {
                assertEq(sel, AAVE_SEL, string.concat("route ", ids[i], ": Balancer-leg must use Aave entrypoint"));
                aaveCount++;
            } else {
                assertEq(sel, MIXED_SEL, string.concat("route ", ids[i], ": non-Balancer must use mixed entrypoint"));
                mixedCount++;
            }

            // (2) Replay the engine-built calldata on the fork as the gateway. Acceptable
            //     outcomes: success (profit) OR revert with Loss/InsufficientProfit (no-profit).
            //     Any other revert (empty data / swap error) means a broken encoding -> fail.
            (bool ok, bytes memory ret) = address(executor).call(cd);
            if (ok) {
                executedOk++;
                emit log_string(string.concat("route ", ids[i], ": LANDED (profit) sel=", vm.toString(sel)));
            } else {
                bytes4 rsel = ret.length >= 4 ? _sel(ret) : bytes4(0);
                bool noProfit = rsel == Loss.selector || rsel == InsufficientProfit.selector;
                assertTrue(
                    noProfit,
                    string.concat(
                        "route ", ids[i], ": engine calldata reverted for a NON-no-profit reason (rsel=",
                        vm.toString(rsel), ", retlen=", vm.toString(ret.length), ") -> encoding/mechanical bug"
                    )
                );
                executedOk++;
                emit log_string(string.concat("route ", ids[i], ": executed all legs, no-profit at block (rsel=", vm.toString(rsel), ")"));
            }
        }

        emit log_named_uint("engine-built routes replayed", calldatas.length);
        emit log_named_uint("  mixed (Balancer/DEX-flash) entrypoint", mixedCount);
        emit log_named_uint("  aave (Balancer-leg) entrypoint", aaveCount);
        emit log_named_uint("  executed through all legs (profit or no-profit revert)", executedOk);

        assertGe(mixedCount, 5, "need >=5 catalog routes on the mixed entrypoint");
        assertGe(aaveCount, 1, "need >=1 Balancer-leg route on the Aave entrypoint");
        assertEq(executedOk, calldatas.length, "every engine-built calldata must execute through all legs");
    }
}
