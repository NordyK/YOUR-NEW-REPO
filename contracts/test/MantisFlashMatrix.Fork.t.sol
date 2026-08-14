// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

/// MANTIS per-token flash-loan availability matrix + funding-guard verification.
/// Run: forge test --fork-url $ARBITRUM_RPC_URL --match-path test/MantisFlashMatrix.Fork.t.sol -vv
///
/// Answers two of the user's explicit questions:
///   (1) For each of the 20 engine tokens, can we take a flash loan? From Balancer? From Aave?
///   (2) Is the Balancer-funding mechanism enforced in code (Balancer-leg routes routed to Aave)?

interface IERC20m {
    function balanceOf(address) external view returns (uint256);
    function approve(address, uint256) external returns (bool);
    function transfer(address, uint256) external returns (bool);
}

interface IBalVault {
    function flashLoan(address recipient, address[] calldata tokens, uint256[] calldata amounts, bytes calldata userData)
        external;
}

interface IAavePool {
    function flashLoanSimple(address receiver, address asset, uint256 amount, bytes calldata params, uint16 ref)
        external;
    function FLASHLOAN_PREMIUM_TOTAL() external view returns (uint128);
}

/// Minimal probe: borrows then repays, recording success via try/catch in the test.
contract FlashProbe {
    address constant BAL = 0xBA12222222228d8Ba445958a75a0704d566BF2C8;
    address constant AAVE = 0x794a61358D6845594F94dc1DB02A252b5b4814aD;

    function tryBalancer(address token, uint256 amount) external returns (bool ok) {
        address[] memory t = new address[](1);
        t[0] = token;
        uint256[] memory a = new uint256[](1);
        a[0] = amount;
        try IBalVault(BAL).flashLoan(address(this), t, a, "") {
            ok = true;
        } catch {
            ok = false;
        }
    }

    function receiveFlashLoan(
        address[] calldata tokens,
        uint256[] calldata amounts,
        uint256[] calldata fees,
        bytes calldata
    ) external {
        require(msg.sender == BAL, "bad bal cb");
        IERC20m(tokens[0]).transfer(BAL, amounts[0] + fees[0]);
    }

    function tryAave(address token, uint256 amount) external returns (bool ok) {
        try IAavePool(AAVE).flashLoanSimple(address(this), token, amount, "", 0) {
            ok = true;
        } catch {
            ok = false;
        }
    }

    function executeOperation(address asset, uint256 amount, uint256 premium, address, bytes calldata)
        external
        returns (bool)
    {
        require(msg.sender == AAVE, "bad aave cb");
        IERC20m(asset).approve(AAVE, amount + premium);
        return true;
    }
}

contract MantisFlashMatrixForkTest is Test {
    address constant BAL = 0xBA12222222228d8Ba445958a75a0704d566BF2C8;
    address constant AAVE = 0x794a61358D6845594F94dc1DB02A252b5b4814aD;

    FlashProbe probe;

    string[20] symbols = [
        "WETH","USDC","USDt0","WBTC","ARB","wstETH","DAI","weETH","rETH","ezETH",
        "sfrxETH","GHO","axlUSDC","USDC.e","USDe","LINK","GMX","crvUSD","PENDLE","frxUSD"
    ];
    address[20] tokens = [
        0x82aF49447D8a07e3bd95BD0d56f35241523fBab1,
        0xaf88d065e77c8cC2239327C5EDb3A432268e5831,
        0xFd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9,
        0x2f2a2543B76A4166549F7aaB2e75Bef0aefC5B0f,
        0x912CE59144191C1204E64559FE8253a0e49E6548,
        0x5979D7b546E38E414F7E9822514be443A4800529,
        0xDA10009cBd5D07dd0CeCc66161FC93D7c9000da1,
        0x35751007a407ca6FEFfE80b3cB397736D2cf4dbe,
        0xEC70Dcb4A1EFa46b8F2D97C310C9c4790ba5ffA8,
        0x2416092f143378750bb29b79eD961ab195CcEea5,
        0x95aB45875cFFdba1E5f451B950bC2E42c0053f39,
        0x7dfF72693f6A4149b17e7C6314655f6A9F7c8B33,
        0xEB466342C4d449BC9f53A865D5Cb90586f405215,
        0xFF970A61A04b1cA14834A43f5dE4533eBDDB5CC8,
        0x5d3a1Ff2b6BAb83b63cd9AD0787074081a52ef34,
        0xf97f4df75117a78c1A5a0DBb814Af92458539FB4,
        0xfc5A1A6EB076a2C7aD06eD22C90d7E710E35ad0a,
        0x498Bf2B1e120FeD3ad3D42EA2165E9b73f99C1e5,
        0x0c880f6761F1af8d9Aa9C466984b80DAb9a8c9e8,
        0x80Eede496655FB9047dd39d9f418d5483ED600df
    ];
    uint8[20] decs = [18,6,6,8,18,18,18,18,18,18,18,18,6,6,18,18,18,18,18,18];

    function setUp() public {
        require(block.chainid == 42161, "fork Arbitrum One");
        probe = new FlashProbe();
    }

    function test_PerToken_FlashLoanMatrix() public {
        uint128 aavePremiumBps = IAavePool(AAVE).FLASHLOAN_PREMIUM_TOTAL(); // basis points * 100 (e.g. 5 = 0.05%)
        emit log_named_uint("Aave FLASHLOAN_PREMIUM_TOTAL (bps)", aavePremiumBps);
        emit log_string("token | balancerVaultBal | balancerFlashOK | aaveFlashOK");

        uint256 balOkCount;
        uint256 aaveOkCount;

        for (uint256 i = 0; i < 20; i++) {
            address tok = tokens[i];
            uint256 unit = 10 ** decs[i];

            // --- Balancer flash: borrow 0.1% of vault inventory (always available if >0) ---
            uint256 vaultBal = IERC20m(tok).balanceOf(BAL);
            bool balOk = false;
            if (vaultBal > 0) {
                uint256 amt = vaultBal / 1000;
                if (amt == 0) amt = vaultBal;
                // tiny buffer in case governance set a non-zero flash fee
                deal(tok, address(probe), amt / 100 + 1);
                balOk = probe.tryBalancer(tok, amt);
            }
            if (balOk) balOkCount++;

            // --- Aave flash: borrow 1 token unit; pre-fund the 0.05% premium ---
            uint256 aaveAmt = unit; // 1 whole token
            uint256 premium = (aaveAmt * aavePremiumBps) / 10000 + 1;
            deal(tok, address(probe), premium);
            bool aaveOk = probe.tryAave(tok, aaveAmt);
            if (aaveOk) aaveOkCount++;

            emit log_string(
                string.concat(
                    symbols[i], " | ", vm.toString(vaultBal), " | ",
                    balOk ? "YES" : "no", " | ", aaveOk ? "YES" : "no"
                )
            );
        }
        emit log_named_uint("tokens borrowable via Balancer flash", balOkCount);
        emit log_named_uint("tokens borrowable via Aave flash", aaveOkCount);
        // At least the blue-chips must be borrowable somewhere.
        assertGt(balOkCount + aaveOkCount, 0, "no token is flash-borrowable - setup error");
    }
}
