// SPDX-License-Identifier: CC0-1.0

//! What a boundary refusal of a COLLECTION whose `@length` a request broke QUOTES, asserted
//! end-to-end over the wire.
//!
//! THE RULE UNDER TEST: a list or a map outside its `@length` bound is refused with the
//! collection the request SUBMITTED in the value half every clause shares —
//! `N validation error(s) detected: Value '<submitted>' at '<member path>' failed to satisfy
//! constraint: <requirement>` — rendered the way a value of that shape prints: a list as
//! `[<element>, …]`, a map as `{<key>=<value>, …}`, a structure as
//! `<Shape>(<member>=<value>, …)` over every member its shape declares with one the request
//! left unset as `null`, and a union over the one member it carries. The value is the only
//! place a refusal reports the request's own input back, so a client comparing it against
//! what it sent sees its own input; the member path, the requirement and the counting
//! envelope are the model's either way.
//!
//! TWO things keep the valueless clause `Value at '<member path>' …`: a collection the model
//! marks `@sensitive`, whose values this service discloses nowhere, and one the shared echo
//! bounds decline to render (a collection past the member positions one refusal may walk).
//!
//! Each row drives a real `restJson1` request through a router the production emulator serves
//! with, so what is pinned is the status, the error code and the `message` a Bedrock Agent
//! Runtime client reads. The rows span what the rule generalises over: both bounds (a minimum
//! and a maximum), both collection kinds (list and map), elements of both kinds (a scalar and
//! a structure under a union), where the collection is bound (a request body, an `@httpQuery`
//! and a member of a nested structure), which operation carries it — one the generated
//! routing table serves and the hand-authored `InvokeInlineAgent`, whose output is a
//! `@streaming` union — which members of a submitted structure the request set, and, as the
//! contrast the rule turns on, the scalar classes that keep quoting their own value.

use serde_json::{json, Value};

use crate::agent_memory::{Emulator, ACCOUNT_KEY};
use crate::inline_agent_invocations::{request, Answer};
use crate::sessions::{call, error_code, router, ACCOUNT, REGION};

/// The status and code the model binds to a constraint refusal of every operation here
/// (`smithy_query(query="operation",operation="Rerank,TagResource,UntagResource,InvokeInlineAgent")`
/// -> `ValidationException` httpError 400).
const INVALID: (u16, &str) = (400, "ValidationException");

/// A reranker model id the modelled `BedrockModelArn` pattern admits, so no row is refused
/// for the configuration it carries.
const MODEL: &str = "amazon.rerank-v1:0";

/// The model an inline-agent row composes its agent against, which the catalogue publishes
/// under this id.
const INLINE_MODEL: &str = "amazon.nova-lite-v1:0";

/// The `@length` bounds of the collections under test, from the model
/// (`smithy_query(query="shape",shape="RerankQueriesList,RerankSourcesList,TagsMap,ReturnControlInvocationResults")`
/// and `smithy_query(query="operation",operation="UntagResource")` -> `tagKeys`):
/// `queries` 1..=1, `sources` 1..=1000, `tags` 1..=200 entries, `tagKeys` 1..=200,
/// `returnControlInvocationResults` 1..=5.
const QUERIES_MAX: usize = 1;
const SOURCES_MAX: usize = 1000;
const TAGS_MIN: usize = 1;
const TAG_KEYS_MAX: usize = 200;
const RESULTS_MIN: usize = 1;
const RESULTS_MAX: usize = 5;

/// The member path a clause about the returned results names, as the validating walk spells
/// it.
const RESULTS_MEMBER: &str = "inlineSessionState.returnControlInvocationResults";

/// How a member the request left unset renders inside a quoted structure.
const UNSET: &str = "null";

/// The `message` member of an error body, or `""` when there is none.
fn message(body: &Value) -> &str {
    body.get("message").and_then(Value::as_str).unwrap_or("")
}

/// The single-violation envelope the framework counts with.
fn one_error(clause: &str) -> String {
    format!("1 validation error detected: {clause}")
}

