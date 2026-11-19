// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for `ListMemories`.
//!
//! Every test drives the service the way the router does: an `http::Request` through
//! [`MultiServiceRouter::dispatch`], so the wire protocol (`restJson1`), the boundary
//! constraint validation and the handlers are all exercised. The store is the one shared
//! engine, so what `CreateMemory` writes and `DeleteMemory` retires is what the listing
//! reads.
//!
//! The rule under test: a listing of a resource family reports the records the service
//! holds for it — every memory a create minted, each summarised off the same record a read
//! of it answers from, and none whose deletion the clock has already made due — in one
//! ascending `memoryId` order, one page of at most the caller's `maxResults` (the member's
//! modelled default when the caller names none, capped at the top of its modelled range
//! 1..100, from `smithy_query(query="operation", operation="ListMemories")`), resumed
//! strictly after the entry the `nextToken` it handed back was issued for, so the pages of
//! any size concatenate to the whole listing with every memory reported exactly once. The
//! modelled range decides which `maxResults` values are accepted, so one outside it — and a
//! `nextToken` this service never issued — is the modelled HTTP 400 `ValidationException`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The region every request in this suite signs for.
const REGION: &str = "eu-central-1";

/// The modelled bounds of `maxResults` (range 1..100, default 100, from
/// `smithy_query(query="operation", operation="ListMemories")`): the values a listing
/// accepts, the most one page holds, and the page a caller naming none is served.
const MAX_RESULTS_MIN: i64 = 1;
const MAX_RESULTS_MAX: i64 = 100;
const MODELLED_DEFAULT_PAGE: usize = 100;

/// `MemoryStatus`' full value list (`smithy_query(query="shape", shape="MemoryStatus")`),
/// which a summary's `status` is one of.
const MEMORY_STATUS_VALUES: &[&str] = &["ACTIVE", "CREATING", "DELETING", "FAILED", "UPDATING"];

/// How long a test waits for a transition the clock owes a memory (`CREATING` -> `ACTIVE`,
/// the removal of a deleted one). Generous: the condition is polled, never timed, so a
/// loaded host only takes more turns of the loop.
const SETTLE_DEADLINE: Duration = Duration::from_secs(30);

/// How long one turn of a poll loop waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::default();
    router.register_service(EmulatedService::new(engine));
    router
}

/// A SigV4 `Authorization` header for this service, which is what the router routes on.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20240101/{REGION}/bedrock-agentcore/aws4_request, \
         SignedHeaders=host, Signature=abc"
    )
}

