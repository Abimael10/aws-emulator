// SPDX-License-Identifier: CC0-1.0

//! The modelled CLIENT errors of this service: every refusal here is a 4xx a client can
//! branch on, carrying the error code the model declares and a `Message` member.
//!
//! Every test drives the service the way a client does — one `POST /` per call through the
//! real `MultiServiceRouter`, with `X-Amz-Target: KinesisAnalytics_20180523.<Operation>` —
//! so what is asserted is the wire envelope: the HTTP status, the `__type` error code and
//! the message. One engine per test, so a refusal that must store nothing is checked
//! against a store only that test wrote to.
//!
//! The rules under test, each asserted over a SET of inputs rather than over one recorded
//! request:
//!
//! * a value outside a modelled enumerated type is refused by the boundary validator with
//!   HTTP 400 `ValidationException`, naming the member and its value set;
//! * `CreateApplication` provisions only the runtime GENERATIONS this API still creates
//!   applications on: it refuses the retired SQL runtime family with
//!   `UnsupportedOperationException`, and a withdrawn generation of the Studio-notebook
//!   family with `InvalidArgumentException` and the deprecation sentence, whatever the
//!   request names around it — as does an update that would move an application onto one;
//! * an operation that looks an application up by name reports a miss with the error code
//!   its own model declares for that path — `ResourceNotFoundException` for the readers,
//!   `InvalidArgumentException` for `CreateApplicationSnapshot` — and never as a 5xx;
//! * `StartApplication` refuses an application whose stored configuration carries no code
//!   content with `InvalidApplicationConfigurationException`, and leaves it unstarted;
//! * a `ParallelismPerKPU` past the value the service runs an application at is refused with
//!   `InvalidArgumentException`, on the create and on the update alike;
//! * a Studio notebook's inline code is a Zeppelin note, so a create or an update that
//!   writes anything else as the code of an application running on a notebook runtime is
//!   refused with `InvalidArgumentException` and writes nothing;
//! * taking a snapshot and rolling back are features of a Managed Service for Apache Flink
//!   application's job, so on another runtime family each is refused with
//!   `UnsupportedOperationException` in its own wording;
//! * every modelled client error travels with this service's client-error status, whatever
//!   `@httpError` its shape carries.

use serde_json::{json, Value};

use service_com_amazonaws_kinesisanalyticsv2_kinesisanalytics_20180523::ops::types::RUNTIME_ENVIRONMENT_VALUES;

// The wire plumbing lives in `tests/all/harness.rs`, shared with the lifecycle suite, so
// both read one response envelope. The runtime table is plumbing too: every suite that picks
// the runtime of a case reads from it which runtimes this API creates an application on.
use super::harness::{
    call, create_body, current_notebook_runtime, error_code, error_message, notebook_runtimes,
    router, runtimes_with, RuntimeOutcome, ACCOUNT, RUNTIME_OUTCOMES, SETTLE_WAIT,
};
use super::stated_enum_value_set::NAMED_RUNTIME_ENVIRONMENT_VALUES;

// ---------------------------------------------------------------------------
// Boundary validation: a value outside a modelled enum
// ---------------------------------------------------------------------------

/// A `RuntimeEnvironment` outside the modelled value set is refused with HTTP 400
/// `ValidationException`, in the envelope AWS's request validator uses: the offending
/// member, the value the request sent, and the constraint it failed.
///
/// The rows span the forms an out-of-set value takes — an unreleased version of a modelled
/// family, a modelled value in the wrong case, a value of no family at all, and the empty
/// string — because the rule is about enum MEMBERSHIP and not about one recorded value.
#[tokio::test]
async fn a_runtime_environment_outside_the_enum_is_refused_as_a_validation_exception() {
    let router = router();
    for value in [
        "FLINK-99_9",
        "SQL-9_9",
        "flink-1_20",
        "ZEPPELIN-FLINK-9_0",
        "NOT-A-RUNTIME",
        "",
    ] {
        let (status, body) = call(&router, "CreateApplication", &create_body("probe", value)).await;
        assert_eq!(
            status, 400,
            "case {value:?}: expected HTTP 400 for a value outside the enum, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {value:?}: expected ValidationException, got {body}"
        );
        let message = error_message(&body);
        // The envelope, the member path and the constraint are the requirement; what is
        // asserted here is WHICH values the set names. The ORDER it is stated in is its own
        // rule, asserted over several enums in `tests/all/error_envelope_wording.rs`.
        let expected_prefix = format!(
            "1 validation error detected: Value '{value}' at 'runtimeEnvironment' failed to \
             satisfy constraint: Member must satisfy enum value set: ["
        );
        assert!(
            message.starts_with(&expected_prefix) && message.ends_with(']'),
            "case {value:?}: expected a refusal naming the member, the value and the enum \
             constraint ({expected_prefix}…]), got {message:?}"
        );
        // The set the refusal names is the set the SERVICE names for the shape, which is
        // not every value the model declares — `tests/all/stated_enum_value_set.rs` owns
        // that rule and the table behind it.
        for named in NAMED_RUNTIME_ENVIRONMENT_VALUES {
            assert!(
                message.contains(named),
                "case {value:?}: expected the refusal to list the named value \
                 {named:?}, got {message:?}"
            );
        }
        for declared in RUNTIME_ENVIRONMENT_VALUES
            .iter()
            .filter(|declared| !NAMED_RUNTIME_ENVIRONMENT_VALUES.contains(declared))
        {
            assert!(
                !message.contains(declared),
                "case {value:?}: expected the refusal not to list {declared:?}, which the \
                 service does not name for this shape, got {message:?}"
            );
        }
    }

    // Nothing was stored by any of them.
    let (status, listed) = call(&router, "ListApplications", &json!({})).await;
    assert_eq!(status, 200, "ListApplications got {listed}");
    assert_eq!(
        listed["ApplicationSummaries"],
        json!([]),
        "a refused create must store nothing, got {listed}"
    );
}

// ---------------------------------------------------------------------------
// CreateApplication: the runtimes this API still creates
// ---------------------------------------------------------------------------

/// Whether a runtime names a SQL application: the `SQL-` family of the modelled
/// `RuntimeEnvironment` enum, whichever version it carries.
fn is_sql(runtime: &str) -> bool {
    runtime.starts_with("SQL-")
}

/// Every runtime [`RUNTIME_OUTCOMES`] marks withdrawn.
fn deprecated_runtimes() -> Vec<&'static str> {
    runtimes_with(RuntimeOutcome::Deprecated)
}

