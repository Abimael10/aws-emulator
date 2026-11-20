// SPDX-License-Identifier: CC0-1.0

//! The LOCATION READ behind an `s3RecordingConfig`: whether Ground Station could make it,
//! and what S3 answered when it did.
//!
//! RULE: Ground Station obtains a recording bucket's location before `CreateConfig` or
//! `UpdateConfig` stores the config, and it makes that read by TAKING ON the role the config
//! names (`S3RecordingConfig$roleArn` is "the ARN of the role Ground Station assumes to write
//! data to the bucket"). Which of the two refusals a caller gets says which half of that
//! resolution failed:
//!
//! * a read that could NOT BE MADE, or that was made and came back without the location — this
//!   account's IAM holds nothing under the role ARN, the trust document it holds withholds the
//!   assume, or S3 answers about the bucket while refusing the read — is refused with the
//!   sentence naming the permission that read needs, whatever the bucket's own state is;
//! * only a read that WAS made and answered that there is no such bucket is refused with the
//!   sentence saying the bucket does not exist.
//!
//! Both carry the modelled `InvalidParameterException` (HTTP 431) with `parameterName`
//! `configData.s3RecordingConfig.bucketArn`, and each sentence is FIXED text: it interpolates
//! no bucket, ARN, account or region, so every row of one condition gets a byte-identical
//! sentence. The two sentences are written out in this file rather than only imported, so a
//! drift in the crate's constants fails this suite instead of travelling unnoticed.
//!
//! This suite carries the rows the sibling `all/groundstation_recording_bucket_role.rs` does
//! not: it crosses a bucket whose READ S3 REFUSES with each kind of role, and it varies the
//! trust document the role carries (a grant through an action wildcard, a grant among several
//! principals, a document that denies the assume, a readable document that grants nothing) and
//! the ACCOUNT the role ARN names, which is the dimension that decides whether anything is
//! known about the role at all. The three services and the two front doors are that suite's,
//! over one engine.
//!
//! A role under a PATH is no row here: `S3RecordingConfig$roleArn` is modelled
//! `^arn:[a-z0-9-.]{1,63}:iam::[0-9]{12}:role/[\w+=,.@-]{1,64}$`
//! (`smithy_query(query="shape", shape="S3RecordingConfig")`), whose resource part admits no
//! `/`, so such a value never reaches the resolution at all.

use serde_json::{json, Value};
use service_com_amazonaws_groundstation_groundstation::state::{
    GROUNDSTATION_SERVICE_PRINCIPAL, RECORDING_BUCKET_ABSENT, RECORDING_BUCKET_UNRESOLVABLE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::groundstation_recording_bucket_role::{
    create_bucket, emulator, groundstation, message_of, recording_config, role_with_trust,
    BLAMED_PARAMETER, REGION,
};

/// The sentence a read that could not be made, or that came back without the location, is
/// refused with. Spelled out in full, because the whole point of the rule is that it is FIXED
/// text.
const LOCATION_UNREADABLE: &str = "Unable to create new S3 endpoint config - unable to get \
     location of bucket. Ensure that Ground Station has s3:GetBucketLocation permissions on \
     the bucket.";

/// The sentence a read that WAS made and answered no-such-bucket is refused with.
const BUCKET_DOES_NOT_EXIST: &str = "Unable to create new S3 endpoint config - bucket does \
     not exist.";

/// The error shape both refusals carry.
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// A bucket the account holds and whose read S3 allows.
const READABLE_BUCKET: &str = "aws-groundstation-readable";

/// A bucket the account HOLDS and whose read S3 refuses, so S3 answers about it without
/// handing over where it is.
const DENIED_BUCKET: &str = "aws-groundstation-denied";

/// A bucket name nothing ever created.
const ABSENT_BUCKET: &str = "aws-groundstation-never-created";

/// An account that is NOT the one this emulator serves: a role ARN naming it names no role
/// this emulator's IAM can be asked about.
const OTHER_ACCOUNT: &str = "210987654321";

/// `PUT /<bucket>?policy` against the emulator's S3 front door — `PutBucketPolicy` with
/// `document` as the body.
///
/// A bucket policy is how a test puts S3 in the one state that makes it ANSWER about a bucket
/// while withholding where it is: an explicit denial of the read refuses whatever any grant
/// says, so it holds for the caller Ground Station asks as.
pub(crate) async fn put_bucket_policy(router: &MultiServiceRouter, bucket: &str, document: &str) {
    let request = http::Request::builder()
        .method("PUT")
        .uri(format!("https://s3.{REGION}.amazonaws.com/{bucket}?policy"))
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/s3/aws4_request, \
                 SignedHeaders=host, Signature=abc"
            ),
        )
        .body(bytes::Bytes::from(document.to_string()))
        .expect("a well-formed request");
    let status = router.dispatch(request).await.status().as_u16();
    assert!(
        (200..300).contains(&status),
        "PutBucketPolicy {bucket:?}: expected the denial to be attached, got {status}"
    );
}

