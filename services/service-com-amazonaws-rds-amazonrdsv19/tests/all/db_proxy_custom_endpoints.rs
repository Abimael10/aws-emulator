// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `CreateDBProxyEndpoint`.
//!
//! Every call goes over the wire: the service is mounted on a `MultiServiceRouter` exactly
//! as the production router mounts it, and each request is a real awsQuery form POST
//! carrying the SigV4 credential scope the router keys on. The assertions read the XML the
//! emulator answers with, so a handler that computed nothing could not pass them.
//!
//! The rule under test, asserted over a TABLE spanning the class it generalises over rather
//! than over one request: an accepted create of a DB proxy endpoint PERSISTS a new endpoint
//! of the proxy it names — "for Aurora DB clusters, you can associate additional endpoints
//! with the same DB proxy. These endpoints can be read/write or read-only. They can also
//! reside in different VPCs than the associated DB proxy" (`DBProxyEndpoint`) — carrying the
//! name, role, subnets, security groups and network type the request stated or each member's
//! documented default, reporting `creating` until the transition this service owes it comes
//! due, publishing an address a client connects to, and readable from then on through every
//! report of that proxy's endpoints; while a name already taken, a proxy that does not
//! exist, a proxy on its way out and a subnet id that names no subnet are each the modelled
//! fault for that cause, and an endpoint leaves the service with the proxy it belongs to.
//!
//! A transition this service owes is waited for by polling a REPORTING call through the poll
//! loop this test target shares (`lifecycle_poll_loops::poll_until`), so the CONDITION is
//! what ends every wait below.

use service_com_amazonaws_rds_amazonrdsv19::state::{
    db_proxy_endpoint_already_exists_message, db_proxy_endpoint_not_found_message,
    db_proxy_not_found_message, db_proxy_unsupported_state_message,
    DB_PROXY_ENDPOINT_STATUS_AVAILABLE, DB_PROXY_ENDPOINT_STATUS_CREATING,
    DB_PROXY_ENDPOINT_STATUS_DELETING, DB_PROXY_ENDPOINT_TARGET_ROLE_READ_ONLY,
    DB_PROXY_ENDPOINT_TARGET_ROLE_READ_WRITE, DB_PROXY_STATUS_AVAILABLE, DB_PROXY_STATUS_DELETING,
    NETWORK_TYPE_IPV4, TARGET_STATE_AVAILABLE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instance_engine_endpoint::{
    available_instance, mysql_login, Transport, DATABASE, MASTER_PASSWORD, MASTER_USER,
};
use crate::db_instances::{
    call, call_in, emulator, expect_error, expect_ok, subtrees, text, texts, REGION,
};
use crate::lifecycle_poll_loops::poll_until;

/// The canonical account id every generated emulator presents.
const ACCOUNT: &str = "000000000000";

/// An IAM role ARN of the form `CreateDBProxy$RoleArn` takes.
const PROXY_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/rds-proxy";

/// One row of the endpoint table: the proxy the endpoint is added to, and what the create
/// states about the endpoint.
struct EndpointCase {
    case: &'static str,
    /// The proxy the endpoint hangs off, created by [`create_proxy`] for this row.
    proxy: &'static str,
    /// `EngineFamily` of that proxy (`smithy_query(query="shape", shape="EngineFamily")`:
    /// `MYSQL | POSTGRESQL | SQLSERVER`).
    engine_family: &'static str,
    /// `DBProxyEndpointName`.
    endpoint: &'static str,
    /// The `TargetRole` the create asks for, or `None` where it names none — in which case
    /// the endpoint reports that member's documented default ("The default is READ_WRITE").
    target_role: Option<&'static str>,
    /// `VpcSubnetIds`, which the endpoint reports as its own.
    subnets: &'static [&'static str],
    /// `VpcSecurityGroupIds`, which the endpoint reports as its own.
    security_groups: &'static [&'static str],
    /// The `EndpointNetworkType` the create asks for, or `None` where it names none — in
    /// which case the endpoint reports that member's documented default ("Default: IPV4").
    network_type: Option<&'static str>,
}

