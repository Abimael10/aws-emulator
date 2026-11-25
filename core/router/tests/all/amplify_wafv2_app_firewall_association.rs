// SPDX-License-Identifier: CC0-1.0

//! Putting WAF in front of an Amplify application, on the assembled emulator.
//!
//! The rule this suite pins, one level above the recorded case:
//!
//! > A protected resource is bound to a web ACL of the `Scope` its own KIND takes — "for an
//! > Amplify application, use `CLOUDFRONT`"
//! > (`smithy_query(query="doc",shape="CreateWebACLRequest",member="Scope")`) — so
//! > `AssociateWebACL` of a CLOUDFRONT-scoped web ACL with an app's ARN SUCCEEDS (HTTP 200,
//! > empty body) and one of a REGIONAL-scoped web ACL with the same ARN is refused with
//! > `WAFInvalidParameterException` (HTTP 400). From the accepted call on, the app itself
//! > reports that firewall: `App$wafConfiguration` carries the web ACL's ARN and a
//! > `wafStatus` that settles at `ASSOCIATION_SUCCESS`, on every operation that answers with
//! > an app, and WAF's own reader (`GetWebACLForResource`) reports the same web ACL for the
//! > app's ARN. An app no web ACL is in front of — one that was never associated, and one
//! > whose association `DisassociateWebACL` took away — reports no `wafConfiguration` at
//! > all, and neither does WAF report a web ACL for it.
//!
//! Only a cross-service suite can pin this: the web ACL and the association are WAFv2's
//! records, the app and the member that reports the firewall are Amplify's, and both
//! services are the ones the emulator ASSEMBLES (`aws_emulator_router::build_router`) on one
//! engine. Every assertion reads the outcome back through the owning service's own reader
//! over the wire, so a configuration no store holds cannot pass.
//!
//! The rows differ along what the rule generalises over: WHICH app of several carries a
//! firewall (and one that carries none, as the control), the SCOPE of the web ACL named
//! (the accepted one and the refused one), which READER reports the app (`GetApp`,
//! `ListApps`, `UpdateApp`), and whether the association is live or has been taken away.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region a CLOUDFRONT-scoped web ACL is addressed at: "API and SDKs - For all calls,
/// use the Region endpoint us-east-1" (`Scope`'s own doc). The apps are created there too,
/// so an app's ARN and the global web ACL name the same Region.
const REGION: &str = "us-east-1";

/// The message WAF's refusal carries for a web ACL whose scope does not serve the named
/// resource's kind, as recorded: the whole message is this sentence.
const RESOURCE_NOT_SUPPORTED: &str = "The resource is not supported in current region";

/// `WafConfiguration$wafStatus` once the association has been applied, out of the
/// `WafStatus` value set (`smithy_query(query="shape",shape="WafStatus")`).
const ASSOCIATION_SUCCESS: &str = "ASSOCIATION_SUCCESS";

/// `WafConfiguration$wafStatus` while it is still being applied.
const ASSOCIATING: &str = "ASSOCIATING";

/// How long the firewall is given to settle, and how long to wait between looks. Generous:
/// the suite waits for the STATUS, never for a duration, and fails only if it never settles.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);
const SETTLE_POLL: Duration = Duration::from_millis(20);

