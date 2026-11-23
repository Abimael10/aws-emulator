// SPDX-License-Identifier: CC0-1.0

//! The Classic Load Balancers a CREATE associates a group with have to exist.
//!
//! THE RULE UNDER TEST: every name a request's `LoadBalancerNames` carries — "a list of
//! Classic Load Balancers associated with this Auto Scaling group"
//! (`smithy_query(query="doc",shape="CreateAutoScalingGroupType",member="LoadBalancerNames")`)
//! — has to resolve to a Classic Load Balancer the account holds in the Region the request is
//! signed for, because the association is what registers the group's instances with that load
//! balancer. A request naming one that resolves to nothing is refused WHOLE — HTTP 400 with
//! the `ValidationError` sender fault and the service's fixed sentence pair — and nothing is
//! created, so a `DescribeAutoScalingGroups` for that group name then reports no group at all.
//! A request whose every name resolves is served, and the group carries exactly those names.
//!
//! The rows span what the rule generalises over: how many names the create carries (none, one,
//! two, three), whether each resolves, WHERE the one that does not fails (never created, held
//! in another Region, deleted since), and the POSITION of the unresolvable one in the list
//! (first, last, alone).
//!
//! Auto Scaling, Amazon EC2 and Elastic Load Balancing (Classic) are mounted on ONE engine and
//! driven over their own wires: the load balancers a row's create names are the ones a client
//! created through Elastic Load Balancing itself, so a name this suite calls resolvable is one
//! the owning service really holds.

use smithy_server_runtime::MultiServiceRouter;

use crate::support::{
    borrowed, classic_elb_query, create_classic_load_balancer, create_launch_template,
    element_body, emulator_with_classic_elb, p, query, tag_value, tag_values, REGION,
};

/// The launch template every group here launches from, created over Amazon EC2's own wire.
const TEMPLATE: &str = "lt-clb-resolution";

/// The Availability Zone every group here places instances in and every load balancer here is
/// enabled for.
const ZONE: &str = "us-west-2a";

/// A Region other than the one the creates are signed for, used by the row whose load balancer
/// exists — but not where the group does.
const OTHER_REGION: &str = "us-east-1";
const OTHER_REGION_ZONE: &str = "us-east-1a";

/// The load balancers the account HOLDS in the signed Region, created once per emulator.
const HELD: [&str; 3] = ["clb-held-a", "clb-held-b", "clb-held-c"];

/// A name no load balancer of the account ever answered to.
const NEVER_CREATED: &str = "clb-never-created";

/// A load balancer created in [`OTHER_REGION`] only: a Classic Load Balancer is identified
/// within one Region, so this name resolves to nothing for a group of the signed Region.
const OTHER_REGION_ONLY: &str = "clb-other-region";

/// A load balancer the account created in the signed Region and then DELETED, so the name
/// answers to nothing from that moment on.
const DELETED: &str = "clb-deleted";

/// The sentence a request naming Classic Load Balancers that cannot be resolved is refused
/// with: fixed text throughout, naming no value the request carried.
const UNRESOLVABLE_LOAD_BALANCERS: &str =
    "Provided Load Balancers may not be valid. Please ensure they exist and try again.";

/// The wire error code of the sender fault this class of refusal uses.
const VALIDATION_ERROR: &str = "ValidationError";

/// Post one `CreateAutoScalingGroup` naming `names` as the group's `LoadBalancerNames`, and
/// answer its status, the wire error code and the message.
async fn create_group(
    router: &MultiServiceRouter,
    group: &str,
    names: &[&str],
) -> (u16, String, String) {
    let mut params = vec![
        p("AutoScalingGroupName", group),
        p("LaunchTemplate.LaunchTemplateName", TEMPLATE),
        p("AvailabilityZones.member.1", ZONE),
        p("MinSize", "0"),
        p("MaxSize", "0"),
        p("DesiredCapacity", "0"),
    ];
    for (index, name) in names.iter().enumerate() {
        params.push(p(&format!("LoadBalancerNames.member.{}", index + 1), *name));
    }
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    (status, tag_value(&body, "Code"), tag_value(&body, "Message"))
}

/// The `LoadBalancerNames` the account's own read reports for `group`, and `None` when the
/// account holds no group under that name at all.
async fn described_load_balancer_names(
    router: &MultiServiceRouter,
    group: &str,
) -> Option<Vec<String>> {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeAutoScalingGroups for {group:?} expected 200, got {status}: {body}"
    );
    if !tag_values(&body, "AutoScalingGroupName")
        .iter()
        .any(|held| held == group)
    {
        return None;
    }
    // Scoped to the member's own element through the shared reader, which tolerates the
    // `xmlns` the envelope stamps on it, so an answer that reports the member differently
    // fails the case instead of reading as an empty list.
    let inner = element_body(&body, "LoadBalancerNames").unwrap_or_else(|| {
        panic!(
            "case {group:?}: expected the described group to name LoadBalancerNames, got {body}"
        )
    });
    Some(tag_values(inner, "member"))
}

/// One row: the names a create carries, and whether the create must be served.
struct Row {
    case: &'static str,
    names: Vec<&'static str>,
    accepted: bool,
}

