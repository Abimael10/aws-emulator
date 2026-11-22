// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `DescribeDBProxyEndpoints`.
//!
//! Every call goes over the wire: the service is mounted on a `MultiServiceRouter` exactly
//! as the production router mounts it, and each request is a real awsQuery form POST
//! carrying the SigV4 credential scope the router keys on. The assertions read the XML the
//! emulator answers with, so a handler that computed nothing could not pass them.
//!
//! The rule under test, asserted over a TABLE spanning the class it generalises over rather
//! than over one request: "RDS automatically creates one endpoint for each DB proxy"
//! (`DBProxyEndpoint`), so the report of a proxy's endpoints computes that DEFAULT endpoint
//! from the proxy's own stored record — the proxy's name, the ARN of its endpoint, the
//! address the proxy publishes, the VPC, subnets, security groups and network type it was
//! placed in, when it was created, that it is the default endpoint and read/write
//! (`DBProxyEndpoint$IsDefault`), and a `Status` read off the state the proxy is in — for
//! every proxy of the region the request was signed for, or for the one proxy or the one
//! endpoint a request names; while a proxy name that resolves to nothing and an endpoint
//! name that resolves none of the endpoints in scope are the two faults the operation
//! models, and the report changes nothing a later caller sees.
//!
//! A transition this service owes is waited for by polling a REPORTING call through the poll
//! loop this test target shares (`lifecycle_poll_loops::poll_until`), so the CONDITION is
//! what ends every wait below.

use service_com_amazonaws_rds_amazonrdsv19::state::{
    db_proxy_endpoint_not_found_message, db_proxy_not_found_message,
    DB_PROXY_ENDPOINT_STATUS_AVAILABLE, DB_PROXY_ENDPOINT_TARGET_ROLE_READ_WRITE,
    DB_PROXY_STATUS_AVAILABLE, DB_PROXY_STATUS_CREATING, DB_PROXY_STATUS_DELETING,
    DB_PROXY_STATUS_MODIFYING, NETWORK_TYPE_IPV4,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{
    call, call_in, emulator, expect_error, expect_ok, optional_text, subtrees, text, texts, REGION,
};
use crate::lifecycle_poll_loops::poll_until;

/// The canonical account id every generated emulator presents.
const ACCOUNT: &str = "000000000000";

/// An IAM role ARN of the form `CreateDBProxy$RoleArn` takes.
const PROXY_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/rds-proxy";

/// One row of the proxy table: the proxy created, and what its default endpoint reports
/// because of it.
struct ProxyCase {
    case: &'static str,
    proxy: &'static str,
    engine_family: &'static str,
    /// `VpcSubnetIds`, which the endpoint reports as its own `VpcSubnetIds`.
    subnets: &'static [&'static str],
    /// `VpcSecurityGroupIds`, which the endpoint reports as its own.
    security_groups: &'static [&'static str],
    /// The `EndpointNetworkType` the create asks for, or `None` where it names none — in
    /// which case the endpoint reports that member's documented default ("Default: IPV4").
    endpoint_network_type: Option<&'static str>,
}

/// The rows differ along everything the projection reads off the proxy: the `EngineFamily`
/// it was created with (`smithy_query(query="shape", shape="EngineFamily")`: `MYSQL |
/// POSTGRESQL | SQLSERVER` — and only the two whose wire this emulator speaks publish a
/// listener as their address, so the row set spans both forms of published endpoint), how
/// many subnets and security groups it was placed in (none through two), and the
/// `EndpointNetworkType` asked for (`smithy_query(query="shape",
/// shape="EndpointNetworkType")`: `IPV4 | IPV6 | DUAL`, plus the row that names none and
/// takes the default).
const PROXY_CASES: &[ProxyCase] = &[
    ProxyCase {
        case: "a MySQL proxy in one subnet with no security group",
        proxy: "emu-pxe-mysql",
        engine_family: "MYSQL",
        subnets: &["subnet-0a1b2c3d"],
        security_groups: &[],
        endpoint_network_type: None,
    },
    ProxyCase {
        case: "a PostgreSQL proxy in two subnets with one security group",
        proxy: "emu-pxe-postgres",
        engine_family: "POSTGRESQL",
        subnets: &["subnet-0a1b2c3d", "subnet-1b2c3d4e"],
        security_groups: &["sg-0a1b2c3d"],
        endpoint_network_type: Some("DUAL"),
    },
    ProxyCase {
        case: "a SQL Server proxy in three subnets with two security groups",
        proxy: "emu-pxe-sqlserver",
        engine_family: "SQLSERVER",
        subnets: &[
            "subnet-0a1b2c3d",
            "subnet-1b2c3d4e",
            "subnet-2c3d4e5f6a7b8c9d0",
        ],
        security_groups: &["sg-0a1b2c3d", "sg-1b2c3d4e"],
        endpoint_network_type: Some("IPV6"),
    },
];

