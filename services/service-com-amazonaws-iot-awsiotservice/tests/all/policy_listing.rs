// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of `ListPolicies`: the paginated reader over this account's
//! IoT policy registry.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! `GET /policies` request signed for the `iot` signing name — so what is asserted is what
//! an AWS SDK client would see, not an internal function's return value.
//!
//! The rule under test: `ListPolicies` reports ONE PAGE of the policies this account holds
//! — "Lists your policies" (`smithy_query(query="operation",operation="ListPolicies")`) —
//! each entry built from the record `CreatePolicy` persisted, so the `policyName` and
//! `policyArn` it reports are the ones `GetPolicy` reports and a policy `DeletePolicy`
//! removed is reported by nobody. `ascendingOrder` is "Specifies the order for results. If
//! true, the results are returned in ascending creation order", so the two directions are
//! one listing read either way round; `pageSize` bounds the page and `nextMarker` is
//! returned only while policies remain and resumes the SAME listing exactly once; a
//! `marker` this listing never issued is refused the modelled `InvalidRequestException`
//! (400).
//!
//! The tables span what the rule generalises over rather than one recorded request: an
//! account holding no policy, one, and several; both directions; every page size from one
//! to past the number of policies; policy names of the forms and lengths `PolicyName`
//! admits; and a registry a delete has removed a policy from.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::describe_endpoint::percent_encode;

/// The region the harness signs for.
const REGION: &str = "us-west-2";

/// The account id every emulated ARN carries (`engine::mock_account_id`).
const ACCOUNT_ID: &str = "000000000000";

/// An IoT policy document every policy in this suite is created with: which document a
/// policy carries is no part of what this listing reports, so one valid document serves
/// every create.
const POLICY_DOCUMENT: &str =
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"iot:Connect","Resource":"*"}]}"#;

/// The names the multi-policy cases create: the forms and lengths the modelled
/// `PolicyName` admits (`length 1..128, pattern ^[\w+=,.@-]+$`), including the shortest
/// and the longest, so the listing is exercised over names and not over one spelling.
fn policy_names() -> Vec<String> {
    vec![
        "device-policy".to_string(),
        "a".to_string(),
        "Second_Policy-1".to_string(),
        "Ops+Policy=1,2.x@host-9".to_string(),
        "n".repeat(128),
    ]
}

/// A SigV4 `Authorization` header whose credential scope names the `iot` signing name and
/// the region. The signature itself is never checked here — the harness turns request
/// authentication off, exactly as an unauthenticated emulator runs.
fn signed_for_iot() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTIOT/20260101/{REGION}/iot/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: Option<String>,
    body: Value,
}

