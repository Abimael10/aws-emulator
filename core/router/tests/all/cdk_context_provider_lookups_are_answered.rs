// SPDX-License-Identifier: CC0-1.0

//! The two CDK context providers an environment-aware app runs — the EC2 VPC lookup and the
//! Systems Manager parameter lookup — driven end to end through ONE assumed lookup role, so
//! each provider's reads are decided and answered by the service that owns them.
//!
//! THE RULE UNDER TEST: **every read a context provider drives is ANSWERED — served from the
//! account's own state when the identity-based policies in force on the principal the
//! credential speaks for allow it at the instant of the call, and otherwise refused with the
//! modelled refusal of the service that owns the read (Amazon EC2's HTTP 403
//! `UnauthorizedOperation`, Systems Manager's HTTP 400 `AccessDeniedException`) — and each
//! decision is taken on that one service's own action, so a statement about one provider's
//! read never changes what the other provider resolves, and deleting the statement serves the
//! read again.** The VPC a lookup selects is the one the account holds under the tag the
//! request filters on, its subnets report the Availability Zone and the explicitly associated
//! route table they were created with, and a parameter read reports the value that was
//! written — or, once the parameter or the VPC is gone, the modelled absence
//! (`ParameterNotFound`, `InvalidVpcID.NotFound`) rather than a stale answer.
//!
//! Why this suite drives the assembled router rather than one service crate: the rule spans
//! four services at once. Amazon EC2 answers the VPC provider's reads, Systems Manager
//! answers the parameter provider's read, IAM holds the lookup role and the documents in
//! force on it, and STS mints the session whose principal a refusal names. Each crate's own
//! tests can pin only its own half; that both providers run through the SAME role and that
//! one provider's refusal leaves the other's resolution untouched can only be pinned here.
//!
//! The rows span what the rule generalises over:
//!
//! * the ESTATE the VPC provider resolves — how many Availability Zones it spans, how many
//!   subnets it holds, which subnet groups exist at all (a VPC with no public subnet is a
//!   row), the size of the blocks, and the form of the parameter name beside it (a hierarchy,
//!   a deeper hierarchy, a flat name) with a value of its own;
//! * what the documents in force say — nothing denied, one provider's single read denied, a
//!   whole `Describe` family denied under a wildcard, every action of the parameter service
//!   denied, and one read of EACH provider denied by one statement;
//! * the two halves of a policy CHANGE: the statement stored under a live session, and the
//!   same session once the statement is deleted again.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{block_of, element, elements, form_encode, items_of, opt_element};

/// The account every generated emulator presents, which every ARN below carries.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The Region every request here is signed for.
const REGION: &str = "us-west-2";

/// The account's own published credential: a key neither STS nor IAM holds a record for, so
/// it is the administrative caller that builds the estate the lookups resolve.
const ACCOUNT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The API versions a client sends in a query-protocol body.
const EC2_API_VERSION: &str = "2016-11-15";
const IAM_API_VERSION: &str = "2010-05-08";
const STS_API_VERSION: &str = "2011-06-15";

/// The `X-Amz-Target` prefix of Systems Manager's awsJson1_1 wire.
const SSM_TARGET_PREFIX: &str = "AmazonSSM";

/// The session name every `AssumeRole` here asks for, which is the last segment of the
/// assumed-role ARN a refusal names.
const SESSION: &str = "cdk-lookup";

/// The destination a default route out of a VPC carries, and the tag key a lookup selects a
/// VPC by.
const ALL_IPV4: &str = "0.0.0.0/0";
const NAME_TAG: &str = "Name";

/// Amazon EC2's code for a caller that may not perform the action it asked for, and Systems
/// Manager's. Each service words its own refusal: "If you have the required permissions, the
/// error response is DryRunOperation. Otherwise, it is UnauthorizedOperation"
/// (`DescribeVpcsRequest$DryRun`); `AccessDeniedException` is "The requester doesn't have
/// permissions to perform the requested operation" (`GetParameter`).
const UNAUTHORIZED_OPERATION: &str = "UnauthorizedOperation";
const ACCESS_DENIED: &str = "AccessDeniedException";

/// The four Amazon EC2 reads the VPC context provider walks, in the order every table below
/// lists them. Source: the operations whose results the provider is built out of
/// (`smithy_query(query="operation",operation="DescribeAvailabilityZones,DescribeRouteTables,DescribeSubnets,DescribeVpcs")`),
/// each a `Filters`-driven list read declaring no modelled error of its own.
const EC2_READS: [&str; 4] = [
    "DescribeAvailabilityZones",
    "DescribeRouteTables",
    "DescribeSubnets",
    "DescribeVpcs",
];

/// The one Systems Manager read the parameter context provider drives.
const SSM_READ: &str = "GetParameter";

