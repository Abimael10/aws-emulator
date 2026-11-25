// SPDX-License-Identifier: CC0-1.0

//! The LAMBDA@EDGE function a cache behaviour associates, INVOKED on the viewer event it
//! names — end to end over HTTP, across the services on one engine, and read off the address
//! the distribution PUBLISHED.
//!
//! RULE: a cache behaviour's `LambdaFunctionAssociation` names the published function version
//! CloudFront invokes on the viewer event its `EventType` states, and what the function
//! RETURNS is what the edge continues with — an object carrying `status` is a RESPONSE the
//! edge serves straight away, with its status, its headers and its bytes and with no origin
//! request made at all; any other object is the REQUEST the edge then keys on and fetches; and
//! a run that produced neither leaves the edge nothing to continue from, so the viewer is
//! answered CloudFront's own `502`. On the viewer-RESPONSE event the object the function
//! returns is the answer the viewer receives, over the bytes the edge already had.
//!
//! The association is also only STORED where the edge could invoke it at all, which is the
//! lifecycle the first test walks: an association naming `$LATEST` rather than a version, and
//! one whose execution role withholds the assume the edge makes of it, are each refused
//! `InvalidLambdaFunctionAssociation` (HTTP 400) with no distribution left behind, and the
//! SAME version ARN is carried once `UpdateAssumeRolePolicy` has admitted the edge principal.
//!
//! Everything the rule needs is staged through the SERVICES that own it, on one router over
//! one engine: the execution role through IAM, the function and its published version through
//! Lambda, and the distribution through CloudFront. So the function the edge invokes is one a
//! client really created, and the code that answers the viewer is the caller's own — it is
//! LAMBDA that runs it, over the emulator's own in-process transport, so what a viewer is
//! served and what an `Invoke` of that version answers cannot disagree.
//!
//! The rows differ along what the rule generalises over: the viewer path the event states
//! (the root, a one-segment path, a deeply nested one, one carrying a query string), the
//! encoding a generated response states its bytes under (`text` by default and `base64`), WHICH
//! of the two objects the function returns (a response it generated and a request the edge then
//! fetches), the viewer event the association fires on (viewer-request and viewer-response), and
//! a run that returns no object at all.
//!
//! The wire harness is [`crate::distribution_lifecycle`]'s, the dial of the published address
//! [`crate::published_edge_address`]'s and the refusal sentence
//! [`crate::lambda_edge_association`]'s, reused rather than copied.

use std::io::Write as _;
use std::sync::Arc;
use std::time::Instant;

use serde_json::json;
use service_com_amazonaws_cloudfront_cloudfront2020_05_31::EmulatedService;
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use crate::distribution_lifecycle::{
    awaited_deployment, create_distribution, edge_listener_authority, get_distribution,
    is_published_edge_address, list_distributions, API_VERSION, POLL_INTERVAL, READY_DEADLINE,
};
use crate::lambda_edge_association::{refusal_message, reported_arns};
use crate::published_edge_address::dial_as;

/// The Region the function lives in, which is the Region a Lambda@Edge function has to be
/// created in, and the Region every setup call below is made for.
const REGION: &str = "us-east-1";

/// The AWS-managed cache policy `Managed-CachingDisabled`, whose TTLs are all zero, so every
/// viewer request below reaches the plane that answers it and no row reads another row's
/// cached answer. Its id is the one this service's own managed catalogue publishes.
const CACHING_DISABLED: &str = "4135ea2d-6df8-44a3-9df3-4b5a84be39ad";

/// The marker the function puts in every object it returns, so an answer that carries it is
/// the function's own work.
const MARKER: &str = "SIM2REAL-EDGE";

/// The response header the function states on the object it returns, and the one it adds on a
/// viewer-response event.
const EDGE_HEADER: &str = "x-sim2real-edge";

/// The two viewer events an association can fire on.
const VIEWER_REQUEST: &str = "viewer-request";
const VIEWER_RESPONSE: &str = "viewer-response";

/// The two principals a Lambda@Edge execution role has to admit, and the assume they make.
const LAMBDA_PRINCIPAL: &str = "lambda.amazonaws.com";
const EDGE_LAMBDA_PRINCIPAL: &str = "edgelambda.amazonaws.com";
const ASSUME_ROLE: &str = "sts:AssumeRole";

