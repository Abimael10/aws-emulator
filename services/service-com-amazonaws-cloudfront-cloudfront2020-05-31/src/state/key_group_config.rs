// SPDX-License-Identifier: CC0-1.0

//! The judgement a submitted key group CONFIGURATION is admitted by.
//!
//! Every rule below states something about a `KeyGroupConfig` rather than about the call that
//! carries one, so it holds on every payload that states a configuration: `CreateKeyGroup`
//! and `UpdateKeyGroup` both submit one whole, and an update cannot store a configuration a
//! create would have refused. They live here, in ONE judgement both handlers make
//! ([`key_group_config_refusal`]), so the two cannot drift apart; each handler lifts the
//! refusal it hands back into its own modelled error channel.
//!
//! The rules:
//!
//! * "To create a key group, you must specify at least one public key for the key group", and
//!   the model's `PublicKeyIdList` carries no length constraint, so a configuration naming
//!   none is refused here ([`super::key_group_without_a_public_key`]);
//! * "The comment cannot be longer than 128 characters", a documented bound the member
//!   carries no `@length` for ([`super::MAX_KEY_GROUP_COMMENT`]);
//! * the published quota on how many public keys ONE group names
//!   ([`super::MAX_PUBLIC_KEYS_IN_KEY_GROUP`]);
//! * every entry of `Items` names a public key THIS ACCOUNT HOLDS. `KeyGroupConfig$Items` is
//!   "a list of the identifiers of the public keys in the key group", and that list is what
//!   makes those keys the signers a cache behaviour trusting the group accepts — so an entry
//!   resolving to no stored public key names no signer, and the configuration is refused
//!   ([`super::absent_public_key_in_key_group`]) rather than stored holding a reference to a
//!   key that is not there. The reference is resolved against the public key records
//!   themselves, which is the same state `GetPublicKey` answers from and the same direction
//!   `DeletePublicKey` reads when it refuses to release a key a stored group still lists.

use super::*;

/// What a submitted key group configuration is refused with, named by the MODELLED error
/// shape the refusal carries.
///
/// `CreateKeyGroup` and `UpdateKeyGroup` each model both of these, so a handler maps a
/// variant onto its own operation's error enum and nothing here names one operation's channel.
#[derive(Debug)]
pub enum KeyGroupConfigRefusal {
    /// `InvalidArgument` (HTTP 400): the configuration is unusable as submitted — it names no
    /// public key, its comment is past the documented bound, or it names a public key the
    /// account does not hold.
    InvalidArgument(crate::ops::types::InvalidArgument),
    /// `TooManyPublicKeysInKeyGroup` (HTTP 400): the configuration names more public keys
    /// than the published quota on one group's list.
    TooManyPublicKeysInKeyGroup(crate::ops::types::TooManyPublicKeysInKeyGroup),
}

/// The refusal `config` is unusable for, or `None` when this service stores it as submitted.
///
/// Read-only: the judgement reads the public key records and nothing else, and is made BEFORE
/// anything is minted or written, so a refused create stores nothing and a refused update
/// leaves the stored group, its version token and its `LastModifiedTime` exactly as they were.
pub fn key_group_config_refusal(
    store: &ComAmazonawsCloudfrontCloudfront20200531State,
    config: &KeyGroupConfig,
) -> Option<KeyGroupConfigRefusal> {
    if config.items.is_empty() {
        return Some(KeyGroupConfigRefusal::InvalidArgument(
            key_group_without_a_public_key(),
        ));
    }

    let comment_length = config.comment.as_deref().unwrap_or_default().chars().count();
    if comment_length > MAX_KEY_GROUP_COMMENT {
        return Some(KeyGroupConfigRefusal::InvalidArgument(comment_too_long(
            comment_length,
        )));
    }

    if config.items.len() > MAX_PUBLIC_KEYS_IN_KEY_GROUP {
        return Some(KeyGroupConfigRefusal::TooManyPublicKeysInKeyGroup(
            too_many_public_keys_in_key_group(),
        ));
    }

    // A public key id borne by a submitted DOCUMENT addresses nothing on its own, so it is
    // resolved against the store whatever its shape — the entry names a key the account holds
    // or it names nothing, whichever position of the list carries it.
    if config
        .items
        .iter()
        .any(|public_key_id| store.public_key(public_key_id).is_none())
    {
        return Some(KeyGroupConfigRefusal::InvalidArgument(
            absent_public_key_in_key_group(),
        ));
    }

    None
}
