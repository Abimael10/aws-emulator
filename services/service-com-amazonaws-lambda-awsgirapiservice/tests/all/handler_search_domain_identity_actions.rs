// SPDX-License-Identifier: CC0-1.0

//! Which of a RUNNING handler's own signed requests to an Amazon OpenSearch Service domain
//! the identity policy in force on its execution role admits: Lambda, IAM, the token service
//! and that service on ONE engine, a function whose handler signs and sends an index write
//! and then an index read of the same document, and the domain's own data plane deciding
//! each of them.
//!
//! RULE: a signed request to a domain's REST data plane asks for the ONE `es:ESHttp<Method>`
//! action its HTTP method names, so the policies in force on the calling principal admit
//! exactly those requests whose action they allow and refuse every other with `403` — as the
//! ANSWER to that request, never as a request left outstanding — a refused write stores no
//! document, and the policies are read when the request is decided, so rewriting the
//! caller's OWN document changes the verdict of the next request with the domain's access
//! policy untouched.
//!
//! The rows differ along what the rule generalises over: WHICH data-plane actions the role's
//! own document names (the read action alone, the write action alone, both of them, the
//! wildcard every one of them matches, that wildcard beside an explicit deny of the write,
//! and a document naming nothing of this service) and WHICH action each of the two requests
//! the handler makes asks for. Every row asserts the status the domain answered the write
//! and the read with, the sentence a refusal carries, and whether the DOCUMENT is there
//! afterwards — read back through the domain's own data plane, so a write that answered
//! without storing anything and a refusal that stored something both fail here.
//!
//! The domain's access policy names a separate control role throughout and is never
//! rewritten, so every verdict below is decided on the IDENTITY side; the last phase proves
//! that by re-running the write the read-only document refused, once the write action is
//! added to that same role's document, and reading the access policy back unchanged.
//!
//! Every wait here is for a CONDITION and never for a span — the domain publishing an
//! endpoint, the function reaching `Active` — looked at again until it holds or a generous
//! deadline passes, so what the rows assert cannot be decided by how fast or how loaded this
//! host happens to be.
//!
//! The suite RUNS a handler, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::{given_role, iam};
use crate::event_source_mappings::{await_function_active, call, router_on};

/// Where `CreateFunction` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// Where the search service's `CreateDomain` and `DescribeDomain` are bound.
const DOMAIN_URI: &str = "/2021-01-01/opensearch/domain";

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The domain every request below addresses, and the engine release it runs.
const DOMAIN: &str = "fn-identity-actions";
const ENGINE_VERSION: &str = "OpenSearch_2.11";

/// The index the handler writes its document into.
const INDEX: &str = "permissions";

/// The role the function runs as, and the policy name its one document is held under.
const EXECUTION_ROLE: &str = "fn-identity-actions-exec";
const POLICY_NAME: &str = "search-data-plane";

/// The role the DOMAIN's access policy names: a separate control role, so the domain's own
/// policy says nothing about the role the function runs as.
const CONTROL_ROLE: &str = "fn-identity-actions-control";

/// The function the handler runs in, which is also the name of the assumed-role session its
/// own requests are signed as.
const FUNCTION: &str = "fn-identity-actions-writer";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is the authorization decision and not how
/// fast an interpreter gets through the handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// The credential the suite's OWN requests carry: a key no store holds, which is the
/// account's own published credential.
const ACCOUNT_ACCESS_KEY_ID: &str = "AKIAIOSFODNN7EXAMPLE";

/// How often to look while waiting for work a service's own clock owes, and how long to keep
/// looking. The span is the service's own, so nothing here sleeps for a fixed span expecting
/// work to be done.
const LOOK: Duration = Duration::from_millis(100);
const LOOK_DEADLINE: Duration = Duration::from_secs(90);

/// The `es:ESHttp*` action each of the handler's two requests asks for: one per HTTP method,
/// from "Actions defined by Amazon OpenSearch Service" (IAM Service Authorization Reference).
const WRITE_ACTION: &str = "es:ESHttpPut";
const READ_ACTION: &str = "es:ESHttpGet";

/// The handler every row runs: it SIGNS an index write and an index read of its own with the
/// credentials the runtime hands it and reports what the domain answered each with, so a
/// refusal is the probe's answer rather than an exception.
const HANDLER_SOURCE: &str = r#"
import json
import os
import urllib.error
import urllib.request

