// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation-deployed CloudFront distribution is REACHED at the `DomainName` the
//! stack published: a viewer dials that address on a real socket and is served the object
//! in the stack's own Amazon S3 origin.
//!
//! The rule under test: **an address a service publishes for a client to dial — a
//! distribution's `DomainName`, which `AWS::CloudFront::Distribution` surfaces as its
//! `DomainName` attribute and a stack `Output` hands back — is an address this emulator
//! itself answers on, and what answers there is that distribution's edge, applying the
//! configuration the template declared over the origin it declared.**
//! `Distribution$DomainName` is "the domain name that corresponds to the distribution …
//! such as `d111111abcdef8.cloudfront.net`", and the only way a viewer uses a distribution
//! is to issue HTTP(S) to that domain — so a value published under a zone this emulator
//! does not serve names a host that resolves to nothing on the machine it runs on, and
//! nothing downstream of the deploy can be observed at all.
//!
//! Three crates have to agree for that to hold — CloudFormation makes the creates and
//! publishes the attribute, CloudFront mints the address and serves the edge, Amazon S3
//! holds the origin object — so the suite lives here and drives the emulator as it is
//! ASSEMBLED. The viewer here is a real client on a real connection: it opens a socket to
//! the published address and speaks HTTP/1.1 to it, which is the one thing an in-process
//! dispatch addressed by `Host` cannot show.
//!
//! The rows differ along what the rule generalises over: the template spelling (JSON and
//! YAML short form), the object's key form (a bare name and a nested prefix) and its bytes,
//! and the `ViewerProtocolPolicy` the behaviour names — so the one published address is
//! shown both serving the origin's object and answering CloudFront's own redirect for a
//! scheme the policy does not serve.
//!
//! Every expected value is computed here from what the row declares; the distribution's id
//! and its published address are CloudFront's to mint, and what is asserted about the
//! address is its FORM (a host and a port a client can dial), that CloudFront's own
//! `GetDistribution` reports the same one, and that it answers.

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// The awsQuery form encoder and the XML element readers this test crate writes ONCE
// (`tests/all/query_wire.rs`): "Nothing new should write another one — call this module."
use super::cloudfront_oac_s3_origin_edge::{edge_listener_authority, is_assigned_edge_domain};
use super::query_wire::{elements, form_encode, opt_element, outputs_of};

/// The Region every call here is signed for, and so the Region the stack's resources belong
/// to and the one the S3 origin host names.
const REGION: &str = "us-east-1";

/// CloudFront's control-plane host, and the API version its routes are under.
const CLOUDFRONT_HOST: &str = "cloudfront.amazonaws.com";
const API_VERSION: &str = "2020-05-31";

/// The `StackStatus` a completed create reports.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `Status` CloudFront reports a distribution in once it has reached the edge
/// locations, which is the state a viewer request is served in.
const DEPLOYED: &str = "Deployed";

/// The AWS-managed cache policy `Managed-CachingDisabled`, whose TTLs are all zero: every
/// viewer request under it reaches the origin, so each row is served the origin's own
/// answer rather than one cached before the object was written.
const CACHING_DISABLED: &str = "4135ea2d-6df8-44a3-9df3-4b5a84be39ad";

/// The origin id every row's one origin carries, and the one its behaviour targets.
const ORIGIN_ID: &str = "origin-bucket";

/// How long a stack operation and a distribution's propagation are given to reach their
/// terminal state, and how often each is read while waiting. GENEROUS: what is waited for
/// is the CONDITION each reports, polled until it holds, so this bound only decides when a
/// deployment that never settles is reported as a failure.
const SETTLES_WITHIN: Duration = Duration::from_secs(90);
const POLL_EVERY: Duration = Duration::from_millis(50);

/// How long the viewer's own socket is given to connect and to answer. Generous for the
/// same reason: the rule is about WHAT the published address answers, never about how fast
/// the host it runs on answers it.
const DIAL_WITHIN: Duration = Duration::from_secs(10);
const ANSWER_WITHIN: Duration = Duration::from_secs(30);

