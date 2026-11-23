// SPDX-License-Identifier: CC0-1.0

//! What an API capturing FIELD-level logs reports about the request its published GraphQL
//! endpoint answered: the request-level summary of the answer, and the tracing record of each
//! field the execution resolved.
//!
//! The rule these cases assert, one level above the recorded case (one API created with
//! `fieldLogLevel` `ALL`, one `APPSYNC_JS` unit resolver over a `NONE` data source, one query,
//! and the tracing record of the one field it selected):
//!
//! > Every answer a GraphQL API's endpoint writes carries the id of the request it answers, and
//! > an API whose `logConfig` captures field-level logs reports, in its own log group, exactly
//! > one request-level summary of that request — carrying the API's id, the status the CLIENT
//! > observed and how long answering it took — and exactly one tracing record per field the
//! > execution resolved, stamped with the same id. A field's tracing record says where the
//! > field sits: the type that declares it, the field's own name, the type the API's schema
//! > declares it resolves to, and its RESPONSE path (the alias where the selection carries one,
//! > and the index of each element of a list), together with the ARN of the resolver that
//! > resolved it exactly as `CreateResolver` reported that ARN, or no ARN at all for a field
//! > the API attaches no resolver to; and it states the offset the field began resolving at and
//! > how long it took, both as nanosecond counts. An API capturing no field-level logs
//! > (`fieldLogLevel` `NONE`) reports its requests and traces no field of them.
//!
//! Every assertion reads the records back through CloudWatch Logs' own reader
//! (`FilterLogEvents`), so a record that did not reach that store cannot pass. The rows differ
//! along what the rule generalises over: the type a field resolves to (a scalar, an id, an
//! object type), the form of its response path (a plain key, an alias, a nested key), whether
//! the field carries a resolver of its own, the status the client observed, and the level the
//! API captures at.
//!
//! What `fieldLogLevel` and `excludeVerboseContent` SELECT is pinned next door
//! ([`crate::graphql_api_field_logging`]); this suite reads the CONTENT of the records a
//! captured field produces, and the ALL/NONE pair here is two APIs of ONE emulator, which is
//! what says one API's records never answer for the other's.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_api::LOOPBACK_HOST;
use crate::event_api_data_plane::{appsync, loopback_port};
use crate::graphql_api_mapping_template_resolvers::{
    emulator, install_schema, try_post_answer, REGION,
};

/// The response field every answer carries the request's own id in; the service's own copy is
/// `service::graphql_api::query::REQUEST_ID_FIELD`.
const REQUEST_ID_FIELD: &str = "x-amzn-requestid";

/// The `logType` of the records these cases read; the service's own copies are in
/// `service::graphql_api::field_logs`.
const REQUEST_SUMMARY: &str = "RequestSummary";
const TRACING: &str = "Tracing";

/// The `runtime` every resolver of this suite is defined by. Source: `AppSyncRuntime`'s own
/// members (`smithy_query(query="shape",shape="AppSyncRuntime,RuntimeName")`).
const RUNTIME: (&str, &str) = ("APPSYNC_JS", "1.0.0");

/// The data source every resolver here resolves against: `NONE` is "there is no data source",
/// so the resolver's own code decides the field and nothing else does.
const DATA_SOURCE: &str = "local";

/// The schema every API of this suite is created with: one field per type a row traces.
const SCHEMA: &str = "schema { query: Query }\n\
                      \n\
                      type Query {\n  probe(msg: String!): String\n  counted: Int\n  \
                      flagged: Boolean\n  noted: Note\n}\n\
                      \n\
                      type Note {\n  id: ID\n  text: String\n}\n";

/// The value the `noted` field's resolver ends its step with, whose members are fields of
/// `Note` the API attaches no resolver to.
const NOTE_ID: &str = "note-1";
const NOTE_TEXT: &str = "the note";

/// How long a case waits for the records of a request to be readable, and how often it looks
/// again: the wait is on the CONDITION, so nothing here depends on how fast the host runs.
const RECORDS_DEADLINE: Duration = Duration::from_secs(60);
const POLL_SPACING: Duration = Duration::from_millis(50);

/// One CloudWatch Logs call over its `awsJson1_1` wire: the reader this suite grades every
/// record through, so a record that did not reach that store cannot pass.
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
    let parsed = match bytes.is_empty() {
        true => Value::Null,
        false => serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    };
    (status, parsed)
}

