// SPDX-License-Identifier: CC0-1.0

//! WHICH of a viewer's own headers an origin request carries, across the three services the
//! answer involves: CloudFront, the API Gateway stage one distribution fronts and the Amazon S3
//! bucket another fronts.
//!
//! RULE: every viewer header the matched cache behaviour's policies NAME travels to the origin
//! carrying the value the viewer stated for it — wherever the request carried that value, the
//! `Origin` a browser sends included — while a header no policy names reaches no origin at all,
//! and so does the viewer's own CREDENTIAL, which this emulator relays to no origin whatever a
//! policy names. CloudFront states its OWN `User-Agent` on the origin request only where the
//! viewer's own is not forwarded, and every origin request states the address the request came
//! from.
//!
//! Only a cross-service suite can state it. WHAT the origin received is only observable from
//! the origin's own answer, so the first half drives a distribution whose origin is an API
//! Gateway stage that REFLECTS the headers that reached it, and the second half drives one
//! whose origin is a bucket with a cors configuration: an S3 CORS PREFLIGHT is answered from
//! that configuration and from the request's `Origin`, so a viewer that reaches the
//! distribution's own published address with an `Origin` is served Amazon S3's own
//! `Access-Control-Allow-*` answer exactly when that header reached the bucket — and is
//! refused, in the bucket's own vocabulary, when the behaviour forwards it nowhere. The
//! CloudFront crate's own suites fetch origins that reflect nothing, and the S3 crate's own
//! preflight suite answers requests that never went through an edge.
//!
//! The rows differ along what the rule generalises over: the header behaviour the policy states
//! (none at all, a whitelist naming one header, a whitelist naming several, the legacy `*` that
//! names every header), the header asked about (an ordinary one, the viewer's credential, the
//! `User-Agent` CloudFront would otherwise state itself, the `Origin` a browser sends) and,
//! for the preflight, whether the behaviour forwards the headers a preflight asks its question
//! in.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::cloudfront_apigateway_origin_edge::{
    apigw, cloudfront, dispatch, member, view, Answer, API_VERSION, REGION, STAGE,
};

/// The resource path of the api's one method, and the key the bucket's one object sits at.
const ECHO_PATH: &str = "echo";
/// See [`ECHO_PATH`].
const OBJECT_KEY: &str = "asset.txt";

/// The viewer headers the rows ask about, and the values the viewer states for them.
///
/// `Authorization` is the viewer's own CREDENTIAL: it is sent on every row and reaches no
/// origin on any of them, which is what the rows naming it pin.
const TENANT_HEADER: &str = "X-Tenant";
const TENANT_VALUE: &str = "tenant-7";
const AUTHORIZATION_HEADER: &str = "Authorization";
const AUTHORIZATION_VALUE: &str = "Bearer viewer-token";
const USER_AGENT_HEADER: &str = "User-Agent";
const USER_AGENT_VALUE: &str = "viewer-browser/9.1";
const LANGUAGE_HEADER: &str = "Accept-Language";
const LANGUAGE_VALUE: &str = "fr-CA";

/// The header CloudFront states the address a request came from in, which EVERY origin request
/// carries whatever the behaviour's policies name.
const FORWARDED_FOR_HEADER: &str = "X-Forwarded-For";

/// The `User-Agent` CloudFront states on an origin request whose viewer `User-Agent` the
/// behaviour does not forward.
const CLOUDFRONT_USER_AGENT: &str = "Amazon CloudFront";

/// The site the preflight rows act for, and the method they announce.
const BROWSER_ORIGIN: &str = "https://app.example.com";
const PREFLIGHT_METHOD: &str = "GET";

/// The response header a preflight Amazon S3 admits is answered with.
const ALLOW_ORIGIN: &str = "access-control-allow-origin";

/// How long to wait between polls of the clock-owed deployment, and how many polls to make.
const POLL: Duration = Duration::from_millis(50);
const POLLS: usize = 1_200;

