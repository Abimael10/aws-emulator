// SPDX-License-Identifier: CC0-1.0

//! The "require multi-factor authentication" hardening pattern, end to end: ONE `Deny` of
//! everything except the few actions a user needs to enrol and use a device, conditioned on
//! the signing credential not being MFA-backed.
//!
//! The rule under test: **an S3 request is decided against the documents in force on the
//! principal that signed it over a context answering `aws:MultiFactorAuthPresent` from the
//! CREDENTIAL, and an explicit `Deny` whose clause holds refuses the request whatever grants
//! it** — "an explicit deny in any policy overrides any allows" (IAM User Guide, *Policy
//! evaluation logic*), and "this key is present in the request context only when the principal
//! uses temporary credentials" (AWS General Reference, *Global condition context keys*,
//! `aws:MultiFactorAuthPresent`), which is why the `…IfExists` spelling a hardening statement
//! is written in holds for a long-term access key while the plain spelling does not. The
//! refusal is HTTP 403 `AccessDenied` carrying IAM's own sentence for a deny in the
//! principal's own documents, and it leaves no object behind; the same request under a
//! credential whose mint satisfied an MFA challenge is served and round-trips its bytes.
//!
//! What this suite adds beside [`super::credential_scoped_authorization`], which spans the
//! operators that compare the key and the documents that carry them: the SHAPE the hardening
//! pattern itself takes — a `Deny` over the COMPLEMENT of a bootstrap action list, written
//! against every resource — and the fact that the exempted actions stay available under the
//! very credential the deny refuses, which is what lets the holder of the key enrol a device
//! and exchange a pass code for the session that is allowed.
//!
//! The rows differ along what the rule generalises over: how the deny names the actions it
//! covers (the complement of the bootstrap list, an action wildcard over this service), which
//! resources it is written against (everything, this bucket and its objects), which operator
//! compares the key and which value it compares against — and, per row, which of the three
//! credentials of one IAM user signs: its long-term access key (for which the key is absent),
//! a session minted with no challenge (`false`) and one minted with a satisfied challenge
//! (`true`).
//!
//! Every credential is one IAM and STS really minted, through their own operations, over the
//! shared engine this suite's router holds all three services on.

use serde_json::json;

use super::credential_scoped_authorization::{
    ACCOUNT_KEY, Emulator, PRESENTED, Presented, element,
};

/// The bucket every row addresses.
const BUCKET: &str = "deny-unless-mfa-subject";

/// The bytes every write carries. A policy is about who may write, not about what is written,
/// so one payload serves every row.
const BODY: &[u8] = b"deny-unless-mfa-payload";

/// The code S3 answers an authorization refusal with, and the status it carries (Amazon S3
/// "Error responses": `AccessDenied`, HTTP 403).
const ACCESS_DENIED: &str = "AccessDenied";
const ACCESS_DENIED_STATUS: u16 = 403;

/// The clause IAM's refusal sentence carries when a statement of the principal's OWN documents
/// decided the request (IAM User Guide, *Policy evaluation logic*).
const IDENTITY_DENY_CLAUSE: &str = "with an explicit deny in an identity-based policy";

/// The action a write is authorized under: "you must have the `s3:PutObject` permission"
/// (`PutObject`).
const PUT_OBJECT_ACTION: &str = "s3:PutObject";

/// The actions a hardening statement EXEMPTS, so the holder of a long-term key can still
/// enrol a device and exchange one of its pass codes for a session: the action list of AWS's
/// own published sample policy for this pattern (IAM User Guide, *AWS: Allows MFA-authenticated
/// IAM users to manage their own MFA device on the Security credentials page*), trimmed to the
/// actions this emulator serves — the device's own lifecycle, the user read a console performs,
/// and the mint that presents the challenge.
const BOOTSTRAP_ACTIONS: &[&str] = &[
    "iam:GetUser",
    "iam:CreateVirtualMFADevice",
    "iam:EnableMFADevice",
    "iam:ListMFADevices",
    "iam:ListVirtualMFADevices",
    "iam:ResyncMFADevice",
    "iam:DeactivateMFADevice",
    "iam:DeleteVirtualMFADevice",
    "sts:GetSessionToken",
];

