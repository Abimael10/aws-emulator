// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the STAGE RESPONSE CACHE of the `execute-api` data plane: what a
//! deployed stage answers for a method whose settings enable caching.
//!
//! Every case drives the real wire path — an HTTP request through the multi-service router
//! into the mounted service, on the api's own
//! `{rest-api-id}.execute-api.{region}.amazonaws.com` host — so what is asserted is what a
//! client of the published invoke URL sees. The control plane is driven through the same
//! emulator ([`Gateway`]) to build the records the data plane then resolves, so nothing here
//! reaches into state.
//!
//! The rules under test, each over a TABLE of inputs rather than one recorded call:
//!
//! * a method whose settings enable caching on a stage whose cluster is up is answered from
//!   that cluster — the response a previous request was served, served again — until the
//!   entry stops being one the cluster holds, over a table of what ends it: the
//!   `cacheTtlInSeconds` passing, a `FlushStageCache`, and a cache-control ask the stage
//!   honours;
//! * a method the settings do NOT cache is answered by its integration every time, over a
//!   table of the reasons they do not: no settings at all, caching turned off, a TTL of
//!   zero, a stage with no cache cluster, and settings that name a different method;
//! * a `Cache-Control: max-age=0` ask that needs the `execute-api:InvalidateCache` grant and
//!   has none is answered as the stage's `unauthorizedCacheControlHeaderStrategy` says, over
//!   a table spanning each of the three modelled strategies, a stage that requires no
//!   authorization at all, an api whose resource policy grants the action, and each half of
//!   the refusal the failing strategy takes — the missing-token refusal for a caller that
//!   presented no credential, and the IAM-shaped one for a caller that presented one;
//! * the integration's `cacheNamespace` decides which resources share one entry, over a
//!   table of two resources grouped under one namespace and two carrying the per-resource
//!   default;
//! * the `methodSettings` key that enables caching is honoured in each spelling that names
//!   the invoked method — its own path escaped, its own path with a literal separator, and
//!   the whole-stage wildcard;
//! * and the first rule again over a method whose integration is an `AWS` SERVICE integration
//!   rather than a `MOCK`: what the cluster holds is the INTEGRATION's own answer, so the
//!   entry, the span it is usable for and what evicts it are the same whatever produced that
//!   answer — over a table of what ends it, the `cacheTtlInSeconds` each row's setting
//!   carries, and the depth of the resource path its `methodSettings` key names.
//!
//! What makes a cache hit OBSERVABLE: every `MOCK` case serves a method whose own ANSWER —
//! its rendered integration request, which is what a `MOCK` returns without invoking a
//! backend, and so what the cluster holds — is then REWRITTEN through `UpdateIntegration` and
//! redeployed, while the integration response maps that answer through one template that
//! renders it ([`ECHO_TEMPLATE`]). A request the integration answers carries the rewritten
//! answer; one answered from the cluster carries the answer the first request was served. So
//! the two are told apart by what the client reads, not by anything the test knows about the
//! store — and not by rewriting the response TEMPLATE, which is the MAPPING and is applied
//! afresh to every request the entry answers (the `stage_cache_hit_mapping` suite pins that).
//! The service-integration rows tell the two apart the same way without rewriting anything of
//! the api: the TABLE their integration reads is moved on out of band through the service that
//! owns it, so a request the integration answers reports the item that service now holds and
//! one the cluster answers reports what it answered when the entry was filled.

use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_apigateway_backplanecontrolservice::state::CACHE_CLUSTER_AVAILABLE;

use crate::dynamodb_service_integration::{dynamodb, given_role, given_table};
use crate::execute_api_data_plane::{
    deploy, integration_response, invoke, json_template, method_response, put_integration,
    put_method,
};
use crate::flush_stage_cache::{cluster_status, flush_cache};
use crate::rest_api_lifecycle::{
    create_api, gateway_with_dynamodb_and_iam, resource_under, string_member, Answer, Gateway,
};
use crate::stage_lifecycle::get_stage;

/// How long a poll for a transition the clock owes keeps looking, and how often it looks.
/// Generous: nothing here is timed, and the deadline is only what stops a test whose
/// condition will never hold.
const WAIT: Duration = Duration::from_secs(30);
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// The stage every case deploys to.
const STAGE: &str = "s";

/// The resource the cached method sits on.
const RESOURCE: &str = "items";

/// The region the control plane is called in, which is the Region the api's own ARNs name —
/// the one a refusal of an unauthorized cache invalidation reports.
const REGION: &str = "us-east-1";

/// The account every emulator-minted ARN names.
const ACCOUNT: &str = "000000000000";

/// The serial the method's integration answers while the cluster is being filled, and the
/// body that answer maps to through [`ECHO_TEMPLATE`].
const FIRST: &str = "first";
const FIRST_BODY: &str = "{\"serial\":\"first\"}";

/// The serial it answers once the integration's own answer has been rewritten: what a
/// request the INTEGRATION answers reads, and what a request answered from the cluster must
/// not.
const SECOND: &str = "second";
const SECOND_BODY: &str = "{\"serial\":\"second\"}";

/// The response template every method of this suite carries: it renders the serial the
/// INTEGRATION answered, so what a client reads says which answer was mapped — the one the
/// cluster holds or a fresh one.
const ECHO_TEMPLATE: &str = "{\"serial\":\"$input.path('$.serial')\"}";

/// The `requestTemplates` of a `MOCK` whose answer carries `serial`: a `MOCK` integration is
/// "for testing without actually invoking the backend" (`IntegrationType`), so its rendered
/// integration REQUEST is what it answers, and the `statusCode` that rendering names selects
/// the integration response that maps it.
fn answers(serial: &str) -> Value {
    json!({ "application/json": format!("{{\"statusCode\":200,\"serial\":\"{serial}\"}}") })
}

