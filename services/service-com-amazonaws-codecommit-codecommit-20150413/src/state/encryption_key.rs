// SPDX-License-Identifier: CC0-1.0

//! Which KMS key a repository is encrypted under: the selector forms this service
//! accepts for one, and the resolution of a selector to the key it names.
//!
//! `RepositoryMetadata$kmsKeyId` reports "The ID of the Key Management Service
//! encryption key used to encrypt and decrypt the repository"
//! (`smithy_query(query="doc",shape="RepositoryMetadata",member="kmsKeyId")`) — the
//! KEY, not the selector a request happened to name it by. A request names its key
//! in any of the forms KMS documents for `KeyId` ("For more information about
//! acceptable values for kmsKeyID, see KeyId in the Decrypt API description in the
//! Key Management Service API Reference",
//! `smithy_query(query="doc",shape="CreateRepositoryInput",member="kmsKeyId")`): a key
//! id, a key ARN, an alias name or an alias ARN. An alias is a NAME for a key rather
//! than the key, and only KMS can say which key it stands for, so a selector is
//! resolved against the KMS store on the same shared engine ([`resolve_encryption_key`])
//! and the ARN of the key found is what a repository holds and every projection of it
//! reports.
//!
//! A request that names no key asks for the Amazon Web Services managed key of this
//! service ([`DEFAULT_ENCRYPTION_KEY_ALIAS`]), whose alias is a PREDEFINED Amazon Web
//! Services one: it "exists in every account and Region" carrying no target key until
//! something asks for it, when "KMS associates the alias with an Amazon Web Services
//! managed key" — so asking for it is what gives it a key, and every repository created
//! without a key in that account and Region resolves the alias to the SAME key.
//!
//! Only a WRITE resolves: the create and the rotation record the key they resolved, and
//! every read projects that record, so no read materialises a managed key or changes
//! what a later caller sees.

use super::*;

/// The number of hexadecimal digits after the `mrk-` prefix of the id KMS mints
/// for a multi-region key.
pub(super) const MULTI_REGION_KEY_ID_DIGITS: usize = 32;

/// Is this a KMS key id — a UUID for a single-region key, as every KMS `KeyId`
/// example is (`1234abcd-12ab-34cd-56ef-1234567890ab`), or `mrk-` and
/// [`MULTI_REGION_KEY_ID_DIGITS`] hexadecimal digits for a multi-region key?
///
/// Only the FORM of the selector is judged here — which key it names is KMS's
/// answer ([`resolve_encryption_key`]) — but that form is the id's two shapes
/// exactly, read through the same [`is_uuid`] every other UUID-shaped selector in
/// this module is read through.
pub(super) fn is_kms_key_id(candidate: &str) -> bool {
    if let Some(digits) = candidate.strip_prefix("mrk-") {
        return digits.len() == MULTI_REGION_KEY_ID_DIGITS
            && digits.bytes().all(|byte| byte.is_ascii_hexdigit());
    }
    is_uuid(candidate)
}

/// Is this a KMS alias path — `alias/` followed by a non-empty name of the
/// characters `KmsKeyId`'s modelled pattern `^[a-zA-Z0-9:/_-]+$` allows?
pub(super) fn is_kms_alias(candidate: &str) -> bool {
    candidate.strip_prefix("alias/").is_some_and(|name| {
        !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b":/_-".contains(&byte))
    })
}

/// The six fields of a KMS ARN — `arn:<partition>:kms:<region>:<account>:<resource>`
/// — for a value that IS one, and `None` for anything else.
///
/// The one reading of a KMS ARN this module has: [`is_kms_arn`] is this answering
/// `Some`, and [`encryption_key_region`] takes the Region out of the same fields, so
/// the two cannot disagree about what an ARN says. Read with the same six-field
/// `splitn` this module's [`repository_name_of_resource_arn`] uses, and adding only
/// which vendor and resource the value has to name.
fn kms_arn_fields(candidate: &str) -> Option<[&str; 6]> {
    let [prefix, partition, vendor, region, account, resource] =
        candidate.splitn(6, ':').collect::<Vec<_>>()[..]
    else {
        return None;
    };
    let well_formed = prefix == "arn"
        && !partition.is_empty()
        && vendor == "kms"
        && !region.is_empty()
        && account.len() == ACCOUNT_ID_DIGITS
        && account.bytes().all(|byte| byte.is_ascii_digit())
        && (is_kms_alias(resource) || resource.strip_prefix("key/").is_some_and(is_kms_key_id));
    well_formed.then_some([prefix, partition, vendor, region, account, resource])
}

/// Is this the full ARN of a KMS key or alias
/// (`arn:<partition>:kms:<region>:<account>:key/<key id>` or `…:alias/<name>`)?
pub(super) fn is_kms_arn(candidate: &str) -> bool {
    kms_arn_fields(candidate).is_some()
}