/// How long the dialled viewer gives its own socket, generous for the same reason every other
/// deadline here is: what a row asserts is WHAT the published address answers, never how
/// quickly.
const DIAL_WITHIN: Duration = Duration::from_secs(30);

/// CloudFront, API Gateway and Amazon S3 over ONE engine: the two origins the rows front live
/// in the other two services. Request authentication is off, so a setup call carries a
/// credential scope (which is what routes it and names the Region) without a computed
/// signature; a viewer carries no credential at all beyond the header a row asks about.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudfront_cloudfront2020_05_31::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_apigateway_backplanecontrolservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        engine,
    ));
    router
}

/// Poll `GetDistribution` until CloudFront first reports the distribution `Deployed`, which is
/// the state a viewer request is served in. The wait is on the CONDITION, with a deadline far
/// past the span the service owes it.
async fn wait_until_deployed(router: &MultiServiceRouter, case: &str, id: &str) {
    for _ in 0..POLLS {
        let read = cloudfront(
            router,
            "GET",
            &format!("/{API_VERSION}/distribution/{id}"),
            Bytes::new(),
        )
        .await;
        if read.element("Status") == "Deployed" {
            return;
        }
        ::tokio::time::sleep(POLL).await;
    }
    panic!("case {case:?}: CloudFront never reported {id} deployed");
}

// ---------------------------------------------------------------------------
// The origins
// ---------------------------------------------------------------------------

