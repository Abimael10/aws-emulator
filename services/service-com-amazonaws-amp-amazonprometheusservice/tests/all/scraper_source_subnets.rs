// SPDX-License-Identifier: CC0-1.0

//! The subnets a scraper's SOURCE names, end to end over HTTP: `CreateScraper` resolves
//! every subnet identifier its `source` carries against the service that OWNS subnets
//! (Amazon EC2) and refuses a request naming one the account does not have with the
//! modelled `ValidationException` (400), carrying the sentence EC2 answered about that
//! subnet.
//!
//! A subnet is not a resource of this service: a client builds its network with
//! `ec2:CreateVpc`/`ec2:CreateSubnet`, or uses the default VPC Amazon EC2 holds in every
//! Region of the account, and then names those subnets in `EksConfiguration$subnetIds` or
//! `VpcConfiguration$subnetIds`. So this suite mounts aps BESIDE Amazon EC2 on one engine
//! — the way the assembled emulator mounts them — asks EC2 which subnets the account has,
//! and drives the create against both the subnets it holds and identifiers it holds none
//! of. That is why this file carries its own two-service harness instead of
//! [`crate::workspace_lifecycle`]'s single-service [`crate::workspace_lifecycle::Amp`]:
//! the rule under test is about what ANOTHER service answers.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_amp_amazonprometheusservice::state::VALIDATION_REASON;
use service_com_amazonaws_amp_amazonprometheusservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::scraper_creation::{dataset_destination_json, workspace_destination_json};
use crate::scraper_update::scrape_configuration_json;

/// The Region every call below is signed for: the Region whose network a subnet
/// identifier is resolved in.
const REGION: &str = "us-west-2";

/// A Region of the account OTHER than [`REGION`], whose subnets are therefore not ones a
/// scraper created in `REGION` may name.
const OTHER_REGION: &str = "eu-west-1";

/// One response, as this suite reads it.
#[derive(Debug)]
struct Reply {
    status: u16,
    headers: http::HeaderMap,
    body: Value,
}

impl Reply {
    /// The error code a modelled failure renders as the JSON envelope's `__type`.
    fn error_code(&self) -> String {
        self.body["__type"]
            .as_str()
            .unwrap_or_default()
            .rsplit('#')
            .next()
            .unwrap_or_default()
            .to_string()
    }

    /// The refusal's message, as the wire carries it.
    fn message(&self) -> String {
        self.body["message"].as_str().unwrap_or_default().to_string()
    }
}

/// A router carrying this service BESIDE Amazon EC2 over ONE engine, so each test starts
/// from an empty store and the subnets a request names are the ones the owner of subnets
/// really holds.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router
}

/// Dispatch one aps request, signed (unverified — authentication is off here) for
/// `region`.
async fn call(router: &MultiServiceRouter, method: &str, uri: &str, region: &str, body: &str) -> Reply {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("aps.{region}.amazonaws.com"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/aps/aws4_request, \
                 SignedHeaders=host, Signature=0123456789abcdef"
            ),
        )
        .body(bytes::Bytes::from(body.to_owned()))
        .expect("a well-formed test request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    Reply {
        status,
        headers,
        body,
    }
}

/// `POST /scrapers`, the binding the model gives `CreateScraper`.
async fn create_scraper(router: &MultiServiceRouter, body: &Value) -> Reply {
    call(router, "POST", "/scrapers", REGION, &body.to_string()).await
}