/// One `GET` resource under `parent`, on a `MOCK` integration whose answer carries `serial`
/// and whose integration response maps that answer through [`ECHO_TEMPLATE`].
///
/// `namespace` is the integration's `cacheNamespace` where the row groups resources under
/// one, and `None` leaves it at the per-resource default `PutIntegration` writes.
async fn echo_resource(
    gateway: &Gateway,
    api: &str,
    parent: &str,
    path_part: &str,
    serial: &str,
    namespace: Option<&str>,
) -> String {
    let resource = resource_under(gateway, api, parent, path_part).await;
    put_method(
        gateway,
        api,
        &resource,
        "GET",
        json!({ "authorizationType": "NONE" }),
    )
    .await;
    let mut integration = json!({ "type": "MOCK", "requestTemplates": answers(serial) });
    if let Some(namespace) = namespace {
        integration["cacheNamespace"] = json!(namespace);
    }
    put_integration(gateway, api, &resource, "GET", integration).await;
    method_response(gateway, api, &resource, "GET", "200", json!({})).await;
    integration_response(
        gateway,
        api,
        &resource,
        "GET",
        "200",
        json!({ "responseTemplates": json_template(ECHO_TEMPLATE) }),
    )
    .await;
    resource
}

/// Look at `api`'s `stage` until its cache cluster reports `AVAILABLE`, which is what "a
/// cache cluster must be enabled on the stage for responses to be cached"
/// (`MethodSetting$cachingEnabled`) needs of it.
///
/// A cluster coming up, and its return from a flush, are transitions the CLOCK owes, so this
/// waits for the CONDITION by looking again rather than for a span to pass.
///
/// `pub(crate)` for the sibling `stage_cache_hit_mapping` suite, which needs the same settled
/// cluster before it can fill one.
pub(crate) async fn settled_cluster(gateway: &Gateway, api: &str, stage: &str, case: &str) {
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        let read = get_stage(gateway, api, stage).await;
        let status = cluster_status(&read.body, case);
        if status == CACHE_CLUSTER_AVAILABLE {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case}: expected the stage's cache cluster to be up, last saw {status}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The header a stage states an ignored cache-control header in.
const WARNING_HEADER: &str = "warning";

/// The invalidation ask: the header API Gateway documents for reloading a cached entry.
const INVALIDATION_ASK: (&str, &str) = ("cache-control", "max-age=0");

/// A SigV4 credential scope naming this endpoint, as a signed request carries one: what
/// tells a caller that PRESENTED a credential from one that presented none. The signature
/// itself is not verified here (the harness runs with request authentication off), so only
/// its presence is what a row varies.
const SIGNED_CREDENTIAL: &str =
    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/us-east-1/execute-api/aws4_request, \
     SignedHeaders=host, Signature=0123456789abcdef";

/// One deployed api carrying a cached `GET /items`, with the stage's cache cluster settled.
struct Deployed {
    /// The api id.
    api: String,
    /// The api's root resource, for standing a second resource up under it.
    root: String,
    /// The resource the method sits on, for rewriting its integration response.
    resource: String,
}

impl Deployed {
    /// Stand up `GET /items` on a `MOCK` integration answering [`FIRST`], deployed to
    /// [`STAGE`] with `cluster` deciding whether the stage has a cache cluster at all.
    ///
    /// A stage that asked for one is waited out until it reports `AVAILABLE`, because "a
    /// cache cluster must be enabled on the stage for responses to be cached": a cluster
    /// that is still coming up caches nothing, which is a case of its own below.
    async fn stand_up(gateway: &Gateway, name: &str, cluster: bool) -> Self {
        let (api, root) = create_api(gateway, name).await;
        let resource = echo_resource(gateway, &api, &root, RESOURCE, FIRST, None).await;
        let mut request = json!({ "stageName": STAGE });
        if cluster {
            request["cacheClusterEnabled"] = json!(true);
            request["cacheClusterSize"] = json!("0.5");
        }
        let deployed = gateway
            .call("POST", &format!("/restapis/{api}/deployments"), Some(request))
            .await;
        assert_eq!(
            deployed.status, 201,
            "case CreateDeployment({name}): expected 201, got {} with {:?}",
            deployed.status, deployed.body
        );
        if cluster {
            settled_cluster(gateway, &api, STAGE, name).await;
        }
        Self {
            api,
            root,
            resource,
        }
    }

    /// Rewrite what the method's integration ANSWERS to `serial` AND REDEPLOY, so a request
    /// the integration answers reads it and one answered from the cluster does not.
    ///
    /// The answer is the `MOCK`'s own rendered integration request, which is the unit the
    /// cluster holds — the integration RESPONSE template is the mapping, and rewriting that
    /// would change what every request reads, held entry or not.
    ///
    /// The redeploy is what makes the edit the stage's: a stage serves the deployment its
    /// `deploymentId` names, so an edit to a method takes effect at the stage only once the
    /// api is deployed again (`crate::execute_api_data_plane`'s own snapshot rows pin that).
    /// It does not empty the cluster — `FlushStageCache` is the call that does — so what the
    /// stage HOLDS is what the rows below are about.
    async fn rewrite_answer(&self, gateway: &Gateway, case: &str, serial: &str) {
        let patched = gateway
            .call(
                "PATCH",
                &format!(
                    "/restapis/{api}/resources/{resource}/methods/GET/integration",
                    api = self.api,
                    resource = self.resource
                ),
                Some(json!({
                    "patchOperations": [{
                        "op": "replace",
                        "path": "/requestTemplates/application~1json",
                        "value": answers(serial)["application/json"]
                    }]
                })),
            )
            .await;
        assert_eq!(
            patched.status, 200,
            "case {case}: expected 200 from UpdateIntegration, got {} with {:?}",
            patched.status, patched.body
        );
        deploy(gateway, &self.api, STAGE).await;
    }

    /// `UpdateStage` with `operations`, asserting the modelled 200.
    async fn update_stage(&self, gateway: &Gateway, case: &str, operations: Value) -> Answer {
        patch_stage(gateway, &self.api, case, operations).await
    }

    /// `UpdateRestApi` attaching `document` as the api's resource policy, then redeploying
    /// so the stage serves the api as it now stands.
    async fn set_policy(&self, gateway: &Gateway, case: &str, document: &Value) {
        let patched = gateway
            .call(
                "PATCH",
                &format!("/restapis/{api}", api = self.api),
                Some(json!({
                    "patchOperations": [{
                        "op": "replace",
                        "path": "/policy",
                        "value": document.to_string()
                    }]
                })),
            )
            .await;
        assert_eq!(
            patched.status, 200,
            "case {case}: expected 200 from UpdateRestApi, got {} with {:?}",
            patched.status, patched.body
        );
        assert!(
            !string_member(&patched.body, "policy", case).is_empty(),
            "case {case}: expected the api to report the policy that was attached"
        );
    }

    /// The method ARN of the deployed `GET /items`, as a refusal of an unauthorized cache
    /// invalidation reports it.
    fn method_arn(&self) -> String {
        format!(
            "arn:aws:execute-api:{REGION}:{ACCOUNT}:{api}/{STAGE}/GET/{RESOURCE}",
            api = self.api
        )
    }

    /// Invoke `GET /items` on the stage, carrying `headers`.
    async fn invoke(
        &self,
        gateway: &Gateway,
        headers: &[(&str, &str)],
    ) -> crate::execute_api_data_plane::Invoked {
        invoke(
            gateway,
            &self.api,
            "GET",
            &format!("/{STAGE}/{RESOURCE}"),
            headers,
        )
        .await
    }

    /// Invoke `GET /items` on the stage, carrying `headers`, until the body it answers is
    /// `expected` — or the deadline passes with it never being; see [`invoked_until_body`].
    async fn invoked_until(
        &self,
        gateway: &Gateway,
        case: &str,
        headers: &[(&str, &str)],
        expected: &str,
    ) -> crate::execute_api_data_plane::Invoked {
        invoked_until_body(
            gateway,
            &self.api,
            &format!("/{STAGE}/{RESOURCE}"),
            case,
            headers,
            expected,
        )
        .await
    }

    /// One more `GET` resource under the root, on a `MOCK` integration answering `serial` and
    /// carrying `namespace` as its `cacheNamespace`; deployed, so the stage serves it.
    async fn second_resource(&self, gateway: &Gateway, serial: &str, namespace: Option<&str>) {
        echo_resource(
            gateway,
            &self.api,
            &self.root,
            OTHER_RESOURCE,
            serial,
            namespace,
        )
        .await;
        deploy(gateway, &self.api, STAGE).await;
    }

    /// `UpdateIntegration` setting the FIRST resource's `cacheNamespace`, so both resources
    /// name the group the row put them in.
    async fn set_namespace(&self, gateway: &Gateway, case: &str, namespace: &str) {
        let patched = gateway
            .call(
                "PATCH",
                &format!(
                    "/restapis/{api}/resources/{resource}/methods/GET/integration",
                    api = self.api,
                    resource = self.resource
                ),
                Some(json!({
                    "patchOperations": [{
                        "op": "replace",
                        "path": "/cacheNamespace",
                        "value": namespace
                    }]
                })),
            )
            .await;
        assert_eq!(
            patched.status, 200,
            "case {case}: expected 200 from UpdateIntegration, got {} with {:?}",
            patched.status, patched.body
        );
        // Deployed, for the reason [`Self::rewrite_answer`] gives: the group an edit put
        // the resource in is the stage's once the api is deployed again.
        deploy(gateway, &self.api, STAGE).await;
    }
}

/// `UpdateStage` on `api`'s [`STAGE`] with `operations`, asserting the modelled 200.
///
/// The one place this suite patches a stage: [`Deployed::update_stage`] and the
/// service-integration rows, which stand their api up without a [`Deployed`], both go through
/// it.
async fn patch_stage(gateway: &Gateway, api: &str, case: &str, operations: Value) -> Answer {
    let updated = gateway
        .call(
            "PATCH",
            &format!("/restapis/{api}/stages/{STAGE}"),
            Some(json!({ "patchOperations": operations })),
        )
        .await;
    assert_eq!(
        updated.status, 200,
        "case {case}: expected 200 from UpdateStage, got {} with {:?}",
        updated.status, updated.body
    );
    updated
}

/// Invoke `path` on `api`'s stage, carrying `headers`, until the body it answers is `expected`
/// — or the deadline passes with it never being.
///
/// For a row whose held entry ends by its own `cacheTtlInSeconds` PASSING: that is a transition
/// the clock owes, so the row looks again for the condition rather than sleeping a span out.
async fn invoked_until_body(
    gateway: &Gateway,
    api: &str,
    path: &str,
    case: &str,
    headers: &[(&str, &str)],
    expected: &str,
) -> crate::execute_api_data_plane::Invoked {
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        let answered = invoke(gateway, api, "GET", path, headers).await;
        if answered.body == expected {
            return answered;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case}: expected {expected} once the entry ended, last read {:?}",
            answered.body
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The ops that turn caching on for the method path `method_path` with `ttl` seconds to
/// live.
fn caching_on_at(method_path: &str, ttl: i32) -> Vec<Value> {
    vec![
        json!({
            "op": "replace",
            "path": format!("{method_path}/caching/enabled"),
            "value": "true"
        }),
        json!({
            "op": "replace",
            "path": format!("{method_path}/caching/ttlInSeconds"),
            "value": ttl.to_string()
        }),
    ]
}

/// The ops that turn caching on for every method of the stage; see [`caching_on_at`].
fn caching_on(ttl: i32) -> Vec<Value> {
    caching_on_at("/*/*", ttl)
}

/// One op setting `caching/requireAuthorizationForCacheControl` on the method path
/// `method_path`.
///
/// It goes under the SAME key the caching itself was turned on under: the settings in force on
/// a method are the entry that names it, else the whole-stage entry — one entry, not a merge of
/// both — so a stage that enables caching under a method's own key says everything about that
/// method's caching under that key.
fn require_authorization_at(method_path: &str, required: bool) -> Value {
    json!({
        "op": "replace",
        "path": format!("{method_path}/caching/requireAuthorizationForCacheControl"),
        "value": required.to_string()
    })
}

/// One op setting `/*/*/caching/requireAuthorizationForCacheControl`; see
/// [`require_authorization_at`].
fn require_authorization(required: bool) -> Value {
    require_authorization_at("/*/*", required)
}

/// One op setting `/*/*/caching/unauthorizedCacheControlHeaderStrategy`.
fn strategy(value: &str) -> Value {
    json!({
        "op": "replace",
        "path": "/*/*/caching/unauthorizedCacheControlHeaderStrategy",
        "value": value
    })
}

/// A resource policy document carrying one statement.
fn policy_allowing(action: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": "*",
            "Action": action,
            "Resource": "execute-api:/*"
        }]
    })
}

