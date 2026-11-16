// SPDX-License-Identifier: CC0-1.0

//! What `ModifyIpPools` requires a request to NAME.
//!
//! RULE: an operation that modifies a resource by naming the parts of it to change must be
//! told which part — `ModifyIpPools` is documented "Modify the IP pool associated to a load
//! balancer" and carries exactly two optional members, `IpamPools` (the pool to use) and
//! `RemoveIpamPools` (the address families to stop using one for), so a request naming
//! neither asks for no change at all and is refused with `ValidationError` (400) and the
//! sentence [`AT_LEAST_ONE_POOL_MEMBER_MESSAGE`], which names the two MODELLED members the
//! caller has to add. Nothing of the load balancer changes.
//!
//! The rows span what the rule generalises over: which of the two members the request
//! names, naming both at once, and naming neither — against a load balancer whose pool is
//! unset and one that already carries a pool, so a refused row is pinned to the REQUEST
//! rather than to the state it was sent against. Every accepted row is read back through
//! `DescribeLoadBalancers`, which is where a later caller sees the pool.

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::ops::modify_ip_pools::AT_LEAST_ONE_POOL_MEMBER_MESSAGE;

use crate::provisioning_loop::{action, call, p, Reply};
use crate::target_group_attributes_and_modification::unescaped;
use crate::wire::router;

/// The code this API answers an input it cannot accept with.
const VALIDATION_ERROR: &str = "ValidationError";

/// The one value `RemoveIpamPoolEnum` carries
/// (`smithy_query(query="shape",shape="RemoveIpamPoolEnum")`): the address family whose
/// pool a removal releases.
const IPV4: &str = "ipv4";

/// An IPAM pool id, numbered so two rows can name different pools.
fn pool(index: usize) -> String {
    format!("ipam-pool-{index:017x}")
}

/// Create an Application Load Balancer and answer its ARN.
async fn create_load_balancer(router: &MultiServiceRouter, name: &str) -> String {
    let mut form = action("CreateLoadBalancer");
    form.push(p("Name", name));
    form.push(p("Subnets.member.1", "subnet-11111111"));
    form.push(p("Subnets.member.2", "subnet-22222222"));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "case {name:?}: CreateLoadBalancer must answer 200, got {} {}",
        reply.status, reply.body
    );
    reply
        .text("LoadBalancerArn")
        .unwrap_or_else(|| panic!("case {name:?}: no LoadBalancerArn in {}", reply.body))
}

/// The IPv4 IPAM pool `DescribeLoadBalancers` reports for `lb_arn`, which is what a later
/// caller sees.
async fn reported_pool(router: &MultiServiceRouter, lb_arn: &str) -> Option<String> {
    let mut form = action("DescribeLoadBalancers");
    form.push(p("LoadBalancerArns.member.1", lb_arn));
    let reply = call(router, &form).await;
    reply.text("Ipv4IpamPoolId")
}

/// Call `ModifyIpPools` with the members a row names.
async fn modify(
    router: &MultiServiceRouter,
    lb_arn: &str,
    set: Option<&str>,
    remove: Option<&str>,
) -> Reply {
    let mut form = action("ModifyIpPools");
    form.push(p("LoadBalancerArn", lb_arn));
    if let Some(id) = set {
        form.push(p("IpamPools.Ipv4IpamPoolId", id));
    }
    if let Some(family) = remove {
        form.push(p("RemoveIpamPools.member.1", family));
    }
    call(router, &form).await
}

#[tokio::test]
async fn a_request_naming_neither_pool_member_is_refused_and_changes_nothing() {
    let router = router();
    // (case, the pool the load balancer carries before the refused call)
    let states: [(&str, Option<String>); 2] = [
        ("a load balancer with no pool", None),
        ("a load balancer that already carries a pool", Some(pool(1))),
    ];
    for (index, (case, carried)) in states.iter().enumerate() {
        let lb = create_load_balancer(&router, &format!("pools-none-{index}")).await;
        if let Some(id) = carried {
            let set = modify(&router, &lb, Some(id), None).await;
            assert_eq!(
                set.status, 200,
                "case {case:?}: naming a pool must be accepted, got {} {}",
                set.status, set.body
            );
        }

        let refused = modify(&router, &lb, None, None).await;
        let stated = unescaped(&refused.text("Message").unwrap_or_default());
        assert_eq!(
            (refused.status, refused.error_code(), stated.as_str()),
            (
                400,
                VALIDATION_ERROR.to_string(),
                AT_LEAST_ONE_POOL_MEMBER_MESSAGE
            ),
            "case {case:?}: expected 400 {VALIDATION_ERROR} \
             {AT_LEAST_ONE_POOL_MEMBER_MESSAGE:?}, got {}",
            refused.body
        );
        // An empty removal list names no address family, so it is a request naming
        // neither member too.
        let empty = modify(&router, &lb, None, Some("")).await;
        assert_eq!(
            empty.status, 400,
            "case {case:?}: a removal naming no family must be refused, got {} {}",
            empty.status, empty.body
        );

        assert_eq!(
            reported_pool(&router, &lb).await,
            carried.clone(),
            "case {case:?}: a refused call must leave the pool the load balancer carried"
        );
    }
}

#[tokio::test]
async fn a_request_naming_either_pool_member_is_applied() {
    let router = router();
    /// One row: how the case reads, the pool to set, the family to remove, and the pool a
    /// read must then report.
    type Row = (
        &'static str,
        Option<String>,
        Option<&'static str>,
        Option<String>,
    );
    // (case, the pool to set, the family to remove, the pool a read must then report)
    let rows: [Row; 4] = [
        (
            "naming a pool alone",
            Some(pool(2)),
            None,
            Some(pool(2)),
        ),
        (
            "naming a removal alone",
            None,
            Some(IPV4),
            None,
        ),
        (
            "naming both at once, which ends with the family using no pool",
            Some(pool(3)),
            Some(IPV4),
            None,
        ),
        (
            "naming another pool, which replaces the one it carried",
            Some(pool(4)),
            None,
            Some(pool(4)),
        ),
    ];
    for (index, (case, set, remove, expected)) in rows.iter().enumerate() {
        let lb = create_load_balancer(&router, &format!("pools-some-{index}")).await;
        // Every row starts from a load balancer that already carries a pool, so a row
        // naming a removal has something to remove and one naming a pool replaces one.
        let seeded = modify(&router, &lb, Some(&pool(9)), None).await;
        assert_eq!(
            seeded.status, 200,
            "case {case:?}: seeding a pool must be accepted, got {} {}",
            seeded.status, seeded.body
        );

        let reply = modify(&router, &lb, set.as_deref(), *remove).await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: expected 200, got {} {}",
            reply.status, reply.body
        );
        // The answer is what a later read reports, so both are asserted against the row.
        assert_eq!(
            (reply.text("Ipv4IpamPoolId"), reported_pool(&router, &lb).await),
            (expected.clone(), expected.clone()),
            "case {case:?}: expected the pool {expected:?} answered and reported, got {}",
            reply.body
        );
    }
}
