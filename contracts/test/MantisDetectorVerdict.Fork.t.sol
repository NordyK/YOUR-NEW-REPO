// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/MantisExecutor.sol";

/// THE detector-verdict test (DETECTOR_PROFIT_VERDICT.md): take Mantis's OWN live detections
/// (DETECT,route_id,block,amount_in,gross,hops,[eid,pool]×hops captured from a dry-run), fork at the
/// detection-window block, and execute EACH route at Mantis's OWN detected size with minNetProfit=0,
/// measuring REALIZED net = WETH delta to payee (success) or `Loss` (net<0). Answers "does the
/// detector find real money": count net-positive vs the detector's claimed gross.
///
/// These detections are RESYNC-driven (synthetic 2^48 seq) — the engine periodically re-fetches
/// hot-pool state from chain and re-detects on the LATEST state — so forking at the recent
/// detection-window head IS the right state to test them against.
///
/// Run: DETECT_FILE=/tmp/detect_routes.txt forge test --fork-url $ARBITRUM_RPC_URL \
///        --fork-block-number <head> --match-path test/MantisDetectorVerdict.Fork.t.sol -vv
interface IV3d {
    function token0() external view returns (address);
    function token1() external view returns (address);
    function fee() external view returns (uint24);
}

interface IDodoD {
    function _BASE_TOKEN_() external view returns (address);
    function _QUOTE_TOKEN_() external view returns (address);
}

interface IWETHd {
    function balanceOf(address) external view returns (uint256);
}

