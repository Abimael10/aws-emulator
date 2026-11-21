// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what an accepted `DeleteDBProxy` leaves behind.
//!
//! Every call goes over the wire: the service is mounted on a `MultiServiceRouter` exactly
//! as the production router mounts it, and each request is a real awsQuery form POST
//! carrying the SigV4 credential scope the router keys on. The assertions read the XML the
//! emulator answers with, so a handler that computed nothing could not pass them.
//!
//! The rule under test: a delete is ACCEPTED rather than instantaneous — the proxy reports
//! `deleting` from the answer onwards — and the proxy is then RETIRED from this service, so
//! from that moment the name resolves nothing at all: it is gone from the report that lists
//! the region's proxies, every operation that resolves a `DBProxyName` answers the proxy's
//! own not-found fault (HTTP 404, `DBProxyNotFoundFault`, worded with the requested name as
//! its only variable part), everything the proxy held — its one target group, the
//! registrations in it and its tags — went with it, and the name is free for a new proxy,
//! which is a NEW proxy rather than the deleted one coming back. A name whose proxy is
//! still there, one on its way out included, is still taken.
//!
//! The tables below span what the rule generalises over: the `EngineFamily` the proxy was
//! created with, the region it was created in — a delete retires the proxy of the region the
//! request was signed for and leaves a proxy of the same name in another region alone — the
//! status the delete arrived at, and WHICH operation resolves the retired name.
//!
//! Every transition this service owes is waited for by polling a REPORTING call through the
//! poll loop this test target shares (`lifecycle_poll_loops::poll_until`), so the CONDITION
//! is what ends each wait and nothing here depends on how fast the host runs.

