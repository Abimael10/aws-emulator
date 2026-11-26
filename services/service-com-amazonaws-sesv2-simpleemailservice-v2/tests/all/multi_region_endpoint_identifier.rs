// SPDX-License-Identifier: CC0-1.0

//! What a client can depend on about a multi-region endpoint's `EndpointId`, end to end
//! over HTTP.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with this
//! service registered — so routing, deserialization, the handler and serialization are
//! all exercised, and the identifier asserted on is the one the response carried.
//!
//! The rule these cases pin: **`EndpointId` is minted by the service, once, for the
//! endpoint the create persists — it is never caller input, it names that one endpoint
//! for as long as the endpoint exists, and no two mints share a value.** The model fixes
//! no form for it to take (`smithy_query(query="shape",shape="EndpointId")` reports a
//! plain string with no `@pattern` and no `@length`, documented only as "The ID of the
//! multi-region endpoint (global-endpoint)"), so what the surface promises is an
//! identity, and that is what these cases assert:
//!
//! * the create answers an id that is none of the values the request carried — not the
//!   endpoint's name, not either Region;
//! * every read of the endpoint reports exactly that id: `GetMultiRegionEndpoint` while
//!   the endpoint is still being provisioned AND once the provisioning transition the
//!   clock owes has fallen due, and `ListMultiRegionEndpoints` for the same endpoint;
//! * an id is MINTED PER ENDPOINT, not derived from what the request names: the same name
//!   in two primary regions is two endpoints with two ids, and a name minted again after
//!   its endpoint's deletion has completed gets a new id rather than the one the deleted
//!   endpoint held.
//!
//! The TEXT FORM the emulator mints that identifier in is pinned by
//! `tests/all/multi_region_endpoint_identifier_form.rs` — the pair of dot-separated
//! alphanumeric labels SES hands the id back as; the cases here are about the identity
//! those forms carry, so they hold whatever form a future mint takes.
//!
//! The table below spans what the rule generalises over: the endpoint name's length and
//! character set, the secondary region the route details name, how many tags the create
//! carries, and the Region the call executes in (which is the endpoint's primary region).

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_sesv2_simpleemailservice_v2::state::{
    ComAmazonawsSesv2SimpleemailserviceV2State, MAX_TAGS_PER_RESOURCE,
};
use service_com_amazonaws_sesv2_simpleemailservice_v2::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The Region most cases execute in: the one their `Authorization` credential scope
/// names, which is what makes it the created endpoint's primary region.
const HOME: &str = "us-east-1";

/// Another Region, for the cases that mint an endpoint of the same name somewhere else.
const ELSEWHERE: &str = "eu-west-1";

/// One answered request: the status, the error shape when there was one, and the parsed
/// body.
struct Reply {
    status: u16,
    error_type: Option<String>,
    body: Value,
}

impl Reply {
    /// The modelled error code the response names, or an empty string on success.
    ///
    /// restJson1 resolves the error shape from `x-amzn-errortype`; the JSON body's
    /// `__type` carries the same name, so either is read.
    fn error_code(&self) -> String {
        let raw = self
            .error_type
            .as_deref()
            .or_else(|| self.body.get("__type").and_then(Value::as_str))
            .unwrap_or_default();
        let name = raw.split('#').next_back().unwrap_or(raw);
        name.split(':').next().unwrap_or(name).to_string()
    }

    /// A string member of the body, or `""` when absent.
    fn text(&self, member: &str) -> String {
        self.body
            .get(member)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// The emulated service behind its router, plus the shared engine so a case can bring a
/// clock-owed transition due instead of waiting for it.
struct Ses {
    engine: Arc<engine::InMemoryEngine>,
    router: MultiServiceRouter,
}

impl Ses {
    fn new() -> Self {
        let engine = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // These cases exercise handler behaviour, not SigV4.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { engine, router }
    }

    /// Send one request, signed for `region` — the credential scope is what tells the
    /// handler which AWS-Region the operation executes in.
    async fn call(&self, region: &str, method: &str, path: &str, body: Option<Value>) -> Reply {
        let payload = match &body {
            Some(value) => bytes::Bytes::from(serde_json::to_vec(value).expect("serialize body")),
            None => bytes::Bytes::new(),
        };
        let host = format!("email.{region}.amazonaws.com");
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host)
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIAEMULATOR0000000/20260101/{region}/ses/aws4_request, SignedHeaders=host, Signature=00"
                ),
            )
            .body(payload)
            .expect("build request");
        let response = self.router.dispatch(request).await;
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

