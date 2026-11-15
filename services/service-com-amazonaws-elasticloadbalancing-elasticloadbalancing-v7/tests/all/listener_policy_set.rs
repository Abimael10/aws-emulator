// SPDX-License-Identifier: CC0-1.0

//! The set of policies enabled on ONE listener of a Classic load balancer: what
//! `SetLoadBalancerPoliciesOfListener` writes, and what that written set then means for
//! `DeleteLoadBalancerPolicy` — end to end over the awsQuery wire.
//!
//! Every request goes through the awsQuery protocol on the multi-service router (see
//! `super::wire`), so what is asserted is the emulator's HTTP behaviour.
//!
//! The rules under test, each over a TABLE of inputs rather than one recorded request:
//!
//! * `SetLoadBalancerPoliciesOfListener` REPLACES the set of policies enabled on the
//!   listener at the `LoadBalancerPort` the request names with the distinct names its
//!   `PolicyNames` carries — "this list must include all policies to be enabled. If you
//!   omit a policy that is currently enabled, it is disabled. If the list is empty, all
//!   current policies are disabled" — and a later `DescribeLoadBalancers` reports exactly
//!   that set under that listener's `PolicyNames`, leaving every OTHER listener's set as
//!   it was.
//! * a request the operation cannot satisfy — a port no listener is configured at, a
//!   policy the load balancer does not hold, two session stickiness policies at once, a
//!   load balancer no record holds in the region the request is signed for — is refused
//!   with that failure's own modelled error and leaves the stored set as it was.
//! * while a policy is in a listener's set it is in use, so `DeleteLoadBalancerPolicy`
//!   refuses it `InvalidConfigurationRequest` and removes nothing; the same delete after
//!   the policy is no longer enabled succeeds.
//!
//! Every set is asserted on what a LATER describe reports, never on what the call
//! answered alone.

use super::wire::{
    call, emulator, error_message_bytes, kid, kid_text, member_texts, members, param, parse,
};
use smithy_server_runtime::MultiServiceRouter;

/// The region every request below is signed for unless the case names another.
const REGION: &str = "us-east-1";

/// A second region, for the rule that a load balancer is resolved in the region the
/// request is signed for.
const OTHER_REGION: &str = "eu-west-1";

/// The two ports the load balancers below carry a listener at: the set is written per
/// LISTENER, so every case that writes one port asserts the other is left alone.
const PORT: i32 = 80;
const OTHER_PORT: i32 = 8080;

/// A port no listener of these load balancers is configured at.
const UNUSED_PORT: i32 = 8443;