/// The fixed part of the refusal of an association whose execution role withholds the edge's
/// assume, spelled out here rather than imported from the implementation: it is CloudFront's
/// own text for this error class. The resolved role's ARN follows it.
const TRUST_REFUSAL_PREFIX: &str = concat!(
    "The function execution role must be assumable with edgelambda.amazonaws.com as well ",
    "as lambda.amazonaws.com principals. Update the IAM role and try again. Role: "
);

/// The wire error code both refusals carry.
const REFUSAL_CODE: &str = "InvalidLambdaFunctionAssociation";

/// `X-Cache` for an answer a Lambda@Edge function generated, for one an origin produced, and
/// for the page CloudFront produces when that function handed it no object.
const LAMBDA_GENERATED: &str = "lambdageneratedresponse from cloudfront";
const MISS: &str = "miss from cloudfront";
const LAMBDA_ERROR: &str = "lambdaexecutionerror from cloudfront";

/// The ordered behaviour's path pattern, which is the control plane's own route prefix: a
/// viewer request matching it is fetched from the origin, and that origin is this service's
/// own control-plane endpoint, so the answer carries a document this emulator really served.
const ORIGIN_PATTERN: &str = "/2020-05-31/*";

/// A path under [`ORIGIN_PATTERN`] the origin answers, and the element its document carries.
const ORIGIN_PATH: &str = "/2020-05-31/distribution";
const ORIGIN_DOCUMENT: &str = "DistributionList";

/// The placeholder [`HANDLER_SOURCE`] carries for the path it rewrites a request to, replaced
/// with [`ORIGIN_PATH`] before the package is written: the source states dictionaries, so it
/// is not a format string.
const REWRITE_TARGET: &str = "REWRITE_TARGET_PATH";

/// The handler behind the association. It reads the Lambda@Edge event object CloudFront passes
/// it — one `Records` entry under `cf`, carrying the `config` of the request and the `request`
/// itself — and returns, for the viewer path it was handed, the object the edge is to continue
/// with: a response it generates, the request to fetch instead, or nothing at all. It imports
/// only the standard library, so every answer below is the handler's own work.
const HANDLER_SOURCE: &str = r#"
import base64

MARKER = "SIM2REAL-EDGE"


def lambda_handler(event, context):
    cf = event["Records"][0]["cf"]
    request = cf["request"]
    uri = request["uri"]
    if cf["config"]["eventType"] == "viewer-response":
        response = cf["response"]
        response["headers"]["x-sim2real-edge"] = [
            {"key": "X-Sim2Real-Edge", "value": MARKER + " " + uri}
        ]
        return response
    if uri.startswith("/rewrite/"):
        request["uri"] = "REWRITE_TARGET_PATH"
        return request
    if uri.startswith("/fail/"):
        raise Exception("this run hands the edge no object")
    body = MARKER + " " + uri
    if request["querystring"]:
        body = body + "?" + request["querystring"]
    headers = {
        "x-sim2real-edge": [{"key": "X-Sim2Real-Edge", "value": MARKER}],
        "content-type": [{"key": "Content-Type", "value": "text/plain"}],
    }
    if uri.startswith("/base64/"):
        return {
            "status": "200",
            "statusDescription": "OK",
            "headers": headers,
            "bodyEncoding": "base64",
            "body": base64.b64encode(body.encode()).decode(),
        }
    return {
        "status": "200",
        "statusDescription": "OK",
        "headers": headers,
        "body": body,
    }
"#;

