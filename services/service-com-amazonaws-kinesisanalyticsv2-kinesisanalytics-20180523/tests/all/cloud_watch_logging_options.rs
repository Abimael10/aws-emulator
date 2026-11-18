// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the CloudWatch log streams an application is monitored through:
//! `AddApplicationCloudWatchLoggingOption` and
//! `DeleteApplicationCloudWatchLoggingOption`.
//!
//! The rule both operations implement: a change to an application's CloudWatch logging
//! options is accepted only when it is addressed to the version in force — by either
//! `CurrentApplicationVersionId` or `ConditionalToken` — and when accepted it attaches or
//! detaches exactly the one log stream the request names, assigns an added stream a
//! server-minted `CloudWatchLoggingOptionId`, moves the application on to a NEW version,
//! and answers the application's ARN, that new version, the descriptions in force after
//! the change and the `OperationId` of a record this store holds. A refused change writes
//! nothing: neither the descriptions nor the version move.
//!
//! The tables below span what that rule generalises over — the runtime family the
//! application runs on, which of the two members addresses the version, how many log
//! streams are attached, which of them is detached, and the status the application is in —
//! so no row is the recorded case and every row is asserted against the same rule.
//!
//! Every test drives the service the way a client does: one `POST /` per call through the
//! real `MultiServiceRouter`, with `X-Amz-Target` naming the operation.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::harness::{
    application_arn, await_status, call, create, create_startable, describe, error_code,
    error_message, router,
};

/// Which of the two members a change addresses the version in force by.
///
/// "You must provide the CurrentApplicationVersionId or the ConditionalToken" — both
/// members' docs — so the two are alternatives and every property below is asserted over
/// each of them.
#[derive(Debug, Clone, Copy)]
enum Address {
    VersionId,
    Token,
}

/// One row of the table: an application and the log streams a case attaches to it.
struct Case {
    /// The case's name, and the `ApplicationName` it creates.
    name: &'static str,
    /// A value of the `RuntimeEnvironment` enum
    /// (`smithy_query(query="shape",shape="RuntimeEnvironment")`); the rows span the two
    /// families this Region serves — the `FLINK-` applications and the `ZEPPELIN-FLINK-`
    /// Studio notebooks — across several versions of each.
    runtime: &'static str,
    /// Which member this case's changes address the version in force by.
    address: Address,
    /// The `LogStreamARN`s this case attaches, in order.
    streams: Vec<String>,
}

/// The applications and log streams every property below is asserted over.
fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "logs-flink-one",
            runtime: "FLINK-1_20",
            address: Address::VersionId,
            streams: vec![stream("logs-flink-one", "errors")],
        },
        Case {
            name: "logs-studio-two",
            runtime: "ZEPPELIN-FLINK-3_0",
            // A Studio notebook's version is addressed by its ID alone, which
            // `a_notebook_change_that_names_no_version_id_is_refused` asserts.
            address: Address::VersionId,
            streams: vec![
                stream("logs-studio-two", "errors"),
                stream("logs-studio-two", "operator-metrics"),
            ],
        },
        Case {
            name: "logs.notebook-three",
            runtime: "ZEPPELIN-FLINK-3_0",
            address: Address::VersionId,
            streams: vec![
                stream("logs.notebook-three", "a"),
                stream("logs.notebook-three", "b"),
                stream("logs.notebook-three", "c"),
            ],
        },
        Case {
            name: "logs_flink_1_8",
            runtime: "FLINK-1_8",
            address: Address::Token,
            // A long stream name, to span `LogStreamARN`'s 1..2048 length rather than only
            // the short values a fixture happens to use.
            streams: vec![stream("logs_flink_1_8", &"s".repeat(400))],
        },
    ]
}

/// A CloudWatch Logs log-stream ARN, the only member `CloudWatchLoggingOption` carries
/// (`@required`, 1..2048, `^arn:`).
fn stream(application: &str, suffix: &str) -> String {
    format!(
        "arn:aws:logs:us-east-1:000000000000:log-group:/aws/kinesis-analytics/\
         {application}:log-stream:{suffix}"
    )
}

