// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `CancelRotateSecret`, driven over the wire through the
//! multi-service router and read back through `DescribeSecret`, `ListSecrets`,
//! `GetSecretValue` and `RotateSecret`.
//!
//! The rules under test:
//!
//! * `CancelRotateSecret` turns automatic rotation off for the secret a
//!   `SecretId` resolves to — rotation is reported disabled and the schedule
//!   stops making a rotation due — while the rotation configuration the secret
//!   stores is left in place, so a later `RotateSecret` that names none of it
//!   rotates with the values already stored;
//! * a rotation still in progress is cancelled and its `AWSPENDING` version id
//!   is reported back, and only then: a call with no rotation in progress
//!   reports no version id. The staging labels are left as the cancelled
//!   rotation left them;
//! * the call reports the secret's ARN and name and succeeds whether or not
//!   rotation was ever configured, so repeating it answers the same thing;
//! * the call DECIDES whether rotation is on, so from there every projection of
//!   the secret reports `RotationEnabled` as the boolean false even for a secret
//!   `RotateSecret` never configured rotation for, while a secret no rotation call
//!   ever reached reports the member not at all;
//! * a `SecretId` that names no secret is `ResourceNotFoundException`, and a
//!   secret inside its recovery window — which still exists — is
//!   `InvalidRequestException`;
//! * the candidate a cancelled rotation left staked is the one the NEXT
//!   `RotateSecret` works on: that call resumes the cancelled rotation on the id it
//!   already holds — so it reports that id, mints no version for the token it
//!   carried and is not refused for the outstanding candidate — while a rotation
//!   nobody cancelled keeps the documented refusal for a rotation still in progress;
//! * a rotation that was configured but never STARTED stakes no candidate at all,
//!   so the cancel of it clears only the schedule and the rotation after it is a
//!   rotation of its own, on the version the call names.
//!
//! Every test is a TABLE over the class its rule generalises: how the rotation
//! schedule is expressed (days, a `rate()` interval, a `cron()` expression),
//! whether the rotation was started immediately, the form of the `SecretId`
//! (name, full ARN, partial ARN), and whether rotation was ever configured.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_secretsmanager_secretsmanager::state::ROTATION_IN_PROGRESS_MESSAGE;
use service_com_amazonaws_secretsmanager_secretsmanager::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// A SigV4 header naming `secretsmanager` in `us-west-2`; the router routes on
/// the credential scope and the emulator verifies signatures only when an
/// authenticator is registered, which this harness does not do.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-west-2/secretsmanager/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=deadbeef";

/// The ARN of a Lambda rotation function, as `RotationLambdaARN` carries it.
const LAMBDA_ARN: &str = "arn:aws:lambda:us-west-2:123456789012:function:RotateMySecret";

/// The one sentence `ResourceNotFoundException` carries for a secret this
/// service does not hold ("Secrets Manager can't find the resource that you
/// asked for.", `ResourceNotFoundException`, this service's Smithy model).
const SECRET_NOT_FOUND: &str = "Secrets Manager can't find the specified secret.";

/// The sentence an operation a secret's state forbids carries ("The secret is
/// scheduled for deletion.", `InvalidRequestException`, this service's Smithy
/// model).
const MARKED_FOR_DELETION: &str =
    "You can't perform this operation on the secret because it was marked for deletion.";

/// How one row names the secret in its cancel: the secret's name and its full
/// ARN go in, the `SecretId` the call carries comes out.
type SecretIdForm = fn(&str, &str) -> String;

/// How rotation was configured for one row, and whether a rotation was started.
struct Setup {
    /// The `RotationRules` the `RotateSecret` call carries.
    rules: Value,
    /// Whether that call started a rotation straight away, which is what leaves
    /// a rotation in progress for `CancelRotateSecret` to cancel.
    rotate_immediately: bool,
    /// Whether the schedule makes a next rotation due, which every schedule the
    /// service reads does: an interval names one a span past the call, and a
    /// `cron()` expression the next occurrence it names on the UTC calendar.
    schedules_next: bool,
}

/// A fresh emulator holding only this service, over its own engine.
fn emulator() -> MultiServiceRouter {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(EmulatedService::new(engine));
    router
}