/// What ends the entry the cluster is holding, which is what the rows of the first table
/// differ along.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ends {
    /// Nothing: the entry is still held, so the cached body is served again.
    Nothing,
    /// The entry's own TTL passes.
    Ttl,
    /// `FlushStageCache` empties the cluster.
    Flush,
    /// The request asks for it to be reloaded, on a stage that requires no authorization
    /// for the ask.
    Ask,
}

#[tokio::test]
async fn the_methods_own_settings_key_caches_it_as_the_whole_stage_key_does() {
    // Rows differ along the `methodSettings` key the caching is turned on under: the
    // method's own path in the escaped spelling `PatchOperation$path` documents, the same
    // path with its leading separator left to the pointer's own — the spelling
    // `MethodSetting$ResourcePath` states a path in, and so the key a
    // CloudFormation-managed stage carries — and the whole-stage wildcard. All three name
    // the invoked method, so all three cache it, which is what tells a wrong key
    // construction from the wildcard fallback answering for it. A fourth spelling, a
    // resource path whose INNER separator is written literally, names no method at all and
    // is refused by `UpdateStage`, so it stores no key for this lookup to resolve;
    // `method_setting_resource_half_spellings` pins that refusal.
    let rows: &[(&str, &str)] = &[
        ("the method's own key, escaped", "/~1items/GET"),
        ("the method's own key, unprefixed", "/items/GET"),
        ("the whole-stage key", "/*/*"),
    ];

    for (index, (case, method_path)) in rows.iter().enumerate() {
        let gateway = Gateway::new();
        let deployed = Deployed::stand_up(&gateway, &format!("settings-key-{index}"), true).await;
        deployed
            .update_stage(
                &gateway,
                case,
                Value::Array(caching_on_at(method_path, 300)),
            )
            .await;

        let filled = deployed.invoke(&gateway, &[]).await;
        assert_eq!(
            filled.body, FIRST_BODY,
            "case {case}: expected the integration's own body first, got {:?}",
            filled.body
        );
        deployed.rewrite_answer(&gateway, case, SECOND).await;
        let cached = deployed.invoke(&gateway, &[]).await;
        assert_eq!(
            cached.body, FIRST_BODY,
            "case {case}: expected the entry these settings had the stage hold, got {:?}",
            cached.body
        );
    }
}