/// A .zip deployment package holding [`HANDLER_SOURCE`] as `index.py`, written in memory the
/// way a caller uploads one.
fn package() -> Vec<u8> {
    let source = HANDLER_SOURCE.replace(REWRITE_TARGET, ORIGIN_PATH);
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = ::zip::ZipWriter::new(&mut buffer);
        writer
            .start_file("index.py", ::zip::write::SimpleFileOptions::default())
            .expect("the entry starts");
        writer
            .write_all(source.as_bytes())
            .expect("the entry's bytes are written");
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

// ---------------------------------------------------------------------------
// The emulator and what each service stages in it
// ---------------------------------------------------------------------------

/// CloudFront, Lambda, IAM and the token service over ONE shared engine, with a callback into
/// the same router: the function the edge invokes is run by that Lambda as a session of the
/// role IAM holds.
struct Topology {
    router: MultiServiceRouter,
    callback: EmbeddedEmulator,
    /// The shared engine, read for the address this emulator opened for a distribution's edge
    /// ([`crate::distribution_lifecycle::edge_listener_authority`]): a viewer dials that and
    /// states the name the distribution published.
    engine: Arc<engine::InMemoryEngine>,
}

impl Topology {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        let callback = EmbeddedEmulator::from_router(router.clone());
        Self {
            router,
            callback,
            engine,
        }
    }

    /// The `host:port` a viewer dials the distribution `id`'s edge at.
    fn edge_address(&self, case: &str, id: &str) -> String {
        edge_listener_authority(&self.engine, case, id)
    }

    /// The IAM client, built by the one shared constructor over the in-process callback.
    fn iam(&self) -> aws_sdk_iam::Client {
        ::service_common::iam_client::iam_client_through(&self.callback, REGION)
    }

    /// The Lambda client, over the same callback.
    fn lambda(&self) -> aws_sdk_lambda::Client {
        let config = aws_sdk_lambda::Config::builder()
            .behavior_version(aws_sdk_lambda::config::BehaviorVersion::latest())
            .region(aws_sdk_lambda::config::Region::new(REGION))
            .endpoint_url(format!("https://lambda.{REGION}.amazonaws.com"))
            .credentials_provider(self.callback.aws_credentials_provider())
            .http_client(self.callback.http_client())
            .retry_config(aws_sdk_lambda::config::retry::RetryConfig::disabled())
            .timeout_config(aws_sdk_lambda::config::timeout::TimeoutConfig::disabled())
            .identity_cache(aws_sdk_lambda::config::IdentityCache::no_cache())
            .stalled_stream_protection(
                aws_sdk_lambda::config::StalledStreamProtectionConfig::disabled(),
            )
            .build();
        aws_sdk_lambda::Client::from_conf(config)
    }

    /// `CreateRole` whose trust policy admits `principals` to assume it, and the ARN IAM
    /// answered with.
    async fn create_role(&self, case: &str, name: &str, principals: &[&str]) -> String {
        let created = self
            .iam()
            .create_role()
            .role_name(name)
            .assume_role_policy_document(trust_policy(principals))
            .send()
            .await
            .unwrap_or_else(|error| panic!("setup {case:?}: creating the role {name}: {error}"));
        created
            .role()
            .map(|role| role.arn().to_string())
            .filter(|arn| !arn.is_empty())
            .unwrap_or_else(|| panic!("setup {case:?}: the created role {name} reports no ARN"))
    }

    /// `UpdateAssumeRolePolicy`: the call a client repairs a role's trust policy with.
    async fn update_trust(&self, case: &str, name: &str, principals: &[&str]) {
        self.iam()
            .update_assume_role_policy()
            .role_name(name)
            .policy_document(trust_policy(principals))
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("setup {case:?}: updating the trust policy of {name}: {error}")
            });
    }

    /// `CreateFunction` over [`HANDLER_SOURCE`] running as `role_arn`, the wait for the state
    /// the clock owes it, and `PublishVersion`: the version ARN an association names, and the
    /// unqualified ARN beside it.
    async fn published_function(
        &self,
        case: &str,
        name: &str,
        role_arn: &str,
    ) -> (String, String) {
        let code = aws_sdk_lambda::types::FunctionCode::builder()
            .zip_file(::aws_smithy_types::Blob::new(package()))
            .build();
        let created = self
            .lambda()
            .create_function()
            .function_name(name)
            .role(role_arn)
            .runtime(aws_sdk_lambda::types::Runtime::Python312)
            .handler("index.lambda_handler")
            .code(code)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("setup {case:?}: creating the function {name}: {error}")
            });
        let function_arn = created
            .function_arn()
            .map(ToString::to_string)
            .filter(|arn| !arn.is_empty())
            .unwrap_or_else(|| panic!("setup {case:?}: the created function reports no ARN"));

        // An invocation needs the function Active, which is a span the service owes: the
        // CONDITION is what is waited for, never a span of the host's clock.
        let deadline = Instant::now() + READY_DEADLINE;
        loop {
            let configuration = self
                .lambda()
                .get_function_configuration()
                .function_name(name)
                .send()
                .await
                .unwrap_or_else(|error| {
                    panic!("setup {case:?}: reading the configuration of {name}: {error}")
                });
            let state = configuration
                .state()
                .map(|state| state.as_str().to_string())
                .unwrap_or_default();
            if state == "Active" {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "setup {case:?}: {name} never reached Active, last reported {state:?}"
            );
            ::tokio::time::sleep(POLL_INTERVAL).await;
        }

        let published = self
            .lambda()
            .publish_version()
            .function_name(name)
            .send()
            .await
            .unwrap_or_else(|error| panic!("setup {case:?}: publishing {name}: {error}"));
        let version_arn = published
            .function_arn()
            .map(ToString::to_string)
            .filter(|arn| !arn.is_empty())
            .unwrap_or_else(|| panic!("setup {case:?}: the published version reports no ARN"));
        (function_arn, version_arn)
    }
}