/// A REST api with one `GET /echo` method whose `MOCK` integration REFLECTS the headers that
/// reached it, deployed to [`STAGE`]. Answers the api's id.
///
/// The response template reads the METHOD REQUEST's own parameters, so the document the stage
/// answers states exactly what the origin request carried for each header a row asks about.
async fn deployed_reflecting_api(router: &MultiServiceRouter) -> String {
    let created = apigw(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "forwarded-headers-echo" })),
        201,
    )
    .await;
    let api = member("CreateRestApi", &created, "id");
    let root = member("CreateRestApi", &created, "rootResourceId");
    let resource = apigw(
        router,
        "CreateResource",
        "POST",
        &format!("/restapis/{api}/resources/{root}"),
        Some(json!({ "pathPart": ECHO_PATH })),
        201,
    )
    .await;
    let echo = member("CreateResource", &resource, "id");
    apigw(
        router,
        "PutMethod",
        "PUT",
        &format!("/restapis/{api}/resources/{echo}/methods/GET"),
        Some(json!({ "authorizationType": "NONE" })),
        201,
    )
    .await;
    apigw(
        router,
        "PutIntegration",
        "PUT",
        &format!("/restapis/{api}/resources/{echo}/methods/GET/integration"),
        Some(json!({
            "type": "MOCK",
            "requestTemplates": { "application/json": r#"{"statusCode":200}"# },
        })),
        201,
    )
    .await;
    apigw(
        router,
        "PutMethodResponse",
        "PUT",
        &format!("/restapis/{api}/resources/{echo}/methods/GET/responses/200"),
        Some(json!({})),
        201,
    )
    .await;
    let template = format!(
        r#"{{"tenant":"$input.params('{TENANT_HEADER}')","authorization":"$input.params('{AUTHORIZATION_HEADER}')","userAgent":"$input.params('{USER_AGENT_HEADER}')","language":"$input.params('{LANGUAGE_HEADER}')","forwardedFor":"$input.params('{FORWARDED_FOR_HEADER}')"}}"#
    );
    apigw(
        router,
        "PutIntegrationResponse",
        "PUT",
        &format!("/restapis/{api}/resources/{echo}/methods/GET/integration/responses/200"),
        Some(json!({
            "statusCode": "200",
            "responseTemplates": { "application/json": template },
        })),
        201,
    )
    .await;
    apigw(
        router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;
    api
}

/// One Amazon S3 call, addressed PATH-STYLE at the Region's endpoint (which carries no bucket
/// in its host, so the path is the bucket and the key).
async fn s3(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> Answer {
    let host = format!("s3.{REGION}.amazonaws.com");
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/s3/aws4_request, \
         SignedHeaders=host, Signature=0000"
    );
    let mut all: Vec<(&str, &str)> = headers.to_vec();
    all.push(("authorization", &authorization));
    dispatch(
        router,
        method,
        &host,
        &format!("https://{host}{path}"),
        &all,
        body,
    )
    .await
}

/// A PUBLIC bucket holding one object and one cors configuration that shares it with every
/// site: what the preflight rows ask the ORIGIN, and what it answers them from.
async fn staged_bucket(router: &MultiServiceRouter, bucket: &str) {
    for (what, method, path, headers, body) in [
        ("CreateBucket", "PUT", format!("/{bucket}"), vec![], Bytes::new()),
        (
            "PutObject",
            "PUT",
            format!("/{bucket}/{OBJECT_KEY}"),
            vec![("content-type", "text/plain")],
            Bytes::from_static(b"the shared asset"),
        ),
        (
            "PutPublicAccessBlock",
            "PUT",
            format!("/{bucket}?publicAccessBlock"),
            vec![("content-type", "application/xml")],
            Bytes::from_static(
                b"<PublicAccessBlockConfiguration><BlockPublicAcls>false</BlockPublicAcls>\
                  <IgnorePublicAcls>false</IgnorePublicAcls>\
                  <BlockPublicPolicy>false</BlockPublicPolicy>\
                  <RestrictPublicBuckets>false</RestrictPublicBuckets>\
                  </PublicAccessBlockConfiguration>",
            ),
        ),
        (
            "PutBucketPolicy",
            "PUT",
            format!("/{bucket}?policy"),
            vec![("content-type", "application/json")],
            Bytes::from(format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
                 \"Principal\":\"*\",\"Action\":\"s3:GetObject\",\
                 \"Resource\":\"arn:aws:s3:::{bucket}/*\"}}]}}"
            )),
        ),
        (
            "PutBucketCors",
            "PUT",
            format!("/{bucket}?cors"),
            vec![("content-type", "application/xml")],
            Bytes::from_static(
                b"<CORSConfiguration><CORSRule><AllowedOrigin>*</AllowedOrigin>\
                  <AllowedMethod>GET</AllowedMethod><AllowedMethod>HEAD</AllowedMethod>\
                  <AllowedHeader>*</AllowedHeader><MaxAgeSeconds>3000</MaxAgeSeconds>\
                  </CORSRule></CORSConfiguration>",
            ),
        ),
    ] {
        let answered = s3(router, method, &path, &headers, body).await;
        assert!(
            (200..300).contains(&answered.status),
            "setup {what:?} on {bucket} must succeed, got {} with {:?}",
            answered.status,
            answered.text()
        );
    }
}

// ---------------------------------------------------------------------------
// The distributions
// ---------------------------------------------------------------------------

/// The legacy `ForwardedValues` of a behaviour that keys on and forwards the headers `headers`
/// names — the member a behaviour carrying no cache policy states instead ("a `CacheBehavior`
/// must include either a `CachePolicyId` or `ForwardedValues`").
fn forwarded_values(headers: &[&str]) -> String {
    let items = headers
        .iter()
        .map(|name| format!("<Name>{name}</Name>"))
        .collect::<Vec<_>>()
        .join("");
    let behaviour = if headers.is_empty() {
        "<Headers><Quantity>0</Quantity></Headers>".to_string()
    } else {
        format!(
            "<Headers><Quantity>{}</Quantity><Items>{items}</Items></Headers>",
            headers.len()
        )
    };
    format!(
        "<ForwardedValues><QueryString>true</QueryString>\
         <Cookies><Forward>none</Forward></Cookies>{behaviour}</ForwardedValues>"
    )
}

