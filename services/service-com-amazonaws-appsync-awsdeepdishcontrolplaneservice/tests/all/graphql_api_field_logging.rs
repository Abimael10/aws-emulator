// SPDX-License-Identifier: CC0-1.0

//! What an API with a `logConfig` LOGS about the requests its GraphQL endpoint served, and
//! the request id that joins a client's answer to those records.
//!
//! The rule these cases assert, one level above the recorded case (one API created with
//! `fieldLogLevel` `ALL`, one query, and the tracing records for the request id the answer
//! carried):
//!
//! > Every answer a GraphQL API's endpoint writes carries the id of the request it answers in
//! > `x-amzn-RequestId`, and an API carrying a `logConfig` writes what it logs about that
//! > request into the log group of that API, `/aws/appsync/apis/<apiId>`, with every record
//! > stamped with that same id. WHICH records are written is what `fieldLogLevel` states: the
//! > request-level records of the request as a whole whenever logging is configured, and
//! > field-level records — "field-level tracing information" and "the generated
//! > request/response functions that got resolved" — for EVERY field at `ALL`, for only "the
//! > fields that are in error" at `ERROR`, and for none at `NONE`.
//! > `excludeVerboseContent` excludes the sections carrying the context and the evaluated
//! > templates "regardless of logging level". An API carrying no `logConfig` logs nothing,
//! > and the account then holds no such group at all.
//!
//! Every assertion reads the outcome back through CloudWatch Logs' own reader
//! (`FilterLogEvents`), so a record that did not reach that store cannot pass; the rows differ
//! along what the rule generalises over: the LEVEL, whether the field resolved or failed,
//! whether verbose content is excluded, and whether the API configures logging at all.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::graphql_api_mapping_template_resolvers::{
    create_api, emulator, post_graphql_answer, GraphqlApi, REGION,
};

/// The response field every answer carries the request's own id in; the service's own copy is
/// `service::graphql_api::query::REQUEST_ID_FIELD`.
const REQUEST_ID_FIELD: &str = "x-amzn-requestid";

/// The `logType` each kind of record carries; the service's own copies are in
/// `service::graphql_api::field_logs`.
const REQUEST_SUMMARY: &str = "RequestSummary";
const EXECUTION_SUMMARY: &str = "ExecutionSummary";
const TRACING: &str = "Tracing";
const REQUEST_MAPPING: &str = "RequestMapping";
const RESPONSE_MAPPING: &str = "ResponseMapping";

/// The role every `logConfig` below names: the service role AppSync assumes to publish to
/// CloudWatch Logs (`LogConfig$cloudWatchLogsRoleArn`).
fn logs_role() -> String {
    format!("arn:aws:iam::000000000000:role/appsync-{REGION}-logs")
}

/// The `logConfig` of an API logging at `level`, excluding verbose content or not.
fn log_config(level: &str, excludes_verbose_content: bool) -> Value {
    json!({
        "fieldLogLevel": level,
        "cloudWatchLogsRoleArn": logs_role(),
        "excludeVerboseContent": excludes_verbose_content,
    })
}