/// The emulator with the three services this suite exercises, mounted on ONE engine exactly
/// as `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other
/// suite here: a setup call carries a credential scope (which is what routes it and what
/// names the Region) without a computed signature, and a VIEWER carries no credential at
/// all, as a client of a distribution sends none.
fn build_router() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cloudfront_cloudfront2020_05_31::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    (router, engine)
}

/// A SigV4 `Authorization` header whose credential scope names `service` in [`REGION`],
/// which is what routes the request and what each handler reads its Region from.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Dispatch one request as it arrives on the wire.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> (u16, String) {
    let mut builder = ::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = router
        .dispatch(builder.body(body).expect("a well-formed request"))
        .await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    dispatch(
        router,
        "POST",
        &format!("cloudformation.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &authorization("cloudformation")),
        ],
        Bytes::from(body),
    )
    .await
}

/// Call one CloudFront control-plane operation over its restXml wire.
async fn cloudfront(router: &MultiServiceRouter, method: &str, path: &str) -> (u16, String) {
    dispatch(
        router,
        method,
        CLOUDFRONT_HOST,
        path,
        &[("authorization", &authorization("cloudfront"))],
        Bytes::new(),
    )
    .await
}

/// One Amazon S3 call, addressed PATH-STYLE at the regional endpoint so the bucket is named
/// in the path rather than in the host.
async fn s3(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Bytes,
) -> (u16, String) {
    dispatch(
        router,
        method,
        &format!("s3.{REGION}.amazonaws.com"),
        path,
        &[("authorization", &authorization("s3"))],
        body,
    )
    .await
}

/// The `StackStatus` the stack reports now, with the answer it was read from.
async fn stack_status(router: &MultiServiceRouter, stack: &str) -> (String, String) {
    let (status, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "DescribeStacks {stack:?}: expected HTTP 200, got {status} with body {body}"
    );
    (opt_element(&body, "StackStatus").unwrap_or_default(), body)
}

/// Poll `stack` until it first reports `wanted`, answering the last status it reported and
/// the `DescribeStacks` answer it was read from. The wait is on that CONDITION, looked for
/// again until it holds or [`SETTLES_WITHIN`] passes.
async fn wait_for_stack(
    router: &MultiServiceRouter,
    stack: &str,
    wanted: &str,
) -> (String, String) {
    let giving_up_at = Instant::now() + SETTLES_WITHIN;
    let mut last = stack_status(router, stack).await;
    while last.0 != wanted && Instant::now() < giving_up_at {
        tokio::time::sleep(POLL_EVERY).await;
        last = stack_status(router, stack).await;
    }
    last
}

/// Poll `GetDistribution` until CloudFront first reports the distribution [`DEPLOYED`],
/// answering the `Distribution` document that read carried. The same wait on a CONDITION.
async fn wait_until_deployed(router: &MultiServiceRouter, case: &str, id: &str) -> String {
    let path = format!("/{API_VERSION}/distribution/{id}");
    let giving_up_at = Instant::now() + SETTLES_WITHIN;
    loop {
        let (status, body) = cloudfront(router, "GET", &path).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetDistribution({id}), got {status} with \
             body {body}"
        );
        if opt_element(&body, "Status").as_deref() == Some(DEPLOYED) {
            return body;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: CloudFront never reported the distribution {id} {DEPLOYED}, last \
             read {body}"
        );
        tokio::time::sleep(POLL_EVERY).await;
    }
}

/// What a viewer's own connection answered: the status line's code, the headers and the body.
///
/// `pub(crate)` with [`parsed`]: this is the ONE reading this test crate keeps of an HTTP/1.1
/// answer that arrived on a published address's own socket, so a suite that dials another
/// published address — `elbv2_stack_estate_from_a_json_template`, over a load balancer's — takes
/// the same framing rather than a second copy of it.
pub(crate) struct Served {
    pub(crate) status: u16,
    pub(crate) headers: BTreeMap<String, String>,
    pub(crate) body: String,
    pub(crate) raw: String,
}

