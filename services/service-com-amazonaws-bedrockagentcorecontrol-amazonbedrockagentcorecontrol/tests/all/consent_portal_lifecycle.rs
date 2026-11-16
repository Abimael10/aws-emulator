// SPDX-License-Identifier: CC0-1.0

//! The CONSENT PORTAL end to end over HTTP: what a create persists, what every later read
//! reports, what an update changes, what a delete removes, and the address a portal
//! publishes.
//!
//! Each test drives the service the way the router does: an `http::Request` through
//! [`MultiServiceRouter::dispatch`], so the wire protocol (`restJson1`), the boundary
//! constraint walk and the handler are all exercised against the one shared engine.
//!
//! The rules under test, read off the Smithy model rather than off any one recorded request
//! (`smithy_query(query="operation",operation="CreateConsentPortal,GetConsentPortal,\
//! ListConsentPortals,UpdateConsentPortal,DeleteConsentPortal")` and
//! `smithy_query(query="shape",shape="ConsentPortalStatus,ConsentPortalIdpConfig,\
//! ConsentPortalSummary")`):
//!
//! * `CreateConsentPortal` mints a `consentPortalId` matching
//!   `^[a-zA-Z0-9\-_]{1,50}-[A-Za-z0-9]{10}$` and the `consentPortalArn` derived from it,
//!   stores every member the request carried, reports the portal `CREATING` with the
//!   transition to `ACTIVE` owed by the clock rather than by the call (the create is
//!   accepted with HTTP 202), and publishes `portalUrl` on the endpoint THIS emulator is
//!   served at;
//! * a name this account already holds is the modelled `ConflictException` (HTTP 409) and
//!   not a second portal ("The name must be unique within your account");
//! * `GetConsentPortal` and each `ListConsentPortals` summary report that one stored record;
//! * `UpdateConsentPortal` replaces each member the request carries and leaves every member
//!   it does not, reports the portal `UPDATING`, and is refused the modelled
//!   `ConflictException` while the portal's own asynchronous work is still under way;
//! * `DeleteConsentPortal` accepts the removal, the portal reports `DELETING`, and once the
//!   clock has made the removal due the portal is the modelled
//!   `ResourceNotFoundException` (HTTP 404) and its name is free again.
//!
//! The rows differ along what the rules generalise over: the shape and the length of the
//! name at both ends of its 1..50 `@length`, whether the portal carries a description, the
//! form the source's identifier takes (a gateway id and a gateway ARN), the number of
//! scopes the identity provider configuration asks for, whether it names an audience,
//! which members an update carries, and — for the published address — the form of the
//! endpoint this emulator answers on.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::{
    state, EmulatedService,
};

/// The Region every call here signs for, which is the Region a minted ARN carries.
const REGION: &str = "us-east-1";

/// The account every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// An `executionRoleArn` inside the member's
/// `^arn:aws(-[a-z-]+)?:iam::[0-9]{12}:role/[a-zA-Z0-9+=,.@\-_/]+$`.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/ConsentPortalRole";

/// A second role, so an update can be seen to replace the stored one.
const OTHER_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/OtherConsentPortalRole";

/// An `OAuth2CredentialProviderArn` inside the member's own pattern — the identity provider
/// the portal authenticates end users against.
const IDP_ARN: &str =
    "arn:aws:bedrock-agentcore:us-east-1:000000000000:token-vault/default/oauth2credentialprovider/corp-idp";

/// A second one, so an update can be seen to replace the stored configuration.
const OTHER_IDP_ARN: &str =
    "arn:aws:bedrock-agentcore:us-east-1:000000000000:token-vault/default/oauth2credentialprovider/partner-idp";

/// `ConsentPortalSourceType`'s only value: "Currently, we only support type
/// agentcore-gateway" (`smithy_query(query="shape",shape="ConsentPortalSourceType")`).
const SOURCE_TYPE: &str = "agentcore-gateway";

/// `ConsentPortalNameType`'s `@length` maximum, which the longest-name row sits at.
const MAX_NAME_LENGTH: usize = 50;

/// How long a row keeps looking for a clock-owed transition. The span itself is the
/// service's own; this only bounds a hang, so it is generous and nothing asserts on how long
/// the transition took.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long one poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

fn router() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::default();
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// A router whose `OperationContext::endpoint` is `base`, which is the address a published
/// `portalUrl` has to name.
fn router_served_at(base: &str) -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let (mut router, engine) = router();
    router.set_url_base(
        smithy_server_runtime::EndpointUrl::parse(base).expect("a base URL the router accepts"),
    );
    (router, engine)
}

/// The endpoint base a router built by [`router`] reports as its own.
fn unserved_base() -> &'static str {
    smithy_server_runtime::UNSERVED_URL_BASE
}

/// A SigV4 `Authorization` header for this service, which is what the router routes on.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/bedrock-agentcore/\
         aws4_request, SignedHeaders=host, Signature=abc"
    )
}

