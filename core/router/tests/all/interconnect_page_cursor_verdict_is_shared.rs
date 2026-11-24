// SPDX-License-Identifier: CC0-1.0

//! ONE judgement answers the `nextToken` of EVERY paginated Interconnect listing.
//!
//! The rule under test: **a page cursor is judged by one rule for the whole service — a
//! `nextToken` is served only when it is one THAT listing minted (authenticated under the
//! secret the store drew and never reveals, over the scope that decides which sequence was
//! walked), and every other value is refused with HTTP 400
//! `InterconnectValidationException` and this service's one wording for a cursor it did not
//! issue, never with a page — so which listing was called changes neither the branch taken
//! nor the class the refusal travels in.**
//!
//! That is what the shared judgement in the owning crate's `state` module buys: the three
//! listings (`ListConnections`, `ListEnvironments`, `ListAttachPoints`) each supply only the
//! scope their own cursors are minted under and the variant of their own modelled error
//! channel the refusal travels in, so none of them can classify or word this one fault
//! differently from its neighbours. The suite drives the accepting branch and the refusing
//! branch of that judgement through all three of them.
//!
//! The rows differ along what the rule generalises over: the operation (all three), the
//! sequence walked (the Region's published Environment catalog, the account's connections
//! and the attach points of one Environment — the latter two empty on a fresh store, which
//! is what makes a cursor naming a key no sequence holds a case of its own), the position
//! a cursor names (a key the sequence holds and one it does not), the page size the walk is
//! windowed by, and the form of the value offered as a cursor (a value no listing minted,
//! this listing's public tag with a key behind it, this store's secret over a sibling
//! listing's scope, over another Region's endpoint of the same listing and over the listing
//! of an Environment this Region does not hold, another store's secret over this listing's
//! own scope, and this store's secret over an empty key). The owning crate's
//! `tests/all/page_cursors_are_only_what_the_listing_minted.rs` walks the same three
//! listings over POPULATED sequences and at byte level; what this suite adds is that one
//! judgement, rather than three, is what answers all of them.
//!
//! This suite lives here, beside the other suites of the assembled emulator, because the
//! owning crate roots its integration tests at a hand-declared `tests/all.rs`, which a fix
//! may not edit; the router's `tests/all/` picks a new suite up with nothing to declare.
//! Every call goes through a router with Interconnect mounted beside the service that owns
//! the account's Direct Connect gateways on one engine, exactly as the production router
//! mounts them, so each assertion exercises the real wire path.

use std::sync::Arc;

use serde_json::{json, Map, Value};
use service_com_amazonaws_interconnect_interconnect::state::{
    attach_point_cursor_scope, connection_cursor_scope, cursor_not_issued_message, encode_cursor,
    environment_cursor_scope, service_state, PageTokenKey, ATTACH_POINT_CURSOR_TAG,
    CONNECTION_CURSOR_TAG, ENVIRONMENT_CURSOR_TAG,
};
use smithy_server_runtime::MultiServiceRouter;

/// The signing name the Interconnect model gives the service, which is the credential
/// scope the router picks the claiming service by.
const SIGNING_NAME: &str = "interconnect";

/// The Region whose endpoint answers every call below.
const REGION: &str = "us-east-1";

/// A second Region, so a row can offer one endpoint's cursor to another: a cursor names a
/// position in ONE walked sequence, and each regional endpoint walks its own.
const OTHER_REGION: &str = "us-west-2";

/// The error this service models for a fault in the request's own input (HTTP 400), which
/// is the one class every listing refuses a cursor it did not issue with.
const VALIDATION_CODE: &str = "InterconnectValidationException";

/// A key no sequence of this service holds, used wherever a row needs a position rather
/// than a stored record: what a cursor NAMES is not what decides whether it was issued.
const UNHELD_KEY: &str = "aaaaaaaa";

/// One paginated listing under test: the request that makes it, the sequence it walks, and
/// the scopes a cursor of it could be confused with.
struct Listing {
    /// The operation the request names, which is also what its refusal wording names.
    operation: &'static str,
    /// The request this listing is made with, before `nextToken` and `maxResults`.
    body: Value,
    /// The output member the page's items arrive in.
    items_member: &'static str,
    /// The member of an item that is its page key, which is what a cursor carries.
    key_member: &'static str,
    /// The public tag this operation stamps on its cursors: not a secret, which is the
    /// point of the row that sends it with a key of its own behind it.
    tag: &'static str,
    /// The scope this listing mints its cursors under.
    scope: String,
    /// Scopes of THIS store that are not this listing's: a sibling listing's, another
    /// Region's endpoint's, another Environment's. Each is labelled for the assertion.
    foreign_scopes: Vec<(String, String)>,
}

