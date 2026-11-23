// SPDX-License-Identifier: CC0-1.0

//! `ListFlows`, end to end over HTTP: the listing enumerates the flows the account
//! holds and reports each of them from the stored record the matching `GetFlow`
//! read reports.
//!
//! The rules under test, each asserted over a table of inputs rather than one row:
//!
//! * one summary per stored flow, ordered by the instant the flow was created,
//!   every member projected off the record the read reports it from;
//! * a page carries at most `maxResults` summaries and never more than the ten the
//!   modelled `flowSummaries` list admits, and paging with the `nextToken` a page
//!   hands out reports every flow exactly once;
//! * an account holding no flow answers 200 with an empty `flowSummaries`, which is
//!   not an error;
//! * whatever status a flow stands in, the listing reports that status;
//! * a `nextToken` this service never issued is a bad request;
//! * the listing is a READ: it reports the same page however often it is called and
//!   leaves every flow exactly where it stood, while a flow created appears in it
//!   and a flow deleted is gone from it.
//!
//! Every assertion goes through the router, so it exercises the real wire path
//! (route matching, model constraint validation, serialization) rather than calling
//! a handler directly. The two things these tests reach into the store for are
//! SPREADING THE CREATION INSTANTS the flows carry, so the order the listing is
//! asserted in is a property of the stored records rather than of how fast the host
//! ran the creates, and PLACING a flow in each status the stored `FlowStatus`
//! admits, so the status rows do not depend on a clock.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_bedrockagent_amazonbedrockagentbuildtimelambda::state::{
    service_state, FlowStatus, MAX_FLOWS_PER_PAGE,
};
use service_com_amazonaws_bedrockagent_amazonbedrockagentbuildtimelambda::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// A SigV4 `Authorization` header scoped to the `bedrock` signing name: the routing
/// key this service claims on, and the source of the region its ARNs carry.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/eu-west-1/bedrock/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0123456789abcdef";

/// A role ARN matching the modelled `FlowExecutionRoleArn` pattern
/// `^arn:aws(-[^:]+)?:iam::([0-9]{12})?:role/(service-role/)?.+$`.
const EXECUTION_ROLE: &str =
    "arn:aws:iam::000000000000:role/service-role/AmazonBedrockExecutionRoleForFlows";

/// How many pages a listing of this test's inputs can possibly need before the
/// paging under test is broken rather than merely long.
const PAGE_BUDGET: usize = 64;

/// The members a `FlowSummary` carries, as the modelled shape declares them
/// (`smithy_query(query="shape",shape="FlowSummary")`): the required ones plus the
/// optional `description`.
const SUMMARY_MEMBERS: [&str; 8] = [
    "name",
    "description",
    "id",
    "arn",
    "status",
    "createdAt",
    "updatedAt",
    "version",
];

/// The members the modelled `FlowSummary` requires.
const REQUIRED_SUMMARY_MEMBERS: [&str; 7] = [
    "name",
    "id",
    "arn",
    "status",
    "createdAt",
    "updatedAt",
    "version",
];

/// One value as a percent-encoded URI component, as a client encodes a `@httpQuery`
/// value or a non-greedy `@httpLabel`.
fn encoded(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace(':', "%3A")
        .replace('/', "%2F")
}

/// One HTTP answer from the emulator, parsed.
#[derive(Debug)]
struct Answer {
    status: u16,
    /// The restJson1 error shape name, from `x-amzn-errortype` or the body's
    /// `__type`; `None` on a success.
    error_type: Option<String>,
    body: Value,
}

impl Answer {
    fn member(&self, path: &[&str]) -> &Value {
        let mut at = &self.body;
        for key in path {
            at = &at[*key];
        }
        at
    }

    fn string(&self, path: &[&str]) -> String {
        self.member(path)
            .as_str()
            .unwrap_or_else(|| panic!("{path:?} is not a string in {:?}", self.body))
            .to_string()
    }

    /// The `flowSummaries` this answer reports. The member is modelled REQUIRED, so
    /// a success always carries the list, empty or not.
    fn summaries(&self, case: &str) -> Vec<Value> {
        self.member(&["flowSummaries"])
            .as_array()
            .unwrap_or_else(|| {
                panic!(
                    "case {case}: expected a flowSummaries list, got {:?}",
                    self.body
                )
            })
            .clone()
    }

