// SPDX-License-Identifier: CC0-1.0

//! End-to-end behaviour of `ListDataLakeNamespaces`, driven over the wire (restJson1,
//! signing name `scn`) through the same `MultiServiceRouter` the emulator's front door
//! uses — so every assertion here is about what a real client observes, not about a
//! handler's internals.
//!
//! RULE under test: the listing REPORTS the data lake namespaces the store holds for the
//! addressed instance — the ones a create persisted, never the data lake's pre-defined
//! ones and never another instance's — in one stable order, cut into pages of at most
//! `maxResults` with an opaque `nextToken` that resumes where the page stopped; an
//! `instanceId` naming no instance of this account is an authorization failure, a
//! `nextToken` this listing did not issue is a validation failure, and the listing changes
//! nothing a later caller sees.
//!
//! Every namespace a case lists is created through `CreateDataLakeNamespace`, so each
//! assertion is about a record the service itself persisted and `GetDataLakeNamespace`
//! reports.
//!
//! The tables span what the rule generalises over: how many namespaces an instance holds
//! (none, one, more than a page), the form of the names and the order they were created
//! in, the page size (both published bounds of `maxResults`, sizes in between and the
//! modelled default), and how a rejected `nextToken` fails to be one this listing issued.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_supplychain_galaxypublicapigateway::state::{
    namespace_key, service_state, PREDEFINED_NAMESPACES,
};
use service_com_amazonaws_supplychain_galaxypublicapigateway::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region these cases sign for.
const REGION: &str = "us-east-1";

/// The published bounds of `DataLakeNamespaceMaxResults` (`@range(min: 1, max: 20)`).
const MAX_RESULTS_MIN: i64 = 1;
const MAX_RESULTS_MAX: i64 = 20;

/// The `@default` the model gives `maxResults`, applied when a request omits it.
const MAX_RESULTS_DEFAULT: usize = 10;

/// The sentence a paginated read refuses a `nextToken` it did not issue with — fixed, and
/// echoing nothing of the request. Written out HERE rather than read from the crate: the
/// wording is the requirement, so a drift in the constant the handler answers from fails
/// this suite instead of travelling with it.
const UNISSUED_TOKEN_SENTENCE: &str = "nextToken is not a pagination token this service issued";

/// An `instanceId` of the modelled `UUID` form that no `CreateInstance` of a case minted,
/// for the rows about an instance this account holds nothing for.
const UNKNOWN_INSTANCE: &str = "00000000-0000-4000-8000-000000000000";

