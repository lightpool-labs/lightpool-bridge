// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Script, console2} from "forge-std/Script.sol";
import {MockUSDT} from "../src/MockUSDT.sol";
import {Bridge} from "../src/Bridge.sol";
import {Committee} from "../src/Committee.sol";

/// @dev Local deploy for reth --dev.
/// Set VALIDATOR_ETH to the LightPool validator eth address (same as LP wallet address).
/// Usage:
///   VALIDATOR_ETH=0x... forge script script/DeployLocal.s.sol:DeployLocal \
///     --rpc-url http://127.0.0.1:8545 --broadcast --private-key $PK
contract DeployLocal is Script {
    function run() external {
        address deployer = msg.sender;
        address validator = vm.envOr("VALIDATOR_ETH", deployer);

        address[] memory validators = new address[](1);
        validators[0] = validator;
        uint64[] memory stakes = new uint64[](1);
        stakes[0] = 100;
        Committee memory genesis =
            Committee({epoch: 0, validators: validators, stakes: stakes});

        vm.startBroadcast();
        MockUSDT usdt = new MockUSDT();
        Bridge bridge = new Bridge(
            address(usdt),
            genesis,
            5, // disputePeriodSeconds (short for local)
            1000, // blockDurationMillis (~1s blocks)
            validators
        );

        usdt.mint(deployer, 1_000_000e6);
        vm.stopBroadcast();

        console2.log("USDT", address(usdt));
        console2.log("BRIDGE", address(bridge));
        console2.log("DEPLOYER", deployer);
        console2.log("VALIDATOR_ETH", validator);
    }
}