/// How the conditioned `Deny` names the actions it covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeniedActions {
    /// `NotAction` over [`BOOTSTRAP_ACTIONS`]: everything EXCEPT the bootstrap, which is how
    /// the published pattern is written.
    EverythingButTheBootstrap,
    /// `Action` naming this service's whole namespace, the narrower spelling of the same deny.
    EveryActionOfThisService,
}

/// Which resources the conditioned `Deny` is written against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeniedResources {
    /// `"*"`, as the published pattern writes it.
    Everything,
    /// This bucket and its objects, the same deny scoped to what it protects.
    TheBucketAndItsObjects,
}

/// One row: the conditioned `Deny` a user's only policy carries beside its grants, and whether
/// each of the three credentials is REFUSED the write.
struct Row {
    /// What to call the case in a failure.
    case: &'static str,
    actions: DeniedActions,
    resources: DeniedResources,
    /// The statement's `Condition` object, as JSON text.
    condition: &'static str,
    /// Whether the write is refused, per credential, in [`PRESENTED`] order.
    refused: [bool; 3],
}

/// The table. Rows 1-3 are the hardening pattern itself in its three spellings; rows 4 and 5
/// are the same statement with the clause written so that it holds for OTHER credentials, which
/// is what makes the first three a decision about the clause rather than about the deny alone.
const ROWS: &[Row] = &[
    Row {
        case: "the published pattern: a deny of everything but the bootstrap, unless MFA is \
               present",
        actions: DeniedActions::EverythingButTheBootstrap,
        resources: DeniedResources::Everything,
        condition: r#"{"BoolIfExists":{"aws:MultiFactorAuthPresent":"false"}}"#,
        refused: [true, true, false],
    },
    Row {
        case: "the same deny, scoped to the bucket it protects",
        actions: DeniedActions::EverythingButTheBootstrap,
        resources: DeniedResources::TheBucketAndItsObjects,
        condition: r#"{"BoolIfExists":{"aws:MultiFactorAuthPresent":"false"}}"#,
        refused: [true, true, false],
    },
    Row {
        case: "the same deny, written as an action wildcard over this service",
        actions: DeniedActions::EveryActionOfThisService,
        resources: DeniedResources::Everything,
        condition: r#"{"BoolIfExists":{"aws:MultiFactorAuthPresent":"false"}}"#,
        refused: [true, true, false],
    },
    Row {
        // Without the suffix the clause COMPARES a value, and a credential that carries none is
        // not compared at all: the trap `…IfExists` exists for, and the reason a long-term key
        // walks straight through a statement written this way.
        case: "a deny whose clause does not tolerate a missing key",
        actions: DeniedActions::EverythingButTheBootstrap,
        resources: DeniedResources::Everything,
        condition: r#"{"Bool":{"aws:MultiFactorAuthPresent":"false"}}"#,
        refused: [false, true, false],
    },
    Row {
        // The clause read the other way round: only a credential whose mint satisfied a
        // challenge answers `true`, so only that one is refused.
        case: "a deny of exactly the credentials that ARE MFA-backed",
        actions: DeniedActions::EverythingButTheBootstrap,
        resources: DeniedResources::Everything,
        condition: r#"{"Bool":{"aws:MultiFactorAuthPresent":"true"}}"#,
        refused: [false, false, true],
    },
];

/// The two ARNs a statement about [`BUCKET`] and its objects names.
fn bucket_resources() -> serde_json::Value {
    json!([
        format!("arn:aws:s3:::{BUCKET}"),
        format!("arn:aws:s3:::{BUCKET}/*")
    ])
}

/// The identity document one row's user carries: the bootstrap grant, the write grant the deny
/// is layered over, and the row's own conditioned `Deny`.
fn document(row: &Row) -> String {
    let condition: serde_json::Value =
        serde_json::from_str(row.condition).expect("the row's condition must be JSON");
    let mut deny = json!({
        "Sid": "DenyUnlessMFA",
        "Effect": "Deny",
        "Condition": condition,
    });
    let statement = deny
        .as_object_mut()
        .expect("the statement is a JSON object");
    match row.actions {
        DeniedActions::EverythingButTheBootstrap => {
            statement.insert("NotAction".to_string(), json!(BOOTSTRAP_ACTIONS));
        }
        DeniedActions::EveryActionOfThisService => {
            statement.insert("Action".to_string(), json!("s3:*"));
        }
    }
    match row.resources {
        DeniedResources::Everything => {
            statement.insert("Resource".to_string(), json!("*"));
        }
        DeniedResources::TheBucketAndItsObjects => {
            statement.insert("Resource".to_string(), bucket_resources());
        }
    }
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Sid": "BootstrapMFA",
                "Effect": "Allow",
                "Action": BOOTSTRAP_ACTIONS,
                "Resource": "*",
            },
            {
                "Sid": "S3Write",
                "Effect": "Allow",
                "Action": ["s3:PutObject", "s3:GetObject"],
                "Resource": format!("arn:aws:s3:::{BUCKET}/*"),
            },
            deny,
        ],
    })
    .to_string()
}