#[tokio::test]
async fn every_answer_carries_the_request_id_its_records_are_stamped_with() {
    let router = emulator();
    let api = create_api(&router, "request-id", Some(log_config("ALL", false))).await;

    /// One row: what a client sends, and the status the endpoint answers it with.
    struct Row<'a> {
        case: &'a str,
        body: Value,
        fields: &'a [(&'a str, &'a str)],
        status: u16,
    }
    let rows: &[Row] = &[
        Row {
            case: "a query the API resolves",
            body: json!({"query": "query { probe(msg: \"hello\") }"}),
            fields: &[("x-api-key", "")],
            status: 200,
        },
        Row {
            case: "a query whose field fails",
            body: json!({"query": "query { refused }"}),
            fields: &[("x-api-key", "")],
            status: 200,
        },
        Row {
            case: "a request presenting no material at all",
            body: json!({"query": "query { probe(msg: \"hello\") }"}),
            fields: &[],
            status: 401,
        },
        Row {
            case: "a body that is no GraphQL request",
            body: json!({"notAQuery": true}),
            fields: &[("x-api-key", "")],
            status: 400,
        },
    ];
    let mut seen: Vec<String> = Vec::new();
    for row in rows {
        let case = row.case;
        // The key is filled in per row, because the row table cannot name a value the API
        // only minted at creation.
        let fields: Vec<(&str, &str)> = row
            .fields
            .iter()
            .map(|(field, value)| {
                if *field == "x-api-key" && value.is_empty() {
                    (*field, api.api_key.as_str())
                } else {
                    (*field, *value)
                }
            })
            .collect();
        let (status, answered, document) =
            post_graphql_answer(case, &api, &row.body, &fields).await;
        assert_eq!(
            status, row.status,
            "case {case:?}: expected the status {}, got {status} and {document}",
            row.status
        );
        let request_id = answered
            .iter()
            .find(|(field, _)| field == REQUEST_ID_FIELD)
            .map(|(_, value)| value.clone())
            .unwrap_or_else(|| {
                panic!("case {case:?}: every answer carries {REQUEST_ID_FIELD}, got {answered:?}")
            });
        // The id has the form AppSync identifies a request by — the canonical 8-4-4-4-12
        // UUID text form — and no two requests share one.
        let groups: Vec<usize> = request_id.split('-').map(str::len).collect();
        assert_eq!(
            groups,
            vec![8, 4, 4, 4, 12],
            "case {case:?}: expected a UUID, got {request_id:?}"
        );
        assert!(
            !seen.contains(&request_id),
            "case {case:?}: expected an id no earlier request was answered with, got \
             {request_id:?} among {seen:?}"
        );
        seen.push(request_id);
    }

    // Every one of those requests is reported in the API's own log group, each record under
    // the id its answer carried.
    let records = delivered(&router, &api).await;
    for request_id in &seen {
        let stamped: Vec<&Value> = records
            .iter()
            .filter(|record| record["requestId"].as_str() == Some(request_id.as_str()))
            .collect();
        assert!(
            !stamped.is_empty(),
            "expected the records of the request {request_id:?}, got {} records in all",
            records.len()
        );
        assert!(
            stamped
                .iter()
                .any(|record| record["logType"].as_str() == Some(REQUEST_SUMMARY)),
            "expected a {REQUEST_SUMMARY} record for {request_id:?}, got {stamped:?}"
        );
    }
}

