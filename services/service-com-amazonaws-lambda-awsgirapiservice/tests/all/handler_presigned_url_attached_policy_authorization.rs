// SPDX-License-Identifier: CC0-1.0

//! What a presigned URL handed out by a running handler is worth when the function's
//! execution role holds its permissions through ATTACHED MANAGED POLICIES rather than through
//! an inline document.
//!
//! RULE: the permissions in force on the principal a request speaks for are the documents of
//! every policy attached to the identity behind its credential — an AWS-owned managed policy
//! and an account-owned one alike — beside its inline documents, and a request those documents
//! do not allow is refused the serving service's own `AccessDenied` (HTTP 403) naming the
//! principal, the IAM action and the resource it addressed. "Amazon S3 evaluates all of the
//! relevant access policies, user policies, and resource-based policies in deciding whether to
//! authorize the request" (Amazon S3 User Guide, *Identity and Access Management for Amazon
//! S3*), and a SigV4 credential that travels in a query string — which is what a presigned URL
//! is — speaks for its signer exactly as one in an `Authorization` header does. So an
//! ATTACHMENT is not a blanket grant: a URL presigned inside a function is served the actions
//! the attached documents really state and refused every other one.
//!
//! Every assertion drives real HTTP requests through a router holding Lambda beside Amazon S3,
//! IAM and the token service over ONE shared engine, as `core/router/src/lib.rs` mounts them:
//! the role, its attachments and its inline document are made with IAM's own operations
//! (`CreateRole`, `AttachRolePolicy`, `CreatePolicy`, `PutRolePolicy`), the URL is presigned by
//! `boto3` inside a running `python3.x` handler, and it is then FOLLOWED the way a later caller
//! follows one — at the address it names, carrying no `Authorization` header at all.
//!
//! The rows differ along what the rule generalises over: WHERE the document in force lives (a
//! policy AWS owns, a policy this account created, an inline document), WHAT it states (the
//! three log writes alone, every read of every object, one write of one prefix, a read of
//! another prefix, a read of the addressed key), and WHICH action the presigned request asks
//! for (`s3:GetObject` for the read, `s3:PutObject` for the write) — so one attachment that
//! allows the read and not the write is pinned to allow exactly the read.
//!
//! The suite RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::{EndpointUrl, MultiServiceRouter};

use crate::event_source_mapping_role_access::{element, given_role, iam};
use crate::event_source_mappings::{await_function_active, call, router_on};

/// Where `CreateFunction` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The base URL the emulator of this suite is reached on, which every presigned URL leads back
/// to.
const BASE: &str = "https://presigned-attachments.test:8443";

/// The window every URL here is asked to stay valid for — long enough that no row can be
/// decided by the clock.
const EXPIRES_SECONDS: u64 = 600;

/// The managed policy the Lambda console attaches to a new execution role, whose operative
/// document grants the three log writes and nothing else. Source: the entry AWS publishes for
/// it in the AWS Managed Policy Reference (`logs:CreateLogGroup`, `logs:CreateLogStream`,
/// `logs:PutLogEvents`), which is the document this emulator's IAM carries for the ARN.
const BASIC_EXECUTION_POLICY: &str =
    "arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole";

/// The managed policy AWS publishes for reading objects: its operative document allows the
/// `s3:Get*` and `s3:List*` families on every resource, so it reaches a read of an object and
/// no write of one. Source: the AWS Managed Policy Reference entry for `AmazonS3ReadOnlyAccess`.
const S3_READ_ONLY_POLICY: &str = "arn:aws:iam::aws:policy/AmazonS3ReadOnlyAccess";

/// The code S3 answers an authorization refusal with, and the status it carries (Amazon S3
/// "Error responses": `AccessDenied`, HTTP 403).
const ACCESS_DENIED: &str = "AccessDenied";
/// See [`ACCESS_DENIED`].
const ACCESS_DENIED_STATUS: u16 = 403;

/// The IAM action a read of the current version of an object is authorized under: "to use
/// `GetObject`, you must have the `READ` access to the object" — `s3:GetObject` (`GetObject`'s
/// own documentation of its general purpose bucket permissions).
const GET_OBJECT: &str = "s3:GetObject";