/// One created GraphQL API: its id, the address its GraphQL endpoint is published on, a live
/// API key, and the ARN `CreateResolver` reported for each field's resolver.
struct TracedApi {
    api_id: String,
    graphql: String,
    api_key: String,
    resolver_arns: BTreeMap<String, String>,
}

#[tokio::test]
async fn each_field_the_request_resolved_is_traced_where_it_sits() {
    let case = "a request selecting a field of each type the schema declares";
    let router = emulator();
    let api = create_api(&router, "traced", "ALL").await;
    let marker = "the probe marker";

    let (status, answered, document) = post(
        case,
        &api,
        &json!({
            "query": "query P($m: String!) { probe(msg: $m) echoed: probe(msg: $m) counted \
                      flagged noted { id text } }",
            "variables": {"m": marker},
        }),
        &[("x-api-key", api.api_key.as_str())],
    )
    .await;
    assert_eq!(
        (status, &document["data"]),
        (
            200,
            &json!({
                "probe": marker,
                "echoed": marker,
                "counted": 7,
                "flagged": true,
                "noted": {"id": NOTE_ID, "text": NOTE_TEXT},
            })
        ),
        "case {case:?}: the endpoint must resolve every selected field, got {status} and \
         {document}"
    );
    let request_id = request_id(case, &answered);

    /// One row: a response path the request selected, and what the field at that path is.
    /// Source: [`SCHEMA`], whose declarations name each field's parent type and the type it
    /// resolves to.
    struct Row<'a> {
        case: &'a str,
        path: Value,
        parent_type: &'a str,
        field_name: &'a str,
        return_type: &'a str,
        /// The field whose resolver's ARN the record must carry, or `None` for a field the
        /// API attaches no resolver to.
        resolved_by: Option<&'a str>,
    }
    let rows: &[Row] = &[
        Row {
            case: "a scalar field selected under its own name",
            path: json!(["probe"]),
            parent_type: "Query",
            field_name: "probe",
            return_type: "String",
            resolved_by: Some("probe"),
        },
        Row {
            case: "the same field selected under an alias, whose response path is the alias",
            path: json!(["echoed"]),
            parent_type: "Query",
            field_name: "probe",
            return_type: "String",
            resolved_by: Some("probe"),
        },
        Row {
            case: "a field resolving to a number",
            path: json!(["counted"]),
            parent_type: "Query",
            field_name: "counted",
            return_type: "Int",
            resolved_by: Some("counted"),
        },
        Row {
            case: "a field resolving to a boolean",
            path: json!(["flagged"]),
            parent_type: "Query",
            field_name: "flagged",
            return_type: "Boolean",
            resolved_by: Some("flagged"),
        },
        Row {
            case: "a field resolving to an object type",
            path: json!(["noted"]),
            parent_type: "Query",
            field_name: "noted",
            return_type: "Note",
            resolved_by: Some("noted"),
        },
        Row {
            case: "a nested id field the api attaches no resolver to",
            path: json!(["noted", "id"]),
            parent_type: "Note",
            field_name: "id",
            return_type: "ID",
            resolved_by: None,
        },
        Row {
            case: "a nested scalar field the api attaches no resolver to",
            path: json!(["noted", "text"]),
            parent_type: "Note",
            field_name: "text",
            return_type: "String",
            resolved_by: None,
        },
    ];

    // The wait is for what this case GRADES to be readable: one tracing record per row, beside
    // the request-level pair the API writes whatever its fields come to.
    let records = records_of(&router, &api.api_id, &request_id, rows.len() + 2).await;
    for row in rows {
        let case = row.case;
        let traced: Vec<&Value> = records
            .iter()
            .filter(|record| {
                record["logType"].as_str() == Some(TRACING) && record["path"] == row.path
            })
            .collect();
        assert_eq!(
            traced.len(),
            1,
            "case {case:?}: expected exactly one {TRACING} record at the path {}, got {traced:?}",
            row.path
        );
        let record = traced[0];
        assert_eq!(
            (
                record["graphQLAPIId"].as_str(),
                record["parentType"].as_str(),
                record["fieldName"].as_str(),
                record["returnType"].as_str(),
            ),
            (
                Some(api.api_id.as_str()),
                Some(row.parent_type),
                Some(row.field_name),
                Some(row.return_type),
            ),
            "case {case:?}: expected the field the schema declares at that path, got {record}"
        );
        // The ARN is the one `CreateResolver` reported, byte for byte: that is what joins a
        // traced field to the resolver a client created for it.
        let expected = row
            .resolved_by
            .map(|field| api.resolver_arns[field].as_str());
        assert_eq!(
            record["resolverArn"].as_str(),
            expected,
            "case {case:?}: expected the resolver ARN {expected:?}, got {record}"
        );
        // The offset and the span are nanosecond COUNTS, so a reader can place the field
        // inside the request's own execution; a field a resolver ran for took time to resolve.
        let start_offset = record["startOffset"].as_u64();
        let duration = record["duration"].as_u64();
        assert!(
            start_offset.is_some() && duration.is_some(),
            "case {case:?}: expected a nanosecond offset and duration, got {record}"
        );
        if row.resolved_by.is_some() {
            assert!(
                duration.unwrap_or_default() > 0,
                "case {case:?}: expected the span resolving the field took, got {record}"
            );
        }
    }

    // Nothing beyond those fields is traced: one field of the request, one record.
    let traced = records
        .iter()
        .filter(|record| record["logType"].as_str() == Some(TRACING))
        .count();
    assert_eq!(
        traced,
        rows.len(),
        "case {case:?}: expected one {TRACING} record per resolved field, got {traced} over \
         {} fields",
        rows.len()
    );
}

