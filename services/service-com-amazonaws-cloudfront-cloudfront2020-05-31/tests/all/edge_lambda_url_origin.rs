// SPDX-License-Identifier: CC0-1.0

//! A distribution over a LAMBDA FUNCTION URL origin, reached at the address the distribution
//! PUBLISHED, and the decisions CloudFront makes there ITSELF — end to end over HTTP across
//! the services on one engine.
//!
//! The topology is the one a function URL behind a distribution is: a running Lambda function
//! whose function URL is the origin's `DomainName`, a standard distribution in front of it,
//! and a viewer that dials the `host:port` the distribution published with no AWS credential
//! at all. The rules the rows span:
//!
//! * a distribution the control plane reports `Deployed` ANSWERS at its own `DomainName`:
//!   a viewer request there is served from the origin the matched behaviour targets, so the
//!   answer carries the origin's own bytes and `X-Cache: Miss from cloudfront`. The origin
//!   here is a function URL, and whatever that plane answers is what the edge serves. Both
//!   halves of that one address are served — the `http://` and the `https://` a viewer may
//!   dial, "viewers can use HTTP or HTTPS";
//! * `AllowedMethods` decides whether the viewer's METHOD is served at all: a method the
//!   matched behaviour does not list is refused BY CLOUDFRONT with `403` and
//!   `X-Cache: Error from cloudfront`, carries none of the origin's bytes, and makes NO
//!   origin request — which is read off Lambda's own `Invocations` series for the function,
//!   so "the origin was never invoked" is a fact of the owning service's records rather than
//!   an absence in the answer;
//! * `TrustedKeyGroups` decides whether the viewer is AUTHORIZED at all: "when a cache
//!   behavior contains trusted key groups, CloudFront requires signed URLs or signed cookies
//!   for all requests that match the cache behavior", so an unsigned request on such a
//!   behaviour is `403` `MissingKey` with no origin request, while a URL signed by a key in
//!   that group is served from the function — and a policy whose `IpAddress` condition pins
//!   the viewer's own source address is served only for the address the edge ITSELF observes,
//!   which is the one it reports to the origin as `CloudFront-Viewer-Address`;
//! * an `OriginAccessControl` of origin type `lambda` signing `always` makes the EDGE's own
//!   origin request a credentialled caller, so a function URL whose `AuthType` is `AWS_IAM` is
//!   served through the distribution while the same URL called directly with no credential is
//!   refused by Lambda — the boundary that control exists for. That leg is driven on a router
//!   that VERIFIES request signatures, which is how the emulator is started, so the signature
//!   the edge computes is one the emulator's own verifier accepts;
//! * the origin is named by the function URL's own ADDRESS and nothing else: an
//!   `Origin$DomainName` carries a host, and the viewer's path is appended to it, so a
//!   distribution whose origin is the AUTHORITY of a published function URL reaches that
//!   function — and the distribution's `Restrictions.GeoRestriction` then decides which
//!   viewers are served from it at all, a viewer a `blacklist` names (or one a `whitelist`
//!   leaves out) being refused by CloudFront itself with `403`,
//!   `X-Cache: Error from cloudfront` and none of the origin's bytes;
//! * the ORIGIN is asked for `Origin$OriginPath` followed by the viewer's path — "an optional
//!   path that CloudFront appends to the origin domain name when CloudFront requests
//!   content" — so the origin of a behaviour that carries one never sees the unprefixed path,
//!   while a second origin on the SAME backend carrying none sees the viewer's path bare; and
//!   the request the origin receives carries the headers the EDGE generates about the viewer's
//!   connection (the country it is in, the scheme it dialled, the address it came from), which
//!   is what an origin doing geo- or scheme-dependent work reads.
//!
//! The rows differ along what each rule generalises over: every method a client can send (the
//! two the behaviour lists, and five it does not), the scheme the viewer dialled, the
//! behaviour the viewer path matches (the open one and the one that trusts a key group), the
//! policy form (canned and custom), the key that signed it (one in the group and one that is
//! not), whether the policy's source-address condition holds for the address the edge
//! observed, whether the request reaching the function came through the edge or straight
//! from a client, and the `OriginPath` the matched behaviour's origin carries (none, one
//! segment, several) against the viewer path it is prefixed to (a file, a nested path, the
//! root, one carrying a query).
//!
//! What is staged through the SERVICES that own it: the execution role through IAM, the
//! function, its function URL and the resource policy that admits a caller through Lambda, the
//! public key and key group through CloudFront, and the invocation counts through CloudWatch.
//! The signer is [`crate::edge_signed_access`]'s and the dial of the published address
//! [`crate::published_edge_address`]'s, reused rather than copied.

use std::io::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_cloudfront_cloudfront2020_05_31::EmulatedService;
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use crate::distribution_lifecycle::{
    is_reachable_authority, API_VERSION, AUTHORIZATION, HOST, POLL_INTERVAL, READY_DEADLINE,
};
use crate::edge_request_metrics::{cloudwatch, now_secs, window, PERIOD};
use crate::edge_signed_access::{
    canned_policy, canned_query, create_key_group, custom_policy, custom_query, far_future,
    upload_public_key, Signer, CACHING_DISABLED,
};
use crate::published_edge_address::{dial, dial_with};

/// The Region every setup call below is made in, and the Region CloudFront's own calls are
/// signed for.
const REGION: &str = "us-east-1";

/// The AWS-managed origin request policy that forwards all viewer parameters and the
/// `CloudFront-*` headers the edge generates, `Managed-AllViewerAndCloudFrontHeaders-2022-06`:
/// its id is the one this service's own managed catalogue publishes. It is what lets the
/// function report the country and the address the edge observed for the viewer.
pub(crate) const ALL_VIEWER_AND_CLOUDFRONT: &str = "33f36d7e-f396-46d9-90e0-52428a34d9dc";

/// The AWS-managed origin request policy `Managed-AllViewerExceptHostHeader`, whose id is the
/// one that same catalogue publishes. It is the policy CloudFront documents for an OAC-signed
/// origin: forwarding the viewer's own `Host` would not be the host the signature was computed
/// over.
pub(crate) const ALL_VIEWER_EXCEPT_HOST: &str = "b689b0a8-53d0-40ab-baf2-68738e2966ac";

/// The marker the function puts in every answer of its own, so an answer that carries it came
/// from the origin and one that does not was produced by CloudFront itself.
pub(crate) const ORIGIN_MARKER: &str = "FUNCTION-URL-OK";

/// Lambda's own metric namespace and the series that counts the invocations admitted to a
/// function, as that service publishes them.
const LAMBDA_NAMESPACE: &str = "AWS/Lambda";
const INVOCATIONS: &str = "Invocations";

/// The header CloudFront stamps on every origin request it makes, which is what tells an
/// origin the request arrived through an edge rather than from a client.
const EDGE_REQUEST_ID_HEADER: &str = "x-amz-cf-id";

/// The path the open behaviour serves, and the path pattern (and a path under it) of the
/// behaviour that trusts a key group.
pub(crate) const OPEN_PATH: &str = "/whoami";
const PRIVATE_PATTERN: &str = "/private/*";
const PRIVATE_PATH: &str = "/private/object";

/// `X-Cache` for an answer the origin produced for this request, and for one CloudFront
/// produced itself.
pub(crate) const MISS: &str = "Miss from cloudfront";
const ERROR: &str = "Error from cloudfront";

/// The handler behind the function URL: it reports the request it was handed — the method, the
/// path, the `CloudFront-*` headers the edge generates (the viewer's country, the scheme it
/// dialled and the address it came from) and the names of every header that reached it — so
/// what the viewer is served says what reached the origin. It imports only the standard
/// library, so the report is the handler's own work.
const HANDLER_SOURCE: &str = r#"
import json


def lambda_handler(event, context):
    headers = event.get("headers") or {}
    request = (event.get("requestContext") or {}).get("http") or {}
    return {
        "statusCode": 200,
        "headers": {"content-type": "application/json"},
        "body": json.dumps(
            {
                "marker": "FUNCTION-URL-OK",
                "method": request.get("method", ""),
                "path": event.get("rawPath", ""),
                "country": headers.get("cloudfront-viewer-country", ""),
                "proto": headers.get("cloudfront-forwarded-proto", ""),
                "address": headers.get("cloudfront-viewer-address", ""),
                "header_names": sorted(headers.keys()),
            }
        ),
    }
"#;