/// The IAM action an object write is authorized under ("you must have the `s3:PutObject`
/// permission", `PutObject`).
const PUT_OBJECT: &str = "s3:PutObject";

/// The bucket every row addresses, the key each row READS, and the bytes that key holds.
const BUCKET: &str = "presigned-attachment-objects";
const HELD_KEY: &str = "reports/held.json";
const BODY: &str = "{\"report\": \"held\"}";

/// The bytes a row's presigned WRITE sends, so a served write can be read back.
const WRITTEN: &str = "written through a presigned URL";

/// Lambda, Amazon S3, IAM and the token service over ONE shared engine, reached at [`BASE`]:
/// the handler's presigning is served by this same router, so the URL it hands back addresses
/// the endpoint this suite follows it at.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(&engine);
    router.set_url_base(EndpointUrl::parse(BASE).expect("the test base URL must parse"));
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    router
}

/// What one request came back with.
struct Fetched {
    status: u16,
    body: bytes::Bytes,
}

impl Fetched {
    /// Its body as text, for an assertion message and for reading an XML document.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }
}

/// Dispatch one request, with the headers `headers` names and `body` as its payload.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    url: &str,
    headers: &[(&str, String)],
    body: &[u8],
) -> Fetched {
    let host = url
        .split_once("://")
        .and_then(|(_, rest)| rest.split('/').next())
        .unwrap_or_default()
        .to_string();
    let mut builder = http::Request::builder()
        .method(method)
        .uri(url)
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, value);
    }
    let request = builder
        .body(bytes::Bytes::copy_from_slice(body))
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Fetched {
        status: parts.status.as_u16(),
        body,
    }
}

/// One Amazon S3 request this suite makes on its OWN behalf, path style: the credential scope
/// alone, which is what routes it and names the account (the router has request authentication
/// off, as every suite of this target does).
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &[u8]) -> Fetched {
    let host = format!("s3.{REGION}.amazonaws.com");
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/s3/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    );
    dispatch(
        router,
        method,
        &format!("https://{host}{path}"),
        &[("authorization", authorization)],
        body,
    )
    .await
}

/// One Amazon S3 request of this suite's own that must be served.
async fn s3_ok(router: &MultiServiceRouter, case: &str, method: &str, path: &str, body: &[u8]) {
    let answer = s3(router, method, path, body).await;
    assert!(
        (200..300).contains(&answer.status),
        "case {case:?}: S3 {method} {path} must succeed, got {} {}",
        answer.status,
        answer.text()
    );
}

/// Follow a presigned URL the way a later caller follows one: at the address the URL names,
/// carrying no `Authorization` header at all — the signature it carries is its credential.
async fn follow(router: &MultiServiceRouter, method: &str, url: &str, body: &[u8]) -> Fetched {
    dispatch(router, method, url, &[], body).await
}

/// The handler every row runs: it presigns ONE URL per step the event names and hands each
/// back under that step's name.
const HANDLER_SOURCE: &str = r#"
import boto3


def lambda_handler(event, context):
    client = boto3.client("s3")
    answered = {}
    for step in event["steps"]:
        answered[step["name"]] = client.generate_presigned_url(
            step["method"],
            Params={"Bucket": step["bucket"], "Key": step["key"]},
            ExpiresIn=step["expires"],
        )
    return answered
"#;

/// Create the function `name` running as `role_arn`, out of [`HANDLER_SOURCE`], and wait until
/// it is `Active`.
async fn given_function(router: &MultiServiceRouter, case: &str, name: &str, role_arn: &str) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": role_arn,
            "Runtime": "python3.12",
            "Handler": "lambda_function.lambda_handler",
            "Timeout": 30,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(crate::synchronous_invoke_run::package(HANDLER_SOURCE)),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case {case:?}: expected CreateFunction to answer 201, got {} {}",
        created.status,
        created.text()
    );
    await_function_active(router, case, name).await;
}

