// SPDX-License-Identifier: CC0-1.0

//! Whether the permissions in force allow the write a delivery makes, and what a write they
//! withhold reports.
//!
//! A delivered object is written under the role the destination configuration names ("the
//! Amazon Resource Name (ARN) of the Amazon Web Services credentials" —
//! `S3DestinationConfiguration$RoleARN`), so the write is an ordinary `s3:PutObject` made by a
//! principal of this account: TWO documents are in force on it, the identity-based policies of
//! that role and the destination bucket's own policy. Within one account IAM weighs them
//! together — an explicit `Deny` from either side refuses, an `Allow` from either grants, and a
//! call neither allows falls to the implicit deny
//! ([`::service_common::iam_authorization::is_allowed`]) — so a bucket whose policy denies the
//! delivery role takes nothing, however much that role's own policies allow, and a bucket whose
//! policy grants the role takes the delivery even where the role's own policies say nothing.
//!
//! The decision itself is the workspace's ONE evaluation of a policy document
//! ([`::service_common::iam_authorization`]), handed the document the object store holds, so
//! `Principal`, `Action`, `Resource`, `Condition` and an explicit `Deny` count here exactly as
//! they count for a client's own `PutObject` into the same bucket. What a delivery supplies is
//! who is asking and on what: the principal is the role the write is made as
//! ([`write_principals`]) and the resource is the object the write would create. That evaluation
//! ANSWERS about a document rather than unwinding on one — a text it cannot read as a policy
//! states nothing — and it reads at most its own ceiling of a document's bytes, so a document is
//! bounded twice over: once where Amazon S3 accepted it (`PutBucketPolicy` bounds a bucket
//! policy's size) and again by that ceiling.
//!
//! What the ROLE's documents say is asked of IAM itself
//! ([`::service_common::iam_role::role_action_decision_through`]), which is a question that may
//! go UNANSWERED — an emulator with no IAM mounted, a value naming no role of this account, a
//! role this account's IAM holds nothing under. A refusal is grounded in what was established
//! and never in a non-answer ([`withholds`]), so a delivery whose role nothing is known about is
//! withheld only by an explicit deny of the bucket itself.
//!
//! ## What one flush costs
//!
//! The documents are copied out of the object store in ONE pass, at most one per distinct
//! destination bucket of the flush ([`policies_in_force`]), and the decision is then made once
//! per object that flush writes — the objects [`super::objects_of`] worked out, which that
//! module's own budget bounds. Nothing here is sized from a request field.
//!
//! ## Lock order
//!
//! Only the object store is locked, and only while the documents are copied out of it: no
//! evaluation and no hop happens under that guard, and this service's own store has already
//! been released by the delivery that calls here — the same order the object write itself takes.
//! The guard is the shared engine's, which recovers a poisoned lock rather than propagating a
//! panic, so one bad request cannot wedge a later delivery.

use std::collections::BTreeMap;

use ::service_common::iam_authorization::{
    account_principals, is_allowed, resource_decision, Decision, Request,
};

use super::destination_logging::{DeliveryErrorReport, S3_ACCESS_DENIED};
use super::{report, DeliveredObject, DeliveryHop, S3_PUT_OBJECT_ACTION};

/// The name of the SESSION a delivery is made under.
///
/// A role is never acted as directly: a service that delivers under a caller-supplied role
/// assumes it and makes its calls as a session of that role, and the permissions in force on
/// the session are the role's. Stated once here, so the principal a destination bucket's policy
/// may name the delivery by is shaped in one place
/// ([`::service_common::iam_role::assumed_role_session_arn`]).
const DELIVERY_SESSION_NAME: &str = "FirehoseDelivery";

