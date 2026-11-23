// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack provisions the SageMaker AI Studio estate its template declares —
//! the domain, a user profile of it and a shared space in it — and deleting the stack removes
//! all three again.
//!
//! The rule under test: **creating a stack calls the owning service's own create API once per
//! resource the template's `Resources` section declares, so what the stack publishes for a
//! resource is the identifier that service answers for, and a create that service REFUSES
//! fails the resource and rolls the stack back.** The Studio types are where both halves
//! bite:
//!
//! * the identifier is SageMaker's to mint for a domain (`CreateDomain` answers the `d-…`
//!   `DomainId` every later call takes) and the PAIR of that domain and the member's own name
//!   for a user profile and a space — so a stack that published a template's own name for one
//!   would publish a handle no SageMaker call resolves;
//! * a domain admits a member only once its own provisioning has settled, so the members of
//!   the template cannot be created in the same pass as the domain they belong to;
//! * and a declaration SageMaker refuses — an `AuthMode` outside the two values its own enum
//!   declares — must leave the stack rolled back with that service's sentence on the failing
//!   resource, never reported `CREATE_COMPLETE`.
//!
//! Three crates have to agree for that to hold (CloudFormation makes the calls, SageMaker
//! serves them, Amazon EC2 holds the VPC and the subnet the domain's traffic goes through), so
//! the suite lives here rather than in any one of them.
//!
//! The rows differ along what the rule generalises over: the `AuthMode` the domain
//! authenticates with (and so whether its profile carries the IAM Identity Center members),
//! whether the template names the domain or leaves the name to CloudFormation, and whether the
//! space is a private one OWNED by the profile — which makes the estate three dependency
//! levels deep rather than two. Every expected value is computed from what the row declared or
//! read back from the owning service, never from the answer being checked.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery encoder and the XML/Outputs readers this suite needs already exist in the test
// binary's shared modules; they are shared rather than copied.
use crate::cloudformation_provisions_stack_resources::{outputs_of, xml_text};
use crate::query_wire::{elements, form_encode};

/// The region every request here is signed for, and so the region the stack's resources and
/// their ARNs belong to.
const REGION: &str = "us-west-2";

/// The API version an ec2Query request names beside its action.
const EC2_API_VERSION: &str = "2016-11-15";

/// A deadline far longer than any span the two services give a transient status, so a loaded
/// runner never decides whether a poll loop terminates: the CONDITION is what every wait below
/// is for.
const STATUS_DEADLINE: Duration = Duration::from_secs(180);

/// How long a poll loop waits before reading the stack again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The CIDR the VPC a domain's traffic goes through is allocated, and the one its subnet takes
/// out of it.
const VPC_CIDR: &str = "10.42.0.0/16";
const SUBNET_CIDR: &str = "10.42.1.0/24";

/// The role a Studio user runs as, which `DefaultUserSettings.ExecutionRole` names. Its value
/// is a role ARN's documented form; nothing of this suite depends on the role existing, since
/// no call it makes resolves it.
const EXECUTION_ROLE: &str = "arn:aws:iam::000000000000:role/service-role/StudioExecutionRole";

/// The emulator with the three services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sagemaker_sagemaker::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    // A Studio user runs as an IAM role, and the estate a client deploys declares that role
    // beside the domain, so the service that holds it is part of what the stack reaches.
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. The signature is never checked: this router runs with request authentication
/// off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
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
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8_lossy(response.body()).to_string();
    (status, payload)
}

