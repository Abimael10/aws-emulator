// SPDX-License-Identifier: CC0-1.0

//! `DeleteAccountAssociation` against an identifier that resolves to nothing, over the wire:
//! `CreateConnectorDestination` -> `CreateAccountAssociation` -> `DeleteAccountAssociation`
//! (repeated, cross-region, and for identifiers no create ever minted).
//!
//! RULE: `DeleteAccountAssociation` (DELETE `/account-associations/{AccountAssociationId}`,
//! `@idempotent`, success 200 with an empty `Unit` body) promises an END STATE — the signing
//! region holds no account association under that identifier — rather than that this call was
//! the one that reached it. An identifier that region resolves no association under already
//! satisfies it, so the call removes nothing, writes nothing, and answers that same modelled
//! success with no error code and no body, whatever made the record absent (never created,
//! already deleted, or created through another region's endpoint) and whatever form the
//! identifier takes inside `AccountAssociationId`'s modelled `@length` 1..64 and
//! `@pattern ^[0-9a-zA-Z]+$`. An identifier OUTSIDE those constraints is still the modelled
//! `ValidationException` (HTTP 400), and the modelled `ResourceNotFoundException` (HTTP 404)
//! remains what the RESOLVING paths — `GetAccountAssociation` — answer for an absent
//! association.
//!
//! The recorded unit this was derived from (the delete of an identifier no association exists
//! under) is ONE row of the tables below; the rows differ along what the rule generalises
//! over: the form and length of an absent identifier, what made it absent, how many times the
//! same delete is repeated, and the region the request is signed for.
//!
//! Every case drives the real restJson1 request path (`MultiServiceRouter::dispatch`), so the
//! routes, statuses, bodies and modelled errors asserted here are the ones a client sees.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iotmanagedintegrations_iotmanagedintegrations::{
    ComAmazonawsIotmanagedintegrationsIotmanagedintegrationsState as ServiceState, EmulatedService,
};
use smithy_server_runtime::MultiServiceRouter;

/// The region the test's requests are signed for unless a case names another.
const REGION: &str = "us-east-1";

/// The routes this suite drives
/// (`smithy_query(query="operation",operation="DeleteAccountAssociation")` -> DELETE
/// `/account-associations/{AccountAssociationId}`, `CreateAccountAssociation` -> POST
/// `/account-associations`, `CreateConnectorDestination` -> POST `/connector-destinations`,
/// `CreateCloudConnector` -> POST `/cloud-connectors`).
const COLLECTION: &str = "/account-associations";
const CONNECTOR_DESTINATIONS: &str = "/connector-destinations";
const CLOUD_CONNECTORS: &str = "/cloud-connectors";

/// The modelled success status of `DeleteAccountAssociation` (same call -> `http` code 200).
const DELETE_STATUS: u16 = 200;

/// The `@length` bounds of `AccountAssociationId`, in characters (same call ->
/// `AccountAssociationId` len 1..64), which the absent-identifier rows are built against.
const ACCOUNT_ASSOCIATION_ID_LENGTH: (usize, usize) = (1, 64);

/// One HTTP answer, reduced to what the assertions read.
struct Reply {
    status: u16,
    /// The restJson1 error code, from `x-amzn-errortype`.
    error_type: Option<String>,
    body: Value,
}

impl Reply {
    fn string(&self, name: &str) -> Option<&str> {
        self.body
            .get(name)
            .filter(|value| !value.is_null())
            .and_then(Value::as_str)
    }
}

/// A router holding one `iotmanagedintegrations` service over the engine the caller keeps a
/// handle to: the same assembly `core/router` performs, with one service mounted.
fn emulator_over(engine: Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    // The signature itself is not under test here, so requests are unauthenticated.
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// Send one request, signed for `region`, and reduce the answer. The `Authorization`
/// credential scope is what routes the request to this service and names its region.
async fn call_in(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Reply {
    let payload = body.map_or_else(bytes::Bytes::new, |value| {
        bytes::Bytes::from(serde_json::to_vec(&value).expect("serialize the request body"))
    });
    let request = http::Request::builder()
        .method(method)
        .uri(format!(
            "https://iotmanagedintegrations.{region}.amazonaws.com{path}"
        ))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTTESTTESTTEST/20260101/{region}/\
                 iotmanagedintegrations/aws4_request, SignedHeaders=host, Signature=0"
            ),
        )
        .header("content-type", "application/json")
        .body(payload)
        .expect("a well-formed request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = response
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
        error_type,
        body,
    }
}

