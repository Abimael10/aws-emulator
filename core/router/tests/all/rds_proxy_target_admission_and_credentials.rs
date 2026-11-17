// SPDX-License-Identifier: CC0-1.0

//! What an RDS proxy ADMITS, and what the health of its targets reports, when the network
//! and the credentials it is given are resolved against the services that own them.
//!
//! Three rules, each asserted over a table spanning what it generalises over:
//!
//! * A proxy is placed IN a VPC: `CreateDBProxy$VpcSubnetIds` are "one or more VPC subnet
//!   IDs to associate with the new proxy", and a set whose subnets lie in several VPCs names
//!   no VPC to place it in, so the create is refused as a parameter combination before
//!   anything is written — `DescribeDBProxies` then answers `DBProxyNotFoundFault` for that
//!   name — while a set out of ONE VPC is accepted and the proxy reports that VPC as its
//!   `VpcId`.
//! * A proxy's TARGET REGISTRY is served while the proxy is still coming up: the proxy
//!   exists from the moment the create answers, so `RegisterDBProxyTargets` and
//!   `DeregisterDBProxyTargets` are served on a proxy reporting `creating`, and what reports
//!   the proxy's readiness is the TARGET's own health lifecycle — "`registering` >
//!   `unavailable` > `available`" (`TargetHealth$State`) — which cannot reach `AVAILABLE`
//!   before the proxy itself does.
//! * A proxy logs in to a registered database with the credentials in the Secrets Manager
//!   secret its `Auth` names, read with the IAM role `RoleArn` names
//!   ("the Amazon Resource Name (ARN) of the IAM role that the proxy uses to access secrets
//!   in Amazon Web Services Secrets Manager"), and `secretsmanager:GetSecretValue` is what
//!   that read requires ("Required permissions: secretsmanager:GetSecretValue",
//!   `GetSecretValue`): a proxy whose role no document in force entitles to that secret has
//!   no credentials to present, so its target is `UNAVAILABLE` for the reason `AUTH_FAILURE`
//!   (`TargetHealthReason`) and never becomes `AVAILABLE`, and a grant put in force
//!   afterwards heals it.
//!
//! The suite lives beside the router because three other crates own the answers: Amazon EC2
//! mints the VPCs and the subnets the create resolves, IAM holds the roles and the inline
//! documents in force on them, and Secrets Manager holds the secret the proxy's `Auth`
//! names. A crate-local suite can only ever see one side of it.
//!
//! Sources: `smithy_query(query="operation",
//! operation="CreateDBProxy,RegisterDBProxyTargets,DescribeDBProxyTargets")` and
//! `smithy_query(query="shape", shape="TargetHealth,TargetState,TargetHealthReason")` on the
//! rds model.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, elements, form_encode, members_of, opt_element};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The API versions the three awsQuery front doors here take.
const RDS_API_VERSION: &str = "2014-10-31";
const EC2_API_VERSION: &str = "2016-11-15";
const IAM_API_VERSION: &str = "2010-05-08";

/// The IAM action reading a secret's value requires, which a grant names and a withheld
/// grant is the absence of.
const READ_ACTION: &str = "secretsmanager:GetSecretValue";

/// The one inline policy name every role below carries, which a rewrite replaces.
const POLICY_NAME: &str = "inline";

/// `TargetHealth$State` values this suite asserts on.
const TARGET_STATE_REGISTERING: &str = "REGISTERING";
const TARGET_STATE_AVAILABLE: &str = "AVAILABLE";
const TARGET_STATE_UNAVAILABLE: &str = "UNAVAILABLE";

/// `TargetHealthReason` — the reason a proxy that cannot read its credentials reports.
const TARGET_HEALTH_REASON_AUTH_FAILURE: &str = "AUTH_FAILURE";

/// The fixed text a `VpcSubnetIds` set spanning several VPCs is refused with, and the error
/// code that refusal carries.
const SEVERAL_VPCS_PREFIX: &str = "Subnets must belong to only one VPC but found multiple VPCs: ";
const INVALID_PARAMETER_COMBINATION: &str = "InvalidParameterCombination";