/// Invoke `function` with the steps `steps` names, answering what the handler returned.
async fn presigned_by(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    steps: &Value,
) -> Value {
    let answered = call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{function}/invocations"),
        &json!({ "steps": steps }),
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected HTTP 200 from Invoke, got {} {}",
        answered.status,
        answered.text()
    );
    assert_eq!(
        answered.header("x-amz-function-error"),
        None,
        "case {case:?}: expected no function error, got {}",
        answered.text()
    );
    answered.json()
}

/// The URL one step handed back, asserting the step was answered with one that leads back to
/// this emulator and carries a query signature at all — presigning is no call, so it is
/// answered whatever the role may do.
fn url_of(case: &str, step: &str, answered: &Value) -> String {
    let url = answered[step]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected step {step:?} to hand back a URL, got {}",
                answered[step]
            )
        })
        .to_string();
    assert!(
        url.starts_with(&format!("{BASE}/{BUCKET}/")),
        "case {case:?} step {step:?}: expected the object's address under {BASE}, got {url:?}"
    );
    assert!(
        url.contains("X-Amz-Signature="),
        "case {case:?} step {step:?}: expected a query signature, got {url:?}"
    );
    url
}

/// The sentence an S3 error document states, with the XML entities its text is escaped with
/// read back: the refusal quotes the resource it names, and `"` travels in XML as `&quot;`.
fn refusal_sentence(document: &str) -> String {
    element(document, "Message")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
}

/// The clause IAM publishes for a request no readable grant reaches at all.
fn no_grant(action: &str) -> String {
    format!("because no identity-based policy allows the {action} action")
}

/// The sentence S3 refuses such a request with, written out here rather than built from the
/// service's own helper so the wording has something to be wrong about: who was refused, which
/// IAM action on which resource, and the clause naming the rule that refused it.
fn denial_message(principal: &str, action: &str, resource: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: \"{resource}\" {}",
        no_grant(action)
    )
}

/// The assumed-role session a run of `function` as `role` speaks for, as the token service
/// reports it: Lambda opens the session under the function's own name.
fn session_principal(role: &str, function: &str) -> String {
    format!(
        "arn:aws:sts::{}:assumed-role/{role}/{function}",
        engine::emulator_account_id()
    )
}

/// The ARN of the object `key` of [`BUCKET`] — S3's own ARN form, which carries neither a
/// Region nor an account.
fn object_arn(key: &str) -> String {
    format!("arn:aws:s3:::{BUCKET}/{key}")
}

/// A statement allowing `actions` on `resource`, as a policy document carries one.
fn allow(actions: Value, resource: &str) -> Value {
    json!([{ "Effect": "Allow", "Action": actions, "Resource": resource }])
}

/// What a row's presigned request must answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// The policies in force leave the request to the caller.
    Served,
    /// The policies in force do not allow it, so S3 refuses it under this IAM action.
    Refused(&'static str),
}

/// One row: a role carrying its documents in one place or another, and what a presigned read
/// and a presigned write signed by a run of that role must answer.
struct Row {
    case: &'static str,
    /// The role's name, which also names its function and the key its write addresses, so no
    /// row's documents can decide another row's request.
    name: &'static str,
    /// The managed policies AWS owns that are ATTACHED to it.
    aws_managed: &'static [&'static str],
    /// A policy this ACCOUNT creates and attaches, as `(policy name, statements)`.
    customer_managed: Option<(&'static str, Value)>,
    /// The statements of its one INLINE document, when the row gives it any.
    inline: Option<Value>,
    read: Outcome,
    write: Outcome,
}