/// `CreateDistribution` over `origin` (a custom origin reached over TLS, which is how every
/// endpoint this emulator serves is reached) with the behaviour `forwarded` describes,
/// answering `(Id, DomainName)`.
async fn create_distribution(
    router: &MultiServiceRouter,
    case: &str,
    reference: &str,
    origin_host: &str,
    origin_path: &str,
    forwarded: &str,
    methods: &[&str],
) -> (String, String) {
    let items = methods
        .iter()
        .map(|method| format!("<Method>{method}</Method>"))
        .collect::<Vec<_>>()
        .join("");
    let created = cloudfront(
        router,
        "POST",
        &format!("/{API_VERSION}/distribution"),
        Bytes::from(format!(
            "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
             <CallerReference>{reference}</CallerReference>\
             <Origins><Quantity>1</Quantity><Items><Origin><Id>origin</Id>\
             <DomainName>{origin_host}</DomainName><OriginPath>{origin_path}</OriginPath>\
             <CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
             <OriginProtocolPolicy>https-only</OriginProtocolPolicy></CustomOriginConfig>\
             </Origin></Items></Origins>\
             <DefaultCacheBehavior><TargetOriginId>origin</TargetOriginId>\
             <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
             <AllowedMethods><Quantity>{}</Quantity><Items>{items}</Items></AllowedMethods>\
             {forwarded}<MinTTL>0</MinTTL></DefaultCacheBehavior>\
             <Comment>forwarded viewer headers</Comment><Enabled>true</Enabled>\
             </DistributionConfig>",
            methods.len()
        )),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: CreateDistribution answered {} with {:?}",
        created.status,
        created.text()
    );
    (created.element("Id"), created.element("DomainName"))
}

// ---------------------------------------------------------------------------
// The dialled viewer
// ---------------------------------------------------------------------------

/// What a dialled viewer request was answered: the status, the headers under their lowercased
/// names, and the body.
struct Served {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Served {
    /// A response header's value, empty when the answer carries none.
    fn header(&self, name: &str) -> String {
        self.headers
            .iter()
            .find(|(held, _)| held == name)
            .map(|(_, value)| value.clone())
            .unwrap_or_default()
    }
}

/// DIAL the address the distribution published and send one request of `method` for `path`
/// carrying `headers`, exactly as a browser reaches a distribution: a client holding nothing
/// but that address, speaking the protocol it answers rather than going through the router.
///
/// The dial names the LOOPBACK interface explicitly and takes only the PORT from the published
/// authority, so nothing here opens a connection anywhere else.
async fn dial(case: &str, authority: &str, method: &str, path: &str, headers: &[(&str, &str)]) -> Served {
    let port = authority
        .parse::<SocketAddr>()
        .unwrap_or_else(|error| {
            panic!("case {case:?}: the published address {authority:?} is no address: {error}")
        })
        .port();
    let mut stream = ::tokio::time::timeout(
        DIAL_WITHIN,
        ::tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port)),
    )
    .await
    .unwrap_or_else(|_| {
        panic!("case {case:?}: the published address {authority:?} did not accept a connection")
    })
    .unwrap_or_else(|error| {
        panic!("case {case:?}: the published address {authority:?} refused a connection: {error}")
    });
    let stated = headers
        .iter()
        .map(|(name, value)| format!("{name}: {value}\r\n"))
        .collect::<Vec<_>>()
        .join("");
    let request =
        format!("{method} {path} HTTP/1.1\r\nHost: {authority}\r\n{stated}Connection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the request was not sent: {error}"));
    let mut answer = Vec::new();
    ::tokio::time::timeout(DIAL_WITHIN, stream.read_to_end(&mut answer))
        .await
        .unwrap_or_else(|_| {
            panic!("case {case:?}: the published address {authority:?} did not answer in time")
        })
        .unwrap_or_else(|error| panic!("case {case:?}: the answer was not read: {error}"));
    parsed(case, &String::from_utf8_lossy(&answer))
}