import botocore.session
from botocore.auth import SigV4Auth
from botocore.awsrequest import AWSRequest


def _sent(method, url, body):
    """Sign `method` on `url` as this runtime's credentials and report the answer."""
    signed = AWSRequest(
        method=method, url=url, data=body, headers={"Content-Type": "application/json"}
    )
    credentials = botocore.session.get_session().get_credentials()
    SigV4Auth(credentials, "es", os.environ["AWS_REGION"]).add_auth(signed)
    sending = urllib.request.Request(signed.url, data=body, method=method)
    for name, value in signed.headers.items():
        sending.add_header(name, value)
    try:
        with urllib.request.urlopen(sending) as answered:
            return {"status": answered.status, "body": answered.read().decode("utf-8")}
    except urllib.error.HTTPError as refused:
        return {"status": refused.code, "body": refused.read().decode("utf-8")}


def lambda_handler(event, context):
    url = "https://%s/%s/_doc/%s" % (
        os.environ["SEARCH_ENDPOINT"],
        event["index"],
        event["id"],
    )
    return {
        "write": _sent("PUT", url, json.dumps(event["document"]).encode("utf-8")),
        "read": _sent("GET", url, None),
    }
"#;

/// Lambda, IAM, the token service and Amazon OpenSearch Service over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them: the requests the handler signs are dispatched back
/// into this same router, so the domain it writes to is the domain this suite created, and
/// the role it runs as is the role IAM holds.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(&engine);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_opensearch_amazonopensearchservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names the search service, which is
/// what routes the request. The signature is never checked: request authentication is off.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={ACCOUNT_ACCESS_KEY_ID}/20260101/{REGION}/es/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One control-plane call of the search service, over its `restJson1` wire.
async fn opensearch(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("es.{REGION}.amazonaws.com"))
        .header("authorization", authorization())
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(payload))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (
        status,
        serde_json::from_slice(response.body()).unwrap_or(Value::Null),
    )
}

/// One data-plane request to the ADDRESS the control plane published for the domain, dialled
/// as a client dials it: the authority of the published value is the host, and whatever it
/// carries after that is the prefix of the request path.
async fn at_domain(
    router: &MultiServiceRouter,
    address: &str,
    method: &str,
    path: &str,
) -> (u64, Value) {
    let (authority, prefix) = match address.find('/') {
        Some(end) => (&address[..end], &address[end..]),
        None => (address, ""),
    };
    let request = http::Request::builder()
        .method(method)
        .uri(format!("{prefix}{path}"))
        .header("host", authority)
        .header("authorization", authorization())
        .header("content-type", "application/json")
        .body(bytes::Bytes::new())
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = u64::from(response.status().as_u16());
    (
        status,
        serde_json::from_slice(response.body()).unwrap_or(Value::Null),
    )
}

/// The ARN of a role of this account.
fn role_arn(name: &str) -> String {
    format!(
        "arn:aws:iam::{}:role/{name}",
        ::engine::emulator_account_id()
    )
}

/// The domain's ARN, which every policy document here names paths under.
fn domain_arn() -> String {
    format!(
        "arn:aws:es:{REGION}:{}:domain/{DOMAIN}",
        ::engine::emulator_account_id()
    )
}

/// The principal a refusal of the handler's own request blames: the assumed-role session
/// AWS reports for a function's calls.
fn session_arn() -> String {
    format!(
        "arn:aws:sts::{}:assumed-role/{EXECUTION_ROLE}/{FUNCTION}",
        ::engine::emulator_account_id()
    )
}

/// The access policy the DOMAIN carries for every row: the control role allowed on every
/// path of the domain, and nothing said about the role the function runs as.
fn domain_access_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "AWS": role_arn(CONTROL_ROLE) },
            "Action": "es:ESHttp*",
            "Resource": format!("{}/*", domain_arn()),
        }],
    })
    .to_string()
}

