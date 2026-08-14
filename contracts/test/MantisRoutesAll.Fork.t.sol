// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/MantisExecutor.sol";

/// Executes ALL 40 engine-detected routes from arb_routes_for_anvil.csv through MantisExecutor
/// on the fork. Per route: reconstruct directions on-chain, run flashLoanArbMixed, record whether
/// it executed through every leg (success OR Loss=ran-but-arb-closed) vs a mechanical failure.
/// Run: forge test --match-path test/MantisRoutesAll.Fork.t.sol -vv
interface IV3 { function token0() external view returns (address); function token1() external view returns (address); function fee() external view returns (uint24); }
interface IDodoP { function _BASE_TOKEN_() external view returns (address); function _QUOTE_TOKEN_() external view returns (address); }

contract MantisRoutesAllForkTest is Test {
    MantisExecutor executor;
    address constant WETH = 0x82aF49447D8a07e3bd95BD0d56f35241523fBab1;
    uint8 constant DODO = 23;
    uint128 constant AMOUNT_IN = 2e16; // 0.02 WETH — within every route's viable band
    error Loss(uint256);

    uint16[] ids; bool[] tri; uint8[] e0; uint8[] e1; uint8[] e2; address[] p0; address[] p1; address[] p2;

    function _add(uint16 id, bool t, uint8 x0, uint8 x1, uint8 x2, address a0, address a1, address a2) internal {
        ids.push(id); tri.push(t); e0.push(x0); e1.push(x1); e2.push(x2); p0.push(a0); p1.push(a1); p2.push(a2);
    }

    function setUp() public {
        require(block.chainid == 42161, "fork Arbitrum One");
        executor = new MantisExecutor(address(this));
        _add(2665, true, 0,23,0, 0x2f5e87C9312fa29aed5c179E456625D79015299c,0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14,0xC6962004f452bE9203591991D15f6b388e09E8D0);
        _add(2615, true, 0,23,0, 0x2f5e87C9312fa29aed5c179E456625D79015299c,0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14,0xC6962004f452bE9203591991D15f6b388e09E8D0);
        _add(2967, true, 6,23,0, 0x4bfc22A4dA7f31F8a912a79A7e44a822398b4390,0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14,0xC6962004f452bE9203591991D15f6b388e09E8D0);
        _add(2664, true, 0,23,23, 0x2f5e87C9312fa29aed5c179E456625D79015299c,0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14,0x1666EA6E83BCC3473111732Ef086BA47e6EDDeB7);
        _add(2668, true, 0,23,6, 0x2f5e87C9312fa29aed5c179E456625D79015299c,0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14,0x7fCDC35463E3770c2fB992716Cd070B63540b947);
        _add(2917, true, 6,23,0, 0x4bfc22A4dA7f31F8a912a79A7e44a822398b4390,0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14,0xC6962004f452bE9203591991D15f6b388e09E8D0);
        _add(2923, true, 6,23,0, 0x4bfc22A4dA7f31F8a912a79A7e44a822398b4390,0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14,0xC6962004f452bE9203591991D15f6b388e09E8D0);
        _add(2970, true, 6,23,6, 0x4bfc22A4dA7f31F8a912a79A7e44a822398b4390,0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14,0x7fCDC35463E3770c2fB992716Cd070B63540b947);
        _add(2920, true, 6,23,6, 0x4bfc22A4dA7f31F8a912a79A7e44a822398b4390,0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14,0x7fCDC35463E3770c2fB992716Cd070B63540b947);
        _add(2969, true, 6,23,6, 0x4bfc22A4dA7f31F8a912a79A7e44a822398b4390,0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14,0xd9e2a1a61B6E61b275cEc326465d417e52C1b95c);
        _add(2667, true, 0,23,6, 0x2f5e87C9312fa29aed5c179E456625D79015299c,0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14,0xd9e2a1a61B6E61b275cEc326465d417e52C1b95c);
        _add(2618, true, 0,23,6, 0x2f5e87C9312fa29aed5c179E456625D79015299c,0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14,0x7fCDC35463E3770c2fB992716Cd070B63540b947);
        _add(2621, true, 0,23,0, 0x2f5e87C9312fa29aed5c179E456625D79015299c,0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14,0xC6962004f452bE9203591991D15f6b388e09E8D0);
        _add(2971, true, 6,23,25, 0x4bfc22A4dA7f31F8a912a79A7e44a822398b4390,0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14,0xf3Eb87C1F6020982173C908E7eB31aA66c1f0296);
        _add(2966, true, 6,23,23, 0x4bfc22A4dA7f31F8a912a79A7e44a822398b4390,0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14,0x1666EA6E83BCC3473111732Ef086BA47e6EDDeB7);
        _add(2972, true, 6,23,9, 0x4bfc22A4dA7f31F8a912a79A7e44a822398b4390,0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14,0xB1026b8e7276e7AC75410F1fcbbe21796e8f7526);
        _add(2669, true, 0,23,25, 0x2f5e87C9312fa29aed5c179E456625D79015299c,0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14,0xf3Eb87C1F6020982173C908E7eB31aA66c1f0296);
        _add(2926, true, 6,23,6, 0x4bfc22A4dA7f31F8a912a79A7e44a822398b4390,0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14,0x7fCDC35463E3770c2fB992716Cd070B63540b947);
        _add(2670, true, 0,23,9, 0x2f5e87C9312fa29aed5c179E456625D79015299c,0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14,0xB1026b8e7276e7AC75410F1fcbbe21796e8f7526);
        _add(2922, true, 6,23,9, 0x4bfc22A4dA7f31F8a912a79A7e44a822398b4390,0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14,0xB1026b8e7276e7AC75410F1fcbbe21796e8f7526);
        _add(2927, true, 6,23,25, 0x4bfc22A4dA7f31F8a912a79A7e44a822398b4390,0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14,0xf3Eb87C1F6020982173C908E7eB31aA66c1f0296);
        _add(2620, true, 0,23,9, 0x2f5e87C9312fa29aed5c179E456625D79015299c,0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14,0xB1026b8e7276e7AC75410F1fcbbe21796e8f7526);
        _add(2919, true, 6,23,6, 0x4bfc22A4dA7f31F8a912a79A7e44a822398b4390,0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14,0xd9e2a1a61B6E61b275cEc326465d417e52C1b95c);
        _add(2928, true, 6,23,9, 0x4bfc22A4dA7f31F8a912a79A7e44a822398b4390,0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14,0xB1026b8e7276e7AC75410F1fcbbe21796e8f7526);
        _add(2925, true, 6,23,6, 0x4bfc22A4dA7f31F8a912a79A7e44a822398b4390,0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14,0xd9e2a1a61B6E61b275cEc326465d417e52C1b95c);
        _add(2617, true, 0,23,6, 0x2f5e87C9312fa29aed5c179E456625D79015299c,0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14,0xd9e2a1a61B6E61b275cEc326465d417e52C1b95c);
        _add(2921, true, 6,23,25, 0x4bfc22A4dA7f31F8a912a79A7e44a822398b4390,0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14,0xf3Eb87C1F6020982173C908E7eB31aA66c1f0296);
        _add(3287, true, 9,23,0, 0xd845f7D4f4DeB9Ff5bCf09D140Ef13718F6f6C71,0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14,0xC6962004f452bE9203591991D15f6b388e09E8D0);
        _add(2619, true, 0,23,25, 0x2f5e87C9312fa29aed5c179E456625D79015299c,0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14,0xf3Eb87C1F6020982173C908E7eB31aA66c1f0296);
        _add(3237, true, 9,23,0, 0xd845f7D4f4DeB9Ff5bCf09D140Ef13718F6f6C71,0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14,0xC6962004f452bE9203591991D15f6b388e09E8D0);
        _add(2625, true, 0,23,25, 0x2f5e87C9312fa29aed5c179E456625D79015299c,0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14,0xf3Eb87C1F6020982173C908E7eB31aA66c1f0296);
        _add(2624, true, 0,23,6, 0x2f5e87C9312fa29aed5c179E456625D79015299c,0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14,0x7fCDC35463E3770c2fB992716Cd070B63540b947);
        _add(3243, true, 9,23,0, 0xd845f7D4f4DeB9Ff5bCf09D140Ef13718F6f6C71,0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14,0xC6962004f452bE9203591991D15f6b388e09E8D0);
        _add(2626, true, 0,23,9, 0x2f5e87C9312fa29aed5c179E456625D79015299c,0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14,0xB1026b8e7276e7AC75410F1fcbbe21796e8f7526);
        _add(2974, true, 6,23,0, 0x4bfc22A4dA7f31F8a912a79A7e44a822398b4390,0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14,0x6f38e884725a116C9C7fBF208e79FE8828a2595F);
        _add(2672, true, 0,23,0, 0x2f5e87C9312fa29aed5c179E456625D79015299c,0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14,0x6f38e884725a116C9C7fBF208e79FE8828a2595F);
        _add(129, false, 1,0,0, 0xBfCa4230115DE8341F3A3d5e8845fFb3337B2Be3,0xB08a8794A5D3cCCA3725D92964696858d3201909,0x0000000000000000000000000000000000000000);
        _add(3286, true, 9,23,23, 0xd845f7D4f4DeB9Ff5bCf09D140Ef13718F6f6C71,0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14,0x1666EA6E83BCC3473111732Ef086BA47e6EDDeB7);
        _add(135, false, 9,0,0, 0xE461f84C3fE6BCDd1162Eb0Ef4284F3bB6e4CAD3,0xB08a8794A5D3cCCA3725D92964696858d3201909,0x0000000000000000000000000000000000000000);
        _add(89, false, 0,0,0, 0xdbaeB7f0DFe3a0AAFD798CCECB5b22E708f7852c,0xB08a8794A5D3cCCA3725D92964696858d3201909,0x0000000000000000000000000000000000000000);
    }

    function _tok(uint8 ex, address pool) internal view returns (address a, address b) {
        if (ex == DODO) return (IDodoP(pool)._BASE_TOKEN_(), IDodoP(pool)._QUOTE_TOKEN_());
        return (IV3(pool).token0(), IV3(pool).token1());
    }

    function _hop(uint8 ex, address tin, address tout, address pool) internal view returns (MantisExecutor.TradeHopV2 memory h) {
        h.exchangeId = ex; h.tokenIn = tin; h.tokenOut = tout;
        if (ex == DODO) h.pool = pool;
        else if (ex == 0 || ex == 6 || ex == 25) h.fee = IV3(pool).fee(); // V3-family needs fee tier
        // Algebra(9)/CamelotV2(1) routers resolve by token pair — no fee/pool needed
    }

    function test_ExecuteAll40EngineRoutes() public {
        uint256 ran; uint256 failed;
        string memory fails;
        for (uint256 i = 0; i < ids.length; i++) {
            MantisExecutor.TradeHopV2[3] memory route;
            (address q0a, address q0b) = _tok(e0[i], p0[i]);
            if (q0a != WETH && q0b != WETH) { failed++; fails = string.concat(fails, " ", vm.toString(ids[i]), "(no-WETH-entry)"); continue; }
            address A = q0a == WETH ? q0b : q0a;
            if (tri[i]) {
                (address m0, address m1) = _tok(e1[i], p1[i]); // DODO middle
                address B = (m0 == A) ? m1 : m0;
                route[0] = _hop(e0[i], WETH, A, p0[i]);
                route[1] = _hop(e1[i], A, B, p1[i]);
                route[2] = _hop(e2[i], B, WETH, p2[i]);
            } else {
                route[0] = _hop(e0[i], WETH, A, p0[i]);
                route[1] = _hop(e1[i], A, WETH, p1[i]);
            }
            try executor.flashLoanArbMixed(AMOUNT_IN, route, 0) {
                ran++;
            } catch (bytes memory data) {
                bytes4 sel; assembly { sel := mload(add(data, 0x20)) }
                if (sel == Loss.selector) { ran++; }
                else { failed++; fails = string.concat(fails, " ", vm.toString(ids[i]), "(sel=", vm.toString(sel), ")"); }
            }
        }
        emit log_named_uint("total engine routes", ids.length);
        emit log_named_uint("executed mechanically through all legs", ran);
        emit log_named_uint("mechanical failures", failed);
        emit log_string(string.concat("failed route_ids:", fails));
        // After the SushiV3 deadline-ABI fix, ALL 40 engine routes execute mechanically.
        assertEq(ran, ids.length, "every engine route must execute mechanically through Mantis");
    }
}