/// A trust policy whose single `Allow` admits `principals` to `sts:AssumeRole`.
fn trust_policy(principals: &[&str]) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": principals },
            "Action": ASSUME_ROLE,
        }],
    })
    .to_string()
}

/// The `LambdaFunctionAssociations` wrapper for one association, in the member order the
/// shape declares.
fn association_xml(arn: &str, event_type: &str) -> String {
    format!(
        "<LambdaFunctionAssociations><Quantity>1</Quantity><Items>\
         <LambdaFunctionAssociation><LambdaFunctionARN>{arn}</LambdaFunctionARN>\
         <EventType>{event_type}</EventType><IncludeBody>false</IncludeBody>\
         </LambdaFunctionAssociation></Items></LambdaFunctionAssociations>"
    )
}

/// The `DistributionConfig` under test: this service's own control-plane endpoint as the
/// origin, the viewer-request association on the DEFAULT cache behaviour, and the
/// viewer-response association on the ordered behaviour that matches [`ORIGIN_PATTERN`].
///
/// Both behaviours name the managed cache policy that caches nothing, so each row below is
/// answered by the plane that answers it rather than out of the edge cache.
fn config_xml(reference: &str, comment: &str, version_arn: &str) -> String {
    let host = crate::distribution_lifecycle::HOST;
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items>\
         <Origin><Id>primary</Id><DomainName>{host}</DomainName></Origin>\
         </Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>primary</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         {}<CachePolicyId>{CACHING_DISABLED}</CachePolicyId>\
         </DefaultCacheBehavior>\
         <CacheBehaviors><Quantity>1</Quantity><Items><CacheBehavior>\
         <PathPattern>{ORIGIN_PATTERN}</PathPattern>\
         <TargetOriginId>primary</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         {}<CachePolicyId>{CACHING_DISABLED}</CachePolicyId>\
         </CacheBehavior></Items></CacheBehaviors>\
         <Comment>{comment}</Comment>\
         <Enabled>true</Enabled>\
         </DistributionConfig>",
        association_xml(version_arn, VIEWER_REQUEST),
        association_xml(version_arn, VIEWER_RESPONSE),
    )
}

/// What one exchange on the published address observed.
struct Served {
    status: u16,
    /// The whole answer as it came off the connection, status line and headers included.
    raw: String,
    /// The body alone.
    body: String,
}