impl Served {
    /// A response header's value, by its lowercased name.
    fn header(&self, name: &str) -> String {
        self.headers.get(name).cloned().unwrap_or_default()
    }
}

/// Dial `authority` in the clear and send one HTTP/1.1 `GET`, answering what came back.
///
/// This is the VIEWER: a client holding nothing but the address the stack published, with
/// no AWS credential of any kind, speaking the protocol that address answers rather than
/// going through the router. What it proves is that the PUBLISHED address serves.
///
/// The dial names the LOOPBACK interface explicitly and takes only the PORT from the
/// published authority — the address under test is one of this emulator's own listeners,
/// which the caller has already held it to — so nothing here opens a connection anywhere
/// else.
async fn dial(case: &str, authority: &str, host: &str, path: &str) -> Served {
    let port = authority
        .parse::<SocketAddr>()
        .unwrap_or_else(|error| {
            panic!("case {case:?}: the published address {authority:?} is no address: {error}")
        })
        .port();
    let mut stream = tokio::time::timeout(
        DIAL_WITHIN,
        tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port)),
    )
    .await
    .unwrap_or_else(|_| {
        panic!("case {case:?}: the published address {authority:?} did not accept a connection")
    })
    .unwrap_or_else(|error| {
        panic!("case {case:?}: the published address {authority:?} refused a connection: {error}")
    });
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: the request to {authority:?} was not sent: {error}")
        });
    let mut answer = Vec::new();
    let read = tokio::time::timeout(ANSWER_WITHIN, stream.read_to_end(&mut answer))
        .await
        .unwrap_or_else(|_| {
            panic!("case {case:?}: the published address {authority:?} did not answer in time")
        });
    read.unwrap_or_else(|error| {
        panic!("case {case:?}: the answer from {authority:?} was not read: {error}")
    });
    parsed(case, &String::from_utf8_lossy(&answer))
}

/// One HTTP/1.1 answer, taken apart: the status line's code, the headers under their
/// lowercased names, and the body.
///
/// The body is taken by the `Content-Length` the answer states, so what is compared is the
/// entity the edge framed rather than everything that happened to arrive on the connection;
/// an answer that states no length is read to the end of the stream, which is what the
/// `Connection: close` of the dial asks for.
pub(crate) fn parsed(case: &str, raw: &str) -> Served {
    let (head, body) = raw.split_once("\r\n\r\n").unwrap_or_else(|| {
        panic!("case {case:?}: the answer carries no end of headers: {raw:?}")
    });
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or_else(|| {
            panic!("case {case:?}: the answer carries no status code: {status_line:?}")
        });
    let headers: BTreeMap<String, String> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    let framed = headers
        .get("content-length")
        .and_then(|length| length.parse::<usize>().ok())
        .and_then(|length| body.get(..length))
        .unwrap_or(body);
    Served {
        status,
        headers,
        body: framed.to_string(),
        raw: raw.to_string(),
    }
}

/// What a row expects the viewer dialling the published address in the clear to be served.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The origin object, byte for byte, with HTTP 200.
    TheObject,
    /// CloudFront's own `301` to the HTTPS URL of the same request, which is what a
    /// `redirect-to-https` behaviour answers a plaintext viewer — served at the same
    /// published address, and carrying none of the object's bytes.
    RedirectToHttps,
}

/// How a row writes its template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spelling {
    Json,
    /// YAML with the short-form intrinsics (`!Ref`, `!GetAtt`).
    Yaml,
}

/// One row: the estate a template declares, the object put into its origin afterwards, and
/// what the viewer dialling the published address is then served.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// This row's own bucket, so no row's object or grant can serve another's read.
    bucket: &'static str,
    key: &'static str,
    body: &'static str,
    /// The `ViewerProtocolPolicy` the default cache behaviour names, out of the values
    /// `smithy_query(query="shape",shape="ViewerProtocolPolicy")` lists (allow-all,
    /// https-only, redirect-to-https).
    viewer_protocol_policy: &'static str,
    spelling: Spelling,
    expected: Expected,
}