use service_com_amazonaws_rds_amazonrdsv19::state::{
    db_proxy_already_exists_message, db_proxy_not_found_message, DB_PROXY_STATUS_AVAILABLE,
    DB_PROXY_STATUS_CREATING, DB_PROXY_STATUS_DELETING, DEFAULT_DB_PROXY_TARGET_GROUP,
    DEFAULT_IDLE_CLIENT_TIMEOUT,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{call_in, emulator, expect_error, expect_ok, text, texts, REGION};
use crate::lifecycle_poll_loops::poll_until;

/// An IAM role ARN of the form `CreateDBProxy$RoleArn` takes.
const PROXY_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/rds-proxy";

/// A subnet id of the published form (`subnet-` plus eight hexadecimal characters).
const SUBNET_ID: &str = "subnet-0a1b2c3d";

/// A second region, so a row can show that a delete acts on the proxies of the region the
/// request was signed for and on no other (RDS is regional).
const OTHER_REGION: &str = "eu-west-1";

/// One row of the retirement table: the proxy deleted, where it lives, and the status its
/// delete arrives at.
struct RetirementCase {
    case: &'static str,
    proxy: &'static str,
    engine_family: &'static str,
    region: &'static str,
    /// Whether the proxy is polled to `available` before its delete, so the rows span both
    /// a proxy that was ready and one that was still coming up.
    ready_first: bool,
}

/// The rows differ along everything the rule generalises over: every value the
/// `EngineFamily` enum takes (`smithy_query(query="shape", shape="EngineFamily")`:
/// `MYSQL | POSTGRESQL | SQLSERVER`), the region the proxy was created in, and the status
/// its delete arrived at.
const RETIREMENT_CASES: &[RetirementCase] = &[
    RetirementCase {
        case: "a MySQL proxy deleted once it was ready",
        proxy: "emu-retire-mysql",
        engine_family: "MYSQL",
        region: REGION,
        ready_first: true,
    },
    RetirementCase {
        case: "a PostgreSQL proxy in another region deleted while still coming up",
        proxy: "emu-retire-postgres",
        engine_family: "POSTGRESQL",
        region: OTHER_REGION,
        ready_first: false,
    },
    RetirementCase {
        case: "a SQL Server proxy deleted once it was ready",
        proxy: "emu-retire-sqlserver",
        engine_family: "SQLSERVER",
        region: REGION,
        ready_first: true,
    },
];

/// One operation that resolves a `DBProxyName`: the action, and the parameters it carries
/// beyond that name.
struct ResolvingCall {
    action: &'static str,
    extra: &'static [(&'static str, &'static str)],
}

/// Every operation of this service whose input resolves a `DBProxyName` and that binds
/// `DBProxyNotFoundFault` (`smithy_query(query="operation", operation="...")` for each:
/// each one models that fault with `httpError` 404 and the awsQuery code
/// `DBProxyNotFoundFault`). The required members each one carries beyond the proxy name are
/// stated here, so a refusal cannot be an argument check standing in for the resolution:
/// the DB instance the registration rows name does not exist either, and the proxy is what
/// is resolved first.
const PROXY_RESOLVING_CALLS: &[ResolvingCall] = &[
    ResolvingCall {
        action: "DescribeDBProxies",
        extra: &[],
    },
    ResolvingCall {
        action: "DeleteDBProxy",
        extra: &[],
    },
    ResolvingCall {
        action: "ModifyDBProxy",
        extra: &[("IdleClientTimeout", "900")],
    },
    ResolvingCall {
        action: "DescribeDBProxyTargets",
        extra: &[],
    },
    ResolvingCall {
        action: "DescribeDBProxyTargetGroups",
        extra: &[],
    },
    ResolvingCall {
        action: "ModifyDBProxyTargetGroup",
        extra: &[("TargetGroupName", DEFAULT_DB_PROXY_TARGET_GROUP)],
    },
    ResolvingCall {
        action: "RegisterDBProxyTargets",
        extra: &[("DBInstanceIdentifiers.member.1", "emu-retire-db")],
    },
    ResolvingCall {
        action: "DeregisterDBProxyTargets",
        extra: &[("DBInstanceIdentifiers.member.1", "emu-retire-db")],
    },
];

/// `CreateDBProxy` in `region` with its required members plus `extra`, answered with the
/// body so a caller can read the proxy the create stored.
async fn create_proxy_in(
    router: &MultiServiceRouter,
    region: &str,
    name: &str,
    engine_family: &str,
    extra: &[(&str, &str)],
) -> String {
    let mut params = vec![
        ("DBProxyName", name),
        ("EngineFamily", engine_family),
        ("RoleArn", PROXY_ROLE_ARN),
        ("VpcSubnetIds.member.1", SUBNET_ID),
    ];
    params.extend_from_slice(extra);
    expect_ok(
        &format!("create proxy {name} in {region}"),
        call_in(router, region, "CreateDBProxy", &params).await,
    )
}

/// `DescribeDBProxies` in `region` for one proxy, answered with the status and the body.
async fn describe_proxy(
    router: &MultiServiceRouter,
    region: &str,
    proxy: &str,
) -> (u16, String) {
    call_in(
        router,
        region,
        "DescribeDBProxies",
        &[("DBProxyName", proxy)],
    )
    .await
}

/// The names `DescribeDBProxies` reports for `region` when it is asked for no proxy in
/// particular — one `DBProxyName` per listed proxy.
async fn listed_proxy_names(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
) -> Vec<String> {
    let body = expect_ok(
        case,
        call_in(router, region, "DescribeDBProxies", &[] as &[(&str, &str)]).await,
    );
    texts(&body, "DBProxyName")
}

/// Poll the report of `proxy` until it says the proxy is `available`.
async fn poll_until_available(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    proxy: &str,
) {
    poll_until(
        case,
        "the proxy",
        || describe_proxy(router, region, proxy),
        |status, body| status == 200 && text(body, "Status") == DB_PROXY_STATUS_AVAILABLE,
    )
    .await;
}

/// Poll the report of `proxy` until the name resolves no proxy, answering the refusal.
async fn poll_until_retired(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    proxy: &str,
) -> String {
    let (body, _polls) = poll_until(
        case,
        "the deleted proxy",
        || describe_proxy(router, region, proxy),
        |status, _| status == 404,
    )
    .await;
    body
}

/// `DeleteDBProxy` in `region` for `proxy`, asserted accepted with the proxy on its way out,
/// and answered with the body.
async fn accept_delete(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    proxy: &str,
) -> String {
    let answered = expect_ok(
        case,
        call_in(router, region, "DeleteDBProxy", &[("DBProxyName", proxy)]).await,
    );
    let reported = text(&answered, "Status");
    assert_eq!(
        reported, DB_PROXY_STATUS_DELETING,
        "case {case:?}: an accepted delete must answer Status \
         {DB_PROXY_STATUS_DELETING:?}, got {reported:?} in {answered}"
    );
    answered
}

/// Assert every operation that resolves a `DBProxyName` answers the proxy's own not-found
/// fault for `proxy` in `region`, worded with the requested name as its only variable part.
async fn assert_every_resolution_is_not_found(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    proxy: &str,
) {
    let expected_message = db_proxy_not_found_message(proxy);
    for resolving in PROXY_RESOLVING_CALLS {
        let mut params = vec![("DBProxyName", proxy)];
        params.extend_from_slice(resolving.extra);
        let refused = expect_error(
            &format!("case {case:?}: {} for a retired proxy", resolving.action),
            call_in(router, region, resolving.action, &params).await,
            404,
            "DBProxyNotFoundFault",
        );
        let reported = text(&refused, "Message");
        assert_eq!(
            reported, expected_message,
            "case {case:?}: expected {} to carry {expected_message:?}, got {reported:?} in \
             {refused}",
            resolving.action
        );
    }
}

#[tokio::test]
async fn a_retired_proxy_resolves_nothing_for_every_operation_that_names_one() {
    let router = emulator();

    // A bystander in each region, so every delete below is shown to take only the proxy it
    // named, and the region's report is shown to still report what it holds.
    create_proxy_in(&router, REGION, "emu-retire-bystander", "MYSQL", &[]).await;
    create_proxy_in(
        &router,
        OTHER_REGION,
        "emu-retire-bystander",
        "POSTGRESQL",
        &[],
    )
    .await;

    for row in RETIREMENT_CASES {
        let created =
            create_proxy_in(&router, row.region, row.proxy, row.engine_family, &[]).await;
        let arn = text(&created, "DBProxyArn");
        if row.ready_first {
            poll_until_available(&router, row.case, row.region, row.proxy).await;
        }

        // The proxy is in the region's report before its delete, which is what the
        // assertions after the delete are a change FROM.
        let before = listed_proxy_names(&router, row.case, row.region).await;
        assert!(
            before.iter().any(|name| name == row.proxy),
            "case {:?}: the region's report must carry the proxy before its delete, got \
             {before:?}",
            row.case
        );

        accept_delete(&router, row.case, row.region, row.proxy).await;
        poll_until_retired(&router, row.case, row.region, row.proxy).await;

        // The name resolves nothing at all: not for the report that lists the region's
        // proxies, and not for any operation that resolves a `DBProxyName`.
        let after = listed_proxy_names(&router, row.case, row.region).await;
        assert!(
            !after.iter().any(|name| name == row.proxy),
            "case {:?}: the region's report must not carry a retired proxy, got {after:?}",
            row.case
        );
        assert!(
            after.iter().any(|name| name == "emu-retire-bystander"),
            "case {:?}: retiring one proxy must leave the region's other proxies in its \
             report, got {after:?}",
            row.case
        );
        assert_every_resolution_is_not_found(&router, row.case, row.region, row.proxy).await;

        // The tag list the proxy carried went with it, so its ARN resolves no resource of
        // this service either — the fault the tag front door answers for an ARN of a
        // proxy family naming no stored proxy.
        expect_error(
            &format!("case {:?}: the tags of a retired proxy", row.case),
            call_in(
                &router,
                row.region,
                "ListTagsForResource",
                &[("ResourceName", arn.as_str())],
            )
            .await,
            404,
            "DBProxyNotFoundFault",
        );
    }

    // RDS is regional, and one name is a different proxy in each region: a delete signed
    // for one region retires that region's proxy and leaves the other region's alone.
    for region in [REGION, OTHER_REGION] {
        create_proxy_in(&router, region, "emu-retire-shared", "MYSQL", &[]).await;
    }
    accept_delete(
        &router,
        "the proxy of one region out of two holding the name",
        REGION,
        "emu-retire-shared",
    )
    .await;
    poll_until_retired(
        &router,
        "the proxy of one region out of two holding the name",
        REGION,
        "emu-retire-shared",
    )
    .await;
    let (status, elsewhere) = describe_proxy(&router, OTHER_REGION, "emu-retire-shared").await;
    assert_eq!(
        status, 200,
        "retiring a proxy must leave the proxy of that name in another region alone, got \
         {status} {elsewhere}"
    );
    poll_until_available(
        &router,
        "the proxy of the other region",
        OTHER_REGION,
        "emu-retire-shared",
    )
    .await;
}

#[tokio::test]
async fn a_retired_proxy_frees_its_name_and_takes_what_it_held_with_it() {
    let router = emulator();

    // A database for the proxy to have a registration to lose. A registration resolves the
    // DB instance by identifier alone, so the instance does not have to be up for the
    // proxy's target group to hold one.
    expect_ok(
        "the database the registration names",
        call_in(
            &router,
            REGION,
            "CreateDBInstance",
            &[
                ("DBInstanceIdentifier", "emu-retire-db"),
                ("Engine", "postgres"),
                ("DBInstanceClass", "db.t4g.micro"),
            ],
        )
        .await,
    );

    // A proxy carrying settings and a tag of its own, so what the re-created proxy reports
    // shows whether anything of the deleted one survived it.
    let created = create_proxy_in(
        &router,
        REGION,
        "emu-retire-reused",
        "MYSQL",
        &[
            ("RequireTLS", "true"),
            ("IdleClientTimeout", "900"),
            ("Tags.Tag.1.Key", "owner"),
            ("Tags.Tag.1.Value", "emu-retirement-suite"),
        ],
    )
    .await;
    let arn = text(&created, "DBProxyArn");
    poll_until_available(&router, "the proxy to be re-created", REGION, "emu-retire-reused")
        .await;

    // What the proxy holds before its delete: one registration in its one target group, and
    // the tag the create carried.
    expect_ok(
        "the registration the proxy holds",
        call_in(
            &router,
            REGION,
            "RegisterDBProxyTargets",
            &[
                ("DBProxyName", "emu-retire-reused"),
                ("DBInstanceIdentifiers.member.1", "emu-retire-db"),
            ],
        )
        .await,
    );
    let registered = expect_ok(
        "the targets the proxy holds",
        call_in(
            &router,
            REGION,
            "DescribeDBProxyTargets",
            &[("DBProxyName", "emu-retire-reused")],
        )
        .await,
    );
    let held = texts(&registered, "RdsResourceId");
    assert_eq!(
        held,
        vec!["emu-retire-db".to_string()],
        "case \"before the delete\": expected the proxy's target group to hold the \
         registration, got {held:?} in {registered}"
    );
    let tagged = expect_ok(
        "the tags the proxy holds",
        call_in(
            &router,
            REGION,
            "ListTagsForResource",
            &[("ResourceName", arn.as_str())],
        )
        .await,
    );
    assert_eq!(
        texts(&tagged, "Key"),
        vec!["owner".to_string()],
        "case \"before the delete\": expected the proxy to carry the tag the create \
         stated, got {tagged}"
    );

    accept_delete(
        &router,
        "the proxy whose name is re-used",
        REGION,
        "emu-retire-reused",
    )
    .await;

    // The name is still TAKEN while the proxy is on its way out: it is still a proxy of
    // this account in this region, so a create under it is the already-exists fault.
    let taken = expect_error(
        "a create under the name of a proxy that is still on its way out",
        call_in(
            &router,
            REGION,
            "CreateDBProxy",
            &[
                ("DBProxyName", "emu-retire-reused"),
                ("EngineFamily", "POSTGRESQL"),
                ("RoleArn", PROXY_ROLE_ARN),
                ("VpcSubnetIds.member.1", SUBNET_ID),
            ],
        )
        .await,
        400,
        "DBProxyAlreadyExistsFault",
    );
    let expected_message = db_proxy_already_exists_message("emu-retire-reused");
    let reported = text(&taken, "Message");
    assert_eq!(
        reported, expected_message,
        "case \"a create under the name of a proxy that is still on its way out\": expected \
         {expected_message:?}, got {reported:?} in {taken}"
    );

    poll_until_retired(
        &router,
        "the proxy whose name is re-used",
        REGION,
        "emu-retire-reused",
    )
    .await;

    // The name is free again, and what it names now is a NEW proxy: it takes the settings
    // THIS create stated and the documented defaults for the ones it left out, comes up
    // from the start of the lifecycle, and holds neither the registration nor the tag the
    // deleted proxy held.
    let recreated = create_proxy_in(
        &router,
        REGION,
        "emu-retire-reused",
        "POSTGRESQL",
        &[("Tags.Tag.1.Key", "owner"), ("Tags.Tag.1.Value", "someone-else")],
    )
    .await;
    for (member, expected) in [
        ("EngineFamily", "POSTGRESQL".to_string()),
        // The create stated neither, so each takes its documented default rather than the
        // value the deleted proxy of this name carried.
        ("RequireTLS", "false".to_string()),
        ("IdleClientTimeout", DEFAULT_IDLE_CLIENT_TIMEOUT.to_string()),
        ("Status", DB_PROXY_STATUS_CREATING.to_string()),
    ] {
        let actual = text(&recreated, member);
        assert_eq!(
            actual, expected,
            "case \"the re-created proxy\": expected {member} {expected:?}, got {actual:?} \
             in {recreated}"
        );
    }
    let targets = expect_ok(
        "the targets of the re-created proxy",
        call_in(
            &router,
            REGION,
            "DescribeDBProxyTargets",
            &[("DBProxyName", "emu-retire-reused")],
        )
        .await,
    );
    assert!(
        texts(&targets, "RdsResourceId").is_empty(),
        "case \"the re-created proxy\": a new proxy holds no registration of the deleted \
         one, got {targets}"
    );
    let new_arn = text(&recreated, "DBProxyArn");
    let new_tags = expect_ok(
        "the tags of the re-created proxy",
        call_in(
            &router,
            REGION,
            "ListTagsForResource",
            &[("ResourceName", new_arn.as_str())],
        )
        .await,
    );
    assert_eq!(
        texts(&new_tags, "Value"),
        vec!["someone-else".to_string()],
        "case \"the re-created proxy\": expected the tag THIS create stated, got {new_tags}"
    );

    // And the new proxy walks its own lifecycle through to ready.
    poll_until_available(&router, "the re-created proxy", REGION, "emu-retire-reused").await;
}
