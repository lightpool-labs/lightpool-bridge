// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Committee, CommitteeLib} from "./Committee.sol";

/// @title LightPool EVM Bridge Hub
/// @notice Lock/unlock multiple ERC20 tokens with hot-validator quorum.
contract Bridge {
    using CommitteeLib for Committee;

    struct Signature {
        uint8 v;
        bytes32 r;
        bytes32 s;
    }

    struct WithdrawRequest {
        bytes32 id;
        address user;
        address destination;
        address token;
        uint64 amount;
        uint64 nonce;
        uint64 epoch;
    }

    struct PendingWithdraw {
        address user;
        address destination;
        address token;
        uint64 amount;
        uint64 requestedTime;
        uint64 requestedBlock;
        bool exists;
    }

    struct PendingCommitteeUpdate {
        uint64 epoch;
        bytes32 committeeHash;
        address[] validators;
        uint64[] stakes;
        uint64 requestedTime;
        uint64 requestedBlock;
        bool exists;
    }

    bytes32 public committeeHash;
    uint64 public epoch;
    uint64 public totalStake;
    uint64 public disputePeriodSeconds;
    uint64 public blockDurationMillis;
    address public immutable deployer;

    mapping(address => bool) public registeredTokens;
    mapping(address => uint64) public nextDepositId;

    mapping(address => bool) public finalizers;
    mapping(bytes32 => PendingWithdraw) public pendingWithdrawals;
    mapping(bytes32 => bool) public finalizedWithdrawals;
    mapping(bytes32 => bool) public cancelledWithdrawals;
    mapping(bytes32 => bool) public usedMessages;

    PendingCommitteeUpdate public pendingCommitteeUpdate;

    event TokenRegistered(address indexed token);
    event DepositInitiated(
        uint64 indexed depositId,
        address indexed sender,
        address indexed recipient,
        address token,
        uint64 amount,
        uint64 sourceBlock
    );
    event WithdrawRequested(bytes32 indexed id, address user, address destination, uint64 amount);
    event WithdrawCancelled(bytes32 indexed id);
    event WithdrawFinalized(bytes32 indexed id, address destination, uint64 amount);
    event CommitteeUpdateRequested(uint64 indexed epoch, bytes32 committeeHash);
    event CommitteeUpdateCancelled(uint64 indexed epoch);
    event CommitteeUpdateFinalized(uint64 indexed epoch, bytes32 committeeHash);

    error InvalidCommittee();
    error InvalidSignatures();
    error StillInDispute();
    error DisputeEnded();
    error NotPending();
    error AlreadyProcessed();
    error NotFinalizer();
    error TransferFailed();
    error BadAmount();
    error TokenNotRegistered();
    error NotDeployer();

    modifier onlyDeployer() {
        if (msg.sender != deployer) revert NotDeployer();
        _;
    }

    constructor(
        Committee memory genesis,
        uint64 disputePeriodSeconds_,
        uint64 blockDurationMillis_,
        address[] memory finalizerList
    ) {
        require(genesis.validators.length == genesis.stakes.length, "len");
        require(genesis.validators.length > 0, "empty");
        deployer = msg.sender;
        committeeHash = genesis.hash();
        epoch = genesis.epoch;
        totalStake = genesis.totalStake();
        disputePeriodSeconds = disputePeriodSeconds_;
        blockDurationMillis = blockDurationMillis_;
        for (uint256 i = 0; i < finalizerList.length; i++) {
            finalizers[finalizerList[i]] = true;
        }
    }

    function registerToken(address token) external onlyDeployer {
        require(token != address(0), "token");
        registeredTokens[token] = true;
        emit TokenRegistered(token);
    }

    function deposit(address token, uint64 amount, address lightpoolRecipient) external {
        if (!registeredTokens[token]) revert TokenNotRegistered();
        if (amount == 0) revert BadAmount();
        if (!transferFrom(token, msg.sender, address(this), amount)) revert TransferFailed();
        uint64 depositId = ++nextDepositId[token];
        emit DepositInitiated(
            depositId,
            msg.sender,
            lightpoolRecipient,
            token,
            amount,
            uint64(block.number)
        );
    }

    function requestWithdraw(
        WithdrawRequest calldata req,
        Committee calldata active,
        Signature[] calldata signatures
    ) external {
        _checkCommittee(active);
        if (!registeredTokens[req.token]) revert TokenNotRegistered();
        if (pendingWithdrawals[req.id].exists || finalizedWithdrawals[req.id]
            || cancelledWithdrawals[req.id]) {
            revert AlreadyProcessed();
        }
        bytes32 digest = keccak256(
            abi.encode(
                "requestWithdraw",
                req.id,
                req.user,
                req.destination,
                req.token,
                req.amount,
                req.nonce,
                req.epoch
            )
        );
        _checkQuorum(digest, active, signatures);
        pendingWithdrawals[req.id] = PendingWithdraw({
            user: req.user,
            destination: req.destination,
            token: req.token,
            amount: req.amount,
            requestedTime: uint64(block.timestamp),
            requestedBlock: uint64(block.number),
            exists: true
        });
        emit WithdrawRequested(req.id, req.user, req.destination, req.amount);
    }

    function cancelWithdraw(
        bytes32 id,
        Committee calldata active,
        Signature[] calldata signatures
    ) external {
        _checkCommittee(active);
        PendingWithdraw storage p = pendingWithdrawals[id];
        if (!p.exists) revert NotPending();
        if (!_inDispute(p.requestedTime, p.requestedBlock)) revert DisputeEnded();
        bytes32 digest = keccak256(abi.encode("cancelWithdraw", id, active.epoch));
        _checkQuorum(digest, active, signatures);
        delete pendingWithdrawals[id];
        cancelledWithdrawals[id] = true;
        emit WithdrawCancelled(id);
    }

    function finalizeWithdraw(bytes32 id) external {
        if (!finalizers[msg.sender]) revert NotFinalizer();
        PendingWithdraw storage p = pendingWithdrawals[id];
        if (!p.exists) revert NotPending();
        if (_inDispute(p.requestedTime, p.requestedBlock)) revert StillInDispute();
        address destination = p.destination;
        address token = p.token;
        uint64 amount = p.amount;
        delete pendingWithdrawals[id];
        finalizedWithdrawals[id] = true;
        if (!transfer(token, destination, amount)) revert TransferFailed();
        emit WithdrawFinalized(id, destination, amount);
    }

    function requestCommitteeUpdate(
        Committee calldata next,
        Committee calldata active,
        Signature[] calldata signatures
    ) external {
        _checkCommittee(active);
        if (next.epoch <= active.epoch) revert InvalidCommittee();
        if (next.validators.length != next.stakes.length || next.validators.length == 0) {
            revert InvalidCommittee();
        }
        if (pendingCommitteeUpdate.exists) revert AlreadyProcessed();
        bytes32 digest = keccak256(
            abi.encode("requestCommitteeUpdate", next.epoch, next.validators, next.stakes)
        );
        _checkQuorum(digest, active, signatures);
        pendingCommitteeUpdate = PendingCommitteeUpdate({
            epoch: next.epoch,
            committeeHash: next.hash(),
            validators: next.validators,
            stakes: next.stakes,
            requestedTime: uint64(block.timestamp),
            requestedBlock: uint64(block.number),
            exists: true
        });
        emit CommitteeUpdateRequested(next.epoch, next.hash());
    }

    function cancelCommitteeUpdate(Committee calldata active, Signature[] calldata signatures)
        external
    {
        _checkCommittee(active);
        if (!pendingCommitteeUpdate.exists) revert NotPending();
        if (
            !_inDispute(
                pendingCommitteeUpdate.requestedTime, pendingCommitteeUpdate.requestedBlock
            )
        ) {
            revert DisputeEnded();
        }
        bytes32 digest =
            keccak256(abi.encode("cancelCommitteeUpdate", pendingCommitteeUpdate.epoch));
        _checkQuorum(digest, active, signatures);
        uint64 cancelledEpoch = pendingCommitteeUpdate.epoch;
        delete pendingCommitteeUpdate;
        emit CommitteeUpdateCancelled(cancelledEpoch);
    }

    function finalizeCommitteeUpdate() external {
        if (!finalizers[msg.sender]) revert NotFinalizer();
        if (!pendingCommitteeUpdate.exists) revert NotPending();
        if (
            _inDispute(
                pendingCommitteeUpdate.requestedTime, pendingCommitteeUpdate.requestedBlock
            )
        ) {
            revert StillInDispute();
        }
        committeeHash = pendingCommitteeUpdate.committeeHash;
        epoch = pendingCommitteeUpdate.epoch;
        uint64 sum = 0;
        for (uint256 i = 0; i < pendingCommitteeUpdate.stakes.length; i++) {
            sum += pendingCommitteeUpdate.stakes[i];
        }
        totalStake = sum;
        uint64 finalizedEpoch = pendingCommitteeUpdate.epoch;
        bytes32 finalizedHash = pendingCommitteeUpdate.committeeHash;
        delete pendingCommitteeUpdate;
        emit CommitteeUpdateFinalized(finalizedEpoch, finalizedHash);
    }

    function _checkCommittee(Committee calldata active) internal view {
        if (active.hash() != committeeHash || active.epoch != epoch) revert InvalidCommittee();
    }

    function _inDispute(uint64 requestedTime, uint64 requestedBlock) internal view returns (bool) {
        if (block.timestamp <= requestedTime + disputePeriodSeconds) {
            return true;
        }
        uint64 elapsedBlocks = uint64(block.number) - requestedBlock;
        if (elapsedBlocks * blockDurationMillis <= 1000 * disputePeriodSeconds) {
            return true;
        }
        return false;
    }

    function _checkQuorum(
        bytes32 digest,
        Committee calldata active,
        Signature[] calldata signatures
    ) internal {
        bytes32 ethSigned = keccak256(abi.encodePacked("\x19Ethereum Signed Message:\n32", digest));
        uint64 power = 0;
        address last = address(0);
        for (uint256 i = 0; i < signatures.length; i++) {
            address signer =
                ecrecover(ethSigned, signatures[i].v, signatures[i].r, signatures[i].s);
            if (signer == address(0) || signer <= last) revert InvalidSignatures();
            bool found = false;
            uint64 stake = 0;
            for (uint256 j = 0; j < active.validators.length; j++) {
                if (active.validators[j] == signer) {
                    found = true;
                    stake = active.stakes[j];
                    break;
                }
            }
            if (!found) revert InvalidSignatures();
            power += stake;
            last = signer;
        }
        if (power < CommitteeLib.quorumThreshold(totalStake)) revert InvalidSignatures();
        if (usedMessages[digest]) revert AlreadyProcessed();
        usedMessages[digest] = true;
    }

    function transferFrom(address token, address from, address to, uint64 amount)
        internal
        returns (bool)
    {
        (bool ok, bytes memory data) = token.call(
            abi.encodeWithSelector(0x23b872dd, from, to, uint256(amount))
        );
        return ok && (data.length == 0 || abi.decode(data, (bool)));
    }

    function transfer(address token, address to, uint64 amount) internal returns (bool) {
        (bool ok, bytes memory data) =
            token.call(abi.encodeWithSelector(0xa9059cbb, to, uint256(amount)));
        return ok && (data.length == 0 || abi.decode(data, (bool)));
    }
}
