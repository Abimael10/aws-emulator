// SPDX-License-Identifier: CC0-1.0

//! What an explicit `Deny` in an Amazon OpenSearch Service domain's own access policy does to
//! a signed data-plane request whose principal a BROAD identity grant allows — driven end to
//! end on the assembled emulator, the way a workload is wired: IAM holds the role and its
//! inline grant, STS mints the session the request signs as, and the domain publishes the
//! address its HTTP document surface is served at.
//!
//! The rule under test, one sentence: within one account a request is served only when some
//! policy in force allows the action on the resource it addresses and NONE denies it, so an
//! explicit `Deny` in the resource's own resource-based policy refuses the request whatever
//! the principal's identity-based policies allow — and the refusal is answered at once, with
//! the status, the error class and the sentence AWS words such a refusal with, rather than
//! left outstanding.
//!
//! So the assertions span a SET of inputs rather than one recorded call. The rows differ
//! along every dimension the rule generalises over:
//!
//! * HOW the `Deny` names the action — exactly the one the request's method asks for, a list
//!   of actions, a wildcarded family, and every data-plane action at once;
//! * WHICH principal it names — the role by ARN, and every principal (`"*"`);
//! * WHAT resource it covers — the whole domain, and one index of it, which is the row where
//!   the `Deny` does NOT reach the request and the identity grant decides it instead;
//! * the METHOD the request carries, which is where the `es:ESHttp*` action comes from.
//!
//! and each row is judged on BOTH sides of the decision: what the attempt is answered, and
//! what a later read of the same document path reports — a write the `Deny` refused must
//! leave nothing behind.
//!
//! The second test pins the one CHANGE: with IAM untouched, replacing the access policy with
//! a plain `Allow` of the data-plane actions makes the identical request serve and the
//! document read back with its exact source, so the refusal is attributable to the policy and
//! to nothing else.
//!
//! Only the assembled router can pin any of it: IAM holds the role and its inline grant, STS
//! mints the session credential and names the principal it speaks for, and Amazon OpenSearch
//! Service publishes the address and serves the domain's data plane at it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The Region every call here is signed for, and so the Region of the domains' own ARNs.
const REGION: &str = "us-east-1";

/// The API version this service binds its control plane under, which is the first path
/// segment of every control-plane call below.
const CONTROL_PREFIX: &str = "/2021-01-01";

/// The IAM and STS API versions the awsQuery setup calls name.
const IAM_VERSION: &str = "2010-05-08";
const STS_VERSION: &str = "2011-06-15";

/// The engine release every domain here runs, from this service's own catalogue.
const ENGINE_VERSION: &str = "OpenSearch_2.11";

/// The account's own published credential, which the SETUP calls are made with.
const AMBIENT_KEY: &str = "AKIDEXAMPLE";

/// The session name every credential minted here carries.
const SESSION_NAME: &str = "writer";

/// How long a poll waits for work the clock owes — a domain's provisioning falling due —
/// before it calls waiting a failure, and how often it looks while waiting. Generous,
/// because no case here is about how long that span is: the wait is for the CONDITION the
/// control plane reports.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// One answered request: its status, its body as JSON (`Value::Null` when the body is not
/// JSON) and the raw text, which is what an assertion reports.
struct Answer {
    status: u16,
    body: Value,
    text: String,
}

impl Answer {
    fn summary(&self) -> String {
        format!("{} {}", self.status, self.text)
    }

    /// The `Message` an IAM-layer refusal of a data-plane request carries.
    fn message(&self) -> String {
        self.body["Message"].as_str().unwrap_or_default().to_string()
    }
}

/// The emulator as it is ASSEMBLED, on one engine, with request authentication off: the
/// suite presents each credential as a credential SCOPE rather than a real signature, which
/// is what lets one router answer for every service these rules need.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header naming `service` in its credential scope and
/// `access_key_id` as the credential, which is what the principal of the request is resolved
/// from.
fn authorization(service: &str, access_key_id: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

async fn dispatch(router: &MultiServiceRouter, request: http::Request<Bytes>) -> Answer {
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).into_owned();
    let body = serde_json::from_str(&text).unwrap_or(Value::Null);
    Answer { status, body, text }
}