/// A policy denying every principal the read of `bucket`.
pub(crate) fn deny_bucket_read(bucket: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Deny",
            "Principal": "*",
            "Action": "s3:ListBucket",
            "Resource": format!("arn:aws:s3:::{bucket}")
        }]
    })
    .to_string()
}

/// A trust policy with one statement per `(effect, principal, action)`, in the order given:
/// IAM reads an explicit `Deny` as winning over any `Allow`, so the order is not what decides
/// these rows.
fn trust(statements: &[(&str, Value, &str)]) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": statements
            .iter()
            .map(|(effect, principal, action)| json!({
                "Effect": effect,
                "Principal": { "Service": principal },
                "Action": action,
            }))
            .collect::<Vec<Value>>(),
    })
    .to_string()
}

/// Which role a row names, by what IAM holds under its ARN.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    /// Held, admitting Ground Station through an action wildcard (`sts:*`) rather than by
    /// naming the assume.
    TrustedByAnActionWildcard,
    /// Held, admitting several service principals, Ground Station among them.
    TrustedAmongSeveralPrincipals,
    /// Held, carrying a document that admits Ground Station in one statement and DENIES it in
    /// another.
    DeniedByItsTrustPolicy,
    /// Held, carrying a readable policy that grants nothing at all.
    GrantingNothing,
    /// A well-formed role ARN of ANOTHER account, which this emulator's IAM is not asked
    /// about, so nothing is known about the assume.
    OfAnotherAccount,
}

/// Which bucket a row records to.
#[derive(Clone, Copy, Debug)]
enum Bucket {
    /// A bucket the account holds whose read S3 allows.
    Readable,
    /// A bucket the account holds whose read S3 refuses.
    ReadDenied,
    /// A key under the bucket whose read S3 refuses — the same bucket, named with a prefix
    /// after it.
    KeyUnderReadDenied,
    /// A bucket nothing holds.
    Absent,
}

impl Bucket {
    /// The `bucketArn` this bucket is named by.
    fn arn(self) -> String {
        match self {
            Self::Readable => format!("arn:aws:s3:::{READABLE_BUCKET}"),
            Self::ReadDenied => format!("arn:aws:s3:::{DENIED_BUCKET}"),
            Self::KeyUnderReadDenied => format!("arn:aws:s3:::{DENIED_BUCKET}/downlink/2026/01"),
            Self::Absent => format!("arn:aws:s3:::{ABSENT_BUCKET}"),
        }
    }
}

/// One row: the bucket the config records to, the role it names, and the refusal the write
/// earns (`None` for a config that is stored).
struct Row {
    case: &'static str,
    bucket: Bucket,
    role: Role,
    refusal: Option<&'static str>,
}