/// The rows differ along everything the create reads off the request and everything the
/// endpoint publishes because of it: the `TargetRole` asked for
/// (`smithy_query(query="shape", shape="DBProxyEndpointTargetRole")`: `READ_WRITE |
/// READ_ONLY`, plus the row that names none and takes the default), how many subnets and
/// security groups the endpoint is placed in (one through three, none through two), the
/// `EndpointNetworkType` asked for (`smithy_query(query="shape",
/// shape="EndpointNetworkType")`: `IPV4 | IPV6 | DUAL`, plus the row that names none), the
/// `EngineFamily` of the proxy behind it — including the one family this emulator speaks no
/// wire for, so the row set spans both forms of published address — and how many endpoints
/// one proxy carries, since the first two rows are added to the same proxy.
const ENDPOINT_CASES: &[EndpointCase] = &[
    EndpointCase {
        case: "a MySQL proxy endpoint with the role and network type left out",
        proxy: "emu-pxc-mysql",
        engine_family: "MYSQL",
        endpoint: "emu-pxe-readwrite",
        target_role: None,
        subnets: &["subnet-0a1b2c3d"],
        security_groups: &[],
        network_type: None,
    },
    EndpointCase {
        case: "a read-only endpoint on the same proxy, in two subnets with one group",
        proxy: "emu-pxc-mysql",
        engine_family: "MYSQL",
        endpoint: "emu-pxe-readonly",
        target_role: Some(DB_PROXY_ENDPOINT_TARGET_ROLE_READ_ONLY),
        subnets: &["subnet-0a1b2c3d", "subnet-1b2c3d4e"],
        security_groups: &["sg-0a1b2c3d"],
        network_type: Some("DUAL"),
    },
    EndpointCase {
        case: "a read-only endpoint of a PostgreSQL proxy, in three subnets with two groups",
        proxy: "emu-pxc-postgres",
        engine_family: "POSTGRESQL",
        endpoint: "emu-pxe-postgres",
        target_role: Some(DB_PROXY_ENDPOINT_TARGET_ROLE_READ_ONLY),
        subnets: &[
            "subnet-0a1b2c3d",
            "subnet-1b2c3d4e",
            "subnet-2c3d4e5f6a7b8c9d0",
        ],
        security_groups: &["sg-0a1b2c3d", "sg-1b2c3d4e"],
        network_type: Some("IPV6"),
    },
    EndpointCase {
        case: "an endpoint of a SQL Server proxy, whose wire this emulator does not speak",
        proxy: "emu-pxc-sqlserver",
        engine_family: "SQLSERVER",
        endpoint: "emu-pxe-sqlserver",
        target_role: Some(DB_PROXY_ENDPOINT_TARGET_ROLE_READ_WRITE),
        subnets: &["subnet-0a1b2c3d"],
        security_groups: &[],
        network_type: Some(NETWORK_TYPE_IPV4),
    },
];

/// `CreateDBProxy` for `proxy`, in one subnet and with no security group of its own.
async fn create_proxy(router: &MultiServiceRouter, case: &str, proxy: &str, family: &str) {
    expect_ok(
        case,
        call(
            router,
            "CreateDBProxy",
            &[
                ("DBProxyName", proxy),
                ("EngineFamily", family),
                ("RoleArn", PROXY_ROLE_ARN),
                ("VpcSubnetIds.member.1", "subnet-0a1b2c3d"),
            ],
        )
        .await,
    );
}

/// Create every proxy the rows name, once each.
async fn create_the_rows_proxies(router: &MultiServiceRouter) {
    let mut created: Vec<&str> = Vec::new();
    for row in ENDPOINT_CASES {
        if created.contains(&row.proxy) {
            continue;
        }
        create_proxy(router, row.case, row.proxy, row.engine_family).await;
        created.push(row.proxy);
    }
}