/// How the service words a request naming a withdrawn runtime generation: fixed text apart
/// from the runtime identifier, which is the `RuntimeEnvironment` the request named.
fn deprecation_message(runtime: &str) -> String {
    format!(
        "Runtime {runtime} is deprecated. Please choose a valid version from: \
         https://docs.aws.amazon.com/managed-flink/latest/apiv2/API_CreateApplication.html\
         #APIReference-CreateApplication-request-RuntimeEnvironment"
    )
}

/// Assert `answered` is the deprecation refusal of `runtime` under HTTP 400.
fn assert_deprecation_refusal(case: &str, runtime: &str, status: u16, answered: &Value) {
    assert_eq!(
        (status, error_code(answered).as_str()),
        (400, "InvalidArgumentException"),
        "case {case}: expected HTTP 400 InvalidArgumentException, got {status} {answered}"
    );
    assert_eq!(
        error_message(answered),
        deprecation_message(runtime),
        "case {case}: expected the deprecation sentence, got {answered}"
    );
}

/// Assert no application is stored under `name`.
async fn assert_nothing_stored(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    name: &str,
) {
    let (status, described) = call(
        router,
        "DescribeApplication",
        &json!({"ApplicationName": name}),
    )
    .await;
    assert_eq!(
        (status, error_code(&described).as_str()),
        (400, "ResourceNotFoundException"),
        "case {case}: a refused call stores nothing, got {status} {described}"
    );
    assert_eq!(
        error_message(&described),
        format!("Application '{name}' does not exist."),
        "case {case}: expected the miss to name the application, got {described}"
    );
}

/// The table above is held to the model: it names exactly the members the
/// `RuntimeEnvironment` enum declares (the generated `RUNTIME_ENVIRONMENT_VALUES` in
/// `src/ops/types/shared_types.rs`), so every generation the API admits a request for is
/// judged by one of its rows.
#[tokio::test]
async fn the_runtime_table_names_every_runtime_the_model_declares() {
    let mut tabled: Vec<&str> = RUNTIME_OUTCOMES.iter().map(|(runtime, _)| *runtime).collect();
    tabled.sort_unstable();
    let mut declared: Vec<&str> = RUNTIME_ENVIRONMENT_VALUES.to_vec();
    declared.sort_unstable();
    assert_eq!(
        tabled, declared,
        "case table: expected the table to name every declared runtime, got {tabled:?} \
         against {declared:?}"
    );
}

/// `CreateApplication` creates an application for a runtime generation this API still puts
/// one on, and refuses one it does not: a Studio notebook runs the Zeppelin/Flink pair its
/// generation names and this API offers the current pair only, so an earlier generation of
/// that family is refused with HTTP 400 `InvalidArgumentException` and the deprecation
/// sentence, while the retired SQL family keeps its own `UnsupportedOperationException`. A
/// served create is readable back on the runtime it named; a refused one stores nothing.
///
/// The table is the WHOLE enum, held to the generated `RUNTIME_ENVIRONMENT_VALUES` by
/// [`the_runtime_table_names_every_runtime_the_model_declares`], so a runtime the issue never
/// cited is covered and a value added to the model later has to be classified.
#[tokio::test]
async fn create_serves_only_a_runtime_generation_this_api_still_supports() {
    for (runtime, outcome) in RUNTIME_OUTCOMES {
        // One engine per row, so "nothing was stored" is checked against a store only this
        // row wrote to.
        let router = router();
        let name = format!("app-{}", runtime.replace(['-', '_'], "."));
        let (status, body) = call(&router, "CreateApplication", &create_body(&name, runtime)).await;

        match outcome {
            RuntimeOutcome::SqlFamilyRetired => {
                assert!(
                    is_sql(runtime),
                    "case {runtime:?}: only the SQL family earns the retired-family refusal"
                );
                assert_eq!(
                    status, 400,
                    "case {runtime:?}: expected HTTP 400 for a SQL application, got {status} \
                     {body}"
                );
                assert_eq!(
                    error_code(&body),
                    "UnsupportedOperationException",
                    "case {runtime:?}: expected UnsupportedOperationException, got {body}"
                );
                assert_eq!(
                    error_message(&body),
                    "CreateApplication is not supported for SQL applications in this Region. You \
                     can use KDA Studio to build streaming applications with Flink SQL.",
                    "case {runtime:?}: expected the unsupported-runtime message, got {body}"
                );
                assert_nothing_stored(&router, runtime, &name).await;
            }
            RuntimeOutcome::Deprecated => {
                assert_deprecation_refusal(runtime, runtime, status, &body);
                assert_nothing_stored(&router, runtime, &name).await;
            }
            RuntimeOutcome::Created => {
                assert_eq!(
                    status, 200,
                    "case {runtime:?}: expected the application to be created, got {status} \
                     {body}"
                );
                let (status, described) = call(
                    &router,
                    "DescribeApplication",
                    &json!({"ApplicationName": name}),
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {runtime:?}: DescribeApplication got {status} {described}"
                );
                let detail = &described["ApplicationDetail"];
                assert_eq!(
                    (
                        detail["RuntimeEnvironment"].as_str(),
                        detail["ApplicationStatus"].as_str()
                    ),
                    (Some(runtime), Some("READY")),
                    "case {runtime:?}: expected the stored application to run READY on the \
                     runtime it was created on, got {detail}"
                );
            }
        }
    }
}

