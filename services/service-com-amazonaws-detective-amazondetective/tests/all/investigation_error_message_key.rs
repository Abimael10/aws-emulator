// SPDX-License-Identifier: CC0-1.0

//! The wire KEY an error body of this service carries its explanatory text under.
//!
//! The rule under test, asserted over a SET of refusals rather than the one a recording
//! happened to carry:
//!
//! 1. A refusal of an operation bound under `/investigations/` carries its text under
//!    `Message` — the member name the model spells
//!    (`smithy_query(query="shape",shape="ResourceNotFoundException")` reports
//!    `{"Message": {"target": "ErrorMessage"}}`) — and under that spelling ALONE, whatever
//!    the error shape, the status and the condition. The rows below span the five
//!    operations of that surface, both of the error shapes their handlers answer with
//!    (`ResourceNotFoundException` at 404, `ValidationException` at 400) and every
//!    condition each of them refuses.
//! 2. The behavior-graph surface keys the same member lowercase, so the two surfaces
//!    answer one condition — a behavior graph this service does not hold — under two
//!    keys. The control rows pin that contrast in one place; `client_error_bodies` owns
//!    the behavior-graph surface's own table.
//! 3. The text itself is fixed per operation and per condition: it interpolates neither
//!    the graph ARN nor the investigation id the request carried, so every row asserts
//!    the answer echoes neither back. Two operations word the SAME condition — an
//!    investigation that cannot be resolved — differently, and each keeps its own
//!    sentence.
//! 4. Every member of an error body beyond the message keeps the model's own spelling
//!    (`ErrorCode`, `ErrorCodeReason`), and a successful call's body is untouched by any
//!    of this.
//!
//! The scaffold's 501 is deliberately not a row: it is not one of this service's modelled
//! refusals (no operation declares it and AWS answers no such status), and it travels in
//! the framework's own envelope.
//!
//! Every case drives the real wire path: the production `MultiServiceRouter`, restJson1
//! serialization, the modelled-constraint walk and the handlers.

use serde_json::{json, Value};
use service_com_amazonaws_detective_amazondetective::state::{
    INVESTIGATION_GRAPH_NOT_FOUND_MESSAGE, INVESTIGATION_ID_LEN,
};

use crate::support::{
    call, create_graph_ok, emulator, error_code, graph_arn_of, ACCOUNT, REGION,
};

/// The URIs of this service's `/investigations/` surface, from
/// `smithy_query(query="digest")`: the five operations bound under that prefix.
const START_URI: &str = "/investigations/startInvestigation";
const GET_URI: &str = "/investigations/getInvestigation";
const LIST_URI: &str = "/investigations/listInvestigations";
const UPDATE_STATE_URI: &str = "/investigations/updateInvestigationState";

/// Two URIs of the behavior-graph surface, for the control rows.
const LIST_MEMBERS_URI: &str = "/graph/members/list";
const LIST_DATASOURCES_URI: &str = "/graph/datasources/list";

/// The key the model gives the message member of every error shape of this service, and
/// the key the `/investigations/` surface serves it under.
const MODELLED_MESSAGE_KEY: &str = "Message";

/// The key the behavior-graph surface serves the same member under.
const GRAPH_MESSAGE_KEY: &str = "message";

/// The fixed sentence `GetInvestigation` answers an investigation id that resolves to no
/// investigation of the graph with (recorded behaviour; no variable part).
const GET_INVESTIGATION_NOT_FOUND: &str = "The specified investigation does not exist";

/// The fixed sentence `UpdateInvestigationState` answers the same condition with — this
/// operation's own wording, which names the two inputs that together identify the
/// investigation and interpolates neither (recorded behaviour).
const UPDATE_INVESTIGATION_STATE_NOT_FOUND: &str =
    "Unable to find Investigation based on the Graph Arn or Investigation ID provided";

/// A `State` the modelled `State` enum accepts
/// (`smithy_query(query="shape",shape="State")`), so a refusal below is the handler's and
/// not the constraint walk's.
const STATE_ARCHIVED: &str = "ARCHIVED";

/// The text one error body carries, read under the key `expected_key` ALONE — the rule
/// under test — and asserted to be the only spelling the body serves.
fn message_under(case: &str, body: &Value, expected_key: &str, other_key: &str) -> String {
    assert!(
        body.get(other_key).is_none(),
        "case {case:?}: the body must carry its text under {expected_key:?} alone, but it \
         also serves {other_key:?}: got {body:?}"
    );
    body.get(expected_key)
        .unwrap_or_else(|| {
            panic!("case {case:?}: the body carried no {expected_key:?} at all: got {body:?}")
        })
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: {expected_key:?} is not a string: got {body:?}"))
        .to_string()
}