/// The emulator as it is ASSEMBLED, on one engine.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = aws_emulator_router::build_router(engine);
        // The suite exercises the services, not SigV4: every call carries a credential
        // scope so the router can name the target service, and no signature is verified.
        router.set_request_authentication(false);
        Self { router }
    }

    /// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what
    /// routes the request.
    fn authorization(signing_name: &str) -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{signing_name}/\
             aws4_request, SignedHeaders=host, Signature=0000"
        )
    }

    /// Dispatch one request as it arrives on the wire, answering its status and parsed body.
    async fn dispatch(
        &self,
        method: &str,
        host: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Bytes,
    ) -> (u16, Value) {
        let mut builder = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let response = self
            .router
            .dispatch(builder.body(body).expect("a well-formed request"))
            .await;
        let status = response.status().as_u16();
        let bytes = response.into_body();
        let parsed = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, parsed)
    }

    /// One Amplify call, answering its status and parsed body.
    async fn amplify(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let payload = body.map(|value| value.to_string()).unwrap_or_default();
        self.dispatch(
            method,
            &format!("amplify.{REGION}.amazonaws.com"),
            path,
            &[
                ("content-type", "application/json"),
                ("authorization", &Self::authorization("amplify")),
            ],
            Bytes::from(payload),
        )
        .await
    }

    /// One Amplify call that has to succeed.
    async fn amplify_ok(&self, method: &str, path: &str, body: Option<Value>) -> Value {
        let (status, parsed) = self.amplify(method, path, body).await;
        assert_eq!(
            status, 200,
            "{method} {path}: expected HTTP 200, got {status} with {parsed}"
        );
        parsed
    }

    /// One WAFv2 call, answering its status and parsed body.
    async fn waf(&self, operation: &str, body: Value) -> (u16, Value) {
        self.dispatch(
            "POST",
            &format!("wafv2.{REGION}.amazonaws.com"),
            "/",
            &[
                ("content-type", "application/x-amz-json-1.1"),
                ("authorization", &Self::authorization("wafv2")),
                ("x-amz-target", &format!("AWSWAF_20190729.{operation}")),
            ],
            Bytes::from(body.to_string()),
        )
        .await
    }

    /// One WAFv2 call that has to succeed.
    async fn waf_ok(&self, operation: &str, body: Value) -> Value {
        let (status, parsed) = self.waf(operation, body).await;
        assert_eq!(
            status, 200,
            "{operation}: expected HTTP 200, got {status} with {parsed}"
        );
        parsed
    }

    /// Create the app `name` and answer `(appId, appArn)`.
    async fn create_app(&self, name: &str) -> (String, String) {
        let created = self
            .amplify_ok("POST", "/apps", Some(json!({ "name": name })))
            .await;
        let app = &created["app"];
        (
            app["appId"]
                .as_str()
                .unwrap_or_else(|| panic!("CreateApp must answer an appId, got {created}"))
                .to_string(),
            app["appArn"]
                .as_str()
                .unwrap_or_else(|| panic!("CreateApp must answer an appArn, got {created}"))
                .to_string(),
        )
    }

    /// Create a web ACL in `scope` and answer its ARN.
    async fn create_web_acl(&self, scope: &str, name: &str) -> String {
        let created = self
            .waf_ok(
                "CreateWebACL",
                json!({
                    "Name": name,
                    "Scope": scope,
                    "DefaultAction": { "Allow": {} },
                    "VisibilityConfig": {
                        "SampledRequestsEnabled": true,
                        "CloudWatchMetricsEnabled": true,
                        "MetricName": name,
                    },
                    "Rules": [],
                }),
            )
            .await;
        created["Summary"]["ARN"]
            .as_str()
            .unwrap_or_else(|| panic!("CreateWebACL must answer the web ACL's ARN, got {created}"))
            .to_string()
    }

    /// The `wafConfiguration` the app `app_id` reports through `GetApp`, or `None` when it
    /// reports none at all.
    async fn get_app_firewall(&self, app_id: &str) -> Option<Value> {
        let read = self.amplify_ok("GET", &format!("/apps/{app_id}"), None).await;
        firewall_of(&read["app"])
    }

    /// The web ACL `GetWebACLForResource` reports for `resource_arn`, or `None` for a null
    /// web ACL.
    async fn web_acl_for_resource(&self, resource_arn: &str) -> Option<Value> {
        let reported = self
            .waf_ok(
                "GetWebACLForResource",
                json!({ "ResourceArn": resource_arn }),
            )
            .await;
        reported
            .get("WebACL")
            .filter(|web_acl| !web_acl.is_null())
            .cloned()
    }

    /// The app `app_id` as `ListApps` reports it.
    async fn listed_app(&self, app_id: &str) -> Value {
        let listed = self.amplify_ok("GET", "/apps", None).await;
        listed["apps"]
            .as_array()
            .unwrap_or_else(|| panic!("ListApps must answer an apps list, got {listed}"))
            .iter()
            .find(|app| app["appId"].as_str() == Some(app_id))
            .cloned()
            .unwrap_or_else(|| panic!("ListApps must report the app {app_id}, got {listed}"))
    }

    /// The firewall `GetApp` reports for `app_id` once its `wafStatus` has settled.
    ///
    /// Looks again until the status is no longer the one the association starts in, or the
    /// deadline passes — the suite waits for the STATUS rather than for a duration.
    async fn settled_firewall(&self, app_id: &str) -> Value {
        let deadline = Instant::now() + SETTLE_DEADLINE;
        loop {
            let firewall = self
                .get_app_firewall(app_id)
                .await
                .unwrap_or_else(|| panic!("the app {app_id} must report a wafConfiguration"));
            if firewall["wafStatus"].as_str() != Some(ASSOCIATING) {
                return firewall;
            }
            assert!(
                Instant::now() < deadline,
                "the firewall of {app_id} never settled: it still reports {firewall}"
            );
            tokio::time::sleep(SETTLE_POLL).await;
        }
    }
}