/// How a policy one of the cases below enables was created: each of the three operations
/// that create a policy, so a set is asserted over policies of every kind the service
/// holds rather than over stickiness alone.
#[derive(Clone, Copy, Debug)]
enum Created {
    /// `CreateLBCookieStickinessPolicy`, carrying `CookieExpirationPeriod`.
    LbCookie(&'static str),
    /// `CreateAppCookieStickinessPolicy`, carrying `CookieName`.
    AppCookie(&'static str),
    /// `CreateLoadBalancerPolicy`, of this `PolicyTypeName`.
    Generic(&'static str),
}

/// Create a load balancer named `name` in `region` with one HTTP listener at each of the
/// two ports above, failing the test if the create does not answer 200.
async fn create_load_balancer(router: &MultiServiceRouter, region: &str, name: &str) {
    let created = call(
        router,
        region,
        "CreateLoadBalancer",
        &[
            param("LoadBalancerName", name),
            param("AvailabilityZones.member.1", &format!("{region}a")),
            param("Listeners.member.1.Protocol", "HTTP"),
            param("Listeners.member.1.LoadBalancerPort", &PORT.to_string()),
            param("Listeners.member.1.InstancePort", "8080"),
            param("Listeners.member.2.Protocol", "HTTP"),
            param(
                "Listeners.member.2.LoadBalancerPort",
                &OTHER_PORT.to_string(),
            ),
            param("Listeners.member.2.InstancePort", "9090"),
        ],
    )
    .await;
    assert_eq!(
        created.status, 200,
        "creating the load balancer {name:?} in {region:?} must succeed, got {}",
        created.body
    );
}

/// Create the policy `policy_name` on `load_balancer_name` through the operation
/// `created` names, failing the test if the create does not answer 200.
async fn create_policy(
    router: &MultiServiceRouter,
    load_balancer_name: &str,
    policy_name: &str,
    created: Created,
) {
    let (action, mut params) = match created {
        Created::LbCookie(period) => (
            "CreateLBCookieStickinessPolicy",
            vec![param("CookieExpirationPeriod", period)],
        ),
        Created::AppCookie(cookie_name) => (
            "CreateAppCookieStickinessPolicy",
            vec![param("CookieName", cookie_name)],
        ),
        Created::Generic(policy_type_name) => (
            "CreateLoadBalancerPolicy",
            vec![param("PolicyTypeName", policy_type_name)],
        ),
    };
    params.insert(0, param("PolicyName", policy_name));
    params.insert(0, param("LoadBalancerName", load_balancer_name));
    let reply = call(router, REGION, action, &params).await;
    assert_eq!(
        reply.status, 200,
        "creating {policy_name:?} through {action} must answer 200, got {}",
        reply.body
    );
}

/// Ask `SetLoadBalancerPoliciesOfListener` to make `policy_names` the set enabled on the
/// listener at `port`.
async fn set_policies(
    router: &MultiServiceRouter,
    region: &str,
    load_balancer_name: &str,
    port: i32,
    policy_names: &[&str],
) -> super::wire::Wire {
    let mut params = vec![
        param("LoadBalancerName", load_balancer_name),
        param("LoadBalancerPort", &port.to_string()),
    ];
    if policy_names.is_empty() {
        // An awsQuery client sends an EMPTY list as the list's own field carrying no
        // value, there being no `member.N` to send — which is what "if the list is empty,
        // all current policies are disabled" is asked for over the wire.
        params.push(param("PolicyNames", ""));
    }
    for (index, policy_name) in policy_names.iter().enumerate() {
        params.push(param(
            &format!("PolicyNames.member.{}", index + 1),
            policy_name,
        ));
    }
    call(
        router,
        region,
        "SetLoadBalancerPoliciesOfListener",
        &params,
    )
    .await
}

/// The policy names `DescribeLoadBalancers` reports for the listener at `port`, sorted,
/// so a case asserts the SET the listener carries rather than a serialization order.
async fn described_listener_policies(
    router: &MultiServiceRouter,
    load_balancer_name: &str,
    port: i32,
) -> Vec<String> {
    let described = call(
        router,
        REGION,
        "DescribeLoadBalancers",
        &[param("LoadBalancerNames.member.1", load_balancer_name)],
    )
    .await;
    let doc = parse(&described.body);
    let result = described.result(&doc);
    let description = *members(result, "LoadBalancerDescriptions")
        .first()
        .unwrap_or_else(|| {
            panic!(
                "no description of {load_balancer_name} in {}",
                described.body
            )
        });
    let listener = members(description, "ListenerDescriptions")
        .into_iter()
        .find(|member| {
            kid(member, "Listener")
                .and_then(|listener| kid_text(listener, "LoadBalancerPort"))
                .is_some_and(|reported| reported == port.to_string())
        })
        .unwrap_or_else(|| {
            panic!(
                "no listener at port {port} of {load_balancer_name} in {}",
                described.body
            )
        });
    let mut reported = member_texts(listener, "PolicyNames");
    reported.sort();
    reported
}

/// The policy names `DescribeLoadBalancerPolicies` reports for `load_balancer_name`,
/// sorted: what the load balancer HOLDS, which a delete changes and a set does not.
async fn held_policies(router: &MultiServiceRouter, load_balancer_name: &str) -> Vec<String> {
    let described = call(
        router,
        REGION,
        "DescribeLoadBalancerPolicies",
        &[param("LoadBalancerName", load_balancer_name)],
    )
    .await;
    let doc = parse(&described.body);
    let result = described.result(&doc);
    let mut held: Vec<String> = members(result, "PolicyDescriptions")
        .into_iter()
        .filter_map(|member| kid_text(member, "PolicyName"))
        .collect();
    held.sort();
    held
}

/// Ask `DeleteLoadBalancerPolicy` to remove `policy_name` from `load_balancer_name`.
async fn delete_policy(
    router: &MultiServiceRouter,
    load_balancer_name: &str,
    policy_name: &str,
) -> super::wire::Wire {
    call(
        router,
        REGION,
        "DeleteLoadBalancerPolicy",
        &[
            param("LoadBalancerName", load_balancer_name),
            param("PolicyName", policy_name),
        ],
    )
    .await
}

/// `expected`, sorted, as a describe reports a set.
fn as_set(expected: &[&str]) -> Vec<String> {
    let mut set: Vec<String> = expected.iter().map(|name| name.to_string()).collect();
    set.sort();
    set
}

/// The policies every case below has the load balancer hold: one of each kind, so a set
/// can carry policies of more than one kind and a stickiness rule can be driven by two
/// policies that follow a cookie.
const POLICIES: &[(&str, Created)] = &[
    ("lb-sticky", Created::LbCookie("120")),
    ("app-sticky", Created::AppCookie("JSESSIONID")),
    // `PolicyTypeName` values this service predefines (`state::POLICY_TYPE_NAMES`, as
    // `DescribeLoadBalancerPolicyTypes` reports them).
    ("proxy-protocol", Created::Generic("ProxyProtocolPolicyType")),
    ("ssl-negotiation", Created::Generic("SSLNegotiationPolicyType")),
];

/// Create a load balancer named `name` holding every policy of [`POLICIES`].
async fn load_balancer_holding_every_policy(router: &MultiServiceRouter, name: &str) {
    create_load_balancer(router, REGION, name).await;
    for (policy_name, created) in POLICIES {
        create_policy(router, name, policy_name, *created).await;
    }
}

/// One row of the replacement table: the case, the `PolicyNames` the request carries, and
/// the set a later describe must then report for that listener.
struct SetCase {
    case: &'static str,
    requested: &'static [&'static str],
    enabled: &'static [&'static str],
}

/// A set REPLACES the listener's set with the distinct names the request carries, and
/// every other listener keeps its own.
#[tokio::test]
async fn a_set_replaces_the_policies_enabled_on_that_one_listener() {
    // The rows differ along what the rule generalises over: how many names the list
    // carries (none, one, several), which kinds they are, whether the new set drops,
    // keeps or adds a name the listener already carried, and whether the list repeats a
    // name — a set, so a name enabled twice is enabled once.
    let cases = [
        SetCase {
            case: "one policy on a listener carrying none",
            requested: &["lb-sticky"],
            enabled: &["lb-sticky"],
        },
        SetCase {
            case: "a second policy of another kind, added beside the first",
            requested: &["lb-sticky", "proxy-protocol"],
            enabled: &["lb-sticky", "proxy-protocol"],
        },
        SetCase {
            case: "the same two names in the other order",
            requested: &["proxy-protocol", "lb-sticky"],
            enabled: &["lb-sticky", "proxy-protocol"],
        },
        SetCase {
            case: "a set that drops one of the two and keeps the other",
            requested: &["proxy-protocol"],
            enabled: &["proxy-protocol"],
        },
        SetCase {
            case: "a set naming three policies at once",
            requested: &["app-sticky", "proxy-protocol", "ssl-negotiation"],
            enabled: &["app-sticky", "proxy-protocol", "ssl-negotiation"],
        },
        SetCase {
            case: "a list repeating one name: a set, so it is enabled once",
            requested: &["ssl-negotiation", "ssl-negotiation", "ssl-negotiation"],
            enabled: &["ssl-negotiation"],
        },
        SetCase {
            case: "a list repeating one of two names",
            requested: &["ssl-negotiation", "lb-sticky", "ssl-negotiation"],
            enabled: &["lb-sticky", "ssl-negotiation"],
        },
        SetCase {
            case: "an empty list: every current policy is disabled",
            requested: &[],
            enabled: &[],
        },
        SetCase {
            case: "a policy enabled again after the list emptied the set",
            requested: &["app-sticky"],
            enabled: &["app-sticky"],
        },
    ];

    let router = emulator();
    load_balancer_holding_every_policy(&router, "set-lb").await;

    // The OTHER listener is given a set of its own first, so every row below asserts
    // that writing one port leaves the other port's set untouched.
    let other = set_policies(
        &router,
        REGION,
        "set-lb",
        OTHER_PORT,
        &["ssl-negotiation", "app-sticky"],
    )
    .await;
    assert_eq!(
        other.status, 200,
        "the set on port {OTHER_PORT} must answer 200, got {}",
        other.body
    );

    for case in &cases {
        let reply = set_policies(&router, REGION, "set-lb", PORT, case.requested).await;
        assert_eq!(
            reply.status, 200,
            "case {:?}: the set must answer 200, got {} with {}",
            case.case, reply.status, reply.body
        );

        let reported = described_listener_policies(&router, "set-lb", PORT).await;
        assert_eq!(
            reported,
            as_set(case.enabled),
            "case {:?}: the describe must report exactly the enabled set, got {reported:?}",
            case.case
        );

        // A describe is a report: reading twice reports the same set.
        let again = described_listener_policies(&router, "set-lb", PORT).await;
        assert_eq!(
            again, reported,
            "case {:?}: a second describe must report the same set, got {again:?}",
            case.case
        );

        let untouched = described_listener_policies(&router, "set-lb", OTHER_PORT).await;
        assert_eq!(
            untouched,
            as_set(&["app-sticky", "ssl-negotiation"]),
            "case {:?}: the set on port {OTHER_PORT} must be left as it was, got \
             {untouched:?}",
            case.case
        );

        // Enabling a policy does not create or remove one: the load balancer holds the
        // same policies throughout.
        let held = held_policies(&router, "set-lb").await;
        assert_eq!(
            held,
            as_set(&POLICIES.iter().map(|(name, _)| *name).collect::<Vec<_>>()),
            "case {:?}: the policies the load balancer holds must not change, got \
             {held:?}",
            case.case
        );
    }
}

/// One row of the refusal table: the case, the region the request is signed for, the
/// load balancer name and port it asks about, the names it carries, and the status and
/// error code it must be refused with.
struct RefusalCase {
    case: &'static str,
    region: &'static str,
    load_balancer_name: &'static str,
    port: i32,
    requested: &'static [&'static str],
    status: u16,
    code: &'static str,
}