    /// The `nextToken` this answer hands out, if any.
    fn next_token(&self) -> Option<String> {
        self.member(&["nextToken"]).as_str().map(ToString::to_string)
    }
}

/// The emulator under test: one router over one shared engine, exactly as the
/// production router mounts this service.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
    }

    async fn call(&self, method: &str, uri: &str, body: Option<Value>) -> Answer {
        let bytes = body
            .map(|value| Bytes::from(serde_json::to_vec(&value).expect("serialize request body")))
            .unwrap_or_default();
        let request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "bedrock-agent.eu-west-1.amazonaws.com")
            .header("authorization", AUTHORIZATION)
            .header("content-type", "application/json")
            .body(bytes)
            .expect("build the request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let header_error = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(ToString::to_string);
        let raw = response.into_body();
        let body: Value = if raw.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&raw).unwrap_or_else(|error| {
                panic!(
                    "{method} {uri} -> {status} body is not JSON ({error}): {}",
                    String::from_utf8_lossy(&raw)
                )
            })
        };
        let error_type = header_error.or_else(|| {
            body.get("__type")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        });
        Answer {
            status,
            error_type,
            body,
        }
    }

    /// `CreateFlow`, the operation that mints the flow and the DRAFT version the
    /// listing reports.
    async fn create_flow(&self, case: &str, request: Value) -> Answer {
        let created = self.call("POST", "/flows/", Some(request)).await;
        assert_eq!(
            created.status, 201,
            "case {case}: expected CreateFlow -> 201, got {created:?}"
        );
        created
    }

    /// `GetFlow` on any `FlowIdentifier` form.
    async fn get_flow(&self, identifier: &str) -> Answer {
        self.call("GET", &format!("/flows/{}/", encoded(identifier)), None)
            .await
    }

    /// `DeleteFlow`, which takes the flow out of the store and so out of the
    /// listing.
    async fn delete_flow(&self, case: &str, identifier: &str) -> Answer {
        let deleted = self
            .call("DELETE", &format!("/flows/{}/", encoded(identifier)), None)
            .await;
        assert_eq!(
            deleted.status, 200,
            "case {case}: expected DeleteFlow -> 200, got {deleted:?}"
        );
        deleted
    }

    /// `ListFlows`, with whichever of its two `@httpQuery` members the case names.
    async fn list_flows(&self, max_results: Option<i32>, next_token: Option<&str>) -> Answer {
        let mut query: Vec<String> = Vec::new();
        if let Some(max_results) = max_results {
            query.push(format!("maxResults={max_results}"));
        }
        if let Some(next_token) = next_token {
            query.push(format!("nextToken={}", encoded(next_token)));
        }
        let uri = if query.is_empty() {
            "/flows/".to_string()
        } else {
            format!("/flows/?{}", query.join("&"))
        };
        self.call("GET", &uri, None).await
    }

    /// Every summary the listing reports, read page by page with the `nextToken`
    /// each page hands out. Asserts along the way that no page is longer than the
    /// bound in force.
    async fn drain(&self, case: &str, max_results: Option<i32>) -> Vec<Value> {
        let bound = max_results
            .map(|requested| usize::try_from(requested).unwrap_or(MAX_FLOWS_PER_PAGE))
            .unwrap_or(MAX_FLOWS_PER_PAGE)
            .min(MAX_FLOWS_PER_PAGE);
        let mut collected: Vec<Value> = Vec::new();
        let mut token: Option<String> = None;
        for page in 0..PAGE_BUDGET {
            let answer = self.list_flows(max_results, token.as_deref()).await;
            assert_eq!(
                answer.status, 200,
                "case {case}: expected ListFlows -> 200 on page {page}, got {answer:?}"
            );
            let summaries = answer.summaries(case);
            assert!(
                summaries.len() <= bound,
                "case {case}: expected at most {bound} summaries on page {page}, \
                 got {} in {answer:?}",
                summaries.len()
            );
            collected.extend(summaries.iter().cloned());
            match answer.next_token() {
                None => return collected,
                Some(next) => {
                    assert!(
                        !summaries.is_empty(),
                        "case {case}: a page that hands out {next:?} reported something, \
                         got {answer:?}"
                    );
                    token = Some(next);
                }
            }
        }
        panic!("case {case}: the listing still had not finished after {PAGE_BUDGET} pages");
    }

    /// Spread the creation instants of the flows `flow_ids` names, in the order
    /// given: the first carries the earliest instant. The order the listing reports
    /// is a property of those stored instants, so setting them is what makes the
    /// expected order of a case exact rather than a race against the host's clock
    /// resolution.
    fn spread_creation_instants(&self, case: &str, flow_ids: &[String]) {
        let state = service_state(&*self.engine).expect("the bedrock-agent store");
        let mut state = state.lock();
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_767_225_600);
        for (position, flow_id) in flow_ids.iter().enumerate() {
            let flow = state
                .flows
                .get_mut(flow_id)
                .unwrap_or_else(|| panic!("case {case}: expected the stored flow {flow_id}"));
            let created_at = base + Duration::from_secs(u64::try_from(position).unwrap_or(0) + 1);
            flow.created_at = created_at;
            flow.updated_at = created_at;
        }
    }

    /// Put the stored flow in `status`, with no transition left owed on it, so the
    /// status the listing reports is the stored one and not one a clock is moving.
    fn place_in_status(&self, case: &str, flow_id: &str, status: FlowStatus) {
        let state = service_state(&*self.engine).expect("the bedrock-agent store");
        let mut state = state.lock();
        let flow = state
            .flows
            .get_mut(flow_id)
            .unwrap_or_else(|| panic!("case {case}: expected the stored flow {flow_id}"));
        flow.status = status;
        flow.pending_preparation = None;
    }
}