/// A router with Interconnect and the service owning the account's Direct Connect gateways
/// mounted over ONE engine, plus the secret that engine's Interconnect store authenticates
/// its page cursors under — so a row can mint a cursor of a scope a listing does NOT serve,
/// which only the scope can refuse.
fn emulator() -> (MultiServiceRouter, PageTokenKey) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The Authorization header below still carries the credential scope the router routes
    // by; the signature itself is not what this suite is about.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_interconnect_interconnect::EmulatedService::new(Arc::clone(&engine)),
    );
    // An attach point IS a Direct Connect gateway, so the service that owns those gateways
    // is mounted beside Interconnect: without it the attach-point listing could not resolve
    // the account's gateways at all, which is a different verdict.
    router.register_service(
        service_com_amazonaws_directconnect_overtureservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    let token_key = service_state(&*engine)
        .expect("the Interconnect store is mounted on this engine")
        .lock()
        .page_token_key;
    (router, token_key)
}

/// One Interconnect operation (`awsJson1_0`) signed for `region`, as a `(status, body)`
/// pair.
async fn ic(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: Value,
) -> (u16, Value) {
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/{SIGNING_NAME}/aws4_request, \
         SignedHeaders=host;x-amz-date;x-amz-target, Signature=not-verified"
    );
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{SIGNING_NAME}.localhost"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("Interconnect.{operation}"))
        .header("authorization", authorization)
        .body(bytes::Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let decoded = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, decoded)
}

/// One page of `listing`, optionally resuming from `token` and windowed by `max_results`.
async fn page(
    router: &MultiServiceRouter,
    listing: &Listing,
    token: Option<&str>,
    max_results: Option<i64>,
) -> (u16, Value) {
    let mut request: Map<String, Value> = listing.body.as_object().cloned().unwrap_or_default();
    if let Some(token) = token {
        request.insert("nextToken".to_string(), json!(token));
    }
    if let Some(max) = max_results {
        request.insert("maxResults".to_string(), json!(max));
    }
    ic(router, REGION, listing.operation, Value::Object(request)).await
}

/// A string member, or `""` when absent — so a missing member fails an assertion rather
/// than panicking the suite.
fn text(value: &Value, member: &str) -> String {
    value[member].as_str().unwrap_or_default().to_string()
}

/// The wire error code of an error body.
fn code(body: &Value) -> String {
    text(body, "__type")
}