/// The emulator under test: one engine, one service, driven through the router.
struct Emulator {
    router: MultiServiceRouter,
    /// The SAME engine the service was built from, so a case can read the store back the
    /// way a handler does — and drop a record, which is how a case shows a page token
    /// survives the disappearance of the namespace it names.
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Keep the test independent of how this process was launched: no authenticator,
        // so a request routes on its credential scope alone.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
    }

    /// Send one request and return its HTTP status and JSON body.
    async fn send(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let body_bytes = match &body {
            Some(value) => Bytes::from(serde_json::to_vec(value).expect("serialize request body")),
            None => Bytes::new(),
        };
        // A SigV4 `Authorization` header whose credential scope names this service
        // (`scn`), which is how the router picks the service that claims the request.
        let credential = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/scn/aws4_request, \
             SignedHeaders=host, Signature=00"
        );
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://scn.{REGION}.amazonaws.com{path}"))
            .header("host", format!("scn.{REGION}.amazonaws.com"))
            .header("authorization", credential)
            .header("content-type", "application/json")
            .body(body_bytes)
            .expect("build the request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.into_body();
        let parsed = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                panic!(
                    "{method} {path}: body is not JSON ({error}): {}",
                    String::from_utf8_lossy(&bytes)
                )
            })
        };
        (status, parsed)
    }

    /// A live instance of this account, created through `CreateInstance`.
    async fn given_instance(&self, name: &str) -> String {
        let (status, body) = self
            .send("POST", "/api/instance", Some(json!({ "instanceName": name })))
            .await;
        assert_eq!(
            status, 200,
            "CreateInstance expected 200, got {status} with {body}"
        );
        body.get("instance")
            .and_then(|instance| instance.get("instanceId"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("CreateInstance carried no instanceId: {body}"))
            .to_string()
    }

    /// One namespace of `instance_id`, created through `CreateDataLakeNamespace`.
    async fn given_namespace(&self, instance_id: &str, name: &str, description: Option<&str>) {
        let mut request = json!({});
        if let Some(description) = description {
            request["description"] = json!(description);
        }
        let (status, body) = self
            .send(
                "PUT",
                &format!("/api/datalake/instance/{instance_id}/namespaces/{name}"),
                Some(request),
            )
            .await;
        assert_eq!(
            status, 200,
            "CreateDataLakeNamespace {name:?} expected 200, got {status} with {body}"
        );
    }

    /// `GetDataLakeNamespace` for `name` under `instance_id`.
    async fn get_namespace(&self, instance_id: &str, name: &str) -> (u16, Value) {
        self.send(
            "GET",
            &format!("/api/datalake/instance/{instance_id}/namespaces/{name}"),
            None,
        )
        .await
    }

    /// `ListDataLakeNamespaces` for `instance_id`, with `query` appended verbatim.
    async fn list_namespaces(&self, instance_id: &str, query: &str) -> (u16, Value) {
        let path = format!("/api/datalake/instance/{instance_id}/namespaces");
        let path = if query.is_empty() {
            path
        } else {
            format!("{path}?{query}")
        };
        self.send("GET", &path, None).await
    }

    /// How many namespace records the store holds for `instance_id`.
    fn stored_namespaces(&self, instance_id: &str) -> usize {
        service_state(&*self.engine)
            .expect("the supplychain store is reachable on this engine")
            .lock()
            .namespace_count(instance_id)
    }

    /// Drop one stored namespace of `instance_id`, standing in for the disappearance of a
    /// record between two pages of one listing.
    fn drop_namespace(&self, instance_id: &str, name: &str) {
        let removed = service_state(&*self.engine)
            .expect("the supplychain store is reachable on this engine")
            .lock()
            .namespaces
            .remove(&namespace_key(instance_id, name));
        assert!(
            removed.is_some(),
            "the store held no namespace {name:?} of instance {instance_id} to drop"
        );
    }
}

/// The `namespaces` member of a success body, as a list.
fn namespaces_of(case: &str, body: &Value) -> Vec<Value> {
    body.get("namespaces")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: response carries no `namespaces` list: {body}"))
}

/// The `name` of every entry of a success body's `namespaces`, in the order answered.
fn listed_names(case: &str, body: &Value) -> Vec<String> {
    namespaces_of(case, body)
        .iter()
        .map(|namespace| {
            namespace
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: a listed namespace carries no `name`: {namespace}")
                })
                .to_string()
        })
        .collect()
}