/// The `wafConfiguration` an app record carries, or `None` when the member is absent or
/// JSON `null` — the two ways a reader is told the app carries no firewall.
fn firewall_of(app: &Value) -> Option<Value> {
    app.get("wafConfiguration")
        .filter(|configuration| !configuration.is_null())
        .cloned()
}

/// A CLOUDFRONT-scoped web ACL protects an Amplify app: the association is accepted, every
/// reader of the app reports it, WAF reports it for the app's ARN, and taking it away leaves
/// the app with no firewall. A REGIONAL-scoped web ACL for the same app is refused.
#[tokio::test]
async fn a_cloudfront_web_acl_fronts_an_amplify_app_and_a_regional_one_is_refused() {
    let emulator = Emulator::new();

    // Three apps the firewall is put in front of, in turn, and one that is left alone as
    // the control: a firewall belongs to the app it was associated with and to no other.
    let mut apps: Vec<(String, String)> = Vec::new();
    for ordinal in 0..3 {
        apps.push(emulator.create_app(&format!("fronted-app-{ordinal}")).await);
    }
    let (control_id, control_arn) = emulator.create_app("unprotected-app").await;

    let global_acl = emulator
        .create_web_acl("CLOUDFRONT", "amplify-firewall")
        .await;
    let regional_acl = emulator
        .create_web_acl("REGIONAL", "regional-firewall")
        .await;

    // Before anything is associated, no app reports a firewall and WAF reports no web ACL
    // for any of them.
    for (app_id, app_arn) in apps.iter().chain(std::iter::once(&(
        control_id.clone(),
        control_arn.clone(),
    ))) {
        let firewall = emulator.get_app_firewall(app_id).await;
        assert!(
            firewall.is_none(),
            "case {app_id}: an app nothing was put in front of must report no \
             wafConfiguration, got {firewall:?}"
        );
        let reported = emulator.web_acl_for_resource(app_arn).await;
        assert!(
            reported.is_none(),
            "case {app_id}: WAF must report no web ACL for an unassociated app, \
             got {reported:?}"
        );
    }

    for (app_id, app_arn) in &apps {
        // The REGIONAL web ACL is refused for the app, and leaves it unprotected: an
        // Amplify app's firewall is a CLOUDFRONT-scoped web ACL.
        let (status, refusal) = emulator
            .waf(
                "AssociateWebACL",
                json!({ "ResourceArn": app_arn, "WebACLArn": regional_acl }),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {app_id}: a REGIONAL web ACL must be refused for an app, got {status} \
             with {refusal}"
        );
        assert!(
            refusal["__type"]
                .as_str()
                .is_some_and(|code| code.ends_with("WAFInvalidParameterException")),
            "case {app_id}: expected WAFInvalidParameterException, got {refusal}"
        );
        assert_eq!(
            refusal["message"].as_str(),
            Some(RESOURCE_NOT_SUPPORTED),
            "case {app_id}: expected WAF's unsupported-resource sentence, got {refusal}"
        );
        let after_refusal = emulator.get_app_firewall(app_id).await;
        assert!(
            after_refusal.is_none(),
            "case {app_id}: a refused association must leave the app unprotected, \
             got {after_refusal:?}"
        );

        // The CLOUDFRONT web ACL is accepted, with the empty body the operation models.
        let associated = emulator
            .waf_ok(
                "AssociateWebACL",
                json!({ "ResourceArn": app_arn, "WebACLArn": global_acl }),
            )
            .await;
        assert!(
            associated.is_null()
                || associated
                    .as_object()
                    .is_some_and(|members| members.is_empty()),
            "case {app_id}: AssociateWebACL answers a response with no members, \
             got {associated}"
        );

        // The app reports that firewall, and its status settles at ASSOCIATION_SUCCESS.
        let settled = emulator.settled_firewall(app_id).await;
        assert_eq!(
            settled["webAclArn"].as_str(),
            Some(global_acl.as_str()),
            "case {app_id}: expected the web ACL {global_acl}, got {settled}"
        );
        assert_eq!(
            settled["wafStatus"].as_str(),
            Some(ASSOCIATION_SUCCESS),
            "case {app_id}: expected the status {ASSOCIATION_SUCCESS}, got {settled}"
        );

        // Every reader of the app reports the same firewall: the listing element and the
        // record an update answers, beside the `GetApp` the status was read through.
        let listed = emulator.listed_app(app_id).await;
        assert_eq!(
            firewall_of(&listed),
            Some(settled.clone()),
            "case {app_id}: ListApps must report the firewall GetApp reports, got {listed}"
        );
        let updated = emulator
            .amplify_ok(
                "POST",
                &format!("/apps/{app_id}"),
                Some(json!({ "description": "firewalled" })),
            )
            .await;
        assert_eq!(
            firewall_of(&updated["app"]),
            Some(settled.clone()),
            "case {app_id}: UpdateApp must report the firewall GetApp reports, got {updated}"
        );

        // WAF's own reader agrees, for the app's ARN.
        let reported = emulator
            .web_acl_for_resource(app_arn)
            .await
            .unwrap_or_else(|| panic!("case {app_id}: WAF must report the app's web ACL"));
        assert_eq!(
            reported["ARN"].as_str(),
            Some(global_acl.as_str()),
            "case {app_id}: WAF must report the web ACL {global_acl}, got {reported}"
        );

        // The app that was never associated still reports nothing: one association is one
        // app's.
        let control = emulator.get_app_firewall(&control_id).await;
        assert!(
            control.is_none(),
            "case {app_id}: the unassociated app must still report no wafConfiguration, \
             got {control:?}"
        );
    }

    // Taken away again: the app reports no firewall and WAF reports no web ACL for it.
    for (app_id, app_arn) in &apps {
        emulator
            .waf_ok("DisassociateWebACL", json!({ "ResourceArn": app_arn }))
            .await;
        let firewall = emulator.get_app_firewall(app_id).await;
        assert!(
            firewall.is_none(),
            "case {app_id}: a disassociated app must report no wafConfiguration, \
             got {firewall:?}"
        );
        let reported = emulator.web_acl_for_resource(app_arn).await;
        assert!(
            reported.is_none(),
            "case {app_id}: WAF must report no web ACL for a disassociated app, \
             got {reported:?}"
        );
    }
}

