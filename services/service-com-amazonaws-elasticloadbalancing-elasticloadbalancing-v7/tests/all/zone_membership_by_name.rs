// SPDX-License-Identifier: CC0-1.0

//! `EnableAvailabilityZonesForLoadBalancer` and
//! `DisableAvailabilityZonesForLoadBalancer`: the zones a load balancer is in, named
//! directly rather than through a subnet.
//!
//! Every request goes through the awsQuery protocol on the multi-service router (see
//! `super::wire`), so what is asserted is the emulator's HTTP behaviour.
//!
//! The rules under test, each over a TABLE of inputs rather than one recorded request:
//!
//! * The operation adds the requested zones to the set the load balancer is in and answers
//!   "the updated list of Availability Zones for the load balancer" — the WHOLE stored set,
//!   which a later `DescribeLoadBalancers` then reports: a zone it is not in yet joins the
//!   set, a zone it is already in re-states the same membership, and a zone named twice in
//!   one request is enabled once.
//! * The zone-removal beside it takes the named zones out of that same set and answers "the
//!   remaining Availability Zones for the load balancer", again the whole stored set: a zone
//!   the load balancer is in leaves it, a zone it is not in removes nothing, and a zone named
//!   twice in one request is removed once.
//! * "There must be at least one Availability Zone registered with a load balancer at all
//!   times", so a removal that would leave the load balancer in none is refused as a
//!   configuration change this service cannot apply (`InvalidConfigurationRequest`,
//!   HTTP 409) and removes nothing.
//! * A name the request's region holds no load balancer for earns the operation's one
//!   modelled failure (`LoadBalancerNotFound`, HTTP 400), and a request that names no zone,
//!   or more zones than a load balancer can be in, is refused under the code this service
//!   answers values it cannot accept with. Each refusal leaves the stored set exactly as it
//!   was.
//! * Which VPC a load balancer is in is EC2's to say, and this crate mounts no EC2, so
//!   nothing is known here to refuse the call on: the zone-enable of a load balancer placed
//!   in subnets is accepted. The other side of that rule — a load balancer EC2 reports in a
//!   VPC of the account's own, which this operation does not apply to — needs both services
//!   on one engine and is pinned in
//!   `core/router/tests/all/elb_classic_zone_enable_turns_on_the_vpc.rs`.

use super::wire::{
    age_registrations, call, emulator, emulator_with_engine, error_message, find, kid_text,
    member_texts, members, param, parse, Wire,
};
use service_com_amazonaws_elasticloadbalancing_elasticloadbalancing_v7::state::HEALTH_CHECK_SPAN;
use smithy_server_runtime::MultiServiceRouter;

/// The region every request below is signed for.
const REGION: &str = "us-west-2";

/// The error code this service answers a request whose values it cannot accept with.
const VALIDATION_ERROR: &str = "ValidationError";

/// The awsQuery code the modelled `InvalidConfigurationRequestException` (HTTP 409) is
/// answered under: the refusal of a configuration change a load balancer's own rules do not
/// admit.
const INVALID_CONFIGURATION_REQUEST: &str = "InvalidConfigurationRequest";

/// The `UnhealthyThreshold` of the health check a newly created load balancer carries — the
/// Classic Load Balancer default, which `DescribeLoadBalancers` reports as 2, and so the
/// number of consecutive failed checks a registration needs before its verdict moves.
const DEFAULT_UNHEALTHY_THRESHOLD: u32 = 2;

/// The number of placements one load balancer's stored set is bounded by
/// (`state::MAX_PLACEMENTS_PER_LOAD_BALANCER`), which the over-bound refusals below are
/// stated over.
const MAX_PLACEMENTS: usize = 50;

/// Create `name` with one HTTP 80 listener and the placement in `extra`.
async fn create(router: &MultiServiceRouter, name: &str, extra: &[(String, String)]) -> Wire {
    let mut params = vec![
        param("LoadBalancerName", name),
        param("Listeners.member.1.Protocol", "HTTP"),
        param("Listeners.member.1.LoadBalancerPort", "80"),
        param("Listeners.member.1.InstancePort", "80"),
    ];
    params.extend_from_slice(extra);
    let created = call(router, REGION, "CreateLoadBalancer", &params).await;
    assert_eq!(
        created.status, 200,
        "the fixture {name} must be created, got {} with {}",
        created.status, created.body
    );
    created
}