/// The `CreateFlow` request for the `index`th flow of a case. The rows a case
/// creates differ in what the summary has to carry: a described flow, a flow with
/// no description at all, and a described flow carrying a definition.
fn flow_request(index: usize) -> Value {
    match index % 3 {
        0 => json!({
            "name": format!("listed-flow-{index}"),
            "description": format!("the {index}th listed flow"),
            "executionRoleArn": EXECUTION_ROLE,
        }),
        1 => json!({
            "name": format!("bare-listed-flow-{index}"),
            "executionRoleArn": EXECUTION_ROLE,
        }),
        _ => json!({
            "name": format!("defined-listed-flow-{index}"),
            "description": format!("the {index}th listed flow, with a definition"),
            "executionRoleArn": EXECUTION_ROLE,
            "definition": passthrough_definition(),
        }),
    }
}

/// A passthrough flow definition: an `Input` node wired straight to an `Output`
/// node. The node types, the union keys and the IO data types are the modelled
/// `FlowNodeType` / `FlowNodeConfiguration` / `FlowNodeIODataType` values.
fn passthrough_definition() -> Value {
    json!({
        "nodes": [
            {
                "name": "FlowInput",
                "type": "Input",
                "configuration": { "input": {} },
                "outputs": [{ "name": "document", "type": "String" }],
            },
            {
                "name": "FlowOutput",
                "type": "Output",
                "configuration": { "output": {} },
                "inputs": [{ "name": "document", "type": "String", "expression": "$.data" }],
            },
        ],
        "connections": [
            {
                "name": "InputToOutput",
                "type": "Data",
                "source": "FlowInput",
                "target": "FlowOutput",
                "configuration": {
                    "data": { "sourceOutput": "document", "targetInput": "document" }
                },
            },
        ],
    })
}

/// Assert that `summary` reports the same values the `GetFlow` read of the same
/// flow reports — which is what makes a summary a projection of the stored record
/// rather than a second account of it — and that it carries every member the
/// modelled shape requires and no member the shape does not declare.
async fn assert_summary_matches_the_read(
    emulator: &Emulator,
    case: &str,
    summary: &Value,
    identifier: &str,
) {
    let read = emulator.get_flow(identifier).await;
    assert_eq!(
        read.status, 200,
        "case {case}: expected GetFlow -> 200 for {identifier}, got {read:?}"
    );
    for member in SUMMARY_MEMBERS {
        assert_eq!(
            summary.get(member).unwrap_or(&Value::Null),
            read.member(&[member]),
            "case {case}: the summary's {member} is the one the read reports, \
             got {summary:?} against {read:?}"
        );
    }
    for member in REQUIRED_SUMMARY_MEMBERS {
        assert!(
            summary.get(member).and_then(Value::as_str).is_some(),
            "case {case}: expected the required member {member} in {summary:?}"
        );
    }
    let object = summary
        .as_object()
        .unwrap_or_else(|| panic!("case {case}: expected a summary object, got {summary:?}"));
    for member in object.keys() {
        assert!(
            SUMMARY_MEMBERS.contains(&member.as_str()),
            "case {case}: {member} is not a member of the modelled FlowSummary, \
             got {summary:?}"
        );
    }
}

