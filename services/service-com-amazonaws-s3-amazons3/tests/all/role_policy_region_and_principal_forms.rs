// SPDX-License-Identifier: CC0-1.0

//! Two inputs to an S3 authorization decision about an assumed-role session that are
//! properties of the REQUEST and of the POLICY VALUE rather than of the session: the Region
//! the request was addressed to, as a condition clause in the role's OWN permissions policy
//! reads it, and the exact ARN a resource policy's `Principal` element names.
//!
//! The rules under test:
//!
//! 1. **A statement's `Condition` over `aws:RequestedRegion` is decided against the Region the
//!    request was addressed to, in every document in force — the role's own identity-based
//!    policy included.** "aws:RequestedRegion — the Region that was called in the request"
//!    (AWS General Reference, *Global condition context keys*): the value is derived from the
//!    endpoint the client reached and is carried in no request member, so one session of one
//!    role reaches exactly the Regions its clause holds for and is refused 403 `AccessDenied`
//!    in every other, with no session policy anywhere in the picture. The unconditioned rows
//!    are the control: the same role with an unconditional grant reaches both Regions, which
//!    is what makes the refusal the clause's and not the bucket's or the Region's.
//! 2. **A resource policy's `Principal` names a principal by its WHOLE ARN.** IAM compares a
//!    principal literally (the grammar gives `Principal` no partial wildcards — only the
//!    whole-element `"*"`), so a value that differs from the caller in ANY segment names a
//!    different principal and grants it nothing: `iam::<account>:user/<name>` and
//!    `iam::<account>:role/<name>` are different principals even where `<name>` is the same,
//!    and `sts::<account>:assumed-role/<role>/<session>` names ONE session, so the session
//!    segment participates in the comparison rather than being truncated to the role. The
//!    spellings that DO name the caller — the session's own ARN and the ARN of the role it is
//!    a session of — grant it, which is what keeps the rows above from passing because the
//!    document was ignored.
//! 3. **A bucket-scoped refusal of an OBJECT-plane call carries `<BucketName>` naming the
//!    bucket the request addressed.** `com.amazonaws.s3#NoSuchBucket` declares no members, so
//!    the element is the wire document's: the fixed parts are the element name and `Code` =
//!    `NoSuchBucket`, and the value is the bucket named in the request URI, which is what an
//!    SDK surfaces as `BucketName` and hands back as absent when it is missing.
//!
//! Every assertion drives real HTTP requests through a router holding S3 beside the two
//! services such a decision reads from — IAM (the role, the same-named user and their
//! identity-based documents) and STS (the mint) — over ONE shared engine, through the harness
//! [`super::session_policy_condition_keys`] already stands up. The role, the user, their
//! documents and the session are made with those services' own operations, each S3 request
//! presents the credential's own access key id in the credential scope of the Region it
//! addresses, and nothing reaches into a store to plant a policy or a decision.
//!
//! The tables span what the rules generalise over rather than one recorded call: for rule 1
//! the operator comparing the Region (equality, negated equality, a set, a pattern), WHICH
//! Region the clause names, which Region the call addresses (three of them, none of them the
//! default `us-east-1`), whether the conditioned statement is the grant or a `Deny` over a
//! broad grant, and the operation called (a write and a read); for rule 2 the FORM of the
//! principal value and which credential presents itself against it; and for rule 3 the
//! operation, the shape of the bucket name (the published 3-character minimum and
//! 63-character maximum, a dotted name, a digit-bearing one) and whether the name never held
//! a bucket or held one that was deleted.

use serde_json::{json, Value};

// The element a bucket-scoped error document names the bucket by, stated once by the suite
// that pins the BUCKET-configuration half of the same rule.
use super::bucket_error_envelope::BUCKET_NAME_ELEMENT;
// The account every ARN a row builds belongs to and the IAM user ARN spelling of this
// account, read through the one helper this test binary already states each with.
use super::global_condition_keys::{account, user_arn};
// The readings and spellings the role-session suites of this test binary already state,
// reused rather than copied: the identity-document builder, S3's object ARN, and the two
// clauses that close a refusal no grant reached and one an identity-based document denied.
use super::role_session_object_authorization::{
    identity_document, no_grant, object_arn, IDENTITY_DENY,
};
// The three-service harness — S3 beside IAM and STS over one engine, with the Region of each
// request in its credential scope — and the two assertions every authorization row of this
// test binary is judged by.
use super::session_policy_condition_keys::{assert_refused, assert_served, Emulator, ACCOUNT_KEY};

