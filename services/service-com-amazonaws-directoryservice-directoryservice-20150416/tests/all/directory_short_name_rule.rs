// SPDX-License-Identifier: CC0-1.0

//! THE SHORT NAME a create may give a directory, driven end to end over HTTP.
//!
//! Every case goes through the same front door a client uses: `POST /` with
//! `X-Amz-Target: DirectoryService_20150416.<Operation>` and an `awsJson1_1` body
//! (`crate::harness`), so nothing here reaches into the state struct and an answer that is
//! not computed from persisted state cannot pass.
//!
//! The rule under test: a `ShortName` is the NetBIOS name of the directory's Windows
//! domain, so Directory Service holds it to its own rule — one to fifteen characters, each
//! an alphanumeric or one of the special characters the refusal publishes — which is
//! NARROWER than the modelled `DirectoryShortName` expression the request boundary applies.
//! A create that names a short name outside that rule is refused with the modelled
//! `InvalidParameterException`, HTTP 400, whose `Message` publishes the rule and closes with
//! the call's own request id, and the create STORES NOTHING.
//!
//! It is asserted over a TABLE spanning what the rule turns on — the length at and past each
//! published bound, a character outside the admitted set, a period in each position it can
//! take — and over every operation that names a short name (the two creates that make a
//! directory and the create that connects one), with the admitted characters and the
//! published bounds as the positive control, so no row can pass by matching one recorded
//! name. Every refused row is a name the modelled `DirectoryShortName` expression ADMITS, so
//! what each row shows is this service's own rule rather than the constraint the request
//! boundary applies.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, ok, service};

/// What this service's refusal envelope appends to every diagnostic it words: the call's own
/// correlation handle, whose value is per call.
const REQUEST_ID_SEPARATOR: &str = " : RequestId: ";

/// The published rule, as the refusal states it: FIXED text that quotes neither the name it
/// rejected nor the member it arrived in, opening with a space.
const PUBLISHED_RULE: &str = " Invalid directory short name. The short name must be between 1 \
                              and 15 characters, and can consist of alphanumeric characters \
                              and some special characters (! @ # $ % ^ & ( ) - _ ' { } or \
                              ~). The following special characters are not allowed: \
                              ^ \\ / : * ? \" < > . and |. The name cannot begin with a \
                              period.";

/// The longest short name the published rule admits, and the shortest one past it, built
/// FROM the bounds the rule publishes rather than written out.
fn at_the_maximum() -> String {
    "A".repeat(15)
}
fn past_the_maximum() -> String {
    "A".repeat(16)
}

/// One operation that names a short name, and the body it names it in.
struct ShortNameCase {
    case: &'static str,
    operation: &'static str,
    body: fn(&str, Option<&str>) -> Value,
}

/// Every operation of this service whose input carries a `ShortName`
/// (`smithy_query(query="operation",operation="CreateDirectory,CreateMicrosoftAD,ConnectDirectory")`),
/// each with a body whose other members are well formed, so what decides a row is the short
/// name and nothing else.
fn short_name_rows() -> Vec<ShortNameCase> {
    vec![
        ShortNameCase {
            case: "a Managed Microsoft AD create",
            operation: "CreateMicrosoftAD",
            body: |name, short_name| {
                let mut body = json!({
                    "Name": name,
                    "Password": crate::directory_lifecycle::PASSWORD,
                    "VpcSettings": {
                        "VpcId": "vpc-0a1b2c3d",
                        "SubnetIds": ["subnet-0a1b2c3d", "subnet-1a2b3c4d"]
                    }
                });
                with_short_name(&mut body, short_name);
                body
            },
        },
        ShortNameCase {
            case: "a Simple AD create",
            operation: "CreateDirectory",
            body: |name, short_name| {
                let mut body = json!({
                    "Name": name,
                    "Password": crate::directory_lifecycle::PASSWORD,
                    "Size": "Small",
                });
                with_short_name(&mut body, short_name);
                body
            },
        },
        ShortNameCase {
            case: "an AD Connector create",
            operation: "ConnectDirectory",
            body: |name, short_name| {
                let mut body = json!({
                    "Name": name,
                    "Password": "Sim2Real-Connect-Pw1!",
                    "Size": "Small",
                    "ConnectSettings": {
                        "VpcId": "vpc-0a1b2c3d",
                        "SubnetIds": ["subnet-0a1b2c3d", "subnet-1a2b3c4d"],
                        "CustomerDnsIps": ["10.0.0.10", "10.0.0.11"],
                        "CustomerUserName": "ad-connector-svc"
                    }
                });
                with_short_name(&mut body, short_name);
                body
            },
        },
    ]
}

/// Put `short_name` in `body`, or leave the member out when the row names none.
fn with_short_name(body: &mut Value, short_name: Option<&str>) {
    if let (Some(object), Some(short_name)) = (body.as_object_mut(), short_name) {
        object.insert("ShortName".to_string(), json!(short_name));
    }
}

/// How many directories the store holds, read back through the reporting operation.
async fn stored_count(router: &MultiServiceRouter, case: &str) -> usize {
    let answer = ok(router, "DescribeDirectories", json!({}), case).await;
    answer["DirectoryDescriptions"]
        .as_array()
        .map(Vec::len)
        .unwrap_or_default()
}

