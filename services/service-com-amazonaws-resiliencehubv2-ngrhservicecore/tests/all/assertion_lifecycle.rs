// SPDX-License-Identifier: CC0-1.0

//! The `Assertion` lifecycle, end to end over HTTP: `CreateAssertion` -> `ListAssertions`,
//! and the cascade `DeleteService` performs over the assertions a service holds.
//!
//! The rule under test: a resilience assertion is a child of exactly one service, so a
//! create stores it under that service's record and every member of the `Assertion` it
//! answers is projected from what was stored — the caller's `text` as supplied, a minted
//! `assertionId` of the modelled `Uuid` form, the `USER` source for a caller-created
//! assertion, and `createdAt` / `updatedAt`; a listing projects the same stored records in
//! creation order, filtered by `source` when the request names one, a `serviceArn` naming
//! no stored service is refused `ResourceNotFoundException`, a replayed `clientToken`
//! answers the assertion the first call created, and deleting the service removes its
//! assertions with it.
//!
//! Every suite drives the real wire path (`MultiServiceRouter::dispatch`), so the modelled
//! constraints, the restJson1 (de)serializers and the HTTP status codes are exercised
//! exactly as a client would exercise them.
//!
//! The suites are TABLES: each row is one input of the class the rule covers (text length
//! and shape, token shape, page size, source filter, child count), not one recorded
//! request.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_resiliencehubv2_ngrhservicecore::state::{
    ASSERTION_SOURCE_USER, MAX_RESULTS_MAX, MAX_RESULTS_MIN,
};
use service_com_amazonaws_resiliencehubv2_ngrhservicecore::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The modelled routes this suite drives.
const CREATE_SERVICE: &str = "/v2/create-service";
const CREATE_ASSERTION: &str = "/v2/create-assertion";
const DELETE_SERVICE: &str = "/v2/delete-service";
const LIST_ASSERTIONS: &str = "/v2/list-assertions";

/// A SigV4 `Authorization` header whose credential scope names this service's signing
/// name (`resiliencehub`) and the Region ARNs are minted in. Signature verification is off
/// for these tests, so the value only has to parse.
const SIGNED: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDTESTONLY00000000/20260101/us-east-1/resiliencehub/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0123456789abcdef";

/// `AssertionText`'s modelled bounds.
///
/// SOURCE: `smithy_query(query="operation", operation="CreateAssertion")` reports
/// `text` targeting `AssertionText` with `length {min: 1, max: 1000}`.
const ASSERTION_TEXT_MIN_LENGTH: usize = 1;
const ASSERTION_TEXT_MAX_LENGTH: usize = 1000;

/// The values `AssertionSource` accepts on the wire.
///
/// SOURCE: `smithy_query(query="shape", shape="AssertionSource")` — an enum of exactly
/// `AI_GENERATED` and `USER`.
const ASSERTION_SOURCE_VALUES: [&str; 2] = ["AI_GENERATED", "USER"];

/// An emulator holding only this service, over one shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body
        .map(|value| serde_json::to_vec(&value).expect("a JSON body serializes"))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", SIGNED)
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(payload))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// Create a service named `name` and answer its ARN.
async fn create_service(router: &MultiServiceRouter, name: &str) -> String {
    let (status, body) = call(
        router,
        "POST",
        CREATE_SERVICE,
        Some(json!({
            "name": name,
            "regions": ["us-east-1"],
            "permissionModel": {"invokerRoleName": "ResilienceHubRole"},
        })),
    )
    .await;
    assert_eq!(status, 200, "creating service {name}: got {body:?}");
    body["service"]["serviceArn"]
        .as_str()
        .unwrap_or_else(|| panic!("no service.serviceArn in {body:?}"))
        .to_string()
}

async fn create_assertion(router: &MultiServiceRouter, body: Value) -> (u16, Value) {
    call(router, "POST", CREATE_ASSERTION, Some(body)).await
}

async fn list_assertions(router: &MultiServiceRouter, query: &str) -> (u16, Value) {
    call(router, "GET", &format!("{LIST_ASSERTIONS}?{query}"), None).await
}

async fn delete_service(router: &MultiServiceRouter, service_arn: &str) -> (u16, Value) {
    call(
        router,
        "POST",
        DELETE_SERVICE,
        Some(json!({"serviceArn": service_arn})),
    )
    .await
}

/// The modelled error code of a refusal, from the restJson1 `__type` discriminator.
fn error_code(body: &Value) -> String {
    body["__type"]
        .as_str()
        .unwrap_or("<no __type>")
        .rsplit(['#', ':'])
        .next()
        .unwrap_or("<no __type>")
        .to_string()
}

