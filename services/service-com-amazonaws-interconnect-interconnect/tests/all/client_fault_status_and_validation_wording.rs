// SPDX-License-Identifier: CC0-1.0

//! What this service answers a fault in the REQUEST with: the status, and the words.
//!
//! Three rules, all about how a refusal is rendered rather than about which records
//! exist:
//!
//! * a `ConnectionId` that names no connection of the called endpoint is a CLIENT fault,
//!   so it is answered with the HTTP 400 this service answers a client fault with (the
//!   status its own `InterconnectClientException` and `InterconnectValidationException`
//!   carry) while keeping the modelled connection-scoped `ResourceNotFoundException` and
//!   its request-derived wording. The reads and the writes agree on it;
//! * that status is the SERVICE's and not one shape's: every handle a record this
//!   endpoint does not hold can be addressed by — its `ConnectionId`, the
//!   `activationKey` of the proposal it holds, an attach point naming it, its ARN — is
//!   refused with that one status, whichever modelled shape names the fault, so the
//!   status the model records on the not-found shape never reaches the wire;
//! * a value outside a modelled input constraint is refused with the generated
//!   validation envelope, which names the MEMBER PATH and the CONSTRAINT and nothing of
//!   the value the caller sent, and quotes a `@pattern` WITHOUT the `^`/`$` anchors the
//!   model spells it with.
//!
//! Each rule is asserted over a TABLE whose rows differ along what it generalises over:
//! the operation, the identifier's prefix and character classes, the Region the request
//! addresses, the handle the absent record is addressed by and the shape of the value it
//! carries, and — for the validation envelope — the member, the constraint kind
//! (`@pattern`, `@length`, `@range`, `@required`) and the number of violations in one
//! request.

use serde_json::{json, Value};

use service_com_amazonaws_interconnect_interconnect::state::{
    arn_resource_absent_message, attach_point_not_valid_message, connection_absent_message,
    connection_not_retrievable_message, proposal_not_retrievable_message,
};

use crate::emulator::{code, message, text, Emulator, DEFAULT_SIGNING_REGION};

/// The Regions the not-found rows address, so no row's answer depends on the harness
/// default Region.
const REGIONS: [&str; 3] = [DEFAULT_SIGNING_REGION, "eu-west-1", "ap-southeast-2"];

/// The status the model records on the connection-scoped not-found shape
/// (`smithy_query(query="shape",shape="ResourceNotFoundException")` -> `httpError: 404`),
/// which is the only shape in this service's model carrying it — and which this service's
/// wire never answers with.
const MODELED_ABSENT_RESOURCE_STATUS: u16 = 404;

/// `ConnectionId`s no create ever allocates, spanning what the member's pattern
/// (`^(mcc|lmcc)-[a-z0-9]{8}$`) allows: both prefixes, and suffixes of digits only,
/// letters only and both mixed.
const UNALLOCATED_IDS: [&str; 6] = [
    "mcc-00000000",
    "lmcc-00000000",
    "mcc-zzzzzzzz",
    "lmcc-zzzzzzzz",
    "mcc-0a1b2c3d",
    "lmcc-9z8y7x6w",
];

/// The `@pattern` the model gives `ConnectionBandwidth`, as the model spells it
/// (`smithy_query(query="shape",shape="ConnectionBandwidth")`).
const BANDWIDTH_PATTERN: &str = r"^\d+[MG]bps$";

/// The `@pattern` the model gives `ConnectionDescription`.
const DESCRIPTION_PATTERN: &str = "^[-a-zA-Z0-9_ ]+$";

/// The `@pattern` the model gives `ConnectionId`.
const CONNECTION_ID_PATTERN: &str = "^(mcc|lmcc)-[a-z0-9]{8}$";

/// The `@pattern` the model gives `AmazonResourceName`, which `TagResource$arn` targets.
const ARN_PATTERN: &str =
    "^arn:aws[a-z-]*:interconnect:[^:]+:[0-9]{12}:connection/(mcc|lmcc)-[a-z0-9]{8}$";

/// An ARN inside `AmazonResourceName`'s `@length(59, 150)` that its pattern does not
/// admit (the id is upper-cased), so the row is refused for the pattern alone.
const ARN_OUTSIDE_THE_PATTERN: &str =
    "arn:aws:interconnect:us-east-1:123456789012:connection/MCC-00000000";