/// THE RULE: the clause a COLLECTION's `@length` violation states — the member, the
/// requirement, and the collection the request submitted.
fn collection_clause(quoted: &str, path: &str, requirement: &str) -> String {
    format!("Value '{quoted}' at '{path}' failed to satisfy constraint: {requirement}")
}

/// The clause a collection this service quotes NOTHING of states: a `@sensitive` one, or one
/// past the bounds the shared walk renders a collection under.
fn valueless_clause(path: &str, requirement: &str) -> String {
    format!("Value at '{path}' failed to satisfy constraint: {requirement}")
}

/// The contrast: the clause a SCALAR member's violation states, quoting what it submitted.
fn scalar_clause(value: &str, path: &str, requirement: &str) -> String {
    format!("Value '{value}' at '{path}' failed to satisfy constraint: {requirement}")
}

/// The framework's wording of a `@length` maximum.
fn at_most(bound: usize) -> String {
    format!("Member must have length less than or equal to {bound}")
}

/// The framework's wording of a `@length` minimum.
fn at_least(bound: usize) -> String {
    format!("Member must have length greater than or equal to {bound}")
}

/// One `TEXT` query carrying `text`.
fn text_query(text: &str) -> Value {
    json!({ "type": "TEXT", "textQuery": { "text": text } })
}

/// One INLINE `TEXT` source carrying `text`.
fn text_source(text: &str) -> Value {
    json!({
        "type": "INLINE",
        "inlineDocumentSource": { "type": "TEXT", "textDocument": { "text": text } },
    })
}

/// A `Rerank` body over `queries` and `sources`, with the reranking configuration every
/// row needs to get as far as the collection it is about.
fn rerank_body(queries: Value, sources: Value) -> Value {
    json!({
        "queries": queries,
        "sources": sources,
        "rerankingConfiguration": {
            "type": "BEDROCK_RERANKING_MODEL",
            "bedrockRerankingConfiguration": { "modelConfiguration": { "modelArn": MODEL } },
        },
    })
}

/// `count` distinct `TEXT` queries.
fn queries(count: usize) -> Value {
    Value::Array((0..count).map(|i| text_query(&format!("query {i}"))).collect())
}

/// `count` distinct INLINE `TEXT` sources.
fn sources(count: usize) -> Value {
    Value::Array(
        (0..count)
            .map(|i| text_source(&format!("document {i}")))
            .collect(),
    )
}

/// A well-formed session ARN for `uuid`, which this service's `TaggableResourcesArn`
/// pattern admits: the tagging rows are refused by the boundary walk, so the session it
/// names need not exist.
fn session_arn(uuid: &str) -> String {
    format!("arn:aws:bedrock:{REGION}:{ACCOUNT}:session/{uuid}")
}

/// Percent-encode an ARN for the non-greedy `@httpLabel` the tagging routes bind: a
/// session ARN carries `:` and `/`, which would otherwise split the path segment.
fn label(arn: &str) -> String {
    arn.replace('%', "%25")
        .replace(':', "%3A")
        .replace('/', "%2F")
}

/// `elements` as a list renders: in order, comma-separated, in brackets.
fn quoted_list(elements: &[String]) -> String {
    format!("[{}]", elements.join(", "))
}

