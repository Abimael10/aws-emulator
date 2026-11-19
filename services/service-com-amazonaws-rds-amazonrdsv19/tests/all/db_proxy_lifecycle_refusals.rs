// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the two refusals a DB proxy's lifecycle decides: WHICH class a
//! call naming a proxy this service no longer holds is refused with, and WHICH sentence a
//! call refused because the proxy is not usable carries.
//!
//! Every call goes over the wire: the service is mounted on a `MultiServiceRouter` exactly
//! as the production router mounts it, and each request is a real awsQuery form POST
//! carrying the SigV4 credential scope the router keys on. The assertions read the XML the
//! emulator answers with, so a handler that computed nothing could not pass them.
//!
//! The two rules under test, each asserted over a TABLE spanning the class it generalises
//! over rather than over one request:
//!
//! * a `DBProxyName` that names no proxy of the account in the region — a name whose proxy
//!   this service has taken away after the delete it accepted for it, as much as a name
//!   nothing was ever created under — "doesn't correspond to a proxy owned by your Amazon
//!   Web Services account in the specified Amazon Web Services Region"
//!   (`DBProxyNotFoundFault`), so EVERY operation that resolves a proxy by name answers
//!   HTTP 404 with that fault and the proxy's own not-found sentence, acting operations
//!   and reporting ones alike: no mutation is applied to a proxy that is not there, no
//!   further delete is accepted for it, and the name is free for a new proxy;
//! * a proxy operation that needs the proxy USABLE and finds it in another state answers
//!   HTTP 400 `InvalidDBProxyStateFault` naming both the state the proxy is in and the
//!   state it has to reach — `DB Proxy <name> is in an unsupported state - <STATE>, needs
//!   to be in [ACTIVE]` — so a caller that parses the required-state list out of the
//!   refusal sees what to wait for.
//!
//! A transition this service owes is waited for by polling a REPORTING call through the
//! poll loop this test target shares (`lifecycle_poll_loops::poll_until`), so the
//! CONDITION is what ends every wait below; the rows that assert a state refusal make
//! their call straight after the one that put the proxy in that state, and wait for
//! nothing.