/// The listing reports one summary per stored flow, in the order the flows were
/// created, and pages over them.
///
/// Rows differ in the COUNT of flows held — none, fewer than one page, exactly one
/// page and more than one page — and in the `maxResults` the request names,
/// including the bounds of its modelled `range 1..1000`.
#[tokio::test]
async fn list_flows_reports_every_flow_in_creation_order() {
    for count in [0_usize, 1, 3, 10, 12, 23] {
        for max_results in [None, Some(1), Some(3), Some(10), Some(1000)] {
            let case = format!("{count} flow(s), maxResults {max_results:?}");
            let emulator = Emulator::new();

            let mut created: Vec<Value> = Vec::new();
            for index in 0..count {
                created.push(emulator.create_flow(&case, flow_request(index)).await.body);
            }
            // The expected order is the order the flows were CREATED in, which this
            // reverses relative to the order they were stored in: a listing that
            // reported insertion order, or the store's own iteration order, would
            // not match.
            let ids: Vec<String> = created
                .iter()
                .rev()
                .map(|flow| flow["id"].as_str().expect("the minted id").to_string())
                .collect();
            emulator.spread_creation_instants(&case, &ids);

            let summaries = emulator.drain(&case, max_results).await;
            assert_eq!(
                summaries.len(),
                count,
                "case {case}: expected one summary per flow, got {summaries:?}"
            );
            let listed: Vec<String> = summaries
                .iter()
                .map(|summary| summary["id"].as_str().unwrap_or_default().to_string())
                .collect();
            assert_eq!(
                listed, ids,
                "case {case}: the listing reports the flows in creation order"
            );
            for summary in &summaries {
                let id = summary["id"].as_str().expect("the summary's id");
                assert_eq!(
                    summary["version"],
                    json!("DRAFT"),
                    "case {case}: the listing reports each flow's DRAFT version, \
                     got {summary:?}"
                );
                assert_summary_matches_the_read(&emulator, &case, summary, id).await;
            }
        }
    }
}

/// An account holding no flow answers 200 with an empty `flowSummaries` and no
/// token, in each form of the request, rather than any error.
#[tokio::test]
async fn list_flows_on_an_account_holding_none_reports_an_empty_list() {
    let emulator = Emulator::new();
    for max_results in [None, Some(1), Some(10), Some(1000)] {
        let case = format!("an empty account, maxResults {max_results:?}");
        let answer = emulator.list_flows(max_results, None).await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected ListFlows -> 200, got {answer:?}"
        );
        assert_eq!(
            answer.summaries(&case),
            Vec::<Value>::new(),
            "case {case}: expected an empty flowSummaries, got {answer:?}"
        );
        assert_eq!(
            answer.next_token(),
            None,
            "case {case}: an account with nothing more to report hands out no token, \
             got {answer:?}"
        );
    }
}

/// Whatever status a flow stands in, the listing reports that status — the same one
/// the read of the flow reports.
///
/// Rows are every value the stored `FlowStatus` admits, one flow in each, listed
/// together.
#[tokio::test]
async fn list_flows_reports_the_status_each_flow_stands_in() {
    let case = "a flow in each status";
    let emulator = Emulator::new();

    let statuses = [
        (FlowStatus::NotPrepared, "NotPrepared"),
        (FlowStatus::Preparing, "Preparing"),
        (FlowStatus::Prepared, "Prepared"),
        (FlowStatus::Failed, "Failed"),
    ];
    let mut expected: Vec<(String, &str)> = Vec::new();
    for (index, (status, wire)) in statuses.iter().enumerate() {
        let created = emulator.create_flow(case, flow_request(index)).await;
        let flow_id = created.string(&["id"]);
        emulator.place_in_status(case, &flow_id, *status);
        expected.push((flow_id, wire));
    }

    let summaries = emulator.drain(case, None).await;
    assert_eq!(
        summaries.len(),
        expected.len(),
        "case {case}: expected one summary per flow, got {summaries:?}"
    );
    for (flow_id, wire) in &expected {
        let summary = summaries
            .iter()
            .find(|summary| summary["id"].as_str() == Some(flow_id.as_str()))
            .unwrap_or_else(|| panic!("case {case}: expected {flow_id} in {summaries:?}"));
        assert_eq!(
            summary["status"],
            json!(wire),
            "case {case}: expected {flow_id} reported {wire}, got {summary:?}"
        );
        assert_summary_matches_the_read(&emulator, case, summary, flow_id).await;
    }
}