/// The objects of this delivery whose write the permissions in force do not allow, as the
/// failures the destinations' logging reports — paired with whether any object was withheld at
/// all.
///
/// Both sides of the permission are weighed for each object the flush writes (see the module
/// docs): what the destination bucket's own policy says about that write, and what IAM answers
/// about the role it would be made under. A withheld write is reported through the destination's
/// own `CloudWatchLoggingOptions` as the condition Amazon S3 answers such a write with
/// ([`S3_ACCESS_DENIED`]) and its records stay buffered for a later attempt, which is all a
/// producer can observe of it.
///
/// Nothing is withheld for a destination naming no role at all: there is no principal to decide
/// about, and `RoleARN` is the member that names one.
pub(super) async fn withheld_writes<Eng: Send + Sync + 'static>(
    engine: &Eng,
    hop: &DeliveryHop,
    region: &str,
    objects: &[DeliveredObject],
) -> (bool, Vec<DeliveryErrorReport>) {
    let policies = policies_in_force(engine, objects.iter().map(|object| object.bucket.as_str()));
    let mut withheld = false;
    let mut reported = Vec::new();
    for object in objects {
        let destination = &object.destination;
        if destination.role_arn.is_empty() {
            continue;
        }
        // The resource is the object this write would create, which is what an object-granting
        // statement (`arn:<partition>:s3:::<bucket>/<key pattern>`) is written about.
        let resource = format!("{}/{}", destination.bucket_arn, object.key);
        let from_bucket = bucket_decision(
            policies.get(&object.bucket).map(String::as_str),
            &destination.role_arn,
            S3_PUT_OBJECT_ACTION,
            &resource,
        );
        let from_role = match hop.emulator() {
            Some(emulator) => {
                ::service_common::iam_role::role_action_decision_through(
                    emulator,
                    region,
                    &destination.role_arn,
                    S3_PUT_OBJECT_ACTION,
                    &resource,
                    // A delivery carries no condition context of its own: nothing is guessed
                    // for a key it does not state, which is the fail-closed reading every
                    // clause this emulator cannot decide is given.
                    &[],
                )
                .await
            }
            None => None,
        };
        if !withholds(from_role, from_bucket) {
            continue;
        }
        ::tracing::warn!(
            bucket = %object.bucket,
            key = %object.key,
            "the permissions in force do not allow this object's write, so this delivery was \
             not written"
        );
        withheld = true;
        report(&mut reported, destination, S3_ACCESS_DENIED);
    }
    (withheld, reported)
}

/// The policy document in force on each of `buckets` that carries one, by bucket name.
///
/// Read from the object store this delivery writes into — the store the bucket's own
/// `GetBucketPolicy` answers from — so a policy put a moment before the flush is the one the
/// write is decided against, and a bucket carrying none has no entry. One entry per DISTINCT
/// bucket, so a flush writing many objects into one destination reads its document once. A
/// store this engine does not hold states nothing: it is also the store the write itself would
/// fail against, which is the delivery's own report to make.
fn policies_in_force<'a, Eng, Buckets>(engine: &Eng, buckets: Buckets) -> BTreeMap<String, String>
where
    Eng: Send + Sync + 'static,
    Buckets: IntoIterator<Item = &'a str>,
{
    let mut documents = BTreeMap::new();
    let Some(state) = ::service_com_amazonaws_s3_amazons3::state::service_state(engine) else {
        return documents;
    };
    let store = state.lock();
    for bucket in buckets {
        if documents.contains_key(bucket) {
            continue;
        }
        if let Some(document) = store.bucket(bucket).and_then(|held| held.policy.clone()) {
            documents.insert(bucket.to_string(), document);
        }
    }
    documents
}

/// What the destination bucket's policy says about the write of `resource` by the role
/// `role_arn` — [`Decision::NotStated`] for a bucket that carries no policy at all, which
/// neither grants the write nor refuses it.
///
/// Asked TWICE, because naming an account in a policy's `Principal` is not naming a principal
/// (see [`account_principals`]): a statement that DENIES the account refuses each of its
/// principals, while one that ALLOWS the account delegates the decision to that account's own
/// identity-based policies rather than granting the call by itself. So the deny is decided with
/// the account's spellings beside the role's own and the grant with the role's alone.
fn bucket_decision(
    document: Option<&str>,
    role_arn: &str,
    action: &str,
    resource: &str,
) -> Decision {
    let Some(document) = document else {
        return Decision::NotStated;
    };
    let granting = write_principals(role_arn);
    let mut denying = granting.clone();
    denying.extend(account_principals(role_arn));
    if resource_decision(document, &Request::new(action, resource, &denying)) == Decision::Denied {
        return Decision::Denied;
    }
    resource_decision(document, &Request::new(action, resource, &granting))
}

/// Every spelling a destination bucket's policy may name the principal a delivery writes as:
/// the role's own ARN, which is how a policy written for a delivery role names it, and the
/// session of that role the write is really made by ([`DELIVERY_SESSION_NAME`]).
fn write_principals(role_arn: &str) -> Vec<String> {
    vec![
        role_arn.to_string(),
        ::service_common::iam_role::assumed_role_session_arn(role_arn, DELIVERY_SESSION_NAME),
    ]
}