/// THE RULE over a TABLE: every collection outside its `@length` is refused quoting the
/// collection it submitted, every `@sensitive` one is refused quoting nothing, and every
/// scalar outside its own bound keeps the value it sent.
#[tokio::test]
async fn a_collection_outside_its_length_is_refused_quoting_what_was_submitted() {
    let arn = session_arn("3f6c0a52-7b1d-4a9e-8c21-55d0f2a7b914");
    let tag_keys: Vec<String> = (0..=TAG_KEYS_MAX).map(|i| format!("key{i}")).collect();

    // (case, method, path, body, the clause the refusal must state)
    let rows: Vec<(&str, &str, String, Value, String)> = vec![
        (
            // `queries` and `sources` are `@sensitive`
            // (`smithy_query(query="shape",shape="RerankQueriesList,RerankSourcesList")`),
            // so neither is quoted whichever bound it breaks.
            "a sensitive queries list one entry over its maximum",
            "POST",
            "/rerank".to_string(),
            rerank_body(queries(QUERIES_MAX + 1), sources(1)),
            valueless_clause("queries", &at_most(QUERIES_MAX)),
        ),
        (
            "an empty sensitive queries list, under its minimum",
            "POST",
            "/rerank".to_string(),
            rerank_body(json!([]), sources(1)),
            valueless_clause("queries", &at_least(1)),
        ),
        (
            "an empty sensitive sources list, under its minimum",
            "POST",
            "/rerank".to_string(),
            rerank_body(queries(1), json!([])),
            valueless_clause("sources", &at_least(1)),
        ),
        (
            "a sensitive sources list one entry over a much larger maximum",
            "POST",
            "/rerank".to_string(),
            rerank_body(queries(1), sources(SOURCES_MAX + 1)),
            valueless_clause("sources", &at_most(SOURCES_MAX)),
        ),
        (
            "an empty tags map, under its minimum",
            "POST",
            format!("/tags/{}", label(&arn)),
            json!({ "tags": {} }),
            collection_clause("{}", "tags", &at_least(TAGS_MIN)),
        ),
        (
            "a tags map inside its bound is not refused for its length",
            "POST",
            format!("/tags/{}", label(&arn)),
            json!({ "tags": { "owner": "fleet", "tier": "gold" } }),
            // Accepted, so this row is driven separately below; see the `None` branch.
            String::new(),
        ),
        (
            "a query-bound tagKeys list one entry over its maximum",
            "DELETE",
            format!("/tags/{}?tagKeys={}", label(&arn), tag_keys.join("&tagKeys=")),
            Value::Null,
            collection_clause(
                &quoted_list(&tag_keys),
                "tagKeys",
                &at_most(TAG_KEYS_MAX),
            ),
        ),
        (
            "a scalar below its @range minimum keeps its value",
            "POST",
            "/rerank".to_string(),
            json!({
                "queries": queries(1),
                "sources": sources(1),
                "rerankingConfiguration": {
                    "type": "BEDROCK_RERANKING_MODEL",
                    "bedrockRerankingConfiguration": {
                        "modelConfiguration": { "modelArn": MODEL },
                        "numberOfResults": 0,
                    },
                },
            }),
            scalar_clause(
                "0",
                "rerankingConfiguration.bedrockRerankingConfiguration.numberOfResults",
                "Member must have value greater than or equal to 1",
            ),
        ),
        (
            "a scalar string under its @length minimum keeps its value",
            "POST",
            "/rerank".to_string(),
            json!({
                "queries": queries(1),
                "sources": sources(1),
                "rerankingConfiguration": {
                    "type": "BEDROCK_RERANKING_MODEL",
                    "bedrockRerankingConfiguration": { "modelConfiguration": { "modelArn": MODEL } },
                },
                "nextToken": "",
            }),
            scalar_clause("", "nextToken", &at_least(1)),
        ),
    ];

    let router = router();
    for (case, method, path, body, clause) in &rows {
        if clause.is_empty() {
            // A collection INSIDE its bound is not refused for its length at all, which is
            // what makes the rows above refusals of the bound rather than of the member.
            let (status, answered) = call(&router, method, path, body.clone()).await;
            assert!(
                !message(&answered).contains("failed to satisfy constraint"),
                "case {case:?}: a collection inside its bound must not be refused for its \
                 length, got {status} {answered:?}"
            );
            continue;
        }
        let (status, refused) = call(&router, method, path, body.clone()).await;
        assert_eq!(
            (status, error_code(&refused)),
            INVALID,
            "case {case:?}: expected {INVALID:?}, got {status} {refused:?}"
        );
        let expected = one_error(clause);
        let stated = message(&refused);
        assert_eq!(
            stated, expected,
            "case {case:?}: expected message {expected:?}, got {stated:?}"
        );
    }
}