    /// Create one endpoint, asserting the create succeeded so no later assertion can be
    /// fooled by a failed setup, and answer the `EndpointId` it minted.
    async fn mint(&self, case: &str, region: &str, name: &str, secondary: &str, tags: usize) -> String {
        let created = self
            .call(
                region,
                "POST",
                "/v2/email/multi-region-endpoints",
                Some(json!({
                    "EndpointName": name,
                    "Details": {"RoutesDetails": [{"Region": secondary}]},
                    "Tags": tag_list(tags),
                })),
            )
            .await;
        assert_eq!(
            created.status,
            200,
            "case {case:?}: the create must succeed, got {} {} {:?}",
            created.status,
            created.error_code(),
            created.body
        );
        let id = created.text("EndpointId");
        assert!(
            !id.is_empty() && !id.contains(char::is_whitespace),
            "case {case:?}: expected a minted EndpointId, got {id:?}"
        );
        // An identifier the service mints is not the request read back: a client that
        // sends a name cannot be handed its own input as the resource's id.
        for (member, sent) in [
            ("EndpointName", name),
            ("the primary region", region),
            ("Details.RoutesDetails[0].Region", secondary),
        ] {
            assert_ne!(
                id, sent,
                "case {case:?}: the minted EndpointId must not be the request's {member}, got {id:?}"
            );
        }
        id
    }

    async fn get(&self, region: &str, name: &str) -> Reply {
        self.call(
            region,
            "GET",
            &format!("/v2/email/multi-region-endpoints/{name}"),
            None,
        )
        .await
    }

    async fn delete(&self, region: &str, name: &str) -> Reply {
        self.call(
            region,
            "DELETE",
            &format!("/v2/email/multi-region-endpoints/{name}"),
            None,
        )
        .await
    }

    async fn list(&self, region: &str) -> Reply {
        self.call(region, "GET", "/v2/email/multi-region-endpoints", None)
            .await
    }

    /// Bring the clock-owed provisioning transition of `name` due, without waiting for
    /// it: the handler stores a due instant, so a case moves that instant instead of
    /// sleeping, which is what keeps it independent of how loaded the machine is.
    fn settle_provisioning(&self, case: &str, region: &str, name: &str) {
        let state = self
            .engine
            .state::<ComAmazonawsSesv2SimpleemailserviceV2State>();
        let mut store = state.lock();
        let record = store
            .multi_region_endpoints
            .get_mut(&(region.to_string(), name.to_string()))
            .unwrap_or_else(|| panic!("case {case:?}: the endpoint must exist in the store"));
        record.ready_due_at = SystemTime::now() - Duration::from_secs(1);
    }

    /// Bring the in-flight deletion of `name` due, the same way.
    fn complete_deletion(&self, case: &str, region: &str, name: &str) {
        let state = self
            .engine
            .state::<ComAmazonawsSesv2SimpleemailserviceV2State>();
        let mut store = state.lock();
        let record = store
            .multi_region_endpoints
            .get_mut(&(region.to_string(), name.to_string()))
            .unwrap_or_else(|| panic!("case {case:?}: the endpoint must exist in the store"));
        assert!(
            record.deletion_due_at.is_some(),
            "case {case:?}: a deletion of it must be in flight"
        );
        record.deletion_due_at = Some(SystemTime::now() - Duration::from_secs(1));
    }
}

/// `count` tags, as a create request carries them.
fn tag_list(count: usize) -> Vec<Value> {
    (0..count)
        .map(|index| json!({"Key": format!("key{index}"), "Value": format!("value{index}")}))
        .collect()
}

/// The `EndpointId` a listing reports for the endpoint named `name`, or `None` when the
/// page carries no entry for it.
fn listed_id(reply: &Reply, name: &str) -> Option<String> {
    reply
        .body
        .get("MultiRegionEndpoints")?
        .as_array()?
        .iter()
        .find(|row| row.get("EndpointName").and_then(Value::as_str) == Some(name))
        .and_then(|row| row.get("EndpointId").and_then(Value::as_str))
        .map(str::to_string)
}

/// One created endpoint: the request that creates it, spanning what the rule generalises
/// over.
struct Case {
    case: &'static str,
    /// `EndpointName`: within the modelled `@length 1..64` and `@pattern ^[\w\-_]+$`.
    name: &'static str,
    /// The Region the create executes in, which becomes the endpoint's primary region.
    primary: &'static str,
    /// The Region the one route configuration names.
    secondary: &'static str,
    /// How many tags the create carries.
    tags: usize,
}

/// The rows span the name's length and character set (the shortest and the longest name
/// the model admits, word characters, digits, hyphens and underscores), the secondary
/// region, the tag count (none, one, and the documented maximum of
/// [`MAX_TAGS_PER_RESOURCE`]) and the Region the call executes in.
fn cases() -> Vec<Case> {
    vec![
        Case {
            case: "the shortest name the model admits, no tags",
            name: "e",
            primary: HOME,
            secondary: "us-west-2",
            tags: 0,
        },
        Case {
            case: "the longest name the model admits, one tag",
            // 64 characters: `EndpointName`'s @length maximum.
            name: "endpoint_name_of_the_full_sixty_four_characters_the_model_admits",
            primary: HOME,
            secondary: "eu-west-1",
            tags: 1,
        },
        Case {
            case: "hyphens and underscores, as many tags as a resource may carry",
            name: "global-endpoint_ID-2",
            primary: ELSEWHERE,
            secondary: HOME,
            tags: MAX_TAGS_PER_RESOURCE,
        },
        Case {
            case: "digits only, created in a third Region",
            name: "20261116",
            primary: "ap-south-1",
            secondary: "eu-central-1",
            tags: 2,
        },
    ]
}