/// `DeleteAccountAssociation` of `id`, signed for `region`.
async fn delete_in(router: &MultiServiceRouter, region: &str, id: &str) -> Reply {
    call_in(router, region, "DELETE", &format!("{COLLECTION}/{id}"), None).await
}

/// `GetAccountAssociation` of `id`, signed for `region`.
async fn get_in(router: &MultiServiceRouter, region: &str, id: &str) -> Reply {
    call_in(router, region, "GET", &format!("{COLLECTION}/{id}"), None).await
}

/// Create a connector destination in `region` through the wire and answer the
/// `ConnectorDestinationId` it minted.
///
/// `CreateAccountAssociation` resolves its `ConnectorDestinationId` against this service's own
/// connector-destination store, so every association below is created against a destination a
/// client really created; a connector destination in turn names a C2C connector
/// (`CloudConnectorId` is `@required`), so the connector comes first.
async fn connector_destination_in(router: &MultiServiceRouter, region: &str, case: &str) -> String {
    let connector = call_in(
        router,
        region,
        "POST",
        CLOUD_CONNECTORS,
        Some(json!({
            "Name": "partner cloud connector",
            "EndpointConfig": {"lambda": {"arn": "connector-handler"}},
        })),
    )
    .await;
    assert_eq!(
        connector.status, 201,
        "case {case:?}: expected HTTP 201 from CreateCloudConnector, got {} with body {:?}",
        connector.status, connector.body
    );
    let destination = call_in(
        router,
        region,
        "POST",
        CONNECTOR_DESTINATIONS,
        Some(json!({
            "CloudConnectorId": connector.string("Id").unwrap_or_default(),
            "AuthConfig": {},
            // The ARN of the secret a destination keeps its credentials in, inside the
            // `SecretsManagerArn` `@pattern` and the `SecretsManagerVersionId` `@length`.
            "SecretsManager": {
                "arn": format!(
                    "arn:aws:secretsmanager:{region}:{}:secret:destination-A1b2C3",
                    engine::EMULATOR_ACCOUNT_ID
                ),
                "versionId": "a".repeat(32),
            },
        })),
    )
    .await;
    assert_eq!(
        destination.status, 201,
        "case {case:?}: expected HTTP 201 from CreateConnectorDestination, got {} with body {:?}",
        destination.status, destination.body
    );
    destination
        .string("Id")
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected CreateConnectorDestination to answer an Id, got body {:?}",
                destination.body
            )
        })
        .to_string()
}

/// `CreateAccountAssociation` against `connector_destination_id` carrying `members`, signed
/// for `region`, and the `AccountAssociationId` it minted.
async fn create_in(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    connector_destination_id: &str,
    members: Value,
) -> String {
    let mut body = members;
    body["ConnectorDestinationId"] = json!(connector_destination_id);
    let created = call_in(router, region, "POST", COLLECTION, Some(body)).await;
    assert_eq!(
        created.status, 201,
        "case {case:?}: expected HTTP 201 from CreateAccountAssociation, got {} with body {:?}",
        created.status, created.body
    );
    created
        .string("AccountAssociationId")
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected an AccountAssociationId, got body {:?}",
                created.body
            )
        })
        .to_string()
}

/// Assert one answer is this delete's modelled success: the status the model binds, no error
/// code, and no body at all (the output shape is `Unit`).
fn assert_delete_succeeded(case: &str, reply: &Reply) {
    assert_eq!(
        reply.status, DELETE_STATUS,
        "case {case:?}: expected HTTP {DELETE_STATUS}, got {} with body {:?}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.error_type, None,
        "case {case:?}: a successful delete carries no error code, got {:?}",
        reply.error_type
    );
    assert_eq!(
        reply.body,
        Value::Null,
        "case {case:?}: the modelled output has no members, so the body is empty, got {:?}",
        reply.body
    );
}

/// The identifiers the store holds, so a no-op delete can be shown to have written nothing.
fn held_ids(engine: &engine::InMemoryEngine) -> Vec<String> {
    engine
        .state::<ServiceState>()
        .lock()
        .account_association_order
        .clone()
}

