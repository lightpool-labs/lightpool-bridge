// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

struct Committee {
    uint64 epoch;
    address[] validators;
    uint64[] stakes;
}

library CommitteeLib {
    function hash(Committee memory c) internal pure returns (bytes32) {
        return keccak256(abi.encode(c.epoch, c.validators, c.stakes));
    }

    function totalStake(Committee memory c) internal pure returns (uint64) {
        uint64 sum = 0;
        for (uint256 i = 0; i < c.stakes.length; i++) {
            sum += c.stakes[i];
        }
        return sum;
    }

    function quorumThreshold(uint64 total) internal pure returns (uint64) {
        return (2 * total) / 3 + 1;
    }
}