impl Reply {
    /// A string member of the body, or `None` when it is absent or not a string.
    fn text(&self, name: &str) -> Option<String> {
        self.body
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// The `policies` the page reports, as the list it is — an absent member is NOT the
    /// same as an empty one, so this reports `None` for an absent member.
    fn policies(&self) -> Option<Vec<Value>> {
        self.body
            .get("policies")
            .and_then(Value::as_array)
            .cloned()
    }

    /// The `policyName` of each entry, in the order the page reports them.
    fn listed_names(&self) -> Vec<String> {
        self.policies()
            .unwrap_or_default()
            .iter()
            .filter_map(|entry| entry.get("policyName"))
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect()
    }

    /// The `policyArn` the page reports for `policy_name`, or `None` when the page does
    /// not report that policy.
    fn listed_arn(&self, policy_name: &str) -> Option<String> {
        self.policies()?
            .iter()
            .find(|entry| entry.get("policyName").and_then(Value::as_str) == Some(policy_name))
            .and_then(|entry| entry.get("policyArn"))
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// `nextMarker`, "The marker for the next set of results, or null if there are no
    /// additional results."
    fn next_marker(&self) -> Option<String> {
        self.text("nextMarker")
    }
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it, reached
/// the way the production router reaches it.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    async fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://iot.{REGION}.amazonaws.com{path}"))
            .header("authorization", signed_for_iot())
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply {
            status,
            error_code,
            body,
        }
    }

    /// `CreatePolicy`: `POST /policies/{policyName}`.
    async fn create_policy(&self, policy_name: &str) -> Reply {
        self.call(
            "POST",
            &format!("/policies/{policy_name}"),
            Some(&json!({ "policyDocument": POLICY_DOCUMENT })),
        )
        .await
    }

    /// `GetPolicy`: `GET /policies/{policyName}` — the reader of ONE policy, so what the
    /// listing reports is checked against the record itself.
    async fn get_policy(&self, policy_name: &str) -> Reply {
        self.call("GET", &format!("/policies/{policy_name}"), None)
            .await
    }

    /// `DeletePolicy`: `DELETE /policies/{policyName}`.
    async fn delete_policy(&self, policy_name: &str) -> Reply {
        self.call("DELETE", &format!("/policies/{policy_name}"), None)
            .await
    }

    /// `ListPolicies`: `GET /policies`, whose members all travel in the query string.
    async fn list_policies(&self, query: &str) -> Reply {
        let path = if query.is_empty() {
            "/policies".to_string()
        } else {
            format!("/policies?{query}")
        };
        self.call("GET", &path, None).await
    }

    /// Create each of `names`, asserting each create is taken.
    async fn create_each(&self, case: &str, names: &[String]) {
        for name in names {
            let created = self.create_policy(name).await;
            assert_eq!(
                created.status, 200,
                "case {case:?}: CreatePolicy {name:?} must succeed, got {created:?}"
            );
        }
    }

    /// Every policy the listing reports, walked page by page through its own
    /// `nextMarker` at `page_size` per page.
    async fn walk(&self, case: &str, ascending_order: bool, page_size: usize) -> Vec<String> {
        let mut walked: Vec<String> = Vec::new();
        let mut marker: Option<String> = None;
        // One page per policy at worst, plus the page that reports the end of the listing:
        // a listing that asks for more pages than that is not converging.
        let mut pages_left = policy_names().len() + 2;
        loop {
            // `ascendingOrder` is bound to the query parameter `isAscendingOrder`
            // (`smithy_query(query="operation",operation="ListPolicies")`), which is the
            // name a client sends it under.
            let mut query = format!("isAscendingOrder={ascending_order}&pageSize={page_size}");
            if let Some(marker) = &marker {
                // The modelled `Marker` pattern admits `+`, `/` and `=`, and `+` would
                // decode to a space in a query string, so the token is percent-encoded
                // rather than interpolated raw.
                query.push_str(&format!("&marker={}", percent_encode(marker)));
            }
            let page = self.list_policies(&query).await;
            assert_eq!(
                page.status, 200,
                "case {case:?}: ListPolicies must succeed, got {page:?}"
            );
            let names = page.listed_names();
            assert!(
                names.len() <= page_size,
                "case {case:?}: a page must not exceed pageSize {page_size}, got {names:?}"
            );
            walked.extend(names);
            marker = page.next_marker();
            if marker.is_none() {
                return walked;
            }
            pages_left -= 1;
            assert!(
                pages_left > 0,
                "case {case:?}: the walk must end, got {walked:?} so far"
            );
        }
    }
}

/// RULE: the listing reports the registry — every policy the account holds, each entry
/// built from that policy's own record, and nothing else.
#[tokio::test]
async fn the_listing_reports_every_policy_the_account_holds() {
    for count in 0..=policy_names().len() {
        let case = format!("{count} policies");
        let emulator = Emulator::new();
        let created = &policy_names()[..count];
        emulator.create_each(&case, created).await;

        let listed = emulator.list_policies("").await;
        assert_eq!(
            listed.status, 200,
            "case {case:?}: ListPolicies must succeed, got {listed:?}"
        );
        // "you hold none" is an answer: the member is reported, as the empty list.
        assert!(
            listed.policies().is_some(),
            "case {case:?}: `policies` must be reported even when the account holds none, \
             got {listed:?}"
        );
        let names: BTreeSet<String> = listed.listed_names().into_iter().collect();
        let expected: BTreeSet<String> = created.iter().cloned().collect();
        assert_eq!(
            names, expected,
            "case {case:?}: expected {expected:?}, got {:?}",
            listed.listed_names()
        );
        assert_eq!(
            listed.listed_names().len(),
            count,
            "case {case:?}: each policy must be reported exactly once, got {:?}",
            listed.listed_names()
        );
        // Nothing remains to page through, so no marker is issued.
        assert_eq!(
            listed.next_marker(),
            None,
            "case {case:?}: a complete listing must report no nextMarker, got {listed:?}"
        );

        // Each entry is the record itself: the ARN the listing reports is the one the
        // reader of that one policy reports, and the one the create minted.
        for name in created {
            let read = emulator.get_policy(name).await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: GetPolicy {name:?} must succeed, got {read:?}"
            );
            assert_eq!(
                listed.listed_arn(name),
                read.text("policyArn"),
                "case {case:?}: the listed ARN of {name:?} must be the one GetPolicy reports, \
                 got {listed:?}"
            );
            assert_eq!(
                listed.listed_arn(name),
                Some(format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:policy/{name}")),
                "case {case:?}: unexpected ARN for {name:?}, got {listed:?}"
            );
        }
    }
}

/// RULE: the two directions `ascendingOrder` names are ONE listing read either way round,
/// so each is the exact reverse of the other.
#[tokio::test]
async fn the_two_directions_are_one_listing_read_either_way_round() {
    let emulator = Emulator::new();
    let names = policy_names();
    emulator.create_each("both directions", &names).await;

    let ascending = emulator.list_policies("isAscendingOrder=true").await;
    let descending = emulator.list_policies("isAscendingOrder=false").await;
    assert_eq!(
        ascending.status, 200,
        "the ascending listing must succeed, got {ascending:?}"
    );
    assert_eq!(
        descending.status, 200,
        "the descending listing must succeed, got {descending:?}"
    );

    let mut reversed = descending.listed_names();
    reversed.reverse();
    assert_eq!(
        ascending.listed_names(),
        reversed,
        "the descending listing must be the ascending one reversed, got {:?} against {:?}",
        ascending.listed_names(),
        descending.listed_names()
    );
    assert_eq!(
        ascending.listed_names().len(),
        names.len(),
        "both directions must report every policy, got {:?}",
        ascending.listed_names()
    );
}

/// RULE: `pageSize` bounds the page and `nextMarker` resumes the SAME listing exactly
/// once, whatever size the pages are cut at.
#[tokio::test]
async fn every_page_size_walks_the_listing_once_through_its_marker() {
    let emulator = Emulator::new();
    let names = policy_names();
    emulator.create_each("paging", &names).await;

    for ascending_order in [true, false] {
        // Exhaustive over the page size rather than sampled: every size from the modelled
        // minimum (`PageSize` is `range 1..250`) to one past the number of policies, plus
        // the modelled maximum.
        let mut page_sizes: Vec<usize> = (1..=names.len() + 1).collect();
        page_sizes.push(250);

        let whole = emulator
            .list_policies(&format!("isAscendingOrder={ascending_order}"))
            .await;
        let expected = whole.listed_names();
        assert_eq!(
            expected.len(),
            names.len(),
            "case ascending={ascending_order}: the unpaged listing must report every policy, \
             got {expected:?}"
        );

        for page_size in page_sizes {
            let case = format!("ascending={ascending_order}, pageSize={page_size}");
            let walked = emulator.walk(&case, ascending_order, page_size).await;
            assert_eq!(
                walked, expected,
                "case {case:?}: the walk must visit the listing once in its own order, \
                 expected {expected:?}, got {walked:?}"
            );
        }
    }
}

/// RULE: a `marker` this listing never issued is refused the modelled
/// `InvalidRequestException` (400) rather than resuming some other listing from the
/// middle.
#[tokio::test]
async fn a_marker_this_listing_never_issued_is_refused() {
    let emulator = Emulator::new();
    let names = policy_names();
    emulator.create_each("a foreign marker", &names).await;

    // The marker the OTHER direction issued is one of the rows: it names a position in a
    // differently ordered row set, so this direction cannot resume from it.
    let other_direction = emulator
        .list_policies("isAscendingOrder=false&pageSize=1")
        .await;
    let foreign = other_direction.next_marker().unwrap_or_else(|| {
        panic!("a truncated listing must issue a marker, got {other_direction:?}")
    });

    // `Marker` is modelled `length 0..1024, pattern ^[A-Za-z0-9+/]+={0,2}$`
    // (`smithy_query(query="operation",operation="ListPolicies")`), so every value below
    // satisfies the member's own constraints and is refused by the handler.
    for marker in [foreign.as_str(), "AAAA", "ZmFicmljYXRlZA=="] {
        let refused = emulator
            .list_policies(&format!(
                "isAscendingOrder=true&marker={}",
                percent_encode(marker)
            ))
            .await;
        assert_eq!(
            refused.status, 400,
            "case {marker:?}: a foreign marker must be refused 400, got {refused:?}"
        );
        assert_eq!(
            refused
                .error_code
                .as_deref()
                .map(|code| code.split(':').next().unwrap_or(code)),
            Some("InvalidRequestException"),
            "case {marker:?}: expected InvalidRequestException, got {refused:?}"
        );
    }

    // The marker this direction DID issue resumes it, so the refusals above are about the
    // token and not about the member being supplied at all.
    let first = emulator
        .list_policies("isAscendingOrder=true&pageSize=1")
        .await;
    let marker = first
        .next_marker()
        .unwrap_or_else(|| panic!("a truncated listing must issue a marker, got {first:?}"));
    let resumed = emulator
        .list_policies(&format!(
            "isAscendingOrder=true&pageSize=1&marker={}",
            percent_encode(&marker)
        ))
        .await;
    assert_eq!(
        resumed.status, 200,
        "the marker this listing issued must resume it, got {resumed:?}"
    );
    assert_eq!(
        resumed.listed_names().len(),
        1,
        "the resumed page must report the next policy, got {resumed:?}"
    );
    assert_ne!(
        resumed.listed_names(),
        first.listed_names(),
        "the resumed page must come AFTER the first one, got {resumed:?}"
    );
}

/// RULE: the listing reports the registry as it stands, so a policy a delete removed is
/// reported by nobody and one created again is reported again.
#[tokio::test]
async fn a_deleted_policy_leaves_the_listing_and_a_recreated_one_returns_to_it() {
    let emulator = Emulator::new();
    let names = policy_names();
    emulator.create_each("deletes", &names).await;

    let mut remaining: BTreeSet<String> = names.iter().cloned().collect();
    for name in &names {
        let deleted = emulator.delete_policy(name).await;
        assert_eq!(
            deleted.status, 200,
            "case {name:?}: DeletePolicy must succeed, got {deleted:?}"
        );
        remaining.remove(name);

        let listed = emulator.list_policies("").await;
        let reported: BTreeSet<String> = listed.listed_names().into_iter().collect();
        assert_eq!(
            reported, remaining,
            "case {name:?}: expected {remaining:?} after the delete, got {reported:?}"
        );
    }

    // The name is free again, and the policy created under it is in the listing once.
    let recreated = &names[0];
    let created = emulator.create_policy(recreated).await;
    assert_eq!(
        created.status, 200,
        "the name must be free for a later create, got {created:?}"
    );
    let listed = emulator.list_policies("").await;
    assert_eq!(
        listed.listed_names(),
        vec![recreated.clone()],
        "the re-created policy must be the only one listed, got {listed:?}"
    );
}