/// Create the domain every row addresses.
async fn given_domain(router: &MultiServiceRouter, case: &str, access_policy: &str) {
    let (status, created) = opensearch(
        router,
        "POST",
        DOMAIN_URI,
        Some(json!({
            "DomainName": DOMAIN,
            "EngineVersion": ENGINE_VERSION,
            "ClusterConfig": { "InstanceType": "t3.small.search", "InstanceCount": 1 },
            "EBSOptions": { "EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 10 },
            "AccessPolicies": access_policy,
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: setup: CreateDomain({DOMAIN:?}) must succeed, got {status} with {created}"
    );
}

/// `DescribeDomain`, as a client reads the domain back.
async fn described(router: &MultiServiceRouter) -> (u16, Value) {
    opensearch(router, "GET", &format!("{DOMAIN_URI}/{DOMAIN}"), None).await
}

/// The address the control plane publishes for the domain's data plane, waited for as the
/// CONDITION it is: a domain publishes none until the cluster behind it is up.
async fn published_endpoint(router: &MultiServiceRouter, case: &str) -> String {
    let deadline = Instant::now() + LOOK_DEADLINE;
    loop {
        let (status, read) = described(router).await;
        let published = read["DomainStatus"]["Endpoint"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if !published.is_empty() {
            return published;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: setup: the domain must publish an endpoint once it is up, last read \
             answered {status} with {read}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// Create the function the handler runs in, pointed at the published address.
async fn given_function(router: &MultiServiceRouter, case: &str, role: &str, endpoint: &str) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": FUNCTION,
            "Role": role,
            "Runtime": "python3.12",
            "Handler": "lambda_function.lambda_handler",
            "Timeout": FUNCTION_TIMEOUT,
            "Environment": { "Variables": { "SEARCH_ENDPOINT": endpoint } },
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(crate::synchronous_invoke_run::package(HANDLER_SOURCE)),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: setup: CreateFunction({FUNCTION:?}) must answer 201, got {} {}",
        created.status,
        created.text()
    );
    await_function_active(router, case, FUNCTION).await;
}

/// Put `document` in force as the execution role's ONE identity policy, which is the only
/// thing a row changes.
async fn put_role_policy(router: &MultiServiceRouter, case: &str, document: &str) {
    let (status, written) = iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", EXECUTION_ROLE),
            ("PolicyName", POLICY_NAME),
            ("PolicyDocument", document),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutRolePolicy on {EXECUTION_ROLE:?} must answer 200, got {status}: \
         {written}"
    );
}

/// The statuses Lambda answers an invocation it has NOT served with, each of which says "not
/// now" and carries no decision about the requests the handler would have made: the account's
/// concurrent-execution throughput was reached (`TooManyRequestsException`, `@httpError(429)`),
/// or the function is not `Active` yet (`ResourceConflictException`, `@httpError(409)`).
///
/// Whether one arrives is decided by what the rest of this process is running at that instant
/// and by nothing a row varies, so a probe LOOKS AGAIN on one instead of reading an
/// authorization decision out of a run that never happened. Nothing is tolerated by looking
/// again: one that still stands at the deadline fails the row.
const UNSERVED: &[u16] = &[409, 429];

/// Invoke the function over `id` and answer what the handler reported.
async fn invoked(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let event = json!({
        "index": INDEX,
        "id": id,
        "document": { "id": id, "case": case },
    });
    let deadline = Instant::now() + LOOK_DEADLINE;
    loop {
        let answered = call(
            router,
            "POST",
            &format!("{FUNCTIONS_URI}/{FUNCTION}/invocations"),
            &event,
        )
        .await;
        if UNSERVED.contains(&answered.status) {
            assert!(
                Instant::now() < deadline,
                "case {case:?}: Lambda must serve this probe's invocation, last answered {} {}",
                answered.status,
                answered.text()
            );
            tokio::time::sleep(LOOK).await;
            continue;
        }
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: the invocation must be served, got {} {}",
            answered.status,
            answered.text()
        );
        let faulted = answered.header("x-amz-function-error");
        assert_eq!(
            faulted,
            None,
            "case {case:?}: the handler must run to the end, got {faulted:?} with {}",
            answered.text()
        );
        return answered.json();
    }
}

/// What the handler reported for one of its two requests: the status the domain answered and
/// the body it carried.
fn step(reported: &Value, name: &str, case: &str) -> (u64, String) {
    let answered = &reported[name];
    let status = answered["status"].as_u64().unwrap_or_else(|| {
        panic!("case {case:?}: expected a status for the {name}, got {reported}")
    });
    (
        status,
        answered["body"].as_str().unwrap_or_default().to_string(),
    )
}

/// The `Message` an IAM-layer refusal of a data-plane request carries.
fn message(body: &str) -> String {
    let answered = serde_json::from_str::<Value>(body).unwrap_or(Value::Null);
    answered["Message"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The status the domain answers an admitted write of a document the index does not hold
/// with — a create — and the one it answers an admitted read of a document that is there
/// with. Every row addresses a document id of its own, so an admitted write here is always a
/// create and neither status is a range.
const CREATED: u64 = 201;
const FOUND: u64 = 200;

/// What the domain must answer one of the handler's two requests with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// The request is admitted and the domain answers it with this status.
    Admitted(u64),
    /// IAM refuses it `403` before the cluster is asked anything.
    Refused,
    /// It is admitted, and the document it addresses is not there (`404`) — which is what a
    /// read of the document a refused write never stored answers.
    Missing,
}

/// One row: the actions the execution role's own document names, and what the domain must
/// answer each of the handler's two requests with.
struct Row {
    case: &'static str,
    /// The actions its one `Allow` names, written as a client writes them.
    allowed: &'static [&'static str],
    /// The actions it explicitly `Deny`s beside them.
    denied: &'static [&'static str],
    write: Expect,
    read: Expect,
}

/// The rows: every position the data-plane actions of this service can be in in the caller's
/// own document. The action names are the `ESHttp*` actions of "Actions defined by Amazon
/// OpenSearch Service" (IAM Service Authorization Reference), one per HTTP method, and the
/// last row names an action of another service entirely.
const ROWS: &[Row] = &[
    Row {
        case: "a document allowing only the read action",
        allowed: &[READ_ACTION],
        denied: &[],
        write: Expect::Refused,
        read: Expect::Missing,
    },
    Row {
        case: "a document allowing only the write action",
        allowed: &[WRITE_ACTION],
        denied: &[],
        write: Expect::Admitted(CREATED),
        read: Expect::Refused,
    },
    Row {
        case: "a document allowing both the write and the read action",
        allowed: &[WRITE_ACTION, READ_ACTION],
        denied: &[],
        write: Expect::Admitted(CREATED),
        read: Expect::Admitted(FOUND),
    },
    Row {
        case: "the wildcard every data-plane action of the service matches",
        allowed: &["es:ESHttp*"],
        denied: &[],
        write: Expect::Admitted(CREATED),
        read: Expect::Admitted(FOUND),
    },
    Row {
        case: "that wildcard beside an explicit deny of the write action",
        allowed: &["es:ESHttp*"],
        denied: &[WRITE_ACTION],
        write: Expect::Refused,
        read: Expect::Missing,
    },
    Row {
        case: "a document naming nothing of this service at all",
        allowed: &["logs:CreateLogStream"],
        denied: &[],
        write: Expect::Refused,
        read: Expect::Refused,
    },
];

/// The document one row puts in force on the execution role: its allow over every path of the
/// domain, and the explicit deny it carries beside it.
fn identity_document(row: &Row) -> String {
    let resource = format!("{}/*", domain_arn());
    let mut statements = vec![json!({
        "Effect": "Allow",
        "Action": row.allowed,
        "Resource": resource,
    })];
    if !row.denied.is_empty() {
        statements.push(json!({
            "Effect": "Deny",
            "Action": row.denied,
            "Resource": resource,
        }));
    }
    json!({ "Version": "2012-10-17", "Statement": statements }).to_string()
}

/// The sentence the domain refuses `action` with for one row: the principal that signed, the
/// action its method asks for, and WHICH of IAM's rules refused — an explicit deny names the
/// side it was read from, and with neither side allowing, the sentence names the kind of
/// policy that would have had to allow, which for a domain carrying an access policy is the
/// resource-based one.
fn refusal_sentence(row: &Row, action: &str) -> String {
    let reason = if row.denied.contains(&action) {
        "with an explicit deny in an identity-based policy".to_string()
    } else {
        format!("because no resource-based policy allows the {action} action")
    };
    format!(
        "User: {} is not authorized to perform: {action} {reason}",
        session_arn()
    )
}

/// Assert the domain answered one of the handler's two requests as the row states.
fn assert_answered(case: &str, name: &str, expect: Expect, answered: (u64, String), refusal: &str) {
    let (status, body) = answered;
    match expect {
        Expect::Admitted(expected) => assert_eq!(
            status, expected,
            "case {case:?}: the {name} the role's own document allows must be answered \
             {expected} by the domain, got {status} with {body:?}"
        ),
        Expect::Refused => {
            assert_eq!(
                status, 403,
                "case {case:?}: the {name} nothing in force allows must be refused 403, got \
                 {status} with {body:?}"
            );
            let carried = message(&body);
            assert_eq!(
                carried, refusal,
                "case {case:?}: the refusal of the {name} must name the principal, the action and \
                 the rule that refused, got {carried:?}"
            );
        }
        Expect::Missing => assert_eq!(
            status, 404,
            "case {case:?}: the {name} must report the document the refused write never stored as \
             absent, got {status} with {body:?}"
        ),
    }
}

/// The document id one row's requests address: its own, so what every other row wrote or
/// failed to write says nothing about it.
fn document_id(ordinal: usize) -> String {
    format!("document-{ordinal}")
}

/// Every row, and then the rewrite: the actions the caller's own document names decide each
/// request the handler signs, a refused write stores nothing, and adding the write action to
/// that same document admits the write it had refused — with the domain's access policy
/// untouched throughout.
#[tokio::test]
async fn the_actions_an_identity_policy_names_decide_each_request_a_handler_signs() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let setup = "a function signing its own index write and read";
    let router = emulator();
    given_domain(&router, setup, &domain_access_policy()).await;
    let endpoint = published_endpoint(&router, setup).await;
    let (status, created) = described(&router).await;
    assert_eq!(
        status, 200,
        "case {setup:?}: setup: DescribeDomain must answer 200, got {status} with {created}"
    );
    let access_policy = created["DomainStatus"]["AccessPolicies"].clone();
    given_role(&router, setup, CONTROL_ROLE, None).await;
    let role = given_role(&router, setup, EXECUTION_ROLE, None).await;
    given_function(&router, setup, &role, &endpoint).await;

    for (ordinal, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let id = document_id(ordinal);
        put_role_policy(&router, case, &identity_document(row)).await;
        let reported = invoked(&router, case, &id).await;
        assert_answered(
            case,
            "write",
            row.write,
            step(&reported, "write", case),
            &refusal_sentence(row, WRITE_ACTION),
        );
        assert_answered(
            case,
            "read",
            row.read,
            step(&reported, "read", case),
            &refusal_sentence(row, READ_ACTION),
        );
        if matches!(row.read, Expect::Admitted(_)) {
            let (_, body) = step(&reported, "read", case);
            let read = serde_json::from_str::<Value>(&body).unwrap_or(Value::Null);
            assert_eq!(
                read["_source"]["case"],
                json!(case),
                "case {case:?}: the read the document allows must report the document the write \
                 stored, got {body:?}"
            );
        }

        // What the DOMAIN itself holds for that id, read through its own data plane: the
        // document is there exactly when the write was admitted.
        let (status, held) =
            at_domain(&router, &endpoint, "GET", &format!("/{INDEX}/_doc/{id}")).await;
        if matches!(row.write, Expect::Admitted(_)) {
            assert_eq!(
                status, FOUND,
                "case {case:?}: the document an admitted write stored must be held by the domain, \
                 got {status} with {held}"
            );
            assert_eq!(
                held["_source"]["case"],
                json!(case),
                "case {case:?}: the stored document must be the one the handler wrote, got {held}"
            );
        } else {
            assert_eq!(
                status, 404,
                "case {case:?}: the document a refused write never stored must not be held by the \
                 domain, got {status} with {held}"
            );
        }
    }

    // The write the read-only document refused succeeds once the write action is added to
    // that SAME role's own document, and the document lands.
    let added = "the write action added to the document that refused the write";
    let refused = document_id(0);
    put_role_policy(&router, added, &identity_document(&ROWS[2])).await;
    let reported = invoked(&router, added, &refused).await;
    let (status, body) = step(&reported, "write", added);
    assert_eq!(
        status, CREATED,
        "case {added:?}: the identical write must be admitted once the action is granted, got \
         {status} with {body:?}"
    );
    let (status, held) =
        at_domain(&router, &endpoint, "GET", &format!("/{INDEX}/_doc/{refused}")).await;
    assert_eq!(
        status, FOUND,
        "case {added:?}: the document the admitted write stored must now be held by the domain, \
         got {status} with {held}"
    );

    // And the domain's own access policy is the one it was created with: nothing above
    // changed it, so every verdict was decided on the identity side.
    let (status, read) = described(&router).await;
    assert_eq!(
        status, 200,
        "case {added:?}: DescribeDomain must answer 200, got {status} with {read}"
    );
    assert_eq!(
        read["DomainStatus"]["AccessPolicies"], access_policy,
        "case {added:?}: the domain's access policy must be untouched, got {}",
        read["DomainStatus"]["AccessPolicies"]
    );
}
