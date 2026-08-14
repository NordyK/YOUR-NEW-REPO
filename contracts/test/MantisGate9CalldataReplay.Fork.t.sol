// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/MantisExecutor.sol";

interface IERC20Gate9Replay {
    function balanceOf(address account) external view returns (uint256);
}

contract Gate9TransientStorageProbe {
    function touch() external {
        assembly {
            tstore(0, 1)
        }
    }
}

/// Replays actual Gate9 bot-emitted calldata on a transaction-positioned Arbitrum fork.
///
/// This is stricter than the ABI smoke test: Loss/InsufficientProfit is not accepted here.
/// A row passes only when the executor call succeeds and transfers positive WETH profit to the
/// payee. The harness first rolls to the trigger transaction and then executes that historical
/// transaction so the arb runs against post-trigger/backrun state.
contract MantisGate9CalldataReplayForkTest is Test {
    bytes4 constant MIXED_3_SEL = 0xfe95fbbd;
    bytes4 constant MIXED_4_SEL = 0x29649abb;
    uint256 constant EXPECTED_BLOCK = 465_686_729;

    error Loss(uint256);
    error InsufficientProfit(uint256 actual, uint256 minimum);

    function setUp() public {
        require(block.chainid == 42161, "fork Arbitrum One");
    }

    function test_ForkRunnerSupportsTransientStorage() public {
        Gate9TransientStorageProbe probe = new Gate9TransientStorageProbe();
        probe.touch();
    }

    function _sel(bytes memory b) internal pure returns (bytes4 s) {
        require(b.length >= 4, "short calldata");
        assembly {
            s := mload(add(b, 0x20))
        }
    }

    function _requireArrayLengths(
        string[] memory ids,
        uint256[] memory routeIds,
        uint256[] memory hopCounts,
        string[] memory txHashes,
        address[] memory measureTokens,
        uint256[] memory expectedDeltas,
        bytes[] memory calldatas
    ) internal pure {
        uint256 n = ids.length;
        require(n == 3, "fixture must cover 2/3/4 hop rows");
        require(routeIds.length == n, "route id length mismatch");
        require(hopCounts.length == n, "hop count length mismatch");
        require(txHashes.length == n, "tx hash length mismatch");
        require(measureTokens.length == n, "measure token length mismatch");
        require(expectedDeltas.length == n, "expected delta length mismatch");
        require(calldatas.length == n, "calldata length mismatch");
    }

    function _assertSelector(uint256 hopCount, bytes4 selector, string memory id) internal pure {
        if (hopCount == 4) {
            assertEq(selector, MIXED_4_SEL, string.concat(id, ": 4-hop row must use flashLoanArbMixed4"));
        } else {
            assertEq(selector, MIXED_3_SEL, string.concat(id, ": 2/3-hop row must use flashLoanArbMixed"));
        }
    }

    function _replayOne(
        string memory id,
        uint256 routeId,
        uint256 hopCount,
        string memory txHashRaw,
        address measureToken,
        uint256 expectedDelta,
        bytes memory cd,
        bool executeTrigger
    ) internal returns (uint256 realizedDelta) {
        bytes4 selector = _sel(cd);
        _assertSelector(hopCount, selector, id);

        bytes32 txHash = vm.parseBytes32(txHashRaw);
        vm.rollFork(txHash);
        assertEq(block.number, EXPECTED_BLOCK, string.concat(id, ": wrong rolled block"));

        if (executeTrigger) {
            vm.transact(txHash);
            assertEq(block.number, EXPECTED_BLOCK, string.concat(id, ": wrong post-transaction block"));
        }

        MantisExecutor executor = new MantisExecutor(address(this));
        uint256 beforeBalance = IERC20Gate9Replay(measureToken).balanceOf(address(this));
        (bool ok, bytes memory ret) = address(executor).call(cd);
        uint256 afterBalance = IERC20Gate9Replay(measureToken).balanceOf(address(this));

        if (!ok) {
            bytes4 revertSelector = ret.length >= 4 ? _sel(ret) : bytes4(0);
            emit log_named_uint("route id", routeId);
            emit log_named_uint("hop count", hopCount);
            emit log_named_uint("execute trigger transaction", executeTrigger ? 1 : 0);
            emit log_named_uint("revert data length", ret.length);
            emit log_named_bytes32("revert selector", bytes32(revertSelector));
            assertTrue(false, string.concat(id, ": Gate9 bot calldata reverted instead of realizing profit"));
        }

        realizedDelta = afterBalance - beforeBalance;
        emit log_string(id);
        emit log_named_uint("route id", routeId);
        emit log_named_uint("hop count", hopCount);
        emit log_named_uint("execute trigger transaction", executeTrigger ? 1 : 0);
        emit log_named_uint("expected REVM WETH delta wei", expectedDelta);
        emit log_named_uint("realized fork WETH delta wei", realizedDelta);

        assertGt(realizedDelta, 0, string.concat(id, ": fork replay did not realize positive WETH"));
        assertGe(realizedDelta, expectedDelta, string.concat(id, ": fork replay realized less WETH than REVM row"));
    }

    function _classifyOne(
        string memory id,
        uint256 routeId,
        uint256 hopCount,
        string memory txHashRaw,
        address measureToken,
        bytes memory cd,
        bool executeTrigger
    ) internal returns (bool ok, uint256 realizedDelta, bytes4 revertSelector, uint256 revertLength) {
        bytes4 selector = _sel(cd);
        _assertSelector(hopCount, selector, id);

        bytes32 txHash = vm.parseBytes32(txHashRaw);
        vm.rollFork(txHash);
        assertEq(block.number, EXPECTED_BLOCK, string.concat(id, ": wrong rolled block"));
        if (executeTrigger) {
            vm.transact(txHash);
            assertEq(block.number, EXPECTED_BLOCK, string.concat(id, ": wrong post-transaction block"));
        }

        MantisExecutor executor = new MantisExecutor(address(this));
        uint256 beforeBalance = IERC20Gate9Replay(measureToken).balanceOf(address(this));
        bytes memory ret;
        (ok, ret) = address(executor).call(cd);
        uint256 afterBalance = IERC20Gate9Replay(measureToken).balanceOf(address(this));
        if (ok && afterBalance >= beforeBalance) realizedDelta = afterBalance - beforeBalance;
        if (!ok) {
            revertLength = ret.length;
            revertSelector = ret.length >= 4 ? _sel(ret) : bytes4(0);
        }

        emit log_string(id);
        emit log_named_uint("route id", routeId);
        emit log_named_uint("hop count", hopCount);
        emit log_named_uint("execute trigger transaction", executeTrigger ? 1 : 0);
        emit log_named_uint("call ok", ok ? 1 : 0);
        emit log_named_uint("realized WETH delta wei", realizedDelta);
        emit log_named_uint("revert data length", revertLength);
        emit log_named_bytes32("revert selector", bytes32(revertSelector));
    }

    function test_Gate9BotCalldata_Weth2Hop3Hop4Hop_RealizesPositiveProfit() public {
        string memory json = vm.readFile("./test/fixtures/gate9_calldata_replay_clean_weth_sample.json");
        string[] memory ids = vm.parseJsonStringArray(json, ".candidate_ids");
        uint256[] memory routeIds = vm.parseJsonUintArray(json, ".route_ids");
        uint256[] memory hopCounts = vm.parseJsonUintArray(json, ".hop_counts");
        string[] memory txHashes = vm.parseJsonStringArray(json, ".tx_hashes");
        address[] memory measureTokens = vm.parseJsonAddressArray(json, ".measure_tokens");
        uint256[] memory expectedDeltas = vm.parseJsonUintArray(json, ".expected_balance_delta_wei");
        bytes[] memory calldatas = vm.parseJsonBytesArray(json, ".calldatas");

        _requireArrayLengths(ids, routeIds, hopCounts, txHashes, measureTokens, expectedDeltas, calldatas);

        uint256 rows;
        uint256 totalRealized;
        bool saw2;
        bool saw3;
        bool saw4;

        for (uint256 i = 0; i < ids.length; i++) {
            totalRealized += _replayOne(
                ids[i], routeIds[i], hopCounts[i], txHashes[i], measureTokens[i], expectedDeltas[i], calldatas[i], true
            );
            rows++;
            if (hopCounts[i] == 2) saw2 = true;
            else if (hopCounts[i] == 3) saw3 = true;
            else if (hopCounts[i] == 4) saw4 = true;
            else assertTrue(false, "unexpected hop count");
        }

        emit log_named_uint("Gate9 WETH calldata rows replayed", rows);
        emit log_named_uint("Gate9 total realized WETH delta wei", totalRealized);

        assertTrue(saw2, "missing 2-hop Gate9 calldata row");
        assertTrue(saw3, "missing 3-hop Gate9 calldata row");
        assertTrue(saw4, "missing 4-hop Gate9 calldata row");
    }

    function test_Gate9BotCalldata_PrefixState_Diagnostic() public {
        string memory json = vm.readFile("./test/fixtures/gate9_calldata_replay_clean_weth_sample.json");
        string[] memory ids = vm.parseJsonStringArray(json, ".candidate_ids");
        uint256[] memory routeIds = vm.parseJsonUintArray(json, ".route_ids");
        uint256[] memory hopCounts = vm.parseJsonUintArray(json, ".hop_counts");
        string[] memory txHashes = vm.parseJsonStringArray(json, ".tx_hashes");
        address[] memory measureTokens = vm.parseJsonAddressArray(json, ".measure_tokens");
        bytes[] memory calldatas = vm.parseJsonBytesArray(json, ".calldatas");

        _requireArrayLengths(
            ids,
            routeIds,
            hopCounts,
            txHashes,
            measureTokens,
            vm.parseJsonUintArray(json, ".expected_balance_delta_wei"),
            calldatas
        );

        uint256 totalRealized;
        for (uint256 i = 0; i < ids.length; i++) {
            (bool ok, uint256 realizedDelta,,) =
                _classifyOne(ids[i], routeIds[i], hopCounts[i], txHashes[i], measureTokens[i], calldatas[i], false);
            assertTrue(ok, string.concat(ids[i], ": prefix-state diagnostic call reverted"));
            assertGt(realizedDelta, 0, string.concat(ids[i], ": prefix-state diagnostic did not realize WETH"));
            totalRealized += realizedDelta;
        }
        emit log_named_uint("Gate9 prefix-state total realized WETH delta wei", totalRealized);
    }

    function test_Gate9BotCalldata_AllRows_ClassifyPrefixAndPostTrigger_Diagnostic() public {
        string memory json = vm.readFile("./test/fixtures/gate9_calldata_replay_clean_weth_sample.json");
        string[] memory ids = vm.parseJsonStringArray(json, ".candidate_ids");
        uint256[] memory routeIds = vm.parseJsonUintArray(json, ".route_ids");
        uint256[] memory hopCounts = vm.parseJsonUintArray(json, ".hop_counts");
        string[] memory txHashes = vm.parseJsonStringArray(json, ".tx_hashes");
        address[] memory measureTokens = vm.parseJsonAddressArray(json, ".measure_tokens");
        uint256[] memory expectedDeltas = vm.parseJsonUintArray(json, ".expected_balance_delta_wei");
        bytes[] memory calldatas = vm.parseJsonBytesArray(json, ".calldatas");

        _requireArrayLengths(ids, routeIds, hopCounts, txHashes, measureTokens, expectedDeltas, calldatas);

        uint256 okRows;
        for (uint256 mode = 0; mode < 2; mode++) {
            bool executeTrigger = mode == 1;
            for (uint256 i = 0; i < ids.length; i++) {
                (bool ok,,,) = _classifyOne(
                    ids[i], routeIds[i], hopCounts[i], txHashes[i], measureTokens[i], calldatas[i], executeTrigger
                );
                if (ok) okRows++;
            }
        }

        emit log_named_uint("Gate9 diagnostic successful row executions", okRows);
        assertEq(ids.length, 3, "diagnostic fixture must keep 2/3/4-hop coverage");
    }

    function _classifyFixtureRow(uint256 index, bool executeTrigger) internal {
        string memory json = vm.readFile("./test/fixtures/gate9_calldata_replay_clean_weth_sample.json");
        string[] memory ids = vm.parseJsonStringArray(json, ".candidate_ids");
        uint256[] memory routeIds = vm.parseJsonUintArray(json, ".route_ids");
        uint256[] memory hopCounts = vm.parseJsonUintArray(json, ".hop_counts");
        string[] memory txHashes = vm.parseJsonStringArray(json, ".tx_hashes");
        address[] memory measureTokens = vm.parseJsonAddressArray(json, ".measure_tokens");
        uint256[] memory expectedDeltas = vm.parseJsonUintArray(json, ".expected_balance_delta_wei");
        bytes[] memory calldatas = vm.parseJsonBytesArray(json, ".calldatas");

        _requireArrayLengths(ids, routeIds, hopCounts, txHashes, measureTokens, expectedDeltas, calldatas);
        require(index < ids.length, "fixture index out of range");
        _classifyOne(
            ids[index],
            routeIds[index],
            hopCounts[index],
            txHashes[index],
            measureTokens[index],
            calldatas[index],
            executeTrigger
        );
    }

    function test_Gate9BotCalldata_3Hop_PostTrigger_Diagnostic() public {
        _classifyFixtureRow(1, true);
    }

    function test_Gate9BotCalldata_4Hop_PostTrigger_Diagnostic() public {
        _classifyFixtureRow(2, true);
    }
}