impl Served {
    /// The answer of one viewer request for the distribution published at `domain`, dialled in
    /// the clear at `address` with no credential: the client dials where that name resolves and
    /// states the name as the host it addressed.
    async fn of(case: &str, address: &str, domain: &str, path: &str) -> Self {
        let raw = dial_as(address, "GET", path, domain).await;
        let status = raw
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse::<u16>().ok())
            .unwrap_or_else(|| panic!("case {case:?}: no status line in {raw:?}"));
        let body = raw
            .split_once("\r\n\r\n")
            .map(|(_head, body)| body.to_string())
            .unwrap_or_default();
        Self { status, raw, body }
    }

    /// The value the answer carries for the header `name`, or `""`.
    fn header(&self, name: &str) -> String {
        let lowered = self.raw.to_ascii_lowercase();
        let marker = format!("\r\n{}: ", name.to_ascii_lowercase());
        lowered
            .find(&marker)
            .map(|at| at + marker.len())
            .and_then(|start| {
                let rest = &self.raw[start..];
                rest.find("\r\n").map(|end| rest[..end].trim().to_string())
            })
            .unwrap_or_default()
    }

    /// Whether the answer was reported as one the edge served from `x_cache`.
    fn reports_cache(&self, x_cache: &str) -> bool {
        self.header("x-cache").to_ascii_lowercase() == x_cache
    }
}

// ---------------------------------------------------------------------------
// The lifecycle: the two refusals, the repair, and the association that then serves
// ---------------------------------------------------------------------------

