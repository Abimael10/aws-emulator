// SPDX-License-Identifier: CC0-1.0

//! `ListStreams` is the DISCOVERY entry point of a streams consumer, so this suite
//! pins the rule it answers by: every stream the control-plane seam has registered
//! and not yet reclaimed is reported, with `StreamArn`, `TableName` and
//! `StreamLabel` populated, and a `Limit` up to the documented upper limit of 100
//! is honoured exactly. (A `Limit` ABOVE that ceiling is refused, which is pinned
//! with this service's other parameter bounds in
//! `service_enforced_parameter_bounds.rs`.) The page the width closed reports the
//! stream it stopped at in `LastEvaluatedStreamArn`, which is what a client resumes
//! the enumeration from.
//!
//! The rows span the class rather than one table: every `StreamViewType` the model
//! declares, table names at both ends of the documented `TableName` bound and over
//! every character class its pattern allows, and names one of which is a prefix of
//! another (the registry ranges a table's streams by an ARN prefix, so that is the
//! case a prefix range can get wrong).
//!
//! Streams get INTO the registry through the publishing seam on this service's
//! state (`register_stream`), which is what DynamoDB's
//! `CreateTable`/`UpdateTable` call for a `StreamSpecification` that enables one.
//! Every assertion reads the result back over the wire (`POST /` +
//! `X-Amz-Target: DynamoDBStreams_20120810.<Op>`), so nothing is proved about the
//! store that a client cannot observe.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use service_com_amazonaws_dynamodbstreams_dynamodbstreams_20120810 as streams;
use smithy_server_runtime::MultiServiceRouter;
use streams::ops::types::{KeySchemaElement, STREAM_VIEW_TYPE_VALUES};
use streams::state::{
    ComAmazonawsDynamodbstreamsDynamodbstreams20120810State as StreamsState, StreamRegistration,
    StreamViewType, MAX_PAGE_LIMIT, MAX_STREAMS_PER_TABLE,
};

/// A SigV4 credential scope naming `dynamodb`, which is how DynamoDB Streams
/// requests are signed. Request authentication is off in these tests, so only the
/// scope's routing signal matters.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/dynamodb/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=deadbeef";

const REGION: &str = "us-east-1";

/// The documented bound on a table name, from `smithy_query(query="operation",
/// operation="ListStreams")`: `TableName` has length 3..255 and pattern
/// `^[a-zA-Z0-9_.-]+$`.
const MIN_TABLE_NAME_LENGTH: usize = 3;
const MAX_TABLE_NAME_LENGTH: usize = 255;

fn build() -> (Arc<engine::InMemoryEngine>, MultiServiceRouter) {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(streams::EmulatedService::new(Arc::clone(&engine)));
    (engine, router)
}

