// SPDX-License-Identifier: CC0-1.0

//! The expression a constraint-violation message quotes for a `@pattern` requirement.
//!
//! The rule under test: a `@pattern` clause states the expression the WHOLE value has to
//! satisfy — the expression this service publishes for the member's shape — and not the
//! model's trait source, so an end the model ANCHORS loses its anchor and an end the model
//! leaves OPEN is spelled with the unconstrained run that end admits. Everything else about
//! the clause stays request-driven: the value echoed and the member path are the caller's.
//!
//! The rows span every distinct `@pattern` this model declares, read from the model itself
//! (`smithy_query(query="shape",shape="RoleARN" | "DatabaseARN" | "KinesisAnalyticsARN" |
//! "LogStreamARN" | "ApplicationName" | "SnapshotName" | "ConditionalToken" |
//! "ApplicationMaintenanceWindowStartTime" | "BasePath")`): the `^arn:` family, which anchors
//! only its opening, and the fully anchored `^…$` expressions. They span the two ways a
//! member is reached as well — a top-level member and one nested inside a sub-configuration —
//! and five operations, because the expression quoted belongs to the member's shape rather
//! than to the operation that carried it.

use serde_json::{json, Value};

use super::harness::{call, create_body, error_code, error_message, router};

/// The application every case that needs one to exist names.
const APPLICATION: &str = "pattern-clause-probe";

/// The runtime the probe application runs on.
const RUNTIME: &str = "FLINK-1_20";

/// A `RoleARN` that satisfies its own `^arn:` pattern, so a case varying another member
/// reports exactly one violation.
const ROLE: &str = "arn:aws:iam::000000000000:role/kda-service-role";

/// One case: the request to make, the member the value sat at, the value submitted, and the
/// expression the clause must quote for that member.
struct PatternCase {
    case: &'static str,
    operation: &'static str,
    body: Value,
    /// The member path, as the framework's constraint walk spells one.
    path: &'static str,
    submitted: &'static str,
    /// The expression the whole value must satisfy, for the member's declared `@pattern`.
    expression: &'static str,
}

