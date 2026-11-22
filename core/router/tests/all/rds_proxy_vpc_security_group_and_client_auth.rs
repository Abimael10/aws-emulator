// SPDX-License-Identifier: CC0-1.0

//! Where an RDS proxy is PLACED and how its clients AUTHENTICATE, as the proxy reports them
//! once the services that own those facts have answered.
//!
//! Two rules, each asserted over a table spanning what it generalises over:
//!
//! * A resource Amazon RDS places in a VPC belongs to at least one VPC security group: a
//!   create that names `VpcSecurityGroupIds` is placed in exactly those groups, and one that
//!   names NONE is placed in that VPC's default security group — "your VPC automatically
//!   comes with a default security group" (Amazon VPC User Guide) — so `DBProxy$VpcId` and
//!   `DBProxy$VpcSecurityGroupIds` report the VPC and the groups the owner of that network
//!   minted, never an empty membership.
//! * `UserAuthConfig$ClientPasswordAuthType` is "the type of authentication the proxy uses
//!   for connections from clients", and a proxy authenticates its clients whether or not the
//!   request spelled the type out: an `Auth` entry that states none reports the default its
//!   `EngineFamily` publishes ("RDS for MySQL: `MYSQL_CACHING_SHA2_PASSWORD`; RDS for SQL
//!   Server: `SQL_SERVER_AUTHENTICATION`; RDS for PostgreSQL: `POSTGRES_SCRAM_SHA2_256`",
//!   spelled on the wire as the `ClientPasswordAuthType` enum publishes it), and one that
//!   states a type reports that type — on the create and on the modification alike.
//!
//! The suite lives beside the router because the network belongs to another crate: Amazon EC2
//! mints the VPC, its subnets and its security groups — including the default group a VPC
//! comes with — and IAM holds the role the proxy reads its secrets with. A crate-local suite
//! can only ever see the RDS side, where no EC2 answer is available at all.
//!
//! Sources: `smithy_query(query="operation", operation="CreateDBProxy,ModifyDBProxy")` and
//! `smithy_query(query="shape", shape="DBProxy,UserAuthConfig,ClientPasswordAuthType")` on
//! the rds model.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{block_of, element, elements, form_encode, members_of, opt_element};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The API versions the three awsQuery front doors here take.
const RDS_API_VERSION: &str = "2014-10-31";
const EC2_API_VERSION: &str = "2016-11-15";
const IAM_API_VERSION: &str = "2010-05-08";

/// `DBProxyStatus` — the status a proxy a modification is accepted on has to be in.
const DB_PROXY_STATUS_AVAILABLE: &str = "available";

/// How often a poll looks again, and how long it keeps looking: a proxy reaches `available`
/// when the CLOCK makes it due, so the wait is for that condition under a deadline generous
/// enough that a loaded host cannot decide the outcome.
const LOOK: Duration = Duration::from_millis(50);
const DEADLINE: Duration = Duration::from_secs(60);

/// The emulator as it is ASSEMBLED, on one engine: the hops Amazon RDS makes to EC2 are
/// dispatched back into this same router, so every id a request carries is one EC2 handed
/// out.
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

/// One Amazon RDS operation that must be served, answered with its XML body.
async fn rds_ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = query(router, "rds", RDS_API_VERSION, action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with {body}"
    );
    body
}

/// One Amazon EC2 action that must be served, answered with its XML body.
async fn ec2(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> String {
    let (status, body) = query(router, "ec2", EC2_API_VERSION, action, members).await;
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with {body}"
    );
    body
}

/// One VPC of the account, its subnets, and the DEFAULT security group EC2 created with it.
struct Network {
    vpc_id: String,
    subnets: Vec<String>,
    /// The id of the group named `default` in this VPC, read back from the service that owns
    /// security groups rather than built here.
    default_security_group: String,
}