/// A .zip deployment package holding [`HANDLER_SOURCE`] as `index.py`, written in memory the
/// way a caller uploads one.
fn package() -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = ::zip::ZipWriter::new(&mut buffer);
        writer
            .start_file("index.py", ::zip::write::SimpleFileOptions::default())
            .expect("the entry starts");
        writer
            .write_all(HANDLER_SOURCE.as_bytes())
            .expect("the entry's bytes are written");
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// The text of the first `<name>` element of an XML body, or `""`.
///
/// `pub(crate)` so a sibling suite that stages a resource through [`Topology::cloudfront`]
/// reads the minted identifier out of the answer the same way.
pub(crate) fn element(body: &str, name: &str) -> String {
    let open = format!("<{name}>");
    let Some(start) = body.find(&open).map(|at| at + open.len()) else {
        return String::new();
    };
    body[start..]
        .find(&format!("</{name}>"))
        .map(|end| body[start..start + end].to_string())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// The emulator and what each service stages in it
// ---------------------------------------------------------------------------

/// CloudFront, Lambda, IAM, the token service and CloudWatch over ONE shared engine, as the
/// assembled router mounts them: the function the edge fetches is run by that Lambda as a
/// session of the role IAM holds, and the invocations it admits are counted in the CloudWatch
/// store both services share.
pub(crate) struct Topology {
    router: MultiServiceRouter,
    callback: EmbeddedEmulator,
    engine: Arc<engine::InMemoryEngine>,
    /// Whether this emulator VERIFIES the signature of every request, as it does when it is
    /// started: a suite that drives the OAC-signed origin fetch needs the verifier on, because
    /// the fetch the edge makes is a signed in-process call like any other.
    verifying: bool,
}

impl Topology {
    /// An emulator that does not verify signatures, so a setup call carries a credential scope
    /// (which is what names the Region and routes it) without a computed signature.
    pub(crate) fn new() -> Self {
        Self::with_verification(false)
    }

    /// An emulator that VERIFIES every request's signature, where each call below is signed
    /// the way an in-process call of this emulator is.
    fn verifying() -> Self {
        Self::with_verification(true)
    }

    fn with_verification(verifying: bool) -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = Self::router_on(&engine);
        router.set_request_authentication(verifying);
        let callback = EmbeddedEmulator::from_router(router.clone());
        Self {
            router,
            callback,
            engine,
            verifying,
        }
    }

    /// The same services, mounted again over the SAME engine, with the verifier ON: the router
    /// a DIRECT caller of a function URL meets when the emulator is started.
    fn authenticating(&self) -> MultiServiceRouter {
        let mut router = Self::router_on(&self.engine);
        router.set_request_authentication(true);
        router
    }

    /// Every service this suite needs, mounted on `engine`.
    fn router_on(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
        let mut router = MultiServiceRouter::new();
        router.register_service(EmulatedService::new(Arc::clone(engine)));
        router.register_service(
            service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(
                engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
                Arc::clone(engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
                Arc::clone(engine),
            ),
        );
        router
    }

    /// The IAM client, over the in-process callback.
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

    /// One CloudFront control-plane call, as the status and the body it answered.
    ///
    /// On a verifying emulator the request is SIGNED the way an in-process call of this
    /// emulator is signed, through the one signing path the services share; otherwise it
    /// carries the credential scope alone, which is what routes it.
    pub(crate) async fn cloudfront(
        &self,
        case: &str,
        method: &str,
        uri: &str,
        body: Option<&str>,
    ) -> (u16, String) {
        let mut builder = ::http::Request::builder()
            .method(method)
            .uri(format!("https://{HOST}{uri}"))
            .header("host", HOST);
        if body.is_some() {
            builder = builder.header("content-type", "application/xml");
        }
        let mut request = builder
            .body(body.unwrap_or_default().as_bytes().to_vec())
            .expect("the CloudFront request is well formed");
        if self.verifying {
            ::service_common::in_process_sigv4::sign_callback_request(
                &mut request,
                "cloudfront",
                REGION,
                &self.callback,
            )
            .await
            .unwrap_or_else(|error| panic!("setup {case}: signing {method} {uri}: {error}"));
        } else {
            let _ = request.headers_mut().insert(
                ::http::header::AUTHORIZATION,
                ::http::HeaderValue::from_static(AUTHORIZATION),
            );
        }
        let (parts, payload) = request.into_parts();
        let response = self
            .router
            .dispatch(::http::Request::from_parts(
                parts,
                ::bytes::Bytes::from(payload),
            ))
            .await;
        let status = response.status().as_u16();
        let raw = String::from_utf8_lossy(&response.into_body()).to_string();
        (status, raw)
    }

    /// The HOST of a PUBLIC function URL over [`HANDLER_SOURCE`]: `AuthType=NONE` and the
    /// resource-policy grant that admits the anonymous caller an unsigned origin fetch is.
    pub(crate) async fn public_function_url_host(&self, case: &str, function: &str) -> String {
        let host = self
            .function_url_host(case, function, aws_sdk_lambda::types::FunctionUrlAuthType::None)
            .await;
        // `AuthType=NONE` leaves IAM out of it; the function's own resource policy is what
        // admits the anonymous caller the edge's unsigned origin fetch is.
        self.lambda()
            .add_permission()
            .function_name(function)
            .statement_id("public-url")
            .action("lambda:InvokeFunctionUrl")
            .principal("*")
            .function_url_auth_type(aws_sdk_lambda::types::FunctionUrlAuthType::None)
            .send()
            .await
            .unwrap_or_else(|error| panic!("setup {case}: granting the public invoke: {error}"));
        host
    }

    /// The HOST of a function URL over [`HANDLER_SOURCE`] whose `AuthType` is `auth_type`: the
    /// execution role, the function, the wait for the state the clock owes it, and the URL.
    async fn function_url_host(
        &self,
        case: &str,
        function: &str,
        auth_type: aws_sdk_lambda::types::FunctionUrlAuthType,
    ) -> String {
        let trust = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": { "Service": "lambda.amazonaws.com" },
                "Action": "sts:AssumeRole",
            }],
        })
        .to_string();
        let created_role = self
            .iam()
            .create_role()
            .role_name(format!("{function}-role"))
            .assume_role_policy_document(trust)
            .send()
            .await
            .unwrap_or_else(|error| panic!("setup {case}: creating the execution role: {error}"));
        let role = created_role
            .role()
            .map(|role| role.arn().to_string())
            .filter(|arn| !arn.is_empty())
            .unwrap_or_else(|| panic!("setup {case}: the created execution role has no ARN"));
        let code = aws_sdk_lambda::types::FunctionCode::builder()
            .zip_file(::aws_smithy_types::Blob::new(package()))
            .build();
        self.lambda()
            .create_function()
            .function_name(function)
            .role(role)
            .runtime(aws_sdk_lambda::types::Runtime::Python312)
            .handler("index.lambda_handler")
            .code(code)
            .send()
            .await
            .unwrap_or_else(|error| panic!("setup {case}: creating the function: {error}"));

        // An invocation needs the function Active, which is a span the service owes: the
        // CONDITION is what is waited for, never a span of the host's clock.
        let deadline = Instant::now() + READY_DEADLINE;
        let mut state;
        loop {
            let configuration = self
                .lambda()
                .get_function_configuration()
                .function_name(function)
                .send()
                .await
                .unwrap_or_else(|error| {
                    panic!("setup {case}: reading the function's configuration: {error}")
                });
            state = configuration
                .state()
                .map(|state| state.as_str().to_string())
                .unwrap_or_default();
            if state == "Active" {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "setup {case}: the function never reached Active, last reported {state:?}"
            );
            ::tokio::time::sleep(POLL_INTERVAL).await;
        }

        let url = self
            .lambda()
            .create_function_url_config()
            .function_name(function)
            .auth_type(auth_type)
            .send()
            .await
            .unwrap_or_else(|error| panic!("setup {case}: creating the function URL: {error}"));
        let endpoint = url.function_url().to_string();
        assert!(
            !endpoint.is_empty(),
            "setup {case}: the created function URL reports no address"
        );
        endpoint
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_end_matches('/')
            .to_string()
    }

    /// How many invocations Lambda has admitted to `function`, as its own `Invocations` series
    /// reports them over a window around now: the owning service's record of whether the
    /// origin was reached at all. `0` for a function nobody invoked, which publishes no
    /// datapoint.
    pub(crate) async fn invocations(&self, case: &str, function: &str) -> f64 {
        let (start, end) = window(now_secs());
        let answered = cloudwatch(
            &self.router,
            case,
            "GetMetricStatistics",
            &json!({
                "Namespace": LAMBDA_NAMESPACE,
                "MetricName": INVOCATIONS,
                "Dimensions": [{ "Name": "FunctionName", "Value": function }],
                "StartTime": start,
                "EndTime": end,
                "Period": PERIOD,
                "Statistics": ["Sum"],
            }),
        )
        .await;
        answered["Datapoints"]
            .as_array()
            .map(|points| {
                points
                    .iter()
                    .filter_map(|point| point["Sum"].as_f64())
                    .sum::<f64>()
            })
            .unwrap_or_default()
    }

    /// One `OriginAccessControl` over a Lambda function URL origin: `sigv4`, signing always, of
    /// origin type `lambda`. Answers the id it minted.
    async fn origin_access_control(&self, case: &str, name: &str) -> String {
        let (status, raw) = self
            .cloudfront(
                case,
                "POST",
                &format!("/{API_VERSION}/origin-access-control"),
                Some(&format!(
                    "<OriginAccessControlConfig \
                     xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
                     <Name>{name}</Name><Description>a function url origin</Description>\
                     <SigningProtocol>sigv4</SigningProtocol>\
                     <SigningBehavior>always</SigningBehavior>\
                     <OriginAccessControlOriginType>lambda</OriginAccessControlOriginType>\
                     </OriginAccessControlConfig>"
                )),
            )
            .await;
        assert_eq!(
            status, 201,
            "setup {case}: CreateOriginAccessControl must be accepted, got {status} with {raw}"
        );
        let id = element(&raw, "Id");
        assert!(
            !id.is_empty(),
            "setup {case}: the created origin access control reports no id: {raw}"
        );
        id
    }

    /// One distribution from `config`: its `(Id, ARN, DomainName)`, with the domain held to
    /// being an address a client on this machine can dial.
    pub(crate) async fn created_distribution(
        &self,
        case: &str,
        config: &str,
    ) -> (String, String, String) {
        let (status, raw) = self
            .cloudfront(case, "POST", &format!("/{API_VERSION}/distribution"), Some(config))
            .await;
        assert_eq!(
            status, 201,
            "setup {case}: CreateDistribution must be accepted, got {status} with {raw}"
        );
        let (id, arn, domain) = (
            element(&raw, "Id"),
            element(&raw, "ARN"),
            element(&raw, "DomainName"),
        );
        assert!(
            !id.is_empty() && !arn.is_empty(),
            "setup {case}: the created distribution reports no Id or ARN: {raw}"
        );
        assert!(
            is_reachable_authority(&domain),
            "setup {case}: expected an address a viewer can dial, got {domain:?}"
        );
        (id, arn, domain)
    }

    /// Look again until `GetDistribution` reports `id` `Deployed` — a configuration that has
    /// not reached the edge locations serves no viewer — or the deadline passes, which fails
    /// the case. The CONDITION is what is waited for, never a span of the host's clock.
    pub(crate) async fn awaited_deployment(&self, case: &str, id: &str) {
        let deadline = Instant::now() + READY_DEADLINE;
        loop {
            let (status, raw) = self
                .cloudfront(
                    case,
                    "GET",
                    &format!("/{API_VERSION}/distribution/{id}"),
                    None,
                )
                .await;
            assert_eq!(
                status, 200,
                "setup {case}: GetDistribution must answer 200, got {status} with {raw}"
            );
            if element(&raw, "Status") == "Deployed" {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "setup {case}: the distribution never reached Deployed, last answer {raw}"
            );
            ::tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// One deployed distribution over the function URL `origin` with the behaviours
    /// `behaviors` carries: the `DomainName` it published.
    ///
    /// `pub(crate)` so a sibling suite over the same topology stages its distribution the same
    /// way rather than writing a second configuration for it.
    pub(crate) async fn deployed_distribution(
        &self,
        case: &str,
        caller_reference: &str,
        origin: &str,
        behaviors: &str,
    ) -> String {
        let (id, _arn, domain) = self
            .created_distribution(
                case,
                &config_xml(
                    caller_reference,
                    origin,
                    "",
                    behaviors,
                    ALL_VIEWER_AND_CLOUDFRONT,
                ),
            )
            .await;
        self.awaited_deployment(case, &id).await;
        domain
    }
}

/// One `DistributionConfig` over the function URL `origin`: `origin_extra` goes inside the
/// origin (the `OriginAccessControlId` an OAC-fronted origin carries), `behaviors` beside the
/// default behaviour, and `origin_request_policy` on the default behaviour.
fn config_xml(
    caller_reference: &str,
    origin: &str,
    origin_extra: &str,
    behaviors: &str,
    origin_request_policy: &str,
) -> String {
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{caller_reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items><Origin>\
         <Id>function-url</Id><DomainName>{origin}</DomainName>{origin_extra}\
         <CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
         <OriginProtocolPolicy>https-only</OriginProtocolPolicy>\
         <OriginSslProtocols><Quantity>1</Quantity>\
         <Items><SslProtocol>TLSv1.2</SslProtocol></Items></OriginSslProtocols>\
         </CustomOriginConfig>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>function-url</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <AllowedMethods><Quantity>2</Quantity>\
         <Items><Method>GET</Method><Method>HEAD</Method></Items></AllowedMethods>\
         <CachePolicyId>{CACHING_DISABLED}</CachePolicyId>\
         <OriginRequestPolicyId>{origin_request_policy}</OriginRequestPolicyId>\
         </DefaultCacheBehavior>{behaviors}\
         <Comment>a function url behind a distribution</Comment>\
         <Enabled>true</Enabled>\
         </DistributionConfig>"
    )
}

/// The ordered behaviour that trusts `key_group`, which is what makes a signed URL the only
/// way through [`PRIVATE_PATTERN`].
fn trusted_behavior(key_group: &str) -> String {
    format!(
        "<CacheBehaviors><Quantity>1</Quantity><Items><CacheBehavior>\
         <PathPattern>{PRIVATE_PATTERN}</PathPattern>\
         <TargetOriginId>function-url</TargetOriginId>\
         <TrustedKeyGroups><Enabled>true</Enabled><Quantity>1</Quantity>\
         <Items><KeyGroup>{key_group}</KeyGroup></Items></TrustedKeyGroups>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <AllowedMethods><Quantity>2</Quantity>\
         <Items><Method>GET</Method><Method>HEAD</Method></Items></AllowedMethods>\
         <CachePolicyId>{CACHING_DISABLED}</CachePolicyId>\
         <OriginRequestPolicyId>{ALL_VIEWER_AND_CLOUDFRONT}</OriginRequestPolicyId>\
         </CacheBehavior></Items></CacheBehaviors>"
    )
}

// ---------------------------------------------------------------------------
// Reading one answer off the connection
// ---------------------------------------------------------------------------

/// What one exchange on the published address observed.
pub(crate) struct Answered {
    pub(crate) status: u16,
    /// The whole answer as it came off the connection, status line and headers included.
    pub(crate) raw: String,
    /// The body alone, which is where the origin's own bytes are.
    pub(crate) body: String,
}

impl Answered {
    /// The answer of one viewer request to `domain`, dialled in the clear with no credential.
    async fn of(case: &str, domain: &str, method: &str, path_and_query: &str) -> Self {
        let raw = dial(domain, method, path_and_query).await;
        Self::read(case, raw)
    }

    /// [`Answered::of`] for a viewer that STATES `headers` of its own beside the `Host` — a
    /// browser that carries a header the edge reads about the connection it opened.
    pub(crate) async fn of_stating(
        case: &str,
        domain: &str,
        method: &str,
        path_and_query: &str,
        headers: &[(&str, &str)],
    ) -> Self {
        let raw = dial_with(domain, method, path_and_query, domain, headers).await;
        Self::read(case, raw)
    }

    /// One answer as it came off the connection, split into the status, the whole exchange and
    /// the body — the one reading both constructors above share.
    fn read(case: &str, raw: String) -> Self {
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

    /// Whether the answer carries the origin's own bytes.
    pub(crate) fn carries_the_origins_bytes(&self) -> bool {
        self.body.contains(ORIGIN_MARKER)
    }

    /// Whether the answer was reported as one the edge served from `x_cache`.
    pub(crate) fn reports_cache(&self, x_cache: &str) -> bool {
        self.raw
            .to_ascii_lowercase()
            .contains(&format!("x-cache: {}", x_cache.to_ascii_lowercase()))
    }

    /// Whether CloudFront's own error body carries the code `code`.
    fn carries_code(&self, code: &str) -> bool {
        self.body.contains(&format!("<Code>{code}</Code>"))
    }

    /// The function's own report, as the document it answered with.
    pub(crate) fn report(&self, case: &str) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|error| {
            panic!(
                "case {case:?}: the origin's answer is no document ({error}): {:?}",
                self.body
            )
        })
    }
}