/// THE RULE for a request breaking a COLLECTION's bound AND a scalar's at once: the
/// envelope counts both clauses, and each states its own value half — the `@sensitive`
/// collection's none, the scalar's its own.
#[tokio::test]
async fn an_envelope_over_two_violations_states_each_value_half() {
    let router = router();
    let (status, refused) = call(
        &router,
        "POST",
        "/rerank",
        json!({
            "queries": queries(QUERIES_MAX + 1),
            "sources": sources(1),
            "rerankingConfiguration": {
                "type": "BEDROCK_RERANKING_MODEL",
                "bedrockRerankingConfiguration": {
                    "modelConfiguration": { "modelArn": MODEL },
                    "numberOfResults": 0,
                },
            },
        }),
    )
    .await;
    assert_eq!(
        (status, error_code(&refused)),
        INVALID,
        "two violations: expected {INVALID:?}, got {status} {refused:?}"
    );
    let stated = message(&refused);
    assert!(
        stated.starts_with("2 validation errors detected: "),
        "two violations: expected the two-error envelope, got {stated:?}"
    );
    let list_clause = valueless_clause("queries", &at_most(QUERIES_MAX));
    assert!(
        stated.contains(&list_clause),
        "two violations: expected the clause {list_clause:?}, got {stated:?}"
    );
    let range_clause = scalar_clause(
        "0",
        "rerankingConfiguration.bedrockRerankingConfiguration.numberOfResults",
        "Member must have value greater than or equal to 1",
    );
    assert!(
        stated.contains(&range_clause),
        "two violations: expected the clause {range_clause:?}, got {stated:?}"
    );
    assert!(
        !stated.contains("items"),
        "two violations: a clause never states the framework's count summary, got {stated:?}"
    );
}

/// One `functionResult` the caller returns, as the request carries it and as a clause quotes
/// it.
///
/// The member list and its ORDER are the model's
/// (`smithy_query(query="shape",shape="FunctionResult")` -> `actionGroup`,
/// `confirmationState`, `function`, `responseBody`, `responseState`, `agentId`; and
/// `smithy_query(query="shape",shape="ContentBody")` -> `body`, `images`), written out here
/// so the expected text is the template and not a second copy of the renderer.
struct FunctionResult<'r> {
    action_group: &'r str,
    confirmation_state: Option<&'r str>,
    function: Option<&'r str>,
    /// The response body as `(content type, the body text that entry carries)`, or `None`
    /// for a request that sends no `responseBody` at all.
    response_body: Option<(&'r str, Option<&'r str>)>,
    response_state: Option<&'r str>,
    agent_id: Option<&'r str>,
}

impl FunctionResult<'_> {
    /// The `InvocationResultMember` the request sends: the union's `functionResult` arm,
    /// carrying only the members this result sets.
    fn submitted(&self) -> Value {
        let mut result = json!({ "actionGroup": self.action_group });
        if let Some(state) = self.confirmation_state {
            result["confirmationState"] = json!(state);
        }
        if let Some(function) = self.function {
            result["function"] = json!(function);
        }
        if let Some((content_type, body)) = self.response_body {
            let mut content = json!({});
            if let Some(body) = body {
                content["body"] = json!(body);
            }
            result["responseBody"] = json!({ content_type: content });
        }
        if let Some(state) = self.response_state {
            result["responseState"] = json!(state);
        }
        if let Some(agent) = self.agent_id {
            result["agentId"] = json!(agent);
        }
        json!({ "functionResult": result })
    }

    /// The element as a clause quotes it: the union over its one member, that member over
    /// every member its own shape declares.
    fn quoted(&self) -> String {
        let body = match self.response_body {
            Some((content_type, body)) => format!(
                "{{{content_type}=ContentBody(body={}, images={UNSET})}}",
                body.unwrap_or(UNSET)
            ),
            None => UNSET.to_string(),
        };
        format!(
            "InvocationResultMember(functionResult=FunctionResult(actionGroup={}, \
             confirmationState={}, function={}, responseBody={body}, responseState={}, \
             agentId={}))",
            self.action_group,
            self.confirmation_state.unwrap_or(UNSET),
            self.function.unwrap_or(UNSET),
            self.response_state.unwrap_or(UNSET),
            self.agent_id.unwrap_or(UNSET),
        )
    }
}

