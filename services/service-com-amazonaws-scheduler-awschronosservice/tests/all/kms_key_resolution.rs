// SPDX-License-Identifier: CC0-1.0

//! The customer managed key a WRITE states is resolved by that write: end to end over the
//! wire through the real `MultiServiceRouter`, with Amazon SQS, IAM and KMS mounted beside
//! EventBridge Scheduler on ONE engine — so the key a request names is resolved against the
//! very records `CreateKey` and `CreateAlias` wrote.
//!
//! RULE: `KmsKeyArn` names "the customer managed KMS key that EventBridge Scheduler will use
//! to encrypt and decrypt your data", so every write that states one resolves that reference
//! against KMS and is refused with 400 `ValidationException` — and persists nothing — unless
//! this account and Region hold a key the identifier resolves to. The refusal is the
//! service's own sentence around the key the request named
//! ([`state::kms_key_refusal`](service_com_amazonaws_scheduler_awschronosservice::state::kms_key_refusal)),
//! and a refused update leaves the stored schedule, key included, as the last accepted write
//! left it.
//!
//! The rows span the class rather than one recorded case: they differ along the FORM of the
//! identifier (a key ARN, an alias ARN), along WHERE the key it names lives (the Region the
//! request is signed for, another Region), and along whether KMS holds anything it resolves
//! to. The row stating no key at all is the control that what refuses a write is the key and
//! not the member.

use serde_json::{json, Value};
use service_com_amazonaws_scheduler_awschronosservice::state::kms_key_refusal;
use smithy_server_runtime::MultiServiceRouter;
use std::sync::Arc;

use crate::schedule_invocation::{
    at_expression_body, given_execution_role_for, given_queue, schedule_read, scheduler,
    seconds_from_now, REGION,
};
use crate::wire_harness::{account_id, error_code, error_message};

/// The group a schedule lands in when a write names none.
const DEFAULT_GROUP: &str = "default";

/// The error code every write of this service declares for an input it does not admit
/// (`smithy_query(query="operation", operation="CreateSchedule")`: `ValidationException`,
/// `@httpError 400`).
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// A second Region, for the rows that show a KMS key is a REGIONAL resource: it is resolved
/// where its own ARN says it lives.
const OTHER_REGION: &str = "eu-central-1";

/// How far ahead the schedules below state their one occurrence: far enough that no
/// occurrence falls due while the suite writes and reads them, because what is asserted
/// here is the WRITE.
const OCCURRENCE_IN: u64 = 3_600;

/// The emulator: EventBridge Scheduler, the service its targets live in, IAM, and — when
/// `with_kms` — KMS, on ONE engine, the shape `core/router/src/lib.rs` mounts them in. The
/// suite that leaves KMS out is the one where the lookup establishes nothing.
fn emulator_with_kms(with_kms: bool) -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_scheduler_awschronosservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    if with_kms {
        router.register_service(service_com_amazonaws_kms_trentservice::EmulatedService::new(
            Arc::clone(&engine),
        ));
    }
    router
}

/// One awsJson1_1 call against KMS, addressed to `region`'s endpoint.
async fn kms(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let host = format!("kms.{region}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", host.clone())
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("TrentService.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTTESTTEST/20260101/{region}/kms/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// Mint a key through the mounted KMS service's `region` endpoint and answer its ARN, so a
/// row whose key is meant to be REAL names one KMS can resolve.
async fn given_key(router: &MultiServiceRouter, case: &str, region: &str) -> String {
    let (status, created) = kms(router, region, "CreateKey", &json!({})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateKey in {region} answered {status} {created}"
    );
    created["KeyMetadata"]["Arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no KeyMetadata.Arn in {created}"))
        .to_string()
}

/// An alias in `region` for the key `target`, answered as the ALIAS ARN — the other form the
/// modelled `KmsKeyArn` pattern admits (`:(key|alias)\/[0-9a-zA-Z-_]*$`).
async fn given_alias(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    name: &str,
    target: &str,
) -> String {
    let (status, created) = kms(
        router,
        region,
        "CreateAlias",
        &json!({ "AliasName": format!("alias/{name}"), "TargetKeyId": target }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateAlias in {region} answered {status} {created}"
    );
    format!("arn:aws:kms:{region}:{}:alias/{name}", account_id())
}

/// A well-formed key ARN in `region` naming `key_id`, built to the modelled `KmsKeyArn`
/// pattern rather than taken from any recorded call — for a row whose key is meant to be
/// absent.
fn key_arn(region: &str, key_id: &str) -> String {
    format!("arn:aws:kms:{region}:{}:key/{key_id}", account_id())
}

/// WHAT the write states as its customer managed key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    /// No key at all, which asks for no customer managed key and resolves nothing.
    Unstated,
    /// A key KMS holds in the Region the request is signed for.
    HeldHere,
    /// A key KMS holds in another Region, named by its own ARN.
    HeldElsewhere,
    /// An alias ARN for a key KMS holds here.
    AliasHeldHere,
    /// A key ARN of this Region naming a key nothing minted.
    AbsentHere,
    /// A key ARN of another Region naming a key nothing minted there.
    AbsentElsewhere,
    /// An alias ARN naming an alias nothing created.
    AbsentAlias,
}

/// One row: the key the write states, and whether the write is served.
struct Row {
    case: &'static str,
    key: Key,
    served: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "no customer managed key at all",
        key: Key::Unstated,
        served: true,
    },
    Row {
        case: "a key KMS holds in the signing region",
        key: Key::HeldHere,
        served: true,
    },
    Row {
        case: "a key KMS holds in another region",
        key: Key::HeldElsewhere,
        served: true,
    },
    Row {
        case: "an alias ARN for a key KMS holds",
        key: Key::AliasHeldHere,
        served: true,
    },
    Row {
        case: "a key ARN of this region naming no key",
        key: Key::AbsentHere,
        served: false,
    },
    Row {
        case: "a key ARN of another region naming no key",
        key: Key::AbsentElsewhere,
        served: false,
    },
    Row {
        case: "an alias ARN naming no alias",
        key: Key::AbsentAlias,
        served: false,
    },
];

