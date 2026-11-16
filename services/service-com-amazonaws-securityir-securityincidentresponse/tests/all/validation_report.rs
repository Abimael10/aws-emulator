// SPDX-License-Identifier: CC0-1.0

//! How the Security IR emulator states a request BODY it refused for a rule its request
//! schema declares, end to end over `restJson1`.
//!
//! The rule under test: such a request — refused before any handler runs, because a value
//! broke a rule the operation's input schema declares — is answered with the validating
//! layer's own bracketed REPORT: one finding per broken rule, then one per JSON Schema
//! `format` attribute THAT operation's schema declares and the validator implements no
//! check for. It is never the AWS-wide `N validation error(s) detected: …` sentence. The
//! status (400) and the modelled `ValidationException` code are unchanged.
//!
//! So the table below is one row per CLASS of rule the model declares, over two operations
//! whose schemas owe DIFFERENT `format` findings — `CreateCase`, which declares `@sensitive`
//! strings and a timestamp, and `ListCases`, which declares neither — each row asserting the
//! WHOLE message the wire carries. Every bound, expression and value set a row names comes
//! from this service's model
//! (`smithy_query(query="operation", operation="CreateCase,ListCases")` and
//! `smithy_query(query="shape", shape="EngagementType,ResolverType,TagMap,AWSAccountId")`),
//! never from one recorded response.
//!
//! The last suite pins what is deliberately NOT a report: a value the request carries
//! outside its body, a value the refusal withholds, and a refusal a handler words. The rule
//! stays where the evidence puts it.

use serde_json::{json, Map, Value};

use super::case_lifecycle::{create_request, Emulator};

/// The region every request here is signed into. Which region a refusal is asked for does
/// not change how it is worded.
const REGION: &str = "us-east-1";

/// The `format` findings `CreateCase`'s request schema owes: its `title` and `description`
/// are `@sensitive` strings (declared as a password) and `reportedIncidentStartDate` a
/// timestamp (declared as a number with a double format). Each is stated once.
const CREATE_CASE_FORMATS: &[&str] = &["password", "double"];

/// The `format` findings `ListCases`'s request schema owes: none — a page token is a plain
/// string and a page size an integer.
const LIST_CASES_FORMATS: &[&str] = &[];

/// The whole message a report of `findings` reaches the wire as, for an operation whose
/// schema owes the `format` attributes `formats`.
fn report(findings: &[&str], formats: &[&str]) -> String {
    let listed: Vec<String> = findings
        .iter()
        .map(|finding| (*finding).to_string())
        .chain(
            formats
                .iter()
                .map(|format| format!("format attribute \"{format}\" not supported")),
        )
        .collect();
    format!("[{}]", listed.join(", "))
}

/// A valid `CreateCase` body with `member` replaced by `value`, so a row varies exactly the
/// one member whose rule it is about.
fn body_with(member: &str, value: Value) -> Value {
    let mut body = create_request("a validation case", 0);
    body.as_object_mut()
        .expect("the request body is a JSON object")
        .insert(member.to_string(), value);
    body
}

/// A valid `CreateCase` body with `member` left out altogether.
fn body_without(member: &str) -> Value {
    let mut body = create_request("a validation case", 0);
    body.as_object_mut()
        .expect("the request body is a JSON object")
        .remove(member);
    body
}

/// `count` distinct, well-formed 12-digit account ids — the form `AWSAccountId`'s
/// `^[0-9]{12}$` requires, so the only rule such a list can break is its own element count.
fn account_ids(count: usize) -> Value {
    Value::Array(
        (0..count)
            .map(|index| Value::String(format!("{index:012}")))
            .collect(),
    )
}

/// A tag map carrying `count` distinct entries, each key and value inside `TagKey`'s and
/// `TagValue`'s own length bounds, so the only rule such a map can break is its property
/// count.
fn tags(count: usize) -> Value {
    let mut map = Map::new();
    for index in 0..count {
        map.insert(format!("key-{index}"), Value::String("v".to_string()));
    }
    Value::Object(map)
}

/// One row of the boundary table: the request to send, and the message the report must
/// carry.
struct Row {
    case: &'static str,
    /// The path the request is sent to — `CreateCase`'s or `ListCases`'s — so the table
    /// spans two operations, whose schemas owe different `format` findings.
    path: &'static str,
    body: Value,
    expected: String,
}