/// The header names the function reported on the request it served.
///
/// `pub(crate)` so a sibling suite reading the same report asks it the same question.
pub(crate) fn header_names(report: &Value) -> Vec<String> {
    report["header_names"]
        .as_array()
        .map(|names| {
            names
                .iter()
                .filter_map(|name| name.as_str().map(str::to_ascii_lowercase))
                .collect()
        })
        .unwrap_or_default()
}

/// The viewer's ADDRESS out of the value the edge reported for it: `CloudFront-Viewer-Address`
/// carries the address the request came from, with the viewer's port after it where that port
/// is known, so the address is read off the front of the value. An IPv6 literal carries its
/// own colons inside brackets.
pub(crate) fn viewer_ip(reported: &str) -> String {
    let trimmed = reported.trim();
    if let Some(end) = trimmed.find(']') {
        return trimmed[..=end].to_string();
    }
    if trimmed.parse::<::std::net::IpAddr>().is_ok() {
        return trimmed.to_string();
    }
    trimmed
        .rsplit_once(':')
        .map_or(trimmed.to_string(), |(address, _port)| address.to_string())
}

// ---------------------------------------------------------------------------
// The method gate at the published address
// ---------------------------------------------------------------------------

/// One method row: the method a viewer sends, and whether the behaviour's `AllowedMethods`
/// lists it.
struct MethodRow {
    case: &'static str,
    method: &'static str,
    allowed: bool,
}