/// Create one VPC carrying `count` subnets, and read the default security group EC2 gave it.
async fn network(router: &MultiServiceRouter, second_octet: u8, count: usize) -> Network {
    let cidr = format!("10.{second_octet}.0.0/16");
    let vpc_id = element(&ec2(router, "CreateVpc", &[("CidrBlock", &cidr)]).await, "vpcId");
    let zones = ["us-west-2a", "us-west-2b", "us-west-2c"];
    let mut subnets = Vec::new();
    for index in 0..count {
        let subnet_cidr = format!("10.{second_octet}.{}.0/24", index + 1);
        subnets.push(element(
            &ec2(
                router,
                "CreateSubnet",
                &[
                    ("VpcId", vpc_id.as_str()),
                    ("CidrBlock", subnet_cidr.as_str()),
                    ("AvailabilityZone", zones[index % zones.len()]),
                ],
            )
            .await,
            "subnetId",
        ));
    }
    let groups = ec2(
        router,
        "DescribeSecurityGroups",
        &[
            ("Filter.1.Name", "vpc-id"),
            ("Filter.1.Value.1", vpc_id.as_str()),
            ("Filter.2.Name", "group-name"),
            ("Filter.2.Value.1", "default"),
        ],
    )
    .await;
    let default_security_group = element(&groups, "groupId");
    Network {
        vpc_id,
        subnets,
        default_security_group,
    }
}

/// Create one security group of `vpc_id` and answer the id EC2 minted for it.
async fn security_group(router: &MultiServiceRouter, vpc_id: &str, name: &str) -> String {
    element(
        &ec2(
            router,
            "CreateSecurityGroup",
            &[
                ("GroupName", name),
                ("GroupDescription", "a group a proxy is placed in"),
                ("VpcId", vpc_id),
            ],
        )
        .await,
        "groupId",
    )
}

/// Create the IAM role a proxy reads its secrets with, and answer its ARN.
async fn proxy_role(router: &MultiServiceRouter, name: &str) -> String {
    let trust = serde_json::json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "rds.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let (status, created) = query(
        router,
        "iam",
        IAM_API_VERSION,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateRole({name:?}) must succeed, got {status} with {created}"
    );
    element(&created, "Arn")
}

/// The `DescribeDBProxies` answer for `name`, asserted to report that one proxy.
///
/// The whole body is the scope every read below works in: the request names ONE proxy, so
/// the report carries one `DBProxy` and each member read from it is that proxy's.
async fn described_proxy(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let body = rds_ok(router, case, "DescribeDBProxies", &[("DBProxyName", name)]).await;
    let reported = elements(&body, "DBProxyName");
    assert_eq!(
        reported,
        vec![name.to_string()],
        "case {case:?}: expected the report to carry exactly the proxy {name}, got {body}"
    );
    body
}

/// The `VpcSecurityGroupIds` one proxy reports, in reported order.
fn reported_security_groups(proxy: &str) -> Vec<String> {
    block_of(proxy, "VpcSecurityGroupIds").map_or_else(Vec::new, |list| elements(list, "member"))
}

/// The `ClientPasswordAuthType` of each `Auth` entry one proxy reports, in reported order.
fn reported_client_auth_types(proxy: &str) -> Vec<Option<String>> {
    block_of(proxy, "Auth").map_or_else(Vec::new, |list| {
        members_of(list)
            .into_iter()
            .map(|entry| opt_element(entry, "ClientPasswordAuthType"))
            .collect()
    })
}

/// `VpcSubnetIds.member.N=<subnet>` for each of `subnets`.
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