/// One `AvailabilityZones` member per entry of `zones`, which is the input BOTH zone
/// operations take — a zone repeated in the list is sent twice, which is what the
/// deduplication rows drive.
fn zone_params(name: &str, zones: &[&str]) -> Vec<(String, String)> {
    let mut params = vec![param("LoadBalancerName", name)];
    for (index, zone) in zones.iter().enumerate() {
        params.push(param(
            &format!("AvailabilityZones.member.{}", index + 1),
            zone,
        ));
    }
    params
}

/// The `AvailabilityZones` a success envelope answers, in document order.
fn answered_zones(answer: &Wire) -> Vec<String> {
    let doc = parse(&answer.body);
    member_texts(answer.result(&doc), "AvailabilityZones")
}

/// The `AvailabilityZones` a later `DescribeLoadBalancers` reports for `name`.
async fn described_zones(router: &MultiServiceRouter, name: &str) -> Vec<String> {
    let described = call(
        router,
        REGION,
        "DescribeLoadBalancers",
        &[param("LoadBalancerNames.member.1", name)],
    )
    .await;
    assert_eq!(
        described.status, 200,
        "the load balancer {name} must be describable, got {}",
        described.body
    );
    let doc = parse(&described.body);
    let description = find(described.result(&doc), "LoadBalancerDescriptions")
        .and_then(|list| list.children().next())
        .unwrap_or_else(|| panic!("no description in {}", described.body));
    member_texts(description, "AvailabilityZones")
}

fn owned(zones: &[&str]) -> Vec<String> {
    zones.iter().map(|zone| (*zone).to_string()).collect()
}

/// The zone set FOLLOWS the requests that enable zones, and the answer is the whole stored
/// set — which is what the next read reports.
///
/// The rows walk one load balancer, differing along what the rule generalises over: how
/// many zones one request names, whether the load balancer is already in them, and a zone
/// named twice in the same request.
#[tokio::test]
async fn enabling_zones_adds_them_to_the_stored_set() {
    let router = emulator();
    let name = "zone-enable-elb";
    create(
        &router,
        name,
        &[param("AvailabilityZones.member.1", "us-west-2a")],
    )
    .await;

    // One row: the zones the request names, and the set the load balancer is then in.
    struct Row<'a> {
        case: &'a str,
        requested: Vec<&'a str>,
        expected: Vec<&'a str>,
    }
    let rows: [Row<'_>; 6] = [
        Row {
            case: "a zone the load balancer is not in yet",
            requested: vec!["us-west-2b"],
            expected: vec!["us-west-2a", "us-west-2b"],
        },
        Row {
            case: "the same zone again re-states the same membership",
            requested: vec!["us-west-2b"],
            expected: vec!["us-west-2a", "us-west-2b"],
        },
        Row {
            case: "a zone it is already in named beside one it is not",
            requested: vec!["us-west-2b", "us-west-2c"],
            expected: vec!["us-west-2a", "us-west-2b", "us-west-2c"],
        },
        Row {
            case: "one zone named twice in one request is enabled once",
            requested: vec!["us-west-2d", "us-west-2d"],
            expected: vec!["us-west-2a", "us-west-2b", "us-west-2c", "us-west-2d"],
        },
        Row {
            case: "every zone of the request already held",
            requested: vec!["us-west-2a", "us-west-2d"],
            expected: vec!["us-west-2a", "us-west-2b", "us-west-2c", "us-west-2d"],
        },
        Row {
            case: "three new zones in one request",
            requested: vec!["us-west-2e", "us-west-2f", "us-west-2g"],
            expected: vec![
                "us-west-2a",
                "us-west-2b",
                "us-west-2c",
                "us-west-2d",
                "us-west-2e",
                "us-west-2f",
                "us-west-2g",
            ],
        },
    ];

    for Row {
        case,
        requested,
        expected,
    } in rows
    {
        let answered = call(
            &router,
            REGION,
            "EnableAvailabilityZonesForLoadBalancer",
            &zone_params(name, &requested),
        )
        .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: the zone-enable must be accepted, got {} with {}",
            answered.status, answered.body
        );
        let answered_set = answered_zones(&answered);
        assert_eq!(
            answered_set,
            owned(&expected),
            "case {case:?}: expected the answer to carry the stored set {:?}, got \
             {answered_set:?}",
            owned(&expected)
        );
        let reported = described_zones(&router, name).await;
        assert_eq!(
            reported,
            owned(&expected),
            "case {case:?}: expected a later describe to report {:?}, got {reported:?}",
            owned(&expected)
        );
    }
}