#[tokio::test]
async fn the_field_log_level_decides_which_fields_are_reported() {
    /// One row: the level an API logs at, and whether a resolved and a failed field are
    /// reported at that level. Source: `LogConfig$fieldLogLevel`'s own documentation and
    /// `FieldLogLevel`'s values (`smithy_query(query="shape",shape="FieldLogLevel")`).
    struct Row<'a> {
        case: &'a str,
        level: Option<&'a str>,
        resolved_field_reported: bool,
        failed_field_reported: bool,
    }
    let rows: &[Row] = &[
        Row {
            case: "ALL, which reports every field",
            level: Some("ALL"),
            resolved_field_reported: true,
            failed_field_reported: true,
        },
        Row {
            case: "ERROR, which reports only the fields in error",
            level: Some("ERROR"),
            resolved_field_reported: false,
            failed_field_reported: true,
        },
        Row {
            case: "NONE, which reports no field",
            level: Some("NONE"),
            resolved_field_reported: false,
            failed_field_reported: false,
        },
        Row {
            case: "DEBUG, a level above ERROR, which reports every field",
            level: Some("DEBUG"),
            resolved_field_reported: true,
            failed_field_reported: true,
        },
        Row {
            case: "an API that configures no logging at all",
            level: None,
            resolved_field_reported: false,
            failed_field_reported: false,
        },
    ];
    for row in rows {
        let case = row.case;
        let router = emulator();
        let api = create_api(
            &router,
            &format!("level-{}", row.level.unwrap_or("absent")).to_ascii_lowercase(),
            row.level.map(|level| log_config(level, false)),
        )
        .await;
        for field in ["probe(msg: \"hello\")", "refused"] {
            let (status, _, document) = post_graphql_answer(
                case,
                &api,
                &json!({"query": format!("query {{ {field} }}")}),
                &[("x-api-key", api.api_key.as_str())],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the query is answered whatever the API logs, got {document}"
            );
        }
        let records = delivered(&router, &api).await;
        if row.level.is_none() {
            assert!(
                records.is_empty(),
                "case {case:?}: an API with no logConfig writes nothing, got {records:?}"
            );
            continue;
        }
        // The request-level records are of the request as a whole, so they are written at
        // every level — including the one that reports no field.
        for log_type in [REQUEST_SUMMARY, EXECUTION_SUMMARY] {
            assert!(
                records
                    .iter()
                    .any(|record| record["logType"].as_str() == Some(log_type)),
                "case {case:?}: expected a {log_type} record, got {records:?}"
            );
        }
        let reported = |field: &str| {
            records.iter().any(|record| {
                record["fieldName"].as_str() == Some(field)
                    && matches!(
                        record["logType"].as_str(),
                        Some(TRACING) | Some(REQUEST_MAPPING) | Some(RESPONSE_MAPPING)
                    )
            })
        };
        assert_eq!(
            reported("probe"),
            row.resolved_field_reported,
            "case {case:?}: expected a resolved field reported={}, got {records:?}",
            row.resolved_field_reported
        );
        assert_eq!(
            reported("refused"),
            row.failed_field_reported,
            "case {case:?}: expected a failed field reported={}, got {records:?}",
            row.failed_field_reported
        );
        // Whatever the level, a record of a field that failed carries the error, and one of
        // a field that resolved does not.
        for record in &records {
            let in_error = record["fieldInError"].as_bool();
            if record["fieldName"].as_str() == Some("refused") && in_error == Some(true) {
                assert!(
                    record["errors"]
                        .as_array()
                        .is_some_and(|errors| !errors.is_empty()),
                    "case {case:?}: a failed field's record carries the error, got {record}"
                );
            }
        }
    }
}

#[tokio::test]
async fn excluding_verbose_content_leaves_the_context_and_the_templates_out() {
    for (case, excludes_verbose_content) in [
        ("an API that carries verbose content", false),
        ("an API that excludes it", true),
    ] {
        let router = emulator();
        let api = create_api(
            &router,
            &format!("verbose-{excludes_verbose_content}"),
            Some(log_config("ALL", excludes_verbose_content)),
        )
        .await;
        let (status, _, document) = post_graphql_answer(
            case,
            &api,
            &json!({"query": "query { probe(msg: \"hello\") }"}),
            &[("x-api-key", api.api_key.as_str())],
        )
        .await;
        assert_eq!(status, 200, "case {case:?}: the query is answered, got {document}");
        let records = delivered(&router, &api).await;
        let generated: Vec<&Value> = records
            .iter()
            .filter(|record| record["logType"].as_str() == Some(REQUEST_MAPPING))
            .collect();
        assert!(
            !generated.is_empty(),
            "case {case:?}: ALL reports the generated request of every field, got {records:?}"
        );
        for record in generated {
            let carries_context = !record["context"].is_null();
            let carries_template = !record["transformedTemplate"].is_null();
            assert_eq!(
                (carries_context, carries_template),
                (!excludes_verbose_content, !excludes_verbose_content),
                "case {case:?}: expected the verbose sections to be \
                 carried={}, got {record}",
                !excludes_verbose_content
            );
        }
    }
}