/// The `nextToken` a success body carries, if any.
fn next_token(body: &Value) -> Option<String> {
    body.get("nextToken")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The error code of a refusal envelope (restJson1 renders it in `__type`).
fn error_code(case: &str, body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: refusal carries no `__type`: {body}"))
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The `message` of a refusal envelope.
fn error_message(case: &str, body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: refusal carries no message: {body}"))
        .to_string()
}

/// How the gateway refuses a call whose `instanceId` names no instance of this account.
fn expected_authorization_failure(instance_id: &str) -> String {
    format!(
        "Authorization Failed (Public). The instanceId {instance_id} does not exist, or does not \
         belong to the account {}",
        engine::emulator_account_id()
    )
}

/// Walk every page of one instance's listing and return the names answered, in order,
/// together with how many pages it took.
async fn walk_pages(
    emulator: &Emulator,
    case: &str,
    instance_id: &str,
    max_results: Option<i64>,
) -> (Vec<String>, usize) {
    let limit = match max_results {
        Some(size) => usize::try_from(size).expect("a page size a case asks for is positive"),
        None => MAX_RESULTS_DEFAULT,
    };
    let mut names = Vec::new();
    let mut pages = 0;
    let mut token: Option<String> = None;
    // A listing of a bounded store terminates in a bounded number of pages; the cap is
    // here so a token that failed to advance fails the case instead of looping forever.
    while pages < 64 {
        let mut query: Vec<String> = Vec::new();
        if let Some(size) = max_results {
            query.push(format!("maxResults={size}"));
        }
        if let Some(value) = &token {
            query.push(format!("nextToken={value}"));
        }
        let (status, body) = emulator
            .list_namespaces(instance_id, &query.join("&"))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: page {pages} expected 200, got {status} with {body}"
        );
        let page = listed_names(case, &body);
        assert!(
            page.len() <= limit,
            "case {case:?}: page {pages} holds {} namespaces, more than maxResults {limit}",
            page.len()
        );
        pages += 1;
        token = next_token(&body);
        if token.is_some() {
            assert!(
                !page.is_empty(),
                "case {case:?}: page {pages} is empty yet carries a nextToken, so a client \
                 paging in a loop could never advance"
            );
        }
        names.extend(page);
        if token.is_none() {
            return (names, pages);
        }
    }
    panic!("case {case:?}: the listing never ran out of pages after {pages} pages");
}

/// RULE: the listing reports exactly the namespaces the instance's data lake holds — every
/// member of each one identical to what the namespace read answers — in name order, and
/// reports none of the data lake's pre-defined namespaces.
#[tokio::test]
async fn a_listing_reports_every_created_namespace_exactly_as_a_read_does() {
    let emulator = Emulator::new();
    let instance = emulator.given_instance("listing-host").await;

    // An instance whose data lake nobody has added to holds no CUSTOM namespace, and the
    // pre-defined ones are not reported — so the listing is empty rather than absent.
    let (status, body) = emulator.list_namespaces(&instance, "").await;
    assert_eq!(
        status, 200,
        "case the empty data lake: expected 200, got {status} with {body}"
    );
    assert!(
        namespaces_of("the empty data lake", &body).is_empty(),
        "case the empty data lake: a data lake with no created namespace reports none, got {body}"
    );
    assert_eq!(
        next_token(&body),
        None,
        "case the empty data lake: a listing with nothing left to report carries no nextToken, \
         got {body}"
    );

    // Rows differ along the name's form and length (both published bounds of
    // `DataLakeNamespaceName`) and whether a description was stored, and are CREATED in an
    // order that is not the name order, so the order the listing reports cannot be the
    // creation order by accident.
    let longest_name = "z".repeat(50);
    let rows: Vec<(&str, Option<&str>)> = vec![
        ("orders_2026_q1", Some("order lines for 2026 Q1")),
        ("2026", None),
        (longest_name.as_str(), Some("fifty characters of name")),
        ("a", None),
        ("my_aws_orders", Some("a reserved word that is not a prefix")),
        ("shipments", Some("inbound shipments")),
    ];
    for (name, description) in &rows {
        emulator
            .given_namespace(&instance, name, *description)
            .await;
    }

    let mut expected: Vec<&str> = rows.iter().map(|(name, _)| *name).collect();
    expected.sort_unstable();

    let case = "every created namespace";
    let (status, body) = emulator
        .list_namespaces(&instance, &format!("maxResults={MAX_RESULTS_MAX}"))
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200, got {status} with {body}"
    );
    assert_eq!(
        listed_names(case, &body),
        expected,
        "case {case:?}: the listing must report every created namespace in name order, got {body}"
    );
    assert_eq!(
        next_token(&body),
        None,
        "case {case:?}: a page holding the whole listing carries no nextToken, got {body}"
    );

    // Every listed entry is the record the namespace read answers from, member for member.
    for namespace in namespaces_of(case, &body) {
        let name = namespace
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: a listed namespace has no name: {namespace}"))
            .to_string();
        let (status, read) = emulator.get_namespace(&instance, &name).await;
        assert_eq!(
            status, 200,
            "case {name:?}: GetDataLakeNamespace expected 200, got {status} with {read}"
        );
        assert_eq!(
            Some(&namespace),
            read.get("namespace"),
            "case {name:?}: the listed namespace must be the one the read reports, got {read}"
        );
    }

    // The pre-defined namespaces exist for the dataset operations but are not records of
    // this store, so no listing reports them.
    let listed = listed_names(case, &body);
    for predefined in PREDEFINED_NAMESPACES {
        assert!(
            !listed.contains(&predefined.to_string()),
            "case {case:?}: the listing must not report the pre-defined namespace \
             {predefined:?}, got {listed:?}"
        );
    }
}