/// Every method a client can send against a behaviour whose `AllowedMethods` is `GET`, `HEAD`:
/// the two it lists are served from the function URL origin, and each of the five it does not
/// is refused by CloudFront itself with no origin request made at all.
#[tokio::test]
async fn the_published_domain_serves_a_function_url_origin_and_gates_the_method() {
    let topology = Topology::new();
    let case = "a function URL origin behind a published edge domain";
    let function = "cf-edge-method-gate";
    let origin = topology.public_function_url_host(case, function).await;
    let domain = topology
        .deployed_distribution(case, "edge-lambda-url-methods", &origin, "")
        .await;

    // The rows differ along the method and along whether the behaviour lists it.
    let rows: &[MethodRow] = &[
        MethodRow {
            case: "a GET, which the behaviour lists",
            method: "GET",
            allowed: true,
        },
        MethodRow {
            case: "a HEAD, which it lists too",
            method: "HEAD",
            allowed: true,
        },
        MethodRow {
            case: "a POST, which it does not list",
            method: "POST",
            allowed: false,
        },
        MethodRow {
            case: "a PUT",
            method: "PUT",
            allowed: false,
        },
        MethodRow {
            case: "a PATCH",
            method: "PATCH",
            allowed: false,
        },
        MethodRow {
            case: "a DELETE",
            method: "DELETE",
            allowed: false,
        },
        MethodRow {
            case: "an OPTIONS",
            method: "OPTIONS",
            allowed: false,
        },
    ];
    for row in rows {
        let before = topology.invocations(row.case, function).await;
        let answered = Answered::of(row.case, &domain, row.method, OPEN_PATH).await;
        let after = topology.invocations(row.case, function).await;
        if !row.allowed {
            assert_eq!(
                answered.status, 403,
                "case {:?}: expected CloudFront's own 403 for a method the behaviour does not \
                 allow, got {} with {:?}",
                row.case, answered.status, answered.raw
            );
            assert!(
                !answered.carries_the_origins_bytes(),
                "case {:?}: expected none of the origin's bytes in a refusal CloudFront \
                 produced, got {:?}",
                row.case, answered.body
            );
            assert!(
                answered.reports_cache(ERROR),
                "case {:?}: expected the refusal reported as CloudFront's own answer \
                 ({ERROR:?}), got {:?}",
                row.case, answered.raw
            );
            assert_eq!(
                after, before,
                "case {:?}: a method the behaviour does not allow must make no origin request, \
                 but the function's invocations went from {before} to {after}",
                row.case
            );
            continue;
        }
        assert_eq!(
            answered.status, 200,
            "case {:?}: expected the function's own answer through the published address, got \
             {} with {:?}",
            row.case, answered.status, answered.raw
        );
        assert!(
            answered.reports_cache(MISS),
            "case {:?}: expected an answer the origin produced for this request ({MISS:?}), got \
             {:?}",
            row.case, answered.raw
        );
        assert!(
            after > before,
            "case {:?}: a method the behaviour allows must reach the origin, but the function's \
             invocations stayed at {before} (now {after})",
            row.case
        );
        // A `HEAD` carries no body, so only a `GET` can report what the origin saw.
        if row.method == "GET" {
            assert!(
                answered.carries_the_origins_bytes(),
                "case {:?}: expected the origin's own bytes, got {:?}",
                row.case, answered.body
            );
            let report = answered.report(row.case);
            assert_eq!(
                report["method"].as_str().unwrap_or_default(),
                row.method,
                "case {:?}: expected the origin to see the viewer's method, got {report}",
                row.case
            );
            assert_eq!(
                report["path"].as_str().unwrap_or_default(),
                OPEN_PATH,
                "case {:?}: expected the origin to see the viewer's path, got {report}",
                row.case
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The trusted key group at the same address
// ---------------------------------------------------------------------------

/// What one signed-access row expects of the edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The function answered it: a `200` carrying the origin's own bytes.
    Served,
    /// CloudFront refused it itself, under this error code, with no origin request made.
    Refused(&'static str),
}

/// The signed-access rule over the same topology: the open behaviour serves an unsigned
/// viewer, and the behaviour that trusts a key group serves only a URL signed by a key in that
/// group over a policy whose conditions hold — the source-address condition among them,
/// against the very address the edge reported to the origin.
#[tokio::test]
async fn a_trusted_key_group_decides_a_function_url_origin_at_the_published_domain() {
    let topology = Topology::new();
    let case = "a trusted key group over a function URL origin";
    let function = "cf-edge-signed-access";
    let origin = topology.public_function_url_host(case, function).await;
    let signer = Signer::generate();
    let stranger = Signer::generate();
    let key = upload_public_key(&topology.router, "edge-lambda-url-trusted", &signer).await;
    let other = upload_public_key(&topology.router, "edge-lambda-url-stranger", &stranger).await;
    let (group, _etag) =
        create_key_group(&topology.router, "edge-lambda-url", &[key.as_str()]).await;
    let domain = topology
        .deployed_distribution(
            case,
            "edge-lambda-url-signed",
            &origin,
            &trusted_behavior(&group),
        )
        .await;

    // The baseline the signed rows are read against: the open behaviour, unsigned, served by
    // the function — and the address the EDGE observed for this viewer, as it reported it to
    // the origin.
    let baseline_case = "the unsigned open behaviour";
    let baseline = Answered::of(baseline_case, &domain, "GET", OPEN_PATH).await;
    assert_eq!(
        baseline.status, 200,
        "case {baseline_case:?}: expected the open behaviour to serve the function's answer, \
         got {} with {:?}",
        baseline.status, baseline.raw
    );
    assert!(
        baseline.carries_the_origins_bytes(),
        "case {baseline_case:?}: expected the origin's own bytes, got {:?}",
        baseline.body
    );
    let report = baseline.report(baseline_case);
    let country = report["country"].as_str().unwrap_or_default().to_string();
    assert!(
        country.len() == 2 && country.chars().all(|letter| letter.is_ascii_uppercase()),
        "case {baseline_case:?}: expected the edge to tell the origin the viewer's country as a \
         two-letter uppercase code, got {country:?} ({report})"
    );
    let reported_address = report["address"].as_str().unwrap_or_default().to_string();
    let observed = viewer_ip(&reported_address);
    assert!(
        observed.parse::<::std::net::IpAddr>().is_ok(),
        "case {baseline_case:?}: expected the edge to tell the origin the viewer's address, got \
         {reported_address:?} ({report})"
    );

    // A CANNED policy is the one CloudFront rebuilds from the URL itself, so its `Resource`
    // is the exact URL the viewer asks for; a CUSTOM one may name the object by a wildcard.
    let exact = format!("http://{domain}{PRIVATE_PATH}");
    let resource = format!("http://{domain}{PRIVATE_PATTERN}");
    let signed = |statement: &str| signer.sign(statement);
    let canned = canned_policy(&exact, far_future());
    let plain = custom_policy(&resource, far_future(), None, None);
    let pinned = custom_policy(
        &resource,
        far_future(),
        None,
        Some(&format!("{observed}/32")),
    );
    let elsewhere = custom_policy(&resource, far_future(), None, Some("10.0.0.0/8"));

    // (case, the viewer path and query, what the edge must do)
    let rows: Vec<(String, String, Expected)> = vec![
        (
            "an unsigned request on the behaviour that trusts a key group".to_string(),
            PRIVATE_PATH.to_string(),
            Expected::Refused("MissingKey"),
        ),
        (
            "a canned policy signed by a key in the group".to_string(),
            format!(
                "{PRIVATE_PATH}?{}",
                canned_query(far_future(), &signed(&canned), &key)
            ),
            Expected::Served,
        ),
        (
            "a custom policy signed by a key in the group".to_string(),
            format!(
                "{PRIVATE_PATH}?{}",
                custom_query(&plain, &signed(&plain), &key)
            ),
            Expected::Served,
        ),
        (
            "a custom policy pinning the address the edge observed".to_string(),
            format!(
                "{PRIVATE_PATH}?{}",
                custom_query(&pinned, &signed(&pinned), &key)
            ),
            Expected::Served,
        ),
        (
            "a custom policy pinning a range that address is not in".to_string(),
            format!(
                "{PRIVATE_PATH}?{}",
                custom_query(&elsewhere, &signed(&elsewhere), &key)
            ),
            Expected::Refused("AccessDenied"),
        ),
        (
            "a policy signed by a key the group does not carry".to_string(),
            format!(
                "{PRIVATE_PATH}?{}",
                custom_query(&plain, &stranger.sign(&plain), &other)
            ),
            Expected::Refused("InvalidKey"),
        ),
    ];

    for (case, path_and_query, expected) in rows {
        let before = topology.invocations(&case, function).await;
        let answered = Answered::of(&case, &domain, "GET", &path_and_query).await;
        let after = topology.invocations(&case, function).await;
        match expected {
            Expected::Served => {
                assert_eq!(
                    answered.status, 200,
                    "case {case:?}: expected the function's own answer, got {} with {:?}",
                    answered.status, answered.raw
                );
                assert!(
                    answered.carries_the_origins_bytes(),
                    "case {case:?}: expected the origin's own bytes, got {:?}",
                    answered.body
                );
                assert!(
                    after > before,
                    "case {case:?}: an admitted request must reach the origin, but the \
                     function's invocations stayed at {before} (now {after})"
                );
            }
            Expected::Refused(code) => {
                assert_eq!(
                    answered.status, 403,
                    "case {case:?}: expected CloudFront's own 403, got {} with {:?}",
                    answered.status, answered.raw
                );
                assert!(
                    answered.carries_code(code),
                    "case {case:?}: expected the error code {code:?}, got {:?}",
                    answered.body
                );
                assert!(
                    !answered.carries_the_origins_bytes(),
                    "case {case:?}: expected none of the origin's bytes, got {:?}",
                    answered.body
                );
                assert_eq!(
                    after, before,
                    "case {case:?}: a request CloudFront refuses must make no origin request, \
                     but the function's invocations went from {before} to {after}"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The `https://` half of the one published address
// ---------------------------------------------------------------------------

/// How long a dial of the published address waits for its answer. Generous: nothing here
/// measures the wait, only what came back.
const TLS_DIAL_DEADLINE: Duration = Duration::from_secs(30);

/// One viewer that dials the published address with `https://`, as a client handed a
/// distribution's `DomainName` does: a stock TLS client that does not verify this emulator's
/// self-signed certificate. Answers the status and the body.
///
/// The request runs on a blocking thread, so the listener it addresses — a task of this same
/// runtime — is served while this call waits for the answer.
async fn dial_over_tls(case: &str, url: String) -> (u16, String) {
    ::tokio::task::spawn_blocking(move || {
        let client = ::reqwest::blocking::Client::builder()
            .danger_accept_invalid_certs(true)
            .timeout(TLS_DIAL_DEADLINE)
            .build()
            .expect("a TLS client");
        let answered = client
            .get(&url)
            .send()
            .unwrap_or_else(|error| panic!("the published address did not answer {url}: {error}"));
        let status = answered.status().as_u16();
        (status, answered.text().unwrap_or_default())
    })
    .await
    .unwrap_or_else(|error| panic!("case {case:?}: the dial did not finish: {error}"))
}

/// The address a distribution publishes serves the `https://` a viewer may dial as well as the
/// `http://`: the encryption of that one address is negotiated in band, so a stock TLS client
/// handed the published `DomainName` is served the function URL origin's own answer there.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_published_domain_serves_the_https_a_viewer_dials() {
    let topology = Topology::new();
    let case = "a viewer that dialled TLS at the published address";
    let function = "cf-edge-tls-viewer";
    let origin = topology.public_function_url_host(case, function).await;
    let domain = topology
        .deployed_distribution(case, "edge-lambda-url-tls", &origin, "")
        .await;

    let (status, body) = dial_over_tls(case, format!("https://{domain}{OPEN_PATH}")).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the published address to serve the origin's answer over TLS, \
         got {status} with {body:?}"
    );
    assert!(
        body.contains(ORIGIN_MARKER),
        "case {case:?}: expected the origin's own bytes, got {body:?}"
    );
    let report: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    assert_eq!(
        report["path"].as_str().unwrap_or_default(),
        OPEN_PATH,
        "case {case:?}: expected the origin to see the viewer's path, got {report}"
    );
}

// ---------------------------------------------------------------------------
// The same origin behind an ORIGIN ACCESS CONTROL
// ---------------------------------------------------------------------------

/// The function URL called DIRECTLY, with no credential at all, as a client that found the
/// address does: the status and the body.
async fn direct_call(router: &MultiServiceRouter, host: &str, path: &str) -> (u16, String) {
    let request = ::http::Request::builder()
        .method("GET")
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .body(::bytes::Bytes::new())
        .expect("the direct request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(&response.into_body()).to_string(),
    )
}

/// RULE, over the topology the issue was filed on: an `OriginAccessControl` of origin type
/// `lambda` signs the edge's own fetch of a function URL origin that is named by the
/// function URL's own ADDRESS — the authority a consumer of the published endpoint carries —
/// so a viewer of the distribution is served the function's answer. Driven on an emulator
/// SERVED at an endpoint, as a started one is, and one that VERIFIES every signature.
#[tokio::test]
async fn an_oac_signs_a_function_url_origin_named_by_its_published_authority() {
    let topology = Topology::verifying();
    // The emulator as a started one is: served at an endpoint, so every address it publishes
    // is one a client of that process reads.
    ::emulator_server::EmulatorService::serving_at(&topology.router, "https://127.0.0.1:9297");
    let case = "an OAC-signed function URL origin on a served emulator";
    let function = "cf-edge-oac-served";
    let announced = topology
        .function_url_host(
            case,
            function,
            aws_sdk_lambda::types::FunctionUrlAuthType::AwsIam,
        )
        .await;
    // What an `Origin$DomainName` carries: the authority of the published endpoint, with the
    // viewer's own path appended to it by the edge.
    let origin = announced.split('/').next().unwrap_or_default().to_string();
    let control = topology
        .origin_access_control(case, "edge-lambda-url-oac-served")
        .await;
    let (id, arn, domain) = topology
        .created_distribution(
            case,
            &config_xml(
                "edge-lambda-url-oac-served",
                &origin,
                &format!("<OriginAccessControlId>{control}</OriginAccessControlId>"),
                "",
                ALL_VIEWER_EXCEPT_HOST,
            ),
        )
        .await;
    // The grant that makes the signed fetch one Lambda admits FOR this distribution: the
    // CloudFront service principal, scoped to the distribution that signs.
    for action in ["lambda:InvokeFunctionUrl", "lambda:InvokeFunction"] {
        topology
            .lambda()
            .add_permission()
            .function_name(function)
            .statement_id(action.replace(':', "-"))
            .action(action)
            .principal("cloudfront.amazonaws.com")
            .source_arn(&arn)
            .send()
            .await
            .unwrap_or_else(|error| panic!("setup {case}: granting {action}: {error}"));
    }
    topology.awaited_deployment(case, &id).await;

    // The viewer's own query travels to the origin, which is what says the request the
    // function served is this one.
    let through_the_edge = Answered::of(case, &domain, "GET", &format!("{OPEN_PATH}?nonce=abc")).await;
    assert_eq!(
        through_the_edge.status, 200,
        "case {case:?}: expected the origin named by its published authority ({origin:?}, \
         announced {announced:?}) to be served through the edge, got {} with {:?}",
        through_the_edge.status, through_the_edge.raw
    );
    assert!(
        through_the_edge.carries_the_origins_bytes(),
        "case {case:?}: expected the origin's own bytes, got {:?}",
        through_the_edge.body
    );
    let report = through_the_edge.report(case);
    assert_eq!(
        report["path"].as_str(),
        Some(OPEN_PATH),
        "case {case:?}: expected the origin to be told the viewer's own path, got {report}"
    );
    let names = header_names(&report);
    assert!(
        names.iter().any(|name| name == EDGE_REQUEST_ID_HEADER),
        "case {case:?}: expected the origin to see {EDGE_REQUEST_ID_HEADER:?} on a request \
         that arrived through the edge, got {names:?}"
    );
}

/// An `OriginAccessControl` of origin type `lambda` over a function URL whose `AuthType` is
/// `AWS_IAM`: "all origin requests" are signed, so the edge's own fetch is a credentialled
/// caller Lambda admits and the viewer is served the function's answer — while the SAME
/// function URL called directly with no credential is refused by Lambda and leaks none of the
/// function's bytes. That difference is the whole point of the control: only CloudFront can
/// invoke the function.
///
/// Driven on an emulator that VERIFIES every signature, which is how it is started: the fetch
/// the edge makes is a signed in-process call, so only a verifier can say that the signature
/// it computes is one Lambda's endpoint admits.
#[tokio::test]
async fn an_origin_access_control_signs_a_function_url_origin_the_direct_caller_cannot_reach() {
    let topology = Topology::verifying();
    let case = "an OAC-signed function URL origin";
    let function = "cf-edge-oac-signed";
    let origin = topology
        .function_url_host(
            case,
            function,
            aws_sdk_lambda::types::FunctionUrlAuthType::AwsIam,
        )
        .await;
    let control = topology
        .origin_access_control(case, "edge-lambda-url-oac")
        .await;
    let (id, arn, domain) = topology
        .created_distribution(
            case,
            &config_xml(
                "edge-lambda-url-oac",
                &origin,
                &format!("<OriginAccessControlId>{control}</OriginAccessControlId>"),
                "",
                ALL_VIEWER_EXCEPT_HOST,
            ),
        )
        .await;
    // The grant that makes the signed fetch one Lambda admits FOR this distribution: the
    // CloudFront service principal, scoped to the distribution that signs.
    for action in ["lambda:InvokeFunctionUrl", "lambda:InvokeFunction"] {
        topology
            .lambda()
            .add_permission()
            .function_name(function)
            .statement_id(action.replace(':', "-"))
            .action(action)
            .principal("cloudfront.amazonaws.com")
            .source_arn(&arn)
            .send()
            .await
            .unwrap_or_else(|error| panic!("setup {case}: granting {action}: {error}"));
    }
    topology.awaited_deployment(case, &id).await;

    let through_the_edge = Answered::of(case, &domain, "GET", OPEN_PATH).await;
    assert_eq!(
        through_the_edge.status, 200,
        "case {case:?}: expected the signed origin fetch to be served, got {} with {:?}",
        through_the_edge.status, through_the_edge.raw
    );
    assert!(
        through_the_edge.carries_the_origins_bytes(),
        "case {case:?}: expected the origin's own bytes, got {:?}",
        through_the_edge.body
    );
    let names = header_names(&through_the_edge.report(case));
    assert!(
        names.iter().any(|name| name == EDGE_REQUEST_ID_HEADER),
        "case {case:?}: expected the origin to see {EDGE_REQUEST_ID_HEADER:?} on a request that \
         arrived through the edge, got {names:?}"
    );

    // The same URL, dialled directly with no credential, on a router that verifies request
    // signatures: an `AWS_IAM` function URL admits only an authenticated caller, so Lambda
    // refuses it and the function's bytes stay behind the control.
    let (status, body) = direct_call(&topology.authenticating(), &origin, OPEN_PATH).await;
    assert_eq!(
        status, 403,
        "case {case:?}: expected an unsigned direct call to an AWS_IAM function URL to be \
         refused, got {status} with {body:?}"
    );
    assert!(
        !body.contains(ORIGIN_MARKER),
        "case {case:?}: expected none of the function's bytes in the refusal, got {body:?}"
    );

    // And the distribution still serves it afterwards: the refusal of the direct caller is
    // about the caller, not about the function.
    let again = Answered::of(case, &domain, "GET", OPEN_PATH).await;
    assert_eq!(
        again.status, 200,
        "case {case:?}: expected the distribution to keep serving the signed origin, got {} \
         with {:?}",
        again.status, again.raw
    );
    assert!(
        again.carries_the_origins_bytes(),
        "case {case:?}: expected the origin's own bytes again, got {:?}",
        again.body
    );
}

// ---------------------------------------------------------------------------
// The geographic restriction over an origin addressed by the function URL's AUTHORITY
// ---------------------------------------------------------------------------

/// The header a viewer states its country in, as CloudFront spells it: every viewer of this
/// emulator dials a loopback address, which is in no country the databases `GeoRestriction`
/// names, so the country this request is in is stated on the request itself.
const VIEWER_COUNTRY_HEADER: &str = "cloudfront-viewer-country";

/// One geo row: the `Restrictions` the distribution carries, the country the viewer states
/// (`None` for a viewer that states none), and whether CloudFront refuses it.
struct GeoRow {
    case: &'static str,
    restriction_type: &'static str,
    locations: &'static [&'static str],
    stated_country: Option<&'static str>,
    refused: bool,
}

/// A `Restrictions` element of `restriction_type` over `locations`; `Quantity` is the number
/// of locations it carries, which is what the control plane checks it against.
fn restrictions(restriction_type: &str, locations: &[&str]) -> String {
    let items: String = locations
        .iter()
        .map(|location| format!("<Location>{location}</Location>"))
        .collect();
    let items = if locations.is_empty() {
        String::new()
    } else {
        format!("<Items>{items}</Items>")
    };
    format!(
        "<Restrictions><GeoRestriction>\
         <RestrictionType>{restriction_type}</RestrictionType>\
         <Quantity>{}</Quantity>{items}\
         </GeoRestriction></Restrictions>",
        locations.len()
    )
}

/// One VIEWER request to `domain`, carrying no credential and stating `country` the way
/// CloudFront states it — a client of a distribution, addressing the domain the distribution
/// published.
async fn viewer(
    router: &MultiServiceRouter,
    domain: &str,
    path: &str,
    country: Option<&str>,
) -> (u16, String, String) {
    let mut builder = ::http::Request::builder()
        .method("GET")
        .uri(format!("https://{domain}{path}"))
        .header("host", domain);
    if let Some(country) = country {
        builder = builder.header(VIEWER_COUNTRY_HEADER, country);
    }
    let request = builder
        .body(::bytes::Bytes::new())
        .expect("the viewer request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let x_cache = response
        .headers()
        .get("x-cache")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = String::from_utf8_lossy(&response.into_body()).to_string();
    (status, x_cache, body)
}

/// One `DistributionConfig` over the function URL `origin` carrying `restrictions`: the
/// shape the geo rows drive, with the AWS-managed `CachingDisabled` cache policy, so every
/// row's viewer request is an origin fetch rather than another row's cached answer.
fn geo_config_xml(
    caller_reference: &str,
    origin: &str,
    restrictions: &str,
    origin_request_policy: &str,
) -> String {
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{caller_reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items><Origin>\
         <Id>function-url</Id><DomainName>{origin}</DomainName>\
         <CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
         <OriginProtocolPolicy>https-only</OriginProtocolPolicy>\
         <OriginSslProtocols><Quantity>1</Quantity>\
         <Items><SslProtocol>TLSv1.2</SslProtocol></Items></OriginSslProtocols>\
         </CustomOriginConfig>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>function-url</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <AllowedMethods><Quantity>2</Quantity>\
         <Items><Method>GET</Method><Method>HEAD</Method></Items></AllowedMethods>\
         <CachePolicyId>{CACHING_DISABLED}</CachePolicyId>\
         <OriginRequestPolicyId>{origin_request_policy}</OriginRequestPolicyId>\
         </DefaultCacheBehavior>\
         <Comment>a geo-restricted function url behind a distribution</Comment>\
         {restrictions}\
         <Enabled>true</Enabled>\
         </DistributionConfig>"
    )
}

/// RULE: a function URL is published at an address that IDENTIFIES it, so a consumer handed
/// that endpoint reaches the function by its AUTHORITY alone while supplying a path of its
/// own — which is what an `Origin$DomainName` carries — and the distribution's
/// `Restrictions.GeoRestriction` then decides, at the edge, which viewers are served from
/// that origin at all.
///
/// Driven on an emulator SERVED at an endpoint, as a started one is, so the function URL is
/// announced the way a client of a running emulator reads it; the origin is that announced
/// URL's AUTHORITY and nothing else.
///
/// The rows differ along what the rule generalises over: the restriction type (`none`,
/// `blacklist`, `whitelist`), how many locations it names, whether the viewer's country is
/// one of them, the case the viewer states it in — a `Location` is a code rather than a
/// spelling — and a viewer that states none at all.
#[tokio::test]
async fn the_geo_restriction_decides_a_function_url_origin_reached_by_its_authority() {
    let topology = Topology::new();
    // The emulator as a started one is: served at an endpoint, so every address it publishes
    // is one a client of that process reads.
    ::emulator_server::EmulatorService::serving_at(&topology.router, "https://127.0.0.1:9297");
    let case = "a function URL origin addressed by its own authority";
    let function = "cf-edge-geo-restriction";
    let announced = topology.public_function_url_host(case, function).await;
    // What a consumer of the published endpoint carries: its AUTHORITY. An
    // `Origin$DomainName` is a host — it carries no path — and the viewer's own path is
    // appended to it, so this is the whole of the endpoint the origin names.
    let origin = announced
        .split('/')
        .next()
        .unwrap_or_default()
        .to_string();
    assert!(
        origin
            .rsplit_once(':')
            .is_some_and(|(host, port)| !host.is_empty() && port.parse::<u16>().is_ok()),
        "setup {case:?}: expected the function URL to be published at a host:port an origin \
         can name, got {announced:?}"
    );

    let rows: &[GeoRow] = &[
        GeoRow {
            case: "no restriction at all, which restricts nobody",
            restriction_type: "none",
            locations: &[],
            stated_country: Some("US"),
            refused: false,
        },
        GeoRow {
            case: "a blocklisted country",
            restriction_type: "blacklist",
            locations: &["US"],
            stated_country: Some("US"),
            refused: true,
        },
        GeoRow {
            case: "a blocklisted country stated in lower case",
            restriction_type: "blacklist",
            locations: &["US"],
            stated_country: Some("us"),
            refused: true,
        },
        GeoRow {
            case: "the last of several blocklisted countries",
            restriction_type: "blacklist",
            locations: &["RU", "CN", "DE"],
            stated_country: Some("DE"),
            refused: true,
        },
        GeoRow {
            case: "a country outside the blocklist",
            restriction_type: "blacklist",
            locations: &["RU", "CN"],
            stated_country: Some("DE"),
            refused: false,
        },
        GeoRow {
            case: "the same country moved to the allowlist",
            restriction_type: "whitelist",
            locations: &["US"],
            stated_country: Some("US"),
            refused: false,
        },
        GeoRow {
            case: "a country outside the allowlist",
            restriction_type: "whitelist",
            locations: &["US"],
            stated_country: Some("RU"),
            refused: true,
        },
        GeoRow {
            case: "a viewer that states no country, under an allowlist naming another",
            restriction_type: "whitelist",
            locations: &["RU"],
            stated_country: None,
            refused: true,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let (id, _arn, domain) = topology
            .created_distribution(
                case,
                &geo_config_xml(
                    &format!("edge-lambda-url-geo-{index}"),
                    &origin,
                    &restrictions(row.restriction_type, row.locations),
                    ALL_VIEWER_AND_CLOUDFRONT,
                ),
            )
            .await;
        topology.awaited_deployment(case, &id).await;
        let (status, x_cache, body) =
            viewer(&topology.router, &domain, OPEN_PATH, row.stated_country).await;
        if row.refused {
            assert_eq!(
                (status, x_cache.as_str()),
                (403, ERROR),
                "case {case:?}: expected CloudFront's own 403 with X-Cache {ERROR:?}, got \
                 {status} with {x_cache:?} and {body:?}"
            );
            assert!(
                !body.contains(ORIGIN_MARKER),
                "case {case:?}: a refused viewer must be served none of the origin's bytes, \
                 got {body:?}"
            );
            continue;
        }
        assert_eq!(
            (status, x_cache.as_str()),
            (200, MISS),
            "case {case:?}: expected the origin's answer through the edge with X-Cache \
             {MISS:?}, got {status} with {x_cache:?} and {body:?}"
        );
        assert!(
            body.contains(ORIGIN_MARKER),
            "case {case:?}: expected the function's own bytes through the edge, got {body:?}"
        );
        let report: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        let country = report["country"].as_str().unwrap_or_default();
        assert_eq!(
            country.len(),
            2,
            "case {case:?}: expected the origin to be told a two-letter country code, got \
             {country:?} in {body:?}"
        );
        assert_eq!(
            country,
            country.to_ascii_uppercase(),
            "case {case:?}: expected an uppercase country code at the origin, got {country:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// What the ORIGIN is asked for: `OriginPath` ahead of the viewer path, and the
// headers the EDGE generates for the request it makes
// ---------------------------------------------------------------------------

/// The other two `CloudFront-*` headers the edge generates for an origin request beside the
/// country: the scheme the viewer dialled and the address it came from. Lower case, as an
/// origin request carries them.
const FORWARDED_PROTO_HEADER: &str = "cloudfront-forwarded-proto";
const VIEWER_ADDRESS_HEADER: &str = "cloudfront-viewer-address";

/// The `OriginPath` the default behaviour's origin carries, which every path the other
/// behaviours do not match is asked for under.
const PREFIXED_ORIGIN_PATH: &str = "/content";

/// One origin of the distribution the table below drives: its id, the `OriginPath` it carries
/// (`None` for the origin that carries none at all) and the path pattern of the behaviour that
/// targets it (`None` for the DEFAULT behaviour, which every other viewer path reaches).
struct OriginRow {
    id: &'static str,
    origin_path: Option<&'static str>,
    pattern: Option<&'static str>,
}

/// What every behaviour of that distribution carries after its target: the methods a viewer may
/// send, the AWS-managed `CachingDisabled` cache policy — so each row's request is an origin
/// fetch rather than another row's cached answer — and the origin request policy that forwards
/// the headers the edge generates, without which no behaviour sees them.
fn behavior_tail() -> String {
    format!(
        "<ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <AllowedMethods><Quantity>2</Quantity>\
         <Items><Method>GET</Method><Method>HEAD</Method></Items></AllowedMethods>\
         <CachePolicyId>{CACHING_DISABLED}</CachePolicyId>\
         <OriginRequestPolicyId>{ALL_VIEWER_AND_CLOUDFRONT}</OriginRequestPolicyId>"
    )
}

/// One `DistributionConfig` whose origins ALL name the same backend `host` and differ only in
/// the `OriginPath` each carries, with one behaviour per origin: the first is the default
/// behaviour and each of the others is an ordered behaviour on its own path pattern. So the one
/// thing deciding what the origin is asked for is the `OriginPath` of the origin the matched
/// behaviour targets.
fn origin_path_config_xml(caller_reference: &str, host: &str, origins: &[OriginRow]) -> String {
    let items: String = origins
        .iter()
        .map(|row| {
            let origin_path = row
                .origin_path
                .map(|path| format!("<OriginPath>{path}</OriginPath>"))
                .unwrap_or_default();
            format!(
                "<Origin><Id>{}</Id><DomainName>{host}</DomainName>{origin_path}\
                 <CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
                 <OriginProtocolPolicy>https-only</OriginProtocolPolicy>\
                 <OriginSslProtocols><Quantity>1</Quantity>\
                 <Items><SslProtocol>TLSv1.2</SslProtocol></Items></OriginSslProtocols>\
                 </CustomOriginConfig></Origin>",
                row.id
            )
        })
        .collect();
    let tail = behavior_tail();
    let ordered: Vec<&OriginRow> = origins.iter().filter(|row| row.pattern.is_some()).collect();
    let behaviors: String = ordered
        .iter()
        .map(|row| {
            format!(
                "<CacheBehavior><PathPattern>{}</PathPattern>\
                 <TargetOriginId>{}</TargetOriginId>{tail}</CacheBehavior>",
                row.pattern.unwrap_or_default(),
                row.id
            )
        })
        .collect();
    let cache_behaviors = if ordered.is_empty() {
        String::new()
    } else {
        format!(
            "<CacheBehaviors><Quantity>{}</Quantity><Items>{behaviors}</Items></CacheBehaviors>",
            ordered.len()
        )
    };
    let default_target = origins.first().map(|row| row.id).unwrap_or_default();
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{caller_reference}</CallerReference>\
         <Origins><Quantity>{}</Quantity><Items>{items}</Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>{default_target}</TargetOriginId>{tail}\
         </DefaultCacheBehavior>{cache_behaviors}\
         <Comment>origin paths over one backend</Comment>\
         <Enabled>true</Enabled>\
         </DistributionConfig>",
        origins.len()
    )
}

/// One viewer request of the table: what it asks the edge for, what it states about itself, the
/// path the ORIGIN must then be asked for, and the country the origin must be told.
struct PathRow {
    case: &'static str,
    /// The path and query the viewer asks the edge for.
    viewer: &'static str,
    /// The country code the viewer states, where it states one.
    stated_country: Option<&'static str>,
    /// The path the origin must be asked for: the matched behaviour's origin's `OriginPath`
    /// followed by the viewer path.
    origin_sees: &'static str,
    /// The country the origin must be told, or `None` where the edge's OWN country is what it
    /// reports — one value for every such request, which the row holds it to.
    country: Option<&'static str>,
}

/// A request the edge serves from an origin is asked of that origin under the origin's
/// `OriginPath` followed by the viewer's path, and carries the headers the edge GENERATES about
/// the viewer's connection — the country it is in and the scheme it dialled.
///
/// One distribution, four origins on ONE function URL backend differing only in the
/// `OriginPath` each carries, and rows that span what the rule generalises over: no
/// `OriginPath` at all, one segment, several segments, and one spelled like the viewer's own
/// first segment; a viewer path that is a file, a nested path, the root, and one carrying a
/// query; and a viewer that states its country, one that states something that is no country,
/// and one that states nothing. Each row reads what the origin was asked for from the origin's
/// OWN report, and holds Lambda's `Invocations` series to having counted the fetch, so "the
/// origin was reached" is a fact of the owning service's records.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_origin_sees_its_origin_path_ahead_of_the_viewer_path_and_the_edges_own_headers() {
    let topology = Topology::new();
    let case = "origin paths over one function URL backend";
    let function = "cf-edge-origin-path";
    let backend = topology.public_function_url_host(case, function).await;
    let origins = [
        OriginRow {
            id: "prefixed",
            origin_path: Some(PREFIXED_ORIGIN_PATH),
            pattern: None,
        },
        OriginRow {
            id: "bare",
            origin_path: None,
            pattern: Some("/bare/*"),
        },
        OriginRow {
            id: "deep",
            origin_path: Some("/deep/nested"),
            pattern: Some("/under/*"),
        },
        OriginRow {
            id: "doubled",
            origin_path: Some("/echo"),
            pattern: Some("/echo/*"),
        },
    ];
    let (id, _arn, domain) = topology
        .created_distribution(
            case,
            &origin_path_config_xml("edge-origin-path", &backend, &origins),
        )
        .await;
    topology.awaited_deployment(case, &id).await;

    let rows: &[PathRow] = &[
        PathRow {
            case: "the default behaviour's prefixed origin",
            viewer: OPEN_PATH,
            stated_country: None,
            origin_sees: "/content/whoami",
            country: None,
        },
        PathRow {
            case: "a nested viewer path under the same origin",
            viewer: "/assets/img/logo.png",
            stated_country: None,
            origin_sees: "/content/assets/img/logo.png",
            country: None,
        },
        PathRow {
            case: "a viewer query, which is no part of the path",
            viewer: "/whoami?b=2&a=1",
            stated_country: None,
            origin_sees: "/content/whoami",
            country: None,
        },
        PathRow {
            case: "the root path",
            viewer: "/",
            stated_country: None,
            origin_sees: "/content/",
            country: None,
        },
        PathRow {
            case: "a second origin on the same backend carrying no OriginPath",
            viewer: "/bare/whoami",
            stated_country: None,
            origin_sees: "/bare/whoami",
            country: None,
        },
        PathRow {
            case: "an OriginPath several segments deep",
            viewer: "/under/whoami",
            stated_country: None,
            origin_sees: "/deep/nested/under/whoami",
            country: None,
        },
        PathRow {
            case: "an OriginPath spelled like the viewer's own first segment",
            viewer: "/echo/whoami",
            stated_country: None,
            origin_sees: "/echo/echo/whoami",
            country: None,
        },
        PathRow {
            case: "a viewer that states the country it is in",
            viewer: OPEN_PATH,
            stated_country: Some("de"),
            origin_sees: "/content/whoami",
            country: Some("DE"),
        },
        PathRow {
            case: "a viewer that states something that is no country",
            viewer: OPEN_PATH,
            stated_country: Some("Elbonia"),
            origin_sees: "/content/whoami",
            country: None,
        },
    ];

    // The country the edge reports for a viewer it is told nothing about: read off the first
    // such row and held to being the same for every other one.
    let mut edge_country = String::new();
    for row in rows {
        let stated: Vec<(&str, &str)> = row
            .stated_country
            .map(|country| vec![(VIEWER_COUNTRY_HEADER, country)])
            .unwrap_or_default();
        let before = topology.invocations(row.case, function).await;
        let answered = Answered::of_stating(row.case, &domain, "GET", row.viewer, &stated).await;
        let after = topology.invocations(row.case, function).await;
        assert_eq!(
            answered.status, 200,
            "case {:?}: expected the origin's answer through the edge, got {} with {:?}",
            row.case, answered.status, answered.raw
        );
        assert!(
            answered.reports_cache(MISS),
            "case {:?}: expected an answer the origin produced for this request ({MISS:?}), got \
             {:?}",
            row.case,
            answered.raw
        );
        assert!(
            answered.carries_the_origins_bytes(),
            "case {:?}: expected the origin's own bytes, got {:?}",
            row.case,
            answered.body
        );
        assert!(
            after > before,
            "case {:?}: the fetch must reach the origin, but the function's invocations stayed \
             at {before} (now {after})",
            row.case
        );
        let report = answered.report(row.case);
        let seen = report["path"].as_str().unwrap_or_default();
        assert_eq!(
            seen, row.origin_sees,
            "case {:?}: expected the origin to be asked for {:?}, got {seen:?} ({report})",
            row.case, row.origin_sees
        );
        let names = header_names(&report);
        for generated in [
            VIEWER_COUNTRY_HEADER,
            FORWARDED_PROTO_HEADER,
            VIEWER_ADDRESS_HEADER,
        ] {
            assert!(
                names.iter().any(|name| name == generated),
                "case {:?}: expected the edge to generate {generated:?} for the origin request, \
                 got the headers {names:?}",
                row.case
            );
        }
        let country = report["country"].as_str().unwrap_or_default().to_string();
        assert!(
            country.len() == 2 && country.chars().all(|letter| letter.is_ascii_uppercase()),
            "case {:?}: expected a two-letter uppercase country code at the origin, got \
             {country:?} ({report})",
            row.case
        );
        match row.country {
            Some(expected) => assert_eq!(
                country, expected,
                "case {:?}: expected the origin to be told the country {expected:?}, got \
                 {country:?}",
                row.case
            ),
            None => {
                if edge_country.is_empty() {
                    edge_country = country.clone();
                }
                assert_eq!(
                    country, edge_country,
                    "case {:?}: expected the edge's own country {edge_country:?} for a viewer it \
                     was told no code for, got {country:?}",
                    row.case
                );
            }
        }
        let proto = report["proto"].as_str().unwrap_or_default();
        assert_eq!(
            proto,
            PLAINTEXT_SCHEME,
            "case {:?}: expected the origin to be told the scheme the viewer dialled, got \
             {proto:?}",
            row.case
        );
        let reported_address = report["address"].as_str().unwrap_or_default().to_string();
        let observed = viewer_ip(&reported_address);
        assert!(
            observed.parse::<::std::net::IpAddr>().is_ok(),
            "case {:?}: expected the origin to be told the viewer's address, got \
             {reported_address:?} ({report})",
            row.case
        );
    }

    // The scheme the origin is told is the one the VIEWER dialled rather than a constant: the
    // same path over the `https://` half of that one published address is reported as `https`,
    // and is asked of the origin under the same `OriginPath`.
    let tls_case = "the same viewer path dialled over TLS";
    let (status, body) = dial_over_tls(tls_case, format!("https://{domain}{OPEN_PATH}")).await;
    assert_eq!(
        status, 200,
        "case {tls_case:?}: expected the origin's answer over TLS, got {status} with {body:?}"
    );
    let report: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    assert_eq!(
        report["path"].as_str().unwrap_or_default(),
        format!("{PREFIXED_ORIGIN_PATH}{OPEN_PATH}"),
        "case {tls_case:?}: expected the origin to be asked for the prefixed path, got {report}"
    );
    assert_eq!(
        report["proto"].as_str().unwrap_or_default(),
        SECURE_SCHEME,
        "case {tls_case:?}: expected the origin to be told the scheme the viewer dialled, got \
         {report}"
    );
}

/// The two schemes a viewer may dial the one published address over, as the edge reports them
/// to the origin.
const PLAINTEXT_SCHEME: &str = "http";
const SECURE_SCHEME: &str = "https";