/// The expression a `@pattern` requirement quotes: the model's own, without its anchors.
fn unanchored(pattern: &str) -> &str {
    let body = pattern.strip_prefix('^').unwrap_or(pattern);
    body.strip_suffix('$').unwrap_or(body)
}

/// The clause the envelope carries for a member that violated a `@pattern`.
fn pattern_clause(path: &str, pattern: &str) -> String {
    format!(
        "Value at '{path}' failed to satisfy constraint: Member must satisfy regular expression pattern: {}",
        unanchored(pattern)
    )
}

/// One row of the validation table.
struct ValidationCase {
    label: &'static str,
    operation: &'static str,
    body: Value,
    /// The clauses the envelope must carry, in no particular order.
    clauses: Vec<String>,
    /// What the caller sent, which the message must not reproduce.
    sent: Vec<String>,
}

/// RULE: a `ConnectionId` that names no connection of the called endpoint is answered
/// with the HTTP 400 this service gives a client fault, under the modelled
/// `ResourceNotFoundException` and in each operation's own request-derived wording —
/// from the reads and from the write alike, in every Region.
#[tokio::test]
async fn a_connection_identifier_that_names_nothing_is_a_client_fault() {
    let emulator = Emulator::new();

    for region in REGIONS {
        for identifier in UNALLOCATED_IDS {
            // Rows differ in the operation and in the wording it builds: the two reads
            // state that the retrieval cannot be served, the update that no connection
            // exists for the identifier it was asked to modify.
            let rows: [(&str, Value, String); 3] = [
                (
                    "GetConnection",
                    json!({ "identifier": identifier }),
                    connection_not_retrievable_message(identifier),
                ),
                (
                    "DeleteConnection",
                    json!({ "identifier": identifier }),
                    connection_not_retrievable_message(identifier),
                ),
                (
                    "UpdateConnection",
                    json!({ "identifier": identifier, "description": "a new description" }),
                    connection_absent_message(identifier),
                ),
            ];

            for (operation, body, expected) in rows {
                let case = format!("{region} / {operation} / {identifier}");
                let (status, answered) = emulator.ic_in(region, operation, body).await;
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected HTTP 400 for an identifier that names no connection, got {status} with {answered:?}"
                );
                assert!(
                    code(&answered).contains("ResourceNotFoundException"),
                    "case {case:?}: expected the modelled ResourceNotFoundException, got {:?}",
                    code(&answered)
                );
                let reported = message(&answered);
                assert_eq!(
                    reported, expected,
                    "case {case:?}: expected this operation's request-derived wording, got {reported:?}"
                );
            }
        }
    }

    // The wording itself, spelled out rather than taken from the helper the handlers
    // build it with: the service's envelope, each operation's own sentence, and the
    // identifier the REQUEST carried interpolated into it.
    let identifier = "mcc-0a1b2c3d";
    for (operation, body, expected) in [
        (
            "GetConnection",
            json!({ "identifier": identifier }),
            format!(
                "Unable to process request: Unable to retrieve connection {identifier}, as it does not exist"
            ),
        ),
        (
            "UpdateConnection",
            json!({ "identifier": identifier, "description": "a new description" }),
            format!("Unable to process request: Connection does not exist for {identifier}"),
        ),
    ] {
        let (status, answered) = emulator.ic(operation, body).await;
        assert_eq!(
            status, 400,
            "case {operation:?}: expected HTTP 400 for an identifier that names no connection, got {status} with {answered:?}"
        );
        let reported = message(&answered);
        assert_eq!(
            reported, expected,
            "case {operation:?}: expected {expected:?}, got {reported:?}"
        );
    }
}

/// RULE: the status above is the one a MISSING connection gets, not the one every call
/// gets — a connection the endpoint does hold is still served by the same operations.
#[tokio::test]
async fn a_connection_the_endpoint_holds_is_still_served() {
    let emulator = Emulator::new();
    let attach_point = emulator.attach_point().await;
    let catalog = emulator.environments().await;
    let environment_id = text(&catalog[0], "environmentId");
    let bandwidth = crate::emulator::available_bandwidths(&catalog[0])
        .into_iter()
        .next()
        .expect("the published Environment offers a bandwidth");

    let created = emulator
        .create_connection(&environment_id, &bandwidth, &attach_point, "served link")
        .await;
    let id = text(&created, "id");

    for operation in ["GetConnection", "UpdateConnection"] {
        let body = if operation == "UpdateConnection" {
            json!({ "identifier": id, "description": "renamed link" })
        } else {
            json!({ "identifier": id })
        };
        let (status, answered) = emulator.ic(operation, body).await;
        assert_eq!(
            status, 200,
            "case {operation:?}: expected the connection the endpoint holds to be served, got {status} with {answered:?}"
        );
    }
}

