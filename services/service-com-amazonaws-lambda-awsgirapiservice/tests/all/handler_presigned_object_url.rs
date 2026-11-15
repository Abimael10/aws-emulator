// SPDX-License-Identifier: CC0-1.0

//! The URL a RUNNING handler's own AWS SDK PRESIGNS, end to end over the wire: Lambda,
//! Amazon S3, IAM and the token service on ONE engine, a `python3.x` function whose code does
//! `import boto3` and calls `generate_presigned_url`, and the URL it handed back FOLLOWED the
//! way a later caller follows one — at the address it names, carrying no `Authorization`
//! header at all.
//!
//! RULE: a URL an operation or a running handler hands a caller to fetch later leads back to
//! the base URL this emulator is reached on, and carries a SigV4 query signature of the
//! identity that presigned it — so the request it names is served, and AUTHORIZED, exactly as
//! a request that identity signed itself. A presigned URL is therefore worth no more than
//! what its signer may do: a function's URL speaks for the session of the function's
//! execution role, and what that role may not do is refused the serving service's own
//! `AccessDenied` when the URL is followed.
//!
//! The rows differ along what the rule generalises over: WHICH operation is presigned (a read
//! of an object, a write of one), the FORM of the key (a flat name, one with `/` separators,
//! one holding a space and a `+`, which only survives if the path is encoded the way the
//! verifier reads it back), whether the presigning role may perform the action at all and how
//! widely its grant is scoped, the WINDOW the URL is asked to stay valid for (each published
//! bound and one past it), and the boundary of what is presigned at all (an operation no URL
//! is handed out for, and a service whose input does not travel in an address).
//!
//! The suite RUNS a handler, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::{EndpointUrl, MultiServiceRouter};

use crate::event_source_mapping_role_access::{element, given_role};
use crate::event_source_mappings::{await_function_active, call, router_on};

/// Where `CreateFunction` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The Region every request below is signed for, which is the one the harness's Lambda calls
/// carry.
const REGION: &str = "us-east-1";

/// The base URL the emulator of this suite is reached on: every URL a handler presigns has to
/// lead back to it, and nothing of AWS's own hostnames may appear in one.
const BASE: &str = "https://emulator.test:8443";

/// The algorithm a SigV4 query-presigned URL declares.
const SIGV4_ALGORITHM: &str = "AWS4-HMAC-SHA256";

/// The longest window a query signature may carry: "X-Amz-Expires … the maximum value is
/// 604800" (AWS General Reference, *Authenticating Requests: Using Query Parameters*).
const MAX_EXPIRES_SECONDS: u64 = 604_800;

/// The shortest one it may carry, which is the other end of the same published range.
const MIN_EXPIRES_SECONDS: u64 = 1;

/// A window well inside that range, which the rows that FOLLOW their URL ask for.
const GENEROUS_EXPIRES_SECONDS: u64 = 600;

/// The code S3 answers an authorization refusal with, and the status it carries (Amazon S3
/// "Error responses": `AccessDenied`, HTTP 403).
const ACCESS_DENIED: &str = "AccessDenied";
const ACCESS_DENIED_STATUS: u16 = 403;

/// Lambda, Amazon S3, IAM and the token service over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them, reached at `base`: the handler's own presigning is
/// served by this same router, so the URL it hands back addresses the endpoint this suite
/// follows it at.
fn emulator_at(base: &str) -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(&engine);
    router.set_url_base(EndpointUrl::parse(base).expect("the test base URL must parse"));
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
    headers: http::HeaderMap,
    body: bytes::Bytes,
}

impl Fetched {
    /// Its body as text, for an assertion message and for reading an XML document.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    /// One response header, as text.
    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
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
        headers: parts.headers,
        body,
    }
}