/// The result the recorded return-control turn answers with: one function of one action
/// group, its text body, and nothing else set.
fn launch_code_result() -> FunctionResult<'static> {
    FunctionResult {
        action_group: "LaunchCodeLookup",
        confirmation_state: None,
        function: Some("get_launch_code"),
        response_body: Some(("TEXT", Some("The launch code for Falcon is LAUNCHCODE"))),
        response_state: None,
        agent_id: None,
    }
}

/// One `InvokeInlineAgent` body returning `results` for an invocation.
///
/// The `invocationId` is carried the way the caller's half of the return-control contract
/// carries one, and is never read by these rows: the model's own bound on the result list is
/// checked at the boundary, before any handler resolves an id.
fn returning(results: Value) -> Value {
    let mut body = request(INLINE_MODEL);
    body["inlineSessionState"] = json!({
        "invocationId": "9fd4de62-4e2a-4ee0-9f6c-ec6e1f0a4d1c",
        "returnControlInvocationResults": results,
    });
    body
}

/// Drive one inline-agent row and give back the refusal it earned.
async fn returned(emulator: &Emulator, session: &str, results: Value) -> Answer {
    emulator
        .invoke_inline_agent(ACCOUNT_KEY, session, returning(results))
        .await
}

/// Assert `answer` is the modelled refusal and state the clause it must carry.
fn refused_with(case: &str, answer: &Answer, clause: &str) {
    assert_eq!(
        (answer.status, answer.error_code(case).as_str()),
        INVALID,
        "case {case:?}: expected {INVALID:?}, got {} {:?}",
        answer.status,
        answer.json(case)
    );
    let expected = one_error(clause);
    let stated = answer.message(case);
    assert_eq!(
        stated, expected,
        "case {case:?}: expected message {expected:?}, got {stated:?}"
    );
}