/// Does `key_id` select a KMS key at all?
///
/// `KmsKeyId`'s documentation sends a caller to KMS for the acceptable values —
/// "For more information about acceptable values for keyID, see KeyId in the
/// Decrypt API description in the Key Management Service API Reference"
/// (`smithy_query(query="doc",shape="UpdateRepositoryEncryptionKeyInput",member="kmsKeyId")`)
/// — and that description lists four forms: a key id, a key ARN, an alias name
/// and an alias ARN. The modelled pattern `^[a-zA-Z0-9:/_-]+$` admits text that
/// is none of them, so this is the rule a handler is left to apply; a value
/// outside every form names no key, and the operation answers the modelled
/// `EncryptionKeyInvalidIdException`.
pub fn is_kms_key_selector(key_id: &str) -> bool {
    is_kms_alias(key_id) || is_kms_key_id(key_id) || is_kms_arn(key_id)
}

/// The message `EncryptionKeyInvalidIdException` carries: one code, one
/// sentence, worded in a single place so the two operations that take a
/// `kmsKeyId` cannot word the same refusal differently. The only variable part
/// is the selector the request named.
pub fn invalid_encryption_key_id_message(key_id: &str) -> String {
    format!(
        "the encryption key id {key_id} is not valid: it names neither a key id, a key ARN, an alias nor an alias ARN"
    )
}

/// The Amazon Web Services managed key a repository is encrypted under when the
/// request names no key of its own, in the alias form KMS names that key by:
/// `kmsKeyId`'s own documentation states "If no key is specified, the default
/// aws/codecommit Amazon Web Services managed key is used"
/// (`smithy_query(query="doc",shape="CreateRepositoryInput",member="kmsKeyId")`).
/// The repository records the KEY that alias resolves to, never the alias
/// ([`resolve_encryption_key`]).
pub const DEFAULT_ENCRYPTION_KEY_ALIAS: &str = "alias/aws/codecommit";

/// The key a repository being created or rotated in `region` is encrypted under:
/// the ARN of the key `requested` names, or of the managed key this service
/// encrypts a repository with when a request names no key at all.
///
/// The ONE place a selector becomes the key a repository holds, so `CreateRepository`
/// and `UpdateRepositoryEncryptionKey` cannot record one key in two forms and every
/// projection of a repository reports a key identifier.
///
/// `requested` is a selector [`is_kms_key_selector`] has already accepted, in any of
/// the four forms KMS documents for `KeyId`; which key it names is KMS's own answer,
/// read from the KMS store on the shared engine. A predefined Amazon Web Services
/// alias — this service's managed key above all — is resolved through the association
/// KMS makes on first use, which is what makes the managed key resolvable in a Region
/// no KMS caller has written to yet.
///
/// A selector KMS holds no key for is recorded in the full form the selector itself
/// names, which for a bare key id is the ARN of that key in this account and
/// `region` ([`canonical_encryption_key`]): the key is the caller's own, in an
/// account or a key store this emulator does not keep, so nothing here invents
/// another identity for it or refuses a create AWS serves.
///
/// LOCKING: this takes the KMS store's lock and no other, and no KMS operation
/// reaches into the CodeCommit store, so the two can never be taken in opposite
/// orders: the create asks while holding this service's own guard — resolving only
/// once the create is going to be served, because resolving a predefined alias is
/// what gives it its key — and the rotation asks with no guard of this service's
/// held at all.
pub fn resolve_encryption_key<Eng: Send + Sync + 'static>(
    engine: &Eng,
    region: &str,
    requested: Option<&str>,
) -> String {
    use ::service_com_amazonaws_kms_trentservice::state as kms_store;

    let selector = requested.unwrap_or(DEFAULT_ENCRYPTION_KEY_ALIAS);
    // No KMS store in this process (an engine that is not the shared in-memory one):
    // nothing can resolve the selector, so it is held in the fullest form it names
    // by itself.
    let Some(keys) = kms_store::service_state(engine) else {
        return canonical_encryption_key(selector, region);
    };
    let mut keys = keys.lock();
    // Which alias of this endpoint the selector names, in either form KMS accepts
    // for one, is KMS's own reading of it — and a PREDEFINED Amazon Web Services
    // alias is resolved through the association KMS makes the first time one is
    // asked for rather than through the plain lookup, because it carries no key
    // until then.
    if let Some(alias) = kms_store::alias_named_by(region, selector)
        .filter(|alias| kms_store::is_predefined_aws_alias(alias))
    {
        return match keys.associate_predefined_aws_alias(region, &alias) {
            Some(key) => key.arn.clone(),
            None => canonical_encryption_key(selector, region),
        };
    }
    match keys.resolve(region, selector) {
        kms_store::KeyRef::Found(key) => key.arn.clone(),
        _ => canonical_encryption_key(selector, region),
    }
}

/// How this service words `EncryptionKeyDisabledException`: the fixed words around
/// the KEY the refused call would have used, which is the refusal's one variable part
/// — the key the repository is encrypted under for a call on its contents, and the
/// key the request's selector resolved to for a call that points the repository at
/// one. Never the selector a request happened to name it by, so a call that names no
/// key at all is refused in exactly the same terms.
pub fn encryption_key_disabled_message(kms_key_id: &str) -> String {
    format!("KMS key {kms_key_id} is disabled")
}

