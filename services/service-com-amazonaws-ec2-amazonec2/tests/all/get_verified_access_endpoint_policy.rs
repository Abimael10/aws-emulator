// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `GetVerifiedAccessEndpointPolicy`, driven over the wire the service
//! actually speaks — `ec2Query`, through the crate's shared [`super::ec2_query`] harness.
//!
//! The rules under test are:
//!
//! 1. THE REPORT IS THE POLICY THE STORE HOLDS FOR THE ENDPOINT THE REQUEST NAMES: the
//!    document `CreateVerifiedAccessEndpoint` accepted comes back byte for byte, and
//!    `PolicyEnabled` says the endpoint enforces one exactly when it holds one to enforce;
//! 2. an endpoint created with NO policy — no member at all, or one carrying no text —
//!    reports no document and `PolicyEnabled` false, rather than an empty document or a
//!    policy it does not have;
//! 3. the policy is the ENDPOINT's own: two endpoints of one store each report their own
//!    document, and the policy of the GROUP an endpoint hangs off is not the endpoint's;
//! 4. an endpoint id the account holds no endpoint for is refused HTTP 400
//!    `InvalidVerifiedAccessEndpointId.NotFound` naming that id, and a `DryRun` reports the
//!    outcome the request would have had instead of any policy;
//! 5. the report is a READ: asking twice answers the same thing, and the endpoint a caller
//!    describes afterwards is the one that was there before.
//!
//! Each row of the document table is a distinct input along what rule 1 generalises over —
//! how long the document is, which characters it carries (the quotes, newlines, `&` and `=`
//! of a real Cedar policy, and text outside ASCII), and whether it is a policy at all — so
//! no row is the specification. `PolicyDocument` is an unconstrained `String` in the model
//! (`smithy_query(query="operation",operation="GetVerifiedAccessEndpointPolicy")`), so every
//! row is a document the API accepts.
//!
//! The endpoints are created through `CreateVerifiedAccessEndpoint` itself and the resources
//! they reference are seeded through the same insert seams the creating operations land on,
//! the way the rest of the Verified Access suites do.

use ec2::ops::types::{VerifiedAccessGroup, VerifiedAccessInstance};
use ec2::state::{
    availability_zones_in_region, ComAmazonawsEc2Amazonec2State as Ec2State, SecurityGroupSpec,
    SubnetSpec, VerifiedAccessGroupRecord, VERIFIED_ACCESS_ENDPOINT_TYPE_LOAD_BALANCER,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, error_code, error_message, member};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// A well-formed Verified Access endpoint id this store holds no endpoint for.
const UNKNOWN_ENDPOINT_ID: &str = "vae-0123456789abcdef0";

/// The policy the seeded GROUP carries: the document that must NOT be reported as any
/// endpoint's own policy.
const GROUP_POLICY: &str =
    "permit(principal, action, resource) when { context.identity.groups.contains(\"finance\") };";

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// One request's form parameters, as the harness takes them.
type Params = Vec<(String, String)>;

/// What the seeded store holds, for the tables to name rows by.
struct Fixture {
    /// The group every endpoint under test hangs off, which carries [`GROUP_POLICY`].
    group_id: String,
    /// The security group the endpoints are associated with.
    security_group_id: String,
    /// The subnet the endpoints' options block names.
    subnet_id: String,
}

/// Stand up an emulator and seed the resources a create references.
fn emulator() -> (MultiServiceRouter, Fixture) {
    let (router, engine) = super::ec2_query::emulator();
    let fixture = seed(&engine);
    (router, fixture)
}