/// Call one Amazon EC2 action over its ec2Query wire.
async fn ec2(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> (u16, String) {
    let mut body = format!("Action={action}&Version={EC2_API_VERSION}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ec2.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("ec2"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8_lossy(response.body()).to_string();
    (status, payload)
}

/// Call one SageMaker operation over its awsJson1_1 wire.
async fn sagemaker(router: &MultiServiceRouter, operation: &str, request: &Value) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("api.sagemaker.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("SageMaker.{operation}"))
        .header("authorization", authorization("sagemaker"))
        .body(Bytes::from(request.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// The VPC and the subnet a domain's traffic goes through, created in Amazon EC2 itself — so
/// the placement the template declares is one that service really holds.
async fn studio_network(router: &MultiServiceRouter) -> (String, String) {
    let (status, body) = ec2(router, "CreateVpc", &[("CidrBlock", VPC_CIDR)]).await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from CreateVpc, got {status} with body {body}"
    );
    let vpc_id = xml_text(&body, "vpcId").unwrap_or_else(|| panic!("no <vpcId> in {body}"));
    let (status, body) = ec2(
        router,
        "CreateSubnet",
        &[("VpcId", &vpc_id), ("CidrBlock", SUBNET_CIDR)],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from CreateSubnet, got {status} with body {body}"
    );
    let subnet_id = xml_text(&body, "subnetId").unwrap_or_else(|| panic!("no <subnetId> in {body}"));
    (vpc_id, subnet_id)
}

/// Create a stack from `template` and answer its id, failing the case with the response body
/// when the create is refused.
async fn create_stack(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    template: &str,
) -> String {
    let (status, body) = cloudformation(
        router,
        "CreateStack",
        &[("StackName", stack_name), ("TemplateBody", template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    xml_text(&body, "StackId").unwrap_or_else(|| panic!("case {case:?}: no <StackId> in {body}"))
}

/// The stack `stack` names, as the response body.
async fn describe_stack(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let (status, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStacks for {stack:?}, got {status} with \
         body {body}"
    );
    body
}

/// The statuses no further transition takes a stack out of: a stack resting in one of these
/// has settled, so a wait for a DIFFERENT one is over — and reporting that straight away is
/// what makes a case that the rule does not hold fail with the stack's own account of why
/// rather than at a deadline.
const SETTLED_STATUSES: &[&str] = &[
    "CREATE_COMPLETE",
    "DELETE_COMPLETE",
    "DELETE_FAILED",
    "ROLLBACK_COMPLETE",
    "ROLLBACK_FAILED",
    "UPDATE_COMPLETE",
    "UPDATE_ROLLBACK_COMPLETE",
];

/// Read the stack until it reports `wanted`, and answer the body that did.
///
/// The CONDITION is what is waited for, never a span: a stack whose resources are still coming
/// up in SageMaker is read again — which is also what drives the deployment's own next pass —
/// until it rests in the status the rule says it reaches, settles in another one, or
/// [`STATUS_DEADLINE`] passes.
async fn stack_resting_in(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    wanted: &str,
) -> String {
    let deadline = Instant::now() + STATUS_DEADLINE;
    loop {
        let body = describe_stack(router, case, stack).await;
        let status = xml_text(&body, "StackStatus").unwrap_or_default();
        if status == wanted {
            return body;
        }
        assert!(
            !SETTLED_STATUSES.contains(&status.as_str()),
            "case {case:?}: expected the stack to reach {wanted:?}, it settled in {status:?} \
             with body {body}"
        );
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the stack to reach {wanted:?}, got {status:?} with body \
             {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The physical resource id the stack reports for one of its logical resources — the handle a
/// client reads off the stack and calls the owning service with.
async fn physical_resource_id(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
) -> String {
    let (status, body) = cloudformation(
        router,
        "DescribeStackResource",
        &[("StackName", stack), ("LogicalResourceId", logical_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackResource for {logical_id:?}, got \
         {status} with body {body}"
    );
    xml_text(&body, "PhysicalResourceId")
        .unwrap_or_else(|| panic!("case {case:?}: no <PhysicalResourceId> in {body}"))
}

/// One text member of a SageMaker answer.
fn str_field(answered: &Value, member: &str) -> String {
    answered
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("expected a {member} in {answered}"))
        .to_string()
}

/// The logical ids this suite's templates declare its three resources under.
const DOMAIN: &str = "StudioDomain";
const PROFILE: &str = "StudioProfile";
const SPACE: &str = "StudioSpace";

/// One row of the table: the Studio estate a template declares, and what the rule says the
/// stack must then have created in SageMaker.
struct Row {
    case: &'static str,
    /// The `AuthMode` the domain authenticates its members with, which is also what decides
    /// whether its user profile carries the IAM Identity Center members.
    auth_mode: &'static str,
    /// The `DomainName` the template declares, or `None` where it leaves the name to
    /// CloudFormation.
    domain_name: Option<&'static str>,
    /// The name the template gives the user profile, and the one it gives the space.
    profile_name: &'static str,
    space_name: &'static str,
    /// Whether the space is a PRIVATE one owned by that user profile, which makes the estate
    /// three dependency levels deep: the space can be created only once the profile exists.
    owned_space: bool,
}

/// The rows: one axis per thing the rule generalises over.
const ROWS: &[Row] = &[
    Row {
        case: "an IAM domain, named by the template, with a shared space",
        auth_mode: "IAM",
        domain_name: Some("iam-studio-domain"),
        profile_name: "data-scientist",
        space_name: "shared-space",
        owned_space: false,
    },
    Row {
        case: "an IAM Identity Center domain CloudFormation names, with a space the profile owns",
        auth_mode: "SSO",
        domain_name: None,
        profile_name: "analyst",
        space_name: "private-space",
        owned_space: true,
    },
];

/// The template one row declares its estate with, resolved against the network EC2 holds.
fn template(row: &Row, vpc_id: &str, subnet_id: &str) -> String {
    let mut domain = json!({
        "AuthMode": row.auth_mode,
        "VpcId": vpc_id,
        "SubnetIds": [subnet_id],
        "DefaultUserSettings": { "ExecutionRole": EXECUTION_ROLE },
        "Tags": [{ "Key": "estate", "Value": "studio" }],
    });
    if let Some(name) = row.domain_name {
        domain["DomainName"] = json!(name);
    }
    let mut profile = json!({
        "DomainId": { "Ref": DOMAIN },
        "UserProfileName": row.profile_name,
    });
    if row.auth_mode == "SSO" {
        // The two members are required for a domain whose `AuthMode` is IAM Identity Center
        // and cannot be specified for any other, so they travel with exactly that row.
        profile["SingleSignOnUserIdentifier"] = json!("UserName");
        profile["SingleSignOnUserValue"] = json!(row.profile_name);
    }
    let mut space = json!({
        "DomainId": { "Fn::GetAtt": [DOMAIN, "DomainId"] },
        "SpaceName": row.space_name,
        "SpaceDisplayName": "The row's own space",
    });
    let mut space_resource = json!({ "Type": "AWS::SageMaker::Space" });
    if row.owned_space {
        // The owner is named by the profile's own NAME, which is the value
        // `OwnershipSettings.OwnerUserProfileName` takes, and the ORDER is stated as the
        // `DependsOn` CloudFormation reads it from: a space cannot be created before the
        // profile that owns it exists.
        space["OwnershipSettings"] = json!({ "OwnerUserProfileName": row.profile_name });
        space["SpaceSharingSettings"] = json!({ "SharingType": "Private" });
        space_resource["DependsOn"] = json!(PROFILE);
    }
    space_resource["Properties"] = space;
    json!({
        "Resources": {
            DOMAIN: { "Type": "AWS::SageMaker::Domain", "Properties": domain },
            PROFILE: { "Type": "AWS::SageMaker::UserProfile", "Properties": profile },
            SPACE: space_resource,
        },
        "Outputs": {
            "DomainId": { "Value": { "Ref": DOMAIN } },
            "DomainArn": { "Value": { "Fn::GetAtt": [DOMAIN, "DomainArn"] } },
            "DomainUrl": { "Value": { "Fn::GetAtt": [DOMAIN, "Url"] } },
            "ProfileArn": { "Value": { "Fn::GetAtt": [PROFILE, "UserProfileArn"] } },
            "SpaceArn": { "Value": { "Fn::GetAtt": [SPACE, "SpaceArn"] } },
            "SpaceUrl": { "Value": { "Fn::GetAtt": [SPACE, "Url"] } },
        }
    })
    .to_string()
}

/// RULE: a stack's Studio estate is created in SageMaker, the stack publishes the identifiers
/// that service answered for each of its three resources, and the stack's deletion removes all
/// three.
#[tokio::test]
async fn a_stack_provisions_the_studio_domain_its_profile_and_its_space() {
    let router = build_router();
    let (vpc_id, subnet_id) = studio_network(&router).await;

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let stack_name = format!("studio-estate-{index}");
        let template = template(row, &vpc_id, &subnet_id);
        let stack_id = create_stack(&router, case, &stack_name, &template).await;

        // Every one of the three resources comes up `Pending` in SageMaker and a domain admits
        // a member only once it is `InService`, so the case waits for the CONDITION the rule
        // names: the stack rests in `CREATE_COMPLETE`.
        let body = stack_resting_in(&router, case, &stack_name, "CREATE_COMPLETE").await;
        let outputs = outputs_of(&body);

        // The domain's physical id is the one SageMaker minted, not the name the template
        // asked for: it is what `Ref` resolves to and the handle `DescribeDomain` takes.
        let domain_id = physical_resource_id(&router, case, &stack_name, DOMAIN).await;
        assert!(
            domain_id.starts_with("d-"),
            "case {case:?}: expected the domain's physical id to be the DomainId SageMaker \
             minted, got {domain_id:?}"
        );
        if let Some(declared) = row.domain_name {
            assert_ne!(
                domain_id, declared,
                "case {case:?}: expected the minted id rather than the declared name, got \
                 {domain_id:?}"
            );
        }
        assert_eq!(
            outputs.get("DomainId").map(String::as_str),
            Some(domain_id.as_str()),
            "case {case:?}: expected the stack to publish the minted domain id, got {outputs:?}"
        );

        // The domain the template declared exists in SageMaker, as that service's own create
        // would have made it, and what the stack publishes for it is what that service reports.
        let (status, described) = sagemaker(
            &router,
            "DescribeDomain",
            &json!({ "DomainId": domain_id.clone() }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeDomain for {domain_id:?}, got \
             {status} with body {described}"
        );
        assert_eq!(
            described.get("AuthMode").and_then(Value::as_str),
            Some(row.auth_mode),
            "case {case:?}: expected the declared AuthMode, got {described}"
        );
        assert_eq!(
            described.get("VpcId").and_then(Value::as_str),
            Some(vpc_id.as_str()),
            "case {case:?}: expected the declared VpcId, got {described}"
        );
        assert_eq!(
            described.get("SubnetIds"),
            Some(&json!([subnet_id.clone()])),
            "case {case:?}: expected the declared SubnetIds, got {described}"
        );
        assert_eq!(
            described.get("Status").and_then(Value::as_str),
            Some("InService"),
            "case {case:?}: expected a domain the stack reported created to be in service, got \
             {described}"
        );
        if let Some(declared) = row.domain_name {
            assert_eq!(
                described.get("DomainName").and_then(Value::as_str),
                Some(declared),
                "case {case:?}: expected the create to carry the declared name, got {described}"
            );
        }
        assert_eq!(
            outputs.get("DomainArn").map(String::as_str),
            Some(str_field(&described, "DomainArn").as_str()),
            "case {case:?}: expected the stack's DomainArn to be the one SageMaker reports, got \
             {outputs:?}"
        );
        assert_eq!(
            outputs.get("DomainUrl").map(String::as_str),
            Some(str_field(&described, "Url").as_str()),
            "case {case:?}: expected the stack's DomainUrl to be the one SageMaker reports, got \
             {outputs:?}"
        );

        // The profile and the space are each identified by the PAIR their own API addresses
        // them by — the member's own name, and the domain SageMaker minted an id for.
        let profile_id = physical_resource_id(&router, case, &stack_name, PROFILE).await;
        assert_eq!(
            profile_id,
            format!("{}|{domain_id}", row.profile_name),
            "case {case:?}: expected the profile's physical id to be its own pair, got \
             {profile_id:?}"
        );
        let space_id = physical_resource_id(&router, case, &stack_name, SPACE).await;
        assert_eq!(
            space_id,
            format!("{domain_id}|{}", row.space_name),
            "case {case:?}: expected the space's physical id to be its own pair, got {space_id:?}"
        );

        let (status, profile) = sagemaker(
            &router,
            "DescribeUserProfile",
            &json!({ "DomainId": domain_id.clone(), "UserProfileName": row.profile_name }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeUserProfile for {:?}, got {status} \
             with body {profile}",
            row.profile_name
        );
        assert_eq!(
            profile.get("Status").and_then(Value::as_str),
            Some("InService"),
            "case {case:?}: expected the profile the stack reported created to be in service, \
             got {profile}"
        );
        assert_eq!(
            outputs.get("ProfileArn").map(String::as_str),
            Some(str_field(&profile, "UserProfileArn").as_str()),
            "case {case:?}: expected the stack's ProfileArn to be the one SageMaker reports, \
             got {outputs:?}"
        );
        if row.auth_mode == "SSO" {
            assert_eq!(
                profile.get("SingleSignOnUserValue").and_then(Value::as_str),
                Some(row.profile_name),
                "case {case:?}: expected the declared IAM Identity Center value, got {profile}"
            );
        }

        let (status, space) = sagemaker(
            &router,
            "DescribeSpace",
            &json!({ "DomainId": domain_id.clone(), "SpaceName": row.space_name }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeSpace for {:?}, got {status} with \
             body {space}",
            row.space_name
        );
        assert_eq!(
            space.get("SpaceDisplayName").and_then(Value::as_str),
            Some("The row's own space"),
            "case {case:?}: expected the declared display name, got {space}"
        );
        assert_eq!(
            space.get("Status").and_then(Value::as_str),
            Some("InService"),
            "case {case:?}: expected the space the stack reported created to be in service, got \
             {space}"
        );
        assert_eq!(
            outputs.get("SpaceArn").map(String::as_str),
            Some(str_field(&space, "SpaceArn").as_str()),
            "case {case:?}: expected the stack's SpaceArn to be the one SageMaker reports, got \
             {outputs:?}"
        );
        assert_eq!(
            outputs.get("SpaceUrl").map(String::as_str),
            Some(str_field(&space, "Url").as_str()),
            "case {case:?}: expected the stack's SpaceUrl to be the one SageMaker reports, got \
             {outputs:?}"
        );
        if row.owned_space {
            assert_eq!(
                space.get("OwnershipSettings"),
                Some(&json!({ "OwnerUserProfileName": row.profile_name })),
                "case {case:?}: expected the space to be owned by the declared profile, got \
                 {space}"
            );
        }

        // Deleting the stack deletes all three, in the reverse order their dependencies allow:
        // the member of the domain goes first, and the domain only once SageMaker has let the
        // member go. A stack that has been deleted is read by its ID, which is the handle that
        // outlives the name.
        let (status, body) =
            cloudformation(&router, "DeleteStack", &[("StackName", &stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {body}"
        );
        let body = stack_resting_in(&router, case, &stack_id, "DELETE_COMPLETE").await;
        assert!(
            !body.is_empty(),
            "case {case:?}: expected a DescribeStacks body for the deleted stack"
        );
        for (operation, request) in [
            ("DescribeSpace", json!({ "DomainId": domain_id.clone(), "SpaceName": row.space_name })),
            (
                "DescribeUserProfile",
                json!({ "DomainId": domain_id.clone(), "UserProfileName": row.profile_name }),
            ),
            ("DescribeDomain", json!({ "DomainId": domain_id.clone() })),
        ] {
            let (status, gone) = sagemaker(&router, operation, &request).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected {operation} to report the resource gone after the \
                 stack's deletion, got {status} with body {gone}"
            );
        }
    }
}

/// RULE: the same estate deployed as ONE template — the VPC and the subnet the domain's
/// traffic goes through, the role its users run as, and the three Studio resources — is
/// created in every owning service, with each declaration reaching its service carrying the
/// identifiers the resources it names really have.
///
/// This is the shape a client deploys a Studio domain in: nothing of the network exists before
/// the stack, so every reference in the domain's declaration is resolved against a resource the
/// same deployment creates. One case, because what it adds over the table above is the
/// dependency chain rather than another row of the same rule.
#[tokio::test]
async fn a_stack_provisions_the_network_its_studio_domain_goes_through() {
    let case = "a template that declares the network and the role beside the estate";
    let router = build_router();
    let stack_name = "studio-estate-network";
    let template = json!({
        "Resources": {
            "StudioVpc": {
                "Type": "AWS::EC2::VPC",
                "Properties": { "CidrBlock": VPC_CIDR }
            },
            "StudioSubnet": {
                "Type": "AWS::EC2::Subnet",
                "Properties": {
                    "VpcId": { "Ref": "StudioVpc" },
                    "CidrBlock": SUBNET_CIDR
                }
            },
            "StudioRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": { "Service": "sagemaker.amazonaws.com" },
                            "Action": "sts:AssumeRole"
                        }]
                    }
                }
            },
            "StudioSecurityGroup": {
                "Type": "AWS::EC2::SecurityGroup",
                "Properties": {
                    "GroupDescription": "the security group a Studio app communicates through",
                    "VpcId": { "Ref": "StudioVpc" }
                }
            },
            DOMAIN: {
                "Type": "AWS::SageMaker::Domain",
                "Properties": {
                    "AuthMode": "IAM",
                    "DomainName": "network-studio-domain",
                    "VpcId": { "Ref": "StudioVpc" },
                    "SubnetIds": [{ "Ref": "StudioSubnet" }],
                    "AppNetworkAccessType": "VpcOnly",
                    "AppSecurityGroupManagement": "Service",
                    "TagPropagation": "ENABLED",
                    "KmsKeyId": "alias/aws/sagemaker",
                    "DomainSettings": {
                        "SecurityGroupIds": [{ "Ref": "StudioSecurityGroup" }]
                    },
                    "DefaultSpaceSettings": {
                        "ExecutionRole": { "Fn::GetAtt": ["StudioRole", "Arn"] }
                    },
                    "DefaultUserSettings": {
                        "ExecutionRole": { "Fn::GetAtt": ["StudioRole", "Arn"] },
                        "SecurityGroups": [{ "Ref": "StudioSecurityGroup" }],
                        "StudioWebPortal": "ENABLED",
                        "DefaultLandingUri": "studio::"
                    },
                    "Tags": [{ "Key": "estate", "Value": "studio-network" }]
                }
            },
            PROFILE: {
                "Type": "AWS::SageMaker::UserProfile",
                "Properties": {
                    "DomainId": { "Ref": DOMAIN },
                    "UserProfileName": "network-scientist",
                    "UserSettings": {
                        "ExecutionRole": { "Fn::GetAtt": ["StudioRole", "Arn"] }
                    }
                }
            },
            SPACE: {
                "Type": "AWS::SageMaker::Space",
                "Properties": {
                    "DomainId": { "Ref": DOMAIN },
                    "SpaceName": "network-space"
                }
            }
        },
        "Outputs": {
            "DomainId": { "Value": { "Ref": DOMAIN } },
            "VpcId": { "Value": { "Ref": "StudioVpc" } },
            "SubnetId": { "Value": { "Ref": "StudioSubnet" } },
            "RoleArn": { "Value": { "Fn::GetAtt": ["StudioRole", "Arn"] } }
        }
    })
    .to_string();

    let (status, body) = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", stack_name),
            ("TemplateBody", &template),
            // The template declares an IAM role CloudFormation names, which is what the
            // acknowledgement covers.
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    let body = stack_resting_in(&router, case, stack_name, "CREATE_COMPLETE").await;
    let outputs = outputs_of(&body);

    // The domain reached SageMaker carrying the ids EC2 really minted for the VPC and the
    // subnet of the same deployment — a placement that did not resolve would have left the
    // domain `Failed` and the resource with it.
    let domain_id = outputs
        .get("DomainId")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no DomainId output in {body}"));
    let (status, described) = sagemaker(
        &router,
        "DescribeDomain",
        &json!({ "DomainId": domain_id.clone() }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeDomain, got {status} with body {described}"
    );
    assert_eq!(
        described.get("Status").and_then(Value::as_str),
        Some("InService"),
        "case {case:?}: expected the domain to be in service, got {described}"
    );
    assert_eq!(
        described.get("VpcId").and_then(Value::as_str),
        outputs.get("VpcId").map(String::as_str),
        "case {case:?}: expected the domain to carry the VPC the stack created, got {described}"
    );
    assert_eq!(
        described.get("SubnetIds"),
        Some(&json!([outputs.get("SubnetId").cloned().unwrap_or_default()])),
        "case {case:?}: expected the domain to carry the subnet the stack created, got \
         {described}"
    );
    assert_eq!(
        described
            .get("DefaultUserSettings")
            .and_then(|settings| settings.get("ExecutionRole"))
            .and_then(Value::as_str),
        outputs.get("RoleArn").map(String::as_str),
        "case {case:?}: expected the domain to run its users as the role the stack created, \
         got {described}"
    );

    // ...and so did both members of it.
    let (status, profile) = sagemaker(
        &router,
        "DescribeUserProfile",
        &json!({ "DomainId": domain_id.clone(), "UserProfileName": "network-scientist" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeUserProfile, got {status} with body \
         {profile}"
    );
    let (status, space) = sagemaker(
        &router,
        "DescribeSpace",
        &json!({ "DomainId": domain_id.clone(), "SpaceName": "network-space" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeSpace, got {status} with body {space}"
    );
}

/// RULE: a declared value outside the set its property admits is refused and nothing is
/// provisioned for it — by CloudFormation's own pre-deployment validation, which fails the
/// operation before any resource is asked for, and by the owning service itself where the
/// request turns that validation off; either way the stack rolls back and no domain is left
/// behind.
///
/// The rows span both directions of the rule for the member whose value set the type publishes
/// (`AuthMode` is `SSO` or `IAM`): a value outside that set in two shapes, each with the
/// validation on and off, and the positive control of a value inside it.
#[tokio::test]
async fn a_domain_whose_auth_mode_is_outside_the_published_set_rolls_the_stack_back() {
    let router = build_router();
    // The placement these rows declare names a VPC and a subnet this emulator's Amazon EC2
    // holds nothing for, which is the shape a client's own template has: the network the
    // domain goes through exists in the caller's account rather than in the stack. Nothing EC2
    // says nothing about is held to anything, so the placement is not what decides these rows
    // — the `AuthMode` is.
    let vpc_id = "vpc-00000000000000042".to_string();
    let subnet_id = "subnet-00000000000000042".to_string();

    /// One row: the case, the declared `AuthMode`, whether the request turns the
    /// pre-deployment validations off, and the status the stack must rest in.
    struct Rejected {
        case: &'static str,
        auth_mode: &'static str,
        disable_validation: bool,
        wanted: &'static str,
    }
    let rows = [
        Rejected {
            case: "a value outside the published set",
            auth_mode: "OPEN_ID",
            disable_validation: false,
            wanted: "ROLLBACK_COMPLETE",
        },
        Rejected {
            case: "the right value in the wrong case",
            auth_mode: "iam",
            disable_validation: false,
            wanted: "ROLLBACK_COMPLETE",
        },
        Rejected {
            case: "a value outside the set, with the validations turned off",
            auth_mode: "OPEN_ID",
            disable_validation: true,
            wanted: "ROLLBACK_COMPLETE",
        },
        Rejected {
            case: "the positive control, a value inside it",
            auth_mode: "IAM",
            disable_validation: false,
            wanted: "CREATE_COMPLETE",
        },
    ];
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let domain_name = format!("rejected-studio-{index}");
        let template = json!({
            "Resources": {
                DOMAIN: {
                    "Type": "AWS::SageMaker::Domain",
                    "Properties": {
                        "DomainName": domain_name.clone(),
                        "AuthMode": row.auth_mode,
                        "VpcId": vpc_id.clone(),
                        "SubnetIds": [subnet_id.clone()],
                        "DefaultUserSettings": { "ExecutionRole": EXECUTION_ROLE },
                    }
                }
            },
            "Outputs": { "DomainId": { "Value": { "Ref": DOMAIN } } }
        })
        .to_string();
        let stack_name = format!("studio-authmode-{index}");
        let mut request: Vec<(&str, &str)> =
            vec![("StackName", &stack_name), ("TemplateBody", &template)];
        if row.disable_validation {
            request.push(("DisableValidation", "true"));
        }
        let (status, created) = cloudformation(&router, "CreateStack", &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {created}"
        );
        let body = stack_resting_in(&router, case, &stack_name, row.wanted).await;

        if row.wanted == "CREATE_COMPLETE" {
            // The control deploys: the same template with a value the enum declares reaches
            // SageMaker, which holds a domain of that name.
            let domain_id = outputs_of(&body)
                .get("DomainId")
                .cloned()
                .unwrap_or_else(|| panic!("case {case:?}: no DomainId output in {body}"));
            let (status, described) = sagemaker(
                &router,
                "DescribeDomain",
                &json!({ "DomainId": domain_id.clone() }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the control's domain to exist, got {status} with body \
                 {described}"
            );
            assert_eq!(
                described.get("DomainName").and_then(Value::as_str),
                Some(domain_name.as_str()),
                "case {case:?}: expected the control's declared name, got {described}"
            );
            continue;
        }

        if row.disable_validation {
            // The validations are off, so the declaration reaches SageMaker and the failing
            // resource's own event carries the refusal that service worded, inside the envelope
            // CloudFormation quotes a resource provider's answer in.
            let (status, events) =
                cloudformation(&router, "DescribeStackEvents", &[("StackName", &stack_name)])
                    .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from DescribeStackEvents, got {status} with \
                 body {events}"
            );
            let statuses = elements(&events, "ResourceStatus");
            assert!(
                statuses.iter().any(|reported| reported == "CREATE_FAILED"),
                "case {case:?}: expected the domain's own event to report CREATE_FAILED, got \
                 {statuses:?}"
            );
            let reasons = elements(&events, "ResourceStatusReason");
            assert!(
                reasons.iter().any(|reason| {
                    reason.contains("Resource handler returned message:")
                        && reason.contains("authMode")
                        && reason.contains("enum value set")
                }),
                "case {case:?}: expected the failing resource's event to carry SageMaker's own \
                 refusal naming the member and its value set, got {reasons:?}"
            );
        } else {
            // The validations are on, so the operation failed BEFORE anything was asked of
            // SageMaker: the validation result names the validation, how its failure was
            // handled, the value it rejected beside the set the property admits, and the
            // element of the template it judged.
            let (status, events) =
                cloudformation(&router, "DescribeEvents", &[("StackName", &stack_name)]).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from DescribeEvents, got {status} with body \
                 {events}"
            );
            let reported: Vec<Vec<String>> = ["LogicalResourceId", "ValidationName",
                "ValidationStatus", "ValidationFailureMode", "ValidationStatusReason",
                "ValidationPath"]
                .iter()
                .map(|member| elements(&events, member))
                .collect();
            let expected = [
                DOMAIN.to_string(),
                "PROPERTY_VALIDATION".to_string(),
                "FAILED".to_string(),
                "FAIL".to_string(),
                format!(
                    "{} is not a valid enum value. Supported values: [SSO, IAM]",
                    row.auth_mode
                ),
                format!("/Resources/{DOMAIN}/Properties/AuthMode"),
            ];
            for (member, expected) in reported.iter().zip(expected.iter()) {
                assert!(
                    member.contains(expected),
                    "case {case:?}: expected a validation event carrying {expected:?}, got \
                     {member:?} in {events}"
                );
            }
        }

        // ...and nothing is left provisioned: SageMaker holds no domain of that name.
        let (status, listed) = sagemaker(&router, "ListDomains", &json!({})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from ListDomains, got {status} with body {listed}"
        );
        let names: Vec<&str> = listed
            .get("Domains")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter_map(|domain| domain.get("DomainName").and_then(Value::as_str))
            .collect();
        assert!(
            !names.contains(&domain_name.as_str()),
            "case {case:?}: expected no domain named {domain_name:?} to be left behind, got \
             {names:?}"
        );
    }
}