/// `CreateDBProxyEndpoint` for one row, answered with the status and the body.
async fn create_endpoint(router: &MultiServiceRouter, row: &EndpointCase) -> (u16, String) {
    let mut params: Vec<(String, String)> = vec![
        ("DBProxyName".to_string(), row.proxy.to_string()),
        (
            "DBProxyEndpointName".to_string(),
            row.endpoint.to_string(),
        ),
    ];
    for (index, subnet) in row.subnets.iter().enumerate() {
        params.push((
            format!("VpcSubnetIds.member.{}", index + 1),
            (*subnet).to_string(),
        ));
    }
    for (index, group) in row.security_groups.iter().enumerate() {
        params.push((
            format!("VpcSecurityGroupIds.member.{}", index + 1),
            (*group).to_string(),
        ));
    }
    if let Some(role) = row.target_role {
        params.push(("TargetRole".to_string(), role.to_string()));
    }
    if let Some(network_type) = row.network_type {
        params.push(("EndpointNetworkType".to_string(), network_type.to_string()));
    }
    call(router, "CreateDBProxyEndpoint", &params).await
}

/// `DescribeDBProxyEndpoints` with `params`, answered with the status and the body.
async fn describe_endpoints<N: AsRef<str>, V: AsRef<str>>(
    router: &MultiServiceRouter,
    params: &[(N, V)],
) -> (u16, String) {
    call(router, "DescribeDBProxyEndpoints", params).await
}

/// The report of the one endpoint `endpoint`, asserted to be served.
async fn report_endpoint(router: &MultiServiceRouter, case: &str, endpoint: &str) -> String {
    expect_ok(
        case,
        describe_endpoints(router, &[("DBProxyEndpointName", endpoint)]).await,
    )
}

/// The address the proxy `proxy` publishes as its own `Endpoint`.
async fn proxy_endpoint(router: &MultiServiceRouter, case: &str, proxy: &str) -> String {
    let body = expect_ok(
        case,
        call(router, "DescribeDBProxies", &[("DBProxyName", proxy)]).await,
    );
    text(&body, "Endpoint")
}

/// The ids one `<member>` list of a report carries, in the order it reports them.
fn reported_ids(body: &str, member: &str) -> Vec<String> {
    subtrees(body, member)
        .first()
        .map(|list| texts(list, "member"))
        .unwrap_or_default()
}

/// Poll the report of `endpoint` until it reports `available`, which is what the create owes
/// it.
async fn poll_until_available(router: &MultiServiceRouter, case: &str, endpoint: &str) {
    let params = [("DBProxyEndpointName", endpoint)];
    poll_until(
        case,
        "the created proxy endpoint",
        || describe_endpoints(router, &params),
        |status, body| {
            status == 200 && text(body, "Status") == DB_PROXY_ENDPOINT_STATUS_AVAILABLE
        },
    )
    .await;
}

/// Every member the endpoint of one row must report, whichever call reported it.
fn assert_members(case: &str, body: &str, row: &EndpointCase) {
    for (element, expected) in [
        ("DBProxyEndpointName", row.endpoint.to_string()),
        ("DBProxyName", row.proxy.to_string()),
        (
            "DBProxyEndpointArn",
            format!(
                "arn:aws:rds:{REGION}:{ACCOUNT}:db-proxy-endpoint:{}",
                row.endpoint
            ),
        ),
        (
            "TargetRole",
            row.target_role
                .unwrap_or(DB_PROXY_ENDPOINT_TARGET_ROLE_READ_WRITE)
                .to_string(),
        ),
        (
            "EndpointNetworkType",
            row.network_type.unwrap_or(NETWORK_TYPE_IPV4).to_string(),
        ),
        // "Indicates whether this endpoint is the default endpoint for the associated DB
        // proxy": this one exists because a caller created it, so it is not.
        ("IsDefault", "false".to_string()),
    ] {
        let actual = text(body, element);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {element} {expected:?}, got {actual:?} in {body}"
        );
    }
    let status = text(body, "Status");
    assert!(
        [
            DB_PROXY_ENDPOINT_STATUS_CREATING,
            DB_PROXY_ENDPOINT_STATUS_AVAILABLE
        ]
        .contains(&status.as_str()),
        "case {case:?}: expected the endpoint to be coming up or available, got {status:?} in \
         {body}"
    );
    assert!(
        !text(body, "CreatedDate").is_empty(),
        "case {case:?}: expected the endpoint to report when it was created, got {body}"
    );
    for (member, expected) in [
        ("VpcSubnetIds", row.subnets),
        ("VpcSecurityGroupIds", row.security_groups),
    ] {
        let actual = reported_ids(body, member);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {member} {expected:?}, got {actual:?} in {body}"
        );
    }
}

