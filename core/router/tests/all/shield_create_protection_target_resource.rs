// SPDX-License-Identifier: CC0-1.0

//! AWS Shield Advanced resolves the resource a `CreateProtection` names against the
//! service that OWNS that resource, before it consults the account's subscription.
//!
//! RULE: `CreateProtection` "enables Shield Advanced for a specific Amazon Web Services
//! resource. The resource can be an Amazon CloudFront distribution, Amazon Route 53 hosted
//! zone, Global Accelerator standard accelerator, Elastic IP Address, Application Load
//! Balancer, or a Classic Load Balancer", and `CreateProtectionRequest$ResourceArn`
//! documents the ARN layout of each. Shield holds none of those resources, so a create
//! RESOLVES the ARN in the service that owns it first: an ARN that service reports holding
//! nothing under is refused with the modelled `InvalidResourceException` (HTTP 400) stating
//! the class it failed to validate, the ARN and Region the request carried and the answer
//! the owning service gave — ahead of the account-level subscription gate, so a caller that
//! sent a stale or wrong ARN is told about its ARN. An ARN that DOES resolve passes that
//! gate and is judged on the account alone.
//!
//! The refusal is GROUNDED in the owning service's answer: an ARN in no documented layout
//! and an emulator with no such service mounted establish nothing, so neither is refused
//! for its resource.
//!
//! The rows span what the rule generalises over: every protectable resource class (so six
//! owning services over four wire protocols), the two Elastic IP failures Amazon EC2
//! separates (an id it holds nothing under and a value that is not an allocation id), the
//! Region the ARN names (several Regions, and the empty Region of a partition-global
//! resource), resources that really exist against resources that do not, and ARNs of a
//! vendor the layouts do not name.
//!
//! The resolution goes over the in-process callback, so a single-service router has nobody
//! to ask: that is why this suite lives here rather than in the Shield crate, whose own
//! suites pin the halves of `CreateProtection` that need no other service.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region most calls here are signed for, and the Region the regional ARNs name.
const REGION: &str = "us-east-1";

/// The account id every emulated ARN carries (`engine::emulator_account_id`).
const ACCOUNT: &str = "000000000000";

/// The `X-Amz-Target` prefix of this service's `awsJson1_1` surface (the Shield crate's
/// `OPERATIONS.md` wire-protocol section).
const SHIELD_TARGET_PREFIX: &str = "AWSShield_20160616.";

/// The same, for AWS Global Accelerator (that crate's `OPERATIONS.md`).
const ACCELERATOR_TARGET_PREFIX: &str = "GlobalAccelerator_V20180706.";

/// The Amazon EC2 API version an `ec2Query` body carries.
const EC2_API_VERSION: &str = "2016-11-15";

/// The one sentence Shield words the absence of the account's subscription with — what a
/// `CreateProtection` is refused with once its resource HAS resolved and the account holds
/// no subscription.
const SUBSCRIPTION_NOT_FOUND: &str = "The subscription does not exist.";

/// The prefix of the sentence a request refused at the modelled-constraint boundary
/// carries, which is what a `Name` outside `ProtectionName`'s pattern is refused with.
const VALIDATION_PREFIX: &str = "Error validating the following inputs: ";

/// One answer: the HTTP status, the wire error code and the parsed body.
#[derive(Debug)]
struct Reply {
    status: u16,
    code: String,
    body: Value,
}

impl Reply {
    /// The error message as the `awsJson1_1` envelope carries it.
    fn message(&self) -> &str {
        self.body
            .get("message")
            .or_else(|| self.body.get("Message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
    }
}

/// The bare error code out of a `__type` / `x-amzn-errortype` value, which may carry a
/// namespace or a URL beside it.
fn bare_code(raw: &str) -> String {
    let without_url = raw.split(':').next().unwrap_or(raw);
    without_url
        .rsplit('#')
        .next()
        .unwrap_or(without_url)
        .to_string()
}

/// Every service this suite drives, over ONE shared engine, as `router/src/lib.rs` mounts
/// them — so Shield's in-process callback reaches these very services.
///
/// Both Elastic Load Balancing models are mounted: they publish one endpoint, and the
/// Classic front door is what serves a `Version=2015-12-01` envelope from the v2 model.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_elasticloadbalancing_elasticloadbalancing_v7::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_cloudfront_cloudfront2020_05_31::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_route53_awsdnsv20130401::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_globalaccelerator_globalaccelerator_v20180706::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_shield_awsshield_20160616::EmulatedService::new(engine),
    );
    router
}

