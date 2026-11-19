// SPDX-License-Identifier: CC0-1.0

//! The address an Amazon OpenSearch Service domain PUBLISHES for its own data plane, and the
//! grant that decides what a caller may do there — driven end to end on the assembled
//! emulator, the way a workload is wired: read `DomainStatus$Endpoint` from the control
//! plane, then index and read documents at that address with a credential IAM and STS answer
//! for.
//!
//! Two rules, each asserted over a SET of inputs rather than one recorded call:
//!
//! * **the host a control-plane read publishes for a domain's data plane serves THAT
//!   domain's HTTP document surface**: `PUT /<index>/_doc/<id>` indexes the body under the
//!   index and id the request carries (`201` on the first write, `200` once it replaces a
//!   document), `GET /<index>/_doc/<id>` answers `200` with `"found": true` and the stored
//!   source, and `POST /<index>/_doc` indexes under an id the cluster mints and answers it —
//!   while the address another domain publishes holds none of those documents. The rows span
//!   the index name, the document id, the shape of the document and which domain was
//!   addressed, so nothing in the answer can be fixed;
//! * **a signed data-plane request is admitted when a policy in force allows the `es:ESHttp*`
//!   action its METHOD asks for on the resource its PATH names — the principal's
//!   identity-based policies or the domain's own access policy, either alone being enough
//!   within one account — and refused `403` only when neither allows**, with AWS's own
//!   sentence naming the principal that signed, that action, and which rule refused. The rows
//!   span which side carries the grant, the method the action is read from, and both
//!   directions of the one change: with the identity grant in force the write is indexed, and
//!   once that grant is removed the same request is refused — unless the domain's access
//!   policy admits the principal on its own, which keeps it served.
//!
//! Only the assembled router can pin either: IAM holds the roles and their inline policies,
//! STS mints the session credential and names the principal it speaks for, and Amazon
//! OpenSearch Service publishes the address and serves the domain at it.

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
const SESSION_NAME: &str = "indexer";

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

/// The name of the inline policy every grant here is held under, so the same name removes it.
const GRANT_NAME: &str = "data-plane";

/// Create `name` as a role this account may assume, carrying `grant` as its one inline
/// policy where it carries a grant at all, and answer the role ARN IAM recorded.
async fn created_role(router: &MultiServiceRouter, name: &str, grant: Option<Value>) -> String {
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
    if let Some(document) = grant {
        let attached = query(
            router,
            "iam",
            format!(
                "Action=PutRolePolicy&Version={IAM_VERSION}&RoleName={name}\
                 &PolicyName={GRANT_NAME}&PolicyDocument={}",
                form_encode(&document.to_string())
            ),
        )
        .await;
        assert_eq!(
            attached.status, 200,
            "setup PutRolePolicy({name:?}): expected 200, got {}",
            attached.summary()
        );
    }
    arn
}

/// Remove the inline grant from `name` — the detach a client makes when it takes a role's
/// permission away.
async fn removed_grant(router: &MultiServiceRouter, name: &str) {
    let removed = query(
        router,
        "iam",
        format!(
            "Action=DeleteRolePolicy&Version={IAM_VERSION}&RoleName={name}&PolicyName={GRANT_NAME}"
        ),
    )
    .await;
    assert_eq!(
        removed.status, 200,
        "setup DeleteRolePolicy({name:?}): expected 200, got {}",
        removed.summary()
    );
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

/// The ARN pattern an access policy scopes a statement to one domain's own paths by.
fn domain_resource(domain: &str) -> String {
    format!(
        "arn:aws:es:{REGION}:{}:domain/{domain}/*",
        engine::emulator_account_id()
    )
}

/// An identity-based grant of this service's data-plane actions on `domain`.
fn data_plane_grant(domain: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": "es:ESHttp*",
            "Resource": domain_resource(domain),
        }],
    })
}