const ROWS: &[Row] = &[
    Row {
        case: "an object at the bucket root, served to a plaintext viewer",
        stack: "cdn-published-root",
        bucket: "cdn-published-root-origin",
        key: "index.txt",
        body: "the object the stack's origin holds",
        viewer_protocol_policy: "allow-all",
        spelling: Spelling::Json,
        expected: Expected::TheObject,
    },
    Row {
        case: "an object under a nested prefix, declared in YAML short form",
        stack: "cdn-published-nested",
        bucket: "cdn-published-nested-origin",
        key: "assets/2026/report.csv",
        body: "region,served\nus-east-1,yes\n",
        viewer_protocol_policy: "allow-all",
        spelling: Spelling::Yaml,
        expected: Expected::TheObject,
    },
    Row {
        case: "a redirect-to-https behaviour, dialled in the clear",
        stack: "cdn-published-redirect",
        bucket: "cdn-published-redirect-origin",
        key: "index.txt",
        body: "never served to a plaintext viewer",
        viewer_protocol_policy: "redirect-to-https",
        spelling: Spelling::Json,
        expected: Expected::RedirectToHttps,
    },
];

/// The bucket policy the template declares: the grant an origin access control's signed
/// fetch is admitted by — `cloudfront.amazonaws.com` reading an object of this bucket.
fn policy_document(bucket: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\
         \"Sid\":\"AllowCloudFrontServicePrincipal\",\"Effect\":\"Allow\",\
         \"Principal\":{{\"Service\":\"cloudfront.amazonaws.com\"}},\
         \"Action\":\"s3:GetObject\",\"Resource\":\"arn:aws:s3:::{bucket}/*\"}}]}}"
    )
}

/// The row's template: the origin bucket, the origin access control its origin is read
/// through, the bucket policy granting that read, the distribution over it, and the outputs
/// a client deploying the stack reads — the bucket to put an object into and the domain name
/// to dial.
fn template(row: &Row) -> String {
    match row.spelling {
        Spelling::Json => json_template(row),
        Spelling::Yaml => yaml_template(row),
    }
}

fn json_template(row: &Row) -> String {
    let document: serde_json::Value = serde_json::from_str(&policy_document(row.bucket))
        .expect("the policy document this suite wrote");
    serde_json::json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "OriginBucket": {
                "Type": "AWS::S3::Bucket",
                "Properties": {"BucketName": row.bucket}
            },
            "Access": {
                "Type": "AWS::CloudFront::OriginAccessControl",
                "Properties": {"OriginAccessControlConfig": {
                    "Name": format!("{}-access", row.stack),
                    "SigningProtocol": "sigv4",
                    "SigningBehavior": "always",
                    "OriginAccessControlOriginType": "s3"
                }}
            },
            "BucketPolicy": {
                "Type": "AWS::S3::BucketPolicy",
                "Properties": {
                    "Bucket": {"Ref": "OriginBucket"},
                    "PolicyDocument": document
                }
            },
            "Distribution": {
                "Type": "AWS::CloudFront::Distribution",
                "DependsOn": ["BucketPolicy"],
                "Properties": {"DistributionConfig": {
                    "Enabled": true,
                    "Comment": row.case,
                    "Origins": [{
                        "Id": ORIGIN_ID,
                        "DomainName": {"Fn::GetAtt": ["OriginBucket", "RegionalDomainName"]},
                        "S3OriginConfig": {"OriginAccessIdentity": ""},
                        "OriginAccessControlId": {"Ref": "Access"}
                    }],
                    "DefaultCacheBehavior": {
                        "TargetOriginId": ORIGIN_ID,
                        "ViewerProtocolPolicy": row.viewer_protocol_policy,
                        "CachePolicyId": CACHING_DISABLED
                    }
                }}
            }
        },
        "Outputs": {
            "BucketName": {"Value": {"Ref": "OriginBucket"}},
            "DistributionId": {"Value": {"Fn::GetAtt": ["Distribution", "Id"]}},
            "DomainName": {"Value": {"Fn::GetAtt": ["Distribution", "DomainName"]}}
        }
    })
    .to_string()
}