#[tokio::test]
async fn a_cached_method_is_answered_from_the_cluster_until_the_entry_it_holds_is_gone() {
    // Rows differ along what ENDS the held entry — the TTL passing, a flush, an honoured
    // ask — plus the row where nothing does, which is what makes the other three mean
    // anything: in all four the integration's own answer has been rewritten, so the body
    // says which side answered.
    let rows: &[(&str, Ends, &str)] = &[
        ("nothing ends the entry", Ends::Nothing, FIRST_BODY),
        ("the entry's TTL passes", Ends::Ttl, SECOND_BODY),
        ("a flush empties the cluster", Ends::Flush, SECOND_BODY),
        ("the caller asks for a reload", Ends::Ask, SECOND_BODY),
    ];

    for (case, ends, expected) in rows {
        let gateway = Gateway::new();
        let deployed = Deployed::stand_up(&gateway, &format!("cache-{ends:?}"), true).await;
        // A one-second TTL is the shortest a method setting can carry (the member is in
        // whole seconds), so the row that waits one out waits the least it can.
        let ttl = if *ends == Ends::Ttl { 1 } else { 300 };
        let mut operations = caching_on(ttl);
        operations.push(require_authorization(*ends != Ends::Ask));
        deployed
            .update_stage(&gateway, case, Value::Array(operations))
            .await;

        // The first request is a MISS: the integration answers it, and what it answered is
        // what the cluster now holds.
        let first = deployed.invoke(&gateway, &[]).await;
        assert_eq!(
            first.status, 200,
            "case {case}: expected 200 from the first invoke, got {} with {:?}",
            first.status, first.body
        );
        assert_eq!(
            first.body, FIRST_BODY,
            "case {case}: expected the integration's own body on a cache miss, got {:?}",
            first.body
        );

        // From here the integration answers differently, so a cached entry is visible.
        deployed.rewrite_answer(&gateway, case, SECOND).await;
        let cached = deployed.invoke(&gateway, &[]).await;
        assert_eq!(
            cached.body, FIRST_BODY,
            "case {case}: expected the held entry to be served while it is held, got {:?}",
            cached.body
        );

        match ends {
            Ends::Nothing | Ends::Ttl | Ends::Ask => {}
            Ends::Flush => {
                let flushed = flush_cache(&gateway, &deployed.api, STAGE).await;
                assert_eq!(
                    flushed.status, 202,
                    "case {case}: expected 202 from FlushStageCache, got {} with {:?}",
                    flushed.status, flushed.body
                );
                // A cluster that is being emptied is not one that is up, so it caches
                // nothing while the flush is outstanding: the row looks until the cluster is
                // settled again, so what the next request reads is the EMPTIED cluster
                // rather than one that is mid-flush.
                settled_cluster(&gateway, &deployed.api, STAGE, case).await;
            }
        }
        let headers: &[(&str, &str)] = if *ends == Ends::Ask {
            &[INVALIDATION_ASK]
        } else {
            &[]
        };
        // The row whose entry ends by its own TTL PASSING waits for that condition by
        // looking again until the entry is no longer the one served: the expiry is a
        // transition the clock owes, and an invoke that finds the entry gone is answered by
        // the integration and fills the cluster with the same answer, so looking again
        // cannot change what the assertions below read.
        let after = match ends {
            Ends::Ttl => deployed.invoked_until(&gateway, case, headers, expected).await,
            _ => deployed.invoke(&gateway, headers).await,
        };
        assert_eq!(
            after.status, 200,
            "case {case}: expected 200 after the entry ended, got {} with {:?}",
            after.status, after.body
        );
        assert_eq!(
            &after.body, expected,
            "case {case}: expected {expected} after the entry ended, got {:?}",
            after.body
        );
        assert!(
            after.header(WARNING_HEADER).is_none(),
            "case {case}: nothing was ignored, so no warning is stated, got {:?}",
            after.header(WARNING_HEADER)
        );

        // And the response that followed is cached in its turn: the entry a request filled
        // is the entry the next request reads, whichever request filled it.
        deployed.rewrite_answer(&gateway, case, FIRST).await;
        let again = deployed.invoke(&gateway, &[]).await;
        assert_eq!(
            &again.body, expected,
            "case {case}: expected the entry the last request filled, got {:?}",
            again.body
        );
    }
}