/// The Region a rule-1 clause names first, and the one rules 2 and 3 place their fixtures in.
/// Deliberately NOT `us-east-1`: that Region is what a request carrying no parseable
/// credential scope is read as, so a rule pinned only there could pass on a Region nothing
/// derived.
const PERMITTED_REGION: &str = "us-west-2";

/// A SECOND Region, which the same session addresses to make the away leg of every rule-1
/// row. A real Region code of the same partition, so nothing but the clause can decide the
/// difference — and outside the `us-` prefix, so the pattern row has something to not match.
const OTHER_REGION: &str = "eu-west-1";

/// A THIRD Region, for the row whose clause names a SET of the two above: a request addressed
/// here is outside that set.
const THIRD_REGION: &str = "ap-south-1";

/// The code S3 answers a call naming a bucket it does not hold with
/// (`com.amazonaws.s3#NoSuchBucket`, `httpError 404`).
const NO_SUCH_BUCKET: &str = "NoSuchBucket";

/// The `<Message>` S3 answers that code with: "NoSuchBucket — The specified bucket does not
/// exist" (Amazon S3 *Error responses*, the published error-code list). Written out here
/// rather than read from the service's own catalog so the wording has something to be wrong
/// about; the modelled shape's doc sentence ends in a period that a general purpose bucket's
/// refusal does not send.
const NO_SUCH_BUCKET_MESSAGE: &str = "The specified bucket does not exist";

/// The IAM action an object write is authorized under ("you must have the `s3:PutObject`
/// permission", `PutObject`).
const PUT_OBJECT: &str = "s3:PutObject";

/// The IAM action a read of the current version of an object is authorized under ("to use
/// `GetObject`, you must have the `READ` access to the object" — `s3:GetObject`).
const GET_OBJECT: &str = "s3:GetObject";

/// The bytes every fixture object holds. A policy is about who may reach an object, not about
/// what is in it, so one payload serves every row.
const BODY: &[u8] = b"region-and-principal-subject";

/// The bucket a rule-1 row addresses in `region` — one per Region, so each row's bucket really
/// lives in the Region its call is addressed to.
fn region_bucket(region: &str) -> String {
    format!("region-gated-{region}")
}

// ---------------------------------------------------------------------------
// RULE 1: a clause over `aws:RequestedRegion` in the ROLE's own permissions policy is decided
// against the Region the request was addressed to.
// ---------------------------------------------------------------------------

/// What a rule-1 row's role policy says about the Region. Each is built at run time from the
/// Region constants above, so no value here is copied out of one recorded run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegionPolicy {
    /// The grant holds only where the Region equals [`PERMITTED_REGION`].
    EqualsPermitted,
    /// The grant holds only where it equals [`OTHER_REGION`] — the mirror of the row above,
    /// so neither Region is privileged by the fixtures.
    EqualsOther,
    /// The grant holds for either of the two, as a SET.
    EqualsEitherOfTwo,
    /// The grant holds for every Region BUT [`PERMITTED_REGION`], under a negated operator.
    NotPermitted,
    /// The grant holds for every Region whose code matches the `us-` prefix, by pattern.
    LikeUsPrefix,
    /// A broad, unconditioned grant with an explicit `Deny` conditioned on [`OTHER_REGION`]
    /// beside it: the clause decides the DENY rather than the grant.
    DeniedInOther,
    /// A broad, unconditioned grant and nothing else — the control that proves every Region
    /// the rows above refuse is otherwise reachable by this very role.
    Unconditional,
}