/// What the deprecation refusal is about is the runtime GENERATION, so it holds whatever the
/// request names around it: every `ApplicationMode` a create can carry — none at all, the
/// mode a notebook runs in, and the mode of the other kind of application — and
/// configurations of different shapes all draw the same sentence, and none of them creates
/// anything.
///
/// The mode rows matter on their own: a mode a runtime does not run is refused by a rule of
/// its own, and the withdrawal of the generation is decided FIRST, so a client is told what
/// is actually wrong with its request rather than being pointed at the mode.
#[tokio::test]
async fn a_deprecated_runtime_is_refused_whatever_the_request_names_around_it() {
    // (case, the members the create carries besides its name and its runtime)
    let shapes: [(&str, Value); 5] = [
        ("no mode", json!({})),
        (
            "the interactive mode",
            json!({"ApplicationMode": "INTERACTIVE"}),
        ),
        (
            "the streaming mode",
            json!({"ApplicationMode": "STREAMING"}),
        ),
        (
            "a note as its code",
            json!({
                "ApplicationMode": "INTERACTIVE",
                "ApplicationConfiguration": {
                    "ApplicationCodeConfiguration": {
                        "CodeContentType": "PLAINTEXT",
                        "CodeContent": {"TextContent": r#"{"id": "2A94M5J1Z", "name": "orders"}"#},
                    },
                },
            }),
        ),
        (
            "program text as its code, and tags",
            json!({
                "ApplicationConfiguration": {
                    "ApplicationCodeConfiguration": {
                        "CodeContentType": "PLAINTEXT",
                        "CodeContent": {"TextContent": "%flink.ssql\nSELECT 1"},
                    },
                },
                "Tags": [{"Key": "team", "Value": "analytics"}],
            }),
        ),
    ];

    for runtime in deprecated_runtimes() {
        for (shape, extra) in &shapes {
            let router = router();
            let name = "mode-independent";
            let case = format!("{runtime}/{shape}");
            let mut body = json!({
                "ApplicationName": name,
                "RuntimeEnvironment": runtime,
                "ServiceExecutionRole": "arn:aws:iam::000000000000:role/kda-service-role",
            });
            for (member, value) in extra
                .as_object()
                .expect("each row's members are a JSON object")
            {
                body[member] = value.clone();
            }

            let (status, answered) = call(&router, "CreateApplication", &body).await;
            assert_deprecation_refusal(&case, runtime, status, &answered);
            assert_nothing_stored(&router, &case, name).await;
        }
    }
}

/// An update is the other way a request could put an application on a withdrawn generation,
/// so it is held to the same rule: one naming a deprecated `RuntimeEnvironmentUpdate` is
/// refused with the same sentence and the application keeps the runtime and the version it
/// had, while an update naming no runtime is served whatever generation the application runs
/// on.
#[tokio::test]
async fn an_update_that_moves_an_application_onto_a_deprecated_runtime_is_refused() {
    for stored_runtime in [current_notebook_runtime(), "FLINK-1_20"] {
        for target in deprecated_runtimes() {
            let router = router();
            let name = "update-target";
            let case = format!("{stored_runtime} -> {target}");
            let (status, created) = call(
                &router,
                "CreateApplication",
                &create_body(name, stored_runtime),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case}: CreateApplication got {status} {created}"
            );

            let (status, answered) = call(
                &router,
                "UpdateApplication",
                &json!({
                    "ApplicationName": name,
                    "CurrentApplicationVersionId": 1,
                    "RuntimeEnvironmentUpdate": target,
                }),
            )
            .await;
            assert_deprecation_refusal(&case, target, status, &answered);

            let (status, described) = call(
                &router,
                "DescribeApplication",
                &json!({"ApplicationName": name}),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case}: DescribeApplication got {status} {described}"
            );
            let detail = &described["ApplicationDetail"];
            assert_eq!(
                (
                    detail["RuntimeEnvironment"].as_str(),
                    detail["ApplicationVersionId"].as_i64()
                ),
                (Some(stored_runtime), Some(1)),
                "case {case}: a refused update leaves the runtime and the version as they \
                 were, got {detail}"
            );
        }

        // The control: an update naming no runtime is served, so what the rule refuses is the
        // runtime a request NAMES rather than every update of the application.
        let router = router();
        let name = "untouched-runtime";
        let (status, created) = call(
            &router,
            "CreateApplication",
            &create_body(name, stored_runtime),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {stored_runtime}/control: CreateApplication got {status} {created}"
        );
        let (status, updated) = call(
            &router,
            "UpdateApplication",
            &json!({
                "ApplicationName": name,
                "CurrentApplicationVersionId": 1,
                "ApplicationConfigurationUpdate": {
                    "ApplicationSnapshotConfigurationUpdate": {"SnapshotsEnabledUpdate": true},
                },
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {stored_runtime}/control: expected the update to be served, got {status} \
             {updated}"
        );
        let detail = &updated["ApplicationDetail"];
        assert_eq!(
            (
                detail["RuntimeEnvironment"].as_str(),
                detail["ApplicationVersionId"].as_i64()
            ),
            (Some(stored_runtime), Some(2)),
            "case {stored_runtime}/control: expected the served update to advance the version \
             on the same runtime, got {detail}"
        );
    }
}

// ---------------------------------------------------------------------------
// Lookup misses
// ---------------------------------------------------------------------------

/// Every operation that looks an application up by name answers a miss with the error code
/// its own model declares for that path, HTTP 400 and a `Message` naming the application:
/// `ResourceNotFoundException` for the readers and the lifecycle calls,
/// `InvalidArgumentException` for `CreateApplicationSnapshot`, which reports a missing
/// application as a bad argument and names the account it looked in.
///
/// The names span the forms `ApplicationName` admits (len 1..128, `^[a-zA-Z0-9_.-]+$`):
/// the shortest, one with each admitted separator, and the longest — so what is asserted
/// is the rule and not one recorded name. One STORED application is created first, so a
/// miss is a miss about the store and not about an empty one.
#[tokio::test]
async fn a_name_no_application_is_stored_under_is_refused_with_the_modelled_code() {
    let router = router();
    let (status, created) = call(
        &router,
        "CreateApplication",
        &create_body("stored-app", "FLINK-1_20"),
    )
    .await;
    assert_eq!(status, 200, "CreateApplication got {created}");

    let longest = "n".repeat(128);
    let absent = ["a", "no-such-app", "probe.app_2-absent", longest.as_str()];

    for name in absent {
        // (case, operation, body, expected error code, expected message)
        let rows: [(&str, &str, Value, &str, String); 4] = [
            (
                "describe",
                "DescribeApplication",
                json!({"ApplicationName": name}),
                "ResourceNotFoundException",
                format!("Application '{name}' does not exist."),
            ),
            (
                "start",
                "StartApplication",
                json!({"ApplicationName": name}),
                "ResourceNotFoundException",
                format!("Application '{name}' does not exist."),
            ),
            (
                "stop",
                "StopApplication",
                json!({"ApplicationName": name}),
                "ResourceNotFoundException",
                format!("Application '{name}' does not exist."),
            ),
            (
                "snapshot",
                "CreateApplicationSnapshot",
                json!({"ApplicationName": name, "SnapshotName": "snap-1"}),
                "InvalidArgumentException",
                format!("Application {name} does not exist under accountId {ACCOUNT}"),
            ),
        ];

        for (case, operation, body, expected_code, expected_message) in rows {
            let (status, response) = call(&router, operation, &body).await;
            assert_eq!(
                status, 400,
                "case {case}/{name:?}: expected HTTP 400, got {status} {response}"
            );
            assert_eq!(
                error_code(&response),
                expected_code,
                "case {case}/{name:?}: expected {expected_code}, got {response}"
            );
            assert_eq!(
                error_message(&response),
                expected_message,
                "case {case}/{name:?}: expected the Message to name the absent application, \
                 got {response}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// StartApplication: a configuration it can start
// ---------------------------------------------------------------------------

/// An `ApplicationConfiguration` carrying `code`, or none at all when `code` is null.
fn configuration_with(code: Value) -> Value {
    if code.is_null() {
        return Value::Null;
    }
    json!({"ApplicationCodeConfiguration": code})
}

/// `StartApplication` starts an application whose stored configuration carries code
/// content and refuses one that carries none with HTTP 400
/// `InvalidApplicationConfigurationException`, leaving it unstarted.
///
/// The rows span every form `CodeContent` can take
/// (`smithy_query(query="shape",shape="CodeContent")`: inline text, a zip blob, an S3
/// object location) and every way it can be missing (no configuration at all, a code
/// configuration with no content, an empty text content, an empty zip), across the runtime
/// families — including the `ZEPPELIN-FLINK-` notebook family, whose code is entered in
/// the notebook rather than submitted, so it starts without any.
#[tokio::test]
async fn start_refuses_an_application_with_no_valid_code_content() {
    // (case, runtime, ApplicationCodeConfiguration, whether the start is accepted)
    let rows: [(&str, &str, Value, bool); 9] = [
        ("no configuration at all", "FLINK-1_20", Value::Null, false),
        (
            "a code configuration with no content",
            "FLINK-1_20",
            json!({"CodeContentType": "PLAINTEXT"}),
            false,
        ),
        (
            "an empty text content",
            "FLINK-1_20",
            json!({"CodeContentType": "PLAINTEXT", "CodeContent": {"TextContent": ""}}),
            false,
        ),
        (
            "an empty zip content",
            "FLINK-1_20",
            json!({"CodeContentType": "ZIPFILE", "CodeContent": {"ZipFileContent": ""}}),
            false,
        ),
        (
            "inline text content",
            "FLINK-1_20",
            json!({"CodeContentType": "PLAINTEXT", "CodeContent": {"TextContent": "env.execute();"}}),
            true,
        ),
        (
            "a zip blob",
            "FLINK-1_8",
            json!({"CodeContentType": "ZIPFILE", "CodeContent": {"ZipFileContent": "UEsDBAo="}}),
            true,
        ),
        (
            "an S3 object location",
            "FLINK-2_3",
            json!({
                "CodeContentType": "ZIPFILE",
                "CodeContent": {
                    "S3ContentLocation": {
                        "BucketARN": "arn:aws:s3:::kda-code",
                        "FileKey": "app.jar"
                    }
                }
            }),
            true,
        ),
        // A Studio notebook's code is in the notebook, so it needs none stored.
        (
            "a notebook runtime with no code content",
            "ZEPPELIN-FLINK-3_0",
            Value::Null,
            true,
        ),
        (
            "a streaming runtime of another family with no code content",
            "FLINK-1_11",
            Value::Null,
            false,
        ),
    ];

    for (case, runtime, code, accepted) in rows {
        let router = router();
        let name = "start-probe";
        let mut body = create_body(name, runtime);
        let configuration = configuration_with(code);
        if !configuration.is_null() {
            body["ApplicationConfiguration"] = configuration;
        }
        if runtime.starts_with("ZEPPELIN-FLINK-") {
            // A notebook is an INTERACTIVE application, as its runtime's doc states.
            body["ApplicationMode"] = json!("INTERACTIVE");
        }
        let (status, created) = call(&router, "CreateApplication", &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateApplication got {status} {created}"
        );

        let (status, started) =
            call(&router, "StartApplication", &json!({"ApplicationName": name})).await;
        if accepted {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the start to be accepted, got {status} {started}"
            );
            assert!(
                started["OperationId"].as_str().is_some(),
                "case {case:?}: an accepted start answers with an OperationId, got {started}"
            );
            continue;
        }

        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} {started}"
        );
        assert_eq!(
            error_code(&started),
            "InvalidApplicationConfigurationException",
            "case {case:?}: expected InvalidApplicationConfigurationException, got {started}"
        );
        assert_eq!(
            error_message(&started),
            format!(
                "Application '{name}' under account '{ACCOUNT}' does not have a valid \
                 configuration to start. Application is not configured with a valid code \
                 content. Please update the application with a valid code content."
            ),
            "case {case:?}: expected the start-readiness message, got {started}"
        );

        // A refused start leaves the application where it was: READY, not STARTING.
        let (status, described) = call(
            &router,
            "DescribeApplication",
            &json!({"ApplicationName": name}),
        )
        .await;
        assert_eq!(status, 200, "case {case:?}: DescribeApplication got {described}");
        assert_eq!(
            described["ApplicationDetail"]["ApplicationStatus"].as_str(),
            Some("READY"),
            "case {case:?}: a refused start must not move the stored status, got {described}"
        );
    }
}

// ---------------------------------------------------------------------------
// Snapshots
// ---------------------------------------------------------------------------

/// A snapshot of a RUNNING application is stored and read back, and every other request
/// for one is refused with the modelled code: a duplicate name is
/// `ResourceInUseException`, an application outside RUNNING is `InvalidRequestException`,
/// and a snapshot nothing stored is `ResourceNotFoundException`.
#[tokio::test]
async fn a_snapshot_of_a_running_application_is_stored_and_read_back() {
    let router = router();
    let name = "snapshot-app";
    let mut body = create_body(name, "FLINK-1_20");
    body["ApplicationConfiguration"] = json!({
        "ApplicationCodeConfiguration": {
            "CodeContentType": "PLAINTEXT",
            "CodeContent": {"TextContent": "env.execute();"}
        }
    });
    let (status, created) = call(&router, "CreateApplication", &body).await;
    assert_eq!(status, 200, "CreateApplication got {created}");

    // A READY application holds no running state, so there is nothing to snapshot yet:
    // the request is not a valid one for the application as it stands. The status gate's
    // own rule is asserted over every status in `tests/all/error_envelope_wording.rs`.
    let (status, refused) = call(
        &router,
        "CreateApplicationSnapshot",
        &json!({"ApplicationName": name, "SnapshotName": "snap-ready"}),
    )
    .await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (400, "InvalidRequestException"),
        "case not-running: expected InvalidRequestException, got {status} {refused}"
    );

    let (status, started) =
        call(&router, "StartApplication", &json!({"ApplicationName": name})).await;
    assert_eq!(status, 200, "StartApplication got {started}");
    tokio::time::sleep(SETTLE_WAIT).await;

    for snapshot in ["snap-1", "snap.2_final"] {
        let (status, taken) = call(
            &router,
            "CreateApplicationSnapshot",
            &json!({"ApplicationName": name, "SnapshotName": snapshot}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {snapshot:?}: CreateApplicationSnapshot got {status} {taken}"
        );

        // The round trip: the reader reports the record the writer stored, in the
        // transient status a caller reading straight back must still see.
        let (status, described) = call(
            &router,
            "DescribeApplicationSnapshot",
            &json!({"ApplicationName": name, "SnapshotName": snapshot}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {snapshot:?}: DescribeApplicationSnapshot got {status} {described}"
        );
        let details = &described["SnapshotDetails"];
        assert_eq!(
            details["SnapshotName"].as_str(),
            Some(snapshot),
            "case {snapshot:?}: expected the stored snapshot name, got {details}"
        );
        assert_eq!(
            details["SnapshotStatus"].as_str(),
            Some("CREATING"),
            "case {snapshot:?}: a snapshot is CREATING when the call answers, got {details}"
        );
        assert_eq!(
            details["ApplicationVersionId"].as_i64(),
            Some(1),
            "case {snapshot:?}: expected the application version it was taken of, got {details}"
        );
        assert_eq!(
            details["RuntimeEnvironment"].as_str(),
            Some("FLINK-1_20"),
            "case {snapshot:?}: expected the application's runtime, got {details}"
        );
        assert!(
            details["SnapshotCreationTimestamp"]
                .as_i64()
                .is_some_and(|t| t > 0),
            "case {snapshot:?}: expected a creation timestamp, got {details}"
        );

        // The same name cannot be taken twice.
        let (status, duplicate) = call(
            &router,
            "CreateApplicationSnapshot",
            &json!({"ApplicationName": name, "SnapshotName": snapshot}),
        )
        .await;
        assert_eq!(
            (status, error_code(&duplicate).as_str()),
            (400, "ResourceInUseException"),
            "case {snapshot:?}: a duplicate snapshot name is refused, got {status} {duplicate}"
        );
    }

    // READY becomes due on the clock, without any further call.
    tokio::time::sleep(SETTLE_WAIT).await;
    let (status, described) = call(
        &router,
        "DescribeApplicationSnapshot",
        &json!({"ApplicationName": name, "SnapshotName": "snap-1"}),
    )
    .await;
    assert_eq!(status, 200, "DescribeApplicationSnapshot got {described}");
    assert_eq!(
        described["SnapshotDetails"]["SnapshotStatus"].as_str(),
        Some("READY"),
        "case settled: the clock owed READY, got {described}"
    );

    // A snapshot name nothing stored, and a snapshot of another application's name: the
    // snapshot miss is about the SNAPSHOT, while a name no application is stored under is
    // the parameter violation the snapshot operations give an application miss.
    for (case, body, expected_code) in [
        (
            "unknown snapshot",
            json!({"ApplicationName": name, "SnapshotName": "no-such-snapshot"}),
            "ResourceNotFoundException",
        ),
        (
            "unknown application",
            json!({"ApplicationName": "no-such-app", "SnapshotName": "snap-1"}),
            "InvalidArgumentException",
        ),
    ] {
        let (status, response) = call(&router, "DescribeApplicationSnapshot", &body).await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, expected_code),
            "case {case}: expected {expected_code}, got {status} {response}"
        );
        assert!(
            !error_message(&response).is_empty(),
            "case {case}: expected a Message naming what was not found, got {response}"
        );
    }
}

// ---------------------------------------------------------------------------
// The parallelism an application is asked to run at
// ---------------------------------------------------------------------------

/// `ParallelismPerKPU` is bounded by the value this service runs an application at: a
/// configuration past it is refused with the modelled `InvalidArgumentException`, naming the
/// application the configuration was supplied for, and nothing is stored or changed — while a
/// value within it is taken and read back.
///
/// The rule is the service's own, not the model's (`ParallelismPerKPU` declares only `@range`
/// min 1), so it is asserted on both operations that carry the member — the create that
/// supplies the configuration and the update that replaces it — over a table that spans the
/// class: the lowest value the model admits, the greatest the service runs, the first value
/// past it, a value far past it, and the largest an `integer` carries.
#[tokio::test]
async fn a_parallelism_per_kpu_past_the_supported_value_is_refused() {
    const SUPPORTED: i32 = 8;
    // (case, the value configured, whether the service runs an application at it)
    let rows: [(&str, i32, bool); 5] = [
        ("the lowest value the model admits", 1, true),
        ("the greatest value the service runs", SUPPORTED, true),
        ("the first value past it", SUPPORTED + 1, false),
        ("a value far past it", 1_000, false),
        ("the largest an integer carries", i32::MAX, false),
    ];

    for (case, per_kpu, supported) in rows {
        let router = router();
        let name = "parallelism-probe";
        let mut body = create_body(name, "FLINK-1_20");
        body["ApplicationConfiguration"] = json!({
            "FlinkApplicationConfiguration": {
                "ParallelismConfiguration": {
                    "ConfigurationType": "CUSTOM",
                    "ParallelismPerKPU": per_kpu,
                }
            }
        });
        let expected_message = format!(
            "FlinkApplicationParallelismPerKPU provided for the application {name} should be \
             a positive integer and should not be larger than the supported limit \
             {SUPPORTED}"
        );

        let (status, answered) = call(&router, "CreateApplication", &body).await;
        if supported {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the create to be accepted, got {status} {answered}"
            );
            let described = answered["ApplicationDetail"]["ApplicationConfigurationDescription"]
                ["FlinkApplicationConfigurationDescription"]
                ["ParallelismConfigurationDescription"]["ParallelismPerKPU"]
                .as_i64();
            assert_eq!(
                described,
                Some(i64::from(per_kpu)),
                "case {case:?}: expected the configured value to be stored, got {answered}"
            );
        } else {
            assert_eq!(
                (status, error_code(&answered).as_str()),
                (400, "InvalidArgumentException"),
                "case {case:?}: expected the create to be refused, got {status} {answered}"
            );
            assert_eq!(
                error_message(&answered),
                expected_message,
                "case {case:?}: expected {expected_message:?}, got {answered}"
            );
            // Nothing was stored, so the name still holds no application.
            let (status, described) = call(
                &router,
                "DescribeApplication",
                &json!({"ApplicationName": name}),
            )
            .await;
            assert_eq!(
                (status, error_code(&described).as_str()),
                (400, "ResourceNotFoundException"),
                "case {case:?}: a refused create stores nothing, got {status} {described}"
            );
        }

        // The same bound on the UPDATE that replaces the parallelism of an application the
        // store already holds, which is left on the version and the configuration it was on.
        let updated = "parallelism-update-probe";
        let (status, created) = call(
            &router,
            "CreateApplication",
            &create_body(updated, "FLINK-1_20"),
        )
        .await;
        assert_eq!(status, 200, "case {case:?}: CreateApplication got {created}");
        let (status, answered) = call(
            &router,
            "UpdateApplication",
            &json!({
                "ApplicationName": updated,
                "CurrentApplicationVersionId": 1,
                "ApplicationConfigurationUpdate": {
                    "FlinkApplicationConfigurationUpdate": {
                        "ParallelismConfigurationUpdate": {
                            "ConfigurationTypeUpdate": "CUSTOM",
                            "ParallelismPerKPUUpdate": per_kpu,
                        }
                    }
                },
            }),
        )
        .await;
        if supported {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the update to be accepted, got {status} {answered}"
            );
        } else {
            let expected_message = expected_message.replace(name, updated);
            assert_eq!(
                (status, error_code(&answered).as_str()),
                (400, "InvalidArgumentException"),
                "case {case:?}: expected the update to be refused, got {status} {answered}"
            );
            assert_eq!(
                error_message(&answered),
                expected_message,
                "case {case:?}: expected {expected_message:?}, got {answered}"
            );
            let (status, described) = call(
                &router,
                "DescribeApplication",
                &json!({"ApplicationName": updated}),
            )
            .await;
            assert_eq!(
                (status, described["ApplicationDetail"]["ApplicationVersionId"].as_i64()),
                (200, Some(1)),
                "case {case:?}: a refused update mints no version, got {status} {described}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The status a modelled client error travels with
// ---------------------------------------------------------------------------

/// Every modelled CLIENT error of this service travels with HTTP 400, whatever `@httpError`
/// its shape carries: this is an RPC (`awsJson1_1`) service, whose envelope discriminates a
/// failure by the error TYPE in the body rather than by the transport status.
///
/// `ConcurrentModificationException` is the shape that makes the rule observable — the model
/// marks it `@httpError(409)`
/// (`smithy_query(query="shape",shape="ConcurrentModificationException")`) — so the rows
/// reach it from the three refusals that raise it, over two operations and both members a
/// change addresses a version by, beside one row per other client error class this suite can
/// reach, each of which the model already annotates 400.
#[tokio::test]
async fn every_modelled_client_error_travels_with_the_client_error_status() {
    let router = router();
    let name = "wire-status-app";
    let (status, created) = call(
        &router,
        "CreateApplication",
        &create_body(name, "FLINK-1_20"),
    )
    .await;
    assert_eq!(status, 200, "CreateApplication got {status} {created}");
    let created_at = created["ApplicationDetail"]["CreateTimestamp"]
        .as_i64()
        .unwrap_or_else(|| panic!("expected a CreateTimestamp, got {created}"));
    let token = created["ApplicationDetail"]["ConditionalToken"]
        .as_str()
        .unwrap_or_else(|| panic!("expected a ConditionalToken, got {created}"))
        .to_string();

    // (case, operation, request, the error code the model declares for that refusal)
    let rows: Vec<(&str, &str, Value, &str)> = vec![
        (
            "an update addressed to a version that is not in force",
            "UpdateApplication",
            json!({"ApplicationName": name, "CurrentApplicationVersionId": 7}),
            "ConcurrentModificationException",
        ),
        (
            "an update addressed by a token that is not in force",
            "UpdateApplication",
            json!({"ApplicationName": name, "ConditionalToken": format!("{token}xy")}),
            "ConcurrentModificationException",
        ),
        (
            "a delete naming a create timestamp that is not the stored one",
            "DeleteApplication",
            json!({"ApplicationName": name, "CreateTimestamp": created_at + 60}),
            "ConcurrentModificationException",
        ),
        (
            "a create of a name the store already holds",
            "CreateApplication",
            create_body(name, "FLINK-1_20"),
            "ResourceInUseException",
        ),
        (
            "a read of a name nothing is stored under",
            "DescribeApplication",
            json!({"ApplicationName": "wire-status-absent"}),
            "ResourceNotFoundException",
        ),
        (
            "a create on the retired SQL runtime family",
            "CreateApplication",
            create_body("wire-status-sql", "SQL-1_0"),
            "UnsupportedOperationException",
        ),
        (
            "a value outside a modelled enumerated type",
            "CreateApplication",
            create_body("wire-status-enum", "FLINK-99_9"),
            "ValidationException",
        ),
    ];

    for (case, operation, request, expected_code) in rows {
        let (status, refused) = call(&router, operation, &request).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, expected_code),
            "case {case:?}: expected HTTP 400 {expected_code}, got {status} {refused}"
        );
        assert!(
            !error_message(&refused).is_empty(),
            "case {case:?}: expected a message stating the refusal, got {refused}"
        );
    }
}

// ---------------------------------------------------------------------------
// A Studio notebook's inline code, and the operations its runtime does not keep
// ---------------------------------------------------------------------------
//
// The Studio-notebook runtimes a rule below is driven on come from
// `harness::notebook_runtimes`: the generations of the `ZEPPELIN-FLINK-` family this API
// still creates a notebook on, since no notebook exists on a withdrawn one.

/// What the service answers inline Studio code that is not a Zeppelin note: fixed text, so
/// the sentence is asserted whole.
const NOT_A_NOTE: &str = "Zeppelin application code provided in TextContent must be a valid \
                          Zeppelin note JSON object containing 'id' and 'name' fields";

/// One inline `TextContent` and whether it is a Zeppelin note.
///
/// The rows span the forms a value can take — a note object, a note object carrying the rest
/// of what Zeppelin stores in one, an object missing either identifying member, an object
/// whose member is null, a JSON document that is not an object at all, text that is not JSON,
/// and text that is truncated JSON — because the rule is about the FORM of the value and not
/// about one recorded paragraph.
const CODE_ROWS: [(&str, &str, bool); 11] = [
    ("a note object", r#"{"id": "2A94M5J1Z", "name": "orders"}"#, true),
    (
        "a note object carrying its paragraphs",
        r#"{"id": "2EWM84JXA", "name": "notebooks/orders", "paragraphs": [{"text": "%flink.ssql\nSELECT 1"}], "angularObjects": {}}"#,
        true,
    ),
    (
        "a note object whose members are in the other order",
        r#"{"name": "orders", "id": "2A94M5J1Z"}"#,
        true,
    ),
    (
        "a bare paragraph",
        "%flink.ssql\nSELECT ticker, count(*) FROM orders GROUP BY ticker",
        false,
    ),
    ("a note object with no id", r#"{"name": "orders"}"#, false),
    ("a note object with no name", r#"{"id": "2A94M5J1Z"}"#, false),
    (
        "a note object whose id is null",
        r#"{"id": null, "name": "orders"}"#,
        false,
    ),
    (
        "an array of note objects",
        r#"[{"id": "2A94M5J1Z", "name": "orders"}]"#,
        false,
    ),
    ("a JSON string", r#""2A94M5J1Z""#, false),
    ("no code at all", "", false),
    (
        "a truncated note object",
        r#"{"id": "2A94M5J1Z", "name": "#,
        false,
    ),
];

/// An `ApplicationConfiguration` carrying `text` as the application's inline code.
fn code_configuration(text: &str) -> Value {
    json!({
        "ApplicationCodeConfiguration": {
            "CodeContentType": "PLAINTEXT",
            "CodeContent": {"TextContent": text},
        }
    })
}

/// The inline code `DescribeApplication` reports for one application.
fn described_text_content(detail: &Value) -> Option<&str> {
    detail
        .pointer(
            "/ApplicationConfigurationDescription/ApplicationCodeConfigurationDescription\
             /CodeContentDescription/TextContent",
        )
        .and_then(Value::as_str)
}

/// A Studio notebook's code is a Zeppelin NOTE — the Apache Zeppelin runtime holds one as a
/// JSON object carrying the note's `id` and `name` — so a create supplying anything else as
/// `CodeContent$TextContent` for a notebook runtime is refused with the modelled
/// `InvalidArgumentException` and stores nothing, while the same value is code for a
/// Managed Service for Apache Flink application, which runs inline code as the program text
/// it is.
#[tokio::test]
async fn a_studio_notebooks_inline_code_has_to_be_a_zeppelin_note() {
    let notebooks = notebook_runtimes();
    for (index, (case, text, is_note)) in CODE_ROWS.iter().enumerate() {
        // One engine per row, so "nothing was stored" is checked against a store only this
        // row wrote to; the notebook runtime cycles through every generation of the family
        // this API still creates a notebook on.
        let router = router();
        let runtime = notebooks[index % notebooks.len()];
        let name = format!("note-probe-{index}");
        let mut body = create_body(&name, runtime);
        body["ApplicationConfiguration"] = code_configuration(text);
        let (status, answered) = call(&router, "CreateApplication", &body).await;

        if *is_note {
            assert_eq!(
                status, 200,
                "case {case:?} on {runtime}: expected the note to be accepted, got {status} \
                 {answered}"
            );
            let (status, described) = call(
                &router,
                "DescribeApplication",
                &json!({"ApplicationName": name}),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?} on {runtime}: DescribeApplication got {status} {described}"
            );
            assert_eq!(
                described_text_content(&described["ApplicationDetail"]),
                Some(*text),
                "case {case:?} on {runtime}: expected the stored note to be read back, got \
                 {described}"
            );
            continue;
        }

        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, "InvalidArgumentException"),
            "case {case:?} on {runtime}: expected InvalidArgumentException, got {status} \
             {answered}"
        );
        assert_eq!(
            error_message(&answered),
            NOT_A_NOTE,
            "case {case:?} on {runtime}: expected the note-form refusal, got {answered}"
        );
        // A refused create stores nothing, so the name still resolves to no application.
        let (status, described) = call(
            &router,
            "DescribeApplication",
            &json!({"ApplicationName": name}),
        )
        .await;
        assert_eq!(
            (status, error_code(&described).as_str()),
            (400, "ResourceNotFoundException"),
            "case {case:?} on {runtime}: a refused create stores nothing, got {status} \
             {described}"
        );
        assert_eq!(
            error_message(&described),
            format!("Application '{name}' does not exist."),
            "case {case:?} on {runtime}: expected the miss to name the application, got \
             {described}"
        );
    }
}

/// The same values are CODE for a Managed Service for Apache Flink application: the note
/// rule is the notebook runtime's, not a rule about inline code in general.
#[tokio::test]
async fn a_flink_applications_inline_code_is_not_held_to_the_note_rule() {
    let router = router();
    // Every `FLINK-` version the enum admits a create for, so the control spans the family
    // rather than one version.
    let streaming: Vec<&&str> = RUNTIME_ENVIRONMENT_VALUES
        .iter()
        .filter(|runtime| runtime.starts_with("FLINK-"))
        .collect();
    assert!(
        !streaming.is_empty(),
        "expected the enum to carry a streaming runtime family, got \
         {RUNTIME_ENVIRONMENT_VALUES:?}"
    );
    for (index, runtime) in streaming.into_iter().enumerate() {
        let (case, text, _) = CODE_ROWS[3];
        let name = format!("code-{index}");
        let mut body = create_body(&name, runtime);
        body["ApplicationConfiguration"] = code_configuration(text);
        let (status, answered) = call(&router, "CreateApplication", &body).await;
        assert_eq!(
            status, 200,
            "case {case:?} on {runtime}: expected inline code to be accepted, got {status} \
             {answered}"
        );
        assert_eq!(
            described_text_content(&answered["ApplicationDetail"]),
            Some(text),
            "case {case:?} on {runtime}: expected the code to be stored as given, got \
             {answered}"
        );
    }
}

/// An update that WRITES a notebook's code is held to the same rule, judged on the runtime
/// the application runs on once the update lands — so moving an application onto a notebook
/// runtime while writing program text is refused too — and a refused update leaves the
/// stored code and version exactly as they were.
#[tokio::test]
async fn an_update_that_writes_inline_code_is_held_to_the_rule_of_the_runtime_it_lands_on() {
    let note = CODE_ROWS[0].1;
    let other_note = CODE_ROWS[1].1;
    let paragraph = CODE_ROWS[3].1;

    // (case, the runtime the application is created on, the update body, whether it is
    // accepted) — the four combinations of the runtime the update lands on and the form of
    // the code it writes.
    let rows: [(&str, &str, Value, bool); 5] = [
        (
            "a notebook given another note",
            "ZEPPELIN-FLINK-3_0",
            json!({"ApplicationConfigurationUpdate": {
                "ApplicationCodeConfigurationUpdate": {
                    "CodeContentUpdate": {"TextContentUpdate": other_note}
                }
            }}),
            true,
        ),
        (
            "a notebook given program text",
            "ZEPPELIN-FLINK-3_0",
            json!({"ApplicationConfigurationUpdate": {
                "ApplicationCodeConfigurationUpdate": {
                    "CodeContentUpdate": {"TextContentUpdate": paragraph}
                }
            }}),
            false,
        ),
        (
            "a notebook whose update writes no code at all",
            "ZEPPELIN-FLINK-3_0",
            json!({"ApplicationConfigurationUpdate": {
                "EnvironmentPropertyUpdates": {"PropertyGroups": [
                    {"PropertyGroupId": "producer", "PropertyMap": {"topic": "orders"}}
                ]}
            }}),
            true,
        ),
        (
            "a streaming application given program text",
            "FLINK-1_20",
            json!({"ApplicationConfigurationUpdate": {
                "ApplicationCodeConfigurationUpdate": {
                    "CodeContentUpdate": {"TextContentUpdate": paragraph}
                }
            }}),
            true,
        ),
        (
            "a streaming application moved onto a notebook runtime with program text",
            "FLINK-1_20",
            json!({
                "RuntimeEnvironmentUpdate": "ZEPPELIN-FLINK-3_0",
                "ApplicationConfigurationUpdate": {
                    "ApplicationCodeConfigurationUpdate": {
                        "CodeContentUpdate": {"TextContentUpdate": paragraph}
                    }
                },
            }),
            false,
        ),
    ];

    for (index, (case, runtime, update, accepted)) in rows.into_iter().enumerate() {
        let router = router();
        let name = format!("code-update-{index}");
        // Every application starts from code its own runtime runs: a note for a notebook,
        // program text for a streaming application.
        let stored = if runtime.starts_with("ZEPPELIN-") {
            note
        } else {
            paragraph
        };
        let mut body = create_body(&name, runtime);
        body["ApplicationConfiguration"] = code_configuration(stored);
        let (status, created) = call(&router, "CreateApplication", &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateApplication got {status} {created}"
        );

        let mut request = update;
        request["ApplicationName"] = json!(name);
        request["CurrentApplicationVersionId"] = json!(1);
        let (status, answered) = call(&router, "UpdateApplication", &request).await;
        if accepted {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the update to be accepted, got {status} {answered}"
            );
            assert_eq!(
                answered["ApplicationDetail"]["ApplicationVersionId"].as_i64(),
                Some(2),
                "case {case:?}: an accepted update mints the next version, got {answered}"
            );
            continue;
        }

        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, "InvalidArgumentException"),
            "case {case:?}: expected InvalidArgumentException, got {status} {answered}"
        );
        assert_eq!(
            error_message(&answered),
            NOT_A_NOTE,
            "case {case:?}: expected the note-form refusal, got {answered}"
        );
        let (status, described) = call(
            &router,
            "DescribeApplication",
            &json!({"ApplicationName": name}),
        )
        .await;
        assert_eq!(status, 200, "case {case:?}: DescribeApplication got {described}");
        let detail = &described["ApplicationDetail"];
        assert_eq!(
            (
                described_text_content(detail),
                detail["ApplicationVersionId"].as_i64(),
                detail["RuntimeEnvironment"].as_str(),
            ),
            (Some(stored), Some(1), Some(runtime)),
            "case {case:?}: a refused update changes nothing, got {described}"
        );
    }
}