/// One awsQuery form call of `service`, as the account's own credential.
async fn query(router: &MultiServiceRouter, service: &str, body: String) -> Answer {
    let host = format!("{service}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service, AMBIENT_KEY))
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    dispatch(router, request).await
}

/// One call of this service's own control plane, as the account's own credential.
async fn control(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answer {
    let host = format!("es.{REGION}.amazonaws.com");
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{CONTROL_PREFIX}{path}"))
        .header("host", &host)
        .header("content-type", "application/json")
        .header("authorization", authorization("es", AMBIENT_KEY))
        .body(Bytes::from(payload))
        .expect("the test request is well formed");
    dispatch(router, request).await
}

/// One data-plane request to the ADDRESS the control plane published for a domain, dialled
/// exactly as a client dials it: it builds `https://{address}{path}`, so the authority of the
/// published value is the host the request carries and whatever follows it is the prefix of
/// the request path.
async fn at_address(
    router: &MultiServiceRouter,
    address: &str,
    access_key_id: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answer {
    let (authority, prefix) = match address.find('/') {
        Some(end) => (&address[..end], &address[end..]),
        None => (address, ""),
    };
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{authority}{prefix}{path}"))
        .header("host", authority)
        .header("content-type", "application/json")
        .header("authorization", authorization("es", access_key_id))
        .body(Bytes::from(payload))
        .expect("the test request is well formed");
    dispatch(router, request).await
}

/// The name of the inline policy the role's identity grant is held under.
const GRANT_NAME: &str = "data-plane";

/// Create `name` as a role this account may assume, carrying `grant` as its one inline
/// policy, and answer the role ARN IAM recorded.
async fn created_role(router: &MultiServiceRouter, name: &str, grant: Value) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "AWS": format!("arn:aws:iam::{}:root", engine::emulator_account_id()) },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = query(
        router,
        "iam",
        format!(
            "Action=CreateRole&Version={IAM_VERSION}&RoleName={name}&AssumeRolePolicyDocument={}",
            form_encode(&trust)
        ),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "setup CreateRole({name:?}): expected 200, got {}",
        created.summary()
    );
    let arn = element(&created.text, "Arn");
    assert!(
        !arn.is_empty(),
        "setup CreateRole({name:?}): expected the role ARN IAM recorded, got {}",
        created.summary()
    );
    let attached = query(
        router,
        "iam",
        format!(
            "Action=PutRolePolicy&Version={IAM_VERSION}&RoleName={name}\
             &PolicyName={GRANT_NAME}&PolicyDocument={}",
            form_encode(&grant.to_string())
        ),
    )
    .await;
    assert_eq!(
        attached.status, 200,
        "setup PutRolePolicy({name:?}): expected 200, got {}",
        attached.summary()
    );
    arn
}

/// One assumed-role credential, as a client holds it: the key id its requests carry and the
/// principal STS says it speaks for.
struct Session {
    access_key_id: String,
    principal_arn: String,
}

/// Mint a session of `role_arn` through the token service's own `AssumeRole`.
async fn assumed(router: &MultiServiceRouter, role_arn: &str) -> Session {
    let answer = query(
        router,
        "sts",
        format!(
            "Action=AssumeRole&Version={STS_VERSION}&RoleArn={}&RoleSessionName={SESSION_NAME}",
            form_encode(role_arn)
        ),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "setup AssumeRole({role_arn:?}): expected 200, got {}",
        answer.summary()
    );
    let session = Session {
        access_key_id: element(&answer.text, "AccessKeyId"),
        principal_arn: element(&answer.text, "Arn"),
    };
    assert!(
        !session.access_key_id.is_empty() && session.principal_arn.starts_with("arn:aws:sts::"),
        "setup AssumeRole({role_arn:?}): expected a credential and the session principal it \
         speaks for, got {}",
        answer.summary()
    );
    session
}

/// A domain's own ARN, which is what a policy scopes a statement to its paths under.
fn domain_arn(domain: &str) -> String {
    format!(
        "arn:aws:es:{REGION}:{}:domain/{domain}",
        engine::emulator_account_id()
    )
}

/// The BROAD identity-based grant the role carries throughout: every data-plane action of
/// this service, on every path of every domain of this account. It is the Allow the rows'
/// resource-policy `Deny` has to win over.
fn broad_identity_grant() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": "es:ESHttp*",
            "Resource": format!(
                "arn:aws:es:{REGION}:{}:domain/*",
                engine::emulator_account_id()
            ),
        }],
    })
}