/// A domain access policy admitting each principal of `principals` on the domain's own paths.
fn access_policy(domain: &str, principals: &[&str]) -> String {
    let statements: Vec<Value> = principals
        .iter()
        .map(|principal| {
            json!({
                "Effect": "Allow",
                "Principal": { "AWS": principal },
                "Action": "es:ESHttp*",
                "Resource": domain_resource(domain),
            })
        })
        .collect();
    json!({ "Version": "2012-10-17", "Statement": statements }).to_string()
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
            "ClusterConfig": { "InstanceType": "t3.small.search", "InstanceCount": 1 },
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

/// One document to index at a published address: where it goes, what it carries, and the
/// replacement that must land on the same id.
struct DocumentRow {
    case: &'static str,
    index: &'static str,
    document_id: &'static str,
    document: Value,
    replacement: Value,
}

/// The table for the first rule: the index name, the document id and the shape of the
/// document all differ, so nothing the answers carry can be a fixed value.
fn document_rows() -> Vec<DocumentRow> {
    vec![
        DocumentRow {
            case: "a flat document under a plain index name and a numeric id",
            index: "catalog",
            document_id: "1",
            document: json!({ "title": "a widget", "qty": 7 }),
            replacement: json!({ "title": "a widget", "qty": 4 }),
        },
        DocumentRow {
            case: "a document with a list, under a hyphenated index and id",
            index: "orders-2026",
            document_id: "order-42",
            document: json!({ "total": 19.5, "items": ["a", "b"] }),
            replacement: json!({ "total": 21.0, "items": ["a", "b", "c"] }),
        },
        DocumentRow {
            case: "a nested document under a dotted index name",
            index: "logs.app",
            document_id: "9f0c1e42",
            document: json!({ "level": "warn", "at": { "host": "node-1", "pid": 9 } }),
            replacement: json!({ "level": "error", "at": { "host": "node-1", "pid": 9 } }),
        },
        DocumentRow {
            case: "a document whose id carries an underscore",
            index: "profiles",
            document_id: "user_7",
            document: json!({ "user": { "name": "ada", "tags": ["admin"] } }),
            replacement: json!({ "user": { "name": "ada", "tags": [] } }),
        },
    ]
}

/// RULE: the host a control-plane read publishes for a domain's data plane serves that
/// domain's own document surface — a write at the published address is indexed under the
/// index and id the request carries and reads back as stored, a write with no id is indexed
/// under an id the cluster mints, and the address another domain publishes holds none of it.
#[tokio::test]
async fn the_published_endpoint_serves_the_documents_written_through_it() {
    let router = build_router();
    let (first, second) = ("published-first", "published-second");
    let role = created_role(&router, "published-endpoint-writer", None).await;
    let writer = assumed(&router, &role).await;
    for domain in [first, second] {
        create_domain(&router, domain, &access_policy(domain, &[role.as_str()])).await;
    }
    let first_address = published_endpoint(&router, first).await;
    let second_address = published_endpoint(&router, second).await;
    assert_ne!(
        first_address, second_address,
        "case \"two domains\": each domain must publish its own address, got {first_address:?} \
         for both"
    );

    for row in document_rows() {
        let case = row.case;
        let path = format!("/{}/_doc/{}", row.index, row.document_id);
        let written = at_address(
            &router,
            &first_address,
            &writer.access_key_id,
            "PUT",
            &path,
            Some(row.document.clone()),
        )
        .await;
        assert_eq!(
            (written.status, written.body["result"].as_str()),
            (201, Some("created")),
            "case {case:?}: expected the write at the published address to index the document, \
             got {}",
            written.summary()
        );

        let read = at_address(
            &router,
            &first_address,
            &writer.access_key_id,
            "GET",
            &path,
            None,
        )
        .await;
        assert_eq!(
            (
                read.status,
                read.body["found"].as_bool(),
                &read.body["_source"]
            ),
            (200, Some(true), &row.document),
            "case {case:?}: expected the document to read back at the published address, got {}",
            read.summary()
        );

        // The same id written again REPLACES the document, which is the other half of the
        // surface a client uses: the answer says so and the read reports the new source.
        let replaced = at_address(
            &router,
            &first_address,
            &writer.access_key_id,
            "PUT",
            &path,
            Some(row.replacement.clone()),
        )
        .await;
        assert_eq!(
            (
                replaced.status,
                replaced.body["result"].as_str(),
                replaced.body["_version"].as_i64()
            ),
            (200, Some("updated"), Some(2)),
            "case {case:?}: expected the second write to replace the document, got {}",
            replaced.summary()
        );
        let reread = at_address(
            &router,
            &first_address,
            &writer.access_key_id,
            "GET",
            &path,
            None,
        )
        .await;
        assert_eq!(
            &reread.body["_source"], &row.replacement,
            "case {case:?}: expected the replacement as the stored source, got {}",
            reread.summary()
        );

        // And the OTHER domain's published address holds none of it: each address addresses
        // the data plane of the domain that published it.
        let elsewhere = at_address(
            &router,
            &second_address,
            &writer.access_key_id,
            "GET",
            &path,
            None,
        )
        .await;
        assert_eq!(
            elsewhere.status, 404,
            "case {case:?}: expected the second domain to hold nothing written to the first, \
             got {}",
            elsewhere.summary()
        );
        assert_ne!(
            elsewhere.body["found"].as_bool(),
            Some(true),
            "case {case:?}: the second domain must report no such document, got {}",
            elsewhere.summary()
        );
    }

    // A write carrying NO id is indexed under an id the cluster mints, and that id is what
    // the document is readable under afterwards.
    let minted = at_address(
        &router,
        &first_address,
        &writer.access_key_id,
        "POST",
        "/catalog/_doc",
        Some(json!({ "title": "minted" })),
    )
    .await;
    let document_id = minted.body["_id"].as_str().unwrap_or_default().to_string();
    assert_eq!(
        (minted.status, minted.body["result"].as_str()),
        (201, Some("created")),
        "case \"a write with no document id\": expected the cluster to index it, got {}",
        minted.summary()
    );
    assert!(
        !document_id.is_empty(),
        "case \"a write with no document id\": expected the id the cluster minted, got {}",
        minted.summary()
    );
    let minted_read = at_address(
        &router,
        &first_address,
        &writer.access_key_id,
        "GET",
        &format!("/catalog/_doc/{document_id}"),
        None,
    )
    .await;
    assert_eq!(
        (
            minted_read.status,
            minted_read.body["found"].as_bool(),
            minted_read.body["_source"]["title"].as_str()
        ),
        (200, Some(true), Some("minted")),
        "case \"a write with no document id\": expected the minted id to resolve to the \
         document, got {}",
        minted_read.summary()
    );
}

/// One request whose verdict the grants in force decide: who signs it, what it asks, and
/// the action the sentence must name when it is refused.
struct VerdictRow {
    case: &'static str,
    /// Which seeded principal signs: the one granted by its identity policy, the one the
    /// domain's access policy admits, or the one neither names.
    signer: Signer,
    method: &'static str,
    path: &'static str,
    /// The `es:ESHttp*` action the method asks for, which a refusal's sentence names.
    action: &'static str,
    /// The status before the identity grant is removed, and after it.
    before: u16,
    after: u16,
}

/// Which seeded principal a row signs as.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Signer {
    /// A role whose own identity policy grants the action; the access policy names it
    /// nowhere.
    IdentityGranted,
    /// A role the domain's access policy admits by name.
    PolicyAdmitted,
    /// A role neither side names.
    Ungranted,
}

