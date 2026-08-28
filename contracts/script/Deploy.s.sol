// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Bridge} from "../src/Bridge.sol";
import {Committee} from "../src/Committee.sol";

contract DeployBridge {
    function deploy(
        address[] memory validators,
        uint64[] memory stakes,
        uint64 disputePeriodSeconds,
        uint64 blockDurationMillis
    ) external returns (Bridge) {
        Committee memory genesis =
            Committee({epoch: 0, validators: validators, stakes: stakes});
        return new Bridge(genesis, disputePeriodSeconds, blockDurationMillis, validators);
    }

    function registerToken(Bridge bridge, address token) external {
        bridge.registerToken(token);
    }
}