/// Every row of the table, built from the names this suite's account holds and the three ways
/// a name resolves to nothing.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "no load balancer names at all",
            names: vec![],
            accepted: true,
        },
        Row {
            case: "one name the account holds",
            names: vec![HELD[0]],
            accepted: true,
        },
        Row {
            case: "two names the account holds",
            names: vec![HELD[0], HELD[1]],
            accepted: true,
        },
        Row {
            case: "every name the account holds",
            names: vec![HELD[0], HELD[1], HELD[2]],
            accepted: true,
        },
        Row {
            case: "one name no load balancer ever answered to",
            names: vec![NEVER_CREATED],
            accepted: false,
        },
        Row {
            case: "a held name followed by one nothing answers to",
            names: vec![HELD[0], NEVER_CREATED],
            accepted: false,
        },
        Row {
            case: "a name nothing answers to before a held one",
            names: vec![NEVER_CREATED, HELD[1]],
            accepted: false,
        },
        Row {
            case: "a load balancer held in another Region",
            names: vec![OTHER_REGION_ONLY],
            accepted: false,
        },
        Row {
            case: "a load balancer this account has deleted",
            names: vec![DELETED],
            accepted: false,
        },
        Row {
            case: "two held names beside one the account has deleted",
            names: vec![HELD[1], DELETED, HELD[2]],
            accepted: false,
        },
    ]
}

/// Build the account every row is taken against: the launch template the groups launch from,
/// the load balancers the account holds in the signed Region, the one it holds only in another
/// Region, and the one it created and deleted again.
async fn account() -> MultiServiceRouter {
    let (router, _engine) = emulator_with_classic_elb();
    create_launch_template(&router, TEMPLATE).await;
    for name in HELD {
        create_classic_load_balancer(&router, REGION, ZONE, name).await;
    }
    create_classic_load_balancer(&router, OTHER_REGION, OTHER_REGION_ZONE, OTHER_REGION_ONLY)
        .await;
    create_classic_load_balancer(&router, REGION, ZONE, DELETED).await;
    let (status, body) = classic_elb_query(
        &router,
        REGION,
        "DeleteLoadBalancer",
        &[p("LoadBalancerName", DELETED)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DeleteLoadBalancer for {DELETED:?} expected 200, got {status}: {body}"
    );
    router
}

/// A create is held to the load balancers it names: every name resolves and the group carries
/// them, or the create is refused whole and no group is left behind.
#[tokio::test]
async fn a_create_resolves_every_classic_load_balancer_name_it_carries() {
    let router = account().await;

    for (index, row) in rows().iter().enumerate() {
        let group = format!("asg-clb-resolution-{index}");
        let (status, code, message) = create_group(&router, &group, &row.names).await;
        let described = described_load_balancer_names(&router, &group).await;
        if row.accepted {
            assert_eq!(
                status, 200,
                "case {:?}: expected the create to be served, got {status} {code:?} {message:?}",
                row.case
            );
            let held = described.unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected the account to hold the group {group:?}, it holds none",
                    row.case
                )
            });
            let carried: Vec<&str> = held.iter().map(String::as_str).collect();
            assert_eq!(
                carried, row.names,
                "case {:?}: expected the group to carry the load balancers {:?}, got {held:?}",
                row.case, row.names
            );
        } else {
            assert_eq!(
                status, 400,
                "case {:?}: expected the create to be refused with 400, got {status} {code:?} \
                 {message:?}",
                row.case
            );
            assert_eq!(
                code, VALIDATION_ERROR,
                "case {:?}: expected the {VALIDATION_ERROR} code, got {code:?} {message:?}",
                row.case
            );
            assert_eq!(
                message, UNRESOLVABLE_LOAD_BALANCERS,
                "case {:?}: expected the message {UNRESOLVABLE_LOAD_BALANCERS:?}, got {message:?}",
                row.case
            );
            assert!(
                described.is_none(),
                "case {:?}: a refused create must leave no group behind, the account holds \
                 {described:?} for {group:?}",
                row.case
            );
        }
    }
}

/// A refused create consumes NOTHING: the group name it named is free, so the same name is
/// created the moment its load balancers resolve — and the group then carries them.
#[tokio::test]
async fn a_refused_create_leaves_the_group_name_free() {
    let router = account().await;
    let group = "asg-clb-retried";

    let (status, code, message) = create_group(&router, group, &[HELD[0], NEVER_CREATED]).await;
    assert_eq!(
        status, 400,
        "the first create must be refused, got {status} {code:?} {message:?}"
    );
    assert_eq!(
        message, UNRESOLVABLE_LOAD_BALANCERS,
        "the first create must be refused for its load balancers, got {message:?}"
    );
    assert!(
        described_load_balancer_names(&router, group)
            .await
            .is_none(),
        "the refused create must leave no group behind"
    );

    let (status, code, message) = create_group(&router, group, &[HELD[0]]).await;
    assert_eq!(
        status, 200,
        "the retried create must be served, got {status} {code:?} {message:?}"
    );
    let held = described_load_balancer_names(&router, group)
        .await
        .expect("the retried create must leave the group behind");
    let carried: Vec<&str> = held.iter().map(String::as_str).collect();
    assert_eq!(
        carried,
        vec![HELD[0]],
        "the retried group must carry the load balancer it named, got {held:?}"
    );
}
