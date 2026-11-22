// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the REGION a `kmsKeyId` is resolved in.
//!
//! The rule under test: **a KMS key ARN addresses one key in one Region, and CloudWatch Logs
//! resolves the key through the endpoint the request reached, so a `kmsKeyId` whose ARN names
//! any Region other than the request's names no key that can be found there — refused
//! `InvalidParameterException` with `The specified KMS Key Id could not be found.`, with
//! nothing recorded — whatever key that other Region holds.**
//!
//! The refusal is about the identifier, not about permission: no key of this Region was named,
//! so the key policy that would decide whether a key may be used is never reached. A key ARN
//! that names no Region LABEL at all (an empty Region field) disagrees with no Region, so it is
//! left to KMS to report on and keeps the class the resolution suite pins for a key ARN KMS
//! holds no key under (`tests/all/kms_key_resolution.rs`).
//!
//! CloudWatch Logs and KMS are mounted on ONE shared engine — the same assembly `core/router`
//! performs — so a key a row names in another Region is a key KMS itself minted through that
//! Region's endpoint, and no part of this suite tells CloudWatch Logs anything about it.
//!
//! The rows span what the rule generalises over: a key of the request's own Region, a key that
//! really exists in another Region, a key ARN of another Region that exists nowhere, the
//! request Region's own key id written under another Region, another partition's Region, and an
//! ARN naming no Region at all. Every row is driven through both operations that take a
//! `kmsKeyId` for a log group — `CreateLogGroup` and `AssociateKmsKey` — and the whole table is
//! run with the request signed for each of two Regions, so neither Region is "the" Region: what
//! decides a row is the relationship between the ARN and the request.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The two Regions the table is run in: each is the request's Region in one pass and "another
/// Region" in the other.
const REGIONS: [&str; 2] = ["eu-west-2", "us-east-1"];

/// The error code a refusal about the identifier carries, as `AssociateKmsKey` documents it.
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// The error code the association answers when the key cannot be used for the resource the
/// request names, which is what KMS holding no key under a key ARN of this Region amounts to.
const ACCESS_DENIED: &str = "AccessDeniedException";

/// The whole message an identifier that names no findable key is refused with. Fixed text for
/// its class, naming nothing of the request.
const KEY_NOT_FOUND: &str = "The specified KMS Key Id could not be found.";

/// CloudWatch Logs and KMS on one shared engine, reached over the wire.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Signing is not the subject here, so requests are unauthenticated; the credential
        // scope is still carried, because it is what names the Region.
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(service_com_amazonaws_kms_trentservice::EmulatedService::new(
            engine,
        ));
        Self { router }
    }

    /// One awsJson1_1 call to the service signing as `service` under `prefix`, with the
    /// credential scope naming `region`.
    async fn call(
        &self,
        service: &str,
        prefix: &str,
        region: &str,
        operation: &str,
        body: &Value,
    ) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("{prefix}.{operation}"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/{service}/\
                     aws4_request, SignedHeaders=host;x-amz-target, Signature=0123456789abcdef"
                ),
            )
            .body(bytes::Bytes::from(
                serde_json::to_vec(body).expect("the request body serializes"),
            ))
            .expect("a valid request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// One CloudWatch Logs call in `region`: its status and body.
    async fn logs(&self, region: &str, operation: &str, body: &Value) -> (u16, Value) {
        self.call("logs", "Logs_20140328", region, operation, body)
            .await
    }

    /// One CloudWatch Logs call that must succeed.
    async fn logs_ok(&self, case: &str, region: &str, operation: &str, body: &Value) -> Value {
        let (status, parsed) = self.logs(region, operation, body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {operation} {body} in {region} must succeed, got {status} {parsed}"
        );
        parsed
    }

    /// A KMS key created through `region`'s endpoint, answering its ARN.
    async fn created_key(&self, case: &str, region: &str) -> String {
        let (status, created) = self
            .call(
                "kms",
                "TrentService",
                region,
                "CreateKey",
                &json!({
                    "Policy": permissive_key_policy(),
                    "BypassPolicyLockoutSafetyCheck": true,
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: KMS CreateKey in {region} must succeed, got {status} {created}"
        );
        created["KeyMetadata"]["Arn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a created key carries an Arn, got {created}"))
            .to_string()
    }

    /// Whether CloudWatch Logs holds a log group under `name`.
    async fn holds_group(&self, case: &str, region: &str, name: &str) -> bool {
        self.described_group(case, region, name).await.is_some()
    }

    /// The `kmsKeyId` CloudWatch Logs reports for `name`, or `None` when it reports none.
    async fn reported_key(&self, case: &str, region: &str, name: &str) -> Option<String> {
        self.described_group(case, region, name)
            .await
            .and_then(|group| {
                group
                    .get("kmsKeyId")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
    }

    /// The one log group CloudWatch Logs reports under `name`, read back through the operation
    /// a client reads it with.
    async fn described_group(&self, case: &str, region: &str, name: &str) -> Option<Value> {
        let page = self
            .logs_ok(
                case,
                region,
                "DescribeLogGroups",
                &json!({"logGroupIdentifiers": [name]}),
            )
            .await;
        page["logGroups"]
            .as_array()
            .and_then(|groups| groups.first())
            .cloned()
    }
}

/// A key policy letting CloudWatch Logs use the key for any resource of this account, so the
/// KEY POLICY gate never decides a row here: a row is decided by the Region the ARN names.
fn permissive_key_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Sid": "Enable IAM User Permissions",
                "Effect": "Allow",
                "Principal": {"AWS": format!("arn:aws:iam::{}:root", engine::emulator_account_id())},
                "Action": "kms:*",
                "Resource": "*",
            },
            {
                "Effect": "Allow",
                "Principal": {"Service": "logs.amazonaws.com"},
                "Action": [
                    "kms:Encrypt*",
                    "kms:Decrypt*",
                    "kms:ReEncrypt*",
                    "kms:GenerateDataKey*",
                    "kms:Describe*",
                ],
                "Resource": "*",
            },
        ],
    })
    .to_string()
}

