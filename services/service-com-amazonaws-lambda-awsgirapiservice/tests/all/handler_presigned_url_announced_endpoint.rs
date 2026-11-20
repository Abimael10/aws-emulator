// SPDX-License-Identifier: CC0-1.0

//! WHERE the URL a running handler's own AWS SDK presigns LEADS, over the wire: Lambda, Amazon
//! S3, IAM and the token service on ONE engine, a `python3.x` function that does `import boto3`
//! and calls `generate_presigned_url`, and the URL it handed back fetched the way a later
//! caller fetches one — at the address the URL itself names, with no `Authorization` header.
//!
//! RULE: an address this emulator hands a caller to use later is the address this emulator is
//! reached on — the base URL in force for the request that produced it — never a service's own
//! production AWS hostname, which resolves to nothing this emulator answers on; and the
//! credential scope of a presigned URL names the Region the client that presigned it was built
//! for.
//!
//! `crate::handler_presigned_object_url` pins what ONE presigned URL carries and what following
//! it serves. This suite generalises the ADDRESS and the SCOPE, as the cross product of the base
//! URL this emulator is reached on (host form, with a port and without, and the base in force
//! for an emulator given none) and the Region the presigning client was built for (none, so the
//! invocation's own, and three others); the URL of every row whose Region is the one its object
//! was written in is followed and has to serve the object's bytes.
//!
//! The suite RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]). It is ONE test, so the whole
//! table is one process' worth of invocations whichever runner executes it.

use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::{EndpointUrl, MultiServiceRouter, UNSERVED_URL_BASE};

use crate::event_source_mapping_role_access::given_role;
use crate::event_source_mappings::{await_function_active, call, router_on};

/// Where `CreateFunction` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The Region this harness's own Lambda calls are signed for, which is therefore the Region
/// each invocation below runs in — and the one a client built naming none is built for.
const REGION: &str = "us-east-1";

/// The algorithm a SigV4 query-presigned URL declares.
const SIGV4_ALGORITHM: &str = "AWS4-HMAC-SHA256";

/// The window every row asks its URL to stay valid for, well inside the published range of
/// `X-Amz-Expires`.
const EXPIRES_SECONDS: u64 = 600;

/// The bucket every row presigns a read of, and the object's key and bytes.
const BUCKET: &str = "presign-announced-endpoint";
const KEY: &str = "reports/announced.txt";
const BODY: &str = "bytes behind a presigned URL";

/// Lambda, Amazon S3, IAM and the token service over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them, reached at `base` — or wherever an emulator given no
/// base of its own is reached, for `None`: a handler running here presigns against the endpoint
/// this same router is reached on, so a URL it hands back is followed on this router.
fn emulator_at(base: Option<&str>) -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(&engine);
    if let Some(base) = base {
        router.set_url_base(EndpointUrl::parse(base).expect("the test base URL must parse"));
    }
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
    /// Its body as text, for an assertion message and for reading bytes back.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }
}

/// Dispatch one request at the address `url` names, carrying the headers `headers` names.
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