fn yaml_template(row: &Row) -> String {
    [
        "AWSTemplateFormatVersion: '2010-09-09'".to_string(),
        "Resources:".to_string(),
        "  OriginBucket:".to_string(),
        "    Type: AWS::S3::Bucket".to_string(),
        "    Properties:".to_string(),
        format!("      BucketName: {}", row.bucket),
        "  Access:".to_string(),
        "    Type: AWS::CloudFront::OriginAccessControl".to_string(),
        "    Properties:".to_string(),
        "      OriginAccessControlConfig:".to_string(),
        format!("        Name: {}-access", row.stack),
        "        SigningProtocol: sigv4".to_string(),
        "        SigningBehavior: always".to_string(),
        "        OriginAccessControlOriginType: s3".to_string(),
        "  BucketPolicy:".to_string(),
        "    Type: AWS::S3::BucketPolicy".to_string(),
        "    Properties:".to_string(),
        "      Bucket: !Ref OriginBucket".to_string(),
        "      PolicyDocument:".to_string(),
        "        Version: '2012-10-17'".to_string(),
        "        Statement:".to_string(),
        "          - Sid: AllowCloudFrontServicePrincipal".to_string(),
        "            Effect: Allow".to_string(),
        "            Principal:".to_string(),
        "              Service: cloudfront.amazonaws.com".to_string(),
        "            Action: s3:GetObject".to_string(),
        format!("            Resource: arn:aws:s3:::{}/*", row.bucket),
        "  Distribution:".to_string(),
        "    Type: AWS::CloudFront::Distribution".to_string(),
        "    DependsOn:".to_string(),
        "      - BucketPolicy".to_string(),
        "    Properties:".to_string(),
        "      DistributionConfig:".to_string(),
        "        Enabled: true".to_string(),
        format!("        Comment: {}", row.case),
        "        Origins:".to_string(),
        format!("          - Id: {ORIGIN_ID}"),
        "            DomainName: !GetAtt OriginBucket.RegionalDomainName".to_string(),
        "            S3OriginConfig:".to_string(),
        "              OriginAccessIdentity: ''".to_string(),
        "            OriginAccessControlId: !Ref Access".to_string(),
        "        DefaultCacheBehavior:".to_string(),
        format!("          TargetOriginId: {ORIGIN_ID}"),
        format!(
            "          ViewerProtocolPolicy: {}",
            row.viewer_protocol_policy
        ),
        format!("          CachePolicyId: {CACHING_DISABLED}"),
        "Outputs:".to_string(),
        "  BucketName:".to_string(),
        "    Value: !Ref OriginBucket".to_string(),
        "  DistributionId:".to_string(),
        "    Value: !GetAtt Distribution.Id".to_string(),
        "  DomainName:".to_string(),
        "    Value: !GetAtt Distribution.DomainName".to_string(),
    ]
    .join("\n")
}

/// Whether `value` is the DNS name CloudFront ASSIGNS a distribution, which is what the
/// `DomainName` the stack publishes carries — one label under CloudFront's own zone, and no
/// address, port or path
/// ([`crate::cloudfront_oac_s3_origin_edge::is_assigned_edge_domain`] is the whole rule).
///
/// A name in that zone resolves to nothing on this machine, so the viewer below dials the
/// address this emulator opened for the distribution and carries the published name as the host
/// it addressed — which is what a client that resolved the name sends.
fn is_the_assigned_domain_name(value: &str) -> bool {
    is_assigned_edge_domain(value)
}