/// Create the row's role with everything it carries, through IAM's own operations.
async fn given_role_of(router: &MultiServiceRouter, row: &Row) -> String {
    let role = given_role(router, row.case, row.name, row.inline.clone()).await;
    let mut attached: Vec<String> = row.aws_managed.iter().map(|arn| (*arn).to_string()).collect();
    if let Some((policy_name, statements)) = row.customer_managed.as_ref() {
        let document = json!({ "Version": "2012-10-17", "Statement": statements }).to_string();
        let (status, created) = iam(
            router,
            "CreatePolicy",
            &[("PolicyName", policy_name), ("PolicyDocument", &document)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected CreatePolicy {policy_name:?} to answer 200, got {status}: \
             {created}",
            row.case
        );
        attached.push(element(&created, "Arn"));
    }
    for policy_arn in &attached {
        let (status, answered) = iam(
            router,
            "AttachRolePolicy",
            &[("RoleName", row.name), ("PolicyArn", policy_arn)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected AttachRolePolicy {policy_arn:?} to answer 200, got {status}: \
             {answered}",
            row.case
        );
    }
    role
}

/// One presigned request a row drives: the step that handed its URL back, what the request is,
/// and what the policies in force must make of it.
struct Step<'a> {
    /// The step's name, which is what the handler keyed its URL under.
    name: &'static str,
    /// The method the URL was signed for.
    method: &'static str,
    /// The key it addresses, which the refusal names.
    key: &'a str,
    /// The URL itself.
    url: &'a str,
    /// What it must answer.
    expected: Outcome,
    /// The bytes the request sends, which only a write carries.
    sent: &'a [u8],
}

/// Grade one followed URL against what the row expects of it.
async fn graded(router: &MultiServiceRouter, row: &Row, probe: &Step<'_>) {
    let case = row.case;
    let (step, method, key) = (probe.name, probe.method, probe.key);
    let followed = follow(router, method, probe.url, probe.sent).await;
    match probe.expected {
        Outcome::Served => {
            assert!(
                (200..300).contains(&followed.status),
                "case {case:?} step {step:?}: expected the request the attached documents allow \
                 to be served, got {} {}",
                followed.status,
                followed.text()
            );
            if method == "GET" {
                assert_eq!(
                    followed.text(),
                    BODY,
                    "case {case:?} step {step:?}: expected the stored bytes of {key:?}, got {:?}",
                    followed.text()
                );
            }
        }
        Outcome::Refused(action) => {
            assert_eq!(
                followed.status, ACCESS_DENIED_STATUS,
                "case {case:?} step {step:?}: expected the request the attached documents do not \
                 allow to be refused {ACCESS_DENIED_STATUS}, got {} {}",
                followed.status,
                followed.text()
            );
            let document = followed.text();
            assert_eq!(
                element(&document, "Code"),
                ACCESS_DENIED,
                "case {case:?} step {step:?}: expected the service's own refusal code, got \
                 {document}"
            );
            assert_eq!(
                refusal_sentence(&document),
                denial_message(
                    &session_principal(row.name, &function_of(row)),
                    action,
                    &object_arn(key)
                ),
                "case {case:?} step {step:?}: expected the published sentence, got {document}"
            );
        }
    }
}

/// The function a row's role runs, named for the role so the session a refusal names says which
/// run was refused.
fn function_of(row: &Row) -> String {
    format!("{}-fn", row.name)
}

/// The key a row's presigned WRITE addresses — its own, so a served write cannot decide another
/// row's read.
fn written_key(row: &Row) -> String {
    format!("written/{}.txt", row.name)
}

/// RULE: a presigned URL is authorized as the identity that signed it, against the documents of
/// every policy ATTACHED to that identity beside its inline ones — so an attachment grants
/// exactly what it states and nothing more.
#[tokio::test]
async fn a_presigned_url_grants_only_what_the_roles_attached_policies_state() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let suite = "a handler presigning object URLs under attached policies";
    let router = emulator();
    s3_ok(&router, suite, "PUT", &format!("/{BUCKET}"), &[]).await;
    s3_ok(
        &router,
        suite,
        "PUT",
        &format!("/{BUCKET}/{HELD_KEY}"),
        BODY.as_bytes(),
    )
    .await;

    let rows: [Row; 5] = [
        Row {
            // The recorded instance: the role a client gives a function when it means it to
            // write its logs and reach nothing else.
            case: "the AWS-managed basic-execution policy alone",
            name: "presign-logs-only",
            aws_managed: &[BASIC_EXECUTION_POLICY],
            customer_managed: None,
            inline: None,
            read: Outcome::Refused(GET_OBJECT),
            write: Outcome::Refused(PUT_OBJECT),
        },
        Row {
            // One attachment that reaches the read and no write: the action the request asks
            // for is what decides it.
            case: "the AWS-managed read-only object policy",
            name: "presign-read-only",
            aws_managed: &[S3_READ_ONLY_POLICY],
            customer_managed: None,
            inline: None,
            read: Outcome::Served,
            write: Outcome::Refused(PUT_OBJECT),
        },
        Row {
            // A document this ACCOUNT owns, attached beside the AWS-owned one: both are in
            // force, and between them they allow the write alone.
            case: "a customer-managed write grant beside the basic-execution policy",
            name: "presign-customer-write",
            aws_managed: &[BASIC_EXECUTION_POLICY],
            customer_managed: Some((
                "presign-attachment-writes",
                allow(json!(PUT_OBJECT), &format!("arn:aws:s3:::{BUCKET}/written/*")),
            )),
            inline: None,
            read: Outcome::Refused(GET_OBJECT),
            write: Outcome::Served,
        },
        Row {
            // An INLINE grant of the read, which is in force beside the attachment.
            case: "an inline read grant beside the basic-execution policy",
            name: "presign-inline-read",
            aws_managed: &[BASIC_EXECUTION_POLICY],
            customer_managed: None,
            inline: Some(allow(json!(GET_OBJECT), &object_arn(HELD_KEY))),
            read: Outcome::Served,
            write: Outcome::Refused(PUT_OBJECT),
        },
        Row {
            // The boundary of the resource element: a grant of the very action, scoped to a
            // prefix no row's request addresses, reaches neither request.
            case: "an inline read grant on another prefix beside the basic-execution policy",
            name: "presign-other-prefix",
            aws_managed: &[BASIC_EXECUTION_POLICY],
            customer_managed: None,
            inline: Some(allow(
                json!(GET_OBJECT),
                &format!("arn:aws:s3:::{BUCKET}/public/*"),
            )),
            read: Outcome::Refused(GET_OBJECT),
            write: Outcome::Refused(PUT_OBJECT),
        },
    ];

    for row in &rows {
        let role = given_role_of(&router, row).await;
        let function = function_of(row);
        given_function(&router, row.case, &function, &role).await;
        let written = written_key(row);
        let answered = presigned_by(
            &router,
            row.case,
            &function,
            &json!([
                {
                    "name": "a read of the held key",
                    "method": "get_object",
                    "bucket": BUCKET,
                    "key": HELD_KEY,
                    "expires": EXPIRES_SECONDS,
                },
                {
                    "name": "a write of a new key",
                    "method": "put_object",
                    "bucket": BUCKET,
                    "key": &written,
                    "expires": EXPIRES_SECONDS,
                },
            ]),
        )
        .await;

        let read_url = url_of(row.case, "a read of the held key", &answered);
        graded(
            &router,
            row,
            &Step {
                name: "a read of the held key",
                method: "GET",
                key: HELD_KEY,
                url: &read_url,
                expected: row.read,
                sent: &[],
            },
        )
        .await;

        let write_url = url_of(row.case, "a write of a new key", &answered);
        graded(
            &router,
            row,
            &Step {
                name: "a write of a new key",
                method: "PUT",
                key: &written,
                url: &write_url,
                expected: row.write,
                sent: WRITTEN.as_bytes(),
            },
        )
        .await;

        // A refused request is a request that did not happen, and a served one is backed by
        // the store: the key the write addressed holds its bytes afterwards exactly when the
        // documents in force allowed the write, and holds nothing at all otherwise.
        let read_back = s3(&router, "GET", &format!("/{BUCKET}/{written}"), &[]).await;
        match row.write {
            Outcome::Served => assert_eq!(
                (read_back.status, read_back.text()),
                (200, WRITTEN.to_string()),
                "case {:?}: expected the store to hold what the presigned write sent, got {} {:?}",
                row.case,
                read_back.status,
                read_back.text()
            ),
            Outcome::Refused(_) => assert_eq!(
                read_back.status,
                404,
                "case {:?}: expected the refused write to have left {written:?} absent, got {} {}",
                row.case,
                read_back.status,
                read_back.text()
            ),
        }
    }
}