impl RegionPolicy {
    /// The identity-based document this form builds, granting `action` on `resource`.
    fn document(self, action: &str, resource: &str) -> String {
        let allow = |condition: Option<Value>| {
            let mut statement = json!({
                "Effect": "Allow",
                "Action": action,
                "Resource": resource,
            });
            if let Some(condition) = condition {
                statement["Condition"] = condition;
            }
            statement
        };
        match self {
            Self::EqualsPermitted => identity_document(vec![allow(Some(
                json!({"StringEquals": {"aws:RequestedRegion": PERMITTED_REGION}}),
            ))]),
            Self::EqualsOther => identity_document(vec![allow(Some(
                json!({"StringEquals": {"aws:RequestedRegion": OTHER_REGION}}),
            ))]),
            Self::EqualsEitherOfTwo => identity_document(vec![allow(Some(json!({
                "StringEquals": {"aws:RequestedRegion": [PERMITTED_REGION, OTHER_REGION]},
            })))]),
            Self::NotPermitted => identity_document(vec![allow(Some(
                json!({"StringNotEquals": {"aws:RequestedRegion": PERMITTED_REGION}}),
            ))]),
            Self::LikeUsPrefix => identity_document(vec![allow(Some(
                json!({"StringLike": {"aws:RequestedRegion": "us-*"}}),
            ))]),
            Self::DeniedInOther => identity_document(vec![
                allow(None),
                json!({
                    "Effect": "Deny",
                    "Action": action,
                    "Resource": resource,
                    "Condition": {"StringEquals": {"aws:RequestedRegion": OTHER_REGION}},
                }),
            ]),
            Self::Unconditional => identity_document(vec![allow(None)]),
        }
    }
}

/// What a rule-1 row's call must answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// The policies in force leave the call to the caller.
    Served,
    /// The call is refused because no grant reached it.
    RefusedWithoutGrant,
    /// The call is refused by the explicit `Deny` in the role's own document.
    RefusedByIdentityDeny,
}