#[tokio::test]
async fn an_api_that_configures_logging_has_its_log_group() {
    for (case, level, expected) in [
        ("an API logging every field", Some("ALL"), true),
        ("an API logging the fields in error", Some("ERROR"), true),
        ("an API logging no field", Some("NONE"), true),
        ("an API that configures no logging", None, false),
    ] {
        let router = emulator();
        let api = create_api(
            &router,
            &format!("group-{}", level.unwrap_or("absent")).to_ascii_lowercase(),
            level.map(|level| log_config(level, false)),
        )
        .await;
        // The group is there BEFORE any request is served, because configuring logging is
        // what asks for it.
        let (status, described) = logs(
            &router,
            "DescribeLogGroups",
            json!({"logGroupNamePrefix": format!("/aws/appsync/apis/{}", api.api_id)}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeLogGroups must answer 200, got {described}"
        );
        let held: Vec<&str> = described["logGroups"]
            .as_array()
            .map(|groups| {
                groups
                    .iter()
                    .filter_map(|group| group["logGroupName"].as_str())
                    .collect()
            })
            .unwrap_or_default();
        let group = format!("/aws/appsync/apis/{}", api.api_id);
        assert_eq!(
            held.contains(&group.as_str()),
            expected,
            "case {case:?}: expected the group to be held={expected}, got {held:?}"
        );
    }
}

#[tokio::test]
async fn what_one_request_logs_is_bounded_however_much_it_selects() {
    let router = emulator();
    let api = create_api(&router, "bounded", Some(log_config("ALL", false))).await;
    let case = "a request selecting many fields, each carrying a large argument";

    // One document selecting the same field under as many aliases as one selection set
    // carries, each with a multi-kilobyte argument: what the API logs about it is what this
    // asserts is bounded, not what it answers.
    let argument = "a".repeat(1024);
    let aliases = 64;
    let selections: String = (0..aliases)
        .map(|at| format!("probe{at}: probe(msg: \"{argument}\") "))
        .collect();
    let (status, _, document) = post_graphql_answer(
        case,
        &api,
        &json!({"query": format!("query {{ {selections} }}")}),
        &[("x-api-key", api.api_key.as_str())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the request is answered, got {status} and {document}"
    );
    assert_eq!(
        document["data"].as_object().map(serde_json::Map::len),
        Some(aliases),
        "case {case:?}: every selected field is answered, got {document}"
    );

    // And the API still logged the request as a whole, within a bounded number of bytes: a
    // request cannot choose how much one API writes about it.
    let records = delivered(&router, &api).await;
    assert!(
        records
            .iter()
            .any(|record| record["logType"].as_str() == Some(REQUEST_SUMMARY)),
        "case {case:?}: the request-level record is written whatever the rest comes to, got \
         {} records",
        records.len()
    );
    let written: usize = records
        .iter()
        .map(|record| record.to_string().len())
        .sum();
    assert!(
        written <= DELIVERY_CEILING,
        "case {case:?}: expected at most {DELIVERY_CEILING} bytes of records, got {written} \
         over {} records",
        records.len()
    );
}

/// The ceiling the case above holds the API's own records to: generously above the bound the
/// service states for one request's delivery
/// (`service::graphql_api::field_logs::MAX_DELIVERED_BYTES`), so the case fails on a
/// collection that grows with the request rather than on the exact figure.
const DELIVERY_CEILING: usize = 2 * 1024 * 1024;

/// Every record the API's own log group holds, as CloudWatch Logs' own reader reports them,
/// each parsed as the document it is — and nothing at all when the account holds no such
/// group, which is what "nothing was delivered and nothing was created" looks like.
async fn delivered(router: &MultiServiceRouter, api: &GraphqlApi) -> Vec<Value> {
    let group = format!("/aws/appsync/apis/{}", api.api_id);
    let (status, answered) = logs(
        router,
        "FilterLogEvents",
        json!({"logGroupName": group, "limit": 1_000}),
    )
    .await;
    if status != 200 {
        assert_eq!(
            answered["__type"]
                .as_str()
                .unwrap_or_default()
                .rsplit('#')
                .next(),
            Some("ResourceNotFoundException"),
            "reading the API's log group must either answer its events or report that \
             CloudWatch Logs holds no such group, got {status} and {answered}"
        );
        return Vec::new();
    }
    answered["events"]
        .as_array()
        .map(|events| {
            events
                .iter()
                .filter_map(|event| event["message"].as_str())
                .map(|message| serde_json::from_str::<Value>(message).unwrap_or(Value::Null))
                .collect()
        })
        .unwrap_or_default()
}

/// One CloudWatch Logs call over its `awsJson1_1` wire.
async fn logs(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://logs.{REGION}.amazonaws.com/"))
        .header("host", format!("logs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("Logs_20140328.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIAEMULATOR/20260101/{REGION}/logs/aws4_request, \
                 SignedHeaders=host, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(body.to_string().into_bytes()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}