/// Every handle one and the same connection record is reachable by, as the table of
/// `(handle, operation, request)` rows [`every_handle_to_an_absent_record_answers_one_status`]
/// drives.
///
/// `withheld` is text the refusal must NOT reproduce: a value of a `@sensitive` member.
struct AbsentHandleCase {
    handle: String,
    operation: &'static str,
    body: Value,
    code: &'static str,
    message: String,
    withheld: Option<String>,
}

/// The absent-handle table: one row per (handle, operation) pair, with the handles
/// spanning the three forms a request addresses a connection record by — a
/// `ConnectionId`, the `activationKey` of the proposal the record holds, and an attach
/// point naming a connection (a Direct Connect gateway id, or a connection ARN) — and
/// the rows differing in the shape of the value each handle carries as well as in the
/// operation it arrives on.
async fn absent_handle_table(emulator: &Emulator) -> Vec<AbsentHandleCase> {
    let held = emulator.attach_point().await;
    let placement = crate::emulator::placements(emulator)
        .await
        .into_iter()
        .next()
        .expect("the catalog offers a placement");
    let environment_id = placement.0.clone();
    // One real proposal, so the attach-point rows below reach the handle they are
    // testing instead of stopping at an `activationKey` that names nothing.
    let create = crate::emulator::create_request(
        emulator,
        DEFAULT_SIGNING_REGION,
        &placement,
        0,
        None,
        None,
    )
    .await;
    let live_key = text(
        &emulator.ic_ok("CreateConnection", create.clone()).await["connection"],
        "activationKey",
    );

    let mut rows = Vec::new();

    // The `ConnectionId` handle: ids no create ever allocates, spanning both prefixes
    // the member's pattern admits.
    for identifier in ["mcc-00000000", "lmcc-9z8y7x6w"] {
        rows.push(AbsentHandleCase {
            handle: format!("a ConnectionId of {identifier}"),
            operation: "GetConnection",
            body: json!({ "identifier": identifier }),
            code: "ResourceNotFoundException",
            message: connection_not_retrievable_message(identifier),
            withheld: None,
        });
        rows.push(AbsentHandleCase {
            handle: format!("a ConnectionId of {identifier}"),
            operation: "DeleteConnection",
            body: json!({ "identifier": identifier }),
            code: "ResourceNotFoundException",
            message: connection_not_retrievable_message(identifier),
            withheld: None,
        });
        rows.push(AbsentHandleCase {
            handle: format!("a ConnectionId of {identifier}"),
            operation: "UpdateConnection",
            body: json!({ "identifier": identifier, "description": "a new description" }),
            code: "ResourceNotFoundException",
            message: connection_absent_message(identifier),
            withheld: None,
        });
    }

    // The `activationKey` handle on the very same kind of record: keys that are base64
    // text — so each row is resolved rather than refused for its encoding — of several
    // shapes and lengths, including the published `ActivationKey` maximum
    // (`@length(min: 1, max: 2048)`).
    let keys = [
        ("never issued", "bm9Qb3J0YWxFdmVySXNzdWVkVGhpcw".to_string()),
        ("a prefix of a live one", live_key[..live_key.len() - 1].to_string()),
        ("the shortest base64 text", "AA".to_string()),
        ("the published maximum length", "A".repeat(2048)),
    ];
    for (shape, key) in &keys {
        rows.push(AbsentHandleCase {
            handle: format!("an activationKey, {shape}"),
            operation: "DescribeConnectionProposal",
            body: json!({ "activationKey": key }),
            code: "ResourceNotFoundException",
            message: proposal_not_retrievable_message(),
            withheld: Some(key.clone()),
        });
        rows.push(AbsentHandleCase {
            handle: format!("an activationKey, {shape}"),
            operation: "AcceptConnectionProposal",
            body: json!({
                "activationKey": key,
                "attachPoint": { "directConnectGateway": held },
            }),
            code: "ResourceNotFoundException",
            message: proposal_not_retrievable_message(),
            withheld: Some(key.clone()),
        });
    }

    // The attach-point handle, in both members the `AttachPoint` union carries
    // (`smithy_query(query="shape",shape="AttachPoint")`): a gateway id of the shape the
    // member's pattern admits, and a connection ARN of this account and Region — the
    // third handle on a connection record — each naming nothing this endpoint holds.
    let absent_points = [
        (
            "a Direct Connect gateway id".to_string(),
            "00000000-0000-4000-8000-000000000000".to_string(),
            json!({ "directConnectGateway": "00000000-0000-4000-8000-000000000000" }),
        ),
        (
            "a connection ARN".to_string(),
            format!(
                "arn:aws:interconnect:{DEFAULT_SIGNING_REGION}:{}:connection/mcc-00000000",
                engine::EMULATOR_ACCOUNT_ID
            ),
            json!({
                "arn": format!(
                    "arn:aws:interconnect:{DEFAULT_SIGNING_REGION}:{}:connection/mcc-00000000",
                    engine::EMULATOR_ACCOUNT_ID
                ),
            }),
        ),
    ];
    for (shape, identifier, attach_point) in &absent_points {
        let mut create_with = create.clone();
        create_with["attachPoint"] = attach_point.clone();
        rows.push(AbsentHandleCase {
            handle: format!("an attach point naming nothing, {shape}"),
            operation: "CreateConnection",
            body: create_with,
            code: "ResourceNotFoundException",
            message: attach_point_not_valid_message(identifier, &environment_id),
            withheld: None,
        });
        rows.push(AbsentHandleCase {
            handle: format!("an attach point naming nothing, {shape}"),
            operation: "AcceptConnectionProposal",
            body: json!({ "activationKey": live_key, "attachPoint": attach_point }),
            code: "ResourceNotFoundException",
            message: attach_point_not_valid_message(identifier, &environment_id),
            withheld: None,
        });
    }

    // The ARN handle of the tagging operations on a record this endpoint does not hold:
    // a different modelled CLASS (this service's own client exception, in its own fixed
    // wording, which `arn_scope_refusals` pins) and therefore the row that shows the
    // status is the service's and not one shape's.
    for operation in ["TagResource", "UntagResource"] {
        let arn = format!(
            "arn:aws:interconnect:{DEFAULT_SIGNING_REGION}:{}:connection/mcc-00000000",
            engine::EMULATOR_ACCOUNT_ID
        );
        let body = if operation == "TagResource" {
            json!({ "arn": arn, "tags": { "env": "test" } })
        } else {
            json!({ "arn": arn, "tagKeys": ["env"] })
        };
        rows.push(AbsentHandleCase {
            handle: "an ARN naming nothing".to_string(),
            operation,
            body,
            code: "InterconnectClientException",
            message: arn_resource_absent_message(),
            withheld: None,
        });
    }

    rows
}