/// A log group's ARN in the form a refusal about the key names it — `LogGroup$logGroupArn`,
/// which carries no trailing `:*`.
fn log_group_arn(region: &str, name: &str) -> String {
    format!(
        "arn:aws:logs:{region}:{}:log-group:{name}",
        engine::emulator_account_id()
    )
}

/// The message a refusal body carries, read under the member name the error class names it
/// by: a modelled error of this service names it `message`, and the AWS COMMON
/// `AccessDeniedException` — which this service's model declares no shape for — names it
/// `Message`.
fn refusal_message<'a>(body: &'a Value, code: &str) -> Option<&'a str> {
    let member = if code == ACCESS_DENIED {
        "Message"
    } else {
        "message"
    };
    body.get(member).and_then(Value::as_str)
}

/// The key id an ARN's resource field carries, without the ARN around it.
fn key_id_of(case: &str, key_arn: &str) -> String {
    key_arn
        .rsplit_once("key/")
        .unwrap_or_else(|| panic!("case {case:?}: a key ARN carries a key id, got {key_arn:?}"))
        .1
        .to_string()
}

/// What the service must answer for one `kmsKeyId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The key is of the request's own Region and usable, so the association is recorded.
    Accepted,
    /// Refused: the ARN names another Region, so it names no key findable here.
    OutOfRegion,
    /// Refused: the ARN names no other Region, and KMS reports no key under it in this one.
    NoSuchKeyHere,
}

impl Expected {
    /// The code and message `CreateLogGroup` refuses this outcome with, or `None` when it
    /// creates the group: the create states ONE class for a key that is not there, which is
    /// what its own documentation says.
    fn create_refusal(self) -> Option<(&'static str, String)> {
        match self {
            Self::Accepted => None,
            Self::OutOfRegion | Self::NoSuchKeyHere => {
                Some((INVALID_PARAMETER, KEY_NOT_FOUND.to_string()))
            }
        }
    }

    /// The code and message `AssociateKmsKey` refuses this outcome with for the resource
    /// `resource_arn` names, or `None` when it records the association.
    fn associate_refusal(self, resource_arn: &str) -> Option<(&'static str, String)> {
        match self {
            // A key ARN of this Region KMS holds no key under is reported as the key not being
            // usable for the resource the request named, around that resource's own ARN.
            Self::NoSuchKeyHere => Some((
                ACCESS_DENIED,
                format!(
                    "The specified KMS key does not exist or is not allowed to be used with Arn \
                     '{resource_arn}'"
                ),
            )),
            other => other.create_refusal(),
        }
    }
}

