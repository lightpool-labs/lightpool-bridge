// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Script, console2} from "forge-std/Script.sol";
import {MockUSDT} from "../src/MockUSDT.sol";
import {Bridge} from "../src/Bridge.sol";
import {Committee} from "../src/Committee.sol";

contract DeployLocal is Script {
    function run() external {
        address deployer = msg.sender;
        address validator = vm.envOr("VALIDATOR_ETH", deployer);
        uint64 validatorStake = uint64(vm.envOr("VALIDATOR_STAKE", uint256(100)));

        address[] memory validators = new address[](1);
        validators[0] = validator;
        uint64[] memory stakes = new uint64[](1);
        stakes[0] = validatorStake;
        Committee memory genesis =
            Committee({epoch: 0, validators: validators, stakes: stakes});

        vm.startBroadcast();
        MockUSDT usdt = new MockUSDT();
        Bridge bridge = new Bridge(
            genesis,
            5,
            1000,
            validators
        );
        bridge.registerToken(address(usdt));

        uint256 makerAmount = 1_000_000_000_000e6;
        uint256 userAmount = 10_000e6;
        address user = vm.envOr(
            "USER_ETH",
            address(0xC019cECd52FE1f68b53daf766c4aF0Dea667A2c7)
        );

        usdt.mint(deployer, makerAmount);
        usdt.mint(validator, makerAmount);
        if (user != address(0) && user != deployer && user != validator) {
            usdt.mint(user, userAmount);
        }

        vm.stopBroadcast();

        console2.log("USDT", address(usdt));
        console2.log("BRIDGE", address(bridge));
        console2.log("DEPLOYER", deployer);
        console2.log("VALIDATOR_ETH", validator);
        console2.log("USER_ETH", user);
        console2.log("MAKER_USDT_MINTED", makerAmount);
        console2.log("USER_USDT_MINTED", userAmount);
    }
}