/// One HTTP/1.1 answer, taken apart: the status line's code, the headers under their lowercased
/// names, and the body.
fn parsed(case: &str, raw: &str) -> Served {
    let (head, body) = raw
        .split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("case {case:?}: the answer carries no end of headers: {raw:?}"));
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or_else(|| {
            panic!("case {case:?}: the answer carries no status code: {status_line:?}")
        });
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    Served {
        status,
        headers,
        body: body.to_string(),
    }
}

// ---------------------------------------------------------------------------
// What the API Gateway origin received
// ---------------------------------------------------------------------------

/// One row of the reflecting table: the headers the behaviour's policy names, and what the
/// origin must then have received for each of the four headers the viewer sends.
struct Row {
    case: &'static str,
    /// The `Headers` list the behaviour's `ForwardedValues` states.
    names: &'static [&'static str],
    tenant: &'static str,
    authorization: &'static str,
    user_agent: &'static str,
    language: &'static str,
}

/// The reflecting rows.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a behaviour naming no header at all",
            names: &[],
            tenant: "",
            authorization: "",
            user_agent: CLOUDFRONT_USER_AGENT,
            language: "",
        },
        Row {
            case: "a whitelist naming one ordinary header",
            names: &[TENANT_HEADER],
            tenant: TENANT_VALUE,
            authorization: "",
            user_agent: CLOUDFRONT_USER_AGENT,
            language: "",
        },
        Row {
            case: "a whitelist naming the viewer's credential, which reaches no origin",
            names: &[AUTHORIZATION_HEADER],
            tenant: "",
            authorization: "",
            user_agent: CLOUDFRONT_USER_AGENT,
            language: "",
        },
        Row {
            case: "a whitelist naming the viewer's own user agent",
            names: &[USER_AGENT_HEADER],
            tenant: "",
            authorization: "",
            user_agent: USER_AGENT_VALUE,
            language: "",
        },
        Row {
            case: "a whitelist naming three of the four",
            names: &[TENANT_HEADER, AUTHORIZATION_HEADER, LANGUAGE_HEADER],
            tenant: TENANT_VALUE,
            authorization: "",
            user_agent: CLOUDFRONT_USER_AGENT,
            language: LANGUAGE_VALUE,
        },
        Row {
            case: "the legacy list that names every header the viewer sent",
            names: &["*"],
            tenant: TENANT_VALUE,
            authorization: "",
            user_agent: USER_AGENT_VALUE,
            language: LANGUAGE_VALUE,
        },
    ]
}

/// The first half of the rule, over the reflecting table.
#[tokio::test]
async fn the_origin_receives_every_header_the_behaviour_names_with_the_viewers_own_value() {
    let router = emulator();
    let api = deployed_reflecting_api(&router).await;
    let origin_host = format!("{api}.execute-api.{REGION}.amazonaws.com");

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let (id, domain) = create_distribution(
            &router,
            case,
            &format!("forwarded-headers-{index}"),
            &origin_host,
            &format!("/{STAGE}"),
            &forwarded_values(row.names),
            &["GET", "HEAD"],
        )
        .await;
        wait_until_deployed(&router, case, &id).await;

        let answered = view(
            &router,
            "https",
            &domain,
            &format!("/{ECHO_PATH}?probe={index}"),
            &[
                (TENANT_HEADER, TENANT_VALUE),
                (AUTHORIZATION_HEADER, AUTHORIZATION_VALUE),
                (USER_AGENT_HEADER, USER_AGENT_VALUE),
                (LANGUAGE_HEADER, LANGUAGE_VALUE),
            ],
        )
        .await;
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: expected the origin's reflected answer, got {} with {:?}",
            answered.status,
            answered.text()
        );
        let reflected = answered.json();
        let read = |member: &str| {
            reflected
                .get(member)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        for (member, expected) in [
            ("tenant", row.tenant),
            ("authorization", row.authorization),
            ("userAgent", row.user_agent),
            ("language", row.language),
        ] {
            let actual = read(member);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the origin to have received {member} {expected:?}, got \
                 {actual:?} (reflected {reflected})"
            );
        }
        // And the address the request came from, which every origin request states whatever
        // the behaviour forwards: "CloudFront appends the IP address of the viewer to the end of
        // the header value, or adds the header if the viewer didn't include one".
        let forwarded_for = read("forwardedFor");
        assert!(
            forwarded_for.parse::<::std::net::IpAddr>().is_ok(),
            "case {case:?}: expected the origin to have received an address in \
             {FORWARDED_FOR_HEADER}, got {forwarded_for:?} (reflected {reflected})"
        );
    }
}