/// Poll `DescribeDBProxies` until the proxy reports `available`, which a modification needs.
async fn await_available_proxy(router: &MultiServiceRouter, case: &str, name: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let proxy = described_proxy(router, case, name).await;
        if element(&proxy, "Status") == DB_PROXY_STATUS_AVAILABLE {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {name} to reach {DB_PROXY_STATUS_AVAILABLE}, last \
             reported {proxy}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

#[tokio::test]
async fn a_proxy_reports_the_vpc_and_the_security_groups_it_is_placed_in() {
    let router = build_router();
    let net = network(&router, 20, 2).await;
    let role = proxy_role(&router, "emu-xsvc-proxy-placement").await;
    let first = security_group(&router, &net.vpc_id, "emu-proxy-group-one").await;
    let second = security_group(&router, &net.vpc_id, "emu-proxy-group-two").await;

    /// One row: how many groups the create names, and how many subnets it is placed over.
    struct Row {
        case: &'static str,
        name: &'static str,
        /// Which of the groups created above the request names, by index.
        names_groups: &'static [usize],
        /// How many of the VPC's subnets the create names.
        subnets: usize,
    }

    // The rows differ along what the rule generalises over: whether the create names no
    // group, one group or several, and over how many subnets of the VPC the proxy listens.
    const ROWS: &[Row] = &[
        Row {
            case: "no security group named",
            name: "emu-proxy-placement-default",
            names_groups: &[],
            subnets: 2,
        },
        Row {
            case: "no security group named, one subnet",
            name: "emu-proxy-placement-default-one-subnet",
            names_groups: &[],
            subnets: 1,
        },
        Row {
            case: "one security group named",
            name: "emu-proxy-placement-one-group",
            names_groups: &[0],
            subnets: 2,
        },
        Row {
            case: "two security groups named",
            name: "emu-proxy-placement-two-groups",
            names_groups: &[1, 0],
            subnets: 2,
        },
    ];

    let created_groups = [first, second];
    for row in ROWS {
        let case = row.case;
        let owned = subnet_members(&net.subnets[..row.subnets]);
        let named: Vec<(String, String)> = row
            .names_groups
            .iter()
            .enumerate()
            .map(|(index, group)| {
                (
                    format!("VpcSecurityGroupIds.member.{}", index + 1),
                    created_groups[*group].clone(),
                )
            })
            .collect();
        let mut members: Vec<(&str, &str)> = vec![
            ("DBProxyName", row.name),
            ("EngineFamily", "MYSQL"),
            ("RoleArn", role.as_str()),
        ];
        for (key, value) in owned.iter().chain(named.iter()) {
            members.push((key.as_str(), value.as_str()));
        }
        let created = rds_ok(&router, case, "CreateDBProxy", &members).await;

        // What the create answered and what the read-back reports are one answer.
        let described = described_proxy(&router, case, row.name).await;
        let expected: Vec<String> = if row.names_groups.is_empty() {
            vec![net.default_security_group.clone()]
        } else {
            row.names_groups
                .iter()
                .map(|group| created_groups[*group].clone())
                .collect()
        };
        for (envelope, proxy) in [("create", &created), ("describe", &described)] {
            let reported = reported_security_groups(proxy);
            assert_eq!(
                reported, expected,
                "case {case:?}: the {envelope} envelope must report the security groups the \
                 proxy belongs to {expected:?}, got {reported:?} in {proxy}"
            );
            let vpc_id = opt_element(proxy, "VpcId");
            assert_eq!(
                vpc_id.as_deref(),
                Some(net.vpc_id.as_str()),
                "case {case:?}: the {envelope} envelope must report the VPC the proxy's \
                 subnets lie in, got {vpc_id:?} in {proxy}"
            );
        }
    }
}

#[tokio::test]
async fn an_auth_entry_reports_the_client_password_auth_type_its_engine_family_defaults_to() {
    let router = build_router();
    let net = network(&router, 21, 2).await;
    let role = proxy_role(&router, "emu-xsvc-proxy-client-auth").await;

    /// One row: the engine family a proxy is created for, the `ClientPasswordAuthType` its
    /// one `Auth` entry states, and the type the proxy must report for that entry.
    struct Row {
        case: &'static str,
        name: &'static str,
        engine_family: &'static str,
        /// `Auth.member.1.ClientPasswordAuthType`, or `None` for an entry that states none.
        states: Option<&'static str>,
        /// The `ClientPasswordAuthType` the entry must report.
        reports: &'static str,
    }

    // The rows differ along what the rule generalises over: every engine family
    // `EngineFamily` publishes with no type stated, and a stated type — of the same family's
    // own vocabulary and of another's — on top of one of them.
    const ROWS: &[Row] = &[
        Row {
            case: "a MySQL proxy stating no client authentication type",
            name: "emu-proxy-auth-mysql",
            engine_family: "MYSQL",
            states: None,
            reports: "MYSQL_CACHING_SHA2_PASSWORD",
        },
        Row {
            case: "a PostgreSQL proxy stating no client authentication type",
            name: "emu-proxy-auth-postgresql",
            engine_family: "POSTGRESQL",
            states: None,
            reports: "POSTGRES_SCRAM_SHA_256",
        },
        Row {
            case: "a SQL Server proxy stating no client authentication type",
            name: "emu-proxy-auth-sqlserver",
            engine_family: "SQLSERVER",
            states: None,
            reports: "SQL_SERVER_AUTHENTICATION",
        },
        Row {
            case: "a MySQL proxy stating the other MySQL type",
            name: "emu-proxy-auth-mysql-native",
            engine_family: "MYSQL",
            states: Some("MYSQL_NATIVE_PASSWORD"),
            reports: "MYSQL_NATIVE_PASSWORD",
        },
        Row {
            case: "a PostgreSQL proxy stating the other PostgreSQL type",
            name: "emu-proxy-auth-postgres-md5",
            engine_family: "POSTGRESQL",
            states: Some("POSTGRES_MD5"),
            reports: "POSTGRES_MD5",
        },
    ];

    let owned = subnet_members(&net.subnets);
    for row in ROWS {
        let case = row.case;
        let mut members: Vec<(&str, &str)> = vec![
            ("DBProxyName", row.name),
            ("EngineFamily", row.engine_family),
            ("RoleArn", role.as_str()),
            ("Auth.member.1.AuthScheme", "SECRETS"),
            ("Auth.member.1.UserName", "emu_app"),
        ];
        for (key, value) in &owned {
            members.push((key.as_str(), value.as_str()));
        }
        if let Some(states) = row.states {
            members.push(("Auth.member.1.ClientPasswordAuthType", states));
        }
        let created = rds_ok(&router, case, "CreateDBProxy", &members).await;
        let described = described_proxy(&router, case, row.name).await;
        for (envelope, proxy) in [("create", &created), ("describe", &described)] {
            let reported = reported_client_auth_types(proxy);
            assert_eq!(
                reported,
                vec![Some(row.reports.to_string())],
                "case {case:?}: the {envelope} envelope must report the client \
                 authentication type {:?} for the proxy's one Auth entry, got {reported:?} \
                 in {proxy}",
                row.reports
            );
        }

        // "The new authentication settings for the DBProxy" REPLACE what the proxy held, and
        // an entry of the new list that states no type reports the same family default.
        await_available_proxy(&router, case, row.name).await;
        let modified = rds_ok(
            &router,
            case,
            "ModifyDBProxy",
            &[
                ("DBProxyName", row.name),
                ("Auth.member.1.AuthScheme", "SECRETS"),
                ("Auth.member.1.UserName", "emu_reporting"),
            ],
        )
        .await;
        let default_for_family = ROWS
            .iter()
            .find(|other| other.engine_family == row.engine_family && other.states.is_none())
            .map(|other| other.reports)
            .unwrap_or_else(|| panic!("case {case:?}: no row states this family's default"));
        for (envelope, proxy) in [
            ("modify", &modified),
            ("describe after modify", &described_proxy(&router, case, row.name).await),
        ] {
            let reported = reported_client_auth_types(proxy);
            assert_eq!(
                reported,
                vec![Some(default_for_family.to_string())],
                "case {case:?}: the {envelope} envelope must report the family default \
                 {default_for_family:?} for the replaced Auth entry, got {reported:?} in \
                 {proxy}"
            );
        }
    }
}