/// How often a poll looks again, and how long it keeps looking: the transitions this suite
/// waits for are owed by the CLOCK, so every wait is for the condition under a deadline
/// generous enough that a loaded host cannot decide the outcome.
const LOOK: Duration = Duration::from_millis(50);
const DEADLINE: Duration = Duration::from_secs(60);

/// The emulator as it is ASSEMBLED, on one engine: the hops Amazon RDS makes to EC2, to IAM
/// and to Secrets Manager are dispatched back into this same router, so every id a request
/// carries is one those services handed out.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One awsQuery call of `service`, answered with the HTTP status and the XML body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One Amazon RDS operation over its awsQuery front door.
async fn rds(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(router, "rds", RDS_API_VERSION, action, members).await
}

/// One Amazon RDS operation that must be served, answered with its XML body.
async fn rds_ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = rds(router, action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with {body}"
    );
    body
}

/// One ec2Query action that must succeed, answered with its XML body.
async fn ec2(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> String {
    let (status, body) = query(router, "ec2", EC2_API_VERSION, action, members).await;
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with {body}"
    );
    body
}

/// One Secrets Manager operation over its awsJson1_1 wire.
async fn secretsmanager(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("secretsmanager.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("authorization", authorization("secretsmanager"))
        .header("x-amz-target", format!("secretsmanager.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// One VPC of the account with the subnets EC2 minted in it.
struct Vpc {
    id: String,
    subnets: Vec<String>,
}

/// Create one VPC carrying `count` subnets, through EC2's own operations.
async fn ec2_vpc(router: &MultiServiceRouter, second_octet: u8, count: usize) -> Vpc {
    let cidr = format!("10.{second_octet}.0.0/16");
    let id = element(&ec2(router, "CreateVpc", &[("CidrBlock", &cidr)]).await, "vpcId");
    let zones = ["us-west-2a", "us-west-2b", "us-west-2c"];
    let mut subnets = Vec::new();
    for index in 0..count {
        let subnet_cidr = format!("10.{second_octet}.{}.0/24", index + 1);
        subnets.push(element(
            &ec2(
                router,
                "CreateSubnet",
                &[
                    ("VpcId", id.as_str()),
                    ("CidrBlock", subnet_cidr.as_str()),
                    ("AvailabilityZone", zones[index % zones.len()]),
                ],
            )
            .await,
            "subnetId",
        ));
    }
    Vpc { id, subnets }
}

/// The trust policy a proxy's role carries: Amazon RDS assumes it to read the secret.
fn trust_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "rds.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// A document granting the secret read on `resources`, beside a statement that grants
/// nothing of Secrets Manager at all.
fn document_allowing(resources: &[&str]) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {"Effect": "Allow", "Action": ["logs:PutLogEvents"], "Resource": "*"},
            {"Effect": "Allow", "Action": READ_ACTION, "Resource": resources},
        ],
    })
    .to_string()
}

/// A document granting nothing of Secrets Manager.
fn document_allowing_nothing() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {"Effect": "Allow", "Action": ["logs:PutLogEvents"], "Resource": "*"},
        ],
    })
    .to_string()
}