/// RULE: an identifier no create ever minted resolves to nothing, so the delete of it answers
/// the modelled success and leaves the store exactly as it was — for every form the modelled
/// `@length` 1..64 and `@pattern ^[0-9a-zA-Z]+$` admit.
#[tokio::test]
async fn a_delete_of_an_identifier_no_create_minted_succeeds_and_writes_nothing() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_over(Arc::clone(&engine));
    let (min_length, max_length) = ACCOUNT_ASSOCIATION_ID_LENGTH;

    // One association that EXISTS, so each no-op below has a real record to leave alone.
    let destination = connector_destination_in(&router, REGION, "the destination").await;
    let held = create_in(&router, REGION, "the held association", &destination, json!({})).await;

    // Identifiers inside the modelled constraints that no create stored: both ends of the
    // `@length` bound, lower case, upper case, mixed case, digits only, a digit-and-letter
    // run of the shape a client's generated id takes, and the held id with one character
    // appended (inside the bound, resolving to nothing).
    let absent: Vec<(String, String)> = vec![
        ("shortest admitted".to_string(), "a".repeat(min_length)),
        ("longest admitted".to_string(), "b".repeat(max_length)),
        ("lower case".to_string(), "nosuchassociation".to_string()),
        ("upper case".to_string(), "NOSUCHASSOCIATION".to_string()),
        ("mixed case".to_string(), "NoSuchAssociation1".to_string()),
        ("digits only".to_string(), "1234567890".to_string()),
        (
            "a zero run followed by letters".to_string(),
            format!("{}{}", "0".repeat(40), "A".repeat(6)),
        ),
        (
            "an extension of the held id".to_string(),
            format!("{held}x"),
        ),
    ];

    for (case, identifier) in absent {
        assert_delete_succeeded(&case, &delete_in(&router, REGION, &identifier).await);
        assert_eq!(
            held_ids(&engine),
            vec![held.clone()],
            "case {case:?}: a delete that resolved nothing must leave the store untouched, \
             got {:?}",
            held_ids(&engine)
        );
        let still = get_in(&router, REGION, &held).await;
        assert_eq!(
            still.status, 200,
            "case {case:?}: the held association must still read back, got {} with body {:?}",
            still.status, still.body
        );
    }
}

/// RULE: the promise is the end state, so repeating the SAME delete answers the same success
/// every time — the call that removed the record and every retry behind it are
/// indistinguishable to a client, while the read reports the association absent.
#[tokio::test]
async fn repeating_the_same_delete_answers_the_same_success() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_over(Arc::clone(&engine));
    let destination = connector_destination_in(&router, REGION, "the destination").await;

    // The rows differ along how many times the delete is repeated, so the first call is the
    // one that removes the record in each of them and the rest resolve nothing.
    for repeats in 1..=4u32 {
        let case = format!("{repeats} deletes");
        let id = create_in(&router, REGION, &case, &destination, json!({})).await;
        for attempt in 1..=repeats {
            assert_delete_succeeded(
                &format!("{case}, delete number {attempt}"),
                &delete_in(&router, REGION, &id).await,
            );
            assert!(
                held_ids(&engine).is_empty(),
                "case {case:?}: after delete number {attempt} the store must hold no \
                 association, got {:?}",
                held_ids(&engine)
            );
            let gone = get_in(&router, REGION, &id).await;
            assert_eq!(
                gone.status, 404,
                "case {case:?}: the read after delete number {attempt} reports the \
                 association absent, got {} with body {:?}",
                gone.status, gone.body
            );
            assert_eq!(
                gone.error_type.as_deref(),
                Some("ResourceNotFoundException"),
                "case {case:?}: the read answers the modelled 404 code, got {:?}",
                gone.error_type
            );
        }
    }
}