/// RULE: a listing is cut into pages of at most `maxResults`, and the pages a client walks
/// with the tokens it is handed concatenate to the whole order exactly once — whatever the
/// page size.
#[tokio::test]
async fn pagination_walks_the_whole_order_exactly_once_at_every_page_size() {
    let emulator = Emulator::new();
    let instance = emulator.given_instance("paging-host").await;

    // 24 namespaces: more than the largest page (20) and more than the default (10), so
    // every row below needs at least two pages. Created in DESCENDING name order, so a row
    // that walks the pages in name order cannot be reading the creation order.
    const SEEDED: usize = 24;
    let mut expected: Vec<String> = (1..=SEEDED).map(|index| format!("ns_{index:03}")).collect();
    for name in expected.iter().rev() {
        emulator.given_namespace(&instance, name, None).await;
    }
    expected.sort_unstable();

    // Rows differ along the page size: both published bounds of `maxResults`, sizes in
    // between (one of which divides the total exactly), and the modelled default, which a
    // row omits `maxResults` to get.
    let rows: Vec<(&str, Option<i64>)> = vec![
        ("one namespace per page", Some(MAX_RESULTS_MIN)),
        ("two namespaces per page", Some(2)),
        ("eight namespaces per page", Some(8)),
        ("seven namespaces per page", Some(7)),
        ("the largest page", Some(MAX_RESULTS_MAX)),
        ("the modelled default page", None),
    ];

    for (case, max_results) in rows {
        let (names, pages) = walk_pages(&emulator, case, &instance, max_results).await;
        assert_eq!(
            names, expected,
            "case {case:?}: the pages must concatenate to the whole order exactly once"
        );
        let size = match max_results {
            Some(size) => usize::try_from(size).expect("a page size a row asks for is positive"),
            None => MAX_RESULTS_DEFAULT,
        };
        let expected_pages = expected.len().div_ceil(size);
        assert_eq!(
            pages, expected_pages,
            "case {case:?}: {} namespaces in pages of {size} expected {expected_pages} pages",
            expected.len()
        );
    }
}

/// RULE: `maxResults` outside the bounds the model publishes is a validation failure, and
/// each published bound itself is served.
#[tokio::test]
async fn max_results_is_served_at_its_bounds_and_refused_outside_them() {
    let emulator = Emulator::new();
    let instance = emulator.given_instance("bounds-host").await;
    const SEEDED: usize = 3;
    for index in 1..=SEEDED {
        emulator
            .given_namespace(&instance, &format!("bounds_{index}"), None)
            .await;
    }

    // Rows: both published bounds of `DataLakeNamespaceMaxResults`, and one past each.
    let rows: Vec<(&str, i64, u16)> = vec![
        ("one below the minimum", MAX_RESULTS_MIN - 1, 400),
        ("the minimum", MAX_RESULTS_MIN, 200),
        ("the maximum", MAX_RESULTS_MAX, 200),
        ("one past the maximum", MAX_RESULTS_MAX + 1, 400),
    ];

    for (case, max_results, expected_status) in rows {
        let (status, body) = emulator
            .list_namespaces(&instance, &format!("maxResults={max_results}"))
            .await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: maxResults {max_results} expected HTTP {expected_status}, got \
             {status} with {body}"
        );
        if expected_status == 400 {
            assert_eq!(
                error_code(case, &body),
                "ValidationException",
                "case {case:?}: expected the modelled ValidationException, got {body}"
            );
        } else {
            let listed = listed_names(case, &body);
            let expected = usize::try_from(max_results)
                .expect("a served bound is positive")
                .min(SEEDED);
            assert_eq!(
                listed.len(),
                expected,
                "case {case:?}: expected {expected} namespaces on the page, got {listed:?}"
            );
        }
    }
}