/// One domain whose access policy carries an explicit `Deny`, the request made against it,
/// and the two answers the rule fixes.
struct DenyRow {
    case: &'static str,
    /// The domain this row's policy is attached to; each row needs its own, because the
    /// policy IS the input under test.
    domain: &'static str,
    /// The `Action` element of the `Deny` statement.
    deny_action: Value,
    /// What the `Deny` statement's `Resource` covers, as the suffix appended to the domain's
    /// own ARN.
    deny_resource: &'static str,
    /// Whether the `Deny` names every principal (`"*"`) rather than the role by its ARN.
    deny_every_principal: bool,
    method: &'static str,
    index: &'static str,
    document_id: &'static str,
    /// The `es:ESHttp*` action the method asks for, which a refusal's sentence names.
    action: &'static str,
    /// The status the attempt itself is answered with.
    attempted: u16,
    /// The status a later `GET` of the same document path answers.
    readback: u16,
}

/// The table: the `Deny`'s action element, the principal it names, the resource it covers and
/// the method the request carries all differ, so no answer below can come from a case keyed
/// to one shape of input.
fn deny_rows() -> Vec<DenyRow> {
    vec![
        DenyRow {
            case: "a deny of exactly the action the request's method asks for",
            domain: "deny-exact-write",
            deny_action: json!("es:ESHttpPut"),
            deny_resource: "/*",
            deny_every_principal: false,
            method: "PUT",
            index: "catalog",
            document_id: "blocked",
            action: "es:ESHttpPut",
            attempted: 403,
            readback: 404,
        },
        DenyRow {
            case: "a deny naming a list of actions, one of which the request asks for",
            domain: "deny-listed-actions",
            deny_action: json!(["es:ESHttpPost", "es:ESHttpDelete"]),
            deny_resource: "/*",
            deny_every_principal: false,
            method: "POST",
            index: "orders-2026",
            document_id: "order-42",
            action: "es:ESHttpPost",
            attempted: 403,
            readback: 404,
        },
        DenyRow {
            case: "a deny of a wildcarded action family covering the request's action",
            domain: "deny-action-family",
            deny_action: json!("es:ESHttpP*"),
            deny_resource: "/*",
            deny_every_principal: false,
            method: "PUT",
            index: "logs.app",
            document_id: "9f0c1e42",
            action: "es:ESHttpPut",
            attempted: 403,
            readback: 404,
        },
        DenyRow {
            case: "a deny of every data-plane action, which refuses the read as well",
            domain: "deny-every-action",
            deny_action: json!("es:ESHttp*"),
            deny_resource: "/*",
            deny_every_principal: false,
            method: "DELETE",
            index: "profiles",
            document_id: "user_7",
            action: "es:ESHttpDelete",
            attempted: 403,
            readback: 403,
        },
        DenyRow {
            case: "a deny naming every principal rather than the role by its ARN",
            domain: "deny-every-principal",
            deny_action: json!("es:ESHttpPut"),
            deny_resource: "/*",
            deny_every_principal: true,
            method: "PUT",
            index: "catalog",
            document_id: "anyone",
            action: "es:ESHttpPut",
            attempted: 403,
            readback: 404,
        },
        DenyRow {
            case: "a deny covering another index only, which leaves the identity grant deciding",
            domain: "deny-other-index",
            deny_action: json!("es:ESHttpPut"),
            deny_resource: "/ledger*",
            deny_every_principal: false,
            method: "PUT",
            index: "catalog",
            document_id: "served",
            action: "es:ESHttpPut",
            attempted: 201,
            readback: 200,
        },
    ]
}

/// The document every row writes, named after the row so no two rows can be confused for one
/// another.
fn row_document(row: &DenyRow) -> Value {
    json!({ "case": row.case, "v": 1 })
}

/// The access policy a row's domain carries: an `Allow` of the read action for the role, so a
/// refused write can be shown to have left nothing behind, and the row's own explicit `Deny`.
fn row_access_policy(row: &DenyRow, role_arn: &str) -> String {
    let arn = domain_arn(row.domain);
    let denied_principal = if row.deny_every_principal {
        json!("*")
    } else {
        json!(role_arn)
    };
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Principal": { "AWS": role_arn },
                "Action": "es:ESHttpGet",
                "Resource": format!("{arn}/*"),
            },
            {
                "Effect": "Deny",
                "Principal": { "AWS": denied_principal },
                "Action": row.deny_action.clone(),
                "Resource": format!("{arn}{}", row.deny_resource),
            },
        ],
    })
    .to_string()
}

/// An access policy that plainly ALLOWS the role every data-plane action on the domain —
/// nothing denied.
fn plain_allow_policy(domain: &str, role_arn: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "AWS": role_arn },
            "Action": "es:ESHttp*",
            "Resource": format!("{}/*", domain_arn(domain)),
        }],
    })
    .to_string()
}