/// Send one `awsJson1_1` request and return its status and JSON body.
async fn call(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("secretsmanager.{operation}"))
        .header("authorization", AUTHORIZATION)
        .body(bytes::Bytes::from(
            // A write that stores a value creates a version the caller has to
            // name; see `secret_client_request_token::with_staked_version`.
            serde_json::to_vec(&crate::secret_client_request_token::with_staked_version(
                operation, body,
            ))
            .expect("the request body is serializable"),
        ))
        .expect("a valid request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// Send one request that must succeed, and return its body.
async fn ok(router: &MultiServiceRouter, operation: &str, body: Value, case: &str) -> Value {
    let (status, parsed) = call(router, operation, &body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from {operation}, got {status} with body {parsed}"
    );
    parsed
}

/// Send one request that must be refused with a 400, and return its wire error
/// code and message.
async fn err(
    router: &MultiServiceRouter,
    operation: &str,
    body: Value,
    case: &str,
) -> (String, String) {
    let (status, parsed) = call(router, operation, &body).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected a 400 client error from {operation}, got {status} with body \
         {parsed}"
    );
    let raw = parsed
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no __type in error body {parsed}"));
    let code = raw.split('#').next_back().unwrap_or(raw).to_string();
    let message = parsed
        .get("message")
        .or_else(|| parsed.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no message in error body {parsed}"))
        .to_string();
    (code, message)
}

/// A required string member of a response body.
fn text(parsed: &Value, member: &str, case: &str) -> String {
    parsed
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no {member} in {parsed}"))
        .to_string()
}

/// The staging labels a `VersionIdsToStages` map reports for one version id, or
/// an empty list when the map does not carry that id at all.
fn stages_of(parsed: &Value, member: &str, version_id: &str) -> Vec<String> {
    parsed
        .get(member)
        .and_then(Value::as_object)
        .and_then(|map| map.get(version_id))
        .and_then(Value::as_array)
        .map(|stages| {
            stages
                .iter()
                .map(|stage| stage.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Create a secret holding `value`, and return its ARN and the id of the
/// version `AWSCURRENT` names.
async fn create_secret(
    router: &MultiServiceRouter,
    name: &str,
    value: &str,
    case: &str,
) -> (String, String) {
    let created = ok(
        router,
        "CreateSecret",
        json!({"Name": name, "SecretString": value}),
        case,
    )
    .await;
    (
        text(&created, "ARN", case),
        text(&created, "VersionId", case),
    )
}

/// `DescribeSecret` for one secret.
async fn describe(router: &MultiServiceRouter, secret_id: &str, case: &str) -> Value {
    ok(router, "DescribeSecret", json!({"SecretId": secret_id}), case).await
}

/// The `SecretListEntry` `ListSecrets` reports for the secret named `name`.
async fn list_entry(router: &MultiServiceRouter, name: &str, case: &str) -> Value {
    let listed = ok(
        router,
        "ListSecrets",
        json!({"Filters": [{"Key": "name", "Values": [name]}]}),
        case,
    )
    .await;
    listed
        .get("SecretList")
        .and_then(Value::as_array)
        .and_then(|entries| entries.first())
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no entry for {name} in {listed}"))
}

/// The ARN without the trailing hyphen and six random characters — the partial
/// ARN form a `SecretId` is also accepted in.
fn partial_arn(arn: &str) -> String {
    arn[..arn.len() - 7].to_string()
}

/// `CancelRotateSecret` reports the secret it names, turns rotation off and
/// clears the next due instant, keeps the rotation configuration, and reports
/// the `AWSPENDING` version id exactly when it cancelled a rotation still in
/// progress.
///
/// The rows differ along how the schedule is expressed and at which bound,
/// whether the rotation was started immediately, and the form of the `SecretId`
/// the cancel names.
#[tokio::test]
async fn cancel_turns_rotation_off_and_keeps_the_configuration() {
    // (case, how rotation was configured, how the cancel names the secret)
    let rows: &[(&str, Setup, SecretIdForm)] = &[
        (
            "a days schedule at the documented minimum, rotation in progress, cancelled by name",
            Setup {
                rules: json!({"AutomaticallyAfterDays": 1}),
                rotate_immediately: true,
                schedules_next: true,
            },
            |name, _arn| name.to_string(),
        ),
        (
            "a rate in hours at the documented four-hour floor, rotation in progress, cancelled \
             by full ARN",
            Setup {
                rules: json!({"ScheduleExpression": "rate(4 hours)"}),
                rotate_immediately: true,
                schedules_next: true,
            },
            |_name, arn| arn.to_string(),
        ),
        (
            "a days schedule at the documented maximum, nothing staged, cancelled by partial ARN",
            Setup {
                rules: json!({"AutomaticallyAfterDays": 1000}),
                rotate_immediately: false,
                schedules_next: true,
            },
            |_name, arn| partial_arn(arn),
        ),
        (
            "a cron schedule, nothing staged, cancelled by name",
            Setup {
                rules: json!({"ScheduleExpression": "cron(0 8 1 * ? *)"}),
                rotate_immediately: false,
                schedules_next: true,
            },
            |name, _arn| name.to_string(),
        ),
        (
            "a rate in days with a rotation window, rotation in progress, cancelled by name",
            Setup {
                rules: json!({"ScheduleExpression": "rate(10 days)", "Duration": "3h"}),
                rotate_immediately: true,
                schedules_next: true,
            },
            |name, _arn| name.to_string(),
        ),
    ];

    let router = emulator();
    for (index, (case, setup, secret_id_of)) in rows.iter().enumerate() {
        let name = format!("cancelled-{index}");
        let value = format!("the-current-value-{index}");
        let (arn, current) = create_secret(&router, &name, &value, case).await;

        let rotated = ok(
            &router,
            "RotateSecret",
            json!({
                "SecretId": name,
                "RotationLambdaARN": LAMBDA_ARN,
                "RotationRules": setup.rules,
                "RotateImmediately": setup.rotate_immediately,
            }),
            case,
        )
        .await;
        let rotation_version = text(&rotated, "VersionId", case);

        // Rotation is on, and an interval schedule has fixed the next due
        // instant, BEFORE the cancel — so what the cancel changes is observable.
        let described = describe(&router, &name, case).await;
        assert_eq!(
            described.get("RotationEnabled"),
            Some(&json!(true)),
            "case {case:?}: expected RotationEnabled true after RotateSecret, got {described}"
        );
        assert_eq!(
            described.get("NextRotationDate").is_some(),
            setup.schedules_next,
            "case {case:?}: expected NextRotationDate present={} after RotateSecret, got \
             {described}",
            setup.schedules_next
        );

        let secret_id = secret_id_of(&name, &arn);
        let cancelled = ok(
            &router,
            "CancelRotateSecret",
            json!({"SecretId": secret_id}),
            case,
        )
        .await;
        assert_eq!(
            text(&cancelled, "ARN", case),
            arn,
            "case {case:?}: expected CancelRotateSecret to report the secret's ARN {arn}, got \
             {cancelled}"
        );
        assert_eq!(
            text(&cancelled, "Name", case),
            name,
            "case {case:?}: expected CancelRotateSecret to report the secret's name {name}, got \
             {cancelled}"
        );
        if setup.rotate_immediately {
            // The version the cancelled rotation was creating, which the caller
            // needs in order to clean it up.
            assert_eq!(
                cancelled.get("VersionId"),
                Some(&json!(rotation_version)),
                "case {case:?}: expected the cancelled rotation's AWSPENDING version \
                 {rotation_version:?}, got {cancelled}"
            );
        } else {
            assert_eq!(
                cancelled.get("VersionId"),
                None,
                "case {case:?}: expected no VersionId when no rotation was in progress, got \
                 {cancelled}"
            );
        }

        // Rotation is off and nothing is due, while the configuration the
        // secret stores is untouched.
        let described = describe(&router, &secret_id, case).await;
        assert_eq!(
            described.get("RotationEnabled"),
            Some(&json!(false)),
            "case {case:?}: expected RotationEnabled false after the cancel, got {described}"
        );
        assert_eq!(
            described.get("NextRotationDate"),
            None,
            "case {case:?}: expected no NextRotationDate once rotation is off, got {described}"
        );
        assert_eq!(
            text(&described, "RotationLambdaARN", case),
            LAMBDA_ARN,
            "case {case:?}: expected the rotation function to survive the cancel, got {described}"
        );
        assert_eq!(
            described.get("RotationRules"),
            Some(&setup.rules),
            "case {case:?}: expected the rotation rules to survive the cancel, got {described}"
        );
        assert_eq!(
            described.get("LastRotatedDate"),
            None,
            "case {case:?}: expected no LastRotatedDate for a rotation that never completed, got \
             {described}"
        );

        // The staging labels are left as the cancelled rotation left them: the
        // current value is still current and still readable, and a cancelled
        // in-progress rotation still holds AWSPENDING on the version it was
        // creating.
        assert_eq!(
            stages_of(&described, "VersionIdsToStages", &current),
            vec!["AWSCURRENT".to_string()],
            "case {case:?}: expected the stored version to keep AWSCURRENT, got {described}"
        );
        let expected_pending: Vec<String> = if setup.rotate_immediately {
            vec!["AWSPENDING".to_string()]
        } else {
            Vec::new()
        };
        assert_eq!(
            stages_of(&described, "VersionIdsToStages", &rotation_version),
            expected_pending,
            "case {case:?}: expected the cancelled rotation's labels to be left alone, got \
             {described}"
        );
        let read = ok(
            &router,
            "GetSecretValue",
            json!({"SecretId": secret_id}),
            case,
        )
        .await;
        assert_eq!(
            text(&read, "SecretString", case),
            value,
            "case {case:?}: expected the current value to still be readable, got {read}"
        );

        // ListSecrets projects the same rotation state.
        let listed = ok(
            &router,
            "ListSecrets",
            json!({"Filters": [{"Key": "name", "Values": [name]}]}),
            case,
        )
        .await;
        let entry = listed
            .get("SecretList")
            .and_then(Value::as_array)
            .and_then(|list| list.first())
            .unwrap_or_else(|| panic!("case {case:?}: no SecretList entry in {listed}"));
        assert_eq!(
            entry.get("RotationEnabled"),
            Some(&json!(false)),
            "case {case:?}: expected ListSecrets to report rotation off too, got {entry}"
        );

        // Cancelling again answers exactly the same: nothing about the call
        // depends on rotation having been on.
        let again = ok(
            &router,
            "CancelRotateSecret",
            json!({"SecretId": secret_id}),
            case,
        )
        .await;
        assert_eq!(
            again, cancelled,
            "case {case:?}: expected a repeated cancel to answer the same, got {again} after \
             {cancelled}"
        );
    }
}

/// One call a row makes before its cancel: the operation, the members it carries
/// beside `SecretId`, and whether this service refuses it.
type PriorCall = (&'static str, Value, bool);

/// One row of [`cancel_reports_rotation_off_whatever_was_configured_before`]: the
/// case, the call made before the cancel, and how the cancel names the secret.
type CancelRow = (&'static str, Option<PriorCall>, SecretIdForm);

/// The cancel DECIDES that rotation is off, so from there every projection of the
/// secret reports `RotationEnabled` as the boolean false — whatever rotation state
/// the secret held before, including none at all — while it configures nothing: no
/// rotation function, no schedule and no next due instant appear, and the stored
/// versions are untouched.
///
/// The rows differ along the rotation state the cancel finds (never touched, a
/// `RotateSecret` the service refused for a missing function, one it refused for an
/// unreadable schedule, a write of the secret that configures no rotation) and along
/// the form of the `SecretId` the cancel names.
#[tokio::test]
async fn cancel_reports_rotation_off_whatever_was_configured_before() {
    // (case, the call made after the create and whether it is refused, how the
    // cancel names the secret)
    let rows: &[CancelRow] = &[
        (
            "nothing but the create, cancelled by name",
            None,
            |name, _arn| name.to_string(),
        ),
        (
            "nothing but the create, cancelled by full ARN",
            None,
            |_name, arn| arn.to_string(),
        ),
        (
            "nothing but the create, cancelled by partial ARN",
            None,
            |_name, arn| partial_arn(arn),
        ),
        (
            // "You tried to enable rotation on a secret that doesn't already have a
            // Lambda function ARN configured and you didn't include such an ARN as
            // a parameter in this call" (`InvalidRequestException`, this service's
            // Smithy model): the refused call configures nothing.
            "a RotateSecret refused for naming no rotation function, cancelled by name",
            Some(("RotateSecret", json!({}), true)),
            |name, _arn| name.to_string(),
        ),
        (
            "a RotateSecret refused for an unreadable schedule, cancelled by full ARN",
            Some((
                "RotateSecret",
                json!({
                    "RotationLambdaARN": LAMBDA_ARN,
                    "RotationRules": {"ScheduleExpression": "every 10 days"},
                }),
                true,
            )),
            |_name, arn| arn.to_string(),
        ),
        (
            "a tag written on it, which configures no rotation, cancelled by name",
            Some((
                "TagResource",
                json!({"Tags": [{"Key": "team", "Value": "a"}]}),
                false,
            )),
            |name, _arn| name.to_string(),
        ),
    ];

    let router = emulator();
    // A sibling secret no cancel ever reaches, described at the end: the member is
    // reported only because a call decided it, so it stays absent here.
    create_secret(&router, "untouched", "a-plain-value", "the control").await;

    for (index, (case, before, secret_id_of)) in rows.iter().enumerate() {
        let name = format!("never-rotated-{index}");
        let (arn, current) = create_secret(&router, &name, "a-plain-value", case).await;
        if let Some((operation, members, refused)) = before {
            let mut body = json!({"SecretId": &name});
            for (member, value) in members.as_object().expect("the row states an object") {
                body[member] = value.clone();
            }
            if *refused {
                err(&router, operation, body, case).await;
            } else {
                ok(&router, operation, body, case).await;
            }
        }
        let described = describe(&router, &name, case).await;
        assert_eq!(
            described.get("RotationEnabled"),
            None,
            "case {case:?}: expected no RotationEnabled before any call decided it, got \
             {described}"
        );

        let secret_id = secret_id_of(&name, &arn);
        let cancelled = ok(
            &router,
            "CancelRotateSecret",
            json!({"SecretId": &secret_id}),
            case,
        )
        .await;
        assert_eq!(
            text(&cancelled, "ARN", case),
            arn,
            "case {case:?}: expected the secret's ARN {arn}, got {cancelled}"
        );
        assert_eq!(
            text(&cancelled, "Name", case),
            name,
            "case {case:?}: expected the secret's name {name}, got {cancelled}"
        );
        assert_eq!(
            cancelled.get("VersionId"),
            None,
            "case {case:?}: expected no VersionId when no rotation was in progress, got {cancelled}"
        );

        // Repeated reads answer the same false: the flag is stored, not invented by
        // the read, and the cancel configured no rotation to go with it.
        for pass in 1..=2 {
            let described = describe(&router, &name, case).await;
            assert_eq!(
                described.get("RotationEnabled"),
                Some(&json!(false)),
                "case {case:?} (describe pass {pass}): expected RotationEnabled false once the \
                 cancel turned rotation off, got {described}"
            );
            for member in ["RotationLambdaARN", "RotationRules", "NextRotationDate"] {
                assert_eq!(
                    described.get(member),
                    None,
                    "case {case:?} (describe pass {pass}): expected the cancel to configure no \
                     {member}, got {described}"
                );
            }
            assert_eq!(
                stages_of(&described, "VersionIdsToStages", &current),
                vec!["AWSCURRENT".to_string()],
                "case {case:?}: expected the stored version to be untouched, got {described}"
            );
        }

        // The list projection of the same secret reports the same flag.
        let entry = list_entry(&router, &name, case).await;
        assert_eq!(
            entry.get("RotationEnabled"),
            Some(&json!(false)),
            "case {case:?}: expected ListSecrets to report rotation off too, got {entry}"
        );

        // Cancelling again decides the same thing.
        ok(
            &router,
            "CancelRotateSecret",
            json!({"SecretId": &secret_id}),
            case,
        )
        .await;
        let again = describe(&router, &name, case).await;
        assert_eq!(
            again.get("RotationEnabled"),
            Some(&json!(false)),
            "case {case:?}: expected a repeated cancel to leave rotation off, got {again}"
        );
    }

    let control = describe(&router, "untouched", "the control").await;
    assert_eq!(
        control.get("RotationEnabled"),
        None,
        "case \"the control\": expected a secret no rotation call ever reached to report no \
         RotationEnabled at all, got {control}"
    );
}

/// Because the cancel keeps the rotation configuration, `RotateSecret` turns
/// rotation back on from the values already stored, naming none of them.
///
/// The rows differ along how the kept schedule is expressed.
#[tokio::test]
async fn rotation_restarts_from_the_kept_configuration() {
    // (case, the RotationRules the first RotateSecret stored)
    let rows: &[(&str, Value)] = &[
        ("a days schedule", json!({"AutomaticallyAfterDays": 14})),
        (
            "a rate in hours",
            json!({"ScheduleExpression": "rate(6 hours)"}),
        ),
        (
            "a rate in days at the documented thousand-day maximum",
            json!({"ScheduleExpression": "rate(1000 days)"}),
        ),
    ];

    let router = emulator();
    for (index, (case, rules)) in rows.iter().enumerate() {
        let name = format!("restarted-{index}");
        create_secret(&router, &name, "the-current-value", case).await;

        // `RotateImmediately` false so nothing is staged and the restart below
        // is not refused for a rotation still in progress.
        ok(
            &router,
            "RotateSecret",
            json!({
                "SecretId": name,
                "RotationLambdaARN": LAMBDA_ARN,
                "RotationRules": rules,
                "RotateImmediately": false,
            }),
            case,
        )
        .await;
        ok(
            &router,
            "CancelRotateSecret",
            json!({"SecretId": name}),
            case,
        )
        .await;

        // The restart names no rotation function and no rules at all.
        ok(
            &router,
            "RotateSecret",
            json!({"SecretId": name, "RotateImmediately": false}),
            case,
        )
        .await;
        let described = describe(&router, &name, case).await;
        assert_eq!(
            described.get("RotationEnabled"),
            Some(&json!(true)),
            "case {case:?}: expected rotation back on after a bare RotateSecret, got {described}"
        );
        assert_eq!(
            described.get("RotationRules"),
            Some(rules),
            "case {case:?}: expected the kept rules to be rotated with, got {described}"
        );
        assert!(
            described.get("NextRotationDate").is_some(),
            "case {case:?}: expected the kept schedule to make a rotation due again, got \
             {described}"
        );
    }
}

/// `CancelRotateSecret` refuses by the state of what the `SecretId` resolves to:
/// a secret this service does not hold is `ResourceNotFoundException`, and one
/// inside its recovery window — which still exists — is
/// `InvalidRequestException`.
///
/// The rows differ along the form of the id and why it is refused.
#[tokio::test]
async fn cancel_refuses_an_unknown_or_deleted_secret() {
    let router = emulator();
    let (arn, _current) = create_secret(&router, "to-be-deleted", "a-value", "setup").await;
    // A recovery window, so the secret stays marked for deletion rather than
    // being purged while the table runs.
    ok(
        &router,
        "DeleteSecret",
        json!({"SecretId": "to-be-deleted", "RecoveryWindowInDays": 30}),
        "setup",
    )
    .await;
    let (live_arn, _) = create_secret(&router, "still-live", "a-value", "setup").await;

    // (case, the SecretId the cancel names, the expected code and message)
    let rows: &[(&str, String, &str, &str)] = &[
        (
            "a name no secret has",
            "never-created".to_string(),
            "ResourceNotFoundException",
            SECRET_NOT_FOUND,
        ),
        (
            "a full ARN no secret has",
            format!("{live_arn}x"),
            "ResourceNotFoundException",
            SECRET_NOT_FOUND,
        ),
        (
            "a secret inside its recovery window, by name",
            "to-be-deleted".to_string(),
            "InvalidRequestException",
            MARKED_FOR_DELETION,
        ),
        (
            "a secret inside its recovery window, by full ARN",
            arn.clone(),
            "InvalidRequestException",
            MARKED_FOR_DELETION,
        ),
        (
            "a secret inside its recovery window, by partial ARN",
            partial_arn(&arn),
            "InvalidRequestException",
            MARKED_FOR_DELETION,
        ),
    ];

    for (case, secret_id, expected_code, expected_message) in rows {
        let (code, message) = err(
            &router,
            "CancelRotateSecret",
            json!({"SecretId": secret_id}),
            case,
        )
        .await;
        assert_eq!(
            &code, expected_code,
            "case {case:?}: expected {expected_code}, got {code:?} with message {message:?}"
        );
        assert_eq!(
            &message, expected_message,
            "case {case:?}: expected the message {expected_message:?}, got {message:?}"
        );
    }

    // The live secret is untouched by any of those refusals: its own cancel
    // still succeeds.
    let cancelled = ok(
        &router,
        "CancelRotateSecret",
        json!({"SecretId": "still-live"}),
        "the live secret",
    )
    .await;
    assert_eq!(
        text(&cancelled, "ARN", "the live secret"),
        live_arn,
        "expected the live secret's own cancel to still answer, got {cancelled}"
    );
}

/// The staging label a rotation stakes on the version it is creating, and the one the
/// secret's current value carries (`DescribeSecretResponse$VersionIdsToStages`, this
/// service's Smithy model).
const PENDING_STAGE: &str = "AWSPENDING";
const CURRENT_STAGE: &str = "AWSCURRENT";

/// Every version `member` reports `stage` on, in the order the map lists them.
fn versions_with_stage(parsed: &Value, member: &str, stage: &str) -> Vec<String> {
    parsed
        .get(member)
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .filter(|(_, stages)| {
                    stages
                        .as_array()
                        .is_some_and(|stages| stages.iter().any(|held| held == stage))
                })
                .map(|(version_id, _)| version_id.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Every version id `member` carries at all, whatever labels it holds.
fn version_ids(parsed: &Value, member: &str) -> Vec<String> {
    parsed
        .get(member)
        .and_then(Value::as_object)
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default()
}

/// How the `RotateSecret` that follows a cancel names the version it asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AsksFor {
    /// A `ClientRequestToken` the secret has no version under, which is what a client
    /// that knows nothing of the cancelled candidate generates.
    AFreshVersion,
    /// No token at all, so the service would otherwise mint an id of its own.
    WhateverTheServiceMints,
    /// The cancelled candidate's own id, the token a client retrying that rotation
    /// carries.
    TheCancelledCandidate,
}

/// One row of [`the_cancelled_candidate_is_what_the_next_rotation_resumes`]: how the
/// schedule is expressed, what the resuming call asks for, and whether that call asks to
/// rotate now.
type ResumeRow = (&'static str, Value, AsksFor, bool);

/// A rotation this service STARTED and `CancelRotateSecret` cancelled leaves its
/// candidate staked `AWSPENDING`, and the next `RotateSecret` resumes THAT rotation: it
/// answers with the candidate's id, stakes no version for the token it carried, and is
/// not refused for the outstanding candidate.
///
/// Whether the candidate is still the one to resume afterwards follows from what the
/// resuming call did with it: a call that started the rotation again is driving it, so a
/// further rotation beside it earns the documented in-progress refusal, while a call that
/// asked not to rotate now started nothing, so the candidate is still outstanding and is
/// resumed again.
///
/// The rows differ along how the schedule is expressed, what the resuming call asks for
/// (a fresh token, no token at all, the candidate's own id) and whether it asks to rotate
/// immediately.
#[tokio::test]
async fn the_cancelled_candidate_is_what_the_next_rotation_resumes() {
    let rows: &[ResumeRow] = &[
        (
            "a days schedule, resumed by a call carrying a fresh token and rotating now",
            json!({"AutomaticallyAfterDays": 30}),
            AsksFor::AFreshVersion,
            true,
        ),
        (
            "a rate in days, resumed by a call carrying no token and rotating now",
            json!({"ScheduleExpression": "rate(10 days)"}),
            AsksFor::WhateverTheServiceMints,
            true,
        ),
        (
            "a rate in hours at the documented four-hour floor, resumed by a call naming the \
             candidate itself and rotating now",
            json!({"ScheduleExpression": "rate(4 hours)"}),
            AsksFor::TheCancelledCandidate,
            true,
        ),
        (
            "a cron schedule, resumed by a call carrying a fresh token and asking not to rotate \
             now",
            json!({"ScheduleExpression": "cron(0 8 1 * ? *)"}),
            AsksFor::AFreshVersion,
            false,
        ),
        (
            "a days schedule at the documented minimum, resumed by a call carrying no token and \
             asking not to rotate now",
            json!({"AutomaticallyAfterDays": 1}),
            AsksFor::WhateverTheServiceMints,
            false,
        ),
    ];

    for (index, (case, rules, asks_for, rotate_now)) in rows.iter().enumerate() {
        let router = emulator();
        let name = format!("resumed-{index}");
        let value = format!("the-current-value-{index}");
        let (_arn, current) = create_secret(&router, &name, &value, case).await;

        // A rotation is STARTED, so a candidate is staked for the version its rotation
        // function is to create.
        let started = ok(
            &router,
            "RotateSecret",
            json!({
                "SecretId": name,
                "RotationLambdaARN": LAMBDA_ARN,
                "RotationRules": rules,
                "RotateImmediately": true,
            }),
            case,
        )
        .await;
        let candidate = text(&started, "VersionId", case);
        let described = describe(&router, &name, case).await;
        assert_eq!(
            versions_with_stage(&described, "VersionIdsToStages", PENDING_STAGE),
            vec![candidate.clone()],
            "case {case:?}: expected the started rotation to stake {candidate} as \
             {PENDING_STAGE}, got {described}"
        );

        // The cancel ends the rotation and the schedule, and leaves the candidate where
        // that rotation left it.
        let cancelled = ok(
            &router,
            "CancelRotateSecret",
            json!({"SecretId": name}),
            case,
        )
        .await;
        assert_eq!(
            cancelled.get("VersionId"),
            Some(&json!(candidate)),
            "case {case:?}: expected the cancel to report the candidate {candidate:?}, got \
             {cancelled}"
        );
        let described = describe(&router, &name, case).await;
        assert_eq!(
            described.get("RotationEnabled"),
            Some(&json!(false)),
            "case {case:?}: expected RotationEnabled false after the cancel, got {described}"
        );
        assert_eq!(
            described.get("NextRotationDate"),
            None,
            "case {case:?}: expected no NextRotationDate once rotation is off, got {described}"
        );
        assert_eq!(
            versions_with_stage(&described, "VersionIdsToStages", PENDING_STAGE),
            vec![candidate.clone()],
            "case {case:?}: expected the cancelled rotation's candidate to stay \
             {PENDING_STAGE}, got {described}"
        );

        // The next rotation takes that candidate up, whatever version it asked for.
        let mut request = json!({"SecretId": name, "RotateImmediately": rotate_now});
        let asked_for = match asks_for {
            AsksFor::AFreshVersion => Some(crate::secret_client_request_token::fresh_token()),
            AsksFor::WhateverTheServiceMints => None,
            AsksFor::TheCancelledCandidate => Some(candidate.clone()),
        };
        if let Some(asked_for) = asked_for.as_ref() {
            request["ClientRequestToken"] = json!(asked_for);
        }
        let resumed = ok(&router, "RotateSecret", request, case).await;
        assert_eq!(
            text(&resumed, "VersionId", case),
            candidate,
            "case {case:?}: expected the resumed rotation to report the cancelled candidate \
             {candidate:?}, got {resumed}"
        );

        let described = describe(&router, &name, case).await;
        assert_eq!(
            versions_with_stage(&described, "VersionIdsToStages", PENDING_STAGE),
            vec![candidate.clone()],
            "case {case:?}: expected {PENDING_STAGE} still on {candidate} alone, got {described}"
        );
        assert_eq!(
            versions_with_stage(&described, "VersionIdsToStages", CURRENT_STAGE),
            vec![current.clone()],
            "case {case:?}: expected the current version {current} left alone, got {described}"
        );
        if let Some(asked_for) = asked_for.as_ref().filter(|asked| **asked != candidate) {
            assert!(
                !version_ids(&described, "VersionIdsToStages").contains(asked_for),
                "case {case:?}: expected the resumed rotation to mint nothing for \
                 {asked_for:?}, got {described}"
            );
        }
        assert_eq!(
            described.get("RotationEnabled"),
            Some(&json!(true)),
            "case {case:?}: expected the resuming call to turn rotation back on, got {described}"
        );
        assert!(
            described.get("NextRotationDate").is_some(),
            "case {case:?}: expected the schedule to make a rotation due again, got {described}"
        );
        assert_eq!(
            described.get("LastRotatedDate"),
            None,
            "case {case:?}: expected no LastRotatedDate while no rotation has completed, got \
             {described}"
        );

        // What a FURTHER rotation beside the candidate earns: the in-progress refusal once
        // the candidate is being driven again, and the candidate itself while the resuming
        // call started no rotation of it.
        let further = crate::secret_client_request_token::fresh_token();
        let further_request = json!({"SecretId": name, "ClientRequestToken": further});
        if *rotate_now {
            let (code, message) = err(&router, "RotateSecret", further_request, case).await;
            assert_eq!(
                code, "InvalidRequestException",
                "case {case:?}: expected InvalidRequestException for a further rotation, got \
                 {code:?} with {message:?}"
            );
            assert_eq!(
                message, ROTATION_IN_PROGRESS_MESSAGE,
                "case {case:?}: expected {ROTATION_IN_PROGRESS_MESSAGE:?}, got {message:?}"
            );
        } else {
            let again = ok(&router, "RotateSecret", further_request, case).await;
            assert_eq!(
                text(&again, "VersionId", case),
                candidate,
                "case {case:?}: expected the still-outstanding candidate {candidate:?} to be \
                 resumed again, got {again}"
            );
        }
        let described = describe(&router, &name, case).await;
        assert!(
            !version_ids(&described, "VersionIdsToStages").contains(&further),
            "case {case:?}: expected the further rotation to mint nothing for {further:?}, got \
             {described}"
        );
    }
}

/// A rotation that was CONFIGURED but never started stakes no candidate, so there is
/// nothing for its cancel to report and nothing for the rotation after it to take up: that
/// rotation is one of its own, on the version the call names.
///
/// The rows differ along how the schedule is expressed and along the form of the
/// `SecretId` the cancel names.
#[tokio::test]
async fn a_rotation_that_never_started_leaves_no_candidate_to_resume() {
    // (case, the RotationRules the deferred rotation configured, how the cancel names the
    // secret)
    let rows: &[(&str, Value, SecretIdForm)] = &[
        (
            "a days schedule, cancelled by name",
            json!({"AutomaticallyAfterDays": 30}),
            |name, _arn| name.to_string(),
        ),
        (
            "a rate in days, cancelled by full ARN",
            json!({"ScheduleExpression": "rate(10 days)"}),
            |_name, arn| arn.to_string(),
        ),
        (
            "a cron schedule, cancelled by partial ARN",
            json!({"ScheduleExpression": "cron(0 8 1 * ? *)"}),
            |_name, arn| partial_arn(arn),
        ),
    ];

    for (index, (case, rules, secret_id_of)) in rows.iter().enumerate() {
        let router = emulator();
        let name = format!("unfired-{index}");
        let (arn, current) = create_secret(&router, &name, "the-current-value", case).await;

        // "The default for RotateImmediately is true. If you don't specify this value,
        // Secrets Manager rotates the secret immediately"
        // (`RotateSecretRequest$RotateImmediately`, this service's Smithy model): a call
        // that asks not to rotate now configures the schedule and starts no rotation.
        ok(
            &router,
            "RotateSecret",
            json!({
                "SecretId": name,
                "RotationLambdaARN": LAMBDA_ARN,
                "RotationRules": rules,
                "RotateImmediately": false,
            }),
            case,
        )
        .await;
        let described = describe(&router, &name, case).await;
        assert_eq!(
            described.get("RotationEnabled"),
            Some(&json!(true)),
            "case {case:?}: expected rotation on once the schedule is configured, got {described}"
        );
        assert!(
            described.get("NextRotationDate").is_some(),
            "case {case:?}: expected the schedule to make a rotation due, got {described}"
        );
        assert_eq!(
            versions_with_stage(&described, "VersionIdsToStages", PENDING_STAGE),
            Vec::<String>::new(),
            "case {case:?}: expected no {PENDING_STAGE} candidate for a rotation that never \
             started, got {described}"
        );

        let secret_id = secret_id_of(&name, &arn);
        let cancelled = ok(
            &router,
            "CancelRotateSecret",
            json!({"SecretId": secret_id}),
            case,
        )
        .await;
        assert_eq!(
            cancelled.get("VersionId"),
            None,
            "case {case:?}: expected no VersionId where no rotation was in progress, got \
             {cancelled}"
        );
        let described = describe(&router, &secret_id, case).await;
        assert_eq!(
            described.get("RotationEnabled"),
            Some(&json!(false)),
            "case {case:?}: expected RotationEnabled false after the cancel, got {described}"
        );
        assert_eq!(
            described.get("NextRotationDate"),
            None,
            "case {case:?}: expected the cancel to clear the next due instant, got {described}"
        );
        assert_eq!(
            versions_with_stage(&described, "VersionIdsToStages", PENDING_STAGE),
            Vec::<String>::new(),
            "case {case:?}: expected the cancel to stake nothing, got {described}"
        );

        // With no candidate outstanding, the rotation after the cancel is a rotation of
        // its own: it works on the version its own token names.
        let token = crate::secret_client_request_token::fresh_token();
        let started = ok(
            &router,
            "RotateSecret",
            json!({"SecretId": secret_id, "ClientRequestToken": token}),
            case,
        )
        .await;
        assert_eq!(
            text(&started, "VersionId", case),
            token,
            "case {case:?}: expected the rotation to work on the version it named {token:?}, \
             got {started}"
        );
        let described = describe(&router, &secret_id, case).await;
        assert_eq!(
            versions_with_stage(&described, "VersionIdsToStages", PENDING_STAGE),
            vec![token.clone()],
            "case {case:?}: expected {PENDING_STAGE} staked on {token}, got {described}"
        );
        assert_eq!(
            versions_with_stage(&described, "VersionIdsToStages", CURRENT_STAGE),
            vec![current.clone()],
            "case {case:?}: expected the current version {current} left alone, got {described}"
        );
    }
}