/// THE RULE over a TABLE, on the one operation this service hand-authors because its output
/// is a `@streaming` union: a returned-result list outside its `@length` is refused quoting
/// the results submitted, over every member the model declares, whichever members the request
/// set.
#[tokio::test]
async fn a_returned_result_list_outside_its_length_quotes_the_results_submitted() {
    /// One request's results: the results it sends and how many copies of them.
    struct Row {
        case: &'static str,
        session: &'static str,
        results: Vec<FunctionResult<'static>>,
        copies: usize,
    }

    let rows: Vec<Row> = vec![
        Row {
            case: "six results of the recorded turn, one over the maximum",
            session: "quoted-six-results",
            results: vec![launch_code_result()],
            copies: RESULTS_MAX + 1,
        },
        Row {
            case: "six results whose every member is set",
            session: "quoted-every-member",
            results: vec![FunctionResult {
                action_group: "FleetPumps",
                confirmation_state: Some("CONFIRM"),
                function: Some("schedule_visit"),
                response_body: Some(("TEXT", Some("the visit is booked for Tuesday"))),
                response_state: Some("REPROMPT"),
                agent_id: Some("ABCDEFGHIJ"),
            }],
            copies: RESULTS_MAX + 1,
        },
        Row {
            case: "six results keyed by another content type",
            session: "quoted-images-key",
            results: vec![FunctionResult {
                action_group: "FleetPumps",
                confirmation_state: None,
                function: Some("show_part"),
                response_body: Some(("IMAGES", Some("the part diagram"))),
                response_state: None,
                agent_id: None,
            }],
            copies: RESULTS_MAX + 1,
        },
        Row {
            case: "six results with no response body at all",
            session: "quoted-no-body",
            results: vec![FunctionResult {
                action_group: "FleetPumps",
                confirmation_state: None,
                function: None,
                response_body: None,
                response_state: None,
                agent_id: None,
            }],
            copies: RESULTS_MAX + 1,
        },
        Row {
            case: "six results of three different functions",
            session: "quoted-three-functions",
            results: vec![
                launch_code_result(),
                FunctionResult {
                    action_group: "FleetPumps",
                    confirmation_state: Some("DENY"),
                    function: Some("close_order"),
                    response_body: Some(("TEXT", Some("the order is closed"))),
                    response_state: None,
                    agent_id: None,
                },
                FunctionResult {
                    action_group: "FleetPumps",
                    confirmation_state: None,
                    function: Some("reopen_order"),
                    response_body: Some(("TEXT", Some("the order is open again"))),
                    response_state: None,
                    agent_id: Some("JIHGFEDCBA"),
                },
            ],
            copies: 2,
        },
    ];

    let emulator = Emulator::new();
    for row in &rows {
        let submitted: Vec<Value> = (0..row.copies)
            .flat_map(|_| row.results.iter().map(FunctionResult::submitted))
            .collect();
        assert!(
            submitted.len() > RESULTS_MAX,
            "case {:?}: the row must break the modelled bound, got {} results",
            row.case,
            submitted.len()
        );
        let quoted = quoted_list(
            &(0..row.copies)
                .flat_map(|_| row.results.iter().map(FunctionResult::quoted))
                .collect::<Vec<String>>(),
        );
        let answer = returned(&emulator, row.session, Value::Array(submitted)).await;
        refused_with(
            row.case,
            &answer,
            &collection_clause(&quoted, RESULTS_MEMBER, &at_most(RESULTS_MAX)),
        );
    }
}

/// THE RULE's boundary, where the value a clause would quote cannot be NAMED: a nested
/// structure the request left empty wrote no member, and a member schema carries the member's
/// own name rather than its target shape's
/// (`service_common::validation::DeclaringShapes`), so the shape such a value renders under
/// is not recoverable and the clause states no value at all rather than one missing its own
/// name. Every other row of the table above quotes the results in full.
#[tokio::test]
async fn a_result_whose_nested_structure_is_empty_quotes_nothing() {
    let emulator = Emulator::new();
    let results: Vec<Value> = (0..=RESULTS_MAX)
        .map(|_| {
            FunctionResult {
                action_group: "FleetPumps",
                confirmation_state: None,
                function: Some("list_parts"),
                // A `responseBody` entry carrying no `body` and no `images`.
                response_body: Some(("TEXT", None)),
                response_state: None,
                agent_id: None,
            }
            .submitted()
        })
        .collect();
    let answer = returned(&emulator, "unnamed-empty-body", Value::Array(results)).await;
    refused_with(
        "six results whose content body carries no member",
        &answer,
        &valueless_clause(RESULTS_MEMBER, &at_most(RESULTS_MAX)),
    );
}

/// THE RULE at the other bound: an EMPTY result list is under the modelled minimum, and the
/// clause quotes the empty list the request sent rather than nothing at all.
#[tokio::test]
async fn an_empty_returned_result_list_quotes_the_empty_list() {
    let emulator = Emulator::new();
    let answer = returned(&emulator, "quoted-empty-results", json!([])).await;
    refused_with(
        "an empty result list, under its minimum",
        &answer,
        &collection_clause("[]", RESULTS_MEMBER, &at_least(RESULTS_MIN)),
    );
}