/// `CreateDomain` for `domain` carrying `policy` as its access policy.
async fn create_domain(router: &MultiServiceRouter, domain: &str, policy: &str) {
    let created = control(
        router,
        "POST",
        "/opensearch/domain",
        Some(json!({
            "DomainName": domain,
            "EngineVersion": ENGINE_VERSION,
            "ClusterConfig": {
                "InstanceType": "t3.small.search",
                "InstanceCount": 1,
                "DedicatedMasterEnabled": false,
                "ZoneAwarenessEnabled": false,
            },
            "EBSOptions": { "EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 10 },
            "DomainEndpointOptions": { "EnforceHTTPS": true },
            "AccessPolicies": policy,
        })),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "setup CreateDomain({domain:?}): expected 200, got {}",
        created.summary()
    );
}

/// `UpdateDomainConfig` replacing the domain's access policy, leaving IAM untouched.
async fn replace_access_policy(router: &MultiServiceRouter, domain: &str, policy: &str) {
    let updated = control(
        router,
        "POST",
        &format!("/opensearch/domain/{domain}/config"),
        Some(json!({ "AccessPolicies": policy })),
    )
    .await;
    assert_eq!(
        updated.status, 200,
        "setup UpdateDomainConfig({domain:?}): expected 200, got {}",
        updated.summary()
    );
}

/// Wait until the control plane reports `domain` created with nothing in flight — the
/// condition a client polls for — and answer the address it publishes for its data plane.
async fn published_endpoint(router: &MultiServiceRouter, domain: &str) -> String {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let read = control(router, "GET", &format!("/opensearch/domain/{domain}"), None).await;
        let status = &read.body["DomainStatus"];
        if status["Created"].as_bool() == Some(true) && status["Processing"].as_bool() == Some(false)
        {
            let endpoint = status["Endpoint"].as_str().unwrap_or_default().to_string();
            assert!(
                !endpoint.is_empty(),
                "setup DescribeDomain({domain:?}): expected the address the domain publishes \
                 once it is up, got {}",
                read.summary()
            );
            return endpoint;
        }
        assert!(
            Instant::now() < deadline,
            "setup DescribeDomain({domain:?}): the domain never reported itself settled, last \
             read {}",
            read.summary()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The sentence AWS refuses a data-plane request with that an explicit `Deny` in the
/// resource's own policy stopped: the principal that signed, the action its method asked for,
/// and which of IAM's rules refused.
fn refusal_sentence(principal_arn: &str, action: &str) -> String {
    format!(
        "User: {principal_arn} is not authorized to perform: {action} with an explicit deny in \
         a resource-based policy"
    )
}

/// Assert `answered` is the refusal an explicit resource-policy `Deny` produces: `403`, the
/// cluster's own `security_exception` class, and AWS's sentence for that rule.
fn assert_resource_deny(answered: &Answer, principal_arn: &str, action: &str, case: &str) {
    assert_eq!(
        answered.status, 403,
        "case {case:?}: expected HTTP 403 from the explicit deny, got {}",
        answered.summary()
    );
    assert_eq!(
        answered.message(),
        refusal_sentence(principal_arn, action),
        "case {case:?}: expected AWS's own sentence for an explicit deny in a resource-based \
         policy, got {}",
        answered.summary()
    );
}

/// RULE: within one account a signed data-plane request is served only when a policy in force
/// allows the `es:ESHttp*` action its method asks for on the resource its path names and NONE
/// denies it — so an explicit `Deny` in the domain's own access policy refuses it `403`
/// however broadly the principal's identity policies allow it, and the write it refused
/// leaves nothing behind; a `Deny` that does not reach the request leaves the identity grant
/// deciding, and the request is served.
#[tokio::test]
async fn an_explicit_deny_in_the_access_policy_beats_the_identity_grant() {
    let router = build_router();
    let rows = deny_rows();
    let role = created_role(&router, "deny-rule-writer", broad_identity_grant()).await;
    let writer = assumed(&router, &role).await;
    // Every domain is created before any is polled, so the provisioning the clock owes falls
    // due for all of them over one span rather than one span each.
    for row in &rows {
        create_domain(&router, row.domain, &row_access_policy(row, &role)).await;
    }

    for row in &rows {
        let case = row.case;
        let address = published_endpoint(&router, row.domain).await;
        let path = format!("/{}/_doc/{}", row.index, row.document_id);
        let body = (row.method != "DELETE").then(|| row_document(row));
        let answered = at_address(
            &router,
            &address,
            &writer.access_key_id,
            row.method,
            &path,
            body,
        )
        .await;
        if row.attempted == 403 {
            assert_resource_deny(&answered, &writer.principal_arn, row.action, case);
        } else {
            assert_eq!(
                (answered.status, answered.body["result"].as_str()),
                (row.attempted, Some("created")),
                "case {case:?}: expected the request the deny does not reach to be indexed, \
                 got {}",
                answered.summary()
            );
        }

        // THE OTHER SIDE of the decision: what the document path holds afterwards. A write
        // the deny refused left nothing, and a deny that covers the read refuses that too.
        let read = at_address(
            &router,
            &address,
            &writer.access_key_id,
            "GET",
            &path,
            None,
        )
        .await;
        assert_eq!(
            read.status, row.readback,
            "case {case:?}: expected HTTP {} reading the document path back, got {}",
            row.readback,
            read.summary()
        );
        match row.readback {
            403 => assert_resource_deny(&read, &writer.principal_arn, "es:ESHttpGet", case),
            404 => assert_ne!(
                read.body["found"].as_bool(),
                Some(true),
                "case {case:?}: the refused write must have left no document, got {}",
                read.summary()
            ),
            _ => assert_eq!(
                (read.body["found"].as_bool(), &read.body["_source"]),
                (Some(true), &row_document(row)),
                "case {case:?}: expected the served write to read back as stored, got {}",
                read.summary()
            ),
        }
    }
}

/// RULE, from the other direction: the refusal is attributable to the explicit `Deny` and to
/// nothing else — with IAM untouched, replacing the access policy with a plain `Allow` of the
/// data-plane actions makes the identical request serve, and the document reads back with its
/// exact source.
#[tokio::test]
async fn replacing_the_deny_with_an_allow_serves_the_identical_request() {
    let router = build_router();
    let domain = "deny-then-allow";
    let role = created_role(&router, "deny-then-allow-writer", broad_identity_grant()).await;
    let writer = assumed(&router, &role).await;
    let denied = DenyRow {
        case: "the write the access policy denies",
        domain,
        deny_action: json!("es:ESHttpPut"),
        deny_resource: "/*",
        deny_every_principal: false,
        method: "PUT",
        index: "catalog",
        document_id: "blocked",
        action: "es:ESHttpPut",
        attempted: 403,
        readback: 404,
    };
    create_domain(&router, domain, &row_access_policy(&denied, &role)).await;
    let address = published_endpoint(&router, domain).await;
    let path = format!("/{}/_doc/{}", denied.index, denied.document_id);
    let document = row_document(&denied);

    let refused = at_address(
        &router,
        &address,
        &writer.access_key_id,
        "PUT",
        &path,
        Some(document.clone()),
    )
    .await;
    assert_resource_deny(&refused, &writer.principal_arn, "es:ESHttpPut", denied.case);
    let absent = at_address(
        &router,
        &address,
        &writer.access_key_id,
        "GET",
        &path,
        None,
    )
    .await;
    assert_eq!(
        absent.status, 404,
        "case {:?}: expected the denied write to have left no document, got {}",
        denied.case,
        absent.summary()
    );

    // THE ONE CHANGE: the access policy is replaced with a plain allow. The role's identity
    // grant, the domain and the request are untouched.
    replace_access_policy(&router, domain, &plain_allow_policy(domain, &role)).await;
    let after = published_endpoint(&router, domain).await;
    assert_eq!(
        after, address,
        "case \"after the policy is loosened\": replacing an access policy must not move the \
         address the domain publishes"
    );

    let served = at_address(
        &router,
        &after,
        &writer.access_key_id,
        "PUT",
        &path,
        Some(document.clone()),
    )
    .await;
    assert_eq!(
        (served.status, served.body["result"].as_str()),
        (201, Some("created")),
        "case \"the identical write once the policy allows it\": expected the cluster to index \
         it, got {}",
        served.summary()
    );
    let present = at_address(&router, &after, &writer.access_key_id, "GET", &path, None).await;
    assert_eq!(
        (
            present.status,
            present.body["found"].as_bool(),
            &present.body["_source"]
        ),
        (200, Some(true), &document),
        "case \"the identical write once the policy allows it\": expected the document to read \
         back with its exact source, got {}",
        present.summary()
    );
}
