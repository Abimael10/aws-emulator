// SPDX-License-Identifier: CC0-1.0

//! WHICH fault a refusal reports when the request carries one of its own AND names a
//! resource this store has a verdict about, driven over the wire through the shared
//! [`crate::emulator`] harness.
//!
//! RULE: a fault the REQUEST itself carries — a tag write this service does not store,
//! a value outside the modelled constraints of the member that carries it, including
//! the union members nothing before the handler holds to them, or a `nextToken` the
//! addressed listing never issued — is refused as invalid input (the modelled
//! `InterconnectValidationException`, HTTP 400, worded for that fault) BEFORE anything
//! the request NAMES is resolved, so no verdict about a resource stands in its place:
//! not a handle that resolves to nothing, not a proposal whose acceptance has already
//! been made, not a connection an acceptance can no longer be made on, not an
//! `environmentId` the called Region does not offer and not a `clientToken` already used
//! for another request. A request carrying no fault of its own still gets the resource
//! verdict, unchanged, and a refused request writes nothing.
//!
//! The rows differ along what the rule generalises over: the KIND of fault the request
//! carries (a reserved tag key in either casing, a tag key and a tag value past their
//! modelled lengths, more tags than one resource holds, a value outside each
//! `AttachPoint` member's own constraints, an identifier outside the only member
//! `RemoteAccountIdentifier` offers, each form of a page cursor this endpoint did not
//! mint), the RESOURCE VERDICT it is paired with, the operation that carries it, the
//! environment and bandwidth it is placed on, the page size asked for, and the Region
//! the request is signed for.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use service_com_amazonaws_interconnect_interconnect::state::{
    client_token_reused_message, cursor_not_issued_message, environment_not_found_message,
    not_a_connection_arn_message, not_a_gateway_identifier_message,
    proposal_already_accepted_message, proposal_not_acceptable_message,
    proposal_not_retrievable_message, MAX_TAGS_PER_RESOURCE, REFUSAL_ENVELOPE,
    REMOTE_IDENTIFIER_LENGTH, RESERVED_TAG_KEY_PREFIX, TAG_KEY_LENGTH,
};

use crate::emulator::{
    code, creatable_environments, identifiers, message, placements_in, remote_account_for, tags_of,
    text, Emulator, DEFAULT_SIGNING_REGION,
};

/// The error this service models for a fault in the request's own input (HTTP 400).
const VALIDATION_CODE: &str = "InterconnectValidationException";

/// The error a handle that resolves to no resource is answered with; like every fault
/// in a request, it carries this service's HTTP 400.
const NOT_FOUND_CODE: &str = "ResourceNotFoundException";

/// The error this service models for its own client faults (HTTP 400).
const CLIENT_CODE: &str = "InterconnectClientException";

/// A second Region, so the rule is checked across the Region a request is signed for as
/// well as across the fault it carries.
const OTHER_REGION: &str = "eu-west-1";

/// The gateways the rows attach to: an attach point IS a Direct Connect gateway of the
/// account, so the suite creates its own and the rows differ in which one they name and
/// in each gateway's `amazonSideAsn`.
const GATEWAY_ROWS: [(&str, Option<i64>); 2] =
    [("ic-order-a", None), ("ic-order-b", Some(64_513))];

/// The terminal `ConnectionState` a completed `DeleteConnection` settles into.
const DELETED_STATE: &str = "deleted";

/// How long a wait for a deleted connection to reach its terminal state runs before the
/// case fails; the suite polls rather than sleeping for a span, and the deadline is far
/// past the span the clock owes so a loaded host cannot decide a case.
const DELETED_BUDGET: Duration = Duration::from_secs(60);

/// How often that wait re-reads.
const DELETED_INTERVAL: Duration = Duration::from_millis(10);

// ---------------------------------------------------------------------------
// The faults a request carries on its own
// ---------------------------------------------------------------------------

