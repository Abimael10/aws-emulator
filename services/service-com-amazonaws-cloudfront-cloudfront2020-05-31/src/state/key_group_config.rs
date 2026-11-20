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
//! * `KeyGroupConfig$Name` is "a name to identify the key group", and the name a group is
//!   identified by is bounded in length ([`super::MAX_KEY_GROUP_NAME`]) and built from the
//!   characters CloudFront admits in one ([`super::is_key_group_name`]): a longer one is
//!   refused as the parameter it arrived in ([`super::key_group_name_too_long`]), and one
//!   carrying anything outside that alphabet as the member it arrived in
//!   ([`super::key_group_name_illegal_characters`]);
//! * "To create a key group, you must specify at least one public key for the key group", and
//!   the model's `PublicKeyIdList` carries no length constraint, so a configuration naming
//!   none is refused here ([`super::key_group_without_a_public_key`]);
//! * "The comment cannot be longer than 128 characters", a documented bound the member
//!   carries no `@length` for ([`super::MAX_KEY_GROUP_COMMENT`]);
//! * the published quota on how many public keys ONE group HOLDS
//!   ([`super::MAX_PUBLIC_KEYS_IN_KEY_GROUP`]);
//! * every entry of `Items` names a public key THIS ACCOUNT HOLDS. `KeyGroupConfig$Items` is
//!   "a list of the identifiers of the public keys in the key group", and that list is what
//!   makes those keys the signers a cache behaviour trusting the group accepts — so an entry
//!   resolving to no stored public key names no signer, and the configuration is refused
//!   ([`super::absent_public_key_in_key_group`]) rather than stored holding a reference to a
//!   key that is not there. The reference is resolved against the public key records
//!   themselves, which is the same state `GetPublicKey` answers from and the same direction
//!   `DeletePublicKey` reads when it refuses to release a key a stored group still lists.
//!
//! Both handlers RESOLVE the submitted configuration before they judge it
//! ([`resolve_key_group_config`]): `Items` is the group's MEMBERSHIP, so a repeated id names
//! one member, is collapsed on the way in, and the group holds — and reports — each public key
//! once. The judgement is therefore about the keys the group would hold rather than about how
//! many times the payload mentioned them.

use super::*;

/// What a submitted key group configuration is refused with, named by the MODELLED error
/// shape the refusal carries.
///
/// `CreateKeyGroup` and `UpdateKeyGroup` each model both of these, so a handler maps a
/// variant onto its own operation's error enum and nothing here names one operation's channel.
#[derive(Debug)]
pub enum KeyGroupConfigRefusal {
    /// `InvalidArgument` (HTTP 400): the configuration is unusable as submitted — its name is
    /// longer than the service admits or carries a character it does not admit, it names no
    /// public key, its comment is past the documented bound, or it names a public key the
    /// account does not hold.
    InvalidArgument(crate::ops::types::InvalidArgument),
    /// `TooManyPublicKeysInKeyGroup` (HTTP 400): the configuration names more public keys
    /// than the published quota on one group's list.
    TooManyPublicKeysInKeyGroup(crate::ops::types::TooManyPublicKeysInKeyGroup),
}

/// The refusal `config` is unusable for, or `None` when this service stores it.
///
/// `config` is the RESOLVED configuration ([`resolve_key_group_config`]), so the counts below
/// are counts of what the group would hold.
///
/// Read-only: the judgement reads the public key records and nothing else, and is made BEFORE
/// anything is minted or written, so a refused create stores nothing and a refused update
/// leaves the stored group, its version token and its `LastModifiedTime` exactly as they were.
pub fn key_group_config_refusal(
    store: &ComAmazonawsCloudfrontCloudfront20200531State,
    config: &KeyGroupConfig,
) -> Option<KeyGroupConfigRefusal> {
    // The NAME first: it is the member that identifies the group, and a configuration whose
    // name CloudFront cannot identify a group by — too long, or built from characters it does
    // not admit — is refused on it whatever else the configuration states.
    if config.name.chars().count() > MAX_KEY_GROUP_NAME {
        return Some(KeyGroupConfigRefusal::InvalidArgument(
            key_group_name_too_long(),
        ));
    }
    if !is_key_group_name(&config.name) {
        return Some(KeyGroupConfigRefusal::InvalidArgument(
            key_group_name_illegal_characters(),
        ));
    }

    if config.items.is_empty() {
        return Some(KeyGroupConfigRefusal::InvalidArgument(
            key_group_without_a_public_key(),
        ));
    }

    let comment_length = config.comment.as_deref().unwrap_or_default().chars().count();
    if comment_length > MAX_KEY_GROUP_COMMENT {
        return Some(KeyGroupConfigRefusal::InvalidArgument(comment_too_long()));
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

/// Resolve the members the SERVICE supplies on an admitted `KeyGroupConfig`, so the record a
/// create or an update stores is the configuration the key group runs under rather than the
/// document that was submitted.
///
/// `KeyGroupConfig$Items` is "a list of the identifiers of the public keys in the key group":
/// the MEMBERSHIP of the group, so an id names the same public key however many times a
/// payload repeats it and the group holds that key ONCE. Repeated ids are collapsed here, on
/// the way IN, with the first mention of each id keeping its place — the list is reported in
/// the order it was submitted — so every readback of the group reports each of its keys once,
/// a count a client does over `Items` is the number of signers the group names, and a
/// read-modify-write round trip that re-submits a readback changes nothing. ONE resolution for
/// both calls that write a whole configuration, as [`key_group_config_refusal`] is one
/// judgement for both.
pub fn resolve_key_group_config(mut config: KeyGroupConfig) -> KeyGroupConfig {
    let mut held: BTreeSet<String> = BTreeSet::new();
    config
        .items
        .retain(|public_key_id| held.insert(public_key_id.clone()));
    config
}