/// Whether the permissions in force WITHHOLD the write, from what each side said.
///
/// `identity` is what IAM ANSWERED about the role's own documents, and `None` is IAM answering
/// nothing at all (see the module docs). An answer is weighed against the bucket's the way IAM
/// weighs the two within one account ([`is_allowed`]); a non-answer leaves the bucket's own
/// policy as the only thing established, so only an explicit deny there withholds the write —
/// the same deliberate non-refusal `service_common::iam_role::role_withholds_action` takes of
/// an unanswerable question.
#[must_use]
fn withholds(identity: Option<Decision>, from_bucket: Decision) -> bool {
    match identity {
        Some(said) => !is_allowed(said, from_bucket),
        None => from_bucket == Decision::Denied,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The role every row decides about.
    fn role_arn() -> String {
        format!(
            "arn:aws:iam::{}:role/firehose-delivery",
            ::engine::emulator_account_id()
        )
    }

    /// The account root of the role's own account, as a policy's `Principal` names it.
    fn account_root() -> String {
        format!("arn:aws:iam::{}:root", ::engine::emulator_account_id())
    }

    /// A one-statement bucket policy document.
    fn document(effect: &str, principal: &str, action: &str, resource: &str) -> String {
        ::serde_json::json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": effect,
                "Principal": { "AWS": principal },
                "Action": action,
                "Resource": resource,
            }],
        })
        .to_string()
    }

    /// RULE: the destination bucket's own policy decides the write for the role the delivery is
    /// made as — a statement about that write grants or denies it, and one about another
    /// principal, another action or another resource states nothing.
    #[test]
    fn the_bucket_policy_decides_the_write_it_is_about() {
        let role_arn = role_arn();
        let object = "arn:aws:s3:::archive/data/2026/record";
        let action = "s3:PutObject";
        let others = format!(
            "arn:aws:iam::{}:role/someone-else",
            ::engine::emulator_account_id()
        );
        // (case, the bucket's policy, what it says about this write)
        let rows = [
            ("a bucket carrying no policy", None, Decision::NotStated),
            (
                "a text that is no policy at all",
                Some("not a policy".to_string()),
                Decision::NotStated,
            ),
            (
                "a deny of the write naming the role",
                Some(document("Deny", &role_arn, action, "arn:aws:s3:::archive/*")),
                Decision::Denied,
            ),
            (
                "a deny naming the role's account",
                Some(document(
                    "Deny",
                    &account_root(),
                    action,
                    "arn:aws:s3:::archive/*",
                )),
                Decision::Denied,
            ),
            (
                "a deny of every principal",
                Some(document("Deny", "*", action, "arn:aws:s3:::archive/*")),
                Decision::Denied,
            ),
            (
                "an allow of the write naming the role",
                Some(document("Allow", &role_arn, action, "arn:aws:s3:::archive/*")),
                Decision::Allowed,
            ),
            (
                "a deny of another action",
                Some(document(
                    "Deny",
                    &role_arn,
                    "s3:GetObject",
                    "arn:aws:s3:::archive/*",
                )),
                Decision::NotStated,
            ),
            (
                "a deny scoped to another bucket",
                Some(document(
                    "Deny",
                    &role_arn,
                    action,
                    "arn:aws:s3:::elsewhere/*",
                )),
                Decision::NotStated,
            ),
            (
                "a deny naming another role",
                Some(document("Deny", &others, action, "arn:aws:s3:::archive/*")),
                Decision::NotStated,
            ),
            (
                "an allow naming the role's account alone",
                Some(document(
                    "Allow",
                    &account_root(),
                    action,
                    "arn:aws:s3:::archive/*",
                )),
                Decision::NotStated,
            ),
        ];
        for (case, policy, expected) in rows {
            let said = bucket_decision(policy.as_deref(), &role_arn, action, object);
            assert_eq!(
                said, expected,
                "case {case:?}: expected {expected:?}, got {said:?}"
            );
        }
    }

    /// RULE: the two sides are weighed the way IAM weighs them within one account, and a
    /// question IAM answered nothing about grounds no refusal of its own.
    #[test]
    fn a_deny_from_either_side_withholds_and_an_allow_from_either_grants() {
        // (case, what IAM answered about the role, what the bucket said, whether it is
        // withheld)
        let rows = [
            (
                "both allow",
                Some(Decision::Allowed),
                Decision::Allowed,
                false,
            ),
            (
                "the role allows and the bucket is silent",
                Some(Decision::Allowed),
                Decision::NotStated,
                false,
            ),
            (
                "the role allows and the bucket denies",
                Some(Decision::Allowed),
                Decision::Denied,
                true,
            ),
            (
                "the bucket allows and the role is silent",
                Some(Decision::NotStated),
                Decision::Allowed,
                false,
            ),
            (
                "the role denies and the bucket allows",
                Some(Decision::Denied),
                Decision::Allowed,
                true,
            ),
            (
                "neither states anything",
                Some(Decision::NotStated),
                Decision::NotStated,
                true,
            ),
            (
                "IAM answered nothing and the bucket is silent",
                None,
                Decision::NotStated,
                false,
            ),
            (
                "IAM answered nothing and the bucket allows",
                None,
                Decision::Allowed,
                false,
            ),
            (
                "IAM answered nothing and the bucket denies",
                None,
                Decision::Denied,
                true,
            ),
        ];
        for (case, identity, from_bucket, expected) in rows {
            let withheld = withholds(identity, from_bucket);
            assert_eq!(
                withheld, expected,
                "case {case:?}: expected withheld {expected}, got {withheld}"
            );
        }
    }
}