/// Write the referenced resources into the engine's store and report the ids they were given.
fn seed(engine: &engine::InMemoryEngine) -> Fixture {
    let store = engine.state::<Ec2State>();
    let mut guard = store.lock();
    let zones = availability_zones_in_region(REGION);

    let vpc_id = guard.insert_vpc("10.0.0.0/16", false, "default", "available", REGION, Vec::new());
    let subnet_id = guard.insert_subnet(SubnetSpec {
        vpc_id: vpc_id.clone(),
        cidr_block: "10.0.0.0/24".to_string(),
        availability_zone: zones[0].zone_name.clone(),
        availability_zone_id: zones[0].zone_id.clone(),
        region: REGION.to_string(),
        state: "available".to_string(),
        default_for_az: false,
        map_public_ip_on_launch: false,
        tags: Vec::new(),
    });
    let security_group_id = guard.insert_security_group(SecurityGroupSpec {
        vpc_id: vpc_id.clone(),
        group_name: "verified-access".to_string(),
        description: "reaches the application".to_string(),
        region: REGION.to_string(),
        tags: Vec::new(),
        ip_permissions: Vec::new(),
        ip_permissions_egress: Vec::new(),
    });
    let instance_id = guard.insert_verified_access_instance(
        VerifiedAccessInstance {
            description: Some("the instance under test".to_string()),
            ..VerifiedAccessInstance::default()
        },
        None,
    );
    // The group carries a policy of its OWN, so rule 3 has something to tell the endpoint's
    // policy apart from.
    let group_id = guard.insert_verified_access_group(
        VerifiedAccessGroupRecord {
            view: VerifiedAccessGroup {
                verified_access_instance_id: Some(instance_id),
                description: Some("the group under test".to_string()),
                ..VerifiedAccessGroup::default()
            },
            policy_document: Some(GROUP_POLICY.to_string()),
        },
        REGION,
        None,
    );

    Fixture {
        group_id,
        security_group_id,
        subnet_id,
    }
}

/// One ec2Query call whose names and values are percent-encoded WHOLE
/// ([`super::ec2_query::form_encode`]), so a policy document reaches the handler exactly as
/// written — a real Cedar policy carries newlines, quotes and `&`, which a looser encoder
/// would hand the form parser as structure.
async fn post(router: &MultiServiceRouter, action: &str, params: &[(String, String)]) -> (u16, String) {
    let mut body = format!(
        "Action={action}&Version={}",
        super::ec2_query::API_VERSION
    );
    for (name, value) in params {
        body.push('&');
        body.push_str(&super::ec2_query::form_encode(name));
        body.push('=');
        body.push_str(&super::ec2_query::form_encode(value));
    }
    let (status, _content_type, body) =
        super::ec2_query::call_raw(router, REGION, &[], &body).await;
    (status, body)
}

/// The parameters of a create of the one endpoint kind this suite needs — a `load-balancer`
/// endpoint, whose policy is the same endpoint-level policy every kind carries — with
/// `policy_document` as its `PolicyDocument` when the row names one.
fn create_params(fixture: &Fixture, policy_document: Option<&str>) -> Params {
    let mut params = vec![
        p("VerifiedAccessGroupId", fixture.group_id.clone()),
        p("EndpointType", VERIFIED_ACCESS_ENDPOINT_TYPE_LOAD_BALANCER),
        p("AttachmentType", "vpc"),
        p("SecurityGroupId.1", fixture.security_group_id.clone()),
        p("LoadBalancerOptions.Protocol", "https"),
        p("LoadBalancerOptions.Port", "443"),
        p(
            "LoadBalancerOptions.LoadBalancerArn",
            "arn:aws:elasticloadbalancing:us-west-2:000000000000:loadbalancer/app/web/0a1b",
        ),
        p("LoadBalancerOptions.SubnetId.1", fixture.subnet_id.clone()),
    ];
    if let Some(document) = policy_document {
        params.push(p("PolicyDocument", document));
    }
    params
}

/// Create one endpoint that must succeed and answer the id it was minted.
async fn create_endpoint(
    router: &MultiServiceRouter,
    case: &str,
    fixture: &Fixture,
    policy_document: Option<&str>,
) -> String {
    let (status, body) = post(
        router,
        "CreateVerifiedAccessEndpoint",
        &create_params(fixture, policy_document),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateVerifiedAccessEndpoint, got {status} \
         with body {body}"
    );
    let endpoint_id = element(&body, "verifiedAccessEndpointId").unwrap_or_default();
    assert!(
        endpoint_id.starts_with("vae-"),
        "case {case:?}: expected a minted endpoint id, got {endpoint_id:?} from {body}"
    );
    endpoint_id
}

/// One `GetVerifiedAccessEndpointPolicy` call.
async fn get_policy(router: &MultiServiceRouter, params: &[(String, String)]) -> (u16, String) {
    post(router, "GetVerifiedAccessEndpointPolicy", params).await
}