/// RULE: a short name outside Directory Service's own rule is refused with
/// `InvalidParameterException` and the published sentence, and the create stores nothing.
#[tokio::test]
async fn a_short_name_outside_the_rule_is_refused_and_creates_nothing() {
    let router = service();
    let before = stored_count(&router, "before any refused create").await;

    // (case, the short name the create names) — the rows differ along the clause of the
    // rule each one breaks: the length past the published maximum, a period in each position
    // the modelled expression lets through, and characters outside the admitted set. Every
    // row is a name the modelled `DirectoryShortName` expression ADMITS
    // (`^[^\\/:*?"<>|.]+[^\\/:*?"<>|]*$`, which bounds no length and excludes a period only
    // in the first position), so what refuses each one is this service's own rule rather
    // than the constraint the request boundary applies.
    let past_maximum = past_the_maximum();
    let rows: Vec<(String, String)> = vec![
        ("one character past the published maximum", past_maximum),
        ("a period between two labels", "CORP.EXAMPLE".to_string()),
        ("a period at the end", "CORP.".to_string()),
        ("a space, outside the admitted set", "CORP AD".to_string()),
        (
            "a plus sign, outside the admitted set",
            "CORP+AD".to_string(),
        ),
        ("a comma, outside the admitted set", "CORP,AD".to_string()),
        (
            "an equals sign, outside the admitted set",
            "CORP=AD".to_string(),
        ),
        (
            "a semicolon, outside the admitted set",
            "CORP;AD".to_string(),
        ),
        (
            "a square bracket, outside the admitted set",
            "CORP[AD".to_string(),
        ),
        ("a non-ASCII letter", "CÖRP".to_string()),
    ]
    .into_iter()
    .map(|(case, short_name)| (case.to_string(), short_name))
    .collect();

    for row in short_name_rows() {
        for (index, (clause, short_name)) in rows.iter().enumerate() {
            let case = format!("{}: {clause}", row.case);
            let name = format!("short{index}.example.com");
            let (status, parsed) = call(
                &router,
                row.operation,
                (row.body)(&name, Some(short_name.as_str())),
            )
            .await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected the short name to be refused, got {status} with \
                 {parsed}"
            );
            assert_eq!(
                parsed["__type"],
                json!("InvalidParameterException"),
                "case {case:?}: expected the modelled parameter refusal, got {parsed}"
            );
            let request_id = parsed["RequestId"].as_str().unwrap_or_default();
            assert!(
                !request_id.is_empty(),
                "case {case:?}: expected the refusal to report a RequestId, got {parsed}"
            );
            let message = parsed["Message"].as_str().unwrap_or_default();
            assert_eq!(
                message,
                format!("{PUBLISHED_RULE}{REQUEST_ID_SEPARATOR}{request_id}"),
                "case {case:?}: expected the published rule and the call's request id, got \
                 {message:?}"
            );
        }
    }

    assert_eq!(
        stored_count(&router, "after every refused create").await,
        before,
        "expected a refused short name to create no directory"
    );
}

/// RULE (the positive control for the same rule): a short name the rule admits is NOT
/// refused by it — the create is served and the directory reports the short name it was
/// given back.
#[tokio::test]
async fn a_short_name_the_rule_admits_is_served() {
    let router = service();

    // One row per form the admitted set takes: a plain name, every admitted special
    // character at once, the shortest name and the longest one.
    let at_maximum = at_the_maximum();
    let admitted: Vec<(&str, &str)> = vec![
        ("a plain alphanumeric name", "CORP2"),
        ("every admitted special character", "!@#$%^&()-_'{}~"),
        ("one character, at the published minimum", "C"),
        ("fifteen characters, at the published maximum", &at_maximum),
    ];

    for row in short_name_rows() {
        for (index, (clause, short_name)) in admitted.iter().enumerate() {
            let case = format!("{}: {clause}", row.case);
            let name = format!("admitted{index}-{}.example.com", operation_label(row.operation));
            let answer = ok(
                &router,
                row.operation,
                (row.body)(&name, Some(short_name)),
                &case,
            )
            .await;
            let directory_id = answer["DirectoryId"].as_str().unwrap_or_default().to_string();
            assert!(
                !directory_id.is_empty(),
                "case {case:?}: expected a created DirectoryId, got {answer}"
            );
            let described = ok(
                &router,
                "DescribeDirectories",
                json!({ "DirectoryIds": [&directory_id] }),
                &case,
            )
            .await;
            let reported = described["DirectoryDescriptions"][0]["ShortName"]
                .as_str()
                .unwrap_or_default();
            assert_eq!(
                reported, *short_name,
                "case {case:?}: expected the stored short name to be read back, got \
                 {described}"
            );
        }
    }
}

/// A label for a row's operation, so two rows of one table name two different directories.
fn operation_label(operation: &str) -> String {
    operation.to_lowercase()
}

/// RULE: a create that names NO short name is not refused by this rule — the member is one
/// the caller left to the service, which derives the directory's short name itself.
#[tokio::test]
async fn a_create_that_names_no_short_name_is_served() {
    let router = service();

    for row in short_name_rows() {
        let case = format!("{}: no short name named", row.case);
        let name = format!("derived-{}.example.com", operation_label(row.operation));
        let answer = ok(&router, row.operation, (row.body)(&name, None), &case).await;
        assert!(
            answer["DirectoryId"].as_str().is_some(),
            "case {case:?}: expected the create to be served, got {answer}"
        );
    }
}
