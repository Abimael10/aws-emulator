// SPDX-License-Identifier: CC0-1.0

//! WHICH identity a resource-based policy names, and the UNIQUE ID the policy holds it by.
//!
//! "When you save a resource-based policy that includes an ARN for a principal, AWS
//! transforms the ARN to the underlying unique principal ID when you save the policy. ...
//! However, if you delete the principal, the relationship is broken and the policy displays
//! the unique principal ID" (the IAM user guide's `Principal` element reference). So the ARN
//! a document writes is the SPELLING of the identity it named when it was attached, not the
//! grantee the resource holds: what the resource holds is that identity's unique id, and
//! what a read of the policy reports is the ARN the identity carrying that id has NOW — its
//! current ARN, so a renamed identity reads under the name it carries now, and the bare
//! unique id once the account holds no identity with it at all, which is how an operator
//! sees that a named grantee is gone.
//!
//! Which is why the ATTACH path records, beside the document, the unique id of every
//! identity of this account the document names ([`PrincipalIdentities`], read out of the IAM
//! store on the shared engine, where `iam:CreateUser` and `iam:CreateRole` wrote those ids),
//! and the READ path resolves each recorded id back through that same store
//! ([`super::AttachedPolicy::document_as_read`]). The read stays a pure read: it resolves
//! what the resource holds and rewrites nothing.
//!
//! The DECISION reads the same holding, because that is what the holding is for — "this helps
//! mitigate the risk of someone escalating their privileges by removing and recreating the
//! principal" (the same reference): a statement names the identity its held id belongs to, so
//! that identity is named whatever ARN it carries now, and an identity REMADE under the ARN
//! the document spells carries an id of its own and is named by nothing
//! ([`super::AttachedPolicy::spellings_decided`]).
//!
//! An identity is read by its own ARN, so only the two IAM entities a resource-based policy
//! grants to by name take part — a user and a role. Every other `Principal` value is the
//! caller's own and is reported exactly as it was attached: the wildcard, an account by its
//! id or its root, a service, federated or canonical-user principal, and an entity of
//! ANOTHER account, whose identities this account's IAM is not the one to resolve.
//!
//! The document itself is walked by the tree's one rewriter of a policy element's values
//! ([`::service_common::iam_policy_rendering::rewritten_values`]), so every other byte of the
//! stored document — its member order, its list forms, its spacing — is reported exactly as
//! the attach stored it.

use ::std::collections::BTreeMap;

use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_state;

/// The `Principal` type whose values name an AWS account or one of its identities, which is
/// the only type an identity resolution is about ("AWS", one of
/// [`::service_common::iam_policy_principals::PRINCIPAL_TYPES`]).
pub const IDENTITY_PRINCIPAL_TYPE: &str = "AWS";

/// The identities of this account, as the IAM store held them at ONE instant: each one's
/// unique id and the ARN it carries then.
///
/// Read before this service's own store is locked ([`request_principal_identities`]), so no
/// request holds this service's guard and the IAM guard at once. Empty when the engine
/// carries no IAM store — a router that mounts this service without IAM, where the account
/// holds no user and no role to resolve, and every principal is therefore reported as it was
/// attached.
#[derive(Debug, Default, Clone)]
pub struct PrincipalIdentities {
    /// Unique id -> the ARN the identity holding that id carries now.
    arn_by_unique_id: BTreeMap<String, String>,
    /// ARN -> the unique id of the identity carrying that ARN now.
    unique_id_by_arn: BTreeMap<String, String>,
}

impl PrincipalIdentities {
    /// The unique id of the identity `principal_arn` names, or `None` when this account
    /// holds no identity under that ARN.
    #[must_use]
    pub fn unique_id_of(&self, principal_arn: &str) -> Option<&str> {
        self.unique_id_by_arn
            .get(principal_arn)
            .map(String::as_str)
    }

    /// The ARN the identity holding `unique_id` carries now, or `None` when no identity of
    /// this account holds that id any more — the deleted principal a read then reports by
    /// the id itself.
    #[must_use]
    pub fn arn_of(&self, unique_id: &str) -> Option<&str> {
        self.arn_by_unique_id.get(unique_id).map(String::as_str)
    }
}

/// The account's identities as IAM holds them at this instant, read through STS's own view
/// of that store ([`sts_state::iam_state`]) — the records `iam:CreateUser` and
/// `iam:CreateRole` wrote, rather than a second copy of them.
///
/// Call it with this service's store NOT locked (see [`PrincipalIdentities`]).
#[must_use]
pub fn request_principal_identities<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> PrincipalIdentities {
    let mut identities = PrincipalIdentities::default();
    let Some(iam) = sts_state::iam_state(engine) else {
        return identities;
    };
    let held = iam.lock();
    for (unique_id, arn) in identity_records(&held) {
        identities
            .arn_by_unique_id
            .insert(unique_id.clone(), arn.clone());
        identities
            .unique_id_by_arn
            .insert(arn.clone(), unique_id.clone());
    }
    identities
}