/// RULE 1: a role whose own permissions policy conditions its grant on `aws:RequestedRegion`
/// yields a session that reaches exactly the Regions the clause holds for — the key is derived
/// from the endpoint the request addressed, and no session policy is involved.
#[tokio::test]
async fn an_aws_requested_region_clause_in_the_roles_own_policy_gates_the_sessions_calls() {
    let key = "residency/report.csv";
    // The rows differ along the operator, WHICH Region the clause names, which Region the call
    // addresses, whether the clause decides the grant or a deny, and the operation called.
    let rows: &[(&str, RegionPolicy, &str, &str, Outcome)] = &[
        (
            "the clause names the Region the write addressed",
            RegionPolicy::EqualsPermitted,
            PERMITTED_REGION,
            PUT_OBJECT,
            Outcome::Served,
        ),
        (
            "the clause names another Region than the write addressed",
            RegionPolicy::EqualsPermitted,
            OTHER_REGION,
            PUT_OBJECT,
            Outcome::RefusedWithoutGrant,
        ),
        (
            "the clause names the second Region, called there",
            RegionPolicy::EqualsOther,
            OTHER_REGION,
            PUT_OBJECT,
            Outcome::Served,
        ),
        (
            "the clause names the second Region, called in the first",
            RegionPolicy::EqualsOther,
            PERMITTED_REGION,
            PUT_OBJECT,
            Outcome::RefusedWithoutGrant,
        ),
        (
            "a set the Region addressed is one entry of",
            RegionPolicy::EqualsEitherOfTwo,
            OTHER_REGION,
            PUT_OBJECT,
            Outcome::Served,
        ),
        (
            "a set the Region addressed is not in",
            RegionPolicy::EqualsEitherOfTwo,
            THIRD_REGION,
            PUT_OBJECT,
            Outcome::RefusedWithoutGrant,
        ),
        (
            "a pattern the Region addressed matches",
            RegionPolicy::LikeUsPrefix,
            PERMITTED_REGION,
            PUT_OBJECT,
            Outcome::Served,
        ),
        (
            "a pattern the Region addressed does not match",
            RegionPolicy::LikeUsPrefix,
            OTHER_REGION,
            PUT_OBJECT,
            Outcome::RefusedWithoutGrant,
        ),
        (
            "every Region but the one named, called from another",
            RegionPolicy::NotPermitted,
            OTHER_REGION,
            PUT_OBJECT,
            Outcome::Served,
        ),
        (
            "every Region but the one named, called from it",
            RegionPolicy::NotPermitted,
            PERMITTED_REGION,
            PUT_OBJECT,
            Outcome::RefusedWithoutGrant,
        ),
        (
            "a broad grant denied in the Region addressed",
            RegionPolicy::DeniedInOther,
            OTHER_REGION,
            PUT_OBJECT,
            Outcome::RefusedByIdentityDeny,
        ),
        (
            "a broad grant denied in another Region",
            RegionPolicy::DeniedInOther,
            PERMITTED_REGION,
            PUT_OBJECT,
            Outcome::Served,
        ),
        // The control: the same role with an unconditional grant reaches BOTH Regions, which
        // is what makes every refusal above the clause's rather than the bucket's.
        (
            "the control, unconditional, in the first Region",
            RegionPolicy::Unconditional,
            PERMITTED_REGION,
            PUT_OBJECT,
            Outcome::Served,
        ),
        (
            "the control, unconditional, in the second Region",
            RegionPolicy::Unconditional,
            OTHER_REGION,
            PUT_OBJECT,
            Outcome::Served,
        ),
        (
            "the control, unconditional, in the third Region",
            RegionPolicy::Unconditional,
            THIRD_REGION,
            PUT_OBJECT,
            Outcome::Served,
        ),
        // The same comparison on a READ, so the rule is pinned on both kinds of call.
        (
            "the clause names the Region the read addressed",
            RegionPolicy::EqualsPermitted,
            PERMITTED_REGION,
            GET_OBJECT,
            Outcome::Served,
        ),
        (
            "the clause names another Region than the read addressed",
            RegionPolicy::EqualsPermitted,
            OTHER_REGION,
            GET_OBJECT,
            Outcome::RefusedWithoutGrant,
        ),
    ];

    for (index, (case, policy, region, action, outcome)) in rows.iter().enumerate() {
        let emulator = Emulator::new();
        let bucket = region_bucket(region);
        emulator.create_bucket(&bucket, region).await;
        // Written as the account before any role exists, so the fixture a read addresses is
        // never what the row's own document decided.
        emulator.write_fixture(&bucket, region, key).await;
        let resource = object_arn(&bucket, key);
        let (role_arn, _) = emulator
            .create_role(
                &format!("region-gated-role-{index}"),
                &policy.document(action, &resource),
            )
            .await;
        // No session policy: the clause under test lives in the role's OWN document, which is
        // a different leg of the decision from a scope the mint passed.
        let session = emulator
            .assume(case, &role_arn, &format!("region-session-{index}"), None)
            .await;
        let (method, body): (&str, &[u8]) = match *action {
            PUT_OBJECT => ("PUT", BODY),
            _ => ("GET", b""),
        };
        let answer = emulator
            .s3(
                &session.access_key_id,
                region,
                method,
                &format!("/{bucket}/{key}"),
                body,
            )
            .await;
        match outcome {
            Outcome::Served => assert_served(case, &answer),
            Outcome::RefusedWithoutGrant => assert_refused(
                case,
                &answer,
                &session.principal_arn,
                action,
                &resource,
                &no_grant(action),
            ),
            Outcome::RefusedByIdentityDeny => assert_refused(
                case,
                &answer,
                &session.principal_arn,
                action,
                &resource,
                IDENTITY_DENY,
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// RULE 2: a resource policy's `Principal` names a principal by its whole ARN.
// ---------------------------------------------------------------------------

/// The bucket every rule-2 row addresses.
const PRINCIPAL_BUCKET: &str = "principal-form-subject";

/// The key every rule-2 row addresses.
const PRINCIPAL_KEY: &str = "shared/object.txt";

/// The name an IAM USER and an IAM ROLE of this account BOTH carry, which is what makes the
/// two ARNs differ in nothing but their resource-type segment.
const SHARED_NAME: &str = "shared-identity-name";

/// The session name the admitted session is minted under.
const NAMED_SESSION: &str = "session-one";

/// What a rule-2 row's bucket policy names in its one `Allow`. Each is built at run time from
/// the session the mint really vended, so no value here is copied out of one recorded run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrincipalForm {
    /// The session's OWN ARN, as `sts:AssumeRole` reported it.
    SessionsOwn,
    /// The ARN of the role the session is a session of.
    TheRole,
    /// The ARN of the IAM USER that carries the same name as that role: the same account and
    /// the same trailing name, a different resource type.
    SameNamedUser,
    /// ANOTHER session of the same role: the same role, a different session name.
    OtherSession,
}

impl PrincipalForm {
    /// The `Principal` value this form builds for the session `<role>/<session>`.
    fn value(self, session_arn: &str, role_arn: &str) -> String {
        match self {
            Self::SessionsOwn => session_arn.to_string(),
            Self::TheRole => role_arn.to_string(),
            Self::SameNamedUser => user_arn(SHARED_NAME),
            Self::OtherSession => {
                format!(
                    "arn:aws:sts::{}:assumed-role/{SHARED_NAME}/other-session",
                    account()
                )
            }
        }
    }
}

/// A bucket policy whose only statement allows both object actions on every key of
/// `PRINCIPAL_BUCKET` to `principal`.
fn bucket_policy_for(principal: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"AWS": principal},
            "Action": [GET_OBJECT, PUT_OBJECT],
            "Resource": object_arn(PRINCIPAL_BUCKET, "*"),
        }],
    })
    .to_string()
}