/// THE RULE on a SECOND hand-authored operation, so it belongs to this service's refusal seam
/// rather than to one handler: `InvokeFlow`'s `inputs` is `@length` 1..1
/// (`smithy_query(query="operation",operation="InvokeFlow")`), and a request outside that bound
/// is refused in the same two forms — the submitted list quoted, and nothing quoted where the
/// list holds a value the model marks `@sensitive` (`FlowInput.content`,
/// `smithy_query(query="shape",shape="FlowInput")`).
#[tokio::test]
async fn a_flow_input_list_is_refused_the_same_way() {
    let emulator = Emulator::new();
    let flow_input = json!({
        "nodeName": "FlowInputNode",
        "nodeOutputName": "document",
        "content": { "document": "what is the launch code" },
    });

    // (case, the inputs the request sends, the clause the refusal states)
    let rows: [(&str, Value, String); 2] = [
        (
            "an empty flow input list, under its minimum",
            json!([]),
            collection_clause("[]", "inputs", &at_least(1)),
        ),
        (
            "two flow inputs, over its maximum, each carrying a sensitive content member",
            json!([flow_input, flow_input]),
            valueless_clause("inputs", &at_most(1)),
        ),
    ];

    for (case, inputs, clause) in &rows {
        // A well-formed `flowIdentifier` / `flowAliasIdentifier` no flow carries: the model's
        // own bound on `inputs` is checked at the boundary, before anything is looked up.
        let answer = emulator
            .bedrock(
                ACCOUNT_KEY,
                "POST",
                "/flows/ABCDEFGHIJ/aliases/TSTALIASID",
                Some(json!({ "inputs": inputs })),
            )
            .await;
        assert_eq!(
            (answer.status, answer.error_code(case).as_str()),
            INVALID,
            "case {case:?}: expected {INVALID:?}, got {} {:?}",
            answer.status,
            answer.json(case)
        );
        let expected = one_error(clause);
        let stated = answer.message(case);
        assert_eq!(
            stated, expected,
            "case {case:?}: expected message {expected:?}, got {stated:?}"
        );
    }
}

/// THE RULE's `@sensitive` half, on a result the model marks one member of: the clause names
/// the member and the requirement and quotes nothing, while the SAME result without that
/// member is quoted in full.
///
/// `ApiResult.apiPath` is that member (`smithy_query(query="shape",shape="ApiResult")` ->
/// `apiPath` `@sensitive`), and the members it is quoted over, in order, are that same
/// shape's.
#[tokio::test]
async fn a_returned_result_carrying_a_sensitive_member_quotes_nothing() {
    let emulator = Emulator::new();
    let api_result = |with_path: bool| {
        let mut result = json!({
            "actionGroup": "LaunchCodeLookup",
            "httpMethod": "POST",
            "httpStatusCode": 200,
            "responseBody": { "TEXT": { "body": "the launch code" } },
        });
        if with_path {
            result["apiPath"] = json!("/launch-code");
        }
        json!({ "apiResult": result })
    };
    let quoted_without_path = format!(
        "InvocationResultMember(apiResult=ApiResult(actionGroup=LaunchCodeLookup, \
         httpMethod=POST, apiPath={UNSET}, confirmationState={UNSET}, responseState={UNSET}, \
         httpStatusCode=200, responseBody={{TEXT=ContentBody(body=the launch code, \
         images={UNSET})}}, agentId={UNSET}))"
    );

    // (case, session, whether the sensitive member is sent, the clause the refusal states)
    let rows: [(&str, &str, bool, String); 2] = [
        (
            "six api results carrying no sensitive member",
            "quoted-api-results",
            false,
            collection_clause(
                &quoted_list(&vec![quoted_without_path; RESULTS_MAX + 1]),
                RESULTS_MEMBER,
                &at_most(RESULTS_MAX),
            ),
        ),
        (
            "six api results carrying the sensitive apiPath",
            "sensitive-api-path",
            true,
            valueless_clause(RESULTS_MEMBER, &at_most(RESULTS_MAX)),
        ),
    ];

    for (case, session, with_path, clause) in &rows {
        let results: Vec<Value> = (0..=RESULTS_MAX).map(|_| api_result(*with_path)).collect();
        let answer = returned(&emulator, session, Value::Array(results)).await;
        refused_with(case, &answer, clause);
    }
}