#[tokio::test]
async fn a_deployed_distribution_is_reached_at_the_domain_name_its_stack_published() {
    let (router, engine) = build_router();

    for row in ROWS {
        let case = row.case;

        // Deploy the estate by template, exactly as a client of the emulator does.
        let (status, answered) = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", row.stack),
                ("TemplateBody", &template(row)),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body \
             {answered}"
        );

        let (reported, described) = wait_for_stack(&router, row.stack, CREATE_COMPLETE).await;
        let failures = if reported == CREATE_COMPLETE {
            Vec::new()
        } else {
            let (_, events) =
                cloudformation(&router, "DescribeStackEvents", &[("StackName", row.stack)]).await;
            elements(&events, "ResourceStatusReason")
        };
        assert_eq!(
            reported, CREATE_COMPLETE,
            "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {described} with \
             resource failures {failures:?}"
        );

        // What the client reads back: the bucket to put the object into, and the address to
        // dial. Both come out of the stack's own OUTPUTS, never out of the answer under test.
        let outputs = outputs_of(&described);
        let published = |key: &str| {
            outputs.get(key).cloned().unwrap_or_else(|| {
                panic!("case {case:?}: the stack published no {key} output in {described}")
            })
        };
        let bucket = published("BucketName");
        let distribution_id = published("DistributionId");
        let domain = published("DomainName");
        assert_eq!(
            bucket, row.bucket,
            "case {case:?}: expected the stack to publish the declared bucket name, got \
             {bucket:?}"
        );

        // The published name is the one CloudFront ASSIGNS, and it is the one the OWNING
        // service holds for this distribution: a stack publishes what CloudFront assigned,
        // not a second value of its own.
        assert!(
            is_the_assigned_domain_name(&domain),
            "case {case:?}: expected the DomainName output to be the DNS name CloudFront \
             assigns a distribution (d<label>.cloudfront.net), got {domain:?}"
        );
        let distribution = wait_until_deployed(&router, case, &distribution_id).await;
        assert_eq!(
            opt_element(&distribution, "DomainName").as_deref(),
            Some(domain.as_str()),
            "case {case:?}: expected CloudFront to report the published address {domain:?}, \
             got {distribution}"
        );

        // The object goes into the origin AFTER the deploy, as a client that reads the
        // stack's outputs and then uploads its content does.
        let (status, written) = s3(
            &router,
            "PUT",
            &format!("/{bucket}/{}", row.key),
            Bytes::from(row.body),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from PutObject({:?}), got {status} with body \
             {written}",
            row.key
        );

        // The viewer: a socket to the address the published name resolves to, carrying that
        // name as the host it addressed, and one HTTP/1.1 GET.
        let address = edge_listener_authority(&engine, &distribution_id).unwrap_or_else(|| {
            panic!(
                "case {case:?}: this emulator opened no address for the distribution \
                 {distribution_id}"
            )
        });
        let served = dial(case, &address, &domain, &format!("/{}", row.key)).await;
        match row.expected {
            Expected::TheObject => {
                assert_eq!(
                    served.status, 200,
                    "case {case:?}: expected the published address {domain:?} to serve HTTP \
                     200, got {} with {:?}",
                    served.status, served.raw
                );
                assert_eq!(
                    served.body, row.body,
                    "case {case:?}: expected the origin object's bytes {:?}, got {:?}",
                    row.body, served.body
                );
            }
            Expected::RedirectToHttps => {
                assert_eq!(
                    served.status, 301,
                    "case {case:?}: expected CloudFront's own redirect at {domain:?}, got {} \
                     with {:?}",
                    served.status, served.raw
                );
                assert_eq!(
                    served.header("location"),
                    format!("https://{domain}/{}", row.key),
                    "case {case:?}: expected the HTTPS URL of the same request in Location, \
                     got {:?}",
                    served.raw
                );
                assert!(
                    !served.body.contains(row.body),
                    "case {case:?}: expected none of the object's bytes in a refusal \
                     CloudFront produced itself, got {:?}",
                    served.body
                );
            }
        }
    }
}