/// The keys a row's identifier is built from, minted once for the whole table.
struct Keys {
    here: String,
    elsewhere: String,
    alias_here: String,
}

/// What one row's write states as `KmsKeyArn` — `None` for the row that states none.
fn stated_key(row: &Row, keys: &Keys, index: usize) -> Option<String> {
    match row.key {
        Key::Unstated => None,
        Key::HeldHere => Some(keys.here.clone()),
        Key::HeldElsewhere => Some(keys.elsewhere.clone()),
        Key::AliasHeldHere => Some(keys.alias_here.clone()),
        // Key ids written to the form KMS mints — 32 hex digits in five hyphenated groups —
        // for keys no `CreateKey` of this suite ever made.
        Key::AbsentHere => Some(key_arn(
            REGION,
            &format!("00000000-0000-4000-8000-00000000000{index}"),
        )),
        Key::AbsentElsewhere => Some(key_arn(
            OTHER_REGION,
            &format!("11111111-1111-4111-8111-11111111111{index}"),
        )),
        Key::AbsentAlias => Some(format!(
            "arn:aws:kms:{REGION}:{}:alias/never-created-{index}",
            account_id()
        )),
    }
}

/// The schedule body a write states: one occurrence far enough ahead that nothing is
/// invoked while the suite runs, the target the suite's role may reach, and the key the row
/// is about.
fn schedule_body(target_arn: &str, role_arn: &str, kms_key_arn: Option<&str>) -> Value {
    let mut body = json!({
        "ScheduleExpression": format!("at({})", at_expression_body(seconds_from_now(OCCURRENCE_IN))),
        "ScheduleExpressionTimezone": "UTC",
        "FlexibleTimeWindow": { "Mode": "OFF" },
        "State": "ENABLED",
        "Target": {
            "Arn": target_arn,
            "RoleArn": role_arn,
            "Input": "{}",
        },
    });
    if let Some(key) = kms_key_arn {
        body["KmsKeyArn"] = json!(key);
    }
    body
}

/// The sentence itself, pinned as AWS words it rather than re-asserted from the helper the
/// handler uses: the fixed text, with the key the request named as its one variable part.
/// The rows differ along the FORM of that key, so nothing about the sentence depends on it.
#[test]
fn the_refusal_states_aws_wording_around_the_key_the_request_named() {
    let rows = [
        key_arn(REGION, "33333333-3333-4333-8333-333333333333"),
        key_arn(OTHER_REGION, "mrk-4444444444444444444444444444444"),
        format!(
            "arn:aws:kms:{REGION}:{}:alias/a-named-alias",
            account_id()
        ),
    ];
    for key_arn in rows {
        let expected = format!(
            "Unable to access the KMS key {key_arn}. Check that the key exists, and that the IAM \
             principal you use to call EventBridge Scheduler has permission to access the key, \
             then try again."
        );
        let sentence = kms_key_refusal(&key_arn);
        assert_eq!(
            sentence, expected,
            "case {key_arn:?}: expected {expected:?}, got {sentence:?}"
        );
    }
}