#[tokio::test]
async fn a_method_the_settings_do_not_cache_is_answered_by_its_integration_every_time() {
    // Rows differ along the reason the stage caches nothing for this method: the settings
    // are absent, they turn caching off, they allow no time to live, the stage has no
    // cluster for them to use, and they name a different method than the one invoked.
    let rows: &[(&str, bool, Vec<Value>)] = &[
        ("no method settings at all", true, Vec::new()),
        (
            "caching turned off",
            true,
            vec![json!({ "op": "replace", "path": "/*/*/caching/enabled", "value": "false" })],
        ),
        ("a time to live of zero", true, caching_on(0)),
        ("a stage with no cache cluster", false, caching_on(300)),
        (
            "settings naming another method",
            true,
            vec![
                json!({ "op": "replace", "path": "/~1other/GET/caching/enabled", "value": "true" }),
                json!({
                    "op": "replace",
                    "path": "/~1other/GET/caching/ttlInSeconds",
                    "value": "300"
                }),
            ],
        ),
    ];

    for (index, (case, cluster, operations)) in rows.iter().enumerate() {
        let gateway = Gateway::new();
        let deployed =
            Deployed::stand_up(&gateway, &format!("uncached-{index}"), *cluster).await;
        if !operations.is_empty() {
            deployed
                .update_stage(&gateway, case, Value::Array(operations.clone()))
                .await;
        }
        let first = deployed.invoke(&gateway, &[]).await;
        assert_eq!(
            first.body, FIRST_BODY,
            "case {case}: expected the integration's own body, got {:?}",
            first.body
        );
        deployed.rewrite_answer(&gateway, case, SECOND).await;
        let second = deployed.invoke(&gateway, &[]).await;
        assert_eq!(
            second.body, SECOND_BODY,
            "case {case}: expected the integration to answer again, got {:?}",
            second.body
        );
    }
}

/// The serial the second resource's own integration answers, and the body it maps to: what a
/// shared namespace must hide and a default namespace must not.
const OTHER: &str = "other";
const OTHER_BODY: &str = "{\"serial\":\"other\"}";
const OTHER_RESOURCE: &str = "others";

