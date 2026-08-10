// Copyright (c) LightPool Labs
// Author: xiaoyu1998

use lightpool_crypto::{Digest, PublicKey, Signature};
use lightpool_types::module_types::bridge::{BridgeDepositMessage, BridgeVote};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BridgeVoteKind {
    Deposit,
    WithdrawRequest,
    WithdrawCancel,
    CommitteeUpdate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeLinkVote {
    pub kind: BridgeVoteKind,
    pub message_id: u64,
    pub digest: Digest,
    pub epoch: u64,
    pub validator: PublicKey,
    pub signature: Signature,
}

impl BridgeLinkVote {
    pub fn from_deposit(
        message: &BridgeDepositMessage,
        validator: PublicKey,
        secret: &lightpool_crypto::SecretKey,
    ) -> Self {
        let digest = Digest::from_data(message);
        let signature = Signature::new(&digest, secret);
        Self {
            kind: BridgeVoteKind::Deposit,
            message_id: message.message_id,
            digest,
            epoch: message.epoch,
            validator,
            signature,
        }
    }

    pub fn into_bridge_vote(self) -> BridgeVote {
        BridgeVote {
            validator: self.validator,
            signature: self.signature,
        }
    }
}