/// A request the operation cannot satisfy is refused with that failure's own modelled
/// error, and the stored set is left as it was.
#[tokio::test]
async fn a_set_the_operation_cannot_satisfy_is_refused_and_writes_nothing() {
    let router = emulator();
    load_balancer_holding_every_policy(&router, "refusal-lb").await;

    // The set every refusal below must leave in place.
    let before = set_policies(
        &router,
        REGION,
        "refusal-lb",
        PORT,
        &["lb-sticky", "proxy-protocol"],
    )
    .await;
    assert_eq!(
        before.status, 200,
        "the set under test must answer 200, got {}",
        before.body
    );

    let cases = [
        RefusalCase {
            case: "a load balancer name nothing created",
            region: REGION,
            load_balancer_name: "no-such-lb",
            port: PORT,
            requested: &["lb-sticky"],
            status: 400,
            code: "LoadBalancerNotFound",
        },
        RefusalCase {
            case: "a name held only in another region",
            region: OTHER_REGION,
            load_balancer_name: "refusal-lb",
            port: PORT,
            requested: &["lb-sticky"],
            status: 400,
            code: "LoadBalancerNotFound",
        },
        RefusalCase {
            case: "a port no listener is configured at",
            region: REGION,
            load_balancer_name: "refusal-lb",
            port: UNUSED_PORT,
            requested: &["lb-sticky"],
            status: 400,
            code: "ListenerNotFound",
        },
        RefusalCase {
            case: "a policy the load balancer does not hold",
            region: REGION,
            load_balancer_name: "refusal-lb",
            port: PORT,
            requested: &["no-such-policy"],
            status: 400,
            code: "PolicyNotFound",
        },
        RefusalCase {
            case: "one unknown policy among names the load balancer does hold",
            region: REGION,
            load_balancer_name: "refusal-lb",
            port: PORT,
            requested: &["ssl-negotiation", "no-such-policy", "proxy-protocol"],
            status: 400,
            code: "PolicyNotFound",
        },
        RefusalCase {
            case: "two session stickiness policies at once",
            region: REGION,
            load_balancer_name: "refusal-lb",
            port: PORT,
            requested: &["lb-sticky", "app-sticky"],
            status: 409,
            code: "InvalidConfigurationRequest",
        },
        RefusalCase {
            case: "two session stickiness policies among others",
            region: REGION,
            load_balancer_name: "refusal-lb",
            port: PORT,
            requested: &["ssl-negotiation", "app-sticky", "lb-sticky"],
            status: 409,
            code: "InvalidConfigurationRequest",
        },
    ];

    for case in &cases {
        let refused = set_policies(
            &router,
            case.region,
            case.load_balancer_name,
            case.port,
            case.requested,
        )
        .await;
        assert_eq!(
            (refused.status, refused.error_code().as_str()),
            (case.status, case.code),
            "case {:?}: the set must be refused {} {}, got {} with {}",
            case.case,
            case.status,
            case.code,
            refused.status,
            refused.body
        );

        let reported = described_listener_policies(&router, "refusal-lb", PORT).await;
        assert_eq!(
            reported,
            as_set(&["lb-sticky", "proxy-protocol"]),
            "case {:?}: a refused set must leave the stored set as it was, got \
             {reported:?}",
            case.case
        );
    }
}