/// What the call reports for `endpoint_id`, as `(the document, the status)` — each `None`
/// when the response carries no such member at all.
async fn reported_policy(
    router: &MultiServiceRouter,
    case: &str,
    endpoint_id: &str,
) -> (Option<String>, Option<String>) {
    let (status, body) = get_policy(router, &[p("VerifiedAccessEndpointId", endpoint_id)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetVerifiedAccessEndpointPolicy, got {status} \
         with body {body}"
    );
    (
        member(&body, "policyDocument"),
        member(&body, "policyEnabled"),
    )
}

/// The documents rule 1 is asserted over: `(case, the document)`. The rows differ in length
/// and in which characters the text carries, which is what the rule generalises over — a
/// document is an opaque, unconstrained `String` to this API.
fn policy_documents() -> Vec<(&'static str, String)> {
    vec![
        (
            "a one-line permit",
            "permit(principal, action, resource);".to_string(),
        ),
        (
            "a multi-line policy with quotes",
            "permit(principal, action, resource)\nwhen {\n  context.identity.email \
             == \"analyst@example.test\"\n};"
                .to_string(),
        ),
        (
            "a policy whose text carries form separators",
            "permit(principal, action, resource) when { context.http_request.path == \
             \"/a?b=1&c=2\" };"
                .to_string(),
        ),
        (
            "a policy whose text is outside ASCII",
            "permit(principal, action, resource) when { context.identity.name == \
             \"Ünïcödé — 名前\" };"
                .to_string(),
        ),
        (
            "a long document",
            format!(
                "permit(principal, action, resource) when {{ context.identity.groups.contains(\
                 \"{}\") }};",
                "g".repeat(4_000)
            ),
        ),
        (
            "a document that is only indentation around a policy",
            "   permit(principal, action, resource);\n   ".to_string(),
        ),
    ]
}

/// RULE 1: the document the create accepted is the document this reader reports, byte for
/// byte, and an endpoint that holds one reports its policy as enabled.
#[tokio::test]
async fn the_created_document_round_trips_with_the_policy_enabled() {
    let (router, fixture) = emulator();
    for (case, document) in policy_documents() {
        let endpoint_id = create_endpoint(&router, case, &fixture, Some(&document)).await;
        let (reported, enabled) = reported_policy(&router, case, &endpoint_id).await;
        assert_eq!(
            reported.as_deref(),
            Some(document.as_str()),
            "case {case:?}: expected the stored document back, got {reported:?}"
        );
        assert_eq!(
            enabled.as_deref(),
            Some("true"),
            "case {case:?}: expected policyEnabled true for an endpoint holding a policy, \
             got {enabled:?}"
        );
    }
}

/// RULE 2: an endpoint created with no policy at all — or with a document carrying no text —
/// holds nothing to enforce, so it reports no document and `PolicyEnabled` false.
#[tokio::test]
async fn an_endpoint_with_no_policy_reports_none() {
    let (router, fixture) = emulator();
    let rows: Vec<(&str, Option<&str>)> = vec![
        ("no PolicyDocument member at all", None),
        ("an empty document", Some("")),
        ("a document of only spaces", Some("   ")),
        ("a document of only whitespace", Some(" \t\r\n ")),
    ];
    for (case, document) in rows {
        let endpoint_id = create_endpoint(&router, case, &fixture, document).await;
        let (reported, enabled) = reported_policy(&router, case, &endpoint_id).await;
        assert_eq!(
            reported, None,
            "case {case:?}: expected no policyDocument member, got {reported:?}"
        );
        assert_eq!(
            enabled.as_deref(),
            Some("false"),
            "case {case:?}: expected policyEnabled false for an endpoint holding no policy, \
             got {enabled:?}"
        );
    }
}

/// RULE 3: the policy reported is the ENDPOINT's own — each endpoint of one store answers
/// its own document, and the policy of the group an endpoint hangs off is not reported as
/// the endpoint's.
#[tokio::test]
async fn each_endpoint_reports_its_own_policy() {
    let (router, fixture) = emulator();
    let case = "each endpoint reports its own policy";
    let documents = policy_documents();
    let mut created: Vec<(&str, String, String)> = Vec::new();
    for (row, document) in &documents {
        let endpoint_id = create_endpoint(&router, row, &fixture, Some(document)).await;
        created.push((row, document.clone(), endpoint_id));
    }
    // One endpoint on the SAME group that was created with no policy of its own: the group's
    // document must not stand in for it.
    let policyless = create_endpoint(&router, case, &fixture, None).await;

    for (row, document, endpoint_id) in &created {
        let (reported, enabled) = reported_policy(&router, row, endpoint_id).await;
        assert_eq!(
            reported.as_deref(),
            Some(document.as_str()),
            "case {row:?}: expected {endpoint_id}'s own document, got {reported:?}"
        );
        assert_eq!(
            enabled.as_deref(),
            Some("true"),
            "case {row:?}: expected policyEnabled true, got {enabled:?}"
        );
    }

    let (reported, enabled) = reported_policy(&router, case, &policyless).await;
    assert_eq!(
        reported, None,
        "case {case:?}: the group's own policy is not the endpoint's, got {reported:?}"
    );
    assert_eq!(
        enabled.as_deref(),
        Some("false"),
        "case {case:?}: expected policyEnabled false, got {enabled:?}"
    );
    assert_ne!(
        reported.as_deref(),
        Some(GROUP_POLICY),
        "case {case:?}: the group's policy must not be reported as the endpoint's, got \
         {reported:?}"
    );
}

/// RULE 4: an id this account holds no endpoint for is refused with that resource's own
/// not-found code and sentence, and a `DryRun` of a request that would have succeeded is
/// answered the permitted side of the probe rather than with any policy.
#[tokio::test]
async fn an_unknown_endpoint_is_refused_and_a_dry_run_reports_no_policy() {
    let (router, fixture) = emulator();
    let case = "an unknown endpoint";
    let endpoint_id = create_endpoint(
        &router,
        case,
        &fixture,
        Some("permit(principal, action, resource);"),
    )
    .await;

    for presented in [UNKNOWN_ENDPOINT_ID, "vae-00000000", "vae-not-an-id"] {
        let (status, body) =
            get_policy(&router, &[p("VerifiedAccessEndpointId", presented)]).await;
        assert_eq!(
            status, 400,
            "case {presented:?}: expected HTTP 400 for an id naming no endpoint, got {status} \
             with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidVerifiedAccessEndpointId.NotFound",
            "case {presented:?}: expected the endpoint's own not-found code, got {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("The verified-access-endpoint ID '{presented}' does not exist"),
            "case {presented:?}: expected the refusal to name the id the request gave, got \
             {body}"
        );
    }

    let (status, body) = get_policy(
        &router,
        &[
            p("VerifiedAccessEndpointId", endpoint_id.clone()),
            p("DryRun", "true"),
        ],
    )
    .await;
    assert_eq!(
        status, 412,
        "case {case:?}: expected HTTP 412 from the dry run, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "DryRunOperation",
        "case {case:?}: expected DryRunOperation, got {body}"
    );
    assert_eq!(
        member(&body, "policyDocument"),
        None,
        "case {case:?}: a dry run reports no policy, got {body}"
    );
}

/// The members `DescribeVerifiedAccessEndpoints` reports for one endpoint, as the "nothing
/// moved" assertion of rule 5 compares them: the identity the service minted for it, the
/// group it hangs off, its generated domain and when it was created.
async fn described_endpoint(
    router: &MultiServiceRouter,
    case: &str,
    endpoint_id: &str,
) -> (Option<String>, Option<String>, Option<String>, Option<String>) {
    let (status, body) = post(
        router,
        "DescribeVerifiedAccessEndpoints",
        &[p("VerifiedAccessEndpointId.1", endpoint_id.to_string())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeVerifiedAccessEndpoints must succeed, got {status} with body \
         {body}"
    );
    (
        element(&body, "verifiedAccessEndpointId"),
        element(&body, "verifiedAccessGroupId"),
        element(&body, "endpointDomain"),
        element(&body, "creationTime"),
    )
}

/// RULE 5: the report is a read — two identical calls answer the same thing, and the
/// endpoint a later `DescribeVerifiedAccessEndpoints` reports is the one that was there
/// before the read.
#[tokio::test]
async fn reading_the_policy_changes_nothing() {
    let (router, fixture) = emulator();
    let case = "reading the policy changes nothing";
    let document = "permit(principal, action, resource) when { context.identity.email == \
                    \"analyst@example.test\" };";
    let endpoint_id = create_endpoint(&router, case, &fixture, Some(document)).await;

    let before = described_endpoint(&router, case, &endpoint_id).await;
    let first = reported_policy(&router, case, &endpoint_id).await;
    let second = reported_policy(&router, case, &endpoint_id).await;
    let after = described_endpoint(&router, case, &endpoint_id).await;

    assert_eq!(
        first, second,
        "case {case:?}: two identical reads must answer the same thing, got {first:?} then \
         {second:?}"
    );
    assert_eq!(
        first.0.as_deref(),
        Some(document),
        "case {case:?}: expected the stored document, got {:?}",
        first.0
    );
    assert_eq!(
        before, after,
        "case {case:?}: the endpoint a describe reports must be unchanged by the read, got \
         {before:?} then {after:?}"
    );
}