/// The boundary table: one row per class of rule the model declares, over two operations.
fn boundary_table() -> Vec<Row> {
    let create = "/v1/create-case";
    let list = "/v1/list-cases";
    // 255 is `clientToken`'s declared `@length` maximum; 300 characters is past it by
    // enough to outrun the framework's own echo ceiling, so the finding has to quote the
    // whole submitted value rather than an elision of it.
    let over_long_token = "t".repeat(300);
    let account = "000123123123";
    vec![
        Row {
            case: "a value outside EngagementType",
            path: create,
            body: body_with("engagementType", json!("BadType")),
            expected: report(
                &[
                    "instance value (\"BadType\") not found in enum (possible values: \
                     [\"Security Incident\",\"Investigation\"])",
                ],
                CREATE_CASE_FORMATS,
            ),
        },
        Row {
            case: "a value outside ResolverType",
            path: create,
            body: body_with("resolverType", json!("Neither")),
            expected: report(
                &[
                    "instance value (\"Neither\") not found in enum (possible values: \
                     [\"AWS\",\"Self\"])",
                ],
                CREATE_CASE_FORMATS,
            ),
        },
        Row {
            case: "a required member the request leaves out",
            path: create,
            body: body_without("engagementType"),
            expected: report(
                &["object has missing required properties ([\"engagementType\"])"],
                CREATE_CASE_FORMATS,
            ),
        },
        Row {
            case: "a string below its length minimum",
            path: create,
            body: body_with("clientToken", json!("")),
            expected: report(
                &["string \"\" is too short (length: 0, required minimum: 1)"],
                CREATE_CASE_FORMATS,
            ),
        },
        Row {
            case: "a string above its length maximum",
            path: create,
            body: body_with("clientToken", json!(over_long_token)),
            expected: report(
                &[&format!(
                    "string \"{over_long_token}\" is too long (length: 300, maximum \
                     allowed: 255)"
                )],
                CREATE_CASE_FORMATS,
            ),
        },
        Row {
            case: "a list element outside its pattern",
            path: create,
            body: body_with("impactedAccounts", json!(["abcdefghijkl"])),
            expected: report(
                &[
                    "ECMA 262 regex \"^[0-9]{12}$\" does not match input string \
                     \"abcdefghijkl\"",
                ],
                CREATE_CASE_FORMATS,
            ),
        },
        Row {
            case: "a list carrying the same element twice",
            path: create,
            body: body_with("impactedAccounts", json!([account, account])),
            expected: report(
                &["array must not contain duplicate elements"],
                CREATE_CASE_FORMATS,
            ),
        },
        Row {
            case: "a list above its element maximum",
            path: create,
            body: body_with("impactedAccounts", account_ids(201)),
            expected: report(
                &[
                    "array is too long: must have at most 200 elements but instance has 201 \
                     elements",
                ],
                CREATE_CASE_FORMATS,
            ),
        },
        Row {
            case: "a map above its property maximum",
            path: create,
            body: body_with("tags", tags(201)),
            expected: report(
                &["object has too many properties (found 201 but schema requires at most 200)"],
                CREATE_CASE_FORMATS,
            ),
        },
        Row {
            case: "a number below its range minimum",
            path: list,
            body: json!({"maxResults": 0}),
            expected: report(
                &["numeric instance is lower than the required minimum (minimum: 1, found: 0)"],
                LIST_CASES_FORMATS,
            ),
        },
        Row {
            case: "a number above its range maximum",
            path: list,
            body: json!({"maxResults": 26}),
            expected: report(
                &[
                    "numeric instance is greater than the required maximum (maximum: 25, \
                     found: 26)",
                ],
                LIST_CASES_FORMATS,
            ),
        },
    ]
}

/// Every class of rule the schema declares is reported as the validating layer's own
/// finding, with the modelled `ValidationException` code and HTTP 400 — and the `format`
/// findings are the ones the refused operation's OWN schema declares, so `ListCases` is
/// answered with none.
#[tokio::test]
async fn every_broken_schema_rule_is_reported_as_a_validator_finding() {
    let emulator = Emulator::new();

    for row in boundary_table() {
        let (status, body) = emulator
            .call("POST", row.path, REGION, Some(row.body.clone()))
            .await;
        let case = row.case;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body:?}"
        );
        assert_eq!(
            body["__type"].as_str(),
            Some("ValidationException"),
            "case {case:?}: expected the modelled ValidationException code, got {:?}",
            body["__type"]
        );
        assert_eq!(
            body["message"].as_str(),
            Some(row.expected.as_str()),
            "case {case:?}: expected {:?}, got {:?}",
            row.expected,
            body["message"]
        );
    }
}