/// RULE: a request naming a resource this endpoint does not hold is a fault in the
/// REQUEST, so it is answered with the HTTP 400 this service answers every client fault
/// with — whichever handle the absent resource was addressed by and whichever modelled
/// shape names the fault — while the shape and the wording stay the operation's own.
/// The status the model records on the connection-scoped not-found shape is not what
/// this service's wire answers, so no refusal here is ever served with it: one absent
/// record cannot have two statuses.
#[tokio::test]
async fn every_handle_to_an_absent_record_answers_one_status() {
    let emulator = Emulator::new();
    let table = absent_handle_table(&emulator).await;
    assert!(
        table.len() >= 12,
        "case \"coverage\": expected the table to span every handle on several shapes, got {} rows",
        table.len()
    );

    let mut statuses = std::collections::BTreeSet::new();
    for case in &table {
        let label = format!("{} / {}", case.operation, case.handle);
        let (status, body) = emulator.ic(case.operation, case.body.clone()).await;
        statuses.insert(status);
        assert_eq!(
            status, 400,
            "case {label:?}: expected HTTP 400 for a handle naming nothing this endpoint holds, got {status} with {body:?}"
        );
        assert_ne!(
            status, MODELED_ABSENT_RESOURCE_STATUS,
            "case {label:?}: expected the modelled REST status never to reach the wire, got {status} with {body:?}"
        );
        assert!(
            code(&body).contains(case.code),
            "case {label:?}: expected the modelled {}, got {:?}",
            case.code,
            code(&body)
        );
        let reported = message(&body);
        assert_eq!(
            reported, case.message,
            "case {label:?}: expected this operation's own refusal wording, got {reported:?}"
        );
        if let Some(withheld) = &case.withheld {
            assert!(
                !reported.contains(withheld.as_str()),
                "case {label:?}: expected the refusal not to reproduce the @sensitive value, got {reported:?}"
            );
        }
    }
    assert_eq!(
        statuses.len(),
        1,
        "case \"one record, one status\": expected every absent handle to answer one status, got {statuses:?}"
    );
}