/// Create [`BUCKET`] as the account's own credential — and nothing in it, so a key the owner
/// finds afterwards can only have been written by the request under test.
async fn given_empty_bucket(emulator: &Emulator) {
    let created = emulator
        .s3(ACCOUNT_KEY, "PUT", &format!("/{BUCKET}"), b"")
        .await;
    assert_eq!(
        created.status, 200,
        "the bucket must be created first, got {} with {}",
        created.status, created.body
    );
}

/// What one attempted write answered, and what the OWNER then finds at the key it addressed —
/// the two halves of "a refused write leaves nothing".
struct Attempt {
    /// The HTTP status the write itself answered.
    status: u16,
    /// The `<Code>` of its error document, empty for a served write.
    code: String,
    /// The `<Message>` of that document.
    message: String,
    /// The status the bucket owner's `HEAD` of the same key answers afterwards.
    owner_head: u16,
}

/// Write `key` as `access_key_id`, and answer what the write was told together with what the
/// OWNER finds at that key afterwards.
async fn write_and_look(
    emulator: &Emulator,
    access_key_id: &str,
    key: &str,
) -> Attempt {
    let path = format!("/{BUCKET}/{key}");
    let answer = emulator.s3(access_key_id, "PUT", &path, BODY).await;
    let head = emulator.s3(ACCOUNT_KEY, "HEAD", &path, b"").await;
    Attempt {
        status: answer.status,
        code: answer.error_code(),
        message: answer.error_message(),
        owner_head: head.status,
    }
}

/// Assert that the write of `key` by `access_key_id` was refused with IAM's explicit-deny
/// sentence and stored nothing.
async fn assert_refused(emulator: &Emulator, case: &str, access_key_id: &str, key: &str, arn: &str) {
    let Attempt {
        status,
        code,
        message,
        owner_head: found,
    } = write_and_look(emulator, access_key_id, key).await;
    assert_eq!(
        status, ACCESS_DENIED_STATUS,
        "case {case:?}: expected HTTP {ACCESS_DENIED_STATUS}, got {status} with {message:?}"
    );
    assert_eq!(
        code, ACCESS_DENIED,
        "case {case:?}: expected {ACCESS_DENIED}, got {code:?}"
    );
    let expected = format!(
        "User: {arn} is not authorized to perform: {PUT_OBJECT_ACTION} on resource: \
         \"arn:aws:s3:::{BUCKET}/{key}\" {IDENTITY_DENY_CLAUSE}"
    );
    assert_eq!(
        message, expected,
        "case {case:?}: expected {expected:?}, got {message:?}"
    );
    assert_eq!(
        found, 404,
        "case {case:?}: the refused write must leave no object, but the owner's HEAD answered \
         {found}"
    );
}

/// Assert that the write of `key` by `access_key_id` was served and really stored [`BODY`].
async fn assert_served(emulator: &Emulator, case: &str, access_key_id: &str, key: &str) {
    let Attempt {
        status,
        code,
        message,
        owner_head: found,
    } = write_and_look(emulator, access_key_id, key).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the write to be authorized, got {status} {code} {message:?}"
    );
    assert_eq!(
        found, 200,
        "case {case:?}: the served write must leave the object behind, but the owner's HEAD \
         answered {found}"
    );
    let read = emulator
        .s3(ACCOUNT_KEY, "GET", &format!("/{BUCKET}/{key}"), b"")
        .await;
    let expected = String::from_utf8_lossy(BODY);
    assert_eq!(
        read.body, expected,
        "case {case:?}: the stored bytes must round-trip, got {:?}",
        read.body
    );
}

