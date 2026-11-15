// SPDX-License-Identifier: CC0-1.0

//! The KMS key a request named, as the ARN a read of the resource reports it by.
//!
//! Every member of this service that takes a customer managed key takes a `KmsKeyId`,
//! whose `@pattern` admits four spellings of one key — a key ARN, a bare 36-character key
//! id, `alias/<name>` and an alias ARN
//! (`smithy_query(query="shape",shape="KmsKeyId")`) — while every member a read reports
//! the key through is a `KmsKeyArn`, whose `@pattern` admits
//! `arn:aws…:kms:<region>:<account>:key/<id>` and NOTHING ELSE
//! (`smithy_query(query="shape",shape="KmsKeyArn")`). So the value a read reports is the
//! ARN of the KEY the request named, whichever of those four spellings named it: the
//! member has no representation for an alias, and reporting nothing for a key the request
//! did name would leave a caller unable to tell a resource encrypted with its own key from
//! one encrypted with the service's.
//!
//! Which key an ALIAS names is a fact only AWS KMS holds — "an alias is a friendly name
//! for a KMS key" and the alias-to-key association lives in that service — so it is asked
//! of this emulator's own KMS, through that service's OWN resolver
//! ([`kms_store::ComAmazonawsKmsTrentserviceState::resolve`], the very lookup every KMS
//! operation taking a `KeyId` resolves its member through). An alias KMS holds no key for
//! resolves to nothing, and so is reported as no key at all rather than as an invented ARN
//! or as the alias's own ARN, which the member could not carry.
//!
//! A key ARN is already the ARN of the key it names, so it is reported as given — including
//! one of another account, whose key this emulator's KMS does not hold and about which it
//! has nothing to say. A bare key id names a key of the Region the request addressed, whose
//! ARN is that Region's key ARN for the id, the same ARN KMS mints for a key it issued
//! ([`kms_store::key_arn`]).
//!
//! LOCKING: this takes the KMS store's lock and no other, and nothing in KMS reaches into
//! this service's store, so the two can never be taken in opposite orders. Callers ask it
//! before taking this service's own guard all the same, so one request holds one lock.

use ::service_com_amazonaws_kms_trentservice::state as kms_store;

/// How many characters a KMS key id has: `KmsKeyId`'s `@pattern` spells a bare one as
/// `[a-zA-Z0-9-]{36}`, the length of the version-4 UUID KMS issues.
const KEY_ID_LENGTH: usize = 36;

/// The `key/` resource prefix of a KMS key ARN, as `KmsKeyArn`'s `@pattern` spells it.
const KEY_ARN_RESOURCE_PREFIX: &str = ":key/";

/// The ARN a read reports for the `KmsKeyId` a request named (`kms_key_id`), with `region`
/// the Region the request addressed.
///
/// `None` when the request named no key, and when it named an alias this emulator's KMS
/// holds no key for: the member is modelled optional, and there is then no key ARN to
/// report.
pub fn kms_key_arn<Eng: Send + Sync + 'static>(
    engine: &Eng,
    kms_key_id: Option<&str>,
    region: &str,
) -> Option<String> {
    let requested = kms_key_id?;
    if let Some(arn) = named_key_arn(requested, region) {
        return Some(arn);
    }
    // Everything else the member admits names the key through an ALIAS, which only KMS can
    // resolve to a key.
    alias_target_key_arn(engine, region, requested)
}

/// The key ARN a spelling that names the KEY ITSELF carries: a key ARN as given, and a bare
/// key id completed into the key ARN of `region`.
///
/// `None` for a spelling that names the key through an alias, and for a value in neither
/// form — the member's own `@pattern` admits nothing else.
fn named_key_arn(requested: &str, region: &str) -> Option<String> {
    if requested.starts_with("arn:") {
        return requested
            .contains(KEY_ARN_RESOURCE_PREFIX)
            .then(|| requested.to_string());
    }
    let names_a_key_id = requested.len() == KEY_ID_LENGTH
        && requested
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
    names_a_key_id.then(|| kms_store::key_arn(region, requested))
}

/// The ARN of the key the alias `reference` targets, asked of this emulator's own AWS KMS.
///
/// `None` when no KMS store is mounted on this engine, and when KMS holds no key for the
/// alias: nothing is invented for a key the service that owns keys does not have.
fn alias_target_key_arn<Eng: Send + Sync + 'static>(
    engine: &Eng,
    region: &str,
    reference: &str,
) -> Option<String> {
    let state = kms_store::service_state(engine)?;
    let store = state.lock();
    match store.resolve(region, reference) {
        kms_store::KeyRef::Found(key) => Some(key.arn.clone()),
        kms_store::KeyRef::NotFound
        | kms_store::KeyRef::MalformedArn
        | kms_store::KeyRef::UnauthorizedAccount => None,
    }
}