async fn call(
    router: &MultiServiceRouter,
    operation: &str,
    body: serde_json::Value,
) -> (u16, serde_json::Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("http://localhost/")
        .header("authorization", AUTHORIZATION)
        .header(
            "x-amz-target",
            format!("DynamoDBStreams_20120810.{operation}"),
        )
        .header("content-type", "application/x-amz-json-1.0")
        .body(bytes::Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(serde_json::Value::Null);
    (status, parsed)
}

/// One `ListStreams` page, as a 200 plus its entries.
async fn list_streams(
    router: &MultiServiceRouter,
    case: &str,
    body: serde_json::Value,
) -> serde_json::Value {
    let (status, page) = call(router, "ListStreams", body).await;
    assert_eq!(
        status, 200,
        "case {case}: ListStreams must answer 200, got {status} with {page}"
    );
    page
}

/// The `StreamArn` of every entry of a page, in the order the page reports them.
fn page_arns(case: &str, page: &serde_json::Value) -> Vec<String> {
    page["Streams"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case}: Streams must be a list, got {page}"))
        .iter()
        .map(|entry| {
            entry["StreamArn"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case}: an entry has no StreamArn: {entry}"))
                .to_string()
        })
        .collect()
}

/// A whole-second instant `seconds_ago` before now. Registrations are placed in the
/// past so no stream is still `ENABLING`, and one second apart so each gets its own
/// millisecond-resolution label.
fn seconds_ago(seconds: u64) -> SystemTime {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after the Unix epoch")
        .as_secs();
    UNIX_EPOCH + Duration::from_secs(now - seconds)
}

fn key_schema() -> Vec<KeySchemaElement> {
    vec![KeySchemaElement {
        attribute_name: "Id".to_string(),
        key_type: "HASH".to_string(),
    }]
}

/// Enable a stream on `table_name` through the publishing seam and return its ARN.
fn register(
    engine: &engine::InMemoryEngine,
    table_name: &str,
    stream_view_type: StreamViewType,
    at: SystemTime,
) -> String {
    engine
        .state::<StreamsState>()
        .lock()
        .register_stream(
            &StreamRegistration {
                table_name: table_name.to_string(),
                stream_view_type,
                key_schema: key_schema(),
                region: REGION.to_string(),
            },
            at,
        )
        .unwrap_or_else(|rejected| {
            panic!("the registry must accept a stream for {table_name}: {rejected}")
        })
}

/// Table-name forms spanning the documented `TableName` constraint (length 3..255,
/// pattern `^[a-zA-Z0-9_.-]+$`), none of them the name the recordings use.
fn table_name_forms() -> Vec<(&'static str, String)> {
    vec![
        (
            "the shortest name the bound allows",
            "a".repeat(MIN_TABLE_NAME_LENGTH),
        ),
        (
            "the longest name the bound allows",
            "t".repeat(MAX_TABLE_NAME_LENGTH),
        ),
        (
            "every character class the pattern allows",
            "Ledger_2026.v1-BETA".to_string(),
        ),
    ]
}

/// Every registered stream is discoverable with its identity populated, for every
/// `StreamViewType` the model declares and whatever the table is named: the
/// unscoped enumeration reports it, scoping by `TableName` narrows to exactly that
/// table's streams, and `DescribeStream` reads the same identity back.
#[tokio::test]
async fn every_registered_stream_is_discoverable_with_its_identity_populated() {
    let (engine, router) = build();

    // The view-type catalog is the model's own value list
    // (`streams::ops::types::STREAM_VIEW_TYPE_VALUES`), so a value added to the
    // enum is covered here without this test being edited.
    let mut registered: Vec<(String, String, String, String)> = Vec::new();
    let mut per_table: HashMap<String, Vec<String>> = HashMap::new();
    let mut offset = 0_u64;
    for (form, table_name) in table_name_forms() {
        for wire_view_type in STREAM_VIEW_TYPE_VALUES {
            let view_type = StreamViewType::from_wire(wire_view_type).unwrap_or_else(|| {
                panic!("the state parses every modelled StreamViewType, not {wire_view_type}")
            });
            let case = format!("{form} / {wire_view_type}");
            let stream_arn = register(
                &engine,
                &table_name,
                view_type,
                seconds_ago(3_600 - offset),
            );
            offset += 1;
            per_table
                .entry(table_name.clone())
                .or_default()
                .push(stream_arn.clone());
            registered.push((
                case,
                table_name.clone(),
                (*wire_view_type).to_string(),
                stream_arn,
            ));
        }
    }

    let page = list_streams(&router, "the whole registry", serde_json::json!({})).await;
    let listed: HashMap<String, serde_json::Value> = page["Streams"]
        .as_array()
        .unwrap_or_else(|| panic!("Streams must be a list, got {page}"))
        .iter()
        .map(|entry| {
            (
                entry["StreamArn"].as_str().unwrap_or_default().to_string(),
                entry.clone(),
            )
        })
        .collect();

    for (case, table_name, wire_view_type, stream_arn) in &registered {
        let entry = listed.get(stream_arn).unwrap_or_else(|| {
            panic!("case {case}: ListStreams must report {stream_arn}, got {page}")
        });
        assert_eq!(
            entry["TableName"].as_str(),
            Some(table_name.as_str()),
            "case {case}: the entry must name its table, got {entry}"
        );
        // "A timestamp, in ISO 8601 format, for this stream" (`Stream$StreamLabel`),
        // and the label a stream ARN ends with is that same stream's label — the
        // registry builds the ARN from it, so the two can never disagree.
        let label = entry["StreamLabel"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: the entry has no StreamLabel: {entry}"));
        assert!(
            !label.is_empty() && stream_arn.ends_with(&format!("/stream/{label}")),
            "case {case}: StreamLabel {label:?} must be the label of {stream_arn}"
        );

        // Scoped: "only the streams associated with this table name are returned".
        let scoped = list_streams(
            &router,
            case,
            serde_json::json!({ "TableName": table_name }),
        )
        .await;
        let mut scoped_arns = page_arns(case, &scoped);
        scoped_arns.sort();
        let mut expected = per_table
            .get(table_name)
            .cloned()
            .unwrap_or_else(|| panic!("case {case}: {table_name} registered nothing"));
        expected.sort();
        assert_eq!(
            scoped_arns, expected,
            "case {case}: TableName must report exactly that table's streams"
        );

        // Round trip: the discovered ARN is usable, and reports the same identity.
        let (status, described) = call(
            &router,
            "DescribeStream",
            serde_json::json!({ "StreamArn": stream_arn }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: DescribeStream on a discovered ARN must answer 200, got {status} with \
             {described}"
        );
        let description = &described["StreamDescription"];
        assert_eq!(
            description["StreamArn"].as_str(),
            Some(stream_arn.as_str()),
            "case {case}: DescribeStream must echo the ARN, got {described}"
        );
        assert_eq!(
            description["TableName"].as_str(),
            Some(table_name.as_str()),
            "case {case}: DescribeStream must name the same table, got {described}"
        );
        assert_eq!(
            description["StreamViewType"].as_str(),
            Some(wire_view_type.as_str()),
            "case {case}: DescribeStream must report the registered view type, got {described}"
        );
        assert_eq!(
            description["StreamLabel"].as_str(),
            Some(label),
            "case {case}: DescribeStream must report the listed label, got {described}"
        );
        // "The key attribute(s) of the stream's DynamoDB table"
        // (`StreamDescription$KeySchema`): the schema the registration carried,
        // reported element for element.
        assert_eq!(
            description["KeySchema"],
            serde_json::json!([{ "AttributeName": "Id", "KeyType": "HASH" }]),
            "case {case}: DescribeStream must report the registered key schema, got {described}"
        );
    }
}

/// A table whose name is a PREFIX of another table's name owns only its own
/// streams. The registry ranges a table's streams by their shared ARN prefix, so
/// this is the case a range can over-report; the rows are names that differ only
/// past the shorter one, over every separator the name pattern allows.
#[tokio::test]
async fn a_table_name_that_prefixes_another_owns_only_its_own_streams() {
    let (engine, router) = build();
    let names = ["abc", "abcd", "abc.1", "abc-1", "abc_1", "abcde"];
    let mut expected: Vec<(&str, String)> = Vec::new();
    for (index, name) in names.iter().enumerate() {
        let at = seconds_ago(3_600 - index as u64);
        expected.push((name, register(&engine, name, StreamViewType::KeysOnly, at)));
    }

    for (name, stream_arn) in &expected {
        let page = list_streams(&router, name, serde_json::json!({ "TableName": name })).await;
        assert_eq!(
            page_arns(name, &page),
            vec![stream_arn.clone()],
            "case {name}: a table reports its own one stream and no other table's"
        );
    }

    // And the unscoped enumeration is all of them, once each, in ARN order.
    let page = list_streams(&router, "the whole registry", serde_json::json!({})).await;
    let mut listed = page_arns("the whole registry", &page);
    let mut all: Vec<String> = expected.iter().map(|(_, arn)| arn.clone()).collect();
    listed.sort();
    all.sort();
    assert_eq!(
        listed, all,
        "the registry must report every registered stream exactly once"
    );
}

/// `Limit` is honoured exactly up to the documented ceiling: "The maximum number of
/// streams to return. The upper limit is 100" (`ListStreamsInput$Limit`), whose
/// declared range is 1.. — so 0 is refused at the boundary, an absent member asks for
/// the widest page, and 100 is the largest page served.
#[tokio::test]
async fn one_page_never_exceeds_the_documented_hundred_streams() {
    let (engine, router) = build();
    // More streams than one page may carry, spread over enough tables that no table
    // meets the registry's own per-table bound.
    let ceiling = usize::try_from(MAX_PAGE_LIMIT).expect("the page ceiling is positive");
    let per_table = MAX_STREAMS_PER_TABLE - 2;
    let tables = ceiling / per_table + 2;
    let mut all: Vec<String> = Vec::new();
    let mut offset = 0_u64;
    for table in 0..tables {
        let table_name = format!("ledger-{table:03}");
        for _ in 0..per_table {
            all.push(register(
                &engine,
                &table_name,
                StreamViewType::NewAndOldImages,
                seconds_ago(20_000 - offset),
            ));
            offset += 1;
        }
    }
    all.sort();
    assert!(
        all.len() > ceiling,
        "the fixture must hold more streams than one page carries, got {}",
        all.len()
    );

    // Each row: the `Limit` sent (None for an absent member) and the page size the
    // rule requires — the smallest the range allows, two values inside the ceiling,
    // just inside it, and the published bound itself.
    let cases: [(&str, Option<i64>, usize); 5] = [
        ("no Limit at all", None, 100),
        ("Limit 1, the smallest the range allows", Some(1), 1),
        ("Limit 17", Some(17), 17),
        ("Limit 99, just inside the ceiling", Some(99), 99),
        ("Limit 100, the published ceiling", Some(100), 100),
    ];

    for (case, limit, expected_len) in cases {
        let body = match limit {
            Some(limit) => serde_json::json!({ "Limit": limit }),
            None => serde_json::json!({}),
        };
        let page = list_streams(&router, case, body).await;
        let arns = page_arns(case, &page);
        assert_eq!(
            arns.len(),
            expected_len,
            "case {case}: expected a page of {expected_len}, got {}",
            arns.len()
        );
        assert_eq!(
            arns,
            all[..expected_len].to_vec(),
            "case {case}: a page is the next entries in stream-ARN order"
        );
        // Every row above truncates the enumeration, so each must hand back the
        // cursor to resume from: "the stream ARN of the item where the operation
        // stopped, inclusive of the previous result set".
        assert_eq!(
            page["LastEvaluatedStreamArn"].as_str(),
            arns.last().map(String::as_str),
            "case {case}: a truncated page ends at LastEvaluatedStreamArn, got {page}"
        );
    }

    // Paging at the ceiling still visits every stream exactly once.
    let case = "paging with Limit 100";
    let mut seen: Vec<String> = Vec::new();
    let mut start: Option<String> = None;
    for _ in 0..all.len() + 2 {
        let mut body = serde_json::json!({ "Limit": MAX_PAGE_LIMIT });
        if let (Some(target), Some(start)) = (body.as_object_mut(), start.as_ref()) {
            target.insert(
                "ExclusiveStartStreamArn".to_string(),
                serde_json::Value::String(start.clone()),
            );
        }
        let page = list_streams(&router, case, body).await;
        let arns = page_arns(case, &page);
        assert!(
            arns.len() <= ceiling,
            "case {case}: a page of {} exceeded the documented ceiling",
            arns.len()
        );
        seen.extend(arns);
        match page["LastEvaluatedStreamArn"].as_str() {
            Some(last) => start = Some(last.to_string()),
            None => break,
        }
    }
    assert_eq!(
        seen, all,
        "case {case}: paging must visit every stream once, in ARN order"
    );

    // `Limit`'s declared range is 1.., so 0 is refused at the boundary rather than
    // silently clamped into a page.
    let (status, body) = call(&router, "ListStreams", serde_json::json!({ "Limit": 0 })).await;
    assert_eq!(
        status, 400,
        "Limit 0 is outside the declared range and must be refused, got {status} with {body}"
    );
}

/// One row of the cursor table: the case, the `Limit` it sends (`None` for an absent
/// member), the table the page is scoped to (`None` for the whole registry), and the
/// index in the matching list the page resumes AFTER (`None` for the start of it).
type CursorCase = (
    &'static str,
    Option<i64>,
    Option<&'static str>,
    Option<usize>,
);

/// The resume point a page reports: "the stream ARN of the item where the operation
/// stopped, inclusive of the previous result set. Use this value to start a new
/// operation, excluding this value in the new request"
/// (`ListStreamsOutput$LastEvaluatedStreamArn`).
///
/// The rule pinned here: the page STOPPED at an item exactly when the width it was
/// allowed ran out, so the cursor is reported whenever the page was filled to that
/// width — including a full page with nothing after it, which is why "If
/// LastEvaluatedStreamArn is not empty, it does not necessarily mean that there is more
/// data in the result set" — and is absent only for a page the width did not close,
/// which is the signal that the last page has been processed.
///
/// The rows span what the rule generalises over: how wide the page asked for is (absent,
/// the narrowest, inside the registry, exactly as wide as what matched, wider, and the
/// published ceiling), how many streams match (the whole registry or one table's
/// streams), and where the page resumes from (the start, after an entry, and past the
/// last one).
#[tokio::test]
async fn the_cursor_is_reported_whenever_the_requested_width_closed_the_page() {
    let (engine, router) = build();
    let base = seconds_ago(7_200);
    // Five streams over two tables, so a page can be narrowed by `TableName` as well
    // as by `Limit`, and the two tables hold different numbers of them.
    let mut by_table: HashMap<String, Vec<String>> = HashMap::new();
    let mut offset = 0_u64;
    for (table_name, count) in [("ledger-a", 3_usize), ("ledger-b", 2_usize)] {
        for _ in 0..count {
            // One second apart, so every stream gets its own millisecond label.
            let at = base + Duration::from_secs(offset);
            let stream_arn = register(&engine, table_name, StreamViewType::KeysOnly, at);
            by_table
                .entry(table_name.to_string())
                .or_default()
                .push(stream_arn);
            offset += 1;
        }
    }
    for arns in by_table.values_mut() {
        arns.sort();
    }
    let mut all: Vec<String> = by_table.values().flatten().cloned().collect();
    all.sort();
    assert_eq!(all.len(), 5, "the fixture must hold five streams, got {all:?}");

    // Each row: the `Limit` sent (None for an absent member), the table the page is
    // scoped to (None for the whole registry), and the index in the matching list the
    // page resumes AFTER (None for the start of it).
    let rows: [CursorCase; 12] = [
        ("no Limit at all", None, None, None),
        ("Limit 1, the narrowest page", Some(1), None, None),
        ("Limit 1 resuming after the first stream", Some(1), None, Some(0)),
        ("Limit 2, inside the registry", Some(2), None, None),
        ("Limit 4, one short of the registry", Some(4), None, None),
        ("Limit 5, exactly the registry", Some(5), None, None),
        ("Limit 6, wider than the registry", Some(6), None, None),
        (
            "Limit 100, the published ceiling",
            Some(i64::from(MAX_PAGE_LIMIT)),
            None,
            None,
        ),
        (
            "Limit 2 scoped to the table with three streams",
            Some(2),
            Some("ledger-a"),
            None,
        ),
        (
            "Limit 2 scoped to the table with two streams",
            Some(2),
            Some("ledger-b"),
            None,
        ),
        (
            "Limit 3 scoped to the table with two streams",
            Some(3),
            Some("ledger-b"),
            None,
        ),
        ("Limit 1 resuming after the last stream", Some(1), None, Some(4)),
    ];

    for (case, limit, table_name, resume_after) in rows {
        let matching = match table_name {
            Some(table_name) => by_table
                .get(table_name)
                .cloned()
                .unwrap_or_else(|| panic!("case {case}: {table_name} registered nothing")),
            None => all.clone(),
        };
        let width = usize::try_from(limit.unwrap_or(i64::from(MAX_PAGE_LIMIT)))
            .expect("every row asks for a positive width");
        let start = resume_after.map_or(0, |index| index + 1);
        let expected_page: Vec<String> = matching
            .iter()
            .skip(start.min(matching.len()))
            .take(width)
            .cloned()
            .collect();
        // The cursor is the item the width stopped the page at, so it is there exactly
        // when the page was filled to the width asked for.
        let expected_cursor = (expected_page.len() == width)
            .then(|| expected_page.last().cloned())
            .flatten();

        let mut body = serde_json::json!({});
        if let Some(target) = body.as_object_mut() {
            if let Some(limit) = limit {
                target.insert("Limit".to_string(), serde_json::json!(limit));
            }
            if let Some(table_name) = table_name {
                target.insert("TableName".to_string(), serde_json::json!(table_name));
            }
            if let Some(index) = resume_after {
                target.insert(
                    "ExclusiveStartStreamArn".to_string(),
                    serde_json::json!(matching[index]),
                );
            }
        }
        let page = list_streams(&router, case, body).await;
        let arns = page_arns(case, &page);
        assert_eq!(
            arns, expected_page,
            "case {case}: expected the page {expected_page:?}, got {arns:?}"
        );
        let cursor = page["LastEvaluatedStreamArn"].as_str().map(str::to_string);
        assert_eq!(
            cursor, expected_cursor,
            "case {case}: expected the cursor {expected_cursor:?}, got {cursor:?}"
        );
    }
}

/// The cursor is what a client's enumeration is driven by, so following it must visit
/// every stream exactly once and terminate — whatever width the pages are asked for,
/// including one that divides the registry exactly (where the last full page hands back
/// a cursor whose next call is the empty page that ends the walk).
#[tokio::test]
async fn following_the_cursor_visits_every_stream_once_and_terminates() {
    let (engine, router) = build();
    let base = seconds_ago(7_200);
    let mut all: Vec<String> = (0..6_u64)
        .map(|index| {
            register(
                &engine,
                &format!("ledger-{index}"),
                StreamViewType::NewImage,
                base + Duration::from_secs(index),
            )
        })
        .collect();
    all.sort();

    // The widths differ in how they fall against the registry: narrower than it, a
    // divisor of it, one short of it, exactly it, and wider than it.
    for width in [1_i64, 2, 5, 6, 7] {
        let case = format!("walking with Limit {width}");
        let mut seen: Vec<String> = Vec::new();
        let mut start: Option<String> = None;
        // Generous: one call per stream plus the terminating one and a few spare, so a
        // walk that fails to terminate is caught instead of looping for ever.
        for _ in 0..all.len() + 4 {
            let mut body = serde_json::json!({ "Limit": width });
            if let (Some(target), Some(start)) = (body.as_object_mut(), start.as_ref()) {
                target.insert(
                    "ExclusiveStartStreamArn".to_string(),
                    serde_json::Value::String(start.clone()),
                );
            }
            let page = list_streams(&router, &case, body).await;
            seen.extend(page_arns(&case, &page));
            match page["LastEvaluatedStreamArn"].as_str() {
                Some(cursor) => start = Some(cursor.to_string()),
                None => break,
            }
        }
        assert_eq!(
            seen, all,
            "case {case}: the walk must visit every stream once, in ARN order"
        );
    }
}