/// Every distinct `@pattern` of this model, quoted as the expression the whole value must
/// satisfy: an ARN member declared `^arn:` is quoted with the run its open end admits, and a
/// fully anchored expression is quoted without the pair the model wrote around it.
#[tokio::test]
async fn a_pattern_clause_quotes_the_expression_the_whole_value_must_satisfy() {
    let rows = vec![
        PatternCase {
            case: "the service execution role a create names",
            operation: "CreateApplication",
            body: json!({
                "ApplicationName": "pattern-probe",
                "RuntimeEnvironment": RUNTIME,
                "ServiceExecutionRole": "kda-service-role",
            }),
            path: "serviceExecutionRole",
            submitted: "kda-service-role",
            expression: "arn:.*",
        },
        PatternCase {
            case: "the Glue database a Studio notebook's catalog names",
            operation: "CreateApplication",
            body: json!({
                "ApplicationName": "pattern-probe",
                "RuntimeEnvironment": "ZEPPELIN-FLINK-3_0",
                "ServiceExecutionRole": ROLE,
                "ApplicationConfiguration": {
                    "ZeppelinApplicationConfiguration": {
                        "CatalogConfiguration": {
                            "GlueDataCatalogConfiguration": {"DatabaseARN": "analytics_db"}
                        }
                    }
                },
            }),
            path: "applicationConfiguration.zeppelinApplicationConfiguration.\
                   catalogConfiguration.glueDataCatalogConfiguration.databaseARN",
            submitted: "analytics_db",
            expression: "arn:.*",
        },
        PatternCase {
            case: "the base path a notebook's deployment location names",
            operation: "CreateApplication",
            body: json!({
                "ApplicationName": "pattern-probe",
                "RuntimeEnvironment": "ZEPPELIN-FLINK-3_0",
                "ServiceExecutionRole": ROLE,
                "ApplicationConfiguration": {
                    "ZeppelinApplicationConfiguration": {
                        "DeployAsApplicationConfiguration": {
                            "S3ContentLocation": {
                                "BucketARN": "arn:aws:s3:::notebook-artifacts",
                                "BasePath": "base path"
                            }
                        }
                    }
                },
            }),
            path: "applicationConfiguration.zeppelinApplicationConfiguration.\
                   deployAsApplicationConfiguration.s3ContentLocation.basePath",
            submitted: "base path",
            expression: "[a-zA-Z0-9/!-_.*'()]+",
        },
        PatternCase {
            case: "the name a create gives its application",
            operation: "CreateApplication",
            body: json!({
                "ApplicationName": "pattern probe",
                "RuntimeEnvironment": RUNTIME,
                "ServiceExecutionRole": ROLE,
            }),
            path: "applicationName",
            submitted: "pattern probe",
            expression: "[a-zA-Z0-9_.-]+",
        },
        PatternCase {
            case: "the conditional token an update carries",
            operation: "UpdateApplication",
            body: json!({"ApplicationName": APPLICATION, "ConditionalToken": "token one"}),
            path: "conditionalToken",
            submitted: "token one",
            expression: "[a-zA-Z0-9-_+/=]+",
        },
        PatternCase {
            case: "the maintenance window start a maintenance update names",
            operation: "UpdateApplicationMaintenanceConfiguration",
            body: json!({
                "ApplicationName": APPLICATION,
                "ApplicationMaintenanceConfigurationUpdate": {
                    "ApplicationMaintenanceWindowStartTimeUpdate": "25:00"
                },
            }),
            path: "applicationMaintenanceConfigurationUpdate.\
                   applicationMaintenanceWindowStartTimeUpdate",
            submitted: "25:00",
            expression: "([01][0-9]|2[0-3]):[0-5][0-9]",
        },
        PatternCase {
            case: "the name a snapshot is taken under",
            operation: "CreateApplicationSnapshot",
            body: json!({"ApplicationName": APPLICATION, "SnapshotName": "snapshot one"}),
            path: "snapshotName",
            submitted: "snapshot one",
            expression: "[a-zA-Z0-9_.-]+",
        },
        PatternCase {
            case: "the log stream a logging option names",
            operation: "AddApplicationCloudWatchLoggingOption",
            body: json!({
                "ApplicationName": APPLICATION,
                "CloudWatchLoggingOption": {"LogStreamARN": "kda-log-stream"},
            }),
            path: "cloudWatchLoggingOption.logStreamARN",
            submitted: "kda-log-stream",
            expression: "arn:.*",
        },
        PatternCase {
            case: "the resource a tag read names",
            operation: "ListTagsForResource",
            body: json!({"ResourceARN": "application/pattern-clause-probe"}),
            path: "resourceARN",
            submitted: "application/pattern-clause-probe",
            expression: "arn:.*",
        },
    ];

    let router = router();
    let (status, created) = call(
        &router,
        "CreateApplication",
        &create_body(APPLICATION, RUNTIME),
    )
    .await;
    assert_eq!(status, 200, "CreateApplication got {status} {created}");

    for PatternCase {
        case,
        operation,
        body,
        path,
        submitted,
        expression,
    } in rows
    {
        let (status, refused) = call(&router, operation, &body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} {refused}"
        );
        assert_eq!(
            error_code(&refused),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {refused}"
        );
        let expected = format!(
            "1 validation error detected: Value '{submitted}' at '{path}' failed to satisfy \
             constraint: Member must satisfy regular expression pattern: {expression}"
        );
        let actual = error_message(&refused);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );
    }
}

/// The clause quotes the service's expression and never the model's trait source: no message
/// carries the whole-value anchors the model writes around it.
#[tokio::test]
async fn no_pattern_clause_quotes_the_model_trait_source() {
    // (operation, a request whose only fault is a value one `@pattern` does not admit, the
    // anchored expression the model declares for that member)
    let rows: [(&str, Value, &str); 3] = [
        (
            "CreateApplication",
            json!({
                "ApplicationName": "anchored probe",
                "RuntimeEnvironment": RUNTIME,
                "ServiceExecutionRole": ROLE,
            }),
            "^[a-zA-Z0-9_.-]+$",
        ),
        (
            "CreateApplication",
            json!({
                "ApplicationName": "anchored-probe",
                "RuntimeEnvironment": RUNTIME,
                "ServiceExecutionRole": "kda-service-role",
            }),
            "^arn:",
        ),
        (
            "CreateApplicationSnapshot",
            json!({"ApplicationName": APPLICATION, "SnapshotName": "snapshot one"}),
            "^[a-zA-Z0-9_.-]+$",
        ),
    ];

    let router = router();
    for (operation, body, declared) in rows {
        let (status, refused) = call(&router, operation, &body).await;
        let actual = error_message(&refused);
        assert_eq!(
            status, 400,
            "case {declared:?} on {operation}: expected HTTP 400, got {status} {refused}"
        );
        assert!(
            actual.contains("regular expression pattern: ") && !actual.contains(declared),
            "case {declared:?} on {operation}: expected a pattern clause quoting no anchored \
             trait source, got {actual:?}"
        );
    }
}
