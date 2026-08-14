// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/MantisExecutor.sol";

/// DET-3.2 STATE-STALENESS SENSITIVITY. Re-evaluate the SAME detected route at a FIXED size at
/// trigger+k blocks (k in {0,1,2,5,10,50}) without our own intervention. A REAL edge on a liquid
/// hub pool must decay within a few blocks (competition closes it); an edge that persists identically
/// for ~50 blocks is the route-6747/2675 phantom signature.
///
/// Flagship clean 2-hop arb, fixed 4 ETH input:
///   WETH --Pancake V3(100)--> WBTC --Uniswap V3(500)--> WETH.
/// Detection (pre-arb) state is the PARENT block 465686728; the arb tx is in 465686729.
///
/// Run (per k): forge test --fork-url $ARBITRUM_RPC_URL --fork-block-number <465686728+k> \
///        --match-path test/MantisStaleness.Fork.t.sol -vv
interface IWETHst { function balanceOf(address) external view returns (uint256); }

contract MantisStalenessForkTest is Test {
    MantisExecutor executor;
    address constant WETH = 0x82aF49447D8a07e3bd95BD0d56f35241523fBab1;
    address constant WBTC = 0x2f2a2543B76A4166549F7aaB2e75Bef0aefC5B0f;
    uint8 constant UNIV3 = 0;
    uint8 constant PANCAKE_V3 = 6;

    function setUp() public {
        require(block.chainid == 42161, "fork Arbitrum One");
        executor = new MantisExecutor(address(this));
    }

    function _v3(uint8 ex, address tin, address tout, uint24 fee) internal pure returns (MantisExecutor.TradeHopV2 memory h) {
        h.exchangeId = ex; h.tokenIn = tin; h.tokenOut = tout; h.fee = fee;
    }

    function _measureAt(uint256 forkBlock) internal returns (bool profitable, int256 realized) {
        vm.createSelectFork(vm.envString("ARBITRUM_RPC_URL"), forkBlock);
        MantisExecutor localExecutor = new MantisExecutor(address(this));
        MantisExecutor.TradeHopV2[3] memory route;
        route[0] = _v3(PANCAKE_V3, WETH, WBTC, 100);
        route[1] = _v3(UNIV3, WBTC, WETH, 500);
        uint128 size = 4 ether; // fixed across blocks — isolates state decay, not size effects
        uint256 bal0 = IWETHst(WETH).balanceOf(address(this));
        try localExecutor.flashLoanArbMixed(size, route, 0) {
            realized = int256(IWETHst(WETH).balanceOf(address(this)) - bal0);
            emit log_named_int("fork.block", int256(block.number));
            emit log_named_int("realized profit wei @4ETH (positive = edge still open)", realized);
            profitable = realized > 0;
        } catch {
            emit log_named_int("fork.block", int256(block.number));
            emit log_string("EDGE CLOSED: flashLoanArbMixed reverted (Loss) -> realized <= 0");
            realized = 0;
            profitable = false;
        }
    }

    function test_StalenessFixedSize() public {
        uint256 base = vm.envOr("STALENESS_BASE_BLOCK", uint256(465686728));
        (bool p0, int256 net0) = _measureAt(base);
        (bool p10, ) = _measureAt(base + 10);
        (bool p50, ) = _measureAt(base + 50);

        assertTrue(p0, "base edge must be profitable at parent state");
        assertGt(net0, 0, "base edge net must be positive");
        assertFalse(p10 && p50, "edge persists too long and looks stale/phantom");
    }
}