contract MantisDetectorVerdictForkTest is Test {
    MantisExecutor executor;
    address constant WETH = 0x82aF49447D8a07e3bd95BD0d56f35241523fBab1;
    uint8 constant CURVE_TRICRYPTO_NG = 16;
    uint8 constant CURVE_TWOCRYPTO_NG = 17;
    uint8 constant CURVE_STABLESWAP_PLAIN_NG = 18;
    uint8 constant CURVE_STABLESWAP_META_NG = 19;
    uint8 constant UNISWAP_V4 = 20;
    uint8 constant FLUID = 21;
    uint8 constant MAVERICK = 22;
    uint8 constant DODO = 23;
    uint8 constant BALANCER_STABLE = 26;
    uint8 constant BALANCER_WEIGHTED = 27;
    bytes4 constant ERROR_STRING = 0x08c379a0;
    error Loss(uint256);

    function setUp() public {
        require(block.chainid == 42161, "fork Arbitrum One");
        executor = new MantisExecutor(address(this));
    }

    function _tok(uint8 ex, address pool) internal view returns (address a, address b) {
        if (ex == DODO) return (IDodoD(pool)._BASE_TOKEN_(), IDodoD(pool)._QUOTE_TOKEN_());
        return (IV3d(pool).token0(), IV3d(pool).token1());
    }

    function _hop(uint8 ex, address tin, address tout, address pool)
        internal
        view
        returns (MantisExecutor.TradeHopV2 memory h)
    {
        h.exchangeId = ex;
        h.tokenIn = tin;
        h.tokenOut = tout;
        if (ex == 0 || ex == 6 || ex == 25) h.fee = IV3d(pool).fee();
        else h.pool = pool;
    }

    function _hopCount(string[] memory f) internal pure returns (uint256) {
        return vm.parseUint(f[5]);
    }

    function _fieldEq(string memory a, string memory b) internal pure returns (bool) {
        return keccak256(bytes(a)) == keccak256(bytes(b));
    }

    function _isTokenAware(string[] memory f) internal pure returns (bool) {
        return f.length > 0 && _fieldEq(f[0], "TOKEN_DETECT");
    }

    function _exchangeId(string[] memory f, uint256 i) internal pure returns (uint8) {
        if (_isTokenAware(f)) return uint8(vm.parseUint(f[7 + i * 3]));
        return uint8(vm.parseUint(f[6 + i * 2]));
    }

    function _pool(string[] memory f, uint256 i) internal pure returns (address) {
        if (_isTokenAware(f)) return vm.parseAddress(f[8 + i * 3]);
        return vm.parseAddress(f[7 + i * 2]);
    }

    function _token(string[] memory f, uint256 i) internal pure returns (address) {
        return vm.parseAddress(f[6 + i * 3]);
    }

    function _expectedFieldCount(string[] memory f, uint256 hopCount) internal pure returns (uint256) {
        if (_isTokenAware(f)) return 7 + hopCount * 3;
        return 6 + hopCount * 2;
    }

    function _isCurve(uint8 ex) internal pure returns (bool) {
        return ex == CURVE_TRICRYPTO_NG || ex == CURVE_TWOCRYPTO_NG || ex == CURVE_STABLESWAP_PLAIN_NG
            || ex == CURVE_STABLESWAP_META_NG;
    }

    function _needsMetadataNotPresentInDetectLine(uint8 ex) internal pure returns (bool) {
        return ex == UNISWAP_V4 || _isCurve(ex) || ex == FLUID || ex == MAVERICK || ex == BALANCER_STABLE
            || ex == BALANCER_WEIGHTED;
    }

    function _hasMetadataLimitedHop(string[] memory f, uint256 hopCount) internal pure returns (bool) {
        for (uint256 i = 0; i < hopCount; i++) {
            if (_needsMetadataNotPresentInDetectLine(_exchangeId(f, i))) return true;
        }
        return false;
    }

    // Returns outcome (0=profit, 1=loss/net<0, 2=other revert) + realized net wei. Kept as its own
    // function so the main loop's stack stays shallow (avoids Yul stack-too-deep).
    function runRoute(string[] memory f) external returns (uint8 outcome, int256 net) {
        uint16 id = uint16(vm.parseUint(f[1]));
        uint128 amount = uint128(vm.parseUint(f[3]));
        MantisExecutor.TradeHopV2[4] memory route = _build4(f);
        address measureToken = route[0].tokenIn;
        uint256 balBefore = IWETHd(measureToken).balanceOf(address(this));
        try executor.flashLoanArbMixed4(amount, route, 0) {
            net = int256(IWETHd(measureToken).balanceOf(address(this))) - int256(balBefore);
            outcome = net > 0 ? 0 : 1;
            emit log_named_int(string.concat("route ", vm.toString(id), " REALIZED net wei"), net);
            emit log_string(string.concat("route ", vm.toString(id), " outcome ", _outcomeName(outcome)));
        } catch (bytes memory data) {
            bytes4 sel;
            assembly { sel := mload(add(data, 0x20)) }
            if (sel == Loss.selector) {
                outcome = 1;
                uint256 sf;
                assembly { sf := mload(add(data, 0x24)) }
                net = -int256(sf);
                emit log_named_uint(string.concat("route ", vm.toString(id), " LOSS shortfall wei"), sf);
                emit log_string(string.concat("route ", vm.toString(id), " outcome loss"));
            } else {
                outcome = 2;
                emit log_named_bytes32(string.concat("route ", vm.toString(id), " other-revert"), bytes4(sel));
                if (sel == ERROR_STRING) {
                    emit log_string(string.concat("route ", vm.toString(id), " revert reason ", _errorReason(data)));
                }
                emit log_string(string.concat("route ", vm.toString(id), " outcome other_revert"));
            }
        }
    }

    function _classifyDetection(string[] memory f) internal returns (uint8 outcome, int256 net) {
        if (f.length < 6) {
            emit log_string(string.concat(_routeLabel(f), " malformed DETECT row"));
            return (2, 0);
        }
        uint256 hopCount = _hopCount(f);
        if (hopCount < 2 || hopCount > 4) {
            emit log_string(string.concat(_routeLabel(f), " unsupported hop_count ", f[5]));
            return (2, 0);
        }
        if (f.length < _expectedFieldCount(f, hopCount)) {
            emit log_string(string.concat(_routeLabel(f), " malformed DETECT row for hop_count ", f[5]));
            return (2, 0);
        }
        if (_hasMetadataLimitedHop(f, hopCount)) {
            emit log_string(string.concat(_routeLabel(f), " unsupported metadata-limited family in current DETECT row"));
            return (2, 0);
        }

        try this.runRoute(f) returns (uint8 gotOutcome, int256 gotNet) {
            return (gotOutcome, gotNet);
        } catch (bytes memory data) {
            bytes4 sel;
            assembly { sel := mload(add(data, 0x20)) }
            emit log_named_bytes32(string.concat(_routeLabel(f), " build/execution other-revert"), bytes4(sel));
            if (sel == ERROR_STRING) {
                emit log_string(string.concat(_routeLabel(f), " build/execution revert reason ", _errorReason(data)));
            }
            return (2, 0);
        }
    }

    function _routeLabel(string[] memory f) internal pure returns (string memory) {
        if (f.length > 1) return string.concat("route ", f[1]);
        return "route <missing-id>";
    }

    function _outcomeName(uint8 outcome) internal pure returns (string memory) {
        if (outcome == 0) return "positive";
        if (outcome == 1) return "loss";
        return "other_revert";
    }

    function _errorReason(bytes memory data) internal pure returns (string memory) {
        if (data.length < 68) return "";
        bytes memory encoded = new bytes(data.length - 4);
        for (uint256 i = 4; i < data.length; i++) {
            encoded[i - 4] = data[i];
        }
        return abi.decode(encoded, (string));
    }

    function _nextToken(uint8 ex, address pool, address tokenIn, bool lastHop)
        internal
        view
        returns (address tokenOut)
    {
        (address a, address b) = _tok(ex, pool);
        require(a == tokenIn || b == tokenIn, "pool missing tokenIn");
        tokenOut = a == tokenIn ? b : a;
        if (lastHop) {
            require(a == WETH || b == WETH, "last pool missing WETH");
            tokenOut = WETH;
        }
    }

    function _requirePoolContains(address pool, uint8 ex, address tokenIn, address tokenOut) internal view {
        (address a, address b) = _tok(ex, pool);
        require(a == tokenIn || b == tokenIn, "pool missing tokenIn");
        require(a == tokenOut || b == tokenOut, "pool missing tokenOut");
    }

    function _build4(string[] memory f) internal view returns (MantisExecutor.TradeHopV2[4] memory route) {
        uint256 hopCount = _hopCount(f);
        bool tokenAware = _isTokenAware(f);
        address tokenIn = tokenAware ? _token(f, 0) : WETH;
        address startToken = tokenIn;
        for (uint256 i = 0; i < hopCount; i++) {
            uint8 ex = _exchangeId(f, i);
            address p = _pool(f, i);
            address tokenOut;
            if (tokenAware) {
                tokenOut = _token(f, i + 1);
                _requirePoolContains(p, ex, tokenIn, tokenOut);
            } else {
                tokenOut = _nextToken(ex, p, tokenIn, i == hopCount - 1);
            }
            route[i] = _hop(ex, tokenIn, tokenOut, p);
            tokenIn = tokenOut;
        }
        if (tokenAware) require(tokenIn == startToken, "route is not a cycle");
    }

    function test_DetectorVerdict_RealizedNet() public {
        string memory path = vm.envString("DETECT_FILE");
        uint256 nProfit;
        uint256 nLoss;
        uint256 nOther;
        uint256 nTotal;
        int256 netSum;
        while (true) {
            string memory line = vm.readLine(path);
            if (bytes(line).length == 0) break;
            string[] memory f = vm.split(line, ",");
            nTotal++;
            emit log_string(string.concat("DETECT_ROW ", line));
            (uint8 outcome, int256 net) = _classifyDetection(f);
            emit log_string(string.concat(_routeLabel(f), " final outcome ", _outcomeName(outcome)));
            netSum += net;
            if (outcome == 0) nProfit++;
            else if (outcome == 1) nLoss++;
            else nOther++;
        }
        emit log_string("================ DETECTOR VERDICT ================");
        emit log_named_uint("detections tested", nTotal);
        emit log_named_uint("ACTUALLY net-positive (realized profit)", nProfit);
        emit log_named_uint("net-negative / Loss", nLoss);
        emit log_named_uint("other reverts (mechanical)", nOther);
        emit log_named_int("sum realized net (wei, all routes)", netSum);

        uint256 minTotal = vm.envOr("DETECTOR_MIN_TOTAL", uint256(1));
        uint256 minProfit = vm.envOr("DETECTOR_MIN_PROFIT", uint256(1));
        uint256 maxLoss = vm.envOr("DETECTOR_MAX_LOSS", uint256(0));
        uint256 maxOther = vm.envOr("DETECTOR_MAX_OTHER_REVERTS", uint256(0));
        uint256 minNetSumWei = vm.envOr("DETECTOR_MIN_NET_SUM_WEI", uint256(1));

        assertGe(nTotal, minTotal, "detector verdict corpus is empty or below minimum");
        assertGe(nProfit, minProfit, "not enough realized-positive detector routes");
        assertLe(nLoss, maxLoss, "Loss profit-gate routes exceed maximum");
        assertLe(nOther, maxOther, "mechanical/non-profit-gate reverts exceed maximum");
        assertTrue(netSum >= int256(minNetSumWei), "realized net sum is not positive enough");
    }
}