/// RULE: a request is decided against the principal's own documents over a context answering
/// the MFA key from the CREDENTIAL that signed it, so a `Deny` conditioned on that key refuses
/// exactly the credentials its clause holds for — whatever the shape of the statement, and
/// however broadly the grant beside it is written — with HTTP 403 `AccessDenied`, IAM's
/// explicit-deny sentence naming the principal, the action and the object, and nothing stored.
#[tokio::test]
async fn a_deny_unless_mfa_refuses_exactly_the_credentials_its_clause_holds_for() {
    for (index, row) in ROWS.iter().enumerate() {
        let emulator = Emulator::new();
        given_empty_bucket(&emulator).await;
        let credentials = emulator
            .user_with_document(&format!("hardened{index}"), &document(row))
            .await;
        for (presented, refused) in PRESENTED.into_iter().zip(row.refused) {
            let case = format!("{} / {presented:?}", row.case);
            let key = format!("deny-unless-mfa/{index}-{presented:?}.bin");
            let access_key_id = presented.access_key_id(&credentials);
            if refused {
                assert_refused(&emulator, &case, access_key_id, &key, &credentials.arn).await;
            } else {
                assert_served(&emulator, &case, access_key_id, &key).await;
            }
        }
    }
}

/// RULE, on the exemption half: the actions the `Deny` does not cover stay available under the
/// very credential it refuses — which is what lets the holder of a long-term key reach an
/// MFA-backed session at all — the session so minted speaks for the SAME IAM user, and the
/// long-term key's refusal is unchanged by the fact that a permitted credential exists.
#[tokio::test]
async fn the_bootstrap_actions_survive_the_deny_and_the_refusal_is_stable() {
    let row = &ROWS[0];
    let user = "hardened-bootstrap";
    let emulator = Emulator::new();
    given_empty_bucket(&emulator).await;
    // Minting the two sessions is itself part of the assertion: both are `sts:GetSessionToken`
    // calls made with the long-term key, which the deny exempts, so a deny applied to them
    // would fail this setup.
    let credentials = emulator.user_with_document(user, &document(row)).await;

    // The user read a console performs, and the device listing beside it: exempted actions this
    // issue's own report did not name, under the credential the deny refuses for S3.
    let long_term = Presented::LongTermKey.access_key_id(&credentials);
    for action in ["GetUser", "ListVirtualMFADevices", "ListMFADevices"] {
        let params = match action {
            "ListVirtualMFADevices" => Vec::new(),
            _ => vec![("UserName", user.to_string())],
        };
        let (status, body) = emulator
            .query(long_term, "iam", "2010-05-08", action, &params)
            .await;
        assert_eq!(
            status, 200,
            "case {action:?}: an action the deny exempts must still be served, got {status} \
             {body}"
        );
        if action == "GetUser" {
            let arn = element(&body, "Arn").unwrap_or_default();
            assert_eq!(
                arn, credentials.arn,
                "case {action:?}: the exempted read must answer the caller's own user, got \
                 {arn:?}"
            );
        }
    }

    // The write the deny covers, under the long-term key: refused, with nothing stored.
    assert_refused(
        &emulator,
        "the long-term key before the MFA session is used",
        long_term,
        "deny-unless-mfa/plain-attempt.bin",
        &credentials.arn,
    )
    .await;

    // The MFA-backed session speaks for the same IAM user ("no permissions are required to
    // perform this operation", `GetCallerIdentity`), and is permitted the identical write.
    let mfa_key = Presented::MfaSession.access_key_id(&credentials);
    let (status, body) = emulator
        .query(mfa_key, "sts", "2011-06-15", "GetCallerIdentity", &[])
        .await;
    assert_eq!(
        status, 200,
        "the MFA session must be able to report its own identity, got {status} {body}"
    );
    let arn = element(&body, "Arn").unwrap_or_default();
    assert_eq!(
        arn, credentials.arn,
        "the MFA session must speak for the same IAM user, got {arn:?}"
    );
    assert_served(
        &emulator,
        "the MFA-backed session",
        mfa_key,
        "deny-unless-mfa/mfa-attempt.bin",
    )
    .await;

    // And the long-term key is refused exactly as before: a credential that is allowed does not
    // lift the deny off one that is not.
    assert_refused(
        &emulator,
        "the long-term key after the MFA session wrote",
        long_term,
        "deny-unless-mfa/plain-retry.bin",
        &credentials.arn,
    )
    .await;
}