/// RULE: only a `nextToken` this listing issued for THIS instance is a page boundary;
/// anything else is a validation failure rather than a silently truncated or restarted
/// page.
#[tokio::test]
async fn a_next_token_this_listing_did_not_issue_is_refused() {
    let emulator = Emulator::new();
    let instance = emulator.given_instance("token-host").await;
    let other_instance = emulator.given_instance("other-token-host").await;
    for index in 1..=3 {
        let name = format!("token_{index}");
        emulator.given_namespace(&instance, &name, None).await;
        emulator.given_namespace(&other_instance, &name, None).await;
    }

    // The token this listing hands out is the control every forged row below is a
    // variation on: it names the instance the page was cut from and the namespace to
    // resume at.
    let issued = |instance_id: &str, name: &str| {
        hex::encode(format!("scn:1:namespace:{instance_id}:{name}"))
    };

    // Rows differ along HOW the value fails to be a token this listing issued for this
    // instance: not hex at all, hex of a foreign payload, hex of a bare store key with no
    // stamp, a token stamped for another of this service's listings, the stamp over a name
    // of a form the model does not admit, a token carrying a field too many, and a token
    // issued for another instance's data lake.
    let rows: Vec<(&str, String)> = vec![
        ("not hex", "not-a-token".to_string()),
        (
            "hex of a foreign payload",
            hex::encode("someone-elses-cursor"),
        ),
        ("hex of a bare store key", hex::encode("token_2")),
        (
            "a token of another listing of this service",
            hex::encode(format!("scn:1:data-integration-event:{instance}::token_2")),
        ),
        (
            "the stamp over a name the model does not admit",
            issued(&instance, "Token_2"),
        ),
        (
            "a field too many behind the stamp",
            issued(&instance, "token_2:token_3"),
        ),
        (
            "a token issued for another instance",
            issued(&other_instance, "token_2"),
        ),
    ];

    for (case, token) in rows {
        let (status, body) = emulator
            .list_namespaces(&instance, &format!("nextToken={token}"))
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body}"
        );
        assert_eq!(
            error_code(case, &body),
            "ValidationException",
            "case {case:?}: expected the modelled ValidationException, got {body}"
        );
        assert_eq!(
            error_message(case, &body),
            UNISSUED_TOKEN_SENTENCE,
            "case {case:?}: expected the documented refusal sentence, got {body}"
        );
    }

    // The control: the token the listing itself issued resumes the listing.
    let (status, body) = emulator
        .list_namespaces(&instance, &format!("maxResults={MAX_RESULTS_MIN}"))
        .await;
    assert_eq!(
        status, 200,
        "case the issued token: the first page expected 200, got {status} with {body}"
    );
    let token = next_token(&body)
        .unwrap_or_else(|| panic!("case the issued token: the first page carried none: {body}"));
    let (status, body) = emulator
        .list_namespaces(
            &instance,
            &format!("maxResults={MAX_RESULTS_MIN}&nextToken={token}"),
        )
        .await;
    assert_eq!(
        status, 200,
        "case the issued token: resuming expected 200, got {status} with {body}"
    );
    assert_eq!(
        listed_names("the issued token", &body),
        vec!["token_2".to_string()],
        "case the issued token: the next page must resume where the first stopped, got {body}"
    );
}

/// RULE: a page token names a POSITION in the listing's order, so the page it resumes
/// reports the namespaces behind it even when the namespace the token names is gone.
#[tokio::test]
async fn a_page_token_still_reports_the_rest_when_the_namespace_it_names_is_gone() {
    let emulator = Emulator::new();
    let instance = emulator.given_instance("vanishing-host").await;
    for name in ["first", "second", "third"] {
        emulator.given_namespace(&instance, name, None).await;
    }

    // Name order puts `first` ahead of `second` and `third`, so a page of one stops with a
    // token naming `second`.
    let (status, body) = emulator
        .list_namespaces(&instance, &format!("maxResults={MAX_RESULTS_MIN}"))
        .await;
    assert_eq!(
        status, 200,
        "case the first page: expected 200, got {status} with {body}"
    );
    assert_eq!(
        listed_names("the first page", &body),
        vec!["first".to_string()],
        "case the first page: expected the first namespace in name order, got {body}"
    );
    let token = next_token(&body)
        .unwrap_or_else(|| panic!("case the first page: it carried no nextToken: {body}"));

    emulator.drop_namespace(&instance, "second");

    let case = "resuming past a namespace that is gone";
    let (status, body) = emulator
        .list_namespaces(&instance, &format!("maxResults={MAX_RESULTS_MAX}&nextToken={token}"))
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200, got {status} with {body}"
    );
    assert_eq!(
        listed_names(case, &body),
        vec!["third".to_string()],
        "case {case:?}: the rest of the listing must still be reported, got {body}"
    );
}

