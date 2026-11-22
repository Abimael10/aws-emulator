// SPDX-License-Identifier: CC0-1.0

//! `ListHarnesses` over HTTP: the harnesses this account holds.
//!
//! One rule is under test, read off the Smithy model rather than off any one recorded
//! request: a listing of harnesses reports EXACTLY the harnesses the store persists — one
//! `HarnessSummary` per stored harness, every member read off that stored harness, in the
//! store's own deterministic order and paged by the caller's `maxResults`, with a
//! `nextToken` handed back only while harnesses are left over — while the listing writes
//! nothing of its own, and a `nextToken` this service never issued is its modelled
//! `ValidationException`.
//!
//! The suites below take that rule apart: what a page carries (rows spanning the number of
//! stored harnesses and the configuration each create carried), how a walk pages (rows
//! spanning every `maxResults` from the modelled minimum to the modelled maximum), what a
//! delete removes from the listing, that the listing changes nothing a later caller sees,
//! and what it refuses.
//!
//! Sources: `smithy_query(query="operation",operation="ListHarnesses,CreateHarness,\
//! GetHarness,DeleteHarness")` for the routes, the status codes, the members, the
//! `maxResults` range and the errors, and `smithy_query(query="shape",\
//! shape="HarnessSummary,HarnessStatus,HarnessVersion,HarnessName")` for the summary's
//! members, the status values and the name grammar asserted below.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The region the rows sign for.
const REGION: &str = "us-east-1";

/// A role ARN inside `RoleArn`'s pattern `^arn:aws(-[^:]+)?:iam::([0-9]{12})?:role/.+$`.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/HarnessRole";

/// The `HarnessVersion` a created harness carries: the grammar `^([1-9][0-9]{0,4})$` starts
/// at 1, and only an update publishes another one.
const FIRST_VERSION: &str = "1";

/// Every `HarnessStatus` value (`smithy_query shape=HarnessStatus`).
const HARNESS_STATUS_VALUES: [&str; 7] = [
    "CREATING",
    "CREATE_FAILED",
    "UPDATING",
    "UPDATE_FAILED",
    "READY",
    "DELETING",
    "DELETE_FAILED",
];

/// How long a row waiting for the `CREATING` -> `READY` transition the clock owes keeps
/// looking. Generous: the suite polls for the CONDITION rather than for a span, so this is
/// only the deadline past which the emulator is considered broken.
const SETTLE_DEADLINE: Duration = Duration::from_secs(30);

/// How long a poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::default();
    router.register_service(EmulatedService::new(engine));
    router
}

/// A SigV4 `Authorization` header for this service: what the router routes on, and what the
/// handlers read the ARN region from.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/bedrock-agentcore/\
         aws4_request, SignedHeaders=host, Signature=abc"
    )
}

/// One answer: the status, the `x-amzn-errortype` header and the parsed body.
struct Answer {
    status: u16,
    error_type: String,
    body: Value,
}

async fn send(router: &MultiServiceRouter, method: &str, uri: &str, body: Option<&Value>) -> Answer {
    let payload = body.map_or_else(Bytes::new, |value| {
        Bytes::from(serde_json::to_vec(value).expect("the request body serializes"))
    });
    let mut request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", authorization())
        .header("host", "bedrock-agentcore-control.us-east-1.amazonaws.com");
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let request = request.body(payload).expect("a well-formed request");
    let response = router.dispatch(request).await;
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    Answer {
        status: response.status().as_u16(),
        error_type,
        body: if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        },
    }
}

/// The member at `pointer`, or `Value::Null` when the body carries none.
fn at(body: &Value, pointer: &str) -> Value {
    body.pointer(pointer).cloned().unwrap_or(Value::Null)
}

/// A `CreateHarness` body for `name`: the two `@required` members, the `model` this service
/// has no default for, and whatever else the row configures.
fn harness_body(name: &str, extra: Value) -> Value {
    let mut body = json!({
        "harnessName": name,
        "executionRoleArn": ROLE_ARN,
        "model": {"bedrockModelConfig": {"modelId": "amazon.nova-pro-v1:0"}},
    });
    if let Value::Object(extra) = extra {
        for (key, value) in extra {
            body[key] = value;
        }
    }
    body
}

/// Create one harness and return (harnessId, the created `harness` member).
async fn create(router: &MultiServiceRouter, name: &str, extra: Value) -> (String, Value) {
    let answer = send(router, "POST", "/harnesses", Some(&harness_body(name, extra))).await;
    assert_eq!(
        answer.status, 201,
        "creating {name:?} must succeed for this row to say anything, got {} with {}",
        answer.status, answer.body
    );
    let harness = at(&answer.body, "/harness");
    let id = at(&harness, "/harnessId")
        .as_str()
        .unwrap_or_else(|| panic!("CreateHarness reports the minted harnessId, got {harness}"))
        .to_string();
    (id, harness)
}