/// The same emulator with NOTHING but Shield mounted: no owning service to resolve a
/// resource against.
fn shield_only() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_shield_awsshield_20160616::EmulatedService::new(engine),
    );
    router
}

/// One `awsJson1_1` call: `POST /` with `X-Amz-Target: <prefix><operation>`, signed for
/// `signing_name` so the router resolves the service that claims it.
async fn json_call(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    body: &Value,
) -> Reply {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("x-amz-target", format!("{target_prefix}{operation}"))
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEMULATOR/20260101/{REGION}/{signing_name}/\
                 aws4_request, SignedHeaders=host;x-amz-target, Signature=0123456789abcdef"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed test request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let header_code = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(bare_code);
    let bytes = response.into_body();
    let body: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    let code = header_code.unwrap_or_else(|| {
        body.get("__type")
            .and_then(Value::as_str)
            .map(bare_code)
            .unwrap_or_default()
    });
    Reply { status, code, body }
}

/// One call to Shield.
async fn shield(router: &MultiServiceRouter, operation: &str, body: &Value) -> Reply {
    json_call(router, "shield", SHIELD_TARGET_PREFIX, operation, body).await
}

/// `CreateProtection` for `name` over `resource_arn`.
async fn create_protection(router: &MultiServiceRouter, name: &str, resource_arn: &str) -> Reply {
    shield(
        router,
        "CreateProtection",
        &json!({ "Name": name, "ResourceArn": resource_arn }),
    )
    .await
}

/// Take the account's Shield Advanced subscription on, asserting the activation succeeded.
async fn subscribe(router: &MultiServiceRouter) {
    let reply = shield(router, "CreateSubscription", &json!({})).await;
    assert_eq!(
        reply.status, 200,
        "case CreateSubscription: expected HTTP 200, got {reply:?}"
    );
}

/// One `ec2Query` call: an `application/x-www-form-urlencoded` POST whose body carries
/// `Action=<Operation>`, answered with XML.
async fn ec2(
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={EC2_API_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(value);
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ec2.{region}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEMULATOR/20260101/{region}/ec2/aws4_request, \
                 SignedHeaders=host, Signature=0123456789abcdef"
            ),
        )
        .body(bytes::Bytes::from(body))
        .expect("a well-formed ec2Query request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One `awsQuery` call: an `application/x-www-form-urlencoded` POST naming the `Action`
/// and the API `version` of the model it is for, answered with XML.
async fn query_call(
    router: &MultiServiceRouter,
    region: &str,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{region}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEMULATOR/20260101/{region}/{signing_name}/\
                 aws4_request, SignedHeaders=host, Signature=0123456789abcdef"
            ),
        )
        .body(bytes::Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One `restXml` GET on `uri`, signed for `signing_name`.
async fn rest_get(
    router: &MultiServiceRouter,
    signing_name: &str,
    uri: &str,
) -> (u16, String) {
    let request = http::Request::builder()
        .method("GET")
        .uri(uri)
        .header("host", format!("{signing_name}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEMULATOR/20260101/{REGION}/{signing_name}/\
                 aws4_request, SignedHeaders=host, Signature=0123456789abcdef"
            ),
        )
        .body(bytes::Bytes::new())
        .expect("a well-formed restXml request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// Percent-encode one form component, leaving only RFC 3986's unreserved characters
/// literal — an ARN carries `:` and `/`, which a form parser would otherwise read.
fn form_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(byte));
            }
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }
    encoded
}

/// The text of the first `<name>` element of an XML body.
fn element(body: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].to_string())
}

/// Allocate one Elastic IP address out of Amazon's pool in `region`, answering the ARN of
/// the allocation — a resource Amazon EC2 really holds.
async fn allocated_elastic_ip(router: &MultiServiceRouter, region: &str) -> String {
    let (status, body) = ec2(router, region, "AllocateAddress", &[]).await;
    assert_eq!(
        status, 200,
        "case AllocateAddress: expected HTTP 200, got {status} with body {body}"
    );
    let allocation_id = element(&body, "allocationId")
        .unwrap_or_else(|| panic!("case AllocateAddress: minted no allocation id: {body}"));
    format!("arn:aws:ec2:{region}:{ACCOUNT}:eip-allocation/{allocation_id}")
}