/// The `assertions` array of a successful listing.
fn listed(case: &str, status: u16, body: &Value) -> Vec<Value> {
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ListAssertions, got {status} {body:?}"
    );
    body["assertions"]
        .as_array()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the @required assertions member is absent from {body:?}")
        })
        .clone()
}

/// Whether `candidate` satisfies the `Uuid` shape every `assertionId` is typed with.
///
/// SOURCE: `smithy_query(query="operation", operation="DeleteAssertion")` reports
/// `assertionId` targeting `Uuid` with `length {min: 36, max: 36}` and pattern
/// `^[0-9a-f]{8}-[0-9a-f]{4}-[0-5][0-9a-f]{3}-[089ab][0-9a-f]{3}-[0-9a-f]{12}$` — so the
/// id is lowercase hexadecimal in 8-4-4-4-12 groups, with a version nibble in `0..=5` and
/// a variant nibble in `[089ab]`.
fn is_modelled_uuid(candidate: &str) -> bool {
    let groups: Vec<&str> = candidate.split('-').collect();
    if groups.len() != 5 {
        return false;
    }
    if [8usize, 4, 4, 4, 12]
        .iter()
        .zip(&groups)
        .any(|(expected, group)| group.len() != *expected)
    {
        return false;
    }
    if !candidate
        .chars()
        .all(|c| c == '-' || c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        return false;
    }
    let version = groups[2].chars().next().unwrap_or('f');
    let variant = groups[3].chars().next().unwrap_or('f');
    ('0'..='5').contains(&version) && "089ab".contains(variant)
}

/// The create table: rows differ along `text` length and shape — the shortest
/// `AssertionText`, a sentence, one carrying punctuation and newlines, one carrying
/// non-ASCII characters, and the longest.
fn text_rows() -> Vec<(&'static str, String)> {
    vec![
        ("the shortest text", "a".repeat(ASSERTION_TEXT_MIN_LENGTH)),
        (
            "a sentence",
            "The checkout service survives the loss of one Availability Zone.".to_string(),
        ),
        (
            "punctuation and newlines",
            "RTO <= 60s; RPO <= 5s.\nFails over to us-west-2 (warm standby).".to_string(),
        ),
        (
            "non-ASCII characters",
            "Le service tolère la perte d'une zone — RTO ≤ 60 s.".to_string(),
        ),
        ("the longest text", "t".repeat(ASSERTION_TEXT_MAX_LENGTH)),
    ]
}

/// Assert that `assertion` is the stored assertion `text` created under `service_arn`.
fn assert_projects_text(case: &str, text: &str, service_arn: &str, assertion: &Value) {
    assert_eq!(
        assertion["serviceArn"].as_str(),
        Some(service_arn),
        "case {case:?}: expected the parent service's ARN, got {:?}",
        assertion["serviceArn"]
    );
    assert_eq!(
        assertion["text"].as_str(),
        Some(text),
        "case {case:?}: expected the supplied text, got {:?}",
        assertion["text"]
    );
    assert_eq!(
        assertion["source"].as_str(),
        Some(ASSERTION_SOURCE_USER),
        "case {case:?}: a caller-created assertion's source is {ASSERTION_SOURCE_USER}, got {:?}",
        assertion["source"]
    );
    let id = assertion["assertionId"].as_str().unwrap_or_default();
    assert!(
        is_modelled_uuid(id),
        "case {case:?}: expected an assertionId of the modelled Uuid form, got {id:?}"
    );
    for member in ["createdAt", "updatedAt"] {
        assert!(
            assertion[member].is_number(),
            "case {case:?}: expected a {member} timestamp, got {:?}",
            assertion[member]
        );
    }
    assert_eq!(
        assertion["createdAt"], assertion["updatedAt"],
        "case {case:?}: an assertion nothing has updated reports updatedAt == createdAt, got \
         {:?} and {:?}",
        assertion["createdAt"], assertion["updatedAt"]
    );
}