/// One row: an identifier to name, and what the service must answer for it.
struct Row {
    /// What the row varies, named in every assertion.
    case: String,
    /// The `kmsKeyId` the row names.
    kms_key_id: String,
    /// What the service must answer.
    expected: Expected,
}

/// The rows for a request signed for `region`, where `elsewhere` is another Region: each key a
/// row names is minted through KMS's own API here, through the endpoint of the Region whose key
/// it is.
async fn rows(emulator: &Emulator, region: &str, elsewhere: &str) -> Vec<Row> {
    let case = "setup";
    let here = emulator.created_key(case, region).await;
    let there = emulator.created_key(case, elsewhere).await;
    let here_key_id = key_id_of(case, &here);
    let account = engine::emulator_account_id();

    vec![
        Row {
            case: format!("a key of the request's own Region {region}"),
            kms_key_id: here.clone(),
            expected: Expected::Accepted,
        },
        Row {
            case: format!("a key that really exists in {elsewhere}"),
            kms_key_id: there,
            expected: Expected::OutOfRegion,
        },
        Row {
            case: format!("a key ARN of {elsewhere} no key was ever minted under"),
            kms_key_id: engine::emulator_arn(
                "kms",
                elsewhere,
                "key/8d6a0b2e-19cc-4ff6-9a6f-2d0f6b5b1cf3",
            ),
            expected: Expected::OutOfRegion,
        },
        Row {
            case: format!("this Region's own key id written under {elsewhere}"),
            kms_key_id: format!("arn:aws:kms:{elsewhere}:{account}:key/{here_key_id}"),
            expected: Expected::OutOfRegion,
        },
        Row {
            case: "a key ARN of another partition's Region".to_string(),
            kms_key_id: format!("arn:aws-us-gov:kms:us-gov-west-1:{account}:key/{here_key_id}"),
            expected: Expected::OutOfRegion,
        },
        Row {
            case: "a key ARN naming no Region at all".to_string(),
            kms_key_id: format!("arn:aws:kms::{account}:key/{here_key_id}"),
            expected: Expected::NoSuchKeyHere,
        },
    ]
}

/// `CreateLogGroup` resolves the key in the Region the request was made in, and a create it
/// refuses leaves no group behind.
#[tokio::test]
async fn create_log_group_resolves_the_key_in_the_requests_own_region() {
    let emulator = Emulator::new();
    for (pass, region) in REGIONS.iter().enumerate() {
        let elsewhere = REGIONS[(pass + 1) % REGIONS.len()];
        for (index, row) in rows(&emulator, region, elsewhere).await.into_iter().enumerate() {
            let group = format!("region-scope/create-{pass}-{index}");
            let case = &row.case;
            let (status, parsed) = emulator
                .logs(
                    region,
                    "CreateLogGroup",
                    &json!({"logGroupName": group, "kmsKeyId": row.kms_key_id}),
                )
                .await;

            match row.expected.create_refusal() {
                None => {
                    assert_eq!(
                        status, 200,
                        "case {case:?}: the key is of this Region, so the create must succeed, \
                         got {status} {parsed}"
                    );
                    assert_eq!(
                        emulator.reported_key(case, region, &group).await.as_deref(),
                        Some(row.kms_key_id.as_str()),
                        "case {case:?}: the group reports the key it was created with"
                    );
                }
                Some((expected_code, expected)) => {
                    assert_eq!(
                        status, 400,
                        "case {case:?}: expected the create to be refused, got {status} {parsed}"
                    );
                    assert_eq!(
                        crate::harness::error_code(&parsed),
                        expected_code,
                        "case {case:?}: expected {expected_code}, got {parsed}"
                    );
                    assert_eq!(
                        refusal_message(&parsed, expected_code),
                        Some(expected.as_str()),
                        "case {case:?}: expected {expected:?}, got {parsed}"
                    );
                    assert!(
                        !emulator.holds_group(case, region, &group).await,
                        "case {case:?}: a refused create leaves no log group behind"
                    );
                }
            }
        }
    }
}