/// One fault in the REQUEST: the members to send and how the service words the refusal.
#[derive(Clone)]
struct RequestFault {
    name: String,
    /// The request members this row sets, written over the well-formed ones.
    members: Vec<(&'static str, Value)>,
    /// The refusal this fault is worded with.
    message: String,
}

fn fault(name: &str, members: Vec<(&'static str, Value)>, message: String) -> RequestFault {
    RequestFault {
        name: name.to_string(),
        members,
        message,
    }
}

/// A `tags` member carrying `pairs`.
fn tags(pairs: Vec<(String, String)>) -> Value {
    json!(pairs.into_iter().collect::<BTreeMap<String, String>>())
}

/// One entry, as the `tags` member carries it.
fn one_tag(key: &str, value: &str) -> Value {
    tags(vec![(key.to_string(), value.to_string())])
}

/// How the service words a key inside the namespace AWS keeps for its own system tags.
fn reserved_key_message(key: &str) -> String {
    format!(
        "{REFUSAL_ENVELOPE}the tag key {key} is in the {RESERVED_TAG_KEY_PREFIX} namespace that AWS reserves for its own tags, which a caller cannot create, change or remove"
    )
}

/// The tag writes this service refuses whatever the request names: a key in the reserved
/// namespace, a key outside the length the model gives `TagKey`
/// (`smithy_query(query="shape",shape="TagKey")`), and more entries than one resource
/// holds. A tag VALUE is not among them: the boundary validator holds a map's values to
/// their target's constraints, so one past `TagValue`'s length never reaches a handler at
/// all, while a map's KEYS and the entry count are the handler's to judge.
fn tag_faults() -> Vec<RequestFault> {
    let (key_min, key_max) = TAG_KEY_LENGTH;
    let long_key = "k".repeat(key_max + 1);
    // One entry past the number of tags a resource holds, every key of them legal, so
    // what refuses the row is the count and nothing else.
    let too_many: Vec<(String, String)> = (0..=MAX_TAGS_PER_RESOURCE)
        .map(|index| (format!("order-tag-{index}"), "yes".to_string()))
        .collect();
    let too_many_count = too_many.len();
    vec![
        fault(
            "a tag key in the namespace AWS reserves, beside a legal entry",
            vec![(
                "tags",
                tags(vec![
                    (
                        "aws:cloudformation:stack-name".to_string(),
                        "mystack".to_string(),
                    ),
                    ("env".to_string(), "test".to_string()),
                ]),
            )],
            reserved_key_message("aws:cloudformation:stack-name"),
        ),
        fault(
            "the reserved namespace upper-cased",
            vec![("tags", one_tag("AWS:owner", "ops"))],
            reserved_key_message("AWS:owner"),
        ),
        fault(
            "a tag key one character past its modelled length",
            vec![("tags", one_tag(&long_key, "yes"))],
            format!(
                "{REFUSAL_ENVELOPE}a tag key must be between {key_min} and {key_max} characters long, and one of length {} is not",
                key_max + 1
            ),
        ),
        fault(
            "one more tag than a resource holds",
            vec![("tags", tags(too_many))],
            format!(
                "{REFUSAL_ENVELOPE}a connection holds at most {MAX_TAGS_PER_RESOURCE} tags, and this request would leave it with {too_many_count}"
            ),
        ),
    ]
}

/// The values an `AttachPoint` member cannot hold: the constraint sits on a union member
/// one level down, which the boundary validator does not reach, so each is a fault the
/// handler itself judges. The rows span both members and several ways of leaving a
/// member's `@pattern`/`@length`.
fn attach_point_faults() -> Vec<RequestFault> {
    let rows: Vec<(&str, &str, &str)> = vec![
        (
            "a directConnectGateway that is not a UUID at all",
            "directConnectGateway",
            "not-a-uuid",
        ),
        (
            "a directConnectGateway with a character outside the hex alphabet",
            "directConnectGateway",
            "0000000g-0000-4000-8000-000000000000",
        ),
        (
            "an empty directConnectGateway",
            "directConnectGateway",
            "",
        ),
        ("an arn that is not an ARN at all", "arn", "not-an-arn"),
        (
            "an arn of another resource type",
            "arn",
            "arn:aws:interconnect:us-east-1:000000000000:environment/mcc-00000000",
        ),
    ];
    rows.into_iter()
        .map(|(name, member, value)| {
            let message = if member == "arn" {
                not_a_connection_arn_message(value)
            } else {
                not_a_gateway_identifier_message(value)
            };
            let mut attach_point = Map::new();
            attach_point.insert(member.to_string(), json!(value));
            fault(
                name,
                vec![("attachPoint", Value::Object(attach_point))],
                message,
            )
        })
        .collect()
}

/// The identifiers the only member `RemoteAccountIdentifier` offers cannot hold —
/// the sibling union, constrained the same way one level down, so the same rule reaches
/// it. `CreateConnection` is the only operation that carries this member.
fn remote_account_faults() -> Vec<RequestFault> {
    let (min, max) = REMOTE_IDENTIFIER_LENGTH;
    let too_long = "a".repeat(max + 1);
    vec![
        fault(
            "a remoteAccount identifier carrying a character outside the member's alphabet",
            vec![("remoteAccount", json!({ "identifier": "partner identity" }))],
            format!(
                "{REFUSAL_ENVELOPE}remoteAccount.identifier must be made up of letters, digits and the characters -, _, @ and ., and this one is not"
            ),
        ),
        fault(
            "a remoteAccount identifier one character past the member's modelled length",
            vec![(
                "remoteAccount",
                json!({ "identifier": too_long.clone() }),
            )],
            format!(
                "{REFUSAL_ENVELOPE}remoteAccount.identifier must be between {min} and {max} characters long, and one of length {} is not",
                max + 1
            ),
        ),
    ]
}

// ---------------------------------------------------------------------------
// The calls
// ---------------------------------------------------------------------------

/// A `CreateConnection` body for `placement` in `region`, with `overrides` written over
/// it — the identifying information the Environment asks for included, so what a row
/// refuses is the member it set and nothing the Environment needed.
async fn create_body(
    emulator: &Emulator,
    region: &str,
    placement: &(String, String),
    gateway: &str,
    overrides: &[(&'static str, Value)],
) -> Value {
    let mut request = Map::new();
    request.insert("environmentId".to_string(), json!(placement.0));
    request.insert("bandwidth".to_string(), json!(placement.1));
    request.insert(
        "attachPoint".to_string(),
        json!({ "directConnectGateway": gateway }),
    );
    let environment = emulator.environment_in(region, &placement.0).await;
    if let Some(remote_account) = remote_account_for(&environment) {
        request.insert("remoteAccount".to_string(), remote_account);
    }
    for (member, value) in overrides {
        request.insert((*member).to_string(), value.clone());
    }
    Value::Object(request)
}

/// `CreateConnection` in `region`, as a `(status, body)` pair so a refusal is readable.
async fn create(
    emulator: &Emulator,
    region: &str,
    placement: &(String, String),
    gateway: &str,
    overrides: &[(&'static str, Value)],
) -> (u16, Value) {
    let body = create_body(emulator, region, placement, gateway, overrides).await;
    emulator.ic_in(region, "CreateConnection", body).await
}

/// A connection awaiting its acceptance, created on `region`'s own catalog.
async fn proposal(
    emulator: &Emulator,
    region: &str,
    placement: &(String, String),
    gateway: &str,
    description: &str,
) -> Value {
    let (status, body) = create(
        emulator,
        region,
        placement,
        gateway,
        &[("description", json!(description))],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup {region:?}: expected a proposal on {gateway:?}, got {status} with {body:?}"
    );
    body["connection"].clone()
}

/// `AcceptConnectionProposal` in `region`, with `overrides` written over a request whose
/// every member is well formed.
async fn accept(
    emulator: &Emulator,
    region: &str,
    activation_key: &str,
    attach_point: &Value,
    overrides: &[(&'static str, Value)],
) -> (u16, Value) {
    let mut request = Map::new();
    request.insert("activationKey".to_string(), json!(activation_key));
    request.insert("attachPoint".to_string(), attach_point.clone());
    for (member, value) in overrides {
        request.insert((*member).to_string(), value.clone());
    }
    emulator
        .ic_in(region, "AcceptConnectionProposal", Value::Object(request))
        .await
}

/// How many connections `region`'s endpoint holds, so a refused create can be shown to
/// have allocated nothing.
async fn connection_count(emulator: &Emulator, region: &str) -> usize {
    let listed = emulator
        .ic_ok_in(region, "ListConnections", json!({ "maxResults": 100 }))
        .await;
    listed["connections"]
        .as_array()
        .map(Vec::len)
        .unwrap_or_default()
}

/// Delete `identifier` and poll until `region`'s endpoint reports it in the terminal
/// deleted state, which is the state a connection no acceptance can be made on reports.
async fn deleted_connection(emulator: &Emulator, region: &str, identifier: &str) -> String {
    let (status, body) = emulator
        .ic_in(region, "DeleteConnection", json!({ "identifier": identifier }))
        .await;
    assert_eq!(
        status, 200,
        "setup {identifier:?}: expected the delete to be accepted, got {status} with {body:?}"
    );
    let started = Instant::now();
    loop {
        let read = emulator
            .ic_ok_in(region, "GetConnection", json!({ "identifier": identifier }))
            .await;
        let state = text(&read["connection"], "state");
        if state == DELETED_STATE {
            return state;
        }
        assert!(
            started.elapsed() < DELETED_BUDGET,
            "setup {identifier:?}: expected the connection to reach {DELETED_STATE:?}, it reports {state:?}"
        );
        tokio::time::sleep(DELETED_INTERVAL).await;
    }
}

// ---------------------------------------------------------------------------
// The acceptance
// ---------------------------------------------------------------------------

/// One verdict this store has about a resource an acceptance names, and the well-formed
/// request that reaches it.
struct AcceptVerdict {
    name: String,
    region: &'static str,
    activation_key: String,
    /// An attach point every member constraint admits, so a request carrying no fault of
    /// its own reaches the resource verdict.
    attach_point: Value,
    status: u16,
    code: &'static str,
    message: String,
}

/// RULE: `AcceptConnectionProposal` reports the fault the REQUEST carries — the modelled
/// validation error, worded for that fault — before it resolves the proposal the
/// `activationKey` names, so neither a key that resolves to nothing, nor a proposal
/// already accepted, nor a connection no acceptance can be made on answers in its place;
/// a request carrying no such fault still gets the resource verdict, and every refusal
/// leaves the records exactly as they were.
#[tokio::test]
async fn an_acceptances_own_faults_are_reported_before_its_proposal_is_resolved() {
    let emulator = Emulator::new();
    let gateways = emulator.ready_gateways(&GATEWAY_ROWS).await;
    let (first, second) = (gateways[0].clone(), gateways[1].clone());

    let here = placements_in(&emulator, DEFAULT_SIGNING_REGION).await;
    let there = placements_in(&emulator, OTHER_REGION).await;
    assert!(
        !here.is_empty() && !there.is_empty(),
        "case \"catalog\": expected both Regions to offer a placement, got {} and {}",
        here.len(),
        there.len()
    );

    // A proposal whose acceptance has already been made, in the default Region.
    let accepted = proposal(
        &emulator,
        DEFAULT_SIGNING_REGION,
        &here[0],
        &first,
        "order accepted proposal",
    )
    .await;
    let accepted_id = text(&accepted, "id");
    let held_tags = one_tag("env", "test");
    let (status, body) = accept(
        &emulator,
        DEFAULT_SIGNING_REGION,
        &text(&accepted, "activationKey"),
        &json!({ "directConnectGateway": first }),
        &[("tags", held_tags.clone())],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup \"the acceptance that is made\": expected it to be accepted, got {status} with {body:?}"
    );

    // A connection an acceptance can no longer be made on, in the other Region.
    let torn_down = proposal(
        &emulator,
        OTHER_REGION,
        &there[there.len() - 1],
        &second,
        "order deleted proposal",
    )
    .await;
    let torn_down_id = text(&torn_down, "id");
    let torn_down_state = deleted_connection(&emulator, OTHER_REGION, &torn_down_id).await;

    let verdicts = vec![
        AcceptVerdict {
            name: "an activationKey that was never issued".to_string(),
            region: DEFAULT_SIGNING_REGION,
            // Base64 text — the encoding is what makes a value an activation key at
            // all, so a key that merely names nothing has to be well-formed for this
            // row to reach the resource verdict.
            activation_key: "bm9Qb3J0YWxFdmVySXNzdWVkVGhpcw".to_string(),
            attach_point: json!({ "directConnectGateway": first }),
            status: 400,
            code: NOT_FOUND_CODE,
            message: proposal_not_retrievable_message(),
        },
        AcceptVerdict {
            name: "a proposal whose acceptance has already been made".to_string(),
            region: DEFAULT_SIGNING_REGION,
            activation_key: text(&accepted, "activationKey"),
            // The OTHER gateway, so the well-formed request asks for something the
            // connection does not hold and reaches the already-accepted verdict rather
            // than the idempotent repeat.
            attach_point: json!({ "directConnectGateway": second }),
            status: 400,
            code: CLIENT_CODE,
            message: proposal_already_accepted_message(&accepted_id),
        },
        AcceptVerdict {
            name: "a connection whose delete has been made".to_string(),
            region: OTHER_REGION,
            activation_key: text(&torn_down, "activationKey"),
            attach_point: json!({ "directConnectGateway": first }),
            status: 400,
            code: CLIENT_CODE,
            message: proposal_not_acceptable_message(&torn_down_id, &torn_down_state),
        },
    ];

    let faults: Vec<RequestFault> = [tag_faults(), attach_point_faults()].concat();
    assert!(
        faults.len() >= 8
            && faults
                .iter()
                .any(|fault| fault.members.iter().any(|(member, _)| *member == "tags"))
            && faults.iter().any(|fault| fault
                .members
                .iter()
                .any(|(member, _)| *member == "attachPoint")),
        "case \"table\": expected rows for both kinds of request fault, got {} rows",
        faults.len()
    );

    for verdict in &verdicts {
        // A request carrying no fault of its own gets the resource verdict, unchanged.
        let (status, body) = accept(
            &emulator,
            verdict.region,
            &verdict.activation_key,
            &verdict.attach_point,
            &[("tags", one_tag("env", "baseline"))],
        )
        .await;
        let case = format!("{} / a well-formed request", verdict.name);
        assert_eq!(
            status, verdict.status,
            "case {case:?}: expected HTTP {}, got {status} with {body:?}",
            verdict.status
        );
        assert!(
            code(&body).contains(verdict.code),
            "case {case:?}: expected {}, got {:?} with {body:?}",
            verdict.code,
            code(&body)
        );
        assert_eq!(
            message(&body),
            verdict.message,
            "case {case:?}: expected the resource verdict's wording, got {:?}",
            message(&body)
        );

        for RequestFault {
            name,
            members,
            message: expected,
        } in &faults
        {
            let case = format!("{} / {name}", verdict.name);
            let (status, body) = accept(
                &emulator,
                verdict.region,
                &verdict.activation_key,
                &verdict.attach_point,
                members.as_slice(),
            )
            .await;

            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400 for a fault the request carries, got {status} with {body:?}"
            );
            assert!(
                code(&body).contains(VALIDATION_CODE),
                "case {case:?}: expected {VALIDATION_CODE} rather than the resource verdict {:?}, got {:?} with {body:?}",
                verdict.code,
                code(&body)
            );
            assert_eq!(
                message(&body),
                *expected,
                "case {case:?}: expected the refusal to report the fault the request carries, got {:?}",
                message(&body)
            );
        }
    }

    // A request carrying BOTH kinds of fault at once is still refused for one of them,
    // never for the resource it names.
    let (status, body) = accept(
        &emulator,
        verdicts[0].region,
        &verdicts[0].activation_key,
        &json!({ "directConnectGateway": "not-a-uuid" }),
        &[("tags", one_tag("aws:owner", "ops"))],
    )
    .await;
    let case = "an unissued key, a reserved tag key and a malformed attach point";
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with {body:?}"
    );
    assert!(
        code(&body).contains(VALIDATION_CODE),
        "case {case:?}: expected {VALIDATION_CODE}, got {:?} with {body:?}",
        code(&body)
    );
    let both = [
        reserved_key_message("aws:owner"),
        not_a_gateway_identifier_message("not-a-uuid"),
    ];
    assert!(
        both.contains(&message(&body)),
        "case {case:?}: expected one of the request's own faults to be reported, got {:?}",
        message(&body)
    );

    // Not one of those refusals wrote anything: the accepted connection still holds the
    // attach point and the tag set its acceptance left, and the deleted one is still
    // deleted.
    let read = emulator
        .ic_ok_in(
            DEFAULT_SIGNING_REGION,
            "GetConnection",
            json!({ "identifier": accepted_id }),
        )
        .await;
    assert_eq!(
        read["connection"]["attachPoint"],
        json!({ "directConnectGateway": first }),
        "case \"after the acceptance table\": expected the accepted attach point to be untouched, got {:?}",
        read["connection"]["attachPoint"]
    );
    assert_eq!(
        tags_of(&read["connection"]),
        tags_of(&json!({ "tags": held_tags })),
        "case \"after the acceptance table\": expected the accepted tag set to be untouched, got {:?}",
        read["connection"]["tags"]
    );
    let read = emulator
        .ic_ok_in(
            OTHER_REGION,
            "GetConnection",
            json!({ "identifier": torn_down_id }),
        )
        .await;
    assert_eq!(
        text(&read["connection"], "state"),
        DELETED_STATE,
        "case \"after the acceptance table\": expected the deleted connection to stay deleted, got {:?}",
        read["connection"]["state"]
    );
}

// ---------------------------------------------------------------------------
// The create
// ---------------------------------------------------------------------------

/// One verdict this store has about something a create names, and the members that reach
/// it.
struct CreateVerdict {
    name: String,
    region: &'static str,
    /// The members that make the request name what the store refuses, written over a
    /// request every member of which is otherwise well formed.
    members: Vec<(&'static str, Value)>,
    status: u16,
    code: &'static str,
    message: String,
}

/// RULE, on the other writer of these members: `CreateConnection` reports the fault the
/// REQUEST carries before it resolves the `environmentId` or the `clientToken` the
/// request names, so neither an Environment the called Region does not offer nor a token
/// already used for another request answers in its place — and a refused create
/// allocates nothing.
#[tokio::test]
async fn a_creates_own_faults_are_reported_before_what_it_names_is_resolved() {
    let emulator = Emulator::new();
    let gateway = emulator.attach_point().await;
    let here = placements_in(&emulator, DEFAULT_SIGNING_REGION).await;
    let there = placements_in(&emulator, OTHER_REGION).await;
    assert!(
        !here.is_empty() && !there.is_empty(),
        "case \"catalog\": expected both Regions to offer a placement, got {} and {}",
        here.len(),
        there.len()
    );

    // A token that has already created a connection, so a request repeating it with
    // anything else is the store's own verdict about that token.
    let token = "order-of-judgement-token";
    let (status, body) = create(
        &emulator,
        DEFAULT_SIGNING_REGION,
        &here[0],
        &gateway,
        &[
            ("clientToken", json!(token)),
            ("description", json!("order token baseline")),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup \"the create the token made\": expected it to be accepted, got {status} with {body:?}"
    );
    let token_connection_id = text(&body["connection"], "id");

    let verdicts = vec![
        CreateVerdict {
            name: "an environmentId no catalog holds".to_string(),
            region: DEFAULT_SIGNING_REGION,
            members: vec![("environmentId", json!("not-an-environment"))],
            status: 400,
            code: CLIENT_CODE,
            message: environment_not_found_message("not-an-environment"),
        },
        CreateVerdict {
            name: "an Environment of another Region's catalog".to_string(),
            region: OTHER_REGION,
            members: vec![("environmentId", json!(here[0].0.clone()))],
            status: 400,
            code: CLIENT_CODE,
            message: environment_not_found_message(&here[0].0),
        },
        CreateVerdict {
            name: "a clientToken that already created another connection".to_string(),
            region: DEFAULT_SIGNING_REGION,
            members: vec![
                ("clientToken", json!(token)),
                ("description", json!("order token repeated differently")),
            ],
            status: 400,
            code: VALIDATION_CODE,
            message: client_token_reused_message(token, &token_connection_id),
        },
    ];

    let faults: Vec<RequestFault> =
        [tag_faults(), attach_point_faults(), remote_account_faults()].concat();
    let before: Vec<usize> = vec![
        connection_count(&emulator, DEFAULT_SIGNING_REGION).await,
        connection_count(&emulator, OTHER_REGION).await,
    ];

    for verdict in &verdicts {
        let placement = if verdict.region == DEFAULT_SIGNING_REGION {
            &here[0]
        } else {
            &there[there.len() - 1]
        };

        // A request carrying no fault of its own gets the store's verdict, unchanged.
        let case = format!("{} / a well-formed request", verdict.name);
        let (status, body) = create(
            &emulator,
            verdict.region,
            placement,
            &gateway,
            &verdict.members,
        )
        .await;
        assert_eq!(
            status, verdict.status,
            "case {case:?}: expected HTTP {}, got {status} with {body:?}",
            verdict.status
        );
        assert!(
            code(&body).contains(verdict.code),
            "case {case:?}: expected {}, got {:?} with {body:?}",
            verdict.code,
            code(&body)
        );
        assert_eq!(
            message(&body),
            verdict.message,
            "case {case:?}: expected the store's own verdict, got {:?}",
            message(&body)
        );

        for RequestFault {
            name,
            members,
            message: expected,
        } in &faults
        {
            let case = format!("{} / {name}", verdict.name);
            let overrides: Vec<(&'static str, Value)> = verdict
                .members
                .iter()
                .chain(members.iter())
                .cloned()
                .collect();
            let (status, body) =
                create(&emulator, verdict.region, placement, &gateway, &overrides).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400 for a fault the request carries, got {status} with {body:?}"
            );
            assert!(
                code(&body).contains(VALIDATION_CODE),
                "case {case:?}: expected {VALIDATION_CODE}, got {:?} with {body:?}",
                code(&body)
            );
            assert_eq!(
                message(&body),
                *expected,
                "case {case:?}: expected the refusal to report the fault the request carries, got {:?}",
                message(&body)
            );
            assert!(
                body["connection"].is_null(),
                "case {case:?}: expected a refusal to carry no connection, got {body:?}"
            );
        }
    }

    // Nothing the table sent allocated anything.
    let after: Vec<usize> = vec![
        connection_count(&emulator, DEFAULT_SIGNING_REGION).await,
        connection_count(&emulator, OTHER_REGION).await,
    ];
    assert_eq!(
        after, before,
        "case \"after the create table\": expected the refused creates to have allocated nothing, got {after:?} where there were {before:?}"
    );
}

// ---------------------------------------------------------------------------
// The attach-point listing
// ---------------------------------------------------------------------------

/// The page sizes the listing rows ask for: no `maxResults` at all, and each published
/// bound of `MaxResults` (`smithy_query(query="shape",shape="MaxResults")`: `@range(min:
/// 1, max: 100)`), because a page size decides nothing about which fault is reported.
const LISTING_PAGE_SIZES: [Option<i64>; 3] = [None, Some(1), Some(100)];

/// An `environmentId` no Region's catalog publishes, in the form the published ids take.
const ABSENT_ENVIRONMENT: &str = "mce-aws-nobody-nowhere";

/// A second `environmentId` no catalog publishes, of a different length and form, so no
/// row is keyed to one shape of unresolvable id.
const SHORT_ABSENT_ENVIRONMENT: &str = "mce";

/// One `ListAttachPoints` request, as a `(status, body)` pair so a refusal is readable.
async fn list_attach_points(
    emulator: &Emulator,
    region: &str,
    environment_id: &str,
    token: Option<&str>,
    max_results: Option<i64>,
) -> (u16, Value) {
    let mut request = Map::new();
    request.insert("environmentId".to_string(), json!(environment_id));
    if let Some(token) = token {
        request.insert("nextToken".to_string(), json!(token));
    }
    if let Some(max_results) = max_results {
        request.insert("maxResults".to_string(), json!(max_results));
    }
    emulator
        .ic_in(region, "ListAttachPoints", Value::Object(request))
        .await
}

/// The `environmentId` of the first Environment `region` publishes that a Connection can
/// be placed on, which is one whose attach points a listing really walks.
async fn listable_environment(emulator: &Emulator, region: &str) -> String {
    let catalog = emulator.environments_in(region).await;
    let creatable = creatable_environments(&catalog);
    let environment = creatable.first().unwrap_or_else(|| {
        panic!(
            "setup {region:?}: expected the catalog to publish an Environment a Connection can \
             be placed on, got {catalog:?}"
        )
    });
    text(environment, "environmentId")
}

/// The `identifier`s a page reports, in the order it reports them: an attach point is
/// keyed by its identifier, which is also what a page cursor names a position by.
fn page_keys(body: &Value) -> Vec<String> {
    body["attachPoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|descriptor| text(descriptor, "identifier"))
        .collect()
}

/// Every key the listing of `(region, environment_id)` walks, in the order it reports
/// them, read in one unwindowed page.
async fn sequence_keys(emulator: &Emulator, region: &str, environment_id: &str) -> Vec<String> {
    let (status, body) =
        list_attach_points(emulator, region, environment_id, None, Some(100)).await;
    assert_eq!(
        status, 200,
        "setup {region:?}/{environment_id:?}: expected the whole sequence in one page, got {status} with {body:?}"
    );
    page_keys(&body)
}

/// A page cursor the listing of `(region, environment_id)` really minted, with the page
/// key it handed out first: the values a row needs to offer this service's OWN token to
/// another sequence, and a bare key carrying no authenticator at all.
async fn first_page_cursor(
    emulator: &Emulator,
    region: &str,
    environment_id: &str,
) -> (String, String) {
    let (status, body) = list_attach_points(emulator, region, environment_id, None, Some(1)).await;
    assert_eq!(
        status, 200,
        "setup {region:?}/{environment_id:?}: expected a first page of one, got {status} with {body:?}"
    );
    let key = text(&body["attachPoints"][0], "identifier");
    let token = body["nextToken"].as_str().unwrap_or_else(|| {
        panic!(
            "setup {region:?}/{environment_id:?}: expected a first page of one to leave a cursor \
             behind, got {body:?}"
        )
    });
    (token.to_string(), key)
}

/// A page cursor this service really minted, and the sequence it names a position in:
/// offered to any OTHER sequence it is a token that listing never issued.
struct IssuedCursor {
    case: &'static str,
    region: &'static str,
    environment_id: String,
    token: String,
}

/// One verdict this store has about the `environmentId` a listing names — the served
/// listing for one the called Region publishes, the client fault for one it does not.
struct ListingVerdict {
    name: &'static str,
    region: &'static str,
    environment_id: String,
    status: u16,
    code: &'static str,
    message: String,
}

/// RULE, on the paginated reader of these members: `ListAttachPoints` reports the fault
/// the REQUEST carries — a `nextToken` the addressed listing never issued — before it
/// resolves the `environmentId` the request names, so an Environment the called Region
/// does not offer never answers in its place; a request carrying no such fault still
/// gets the verdict about that id, and a cursor the listing did issue is still served.
#[tokio::test]
async fn a_listings_own_cursor_fault_is_reported_before_the_environment_it_names_is_resolved() {
    let emulator = Emulator::new();
    // The harness's own wait for the account's gateways to be ready for use, which is
    // what makes each of them an attach point.
    emulator.ready_gateways(&GATEWAY_ROWS).await;

    let here = listable_environment(&emulator, DEFAULT_SIGNING_REGION).await;
    let there = listable_environment(&emulator, OTHER_REGION).await;
    // Both endpoints offer the account's gateways, so each listing is pageable: a first
    // page of one leaves a cursor behind only where a second entry remains.
    for (region, environment_id) in [(DEFAULT_SIGNING_REGION, &here), (OTHER_REGION, &there)] {
        let walked = sequence_keys(&emulator, region, environment_id).await;
        assert!(
            walked.len() > 1,
            "setup {region:?}/{environment_id:?}: expected more than one attach point, so a page \
             of one leaves a cursor behind, got {walked:?}"
        );
    }

    let (here_cursor, here_key) =
        first_page_cursor(&emulator, DEFAULT_SIGNING_REGION, &here).await;
    let (there_cursor, _) = first_page_cursor(&emulator, OTHER_REGION, &there).await;

    // Cursors this service really minted: each is one its OWN sequence serves, and one
    // every other sequence never issued.
    let issued = [
        IssuedCursor {
            case: "a cursor the called Region's listing of this Environment issued",
            region: DEFAULT_SIGNING_REGION,
            environment_id: here.clone(),
            token: here_cursor.clone(),
        },
        IssuedCursor {
            case: "a cursor another Region's endpoint issued",
            region: OTHER_REGION,
            environment_id: there.clone(),
            token: there_cursor,
        },
    ];

    // Values no listing of this service ever minted, whatever it was listing.
    let unissued: Vec<(&'static str, String)> = vec![
        (
            "no cursor shape at all",
            "not-a-token-this-service-minted".to_string(),
        ),
        (
            "the bare page key of an attach point a listing handed out",
            here_key,
        ),
        (
            "an issued cursor with its last hex digit pair dropped",
            here_cursor[..here_cursor.len() - 2].to_string(),
        ),
    ];

    let rows = vec![
        ListingVerdict {
            name: "the Environment the called Region publishes",
            region: DEFAULT_SIGNING_REGION,
            environment_id: here.clone(),
            status: 200,
            code: "",
            message: String::new(),
        },
        ListingVerdict {
            name: "an environmentId no catalog holds",
            region: DEFAULT_SIGNING_REGION,
            environment_id: ABSENT_ENVIRONMENT.to_string(),
            status: 400,
            code: CLIENT_CODE,
            message: environment_not_found_message(ABSENT_ENVIRONMENT),
        },
        ListingVerdict {
            name: "an environmentId of another form no catalog holds",
            region: OTHER_REGION,
            environment_id: SHORT_ABSENT_ENVIRONMENT.to_string(),
            status: 400,
            code: CLIENT_CODE,
            message: environment_not_found_message(SHORT_ABSENT_ENVIRONMENT),
        },
        ListingVerdict {
            name: "an Environment of another Region's catalog",
            region: DEFAULT_SIGNING_REGION,
            environment_id: there.clone(),
            status: 400,
            code: CLIENT_CODE,
            message: environment_not_found_message(&there),
        },
        ListingVerdict {
            name: "the Environment the other Region publishes",
            region: OTHER_REGION,
            environment_id: there.clone(),
            status: 200,
            code: "",
            message: String::new(),
        },
    ];

    let before = emulator
        .attach_points_in(DEFAULT_SIGNING_REGION, &here)
        .await;

    for row in &rows {
        // A request carrying no fault of its own gets this store's verdict about the id
        // it names, unchanged.
        let case = format!("{} / a request carrying no fault of its own", row.name);
        let (status, body) =
            list_attach_points(&emulator, row.region, &row.environment_id, None, None).await;
        assert_eq!(
            status, row.status,
            "case {case:?}: expected HTTP {}, got {status} with {body:?}",
            row.status
        );
        if row.status == 200 {
            assert!(
                !body["attachPoints"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .is_empty(),
                "case {case:?}: expected the listing to be served, got {body:?}"
            );
            // And the cursor THAT listing issued still resumes it, so what the rows
            // below refuse is a token it did not mint rather than every token.
            if let Some(own) = issued
                .iter()
                .find(|cursor| cursor.region == row.region && cursor.environment_id == row.environment_id)
            {
                let case = format!("{} / the cursor this listing issued", row.name);
                let (status, resumed) = list_attach_points(
                    &emulator,
                    row.region,
                    &row.environment_id,
                    Some(&own.token),
                    None,
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the issued cursor to resume the walk, got {status} with {resumed:?}"
                );
                // The REST of the sequence, not merely a page: the cursor was minted
                // after a first page of one, so what it resumes is every key but the
                // first, in order.
                let walked = sequence_keys(&emulator, row.region, &row.environment_id).await;
                assert_eq!(
                    page_keys(&resumed),
                    walked[1..].to_vec(),
                    "case {case:?}: expected the rest of the sequence after {:?}, got {resumed:?}",
                    walked.first()
                );
            }
        } else {
            assert!(
                code(&body).contains(row.code),
                "case {case:?}: expected {}, got {:?} with {body:?}",
                row.code,
                code(&body)
            );
            assert_eq!(
                message(&body),
                row.message,
                "case {case:?}: expected the store's own verdict, got {:?}",
                message(&body)
            );
        }

        // The same request, carrying a cursor this listing did not issue: the fault the
        // REQUEST carries is what the refusal reports, whatever the id it names resolves
        // to and whatever page size it asks for.
        let forged = unissued
            .iter()
            .map(|(case, token)| (*case, token.clone()))
            .chain(
                issued
                    .iter()
                    .filter(|cursor| {
                        !(cursor.region == row.region
                            && cursor.environment_id == row.environment_id)
                    })
                    .map(|cursor| (cursor.case, cursor.token.clone())),
            );
        for (fault, token) in forged {
            for max_results in LISTING_PAGE_SIZES {
                let case = format!("{} / {fault} / maxResults {max_results:?}", row.name);
                let (status, body) = list_attach_points(
                    &emulator,
                    row.region,
                    &row.environment_id,
                    Some(&token),
                    max_results,
                )
                .await;
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected HTTP 400 for a cursor this listing did not issue, got {status} with {body:?}"
                );
                assert!(
                    code(&body).contains(VALIDATION_CODE),
                    "case {case:?}: expected {VALIDATION_CODE}, got {:?} with {body:?}",
                    code(&body)
                );
                assert_eq!(
                    message(&body),
                    cursor_not_issued_message("ListAttachPoints"),
                    "case {case:?}: expected the refusal to report the fault the request carries, got {:?}",
                    message(&body)
                );
                assert!(
                    !message(&body).contains(&row.environment_id),
                    "case {case:?}: expected no verdict about the environment the request names, got {:?}",
                    message(&body)
                );
                assert!(
                    body["attachPoints"].as_array().is_none(),
                    "case {case:?}: expected a refusal to carry no page, got {body:?}"
                );
            }
        }
    }

    // The refusals were reads: what a later caller sees is what it saw before them.
    let after = emulator
        .attach_points_in(DEFAULT_SIGNING_REGION, &here)
        .await;
    assert_eq!(
        identifiers(&after),
        identifiers(&before),
        "case \"after the listing table\": expected the refused listings to have changed nothing, got {after:?} where there were {before:?}"
    );
}