/// One answer: the HTTP status and the parsed body.
struct Answer {
    status: u16,
    body: Value,
}

async fn call(router: &MultiServiceRouter, operation: &str, body: &Value) -> Answer {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("/identities/{operation}"))
        .header("authorization", authorization())
        .header("host", "bedrock-agentcore-control.us-east-1.amazonaws.com")
        .header("content-type", "application/json")
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    Answer {
        status: response.status().as_u16(),
        body: if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        },
    }
}

/// One page a client FETCHES: the status, the content type and the document.
struct Page {
    status: u16,
    content_type: String,
    body: String,
}

/// Fetch `url` the way a browser holding a `portalUrl` does: no credential, no target
/// header, just the address the portal published.
async fn fetch(router: &MultiServiceRouter, method: &str, url: &str) -> Page {
    let (host, path) = split_url(url);
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", host)
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    Page {
        status: response.status().as_u16(),
        content_type: response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string(),
        body: String::from_utf8_lossy(response.body()).into_owned(),
    }
}

/// The authority and the path of an absolute URL, which is how a client addresses it.
fn split_url(url: &str) -> (String, String) {
    let without_scheme = url
        .split_once("://")
        .map_or(url, |(_scheme, rest)| rest);
    match without_scheme.find('/') {
        Some(at) => (
            without_scheme[..at].to_string(),
            without_scheme[at..].to_string(),
        ),
        None => (without_scheme.to_string(), "/".to_string()),
    }
}

/// The error code `restJson1` puts in the body's `__type`.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