/// One Amazon S3 request this suite makes on its OWN behalf, path style: the credential scope
/// alone, which is what routes it (the router has request authentication off).
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &[u8]) -> Fetched {
    let host = format!("s3.{REGION}.amazonaws.com");
    let authorization = format!(
        "{SIGV4_ALGORITHM} Credential=emulator-test-key/20260101/{REGION}/s3/aws4_request, \
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

/// One Amazon S3 request that must be served.
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
async fn follow(
    router: &MultiServiceRouter,
    method: &str,
    url: &str,
    headers: &[(&str, String)],
    body: &[u8],
) -> Fetched {
    dispatch(router, method, url, headers, body).await
}

/// One query field of a presigned URL, with the `/` separators of a credential scope read
/// back.
fn query_field(url: &str, name: &str) -> Option<String> {
    let (_, query) = url.split_once('?')?;
    query.split('&').find_map(|pair| {
        let (field, value) = pair.split_once('=')?;
        (field == name).then(|| value.replace("%2F", "/"))
    })
}

/// The handler every row runs: it presigns ONE URL per step the event names, with the client
/// that step names, and reports what the SDK gave back — the URL, or the refusal it raised.
const HANDLER_SOURCE: &str = r#"
import boto3


def lambda_handler(event, context):
    answered = {}
    for step in event["steps"]:
        client = boto3.client(step.get("service") or "s3")
        try:
            answered[step["name"]] = {
                "url": client.generate_presigned_url(
                    step["method"],
                    Params=step.get("params") or {},
                    ExpiresIn=step["expires"],
                )
            }
        except BaseException as error:
            answered[step["name"]] = {"failed": repr(error)}
    return answered
"#;

/// A statement allowing `actions` on `resource`, as an execution role's inline policy carries
/// one.
fn allow(actions: Value, resource: &str) -> Value {
    json!([{ "Effect": "Allow", "Action": actions, "Resource": resource }])
}

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
        created.status,
        201,
        "case {case:?}: expected CreateFunction to answer 201, got {} {}",
        created.status,
        created.text()
    );
    await_function_active(router, case, name).await;
}

/// Invoke `name` with the steps `steps` names, answering what the handler returned.
async fn presigned_by(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    steps: &Value,
) -> Value {
    let answered = call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/invocations"),
        &json!({ "steps": steps }),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
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

/// The URL one step handed back, asserting the step was answered with one at all.
fn url_of(case: &str, answered: &Value, step: &str) -> String {
    answered[step]["url"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected step {step:?} to hand back a URL, got {}",
                answered[step]
            )
        })
        .to_string()
}

/// Every presigned URL carries the same terms, whatever it addresses: this emulator's own
/// base URL, the bucket the request names, the SigV4 query fields, the window it was asked
/// for, and the session token of the TEMPORARY credential it was signed with — the session of
/// the function's execution role, which is what the serving service authorizes it as.
fn assert_presigned(case: &str, url: &str, bucket: &str, expires: u64) {
    assert!(
        url.starts_with(&format!("{BASE}/")),
        "case {case:?}: expected a URL on {BASE}, got {url:?}"
    );
    assert!(
        !url.contains("amazonaws.com"),
        "case {case:?}: a presigned URL may not name a host only AWS serves, got {url:?}"
    );
    assert!(
        url.starts_with(&format!("{BASE}/{bucket}/")),
        "case {case:?}: expected the object's path-style address under {bucket:?}, got {url:?}"
    );
    assert_eq!(
        query_field(url, "X-Amz-Algorithm").as_deref(),
        Some(SIGV4_ALGORITHM),
        "case {case:?}: expected the SigV4 algorithm, got {url:?}"
    );
    assert_eq!(
        query_field(url, "X-Amz-Expires").as_deref(),
        Some(expires.to_string().as_str()),
        "case {case:?}: expected a window of {expires} seconds, got {url:?}"
    );
    assert!(
        query_field(url, "X-Amz-Signature").is_some_and(|value| !value.is_empty()),
        "case {case:?}: expected a signature, got {url:?}"
    );
    assert!(
        query_field(url, "X-Amz-Security-Token").is_some_and(|value| !value.is_empty()),
        "case {case:?}: expected the session token of the credential it was signed with, got \
         {url:?}"
    );
    let scope = query_field(url, "X-Amz-Credential").unwrap_or_default();
    assert!(
        scope.ends_with(&format!("/{REGION}/s3/aws4_request")),
        "case {case:?}: expected a credential scope naming s3 in {REGION}, got {scope:?}"
    );
}