/// One row of the refusal table: the request, and what the answer must state.
struct Case {
    name: &'static str,
    uri: &'static str,
    body: Value,
    status: u16,
    code: &'static str,
    /// The exact text, where this surface publishes a fixed one for the condition;
    /// `None` where another suite owns the wording and only the KEY is under test here.
    message: Option<String>,
    /// Values of the request that the answer must not echo back.
    echoes_nothing: Vec<String>,
}

/// A `GraphArn` satisfying the modelled pattern that nothing was ever stored under; the
/// rows differ along WHY it resolves to nothing.
fn absent_graphs() -> Vec<(&'static str, String)> {
    vec![
        (
            "a graph id nothing was stored under",
            graph_arn_of("aws", REGION, ACCOUNT, &"c".repeat(32)),
        ),
        (
            "a graph of another region",
            graph_arn_of("aws", "eu-central-1", ACCOUNT, &"d".repeat(32)),
        ),
        (
            "a graph of another account",
            graph_arn_of("aws", REGION, "123456789012", &"e".repeat(32)),
        ),
    ]
}

/// Well-formed `InvestigationId`s (the modelled length 21..21 and `^[0-9]+$`,
/// `smithy_query(query="shape",shape="InvestigationId")`) that no investigation carries,
/// so what refuses them is the handler and not the constraint walk.
fn absent_investigation_ids() -> Vec<(&'static str, String)> {
    let ids = vec![
        ("all nines", "9".repeat(INVESTIGATION_ID_LEN)),
        (
            "a leading one",
            format!("1{}", "0".repeat(INVESTIGATION_ID_LEN - 1)),
        ),
        (
            "all zeros",
            "0".repeat(INVESTIGATION_ID_LEN),
        ),
    ];
    for (case, id) in &ids {
        assert_eq!(
            id.len(),
            INVESTIGATION_ID_LEN,
            "case {case:?}: a row must satisfy the modelled length so the LOOKUP is what \
             fails: {id:?}"
        );
    }
    ids
}