/// The second half: the viewer's `Origin` reaches the origin exactly where the behaviour names
/// it, which is what makes an S3 CORS preflight answerable through a distribution at all.
///
/// The rows differ along the behaviour's `Headers` list — one naming the three headers a
/// preflight asks its question in, and one naming none — and along the way the viewer reached
/// the edge: the address the distribution PUBLISHES, dialled, which is the only place a
/// browser's own `Origin` arrives.
#[tokio::test]
async fn a_preflight_is_answered_from_the_origin_only_where_the_behaviour_forwards_the_origin_header()
{
    let router = emulator();
    let bucket = "cloudfront-preflight-origin";
    staged_bucket(&router, bucket).await;
    let origin_host = format!("{bucket}.s3.{REGION}.amazonaws.com");

    for (index, (case, names, answered_by_the_origin)) in [
        (
            "a behaviour forwarding the three headers a preflight asks with",
            &[
                "Origin",
                "Access-Control-Request-Method",
                "Access-Control-Request-Headers",
            ][..],
            true,
        ),
        ("a behaviour forwarding no header at all", &[][..], false),
    ]
    .into_iter()
    .enumerate()
    {
        let (id, domain) = create_distribution(
            &router,
            case,
            &format!("preflight-{index}"),
            &origin_host,
            "",
            &forwarded_values(names),
            &["GET", "HEAD", "OPTIONS"],
        )
        .await;
        wait_until_deployed(&router, case, &id).await;

        let served = dial(
            case,
            &domain,
            "OPTIONS",
            &format!("/{OBJECT_KEY}"),
            &[
                ("Origin", BROWSER_ORIGIN),
                ("Access-Control-Request-Method", PREFLIGHT_METHOD),
            ],
        )
        .await;
        if answered_by_the_origin {
            assert_eq!(
                served.status, 200,
                "case {case:?}: expected the origin's own preflight answer, got {} with {:?}",
                served.status, served.body
            );
            assert_eq!(
                served.header(ALLOW_ORIGIN),
                "*",
                "case {case:?}: expected the cors rule's own grant, got {:?} (headers {:?})",
                served.header(ALLOW_ORIGIN),
                served.headers
            );
        } else {
            assert_ne!(
                served.status, 200,
                "case {case:?}: a preflight whose Origin reached no origin cannot be admitted, \
                 got {} with {:?}",
                served.status, served.body
            );
            assert!(
                served.header(ALLOW_ORIGIN).is_empty(),
                "case {case:?}: expected no cross-origin grant, got {:?}",
                served.header(ALLOW_ORIGIN)
            );
        }

        // And the object itself is served to a viewer that asks for it, whatever the behaviour
        // forwards: withholding a header is not refusing the request.
        let object = dial(case, &domain, "GET", &format!("/{OBJECT_KEY}"), &[]).await;
        assert_eq!(
            object.status, 200,
            "case {case:?}: expected the object through the edge, got {} with {:?}",
            object.status, object.body
        );
    }
}