/// Every URL a handler presigns leads back to this emulator and serves the request it names:
/// a read of the object the store holds, and a WRITE whose bytes the store holds afterwards.
#[tokio::test]
async fn a_presigned_url_leads_back_to_this_emulator_and_serves_the_request_it_names() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let case = "a handler presigning object URLs";
    let router = emulator_at(BASE);
    let bucket = "presign-runtime-objects";
    s3_ok(&router, case, "PUT", &format!("/{bucket}"), &[]).await;

    // The two keys the read rows address, written through S3's own API.
    let nested = ("reports/first.json", "{\"report\": \"first\"}");
    let flat = ("flat.bin", "flat bytes");
    for (key, body) in [nested, flat] {
        s3_ok(
            &router,
            case,
            "PUT",
            &format!("/{bucket}/{key}"),
            body.as_bytes(),
        )
        .await;
    }

    // A key holding a space and a `+`, which no presigned URL resolves unless its path is
    // encoded the way the verifier reads it back. It is WRITTEN through its own presigned URL,
    // so nothing but the runtime's own encoding decides where the bytes land.
    let spaced = "quarter report+final.bin";
    let written = "written/by-url.txt";
    let role = given_role(
        &router,
        case,
        "presign-object-role",
        Some(allow(
            json!(["s3:GetObject", "s3:PutObject", "s3:ListBucket"]),
            "*",
        )),
    )
    .await;
    let function = "presign-object-urls";
    given_function(&router, case, function, &role).await;
    let answered = presigned_by(
        &router,
        case,
        function,
        &json!([
            {
                "name": "a read of a key with / separators",
                "method": "get_object",
                "params": { "Bucket": bucket, "Key": nested.0 },
                "expires": GENEROUS_EXPIRES_SECONDS,
            },
            {
                "name": "a read of a flat key",
                "method": "get_object",
                "params": { "Bucket": bucket, "Key": flat.0 },
                "expires": GENEROUS_EXPIRES_SECONDS,
            },
            {
                "name": "a write declaring the object's media type",
                "method": "put_object",
                "params": { "Bucket": bucket, "Key": written, "ContentType": "text/plain" },
                "expires": GENEROUS_EXPIRES_SECONDS,
            },
            {
                "name": "a write of a key holding a space and a plus",
                "method": "put_object",
                "params": { "Bucket": bucket, "Key": spaced },
                "expires": GENEROUS_EXPIRES_SECONDS,
            },
            {
                "name": "a read of that same key",
                "method": "get_object",
                "params": { "Bucket": bucket, "Key": spaced },
                "expires": GENEROUS_EXPIRES_SECONDS,
            },
        ]),
    )
    .await;

    // Every one of them is presigned for this emulator's own endpoint.
    let urls: Vec<(String, String)> = [
        "a read of a key with / separators",
        "a read of a flat key",
        "a write declaring the object's media type",
        "a write of a key holding a space and a plus",
        "a read of that same key",
    ]
    .into_iter()
    .map(|step| (step.to_string(), url_of(case, &answered, step)))
    .collect();
    for (step, url) in &urls {
        assert_presigned(step, url, bucket, GENEROUS_EXPIRES_SECONDS);
    }

    // The reads serve exactly the bytes the store holds.
    for (step, (key, expected)) in [(&urls[0], nested), (&urls[1], flat)] {
        let answer = follow(&router, "GET", &step.1, &[], &[]).await;
        assert_eq!(
            answer.status, 200,
            "case {:?}: expected 200 following {key:?}, got {} {}",
            step.0,
            answer.status,
            answer.text()
        );
        assert_eq!(
            answer.text(),
            expected,
            "case {:?}: expected the stored bytes of {key:?}, got {:?}",
            step.0,
            answer.text()
        );
    }

    // The write a presigned URL names is performed, with the media type it bound in.
    let payload = "written through a presigned URL";
    let typed = follow(
        &router,
        "PUT",
        &urls[2].1,
        &[("content-type", "text/plain".to_string())],
        payload.as_bytes(),
    )
    .await;
    assert!(
        (200..300).contains(&typed.status),
        "case {:?}: expected the presigned write to be served, got {} {}",
        urls[2].0,
        typed.status,
        typed.text()
    );
    let read_back = s3(&router, "GET", &format!("/{bucket}/{written}"), &[]).await;
    assert_eq!(
        (read_back.status, read_back.text()),
        (200, payload.to_string()),
        "case {:?}: expected the store to hold what the presigned write sent, got {} {:?}",
        urls[2].0,
        read_back.status,
        read_back.text()
    );
    assert_eq!(
        read_back.header("content-type").as_deref(),
        Some("text/plain"),
        "case {:?}: expected the media type the URL bound in, got {:?}",
        urls[2].0,
        read_back.header("content-type")
    );

    // The odd key round-trips: written through one presigned URL, read back through another,
    // and held by the store under the exact name the handler asked for.
    let odd_payload = "bytes under an awkward key";
    let odd_write = follow(&router, "PUT", &urls[3].1, &[], odd_payload.as_bytes()).await;
    assert!(
        (200..300).contains(&odd_write.status),
        "case {:?}: expected the presigned write to be served, got {} {}",
        urls[3].0,
        odd_write.status,
        odd_write.text()
    );
    let odd_read = follow(&router, "GET", &urls[4].1, &[], &[]).await;
    assert_eq!(
        (odd_read.status, odd_read.text()),
        (200, odd_payload.to_string()),
        "case {:?}: expected the bytes written under {spaced:?}, got {} {:?}",
        urls[4].0,
        odd_read.status,
        odd_read.text()
    );
    let listed = s3(&router, "GET", &format!("/{bucket}?list-type=2"), &[]).await;
    assert!(
        listed.text().contains(&format!("<Key>{spaced}</Key>")),
        "case {case:?}: expected the store to hold the key as asked for, got {}",
        listed.text()
    );
}