/// A `nextToken` this service never issued names no position in the listing, which
/// is a bad request rather than an empty page or a server error; the token a page
/// does hand out is accepted.
///
/// Rows differ in HOW the token misses the one spelling this listing hands out: a
/// position before the first, a padded number, the spelling another listing of this
/// service uses, a cursor whose id is not one this service mints, and a value this
/// service never writes at all.
#[tokio::test]
async fn list_flows_refuses_a_token_it_never_issued() {
    let case = "an unissued nextToken";
    let emulator = Emulator::new();
    for index in 0..12 {
        emulator.create_flow(case, flow_request(index)).await;
    }

    for token in [
        "0",
        "01",
        "1",
        "DRAFT",
        "not-a-token",
        "12345-short",
        "0-abcdefghij",
        "-abcdefghij",
        "17-abcdefghij!",
        "000017-abcdefghij",
    ] {
        let refused = emulator.list_flows(None, Some(token)).await;
        assert_eq!(
            refused.status, 400,
            "case {case} ({token:?}): expected ListFlows -> 400, got {refused:?}"
        );
        assert_eq!(
            refused.error_type.as_deref(),
            Some("ValidationException"),
            "case {case} ({token:?}): expected ValidationException, got {refused:?}"
        );
    }

    // The token the listing itself hands out IS accepted, so the refusals above are
    // about the token's provenance and not about paging being refused outright.
    let first = emulator.list_flows(None, None).await;
    let token = first
        .next_token()
        .unwrap_or_else(|| panic!("case {case}: expected a token over 12 flows, got {first:?}"));
    let second = emulator.list_flows(None, Some(&token)).await;
    assert_eq!(
        second.status, 200,
        "case {case}: expected the listing's own token to be accepted, got {second:?}"
    );
    assert_eq!(
        second.summaries(case).len(),
        2,
        "case {case}: expected the remaining flows on the second page, got {second:?}"
    );
}

/// The listing is a READ: it reports the same page however often it is called and
/// leaves every flow exactly where it stood. A flow created appears in it; a flow
/// deleted is gone from it.
#[tokio::test]
async fn listing_flows_changes_nothing_a_later_caller_sees() {
    let case = "repeated listings";
    let emulator = Emulator::new();

    let mut ids: Vec<String> = Vec::new();
    for index in 0..4 {
        ids.push(
            emulator
                .create_flow(case, flow_request(index))
                .await
                .string(&["id"]),
        );
    }
    let first_id = ids[0].clone();

    let before_listing = emulator.list_flows(None, None).await;
    let before_read = emulator.get_flow(&first_id).await;

    for round in 0..3 {
        let again = emulator.list_flows(None, None).await;
        assert_eq!(
            again.body, before_listing.body,
            "case {case}: round {round} of the listing reports the same page"
        );
    }
    assert_eq!(
        emulator.get_flow(&first_id).await.body,
        before_read.body,
        "case {case}: listing leaves the flow as it stood"
    );

    let reported: BTreeSet<String> = before_listing
        .summaries(case)
        .iter()
        .map(|summary| summary["id"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        reported,
        ids.iter().cloned().collect::<BTreeSet<String>>(),
        "case {case}: the listing reports each flow created exactly once"
    );

    // Absence is reportable too: the flow a delete removed is in no later page,
    // while every other flow still is.
    emulator.delete_flow(case, &first_id).await;
    let after: BTreeSet<String> = emulator
        .drain(case, None)
        .await
        .iter()
        .map(|summary| summary["id"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        after,
        ids.iter().skip(1).cloned().collect::<BTreeSet<String>>(),
        "case {case}: the deleted flow is gone from the listing and the others remain"
    );
}