#[tokio::test]
async fn the_cache_namespace_decides_which_resources_share_an_entry() {
    // Rows differ along the namespace the two resources' integrations carry: one the
    // developer grouped them under, and the per-resource default `PutIntegration` writes.
    // The grouped pair shares the entry the first request filled — "you can specify the
    // same cacheNamespace across resources to return the same cached data for requests to
    // different resources" — and the ungrouped pair does not.
    let rows: &[(&str, Option<&str>, &str)] = &[
        ("one namespace across both resources", Some("grouped"), FIRST_BODY),
        ("each resource's own default namespace", None, OTHER_BODY),
    ];

    for (index, (case, namespace, expected)) in rows.iter().enumerate() {
        let gateway = Gateway::new();
        let deployed = Deployed::stand_up(&gateway, &format!("namespace-{index}"), true).await;
        // The second resource answers a body of its own, so what the client reads says
        // which resource's integration produced it — or that neither did, because the
        // cluster answered.
        deployed
            .second_resource(&gateway, OTHER, *namespace)
            .await;
        if let Some(namespace) = namespace {
            // The FIRST resource has to be grouped too: a namespace is shared, so both
            // integrations have to name it.
            deployed.set_namespace(&gateway, case, namespace).await;
        }
        deployed
            .update_stage(&gateway, case, Value::Array(caching_on(300)))
            .await;

        let filled = deployed.invoke(&gateway, &[]).await;
        assert_eq!(
            filled.body, FIRST_BODY,
            "case {case}: expected the first resource's own body, got {:?}",
            filled.body
        );
        let other = invoke(
            &gateway,
            &deployed.api,
            "GET",
            &format!("/{STAGE}/{OTHER_RESOURCE}"),
            &[],
        )
        .await;
        assert_eq!(
            other.status, 200,
            "case {case}: expected 200 from the second resource, got {} with {:?}",
            other.status, other.body
        );
        assert_eq!(
            &other.body, expected,
            "case {case}: expected {expected} from the second resource, got {:?}",
            other.body
        );
    }
}

#[derive(Debug, Clone, Copy)]
struct Answered {
    /// The status the ask is answered with.
    status: u16,
    /// The body it is answered with, or `None` when the body is the refusal rather than one
    /// of the two integration bodies — which refusal is decided by whether the caller
    /// presented a credential at all ([`REFUSED_UNSIGNED_MESSAGE`]).
    body: Option<&'static str>,
    /// Whether the response carries the warning header.
    warned: bool,
}

/// The sentence an ask from a caller that presented NO credential is refused with: nothing
/// authenticated, so the endpoint answers the missing-token refusal rather than a decision
/// about a principal.
const REFUSED_UNSIGNED_MESSAGE: &str = "Missing Authentication Token";

/// What the refusal of an ask from a caller that DID present a credential names: the action
/// the api's policy decided against, in the sentence AWS refuses an unauthorized call with.
const REFUSED_SIGNED_ACTION: &str = "is not authorized to perform: execute-api:InvalidateCache";

#[tokio::test]
async fn an_unauthorized_cache_control_ask_is_answered_as_the_stages_strategy_says() {
    // Rows span every modelled `unauthorizedCacheControlHeaderStrategy` value, plus the two
    // ways an ask is AUTHORIZED at all: a stage that requires no authorization for it, and
    // an api whose resource policy grants the action. Two rows are the control for that
    // grant: a policy granting only `execute-api:Invoke` authorizes no invalidation, for a
    // caller that presented a credential and for one that presented none — the two halves
    // of the refusal the failing strategy takes.
    type Row = (
        &'static str,
        bool,
        Option<&'static str>,
        Option<&'static str>,
        bool,
        Answered,
    );
    let rows: &[Row] = &[
        (
            "the strategy that fails the request",
            true,
            Some("FAIL_WITH_403"),
            None,
            false,
            Answered {
                status: 403,
                body: None,
                warned: false,
            },
        ),
        (
            "the strategy that states the header was ignored",
            true,
            Some("SUCCEED_WITH_RESPONSE_HEADER"),
            None,
            false,
            Answered {
                status: 200,
                body: Some(FIRST_BODY),
                warned: true,
            },
        ),
        (
            "the strategy that ignores it silently",
            true,
            Some("SUCCEED_WITHOUT_RESPONSE_HEADER"),
            None,
            false,
            Answered {
                status: 200,
                body: Some(FIRST_BODY),
                warned: false,
            },
        ),
        (
            "a stage requiring no authorization for the ask",
            false,
            Some("FAIL_WITH_403"),
            None,
            false,
            Answered {
                status: 200,
                body: Some(SECOND_BODY),
                warned: false,
            },
        ),
        (
            "a policy granting the invalidation",
            true,
            Some("FAIL_WITH_403"),
            Some("execute-api:InvalidateCache"),
            false,
            Answered {
                status: 200,
                body: Some(SECOND_BODY),
                warned: false,
            },
        ),
        (
            "a policy granting only the invoke",
            true,
            Some("FAIL_WITH_403"),
            Some("execute-api:Invoke"),
            false,
            Answered {
                status: 403,
                body: None,
                warned: false,
            },
        ),
        (
            "a signed caller a policy granting only the invoke does not authorize",
            true,
            Some("FAIL_WITH_403"),
            Some("execute-api:Invoke"),
            true,
            Answered {
                status: 403,
                body: None,
                warned: false,
            },
        ),
    ];

    for (index, (case, require, named_strategy, granted, signed, expected)) in
        rows.iter().enumerate()
    {
        let gateway = Gateway::new();
        let deployed = Deployed::stand_up(&gateway, &format!("ask-{index}"), true).await;
        if let Some(action) = granted {
            // A policy that grants an action must still admit the invoke itself, which an
            // api carrying a document only does where the document says so: the grant
            // under test is added to an allow of the invoke, except in the row that grants
            // the invoke alone.
            let document = if *action == "execute-api:Invoke" {
                policy_allowing(action)
            } else {
                json!({
                    "Version": "2012-10-17",
                    "Statement": [
                        policy_allowing("execute-api:Invoke")["Statement"][0].clone(),
                        policy_allowing(action)["Statement"][0].clone()
                    ]
                })
            };
            deployed.set_policy(&gateway, case, &document).await;
        }
        let mut operations = caching_on(300);
        operations.push(require_authorization(*require));
        if let Some(named) = named_strategy {
            operations.push(strategy(named));
        }
        deployed
            .update_stage(&gateway, case, Value::Array(operations))
            .await;

        // Fill the cluster, then rewrite the records, so the ask has something to reload
        // and the body says whether it was reloaded.
        let filled = deployed.invoke(&gateway, &[]).await;
        assert_eq!(
            filled.body, FIRST_BODY,
            "case {case}: expected the first invoke to be answered by the integration, got {:?}",
            filled.body
        );
        deployed.rewrite_answer(&gateway, case, SECOND).await;

        let ask: Vec<(&str, &str)> = match signed {
            true => vec![INVALIDATION_ASK, ("authorization", SIGNED_CREDENTIAL)],
            false => vec![INVALIDATION_ASK],
        };
        let answered = deployed.invoke(&gateway, &ask).await;
        assert_eq!(
            answered.status, expected.status,
            "case {case}: expected {} for the ask, got {} with {:?}",
            expected.status, answered.status, answered.body
        );
        match expected.body {
            Some(body) => assert_eq!(
                &answered.body, body,
                "case {case}: expected {body} for the ask, got {:?}",
                answered.body
            ),
            None if *signed => {
                // A caller that PRESENTED a credential names a principal the api's policy
                // decided against, so the refusal is the IAM-shaped sentence naming the
                // action it asked for and the method it asked for — reported, as every
                // `execute-api` refusal reports a resource, with the account masked.
                assert!(
                    answered.body.contains(REFUSED_SIGNED_ACTION)
                        && answered.body.contains(
                            &crate::stage_invoke_identity_policy::reported_resource(
                                &deployed.method_arn()
                            )
                        ),
                    "case {case}: expected the IAM-shaped refusal sentence naming \
                     {REFUSED_SIGNED_ACTION}, got {:?}",
                    answered.body
                );
            }
            None => {
                // A caller that presented NO credential authenticated nobody, so the
                // endpoint answers the refusal it takes for an authorization it cannot take
                // at all rather than a decision about a principal.
                let parsed: Value = serde_json::from_str(&answered.body)
                    .unwrap_or_else(|_| panic!("case {case}: expected a JSON body, got {:?}", answered.body));
                assert_eq!(
                    parsed["message"].as_str(),
                    Some(REFUSED_UNSIGNED_MESSAGE),
                    "case {case}: expected the missing-token refusal, got {:?}",
                    answered.body
                );
            }
        }
        assert_eq!(
            answered.header(WARNING_HEADER).is_some(),
            expected.warned,
            "case {case}: expected warned={}, got {:?}",
            expected.warned,
            answered.header(WARNING_HEADER)
        );
    }
}