/// Each refusal the operation answers, and the stored set each of them leaves alone.
#[tokio::test]
async fn a_refused_zone_enable_changes_nothing() {
    let router = emulator();
    let name = "zone-enable-refusal-elb";
    create(
        &router,
        name,
        &[param("AvailabilityZones.member.1", "us-west-2a")],
    )
    .await;

    // One row: the load balancer the request names, the zones it names, and the status and
    // code it must be refused with.
    struct Row<'a> {
        case: &'a str,
        named: &'a str,
        zones: Vec<&'a str>,
        status: u16,
        code: &'a str,
    }
    let too_many: Vec<String> = (0..=MAX_PLACEMENTS)
        .map(|index| format!("us-west-2-{index}"))
        .collect();
    let rows: Vec<Row<'_>> = vec![
        Row {
            case: "a name the region holds no load balancer for",
            named: "no-such-classic-elb",
            zones: vec!["us-west-2b"],
            status: 400,
            code: "LoadBalancerNotFound",
        },
        Row {
            // `AvailabilityZones` is a required member, so a request carrying none of it
            // is refused by the boundary validator — before the handler, under the code
            // the framework answers a request that does not satisfy the MODEL with.
            case: "a request that names no zone at all",
            named: name,
            zones: vec![],
            status: 400,
            code: "MalformedInput",
        },
        Row {
            case: "one zone more than a load balancer can be in",
            named: name,
            zones: too_many.iter().map(String::as_str).collect(),
            status: 400,
            code: VALIDATION_ERROR,
        },
    ];

    for Row {
        case,
        named,
        zones,
        status,
        code,
    } in rows
    {
        let answered = call(
            &router,
            REGION,
            "EnableAvailabilityZonesForLoadBalancer",
            &zone_params(named, &zones),
        )
        .await;
        assert_eq!(
            (answered.status, answered.error_code().as_str()),
            (status, code),
            "case {case:?}: expected HTTP {status} {code}, got {} {} with {}",
            answered.status,
            answered.error_code(),
            answered.body
        );
        assert!(
            !error_message(&answered).is_empty(),
            "case {case:?}: a refusal must state why, got {}",
            answered.body
        );
        let reported = described_zones(&router, name).await;
        assert_eq!(
            reported,
            owned(&["us-west-2a"]),
            "case {case:?}: a refused request must leave the stored set alone, got \
             {reported:?}"
        );
    }

    // The name the first row asked about is still held by nothing: a refusal invents no
    // record.
    let described = call(
        &router,
        REGION,
        "DescribeLoadBalancers",
        &[param("LoadBalancerNames.member.1", "no-such-classic-elb")],
    )
    .await;
    assert_eq!(
        (described.status, described.error_code().as_str()),
        (400, "LoadBalancerNotFound"),
        "a refused zone-enable must leave no record behind, got {}",
        described.body
    );
}

/// With no EC2 to ask, this service learned nothing about the load balancer's network, so
/// nothing is refused on that silence: the zone-enable of a load balancer placed in subnets
/// is accepted and the zone joins the stored set.
#[tokio::test]
async fn nothing_is_refused_where_there_was_no_ec2_to_ask() {
    let router = emulator();
    let name = "zone-enable-silent-network-elb";
    create(
        &router,
        name,
        &[param("Subnets.member.1", "subnet-0f1e2d3c4b5a69780")],
    )
    .await;

    let answered = call(
        &router,
        REGION,
        "EnableAvailabilityZonesForLoadBalancer",
        &zone_params(name, &["us-west-2b"]),
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "nothing is refused on silence, got {} with {}",
        answered.status, answered.body
    );
    let reported = described_zones(&router, name).await;
    assert_eq!(
        reported,
        owned(&["us-west-2b"]),
        "the enabled zone must join the stored set, got {reported:?}"
    );
}