/// A policy in a listener's set is in use: the delete is refused with the one sentence
/// this service states for a policy that is still enabled, and the policy stays.
#[tokio::test]
async fn a_policy_a_listener_still_enables_cannot_be_deleted() {
    let router = emulator();
    load_balancer_holding_every_policy(&router, "in-use-lb").await;

    // The rows span the three operations that create a policy and both listeners, so the
    // refusal is the policy's being enabled anywhere rather than one port's or one kind's.
    let cases: &[(&str, &str, i32)] = &[
        ("a duration-based stickiness policy", "lb-sticky", PORT),
        (
            "an application-controlled stickiness policy",
            "app-sticky",
            OTHER_PORT,
        ),
        ("a Proxy Protocol policy", "proxy-protocol", PORT),
        ("an SSL negotiation policy", "ssl-negotiation", OTHER_PORT),
    ];

    for (case, policy_name, port) in cases {
        let enabled = set_policies(&router, REGION, "in-use-lb", *port, &[policy_name]).await;
        assert_eq!(
            enabled.status, 200,
            "case {case:?}: enabling the policy must answer 200, got {}",
            enabled.body
        );

        let refused = delete_policy(&router, "in-use-lb", policy_name).await;
        assert_eq!(
            (refused.status, refused.error_code().as_str()),
            (409, "InvalidConfigurationRequest"),
            "case {case:?}: the delete must be refused 409 InvalidConfigurationRequest, \
             got {} with {}",
            refused.status,
            refused.body
        );
        let message = error_message_bytes(&refused);
        assert_eq!(
            message,
            format!(
                "You cannot delete policy [{policy_name}] since it is enabled or \
                 referred by other enabled policy."
            ),
            "case {case:?}: the refusal must state the policy is enabled, got \
             {message:?}"
        );

        // The refused delete removed nothing: the policy is still held and still enabled.
        let held = held_policies(&router, "in-use-lb").await;
        assert!(
            held.contains(&policy_name.to_string()),
            "case {case:?}: a refused delete must leave the policy held, got {held:?}"
        );
        let reported = described_listener_policies(&router, "in-use-lb", *port).await;
        assert_eq!(
            reported,
            as_set(&[policy_name]),
            "case {case:?}: a refused delete must leave the listener's set as it was, \
             got {reported:?}"
        );

        // Disabled on that listener, the same delete succeeds and the policy is gone.
        let disabled = set_policies(&router, REGION, "in-use-lb", *port, &[]).await;
        assert_eq!(
            disabled.status, 200,
            "case {case:?}: emptying the listener's set must answer 200, got {}",
            disabled.body
        );
        let deleted = delete_policy(&router, "in-use-lb", policy_name).await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: the delete of a policy no listener enables must answer 200, \
             got {} with {}",
            deleted.status,
            deleted.body
        );
        let held = held_policies(&router, "in-use-lb").await;
        assert!(
            !held.contains(&policy_name.to_string()),
            "case {case:?}: the deleted policy must no longer be held, got {held:?}"
        );
    }

    // Every policy created above was deleted once it was no longer enabled.
    let held = held_policies(&router, "in-use-lb").await;
    assert!(
        held.is_empty(),
        "the load balancer must hold no policy once each was disabled and deleted, got \
         {held:?}"
    );
}