/// The table the service integration of the rows below READS, the item it reads, and the two
/// counts that item carries: the one the entry is filled from, and the one written out of band
/// afterwards — which only a request the integration ANSWERS can report.
const SERVICE_TABLE: &str = "readings";
const SERVICE_ITEM: &str = "reading-1";
const FILLED_COUNT: i64 = 1;
const WRITTEN_COUNT: i64 = 7;

/// The role each such integration is made under, holding the one grant the call it makes needs.
const SERVICE_ROLE: &str = "apigateway-cached-readings";

/// The response template those methods carry: it renders the count read off what the TABLE
/// answered, so what a client reads says which answer was mapped — the one the cluster holds,
/// or one the integration has just been performed for.
const COUNT_TEMPLATE: &str = "{\"count\":\"$input.path('$.Item.count.N')\"}";

/// The body [`COUNT_TEMPLATE`] renders for an item carrying `count`.
fn count_body(count: i64) -> String {
    format!("{{\"count\":\"{count}\"}}")
}

/// Write `count` onto the item those integrations read, through the service that OWNS the
/// table — so what the table holds is that service's own record and not something written into
/// a store by hand.
async fn put_reading(gateway: &Gateway, count: i64, case: &str) {
    let (status, answered) = dynamodb(
        gateway,
        "PutItem",
        json!({
            "TableName": SERVICE_TABLE,
            "Item": {"id": {"S": SERVICE_ITEM}, "count": {"N": count.to_string()}},
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case}: expected 200 from PutItem({count}), got {status} with {answered:?}"
    );
}

/// Stand one row's api up: a `GET` on the resource the path `segments` name, over an `AWS`
/// integration that reads [`SERVICE_ITEM`] out of [`SERVICE_TABLE`] under `role`, deployed to
/// [`STAGE`] with a cache cluster waited out until it is up. Answers the api id.
async fn given_cached_service_method(
    gateway: &Gateway,
    name: &str,
    segments: &[&str],
    role: &str,
) -> String {
    let (api, root) = create_api(gateway, name).await;
    let mut resource = root;
    for segment in segments {
        resource = resource_under(gateway, &api, &resource, segment).await;
    }
    put_method(
        gateway,
        &api,
        &resource,
        "GET",
        json!({ "authorizationType": "NONE" }),
    )
    .await;
    put_integration(
        gateway,
        &api,
        &resource,
        "GET",
        json!({
            "type": "AWS",
            "httpMethod": "POST",
            "uri": format!("arn:aws:apigateway:{REGION}:dynamodb:action/GetItem"),
            "credentials": role,
            "requestTemplates": json_template(&format!(
                "{{\"TableName\":\"{SERVICE_TABLE}\",\
                 \"Key\":{{\"id\":{{\"S\":\"{SERVICE_ITEM}\"}}}}}}"
            )),
        }),
    )
    .await;
    method_response(gateway, &api, &resource, "GET", "200", json!({})).await;
    integration_response(
        gateway,
        &api,
        &resource,
        "GET",
        "200",
        json!({ "responseTemplates": json_template(COUNT_TEMPLATE) }),
    )
    .await;
    let deployed = gateway
        .call(
            "POST",
            &format!("/restapis/{api}/deployments"),
            Some(json!({
                "stageName": STAGE,
                "cacheClusterEnabled": true,
                "cacheClusterSize": "0.5"
            })),
        )
        .await;
    assert_eq!(
        deployed.status, 201,
        "case CreateDeployment({name}): expected 201, got {} with {:?}",
        deployed.status, deployed.body
    );
    settled_cluster(gateway, &api, STAGE, name).await;
    api
}

#[tokio::test]
async fn a_cached_service_integration_is_performed_again_once_the_entry_it_filled_is_gone() {
    // The rule the first table pins is about the ENTRY and not about what filled it: what a
    // cluster holds is the integration's own ANSWER. So these rows run it over a method whose
    // integration is an `AWS` service integration — the entry holds what another service
    // answered to a call this one made — and differ along what ends the entry, the
    // `cacheTtlInSeconds` the setting carries, and the depth of the resource path the
    // `methodSettings` key names.
    let rows: &[(&str, Ends, i32, &[&str])] = &[
        ("nothing ends the entry", Ends::Nothing, 300, &["readings"]),
        ("the entry's TTL passes", Ends::Ttl, 1, &["ttl", "short"]),
        (
            "a flush empties the cluster",
            Ends::Flush,
            300,
            &["ttl", "long", "nested"],
        ),
        (
            "the caller asks for a reload",
            Ends::Ask,
            300,
            &["readings", "current"],
        ),
    ];

    for (index, (case, ends, ttl, segments)) in rows.iter().enumerate() {
        let gateway = gateway_with_dynamodb_and_iam();
        given_table(&gateway, SERVICE_TABLE).await;
        put_reading(&gateway, FILLED_COUNT, case).await;
        let role = given_role(
            &gateway,
            SERVICE_ROLE,
            json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Action": "dynamodb:GetItem",
                    "Resource": format!(
                        "arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/{SERVICE_TABLE}"
                    ),
                }],
            }),
        )
        .await;
        let api = given_cached_service_method(
            &gateway,
            &format!("cached-service-{index}"),
            segments,
            &role,
        )
        .await;
        // The method's OWN settings key, which is what makes the depth of the resource path a
        // dimension these rows span: the path as one reference token with its separators
        // escaped, the spelling `PatchOperation$path` requires.
        let method_path = format!("/~1{}/GET", segments.join("~1"));
        let mut operations = caching_on_at(&method_path, *ttl);
        operations.push(require_authorization_at(&method_path, *ends != Ends::Ask));
        patch_stage(&gateway, &api, case, Value::Array(operations)).await;

        let path = format!("/{STAGE}/{}", segments.join("/"));
        let asked: &[(&str, &str)] = if *ends == Ends::Ask {
            &[INVALIDATION_ASK]
        } else {
            &[]
        };

        // The first request is a MISS, so the integration is PERFORMED: the call reaches the
        // service that owns the table, and what that service answered is what the cluster now
        // holds.
        let filled = invoke(&gateway, &api, "GET", &path, &[]).await;
        assert_eq!(
            filled.status, 200,
            "case {case}: expected 200 from the integration on the first request, got {} with \
             {:?}",
            filled.status, filled.body
        );
        assert_eq!(
            filled.body,
            count_body(FILLED_COUNT),
            "case {case}: expected the count the table holds on a cache miss, got {:?}",
            filled.body
        );

        // The TABLE moves on, out of band through the service that owns it: from here a
        // request the cluster answers reports the count the entry holds, and one the
        // integration answers reports the count the table holds now.
        put_reading(&gateway, WRITTEN_COUNT, case).await;

        let after = match ends {
            // Nothing ends the entry, so the integration is not performed again and the
            // count the table now holds is not reported.
            Ends::Nothing => invoke(&gateway, &api, "GET", &path, asked).await,
            Ends::Flush => {
                let held = invoke(&gateway, &api, "GET", &path, asked).await;
                assert_eq!(
                    held.body,
                    count_body(FILLED_COUNT),
                    "case {case}: expected the held entry while it is held, got {:?}",
                    held.body
                );
                let flushed = flush_cache(&gateway, &api, STAGE).await;
                assert_eq!(
                    flushed.status, 202,
                    "case {case}: expected 202 from FlushStageCache, got {} with {:?}",
                    flushed.status, flushed.body
                );
                // A cluster that is being emptied is not one that is up, so the row looks
                // until it is settled again: what the next request reads is the EMPTIED
                // cluster rather than one that is mid-flush.
                settled_cluster(&gateway, &api, STAGE, case).await;
                invoke(&gateway, &api, "GET", &path, asked).await
            }
            Ends::Ask => {
                let held = invoke(&gateway, &api, "GET", &path, &[]).await;
                assert_eq!(
                    held.body,
                    count_body(FILLED_COUNT),
                    "case {case}: expected the held entry for a request that asks nothing of \
                     it, got {:?}",
                    held.body
                );
                invoke(&gateway, &api, "GET", &path, asked).await
            }
            // The expiry is a transition the clock owes, so the row looks again until the
            // entry is no longer the one served rather than sleeping its span out. Nothing
            // asserts a HIT on this row: at the shortest TTL a setting can carry, whether one
            // is still held a moment later is the host's speed — the row above whose entry
            // nothing ends is what pins the hit.
            Ends::Ttl => {
                invoked_until_body(
                    &gateway,
                    &api,
                    &path,
                    case,
                    asked,
                    &count_body(WRITTEN_COUNT),
                )
                .await
            }
        };

        let expected = match ends {
            Ends::Nothing => count_body(FILLED_COUNT),
            _ => count_body(WRITTEN_COUNT),
        };
        assert_eq!(
            after.status, 200,
            "case {case}: expected 200 once the entry ended, got {} with {:?}",
            after.status, after.body
        );
        assert_eq!(
            after.body, expected,
            "case {case}: expected {expected}, got {:?}",
            after.body
        );
    }
}