/// The zone set FOLLOWS the removals as well, and the answer is the whole REMAINING set —
/// which is what the next read reports.
///
/// The rows walk one load balancer, differing along what the rule generalises over: whether
/// the load balancer is in the named zone at all, how many zones one request names, and a
/// zone named twice in the same request.
#[tokio::test]
async fn disabling_zones_removes_them_from_the_stored_set() {
    let router = emulator();
    let name = "zone-disable-elb";
    create(
        &router,
        name,
        &[
            param("AvailabilityZones.member.1", "us-west-2a"),
            param("AvailabilityZones.member.2", "us-west-2b"),
            param("AvailabilityZones.member.3", "us-west-2c"),
            param("AvailabilityZones.member.4", "us-west-2d"),
        ],
    )
    .await;

    // One row: the zones the request names, and the set the load balancer is left in.
    struct Row<'a> {
        case: &'a str,
        requested: Vec<&'a str>,
        expected: Vec<&'a str>,
    }
    let rows: [Row<'_>; 5] = [
        Row {
            case: "a zone the load balancer is in",
            requested: vec!["us-west-2d"],
            expected: vec!["us-west-2a", "us-west-2b", "us-west-2c"],
        },
        Row {
            case: "the same zone again removes nothing",
            requested: vec!["us-west-2d"],
            expected: vec!["us-west-2a", "us-west-2b", "us-west-2c"],
        },
        Row {
            case: "a zone the load balancer was never in",
            requested: vec!["us-west-2f"],
            expected: vec!["us-west-2a", "us-west-2b", "us-west-2c"],
        },
        Row {
            case: "one zone named twice in one request is removed once",
            requested: vec!["us-west-2c", "us-west-2c"],
            expected: vec!["us-west-2a", "us-west-2b"],
        },
        Row {
            case: "a zone it is in named beside one it is not",
            requested: vec!["us-west-2b", "us-west-2f"],
            expected: vec!["us-west-2a"],
        },
    ];

    for Row {
        case,
        requested,
        expected,
    } in rows
    {
        let answered = call(
            &router,
            REGION,
            "DisableAvailabilityZonesForLoadBalancer",
            &zone_params(name, &requested),
        )
        .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: the zone-disable must be accepted, got {} with {}",
            answered.status, answered.body
        );
        let answered_set = answered_zones(&answered);
        assert_eq!(
            answered_set,
            owned(&expected),
            "case {case:?}: expected the answer to carry the remaining set {:?}, got \
             {answered_set:?}",
            owned(&expected)
        );
        let reported = described_zones(&router, name).await;
        assert_eq!(
            reported,
            owned(&expected),
            "case {case:?}: expected a later describe to report {:?}, got {reported:?}",
            owned(&expected)
        );
    }
}

/// Each refusal the zone-removal answers, and the stored set each of them leaves alone —
/// including the one rule that is this operation's own: "there must be at least one
/// Availability Zone registered with a load balancer at all times".
#[tokio::test]
async fn a_refused_zone_disable_changes_nothing() {
    let router = emulator();
    let name = "zone-disable-refusal-elb";
    create(
        &router,
        name,
        &[
            param("AvailabilityZones.member.1", "us-west-2a"),
            param("AvailabilityZones.member.2", "us-west-2b"),
        ],
    )
    .await;

    // One row: the load balancer the request names, the zones it names, and the status and
    // code it must be refused with.
    struct Row<'a> {
        case: &'a str,
        named: &'a str,
        zones: Vec<&'a str>,
        status: u16,
        code: &'a str,
    }
    let too_many: Vec<String> = (0..=MAX_PLACEMENTS)
        .map(|index| format!("us-west-2-{index}"))
        .collect();
    let rows: Vec<Row<'_>> = vec![
        Row {
            case: "a name the region holds no load balancer for",
            named: "no-such-classic-elb",
            zones: vec!["us-west-2a"],
            status: 400,
            code: "LoadBalancerNotFound",
        },
        Row {
            // `AvailabilityZones` is a required member, so a request carrying none of it
            // is refused by the boundary validator — before the handler, under the code
            // the framework answers a request that does not satisfy the MODEL with.
            case: "a request that names no zone at all",
            named: name,
            zones: vec![],
            status: 400,
            code: "MalformedInput",
        },
        Row {
            case: "one zone more than a load balancer can be in",
            named: name,
            zones: too_many.iter().map(String::as_str).collect(),
            status: 400,
            code: VALIDATION_ERROR,
        },
        Row {
            case: "every zone the load balancer is in",
            named: name,
            zones: vec!["us-west-2a", "us-west-2b"],
            status: 409,
            code: INVALID_CONFIGURATION_REQUEST,
        },
        Row {
            case: "every zone it is in, beside one it is not",
            named: name,
            zones: vec!["us-west-2b", "us-west-2f", "us-west-2a"],
            status: 409,
            code: INVALID_CONFIGURATION_REQUEST,
        },
    ];

    for Row {
        case,
        named,
        zones,
        status,
        code,
    } in rows
    {
        let answered = call(
            &router,
            REGION,
            "DisableAvailabilityZonesForLoadBalancer",
            &zone_params(named, &zones),
        )
        .await;
        assert_eq!(
            (answered.status, answered.error_code().as_str()),
            (status, code),
            "case {case:?}: expected HTTP {status} {code}, got {} {} with {}",
            answered.status,
            answered.error_code(),
            answered.body
        );
        assert!(
            !error_message(&answered).is_empty(),
            "case {case:?}: a refusal must state why, got {}",
            answered.body
        );
        let reported = described_zones(&router, name).await;
        assert_eq!(
            reported,
            owned(&["us-west-2a", "us-west-2b"]),
            "case {case:?}: a refused request must leave the stored set alone, got \
             {reported:?}"
        );
    }

    // The refusal of this operation's own rule states it over the load balancer the request
    // asked about, which is what the caller has to fix.
    let last_zone = call(
        &router,
        REGION,
        "DisableAvailabilityZonesForLoadBalancer",
        &zone_params(name, &["us-west-2a", "us-west-2b"]),
    )
    .await;
    let stated = error_message(&last_zone);
    assert!(
        stated.contains(name) && stated.contains("Availability Zone"),
        "the zero-zone refusal must name the load balancer and the rule, got {stated:?}"
    );
}