/// An identity-based document that grants the row's action on ANOTHER bucket's objects: the
/// role carries a policy, and that policy says nothing about the bucket under test, so the
/// bucket's own `Principal` element is the only thing that can grant the call.
fn unrelated_identity_document() -> String {
    identity_document(vec![json!({
        "Effect": "Allow",
        "Action": [GET_OBJECT, PUT_OBJECT],
        "Resource": object_arn("another-bucket-entirely", "*"),
    })])
}

/// Install `document` as the bucket policy of `PRINCIPAL_BUCKET`, as the account.
async fn put_bucket_policy(emulator: &Emulator, case: &str, document: &str) {
    let stored = emulator
        .s3(
            ACCOUNT_KEY,
            PERMITTED_REGION,
            "PUT",
            &format!("/{PRINCIPAL_BUCKET}?policy"),
            document.as_bytes(),
        )
        .await;
    assert_eq!(
        stored.status,
        204,
        "case {case:?}: the bucket policy must be stored, got {} with {:?}",
        stored.status,
        String::from_utf8_lossy(&stored.body)
    );
}

/// RULE 2: the `Principal` of a bucket policy is compared as a whole ARN, so a value that
/// differs from the caller in its resource-TYPE segment (a same-named user) or in its SESSION
/// segment (another session of the same role, or no session at all) grants the caller nothing,
/// while the spellings that do name it grant it.
#[tokio::test]
async fn a_bucket_policys_principal_admits_only_the_arn_it_spells_in_full() {
    // The rows differ along the FORM of the principal value and along the operation called.
    let rows: &[(&str, PrincipalForm, &str, bool)] = &[
        (
            "the session's own ARN",
            PrincipalForm::SessionsOwn,
            GET_OBJECT,
            true,
        ),
        (
            "the ARN of the role the session is of",
            PrincipalForm::TheRole,
            GET_OBJECT,
            true,
        ),
        (
            "the ARN of the same-named IAM user",
            PrincipalForm::SameNamedUser,
            GET_OBJECT,
            false,
        ),
        (
            "another session of the same role",
            PrincipalForm::OtherSession,
            GET_OBJECT,
            false,
        ),
        // The same comparison on a WRITE, so the rule is pinned on both kinds of call.
        (
            "the session's own ARN, on a write",
            PrincipalForm::SessionsOwn,
            PUT_OBJECT,
            true,
        ),
        (
            "the ARN of the same-named IAM user, on a write",
            PrincipalForm::SameNamedUser,
            PUT_OBJECT,
            false,
        ),
        (
            "another session of the same role, on a write",
            PrincipalForm::OtherSession,
            PUT_OBJECT,
            false,
        ),
    ];

    for (case, form, action, allowed) in rows {
        let emulator = Emulator::new();
        emulator
            .create_bucket(PRINCIPAL_BUCKET, PERMITTED_REGION)
            .await;
        emulator
            .write_fixture(PRINCIPAL_BUCKET, PERMITTED_REGION, PRINCIPAL_KEY)
            .await;
        // An IAM USER and an IAM ROLE of the SAME name, so the two ARNs a policy can spell
        // differ in nothing but their resource type. The user's own document grants nothing
        // about this bucket either.
        emulator
            .iam_user(SHARED_NAME, &["s3:GetBucketLocation"])
            .await;
        let (role_arn, _) = emulator
            .create_role(SHARED_NAME, &unrelated_identity_document())
            .await;
        let session = emulator
            .assume(case, &role_arn, NAMED_SESSION, None)
            .await;
        put_bucket_policy(
            &emulator,
            case,
            &bucket_policy_for(&form.value(&session.principal_arn, &role_arn)),
        )
        .await;
        let (method, body): (&str, &[u8]) = match *action {
            PUT_OBJECT => ("PUT", BODY),
            _ => ("GET", b""),
        };
        let answer = emulator
            .s3(
                &session.access_key_id,
                PERMITTED_REGION,
                method,
                &format!("/{PRINCIPAL_BUCKET}/{PRINCIPAL_KEY}"),
                body,
            )
            .await;
        if *allowed {
            assert_served(case, &answer);
        } else {
            assert_refused(
                case,
                &answer,
                &session.principal_arn,
                action,
                &object_arn(PRINCIPAL_BUCKET, PRINCIPAL_KEY),
                &no_grant(action),
            );
        }
    }
}