/// Every row: a create stating a key KMS resolves is served and reports that key back,
/// while one stating a key KMS holds none of is refused with the service's sentence and
/// stores no schedule at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_create_resolves_the_customer_managed_key_it_states() {
    let emulator = emulator_with_kms(true);
    let (_, target_arn) = given_queue(&emulator, "the suite", "kms-key-target").await;
    let role = given_execution_role_for(&emulator, "the suite", "kms-key-role", &[&target_arn]).await;
    let here = given_key(&emulator, "the suite", REGION).await;
    let keys = Keys {
        alias_here: given_alias(&emulator, "the suite", REGION, "kms-key-suite", &here).await,
        here,
        elsewhere: given_key(&emulator, "the suite", OTHER_REGION).await,
    };

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let name = format!("kms-key-create-{index}");
        let stated = stated_key(row, &keys, index);
        let (status, answered) = scheduler(
            &emulator,
            "POST",
            &format!("/schedules/{name}"),
            Some(schedule_body(&target_arn, &role, stated.as_deref())),
        )
        .await;

        let (read_status, read) = schedule_read(&emulator, DEFAULT_GROUP, &name).await;
        if row.served {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the create to be served, got {status} {answered}"
            );
            assert_eq!(
                read_status, 200,
                "case {case:?}: GetSchedule answered {read_status} {read}"
            );
            assert_eq!(
                read["KmsKeyArn"].as_str(),
                stated.as_deref(),
                "case {case:?}: expected the stored key {stated:?}, got {read}"
            );
        } else {
            let stated = stated.expect("a refused row states a key");
            assert_eq!(
                (status, error_code(&answered).as_str()),
                (400, VALIDATION_EXCEPTION),
                "case {case:?}: expected the modelled refusal, got {status} {answered}"
            );
            assert_eq!(
                error_message(&answered),
                kms_key_refusal(&stated),
                "case {case:?}: expected the key sentence, got {answered}"
            );
            // Nothing was written: the schedule the refused create named does not exist.
            assert_eq!(
                read_status, 404,
                "case {case:?}: a refused create must store no schedule, got {read_status} {read}"
            );
        }
    }
}

/// Every row: an update states the schedule's whole configuration, so it resolves the key it
/// states exactly as the create does — and a refused update leaves the stored key as the
/// last accepted write left it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_update_resolves_the_customer_managed_key_it_states() {
    let emulator = emulator_with_kms(true);
    let (_, target_arn) = given_queue(&emulator, "the suite", "kms-update-target").await;
    let role =
        given_execution_role_for(&emulator, "the suite", "kms-update-role", &[&target_arn]).await;
    let here = given_key(&emulator, "the suite", REGION).await;
    let keys = Keys {
        alias_here: given_alias(&emulator, "the suite", REGION, "kms-update-suite", &here).await,
        here,
        elsewhere: given_key(&emulator, "the suite", OTHER_REGION).await,
    };
    // The key every row's schedule is CREATED with, and the one a refused update has to
    // leave in place.
    let original = keys.here.clone();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let name = format!("kms-key-update-{index}");
        let (status, created) = scheduler(
            &emulator,
            "POST",
            &format!("/schedules/{name}"),
            Some(schedule_body(&target_arn, &role, Some(&original))),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateSchedule answered {status} {created}"
        );

        let stated = stated_key(row, &keys, index);
        let (status, answered) = scheduler(
            &emulator,
            "PUT",
            &format!("/schedules/{name}"),
            Some(schedule_body(&target_arn, &role, stated.as_deref())),
        )
        .await;
        if row.served {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the update to be served, got {status} {answered}"
            );
        } else {
            let refused = stated.clone().expect("a refused row states a key");
            assert_eq!(
                (status, error_code(&answered).as_str()),
                (400, VALIDATION_EXCEPTION),
                "case {case:?}: expected the modelled refusal, got {status} {answered}"
            );
            assert_eq!(
                error_message(&answered),
                kms_key_refusal(&refused),
                "case {case:?}: expected the key sentence, got {answered}"
            );
        }

        // What a read reports is what the schedule is encrypted under: an accepted update
        // carries the key it stated (the unstated row clears it), and a refused one leaves
        // the key the create took.
        let expected = if row.served {
            stated
        } else {
            Some(original.clone())
        };
        let (read_status, read) = schedule_read(&emulator, DEFAULT_GROUP, &name).await;
        assert_eq!(
            read_status, 200,
            "case {case:?}: GetSchedule answered {read_status} {read}"
        );
        assert_eq!(
            read["KmsKeyArn"].as_str(),
            expected.as_deref(),
            "case {case:?}: expected the stored key {expected:?}, got {read}"
        );
    }
}

/// With no KMS to ask, nothing is established about the key — so no write is refused on it.
/// A refusal has to be grounded in what KMS answered, never assumed from silence.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_no_kms_to_ask_a_key_refuses_nothing() {
    let emulator = emulator_with_kms(false);
    let (_, target_arn) = given_queue(&emulator, "the suite", "kms-silent-target").await;
    let role =
        given_execution_role_for(&emulator, "the suite", "kms-silent-role", &[&target_arn]).await;
    let stated = key_arn(REGION, "22222222-2222-4222-8222-222222222222");

    let name = "kms-key-unasked";
    let (status, answered) = scheduler(
        &emulator,
        "POST",
        &format!("/schedules/{name}"),
        Some(schedule_body(&target_arn, &role, Some(&stated))),
    )
    .await;
    assert_eq!(
        status, 200,
        "an unresolvable lookup must refuse nothing, got {status} {answered}"
    );
    let (read_status, read) = schedule_read(&emulator, DEFAULT_GROUP, name).await;
    assert_eq!(
        read_status, 200,
        "GetSchedule answered {read_status} {read}"
    );
    assert_eq!(
        read["KmsKeyArn"].as_str(),
        Some(stated.as_str()),
        "expected the stored key {stated:?}, got {read}"
    );
}