/// One Amazon S3 request this suite makes on its OWN behalf, path style, which must be served:
/// the credential scope alone routes it (the router has request authentication off).
async fn s3_ok(router: &MultiServiceRouter, case: &str, method: &str, path: &str, body: &[u8]) {
    let host = format!("s3.{REGION}.amazonaws.com");
    let authorization = format!(
        "{SIGV4_ALGORITHM} Credential=emulator-test-key/20260101/{REGION}/s3/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    );
    let answer = dispatch(
        router,
        method,
        &format!("https://{host}{path}"),
        &[("authorization", authorization)],
        body,
    )
    .await;
    assert!(
        (200..300).contains(&answer.status),
        "case {case:?}: S3 {method} {path} must succeed, got {} {}",
        answer.status,
        answer.text()
    );
}

/// The bucket and the object every row reads, written through S3's own API.
async fn given_object(router: &MultiServiceRouter, case: &str) {
    s3_ok(router, case, "PUT", &format!("/{BUCKET}"), &[]).await;
    s3_ok(
        router,
        case,
        "PUT",
        &format!("/{BUCKET}/{KEY}"),
        BODY.as_bytes(),
    )
    .await;
}

/// One query field of a presigned URL, with the `/` separators of a credential scope read back.
fn query_field(url: &str, name: &str) -> Option<String> {
    let (_, query) = url.split_once('?')?;
    query.split('&').find_map(|pair| {
        let (field, value) = pair.split_once('=')?;
        (field == name).then(|| value.replace("%2F", "/"))
    })
}

/// The handler every row runs: for each step the event names it builds an S3 client — for the
/// Region the step names, or for none at all — asks it to presign a read of the object, and
/// reports what the SDK gave back.
const HANDLER_SOURCE: &str = r#"
import boto3


def lambda_handler(event, context):
    answered = {}
    for step in event["steps"]:
        region = step.get("region")
        if region:
            client = boto3.client("s3", region_name=region)
        else:
            client = boto3.client("s3")
        try:
            answered[step["name"]] = {
                "url": client.generate_presigned_url(
                    "get_object",
                    Params={"Bucket": step["bucket"], "Key": step["key"]},
                    ExpiresIn=step["expires"],
                )
            }
        except BaseException as error:
            answered[step["name"]] = {"failed": repr(error)}
    return answered
"#;

/// A function named `name`, running [`HANDLER_SOURCE`] as a role allowed to read the object,
/// waited until it is `Active`.
async fn given_function(router: &MultiServiceRouter, case: &str, name: &str) {
    let role = given_role(
        router,
        case,
        &format!("{name}-role"),
        Some(json!([{
            "Effect": "Allow",
            "Action": "s3:GetObject",
            "Resource": format!("arn:aws:s3:::{BUCKET}/*"),
        }])),
    )
    .await;
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": role,
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

/// One step presigning a read of the object, for the Region `region` names or for none.
fn read_step(name: &str, region: Option<&str>) -> Value {
    json!({
        "name": name,
        "region": region,
        "bucket": BUCKET,
        "key": KEY,
        "expires": EXPIRES_SECONDS,
    })
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

/// Assert `url` leads back to `base`, addresses the object path style, and carries a SigV4
/// query signature scoped to `region` — and that no production AWS hostname appears in it.
fn assert_leads_to(case: &str, url: &str, base: &str, region: &str) {
    assert!(
        url.starts_with(&format!("{base}/{BUCKET}/")),
        "case {case:?}: expected the object's path-style address under {base}, got {url:?}"
    );
    assert!(
        !url.contains("amazonaws.com"),
        "case {case:?}: a presigned URL may not name a host only AWS serves, got {url:?}"
    );
    assert_eq!(
        query_field(url, "X-Amz-Algorithm").as_deref(),
        Some(SIGV4_ALGORITHM),
        "case {case:?}: expected the SigV4 algorithm, got {url:?}"
    );
    assert_eq!(
        query_field(url, "X-Amz-Expires").as_deref(),
        Some(EXPIRES_SECONDS.to_string().as_str()),
        "case {case:?}: expected a window of {EXPIRES_SECONDS} seconds, got {url:?}"
    );
    assert!(
        query_field(url, "X-Amz-Signature").is_some_and(|value| !value.is_empty()),
        "case {case:?}: expected a signature, got {url:?}"
    );
    assert!(
        query_field(url, "X-Amz-Security-Token").is_some_and(|value| !value.is_empty()),
        "case {case:?}: expected the session token of the credential it was signed with — the \
         session of the function's execution role — got {url:?}"
    );
    let scope = query_field(url, "X-Amz-Credential").unwrap_or_default();
    assert!(
        scope.ends_with(&format!("/{region}/s3/aws4_request")),
        "case {case:?}: expected a credential scope naming s3 in {region}, got {scope:?}"
    );
}

/// Follow `url` the way a later caller follows a presigned URL — at the address it names,
/// carrying no `Authorization` header — and assert it serves the object's bytes.
async fn assert_serves_the_object(router: &MultiServiceRouter, case: &str, url: &str) {
    let followed = dispatch(router, "GET", url, &[], &[]).await;
    assert_eq!(
        (followed.status, followed.text()),
        (200, BODY.to_string()),
        "case {case:?}: expected the object's bytes from {url:?}, got {} {:?}",
        followed.status,
        followed.text()
    );
}

/// Each Region row: the Region the presigning client is built for, and the Region its URL's
/// credential scope must then name.
///
/// Every one of these is a Region of the commercial partition, and the row naming none presigns
/// in the Region the invocation itself runs in.
fn region_rows() -> [(&'static str, Option<&'static str>, &'static str); 4] {
    [
        ("a client built naming no Region", None, REGION),
        ("a client built for us-west-2", Some("us-west-2"), "us-west-2"),
        (
            "a client built for eu-central-1",
            Some("eu-central-1"),
            "eu-central-1",
        ),
        (
            "a client built for ap-southeast-2",
            Some("ap-southeast-2"),
            "ap-southeast-2",
        ),
    ]
}

/// Whatever base URL this emulator is reached on, that is the address inside the URL a handler
/// presigns, whichever Region the presigning client was built for — and following such a URL
/// there serves the object.
///
/// The table is the cross product of two dimensions the rule generalises over. The base URL in
/// force: an endpoint on a named host and a port, one on a literal loopback address and a port,
/// one carrying no port at all, and the base an emulator given none of its own is reached on.
/// The Region the presigning client is built for ([`region_rows`]): none, so the invocation's
/// own, and three others — which is what the URL's credential SCOPE has to name while its
/// ADDRESS stays this emulator's.
///
/// The row presigning in the Region the object was written in is FOLLOWED as well, because a
/// read is served where its bucket is; the rows naming another Region are asserted on the
/// address and the scope alone, which is what this rule is about.
#[tokio::test]
async fn a_presigned_url_leads_to_whatever_endpoint_this_emulator_is_reached_on() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let bases: [(&str, Option<&str>, &str); 4] = [
        (
            "an endpoint on a named host and a port",
            Some("https://presign-endpoint.test:8443"),
            "https://presign-endpoint.test:8443",
        ),
        (
            "an endpoint on a literal loopback address and a port",
            Some("https://127.0.0.1:19297"),
            "https://127.0.0.1:19297",
        ),
        (
            "an endpoint carrying no port",
            Some("https://presign-endpoint.example"),
            "https://presign-endpoint.example",
        ),
        (
            "an emulator given no base of its own",
            None,
            UNSERVED_URL_BASE,
        ),
    ];
    for (reached_on, configured, base) in bases {
        let router = emulator_at(configured);
        given_object(&router, reached_on).await;
        let function = "presign-announced-endpoint-fn";
        given_function(&router, reached_on, function).await;
        let steps: Vec<Value> = region_rows()
            .iter()
            .map(|(row, region, _)| read_step(row, *region))
            .collect();
        let answered = presigned_by(&router, reached_on, function, &json!(steps)).await;
        for (row, _built_for, scoped) in region_rows() {
            let case = format!("{reached_on}, {row}");
            let url = url_of(&case, &answered, row);
            assert_leads_to(&case, &url, base, scoped);
            if scoped == REGION {
                assert_serves_the_object(&router, &case, &url).await;
            }
        }
    }
}