/// The firewall an app reports is WAF's live record and not a copy: moving the app onto
/// another CLOUDFRONT web ACL changes what the app reports, with nothing written through
/// Amplify's own API.
#[tokio::test]
async fn the_reported_firewall_follows_wafs_own_record() {
    let emulator = Emulator::new();
    let (app_id, app_arn) = emulator.create_app("moved-app").await;

    // Two web ACLs the app is moved between, and a row for each move: the first binding,
    // the same binding again (which changes nothing), and the move onto the other web ACL.
    let first = emulator.create_web_acl("CLOUDFRONT", "first-firewall").await;
    let second = emulator
        .create_web_acl("CLOUDFRONT", "second-firewall")
        .await;

    for (case, expected) in [
        ("the first binding", &first),
        ("the same binding again", &first),
        ("moved onto another web ACL", &second),
    ] {
        emulator
            .waf_ok(
                "AssociateWebACL",
                json!({ "ResourceArn": app_arn, "WebACLArn": expected }),
            )
            .await;
        let firewall = emulator
            .get_app_firewall(&app_id)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: the app must report a wafConfiguration"));
        assert_eq!(
            firewall["webAclArn"].as_str(),
            Some(expected.as_str()),
            "case {case:?}: expected the web ACL {expected}, got {firewall}"
        );
        // Whatever the status is at this instant, it is one of the two an association
        // moves through — never a value outside the member's own vocabulary.
        let status = firewall["wafStatus"].as_str().unwrap_or_default();
        assert!(
            status == ASSOCIATING || status == ASSOCIATION_SUCCESS,
            "case {case:?}: expected {ASSOCIATING} or {ASSOCIATION_SUCCESS}, got {status:?}"
        );
        let settled = emulator.settled_firewall(&app_id).await;
        assert_eq!(
            settled["wafStatus"].as_str(),
            Some(ASSOCIATION_SUCCESS),
            "case {case:?}: expected the status to settle, got {settled}"
        );
    }
}