/// The unique id the identity `principal_arn` names carries now, read straight off an IAM
/// store the caller already holds the guard for — the same question
/// [`PrincipalIdentities::unique_id_of`] answers, for a request that has the store open
/// anyway ([`super::authorization::request_caller`]) and needs one ARN rather than the
/// account's whole directory.
#[must_use]
pub fn identity_unique_id<'a>(
    iam: &'a sts_state::IamState,
    principal_arn: &str,
) -> Option<&'a String> {
    identity_records(iam)
        .find(|(_, arn)| *arn == principal_arn)
        .map(|(unique_id, _)| unique_id)
}

/// Every identity of this account as a `(unique id, ARN)` pair: the two IAM entities a
/// resource-based policy grants to by name, each read under its own record
/// (`User$UserId`/`User$Arn`, `Role$RoleId`/`Role$Arn`). The ONE place those records are
/// enumerated, so the directory and the single lookup above cannot disagree.
fn identity_records(
    iam: &sts_state::IamState,
) -> impl Iterator<Item = (&String, &String)> {
    iam.users
        .values()
        .map(|user| (&user.user_id, &user.arn))
        .chain(iam.roles.values().map(|role| (&role.role_id, &role.arn)))
}

/// The unique id of every identity of this account `document` names, keyed by the ARN the
/// document names it under — what the resource HOLDS for those principals.
///
/// Collected through the SAME walk that writes those principals back on a read
/// ([`document_naming_identities`]), over the same element: what the attach holds and what a
/// read can resolve are therefore the same set by construction, whatever case a `Principal`
/// map keys its types by. A principal the account holds no identity for is not recorded at
/// all — nothing of it is resolved, so it is reported and decided exactly as it was attached.
#[must_use]
pub(super) fn stored_principal_ids(
    document: &str,
    identities: &PrincipalIdentities,
) -> BTreeMap<String, String> {
    let stored = ::std::cell::RefCell::new(BTreeMap::new());
    // The walk REPORTS the values; it writes none, so what it hands back is `document`
    // itself and is dropped.
    let _unwritten = ::service_common::iam_policy_rendering::rewritten_values(
        document,
        &::service_common::iam_policy_rendering::ElementValues {
            element: IDENTITY_PRINCIPAL_TYPE,
            written_as: &|value| {
                if let Some(unique_id) = identities.unique_id_of(value) {
                    stored
                        .borrow_mut()
                        .insert(value.to_string(), unique_id.to_string());
                }
                None
            },
        },
    );
    stored.into_inner()
}

/// `document` as a READ of the policy reports it: every principal the resource holds by a
/// unique id written as the ARN the identity carrying that id has now, and as the bare
/// unique id where the account holds no such identity any more.
///
/// `principal_ids` is what the attach recorded ([`stored_principal_ids`]) and `identities`
/// the account's identities at the instant of the read, so the same document reports
/// differently as the account's identities change — which is the whole point of holding a
/// principal by its id.
#[must_use]
pub(super) fn document_naming_identities(
    document: &str,
    principal_ids: &BTreeMap<String, String>,
    identities: &PrincipalIdentities,
) -> String {
    if principal_ids.is_empty() {
        return document.to_string();
    }
    ::service_common::iam_policy_rendering::rewritten_values(
        document,
        &::service_common::iam_policy_rendering::ElementValues {
            element: IDENTITY_PRINCIPAL_TYPE,
            written_as: &|value| {
                let unique_id = principal_ids.get(value)?;
                Some(identities.arn_of(unique_id).unwrap_or(unique_id).to_string())
            },
        },
    )
}

/// The spellings a document holding `principal_ids` DECIDES a caller named by `spellings` and
/// carrying the unique ids `unique_ids` by.
///
/// Holding a principal by its unique id is what the document's statements are read against, so
/// the spellings follow the ids rather than the text: an ARN the document holds by one of the
/// caller's own ids names the caller whatever name that identity has taken since, and an ARN
/// the document holds by SOME OTHER id names the identity that id belongs to and not this
/// caller — which is what the IAM user guide states the holding is for ("this helps mitigate
/// the risk of someone escalating their privileges by removing and recreating the principal").
/// A value the document holds by no id at all is the caller's own text and stands.
#[must_use]
pub(super) fn spellings_decided(
    principal_ids: &BTreeMap<String, String>,
    spellings: &[String],
    unique_ids: &[String],
) -> Vec<String> {
    if principal_ids.is_empty() {
        return spellings.to_vec();
    }
    let carried = |unique_id: &String| unique_ids.iter().any(|held| held == unique_id);
    let mut decided: Vec<String> = spellings
        .iter()
        .filter(|spelling| principal_ids.get(*spelling).is_none_or(carried))
        .cloned()
        .collect();
    for named in principal_ids
        .iter()
        .filter(|(_, unique_id)| carried(unique_id))
        .map(|(named, _)| named)
    {
        if !decided.contains(named) {
            decided.push(named.clone());
        }
    }
    decided
}

// The statements a document's own reading produced are what the ids are resolved off
// ([`stored_principal_ids`]), so nothing here walks the document's JSON a second time.