/// Create a role carrying `document` as its one inline policy, and answer the ARN IAM
/// minted for it.
async fn create_role(router: &MultiServiceRouter, name: &str, document: &str) -> String {
    let (status, created) = query(
        router,
        "iam",
        IAM_API_VERSION,
        "CreateRole",
        &[
            ("RoleName", name),
            ("AssumeRolePolicyDocument", &trust_policy()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateRole({name:?}) must succeed, got {status} with {created}"
    );
    let arn = opt_element(&created, "Arn")
        .unwrap_or_else(|| panic!("setup: CreateRole({name:?}) must answer an ARN, got {created}"));
    put_role_policy(router, name, document).await;
    arn
}

/// Put `document` in force as the one inline policy of `role`.
async fn put_role_policy(router: &MultiServiceRouter, role: &str, document: &str) {
    let (status, answered) = query(
        router,
        "iam",
        IAM_API_VERSION,
        "PutRolePolicy",
        &[
            ("RoleName", role),
            ("PolicyName", POLICY_NAME),
            ("PolicyDocument", document),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: PutRolePolicy({role:?}) must succeed, got {status} with {answered}"
    );
}

/// How many characters a `ClientRequestToken` carries: inside the published 32..64 bound of
/// `ClientRequestTokenType` (`smithy_query(query="shape", shape="ClientRequestTokenType")`),
/// which an SDK would generate for a caller that writes none.
const VERSION_TOKEN_LEN: usize = 40;

/// Create one secret and answer the ARN Secrets Manager minted for it.
async fn create_secret(router: &MultiServiceRouter, name: &str) -> String {
    let mut token = format!("{name}-version-");
    token.truncate(VERSION_TOKEN_LEN);
    while token.len() < VERSION_TOKEN_LEN {
        token.push('0');
    }
    let (status, created) = secretsmanager(
        router,
        "CreateSecret",
        &json!({
            "Name": name,
            "SecretString": json!({"username": "admin", "password": "p-0123456789"}).to_string(),
            "ClientRequestToken": token,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateSecret({name:?}) must succeed, got {status} with {created}"
    );
    created["ARN"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: CreateSecret({name:?}) must answer an ARN, got {created}"))
        .to_string()
}

/// Create a DB instance and wait until it reports `available`, so a registration names a
/// database this service holds a ready record of.
async fn create_available_instance(router: &MultiServiceRouter, identifier: &str) {
    rds_ok(
        router,
        &format!("create {identifier}"),
        "CreateDBInstance",
        &[
            ("DBInstanceIdentifier", identifier),
            ("Engine", "mysql"),
            ("DBInstanceClass", "db.m5.large"),
        ],
    )
    .await;
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = rds_ok(
            router,
            identifier,
            "DescribeDBInstances",
            &[("DBInstanceIdentifier", identifier)],
        )
        .await;
        if element(&body, "DBInstanceStatus") == "available" {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "setup: {identifier} must reach available, last answer {body}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// The `VpcSubnetIds.member.N` parameters naming `subnets`.
fn subnet_members(subnets: &[String]) -> Vec<(String, String)> {
    subnets
        .iter()
        .enumerate()
        .map(|(index, subnet)| {
            (
                format!("VpcSubnetIds.member.{}", index + 1),
                subnet.clone(),
            )
        })
        .collect()
}

/// `CreateDBProxy` naming `subnets`, `role_arn` and — where one is given — the `Auth` entry
/// that names the secret the proxy authenticates with.
async fn create_proxy(
    router: &MultiServiceRouter,
    name: &str,
    subnets: &[String],
    role_arn: &str,
    secret_arn: Option<&str>,
) -> (u16, String) {
    let owned = subnet_members(subnets);
    let mut members: Vec<(&str, &str)> = vec![
        ("DBProxyName", name),
        ("EngineFamily", "MYSQL"),
        ("RoleArn", role_arn),
    ];
    for (key, value) in &owned {
        members.push((key.as_str(), value.as_str()));
    }
    if let Some(secret_arn) = secret_arn {
        members.push(("Auth.member.1.SecretArn", secret_arn));
        members.push(("Auth.member.1.AuthScheme", "SECRETS"));
        members.push(("Auth.member.1.IAMAuth", "DISABLED"));
    }
    rds(router, "CreateDBProxy", &members).await
}

/// The target `DescribeDBProxyTargets` reports for `identifier`, as the `<member>` block
/// carrying it, or `None` when the report holds no such target.
async fn target_of(router: &MultiServiceRouter, proxy: &str, identifier: &str) -> Option<String> {
    let body = rds_ok(
        router,
        proxy,
        "DescribeDBProxyTargets",
        &[("DBProxyName", proxy)],
    )
    .await;
    members_of(&body)
        .into_iter()
        .find(|member| opt_element(member, "RdsResourceId").as_deref() == Some(identifier))
        .map(ToString::to_string)
}

/// The health state the target for `identifier` reports now.
async fn target_state(router: &MultiServiceRouter, proxy: &str, identifier: &str) -> String {
    target_of(router, proxy, identifier)
        .await
        .map(|member| element(&member, "State"))
        .unwrap_or_default()
}

/// Poll the proxy's target report until the target for `identifier` reports `AVAILABLE`,
/// answering the `<member>` block it reported it in.
///
/// The move out of `registering` is owed by the CLOCK, so this waits for that condition
/// under a generous deadline rather than for a fixed span.
async fn await_available(
    router: &MultiServiceRouter,
    case: &str,
    proxy: &str,
    identifier: &str,
) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        if let Some(member) = target_of(router, proxy, identifier).await {
            if element(&member, "State") == TARGET_STATE_AVAILABLE {
                return member;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected the target for {identifier} on {proxy} to reach \
                 {TARGET_STATE_AVAILABLE}, last reported {member}"
            );
        } else {
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected a target for {identifier} on {proxy}, the report has \
                 none"
            );
        }
        tokio::time::sleep(LOOK).await;
    }
}

#[tokio::test]
async fn a_proxy_subnet_set_spanning_several_vpcs_is_refused_and_creates_nothing() {
    let router = build_router();
    let first = ec2_vpc(&router, 10, 2).await;
    let second = ec2_vpc(&router, 11, 2).await;
    let third = ec2_vpc(&router, 12, 1).await;
    let role = create_role(&router, "emu-xsvc-proxy-vpcs", &document_allowing_nothing()).await;

    /// One row: the subnets a create names, and the VPCs those subnets lie in.
    struct Row {
        case: &'static str,
        /// Which (vpc, subnet) pairs the request names, by index into the VPCs above.
        picks: &'static [(usize, usize)],
        /// How many distinct VPCs those picks lie in.
        vpcs: usize,
    }

    // The rows differ along what the rule generalises over: how many subnets the request
    // names, how many VPCs they lie in, WHERE in the list the subnet of another VPC sits,
    // and whether the one-VPC case names one subnet or several.
    const ROWS: &[Row] = &[
        Row {
            case: "one subnet of one VPC",
            picks: &[(0, 0)],
            vpcs: 1,
        },
        Row {
            case: "two subnets of one VPC",
            picks: &[(0, 0), (0, 1)],
            vpcs: 1,
        },
        Row {
            case: "a subnet of another VPC last",
            picks: &[(0, 0), (1, 0)],
            vpcs: 2,
        },
        Row {
            case: "a subnet of another VPC first",
            picks: &[(1, 1), (0, 1)],
            vpcs: 2,
        },
        Row {
            case: "subnets of three VPCs",
            picks: &[(0, 0), (1, 0), (2, 0)],
            vpcs: 3,
        },
    ];

    let vpcs = [&first, &second, &third];
    for (index, row) in ROWS.iter().enumerate() {
        let subnets: Vec<String> = row
            .picks
            .iter()
            .map(|(vpc, subnet)| vpcs[*vpc].subnets[*subnet].clone())
            .collect();
        let named: Vec<String> = row
            .picks
            .iter()
            .map(|(vpc, _)| vpcs[*vpc].id.clone())
            .collect();
        let name = format!("emu-xsvc-proxy-{index}");
        let (status, body) = create_proxy(&router, &name, &subnets, &role, None).await;
        if row.vpcs == 1 {
            assert_eq!(
                status, 200,
                "case {:?}: a set out of one VPC must be accepted, got {status} with {body}",
                row.case
            );
            // The proxy reports the VPC its subnets belong to, which is the one EC2 minted
            // them in.
            let reported = element(&body, "VpcId");
            assert_eq!(
                reported, named[0],
                "case {:?}: expected VpcId {:?}, got {reported:?} in {body}",
                row.case, named[0]
            );
            continue;
        }
        assert_eq!(
            status, 400,
            "case {:?}: a set spanning {} VPCs must be refused, got {status} with {body}",
            row.case, row.vpcs
        );
        let code = element(&body, "Code");
        assert_eq!(
            code, INVALID_PARAMETER_COMBINATION,
            "case {:?}: expected the error code {INVALID_PARAMETER_COMBINATION}, got {code:?} \
             in {body}",
            row.case
        );
        let message = element(&body, "Message");
        assert!(
            message.starts_with(SEVERAL_VPCS_PREFIX),
            "case {:?}: expected a message opening {SEVERAL_VPCS_PREFIX:?}, got {message:?}",
            row.case
        );
        for vpc_id in &named {
            assert!(
                message.contains(vpc_id),
                "case {:?}: expected the message to name the VPC {vpc_id:?}, got {message:?}",
                row.case
            );
        }
        // Nothing was written: the name resolves no proxy at all.
        let (status, read) = rds(&router, "DescribeDBProxies", &[("DBProxyName", &name)]).await;
        assert_eq!(
            status, 404,
            "case {:?}: expected the refused name to resolve no proxy, got {status} with {read}",
            row.case
        );
        let code = element(&read, "Code");
        assert_eq!(
            code, "DBProxyNotFoundFault",
            "case {:?}: expected DBProxyNotFoundFault for the refused name, got {code:?} in \
             {read}",
            row.case
        );
    }
}

#[tokio::test]
async fn a_proxy_takes_a_registration_while_it_is_still_coming_up() {
    let router = build_router();
    let network = ec2_vpc(&router, 20, 2).await;
    let role = create_role(&router, "emu-xsvc-proxy-early", &document_allowing_nothing()).await;
    create_available_instance(&router, "emu-xsvc-early-db").await;

    let created = create_proxy(&router, "emu-xsvc-early-proxy", &network.subnets, &role, None).await;
    assert_eq!(
        created.0, 200,
        "the create must be served, got {} with {}",
        created.0, created.1
    );
    assert_eq!(
        element(&created.1, "Status"),
        "creating",
        "a new proxy is not ready to handle requests yet: {}",
        created.1
    );

    // The registry of a proxy that is still coming up is served: the proxy exists, and what
    // reports that it is not ready is the TARGET's health.
    let registered = rds_ok(
        &router,
        "a registration made while the proxy is creating",
        "RegisterDBProxyTargets",
        &[
            ("DBProxyName", "emu-xsvc-early-proxy"),
            ("DBInstanceIdentifiers.member.1", "emu-xsvc-early-db"),
        ],
    )
    .await;
    assert_eq!(
        elements(&registered, "RdsResourceId"),
        vec!["emu-xsvc-early-db".to_string()],
        "the target reports the identifier the registration named: {registered}"
    );
    assert_eq!(
        element(&registered, "State"),
        TARGET_STATE_REGISTERING,
        "a registration made before the proxy is ready is not in force yet: {registered}"
    );

    // And it comes into force on its own, once the proxy itself is available.
    await_available(
        &router,
        "a registration made while the proxy is creating",
        "emu-xsvc-early-proxy",
        "emu-xsvc-early-db",
    )
    .await;
    let proxy = rds_ok(
        &router,
        "the proxy",
        "DescribeDBProxies",
        &[("DBProxyName", "emu-xsvc-early-proxy")],
    )
    .await;
    assert_eq!(
        element(&proxy, "Status"),
        "available",
        "a target cannot be available before the proxy that pools its connections is: {proxy}"
    );

    // The deregistration is served on a coming-up proxy for the same reason: a second proxy
    // is created and its registration taken straight back off.
    let second = create_proxy(
        &router,
        "emu-xsvc-early-proxy-two",
        &network.subnets,
        &role,
        None,
    )
    .await;
    assert_eq!(
        second.0, 200,
        "the second create must be served, got {} with {}",
        second.0, second.1
    );
    for action in ["RegisterDBProxyTargets", "DeregisterDBProxyTargets"] {
        rds_ok(
            &router,
            &format!("{action} while the proxy is creating"),
            action,
            &[
                ("DBProxyName", "emu-xsvc-early-proxy-two"),
                ("DBInstanceIdentifiers.member.1", "emu-xsvc-early-db"),
            ],
        )
        .await;
    }
    assert_eq!(
        target_state(&router, "emu-xsvc-early-proxy-two", "emu-xsvc-early-db").await,
        "",
        "the deregistration took the registration back off"
    );
}

#[tokio::test]
async fn a_target_is_unavailable_while_the_proxy_may_not_read_its_auth_secret() {
    let router = build_router();
    let network = ec2_vpc(&router, 30, 2).await;
    create_available_instance(&router, "emu-xsvc-auth-db").await;
    let secret = create_secret(&router, "emu-xsvc-auth-secret").await;
    let other_secret = create_secret(&router, "emu-xsvc-auth-other").await;

    /// One row: what the proxy's role is granted, and whether that entitles it to the
    /// secret the proxy's `Auth` names.
    struct Row {
        case: &'static str,
        /// The role's one inline document, as a function of the two secret ARNs.
        document: fn(&str, &str) -> String,
        /// Whether the document entitles the role to the secret the proxy authenticates
        /// with.
        reads_the_secret: bool,
    }

    // The rows differ along what the rule generalises over: a document that grants the read
    // on exactly that secret, one that grants it on ANOTHER secret, one that grants the read
    // nowhere, and one that grants it on every resource.
    const ROWS: &[Row] = &[
        Row {
            case: "the role is granted the read on that secret",
            document: |secret, _| document_allowing(&[secret]),
            reads_the_secret: true,
        },
        Row {
            case: "the role is granted the read on every resource",
            document: |_, _| document_allowing(&["*"]),
            reads_the_secret: true,
        },
        Row {
            case: "the role is granted the read on another secret only",
            document: |_, other| document_allowing(&[other]),
            reads_the_secret: false,
        },
        Row {
            case: "the role is granted nothing of Secrets Manager",
            document: |_, _| document_allowing_nothing(),
            reads_the_secret: false,
        },
    ];

    for (index, row) in ROWS.iter().enumerate() {
        let role_name = format!("emu-xsvc-auth-role-{index}");
        let proxy = format!("emu-xsvc-auth-proxy-{index}");
        let role = create_role(
            &router,
            &role_name,
            &(row.document)(&secret, &other_secret),
        )
        .await;
        let created = create_proxy(&router, &proxy, &network.subnets, &role, Some(&secret)).await;
        assert_eq!(
            created.0, 200,
            "case {:?}: the create must be served, got {} with {}",
            row.case, created.0, created.1
        );
        rds_ok(
            &router,
            row.case,
            "RegisterDBProxyTargets",
            &[
                ("DBProxyName", proxy.as_str()),
                ("DBInstanceIdentifiers.member.1", "emu-xsvc-auth-db"),
            ],
        )
        .await;
        if row.reads_the_secret {
            await_available(&router, row.case, &proxy, "emu-xsvc-auth-db").await;
            continue;
        }

        // The target of a proxy that cannot read its credentials does not become healthy.
        // What proves the span it would have taken has passed is the CONDITION the granted
        // rows wait for: a proxy created and registered in this same suite reaches
        // `AVAILABLE`, and only then is this target read.
        let witness = format!("emu-xsvc-auth-witness-{index}");
        let witness_role = create_role(
            &router,
            &format!("emu-xsvc-auth-witness-role-{index}"),
            &document_allowing(&[&secret]),
        )
        .await;
        let created = create_proxy(
            &router,
            &witness,
            &network.subnets,
            &witness_role,
            Some(&secret),
        )
        .await;
        assert_eq!(
            created.0, 200,
            "case {:?}: the witness create must be served, got {} with {}",
            row.case, created.0, created.1
        );
        rds_ok(
            &router,
            row.case,
            "RegisterDBProxyTargets",
            &[
                ("DBProxyName", witness.as_str()),
                ("DBInstanceIdentifiers.member.1", "emu-xsvc-auth-db"),
            ],
        )
        .await;
        await_available(&router, row.case, &witness, "emu-xsvc-auth-db").await;

        let member = target_of(&router, &proxy, "emu-xsvc-auth-db")
            .await
            .unwrap_or_else(|| panic!("case {:?}: the registration must be reported", row.case));
        let state = element(&member, "State");
        assert_eq!(
            state, TARGET_STATE_UNAVAILABLE,
            "case {:?}: expected {TARGET_STATE_UNAVAILABLE}, got {state:?} in {member}",
            row.case
        );
        let reason = element(&member, "Reason");
        assert_eq!(
            reason, TARGET_HEALTH_REASON_AUTH_FAILURE,
            "case {:?}: expected the reason {TARGET_HEALTH_REASON_AUTH_FAILURE}, got {reason:?} \
             in {member}",
            row.case
        );
        let description = element(&member, "Description");
        assert!(
            description.contains(&secret),
            "case {:?}: expected the description to name the secret {secret:?}, got \
             {description:?}",
            row.case
        );

        // A grant put in force afterwards heals the target: the health is the permissions in
        // force NOW, not something recorded when the registration was made.
        put_role_policy(&router, &role_name, &document_allowing(&[&secret])).await;
        await_available(&router, row.case, &proxy, "emu-xsvc-auth-db").await;
    }
}