/// A zone-removal changes the PLACEMENT, not the registrations: every instance registered
/// with the load balancer is still reported by `DescribeInstanceHealth`, one `InstanceState`
/// member per instance — a list, never one empty element — and each member carries the
/// health that instance's own checks have reached.
#[tokio::test]
async fn the_registered_instances_are_still_reported_after_a_zone_disable() {
    let (router, engine) = emulator_with_engine();
    let name = "zone-disable-health-elb";
    create(
        &router,
        name,
        &[
            param("AvailabilityZones.member.1", "us-west-2a"),
            param("AvailabilityZones.member.2", "us-west-2b"),
        ],
    )
    .await;

    // Two instances the account's EC2 inventory holds nothing for, so nothing stands behind
    // either registration and every check of it fails as the instance's own doing.
    let registered = ["i-0a1b2c3d4e5f60001", "i-0a1b2c3d4e5f60002"];
    let mut params = vec![param("LoadBalancerName", name)];
    for (index, instance) in registered.iter().enumerate() {
        params.push(param(
            &format!("Instances.member.{}.InstanceId", index + 1),
            instance,
        ));
    }
    let answered = call(
        &router,
        REGION,
        "RegisterInstancesWithLoadBalancer",
        &params,
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "the instances must be registered, got {}",
        answered.body
    );

    let disabled = call(
        &router,
        REGION,
        "DisableAvailabilityZonesForLoadBalancer",
        &zone_params(name, &["us-west-2b"]),
    )
    .await;
    assert_eq!(
        disabled.status, 200,
        "the zone-disable must be accepted, got {}",
        disabled.body
    );
    assert_eq!(
        answered_zones(&disabled),
        owned(&["us-west-2a"]),
        "the remaining zone must be the one that was not named, got {}",
        disabled.body
    );

    // Right after the registration the checks each one owes are still running, which is
    // Elastic Load Balancing's own doing.
    assert_eq!(
        health(&router, name).await,
        registered
            .iter()
            .map(|instance| (
                (*instance).to_string(),
                owned(&[
                    "OutOfService",
                    "ELB",
                    "Instance registration is still in progress.",
                ])
            ))
            .collect::<Vec<(String, Vec<String>)>>(),
        "one InstanceState per registered instance must be reported after the zone-disable"
    );

    // Once the checks the default `UnhealthyThreshold` asks for have fallen due — placed by
    // moving the stored registration back, never by waiting — each instance has failed them.
    age_registrations(
        &engine,
        REGION,
        name,
        HEALTH_CHECK_SPAN.saturating_mul(DEFAULT_UNHEALTHY_THRESHOLD),
    );
    assert_eq!(
        health(&router, name).await,
        registered
            .iter()
            .map(|instance| (
                (*instance).to_string(),
                owned(&[
                    "OutOfService",
                    "Instance",
                    "Instance has failed at least the UnhealthyThreshold number of health \
                     checks consecutively.",
                ])
            ))
            .collect::<Vec<(String, Vec<String>)>>(),
        "every registered instance must reach the failed-health-check triple"
    );
}

/// The `(InstanceId, [State, ReasonCode, Description])` of every `InstanceStates` member
/// `DescribeInstanceHealth` answers for `name`, in document order — which is also what
/// proves the members are LIST elements rather than the element's own character data.
async fn health(router: &MultiServiceRouter, name: &str) -> Vec<(String, Vec<String>)> {
    let answered = call(
        router,
        REGION,
        "DescribeInstanceHealth",
        &[param("LoadBalancerName", name)],
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "DescribeInstanceHealth must answer 200 for {name}, got {}",
        answered.body
    );
    let doc = parse(&answered.body);
    members(answered.result(&doc), "InstanceStates")
        .into_iter()
        .map(|member| {
            (
                kid_text(member, "InstanceId").unwrap_or_default(),
                ["State", "ReasonCode", "Description"]
                    .into_iter()
                    .map(|tag| kid_text(member, tag).unwrap_or_default())
                    .collect(),
            )
        })
        .collect()
}