/// `GET /harnesses{query}` — the route the model binds `ListHarnesses` at.
async fn list(router: &MultiServiceRouter, query: &str) -> Answer {
    send(router, "GET", &format!("/harnesses{query}"), None).await
}

/// `GET /harnesses/{harnessId}` — the read a listed summary is checked against.
async fn get(router: &MultiServiceRouter, id: &str) -> Answer {
    send(router, "GET", &format!("/harnesses/{id}"), None).await
}

/// The `harnesses` member of a page, as a vector.
fn harnesses(body: &Value) -> Vec<Value> {
    at(body, "/harnesses").as_array().cloned().unwrap_or_default()
}

/// The `harnessId`s a page carries, in the order the page carries them.
fn listed_ids(body: &Value) -> Vec<String> {
    harnesses(body)
        .iter()
        .map(|summary| {
            at(summary, "/harnessId")
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// Walk the whole listing with `query_prefix` (which carries the row's `maxResults`, if
/// any), following every `nextToken` to the end, and return the ids in page order.
async fn walk(router: &MultiServiceRouter, case: &str, query_prefix: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let mut token: Option<String> = None;
    // One page per stored harness is the worst case a page size of 1 can need; the bound
    // only stops a walk a broken token would otherwise loop on forever.
    for _ in 0..1_000 {
        let query = match token.as_deref() {
            None => query_prefix.to_string(),
            Some(next) => {
                if query_prefix.is_empty() {
                    format!("?nextToken={next}")
                } else {
                    format!("{query_prefix}&nextToken={next}")
                }
            }
        };
        let answer = list(router, &query).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: every page of the walk is HTTP 200, got {} with {}",
            answer.status, answer.body
        );
        ids.extend(listed_ids(&answer.body));
        match at(&answer.body, "/nextToken").as_str() {
            None => return ids,
            Some(next) => token = Some(next.to_string()),
        }
    }
    panic!("case {case:?}: the walk did not end; collected {ids:?}");
}

/// Poll `GET /harnesses/{id}` until it reports `READY` — the transition the clock owes a
/// freshly created harness — and return nothing once it holds.
async fn await_ready(router: &MultiServiceRouter, id: &str) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let read = get(router, id).await;
        if at(&read.body, "/harness/status") == json!("READY") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the CREATING -> READY transition never came due for {id}, last read {}",
            read.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// A page reports one summary per stored harness, every member read off that harness.
///
/// The rows differ along how many harnesses the store holds (none, one, several) and the
/// configuration each create carried, because neither changes which harnesses a listing
/// reports or which members a summary carries.
#[tokio::test]
async fn a_page_reports_one_summary_per_stored_harness() {
    // (case, the harnesses to create as (name, extra))
    let rows: Vec<(&str, Vec<(String, Value)>)> = vec![
        ("an account holding no harness", vec![]),
        (
            "one harness, the required members only",
            vec![("listMinimal".to_string(), json!({}))],
        ),
        (
            "three harnesses, each configured differently",
            vec![
                (
                    "listConfigured".to_string(),
                    json!({
                        "systemPrompt": [{"text": "You are a careful assistant."}],
                        "tools": [{"type": "agentcore_code_interpreter", "name": "python"}],
                    }),
                ),
                (
                    "h".to_string(),
                    json!({"maxIterations": 4, "maxTokens": 2048, "timeoutSeconds": 60}),
                ),
                (format!("h{}", "a".repeat(39)), json!({"memory": {"disabled": {}}})),
            ],
        ),
        (
            "five harnesses, letters, digits and underscores",
            vec![
                ("list_probe_1".to_string(), json!({})),
                ("list_probe_2".to_string(), json!({})),
                ("listProbe3".to_string(), json!({})),
                ("LISTPROBE4".to_string(), json!({})),
                ("l5".to_string(), json!({})),
            ],
        ),
    ];

    for (case, wanted) in rows {
        // A store of its own per row, so the row's count is the whole account's.
        let router = router();
        let mut created: Vec<(String, Value)> = Vec::new();
        for (name, extra) in &wanted {
            created.push(create(&router, name, extra.clone()).await);
        }

        let answer = list(&router, "").await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: listing the harnesses of an account is HTTP 200, got {} with {}",
            answer.status, answer.body
        );
        let listed = harnesses(&answer.body);
        assert_eq!(
            listed.len(),
            created.len(),
            "case {case:?}: the page carries one summary per stored harness, expected {}, got {}",
            created.len(),
            answer.body
        );
        for (id, harness) in &created {
            let summary = listed
                .iter()
                .find(|summary| at(summary, "/harnessId") == json!(id))
                .unwrap_or_else(|| {
                    panic!("case {case:?}: {id} was created, so it is listed, got {}", answer.body)
                });
            // Every identity member is the stored harness's own, as `CreateHarness`
            // reported it.
            for member in ["harnessId", "harnessName", "arn", "createdAt", "updatedAt"] {
                assert_eq!(
                    at(summary, &format!("/{member}")),
                    at(harness, &format!("/{member}")),
                    "case {case:?}: the summary's {member} is the stored harness's, got {summary}"
                );
            }
            assert_eq!(
                at(summary, "/harnessVersion"),
                json!(FIRST_VERSION),
                "case {case:?}: a created harness holds version {FIRST_VERSION}, got {summary}"
            );
            let status = at(summary, "/status")
                .as_str()
                .unwrap_or_default()
                .to_string();
            assert!(
                HARNESS_STATUS_VALUES.contains(&status.as_str()),
                "case {case:?}: the summary's status is a HarnessStatus value, got {status:?}"
            );
        }
        assert!(
            at(&answer.body, "/nextToken").is_null(),
            "case {case:?}: a page that holds every harness carries no nextToken, got {}",
            answer.body
        );
        // The store's own order is the list order, and it is ascending by the key every
        // harness route addresses a harness under.
        let ids = listed_ids(&answer.body);
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(
            ids, sorted,
            "case {case:?}: the page is in ascending harnessId order, got {}",
            answer.body
        );
    }
}

/// A walk pages by the caller's `maxResults` and reports every stored harness exactly once,
/// in one order, whatever the page size.
///
/// The rows span the `maxResults` the caller asks for: absent, the modelled minimum 1, sizes
/// that divide the store and that do not, and the modelled maximum 100 — none of which
/// changes which harnesses the walk reports or in which order.
#[tokio::test]
async fn a_walk_pages_by_max_results_and_reports_every_harness_once() {
    let router = router();
    let mut created: Vec<String> = Vec::new();
    for index in 0..7 {
        let (id, _) = create(&router, &format!("listWalk{index}"), json!({})).await;
        created.push(id);
    }
    created.sort();

    // (case, the query prefix the walk carries, the page size it asks for)
    let rows: Vec<(&str, &str, Option<usize>)> = vec![
        ("no maxResults, so the modelled maximum", "", None),
        ("maxResults at the modelled minimum", "?maxResults=1", Some(1)),
        ("maxResults=2, which does not divide the store", "?maxResults=2", Some(2)),
        ("maxResults=7, exactly the store", "?maxResults=7", Some(7)),
        ("maxResults=8, one past the store", "?maxResults=8", Some(8)),
        ("maxResults at the modelled maximum", "?maxResults=100", Some(100)),
    ];

    for (case, prefix, page_size) in rows {
        let walked = walk(&router, case, prefix).await;
        assert_eq!(
            walked, created,
            "case {case:?}: the walk reports every stored harness exactly once, in ascending \
             harnessId order, expected {created:?}, got {walked:?}"
        );

        // A page never carries more than the caller asked for, and ends the walk only
        // when nothing is left over.
        let first = list(&router, prefix).await;
        let page = harnesses(&first.body).len();
        let wanted = page_size.unwrap_or(created.len()).min(created.len());
        assert_eq!(
            page, wanted,
            "case {case:?}: the first page holds min(maxResults, stored) = {wanted} summaries, \
             got {}",
            first.body
        );
        assert_eq!(
            at(&first.body, "/nextToken").is_null(),
            page >= created.len(),
            "case {case:?}: a nextToken is handed back exactly while harnesses are left over, \
             got {}",
            first.body
        );
    }
}

/// A listed summary reports the status the clock owes its harness, and the listing reports
/// the same status `GetHarness` does, because both read one record — and the listing writes
/// nothing: a second call reports exactly what the first did.
#[tokio::test]
async fn a_listing_reports_the_stored_status_and_changes_nothing() {
    let router = router();
    let (id, _) = create(&router, "listClock", json!({})).await;

    let fresh = list(&router, "").await;
    let fresh_status = at(&fresh.body, "/harnesses/0/status");
    assert_eq!(
        fresh_status,
        at(&get(&router, &id).await.body, "/harness/status"),
        "case \"inside the settle span\": the listing and the read report one stored harness's \
         status, got {}",
        fresh.body
    );

    // The transition is owed at an instant, never slept on by a handler, so the row looks
    // again until the condition holds.
    await_ready(&router, &id).await;

    let settled = list(&router, "").await;
    assert_eq!(
        at(&settled.body, "/harnesses/0/status"),
        json!("READY"),
        "case \"past the settle span\": the CREATING -> READY transition is due, got {}",
        settled.body
    );
    let again = list(&router, "").await;
    assert_eq!(
        settled.body, again.body,
        "case \"a second listing\": a read changes nothing a later caller sees, got {} then {}",
        settled.body, again.body
    );
    assert_eq!(
        at(&again.body, "/harnesses/0/status"),
        at(&get(&router, &id).await.body, "/harness/status"),
        "case \"the listing and the read\": both report the status of one stored harness, got {}",
        again.body
    );
}

/// A harness the account no longer holds is listed no more, and deleting one leaves every
/// other harness on the page.
#[tokio::test]
async fn a_deleted_harness_leaves_the_listing() {
    let router = router();
    let mut remaining: Vec<String> = Vec::new();
    for index in 0..4 {
        let (id, _) = create(&router, &format!("listDelete{index}"), json!({})).await;
        remaining.push(id);
    }
    remaining.sort();

    while let Some(gone) = remaining.pop() {
        let removed = send(&router, "DELETE", &format!("/harnesses/{gone}"), None).await;
        assert_eq!(
            removed.status, 200,
            "the delete must succeed for this row to say anything, got {} with {}",
            removed.status, removed.body
        );
        let answer = list(&router, "").await;
        let ids = listed_ids(&answer.body);
        assert!(
            !ids.contains(&gone),
            "case \"after deleting {gone}\": a harness the account no longer holds is not \
             listed, got {}",
            answer.body
        );
        let mut expected = remaining.clone();
        expected.sort();
        assert_eq!(
            ids, expected,
            "case \"after deleting {gone}\": every other harness is still listed, expected \
             {expected:?}, got {ids:?}"
        );
    }
}

/// What the listing refuses, and what it serves instead of refusing.
///
/// The rows span the shape of the offending input: a `maxResults` one past each modelled
/// bound and a `nextToken` outside the codec this service issues — while a token the
/// service itself issued that now resumes past every stored harness is an empty final page,
/// not an error.
#[tokio::test]
async fn a_harness_listing_refuses_what_it_cannot_serve() {
    let router = router();
    // `-` (0x2D) sorts before `Z`, so the first id sorts before the second whatever suffix
    // each one is minted with: the first page of a size-1 walk is this harness's.
    let (stored, _) = create(&router, "listRefusals", json!({})).await;
    let (last, _) = create(&router, "listRefusalsZ", json!({})).await;

    // (case, the query, the status owed, the errortype owed)
    let rows: Vec<(&str, &str, u16, &str)> = vec![
        (
            "maxResults one below the modelled range 1..100",
            "?maxResults=0",
            400,
            "ValidationException",
        ),
        (
            "maxResults one past the modelled range 1..100",
            "?maxResults=101",
            400,
            "ValidationException",
        ),
        (
            "a nextToken this service never issued",
            "?nextToken=not-a-token-this-service-issued",
            400,
            "ValidationException",
        ),
    ];

    for (case, query, status, error_type) in rows {
        let answer = list(&router, query).await;
        assert_eq!(
            (answer.status, answer.error_type.as_str()),
            (status, error_type),
            "case {case:?}: expected HTTP {status} {error_type:?}, got {} {:?} with {}",
            answer.status,
            answer.error_type,
            answer.body
        );
    }

    // The refusals are about the input each row changed, not about the store: the harness
    // is still listed, and still readable under the id the listing reports.
    let served = list(&router, "?maxResults=1").await;
    assert_eq!(
        (served.status, listed_ids(&served.body)),
        (200, vec![stored.clone()]),
        "case \"the bounds themselves\": maxResults=1 is inside the modelled range, got {} with \
         {}",
        served.status,
        served.body
    );

    // A token the SERVICE issued — never one this test encodes — that resumes after the
    // only harness left: an empty final page, which ends the walk rather than refusing it.
    let token = at(&served.body, "/nextToken")
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "a page that left a harness over hands back a nextToken, got {}",
                served.body
            )
        })
        .to_string();
    let removed = send(&router, "DELETE", &format!("/harnesses/{last}"), None).await;
    assert_eq!(
        removed.status, 200,
        "the delete must succeed for the resumed-past-the-end row to say anything, got {} with \
         {}",
        removed.status, removed.body
    );
    let resumed = list(&router, &format!("?nextToken={token}")).await;
    assert_eq!(
        resumed.status, 200,
        "case \"a nextToken that resumes past every stored harness\": a token this service \
         issued is served, got {} {:?} with {}",
        resumed.status,
        resumed.error_type,
        resumed.body
    );
    assert!(
        harnesses(&resumed.body).is_empty() && at(&resumed.body, "/nextToken").is_null(),
        "case \"a nextToken that resumes past every stored harness\": the page is empty and \
         ends the walk, got {}",
        resumed.body
    );
    let read = get(&router, &stored).await;
    assert_eq!(
        at(&read.body, "/harness/harnessId"),
        json!(stored),
        "case \"the listed id round-trips\": the id a page reports addresses the stored \
         harness, got {}",
        read.body
    );
}