/// Taking a snapshot and rolling back to the previous running version are features of a
/// Managed Service for Apache Flink application's job, so on an application whose runtime
/// runs no such job — a Studio notebook — each is refused with the modelled
/// `UnsupportedOperationException`, in that operation's own wording, and the application's
/// version and status do not move.
#[tokio::test]
async fn snapshot_and_rollback_are_unsupported_for_a_studio_notebooks_runtime() {
    for runtime in notebook_runtimes() {
        let router = router();
        let name = format!("studio-{}", runtime.to_lowercase().replace('_', "-"));
        let (status, created) = call(&router, "CreateApplication", &create_body(&name, runtime))
            .await;
        assert_eq!(
            status, 200,
            "case {runtime}: CreateApplication got {status} {created}"
        );

        // (operation, request, the sentence that operation answers for this refusal)
        let rows: [(&str, Value, String); 2] = [
            (
                "CreateApplicationSnapshot",
                json!({"ApplicationName": name, "SnapshotName": "studio-snapshot"}),
                format!("This operation is not supported for RuntimeEnvironment {runtime}"),
            ),
            (
                "RollbackApplication",
                json!({"ApplicationName": name, "CurrentApplicationVersionId": 1}),
                format!(
                    "RollbackApplication is not supported for application with runtime \
                     environment {runtime}."
                ),
            ),
        ];

        for (operation, request, message) in rows {
            let (status, refused) = call(&router, operation, &request).await;
            assert_eq!(
                (status, error_code(&refused).as_str()),
                (400, "UnsupportedOperationException"),
                "case {runtime}/{operation}: expected UnsupportedOperationException, got \
                 {status} {refused}"
            );
            assert_eq!(
                error_message(&refused),
                message,
                "case {runtime}/{operation}: expected the refusal to name the runtime, got \
                 {refused}"
            );
        }

        let (status, described) = call(
            &router,
            "DescribeApplication",
            &json!({"ApplicationName": name}),
        )
        .await;
        let detail = &described["ApplicationDetail"];
        assert_eq!(
            (
                status,
                detail["ApplicationVersionId"].as_i64(),
                detail["ApplicationStatus"].as_str(),
            ),
            (200, Some(1), Some("READY")),
            "case {runtime}: neither refusal moves the application, got {described}"
        );
    }
}