/// A presigned URL is worth no more than its signer: the request it names is authorized as
/// the session of the function's execution role, so a role the policies do not allow is
/// refused the serving service's own `AccessDenied` when the URL is followed.
///
/// The rows differ along what the grant in force on the presigning role says about the read:
/// it allows it, it allows another action entirely, and it allows the action on another
/// prefix of the same bucket.
#[tokio::test]
async fn a_presigned_url_is_authorized_as_the_role_that_presigned_it() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let suite = "a handler presigning a read its role may or may not perform";
    let router = emulator_at(BASE);
    let bucket = "presign-authorized-reads";
    let key = "reports/first.json";
    let body = "{\"report\": \"first\"}";
    s3_ok(&router, suite, "PUT", &format!("/{bucket}"), &[]).await;
    s3_ok(
        &router,
        suite,
        "PUT",
        &format!("/{bucket}/{key}"),
        body.as_bytes(),
    )
    .await;

    let rows: [(&str, &str, Value, Option<u16>); 3] = [
        (
            "a role granted the read",
            "presign-reader",
            allow(json!("s3:GetObject"), &format!("arn:aws:s3:::{bucket}/*")),
            None,
        ),
        (
            "a role granted only the listing",
            "presign-lister",
            allow(json!("s3:ListBucket"), &format!("arn:aws:s3:::{bucket}")),
            Some(ACCESS_DENIED_STATUS),
        ),
        (
            "a role granted the read on another prefix",
            "presign-other-prefix",
            allow(
                json!("s3:GetObject"),
                &format!("arn:aws:s3:::{bucket}/public/*"),
            ),
            Some(ACCESS_DENIED_STATUS),
        ),
    ];
    for (case, role_name, statements, refused) in rows {
        let role = given_role(&router, case, role_name, Some(statements)).await;
        let function = format!("{role_name}-fn");
        given_function(&router, case, &function, &role).await;
        let answered = presigned_by(
            &router,
            case,
            &function,
            &json!([{
                "name": "read",
                "method": "get_object",
                "params": { "Bucket": bucket, "Key": key },
                "expires": GENEROUS_EXPIRES_SECONDS,
            }]),
        )
        .await;
        // Presigning itself is no call, so it is answered whatever the role may do.
        let url = url_of(case, &answered, "read");
        assert_presigned(case, &url, bucket, GENEROUS_EXPIRES_SECONDS);

        let followed = follow(&router, "GET", &url, &[], &[]).await;
        match refused {
            None => {
                assert_eq!(
                    (followed.status, followed.text()),
                    (200, body.to_string()),
                    "case {case:?}: expected the read the role allows to be served, got {} {:?}",
                    followed.status,
                    followed.text()
                );
            }
            Some(status) => {
                assert_eq!(
                    followed.status, status,
                    "case {case:?}: expected the read the role does not allow to be refused \
                     {status}, got {} {}",
                    followed.status,
                    followed.text()
                );
                let document = followed.text();
                assert_eq!(
                    element(&document, "Code"),
                    ACCESS_DENIED,
                    "case {case:?}: expected the service's own refusal code, got {document}"
                );
                // The refusal names the principal the URL speaks for: the assumed-role
                // session of THIS role run for THIS function.
                let session = format!("assumed-role/{role_name}/{function}");
                assert!(
                    element(&document, "Message").contains(&session),
                    "case {case:?}: expected the refusal to name {session:?}, got {document}"
                );
            }
        }
    }
}