/// The whole trajectory of one Lambda@Edge association: an unpublished version refused, an
/// execution role the edge cannot assume refused, no distribution left behind by either, the
/// same version ARN carried once the role admits the edge, read back on the behaviour it was
/// stated on — and then, once the distribution is deployed, the function itself answering the
/// viewer at the address the distribution published.
#[tokio::test]
async fn the_edge_association_lifecycle_refuses_then_serves_the_functions_own_answer() {
    let case = "the Lambda@Edge association lifecycle";
    let topology = Topology::new();
    let role_name = "edge-lifecycle-role";
    let role_arn = topology
        .create_role(case, role_name, &[LAMBDA_PRINCIPAL])
        .await;
    let (function_arn, version_arn) = topology
        .published_function(case, "edge-lifecycle", &role_arn)
        .await;
    let comment = "the lambda edge association lifecycle";

    // (1) An association naming `$LATEST` names no published version at all.
    let unpublished = format!("{function_arn}:$LATEST");
    let refused = create_distribution(
        &topology.router,
        &config_xml("edge-lifecycle-latest", comment, &unpublished),
    )
    .await;
    assert_eq!(
        refused.status, 400,
        "case {case:?}: expected HTTP 400 for an association naming $LATEST, got {} with {}",
        refused.status, refused.raw
    );
    assert_eq!(
        refused.error_code(),
        REFUSAL_CODE,
        "case {case:?}: expected {REFUSAL_CODE} for an association naming $LATEST, got {}",
        refused.raw
    );
    assert_eq!(
        refused.root().find_text("Message").unwrap_or_default(),
        refusal_message(&unpublished),
        "case {case:?}: expected the version-requirement sentence naming the submitted ARN, \
         got {}",
        refused.raw
    );

    // (2) The published version, under a role the edge cannot assume.
    let withheld = create_distribution(
        &topology.router,
        &config_xml("edge-lifecycle-trust", comment, &version_arn),
    )
    .await;
    assert_eq!(
        withheld.status, 400,
        "case {case:?}: expected HTTP 400 for a role withholding the edge's assume, got {} \
         with {}",
        withheld.status, withheld.raw
    );
    assert_eq!(
        withheld.error_code(),
        REFUSAL_CODE,
        "case {case:?}: expected {REFUSAL_CODE} for a role withholding the edge's assume, got {}",
        withheld.raw
    );
    assert_eq!(
        withheld.root().find_text("Message").unwrap_or_default(),
        format!("{TRUST_REFUSAL_PREFIX}{role_arn}"),
        "case {case:?}: expected the trust sentence naming the resolved role, got {}",
        withheld.raw
    );

    // (3) Neither refusal left a distribution behind.
    let listed = list_distributions(&topology.router, "").await;
    assert!(
        !listed.raw.contains(comment),
        "case {case:?}: expected no distribution carrying the run's comment after the two \
         refusals, got {}",
        listed.raw
    );

    // (4) The same version ARN, once the role admits the edge principal too.
    topology
        .update_trust(
            case,
            role_name,
            &[LAMBDA_PRINCIPAL, EDGE_LAMBDA_PRINCIPAL],
        )
        .await;
    let created = create_distribution(
        &topology.router,
        &config_xml("edge-lifecycle-accepted", comment, &version_arn),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case {case:?}: expected HTTP 201 once the role admits the edge, got {} with {}",
        created.status, created.raw
    );
    let id = created
        .root()
        .find_text("Id")
        .unwrap_or_default()
        .to_string();
    let domain = created
        .root()
        .find_text("DomainName")
        .unwrap_or_default()
        .to_string();
    assert!(
        !id.is_empty() && is_published_edge_address(&domain),
        "case {case:?}: expected an id and an address this emulator answers on (host:port), got \
         {}",
        created.raw
    );

    // The configuration reads back carrying that association on the behaviour it was stated
    // on, under the event type it fires on.
    let read = get_distribution(&topology.router, &id).await;
    let arns = reported_arns(&read);
    assert!(
        arns.iter().any(|reported| reported == &version_arn),
        "case {case:?}: expected the stored association to name {version_arn}, got {arns:?}"
    );
    let events: Vec<String> = read
        .root()
        .descendant("DefaultCacheBehavior")
        .map(|behavior| {
            behavior
                .children_named("LambdaFunctionAssociations")
                .iter()
                .flat_map(|associations| {
                    associations
                        .children_named("Items")
                        .iter()
                        .flat_map(|items| {
                            items
                                .children_named("LambdaFunctionAssociation")
                                .iter()
                                .filter_map(|association| {
                                    association.text_at("EventType").map(str::to_string)
                                })
                                .collect::<Vec<String>>()
                        })
                        .collect::<Vec<String>>()
                })
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        events,
        vec![VIEWER_REQUEST.to_string()],
        "case {case:?}: expected the default behaviour's association on the viewer-request \
         event, got {events:?} in {}",
        read.raw
    );

    // (5) Deployed, the association ANSWERS: the function generates the response itself.
    awaited_deployment(&topology.router, case, &id).await;
    let address = topology.edge_address(case, &id);
    let served = Served::of(case, &address, &domain, "/sim2real/first").await;
    assert_eq!(
        served.status, 200,
        "case {case:?}: expected the function's own answer at the published address, got {} \
         with {:?}",
        served.status, served.raw
    );
    assert_eq!(
        served.header(EDGE_HEADER),
        MARKER,
        "case {case:?}: expected the header the function stated, got {:?}",
        served.raw
    );
    assert_eq!(
        served.body,
        format!("{MARKER} /sim2real/first"),
        "case {case:?}: expected the bytes the function stated, got {:?}",
        served.body
    );
    assert!(
        served.reports_cache(LAMBDA_GENERATED),
        "case {case:?}: expected the answer reported as the Lambda@Edge function's own, got {:?}",
        served.raw
    );
}

// ---------------------------------------------------------------------------
// What the edge continues with, over the objects a function can return
// ---------------------------------------------------------------------------

/// One row: the viewer path dialled, and what the edge must then serve.
struct Row {
    case: &'static str,
    path: &'static str,
    status: u16,
    /// The body the answer must carry exactly, when the function generated it.
    generated_body: Option<String>,
    /// A fragment the body must carry, for an answer an ORIGIN produced.
    origin_document: bool,
    /// The value the edge header must carry, or `None` when the answer carries none.
    header: Option<String>,
    /// The cache outcome the answer must report.
    x_cache: &'static str,
}

fn rows() -> Vec<Row> {
    let generated = |path: &str| Some(format!("{MARKER} {path}"));
    vec![
        Row {
            case: "a one-segment path",
            path: "/sim2real/first",
            status: 200,
            generated_body: generated("/sim2real/first"),
            origin_document: false,
            header: Some(MARKER.to_string()),
            x_cache: LAMBDA_GENERATED,
        },
        Row {
            case: "another path of the same behaviour",
            path: "/sim2real/second",
            status: 200,
            generated_body: generated("/sim2real/second"),
            origin_document: false,
            header: Some(MARKER.to_string()),
            x_cache: LAMBDA_GENERATED,
        },
        Row {
            case: "a deeply nested path",
            path: "/a/b/c/d/object.txt",
            status: 200,
            generated_body: generated("/a/b/c/d/object.txt"),
            origin_document: false,
            header: Some(MARKER.to_string()),
            x_cache: LAMBDA_GENERATED,
        },
        Row {
            case: "the root of the distribution",
            path: "/",
            status: 200,
            generated_body: generated("/"),
            origin_document: false,
            header: Some(MARKER.to_string()),
            x_cache: LAMBDA_GENERATED,
        },
        Row {
            case: "a path carrying a query string",
            path: "/sim2real/first?one=1&two=2",
            status: 200,
            generated_body: Some(format!("{MARKER} /sim2real/first?one=1&two=2")),
            origin_document: false,
            header: Some(MARKER.to_string()),
            x_cache: LAMBDA_GENERATED,
        },
        Row {
            case: "a response whose bytes the function stated as base64",
            path: "/base64/object",
            status: 200,
            generated_body: generated("/base64/object"),
            origin_document: false,
            header: Some(MARKER.to_string()),
            x_cache: LAMBDA_GENERATED,
        },
        Row {
            case: "a request the function returned, which the edge then fetches",
            path: "/rewrite/object",
            status: 200,
            generated_body: None,
            origin_document: true,
            header: None,
            x_cache: MISS,
        },
        Row {
            case: "a run that hands the edge no object at all",
            path: "/fail/object",
            status: 502,
            generated_body: None,
            origin_document: false,
            header: None,
            x_cache: LAMBDA_ERROR,
        },
        Row {
            case: "the viewer-response event, over the answer the origin produced",
            path: ORIGIN_PATH,
            status: 200,
            generated_body: None,
            origin_document: true,
            header: Some(format!("{MARKER} {ORIGIN_PATH}")),
            x_cache: MISS,
        },
    ]
}

/// Every row: what the associated function returned is what the edge continued with.
#[tokio::test]
async fn the_edge_continues_with_the_object_the_associated_function_returned() {
    let case = "the objects a Lambda@Edge function returns";
    let topology = Topology::new();
    let role_arn = topology
        .create_role(
            case,
            "edge-objects-role",
            &[LAMBDA_PRINCIPAL, EDGE_LAMBDA_PRINCIPAL],
        )
        .await;
    let (_function_arn, version_arn) = topology
        .published_function(case, "edge-objects", &role_arn)
        .await;
    let created = create_distribution(
        &topology.router,
        &config_xml(
            "edge-objects",
            "the objects a lambda edge function returns",
            &version_arn,
        ),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "setup {case:?}: CreateDistribution must be accepted, got {} with {}",
        created.status, created.raw
    );
    let id = created
        .root()
        .find_text("Id")
        .unwrap_or_default()
        .to_string();
    let domain = created
        .root()
        .find_text("DomainName")
        .unwrap_or_default()
        .to_string();
    awaited_deployment(&topology.router, case, &id).await;
    let address = topology.edge_address(case, &id);

    for row in rows() {
        let served = Served::of(row.case, &address, &domain, row.path).await;
        assert_eq!(
            served.status, row.status,
            "case {:?}: expected HTTP {}, got {} with {:?}",
            row.case, row.status, served.status, served.raw
        );
        if let Some(body) = &row.generated_body {
            assert_eq!(
                &served.body, body,
                "case {:?}: expected the bytes the function stated, got {:?}",
                row.case, served.body
            );
        }
        assert_eq!(
            served.body.contains(ORIGIN_DOCUMENT),
            row.origin_document,
            "case {:?}: expected the origin's own document carried={}, got {:?}",
            row.case,
            row.origin_document,
            served.body
        );
        let header = served.header(EDGE_HEADER);
        match &row.header {
            Some(expected) => assert_eq!(
                &header, expected,
                "case {:?}: expected the header the function stated, got {:?}",
                row.case, served.raw
            ),
            None => assert!(
                header.is_empty(),
                "case {:?}: expected no header of the function's, got {header:?}",
                row.case
            ),
        }
        assert!(
            served.reports_cache(row.x_cache),
            "case {:?}: expected the answer reported as {:?}, got {:?}",
            row.case,
            row.x_cache,
            served.raw
        );
    }
}