#[tokio::test]
async fn every_created_assertion_is_stored_and_read_back() {
    let router = emulator();
    let service_arn = create_service(&router, "Payments").await;
    let rows = text_rows();

    let mut created: Vec<(&str, String, Value)> = Vec::new();
    for (case, text) in &rows {
        let (status, body) =
            create_assertion(&router, json!({"serviceArn": service_arn, "text": text})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateAssertion, got {status} {body:?}"
        );
        let assertion = body["assertion"].clone();
        assert!(
            !assertion.is_null(),
            "case {case:?}: the @required assertion member is absent from {body:?}"
        );
        assert_projects_text(case, text, &service_arn, &assertion);
        created.push((case, text.clone(), assertion));
    }

    // Every assertion got its own identity, so no create overwrote another.
    let ids: BTreeSet<&str> = created
        .iter()
        .map(|(_, _, assertion)| assertion["assertionId"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        ids.len(),
        created.len(),
        "expected one distinct assertionId per create, got {created:?}"
    );

    // The ROUND TRIP: the listing projects the same stored records, in creation order.
    let (status, body) = list_assertions(&router, &format!("serviceArn={service_arn}")).await;
    let assertions = listed("round trip", status, &body);
    assert!(
        body.get("nextToken").is_none(),
        "every assertion fits one default-sized page, so no nextToken is due: {body:?}"
    );
    assert_eq!(
        assertions.len(),
        created.len(),
        "expected every created assertion in the listing, got {assertions:?}"
    );
    for ((case, text, at_create), from_list) in created.iter().zip(assertions.iter()) {
        assert_projects_text(case, text, &service_arn, from_list);
        assert_eq!(
            at_create, from_list,
            "case {case:?}: the listing must project the same stored record the create \
             answered, got {from_list:?}"
        );
    }

    // A read leaves the store untouched, so two listings agree.
    let (status, again) = list_assertions(&router, &format!("serviceArn={service_arn}")).await;
    assert_eq!(
        listed("repeated read", status, &again),
        assertions,
        "two reads of the same service's assertions must answer identically"
    );
}

#[tokio::test]
async fn a_service_arn_naming_no_service_is_refused_not_served() {
    let router = emulator();
    let real = create_service(&router, "Inventory").await;
    // A well-formed `Arn` for a service this emulator never created, and a service ARN that
    // WAS created and then deleted: neither has a record to hold or list an assertion.
    let deleted = create_service(&router, "Retired").await;
    let (status, body) = delete_service(&router, &deleted).await;
    assert_eq!(status, 200, "deleting the service: got {status} {body:?}");
    let never_created = format!("{real}-absent");

    for (case, service_arn) in [
        ("never created", never_created.as_str()),
        ("created then deleted", deleted.as_str()),
    ] {
        let (status, body) = create_assertion(
            &router,
            json!({"serviceArn": service_arn, "text": "Survives one zone failure"}),
        )
        .await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected HTTP 404 from CreateAssertion, got {status} {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {body:?}"
        );

        let (status, body) = list_assertions(&router, &format!("serviceArn={service_arn}")).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected HTTP 404 from ListAssertions, got {status} {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {body:?}"
        );
    }
}

#[tokio::test]
async fn an_assertion_belongs_to_the_service_it_was_created_for() {
    let router = emulator();
    let first = create_service(&router, "Checkout").await;
    let second = create_service(&router, "Fulfilment").await;

    // The same text in two services is two assertions: the text is not an identity, so
    // neither create is a conflict and each service lists only its own record. The rows
    // differ along the text's shape.
    let texts = [
        "a",
        "Survives the loss of one Availability Zone",
        "RTO <= 60s; RPO <= 5s.",
    ];

    for text in texts {
        let (status, here) =
            create_assertion(&router, json!({"serviceArn": first, "text": text})).await;
        assert_eq!(
            status, 200,
            "case {text:?}: expected HTTP 200 for the first create, got {status} {here:?}"
        );
        let (status, elsewhere) =
            create_assertion(&router, json!({"serviceArn": second, "text": text})).await;
        assert_eq!(
            status, 200,
            "case {text:?}: the same text in another service must be accepted, got {status} \
             {elsewhere:?}"
        );
        assert_ne!(
            elsewhere["assertion"]["assertionId"], here["assertion"]["assertionId"],
            "case {text:?}: two services' assertions are distinct records, got {elsewhere:?}"
        );
    }

    for (case, service_arn) in [("first service", &first), ("second service", &second)] {
        let (status, body) = list_assertions(&router, &format!("serviceArn={service_arn}")).await;
        let assertions = listed(case, status, &body);
        assert_eq!(
            assertions.len(),
            texts.len(),
            "case {case:?}: expected only this service's assertions, got {assertions:?}"
        );
        for assertion in &assertions {
            assert_eq!(
                assertion["serviceArn"].as_str(),
                Some(service_arn.as_str()),
                "case {case:?}: a listing reports only assertions of the service asked for, got \
                 {assertion:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_replayed_client_token_returns_the_assertion_the_first_call_created() {
    let router = emulator();
    let service_arn = create_service(&router, "Idempotent").await;

    // The rows differ along the token's shape within `ClientToken`'s modelled
    // `^[A-Za-z0-9_.-]{0,63}$` / `len 1..63`: the shortest, a dotted one, and the longest.
    let tokens = [
        ("shortest token", "t".to_string()),
        ("dotted token", "req.2026-01-01_42".to_string()),
        ("longest token", "k".repeat(63)),
    ];

    for (index, (case, token)) in tokens.iter().enumerate() {
        let body = json!({
            "serviceArn": service_arn,
            "text": format!("Assertion {index}"),
            "clientToken": token,
        });
        let (status, first) = create_assertion(&router, body.clone()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 for the first call, got {status} {first:?}"
        );
        let (status, replay) = create_assertion(&router, body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 for the replay, got {status} {replay:?}"
        );
        assert_eq!(
            replay["assertion"], first["assertion"],
            "case {case:?}: a replayed idempotency token must answer the stored assertion, got \
             {replay:?}"
        );

        // The SAME token asking for different text is two requests wearing one token, not a
        // retry of either.
        let (status, refused) = create_assertion(
            &router,
            json!({
                "serviceArn": service_arn,
                "text": format!("Assertion {index}, revised"),
                "clientToken": token,
            }),
        )
        .await;
        assert_eq!(
            status, 409,
            "case {case:?}: expected HTTP 409 for a token reused for different text, got \
             {status} {refused:?}"
        );
        assert_eq!(
            error_code(&refused),
            "ConflictException",
            "case {case:?}: expected ConflictException, got {refused:?}"
        );
    }

    let (status, body) = list_assertions(&router, &format!("serviceArn={service_arn}")).await;
    let assertions = listed("after the replays", status, &body);
    assert_eq!(
        assertions.len(),
        tokens.len(),
        "a replay must not store a second assertion, got {assertions:?}"
    );
}

#[tokio::test]
async fn a_listing_pages_in_creation_order_and_refuses_a_token_it_did_not_issue() {
    let router = emulator();
    let service_arn = create_service(&router, "Paged").await;

    let texts: Vec<String> = (0..7).map(|index| format!("Assertion {index}")).collect();
    for text in &texts {
        let (status, body) =
            create_assertion(&router, json!({"serviceArn": service_arn, "text": text})).await;
        assert_eq!(status, 200, "creating {text}: got {status} {body:?}");
    }

    // The rows differ along the page size: each modelled bound of `maxResults`
    // (`range 1..100`), a size that divides the set, one that does not, and the default
    // (no `maxResults` at all).
    let sizes: [(&str, Option<i32>); 5] = [
        ("the minimum page size", Some(MAX_RESULTS_MIN)),
        ("a page size that divides the set", Some(3)),
        ("a page size that does not divide the set", Some(4)),
        ("the maximum page size", Some(MAX_RESULTS_MAX)),
        ("the default page size", None),
    ];

    for (case, max_results) in sizes {
        let mut seen: Vec<String> = Vec::new();
        let mut next: Option<String> = None;
        let mut pages = 0usize;
        loop {
            let mut query = format!("serviceArn={service_arn}");
            if let Some(size) = max_results {
                query.push_str(&format!("&maxResults={size}"));
            }
            if let Some(token) = next.as_deref() {
                query.push_str(&format!("&nextToken={token}"));
            }
            let (status, body) = list_assertions(&router, &query).await;
            let page = listed(case, status, &body);
            if let Some(size) = max_results {
                assert!(
                    page.len() <= size as usize,
                    "case {case:?}: a page must not exceed maxResults={size}, got {page:?}"
                );
            }
            for assertion in &page {
                seen.push(
                    assertion["text"]
                        .as_str()
                        .unwrap_or_else(|| panic!("case {case:?}: no text in {assertion:?}"))
                        .to_string(),
                );
            }
            pages += 1;
            assert!(
                pages <= texts.len() + 1,
                "case {case:?}: paging did not terminate, saw {seen:?}"
            );
            next = body
                .get("nextToken")
                .and_then(Value::as_str)
                .map(str::to_string);
            if next.is_none() {
                break;
            }
        }
        assert_eq!(
            seen, texts,
            "case {case:?}: paging must enumerate every assertion exactly once in creation order"
        );
    }

    // A `nextToken` this service did not issue is a bad request, not an empty page. The
    // rows span a token that is not this service's encoding at all and one that is
    // well-formed but names a position never handed out ("999999" hex-encoded, which is how
    // this service encodes a page position).
    for (case, token) in [
        ("not this service's encoding", "not-a-token"),
        ("a position never handed out", "393939393939"),
    ] {
        let (status, body) = list_assertions(
            &router,
            &format!("serviceArn={service_arn}&nextToken={token}"),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for a forged nextToken, got {status} {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {body:?}"
        );
    }

    // And a `maxResults` outside the modelled range is refused rather than served at some
    // other size — each published bound is tested, and one past it on each side.
    for (case, size) in [
        ("one below the minimum", MAX_RESULTS_MIN - 1),
        ("one above the maximum", MAX_RESULTS_MAX + 1),
    ] {
        let (status, body) = list_assertions(
            &router,
            &format!("serviceArn={service_arn}&maxResults={size}"),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for maxResults={size}, got {status} {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {body:?}"
        );
    }
}

#[tokio::test]
async fn a_listing_reports_only_the_source_the_request_asks_for() {
    let router = emulator();
    let service_arn = create_service(&router, "Filtered").await;

    let texts: Vec<String> = (0..3).map(|index| format!("Assertion {index}")).collect();
    for text in &texts {
        let (status, body) =
            create_assertion(&router, json!({"serviceArn": service_arn, "text": text})).await;
        assert_eq!(status, 200, "creating {text}: got {status} {body:?}");
    }

    // The rows are every modelled `AssertionSource` value plus the unfiltered listing.
    // Nothing in this slice generates an assertion, so `USER` and no filter at all report
    // the same set and `AI_GENERATED` reports none — the filter narrows to the stored
    // `source`, it does not invent records.
    for source in ASSERTION_SOURCE_VALUES {
        let (status, body) =
            list_assertions(&router, &format!("serviceArn={service_arn}&source={source}")).await;
        let assertions = listed(source, status, &body);
        let expected = if source == ASSERTION_SOURCE_USER {
            texts.len()
        } else {
            0
        };
        assert_eq!(
            assertions.len(),
            expected,
            "case {source:?}: expected {expected} assertion(s) carrying that source, got \
             {assertions:?}"
        );
        for assertion in &assertions {
            assert_eq!(
                assertion["source"].as_str(),
                Some(source),
                "case {source:?}: a filtered listing reports only that source, got {assertion:?}"
            );
        }
    }

    let (status, body) = list_assertions(&router, &format!("serviceArn={service_arn}")).await;
    assert_eq!(
        listed("no filter", status, &body).len(),
        texts.len(),
        "an unfiltered listing reports every stored assertion, got {body:?}"
    );
}

#[tokio::test]
async fn deleting_a_service_cascades_to_the_assertions_it_holds() {
    let router = emulator();

    // The rows differ along how many assertions the deleted service held.
    for (case, assertion_count) in [
        ("a service with no assertions", 0usize),
        ("a service with one assertion", 1),
        ("a service with several assertions", 4),
    ] {
        let doomed = create_service(&router, &format!("Doomed{assertion_count}")).await;
        let bystander = create_service(&router, &format!("Bystander{assertion_count}")).await;
        for index in 0..assertion_count {
            for service_arn in [&doomed, &bystander] {
                let (status, body) = create_assertion(
                    &router,
                    json!({"serviceArn": service_arn, "text": format!("Assertion {index}")}),
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: creating assertion {index}: got {status} {body:?}"
                );
            }
        }

        let (status, body) = delete_service(&router, &doomed).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteService, got {status} {body:?}"
        );

        // The deleted service's assertions went with it: there is no service left to list
        // them under.
        let (status, body) = list_assertions(&router, &format!("serviceArn={doomed}")).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected HTTP 404 listing a deleted service's assertions, got \
             {status} {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {body:?}"
        );

        // And the cascade reached only that service: the bystander keeps every assertion.
        let (status, body) = list_assertions(&router, &format!("serviceArn={bystander}")).await;
        let assertions = listed(case, status, &body);
        assert_eq!(
            assertions.len(),
            assertion_count,
            "case {case:?}: an unrelated service must keep its assertions, got {assertions:?}"
        );

        // A service recreated under the deleted one's name starts with no assertions, so
        // nothing leaked out of the removed record.
        let reborn = create_service(&router, &format!("Doomed{assertion_count}")).await;
        let (status, body) = list_assertions(&router, &format!("serviceArn={reborn}")).await;
        let assertions = listed(case, status, &body);
        assert!(
            assertions.is_empty(),
            "case {case:?}: a freshly created service holds no assertions, got {assertions:?}"
        );
    }
}