/// `CreateDBProxy` for one row.
///
/// The target-group suite's shared `create_proxy` carries one subnet and no security group,
/// while these rows vary exactly those members and the network type, so the call is built
/// from the row here.
async fn create_proxy_for(router: &MultiServiceRouter, row: &ProxyCase) {
    let mut params: Vec<(String, String)> = vec![
        ("DBProxyName".to_string(), row.proxy.to_string()),
        ("EngineFamily".to_string(), row.engine_family.to_string()),
        ("RoleArn".to_string(), PROXY_ROLE_ARN.to_string()),
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
    if let Some(network_type) = row.endpoint_network_type {
        params.push((
            "EndpointNetworkType".to_string(),
            network_type.to_string(),
        ));
    }
    expect_ok(row.case, call(router, "CreateDBProxy", &params).await);
}

/// `DescribeDBProxyEndpoints` with `params`, answered with the status and the body.
async fn describe_endpoints<N: AsRef<str>, V: AsRef<str>>(
    router: &MultiServiceRouter,
    params: &[(N, V)],
) -> (u16, String) {
    call(router, "DescribeDBProxyEndpoints", params).await
}

/// `DescribeDBProxyEndpoints` with `params`, asserted to be served.
async fn report_endpoints<N: AsRef<str>, V: AsRef<str>>(
    router: &MultiServiceRouter,
    case: &str,
    params: &[(N, V)],
) -> String {
    expect_ok(case, describe_endpoints(router, params).await)
}

/// `DescribeDBProxies` for `proxy`, answered with the status and the body.
async fn describe_proxy(router: &MultiServiceRouter, proxy: &str) -> (u16, String) {
    call(router, "DescribeDBProxies", &[("DBProxyName", proxy)]).await
}

/// The ids one `<member>` list of a report carries, in the order it reports them.
///
/// `VpcSubnetIds` and `VpcSecurityGroupIds` are both `<member>` lists, so the ids are read
/// out of the one subtree that holds them rather than out of the whole document.
fn reported_ids(body: &str, member: &str) -> Vec<String> {
    subtrees(body, member)
        .first()
        .map(|list| texts(list, "member"))
        .unwrap_or_default()
}

/// Poll the endpoint report of `proxy` until the endpoint reports `available`, which is what
/// the proxy coming up owes it.
async fn poll_until_endpoint_available(router: &MultiServiceRouter, case: &str, proxy: &str) {
    let params = [("DBProxyName", proxy)];
    poll_until(
        case,
        "the proxy's default endpoint",
        || describe_endpoints(router, &params),
        |status, body| {
            status == 200 && text(body, "Status") == DB_PROXY_ENDPOINT_STATUS_AVAILABLE
        },
    )
    .await;
}

#[tokio::test]
async fn each_proxy_reports_the_default_endpoint_rds_created_with_it() {
    let router = emulator();
    for row in PROXY_CASES {
        create_proxy_for(&router, row).await;
    }

    for row in PROXY_CASES {
        poll_until_endpoint_available(&router, row.case, row.proxy).await;
        let body = report_endpoints(&router, row.case, &[("DBProxyName", row.proxy)]).await;

        // One proxy has exactly one endpoint without a create: "RDS automatically creates
        // one endpoint for each DB proxy".
        let names = texts(&body, "DBProxyEndpointName");
        assert_eq!(
            names.len(),
            1,
            "case {:?}: expected exactly one endpoint, got {names:?} in {body}",
            row.case
        );

        // The proxy's own report is where the published ADDRESS comes from: the default
        // endpoint is the proxy's own endpoint, so the two cannot disagree.
        let (proxy_status, proxy_body) = describe_proxy(&router, row.proxy).await;
        assert_eq!(
            proxy_status, 200,
            "case {:?}: the proxy must be reported, got {proxy_status} {proxy_body}",
            row.case
        );
        let published = text(&proxy_body, "Endpoint");

        for (element, expected) in [
            // The default endpoint is the proxy's own address, so it carries the proxy's
            // name rather than one a caller chose.
            ("DBProxyEndpointName", row.proxy.to_string()),
            ("DBProxyName", row.proxy.to_string()),
            (
                "DBProxyEndpointArn",
                format!(
                    "arn:aws:rds:{REGION}:{ACCOUNT}:db-proxy-endpoint:{}",
                    row.proxy
                ),
            ),
            ("Endpoint", published.clone()),
            ("Status", DB_PROXY_ENDPOINT_STATUS_AVAILABLE.to_string()),
            // "Default DB proxy endpoints always have read/write capability."
            ("IsDefault", "true".to_string()),
            (
                "TargetRole",
                DB_PROXY_ENDPOINT_TARGET_ROLE_READ_WRITE.to_string(),
            ),
            (
                "EndpointNetworkType",
                row.endpoint_network_type
                    .unwrap_or(NETWORK_TYPE_IPV4)
                    .to_string(),
            ),
        ] {
            let actual = text(&body, element);
            assert_eq!(
                actual, expected,
                "case {:?}: expected {element} {expected:?}, got {actual:?} in {body}",
                row.case
            );
        }
        assert!(
            !text(&body, "CreatedDate").is_empty(),
            "case {:?}: expected the endpoint to report when it was created, got {body}",
            row.case
        );
        for (member, expected) in [
            ("VpcSubnetIds", row.subnets),
            ("VpcSecurityGroupIds", row.security_groups),
        ] {
            let actual = reported_ids(&body, member);
            assert_eq!(
                actual, expected,
                "case {:?}: expected {member} {expected:?}, got {actual:?} in {body}",
                row.case
            );
        }
        assert_eq!(
            optional_text(&body, "Marker"),
            None,
            "case {:?}: one proxy's one endpoint fits in one page, so no Marker is due: \
             {body}",
            row.case
        );

        // Naming the endpoint reports that one endpoint — with the proxy it belongs to
        // named beside it, and on its own, since "if you omit this parameter the output
        // includes information about all DB proxy endpoints associated with all your DB
        // proxies".
        for (narrowing, params) in [
            (
                "the endpoint of the named proxy",
                vec![
                    ("DBProxyName", row.proxy),
                    ("DBProxyEndpointName", row.proxy),
                ],
            ),
            (
                "the endpoint named on its own",
                vec![("DBProxyEndpointName", row.proxy)],
            ),
        ] {
            let narrowed = report_endpoints(&router, row.case, &params).await;
            assert_eq!(
                texts(&narrowed, "DBProxyEndpointName"),
                names,
                "case {:?}: {narrowing} must report that endpoint alone: {narrowed}",
                row.case
            );
            assert_eq!(
                text(&narrowed, "Endpoint"),
                published,
                "case {:?}: {narrowing} must publish the proxy's own address: {narrowed}",
                row.case
            );
        }
    }

    // A report that names no proxy carries the endpoint of every proxy of the region, in
    // the order the proxies are stored in, and nothing of another region's.
    let all = report_endpoints(&router, "every proxy of the region", &NO_NARROWING).await;
    let mut expected: Vec<String> = PROXY_CASES.iter().map(|row| row.proxy.to_string()).collect();
    expected.sort();
    assert_eq!(
        texts(&all, "DBProxyEndpointName"),
        expected,
        "a report that names no proxy must carry every proxy's endpoint: {all}"
    );
    assert_eq!(
        optional_text(&all, "Marker"),
        None,
        "every endpoint of the region fits in one page, so no Marker is due: {all}"
    );
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

/// One row of the refusal table: a request the operation refuses, and the failure it
/// answers.
struct RefusalCase {
    case: &'static str,
    /// The whole request beyond `Action`, so a row can name a proxy, an endpoint, a filter,
    /// a page size or a marker.
    params: &'static [(&'static str, &'static str)],
    status: u16,
    code: &'static str,
}

#[tokio::test]
async fn the_endpoint_report_refuses_what_names_none_and_changes_nothing() {
    let router = emulator();

    // A proxy name that resolves to nothing is the proxy's own not-found fault before any
    // proxy exists at all.
    let refused = expect_error(
        "an unknown proxy before any exists",
        describe_endpoints(&router, &[("DBProxyName", "emu-pxe-nope")]).await,
        404,
        "DBProxyNotFoundFault",
    );
    let expected_message = db_proxy_not_found_message("emu-pxe-nope");
    let actual_message = text(&refused, "Message");
    assert_eq!(
        actual_message, expected_message,
        "case \"an unknown proxy before any exists\": expected {expected_message:?}, got \
         {actual_message:?} in {refused}"
    );
    // A region that holds no proxy still reports an empty list rather than a fault, since
    // nothing was named for it to resolve.
    let empty = report_endpoints(&router, "a region holding no proxy", &NO_NARROWING).await;
    assert!(
        texts(&empty, "DBProxyEndpointName").is_empty(),
        "a region holding no proxy must report no endpoint: {empty}"
    );

    for row in &PROXY_CASES[..2] {
        create_proxy_for(&router, row).await;
    }
    for row in &PROXY_CASES[..2] {
        poll_until_endpoint_available(&router, row.case, row.proxy).await;
    }

    // The rows span every way a request can name nothing this report can serve, and both
    // published bounds of the page size (`MaxRecords`, "Constraints: Minimum 20, maximum
    // 100") one past each end.
    let refusals: &[RefusalCase] = &[
        RefusalCase {
            case: "a proxy name that resolves to nothing",
            params: &[("DBProxyName", "emu-pxe-nope")],
            status: 404,
            code: "DBProxyNotFoundFault",
        },
        RefusalCase {
            case: "an endpoint name that resolves to nothing",
            params: &[("DBProxyEndpointName", "emu-pxe-no-endpoint")],
            status: 404,
            code: "DBProxyEndpointNotFoundFault",
        },
        RefusalCase {
            case: "an endpoint of another proxy than the one named",
            params: &[
                ("DBProxyName", "emu-pxe-mysql"),
                ("DBProxyEndpointName", "emu-pxe-postgres"),
            ],
            status: 404,
            code: "DBProxyEndpointNotFoundFault",
        },
        RefusalCase {
            case: "a filter, which this report does not support",
            params: &[
                ("Filters.Filter.1.Name", "db-proxy-endpoint-id"),
                ("Filters.Filter.1.Values.Value.1", "emu-pxe-mysql"),
            ],
            status: 400,
            code: "InvalidParameterValue",
        },
        RefusalCase {
            case: "a MaxRecords one below the lowest published bound",
            params: &[("MaxRecords", "19")],
            status: 400,
            code: "InvalidParameterValue",
        },
        RefusalCase {
            case: "a MaxRecords one above the highest published bound",
            params: &[("MaxRecords", "101")],
            status: 400,
            code: "InvalidParameterValue",
        },
        RefusalCase {
            case: "a Marker this report's codec could not have produced",
            params: &[("Marker", "not-a-marker-this-report-issued")],
            status: 400,
            code: "InvalidParameterValue",
        },
    ];
    for row in refusals {
        expect_error(
            row.case,
            describe_endpoints(&router, row.params).await,
            row.status,
            row.code,
        );
    }

    // The endpoint fault is worded as the proxy API words its not-found sentences, with the
    // requested name as the sentence's only variable part.
    let unknown_endpoint = expect_error(
        "the wording of the endpoint fault",
        describe_endpoints(&router, &[("DBProxyEndpointName", "emu-pxe-no-endpoint")]).await,
        404,
        "DBProxyEndpointNotFoundFault",
    );
    let expected_message = db_proxy_endpoint_not_found_message("emu-pxe-no-endpoint");
    let actual_message = text(&unknown_endpoint, "Message");
    assert_eq!(
        actual_message, expected_message,
        "case \"the wording of the endpoint fault\": expected {expected_message:?}, got \
         {actual_message:?} in {unknown_endpoint}"
    );

    // Each published bound of the page size is SERVED, and so is a request that names
    // neither bound.
    for page_size in ["20", "100"] {
        let page = report_endpoints(
            &router,
            "a page size at a published bound",
            &[("MaxRecords", page_size)],
        )
        .await;
        assert_eq!(
            texts(&page, "DBProxyEndpointName").len(),
            2,
            "case \"a page size at a published bound\": a page of {page_size} holds both \
             endpoints: {page}"
        );
    }

    // A report is a READ: asking twice answers the same thing, and the proxies themselves
    // are untouched by it.
    let first = report_endpoints(&router, "a repeated report", &NO_NARROWING).await;
    let again = report_endpoints(&router, "a repeated report", &NO_NARROWING).await;
    assert_eq!(
        texts(&again, "DBProxyEndpointName"),
        texts(&first, "DBProxyEndpointName"),
        "a report must change nothing a later caller sees: {first} then {again}"
    );
    for row in &PROXY_CASES[..2] {
        let (status, body) = describe_proxy(&router, row.proxy).await;
        assert_eq!(
            status, 200,
            "case {:?}: the proxy must be untouched by the report, got {status} {body}",
            row.case
        );
        assert_eq!(
            text(&body, "Status"),
            DB_PROXY_STATUS_AVAILABLE,
            "case {:?}: the report must not move the proxy's status: {body}",
            row.case
        );
    }
}

/// Where in its lifecycle the proxy is when its endpoint is read.
#[derive(Debug, Clone, Copy)]
struct StatusCase {
    case: &'static str,
    /// The statuses the endpoint may report at that moment. More than one wherever this
    /// service's clock is entitled to have settled a transition it owes already: the rule
    /// is that the endpoint reports the state of the PROXY behind it, so no row depends on
    /// how fast the host runs.
    admissible: &'static [&'static str],
}

#[tokio::test]
async fn the_default_endpoint_reports_the_state_the_proxy_is_in() {
    let router = emulator();
    let proxy = "emu-pxe-lifecycle";
    // A bystander, so the stages below are shown to be the named proxy's own.
    create_proxy_for(&router, &PROXY_CASES[1]).await;

    // An endpoint's status list is narrower than a proxy's, and every status this service
    // puts a proxy in is in both: the endpoint of a proxy that is coming up is being created
    // with it, the endpoint of a proxy applying a change is applying it too, and the
    // endpoint of a proxy whose delete was accepted is going with it.
    let coming_up = StatusCase {
        case: "a proxy that is still coming up",
        admissible: &[DB_PROXY_STATUS_CREATING, DB_PROXY_STATUS_AVAILABLE],
    };
    let modifying = StatusCase {
        case: "a proxy that is applying a modification",
        admissible: &[DB_PROXY_STATUS_MODIFYING, DB_PROXY_STATUS_AVAILABLE],
    };
    let deleting = StatusCase {
        case: "a proxy whose delete was accepted",
        admissible: &[DB_PROXY_STATUS_DELETING],
    };

    expect_ok(
        coming_up.case,
        call(
            &router,
            "CreateDBProxy",
            &[
                ("DBProxyName", proxy),
                ("EngineFamily", "MYSQL"),
                ("RoleArn", PROXY_ROLE_ARN),
                ("VpcSubnetIds.member.1", "subnet-0a1b2c3d"),
            ],
        )
        .await,
    );
    assert_endpoint_status(&router, &coming_up, proxy).await;

    // The transition the create owes is waited for by polling the report itself.
    poll_until_endpoint_available(&router, "the proxy coming up", proxy).await;

    expect_ok(
        modifying.case,
        call(
            &router,
            "ModifyDBProxy",
            &[("DBProxyName", proxy), ("IdleClientTimeout", "900")],
        )
        .await,
    );
    assert_endpoint_status(&router, &modifying, proxy).await;
    poll_until_endpoint_available(&router, "the proxy applying a modification", proxy).await;

    // A RENAME moves the endpoint with the proxy: the default endpoint is the proxy's own
    // address, so nothing of it is left reporting the name the proxy no longer has.
    let renamed = "emu-pxe-renamed";
    expect_ok(
        "a renamed proxy",
        call(
            &router,
            "ModifyDBProxy",
            &[("DBProxyName", proxy), ("NewDBProxyName", renamed)],
        )
        .await,
    );
    let after_rename = report_endpoints(&router, "a renamed proxy", &[("DBProxyName", renamed)])
        .await;
    for (element, expected) in [
        ("DBProxyEndpointName", renamed.to_string()),
        ("DBProxyName", renamed.to_string()),
        (
            "DBProxyEndpointArn",
            format!("arn:aws:rds:{REGION}:{ACCOUNT}:db-proxy-endpoint:{renamed}"),
        ),
    ] {
        let actual = text(&after_rename, element);
        assert_eq!(
            actual, expected,
            "case \"a renamed proxy\": expected {element} {expected:?}, got {actual:?} in \
             {after_rename}"
        );
    }
    expect_error(
        "the name a renamed proxy no longer has",
        describe_endpoints(&router, &[("DBProxyEndpointName", proxy)]).await,
        404,
        "DBProxyEndpointNotFoundFault",
    );
    poll_until_endpoint_available(&router, "the renamed proxy", renamed).await;

    // A delete is ACCEPTED rather than instantaneous, so the endpoint reports the state the
    // proxy is leaving in until this service has taken the proxy away.
    expect_ok(
        deleting.case,
        call(&router, "DeleteDBProxy", &[("DBProxyName", renamed)]).await,
    );
    assert_endpoint_status(&router, &deleting, renamed).await;

    // Once the removal this service owes is due, the proxy's endpoint is gone with it: the
    // proxy's name resolves no proxy, and the endpoint's name resolves no endpoint.
    let removed = [("DBProxyName", renamed)];
    poll_until(
        "the removed proxy",
        "the endpoints of the removed proxy",
        || describe_endpoints(&router, &removed),
        |status, _| status == 404,
    )
    .await;
    expect_error(
        "the endpoints of the removed proxy",
        describe_endpoints(&router, &[("DBProxyName", renamed)]).await,
        404,
        "DBProxyNotFoundFault",
    );
    expect_error(
        "the endpoint of the removed proxy",
        describe_endpoints(&router, &[("DBProxyEndpointName", renamed)]).await,
        404,
        "DBProxyEndpointNotFoundFault",
    );

    // Every stage above took only the proxy it named: the bystander still reaches the state
    // this service owes it.
    poll_until_endpoint_available(&router, "the bystander proxy", PROXY_CASES[1].proxy).await;
}

/// Assert the endpoint of `proxy` reports one of the statuses `row` admits.
async fn assert_endpoint_status(router: &MultiServiceRouter, row: &StatusCase, proxy: &str) {
    let body = report_endpoints(router, row.case, &[("DBProxyName", proxy)]).await;
    let reported = text(&body, "Status");
    assert!(
        row.admissible.contains(&reported.as_str()),
        "case {:?}: expected the endpoint to report one of {:?}, got {reported:?} in {body}",
        row.case,
        row.admissible
    );
}