/// RULE 2, from the other side: each of the two spellings the table above refuses the ROLE
/// SESSION for DOES admit the principal it actually names, and refuses the other one. Which is
/// what makes the refusals above the comparison's and not a document the gate ignored.
#[tokio::test]
async fn each_principal_spelling_admits_the_principal_it_names_and_no_other() {
    // The rows are the 2x2 of (which spelling the policy names) x (which credential calls).
    let rows: &[(&str, PrincipalForm, bool, bool)] = &[
        (
            "the user's ARN, called by that user's long-term key",
            PrincipalForm::SameNamedUser,
            false,
            true,
        ),
        (
            "the user's ARN, called by the same-named role's session",
            PrincipalForm::SameNamedUser,
            true,
            false,
        ),
        (
            "the session's ARN, called by that session",
            PrincipalForm::SessionsOwn,
            true,
            true,
        ),
        (
            "the session's ARN, called by the same-named user's long-term key",
            PrincipalForm::SessionsOwn,
            false,
            false,
        ),
    ];

    for (case, form, as_session, allowed) in rows {
        let emulator = Emulator::new();
        emulator
            .create_bucket(PRINCIPAL_BUCKET, PERMITTED_REGION)
            .await;
        emulator
            .write_fixture(PRINCIPAL_BUCKET, PERMITTED_REGION, PRINCIPAL_KEY)
            .await;
        let (user_key, _) = emulator
            .iam_user(SHARED_NAME, &["s3:GetBucketLocation"])
            .await;
        let (role_arn, _) = emulator
            .create_role(SHARED_NAME, &unrelated_identity_document())
            .await;
        let session = emulator
            .assume(case, &role_arn, NAMED_SESSION, None)
            .await;
        put_bucket_policy(
            &emulator,
            case,
            &bucket_policy_for(&form.value(&session.principal_arn, &role_arn)),
        )
        .await;
        let (key_id, principal) = if *as_session {
            (session.access_key_id.clone(), session.principal_arn.clone())
        } else {
            (user_key.clone(), user_arn(SHARED_NAME))
        };
        let answer = emulator
            .s3(
                &key_id,
                PERMITTED_REGION,
                "GET",
                &format!("/{PRINCIPAL_BUCKET}/{PRINCIPAL_KEY}"),
                b"",
            )
            .await;
        if *allowed {
            assert_served(case, &answer);
        } else {
            assert_refused(
                case,
                &answer,
                &principal,
                GET_OBJECT,
                &object_arn(PRINCIPAL_BUCKET, PRINCIPAL_KEY),
                &no_grant(GET_OBJECT),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// RULE 3: an object-plane refusal about a bucket names that bucket in the error document.
// ---------------------------------------------------------------------------

/// A bucket name of the SHORTEST length the published naming rules allow ("bucket names must
/// be between 3 (min) and 63 (max) characters long", Amazon S3 User Guide, *Bucket naming
/// rules*).
const SHORTEST_ABSENT_BUCKET: &str = "obp";

/// A bucket name of the LONGEST length those same rules allow — 63 characters, counted by the
/// assertion in [`an_object_plane_refusal_names_the_bucket_the_request_addressed`].
const LONGEST_ABSENT_BUCKET: &str =
    "object-plane-absent-bucket-with-the-longest-name-ones-may-carry";

/// How a rule-3 row's bucket came to be missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Missing {
    /// The name never held a bucket at all.
    NeverCreated,
    /// The name held a bucket that was then deleted.
    Deleted,
}

/// RULE 3: an object-plane call naming a bucket this emulator does not hold answers 404
/// `NoSuchBucket` whose error document carries a `<BucketName>` naming the bucket the request
/// addressed, whatever shape that name has and however the name came to hold no bucket.
#[tokio::test]
async fn an_object_plane_refusal_names_the_bucket_the_request_addressed() {
    assert_eq!(
        LONGEST_ABSENT_BUCKET.len(),
        63,
        "the longest-name row must name a 63-character bucket, got {}",
        LONGEST_ABSENT_BUCKET.len()
    );
    let message = NO_SUCH_BUCKET_MESSAGE;
    // The rows differ along the operation, the shape of the bucket name, and whether the name
    // never held a bucket or held one that was deleted.
    let rows: &[(&str, &str, &str, &str, Missing)] = &[
        (
            "a write into a bucket that was never created",
            "PUT",
            "object-plane-absent",
            "k.txt",
            Missing::NeverCreated,
        ),
        (
            "a write into the shortest bucket name there can be",
            "PUT",
            SHORTEST_ABSENT_BUCKET,
            "k.txt",
            Missing::NeverCreated,
        ),
        (
            "a write into the longest bucket name there can be",
            "PUT",
            LONGEST_ABSENT_BUCKET,
            "k.txt",
            Missing::NeverCreated,
        ),
        (
            "a write into a dotted bucket name",
            "PUT",
            "object.plane.absent.dotted",
            "k.txt",
            Missing::NeverCreated,
        ),
        (
            "a write into a digit-bearing bucket name",
            "PUT",
            "object-plane-absent-2026",
            "nested/key.bin",
            Missing::NeverCreated,
        ),
        (
            "a write into a bucket that was created and then deleted",
            "PUT",
            "object-plane-deleted",
            "k.txt",
            Missing::Deleted,
        ),
        (
            "a read out of a bucket that was never created",
            "GET",
            "object-plane-absent-read",
            "k.txt",
            Missing::NeverCreated,
        ),
        (
            "a read out of a bucket that was created and then deleted",
            "GET",
            "object-plane-deleted-read",
            "k.txt",
            Missing::Deleted,
        ),
    ];

    for (case, method, bucket, key, missing) in rows {
        let emulator = Emulator::new();
        if *missing == Missing::Deleted {
            emulator.create_bucket(bucket, PERMITTED_REGION).await;
            let removed = emulator
                .s3(ACCOUNT_KEY, PERMITTED_REGION, "DELETE", &format!("/{bucket}"), b"")
                .await;
            assert_eq!(
                removed.status,
                204,
                "case {case:?}: the setup delete must succeed, got {} with {:?}",
                removed.status,
                String::from_utf8_lossy(&removed.body)
            );
        }
        let body: &[u8] = if *method == "PUT" { BODY } else { b"" };
        let answer = emulator
            .s3(
                ACCOUNT_KEY,
                PERMITTED_REGION,
                method,
                &format!("/{bucket}/{key}"),
                body,
            )
            .await;
        assert_eq!(
            answer.status,
            404,
            "case {case:?}: expected HTTP 404, got {} with {:?}",
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
        assert_eq!(
            answer.error_code(),
            NO_SUCH_BUCKET,
            "case {case:?}: expected the {NO_SUCH_BUCKET} code, got {:?}",
            answer.error_code()
        );
        assert_eq!(
            answer.error_message(),
            message,
            "case {case:?}: expected the message {message:?}, got {:?}",
            answer.error_message()
        );
        assert_eq!(
            answer.element(BUCKET_NAME_ELEMENT).as_deref(),
            Some(*bucket),
            "case {case:?}: expected the <{BUCKET_NAME_ELEMENT}> to name {bucket:?}, got {:?}",
            answer.element(BUCKET_NAME_ELEMENT)
        );
    }
}