#[tokio::test]
async fn the_request_summary_states_the_status_the_client_observed() {
    let router = emulator();
    let api = create_api(&router, "summarised", "ALL").await;
    let document = json!({
        "query": "query P($m: String!) { probe(msg: $m) }",
        "variables": {"m": "summary"},
    });

    /// One row: what a client presents, and the status the endpoint answers it with.
    struct Row<'a> {
        case: &'a str,
        body: Value,
        keyed: bool,
        status: u16,
    }
    let rows: &[Row] = &[
        Row {
            case: "a request the api resolves",
            body: document.clone(),
            keyed: true,
            status: 200,
        },
        Row {
            case: "a request presenting no material",
            body: document.clone(),
            keyed: false,
            status: 401,
        },
        Row {
            case: "a body carrying no graphql request",
            body: json!({"notAQuery": true}),
            keyed: true,
            status: 400,
        },
    ];
    for row in rows {
        let case = row.case;
        let fields: Vec<(&str, &str)> = match row.keyed {
            true => vec![("x-api-key", api.api_key.as_str())],
            false => Vec::new(),
        };
        let (status, answered, document) = post(case, &api, &row.body, &fields).await;
        assert_eq!(
            status, row.status,
            "case {case:?}: expected the status {}, got {status} and {document}",
            row.status
        );
        let request_id = request_id(case, &answered);
        // The request-level pair the API writes about every request it served, whatever came of
        // executing it.
        let records = records_of(&router, &api.api_id, &request_id, 2).await;
        let summaries: Vec<&Value> = records
            .iter()
            .filter(|record| record["logType"].as_str() == Some(REQUEST_SUMMARY))
            .collect();
        assert_eq!(
            summaries.len(),
            1,
            "case {case:?}: expected exactly one {REQUEST_SUMMARY} record of the request, got \
             {summaries:?}"
        );
        let summary = summaries[0];
        assert_eq!(
            (
                summary["graphQLAPIId"].as_str(),
                summary["statusCode"].as_u64(),
            ),
            (Some(api.api_id.as_str()), Some(u64::from(row.status))),
            "case {case:?}: expected the api's id and the status the client observed, got \
             {summary}"
        );
        assert!(
            summary["latency"].as_u64().unwrap_or_default() > 0,
            "case {case:?}: expected the span answering the request took, got {summary}"
        );
    }
}