#[tokio::test]
async fn a_created_endpoint_is_persisted_and_reported_for_its_proxy() {
    let router = emulator();
    create_the_rows_proxies(&router).await;

    for row in ENDPOINT_CASES {
        let created = expect_ok(row.case, create_endpoint(&router, row).await);
        assert_members(row.case, &created, row);

        // The address is one a client CONNECTS to: the authority of this endpoint's own
        // listener for a proxy whose wire this emulator speaks — which is neither empty nor
        // the address the proxy itself publishes, since a session that arrives here is
        // served in this endpoint's own role — and otherwise the DNS name RDS gives a proxy
        // endpoint, in front of the proxy's own server-assigned label.
        let published = text(&created, "Endpoint");
        let proxys_own = proxy_endpoint(&router, row.case, row.proxy).await;
        if row.engine_family == "SQLSERVER" {
            let prefix = format!("{}.endpoint.proxy-", row.endpoint);
            let suffix = format!(".{REGION}.rds.amazonaws.com");
            assert!(
                published.starts_with(&prefix) && published.ends_with(&suffix),
                "case {:?}: expected an address of the form {prefix}<label>{suffix}, got \
                 {published:?}",
                row.case
            );
        } else {
            assert!(
                published.contains(':') && published != proxys_own,
                "case {:?}: expected an address of this endpoint's own listener, got \
                 {published:?} beside the proxy's {proxys_own:?}",
                row.case
            );
        }

        // ROUND TRIP: the endpoint the create answered is the endpoint a later read reports,
        // member for member, through `DescribeDBProxyEndpoints`.
        let read_back = report_endpoint(&router, row.case, row.endpoint).await;
        assert_members(row.case, &read_back, row);
        let reported_address = text(&read_back, "Endpoint");
        assert_eq!(
            reported_address, published,
            "case {:?}: the report must publish the address the create answered, got \
             {reported_address:?} in {read_back}",
            row.case
        );

        // The transition the create owes is waited for by polling the report itself.
        poll_until_available(&router, row.case, row.endpoint).await;
    }

    // Each proxy reports the endpoint RDS created with it beside every endpoint added to it,
    // and nothing of another proxy's.
    for proxy in ["emu-pxc-mysql", "emu-pxc-postgres", "emu-pxc-sqlserver"] {
        let body = expect_ok(
            proxy,
            describe_endpoints(&router, &[("DBProxyName", proxy)]).await,
        );
        let mut expected: Vec<String> = ENDPOINT_CASES
            .iter()
            .filter(|row| row.proxy == proxy)
            .map(|row| row.endpoint.to_string())
            .chain([proxy.to_string()])
            .collect();
        expected.sort();
        let reported = texts(&body, "DBProxyEndpointName");
        assert_eq!(
            reported, expected,
            "case {proxy:?}: expected the default endpoint and every added one, got \
             {reported:?} in {body}"
        );
    }

    // A report is a READ: asking twice answers the same thing.
    let all = expect_ok(
        "every endpoint of the region",
        describe_endpoints(&router, &NO_NARROWING).await,
    );
    let again = expect_ok(
        "every endpoint of the region",
        describe_endpoints(&router, &NO_NARROWING).await,
    );
    assert_eq!(
        texts(&again, "DBProxyEndpointName"),
        texts(&all, "DBProxyEndpointName"),
        "a report must change nothing a later caller sees: {all} then {again}"
    );
    // Nothing of this region's endpoints is reported for another one.
    let (status, elsewhere) = call_in(
        &router,
        "us-east-1",
        "DescribeDBProxyEndpoints",
        &NO_NARROWING,
    )
    .await;
    assert_eq!(
        status, 200,
        "a region holding no proxy still answers the report, got {status} {elsewhere}"
    );
    assert!(
        texts(&elsewhere, "DBProxyEndpointName").is_empty(),
        "a report signed for another region must carry none of this region's endpoints: \
         {elsewhere}"
    );
}