fn text(body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The sentence this plane words a consent portal it does not hold with: the identifier the
/// request carried, which is the only variable part.
fn not_found_message(consent_portal_identifier: &str) -> String {
    format!("No consent portal found for id {consent_portal_identifier}")
}

/// The sentence this service refuses a change with while the portal's own asynchronous work
/// is still under way: the id the request carried and the status it is in.
fn status_conflict_message(consent_portal_id: &str, status: &str) -> String {
    format!(
        "Consent portal {consent_portal_id} is in {status} status and cannot be modified. \
         Retry the request once its status is ACTIVE."
    )
}

/// The sentence this control plane refuses a duplicate name with.
fn name_conflict_message(name: &str) -> String {
    format!("Consent portal with name '{name}' already exists in this account")
}

/// The address a portal's page is published at, spelled out here rather than taken from the
/// service: the reserved namespace segment every emulator-served plane is addressed under,
/// the segment naming this service's consent-portal plane, and the portal's own id.
fn published_portal_url(base: &str, consent_portal_id: &str) -> String {
    format!("{base}/_aws/bedrock-agentcore-consent-portal/{consent_portal_id}")
}

/// Make every clock-owed consent portal transition fall due, so the next request
/// materialises the settled status without waiting the span out on the wall clock. Writes
/// only the due instants the emulator itself stores.
fn settle_now(engine: &Arc<engine::InMemoryEngine>) {
    let store = engine
        .state::<state::ComAmazonawsBedrockagentcorecontrolAmazonbedrockagentcorecontrolState>();
    let mut store = store.lock();
    let past = SystemTime::now() - Duration::from_millis(1);
    for portal in store.consent_portals.values_mut() {
        portal.settles_at = past;
        if portal.removal_due_at.is_some() {
            portal.removal_due_at = Some(past);
        }
    }
}

async fn get_portal(router: &MultiServiceRouter, consent_portal_identifier: &str) -> Answer {
    call(
        router,
        "GetConsentPortal",
        &json!({"consentPortalIdentifier": consent_portal_identifier}),
    )
    .await
}

/// Look again until the portal reports `status`, which is what a caller polling the status
/// field does. The deadline only catches a portal that never settles at all.
async fn poll_until_status(
    router: &MultiServiceRouter,
    consent_portal_id: &str,
    status: &str,
    case: &str,
) -> Answer {
    let started = Instant::now();
    loop {
        let read = get_portal(router, consent_portal_id).await;
        if read.status == 200 && text(&read.body, "status") == status {
            return read;
        }
        assert!(
            started.elapsed() < POLL_DEADLINE,
            "case {case:?}: the portal never reached {status}, last answer {} ({})",
            read.status,
            read.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Look again until the portal is gone, which is what a caller polling an accepted deletion
/// does.
async fn poll_until_gone(router: &MultiServiceRouter, consent_portal_id: &str, case: &str) -> Answer {
    let started = Instant::now();
    loop {
        let read = get_portal(router, consent_portal_id).await;
        if read.status == 404 {
            return read;
        }
        assert!(
            started.elapsed() < POLL_DEADLINE,
            "case {case:?}: the portal was never removed, last answer {} ({})",
            read.status,
            read.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One row of the create table.
struct Row {
    case: &'static str,
    /// The portal's name (`ConsentPortalNameType` is `^[a-zA-Z0-9_-]{1,50}$`, 1..50).
    name: String,
    description: Option<&'static str>,
    /// The source's `identifier`, in one of the two forms its `@pattern` admits.
    source_identifier: &'static str,
    /// The scopes the identity provider configuration asks for (`AllowedScopesType`, at
    /// least one).
    scopes: Vec<&'static str>,
    audience: Option<&'static str>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a gateway id source, one scope, no description",
            name: "orders".to_string(),
            description: None,
            source_identifier: "ordersgw-abcdefghij",
            scopes: vec!["openid"],
            audience: None,
        },
        Row {
            case: "a gateway ARN source, several scopes, an audience and a description",
            name: "partner-consent_portal".to_string(),
            description: Some("the portal partners grant consent in"),
            source_identifier:
                "arn:aws:bedrock-agentcore:us-east-1:000000000000:gateway/partnergw-0123456789",
            scopes: vec!["openid", "profile", "agentcore:invoke"],
            audience: Some("https://partners.example.test"),
        },
        Row {
            case: "the shortest name the model admits",
            name: "a".to_string(),
            description: Some("x"),
            source_identifier: "shortgw-zzzzzzzzzz",
            scopes: vec!["openid"],
            audience: None,
        },
        Row {
            case: "the longest name the model admits",
            name: "L".repeat(MAX_NAME_LENGTH),
            description: None,
            source_identifier: "longgw-9876543210",
            scopes: vec!["openid"],
            audience: Some("urn:example:audience"),
        },
    ]
}

impl Row {
    fn idp_config(&self) -> Value {
        let mut config = json!({"credentialProviderArn": IDP_ARN, "scopes": self.scopes});
        if let Some(audience) = self.audience {
            config["audience"] = json!(audience);
        }
        config
    }

    fn sources(&self) -> Value {
        json!([{"identifier": self.source_identifier, "type": SOURCE_TYPE}])
    }

    /// The `CreateConsentPortal` body this row sends.
    fn create_body(&self) -> Value {
        let mut body = json!({
            "name": self.name,
            "executionRoleArn": ROLE_ARN,
            "idpConfig": self.idp_config(),
            "sources": self.sources(),
        });
        if let Some(description) = self.description {
            body["description"] = json!(description);
        }
        body
    }
}

/// `consentPortalId` matches `^[a-zA-Z0-9\-_]{1,50}-[A-Za-z0-9]{10}$`: a stem of up to 50
/// admitted characters, then a `-` and ten alphanumerics.
fn is_consent_portal_id(id: &str) -> bool {
    let Some((stem, suffix)) = id.rsplit_once('-') else {
        return false;
    };
    !stem.is_empty()
        && stem.len() <= MAX_NAME_LENGTH
        && stem
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        && suffix.len() == 10
        && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

/// `createdAt` and `updatedAt` target the PRELUDE `smithy.api#Timestamp` with no
/// `@timestampFormat` of their own (`smithy_query(query="shape",shape="Timestamp")`), so
/// `restJson1`'s default applies and they travel as epoch seconds — unlike this model's own
/// `DateTimestamp`, which is `date-time`. That is what a caller parses.
fn is_epoch_seconds(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_f64)
        .is_some_and(|seconds| seconds > 0.0)
}

/// RULE: a create mints the portal's identity, persists every member the request carried,
/// reports it `CREATING` on the way to the `ACTIVE` the clock owes it, and reads back —
/// through `GetConsentPortal` and through the `ListConsentPortals` summary — exactly as it
/// was created.
#[tokio::test]
async fn a_created_consent_portal_reads_back_and_is_listed() {
    for row in rows() {
        let (router, engine) = router();
        let created = call(&router, "CreateConsentPortal", &row.create_body()).await;
        assert_eq!(
            created.status, 202,
            "case {:?}: expected HTTP 202 from CreateConsentPortal, got {} ({})",
            row.case, created.status, created.body
        );
        let consent_portal_id = text(&created.body, "consentPortalId");
        assert!(
            is_consent_portal_id(&consent_portal_id),
            "case {:?}: expected a minted consentPortalId, got {consent_portal_id:?}",
            row.case
        );
        assert_eq!(
            text(&created.body, "consentPortalArn"),
            format!(
                "arn:aws:bedrock-agentcore:{REGION}:{ACCOUNT}:consent-portal/{consent_portal_id}"
            ),
            "case {:?}: the ARN is derived from the id, the Region and the account, got {}",
            row.case,
            created.body
        );
        assert_eq!(
            text(&created.body, "portalUrl"),
            published_portal_url(unserved_base(), &consent_portal_id),
            "case {:?}: the portal publishes the address this emulator serves it at, got {}",
            row.case,
            created.body
        );
        assert_eq!(
            text(&created.body, "status"),
            "CREATING",
            "case {:?}: a new portal is created, not active, got {}",
            row.case,
            created.body
        );
        assert_eq!(
            created.body.get("statusReason"),
            None,
            "case {:?}: nothing failed, so the status carries no reason, got {}",
            row.case,
            created.body
        );
        assert_eq!(
            text(&created.body, "name"),
            row.name,
            "case {:?}: the create reports the name it was given, got {}",
            row.case,
            created.body
        );
        assert_eq!(
            text(&created.body, "executionRoleArn"),
            ROLE_ARN,
            "case {:?}: the create reports the execution role it was given, got {}",
            row.case,
            created.body
        );
        assert_eq!(
            created.body.get("idpConfig"),
            Some(&row.idp_config()),
            "case {:?}: the create reports the identity provider configuration it was given, \
             got {}",
            row.case,
            created.body
        );
        assert_eq!(
            created.body.get("sources"),
            Some(&row.sources()),
            "case {:?}: the create reports the sources it was given, got {}",
            row.case,
            created.body
        );
        assert_eq!(
            created.body.get("description").and_then(Value::as_str),
            row.description,
            "case {:?}: the create reports the description it was given, got {}",
            row.case,
            created.body
        );
        for member in ["createdAt", "updatedAt"] {
            assert!(
                is_epoch_seconds(created.body.get(member)),
                "case {:?}: {member} travels as epoch seconds on this wire, got {}",
                row.case,
                created.body
            );
        }

        // The round trip: the read reports the record the create wrote.
        let read = get_portal(&router, &consent_portal_id).await;
        assert_eq!(
            read.status, 200,
            "case {:?}: the portal must read back, got {} ({})",
            row.case, read.status, read.body
        );
        for member in [
            "consentPortalId",
            "consentPortalArn",
            "name",
            "description",
            "executionRoleArn",
            "idpConfig",
            "sources",
            "portalUrl",
            "createdAt",
            "updatedAt",
        ] {
            assert_eq!(
                read.body.get(member),
                created.body.get(member),
                "case {:?}: the read reports the {member} the create did, got {}",
                row.case,
                read.body
            );
        }

        // The listing reports the same record projected onto `ConsentPortalSummary`.
        let listed = call(&router, "ListConsentPortals", &json!({})).await;
        assert_eq!(
            listed.status, 200,
            "case {:?}: expected HTTP 200 from ListConsentPortals, got {} ({})",
            row.case, listed.status, listed.body
        );
        let summaries = listed
            .body
            .get("consentPortals")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            summaries.len(),
            1,
            "case {:?}: exactly the created portal is listed, got {}",
            row.case,
            listed.body
        );
        let summary = &summaries[0];
        for member in [
            "consentPortalId",
            "consentPortalArn",
            "name",
            "description",
            "sources",
            "portalUrl",
            "createdAt",
            "updatedAt",
        ] {
            assert_eq!(
                summary.get(member),
                created.body.get(member),
                "case {:?}: the summary reports the {member} the create did, got {summary}",
                row.case
            );
        }

        // The CLOCK, not a call, moves the portal to `ACTIVE`.
        settle_now(&engine);
        let settled = poll_until_status(&router, &consent_portal_id, "ACTIVE", row.case).await;
        assert_eq!(
            settled.body.get("updatedAt"),
            created.body.get("updatedAt"),
            "case {:?}: settling is not a modification, so updatedAt is the create's, got {}",
            row.case,
            settled.body
        );
    }
}

/// RULE: "The name must be unique within your account", so a second create under a name
/// this account already holds is the modelled conflict and the stored portal is untouched.
#[tokio::test]
async fn a_name_this_account_already_holds_is_refused() {
    for row in rows() {
        let (router, engine) = router();
        let created = call(&router, "CreateConsentPortal", &row.create_body()).await;
        assert_eq!(
            created.status, 202,
            "case {:?}: the first create is accepted, got {} ({})",
            row.case, created.status, created.body
        );
        settle_now(&engine);
        poll_until_status(
            &router,
            &text(&created.body, "consentPortalId"),
            "ACTIVE",
            row.case,
        )
        .await;

        let refused = call(&router, "CreateConsentPortal", &row.create_body()).await;
        assert_eq!(
            (
                refused.status,
                error_code(&refused.body),
                text(&refused.body, "message")
            ),
            (
                409,
                "ConflictException".to_string(),
                name_conflict_message(&row.name)
            ),
            "case {:?}: expected HTTP 409 ConflictException naming the name the request \
             carried, got {} ({})",
            row.case,
            refused.status,
            refused.body
        );
        let listed = call(&router, "ListConsentPortals", &json!({})).await;
        assert_eq!(
            listed
                .body
                .get("consentPortals")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(1),
            "case {:?}: the refused create brought no second portal into being, got {}",
            row.case,
            listed.body
        );
    }
}

/// Which members an update carries.
#[derive(Debug, Clone, Copy)]
enum Patch {
    /// No member at all, so nothing about the portal changes.
    Nothing,
    /// A replacement execution role only.
    Role,
    /// A replacement identity provider configuration only.
    Idp,
    /// A replacement description only.
    Description,
    /// Every editable member at once.
    Everything,
}

impl Patch {
    fn case(self) -> &'static str {
        match self {
            Self::Nothing => "no editable member at all",
            Self::Role => "the execution role only",
            Self::Idp => "the identity provider configuration only",
            Self::Description => "the description only",
            Self::Everything => "every editable member at once",
        }
    }

    /// The replacement identity provider configuration an update carrying one sends.
    fn replacement_idp() -> Value {
        json!({"credentialProviderArn": OTHER_IDP_ARN, "scopes": ["openid", "email"]})
    }

    fn body(self, consent_portal_identifier: &str) -> Value {
        let mut body = json!({"consentPortalIdentifier": consent_portal_identifier});
        if matches!(self, Self::Role | Self::Everything) {
            body["executionRoleArn"] = json!(OTHER_ROLE_ARN);
        }
        if matches!(self, Self::Idp | Self::Everything) {
            body["idpConfig"] = Self::replacement_idp();
        }
        if matches!(self, Self::Description | Self::Everything) {
            body["description"] = json!("the description this update applied");
        }
        body
    }

    /// What the portal reports for `member` after this form was applied to a portal that
    /// reported `before`.
    fn expected(self, member: &str, before: Option<&Value>) -> Option<Value> {
        match (self, member) {
            (Self::Role | Self::Everything, "executionRoleArn") => Some(json!(OTHER_ROLE_ARN)),
            (Self::Idp | Self::Everything, "idpConfig") => Some(Self::replacement_idp()),
            (Self::Description | Self::Everything, "description") => {
                Some(json!("the description this update applied"))
            }
            _ => before.cloned(),
        }
    }
}

/// RULE: an accepted update replaces each member the request carries and leaves every member
/// it does not — including the portal's identity, its name and its sources, which the
/// request has no member for — reports the portal `UPDATING` with the transition back to
/// `ACTIVE` owed by the clock, and reads back exactly as the update reported it.
#[tokio::test]
async fn an_update_replaces_only_the_members_it_carries() {
    let row = &rows()[1];
    for patch in [
        Patch::Nothing,
        Patch::Role,
        Patch::Idp,
        Patch::Description,
        Patch::Everything,
    ] {
        let case = patch.case();
        let (router, engine) = router();
        let created = call(&router, "CreateConsentPortal", &row.create_body()).await;
        assert_eq!(
            created.status, 202,
            "case {case:?}: the portal must be created, got {} ({})",
            created.status, created.body
        );
        let consent_portal_id = text(&created.body, "consentPortalId");
        settle_now(&engine);
        let before = poll_until_status(&router, &consent_portal_id, "ACTIVE", case).await;

        let updated = call(
            &router,
            "UpdateConsentPortal",
            &patch.body(&consent_portal_id),
        )
        .await;
        assert_eq!(
            updated.status, 202,
            "case {case:?}: expected HTTP 202 from UpdateConsentPortal, got {} ({})",
            updated.status, updated.body
        );
        assert_eq!(
            text(&updated.body, "status"),
            "UPDATING",
            "case {case:?}: an accepted change reports UPDATING, got {}",
            updated.body
        );
        for member in ["executionRoleArn", "idpConfig", "description"] {
            let expected = patch.expected(member, before.body.get(member));
            assert_eq!(
                updated.body.get(member).cloned(),
                expected,
                "case {case:?}: expected the {member} {expected:?}, got {}",
                updated.body
            );
        }
        // The members this operation carries no value for are left as the create stored
        // them.
        for member in [
            "consentPortalId",
            "consentPortalArn",
            "name",
            "sources",
            "portalUrl",
            "createdAt",
        ] {
            assert_eq!(
                updated.body.get(member),
                before.body.get(member),
                "case {case:?}: the update leaves the {member} the create stored, got {}",
                updated.body
            );
        }
        assert_ne!(
            updated.body.get("updatedAt"),
            before.body.get("updatedAt"),
            "case {case:?}: the update records when the portal was last modified, got {}",
            updated.body
        );

        // The round trip, and the change outliving the clock-owed transition back.
        let read = get_portal(&router, &consent_portal_id).await;
        for member in [
            "executionRoleArn",
            "idpConfig",
            "description",
            "name",
            "sources",
            "portalUrl",
            "updatedAt",
        ] {
            assert_eq!(
                read.body.get(member),
                updated.body.get(member),
                "case {case:?}: the read reports the {member} the update did, got {}",
                read.body
            );
        }
        settle_now(&engine);
        let settled = poll_until_status(&router, &consent_portal_id, "ACTIVE", case).await;
        for member in ["executionRoleArn", "idpConfig", "description"] {
            assert_eq!(
                settled.body.get(member),
                updated.body.get(member),
                "case {case:?}: the settled portal still holds the {member} the update \
                 applied, got {}",
                settled.body
            );
        }
    }
}

/// RULE: a portal whose own asynchronous work is still under way refuses a change and a
/// deletion the modelled conflict, in this service's own sentence, and keeps the record it
/// had — so the refused call changed nothing.
#[tokio::test]
async fn a_portal_that_is_not_active_refuses_a_change_or_a_deletion() {
    let row = &rows()[1];
    // (the status the portal is in, how it got there)
    let (router, engine) = router();
    let created = call(&router, "CreateConsentPortal", &row.create_body()).await;
    let consent_portal_id = text(&created.body, "consentPortalId");
    assert_eq!(
        (created.status, text(&created.body, "status")),
        (202, "CREATING".to_string()),
        "the create reports CREATING, got {} ({})",
        created.status,
        created.body
    );

    for operation in ["UpdateConsentPortal", "DeleteConsentPortal"] {
        let refused = call(
            &router,
            operation,
            &json!({
                "consentPortalIdentifier": consent_portal_id,
                "description": "never applied",
            }),
        )
        .await;
        assert_eq!(
            (
                refused.status,
                error_code(&refused.body),
                text(&refused.body, "message")
            ),
            (
                409,
                "ConflictException".to_string(),
                status_conflict_message(&consent_portal_id, "CREATING")
            ),
            "case {operation:?}/CREATING: expected HTTP 409 ConflictException naming the \
             portal and the status it is in, got {} ({})",
            refused.status,
            refused.body
        );
    }
    let unchanged = get_portal(&router, &consent_portal_id).await;
    assert_eq!(
        (
            unchanged.status,
            unchanged.body.get("description").and_then(Value::as_str)
        ),
        (200, row.description),
        "the refused calls changed nothing, got {} ({})",
        unchanged.status,
        unchanged.body
    );

    // And while an accepted UPDATE is still under way, which is the concurrent change the
    // sentence is about.
    settle_now(&engine);
    poll_until_status(&router, &consent_portal_id, "ACTIVE", "accepted update").await;
    let accepted = call(
        &router,
        "UpdateConsentPortal",
        &json!({
            "consentPortalIdentifier": consent_portal_id,
            "description": "the change that won",
        }),
    )
    .await;
    assert_eq!(
        (accepted.status, text(&accepted.body, "status")),
        (202, "UPDATING".to_string()),
        "the first change is accepted and leaves the portal UPDATING, got {} ({})",
        accepted.status,
        accepted.body
    );
    let second = call(
        &router,
        "UpdateConsentPortal",
        &json!({
            "consentPortalIdentifier": consent_portal_id,
            "description": "never applied either",
        }),
    )
    .await;
    assert_eq!(
        (
            second.status,
            error_code(&second.body),
            text(&second.body, "message")
        ),
        (
            409,
            "ConflictException".to_string(),
            status_conflict_message(&consent_portal_id, "UPDATING")
        ),
        "a concurrent change is the modelled conflict, got {} ({})",
        second.status,
        second.body
    );
    settle_now(&engine);
    let won = poll_until_status(&router, &consent_portal_id, "ACTIVE", "accepted update").await;
    assert_eq!(
        text(&won.body, "description"),
        "the change that won",
        "the refused change left the accepted one's description in place, got {}",
        won.body
    );
}

/// RULE: a delete is ACCEPTED, the portal reports `DELETING` while the removal the clock
/// owes is under way, and once it is due the portal is the modelled 404 — and the name it
/// held is free for another create.
#[tokio::test]
async fn an_accepted_deletion_removes_the_portal_and_frees_its_name() {
    for row in rows() {
        let (router, engine) = router();
        let created = call(&router, "CreateConsentPortal", &row.create_body()).await;
        let consent_portal_id = text(&created.body, "consentPortalId");
        settle_now(&engine);
        poll_until_status(&router, &consent_portal_id, "ACTIVE", row.case).await;

        let deleted = call(
            &router,
            "DeleteConsentPortal",
            &json!({"consentPortalIdentifier": consent_portal_id}),
        )
        .await;
        assert_eq!(
            deleted.status, 204,
            "case {:?}: expected HTTP 204 from DeleteConsentPortal, got {} ({})",
            row.case, deleted.status, deleted.body
        );
        // The portal is still readable while the removal is under way, and says what it is
        // doing.
        let deleting = get_portal(&router, &consent_portal_id).await;
        assert_eq!(
            (deleting.status, text(&deleting.body, "status")),
            (200, "DELETING".to_string()),
            "case {:?}: an accepted deletion reports DELETING, got {} ({})",
            row.case,
            deleting.status,
            deleting.body
        );
        // `DeleteConsentPortal` is `@idempotent`: a repeat while the removal is under way is
        // the same acceptance rather than a conflict.
        let repeat = call(
            &router,
            "DeleteConsentPortal",
            &json!({"consentPortalIdentifier": consent_portal_id}),
        )
        .await;
        assert_eq!(
            repeat.status, 204,
            "case {:?}: a repeated delete is accepted again, got {} ({})",
            row.case, repeat.status, repeat.body
        );

        // The CLOCK removes it, and then every read of it is the modelled 404.
        settle_now(&engine);
        let gone = poll_until_gone(&router, &consent_portal_id, row.case).await;
        assert_eq!(
            (error_code(&gone.body), text(&gone.body, "message")),
            (
                "ResourceNotFoundException".to_string(),
                not_found_message(&consent_portal_id)
            ),
            "case {:?}: a portal that is gone is reported missing in this service's own \
             sentence, got {}",
            row.case,
            gone.body
        );
        let listed = call(&router, "ListConsentPortals", &json!({})).await;
        assert_eq!(
            listed.body.get("consentPortals"),
            Some(&json!([])),
            "case {:?}: a removed portal is not listed, got {}",
            row.case,
            listed.body
        );
        // The name it held is free again, so the create that collided before is accepted.
        let recreated = call(&router, "CreateConsentPortal", &row.create_body()).await;
        assert_eq!(
            recreated.status, 202,
            "case {:?}: the removed portal's name is free again, got {} ({})",
            row.case, recreated.status, recreated.body
        );
        assert_ne!(
            text(&recreated.body, "consentPortalId"),
            consent_portal_id,
            "case {:?}: the second portal is a new resource with an id of its own, got {}",
            row.case,
            recreated.body
        );
    }
}

/// RULE: an identifier naming no consent portal of this account is the modelled 404 for
/// every operation that resolves one, and brings no portal into being by being addressed.
#[tokio::test]
async fn an_unknown_consent_portal_is_refused() {
    let (router, _engine) = router();
    // Every identifier satisfies `ConsentPortalIdentifier`'s
    // `^[a-zA-Z0-9\-_]{1,50}-[A-Za-z0-9]{10}$`, so the refusal is the handler's lookup and
    // not the boundary constraint walk: the shortest the pattern admits, one carrying
    // hyphens, one carrying underscores, and one whose suffix is mixed case.
    let unknown = [
        "a-0123456789",
        "missing-consent-portal-abcdefghij",
        "missing_consent_portal-zzzzzzzzzz",
        "MixedCase-AbCdEfGhIj",
    ];
    for consent_portal_identifier in unknown {
        for operation in [
            "GetConsentPortal",
            "UpdateConsentPortal",
            "DeleteConsentPortal",
        ] {
            let refused = call(
                &router,
                operation,
                &json!({"consentPortalIdentifier": consent_portal_identifier}),
            )
            .await;
            assert_eq!(
                (
                    refused.status,
                    error_code(&refused.body),
                    text(&refused.body, "message")
                ),
                (
                    404,
                    "ResourceNotFoundException".to_string(),
                    not_found_message(consent_portal_identifier)
                ),
                "case {consent_portal_identifier:?}/{operation}: expected HTTP 404 \
                 ResourceNotFoundException, got {} ({})",
                refused.status,
                refused.body
            );
        }
    }
    let listed = call(&router, "ListConsentPortals", &json!({})).await;
    assert_eq!(
        listed.body.get("consentPortals"),
        Some(&json!([])),
        "the refused calls left no portal behind, got {}",
        listed.body
    );
}

/// The endpoint bases this emulator can report as its own: the forms a data port is
/// published at, plus the base of an emulator embedded in another process. One row each,
/// because the published portal address has to follow whichever one this emulator is reached
/// at.
const ENDPOINT_BASES: [&str; 4] = [
    "https://127.0.0.1:9297",
    "https://localhost:39355",
    "https://[::1]:8443",
    smithy_server_runtime::UNSERVED_URL_BASE,
];

/// The DNS zones AWS publishes its own endpoints in: a published address must name none of
/// them, because this emulator answers at none of them.
const FOREIGN_HOST_MARKERS: [&str; 3] = [".on.aws", ".amazonaws.com", ".api.aws"];

/// RULE: `portalUrl` names the endpoint THIS emulator is served at, with the portal's own id
/// in the path — never a host AWS publishes and this emulator does not answer on — and a
/// fetch of that address is served here, from the stored portal.
#[tokio::test]
async fn the_published_portal_url_is_served_by_this_emulator() {
    let row = &rows()[0];
    for base in ENDPOINT_BASES {
        let (router, engine) = router_served_at(base);
        let created = call(&router, "CreateConsentPortal", &row.create_body()).await;
        assert_eq!(
            created.status, 202,
            "case {base:?}: the portal must be created, got {} ({})",
            created.status, created.body
        );
        let consent_portal_id = text(&created.body, "consentPortalId");
        let url = published_portal_url(base, &consent_portal_id);
        assert_eq!(
            text(&created.body, "portalUrl"),
            url,
            "case {base:?}: expected the portal published on this emulator's own endpoint, \
             got {}",
            created.body
        );
        for marker in FOREIGN_HOST_MARKERS {
            assert!(
                !url.contains(marker),
                "case {base:?}: a published portal URL must name no AWS host, got {url:?}"
            );
        }

        let page = fetch(&router, "GET", &url).await;
        assert_eq!(
            page.status, 200,
            "case {base:?}: expected the published portal URL {url:?} to be served here, got \
             {} ({})",
            page.status, page.body
        );
        assert!(
            page.content_type.starts_with("text/html"),
            "case {base:?}: expected a portal page, got content type {:?}",
            page.content_type
        );
        for expected in [consent_portal_id.as_str(), row.name.as_str(), "CREATING"] {
            assert!(
                page.body.contains(expected),
                "case {base:?}: expected the page to name {expected:?}, got {:?}",
                page.body
            );
        }

        // What a client asks when it only wants to know the portal is there.
        let head = fetch(&router, "HEAD", &url).await;
        assert_eq!(
            (head.status, head.body.is_empty()),
            (200, true),
            "case {base:?}: expected a HEAD of {url:?} to answer 200 with no body, got {} \
             ({:?})",
            head.status,
            head.body
        );

        // The page is the stored record and nothing else: it reports the status the clock
        // has moved the portal to.
        settle_now(&engine);
        poll_until_status(&router, &consent_portal_id, "ACTIVE", base).await;
        let settled = fetch(&router, "GET", &url).await;
        assert!(
            settled.body.contains("ACTIVE"),
            "case {base:?}: expected the page to report the status the portal reports, got \
             {:?}",
            settled.body
        );

        // An address of this shape naming a portal this emulator does not hold: the page
        // says there is none, rather than reporting a portal nothing stands behind.
        let absent = published_portal_url(base, "no-such-consent-portal-abcdefghij");
        let missing = fetch(&router, "GET", &absent).await;
        assert_eq!(
            missing.status, 404,
            "case {base:?}: expected no portal at {absent:?}, got {} ({})",
            missing.status, missing.body
        );
        assert!(
            missing.content_type.starts_with("text/html"),
            "case {base:?}: expected the portal route itself to answer for {absent:?}, got \
             content type {:?}",
            missing.content_type
        );
    }
}

/// RULE: `ListConsentPortals` is `@paginated` over the portals this account holds: a page
/// holds at most `maxResults` summaries, the `nextToken` resumes strictly after the portal
/// the page ended on, every portal is reported exactly once, and a token this service did
/// not issue is refused.
#[tokio::test]
async fn the_listing_pages_over_every_portal_once() {
    let (router, engine) = router();
    let mut created = Vec::new();
    for index in 0..5_u32 {
        let answer = call(
            &router,
            "CreateConsentPortal",
            &json!({
                "name": format!("portal-{index}"),
                "executionRoleArn": ROLE_ARN,
                "idpConfig": {"credentialProviderArn": IDP_ARN, "scopes": ["openid"]},
                "sources": [{"identifier": "listgw-abcdefghij", "type": SOURCE_TYPE}],
            }),
        )
        .await;
        assert_eq!(
            answer.status, 202,
            "creating portal-{index}: got {} ({})",
            answer.status, answer.body
        );
        created.push(text(&answer.body, "consentPortalId"));
    }
    settle_now(&engine);
    created.sort();

    for page_size in [1_usize, 2, 5, 100] {
        let case = format!("maxResults {page_size}");
        let mut seen = Vec::new();
        let mut next_token = None;
        loop {
            let mut request = json!({"maxResults": page_size});
            if let Some(token) = &next_token {
                request["nextToken"] = json!(token);
            }
            let listed = call(&router, "ListConsentPortals", &request).await;
            assert_eq!(
                listed.status, 200,
                "case {case:?}: expected HTTP 200 from ListConsentPortals, got {} ({})",
                listed.status, listed.body
            );
            let page = listed
                .body
                .get("consentPortals")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            assert!(
                page.len() <= page_size,
                "case {case:?}: a page holds at most maxResults summaries, got {}",
                listed.body
            );
            for summary in &page {
                seen.push(
                    summary
                        .get("consentPortalId")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                );
            }
            next_token = listed
                .body
                .get("nextToken")
                .and_then(Value::as_str)
                .map(str::to_string);
            if next_token.is_none() {
                break;
            }
        }
        seen.sort();
        assert_eq!(
            seen, created,
            "case {case:?}: every portal is reported exactly once over the pages"
        );
    }

    let refused = call(
        &router,
        "ListConsentPortals",
        &json!({"nextToken": "not-a-token-this-service-issued"}),
    )
    .await;
    assert_eq!(
        (refused.status, error_code(&refused.body)),
        (400, "ValidationException".to_string()),
        "a token this service did not issue is refused, got {} ({})",
        refused.status,
        refused.body
    );
}