#[tokio::test]
async fn every_investigations_refusal_keys_its_message_as_the_model_spells_it() {
    let router = emulator();
    let graph_arn = create_graph_ok(&router, REGION, &json!({})).await;
    let entity_arn = format!("arn:aws:iam::{ACCOUNT}:user/alice");

    // One investigation that DOES exist, so the id rows below are the only thing a lookup
    // cannot resolve, and so the success row has something to read back.
    let (status, started) = call(
        &router,
        START_URI,
        &json!({
            "GraphArn": graph_arn,
            "EntityArn": entity_arn,
            "ScopeStartTime": "2024-05-01T08:00:00Z",
            "ScopeEndTime": "2024-05-01T12:00:00Z",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "StartInvestigation must be served: got {status} {started:?}"
    );
    let investigation_id = started
        .get("InvestigationId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("StartInvestigation served no InvestigationId: {started:?}"))
        .to_string();

    let mut cases: Vec<Case> = Vec::new();

    // An investigation id the graph holds no investigation under: each of the two
    // operations that resolve one keeps its OWN sentence for the one condition.
    for (how, absent_id) in absent_investigation_ids() {
        assert_ne!(
            absent_id, investigation_id,
            "row {how:?} must name no stored investigation"
        );
        cases.push(Case {
            name: "GetInvestigation on an investigation id that resolves to nothing",
            uri: GET_URI,
            body: json!({ "GraphArn": graph_arn, "InvestigationId": absent_id }),
            status: 404,
            code: "ResourceNotFoundException",
            message: Some(GET_INVESTIGATION_NOT_FOUND.to_string()),
            echoes_nothing: vec![absent_id.clone(), graph_arn.clone()],
        });
        cases.push(Case {
            name: "UpdateInvestigationState on an investigation id that resolves to nothing",
            uri: UPDATE_STATE_URI,
            body: json!({
                "GraphArn": graph_arn,
                "InvestigationId": absent_id,
                "State": STATE_ARCHIVED,
            }),
            status: 404,
            code: "ResourceNotFoundException",
            message: Some(UPDATE_INVESTIGATION_STATE_NOT_FOUND.to_string()),
            echoes_nothing: vec![absent_id.clone(), graph_arn.clone()],
        });
    }

    // A behavior graph this service does not hold, over every operation of the surface
    // that resolves one.
    for (how, absent) in absent_graphs() {
        let _ = how;
        cases.push(Case {
            name: "GetInvestigation on a graph that resolves to nothing",
            uri: GET_URI,
            body: json!({ "GraphArn": absent, "InvestigationId": investigation_id }),
            status: 404,
            code: "ResourceNotFoundException",
            message: Some(INVESTIGATION_GRAPH_NOT_FOUND_MESSAGE.to_string()),
            echoes_nothing: vec![absent.clone(), investigation_id.clone()],
        });
        cases.push(Case {
            name: "UpdateInvestigationState on a graph that resolves to nothing",
            uri: UPDATE_STATE_URI,
            body: json!({
                "GraphArn": absent,
                "InvestigationId": investigation_id,
                "State": STATE_ARCHIVED,
            }),
            status: 404,
            code: "ResourceNotFoundException",
            message: Some(INVESTIGATION_GRAPH_NOT_FOUND_MESSAGE.to_string()),
            echoes_nothing: vec![absent.clone(), investigation_id.clone()],
        });
        cases.push(Case {
            name: "ListInvestigations on a graph that resolves to nothing",
            uri: LIST_URI,
            body: json!({ "GraphArn": absent }),
            status: 404,
            code: "ResourceNotFoundException",
            message: Some(INVESTIGATION_GRAPH_NOT_FOUND_MESSAGE.to_string()),
            echoes_nothing: vec![absent.clone()],
        });
        cases.push(Case {
            name: "StartInvestigation on a graph that resolves to nothing",
            uri: START_URI,
            body: json!({
                "GraphArn": absent,
                "EntityArn": entity_arn,
                "ScopeStartTime": "2024-05-01T08:00:00Z",
                "ScopeEndTime": "2024-05-01T12:00:00Z",
            }),
            status: 404,
            code: "ResourceNotFoundException",
            message: Some(INVESTIGATION_GRAPH_NOT_FOUND_MESSAGE.to_string()),
            echoes_nothing: vec![absent.clone(), entity_arn.clone()],
        });
    }

    // The other error shape of the surface, at the other status: the 400s its handlers
    // raise. Their wording belongs to `start_investigation_scope_and_merits` and
    // `list_investigations`; what is under test here is the key.
    cases.push(Case {
        name: "StartInvestigation over an entity that is neither an IAM user nor a role",
        uri: START_URI,
        body: json!({
            "GraphArn": graph_arn,
            "EntityArn": format!("arn:aws:iam::{ACCOUNT}:group/admins"),
            "ScopeStartTime": "2024-05-01T08:00:00Z",
            "ScopeEndTime": "2024-05-01T12:00:00Z",
        }),
        status: 400,
        code: "ValidationException",
        message: None,
        echoes_nothing: Vec::new(),
    });
    cases.push(Case {
        name: "StartInvestigation over a scope window that is not a period",
        uri: START_URI,
        body: json!({
            "GraphArn": graph_arn,
            "EntityArn": entity_arn,
            "ScopeStartTime": "2024-05-01T12:00:00Z",
            "ScopeEndTime": "2024-05-01T08:00:00Z",
        }),
        status: 400,
        code: "ValidationException",
        message: None,
        echoes_nothing: Vec::new(),
    });
    cases.push(Case {
        name: "ListInvestigations with a NextToken it never issued",
        uri: LIST_URI,
        body: json!({ "GraphArn": graph_arn, "NextToken": "not-a-token-from-here" }),
        status: 400,
        code: "ValidationException",
        message: None,
        echoes_nothing: vec!["not-a-token-from-here".to_string()],
    });

    for case in cases {
        let name = case.name;
        let (status, answered) = call(&router, case.uri, &case.body).await;
        assert_eq!(
            status, case.status,
            "case {name:?} ({}): expected {}, got {status} {answered:?}",
            case.body, case.status
        );
        assert_eq!(
            error_code(&answered),
            case.code,
            "case {name:?} ({}): expected {}, got {answered:?}",
            case.body,
            case.code
        );
        let message = message_under(name, &answered, MODELLED_MESSAGE_KEY, GRAPH_MESSAGE_KEY);
        match case.message {
            Some(expected) => assert_eq!(
                message, expected,
                "case {name:?} ({}): expected {expected:?}, got {message:?}",
                case.body
            ),
            None => assert!(
                !message.trim().is_empty(),
                "case {name:?} ({}): expected a non-empty text, got {message:?}",
                case.body
            ),
        }
        for echoed in &case.echoes_nothing {
            assert!(
                !message.contains(echoed.as_str()),
                "case {name:?}: the text is fixed and must not echo {echoed:?} back, got \
                 {message:?}"
            );
        }
    }

    // The success row: nothing above changed what a served call answers, and the body of
    // a 200 carries no message member under either spelling.
    let (status, got) = call(
        &router,
        GET_URI,
        &json!({ "GraphArn": graph_arn, "InvestigationId": investigation_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the investigation that exists must still be served: got {status} {got:?}"
    );
    assert_eq!(
        got.get("InvestigationId").and_then(Value::as_str),
        Some(investigation_id.as_str()),
        "the served investigation must be the one that was started: got {got:?}"
    );
    assert!(
        got.get(MODELLED_MESSAGE_KEY).is_none() && got.get(GRAPH_MESSAGE_KEY).is_none(),
        "a served call states no message under either spelling: got {got:?}"
    );
}

#[tokio::test]
async fn the_two_surfaces_answer_one_condition_under_their_own_keys() {
    let router = emulator();

    // The same condition — a behavior graph this service does not hold — put to each
    // surface, over every row of the absent-graph table.
    for (how, absent) in absent_graphs() {
        let case = format!("the investigations surface: {how}");
        let (status, answered) = call(&router, LIST_URI, &json!({ "GraphArn": absent })).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected 404, got {status} {answered:?}"
        );
        assert_eq!(
            message_under(&case, &answered, MODELLED_MESSAGE_KEY, GRAPH_MESSAGE_KEY),
            INVESTIGATION_GRAPH_NOT_FOUND_MESSAGE,
            "case {case:?}: got {answered:?}"
        );

        // And the behavior-graph surface, whose operations keep the lowercase key: a
        // reader of the members of the same absent graph, and a reader of its data source
        // packages.
        for (operation, uri) in [
            ("ListMembers", LIST_MEMBERS_URI),
            ("ListDatasourcePackages", LIST_DATASOURCES_URI),
        ] {
            let case = format!("the behavior-graph surface, {operation}: {how}");
            let (status, answered) = call(&router, uri, &json!({ "GraphArn": absent })).await;
            assert_eq!(
                status, 404,
                "case {case:?}: expected 404, got {status} {answered:?}"
            );
            assert_eq!(
                error_code(&answered),
                "ResourceNotFoundException",
                "case {case:?}: got {answered:?}"
            );
            let message =
                message_under(&case, &answered, GRAPH_MESSAGE_KEY, MODELLED_MESSAGE_KEY);
            assert!(
                !message.trim().is_empty() && !message.contains(absent.as_str()),
                "case {case:?}: expected that surface's own fixed text, got {message:?}"
            );
        }
    }
}

#[tokio::test]
async fn an_error_member_beyond_the_message_keeps_the_models_spelling() {
    // `ValidationException` carries `ErrorCode` and `ErrorCodeReason` beside its message
    // (`smithy_query(query="shape",shape="ValidationException")`), and only the message
    // member is keyed by the surface: the other two are keyed as the model spells them,
    // on both surfaces.
    let router = emulator();
    let graph_arn = create_graph_ok(&router, REGION, &json!({})).await;

    let (status, answered) = call(
        &router,
        LIST_URI,
        &json!({ "GraphArn": graph_arn, "NextToken": "not-a-token-from-here" }),
    )
    .await;
    assert_eq!(
        status, 400,
        "a NextToken this operation never issued: got {status} {answered:?}"
    );
    let case = "ListInvestigations with an unissued NextToken";
    assert!(
        !message_under(case, &answered, MODELLED_MESSAGE_KEY, GRAPH_MESSAGE_KEY)
            .trim()
            .is_empty(),
        "case {case:?}: expected a non-empty text, got {answered:?}"
    );
    for member in ["ErrorCode", "ErrorCodeReason"] {
        let value = answered
            .get(member)
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: no {member:?} member: got {answered:?}"));
        assert!(
            !value.trim().is_empty(),
            "case {case:?}: {member:?} must carry a value, got {value:?}"
        );
    }
}