/// RULE: the status above is what a refusal carries, not what every call carries — the
/// handles that DO resolve are still served, by the identifier and by the activation key
/// alike.
#[tokio::test]
async fn the_handles_a_record_does_answer_for_are_still_served() {
    let emulator = Emulator::new();
    let placement = crate::emulator::placements(&emulator)
        .await
        .into_iter()
        .next()
        .expect("the catalog offers a placement");
    let request =
        crate::emulator::create_request(&emulator, DEFAULT_SIGNING_REGION, &placement, 1, None, None)
            .await;
    let created = emulator.ic_ok("CreateConnection", request).await["connection"].clone();

    for (operation, body) in [
        (
            "GetConnection",
            json!({ "identifier": text(&created, "id") }),
        ),
        (
            "DescribeConnectionProposal",
            json!({ "activationKey": text(&created, "activationKey") }),
        ),
    ] {
        let (status, answered) = emulator.ic(operation, body).await;
        assert_eq!(
            status, 200,
            "case {operation:?}: expected the handle that resolves to be served, got {status} with {answered:?}"
        );
    }
}

/// The validation table: one row per modelled constraint kind this service's inputs
/// carry, plus a request that violates two at once.
fn validation_table() -> Vec<ValidationCase> {
    vec![
        ValidationCase {
            label: "a bandwidth outside ConnectionBandwidth's pattern",
            operation: "UpdateConnection",
            body: json!({ "identifier": "mcc-00000000", "bandwidth": "10xbps" }),
            clauses: vec![pattern_clause("bandwidth", BANDWIDTH_PATTERN)],
            sent: vec!["10xbps".to_string()],
        },
        ValidationCase {
            label: "a bandwidth longer than ConnectionBandwidth's @length",
            operation: "UpdateConnection",
            body: json!({ "identifier": "mcc-00000000", "bandwidth": "10000Gbps" }),
            clauses: vec![
                "Value at 'bandwidth' failed to satisfy constraint: Member must have length less than or equal to 8"
                    .to_string(),
            ],
            sent: vec!["10000Gbps".to_string()],
        },
        ValidationCase {
            label: "a description outside ConnectionDescription's pattern",
            operation: "UpdateConnection",
            body: json!({ "identifier": "mcc-00000000", "description": "bad*chars" }),
            clauses: vec![pattern_clause("description", DESCRIPTION_PATTERN)],
            sent: vec!["bad*chars".to_string()],
        },
        ValidationCase {
            label: "a description longer than ConnectionDescription's @length",
            operation: "UpdateConnection",
            body: json!({ "identifier": "mcc-00000000", "description": "d".repeat(256) }),
            clauses: vec![
                "Value at 'description' failed to satisfy constraint: Member must have length less than or equal to 255"
                    .to_string(),
            ],
            sent: vec!["d".repeat(256)],
        },
        ValidationCase {
            label: "a read identifier outside ConnectionId's pattern",
            operation: "GetConnection",
            body: json!({ "identifier": "not-an-id" }),
            clauses: vec![pattern_clause("identifier", CONNECTION_ID_PATTERN)],
            sent: vec!["not-an-id".to_string()],
        },
        ValidationCase {
            label: "an upper-cased identifier the pattern does not admit",
            operation: "DeleteConnection",
            body: json!({ "identifier": "MCC-00000000" }),
            clauses: vec![pattern_clause("identifier", CONNECTION_ID_PATTERN)],
            sent: vec!["MCC-00000000".to_string()],
        },
        ValidationCase {
            label: "an ARN outside AmazonResourceName's pattern",
            operation: "TagResource",
            body: json!({ "arn": ARN_OUTSIDE_THE_PATTERN, "tags": { "env": "test" } }),
            clauses: vec![pattern_clause("arn", ARN_PATTERN)],
            sent: vec![ARN_OUTSIDE_THE_PATTERN.to_string()],
        },
        ValidationCase {
            label: "a maxResults above MaxResults' @range",
            operation: "ListConnections",
            body: json!({ "maxResults": 101 }),
            clauses: vec![
                "Value at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 100"
                    .to_string(),
            ],
            sent: vec!["101".to_string()],
        },
        ValidationCase {
            label: "a maxResults below MaxResults' @range",
            operation: "ListEnvironments",
            body: json!({ "maxResults": 0 }),
            clauses: vec![
                "Value at 'maxResults' failed to satisfy constraint: Member must have value greater than or equal to 1"
                    .to_string(),
            ],
            sent: Vec::new(),
        },
        ValidationCase {
            label: "a required member left out altogether",
            operation: "GetConnection",
            body: json!({}),
            clauses: vec![
                "Value null at 'identifier' failed to satisfy constraint: Member must not be null"
                    .to_string(),
            ],
            sent: Vec::new(),
        },
        ValidationCase {
            label: "two members outside their patterns at once",
            operation: "UpdateConnection",
            body: json!({ "identifier": "not-an-id", "bandwidth": "10xbps" }),
            clauses: vec![
                pattern_clause("identifier", CONNECTION_ID_PATTERN),
                pattern_clause("bandwidth", BANDWIDTH_PATTERN),
            ],
            sent: vec!["not-an-id".to_string(), "10xbps".to_string()],
        },
    ]
}