/// Every row, built once so both writers of the resolution are driven over the same set.
///
/// The read is made under the role, so the ROLE decides first: every row whose role stops the
/// assume earns the location sentence whatever its bucket is, and only a row whose role lets
/// the read happen is decided by what S3 answered about the bucket.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a readable bucket through a role trusted by an sts:* grant",
            bucket: Bucket::Readable,
            role: Role::TrustedByAnActionWildcard,
            refusal: None,
        },
        Row {
            case: "an absent bucket through a role trusted by an sts:* grant",
            bucket: Bucket::Absent,
            role: Role::TrustedByAnActionWildcard,
            refusal: Some(BUCKET_DOES_NOT_EXIST),
        },
        Row {
            case: "a readable bucket through a role trusted among several principals",
            bucket: Bucket::Readable,
            role: Role::TrustedAmongSeveralPrincipals,
            refusal: None,
        },
        Row {
            case: "a bucket whose read S3 refuses, through a role trusted by an sts:* grant",
            bucket: Bucket::ReadDenied,
            role: Role::TrustedByAnActionWildcard,
            refusal: Some(LOCATION_UNREADABLE),
        },
        Row {
            case: "an absent bucket through a role trusted among several principals",
            bucket: Bucket::Absent,
            role: Role::TrustedAmongSeveralPrincipals,
            refusal: Some(BUCKET_DOES_NOT_EXIST),
        },
        Row {
            case: "a key under a bucket whose read S3 refuses, through a role trusted among \
                   several principals",
            bucket: Bucket::KeyUnderReadDenied,
            role: Role::TrustedAmongSeveralPrincipals,
            refusal: Some(LOCATION_UNREADABLE),
        },
        Row {
            case: "a readable bucket through a role whose trust policy denies Ground Station",
            bucket: Bucket::Readable,
            role: Role::DeniedByItsTrustPolicy,
            refusal: Some(LOCATION_UNREADABLE),
        },
        Row {
            case: "an absent bucket through a role whose trust policy denies Ground Station",
            bucket: Bucket::Absent,
            role: Role::DeniedByItsTrustPolicy,
            refusal: Some(LOCATION_UNREADABLE),
        },
        Row {
            case: "a readable bucket through a role whose trust policy grants nothing",
            bucket: Bucket::Readable,
            role: Role::GrantingNothing,
            refusal: Some(LOCATION_UNREADABLE),
        },
        Row {
            case: "a bucket whose read S3 refuses, through a role whose trust policy grants \
                   nothing",
            bucket: Bucket::ReadDenied,
            role: Role::GrantingNothing,
            refusal: Some(LOCATION_UNREADABLE),
        },
        Row {
            case: "a readable bucket through another account's role",
            bucket: Bucket::Readable,
            role: Role::OfAnotherAccount,
            refusal: None,
        },
        Row {
            case: "an absent bucket through another account's role",
            bucket: Bucket::Absent,
            role: Role::OfAnotherAccount,
            refusal: Some(BUCKET_DOES_NOT_EXIST),
        },
        Row {
            case: "a bucket whose read S3 refuses, through another account's role",
            bucket: Bucket::ReadDenied,
            role: Role::OfAnotherAccount,
            refusal: Some(LOCATION_UNREADABLE),
        },
    ]
}