/// The `message` an error body carries, under either casing the envelope may use.
fn message(body: &Value) -> String {
    body["message"]
        .as_str()
        .or_else(|| body["Message"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// The page keys a successful page carries, in the order the page reports them.
fn keys_of(listing: &Listing, body: &Value) -> Vec<String> {
    body[listing.items_member]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|item| text(item, listing.key_member))
        .collect()
}

/// The `environmentId`s the Region's published catalog offers, which is the one sequence
/// this suite can walk without provisioning anything.
async fn environment_ids(router: &MultiServiceRouter, region: &str) -> Vec<String> {
    let (status, body) = ic(router, region, "ListEnvironments", json!({})).await;
    assert_eq!(
        status, 200,
        "case {region:?}: expected the Environment catalog to be served, got {status} with \
         {body:?}"
    );
    body["environments"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|environment| text(environment, "environmentId"))
        .collect()
}

/// The three paginated listings of this service, each with the scopes its cursors could be
/// confused with, built from the LIVE catalog rather than from a copy of it.
async fn listings(router: &MultiServiceRouter) -> Vec<Listing> {
    let catalog = environment_ids(router, REGION).await;
    let environment = catalog
        .first()
        .cloned()
        .expect("the Region publishes a catalog of Environments");
    // An `environmentId` of ANOTHER Region's catalog: a well-formed id this Region's
    // endpoint holds no Environment for, so no listing of this Region ever walked the
    // sequence a cursor of that scope would name.
    let other_environment = environment_ids(router, OTHER_REGION)
        .await
        .first()
        .cloned()
        .expect("the other Region publishes a catalog of Environments");
    assert_ne!(
        environment, other_environment,
        "case \"catalog\": expected each Region's catalog to name its own Environments, got \
         {environment:?} in both"
    );
    vec![
        Listing {
            operation: "ListConnections",
            body: json!({}),
            items_member: "connections",
            key_member: "id",
            tag: CONNECTION_CURSOR_TAG,
            scope: connection_cursor_scope(REGION, None, None, None, None),
            foreign_scopes: vec![
                (
                    "another Region's endpoint".to_string(),
                    connection_cursor_scope(OTHER_REGION, None, None, None, None),
                ),
                (
                    "the Environment listing".to_string(),
                    environment_cursor_scope(REGION, None),
                ),
                (
                    "an attach-point listing".to_string(),
                    attach_point_cursor_scope(REGION, &environment),
                ),
            ],
        },
        Listing {
            operation: "ListEnvironments",
            body: json!({}),
            items_member: "environments",
            key_member: "environmentId",
            tag: ENVIRONMENT_CURSOR_TAG,
            scope: environment_cursor_scope(REGION, None),
            foreign_scopes: vec![
                (
                    "another Region's endpoint".to_string(),
                    environment_cursor_scope(OTHER_REGION, None),
                ),
                (
                    "the connection listing".to_string(),
                    connection_cursor_scope(REGION, None, None, None, None),
                ),
                (
                    "an attach-point listing".to_string(),
                    attach_point_cursor_scope(REGION, &environment),
                ),
            ],
        },
        Listing {
            operation: "ListAttachPoints",
            body: json!({ "environmentId": environment }),
            items_member: "attachPoints",
            key_member: "identifier",
            tag: ATTACH_POINT_CURSOR_TAG,
            scope: attach_point_cursor_scope(REGION, &environment),
            foreign_scopes: vec![
                (
                    "another Region's endpoint".to_string(),
                    attach_point_cursor_scope(OTHER_REGION, &environment),
                ),
                (
                    "a listing of an Environment this Region does not hold".to_string(),
                    attach_point_cursor_scope(REGION, &other_environment),
                ),
                (
                    "the connection listing".to_string(),
                    connection_cursor_scope(REGION, None, None, None, None),
                ),
                (
                    "the Environment listing".to_string(),
                    environment_cursor_scope(REGION, None),
                ),
            ],
        },
    ]
}

/// The values to offer `listing` as a `nextToken` it did not issue, each labelled with the
/// case it stands for. The labels are the same for every listing, so a row can be compared
/// across the three.
fn tokens_no_listing_minted(listing: &Listing, store_key: &PageTokenKey) -> Vec<(String, String)> {
    let mut rows = vec![
        (
            "a value no listing minted".to_string(),
            "not-a-token".to_string(),
        ),
        (
            "this listing's public tag with a key behind it".to_string(),
            format!("{}{UNHELD_KEY}", listing.tag),
        ),
        (
            "another store's secret over this listing's own scope".to_string(),
            encode_cursor(&PageTokenKey::new(), &listing.scope, UNHELD_KEY),
        ),
        (
            "this store's secret over an empty key".to_string(),
            encode_cursor(store_key, &listing.scope, ""),
        ),
    ];
    for (label, scope) in &listing.foreign_scopes {
        rows.push((
            format!("this store's secret over the scope of {label}"),
            encode_cursor(store_key, scope, UNHELD_KEY),
        ));
    }
    rows
}

/// RULE: every paginated listing of this service refuses a `nextToken` it did not issue in
/// ONE class and ONE wording — HTTP 400 `InterconnectValidationException` naming the
/// listing whose cursors the value would have had to be one of — and answers no page at
/// all, so no listing can classify or word that fault differently from its neighbours.
#[tokio::test]
async fn every_listing_refuses_a_cursor_it_did_not_issue_in_one_class() {
    let (router, store_key) = emulator();
    let table = listings(&router).await;

    let mut rows_driven = 0_usize;
    for listing in &table {
        for (case, token) in tokens_no_listing_minted(listing, &store_key) {
            rows_driven += 1;
            let label = format!("{} with {case}", listing.operation);
            let (status, body) = page(&router, listing, Some(&token), None).await;
            assert_eq!(
                status, 400,
                "case {label:?}: expected HTTP 400 for a cursor this listing did not issue, got \
                 {status} with {body:?}"
            );
            assert!(
                code(&body).contains(VALIDATION_CODE),
                "case {label:?}: expected {VALIDATION_CODE}, got {:?}",
                code(&body)
            );
            assert_eq!(
                message(&body),
                cursor_not_issued_message(listing.operation),
                "case {label:?}: expected this service's one wording for a cursor it did not \
                 issue, got {:?}",
                message(&body)
            );
            assert!(
                body[listing.items_member].as_array().is_none(),
                "case {label:?}: expected no page at all, got {:?}",
                body[listing.items_member]
            );
        }
    }
    // The table spans all three listings rather than one, so a listing quietly dropping
    // out of it fails here rather than passing vacuously.
    assert!(
        rows_driven >= 21,
        "case \"the unissued-cursor table\": expected every form to be driven through every \
         listing, got {rows_driven} rows"
    );
}

/// RULE: the same fault reaches the same verdict whichever listing it arrives at — the
/// status and the error code are identical across the three, and the sentence differs only
/// in the listing it names.
#[tokio::test]
async fn the_three_listings_answer_one_fault_with_one_verdict() {
    let (router, store_key) = emulator();
    let table = listings(&router).await;

    let forms = tokens_no_listing_minted(&table[0], &store_key).len();
    for form in 0..forms {
        let mut answers: Vec<(&str, String, u16, String, String)> = Vec::new();
        for listing in &table {
            let (case, token) = tokens_no_listing_minted(listing, &store_key).remove(form);
            let (status, body) = page(&router, listing, Some(&token), None).await;
            answers.push((
                listing.operation,
                case,
                status,
                code(&body),
                // The sentence with the listing's own name taken out of it: what is left is
                // the part every listing has to share.
                message(&body).replace(listing.operation, "<operation>"),
            ));
        }
        let (first_operation, first_case, first_status, first_code, first_sentence) =
            answers[0].clone();
        for (operation, case, status, wire_code, sentence) in &answers[1..] {
            assert_eq!(
                (*status, wire_code.as_str(), sentence.as_str()),
                (first_status, first_code.as_str(), first_sentence.as_str()),
                "case {case:?} / {operation} against {first_operation} ({first_case:?}): expected \
                 one status, one error code and one sentence for one fault, got \
                 ({status}, {wire_code:?}, {sentence:?})"
            );
        }
    }
}

/// RULE: a cursor a listing DID mint is served by it — the judgement answers with the key
/// the page resumes after, so the walk continues where the previous page ended and no
/// position is repeated or skipped.
#[tokio::test]
async fn a_cursor_a_listing_minted_resumes_that_listings_walk() {
    let (router, store_key) = emulator();
    let table = listings(&router).await;

    for listing in &table {
        // Nothing offered at all starts at the head of the sequence, which is the other
        // answer the shared judgement gives.
        let (status, whole) = page(&router, listing, None, None).await;
        assert_eq!(
            status, 200,
            "case {}: expected the unwindowed listing to succeed, got {status} with {whole:?}",
            listing.operation
        );
        let keys = keys_of(listing, &whole);

        // A cursor minted the one way this store mints one — under its own secret, over
        // this listing's own scope — is SERVED, whatever position it names: the account's
        // connections and the Environment's attach points are empty here, and a cursor
        // naming a key no sequence holds is still a cursor this listing issued, so it
        // resumes the walk after that key instead of being refused.
        for key in [UNHELD_KEY.to_string(), keys.first().cloned().unwrap_or_default()] {
            if key.is_empty() {
                continue;
            }
            let issued = encode_cursor(&store_key, &listing.scope, &key);
            let (status, resumed) = page(&router, listing, Some(&issued), None).await;
            assert_eq!(
                status, 200,
                "case {} resuming from its own cursor over {key:?}: expected HTTP 200, got \
                 {status} with {resumed:?}",
                listing.operation
            );
            let expected: Vec<String> = keys
                .iter()
                .filter(|held| held.as_str() > key.as_str())
                .cloned()
                .collect();
            assert_eq!(
                keys_of(listing, &resumed),
                expected,
                "case {} resuming from its own cursor over {key:?}: expected the sequence after \
                 that key, got {resumed:?}",
                listing.operation
            );
        }

        // And the cursor the listing HANDS BACK is one it serves: the walk is windowed at
        // every page size from one item to one past the whole sequence, and each page
        // resumes from the token the page before it issued.
        for size in 1..=keys.len() + 1 {
            let mut walked: Vec<String> = Vec::new();
            let mut token: Option<String> = None;
            loop {
                let (status, body) = page(&router, listing, token.as_deref(), Some(size as i64))
                    .await;
                assert_eq!(
                    status, 200,
                    "case {} at page size {size}: expected HTTP 200, got {status} with {body:?}",
                    listing.operation
                );
                walked.extend(keys_of(listing, &body));
                let next = body["nextToken"].as_str().map(str::to_string);
                assert_eq!(
                    next.is_some(),
                    walked.len() < keys.len(),
                    "case {} at page size {size}: expected a cursor exactly while items remain, \
                     got {:?} after {} of {} items",
                    listing.operation,
                    body["nextToken"],
                    walked.len(),
                    keys.len()
                );
                let Some(next) = next else {
                    break;
                };
                token = Some(next);
            }
            assert_eq!(
                walked, keys,
                "case {} at page size {size}: expected the paged walk to equal the whole \
                 sequence, got {walked:?}",
                listing.operation
            );
        }
    }
}