/// RULE: a value outside a modelled input constraint is refused with the generated
/// validation envelope, which states the member path and the requirement, quotes a
/// `@pattern` without its anchors, and reproduces nothing of the value the caller sent.
#[tokio::test]
async fn a_refused_value_is_stated_without_being_echoed() {
    let emulator = Emulator::new();

    for case in validation_table() {
        let label = case.label;
        let (status, body) = emulator.ic(case.operation, case.body.clone()).await;
        assert_eq!(
            status, 400,
            "case {label:?}: expected HTTP 400 for a value outside its constraint, got {status} with {body:?}"
        );
        assert!(
            code(&body).contains("ValidationException"),
            "case {label:?}: expected a ValidationException, got {:?}",
            code(&body)
        );

        let reported = message(&body);
        let count = case.clauses.len();
        let opening = if count == 1 {
            "1 validation error detected: ".to_string()
        } else {
            format!("{count} validation errors detected: ")
        };
        assert!(
            reported.starts_with(&opening),
            "case {label:?}: expected the envelope to open {opening:?}, got {reported:?}"
        );
        for clause in &case.clauses {
            assert!(
                reported.contains(clause),
                "case {label:?}: expected the envelope to state {clause:?}, got {reported:?}"
            );
        }
        // Exactly the clauses, nothing else: the length of the rendered envelope is the
        // opening plus the clauses joined with "; ".
        let expected_length = opening.len()
            + case.clauses.iter().map(String::len).sum::<usize>()
            + 2 * count.saturating_sub(1);
        assert_eq!(
            reported.len(),
            expected_length,
            "case {label:?}: expected the envelope to carry {count} clause(s) and nothing else, got {reported:?}"
        );
        // The value is never reproduced, and a quoted pattern carries no anchors — the
        // `^` inside a character class (`[^:]`) is part of the expression, so what is
        // checked is the first and last character of the expression itself.
        for sent in &case.sent {
            assert!(
                !reported.contains(sent.as_str()),
                "case {label:?}: expected the refusal not to echo the submitted value {sent:?}, got {reported:?}"
            );
        }
        for quoted in reported.split("regular expression pattern: ").skip(1) {
            let expression = quoted.split("; ").next().unwrap_or_default();
            assert!(
                !expression.starts_with('^') && !expression.ends_with('$'),
                "case {label:?}: expected the quoted pattern to carry no anchors, got {expression:?}"
            );
        }
    }
}