/// One emulator holding the two buckets the rows name — one readable, one whose read S3
/// refuses — and the roles the rows name, with the ARN of each.
async fn harness() -> (MultiServiceRouter, Vec<(Role, String)>) {
    let router = emulator();
    create_bucket(&router, READABLE_BUCKET).await;
    create_bucket(&router, DENIED_BUCKET).await;
    put_bucket_policy(&router, DENIED_BUCKET, &deny_bucket_read(DENIED_BUCKET)).await;

    let by_wildcard = role_with_trust(
        &router,
        "gs-recording-action-wildcard",
        &trust(&[(
            "Allow",
            json!(GROUNDSTATION_SERVICE_PRINCIPAL),
            // An action wildcard IAM admits in place of the assume.
            "sts:*",
        )]),
    )
    .await;
    let among_principals = role_with_trust(
        &router,
        "gs-recording-several-principals",
        &trust(&[(
            "Allow",
            json!(["ec2.amazonaws.com", GROUNDSTATION_SERVICE_PRINCIPAL]),
            "sts:AssumeRole",
        )]),
    )
    .await;
    let denied = role_with_trust(
        &router,
        "gs-recording-denied-by-trust",
        &trust(&[
            (
                "Allow",
                json!(GROUNDSTATION_SERVICE_PRINCIPAL),
                "sts:AssumeRole",
            ),
            (
                "Deny",
                json!(GROUNDSTATION_SERVICE_PRINCIPAL),
                "sts:AssumeRole",
            ),
        ]),
    )
    .await;
    let grants_nothing = role_with_trust(
        &router,
        "gs-recording-grants-nothing",
        &json!({ "Version": "2012-10-17", "Statement": [] }).to_string(),
    )
    .await;

    assert_ne!(
        OTHER_ACCOUNT,
        engine::EMULATOR_ACCOUNT_ID,
        "case setup: the other account must not be the account this emulator serves"
    );
    let roles = vec![
        (Role::TrustedByAnActionWildcard, by_wildcard),
        (Role::TrustedAmongSeveralPrincipals, among_principals),
        (Role::DeniedByItsTrustPolicy, denied),
        (Role::GrantingNothing, grants_nothing),
        (
            Role::OfAnotherAccount,
            format!("arn:aws:iam::{OTHER_ACCOUNT}:role/gs-recording-elsewhere"),
        ),
    ];
    (router, roles)
}

/// The ARN of the role `role` names in a harness.
fn role_arn_of(roles: &[(Role, String)], role: Role) -> String {
    roles
        .iter()
        .find(|(held, _)| *held == role)
        .map(|(_, arn)| arn.clone())
        .unwrap_or_else(|| panic!("the harness holds a role for {role:?}"))
}

