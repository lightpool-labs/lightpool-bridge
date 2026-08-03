// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Bridge} from "../src/Bridge.sol";
import {Committee} from "../src/Committee.sol";

/// @dev Example deploy helper. Prefer `forge script` with your own broadcast wrapper,
/// or construct Bridge directly in tests.
contract DeployBridge {
    function deploy(
        address token,
        address[] memory validators,
        uint64[] memory stakes,
        uint64 disputePeriodSeconds,
        uint64 blockDurationMillis
    ) external returns (Bridge) {
        Committee memory genesis =
            Committee({epoch: 0, validators: validators, stakes: stakes});
        return new Bridge(token, genesis, disputePeriodSeconds, blockDurationMillis, validators);
    }
}
