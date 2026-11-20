// SPDX-License-Identifier: CC0-1.0

//! The whole Amazon DataZone subscription approval flow, driven over the wire on the
//! ASSEMBLED router: ask, approve, find the access, read it, give it up.
//!
//! RULE: a subscription request is an ASK, so it is created `PENDING` and awaits a
//! decision; accepting it moves the request to `ACCEPTED` and materialises the access as a
//! subscription that `ListSubscriptions` reports for that request and `GetSubscription`
//! reads as `APPROVED`; revoking that subscription moves it to `REVOKED`, which both
//! readers then report. The statuses are the modelled enums
//! (`SubscriptionRequestStatus` PENDING | ACCEPTED | REJECTED and `SubscriptionStatus`
//! APPROVED | CANCELLED | REVOKED — `smithy_query(query="operation",
//! operation="CreateSubscriptionRequest,ListSubscriptions,RevokeSubscription")`).
//!
//! The service's own suites table the filters, the pages and the refusals each of those
//! operations owes; what this one adds is that the flow a client runs end to end is served
//! by the emulator as ASSEMBLED — every call routed by the same `MultiServiceRouter` the
//! emulator boots with, with nothing read from the store.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A GENEROUS deadline for the transition the clock owes a created domain: far longer than
/// the span the service gives that step, so a host running the whole workspace's tests at
/// once still reaches it. A wait is for the CONDITION; this only bounds how long it is
/// looked for.
const LIFECYCLE_DEADLINE: Duration = Duration::from_secs(180);

/// How long to wait between two looks at a condition the clock owes.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// What a call answered: the HTTP status and the parsed body.
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    /// A member of the response body as a string, or `"<absent>"` so an assertion message
    /// says which member was missing.
    fn s(&self, member: &str) -> String {
        self.body
            .get(member)
            .and_then(Value::as_str)
            .unwrap_or("<absent>")
            .to_string()
    }
}