#[tokio::test]
async fn every_created_endpoint_is_minted_its_own_identifier_that_every_read_reports() {
    let ses = Ses::new();
    // Every id this suite has seen: an id names ONE endpoint, so no two mints may share
    // one, whatever the requests that minted them looked like.
    let mut minted: BTreeSet<String> = BTreeSet::new();

    for row in cases() {
        let case = row.case;
        let id = ses
            .mint(case, row.primary, row.name, row.secondary, row.tags)
            .await;
        assert!(
            minted.insert(id.clone()),
            "case {case:?}: the EndpointId {id:?} was already minted for another endpoint"
        );

        // The read while the endpoint is still being provisioned reports the id the
        // create answered with...
        let creating = ses.get(row.primary, row.name).await;
        assert_eq!(
            creating.status,
            200,
            "case {case:?}: the endpoint must read back, got {} {} {:?}",
            creating.status,
            creating.error_code(),
            creating.body
        );
        assert_eq!(
            creating.text("EndpointId"),
            id,
            "case {case:?}: the read must report the minted id, got {:?}",
            creating.body
        );

        // ...and so does the read once the provisioning transition the clock owes has
        // fallen due: the status moves, the identifier does not.
        ses.settle_provisioning(case, row.primary, row.name);
        let ready = ses.get(row.primary, row.name).await;
        assert_eq!(
            ready.text("Status"),
            "READY",
            "case {case:?}: the settled endpoint must read as READY, got {:?}",
            ready.body
        );
        assert_eq!(
            ready.text("EndpointId"),
            id,
            "case {case:?}: the settled read must report the same id, got {:?}",
            ready.body
        );

        // The listing of that Region reports the same id for the same endpoint.
        let listing = ses.list(row.primary).await;
        assert_eq!(
            listing.status,
            200,
            "case {case:?}: the listing must succeed, got {} {} {:?}",
            listing.status,
            listing.error_code(),
            listing.body
        );
        assert_eq!(
            listed_id(&listing, row.name).as_deref(),
            Some(id.as_str()),
            "case {case:?}: the listing must report the minted id, got {:?}",
            listing.body
        );
    }

    assert_eq!(
        minted.len(),
        cases().len(),
        "every created endpoint must hold an id of its own, got {minted:?}"
    );
}

#[tokio::test]
async fn one_endpoint_name_is_minted_a_fresh_identifier_for_every_endpoint_it_names() {
    let ses = Ses::new();
    // The same name throughout: an id that was a function of what the request names
    // would repeat itself here, and every row below would read back the same value.
    let name = "repeated_endpoint_name";
    let mut minted: Vec<(&str, String)> = Vec::new();

    // The same name in two primary regions: two endpoints, each with its own id.
    for region in [HOME, ELSEWHERE] {
        let case = region;
        let id = ses.mint(case, region, name, "us-west-2", 0).await;
        assert!(
            minted.iter().all(|(_, held)| held != &id),
            "case {case:?}: the EndpointId {id:?} already names another endpoint ({minted:?})"
        );
        minted.push((case, id));
    }

    // And the name minted again, after the deletion of the first endpoint has run to
    // completion, is a new endpoint: it gets an id of its own rather than the one the
    // deleted endpoint held.
    let deleted = ses.delete(HOME, name).await;
    assert_eq!(
        deleted.status,
        200,
        "the delete must be accepted, got {} {} {:?}",
        deleted.status,
        deleted.error_code(),
        deleted.body
    );
    ses.complete_deletion("after the delete", HOME, name);
    // The tear-down is done, so the name is free again.
    let absent = ses.get(HOME, name).await;
    assert_eq!(
        absent.status, 404,
        "the deleted endpoint must be gone, got {} {:?}",
        absent.status, absent.body
    );

    let case = "the name minted again after its endpoint was deleted";
    let reminted = ses.mint(case, HOME, name, "us-west-2", 0).await;
    assert!(
        minted.iter().all(|(_, held)| held != &reminted),
        "case {case:?}: expected an id of its own, got {reminted:?}, already seen in {minted:?}"
    );
    let read = ses.get(HOME, name).await;
    assert_eq!(
        read.text("EndpointId"),
        reminted,
        "case {case:?}: the read must report the id the re-create minted, got {:?}",
        read.body
    );

    // The endpoint in the other Region is untouched by any of it: its own id still names
    // it.
    let elsewhere_id = minted
        .iter()
        .find(|(case, _)| *case == ELSEWHERE)
        .map(|(_, id)| id.clone())
        .expect("the endpoint created in the other Region must have been minted an id");
    let elsewhere = ses.get(ELSEWHERE, name).await;
    assert_eq!(
        elsewhere.text("EndpointId"),
        elsewhere_id,
        "the endpoint in the other Region must still report its own id, got {:?}",
        elsewhere.body
    );
}