/// The index every row of the second rule writes into and reads.
const GRANT_INDEX: &str = "products";

/// The table for the second rule: which side carries the grant, the method the action is
/// read from, and what each verdict becomes once the identity grant is removed — the one
/// change the rows observe from both sides.
const VERDICT_ROWS: &[VerdictRow] = &[
    VerdictRow {
        case: "a write by a role its own identity policy grants",
        signer: Signer::IdentityGranted,
        method: "POST",
        path: "/_doc",
        action: "es:ESHttpPost",
        before: 201,
        after: 403,
    },
    VerdictRow {
        case: "a write to a named id by a role its own identity policy grants",
        signer: Signer::IdentityGranted,
        method: "PUT",
        path: "/_doc/kept",
        action: "es:ESHttpPut",
        before: 201,
        after: 403,
    },
    VerdictRow {
        case: "a read by a role its own identity policy grants",
        signer: Signer::IdentityGranted,
        method: "GET",
        path: "/_doc/kept",
        action: "es:ESHttpGet",
        before: 200,
        after: 403,
    },
    VerdictRow {
        case: "a write by a role the domain's access policy admits",
        signer: Signer::PolicyAdmitted,
        method: "POST",
        path: "/_doc",
        action: "es:ESHttpPost",
        // The access policy allows it on its own, so removing the identity grant changes
        // nothing: within one account either side allowing is enough.
        before: 201,
        after: 201,
    },
    VerdictRow {
        case: "a write by a role neither side names",
        signer: Signer::Ungranted,
        method: "POST",
        path: "/_doc",
        action: "es:ESHttpPost",
        before: 403,
        after: 403,
    },
    VerdictRow {
        case: "a read by a role neither side names",
        signer: Signer::Ungranted,
        method: "GET",
        path: "/_doc/kept",
        action: "es:ESHttpGet",
        before: 403,
        after: 403,
    },
];

/// The body a row sends, if any: a write carries a document naming the case it came from,
/// and a read carries nothing.
fn row_body(row: &VerdictRow) -> Option<Value> {
    (row.method != "GET").then(|| json!({ "case": row.case }))
}