/// A report that narrows nothing: every endpoint of the region the request was signed for.
const NO_NARROWING: [(&str, &str); 0] = [];

/// One row of the refusal table: the request the create refuses, and the failure it answers.
struct RefusalCase {
    case: &'static str,
    /// The whole request beyond `Action`, so a row can name a proxy, a name already taken or
    /// a subnet that is no subnet.
    params: &'static [(&'static str, &'static str)],
    status: u16,
    code: &'static str,
}

#[tokio::test]
async fn the_create_refuses_what_no_endpoint_can_be_made_of() {
    let router = emulator();
    create_proxy(
        &router,
        "the proxy the refusals are asked of",
        "emu-pxc-mysql",
        "MYSQL",
    )
    .await;
    expect_ok(
        "the endpoint the duplicate rows collide with",
        create_endpoint(&router, &ENDPOINT_CASES[0]).await,
    );
    poll_until_available(
        &router,
        "the endpoint the duplicate rows collide with",
        ENDPOINT_CASES[0].endpoint,
    )
    .await;

    // The rows span every way a create can name something no endpoint can be made of.
    let refusals: &[RefusalCase] = &[
        RefusalCase {
            case: "a proxy name that resolves to nothing",
            params: &[
                ("DBProxyName", "emu-pxc-nope"),
                ("DBProxyEndpointName", "emu-pxe-orphan"),
                ("VpcSubnetIds.member.1", "subnet-0a1b2c3d"),
            ],
            status: 404,
            code: "DBProxyNotFoundFault",
        },
        RefusalCase {
            case: "a name another endpoint already holds",
            params: &[
                ("DBProxyName", "emu-pxc-mysql"),
                ("DBProxyEndpointName", "emu-pxe-readwrite"),
                ("VpcSubnetIds.member.1", "subnet-0a1b2c3d"),
            ],
            status: 400,
            code: "DBProxyEndpointAlreadyExistsFault",
        },
        RefusalCase {
            case: "the name of the endpoint RDS created with the proxy",
            params: &[
                ("DBProxyName", "emu-pxc-mysql"),
                ("DBProxyEndpointName", "emu-pxc-mysql"),
                ("VpcSubnetIds.member.1", "subnet-0a1b2c3d"),
            ],
            status: 400,
            code: "DBProxyEndpointAlreadyExistsFault",
        },
        RefusalCase {
            case: "a value that is no subnet id",
            params: &[
                ("DBProxyName", "emu-pxc-mysql"),
                ("DBProxyEndpointName", "emu-pxe-nosubnet"),
                ("VpcSubnetIds.member.1", "not-a-subnet"),
            ],
            status: 400,
            code: "InvalidSubnet",
        },
    ];
    for row in refusals {
        expect_error(
            row.case,
            call(&router, "CreateDBProxyEndpoint", row.params).await,
            row.status,
            row.code,
        );
    }

    // Each refusal is worded as Amazon RDS words that fault, with the names the REQUEST
    // carried as the sentences' only variable parts.
    for (case, params, expected_message) in [
        (
            "a proxy name that resolves to nothing",
            vec![
                ("DBProxyName", "emu-pxc-nope"),
                ("DBProxyEndpointName", "emu-pxe-orphan"),
                ("VpcSubnetIds.member.1", "subnet-0a1b2c3d"),
            ],
            db_proxy_not_found_message("emu-pxc-nope"),
        ),
        (
            "a name another endpoint already holds",
            vec![
                ("DBProxyName", "emu-pxc-mysql"),
                ("DBProxyEndpointName", "emu-pxe-readwrite"),
                ("VpcSubnetIds.member.1", "subnet-0a1b2c3d"),
            ],
            db_proxy_endpoint_already_exists_message("emu-pxe-readwrite"),
        ),
    ] {
        let refused = call(&router, "CreateDBProxyEndpoint", &params).await.1;
        let actual = text(&refused, "Message");
        assert_eq!(
            actual, expected_message,
            "case {case:?}: expected {expected_message:?}, got {actual:?} in {refused}"
        );
    }

    // Nothing of a refused create reaches the store: the endpoint the rows named is reported
    // by no read, and the proxy still has exactly the endpoints it had.
    for named in ["emu-pxe-orphan", "emu-pxe-nosubnet"] {
        expect_error(
            named,
            describe_endpoints(&router, &[("DBProxyEndpointName", named)]).await,
            404,
            "DBProxyEndpointNotFoundFault",
        );
    }
    let body = expect_ok(
        "the proxy after every refusal",
        describe_endpoints(&router, &[("DBProxyName", "emu-pxc-mysql")]).await,
    );
    let reported = texts(&body, "DBProxyEndpointName");
    assert_eq!(
        reported,
        vec![
            "emu-pxc-mysql".to_string(),
            "emu-pxe-readwrite".to_string()
        ],
        "a refused create must leave the proxy's endpoints as they were, got {reported:?} in \
         {body}"
    );

    // A proxy whose DELETE has been accepted is on its way out of this service, so there is
    // nothing left for an endpoint to be added to.
    expect_ok(
        "the proxy on its way out",
        call(&router, "DeleteDBProxy", &[("DBProxyName", "emu-pxc-mysql")]).await,
    );
    let refused = expect_error(
        "a proxy whose delete was accepted",
        call(
            &router,
            "CreateDBProxyEndpoint",
            &[
                ("DBProxyName", "emu-pxc-mysql"),
                ("DBProxyEndpointName", "emu-pxe-toolate"),
                ("VpcSubnetIds.member.1", "subnet-0a1b2c3d"),
            ],
        )
        .await,
        400,
        "InvalidDBProxyStateFault",
    );
    let expected_message =
        db_proxy_unsupported_state_message("emu-pxc-mysql", DB_PROXY_STATUS_DELETING);
    let actual = text(&refused, "Message");
    assert_eq!(
        actual, expected_message,
        "case \"a proxy whose delete was accepted\": expected {expected_message:?}, got \
         {actual:?} in {refused}"
    );
}