/// `AssociateKmsKey` resolves the key in the Region the request was made in: a key ARN of
/// another Region is the identifier being wrong, and the group it named keeps the key it had.
#[tokio::test]
async fn associate_kms_key_resolves_the_key_in_the_requests_own_region() {
    let emulator = Emulator::new();
    for (pass, region) in REGIONS.iter().enumerate() {
        let elsewhere = REGIONS[(pass + 1) % REGIONS.len()];
        for (index, row) in rows(&emulator, region, elsewhere).await.into_iter().enumerate() {
            let group = format!("region-scope/associate-{pass}-{index}");
            let case = &row.case;
            emulator
                .logs_ok(
                    case,
                    region,
                    "CreateLogGroup",
                    &json!({"logGroupName": group}),
                )
                .await;

            let (status, parsed) = emulator
                .logs(
                    region,
                    "AssociateKmsKey",
                    &json!({"logGroupName": group, "kmsKeyId": row.kms_key_id}),
                )
                .await;

            match row.expected.associate_refusal(&log_group_arn(region, &group)) {
                None => {
                    assert_eq!(
                        status, 200,
                        "case {case:?}: the key is of this Region, so the association must \
                         succeed, got {status} {parsed}"
                    );
                    assert_eq!(
                        emulator.reported_key(case, region, &group).await.as_deref(),
                        Some(row.kms_key_id.as_str()),
                        "case {case:?}: the group reports the associated key"
                    );
                }
                Some((expected_code, expected)) => {
                    assert_eq!(
                        status, 400,
                        "case {case:?}: expected the association to be refused, got {status} \
                         {parsed}"
                    );
                    assert_eq!(
                        crate::harness::error_code(&parsed),
                        expected_code,
                        "case {case:?}: expected {expected_code}, got {parsed}"
                    );
                    assert_eq!(
                        refusal_message(&parsed, expected_code),
                        Some(expected.as_str()),
                        "case {case:?}: expected {expected:?}, got {parsed}"
                    );
                    assert_eq!(
                        emulator.reported_key(case, region, &group).await,
                        None,
                        "case {case:?}: a refused association records no key"
                    );
                }
            }
        }
    }
}

/// A key ARN of another Region is refused even when the group already carries a key of its own
/// Region: the refusal is effect-free, so the key the group had stays the key it reports, and a
/// later association with a key of the request's Region is still accepted.
#[tokio::test]
async fn a_refused_cross_region_key_leaves_the_groups_own_key_in_place() {
    let emulator = Emulator::new();
    let case = "a cross-Region key named for a group that already holds one";
    let [region, elsewhere] = REGIONS;
    let group = "region-scope/effect-free";
    let here = emulator.created_key(case, region).await;
    let there = emulator.created_key(case, elsewhere).await;

    emulator
        .logs_ok(
            case,
            region,
            "CreateLogGroup",
            &json!({"logGroupName": group}),
        )
        .await;
    emulator
        .logs_ok(
            case,
            region,
            "AssociateKmsKey",
            &json!({"logGroupName": group, "kmsKeyId": here}),
        )
        .await;

    let (status, parsed) = emulator
        .logs(
            region,
            "AssociateKmsKey",
            &json!({"logGroupName": group, "kmsKeyId": there}),
        )
        .await;
    assert_eq!(
        status, 400,
        "case {case:?}: a key of {elsewhere} is refused, got {status} {parsed}"
    );
    assert_eq!(
        crate::harness::error_code(&parsed),
        INVALID_PARAMETER,
        "case {case:?}: expected {INVALID_PARAMETER}, got {parsed}"
    );
    assert_eq!(
        refusal_message(&parsed, INVALID_PARAMETER),
        Some(KEY_NOT_FOUND),
        "case {case:?}: expected {KEY_NOT_FOUND:?}, got {parsed}"
    );
    assert_eq!(
        emulator.reported_key(case, region, group).await.as_deref(),
        Some(here.as_str()),
        "case {case:?}: the refused association left the group's own key in place"
    );

    // And a key of the request's own Region is still accepted afterwards.
    let replacement = emulator.created_key(case, region).await;
    emulator
        .logs_ok(
            case,
            region,
            "AssociateKmsKey",
            &json!({"logGroupName": group, "kmsKeyId": replacement}),
        )
        .await;
    assert_eq!(
        emulator.reported_key(case, region, group).await.as_deref(),
        Some(replacement.as_str()),
        "case {case:?}: a key of this Region replaces the one the group held"
    );
}