#[tokio::test]
async fn only_an_api_capturing_field_logs_traces_the_fields_of_its_requests() {
    // Twin APIs on one emulator, differing only in the level they capture at: each reports
    // the requests it served in its OWN group, and only the one capturing field-level logs
    // traces the field it resolved.
    let router = emulator();
    for (case, level, traced) in [
        ("the api capturing every field", "ALL", true),
        ("the twin api capturing no field", "NONE", false),
    ] {
        let api = create_api(&router, &level.to_ascii_lowercase(), level).await;
        let (status, answered, document) = post(
            case,
            &api,
            &json!({
                "query": "query P($m: String!) { probe(msg: $m) }",
                "variables": {"m": "twin"},
            }),
            &[("x-api-key", api.api_key.as_str())],
        )
        .await;
        assert_eq!(
            (status, document["data"]["probe"].as_str()),
            (200, Some("twin")),
            "case {case:?}: the request is answered whatever the api captures, got {status} \
             and {document}"
        );
        let request_id = request_id(case, &answered);
        // Both twins write the request-level pair; the ALL twin writes the field's trace on top
        // of it, which is what the row below grades.
        let expected = match traced {
            true => 3,
            false => 2,
        };
        let records = records_of(&router, &api.api_id, &request_id, expected).await;
        assert!(
            records
                .iter()
                .any(|record| record["logType"].as_str() == Some(REQUEST_SUMMARY)),
            "case {case:?}: the request itself is reported at every level, got {records:?}"
        );
        let found = records
            .iter()
            .any(|record| record["logType"].as_str() == Some(TRACING));
        assert_eq!(
            found, traced,
            "case {case:?}: expected the fields of the request to be traced={traced}, got \
             {records:?}"
        );
    }
}

/// The id the answer `fields` carry the request under, which every record of that request is
/// stamped with.
fn request_id(case: &str, fields: &[(String, String)]) -> String {
    fields
        .iter()
        .find(|(field, _)| field == REQUEST_ID_FIELD)
        .map(|(_, value)| value.clone())
        .unwrap_or_else(|| {
            panic!("case {case:?}: every answer carries {REQUEST_ID_FIELD}, got {fields:?}")
        })
}

/// Every record the API's own log group holds for `request_id`, read through CloudWatch Logs'
/// own reader, once at least `at_least` of them are there.
///
/// The wait is on the CONDITION — the records being readable — with a deadline far past any
/// delivery, so a case fails on records that never arrive rather than on how loaded the host
/// is.
async fn records_of(
    router: &MultiServiceRouter,
    api_id: &str,
    request_id: &str,
    at_least: usize,
) -> Vec<Value> {
    let group = format!("/aws/appsync/apis/{api_id}");
    let deadline = tokio::time::Instant::now() + RECORDS_DEADLINE;
    loop {
        let (status, answered) = logs(
            router,
            "FilterLogEvents",
            json!({"logGroupName": group, "limit": 1_000}),
        )
        .await;
        let stamped: Vec<Value> = match status {
            200 => answered["events"]
                .as_array()
                .map(|events| {
                    events
                        .iter()
                        .filter_map(|event| event["message"].as_str())
                        .filter_map(|message| serde_json::from_str::<Value>(message).ok())
                        .filter(|record| record["requestId"].as_str() == Some(request_id))
                        .collect()
                })
                .unwrap_or_default(),
            // Until the API's FIRST record lands, CloudWatch Logs holds no group of its own,
            // which is the one refusal a read of it may answer; anything else is a fault to
            // fail on rather than to keep polling through.
            _ => {
                assert_eq!(
                    answered["__type"]
                        .as_str()
                        .unwrap_or_default()
                        .rsplit('#')
                        .next(),
                    Some("ResourceNotFoundException"),
                    "reading {group} must either answer its events or report that CloudWatch \
                     Logs holds no such group, got {status} and {answered}"
                );
                Vec::new()
            }
        };
        if stamped.len() >= at_least {
            return stamped;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the records of the request {request_id:?} must be readable in {group}, got \
             {status} and {answered}"
        );
        tokio::time::sleep(POLL_SPACING).await;
    }
}

/// POST one GraphQL request to the address the API PUBLISHED, parsed as a client parses it.
async fn post(
    case: &str,
    api: &TracedApi,
    body: &Value,
    fields: &[(&str, &str)],
) -> (u16, Vec<(String, String)>, Value) {
    let (authority, path) = split_published(case, &api.graphql);
    try_post_answer(case, &authority, &path, "POST", body, fields)
        .await
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the published GraphQL endpoint {:?} must answer this request",
                api.graphql
            )
        })
}