/// MySQL's own error code for a write on a server running read-only
/// (`ER_OPTION_PREVENTS_STATEMENT`), which is what a client branches on.
const MYSQL_READ_ONLY_CODE: &str = "1290";

/// The `host:port` a client dials, read out of a published endpoint.
///
/// `DBProxyEndpoint` models no port member of its own, so the address an endpoint this
/// emulator opened a listener for publishes carries the port the core assigned it.
fn dialled(case: &str, endpoint: &str) -> (String, u16) {
    let (host, port) = endpoint.rsplit_once(':').unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected a published endpoint carrying the port a client dials, \
             got {endpoint:?}"
        )
    });
    let port: u16 = port.parse().unwrap_or_else(|_| {
        panic!("case {case:?}: expected a port in the published endpoint, got {endpoint:?}")
    });
    (host.to_string(), port)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_on_an_added_endpoint_is_served_in_that_endpoints_role() {
    let router = emulator();
    let proxy = "emu-pxc-mysql";
    // The database the proxy pools connections to, and the proxy in front of it, both ready.
    available_instance(
        &router,
        "emu-pxe-target",
        "mysql",
        &[("EngineVersion", "8.0.39".to_string())],
    )
    .await;
    create_proxy(&router, "the proxy the endpoints are added to", proxy, "MYSQL").await;
    let proxy_params = [("DBProxyName", proxy)];
    poll_until(
        "the proxy the endpoints are added to",
        "the proxy",
        || call(&router, "DescribeDBProxies", &proxy_params),
        |status, body| status == 200 && text(body, "Status") == DB_PROXY_STATUS_AVAILABLE,
    )
    .await;
    expect_ok(
        "the proxy's target",
        call(
            &router,
            "RegisterDBProxyTargets",
            &[
                ("DBProxyName", proxy),
                ("DBInstanceIdentifiers.member.1", "emu-pxe-target"),
            ],
        )
        .await,
    );
    poll_until(
        "the proxy's target",
        "the proxy's target",
        || call(&router, "DescribeDBProxyTargets", &proxy_params),
        |status, body| {
            status == 200
                && texts(body, "State")
                    .iter()
                    .any(|state| state == TARGET_STATE_AVAILABLE)
        },
    )
    .await;

    // The two endpoints under test: one read/write, one read-only, on that one proxy.
    let read_write = &ENDPOINT_CASES[0];
    let read_only = &ENDPOINT_CASES[1];
    let mut addresses: Vec<(String, u16)> = Vec::new();
    for row in [read_write, read_only] {
        let created = expect_ok(row.case, create_endpoint(&router, row).await);
        addresses.push(dialled(row.case, &text(&created, "Endpoint")));
        poll_until_available(&router, row.case, row.endpoint).await;
    }
    let (writer_host, writer_port) = addresses[0].clone();
    let (reader_host, reader_port) = addresses[1].clone();
    assert_ne!(
        (writer_host.clone(), writer_port),
        (reader_host.clone(), reader_port),
        "each added endpoint is an address of its own, got {addresses:?}"
    );

    // The READ/WRITE endpoint is an address this emulator answers on, and a session on it
    // reaches the database the proxy's target group holds: it writes there.
    let mut writer = mysql_login(
        &writer_host,
        writer_port,
        Transport::Plaintext,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(DATABASE),
    )
    .await
    .expect("the read/write endpoint admits the database's master user");
    writer
        .query("CREATE TABLE added_endpoint_rows (note VARCHAR(48))")
        .await
        .expect("the read/write endpoint runs the statement");
    writer
        .query("INSERT INTO added_endpoint_rows (note) VALUES ('written through read/write')")
        .await
        .expect("the read/write endpoint runs the statement");

    // The READ-ONLY endpoint reaches the same database — it reads what the other endpoint
    // committed — and refuses the statements that write, with the engine's own code: "a value
    // that indicates whether the DB proxy endpoint can be used for read/write or read-only
    // operations".
    let mut reader = mysql_login(
        &reader_host,
        reader_port,
        Transport::Plaintext,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(DATABASE),
    )
    .await
    .expect("the read-only endpoint admits the database's master user");
    let read = reader
        .query("SELECT note FROM added_endpoint_rows")
        .await
        .expect("the read-only endpoint runs the statement");
    assert_eq!(
        read.only(),
        Some("written through read/write"),
        "case {:?}: expected the read-only endpoint to read the row the other endpoint \
         committed, got {read:?}",
        read_only.case
    );
    let refused = reader
        .query("INSERT INTO added_endpoint_rows (note) VALUES ('written through read-only')")
        .await
        .expect_err("a read-only endpoint refuses a write");
    assert_eq!(
        refused.code, MYSQL_READ_ONLY_CODE,
        "case {:?}: expected the engine's own read-only refusal, got {refused:?}",
        read_only.case
    );

    // And nothing of the refused statement reached the database the proxy pools to.
    let after = writer
        .query("SELECT note FROM added_endpoint_rows")
        .await
        .expect("the read/write endpoint runs the statement");
    assert_eq!(
        after.rows.len(),
        1,
        "case {:?}: a refused write must leave the database as it was, got {after:?}",
        read_only.case
    );
}