/// The two groups a looked-up VPC's subnets are partitioned into by their route tables: one
/// that can reach the internet and one that cannot.
const PUBLIC: &str = "Public";
const ISOLATED: &str = "Isolated";

// ---------------------------------------------------------------------------
// The emulator and its wires
// ---------------------------------------------------------------------------

/// The four services this rule spans, over ONE engine, as `core/router/src/lib.rs` mounts
/// them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Nothing here signs: the `Authorization` header carries only the credential scope,
    // which is where a service reads the Region and the caller's access key id.
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_ssm_amazonssm::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` and whose
/// credential is `access_key_id` — the only place the caller's identity appears on the wire.
fn authorization(access_key_id: &str, signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One form-encoded query-protocol call signed by `access_key_id`, as its status and body.
async fn query_as(
    router: &MultiServiceRouter,
    access_key_id: &str,
    signing_name: &str,
    api_version: &str,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={api_version}");
    for (key, value) in params {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(access_key_id, signing_name))
        .body(Bytes::from(body))
        .expect("the test request builds");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One Amazon EC2 call on its ec2Query wire, signed by `access_key_id`.
async fn ec2_as(
    router: &MultiServiceRouter,
    access_key_id: &str,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    query_as(
        router,
        access_key_id,
        "ec2",
        EC2_API_VERSION,
        action,
        params,
    )
    .await
}

/// One IAM call, made by the account's own published credential — the identity that holds the
/// lookup role and the documents in force on it.
async fn iam_call(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    query_as(
        router,
        ACCOUNT_KEY,
        "iam",
        IAM_API_VERSION,
        action,
        params,
    )
    .await
}

/// One Systems Manager call on its awsJson1_1 wire, signed by `access_key_id`. The error
/// shape travels in `__type`; the header the framework also sets is folded in so one accessor
/// reads either.
async fn ssm_as(
    router: &MultiServiceRouter,
    access_key_id: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ssm.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{SSM_TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization(access_key_id, "ssm"))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("the test request builds");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let raw = response.into_body();
    let mut parsed: Value = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).to_string()))
    };
    if let (Some(error_type), Value::Object(map)) = (error_type, &mut parsed) {
        map.entry("__type")
            .or_insert_with(|| Value::String(error_type));
    }
    (status, parsed)
}

/// The wire error code of an awsJson failure body (`__type`, without its namespace).
fn json_error_code(body: &Value) -> String {
    let rendered = body["__type"].as_str().unwrap_or("<no __type>");
    rendered
        .rsplit(['#', '/'])
        .next()
        .unwrap_or(rendered)
        .to_string()
}

/// The `message`/`Message` member of an awsJson refusal, whichever casing it carries.
fn json_message(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The `<Code>` and `<Message>` of a query-protocol failure body.
fn query_error(body: &str) -> (String, String) {
    (
        opt_element(body, "Code").unwrap_or_default(),
        opt_element(body, "Message").unwrap_or_default(),
    )
}

// ---------------------------------------------------------------------------
// The lookup role, and the documents in force on it
// ---------------------------------------------------------------------------

/// A trust policy admitting the account's own principals, which is what lets the published
/// credential mint the session the lookups are driven as.
fn trust_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"AWS": format!("arn:aws:iam::{ACCOUNT}:root")},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// A one-statement identity-based document: `effect` on `actions` over `resource`.
fn statement(effect: &str, actions: &[&str], resource: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": effect,
            "Action": actions,
            "Resource": resource,
        }],
    })
    .to_string()
}

/// The grant a lookup role carries: the Amazon EC2 `Describe` family over every resource (a
/// `Describe` action supports no resource-level permissions) and the parameter read over the
/// account's parameters in this Region.
fn lookup_grants() -> Vec<(&'static str, String)> {
    vec![
        (
            "allow-describes",
            statement("Allow", &["ec2:Describe*"], "*"),
        ),
        (
            "allow-parameter-reads",
            statement(
                "Allow",
                &["ssm:GetParameter"],
                &format!("arn:aws:ssm:{REGION}:{ACCOUNT}:parameter/*"),
            ),
        ),
    ]
}

/// Create the lookup role `role_name` with `inline` documents on it and assume it: answers the
/// session's access key id and the ARN `sts:GetCallerIdentity` reports for it, which a refusal
/// must name.
async fn lookup_session(
    router: &MultiServiceRouter,
    case: &str,
    role_name: &str,
    inline: &[(&str, String)],
) -> (String, String) {
    let trust = trust_policy();
    let (status, created) = iam_call(
        router,
        "CreateRole",
        &[
            ("RoleName", role_name),
            ("AssumeRolePolicyDocument", &trust),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the lookup role must be created, got {status} {created}"
    );
    for (policy_name, document) in inline {
        put_role_policy(router, case, role_name, policy_name, document).await;
    }
    let role_arn = format!("arn:aws:iam::{ACCOUNT}:role/{role_name}");
    let (status, assumed) = query_as(
        router,
        ACCOUNT_KEY,
        "sts",
        STS_API_VERSION,
        "AssumeRole",
        &[("RoleArn", &role_arn), ("RoleSessionName", SESSION)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the lookup role must be assumable, got {status} {assumed}"
    );
    (
        element(&assumed, "AccessKeyId"),
        element(&assumed, "Arn"),
    )
}

/// Store one inline document on `role_name`.
async fn put_role_policy(
    router: &MultiServiceRouter,
    case: &str,
    role_name: &str,
    policy_name: &str,
    document: &str,
) {
    let (status, put) = iam_call(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", role_name),
            ("PolicyName", policy_name),
            ("PolicyDocument", document),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the document {policy_name:?} must be stored on {role_name}, got \
         {status} {put}"
    );
}

/// Delete one inline document from `role_name`.
async fn delete_role_policy(
    router: &MultiServiceRouter,
    case: &str,
    role_name: &str,
    policy_name: &str,
) {
    let (status, deleted) = iam_call(
        router,
        "DeleteRolePolicy",
        &[("RoleName", role_name), ("PolicyName", policy_name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the document {policy_name:?} must be deletable, got {status} {deleted}"
    );
}

/// What a read answers under the documents in force.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// The policies allow it: the read is served.
    Served,
    /// A readable document DENIES it, which refuses the call whatever else is allowed.
    ExplicitDeny,
}

/// The refusal Amazon EC2 words for `principal` asking for `action` under an explicit deny.
fn ec2_refusal(principal: &str, action: &str) -> String {
    format!(
        "You are not authorized to perform this operation. User: {principal} is not \
         authorized to perform: {action} with an explicit deny in an identity-based policy"
    )
}

/// The refusal Systems Manager words for `principal` asking for `action` on `resource` under
/// an explicit deny.
fn ssm_refusal(principal: &str, action: &str, resource: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         with an explicit deny in an identity-based policy"
    )
}

/// The ARN of the parameter `name`, as an IAM statement and a refusal name it: a hierarchical
/// name keeps its whole path after the `parameter/` prefix, and a flat one is addressed under
/// that prefix too.
fn parameter_arn(name: &str) -> String {
    format!(
        "arn:aws:ssm:{REGION}:{ACCOUNT}:parameter/{}",
        name.strip_prefix('/').unwrap_or(name)
    )
}

// ---------------------------------------------------------------------------
// The estate a lookup resolves
// ---------------------------------------------------------------------------

/// One subnet an estate declares: the block cut for it, which of the Region's Availability
/// Zones it sits in, and whether the route table it is associated with carries a default
/// route out through the internet gateway — which is what partitions a looked-up VPC's
/// subnets into its groups.
struct DeclaredSubnet {
    cidr: &'static str,
    zone: usize,
    public: bool,
}

/// One row: a VPC estate a lookup selects by tag, and the parameter beside it.
struct Estate {
    case: &'static str,
    /// The value of the `Name` tag the VPC lookup filters on.
    name_tag: &'static str,
    vpc_cidr: &'static str,
    subnets: &'static [DeclaredSubnet],
    parameter: &'static str,
    value: &'static str,
}

const ESTATES: &[Estate] = &[
    Estate {
        case: "one zone, a public subnet and an isolated one",
        name_tag: "cdk-lookup-one-zone",
        vpc_cidr: "10.42.0.0/16",
        subnets: &[
            DeclaredSubnet {
                cidr: "10.42.0.0/20",
                zone: 0,
                public: true,
            },
            DeclaredSubnet {
                cidr: "10.42.16.0/20",
                zone: 0,
                public: false,
            },
        ],
        parameter: "/sim2real/cdklookup/one-zone",
        value: "marker-one-zone",
    },
    Estate {
        case: "two zones, two public subnets and two isolated ones",
        name_tag: "cdk-lookup-two-zones",
        vpc_cidr: "172.31.0.0/16",
        subnets: &[
            DeclaredSubnet {
                cidr: "172.31.0.0/20",
                zone: 0,
                public: true,
            },
            DeclaredSubnet {
                cidr: "172.31.16.0/20",
                zone: 1,
                public: true,
            },
            DeclaredSubnet {
                cidr: "172.31.32.0/20",
                zone: 0,
                public: false,
            },
            DeclaredSubnet {
                cidr: "172.31.48.0/20",
                zone: 1,
                public: false,
            },
        ],
        parameter: "/sim2real/cdklookup/two-zones/marker",
        value: "a marker with spaces",
    },
    Estate {
        case: "a smaller VPC with one isolated subnet, under a flat parameter name",
        name_tag: "cdk-lookup-isolated-only",
        vpc_cidr: "192.168.0.0/20",
        subnets: &[DeclaredSubnet {
            cidr: "192.168.1.0/24",
            zone: 0,
            public: false,
        }],
        parameter: "cdk-lookup-flat",
        value: "7",
    },
];

/// One subnet of a built estate, as the account really holds it.
#[derive(Debug, Clone)]
struct BuiltSubnet {
    id: String,
    zone: String,
    route_table: String,
    association: String,
    public: bool,
}

/// A built estate: the ids the lookups must resolve to.
#[derive(Debug, Clone)]
struct Built {
    vpc_id: String,
    gateway_id: String,
    subnets: Vec<BuiltSubnet>,
}

impl Built {
    /// Every route table the estate created, each exactly once.
    fn route_tables(&self) -> BTreeSet<String> {
        self.subnets
            .iter()
            .map(|subnet| subnet.route_table.clone())
            .collect()
    }
}

/// One Amazon EC2 call made by the account's own credential — the identity that builds the
/// estate the lookups resolve, which is the caller the policy decision does not bound.
async fn seed(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) = ec2_as(router, ACCOUNT_KEY, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {action} must be served for the account's own credential, got {status} \
         {body}"
    );
    body
}

/// The Availability Zones the Region reports, in the order it reports them.
async fn zone_names(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let body = seed(router, case, "DescribeAvailabilityZones", &[]).await;
    elements(&body, "zoneName")
}

/// Build `estate` through the services' own APIs: the tagged VPC, its internet gateway, one
/// route table per group — the public one carrying the default route out, the isolated one
/// carrying none — the subnets associated with them, and the parameter beside it.
async fn build_estate(router: &MultiServiceRouter, estate: &Estate) -> Built {
    let case = estate.case;
    let zones = zone_names(router, case).await;

    let created = seed(router, case, "CreateVpc", &[("CidrBlock", estate.vpc_cidr)]).await;
    let vpc_id = element(&created, "vpcId");
    seed(
        router,
        case,
        "CreateTags",
        &[
            ("ResourceId.1", &vpc_id),
            ("Tag.1.Key", NAME_TAG),
            ("Tag.1.Value", estate.name_tag),
        ],
    )
    .await;

    let gateway = seed(router, case, "CreateInternetGateway", &[]).await;
    let gateway_id = element(&gateway, "internetGatewayId");
    seed(
        router,
        case,
        "AttachInternetGateway",
        &[("InternetGatewayId", &gateway_id), ("VpcId", &vpc_id)],
    )
    .await;

    let mut tables: BTreeMap<bool, String> = BTreeMap::new();
    for declared in estate.subnets {
        if tables.contains_key(&declared.public) {
            continue;
        }
        let body = seed(router, case, "CreateRouteTable", &[("VpcId", &vpc_id)]).await;
        let table_id = element(&body, "routeTableId");
        if declared.public {
            seed(
                router,
                case,
                "CreateRoute",
                &[
                    ("RouteTableId", &table_id),
                    ("DestinationCidrBlock", ALL_IPV4),
                    ("GatewayId", &gateway_id),
                ],
            )
            .await;
        }
        tables.insert(declared.public, table_id);
    }

    let mut subnets = Vec::new();
    for declared in estate.subnets {
        let zone = zones.get(declared.zone).cloned().unwrap_or_else(|| {
            panic!(
                "case {case:?}: the Region must report at least {} zone(s), got {zones:?}",
                declared.zone + 1
            )
        });
        let body = seed(
            router,
            case,
            "CreateSubnet",
            &[
                ("VpcId", &vpc_id),
                ("CidrBlock", declared.cidr),
                ("AvailabilityZone", &zone),
            ],
        )
        .await;
        let subnet_id = element(&body, "subnetId");
        let route_table = tables
            .get(&declared.public)
            .cloned()
            .expect("a table was created for every group the estate declares");
        let associated = seed(
            router,
            case,
            "AssociateRouteTable",
            &[("RouteTableId", &route_table), ("SubnetId", &subnet_id)],
        )
        .await;
        subnets.push(BuiltSubnet {
            id: subnet_id,
            zone,
            route_table,
            association: element(&associated, "associationId"),
            public: declared.public,
        });
    }

    let (status, written) = ssm_as(
        router,
        ACCOUNT_KEY,
        "PutParameter",
        &json!({ "Name": estate.parameter, "Value": estate.value, "Type": "String" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the parameter must be written, got {status} {written}"
    );

    Built {
        vpc_id,
        gateway_id,
        subnets,
    }
}

// ---------------------------------------------------------------------------
// What the providers resolve
// ---------------------------------------------------------------------------

/// One subnet of a resolved context: its id, the zone it reports and the route table it
/// reports as explicitly associated with it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ResolvedSubnet {
    id: String,
    zone: String,
    route_table: String,
}

/// What the two providers resolve for one estate, read out of the services themselves: the
/// VPC the tag filter selects with its block and owner, the Region's zones, the subnets
/// partitioned into groups by their route tables, and the parameter's value.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Context {
    vpc_id: String,
    vpc_cidr_block: String,
    owner_account_id: String,
    zones: Vec<String>,
    subnet_groups: BTreeMap<String, Vec<ResolvedSubnet>>,
    parameter_value: String,
}

/// One read of the session under test, asserted served.
async fn served(
    router: &MultiServiceRouter,
    case: &str,
    akid: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) = ec2_as(router, akid, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected {action} to be served for the lookup session, got {status} \
         {body}"
    );
    body
}

/// Drive both context providers as the lookup session `akid` and answer what they resolved.
///
/// This walks the reads a provider walks rather than reading the ids back out of the build:
/// the VPC is found by its `Name` tag, its subnets by `vpc-id`, each subnet's explicitly
/// associated route table by `association.subnet-id`, and each table's own routes by its id —
/// so the group a subnet lands in is computed from the estate the services hold.
async fn resolve_context(
    router: &MultiServiceRouter,
    case: &str,
    akid: &str,
    estate: &Estate,
) -> Context {
    let vpcs = served(
        router,
        case,
        akid,
        "DescribeVpcs",
        &[
            ("Filter.1.Name", &format!("tag:{NAME_TAG}")),
            ("Filter.1.Value.1", estate.name_tag),
        ],
    )
    .await;
    let found = elements(&vpcs, "vpcId");
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: expected exactly one VPC tagged {NAME_TAG}={:?}, got {found:?} from \
         {vpcs}",
        estate.name_tag
    );
    let vpc_id = found
        .first()
        .cloned()
        .expect("the length was just asserted to be one");
    let vpc_cidr_block = element(&vpcs, "cidrBlock");
    let owner_account_id = element(&vpcs, "ownerId");

    let zones = zone_names_as(router, case, akid).await;

    let subnets = served(
        router,
        case,
        akid,
        "DescribeSubnets",
        &[("Filter.1.Name", "vpc-id"), ("Filter.1.Value.1", &vpc_id)],
    )
    .await;

    let mut subnet_groups: BTreeMap<String, Vec<ResolvedSubnet>> = BTreeMap::new();
    for subnet_id in elements(&subnets, "subnetId") {
        let one = served(
            router,
            case,
            akid,
            "DescribeSubnets",
            &[("SubnetId.1", &subnet_id)],
        )
        .await;
        let zone = element(&one, "availabilityZone");

        let associated = served(
            router,
            case,
            akid,
            "DescribeRouteTables",
            &[
                ("Filter.1.Name", "association.subnet-id"),
                ("Filter.1.Value.1", &subnet_id),
            ],
        )
        .await;
        let tables: BTreeSet<String> = elements(&associated, "routeTableId").into_iter().collect();
        assert_eq!(
            tables.len(),
            1,
            "case {case:?}: expected the subnet {subnet_id} to report exactly one explicitly \
             associated route table, got {tables:?} from {associated}"
        );
        let route_table = tables
            .into_iter()
            .next()
            .expect("the length was just asserted to be one");

        let group = if egresses_to_the_internet(router, case, akid, &route_table).await {
            PUBLIC
        } else {
            ISOLATED
        };
        subnet_groups
            .entry(group.to_string())
            .or_default()
            .push(ResolvedSubnet {
                id: subnet_id,
                zone,
                route_table,
            });
    }
    for group in subnet_groups.values_mut() {
        group.sort();
    }

    let (status, read) = ssm_as(
        router,
        akid,
        SSM_READ,
        &json!({ "Name": estate.parameter }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the parameter read to be served for the lookup session, got \
         {status} {read}"
    );
    let parameter_value = read
        .pointer("/Parameter/Value")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    Context {
        vpc_id,
        vpc_cidr_block,
        owner_account_id,
        zones,
        subnet_groups,
        parameter_value,
    }
}

/// The Availability Zones the session `akid` reads out of the Region.
async fn zone_names_as(
    router: &MultiServiceRouter,
    case: &str,
    akid: &str,
) -> Vec<String> {
    let body = served(router, case, akid, "DescribeAvailabilityZones", &[]).await;
    elements(&body, "zoneName")
}

/// Does `route_table` carry a default route out through an internet gateway? That is what
/// makes the subnets on it public rather than isolated.
async fn egresses_to_the_internet(
    router: &MultiServiceRouter,
    case: &str,
    akid: &str,
    route_table: &str,
) -> bool {
    let body = served(
        router,
        case,
        akid,
        "DescribeRouteTables",
        &[("RouteTableId.1", route_table)],
    )
    .await;
    let routes = block_of(&body, "routeSet").unwrap_or_default();
    items_of(routes).into_iter().any(|route| {
        opt_element(route, "destinationCidrBlock").as_deref() == Some(ALL_IPV4)
            && opt_element(route, "gatewayId").is_some_and(|target| target.starts_with("igw-"))
    })
}

/// What the estate's own build says the context must be.
fn expected_context(estate: &Estate, built: &Built, zones: &[String], value: &str) -> Context {
    let mut subnet_groups: BTreeMap<String, Vec<ResolvedSubnet>> = BTreeMap::new();
    for subnet in &built.subnets {
        let group = if subnet.public { PUBLIC } else { ISOLATED };
        subnet_groups
            .entry(group.to_string())
            .or_default()
            .push(ResolvedSubnet {
                id: subnet.id.clone(),
                zone: subnet.zone.clone(),
                route_table: subnet.route_table.clone(),
            });
    }
    for group in subnet_groups.values_mut() {
        group.sort();
    }
    Context {
        vpc_id: built.vpc_id.clone(),
        vpc_cidr_block: estate.vpc_cidr.to_string(),
        owner_account_id: ACCOUNT.to_string(),
        zones: zones.to_vec(),
        subnet_groups,
        parameter_value: value.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Both providers resolve the estate the account holds
// ---------------------------------------------------------------------------

/// RULE: a lookup session the documents in force allow resolves BOTH providers out of the
/// account's own state — the VPC the tag filter selects with its stored block and owner, every
/// subnet of it reporting the zone and the explicitly associated route table it was created
/// with, partitioned into groups by whether that table egresses to the internet, and the
/// parameter's value as it was written.
#[tokio::test]
async fn both_context_providers_resolve_the_estate_through_one_lookup_role() {
    let router = build_router();
    let (akid, _) = lookup_session(
        &router,
        "a permitted lookup role",
        "cdk-lookup-role-permitted",
        &lookup_grants(),
    )
    .await;

    // Every row's estate is built first, so each lookup below selects out of an account that
    // really holds the other rows' VPCs and parameters too.
    let mut built = Vec::new();
    for estate in ESTATES {
        built.push(build_estate(&router, estate).await);
    }

    let zones = zone_names(&router, "the Region's zones").await;
    for (estate, built) in ESTATES.iter().zip(built.iter()) {
        let case = estate.case;
        let resolved = resolve_context(&router, case, &akid, estate).await;
        let expected = expected_context(estate, built, &zones, estate.value);
        assert_eq!(
            resolved, expected,
            "case {case:?}: expected the providers to resolve {expected:?}, got {resolved:?}"
        );
        // The estate's gateway is the one the public group's table egresses through, so a row
        // that declares no public subnet resolves no public group at all.
        let expects_public = estate.subnets.iter().any(|subnet| subnet.public);
        assert_eq!(
            resolved.subnet_groups.contains_key(PUBLIC),
            expects_public,
            "case {case:?}: expected a {PUBLIC} group == {expects_public} for the estate \
             behind {}, got {:?}",
            built.gateway_id,
            resolved.subnet_groups.keys().collect::<Vec<&String>>()
        );
        assert_eq!(
            built.route_tables().len(),
            resolved.subnet_groups.len(),
            "case {case:?}: expected one route table per resolved group, got {:?} for {:?}",
            built.route_tables(),
            resolved.subnet_groups.keys().collect::<Vec<&String>>()
        );
    }
}

// ---------------------------------------------------------------------------
// A statement about one provider's read decides that read alone
// ---------------------------------------------------------------------------

/// One row: the `Deny` stored on the lookup role under a live session, which of [`EC2_READS`]
/// it refuses, and whether it refuses the parameter read.
struct DenyRow {
    case: &'static str,
    role: &'static str,
    /// The `Action` element of the stored statement.
    deny_actions: &'static [&'static str],
    /// For each of [`EC2_READS`], in that order, whether the statement refuses it.
    refuses_ec2: [bool; 4],
    refuses_ssm: bool,
}

const DENY_ROWS: &[DenyRow] = &[
    DenyRow {
        case: "a deny of the VPC read the VPC provider starts from",
        role: "cdk-lookup-role-deny-vpcs",
        deny_actions: &["ec2:DescribeVpcs"],
        refuses_ec2: [false, false, false, true],
        refuses_ssm: false,
    },
    DenyRow {
        case: "a deny over the whole Describe family",
        role: "cdk-lookup-role-deny-describes",
        deny_actions: &["ec2:Describe*"],
        refuses_ec2: [true, true, true, true],
        refuses_ssm: false,
    },
    DenyRow {
        case: "a deny of the parameter read",
        role: "cdk-lookup-role-deny-parameter",
        deny_actions: &["ssm:GetParameter"],
        refuses_ec2: [false, false, false, false],
        refuses_ssm: true,
    },
    DenyRow {
        case: "a deny over every action of the parameter service",
        role: "cdk-lookup-role-deny-ssm",
        deny_actions: &["ssm:*"],
        refuses_ec2: [false, false, false, false],
        refuses_ssm: true,
    },
    DenyRow {
        case: "one statement denying a read of each provider",
        role: "cdk-lookup-role-deny-both",
        deny_actions: &["ec2:DescribeVpcs", "ssm:GetParameter"],
        refuses_ec2: [false, false, false, true],
        refuses_ssm: true,
    },
];

/// The name the row's statement is stored under.
const DENY_POLICY: &str = "deny-one-providers-read";

/// Assert what one Amazon EC2 read answers for the session under test.
async fn assert_ec2_read(
    router: &MultiServiceRouter,
    case: &str,
    akid: &str,
    principal: &str,
    read: &str,
    expected: Expect,
) {
    let (status, body) = ec2_as(router, akid, read, &[]).await;
    match expected {
        Expect::Served => assert_eq!(
            status, 200,
            "case {case:?} / {read}: expected the read to be served, got {status} {body}"
        ),
        Expect::ExplicitDeny => {
            let action = format!("ec2:{read}");
            let (code, message) = query_error(&body);
            assert_eq!(
                (status, code.as_str(), message.as_str()),
                (
                    403,
                    UNAUTHORIZED_OPERATION,
                    ec2_refusal(principal, &action).as_str()
                ),
                "case {case:?} / {read}: expected Amazon EC2's own refusal of the denied \
                 action, got {status} {body}"
            );
        }
    }
}

/// Assert what the parameter read answers for the session under test.
async fn assert_parameter_read(
    router: &MultiServiceRouter,
    case: &str,
    akid: &str,
    principal: &str,
    estate: &Estate,
    expected: Expect,
) {
    let (status, body) = ssm_as(
        router,
        akid,
        SSM_READ,
        &json!({ "Name": estate.parameter }),
    )
    .await;
    match expected {
        Expect::Served => assert_eq!(
            (
                status,
                body.pointer("/Parameter/Value").and_then(Value::as_str)
            ),
            (200, Some(estate.value)),
            "case {case:?} / {SSM_READ}: expected the written value to be reported, got \
             {status} {body}"
        ),
        Expect::ExplicitDeny => {
            let action = format!("ssm:{SSM_READ}");
            let resource = parameter_arn(estate.parameter);
            assert_eq!(
                (
                    status,
                    json_error_code(&body),
                    json_message(&body)
                ),
                (
                    400,
                    ACCESS_DENIED.to_string(),
                    ssm_refusal(principal, &action, &resource)
                ),
                "case {case:?} / {SSM_READ}: expected Systems Manager's own refusal of the \
                 denied action, got {status} {body}"
            );
        }
    }
}

/// RULE: each provider's read is decided on the ONE service action it performs, so a statement
/// stored on the lookup role under a live session refuses exactly the reads its `Action`
/// element matches — each with the owning service's own code and sentence — and leaves every
/// other read of either provider serving the estate; deleting the statement lifts the decision
/// again, and the context resolves to exactly what it did before.
#[tokio::test]
async fn a_deny_on_one_providers_read_leaves_the_other_provider_resolving() {
    let router = build_router();
    let estate = ESTATES
        .get(1)
        .expect("the table declares the multi-zone estate");
    let built = build_estate(&router, estate).await;
    let zones = zone_names(&router, "the Region's zones").await;
    let expected = expected_context(estate, &built, &zones, estate.value);

    for row in DENY_ROWS {
        let case = row.case;
        let (akid, principal) = lookup_session(&router, case, row.role, &lookup_grants()).await;

        // Before the statement is stored, both providers resolve the estate.
        let resolved = resolve_context(&router, case, &akid, estate).await;
        assert_eq!(
            resolved, expected,
            "case {case:?}: expected the permitted session to resolve {expected:?}, got \
             {resolved:?}"
        );

        put_role_policy(
            &router,
            case,
            row.role,
            DENY_POLICY,
            &statement("Deny", row.deny_actions, "*"),
        )
        .await;

        for (read, refused) in EC2_READS.iter().zip(row.refuses_ec2.iter()) {
            let expectation = if *refused {
                Expect::ExplicitDeny
            } else {
                Expect::Served
            };
            assert_ec2_read(&router, case, &akid, &principal, read, expectation).await;
        }
        let expectation = if row.refuses_ssm {
            Expect::ExplicitDeny
        } else {
            Expect::Served
        };
        assert_parameter_read(&router, case, &akid, &principal, estate, expectation).await;

        // The reads the statement does not name still resolve the estate: a refusal of one
        // provider is not an outage of the other.
        if !row.refuses_ec2[3] {
            let vpcs = served(
                &router,
                case,
                &akid,
                "DescribeVpcs",
                &[
                    ("Filter.1.Name", &format!("tag:{NAME_TAG}")),
                    ("Filter.1.Value.1", estate.name_tag),
                ],
            )
            .await;
            assert_eq!(
                elements(&vpcs, "vpcId"),
                vec![built.vpc_id.clone()],
                "case {case:?}: expected the undenied VPC read to still select {}, got {vpcs}",
                built.vpc_id
            );
        }

        delete_role_policy(&router, case, row.role, DENY_POLICY).await;

        // With the statement gone the session is back on the grant's side of every gate, and
        // what the providers resolve is what they resolved before the deny.
        let resolved = resolve_context(&router, case, &akid, estate).await;
        assert_eq!(
            resolved, expected,
            "case {case:?}: expected the context to resolve unchanged once the deny is \
             deleted, got {resolved:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// What a provider reads once the estate is gone
// ---------------------------------------------------------------------------

/// RULE: a provider's read reports the estate the services hold at the instant of the call, so
/// once the parameter and the VPC have been deleted the same reads answer the modelled absence
/// — `ParameterNotFound` for the name, `InvalidVpcID.NotFound` for the id, and no VPC at all
/// for the tag the lookup filters on — rather than the value they reported before.
#[tokio::test]
async fn the_reads_answer_a_modelled_absence_once_the_estate_is_gone() {
    let router = build_router();
    let estate = ESTATES
        .first()
        .expect("the table declares at least one estate");
    let case = estate.case;
    let built = build_estate(&router, estate).await;
    let (akid, _) = lookup_session(
        &router,
        case,
        "cdk-lookup-role-teardown",
        &lookup_grants(),
    )
    .await;

    // The context resolves while the estate stands.
    let resolved = resolve_context(&router, case, &akid, estate).await;
    assert_eq!(
        resolved.vpc_id, built.vpc_id,
        "case {case:?}: expected the lookup to select {}, got {resolved:?}",
        built.vpc_id
    );

    // The parameter goes first: the read that reported its value now reports the absence.
    let (status, deleted) = ssm_as(
        &router,
        ACCOUNT_KEY,
        "DeleteParameter",
        &json!({ "Name": estate.parameter }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the parameter must be deletable, got {status} {deleted}"
    );
    let (status, read) = ssm_as(
        &router,
        &akid,
        SSM_READ,
        &json!({ "Name": estate.parameter }),
    )
    .await;
    assert_eq!(
        (status, json_error_code(&read).as_str()),
        (400, "ParameterNotFound"),
        "case {case:?}: expected the deleted parameter to be reported absent, got {status} \
         {read}"
    );

    // Then the network, taken down the way it was built up.
    for subnet in &built.subnets {
        seed(
            &router,
            case,
            "DisassociateRouteTable",
            &[("AssociationId", &subnet.association)],
        )
        .await;
        seed(
            &router,
            case,
            "DeleteSubnet",
            &[("SubnetId", &subnet.id)],
        )
        .await;
    }
    for table in built.route_tables() {
        seed(
            &router,
            case,
            "DeleteRouteTable",
            &[("RouteTableId", &table)],
        )
        .await;
    }
    seed(
        &router,
        case,
        "DetachInternetGateway",
        &[
            ("InternetGatewayId", &built.gateway_id),
            ("VpcId", &built.vpc_id),
        ],
    )
    .await;
    seed(
        &router,
        case,
        "DeleteInternetGateway",
        &[("InternetGatewayId", &built.gateway_id)],
    )
    .await;
    seed(&router, case, "DeleteVpc", &[("VpcId", &built.vpc_id)]).await;

    // The id the lookup resolved to names no VPC any more...
    let (status, body) = ec2_as(
        &router,
        &akid,
        "DescribeVpcs",
        &[("VpcId.1", &built.vpc_id)],
    )
    .await;
    let (code, _) = query_error(&body);
    assert_eq!(
        (status, code.as_str()),
        (400, "InvalidVpcID.NotFound"),
        "case {case:?}: expected the deleted VPC's id to be refused, got {status} {body}"
    );

    // ...and the tag the lookup filters on selects nothing, which is the answer a provider
    // reads as "no VPC matches" rather than a stale one.
    let vpcs = served(
        &router,
        case,
        &akid,
        "DescribeVpcs",
        &[
            ("Filter.1.Name", &format!("tag:{NAME_TAG}")),
            ("Filter.1.Value.1", estate.name_tag),
        ],
    )
    .await;
    assert!(
        elements(&vpcs, "vpcId").is_empty(),
        "case {case:?}: expected the tag filter to select no VPC once the estate is gone, got \
         {vpcs}"
    );
}