/// The Region whose KMS endpoint holds the key `kms_key_id` names.
///
/// A repository records its key as the ARN of the key itself
/// ([`resolve_encryption_key`]), and an ARN says which Region holds it — read out of
/// the one parse of a KMS ARN this module has ([`kms_arn_fields`]). A selector KMS
/// resolved no key for is kept in the form the caller set
/// ([`canonical_encryption_key`]), and a key id or an alias name leaves the Region
/// unsaid, so it is looked for at the endpoint of the Region the repository lives in.
fn encryption_key_region<'a>(kms_key_id: &'a str, repository_region: &'a str) -> &'a str {
    match kms_arn_fields(kms_key_id) {
        Some([_, _, _, region, _, _]) => region,
        None => repository_region,
    }
}

/// The refusal a call on `repository`'s CONTENTS answers while the KMS key that
/// repository is encrypted under is disabled, or `None` while that key is usable.
///
/// A repository's contents are encrypted under its key, so reaching them needs that
/// key: "the default aws/codecommit Amazon Web Services managed key" or the
/// customer-managed one the create resolved
/// (`smithy_query(query="doc",shape="CreateRepositoryInput",member="kmsKeyId")`).
/// While the key is disabled it cannot be used, and the modelled
/// `EncryptionKeyDisabledException` ("The encryption key is disabled", client, 400)
/// is the refusal every operation that reads or writes those contents declares —
/// which is why this is asked in ONE place, after the repository is found and before
/// a single object of it is read or written, rather than worded per operation.
///
/// Whether the key is disabled is KMS's own answer, read from the KMS store on the
/// same shared engine, so disabling a key through `kms:DisableKey` and enabling it
/// again through `kms:EnableKey` is what moves this gate — nothing is cached here.
/// A selector KMS holds no key for (a key in an account or a key store this emulator
/// does not keep) disables nothing: there is no key state to read, and a call AWS
/// serves is not refused for want of one.
///
/// LOCKING: this takes the KMS store's lock and no other, and no KMS operation
/// reaches into the CodeCommit store, so a handler asking while it holds this
/// service's own guard can never take the two in the opposite order.
pub fn disabled_encryption_key<Eng: Send + Sync + 'static>(
    engine: &Eng,
    repository: &Repository,
) -> Option<EncryptionKeyDisabledException> {
    disabled_encryption_key_named(engine, &repository.region, &repository.kms_key_id)
}

/// The refusal a call that has to USE the key `kms_key_id` answers while KMS reports
/// that key disabled, or `None` while the key is usable.
///
/// The one reading of a key's state this service has, which is why both the calls on a
/// repository's CONTENTS ([`disabled_encryption_key`]) and the calls that POINT a
/// repository at a key ask it: encrypting a repository under a key needs that key, so a
/// key KMS cannot use is the modelled `EncryptionKeyDisabledException` ("The encryption
/// key is disabled", client, 400) whichever of them asked. `kms_key_id` is a selector in
/// any of the forms KMS documents for `KeyId`, or the key a selector already resolved
/// to; `repository_region` is the Region to look for it in when the value names none of
/// its own ([`encryption_key_region`]).
///
/// Whether the key is disabled is KMS's own answer, read from the KMS store on the same
/// shared engine, so `kms:DisableKey` and `kms:EnableKey` are what move this gate —
/// nothing is cached here. A selector KMS holds no key for (a key in an account or a key
/// store this emulator does not keep) disables nothing: there is no key state to read,
/// and a call AWS serves is not refused for want of one.
///
/// LOCKING: this takes the KMS store's lock and no other, and no KMS operation reaches
/// into the CodeCommit store, so a handler asking while it holds this service's own
/// guard can never take the two in the opposite order.
pub fn disabled_encryption_key_named<Eng: Send + Sync + 'static>(
    engine: &Eng,
    repository_region: &str,
    kms_key_id: &str,
) -> Option<EncryptionKeyDisabledException> {
    use ::service_com_amazonaws_kms_trentservice::state as kms_store;

    let keys = kms_store::service_state(engine)?;
    let region = encryption_key_region(kms_key_id, repository_region);
    let disabled = match keys.lock().resolve(region, kms_key_id) {
        // The state a READER of the key reports, which is the state its own
        // `kms:DescribeKey` answers with.
        kms_store::KeyRef::Found(key) => {
            key.reported_key_state() == kms_store::KEY_STATE_DISABLED
        }
        _ => false,
    };
    disabled.then(|| EncryptionKeyDisabledException {
        message: Some(encryption_key_disabled_message(kms_key_id)),
    })
}

/// The full form a selector names BY ITSELF, for a selector KMS resolves no key
/// for: of the four forms KMS documents for `KeyId`, only the bare key id leaves
/// the key's account and region unsaid, so a key id becomes the ARN of that key in
/// this account and `region` — the identifier KMS reports a key by — while a
/// selector that already names a key or an alias in full is kept as the caller set
/// it.
pub fn canonical_encryption_key(selector: &str, region: &str) -> String {
    if is_kms_key_id(selector) {
        return ::engine::emulator_arn("kms", region, &format!("key/{selector}"));
    }
    selector.to_string()
}