#[tokio::test]
async fn an_endpoint_leaves_the_service_with_the_proxy_it_belongs_to() {
    let router = emulator();
    let proxy = "emu-pxc-mysql";
    create_proxy(&router, "the proxy whose endpoints cascade", proxy, "MYSQL").await;
    for row in &ENDPOINT_CASES[..2] {
        expect_ok(row.case, create_endpoint(&router, row).await);
    }
    for row in &ENDPOINT_CASES[..2] {
        poll_until_available(&router, row.case, row.endpoint).await;
    }

    // A RENAME of the proxy takes its endpoints with it: "the identifier for the DB proxy
    // that is associated with this DB proxy endpoint" is the proxy as it stands, so no
    // endpoint is left naming a proxy this service no longer holds.
    let renamed = "emu-pxc-renamed";
    expect_ok(
        "a renamed proxy",
        call(
            &router,
            "ModifyDBProxy",
            &[("DBProxyName", proxy), ("NewDBProxyName", renamed)],
        )
        .await,
    );
    for row in &ENDPOINT_CASES[..2] {
        let body = report_endpoint(&router, row.case, row.endpoint).await;
        let reported = text(&body, "DBProxyName");
        assert_eq!(
            reported, renamed,
            "case {:?}: an endpoint of a renamed proxy must name the proxy as it stands, got \
             {reported:?} in {body}",
            row.case
        );
    }
    let body = expect_ok(
        "a renamed proxy",
        describe_endpoints(&router, &[("DBProxyName", renamed)]).await,
    );
    let mut expected: Vec<String> = ENDPOINT_CASES[..2]
        .iter()
        .map(|row| row.endpoint.to_string())
        .chain([renamed.to_string()])
        .collect();
    expected.sort();
    let reported = texts(&body, "DBProxyEndpointName");
    assert_eq!(
        reported, expected,
        "a renamed proxy must still report every endpoint added to it, got {reported:?} in \
         {body}"
    );

    // A delete is ACCEPTED rather than instantaneous, so every endpoint of the proxy reports
    // the state the proxy is leaving in until this service has taken the proxy away.
    expect_ok(
        "the deleted proxy",
        call(&router, "DeleteDBProxy", &[("DBProxyName", renamed)]).await,
    );
    for row in &ENDPOINT_CASES[..2] {
        let body = report_endpoint(&router, row.case, row.endpoint).await;
        let status = text(&body, "Status");
        assert_eq!(
            status, DB_PROXY_ENDPOINT_STATUS_DELETING,
            "case {:?}: an endpoint of a proxy on its way out goes with it, got {status:?} in \
             {body}",
            row.case
        );
    }

    // Once the removal this service owes is due, every endpoint of that proxy is gone with
    // it: each name resolves no endpoint at all.
    for row in &ENDPOINT_CASES[..2] {
        let params = [("DBProxyEndpointName", row.endpoint)];
        poll_until(
            row.case,
            "the endpoints of the removed proxy",
            || describe_endpoints(&router, &params),
            |status, _| status == 404,
        )
        .await;
        let refused = expect_error(
            row.case,
            describe_endpoints(&router, &params).await,
            404,
            "DBProxyEndpointNotFoundFault",
        );
        let expected_message = db_proxy_endpoint_not_found_message(row.endpoint);
        let actual = text(&refused, "Message");
        assert_eq!(
            actual, expected_message,
            "case {:?}: expected {expected_message:?}, got {actual:?} in {refused}",
            row.case
        );
    }
}