/// Whether `id` satisfies the modelled `Id` a `CloudWatchLoggingOptionId` is
/// (`@length` 1..50, `@pattern` `^[a-zA-Z0-9_.-]+$`,
/// `smithy_query(query="shape",shape="CloudWatchLoggingOptionDescription")`).
fn is_modeled_id(id: &str) -> bool {
    (1..=50).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

/// The version in force, the `ConditionalToken` in force and the log streams described,
/// read back through `DescribeApplication` — the reader a client uses to pick up the values
/// its next change is addressed by.
async fn stored(router: &MultiServiceRouter, case: &str, name: &str) -> (i64, String, Vec<Value>) {
    let detail = describe(router, name).await;
    let version = detail["ApplicationVersionId"]
        .as_i64()
        .unwrap_or_else(|| panic!("case {case}: expected an ApplicationVersionId, got {detail}"));
    let token = detail["ConditionalToken"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: expected a ConditionalToken, got {detail}"))
        .to_string();
    let described = detail["CloudWatchLoggingOptionDescriptions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    (version, token, described)
}

/// `body` with the member `address` names added, carrying the value in force.
fn addressed(mut body: Value, address: Address, version: i64, token: &str) -> Value {
    match address {
        Address::VersionId => body["CurrentApplicationVersionId"] = json!(version),
        Address::Token => body["ConditionalToken"] = json!(token),
    }
    body
}

/// The descriptions an answer carries, as a list (empty when the member is absent).
fn descriptions_of(answered: &Value) -> Vec<Value> {
    answered["CloudWatchLoggingOptionDescriptions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The `OperationId` an accepted change answers, checked against `OperationId`'s modelled
/// 1..64 length.
fn operation_id_of(answered: &Value, case: &str) -> String {
    let operation_id = answered["OperationId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: expected an OperationId, got {answered}"))
        .to_string();
    assert!(
        (1..=64).contains(&operation_id.len()),
        "case {case}: an OperationId satisfies len 1..64, got {operation_id:?}"
    );
    operation_id
}

/// Attach one log stream, addressed to the version in force, asserting the call succeeded.
async fn add_stream(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    address: Address,
    log_stream_arn: &str,
) -> Value {
    let (version, token, _) = stored(router, case, name).await;
    let body = addressed(
        json!({
            "ApplicationName": name,
            "CloudWatchLoggingOption": {"LogStreamARN": log_stream_arn},
        }),
        address,
        version,
        &token,
    );
    let (status, answered) = call(router, "AddApplicationCloudWatchLoggingOption", &body).await;
    assert_eq!(
        status, 200,
        "case {case}: AddApplicationCloudWatchLoggingOption got {status} {answered}"
    );
    answered
}

/// Detach the log stream `option_id` names, addressed to the version in force, asserting
/// the call succeeded.
async fn delete_stream(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    address: Address,
    option_id: &str,
) -> Value {
    let (version, token, _) = stored(router, case, name).await;
    let body = addressed(
        json!({"ApplicationName": name, "CloudWatchLoggingOptionId": option_id}),
        address,
        version,
        &token,
    );
    let (status, answered) = call(router, "DeleteApplicationCloudWatchLoggingOption", &body).await;
    assert_eq!(
        status, 200,
        "case {case}: DeleteApplicationCloudWatchLoggingOption got {status} {answered}"
    );
    answered
}

/// The `CloudWatchLoggingOptionId` of one description.
fn id_of(description: &Value, case: &str) -> String {
    description["CloudWatchLoggingOptionId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case}: expected a CloudWatchLoggingOptionId, got {description}")
        })
        .to_string()
}

/// Every added log stream is described with an id of its own, is appended to the
/// descriptions already in force, and moves the application on to exactly one new version —
/// and `DescribeApplication` then reports precisely what the add answered.
#[tokio::test]
async fn each_added_log_stream_is_described_with_its_own_id_and_mints_one_version() {
    let router = router();
    // Ids are per-option state, so no two options in one store may share one — across
    // applications as well as within one.
    let mut minted: Vec<String> = Vec::new();
    for case in cases() {
        create(&router, case.name, case.runtime).await;
        for (index, arn) in case.streams.iter().enumerate() {
            let label = format!("{}/stream {index}", case.name);
            let (version_before, token_before, before) = stored(&router, &label, case.name).await;

            let answered = add_stream(&router, &label, case.name, case.address, arn).await;
            assert_eq!(
                answered["ApplicationARN"].as_str(),
                Some(application_arn(case.name).as_str()),
                "case {label}: expected the application's own ARN, got {answered}"
            );
            assert_eq!(
                answered["ApplicationVersionId"].as_i64(),
                Some(version_before + 1),
                "case {label}: a change to the logging options mints the next version, got \
                 {answered}"
            );
            let operation_id = operation_id_of(&answered, &label);

            let described = descriptions_of(&answered);
            assert_eq!(
                described.len(),
                index + 1,
                "case {label}: expected {} description(s), got {answered}",
                index + 1
            );
            assert_eq!(
                described[..index],
                before[..],
                "case {label}: the options already attached stay as they were, got {answered}"
            );
            let attached = &described[index];
            assert_eq!(
                attached["LogStreamARN"].as_str(),
                Some(arn.as_str()),
                "case {label}: expected the log stream the request named, got {attached}"
            );
            assert!(
                attached.get("RoleARN").is_none(),
                "case {label}: an application created with the current API version has an \
                 application-level execution role, so no option carries one, got {attached}"
            );
            let id = id_of(attached, &label);
            assert!(
                is_modeled_id(&id),
                "case {label}: expected an id of the modelled form (1..50 of \
                 [a-zA-Z0-9_.-]), got {id:?}"
            );
            assert!(
                !minted.contains(&id),
                "case {label}: id {id:?} was already handed to another option"
            );
            assert_ne!(
                id, operation_id,
                "case {label}: the option's id and the operation's id are different values"
            );
            minted.push(id);

            let (version_after, token_after, reported) = stored(&router, &label, case.name).await;
            assert_eq!(
                version_after,
                version_before + 1,
                "case {label}: expected the stored version to be the one answered"
            );
            assert_ne!(
                token_after, token_before,
                "case {label}: a new version carries a fresh ConditionalToken"
            );
            assert_eq!(
                reported, described,
                "case {label}: expected describe to report what the add answered"
            );
        }
    }
}

/// Detaching one log stream leaves every other exactly as it was — same ids, same order —
/// mints one new version per call, and an application whose last option is detached reports
/// none at all.
#[tokio::test]
async fn detaching_one_log_stream_leaves_the_others_and_mints_one_version() {
    let router = router();
    for case in cases() {
        create(&router, case.name, case.runtime).await;
        let mut attached: Vec<Value> = Vec::new();
        for arn in &case.streams {
            attached = descriptions_of(
                &add_stream(&router, case.name, case.name, case.address, arn).await,
            );
        }

        let mut round = 0;
        while !attached.is_empty() {
            round += 1;
            let label = format!("{}/round {round}", case.name);
            // Detach from the MIDDLE while there is a middle, so the rule is asserted on a
            // removal that is neither the first nor the last description.
            let position = if attached.len() >= 3 { 1 } else { 0 };
            let removed = id_of(&attached[position], &label);
            let expected: Vec<Value> = attached
                .iter()
                .enumerate()
                .filter(|(at, _)| *at != position)
                .map(|(_, description)| description.clone())
                .collect();

            let (version_before, _, _) = stored(&router, &label, case.name).await;
            let answered =
                delete_stream(&router, &label, case.name, case.address, &removed).await;
            assert_eq!(
                answered["ApplicationARN"].as_str(),
                Some(application_arn(case.name).as_str()),
                "case {label}: expected the application's own ARN, got {answered}"
            );
            assert_eq!(
                answered["ApplicationVersionId"].as_i64(),
                Some(version_before + 1),
                "case {label}: a detach mints the next version too, got {answered}"
            );
            operation_id_of(&answered, &label);
            assert_eq!(
                descriptions_of(&answered),
                expected,
                "case {label}: expected the options other than {removed:?} to remain, got \
                 {answered}"
            );

            let (version_after, _, reported) = stored(&router, &label, case.name).await;
            assert_eq!(
                version_after,
                version_before + 1,
                "case {label}: expected the stored version to be the one answered"
            );
            assert_eq!(
                reported, expected,
                "case {label}: expected describe to report the remaining options"
            );
            attached = expected;
        }

        // The last option gone, the member is absent rather than an empty list: this is how
        // every reader of this service reports an application monitored through no stream.
        let detail = describe(&router, case.name).await;
        assert!(
            detail.get("CloudWatchLoggingOptionDescriptions").is_none(),
            "case {}: an application with no logging options reports no member, got {detail}",
            case.name
        );
    }
}

/// The version a change LEAVES keeps the logging options it ran with: each retained version
/// record reports the descriptions in force at the instant it was superseded, and the
/// version in force reports the live ones.
///
/// Asserted on the `FLINK-` family, the only one `DescribeApplicationVersion` is served for.
#[tokio::test]
async fn every_version_reports_the_log_streams_it_ran_with() {
    let router = router();
    let name = "logs-version-history";
    create(&router, name, "FLINK-1_20").await;

    let first = stream(name, "first");
    let second = stream(name, "second");
    let after_first = descriptions_of(&add_stream(&router, name, name, Address::VersionId, &first).await);
    let after_second =
        descriptions_of(&add_stream(&router, name, name, Address::Token, &second).await);
    let removed = id_of(&after_second[0], name);
    let after_detach =
        descriptions_of(&delete_stream(&router, name, name, Address::VersionId, &removed).await);

    // v1 ran with none, v2 with the first stream, v3 with both, v4 (in force) with what the
    // detach left.
    for (version_id, expected) in [
        (1_i64, Vec::new()),
        (2, after_first),
        (3, after_second),
        (4, after_detach),
    ] {
        let (status, answered) = call(
            &router,
            "DescribeApplicationVersion",
            &json!({"ApplicationName": name, "ApplicationVersionId": version_id}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case v{version_id}: DescribeApplicationVersion got {status} {answered}"
        );
        let detail = &answered["ApplicationVersionDetail"];
        assert_eq!(
            descriptions_of(detail),
            expected,
            "case v{version_id}: expected the options that version ran with, got {detail}"
        );
    }
}

/// A change addressed to anything other than the version in force is refused — the modelled
/// `ConcurrentModificationException` for a consumed address, `InvalidArgumentException` for a
/// request that addresses no version at all, both on this service's client-error status — and
/// writes nothing: the descriptions and the version stay exactly as they were.
#[tokio::test]
async fn a_change_not_addressed_to_the_version_in_force_is_refused_and_writes_nothing() {
    let router = router();
    let name = "logs-concurrency";
    create(&router, name, "FLINK-1_20").await;
    let attached = descriptions_of(&add_stream(&router, name, name, Address::VersionId, &stream(name, "errors")).await);
    let option_id = id_of(&attached[0], name);
    let (version, token, _) = stored(&router, name, name).await;

    // The address rows, each a member/value pair that is NOT the version in force, plus the
    // row that names neither member at all.
    let rows: Vec<(&str, Value, u16, &str, String)> = vec![
        (
            "the version just consumed",
            json!({"CurrentApplicationVersionId": version - 1}),
            400,
            "ConcurrentModificationException",
            "The provided application version ID does not match the most recent version for \
             the application."
                .to_string(),
        ),
        (
            "a version never reached",
            json!({"CurrentApplicationVersionId": version + 1}),
            400,
            "ConcurrentModificationException",
            "The provided application version ID does not match the most recent version for \
             the application."
                .to_string(),
        ),
        (
            "the top of the modelled version range",
            json!({"CurrentApplicationVersionId": 999_999_999_i64}),
            400,
            "ConcurrentModificationException",
            "The provided application version ID does not match the most recent version for \
             the application."
                .to_string(),
        ),
        (
            "a token that is not the stored one",
            json!({"ConditionalToken": format!("{token}xy")}),
            400,
            "ConcurrentModificationException",
            "The provided ConditionalToken does not match the most recent version for the \
             application."
                .to_string(),
        ),
        (
            "neither member",
            json!({}),
            400,
            "InvalidArgumentException",
            "You must provide the CurrentApplicationVersionId or the ConditionalToken."
                .to_string(),
        ),
    ];

    for (row, address, expected_status, expected_code, expected_message) in rows {
        for (operation, request) in [
            (
                "AddApplicationCloudWatchLoggingOption",
                json!({
                    "ApplicationName": name,
                    "CloudWatchLoggingOption": {"LogStreamARN": stream(name, "second")},
                }),
            ),
            (
                "DeleteApplicationCloudWatchLoggingOption",
                json!({"ApplicationName": name, "CloudWatchLoggingOptionId": option_id}),
            ),
        ] {
            let label = format!("{row}/{operation}");
            let mut body = request;
            for (member, value) in address.as_object().expect("the address is an object") {
                body[member] = value.clone();
            }
            let (status, answered) = call(&router, operation, &body).await;
            assert_eq!(
                (status, error_code(&answered).as_str()),
                (expected_status, expected_code),
                "case {label}: expected {expected_status} {expected_code}, got {status} \
                 {answered}"
            );
            assert_eq!(
                error_message(&answered),
                expected_message,
                "case {label}: expected the refusal to name what did not match, got {answered}"
            );

            let (still, same_token, described) = stored(&router, &label, name).await;
            assert_eq!(
                (still, same_token.as_str()),
                (version, token.as_str()),
                "case {label}: a refused change consumes no version and mints no token"
            );
            assert_eq!(
                described, attached,
                "case {label}: a refused change leaves the stored options untouched"
            );
        }
    }
}

/// A change made against a STUDIO NOTEBOOK's version has to name
/// `CurrentApplicationVersionId`: the either-or both members' docs state holds for an
/// application a `ConditionalToken` addresses a version of, and a notebook is not one — so a
/// change that names no version ID is refused naming the runtime the application runs on,
/// whether or not it carried the token in force, and nothing is written.
///
/// The rows span the class the rule covers: both of the operations that share the
/// version-addressing rule plus `UpdateApplication`, the Zeppelin runtime this API creates a
/// notebook on against a streaming runtime, and both ways of naming no version ID (nothing at
/// all, and the token in force). The streaming rows are the control: there the token DOES
/// address the version, and a request naming neither member is refused with the either-or
/// sentence instead.
#[tokio::test]
async fn a_notebook_change_that_names_no_version_id_is_refused() {
    // (runtime, what the refusal names the application's runtime as)
    let applications = [("ZEPPELIN-FLINK-3_0", true), ("FLINK-1_20", false)];
    for (runtime, is_notebook) in applications {
        for address in ["nothing", "the token in force"] {
            for (operation, request) in [
                ("UpdateApplication", "update"),
                ("AddApplicationCloudWatchLoggingOption", "attach"),
                ("DeleteApplicationCloudWatchLoggingOption", "detach"),
            ] {
                // One application per row, so an accepted control row cannot move the
                // version the next row addresses.
                let router = router();
                let name = "logs-version-address";
                create(&router, name, runtime).await;
                let (version, token, _) = stored(&router, name, name).await;

                let label = format!("{runtime}/{address}/{request}");
                let mut body = match operation {
                    "AddApplicationCloudWatchLoggingOption" => json!({
                        "ApplicationName": name,
                        "CloudWatchLoggingOption": {"LogStreamARN": stream(name, "errors")},
                    }),
                    "DeleteApplicationCloudWatchLoggingOption" => json!({
                        "ApplicationName": name,
                        "CloudWatchLoggingOptionId": "0000ffff-1111-2222-3333-444455556666",
                    }),
                    _ => json!({"ApplicationName": name}),
                };
                if address == "the token in force" {
                    body["ConditionalToken"] = json!(token);
                }
                let (status, answered) = call(&router, operation, &body).await;

                // A streaming application addressed by the token in force is ACCEPTED, so
                // only the rows that address no version at all are refused there. The two
                // logging changes can still be refused on the log stream the request names
                // — a detach of an option nothing holds — so what is asserted for them is
                // that the refusal is not about the version the change is made against.
                if !is_notebook && address == "the token in force" {
                    if operation == "UpdateApplication" {
                        assert_eq!(
                            status, 200,
                            "case {label}: expected the token to address the version, got \
                             {status} {answered}"
                        );
                    } else {
                        let message = error_message(&answered);
                        assert!(
                            !message.contains("CurrentApplicationVersionId"),
                            "case {label}: expected the token to address the version, got \
                             {status} {answered}"
                        );
                    }
                    continue;
                }

                let expected_message = if is_notebook {
                    format!(
                        "CurrentApplicationVersionId parameter should be provided for \
                         application with RuntimeEnvironment: {runtime}"
                    )
                } else {
                    "You must provide the CurrentApplicationVersionId or the ConditionalToken."
                        .to_string()
                };
                assert_eq!(
                    (status, error_code(&answered).as_str()),
                    (400, "InvalidArgumentException"),
                    "case {label}: expected 400 InvalidArgumentException, got {status} {answered}"
                );
                assert_eq!(
                    error_message(&answered),
                    expected_message,
                    "case {label}: expected {expected_message:?}, got {answered}"
                );

                // A refused change consumes no version and mints no token.
                let (still, same_token, _) = stored(&router, &label, name).await;
                assert_eq!(
                    (still, same_token.as_str()),
                    (version, token.as_str()),
                    "case {label}: a refused change moves the application nowhere"
                );
            }
        }
    }
}

/// A change naming a resource the store does not hold is refused on what is missing: the
/// application, or the logging option the id names — and an application monitored through no
/// stream at all is refused on that fact rather than on the id.
#[tokio::test]
async fn a_change_naming_a_resource_the_store_does_not_hold_is_refused() {
    let router = router();
    let unmonitored = "logs-none-attached";
    create(&router, unmonitored, "FLINK-1_20").await;
    let monitored = "logs-one-attached";
    create(&router, monitored, "ZEPPELIN-FLINK-3_0").await;
    let attached = descriptions_of(
        &add_stream(
            &router,
            monitored,
            monitored,
            Address::VersionId,
            &stream(monitored, "errors"),
        )
        .await,
    );
    let option_id = id_of(&attached[0], monitored);
    let absent_id = "0000ffff-1111-2222-3333-444455556666";

    let rows: Vec<(&str, &str, Value, &str, String)> = vec![
        (
            "an unknown application, attaching",
            "AddApplicationCloudWatchLoggingOption",
            json!({
                "ApplicationName": "logs-no-such-application",
                "CurrentApplicationVersionId": 1,
                "CloudWatchLoggingOption": {"LogStreamARN": stream("x", "errors")},
            }),
            "ResourceNotFoundException",
            "Application 'logs-no-such-application' does not exist.".to_string(),
        ),
        (
            "an unknown application, detaching",
            "DeleteApplicationCloudWatchLoggingOption",
            json!({
                "ApplicationName": "logs-no-such-application",
                "CurrentApplicationVersionId": 1,
                "CloudWatchLoggingOptionId": absent_id,
            }),
            "ResourceNotFoundException",
            "Application 'logs-no-such-application' does not exist.".to_string(),
        ),
        (
            "an application monitored through no stream",
            "DeleteApplicationCloudWatchLoggingOption",
            json!({
                "ApplicationName": unmonitored,
                "CurrentApplicationVersionId": 1,
                "CloudWatchLoggingOptionId": absent_id,
            }),
            "InvalidArgumentException",
            "There are no CloudWatchLoggingOptions associated with the Application that you \
             would like to Delete."
                .to_string(),
        ),
        (
            "an id this application holds no option under",
            "DeleteApplicationCloudWatchLoggingOption",
            json!({
                "ApplicationName": monitored,
                "CurrentApplicationVersionId": 2,
                "CloudWatchLoggingOptionId": absent_id,
            }),
            "InvalidArgumentException",
            format!(
                "CloudWatch logging option '{absent_id}' does not exist for application \
                 {monitored}."
            ),
        ),
        (
            "an id belonging to another application",
            "DeleteApplicationCloudWatchLoggingOption",
            json!({
                "ApplicationName": unmonitored,
                "CurrentApplicationVersionId": 1,
                "CloudWatchLoggingOptionId": option_id,
            }),
            "InvalidArgumentException",
            "There are no CloudWatchLoggingOptions associated with the Application that you \
             would like to Delete."
                .to_string(),
        ),
    ];

    for (row, operation, body, expected_code, expected_message) in rows {
        let (status, answered) = call(&router, operation, &body).await;
        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, expected_code),
            "case {row}: expected 400 {expected_code}, got {status} {answered}"
        );
        assert_eq!(
            error_message(&answered),
            expected_message,
            "case {row}: expected the refusal to name what is missing, got {answered}"
        );
    }

    // Nothing above was written: the monitored application still carries its one option on
    // the version it was on, and the unmonitored one still carries none.
    let (version, _, described) = stored(&router, "after the refusals", monitored).await;
    assert_eq!(
        (version, described),
        (2, attached),
        "case after the refusals: a refused change leaves the stored options untouched"
    );
    let detail = describe(&router, unmonitored).await;
    assert!(
        detail.get("CloudWatchLoggingOptionDescriptions").is_none(),
        "case after the refusals: expected no options on {unmonitored}, got {detail}"
    );
}

/// The ceiling on how many log streams one application carries — the emulator's own bound on
/// growth a caller drives, since the model publishes none — is enforced at the boundary of the
/// set: the application accepts options up to it, and the next one is refused as an invalid
/// argument with nothing written.
///
/// The number is the one `MAX_LOGGING_OPTIONS` in
/// `src/ops/add_application_cloud_watch_logging_option.rs` states; the rows are the entry one
/// below it, the entry at it, and the one past it.
#[tokio::test]
async fn attaching_past_the_ceiling_is_refused_and_writes_nothing() {
    const MAX_LOGGING_OPTIONS: usize = 50;

    let router = router();
    let name = "logs-at-the-ceiling";
    create(&router, name, "FLINK-1_20").await;

    let mut attached = Vec::new();
    for index in 1..=MAX_LOGGING_OPTIONS {
        let label = format!("fill {index}");
        attached = descriptions_of(
            &add_stream(
                &router,
                &label,
                name,
                Address::VersionId,
                &stream(name, &format!("s{index}")),
            )
            .await,
        );
        assert_eq!(
            attached.len(),
            index,
            "case {label}: expected {index} option(s) attached"
        );
    }

    let (version, token, _) = stored(&router, "at the ceiling", name).await;
    let (status, refused) = call(
        &router,
        "AddApplicationCloudWatchLoggingOption",
        &json!({
            "ApplicationName": name,
            "CurrentApplicationVersionId": version,
            "CloudWatchLoggingOption": {"LogStreamARN": stream(name, "one-too-many")},
        }),
    )
    .await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (400, "InvalidArgumentException"),
        "case past the ceiling: expected 400 InvalidArgumentException, got {status} {refused}"
    );
    assert_eq!(
        error_message(&refused),
        format!(
            "Application {name} already has {MAX_LOGGING_OPTIONS} CloudWatch logging options, \
             which is the maximum."
        ),
        "case past the ceiling: expected the refusal to name the application and the \
         maximum, got {refused}"
    );

    let (still, same_token, described) = stored(&router, "past the ceiling", name).await;
    assert_eq!(
        (still, same_token.as_str()),
        (version, token.as_str()),
        "case past the ceiling: a refused add consumes no version and mints no token"
    );
    assert_eq!(
        described, attached,
        "case past the ceiling: a refused add leaves the stored options untouched"
    );
}