/// The assembled emulator, with DataZone reached through the router that mounts it.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.register_service(
            service_com_amazonaws_datazone_datazone::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// Send one request. The `Authorization` header carries a SigV4 credential scope,
    /// which is how the router picks the service and where the handlers read the caller's
    /// principal from.
    async fn call(&self, method: &str, uri: &str, body: Option<Value>) -> Answer {
        let payload = body.map(|b| serde_json::to_vec(&b).expect("a JSON body serializes"));
        let mut request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "datazone.us-west-2.amazonaws.com")
            .header(
                "authorization",
                "AWS4-HMAC-SHA256 \
                 Credential=AKIAEXAMPLETESTKEY/20261021/us-west-2/datazone/aws4_request, \
                 SignedHeaders=host, Signature=0",
            );
        if payload.is_some() {
            request = request.header("content-type", "application/json");
        }
        let request = request
            .body(bytes::Bytes::from(payload.unwrap_or_default()))
            .expect("a valid request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.into_body();
        let body: Value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Answer { status, body }
    }

    /// One call that has to be served, with the whole answer in the message when it is not.
    async fn served(&self, what: &str, method: &str, uri: &str, body: Option<Value>) -> Answer {
        let answer = self.call(method, uri, body).await;
        assert!(
            (200..300).contains(&answer.status),
            "{what}: expected a served answer, got {} {:?}",
            answer.status,
            answer.body
        );
        answer
    }

    /// Create a domain and look again until it reports `AVAILABLE`. Waits for the
    /// CONDITION the clock owes, never for a fixed time.
    async fn available_domain(&self, name: &str) -> String {
        let created = self
            .served(
                "creating the domain",
                "POST",
                "/v2/domains",
                Some(json!({
                    "name": name,
                    "domainExecutionRole": ::engine::emulator_arn(
                        "iam",
                        "",
                        "role/service-role/AmazonDataZoneDomainExecution",
                    ),
                })),
            )
            .await;
        let id = created.s("id");
        let started = Instant::now();
        loop {
            let read = self.call("GET", &format!("/v2/domains/{id}"), None).await;
            if read.status == 200 && read.s("status") == "AVAILABLE" {
                return id;
            }
            assert!(
                started.elapsed() < LIFECYCLE_DEADLINE,
                "domain {name:?} never reached AVAILABLE: last answer {} {:?}",
                read.status,
                read.body
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

#[tokio::test]
async fn datazone_subscription_is_asked_approved_listed_and_revoked() {
    let dz = Emulator::new();
    let domain = dz.available_domain("assembled-subscription-flow").await;

    let producer = dz
        .served(
            "creating the producing project",
            "POST",
            &format!("/v2/domains/{domain}/projects"),
            Some(json!({ "name": "producer" })),
        )
        .await
        .s("id");
    let consumer = dz
        .served(
            "creating the consuming project",
            "POST",
            &format!("/v2/domains/{domain}/projects"),
            Some(json!({ "name": "consumer" })),
        )
        .await
        .s("id");

    // What the producer publishes: an asset of its own custom type, and the listing that
    // records it. No subscription target is created for it, which is not what decides a
    // request.
    dz.served(
        "creating the asset type",
        "POST",
        &format!("/v2/domains/{domain}/asset-types"),
        Some(json!({
            "name": "AssembledFlowType",
            "owningProjectIdentifier": producer,
            "formsInput": {},
        })),
    )
    .await;
    let asset = dz
        .served(
            "creating the asset",
            "POST",
            &format!("/v2/domains/{domain}/assets"),
            Some(json!({
                "name": "assembled-flow-asset",
                "typeIdentifier": "AssembledFlowType",
                "owningProjectIdentifier": producer,
            })),
        )
        .await
        .s("id");
    let listing = dz
        .served(
            "publishing the asset",
            "POST",
            &format!("/v2/domains/{domain}/listings/change-set"),
            Some(json!({
                "entityIdentifier": asset,
                "entityType": "ASSET",
                "action": "PUBLISH",
            })),
        )
        .await
        .s("listingId");

    // The ASK: a request for the consuming project, which awaits a decision.
    let asked = dz
        .served(
            "asking for access",
            "POST",
            &format!("/v2/domains/{domain}/subscription-requests"),
            Some(json!({
                "subscribedPrincipals": [{ "project": { "identifier": consumer } }],
                "subscribedListings": [{ "identifier": listing }],
                "requestReason": "the consumer asks for the published asset",
            })),
        )
        .await;
    assert_eq!(
        asked.s("status"),
        "PENDING",
        "an ask awaits a decision, got {:?}",
        asked.body
    );
    let request = asked.s("id");

    // The APPROVAL: the decision moves the request to its terminal status.
    let accepted = dz
        .served(
            "accepting the ask",
            "PUT",
            &format!("/v2/domains/{domain}/subscription-requests/{request}/accept"),
            None,
        )
        .await;
    assert_eq!(
        accepted.s("status"),
        "ACCEPTED",
        "the decision is recorded on the request, got {:?}",
        accepted.body
    );

    // The ACCESS the approval materialised, found by the request it came from.
    let listed = dz
        .served(
            "listing the subscriptions of the request",
            "GET",
            &format!("/v2/domains/{domain}/subscriptions?subscriptionRequestIdentifier={request}"),
            None,
        )
        .await;
    let items = listed
        .body
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        items.len(),
        1,
        "the approval materialised exactly one access, got {:?}",
        listed.body
    );
    let subscription = items[0]
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("<absent>")
        .to_string();
    assert_eq!(
        items[0].get("status").and_then(Value::as_str),
        Some("APPROVED"),
        "the materialised access is granted, got {:?}",
        listed.body
    );

    let read = dz
        .served(
            "reading the access",
            "GET",
            &format!("/v2/domains/{domain}/subscriptions/{subscription}"),
            None,
        )
        .await;
    assert_eq!(
        (read.s("status"), read.s("subscriptionRequestId")),
        ("APPROVED".to_string(), request.clone()),
        "the reader reports the granted access and the request it came from, got {:?}",
        read.body
    );

    // GIVING IT UP: the revoke moves the access to its terminal status, and both readers
    // report that from this call on.
    let revoked = dz
        .served(
            "revoking the access",
            "PUT",
            &format!("/v2/domains/{domain}/subscriptions/{subscription}/revoke"),
            Some(json!({ "retainPermissions": false })),
        )
        .await;
    assert_eq!(
        revoked.s("status"),
        "REVOKED",
        "the revoke gives the access up, got {:?}",
        revoked.body
    );
    let read_again = dz
        .served(
            "reading the revoked access",
            "GET",
            &format!("/v2/domains/{domain}/subscriptions/{subscription}"),
            None,
        )
        .await;
    assert_eq!(
        read_again.s("status"),
        "REVOKED",
        "the reader reports the revocation, got {:?}",
        read_again.body
    );
    let revoked_listing = dz
        .served(
            "listing the revoked subscriptions",
            "GET",
            &format!("/v2/domains/{domain}/subscriptions?status=REVOKED"),
            None,
        )
        .await;
    assert_eq!(
        revoked_listing
            .body
            .get("items")
            .and_then(Value::as_array)
            .map(|items| items
                .iter()
                .filter_map(|item| item.get("id").and_then(Value::as_str))
                .collect::<Vec<_>>()),
        Some(vec![subscription.as_str()]),
        "the revoked listing reports exactly the access this flow gave up, got {:?}",
        revoked_listing.body
    );
}