async fn send(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map_or_else(Bytes::new, |value| {
        Bytes::from(serde_json::to_vec(value).expect("the request body serializes"))
    });
    let mut request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", authorization())
        .header("host", "bedrock-agentcore-control.amazonaws.com");
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let request = request.body(payload).expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// `CreateMemory`: `POST /memories/create`, answering HTTP 202 with the `memoryId` it
/// minted.
async fn create_memory(router: &MultiServiceRouter, name: &str) -> String {
    let (status, created) = send(
        router,
        "POST",
        "/memories/create",
        Some(&json!({"name": name, "eventExpiryDuration": 30})),
    )
    .await;
    assert_eq!(
        status, 202,
        "case {name:?}: expected HTTP 202 from CreateMemory, got {status} ({created})"
    );
    created["memory"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {name:?}: the create reported no id ({created})"))
        .to_string()
}

/// `DeleteMemory`: `DELETE /memories/{memoryId}/delete`.
async fn delete_memory(router: &MultiServiceRouter, memory_id: &str) -> (u16, Value) {
    send(
        router,
        "DELETE",
        &format!("/memories/{memory_id}/delete"),
        None,
    )
    .await
}

/// `GetMemory`: `GET /memories/{memoryId}/details`.
async fn get_memory(router: &MultiServiceRouter, memory_id: &str) -> (u16, Value) {
    send(
        router,
        "GET",
        &format!("/memories/{memory_id}/details"),
        None,
    )
    .await
}

/// `ListMemories`: `POST /memories/`, with `maxResults` and `nextToken` in the payload
/// (`smithy_query(query="operation", operation="ListMemories")`).
async fn list_memories(
    router: &MultiServiceRouter,
    max_results: Option<i64>,
    next_token: Option<&str>,
) -> (u16, Value) {
    let mut body = json!({});
    if let Some(max) = max_results {
        body["maxResults"] = json!(max);
    }
    if let Some(token) = next_token {
        body["nextToken"] = json!(token);
    }
    send(router, "POST", "/memories/", Some(&body)).await
}

/// The error code `restJson1` puts in `__type` / `x-amzn-errortype`.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The summaries a listing reported, in the order it reported them.
fn summaries(body: &Value) -> Vec<Value> {
    body["memories"].as_array().cloned().unwrap_or_default()
}

/// The `memoryId`s a listing reported, in the order it reported them.
fn listed_ids(body: &Value) -> Vec<String> {
    summaries(body)
        .iter()
        .map(|summary| summary["id"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The summary a listing reported for `memory_id`, if any page of it holds one.
async fn summary_of(router: &MultiServiceRouter, case: &str, memory_id: &str) -> Option<Value> {
    let mut token: Option<String> = None;
    loop {
        let (status, page) = list_memories(router, Some(MAX_RESULTS_MAX), token.as_deref()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from ListMemories, got {status} ({page})"
        );
        if let Some(found) = summaries(&page)
            .into_iter()
            .find(|summary| summary["id"].as_str() == Some(memory_id))
        {
            return Some(found);
        }
        match page["nextToken"].as_str() {
            Some(next) => token = Some(next.to_string()),
            None => return None,
        }
    }
}

/// Create `count` memories under `prefix` and hand back the `memoryId`s the account then
/// holds, in the ascending order a listing reports them.
///
/// `Name` is `^[a-zA-Z][a-zA-Z0-9_]{0,47}$`, so the names built here are inside it.
async fn seed(router: &MultiServiceRouter, prefix: &str, count: usize) -> Vec<String> {
    let mut held = Vec::new();
    for index in 1..=count {
        held.push(create_memory(router, &format!("{prefix}{index:03}")).await);
    }
    held.sort();
    held
}

/// Wait until every memory the account holds is reported `ACTIVE` — the transition the
/// clock owes a create — polling the listing rather than timing the span.
async fn await_all_active(router: &MultiServiceRouter, case: &str, held: &[String]) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, page) = list_memories(router, Some(MAX_RESULTS_MAX), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from ListMemories, got {status} ({page})"
        );
        let pending: Vec<Value> = summaries(&page)
            .into_iter()
            .filter(|summary| summary["status"].as_str() != Some("ACTIVE"))
            .collect();
        if pending.is_empty() && listed_ids(&page).len() >= held.len().min(MODELLED_DEFAULT_PAGE) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: every memory the account holds reaches ACTIVE, still pending \
             {pending:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Walk the listing from its first page to its last, following the `nextToken` each page
/// hands back, and report the ids in the order the pages reported them together with how
/// many pages the walk took. `held` is how many memories the account holds, which bounds
/// how many pages even the smallest page size can take.
async fn walk(
    router: &MultiServiceRouter,
    case: &str,
    max_results: Option<i64>,
    held: usize,
) -> (Vec<String>, usize) {
    let mut walked: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    let mut pages = 0_usize;
    loop {
        let (status, page) = list_memories(router, max_results, token.as_deref()).await;
        assert_eq!(
            status, 200,
            "case {case:?} (page {pages}): expected HTTP 200 from ListMemories, got {status} \
             ({page})"
        );
        let ids = listed_ids(&page);
        let allowed = max_results
            .map_or(MODELLED_DEFAULT_PAGE, |max| {
                usize::try_from(max).unwrap_or(MODELLED_DEFAULT_PAGE)
            })
            .min(MODELLED_DEFAULT_PAGE);
        assert!(
            ids.len() <= allowed,
            "case {case:?} (page {pages}): a page holds at most {allowed} summaries, got {} \
             ({page})",
            ids.len()
        );
        walked.extend(ids);
        pages += 1;
        // The smallest page the model admits is one entry, so even that walk cannot take
        // more pages than the account holds memories, plus the empty page a cursor sitting
        // exactly on the last of them would answer.
        assert!(
            pages <= held + 2,
            "case {case:?}: the walk must terminate, got {pages} pages and {walked:?}"
        );
        match page["nextToken"].as_str() {
            Some(next) => token = Some(next.to_string()),
            None => break,
        }
    }
    (walked, pages)
}

/// The listing reports exactly the memories the account holds: nothing before any create,
/// every created memory afterwards, and — once the removal the clock owes a delete comes
/// due — nothing for the deleted one, whose summary reports `DELETING` for as long as it
/// is still held.
///
/// The rows differ along how many memories the account holds when the listing runs, which
/// is what "reports what the service holds" generalises over: none, one, and several.
#[tokio::test]
async fn a_listing_reports_what_the_account_holds() {
    let router = router();

    let (status, empty) = list_memories(&router, None, None).await;
    assert_eq!(
        (status, listed_ids(&empty), empty["nextToken"].clone()),
        (200, Vec::<String>::new(), Value::Null),
        "case \"an account holding no memory\": expected the required memories present and \
         empty with no token, got {empty}"
    );

    // (case, how many memories the account holds by then)
    let mut held: Vec<String> = Vec::new();
    for (case, count) in [("one memory", 1_usize), ("several memories", 4)] {
        while held.len() < count {
            let index = held.len() + 1;
            held.push(create_memory(&router, &format!("listedMemory{index:03}")).await);
        }
        let mut expected = held.clone();
        expected.sort();

        let (status, listed) = list_memories(&router, None, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from ListMemories, got {status} ({listed})"
        );
        assert_eq!(
            listed_ids(&listed),
            expected,
            "case {case:?}: expected every held memory in ascending memoryId order, got {listed}"
        );
        for summary in summaries(&listed) {
            let status_value = summary["status"].as_str().unwrap_or_default().to_string();
            assert!(
                MEMORY_STATUS_VALUES.contains(&status_value.as_str()),
                "case {case:?}: a summary's status is one of {MEMORY_STATUS_VALUES:?}, got \
                 {status_value:?} ({summary})"
            );
        }
    }

    // A delete retires the memory from the listing: while the removal the clock owes it is
    // still outstanding the summary reports `DELETING`, and once it comes due the listing
    // reports the memory no more — while every other memory stays listed.
    let doomed = held[0].clone();
    let (status, accepted) = delete_memory(&router, &doomed).await;
    assert_eq!(
        status, 202,
        "case \"a deleted memory\": expected HTTP 202 from DeleteMemory, got {status} \
         ({accepted})"
    );
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        match summary_of(&router, "a deleted memory", &doomed).await {
            None => break,
            Some(summary) => {
                assert_eq!(
                    summary["status"],
                    json!("DELETING"),
                    "case \"a deleted memory\": a memory whose deletion was accepted is \
                     reported DELETING for as long as it is still held, got {summary}"
                );
                assert!(
                    Instant::now() < deadline,
                    "case \"a deleted memory\": the removal the clock owes it comes due, still \
                     reported as {summary}"
                );
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        }
    }
    let mut survivors = held[1..].to_vec();
    survivors.sort();
    let (status, after) = list_memories(&router, None, None).await;
    assert_eq!(
        (status, listed_ids(&after)),
        (200, survivors.clone()),
        "case \"the memories a delete left\": expected {survivors:?}, got {after}"
    );
}

/// A summary is the same record a read of the memory answers from, projected onto
/// `MemorySummary`: the id, the ARN, the status and both instants a `GetMemory` reports.
#[tokio::test]
async fn a_summary_reports_the_same_record_a_read_does() {
    let router = router();
    let held = seed(&router, "readBackMemory", 3).await;
    await_all_active(&router, "the seeded account", &held).await;

    for memory_id in &held {
        let summary = summary_of(&router, memory_id, memory_id)
            .await
            .unwrap_or_else(|| panic!("case {memory_id:?}: the listing reported no summary"));
        let (status, read) = get_memory(&router, memory_id).await;
        assert_eq!(
            status, 200,
            "case {memory_id:?}: expected HTTP 200 from GetMemory, got {status} ({read})"
        );
        let memory = &read["memory"];
        for member in ["id", "arn", "status", "createdAt", "updatedAt"] {
            assert_eq!(
                summary[member], memory[member],
                "case {memory_id:?}: the summary's {member} is the read's, got {} and {}",
                summary[member], memory[member]
            );
        }
        assert!(
            summary["managedByResourceArn"].is_null(),
            "case {memory_id:?}: no operation of this service creates a managed memory, got {}",
            summary["managedByResourceArn"]
        );
    }
}

/// For every page size the modelled range admits, the pages concatenate to the whole
/// listing: each memory once, in ascending `memoryId` order, with no page holding more than
/// `maxResults` summaries.
///
/// The rows differ along the page size, which is what paging generalises over: one entry
/// per page, sizes that leave a part-full last page, the size that ends exactly on the last
/// memory, one past the number held, each modelled bound, and no `maxResults` at all.
#[tokio::test]
async fn pages_of_every_size_concatenate_to_the_whole_listing() {
    let router = router();
    let held = seed(&router, "pagedMemory", 7).await;
    let count = i64::try_from(held.len()).unwrap_or(i64::MAX);

    // (case, maxResults)
    let rows: [(&str, Option<i64>); 8] = [
        ("the modelled minimum, one memory per page", Some(MAX_RESULTS_MIN)),
        ("two memories per page", Some(2)),
        ("a size leaving a part-full last page", Some(3)),
        ("exactly as many as the account holds", Some(count)),
        ("one more than the account holds", Some(count + 1)),
        ("ten memories per page", Some(10)),
        ("the modelled maximum", Some(MAX_RESULTS_MAX)),
        ("no maxResults at all", None),
    ];

    for (case, max_results) in rows {
        let (walked, pages) = walk(&router, case, max_results, held.len()).await;
        assert_eq!(
            walked, held,
            "case {case:?}: the pages must concatenate to the whole listing {held:?}, got \
             {walked:?}"
        );
        assert!(
            pages >= 1,
            "case {case:?}: a listing is served in at least one page, got {pages}"
        );
    }
}

/// No page holds more than the top of `maxResults`' modelled range, and a caller naming no
/// `maxResults` is served that same modelled default — with the memories left over reachable
/// through the token the page hands back.
#[tokio::test]
async fn no_page_holds_more_than_the_modelled_maximum() {
    let router = router();
    // One more memory than the largest page the model admits, so a full page has something
    // left over to hand a token back for.
    let held = seed(&router, "cappedMemory", MODELLED_DEFAULT_PAGE + 1).await;

    for (case, max_results) in [
        ("the modelled maximum", Some(MAX_RESULTS_MAX)),
        ("no maxResults at all", None),
    ] {
        let (status, page) = list_memories(&router, max_results, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from ListMemories, got {status} ({page})"
        );
        assert_eq!(
            listed_ids(&page),
            held[..MODELLED_DEFAULT_PAGE].to_vec(),
            "case {case:?}: expected the first {MODELLED_DEFAULT_PAGE} memories of the listing, \
             got {} summaries",
            listed_ids(&page).len()
        );
        let token = page["nextToken"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: one memory is left over past a page of \
                     {MODELLED_DEFAULT_PAGE}, so the page hands back a token, got {}",
                    page["nextToken"]
                )
            })
            .to_string();
        let (status, rest) = list_memories(&router, max_results, Some(&token)).await;
        assert_eq!(
            (status, listed_ids(&rest), rest["nextToken"].clone()),
            (200, held[MODELLED_DEFAULT_PAGE..].to_vec(), Value::Null),
            "case {case:?}: expected the memory past the first page and no further token, got \
             {rest}"
        );
    }
}

/// `maxResults` outside its modelled range (1..100) is refused with the modelled HTTP 400
/// `ValidationException`, and each bound of that range is served.
///
/// The rows differ along the page size asked for: each modelled bound and one step outside
/// it, plus a value far outside the range.
#[tokio::test]
async fn max_results_outside_the_modelled_range_is_refused() {
    let router = router();
    let held = seed(&router, "boundedMemory", 3).await;

    // (case, maxResults, expected status, how many summaries a served page holds)
    let rows: [(&str, i64, u16, usize); 5] = [
        ("one below the modelled minimum", MAX_RESULTS_MIN - 1, 400, 0),
        (
            "the modelled minimum",
            MAX_RESULTS_MIN,
            200,
            usize::try_from(MAX_RESULTS_MIN).unwrap_or(1),
        ),
        ("the modelled maximum", MAX_RESULTS_MAX, 200, held.len()),
        ("one above the modelled maximum", MAX_RESULTS_MAX + 1, 400, 0),
        ("far above the modelled maximum", 10_000, 400, 0),
    ];

    for (case, max_results, expected_status, expected_len) in rows {
        let (status, answered) = list_memories(&router, Some(max_results), None).await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected HTTP {expected_status} for maxResults={max_results}, got \
             {status} ({answered})"
        );
        if expected_status == 200 {
            assert_eq!(
                listed_ids(&answered),
                held[..expected_len].to_vec(),
                "case {case:?}: expected the first {expected_len} memories held, got {answered}"
            );
        } else {
            assert_eq!(
                error_code(&answered),
                "ValidationException",
                "case {case:?}: expected the modelled ValidationException, got {answered}"
            );
        }
    }
}

/// A `nextToken` this service did not issue is the modelled HTTP 400 naming that member,
/// while one it could have issued is served — walking off the end of the store is an empty
/// page, not a refusal — and the token a page DID hand back resumes strictly after the
/// entry it was issued for.
#[tokio::test]
async fn a_next_token_this_service_did_not_issue_is_refused() {
    let router = router();
    let held = seed(&router, "resumedMemory", 3).await;

    for (case, token) in [
        ("a word outside the token's alphabet", "notatoken"),
        ("a pair of characters outside it", "zz"),
        ("a token truncated to an odd length", "616"),
        ("a memoryId handed over unencoded", held[0].as_str()),
    ] {
        let (status, answered) = list_memories(&router, None, Some(token)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for nextToken={token:?}, got {status} ({answered})"
        );
        assert_eq!(
            error_code(&answered),
            "ValidationException",
            "case {case:?}: expected the modelled ValidationException, got {answered}"
        );
        assert_eq!(
            answered["fieldList"][0]["name"],
            json!("nextToken"),
            "case {case:?}: expected the refusal to name nextToken, got {}",
            answered["fieldList"]
        );
    }

    // `7a7a7a7a` is the token form this service issues (the hex of the store key a page
    // ended on) for the key `zzzz`, which sorts after every memoryId the account holds — so
    // it resumes past the last of them.
    let (status, beyond) = list_memories(&router, None, Some("7a7a7a7a")).await;
    assert_eq!(
        (status, listed_ids(&beyond), beyond["nextToken"].clone()),
        (200, Vec::<String>::new(), Value::Null),
        "case \"a token that resumes past every memory\": expected the required memories \
         present and empty with nothing left over, got {beyond}"
    );

    let (status, first) = list_memories(&router, Some(MAX_RESULTS_MIN), None).await;
    assert_eq!(
        status, 200,
        "case \"the first single-entry page\": expected HTTP 200, got {status} ({first})"
    );
    let token = first["nextToken"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case \"the first single-entry page\": expected a token ({first})")
        })
        .to_string();
    let (status, second) = list_memories(&router, Some(MAX_RESULTS_MIN), Some(&token)).await;
    assert_eq!(
        (status, listed_ids(&second)),
        (200, vec![held[1].clone()]),
        "case \"the page the token resumes\": expected the memory after {}, got {second}",
        held[0]
    );
}