/// A change to a RUNNING application's logging options is accepted and the application
/// reports `UPDATING` until the clock settles it back to `RUNNING`; one caught in a
/// transient status is refused as in use, on the sentence every operation of this service
/// refuses an action on a status with.
#[tokio::test]
async fn a_change_on_a_running_application_settles_through_updating() {
    let router = router();
    let name = "logs-running-app";
    create_startable(&router, name, "FLINK-1_20").await;

    let (status, started) = call(&router, "StartApplication", &json!({"ApplicationName": name})).await;
    assert_eq!(status, 200, "case start: StartApplication got {status} {started}");

    // STARTING is a status no configuration change is accepted from.
    let (version, _, _) = stored(&router, "while starting", name).await;
    let (status, refused) = call(
        &router,
        "AddApplicationCloudWatchLoggingOption",
        &json!({
            "ApplicationName": name,
            "CurrentApplicationVersionId": version,
            "CloudWatchLoggingOption": {"LogStreamARN": stream(name, "errors")},
        }),
    )
    .await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (400, "ResourceInUseException"),
        "case while starting: expected 400 ResourceInUseException, got {status} {refused}"
    );
    assert_eq!(
        error_message(&refused),
        "Application cannot be Updated in 'STARTING' state",
        "case while starting: expected the refusal to name the status, got {refused}"
    );

    await_status(&router, "started", name, "RUNNING").await;
    let answered = add_stream(
        &router,
        "while running",
        name,
        Address::VersionId,
        &stream(name, "errors"),
    )
    .await;
    let described = descriptions_of(&answered);
    assert_eq!(
        described.len(),
        1,
        "case while running: expected the attached option, got {answered}"
    );
    assert_eq!(
        describe(&router, name).await["ApplicationStatus"].as_str(),
        Some("UPDATING"),
        "case while running: a running application restarts its job, so a caller reading \
         straight back still sees the transient status"
    );

    // The clock owes RUNNING again, and the option the change attached is reported either
    // way: the status settles, the configuration does not change back.
    await_status(&router, "settled", name, "RUNNING").await;
    let (_, _, reported) = stored(&router, "settled", name).await;
    assert_eq!(
        reported, described,
        "case settled: expected the attached option to survive the settle"
    );
}