/// Drive every row at `address` and assert the verdict the grants in force give it —
/// `expected` is the status, and a refusal must carry AWS's own sentence for the action the
/// row's method asks for.
async fn assert_verdicts(
    router: &MultiServiceRouter,
    address: &str,
    sessions: &[(Signer, &Session)],
    expected: fn(&VerdictRow) -> u16,
    stage: &str,
) {
    for row in VERDICT_ROWS {
        let case = row.case;
        let session = sessions
            .iter()
            .find(|(signer, _)| *signer == row.signer)
            .map(|(_, session)| *session)
            .expect("every row's signer is seeded");
        let answered = at_address(
            router,
            address,
            &session.access_key_id,
            row.method,
            &format!("/{GRANT_INDEX}{}", row.path),
            row_body(row),
        )
        .await;
        let status = expected(row);
        assert_eq!(
            answered.status, status,
            "case {case:?} ({stage}): expected HTTP {status} at the published address, got {}",
            answered.summary()
        );
        if status != 403 {
            continue;
        }
        assert_eq!(
            answered.message(),
            format!(
                "User: {} is not authorized to perform: {} because no resource-based policy \
                 allows the {} action",
                session.principal_arn, row.action, row.action
            ),
            "case {case:?} ({stage}): expected AWS's own refusal sentence, got {}",
            answered.summary()
        );
    }
}

/// RULE: a signed data-plane request is admitted when a policy in force allows the
/// `es:ESHttp*` action its method asks for — the principal's identity policies or the
/// domain's access policy, either alone being enough — and refused `403` with AWS's own
/// sentence only when neither allows; so removing the identity grant turns the same request
/// from indexed to refused, while a principal the access policy admits stays served.
#[tokio::test]
async fn the_grant_in_force_decides_the_write_at_the_published_endpoint() {
    let router = build_router();
    let domain = "grant-decides";
    let identity_granted_role =
        created_role(&router, "grant-identity", Some(data_plane_grant(domain))).await;
    let policy_admitted_role =
        created_role(&router, "grant-admitted", Some(data_plane_grant(domain))).await;
    let ungranted_role = created_role(&router, "grant-none", None).await;
    let identity_granted = assumed(&router, &identity_granted_role).await;
    let policy_admitted = assumed(&router, &policy_admitted_role).await;
    let ungranted = assumed(&router, &ungranted_role).await;
    // The domain's access policy names exactly ONE of the three roles, so the other two are
    // decided by their own identity policies alone.
    create_domain(
        &router,
        domain,
        &access_policy(domain, &[policy_admitted_role.as_str()]),
    )
    .await;
    let address = published_endpoint(&router, domain).await;
    let sessions = [
        (Signer::IdentityGranted, &identity_granted),
        (Signer::PolicyAdmitted, &policy_admitted),
        (Signer::Ungranted, &ungranted),
    ];

    assert_verdicts(
        &router,
        &address,
        &sessions,
        |row| row.before,
        "with the identity grant in force",
    )
    .await;

    // What the admitted writes left behind, as the cluster reports it — the number every
    // later count is compared against, read rather than assumed.
    let counted = at_address(
        &router,
        &address,
        &policy_admitted.access_key_id,
        "GET",
        &format!("/{GRANT_INDEX}/_count"),
        None,
    )
    .await;
    let indexed = counted.body["count"].as_i64().unwrap_or_default();
    assert!(
        indexed > 0,
        "case \"after the admitted writes\": expected the index to hold the documents they \
         wrote, got {}",
        counted.summary()
    );

    // THE ONE CHANGE: the inline grant is removed from both roles that carry one. The
    // domain, its access policy and the published address are untouched.
    for role in ["grant-identity", "grant-admitted"] {
        removed_grant(&router, role).await;
    }
    assert_eq!(
        published_endpoint(&router, domain).await,
        address,
        "case \"after the grants are removed\": removing an identity grant must not move the \
         address the domain publishes"
    );

    assert_verdicts(
        &router,
        &address,
        &sessions,
        |row| row.after,
        "once the identity grant is removed",
    )
    .await;

    // Nothing a refused row asked for happened: the only writes that landed are the ones the
    // policy in force admitted.
    let recounted = at_address(
        &router,
        &address,
        &policy_admitted.access_key_id,
        "GET",
        &format!("/{GRANT_INDEX}/_count"),
        None,
    )
    .await;
    assert_eq!(
        recounted.body["count"].as_i64(),
        Some(indexed + 1),
        "case \"after the refused rows\": expected only the one write the access policy still \
         admits to have landed, got {}",
        recounted.summary()
    );
}