/// The authority and the path a published `uris` entry carries.
fn split_published(case: &str, uri: &str) -> (String, String) {
    let rest = uri
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or_else(|| panic!("case {case:?}: expected a published uri, got {uri:?}"));
    let (authority, path) = rest
        .split_once('/')
        .unwrap_or_else(|| panic!("case {case:?}: expected a path, got {uri:?}"));
    (authority.to_string(), format!("/{path}"))
}

/// Create one GraphQL API capturing at `field_log_level`, with an API key, [`SCHEMA`], the
/// `NONE` data source and one `APPSYNC_JS` resolver per field the rows select, and read the
/// address it published for its GraphQL endpoint.
async fn create_api(router: &MultiServiceRouter, name: &str, field_log_level: &str) -> TracedApi {
    let request = json!({
        "name": format!("traced-{name}"),
        "authenticationType": "API_KEY",
        "logConfig": {
            // The service role AppSync assumes to publish to CloudWatch Logs
            // (`LogConfig$cloudWatchLogsRoleArn`).
            "cloudWatchLogsRoleArn": "arn:aws:iam::000000000000:role/appsync-logs",
            "fieldLogLevel": field_log_level,
            "excludeVerboseContent": false,
        },
    });
    let (status, created) = appsync(router, "POST", "/v1/apis", Some(request)).await;
    assert_eq!(status, 200, "CreateGraphqlApi must answer 200, got {created}");
    let api = &created["graphqlApi"];
    let api_id = api["apiId"].as_str().unwrap_or_default().to_string();
    assert_eq!(
        api["logConfig"]["fieldLogLevel"].as_str(),
        Some(field_log_level),
        "CreateGraphqlApi must report the level the api captures at, got {api}"
    );
    let graphql = api["uris"]["GRAPHQL"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let (authority, _) = split_published("the created api", &graphql);
    assert!(
        authority.starts_with(&format!("{LOOPBACK_HOST}:"))
            && loopback_port("the created api", &authority) > 0,
        "uris.GRAPHQL must be an address this emulator serves, got {graphql:?}"
    );

    let (status, key) = appsync(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/apikeys"),
        Some(json!({"description": "the suite's key"})),
    )
    .await;
    assert_eq!(status, 200, "CreateApiKey must answer 200, got {key}");
    install_schema(router, &api_id, SCHEMA).await;
    let (status, source) = appsync(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/datasources"),
        Some(json!({"name": DATA_SOURCE, "type": "NONE"})),
    )
    .await;
    assert_eq!(status, 200, "CreateDataSource must answer 200, got {source}");

    TracedApi {
        api_id: api_id.clone(),
        graphql,
        api_key: key["apiKey"]["id"].as_str().unwrap_or_default().to_string(),
        resolver_arns: create_resolvers(router, &api_id).await,
    }
}

/// Attach one resolver per field of `Query`, each a unit resolver whose request handler ends
/// its own step with the value the field takes, and answer the ARN `CreateResolver` reported
/// for each of them.
async fn create_resolvers(
    router: &MultiServiceRouter,
    api_id: &str,
) -> BTreeMap<String, String> {
    let ended: Vec<(&str, String)> = vec![
        ("probe", "ctx.args.msg".to_string()),
        ("counted", "3 + 4".to_string()),
        ("flagged", "true".to_string()),
        ("noted", format!("{{id: \"{NOTE_ID}\", text: \"{NOTE_TEXT}\"}}")),
    ];
    let mut arns: BTreeMap<String, String> = BTreeMap::new();
    for (field, value) in &ended {
        let (status, created) = appsync(
            router,
            "POST",
            &format!("/v1/apis/{api_id}/types/Query/resolvers"),
            Some(json!({
                "fieldName": field,
                "kind": "UNIT",
                "dataSourceName": DATA_SOURCE,
                "code": format!(
                    "export function request(ctx) {{ runtime.earlyReturn({value}); }}\n\
                     export function response(ctx) {{ return ctx.result; }}\n"
                ),
                "runtime": {"name": RUNTIME.0, "runtimeVersion": RUNTIME.1},
            })),
        )
        .await;
        assert_eq!(
            status, 200,
            "CreateResolver for {field} must answer 200, got {created}"
        );
        let arn = created["resolver"]["resolverArn"]
            .as_str()
            .unwrap_or_else(|| panic!("CreateResolver must report the resolver's ARN, got {created}"))
            .to_string();
        arns.insert((*field).to_string(), arn);
    }
    arns
}