/// RULE: a namespace is addressed by its instance, so a listing reports the addressed
/// instance's namespaces and nobody else's — and an `instanceId` this account holds no
/// instance for is refused by the gateway's authorization step before any data lake is
/// read.
#[tokio::test]
async fn a_listing_reports_one_instances_data_lake_and_refuses_an_unknown_instance() {
    let emulator = Emulator::new();
    let instance = emulator.given_instance("own-host").await;
    let other_instance = emulator.given_instance("other-host").await;

    // The SAME name under two instances is two namespaces, and a third name only the
    // second instance holds.
    emulator.given_namespace(&instance, "shared", Some("mine")).await;
    emulator
        .given_namespace(&other_instance, "shared", Some("theirs"))
        .await;
    emulator.given_namespace(&other_instance, "theirs", None).await;

    let rows: Vec<(&str, &str, Vec<&str>)> = vec![
        ("the first instance", instance.as_str(), vec!["shared"]),
        (
            "the second instance",
            other_instance.as_str(),
            vec!["shared", "theirs"],
        ),
    ];
    for (case, instance_id, expected) in rows {
        let (status, body) = emulator
            .list_namespaces(instance_id, &format!("maxResults={MAX_RESULTS_MAX}"))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} with {body}"
        );
        assert_eq!(
            listed_names(case, &body),
            expected,
            "case {case:?}: a listing reports only the addressed instance's namespaces, got {body}"
        );
    }

    let case = "an instance this account does not hold";
    let (status, body) = emulator
        .list_namespaces(UNKNOWN_INSTANCE, &format!("maxResults={MAX_RESULTS_MAX}"))
        .await;
    assert_eq!(
        status, 403,
        "case {case:?}: expected HTTP 403, got {status} with {body}"
    );
    assert_eq!(
        error_code(case, &body),
        "AccessDeniedException",
        "case {case:?}: expected the modelled AccessDeniedException, got {body}"
    );
    assert_eq!(
        error_message(case, &body),
        expected_authorization_failure(UNKNOWN_INSTANCE),
        "case {case:?}: expected the gateway's authorization sentence, got {body}"
    );

    // An unknown instance is an authorization failure even when the request also carries a
    // token no listing issued: the gateway authorizes before it reads anything.
    let case = "an unknown instance and a forged token";
    let (status, body) = emulator
        .list_namespaces(UNKNOWN_INSTANCE, "nextToken=not-a-token")
        .await;
    assert_eq!(
        status, 403,
        "case {case:?}: expected HTTP 403, got {status} with {body}"
    );
    assert_eq!(
        error_code(case, &body),
        "AccessDeniedException",
        "case {case:?}: expected the modelled AccessDeniedException, got {body}"
    );
}

/// RULE: the listing REPORTS; it changes nothing a later caller sees.
#[tokio::test]
async fn listing_changes_nothing_a_later_caller_sees() {
    let emulator = Emulator::new();
    let instance = emulator.given_instance("idempotent-host").await;
    for index in 1..=4 {
        emulator
            .given_namespace(&instance, &format!("stable_{index}"), None)
            .await;
    }
    let stored = emulator.stored_namespaces(&instance);

    // Rows differ along the page size and whether the read is resumed, so none of the
    // listing's paths consumes anything.
    let rows: Vec<(&str, &str)> = vec![
        ("the whole listing", "maxResults=20"),
        ("a page of one", "maxResults=1"),
        ("the modelled default page", ""),
    ];
    for (case, query) in rows {
        let (status, first) = emulator.list_namespaces(&instance, query).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} with {first}"
        );
        let (status, again) = emulator.list_namespaces(&instance, query).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the repeated listing expected 200, got {status} with {again}"
        );
        assert_eq!(
            first, again,
            "case {case:?}: two identical listings must answer identically"
        );
        assert_eq!(
            emulator.stored_namespaces(&instance),
            stored,
            "case {case:?}: a listing must not change how many namespaces the store holds"
        );
    }
}