/// The same two refusals are keyed on the RUNTIME and not on the operation: a Managed
/// Service for Apache Flink application is never refused on that ground, so what it is
/// answered is about its state instead.
#[tokio::test]
async fn neither_snapshot_nor_rollback_is_runtime_refused_for_a_flink_application() {
    let router = router();
    let name = "flink-capability";
    let (status, created) =
        call(&router, "CreateApplication", &create_body(name, "FLINK-1_20")).await;
    assert_eq!(status, 200, "CreateApplication got {status} {created}");

    // A READY application holds no running state to snapshot, which is a refusal about its
    // STATUS — the class a client corrects its call on, not the one that says the capability
    // is absent.
    let (status, refused) = call(
        &router,
        "CreateApplicationSnapshot",
        &json!({"ApplicationName": name, "SnapshotName": "flink-snapshot"}),
    )
    .await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (400, "InvalidRequestException"),
        "expected the refusal to be about the status, got {status} {refused}"
    );

    // Rolling a Flink application back is not written yet, so this call is answered as a
    // gap in the emulator; what it must NOT be is the runtime refusal, which belongs to the
    // runtimes that keep no previous running version.
    let (_, answered) = call(
        &router,
        "RollbackApplication",
        &json!({"ApplicationName": name, "CurrentApplicationVersionId": 1}),
    )
    .await;
    assert_ne!(
        error_code(&answered),
        "UnsupportedOperationException",
        "expected no runtime refusal for a Flink application, got {answered}"
    );
}
