// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/MantisExecutor.sol";

interface IERC20Gate9CorpusReplay {
    function balanceOf(address account) external view returns (uint256);
}

/// Replays every current Gate9 monitored-B clean REVM success row with exact bot calldata.
///
/// Scope is intentionally narrow and strict: this is not a mechanical route-exec smoke test.
/// Each row starts from Foundry's transaction-positioned Arbitrum fork, executes the historical
/// trigger transaction, calls the exact engine-emitted calldata, and requires the payee token delta
/// to equal the REVM balance_delta_wei recorded in the candidate artifact.
contract MantisGate9CleanSuccessCorpusReplayForkTest is Test {
    bytes4 constant MIXED_3_SEL = 0xfe95fbbd;
    bytes4 constant MIXED_4_SEL = 0x29649abb;
    bytes4 constant LOSS_SELECTOR = 0xb65b2e08;
    uint256 constant EXPECTED_BLOCK = 465_686_729;
    // 22 = the byte-exact-reproducible clean-success rows. The 5 revm=success rows that do NOT
    // reproduce on the trigger-positioned fork are excluded and isolated in
    // V4CurveStateDivergenceAll.Fork.t.sol (all capital-safe, zero kernel/executor bugs):
    //   * route268 (UniswapV4>DodoV2): DODO same-block maintainer-fee forward-apply drift — FIXED
    //     byte-exact in engine-amm/src/dodo_v2.rs.
    //   * 3408/6252/6674/15603 (UniswapV4-containing): V4 PoolManager singleton-state reconstruction
    //     gap (trigger-only fork replay can't reconstruct the V4 singleton pool state) — AWS-gated.
    uint256 constant EXPECTED_ROWS = 22;

    function setUp() public {
        require(block.chainid == 42161, "fork Arbitrum One");
    }

    function _sel(bytes memory b) internal pure returns (bytes4 s) {
        require(b.length >= 4, "short calldata");
        assembly {
            s := mload(add(b, 0x20))
        }
    }

    function _assertSelector(uint256 hopCount, bytes4 selector, string memory id) internal pure {
        if (hopCount == 4) {
            assertEq(selector, MIXED_4_SEL, string.concat(id, ": 4-hop row must use flashLoanArbMixed4"));
        } else {
            assertEq(selector, MIXED_3_SEL, string.concat(id, ": 2/3-hop row must use flashLoanArbMixed"));
        }
    }

    function _decodeSingleUintArg(bytes memory ret) internal pure returns (uint256 value) {
        require(ret.length >= 36, "short error payload");
        assembly {
            value := mload(add(ret, 0x24))
        }
    }

    function _tryBalanceOf(address token, address account) internal view returns (bool ok, uint256 balance) {
        bytes memory ret;
        (ok, ret) = token.staticcall(abi.encodeWithSignature("balanceOf(address)", account));
        if (ok && ret.length >= 32) {
            balance = abi.decode(ret, (uint256));
        } else {
            ok = false;
        }
    }

    function _requireArrayLengths(
        string[] memory ids,
        uint256[] memory routeIds,
        uint256[] memory hopCounts,
        string[] memory txHashes,
        address[] memory measureTokens,
        string[] memory expectedDeltas,
        string[] memory familyPaths,
        bytes[] memory calldatas
    ) internal pure {
        uint256 n = ids.length;
        require(n == EXPECTED_ROWS, "fixture must cover all 22 reproducible clean success rows");
        require(routeIds.length == n, "route id length mismatch");
        require(hopCounts.length == n, "hop count length mismatch");
        require(txHashes.length == n, "tx hash length mismatch");
        require(measureTokens.length == n, "measure token length mismatch");
        require(expectedDeltas.length == n, "expected delta length mismatch");
        require(familyPaths.length == n, "family path length mismatch");
        require(calldatas.length == n, "calldata length mismatch");
    }

    function _replayOne(
        string memory id,
        uint256 routeId,
        uint256 hopCount,
        string memory txHashRaw,
        address measureToken,
        string memory expectedDeltaRaw,
        string memory familyPath,
        bytes memory cd
    ) internal returns (uint256 realizedDelta) {
        _assertSelector(hopCount, _sel(cd), id);

        bytes32 txHash = vm.parseBytes32(txHashRaw);
        vm.rollFork(txHash);
        assertEq(block.number, EXPECTED_BLOCK, string.concat(id, ": wrong rolled block"));

        vm.transact(txHash);
        assertEq(block.number, EXPECTED_BLOCK, string.concat(id, ": wrong post-transaction block"));

        MantisExecutor executor = new MantisExecutor(address(this));
        uint256 beforeBalance = IERC20Gate9CorpusReplay(measureToken).balanceOf(address(this));
        (bool ok, bytes memory ret) = address(executor).call(cd);
        uint256 afterBalance = IERC20Gate9CorpusReplay(measureToken).balanceOf(address(this));

        if (!ok) {
            bytes4 revertSelector = ret.length >= 4 ? _sel(ret) : bytes4(0);
            emit log_string(id);
            emit log_string(familyPath);
            emit log_named_uint("route id", routeId);
            emit log_named_uint("hop count", hopCount);
            emit log_named_uint("revert data length", ret.length);
            emit log_named_bytes32("revert selector", bytes32(revertSelector));
            assertTrue(false, string.concat(id, ": exact clean-success calldata reverted"));
        }

        assertGe(afterBalance, beforeBalance, string.concat(id, ": payee token balance decreased"));
        realizedDelta = afterBalance - beforeBalance;
        uint256 expectedDelta = vm.parseUint(expectedDeltaRaw);

        emit log_string(id);
        emit log_string(familyPath);
        emit log_named_uint("route id", routeId);
        emit log_named_uint("hop count", hopCount);
        emit log_named_address("measure token", measureToken);
        emit log_named_uint("expected REVM token delta", expectedDelta);
        emit log_named_uint("realized fork token delta", realizedDelta);

        assertGt(realizedDelta, 0, string.concat(id, ": fork replay did not realize positive token delta"));
        assertEq(realizedDelta, expectedDelta, string.concat(id, ": fork replay delta does not match REVM"));
    }

    function _classifyOne(
        string memory id,
        uint256 routeId,
        uint256 hopCount,
        string memory txHashRaw,
        address measureToken,
        string memory expectedDeltaRaw,
        string memory familyPath,
        bytes memory cd
    ) internal returns (uint256 outcome, uint256 realizedDelta, uint256 lossShortfall) {
        return _classifyOneAtState(
            id, routeId, hopCount, txHashRaw, measureToken, expectedDeltaRaw, familyPath, cd, true
        );
    }

    function _classifyOneAtState(
        string memory id,
        uint256 routeId,
        uint256 hopCount,
        string memory txHashRaw,
        address measureToken,
        string memory expectedDeltaRaw,
        string memory familyPath,
        bytes memory cd,
        bool executeTrigger
    ) internal returns (uint256 outcome, uint256 realizedDelta, uint256 lossShortfall) {
        _assertSelector(hopCount, _sel(cd), id);

        bytes32 txHash = vm.parseBytes32(txHashRaw);
        vm.rollFork(txHash);
        assertEq(block.number, EXPECTED_BLOCK, string.concat(id, ": wrong rolled block"));
        if (executeTrigger) {
            vm.transact(txHash);
            assertEq(block.number, EXPECTED_BLOCK, string.concat(id, ": wrong post-transaction block"));
        }

        MantisExecutor executor = new MantisExecutor(address(this));
        uint256 beforeBalance = IERC20Gate9CorpusReplay(measureToken).balanceOf(address(this));
        bytes memory ret;
        bool ok;
        (ok, ret) = address(executor).call(cd);
        uint256 afterBalance = IERC20Gate9CorpusReplay(measureToken).balanceOf(address(this));

        uint256 expectedDelta = vm.parseUint(expectedDeltaRaw);
        bytes4 revertSelector = ret.length >= 4 ? _sel(ret) : bytes4(0);
        if (ok && afterBalance >= beforeBalance) {
            realizedDelta = afterBalance - beforeBalance;
            outcome = realizedDelta == expectedDelta ? 0 : 3;
        } else if (!ok && revertSelector == LOSS_SELECTOR) {
            outcome = 1;
            lossShortfall = _decodeSingleUintArg(ret);
        } else {
            outcome = 2;
        }

        emit log_string(id);
        emit log_string(familyPath);
        emit log_named_uint("route id", routeId);
        emit log_named_uint("hop count", hopCount);
        emit log_named_uint("execute trigger transaction", executeTrigger ? 1 : 0);
        emit log_named_address("measure token", measureToken);
        emit log_named_uint("expected REVM token delta", expectedDelta);
        emit log_named_uint("diagnostic outcome 0=match 1=loss 2=other 3=mismatch", outcome);
        emit log_named_uint("realized fork token delta", realizedDelta);
        emit log_named_uint("loss shortfall", lossShortfall);
        emit log_named_uint("revert data length", ret.length);
        emit log_named_bytes32("revert selector", bytes32(revertSelector));
    }

    function test_Gate9CleanSuccessCorpus_PostTrigger_AllRowsMatchRevm() public {
        string memory json = vm.readFile("./test/fixtures/gate9_clean_success_corpus_replay.json");
        string[] memory ids = vm.parseJsonStringArray(json, ".candidate_ids");
        uint256[] memory routeIds = vm.parseJsonUintArray(json, ".route_ids");
        uint256[] memory hopCounts = vm.parseJsonUintArray(json, ".hop_counts");
        string[] memory txHashes = vm.parseJsonStringArray(json, ".tx_hashes");
        address[] memory measureTokens = vm.parseJsonAddressArray(json, ".measure_tokens");
        string[] memory expectedDeltas = vm.parseJsonStringArray(json, ".expected_balance_delta_wei");
        string[] memory familyPaths = vm.parseJsonStringArray(json, ".family_paths");
        bytes[] memory calldatas = vm.parseJsonBytesArray(json, ".calldatas");

        _requireArrayLengths(ids, routeIds, hopCounts, txHashes, measureTokens, expectedDeltas, familyPaths, calldatas);

        uint256 rows;
        uint256 saw2;
        uint256 saw3;
        uint256 saw4;
        for (uint256 i = 0; i < ids.length; i++) {
            _replayOne(
                ids[i],
                routeIds[i],
                hopCounts[i],
                txHashes[i],
                measureTokens[i],
                expectedDeltas[i],
                familyPaths[i],
                calldatas[i]
            );
            rows++;
            if (hopCounts[i] == 2) saw2++;
            else if (hopCounts[i] == 3) saw3++;
            else if (hopCounts[i] == 4) saw4++;
            else assertTrue(false, "unexpected hop count");
        }

        emit log_named_uint("Gate9 clean-success exact calldata rows replayed", rows);
        emit log_named_uint("Gate9 clean-success 2-hop rows", saw2);
        emit log_named_uint("Gate9 clean-success 3-hop rows", saw3);
        emit log_named_uint("Gate9 clean-success 4-hop rows", saw4);

        assertEq(rows, EXPECTED_ROWS, "wrong clean-success row count");
        // 22 reproducible rows = 7 two-hop + 11 three-hop + 4 four-hop. The excluded 5 non-reproducible
        // revm=success rows (isolated in V4CurveStateDivergenceAll, capital-safe) were: route268 (2-hop,
        // DODO maintainer-fee — FIXED), 3408/6252/6674 (3-hop, V4 singleton — AWS-gated), 15603 (4-hop,
        // V4 singleton — AWS-gated).
        assertEq(saw2, 7, "wrong 2-hop clean-success coverage");
        assertEq(saw3, 11, "wrong 3-hop clean-success coverage");
        assertEq(saw4, 4, "wrong 4-hop clean-success coverage");
    }

    function test_Gate9CleanSuccessCorpus_PostTrigger_ClassifyAllRows_Diagnostic() public {
        _classifyRange(0, EXPECTED_ROWS);
    }

    function test_Gate9CleanSuccessCorpus_PostTrigger_ClassifyRangeFromEnv_Diagnostic() public {
        uint256 start = vm.envOr("GATE9_CORPUS_START", uint256(0));
        uint256 limit = vm.envOr("GATE9_CORPUS_LIMIT", EXPECTED_ROWS);
        _classifyRangeAtState(start, limit, true);
    }

    function test_Gate9CleanSuccessCorpus_PreTrigger_ClassifyRangeFromEnv_Diagnostic() public {
        uint256 start = vm.envOr("GATE9_CORPUS_START", uint256(0));
        uint256 limit = vm.envOr("GATE9_CORPUS_LIMIT", EXPECTED_ROWS);
        _classifyRangeAtState(start, limit, false);
    }

    function test_Gate9CleanSuccessCorpus_LoadedState_ClassifyRangeFromEnv_Diagnostic() public {
        uint256 start = vm.envOr("GATE9_CORPUS_START", uint256(0));
        uint256 limit = vm.envOr("GATE9_CORPUS_LIMIT", EXPECTED_ROWS);
        _classifyRangeLoadedState(start, limit);
    }

    function _classifyRange(uint256 start, uint256 limit) internal {
        _classifyRangeAtState(start, limit, true);
    }

    function _classifyRangeAtState(uint256 start, uint256 limit, bool executeTrigger) internal {
        string memory json = vm.readFile("./test/fixtures/gate9_clean_success_corpus_replay.json");
        string[] memory ids = vm.parseJsonStringArray(json, ".candidate_ids");
        uint256[] memory routeIds = vm.parseJsonUintArray(json, ".route_ids");
        uint256[] memory hopCounts = vm.parseJsonUintArray(json, ".hop_counts");
        string[] memory txHashes = vm.parseJsonStringArray(json, ".tx_hashes");
        address[] memory measureTokens = vm.parseJsonAddressArray(json, ".measure_tokens");
        string[] memory expectedDeltas = vm.parseJsonStringArray(json, ".expected_balance_delta_wei");
        string[] memory familyPaths = vm.parseJsonStringArray(json, ".family_paths");
        bytes[] memory calldatas = vm.parseJsonBytesArray(json, ".calldatas");

        _requireArrayLengths(ids, routeIds, hopCounts, txHashes, measureTokens, expectedDeltas, familyPaths, calldatas);
        require(start < ids.length, "range start outside fixture");
        uint256 end = start + limit;
        if (end > ids.length) end = ids.length;

        uint256 exactMatches;
        uint256 losses;
        uint256 otherFailures;
        uint256 mismatches;
        for (uint256 i = start; i < end; i++) {
            (uint256 outcome,,) = _classifyOneAtState(
                ids[i],
                routeIds[i],
                hopCounts[i],
                txHashes[i],
                measureTokens[i],
                expectedDeltas[i],
                familyPaths[i],
                calldatas[i],
                executeTrigger
            );
            if (outcome == 0) exactMatches++;
            else if (outcome == 1) losses++;
            else if (outcome == 2) otherFailures++;
            else if (outcome == 3) mismatches++;
            else assertTrue(false, "unexpected diagnostic outcome");
        }

        emit log_named_uint("Gate9 clean-success diagnostic exact matches", exactMatches);
        emit log_named_uint("Gate9 clean-success diagnostic Loss rows", losses);
        emit log_named_uint("Gate9 clean-success diagnostic other failures", otherFailures);
        emit log_named_uint("Gate9 clean-success diagnostic delta mismatches", mismatches);
        emit log_named_uint("Gate9 clean-success diagnostic range start", start);
        emit log_named_uint("Gate9 clean-success diagnostic range end", end);
        emit log_named_uint("Gate9 clean-success diagnostic execute trigger", executeTrigger ? 1 : 0);

        assertEq(ids.length, EXPECTED_ROWS, "wrong diagnostic row count");
    }

    function _classifyRangeLoadedState(uint256 start, uint256 limit) internal {
        string memory json = vm.readFile("./test/fixtures/gate9_clean_success_corpus_replay.json");
        string[] memory ids = vm.parseJsonStringArray(json, ".candidate_ids");
        uint256[] memory routeIds = vm.parseJsonUintArray(json, ".route_ids");
        uint256[] memory hopCounts = vm.parseJsonUintArray(json, ".hop_counts");
        string[] memory txHashes = vm.parseJsonStringArray(json, ".tx_hashes");
        address[] memory measureTokens = vm.parseJsonAddressArray(json, ".measure_tokens");
        string[] memory expectedDeltas = vm.parseJsonStringArray(json, ".expected_balance_delta_wei");
        string[] memory familyPaths = vm.parseJsonStringArray(json, ".family_paths");
        bytes[] memory calldatas = vm.parseJsonBytesArray(json, ".calldatas");

        _requireArrayLengths(ids, routeIds, hopCounts, txHashes, measureTokens, expectedDeltas, familyPaths, calldatas);
        require(start < ids.length, "range start outside fixture");
        uint256 end = start + limit;
        if (end > ids.length) end = ids.length;

        uint256 exactMatches;
        uint256 losses;
        uint256 otherFailures;
        uint256 mismatches;
        for (uint256 i = start; i < end; i++) {
            uint256 snapshot = vm.snapshotState();
            (uint256 outcome,,) = _classifyOneLoadedState(
                ids[i],
                routeIds[i],
                hopCounts[i],
                txHashes[i],
                measureTokens[i],
                expectedDeltas[i],
                familyPaths[i],
                calldatas[i]
            );
            vm.revertToState(snapshot);

            if (outcome == 0) exactMatches++;
            else if (outcome == 1) losses++;
            else if (outcome == 2) otherFailures++;
            else if (outcome == 3) mismatches++;
            else assertTrue(false, "unexpected diagnostic outcome");
        }

        emit log_named_uint("Gate9 clean-success loaded-state diagnostic exact matches", exactMatches);
        emit log_named_uint("Gate9 clean-success loaded-state diagnostic Loss rows", losses);
        emit log_named_uint("Gate9 clean-success loaded-state diagnostic other failures", otherFailures);
        emit log_named_uint("Gate9 clean-success loaded-state diagnostic delta mismatches", mismatches);
        emit log_named_uint("Gate9 clean-success loaded-state diagnostic range start", start);
        emit log_named_uint("Gate9 clean-success loaded-state diagnostic range end", end);

        assertEq(ids.length, EXPECTED_ROWS, "wrong loaded-state diagnostic row count");
    }

    function _classifyOneLoadedState(
        string memory id,
        uint256 routeId,
        uint256 hopCount,
        string memory txHashRaw,
        address measureToken,
        string memory expectedDeltaRaw,
        string memory familyPath,
        bytes memory cd
    ) internal returns (uint256 outcome, uint256 realizedDelta, uint256 lossShortfall) {
        _assertSelector(hopCount, _sel(cd), id);

        MantisExecutor executor = new MantisExecutor(address(this));
        (bool beforeOk, uint256 beforeBalance) = _tryBalanceOf(measureToken, address(this));
        if (!beforeOk) {
            emit log_string(id);
            emit log_string(familyPath);
            emit log_named_uint("route id", routeId);
            emit log_named_uint("hop count", hopCount);
            emit log_named_bytes32("source tx hash", vm.parseBytes32(txHashRaw));
            emit log_named_uint("loaded-state block number", block.number);
            emit log_named_address("measure token", measureToken);
            emit log_named_string("loaded-state setup failure", "measure token balanceOf failed before executor call");
            return (2, 0, 0);
        }
        bytes memory ret;
        bool ok;
        (ok, ret) = address(executor).call(cd);
        (bool afterOk, uint256 afterBalance) = _tryBalanceOf(measureToken, address(this));
        if (!afterOk) {
            emit log_string(id);
            emit log_string(familyPath);
            emit log_named_uint("route id", routeId);
            emit log_named_uint("hop count", hopCount);
            emit log_named_bytes32("source tx hash", vm.parseBytes32(txHashRaw));
            emit log_named_uint("loaded-state block number", block.number);
            emit log_named_address("measure token", measureToken);
            emit log_named_string("loaded-state setup failure", "measure token balanceOf failed after executor call");
            return (2, 0, 0);
        }

        uint256 expectedDelta = vm.parseUint(expectedDeltaRaw);
        bytes4 revertSelector = ret.length >= 4 ? _sel(ret) : bytes4(0);
        if (ok && afterBalance >= beforeBalance) {
            realizedDelta = afterBalance - beforeBalance;
            outcome = realizedDelta == expectedDelta ? 0 : 3;
        } else if (!ok && revertSelector == LOSS_SELECTOR) {
            outcome = 1;
            lossShortfall = _decodeSingleUintArg(ret);
        } else {
            outcome = 2;
        }

        emit log_string(id);
        emit log_string(familyPath);
        emit log_named_uint("route id", routeId);
        emit log_named_uint("hop count", hopCount);
        emit log_named_bytes32("source tx hash", vm.parseBytes32(txHashRaw));
        emit log_named_uint("loaded-state block number", block.number);
        emit log_named_address("measure token", measureToken);
        emit log_named_uint("expected REVM token delta", expectedDelta);
        emit log_named_uint("loaded-state outcome 0=match 1=loss 2=other 3=mismatch", outcome);
        emit log_named_uint("loaded-state realized token delta", realizedDelta);
        emit log_named_uint("loaded-state loss shortfall", lossShortfall);
        emit log_named_uint("loaded-state revert data length", ret.length);
        emit log_named_bytes32("loaded-state revert selector", bytes32(revertSelector));
    }
}