use service_com_amazonaws_rds_amazonrdsv19::state::{
    db_proxy_not_found_message, db_proxy_unsupported_state_message, DB_PROXY_STATUS_AVAILABLE,
    DB_PROXY_STATUS_CREATING, DB_PROXY_STATUS_DELETING, DEFAULT_DB_PROXY_TARGET_GROUP,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{call, emulator, expect_error, expect_ok, text};
use crate::db_proxy_target_groups::create_proxy;
use crate::lifecycle_poll_loops::poll_until;

/// One call that resolves a proxy by name: the operation and the members it carries beyond
/// `DBProxyName`.
struct ProxyCall {
    case: &'static str,
    action: &'static str,
    extra: &'static [(&'static str, &'static str)],
}

/// One row of the removed-proxy table: a proxy, and how far through its lifecycle it had
/// got when its delete arrived.
struct RemovedProxyCase {
    case: &'static str,
    proxy: &'static str,
    engine_family: &'static str,
    /// Whether the proxy was polled to `available` before its delete, so the rows differ
    /// along the state the proxy left in as well as along what it was created as.
    ready_first: bool,
}

/// One row of the state-refusal table: the proxy the call is made against, whether its
/// delete has been accepted when the call arrives, the call itself, and the state its
/// refusal must name.
struct StateRefusalCase {
    case: &'static str,
    proxy: &'static str,
    engine_family: &'static str,
    /// `true`: the proxy's delete is accepted first, so the call arrives on a proxy on its
    /// way out. `false`: the call arrives while the proxy is still coming up.
    deleted_first: bool,
    action: &'static str,
    extra: &'static [(&'static str, &'static str)],
    /// The `DBProxyStatus` the proxy reports when the call arrives, which the refusal names
    /// in the proxy API's own spelling.
    status: &'static str,
}

/// Every operation of this service that resolves a proxy by `DBProxyName`, acting ones and
/// reporting ones, each with the members it needs besides the proxy's name.
///
/// The rows differ along what the rule generalises over: whether the call would READ the
/// proxy or CHANGE it, whether it addresses the proxy itself, its one target group or the
/// targets in that group, and — for the two modifications — what the request states.
const CALLS_THAT_RESOLVE_A_PROXY: &[ProxyCall] = &[
    ProxyCall {
        case: "a modification",
        action: "ModifyDBProxy",
        extra: &[("DebugLogging", "true")],
    },
    ProxyCall {
        case: "a rename",
        action: "ModifyDBProxy",
        extra: &[("NewDBProxyName", "emu-proxygone-renamed")],
    },
    ProxyCall {
        case: "a delete",
        action: "DeleteDBProxy",
        extra: &[],
    },
    ProxyCall {
        case: "a report of the proxy",
        action: "DescribeDBProxies",
        extra: &[],
    },
    ProxyCall {
        case: "a report of its target groups",
        action: "DescribeDBProxyTargetGroups",
        extra: &[],
    },
    ProxyCall {
        case: "a report of its targets",
        action: "DescribeDBProxyTargets",
        extra: &[],
    },
    ProxyCall {
        case: "a modification of its target group",
        action: "ModifyDBProxyTargetGroup",
        extra: &[
            ("TargetGroupName", DEFAULT_DB_PROXY_TARGET_GROUP),
            ("ConnectionPoolConfig.MaxConnectionsPercent", "50"),
        ],
    },
    ProxyCall {
        case: "a target registration",
        action: "RegisterDBProxyTargets",
        extra: &[("DBInstanceIdentifiers.member.1", "emu-proxygone-db")],
    },
    ProxyCall {
        case: "a target deregistration",
        action: "DeregisterDBProxyTargets",
        extra: &[("DBInstanceIdentifiers.member.1", "emu-proxygone-db")],
    },
];

/// The proxies whose removal the table below reads back, each of a different `EngineFamily`
/// (`smithy_query(query="shape", shape="EngineFamily")`: `MYSQL | POSTGRESQL | SQLSERVER`)
/// and each deleted from a different point in its lifecycle.
const REMOVED_PROXY_CASES: &[RemovedProxyCase] = &[
    RemovedProxyCase {
        case: "a MySQL proxy deleted once it was ready",
        proxy: "emu-proxygone-ready",
        engine_family: "MYSQL",
        ready_first: true,
    },
    RemovedProxyCase {
        case: "a PostgreSQL proxy deleted while it was still coming up",
        proxy: "emu-proxygone-creating",
        engine_family: "POSTGRESQL",
        ready_first: false,
    },
    RemovedProxyCase {
        case: "a SQL Server proxy deleted while it was still coming up",
        proxy: "emu-proxygone-sqlserver",
        engine_family: "SQLSERVER",
        ready_first: false,
    },
];

/// The state refusals: each of the operations that needs the proxy usable, met by a proxy
/// that is not — one still coming up (the state a caller hits right after a create) and
/// one on its way out (the state a caller hits right after a delete) — so the rows differ
/// along both the operation and the state it is refused in.
const STATE_REFUSAL_CASES: &[StateRefusalCase] = &[
    StateRefusalCase {
        case: "a modification of a proxy that is still coming up",
        proxy: "emu-proxystate-creating",
        engine_family: "MYSQL",
        deleted_first: false,
        action: "ModifyDBProxy",
        extra: &[("DebugLogging", "true")],
        status: DB_PROXY_STATUS_CREATING,
    },
    StateRefusalCase {
        case: "a rename of a proxy that is still coming up",
        proxy: "emu-proxystate-renaming",
        engine_family: "SQLSERVER",
        deleted_first: false,
        action: "ModifyDBProxy",
        extra: &[("NewDBProxyName", "emu-proxystate-renamed")],
        status: DB_PROXY_STATUS_CREATING,
    },
    StateRefusalCase {
        case: "a modification of a proxy whose delete has been accepted",
        proxy: "emu-proxystate-deleting",
        engine_family: "POSTGRESQL",
        deleted_first: true,
        action: "ModifyDBProxy",
        extra: &[("IdleClientTimeout", "900")],
        status: DB_PROXY_STATUS_DELETING,
    },
    StateRefusalCase {
        case: "a target-group modification of a proxy whose delete has been accepted",
        proxy: "emu-proxystate-group",
        engine_family: "MYSQL",
        deleted_first: true,
        action: "ModifyDBProxyTargetGroup",
        extra: &[
            ("TargetGroupName", DEFAULT_DB_PROXY_TARGET_GROUP),
            ("ConnectionPoolConfig.MaxConnectionsPercent", "50"),
        ],
        status: DB_PROXY_STATUS_DELETING,
    },
    StateRefusalCase {
        case: "a target registration against a proxy whose delete has been accepted",
        proxy: "emu-proxystate-register",
        engine_family: "MYSQL",
        deleted_first: true,
        action: "RegisterDBProxyTargets",
        extra: &[("DBInstanceIdentifiers.member.1", "emu-proxystate-db")],
        status: DB_PROXY_STATUS_DELETING,
    },
    StateRefusalCase {
        case: "a target deregistration against a proxy whose delete has been accepted",
        proxy: "emu-proxystate-deregister",
        engine_family: "POSTGRESQL",
        deleted_first: true,
        action: "DeregisterDBProxyTargets",
        extra: &[("DBInstanceIdentifiers.member.1", "emu-proxystate-db")],
        status: DB_PROXY_STATUS_DELETING,
    },
];

/// One call of `row` against `proxy`, answered with the HTTP status and the body.
async fn call_for(
    router: &MultiServiceRouter,
    proxy: &str,
    action: &str,
    extra: &[(&str, &str)],
) -> (u16, String) {
    let mut params = vec![("DBProxyName", proxy)];
    params.extend_from_slice(extra);
    call(router, action, &params).await
}

/// `DescribeDBProxies` for `proxy`, answered with the HTTP status and the body.
async fn describe_proxy(router: &MultiServiceRouter, proxy: &str) -> (u16, String) {
    call(router, "DescribeDBProxies", &[("DBProxyName", proxy)]).await
}

/// Poll the report of `proxy` until it says the proxy is `available`.
async fn poll_until_available(router: &MultiServiceRouter, case: &str, proxy: &str) {
    poll_until(
        case,
        "the proxy",
        || describe_proxy(router, proxy),
        |status, body| status == 200 && text(body, "Status") == DB_PROXY_STATUS_AVAILABLE,
    )
    .await;
}

/// Poll the report of `proxy` until the name resolves no proxy at all — the removal this
/// service owes once a delete has been accepted.
async fn poll_until_removed(router: &MultiServiceRouter, case: &str, proxy: &str) {
    poll_until(
        case,
        "the removal of the deleted proxy",
        || describe_proxy(router, proxy),
        |status, _| status == 404,
    )
    .await;
}

#[tokio::test]
async fn every_call_for_a_removed_proxy_is_the_proxys_own_not_found_fault() {
    let router = emulator();
    // A bystander, so every row is shown to have taken only the proxy it named.
    create_proxy(&router, "emu-proxygone-bystander", "MYSQL").await;

    for row in REMOVED_PROXY_CASES {
        create_proxy(&router, row.proxy, row.engine_family).await;
        if row.ready_first {
            poll_until_available(&router, row.case, row.proxy).await;
        }
        expect_ok(
            row.case,
            call(&router, "DeleteDBProxy", &[("DBProxyName", row.proxy)]).await,
        );
        // The removal this service owes the proxy is waited for by polling its report, so
        // the rows below run against a name this service holds no proxy under.
        poll_until_removed(&router, row.case, row.proxy).await;

        let expected_message = db_proxy_not_found_message(row.proxy);
        for proxy_call in CALLS_THAT_RESOLVE_A_PROXY {
            let case = format!("{}: {}", row.case, proxy_call.case);
            let refused = expect_error(
                &case,
                call_for(&router, row.proxy, proxy_call.action, proxy_call.extra).await,
                404,
                "DBProxyNotFoundFault",
            );
            let message = text(&refused, "Message");
            assert_eq!(
                message, expected_message,
                "case {case:?}: expected {} to carry {expected_message:?}, got {message:?} \
                 in {refused}",
                proxy_call.action
            );
        }

        // Nothing the refused calls named came back: neither the proxy under the name a
        // rename would have moved it to, nor the proxy itself.
        for name in [row.proxy, "emu-proxygone-renamed"] {
            let (status, body) = describe_proxy(&router, name).await;
            assert_eq!(
                status, 404,
                "case {:?}: {name} must resolve no proxy after the refusals, got {status} \
                 {body}",
                row.case
            );
        }

        // And the name is free: a proxy created under it afterwards is a NEW proxy, which
        // is what a teardown that re-creates what it deleted depends on.
        create_proxy(&router, row.proxy, row.engine_family).await;
        let recreated = expect_ok(row.case, describe_proxy(&router, row.proxy).await);
        let status = text(&recreated, "Status");
        assert_eq!(
            status, DB_PROXY_STATUS_CREATING,
            "case {:?}: the proxy created under the freed name must be coming up, got \
             {status:?} in {recreated}",
            row.case
        );
    }

    // Every delete above took only the proxy it named.
    let (status, bystander) = describe_proxy(&router, "emu-proxygone-bystander").await;
    assert_eq!(
        status, 200,
        "deleting one proxy must leave another alone, got {status} {bystander}"
    );
}

#[tokio::test]
async fn a_call_a_proxys_state_cannot_serve_is_refused_in_aws_words() {
    let router = emulator();

    for row in STATE_REFUSAL_CASES {
        create_proxy(&router, row.proxy, row.engine_family).await;
        if row.deleted_first {
            let accepted = expect_ok(
                row.case,
                call(&router, "DeleteDBProxy", &[("DBProxyName", row.proxy)]).await,
            );
            let reported = text(&accepted, "Status");
            assert_eq!(
                reported, DB_PROXY_STATUS_DELETING,
                "case {:?}: the accepted delete must leave the proxy on its way out, got \
                 {reported:?} in {accepted}",
                row.case
            );
        }

        let refused = expect_error(
            row.case,
            call_for(&router, row.proxy, row.action, row.extra).await,
            400,
            "InvalidDBProxyStateFault",
        );
        // The sentence is written out here, not read from the service: its fixed text is
        // what a caller matches on, and its variable parts are the name the REQUEST carried
        // and the state the proxy is in, named as the proxy API names it.
        let state = if row.status == DB_PROXY_STATUS_AVAILABLE {
            "ACTIVE".to_string()
        } else {
            row.status.to_ascii_uppercase()
        };
        let expected = format!(
            "DB Proxy {} is in an unsupported state - {state}, needs to be in [ACTIVE]",
            row.proxy
        );
        let message = text(&refused, "Message");
        assert_eq!(
            message, expected, "case {:?}: expected {expected:?}, got {message:?} in {refused}",
            row.case
        );
    }
}

/// Every `DBProxyStatus` the model publishes, with the name the proxy API's refusals give
/// it: the status in upper case, the ready status named `ACTIVE` — the spelling the
/// required-state list of the unsupported-state refusal uses for it — and the hyphen of a
/// two-word status written as an underscore.
///
/// The catalog is the whole of `smithy_query(query="shape", shape="DBProxyStatus")`, so the
/// statuses no lifecycle of this emulator reaches are covered here too.
const STATE_NAMES: &[(&str, &str)] = &[
    ("available", "ACTIVE"),
    ("modifying", "MODIFYING"),
    ("incompatible-network", "INCOMPATIBLE_NETWORK"),
    ("insufficient-resource-limits", "INSUFFICIENT_RESOURCE_LIMITS"),
    ("creating", "CREATING"),
    ("deleting", "DELETING"),
    ("suspended", "SUSPENDED"),
    ("suspending", "SUSPENDING"),
    ("reactivating", "REACTIVATING"),
];

#[test]
fn the_state_refusal_names_every_published_status_as_the_proxy_api_does() {
    for (status, state) in STATE_NAMES {
        let expected =
            format!("DB Proxy emu-proxystate-any is in an unsupported state - {state}, needs to be in [ACTIVE]");
        let actual = db_proxy_unsupported_state_message("emu-proxystate-any", status);
        assert_eq!(
            actual, expected,
            "case {status:?}: expected {expected:?}, got {actual:?}"
        );
    }
}