/// The WINDOW a presigned URL carries, and the boundary of what is presigned at all.
///
/// The window rows are each published bound of `X-Amz-Expires` and one past the top of it: a
/// longer ask is answered with a URL good for the ceiling rather than one nothing can verify.
/// The last two rows are the boundary: an operation no URL is handed out for, and a service
/// whose input travels in a request rather than in an address — each reported to the handler
/// as a URL that was NOT signed, naming what it asked for.
#[tokio::test]
async fn a_presigned_url_carries_the_window_asked_for_inside_the_published_range() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let case = "a handler asking for every window and for what is not presigned";
    let router = emulator_at(BASE);
    let bucket = "presign-windows";
    s3_ok(&router, case, "PUT", &format!("/{bucket}"), &[]).await;
    let role = given_role(
        &router,
        case,
        "presign-window-role",
        Some(allow(json!("s3:GetObject"), "*")),
    )
    .await;
    let function = "presign-windows-fn";
    given_function(&router, case, function, &role).await;

    // Each row's window, and the window the URL it gets back has to carry.
    let windows: [(&str, u64, u64); 4] = [
        ("the shortest window", MIN_EXPIRES_SECONDS, MIN_EXPIRES_SECONDS),
        ("a window inside the range", GENEROUS_EXPIRES_SECONDS, GENEROUS_EXPIRES_SECONDS),
        ("the longest window", MAX_EXPIRES_SECONDS, MAX_EXPIRES_SECONDS),
        ("one second past the longest", MAX_EXPIRES_SECONDS + 1, MAX_EXPIRES_SECONDS),
    ];
    let mut steps: Vec<Value> = windows
        .iter()
        .map(|(case, asked, _)| {
            json!({
                "name": case,
                "method": "get_object",
                "params": { "Bucket": bucket, "Key": "reports/first.json" },
                "expires": asked,
            })
        })
        .collect();
    steps.push(json!({
        "name": "an operation no URL is handed out for",
        "method": "delete_object",
        "params": { "Bucket": bucket, "Key": "reports/first.json" },
        "expires": GENEROUS_EXPIRES_SECONDS,
    }));
    steps.push(json!({
        "name": "a service whose input travels in the request",
        "service": "dynamodb",
        "method": "get_item",
        "params": { "TableName": "orders" },
        "expires": GENEROUS_EXPIRES_SECONDS,
    }));
    let answered = presigned_by(&router, case, function, &json!(steps)).await;

    for (row, _asked, carried) in windows {
        let url = url_of(row, &answered, row);
        assert_presigned(row, &url, bucket, carried);
    }
    for (row, named) in [
        ("an operation no URL is handed out for", "DeleteObject"),
        (
            "a service whose input travels in the request",
            "dynamodb",
        ),
    ] {
        let failed = answered[row]["failed"].as_str().unwrap_or_default();
        assert!(
            failed.contains(named),
            "case {row:?}: expected the refusal to name {named:?}, got {}",
            answered[row]
        );
        assert!(
            answered[row]["url"].is_null(),
            "case {row:?}: expected no URL at all, got {}",
            answered[row]
        );
    }
}