/// The error shape a refusal names, out of the restJson1 envelope's `__type`, which may carry
/// the shape's namespace.
pub(crate) fn error_code_of(payload: &Value) -> String {
    payload
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Assert one refusal: the status, the error shape, the sentence word for word, the member it
/// blames, and that the sentence took nothing from the request.
fn assert_refusal(case: &str, status: u16, payload: &Value, expected: &str, quoted: &[&str]) {
    assert_eq!(
        status, 431,
        "case {case:?}: expected HTTP 431, got {status} with {payload}"
    );
    assert_eq!(
        error_code_of(payload),
        INVALID_PARAMETER,
        "case {case:?}: expected {INVALID_PARAMETER}, got {payload}"
    );
    assert_eq!(
        message_of(payload),
        expected,
        "case {case:?}: expected {expected:?}, got {payload}"
    );
    assert_eq!(
        payload.get("parameterName"),
        Some(&json!(BLAMED_PARAMETER)),
        "case {case:?}: expected the bucketArn member to be blamed, got {payload}"
    );
    for value in quoted {
        assert!(
            !message_of(payload).contains(value),
            "case {case:?}: the refusal interpolates nothing from the request, got {payload}"
        );
    }
}

#[tokio::test]
async fn the_refusal_says_whether_the_location_read_was_made_and_what_it_answered() {
    let (router, roles) = harness().await;

    // The crate's exported sentences and the ones this suite pins are the same text, so a
    // drift in either fails here rather than passing unnoticed.
    assert_eq!(
        RECORDING_BUCKET_UNRESOLVABLE, LOCATION_UNREADABLE,
        "case exported-constant: expected the unobtainable-location sentence, got \
         {RECORDING_BUCKET_UNRESOLVABLE:?}"
    );
    assert_eq!(
        RECORDING_BUCKET_ABSENT, BUCKET_DOES_NOT_EXIST,
        "case exported-constant: expected the bucket-does-not-exist sentence, got \
         {RECORDING_BUCKET_ABSENT:?}"
    );

    let mut stored: Vec<String> = Vec::new();
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let role_arn = role_arn_of(&roles, row.role);
        let bucket_arn = row.bucket.arn();
        let name = format!("recording-{index}");
        let (status, payload) = groundstation(
            &router,
            "POST",
            "/config",
            Some(json!({
                "name": name,
                "configData": recording_config(&bucket_arn, &role_arn),
            })),
        )
        .await;

        match row.refusal {
            None => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: a bucket whose location Ground Station obtained must be \
                     stored, got {status} with {payload}"
                );
                assert_eq!(
                    payload.get("configType"),
                    Some(&json!("s3-recording")),
                    "case {case:?}: expected an s3-recording config, got {payload}"
                );
                // Round trip: the stored config reads back with the configData it was created
                // with.
                let path = format!(
                    "/config/s3-recording/{}",
                    payload["configId"].as_str().expect("a minted config id")
                );
                let (read_status, read_back) = groundstation(&router, "GET", &path, None).await;
                assert_eq!(
                    read_status, 200,
                    "case {case:?}: expected the stored config to be readable, got \
                     {read_status} with {read_back}"
                );
                assert_eq!(
                    read_back.get("configData"),
                    Some(&recording_config(&bucket_arn, &role_arn)),
                    "case {case:?}: expected the created configData, got {read_back}"
                );
                stored.push(name);
            }
            Some(expected) => {
                assert_refusal(
                    case,
                    status,
                    &payload,
                    expected,
                    &[bucket_arn.as_str(), role_arn.as_str()],
                );
            }
        }
    }

    // The listing is the store: exactly the configs whose location read succeeded are there,
    // so no refused row wrote anything.
    let (status, listed) = groundstation(&router, "GET", "/config", None).await;
    assert_eq!(
        status, 200,
        "case refused-creates-store-nothing: expected ListConfigs to answer, got {status} with \
         {listed}"
    );
    let mut names: Vec<String> = listed["configList"]
        .as_array()
        .expect("a configList array")
        .iter()
        .map(|config| config["name"].as_str().unwrap_or_default().to_string())
        .collect();
    // Compared as a SET: which configs are there is the rule, the listing's order is not.
    names.sort();
    stored.sort();
    assert_eq!(
        names, stored,
        "case refused-creates-store-nothing: expected only the stored configs, got {listed}"
    );
}

#[tokio::test]
async fn an_update_is_held_to_the_same_read_and_changes_nothing_when_refused() {
    let (router, roles) = harness().await;
    let readable = role_arn_of(&roles, Role::TrustedByAnActionWildcard);
    let held = recording_config(&Bucket::Readable.arn(), &readable);

    let (status, created) = groundstation(
        &router,
        "POST",
        "/config",
        Some(json!({ "name": "recording-held", "configData": held.clone() })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case setup: the control config must be stored, got {status} with {created}"
    );
    let path = format!(
        "/config/s3-recording/{}",
        created["configId"].as_str().expect("a minted config id")
    );

    for row in rows() {
        let Some(expected) = row.refusal else {
            continue;
        };
        let case = row.case;
        let role_arn = role_arn_of(&roles, row.role);
        let bucket_arn = row.bucket.arn();
        let (status, payload) = groundstation(
            &router,
            "PUT",
            &path,
            Some(json!({
                "name": "recording-held",
                "configData": recording_config(&bucket_arn, &role_arn),
            })),
        )
        .await;
        assert_refusal(
            &format!("update to {case}"),
            status,
            &payload,
            expected,
            &[bucket_arn.as_str(), role_arn.as_str()],
        );
    }

    // Every one of those updates was refused, so the stored config still records where it was
    // created to record.
    let (status, read_back) = groundstation(&router, "GET", &path, None).await;
    assert_eq!(
        status, 200,
        "case refused-updates-change-nothing: expected the config to be readable, got {status} \
         with {read_back}"
    );
    assert_eq!(
        read_back.get("configData"),
        Some(&held),
        "case refused-updates-change-nothing: expected the created configData, got {read_back}"
    );
}
