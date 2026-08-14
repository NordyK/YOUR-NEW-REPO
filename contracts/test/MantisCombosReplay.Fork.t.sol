// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

/// CROSS-VENUE / CROSS-DEX combo replay. The per-family test (MantisFamilyRealizedPnL) proves our
/// fork reproduces each single AMM family. This proves it reproduces the dominant **multi-venue
/// combinations** from the catalog (the 72 distinct dex-combos), including 3-venue and 4-hop arbs:
/// for each top combo we `vm.createFork(rpc, txHash)` + `vm.transact(txHash)` the real arb tx. If our
/// fork could not execute any venue in the combo, the transact would revert. All must reproduce.
contract MantisCombosReplayForkTest is Test {
    struct Combo { string name; bytes32 txHash; uint8 hops; }

    function _combos() internal pure returns (Combo[] memory c) {
        c = new Combo[](12);
        c[0]  = Combo("pancakeswap+uniswap",            0x49fe181b6dd857ca871976ea2f3619acc41dae20858764bdf6cdb91c5962f6b0, 3);
        c[1]  = Combo("camelot+uniswap",                0xd7b25bba082cb2d9afce3a3ec7b882482f976bedb73df2883e9ed4e108a52557, 2);
        c[2]  = Combo("camelot+pancakeswap",            0xa942fbe39f8ac8957463b26938c578f83db38e65a0c9ab6c680951c78c7ec0c1, 2);
        c[3]  = Combo("camelot+pancakeswap+uniswap",    0x6e70d6aa63b3cd11bb0a2b24255eaa4d534582bce8262d7db7c4d5f481f01b2e, 3);
        c[4]  = Combo("sushiswap+uniswap",              0x6050213341b9653003913aa442898a13c61fcfd5b5b932ff86437e5599bed4d5, 2);
        c[5]  = Combo("fluid+uniswap",                  0xbb4607199ca151974147b971ee69194102687fcfbddc1bcad594caa99a9fd166, 2);
        c[6]  = Combo("pancakeswap+sushiswap+uniswap",  0xe3f13cf775a96131a768168fd348e5e5c3e5903cb91eafd775befce3c8faa9dd, 4);
        c[7]  = Combo("balancer+camelot",               0x065fe8afd83c9807958dfc41e932e7d9eaea2df3f08be5dd8a55d2544fc2cbc8, 2);
        c[8]  = Combo("trader_joe+uniswap",             0x0dc3abb2500465f0058f3f3c81804678fc9a5763ecb4f3cc78bb54cabac94ffa, 3);
        c[9]  = Combo("native+uniswap",                 0x316e38225120edab6cf7c5d85f02e638dc01018f064dc9552223e32f909fd377, 2);
        c[10] = Combo("dodo+pancakeswap+uniswap",       0x49b6e9aa9540d1feaf3b70ea7e79d426f0ee2be24204be92e95efebf9b05598e, 3);
        c[11] = Combo("maverick+uniswap",               0xc5854fc7bf10a402734e8430a3fb27c928ac47ed9fddf1aea679907387ce1190, 2);
    }

    function test_TopCombosReproduceOnFork() public {
        string memory rpc = vm.envString("ARBITRUM_RPC_URL");
        Combo[] memory cs = _combos();
        uint256 ok = 0;
        for (uint256 i = 0; i < cs.length; i++) {
            uint256 fk = vm.createFork(rpc, cs[i].txHash);
            vm.selectFork(fk);
            // Replays the real multi-venue arb tx; reverts here would mean our fork can't execute
            // some venue in the combo. It succeeded on-chain, so a faithful fork must reproduce it.
            vm.transact(cs[i].txHash);
            emit log_named_string("combo reproduced", cs[i].name);
            emit log_named_uint("  hops", cs[i].hops);
            ok++;
        }
        emit log_named_uint("COMBOS REPRODUCED ON FORK", ok);
        assertEq(ok, cs.length, "every top dex-combo must reproduce on our fork");
    }
}