/// Create one standard accelerator, answering its ARN — a resource AWS Global Accelerator
/// really holds.
async fn created_accelerator(router: &MultiServiceRouter, name: &str) -> String {
    let reply = json_call(
        router,
        "globalaccelerator",
        ACCELERATOR_TARGET_PREFIX,
        "CreateAccelerator",
        &json!({ "Name": name, "IdempotencyToken": format!("{name}-token") }),
    )
    .await;
    assert_eq!(
        reply.status, 200,
        "case CreateAccelerator: expected HTTP 200, got {reply:?}"
    );
    reply
        .body
        .get("Accelerator")
        .and_then(|accelerator| accelerator.get("AcceleratorArn"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case CreateAccelerator: reported no ARN: {:?}", reply.body))
        .to_string()
}

/// The service that owns a protectable resource class, and so the read that resolves one.
#[derive(Debug, Clone, Copy)]
enum Owner {
    /// Amazon EC2 `DescribeAddresses` over the allocation id (`ec2Query`).
    Ec2,
    /// Elastic Load Balancing v2 `DescribeLoadBalancers` over the ARN (`awsQuery`).
    LoadBalancingV2,
    /// Elastic Load Balancing `DescribeLoadBalancers` over the name (`awsQuery`).
    LoadBalancing,
    /// Amazon CloudFront `GetDistribution` (`restXml`).
    CloudFront,
    /// AWS Global Accelerator `DescribeAccelerator` (`awsJson1_1`).
    GlobalAccelerator,
    /// Amazon Route 53 `GetHostedZone` (`restXml`).
    Route53,
}

/// One protectable resource class, named by an ARN the owning service holds nothing under.
struct AbsentResource {
    case: &'static str,
    /// The ARN, in the layout `CreateProtectionRequest$ResourceArn` documents for the class.
    resource_arn: &'static str,
    /// The Region field of that ARN, which the refusal states — empty for a
    /// partition-global resource.
    region: &'static str,
    /// How Shield's refusal names the class being validated.
    label: &'static str,
    /// The SDK client the owning service is read through, as the refusal names it.
    sdk_service: &'static str,
    /// The service that owns the class, asked the SAME question directly by this suite, so
    /// what the refusal embeds is compared with what that service itself answers rather
    /// than with a copy of it kept here.
    owner: Owner,
    /// The identifier that owner's own read takes out of the ARN.
    resource_id: &'static str,
}

/// One row per protectable resource class, plus the second Elastic IP failure Amazon EC2
/// separates and a second Region for the regional classes.
const ABSENT_RESOURCES: &[AbsentResource] = &[
    AbsentResource {
        case: "an Elastic IP allocation no address of the account has",
        resource_arn: "arn:aws:ec2:us-east-1:000000000000:eip-allocation/eipalloc-0123456789abcdef0",
        region: "us-east-1",
        label: "EIP",
        sdk_service: "AmazonEC2",
        owner: Owner::Ec2,
        resource_id: "eipalloc-0123456789abcdef0",
    },
    AbsentResource {
        case: "a value that is not an allocation id at all, in another Region",
        resource_arn: "arn:aws:ec2:eu-west-2:000000000000:eip-allocation/not-an-allocation",
        region: "eu-west-2",
        label: "EIP",
        sdk_service: "AmazonEC2",
        owner: Owner::Ec2,
        resource_id: "not-an-allocation",
    },
    AbsentResource {
        case: "an Application Load Balancer the account does not hold",
        resource_arn: "arn:aws:elasticloadbalancing:us-east-1:000000000000:loadbalancer/app/web/50dc6c495c0c9188",
        region: "us-east-1",
        label: "ELB",
        sdk_service: "AmazonElasticLoadBalancing",
        owner: Owner::LoadBalancingV2,
        resource_id: "arn:aws:elasticloadbalancing:us-east-1:000000000000:loadbalancer/app/web/50dc6c495c0c9188",
    },
    AbsentResource {
        case: "a Classic Load Balancer the account does not hold, in another Region",
        resource_arn: "arn:aws:elasticloadbalancing:us-west-2:000000000000:loadbalancer/legacy-web",
        region: "us-west-2",
        label: "ELB",
        sdk_service: "AmazonElasticLoadBalancing",
        owner: Owner::LoadBalancing,
        resource_id: "legacy-web",
    },
    AbsentResource {
        case: "a CloudFront distribution the account does not hold",
        resource_arn: "arn:aws:cloudfront::000000000000:distribution/E1XMPLDISTRIBUTION",
        region: "",
        label: "CloudFront",
        sdk_service: "AmazonCloudFront",
        owner: Owner::CloudFront,
        resource_id: "E1XMPLDISTRIBUTION",
    },
    AbsentResource {
        case: "a Global Accelerator standard accelerator the account does not hold",
        resource_arn: "arn:aws:globalaccelerator::000000000000:accelerator/1234abcd-abcd-1234-abcd-1234abcdefgh",
        region: "",
        label: "GlobalAccelerator",
        sdk_service: "AWSGlobalAccelerator",
        owner: Owner::GlobalAccelerator,
        resource_id: "arn:aws:globalaccelerator::000000000000:accelerator/1234abcd-abcd-1234-abcd-1234abcdefgh",
    },
    AbsentResource {
        case: "a Route 53 hosted zone the account does not hold",
        resource_arn: "arn:aws:route53:::hostedzone/Z1XMPLHOSTEDZONE",
        region: "",
        label: "Route53",
        sdk_service: "AmazonRoute53",
        owner: Owner::Route53,
        resource_id: "Z1XMPLHOSTEDZONE",
    },
];

/// What the service that owns `row`'s class answers when IT is asked the same question —
/// the status, the error code and the sentence it words — read over that service's own
/// wire rather than restated here.
async fn owner_answer(router: &MultiServiceRouter, row: &AbsentResource) -> (u16, String, String) {
    match row.owner {
        Owner::Ec2 => {
            let (status, body) = ec2(
                router,
                row.region,
                "DescribeAddresses",
                &[("AllocationId.1", row.resource_id)],
            )
            .await;
            (status, xml_code(&body), xml_message(&body))
        }
        Owner::LoadBalancingV2 => {
            let (status, body) = query_call(
                router,
                row.region,
                "elasticloadbalancing",
                "2015-12-01",
                "DescribeLoadBalancers",
                &[("LoadBalancerArns.member.1", row.resource_id)],
            )
            .await;
            (status, xml_code(&body), xml_message(&body))
        }
        Owner::LoadBalancing => {
            let (status, body) = query_call(
                router,
                row.region,
                "elasticloadbalancing",
                "2012-06-01",
                "DescribeLoadBalancers",
                &[("LoadBalancerNames.member.1", row.resource_id)],
            )
            .await;
            (status, xml_code(&body), xml_message(&body))
        }
        Owner::CloudFront => {
            let (status, body) = rest_get(
                router,
                "cloudfront",
                &format!("/2020-05-31/distribution/{}", row.resource_id),
            )
            .await;
            (status, xml_code(&body), xml_message(&body))
        }
        Owner::Route53 => {
            let (status, body) = rest_get(
                router,
                "route53",
                &format!("/2013-04-01/hostedzone/{}", row.resource_id),
            )
            .await;
            (status, xml_code(&body), xml_message(&body))
        }
        Owner::GlobalAccelerator => {
            let reply = json_call(
                router,
                "globalaccelerator",
                ACCELERATOR_TARGET_PREFIX,
                "DescribeAccelerator",
                &json!({ "AcceleratorArn": row.resource_id }),
            )
            .await;
            (reply.status, reply.code.clone(), reply.message().to_string())
        }
    }
}

/// The error code an XML error envelope carries (`<Code>`), whichever XML envelope the
/// answering protocol words.
fn xml_code(body: &str) -> String {
    element(body, "Code").unwrap_or_default()
}

/// The sentence an XML error envelope carries (`<Message>`), with the entities an XML
/// writer escapes it with read back — an apostrophe in a quoted identifier travels as
/// `&apos;`.
fn xml_message(body: &str) -> String {
    let escaped = element(body, "Message").unwrap_or_default();
    escaped
        .replace("&apos;", "'")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// What the refusal of `row` must open with: the class, the ARN and the Region the request
/// carried.
fn refusal_opening(row: &AbsentResource) -> String {
    format!(
        "Failed to validate {} resource: {} in region: {}, error: ",
        row.label, row.resource_arn, row.region
    )
}

/// What it must close with, past the sentence the owning service worded: that service, the
/// status and code THAT SERVICE answered, the id of the internal call and the proxy it was
/// made through. The call's id is per call, so it is read out of the message rather than
/// predicted — and asserted to be there and non-empty.
fn refusal_closing(row: &AbsentResource, status: u16, code: &str, message: &str) -> String {
    let opening = format!(
        "(Service: {}; Status Code: {status}; Error Code: {code}; Request ID: ",
        row.sdk_service
    );
    let request_id = message
        .rsplit_once(&opening)
        .map(|(_, tail)| tail.trim_end_matches("; Proxy: null)").to_string())
        .unwrap_or_default();
    assert!(
        !request_id.is_empty(),
        "case {}: the refusal must state the id of the call it was made for, got {message:?}",
        row.case
    );
    format!("{opening}{request_id}; Proxy: null)")
}

#[tokio::test]
async fn every_protectable_class_is_refused_where_the_service_that_owns_it_holds_nothing() {
    for row in ABSENT_RESOURCES {
        let router = emulator();
        // Subscribed, so nothing about the account can decide this answer: what is being
        // asserted is the resource gate itself.
        subscribe(&router).await;

        let reply = create_protection(&router, "resource-gate", row.resource_arn).await;
        assert_eq!(
            (reply.status, reply.code.as_str()),
            (400, "InvalidResourceException"),
            "case {}: expected HTTP 400 InvalidResourceException, got {reply:?}",
            row.case
        );
        let message = reply.message().to_string();
        let opening = refusal_opening(row);
        assert!(
            message.starts_with(&opening),
            "case {}: expected the refusal to open {opening:?}, got {message:?}",
            row.case
        );
        // What the owning service answers when THIS suite asks it the same question, over
        // that service's own wire: the refusal has to carry that answer rather than a
        // sentence, status or code of Shield's own.
        let (status, code, answered) = owner_answer(&router, row).await;
        assert!(
            !code.is_empty() && !answered.is_empty(),
            "case {}: expected the owning service to report the absence, got {status} \
             {code:?} {answered:?}",
            row.case
        );
        let closing = refusal_closing(row, status, &code, &message);
        let expected = format!("{opening}{answered} {closing}");
        assert_eq!(
            message, expected,
            "case {}: expected the refusal {expected:?}, got {message:?}",
            row.case
        );
        // Nothing was stored for a refused create.
        let listed = shield(&router, "ListProtections", &json!({})).await;
        assert_eq!(
            listed.body.get("Protections"),
            Some(&json!([])),
            "case {}: a refused create must store no protection, got {listed:?}",
            row.case
        );
    }
}

#[tokio::test]
async fn the_elastic_ip_refusal_states_the_answer_amazon_ec2_gave() {
    // The one class whose whole refusal is recorded: the sentence between the opening and
    // the closing is Amazon EC2's own, naming the allocation id the ARN carried.
    let row = &ABSENT_RESOURCES[0];
    let allocation_id = "eipalloc-0123456789abcdef0";
    let router = emulator();
    subscribe(&router).await;

    let reply = create_protection(&router, "recorded-leg", row.resource_arn).await;
    let message = reply.message().to_string();
    let (status, code, _) = owner_answer(&router, row).await;
    let expected = format!(
        "{}The allocation ID '{allocation_id}' does not exist {}",
        refusal_opening(row),
        refusal_closing(row, status, &code, &message)
    );
    assert_eq!(
        (reply.status, reply.code.as_str(), message.as_str()),
        (400, "InvalidResourceException", expected.as_str()),
        "case an Elastic IP allocation that does not exist: expected HTTP 400 \
         InvalidResourceException {expected:?}, got {reply:?}"
    );
    assert_eq!(
        (status, code.as_str()),
        (400, "InvalidAllocationID.NotFound"),
        "case an Elastic IP allocation that does not exist: expected Amazon EC2's own \
         refusal of the allocation id, got {status} {code:?}"
    );
}

#[tokio::test]
async fn the_name_is_refused_before_the_resource_is_resolved() {
    let router = emulator();
    subscribe(&router).await;

    // A `Name` outside `ProtectionName`'s pattern (`^[ a-zA-Z0-9_\.\-]*$`) is refused at
    // the modelled-constraint boundary, with the resource never reached.
    let reply = create_protection(&router, "bad!name@@", ABSENT_RESOURCES[0].resource_arn).await;
    assert_eq!(
        (reply.status, reply.code.as_str()),
        (400, "ValidationException"),
        "case a Name outside the modelled pattern: expected HTTP 400 ValidationException, \
         got {reply:?}"
    );
    assert_eq!(
        reply.message(),
        format!("{VALIDATION_PREFIX}name"),
        "case a Name outside the modelled pattern: expected the input named back, got \
         {reply:?}"
    );
}

#[tokio::test]
async fn a_resource_the_owning_service_holds_passes_the_resource_gate() {
    let router = emulator();
    // Two classes, two owning services and two wire protocols, each resource really
    // created through its owner's own API.
    let elastic_ip = allocated_elastic_ip(&router, REGION).await;
    let accelerator = created_accelerator(&router, "shield-gate").await;

    for (case, resource_arn) in [
        ("an allocated Elastic IP address", elastic_ip.as_str()),
        ("a created standard accelerator", accelerator.as_str()),
    ] {
        // Before the account subscribes, a resource that RESOLVES is judged on the
        // account: the create is refused for the subscription, not for its resource.
        let reply = create_protection(&router, "resolved", resource_arn).await;
        assert_eq!(
            (reply.status, reply.code.as_str(), reply.message()),
            (400, "ResourceNotFoundException", SUBSCRIPTION_NOT_FOUND),
            "case {case}: expected the account-level refusal, got {reply:?}"
        );
    }

    subscribe(&router).await;
    for (case, resource_arn) in [
        ("an allocated Elastic IP address", elastic_ip.as_str()),
        ("a created standard accelerator", accelerator.as_str()),
    ] {
        let name = format!("protect-{}", resource_arn.len());
        let reply = create_protection(&router, &name, resource_arn).await;
        assert_eq!(
            reply.status, 200,
            "case {case}: expected HTTP 200 from a create over a resource that resolves, \
             got {reply:?}"
        );
        let protection_id = reply
            .body
            .get("ProtectionId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            !protection_id.is_empty(),
            "case {case}: expected a minted ProtectionId, got {reply:?}"
        );

        // Read the protection back through the implemented describe: the resource gate
        // admitted it and the record is what a later caller sees.
        let described = shield(
            &router,
            "DescribeProtection",
            &json!({ "ResourceArn": resource_arn }),
        )
        .await;
        let protection = described.body.get("Protection").cloned().unwrap_or_default();
        assert_eq!(
            (
                described.status,
                protection.get("Id").and_then(Value::as_str),
                protection.get("Name").and_then(Value::as_str),
                protection.get("ResourceArn").and_then(Value::as_str),
            ),
            (
                200,
                Some(protection_id.as_str()),
                Some(name.as_str()),
                Some(resource_arn)
            ),
            "case {case}: expected the protection to read back as it was created, got \
             {described:?}"
        );
    }
}

#[tokio::test]
async fn an_arn_in_no_documented_layout_is_not_refused_for_its_resource() {
    // The layouts `CreateProtectionRequest$ResourceArn` documents are the six protectable
    // classes; an ARN outside them names a resource this emulator resolves nothing about,
    // so nothing is established and nothing is invented.
    let unplaced: &[(&str, &str)] = &[
        (
            "a queue of another service",
            "arn:aws:sqs:us-east-1:000000000000:nothing-holds-this",
        ),
        ("a bucket of another service", "arn:aws:s3:::nothing-holds-this"),
        (
            "a load balancer of a class the layouts do not name",
            "arn:aws:elasticloadbalancing:us-east-1:000000000000:loadbalancer/net/web/50dc6c495c0c9188",
        ),
        (
            "an Amazon EC2 resource of another type",
            "arn:aws:ec2:us-east-1:000000000000:instance/i-0123456789abcdef0",
        ),
    ];

    for (case, resource_arn) in unplaced {
        let router = emulator();
        subscribe(&router).await;

        let reply = create_protection(&router, "unplaced", resource_arn).await;
        assert_eq!(
            reply.status, 200,
            "case {case}: an ARN in no documented layout resolves nothing, so the create \
             must not be refused for its resource, got {reply:?}"
        );
    }
}

#[tokio::test]
async fn an_emulator_with_no_owning_service_refuses_nothing_for_its_resource() {
    for row in ABSENT_RESOURCES {
        // The same ARNs as the refused rows above, with nobody mounted to resolve them:
        // the absence is not established, so the create is judged on the account alone.
        let router = shield_only();
        let unsubscribed = create_protection(&router, "ungrounded", row.resource_arn).await;
        assert_eq!(
            (unsubscribed.status, unsubscribed.code.as_str(), unsubscribed.message()),
            (400, "ResourceNotFoundException", SUBSCRIPTION_NOT_FOUND),
            "case {}: with no owning service to ask, expected the account-level refusal, \
             got {unsubscribed:?}",
            row.case
        );

        subscribe(&router).await;
        let subscribed = create_protection(&router, "ungrounded", row.resource_arn).await;
        assert_eq!(
            subscribed.status, 200,
            "case {}: with no owning service to ask, nothing may be invented about the \
             resource, got {subscribed:?}",
            row.case
        );
    }
}