/// RULE: an association is regional, so another region's endpoint resolves nothing under its
/// identifier: the delete there succeeds, removes nothing, and the creating region keeps
/// reporting the record until its own delete removes it.
#[tokio::test]
async fn a_delete_through_another_region_removes_nothing_and_still_succeeds() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_over(Arc::clone(&engine));
    let destination = connector_destination_in(&router, REGION, "the destination").await;
    let held = create_in(&router, REGION, "the held association", &destination, json!({})).await;

    for region in ["us-west-2", "eu-central-1", "ap-southeast-2"] {
        assert_delete_succeeded(region, &delete_in(&router, region, &held).await);
        assert_eq!(
            held_ids(&engine),
            vec![held.clone()],
            "case {region:?}: another region's delete must remove nothing, got {:?}",
            held_ids(&engine)
        );
        let untouched = get_in(&router, REGION, &held).await;
        assert_eq!(
            untouched.status, 200,
            "case {region:?}: the creating region must still report the association, got {} \
             with body {:?}",
            untouched.status, untouched.body
        );
    }

    // The creating region's own delete is the one that removes it.
    assert_delete_succeeded("the creating region", &delete_in(&router, REGION, &held).await);
    assert!(
        held_ids(&engine).is_empty(),
        "expected the creating region's delete to remove the record, got {:?}",
        held_ids(&engine)
    );
}

/// RULE: a delete that resolved nothing writes nothing — in particular it does not touch the
/// idempotency record of an association it did not name, so a create repeating that
/// `ClientToken` still replays into the association that token minted.
#[tokio::test]
async fn a_delete_that_resolved_nothing_leaves_another_records_client_token_replaying() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_over(Arc::clone(&engine));
    let destination = connector_destination_in(&router, REGION, "the destination").await;

    // The forms the modelled `ClientToken` `@pattern` (`^[a-zA-Z0-9=_-]+$`, len 1..64) admits.
    for token in ["t", "teardown-token-1", "TOKEN_2=="] {
        let case = format!("token {token:?}");
        let members = json!({"ClientToken": token});
        let held = create_in(&router, REGION, &case, &destination, members.clone()).await;

        // A delete of an identifier this region resolves nothing under, performed between the
        // create and its retry.
        assert_delete_succeeded(
            &format!("{case}, deleting an absent identifier"),
            &delete_in(&router, REGION, &format!("{held}0")).await,
        );

        let replayed = create_in(
            &router,
            REGION,
            &format!("{case}, retried"),
            &destination,
            members,
        )
        .await;
        assert_eq!(
            replayed, held,
            "case {case:?}: the token must still replay into {held:?}, got {replayed:?}"
        );

        // Leave the destination free for the next row.
        assert_delete_succeeded(
            &format!("{case}, cleanup"),
            &delete_in(&router, REGION, &held).await,
        );
        assert!(
            held_ids(&engine).is_empty(),
            "case {case:?}: this row's association must be gone, got {:?}",
            held_ids(&engine)
        );
    }
}

/// RULE: only an identifier INSIDE the input's modelled constraints reaches the end-state
/// reading above; one outside `@length` 1..64 or `@pattern ^[0-9a-zA-Z]+$` is still the
/// modelled `ValidationException`, so the idempotent delete did not become a blanket 200.
#[tokio::test]
async fn an_identifier_outside_the_modelled_constraints_is_still_refused() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_over(Arc::clone(&engine));
    let (_, max_length) = ACCOUNT_ASSOCIATION_ID_LENGTH;

    // The rows differ along which constraint the identifier breaks: one past the `@length`
    // maximum, and the characters the `@pattern` does not admit (hyphen, underscore, colon,
    // period).
    let refused: Vec<(&str, String)> = vec![
        ("one past the length maximum", "a".repeat(max_length + 1)),
        ("a hyphen", "no-such-association".to_string()),
        ("an underscore", "no_such_association".to_string()),
        ("a colon", "no:such:association".to_string()),
        ("a period", "no.such.association".to_string()),
    ];

    for (case, identifier) in refused {
        let refusal = delete_in(&router, REGION, &identifier).await;
        assert_eq!(
            refusal.status, 400,
            "case {case:?}: expected HTTP 400, got {} with body {:?}",
            refusal.status, refusal.body
        );
        assert_eq!(
            refusal.error_type.as_deref(),
            Some("ValidationException"),
            "case {case:?}: expected the modelled ValidationException code, got {:?}",
            refusal.error_type
        );
        assert!(
            held_ids(&engine).is_empty(),
            "case {case:?}: a refused delete writes nothing, got {:?}",
            held_ids(&engine)
        );
    }
}