/// A body that breaks two rules is reported as two findings in the ONE list, ahead of the
/// findings about the validator — so a caller splitting the report reads every broken rule,
/// not just the first.
#[tokio::test]
async fn several_broken_rules_are_reported_in_one_list() {
    let emulator = Emulator::new();
    let mut body = body_with("engagementType", json!("BadType"));
    body.as_object_mut()
        .expect("the request body is a JSON object")
        .insert("resolverType".to_string(), json!("Neither"));

    let (status, answered) = emulator.create_case(REGION, body).await;
    assert_eq!(
        status, 400,
        "two broken rules must be refused 400, got {status} with {answered:?}"
    );
    let message = answered["message"]
        .as_str()
        .unwrap_or_else(|| panic!("the refusal must carry a message, got {answered:?}"))
        .to_string();

    let engagement = "instance value (\"BadType\") not found in enum (possible values: \
                      [\"Security Incident\",\"Investigation\"])";
    let resolver = "instance value (\"Neither\") not found in enum (possible values: \
                    [\"AWS\",\"Self\"])";
    for finding in [engagement, resolver] {
        assert!(
            message.contains(finding),
            "case \"two broken rules\": expected the report to state {finding:?}, got {message:?}"
        );
    }
    let tail = report(&[], CREATE_CASE_FORMATS);
    let tail = tail
        .strip_prefix('[')
        .expect("the report is bracketed")
        .to_string();
    assert!(
        message.starts_with('[') && message.ends_with(&tail),
        "case \"two broken rules\": expected one bracketed list ending in {tail:?}, got \
         {message:?}"
    );
    assert_eq!(
        message.matches("instance value (").count(),
        2,
        "case \"two broken rules\": expected both enum findings in the one list, got {message:?}"
    );
}

/// One row of the boundary table's other half: a request this layer does NOT report on, and
/// the answer the layer that refuses it states.
struct Kept {
    case: &'static str,
    method: &'static str,
    path: &'static str,
    body: Option<Value>,
    /// The status that layer answers.
    status: u16,
    /// The error code it names.
    code: &'static str,
    /// A phrase of the message it states.
    phrase: &'static str,
}

/// What this layer does NOT report: a value the request carries outside its body, a value
/// the refusal withholds, and a refusal a handler words. Each keeps the wording the layer
/// that owns it states — never a half-built report.
#[tokio::test]
async fn what_the_validating_layer_does_not_word_keeps_its_own_wording() {
    let emulator = Emulator::new();
    emulator
        .create_case(REGION, create_request("a stored case", 0))
        .await;

    let rows = [
        Kept {
            // `caseId` is this operation's `@httpLabel`, so no body carries it.
            case: "a URI label outside its pattern",
            method: "GET",
            path: "/v1/cases/not-a-case-id/get-case",
            body: None,
            status: 400,
            code: "ValidationException",
            phrase: "failed to satisfy constraint",
        },
        Kept {
            // `title` is `@sensitive`, so the refusal states no value — and a finding
            // that would have quoted one is not written at all.
            case: "a withheld member below its length minimum",
            method: "POST",
            path: "/v1/create-case",
            body: Some(body_with("title", json!(""))),
            status: 400,
            code: "ValidationException",
            phrase: "Value at 'title' failed to satisfy constraint: Member must have length \
                     greater than or equal to 1",
        },
        Kept {
            case: "a caseId nothing is stored under",
            method: "GET",
            path: "/v1/cases/9999999999/get-case",
            body: None,
            status: 404,
            code: "ResourceNotFoundException",
            phrase: "No case found with caseId 9999999999",
        },
        Kept {
            case: "a nextToken this service never issued",
            method: "POST",
            path: "/v1/list-cases",
            body: Some(json!({"nextToken": "not-a-token-this-service-issued"})),
            status: 400,
            code: "ValidationException",
            phrase: "nextToken is not a token returned by a previous ListCases call",
        },
    ];

    for row in rows {
        let case = row.case;
        let (status, answered) = emulator
            .call(row.method, row.path, REGION, row.body)
            .await;
        assert_eq!(
            status, row.status,
            "case {case:?}: expected HTTP {}, got {status} with {answered:?}",
            row.status
        );
        assert_eq!(
            answered["__type"].as_str(),
            Some(row.code),
            "case {case:?}: expected {:?}, got {:?}",
            row.code,
            answered["__type"]
        );
        let message = answered["message"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a message, got {answered:?}"));
        assert!(
            !message.starts_with('['),
            "case {case:?}: expected the wording of the layer that refused it, got the \
             validator's report {message:?}"
        );
        assert!(
            message.contains(row.phrase),
            "case {case:?}: expected a message stating {:?}, got {message:?}",
            row.phrase
        );
    }
}