/// The `scraperId`s `ListScrapers` reports in [`REGION`] — what a refused create must
/// leave unchanged.
async fn listed_scrapers(router: &MultiServiceRouter) -> Vec<String> {
    let reply = call(router, "GET", "/scrapers", REGION, "").await;
    assert_eq!(reply.status, 200, "ListScrapers must answer 200, got {reply:?}");
    reply.body["scrapers"]
        .as_array()
        .map(|scrapers| {
            scrapers
                .iter()
                .map(|scraper| scraper["scraperId"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// One workspace of this service, created through `CreateWorkspace`: the ARN an
/// `ampConfiguration` destination names.
async fn create_workspace(router: &MultiServiceRouter) -> String {
    let reply = call(router, "POST", "/workspaces", REGION, "{}").await;
    assert_eq!(
        reply.status, 202,
        "the destination workspace must be creatable, got {reply:?}"
    );
    reply.body["arn"].as_str().unwrap_or_default().to_string()
}

/// An EC2 client for `region` over the in-process callback this router routes on: no
/// socket is opened, and the endpoint URL is only the address that transport routes by.
///
/// The very seam this service's own subnet resolution reaches EC2 through, so a suite that
/// reads the account's network — or builds one with `ec2:CreateVpc`/`ec2:CreateSubnet` —
/// sees exactly what a create will look for.
fn ec2(router: &MultiServiceRouter, region: &str) -> ::aws_sdk_ec2::Client {
    let callback = ::smithy_server_runtime::EmbeddedEmulator::from_router(router.clone());
    let config = ::aws_sdk_ec2::Config::builder()
        .behavior_version(::aws_sdk_ec2::config::BehaviorVersion::latest())
        .region(::aws_sdk_ec2::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://ec2.{region}.amazonaws.com"))
        .credentials_provider(callback.aws_credentials_provider())
        .http_client(callback.http_client())
        .retry_config(::aws_sdk_ec2::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_ec2::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_ec2::config::IdentityCache::no_cache())
        .stalled_stream_protection(::aws_sdk_ec2::config::StalledStreamProtectionConfig::disabled())
        .build();
    ::aws_sdk_ec2::Client::from_conf(config)
}

/// Every subnet the account has in `region`, as EC2 reports it — ASKED, never assumed.
async fn account_subnet_ids(router: &MultiServiceRouter, region: &str) -> Vec<String> {
    let described = ec2(router, region)
        .describe_subnets()
        .send()
        .await
        .expect("EC2 must report the account's subnets");
    let mut reported: Vec<String> = described
        .subnets()
        .iter()
        .filter_map(|subnet| Some(subnet.subnet_id()?.to_string()))
        .collect();
    reported.sort();
    reported
}

/// What EC2 itself answers about `subnet_ids`: the error code, the HTTP status and the
/// sentence it states — read from the owner of subnets rather than written out here, so
/// the assertions below compare what aps carried through against what EC2 really said.
async fn ec2_subnet_answer(router: &MultiServiceRouter, subnet_ids: &[String]) -> (String, u16, String) {
    use ::aws_smithy_types::error::metadata::ProvideErrorMetadata;

    let error = ec2(router, REGION)
        .describe_subnets()
        .set_subnet_ids(Some(subnet_ids.to_vec()))
        .send()
        .await
        .expect_err("EC2 must refuse an identifier it holds no subnet of");
    let status = error
        .raw_response()
        .map(|response| response.status().as_u16())
        .expect("EC2's answer must carry a status");
    let service_error = error.as_service_error();
    (
        service_error
            .and_then(ProvideErrorMetadata::code)
            .unwrap_or_default()
            .to_string(),
        status,
        service_error
            .and_then(ProvideErrorMetadata::message)
            .unwrap_or_default()
            .to_string(),
    )
}

/// A `CreateScraper` body: a base-64 `scrapeConfiguration`, the `source` given and the
/// `destination` given.
fn create_body(source: Value, destination: Value) -> Value {
    json!({
        "scrapeConfiguration": scrape_configuration_json("global:\n  scrape_interval: 30s\n"),
        "source": source,
        "destination": destination,
    })
}

/// An EKS source naming `subnets`.
fn eks_source(subnets: &[String]) -> Value {
    json!({
        "eksConfiguration": {
            "clusterArn": format!(
                "arn:aws:eks:{REGION}:{}:cluster/collected",
                engine::emulator_account_id()
            ),
            "subnetIds": subnets,
            "securityGroupIds": ["sg-0123456789abcdef0"],
        },
    })
}

/// A VPC source naming `subnets` — the `Source` union's other variant, which carries the
/// same `subnetIds` list.
fn vpc_source(subnets: &[String]) -> Value {
    json!({
        "vpcConfiguration": {
            "securityGroupIds": ["sg-0123456789abcdef0"],
            "subnetIds": subnets,
        },
    })
}

/// A subnet of a VPC of the client's OWN, minted through EC2 — the other way an account
/// comes to have a subnet besides the default VPC — in `region`.
async fn client_subnet(router: &MultiServiceRouter, region: &str, cidr: &str) -> String {
    let client = ec2(router, region);
    let vpc = client
        .create_vpc()
        .cidr_block("10.42.0.0/16")
        .send()
        .await
        .expect("CreateVpc must mint a VPC")
        .vpc
        .as_ref()
        .and_then(::aws_sdk_ec2::types::Vpc::vpc_id)
        .expect("CreateVpc must report the VPC it minted")
        .to_string();
    client
        .create_subnet()
        .vpc_id(&vpc)
        .cidr_block(cidr)
        .send()
        .await
        .expect("CreateSubnet must mint a subnet")
        .subnet
        .as_ref()
        .and_then(::aws_sdk_ec2::types::Subnet::subnet_id)
        .expect("CreateSubnet must report the subnet it minted")
        .to_string()
}

// ---------------------------------------------------------------------------
// Rule: `CreateScraper` refuses, with the modelled `ValidationException` (400), a request
// whose `source` names a subnet identifier the account does not have in the Region the
// request was signed for — whichever variant of the `Source` union named it and wherever
// in the list it sits — and the refusal carries the sentence the owner of subnets
// answered about that subnet, inside the envelope naming the relayed call. The request is
// judged on its own `source`, so the refusal does not depend on whether the
// `destination` resolves, and nothing is stored.
// Source: `CreateScraper$source` (`EksConfiguration$subnetIds` / `VpcConfiguration$subnetIds`),
// EC2's own `DescribeSubnets`, and `services/common/src/network.rs` (the owner of subnets
// is the one answer to what the account has).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_source_subnet_the_account_does_not_have_is_refused_and_stores_nothing() {
    let router = emulator();
    let held = account_subnet_ids(&router, REGION).await;
    assert!(
        held.len() >= 2,
        "the account's default VPC must hold a subnet per zone, got {held:?}"
    );
    let elsewhere = account_subnet_ids(&router, OTHER_REGION).await;
    assert!(
        !elsewhere.is_empty() && !elsewhere.iter().any(|id| held.contains(id)),
        "another Region's subnets are other subnets, got {elsewhere:?} beside {held:?}"
    );
    // A subnet the account HAD in this Region and deleted: the same identifier, no longer
    // held.
    let gone = client_subnet(&router, REGION, "10.42.1.0/24").await;
    ec2(&router, REGION)
        .delete_subnet()
        .subnet_id(&gone)
        .send()
        .await
        .expect("DeleteSubnet must remove the subnet it was given");

    let workspace_arn = create_workspace(&router).await;
    let resolving = workspace_destination_json(&workspace_arn);
    let absent_workspace = workspace_destination_json(&format!(
        "arn:aws:aps:{REGION}:{}:workspace/ws-00000000-0000-0000-0000-000000000000",
        engine::emulator_account_id()
    ));

    // Rows differ along what the rule generalises over: WHICH source variant names the
    // subnets, WHY an identifier resolves to nothing (an unheld long-form id, an unheld
    // short-form id, a value that is not a subnet id at all, a real subnet of another
    // Region, a subnet of this Region that was deleted), WHERE in the list it sits, how
    // many ids the list carries, and whether the destination workspace resolves.
    //
    // `unresolved` is the identifier EC2 is asked about first among those the account does
    // not have: the owner answers for the earliest id in the request it holds no subnet
    // of, and that is the sentence the caller reads.
    let cases: Vec<(&str, Value, Value, String)> = vec![
        (
            "an unheld long-form id, eks source",
            eks_source(&["subnet-0000000000deadbee".to_string()]),
            resolving.clone(),
            "subnet-0000000000deadbee".to_string(),
        ),
        (
            "an unheld short-form id, vpc source",
            vpc_source(&["subnet-0a1b2c3d".to_string()]),
            resolving.clone(),
            "subnet-0a1b2c3d".to_string(),
        ),
        (
            // The request boundary refuses a value that is not a subnet id at all
            // (`SubnetId` is modelled with a `@pattern`), so every row here is a
            // WELL-FORMED identifier the account has no subnet of: what the account HAS
            // is EC2's answer, not a shape this service reads off the value.
            "an unheld id of the longest hexadecimal form",
            eks_source(&["subnet-ffffffffffffffffe".to_string()]),
            resolving.clone(),
            "subnet-ffffffffffffffffe".to_string(),
        ),
        (
            "a subnet of another Region",
            vpc_source(&[elsewhere[0].clone()]),
            resolving.clone(),
            elsewhere[0].clone(),
        ),
        (
            "a subnet this Region held and lost",
            eks_source(std::slice::from_ref(&gone)),
            resolving.clone(),
            gone.clone(),
        ),
        (
            "one unheld id among subnets the account has",
            eks_source(&[
                held[0].clone(),
                "subnet-0000000000deadbee".to_string(),
                held[1].clone(),
            ]),
            resolving.clone(),
            "subnet-0000000000deadbee".to_string(),
        ),
        (
            "the same unheld id named twice",
            vpc_source(&[
                "subnet-0a1b2c3d".to_string(),
                "subnet-0a1b2c3d".to_string(),
            ]),
            resolving.clone(),
            "subnet-0a1b2c3d".to_string(),
        ),
        (
            "an unheld id and a destination workspace that does not resolve either",
            eks_source(&["subnet-0000000000deadbee".to_string()]),
            absent_workspace,
            "subnet-0000000000deadbee".to_string(),
        ),
        (
            "an unheld id and a CloudWatch destination",
            vpc_source(&["subnet-0000000000deadbee".to_string()]),
            dataset_destination_json(REGION, "default"),
            "subnet-0000000000deadbee".to_string(),
        ),
    ];

    for (case, source, destination, unresolved) in cases {
        let (code, status, sentence) =
            ec2_subnet_answer(&router, std::slice::from_ref(&unresolved)).await;
        assert!(
            code.starts_with("InvalidSubnetID."),
            "case {case:?}: the owner of subnets must report {unresolved:?} as no subnet \
             of the account, got {code:?}"
        );

        let reply = create_scraper(&router, &create_body(source, destination)).await;
        assert_eq!(
            reply.status, 400,
            "case {case:?}: a source naming {unresolved:?} must be refused 400, got {reply:?}"
        );
        assert_eq!(
            reply.error_code(),
            "ValidationException",
            "case {case:?}: expected the modelled ValidationException, got {reply:?}"
        );
        assert_eq!(
            reply.headers.get("x-amzn-errortype").and_then(|value| value.to_str().ok()),
            Some("ValidationException"),
            "case {case:?}: the wire error type must name the modelled shape, got {reply:?}"
        );
        assert_eq!(
            reply.body["reason"].as_str(),
            Some(VALIDATION_REASON),
            "case {case:?}: expected this service's validation reason, got {reply:?}"
        );
        assert!(
            reply.body["fieldList"].is_null(),
            "case {case:?}: the request is refused on what it MEANS, not on a member \
             constraint, so no fieldList is reported, got {reply:?}"
        );

        // The message is what EC2 answered, carried through verbatim, then the envelope
        // naming the relayed call: the service it was made to, the status that service
        // answered with, that call's own request id and the one attempt it took.
        let head = format!("{sentence} (Service: Ec2, Status Code: {status}, Request ID: ");
        let tail = ") (SDK Attempt Count: 1)";
        let message = reply.message();
        assert!(
            message.starts_with(&head),
            "case {case:?}: expected the message to start {head:?}, got {message:?}"
        );
        assert!(
            message.ends_with(tail),
            "case {case:?}: expected the message to end {tail:?}, got {message:?}"
        );

        assert_eq!(
            listed_scrapers(&router).await,
            Vec::<String>::new(),
            "case {case:?}: a refused create must store nothing"
        );
    }
}

/// The request id in the envelope belongs to the relayed CALL, so two refusals of the
/// same request never report one id — it is minted per call rather than derived from
/// anything the request carried.
#[tokio::test]
async fn each_refusal_reports_its_own_relayed_request_id() {
    let router = emulator();
    let workspace_arn = create_workspace(&router).await;
    let body = create_body(
        eks_source(&["subnet-0000000000deadbee".to_string()]),
        workspace_destination_json(&workspace_arn),
    );

    let first = create_scraper(&router, &body).await.message();
    let second = create_scraper(&router, &body).await.message();
    assert_ne!(
        first, second,
        "case \"two refusals of one request\": each relayed call reports its own request \
         id, got {first:?} twice"
    );
}

/// A `source` naming only subnets the account HAS is accepted (the modelled 202) and the
/// scraper reads back through `DescribeScraper` reporting that same source — whichever
/// variant named them, however many it named and in whichever order, and whether they are
/// the default VPC's subnets or ones the client minted itself.
#[tokio::test]
async fn a_source_naming_subnets_the_account_has_is_accepted_and_reads_back() {
    let router = emulator();
    let held = account_subnet_ids(&router, REGION).await;
    assert!(
        held.len() >= 2,
        "the account's default VPC must hold a subnet per zone, got {held:?}"
    );
    let own = client_subnet(&router, REGION, "10.42.2.0/24").await;
    let workspace_arn = create_workspace(&router).await;

    let cases: Vec<(&str, Value, Vec<String>)> = vec![
        (
            "one default subnet, eks source",
            eks_source(&held[..1]),
            held[..1].to_vec(),
        ),
        (
            "two default subnets, vpc source",
            vpc_source(&held[..2]),
            held[..2].to_vec(),
        ),
        (
            "the same subnets in reverse order",
            eks_source(&held.iter().rev().cloned().collect::<Vec<_>>()),
            held.iter().rev().cloned().collect(),
        ),
        (
            "one subnet named twice",
            vpc_source(&[held[1].clone(), held[1].clone()]),
            vec![held[1].clone(), held[1].clone()],
        ),
        (
            "a subnet of a VPC the client minted",
            eks_source(std::slice::from_ref(&own)),
            vec![own.clone()],
        ),
    ];

    for (case, source, subnets) in cases {
        let reply = create_scraper(
            &router,
            &create_body(source, workspace_destination_json(&workspace_arn)),
        )
        .await;
        assert_eq!(
            reply.status, 202,
            "case {case:?}: {subnets:?} are subnets the account has, got {reply:?}"
        );
        let scraper_id = reply.body["scraperId"].as_str().unwrap_or_default().to_string();

        let read = call(
            &router,
            "GET",
            &format!("/scrapers/{scraper_id}"),
            REGION,
            "",
        )
        .await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: the created scraper must read back, got {read:?}"
        );
        let source = &read.body["scraper"]["source"];
        let reported: Vec<String> = source["eksConfiguration"]["subnetIds"]
            .as_array()
            .or_else(|| source["vpcConfiguration"]["subnetIds"].as_array())
            .unwrap_or_else(|| panic!("case {case:?}: the scraper must report its source, got {read:?}"))
            .iter()
            .map(|id| id.as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            reported, subnets,
            "case {case:?}: the stored source must carry the subnets the create named, \
             got {reported:?}"
        );
    }
}

/// The subnet check is the request's own, so a destination that does not resolve is still
/// the modelled `ResourceNotFoundException` (404) when every subnet the source names IS
/// the account's: the two refusals are independent, and neither hides the other.
#[tokio::test]
async fn a_destination_that_does_not_resolve_is_still_a_not_found() {
    let router = emulator();
    let held = account_subnet_ids(&router, REGION).await;
    let absent_workspace = workspace_destination_json(&format!(
        "arn:aws:aps:{REGION}:{}:workspace/ws-00000000-0000-0000-0000-000000000000",
        engine::emulator_account_id()
    ));

    let reply = create_scraper(
        &router,
        &create_body(eks_source(&held[..1]), absent_workspace),
    )
    .await;
    assert_eq!(
        reply.status, 404,
        "a destination naming no workspace must be refused 404, got {reply:?}"
    );
    assert_eq!(
        reply.error_code(),
        "ResourceNotFoundException",
        "expected the modelled ResourceNotFoundException, got {reply:?}"
    );
    assert_eq!(
        listed_scrapers(&router).await,
        Vec::<String>::new(),
        "a refused create must store nothing"
    );
}
