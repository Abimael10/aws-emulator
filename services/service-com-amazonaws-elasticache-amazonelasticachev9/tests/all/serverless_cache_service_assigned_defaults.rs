// SPDX-License-Identifier: CC0-1.0

//! What a serverless cache the request did not fully configure is CREATED with,
//! and reports for the rest of its life.
//!
//! The rule under test: a `ServerlessCache` reports the cache's own stored
//! configuration, including the values the SERVICE supplied for the members the
//! request left out — the default security group of the VPC the cache is placed
//! in ("A list of the one or more VPC security groups to be associated with the
//! serverless cache. … If no other information is given this will be the VPC's
//! Default Security Group that is associated with the cluster VPC end-point",
//! `CreateServerlessCache$SecurityGroupIds`) and the snapshot posture a request
//! that configures none gets — so the create and every later read report the
//! same member set, and a client can tell "no automatic snapshots" from "the
//! service did not say".
//!
//! Every row goes over the wire through the same router the production emulator
//! mounts, and every identifier a row expects is minted by Amazon EC2 — the
//! service that owns VPCs, subnets and security groups — so no expectation here
//! is an id this suite wrote down. The table's axis is WHICH of those members
//! the request names: neither, one, the other, both, at their published bounds,
//! and over the engines a serverless cache runs.

use service_com_amazonaws_elasticache_amazonelasticachev9::state;

use crate::harness::{params, Emulator, Reply, REGION};

/// One row of the table: what the request configures, and what the cache must
/// report back on the create and on the read.
#[derive(Debug, Clone, Copy)]
struct DefaultsCase {
    /// Names the row in every assertion.
    case: &'static str,
    /// The form pairs sent beside `ServerlessCacheName`.
    extra: &'static [(&'static str, &'static str)],
    /// How many security groups of the ACCOUNT's own the request names; `0`
    /// leaves the choice to the service.
    named_groups: usize,
    /// How many of the account's default subnets the request names as the
    /// placement; `0` leaves the selection to the service.
    named_subnets: usize,
    /// What `SnapshotRetentionLimit` must report.
    retention: &'static str,
    /// What `DailySnapshotTime` must report, or `None` when the request named
    /// none and the service chooses the time.
    daily_time: Option<&'static str>,
}

/// The axis: which of the two configurations the request states, each on its own
/// and both together, on each published bound of the retention limit, and across
/// the engines a serverless cache runs.
const CASES: &[DefaultsCase] = &[
    DefaultsCase {
        case: "redis, nothing configured",
        extra: &[("Engine", "redis")],
        named_groups: 0,
        named_subnets: 0,
        retention: "0",
        daily_time: None,
    },
    DefaultsCase {
        case: "valkey, nothing configured",
        extra: &[("Engine", "valkey")],
        named_groups: 0,
        named_subnets: 0,
        retention: "0",
        daily_time: None,
    },
    DefaultsCase {
        // "Available for Valkey, Redis OSS and Serverless Memcached only"
        // (`CreateServerlessCache$SnapshotRetentionLimit`), so a Memcached cache
        // carries the same posture.
        case: "memcached, nothing configured",
        extra: &[("Engine", "memcached")],
        named_groups: 0,
        named_subnets: 0,
        retention: "0",
        daily_time: None,
    },
    DefaultsCase {
        case: "a retention limit without a daily time",
        extra: &[("Engine", "redis"), ("SnapshotRetentionLimit", "7")],
        named_groups: 0,
        named_subnets: 0,
        retention: "7",
        daily_time: None,
    },
    DefaultsCase {
        case: "a daily time without a retention limit",
        extra: &[("Engine", "valkey"), ("DailySnapshotTime", "04:30")],
        named_groups: 0,
        named_subnets: 0,
        retention: "0",
        daily_time: Some("04:30"),
    },
    DefaultsCase {
        // The published maximum retention ("The maximum value allowed is 35
        // days") and the last minute of the 24-hour clock.
        case: "both configured at their upper bounds",
        extra: &[
            ("Engine", "valkey"),
            ("SnapshotRetentionLimit", "35"),
            ("DailySnapshotTime", "23:59"),
        ],
        named_groups: 0,
        named_subnets: 0,
        retention: "35",
        daily_time: Some("23:59"),
    },
    DefaultsCase {
        // The lower bound, named explicitly: the same posture a request that
        // names nothing gets, and it must still be reported.
        case: "the retention limit named as zero",
        extra: &[
            ("Engine", "redis"),
            ("SnapshotRetentionLimit", "0"),
            ("DailySnapshotTime", "00:00"),
        ],
        named_groups: 0,
        named_subnets: 0,
        retention: "0",
        daily_time: Some("00:00"),
    },
    DefaultsCase {
        case: "one security group named",
        extra: &[("Engine", "redis")],
        named_groups: 1,
        named_subnets: 0,
        retention: "0",
        daily_time: None,
    },
    DefaultsCase {
        case: "two security groups named",
        extra: &[("Engine", "valkey")],
        named_groups: 2,
        named_subnets: 0,
        retention: "0",
        daily_time: None,
    },
    DefaultsCase {
        case: "the placement named but no security group",
        extra: &[("Engine", "redis")],
        named_groups: 0,
        named_subnets: 2,
        retention: "0",
        daily_time: None,
    },
];

/// Create one serverless cache from already-owned form pairs.
async fn create_cache(emulator: &Emulator, name: &str, extra: &[(String, String)]) -> Reply {
    let mut pairs = params(&[("ServerlessCacheName", name)]);
    pairs.extend(extra.iter().cloned());
    emulator.call("CreateServerlessCache", &pairs).await
}

/// Read one serverless cache back by name.
async fn describe_cache(emulator: &Emulator, name: &str) -> Reply {
    emulator
        .call(
            "DescribeServerlessCaches",
            &params(&[("ServerlessCacheName", name)]),
        )
        .await
}

/// Assert `reply` reports the snapshot posture and the security groups `case`
/// must carry. `where_from` says which response is under test, and
/// `expected_groups` are the ids EC2 minted for the groups that must guard the
/// cache.
fn assert_reports(
    case: &DefaultsCase,
    where_from: &str,
    reply: &Reply,
    expected_groups: &[String],
) {
    let label = case.case;
    assert_eq!(
        reply.text("SnapshotRetentionLimit"),
        Some(case.retention),
        "case {label:?} ({where_from}): expected SnapshotRetentionLimit {:?}, got {:?}",
        case.retention,
        reply.text("SnapshotRetentionLimit")
    );
    let reported_time = reply.text("DailySnapshotTime");
    match case.daily_time {
        Some(expected) => assert_eq!(
            reported_time,
            Some(expected),
            "case {label:?} ({where_from}): expected DailySnapshotTime {expected:?}, got \
             {reported_time:?}"
        ),
        // The time is the SERVICE's choice, so the row asserts what makes it one
        // rather than the value: it is reported at all, and it is a daily
        // wall-clock time of the form the member takes.
        None => assert!(
            reported_time.is_some_and(state::is_daily_snapshot_time),
            "case {label:?} ({where_from}): expected a daily snapshot time of the service's own, \
             got {reported_time:?}"
        ),
    }
    let expected: Vec<&str> = expected_groups.iter().map(String::as_str).collect();
    assert_eq!(
        reply.all("SecurityGroupIds/SecurityGroupId"),
        expected,
        "case {label:?} ({where_from}): expected SecurityGroupIds {expected:?}, got {:?}",
        reply.all("SecurityGroupIds/SecurityGroupId")
    );
}

#[tokio::test]
async fn a_serverless_cache_reports_the_configuration_the_service_chose_for_it() {
    let emulator = Emulator::new();
    // The account's own network, as EC2 reports it: the subnets ElastiCache
    // selects for a request that names none are its per-zone default subnets, and
    // they are the ones a row may name itself.
    let default_subnets: Vec<_> = emulator
        .account_subnets()
        .await
        .into_iter()
        .filter(|subnet| subnet.default_for_az)
        .collect();
    let placement_vpc = default_subnets
        .first()
        .map(|subnet| subnet.vpc.clone())
        .expect("the account must have a default subnet to place a cache in");
    // The group EC2 calls the default one of that VPC — asked of the owner, so
    // the expectation is not an id this suite invented.
    let vpc_default_group = emulator.default_security_group_of(&placement_vpc).await;

    for (index, case) in CASES.iter().enumerate() {
        let name = format!("defaults-{index}");
        let mut extra = params(case.extra);
        extra.extend(
            default_subnets[..case.named_subnets]
                .iter()
                .enumerate()
                .map(|(position, subnet)| {
                    (
                        format!("SubnetIds.SubnetId.{}", position + 1),
                        subnet.id.clone(),
                    )
                }),
        );
        // A row that names groups names groups the account HAS: EC2 mints them,
        // so their ids come from the owner.
        let named_groups = emulator.security_groups(&name, case.named_groups).await;
        extra.extend(named_groups.iter().enumerate().map(|(position, id)| {
            (
                format!("SecurityGroupIds.SecurityGroupId.{}", position + 1),
                id.clone(),
            )
        }));
        // What must guard the cache: the groups the request named, or — when it
        // named none — the default group of the VPC it is placed in.
        let expected_groups = if named_groups.is_empty() {
            vec![vpc_default_group.clone()]
        } else {
            named_groups.clone()
        };

        let created = create_cache(&emulator, &name, &extra).await;
        assert_eq!(
            created.status,
            200,
            "case {:?}: the create must answer 200, got {} {}",
            case.case,
            created.status,
            created.body
        );
        assert_reports(case, "create", &created, &expected_groups);

        // The round trip: the read reports the same configuration, out of the
        // store rather than out of the request that made it.
        let described = describe_cache(&emulator, &name).await;
        assert_eq!(
            described.status,
            200,
            "case {:?}: the read must answer 200, got {} {}",
            case.case,
            described.status,
            described.body
        );
        assert_reports(case, "describe", &described, &expected_groups);
        assert_eq!(
            described.text("DailySnapshotTime"),
            created.text("DailySnapshotTime"),
            "case {:?}: the read must report the daily snapshot time the create stored, got {:?} \
             after {:?}",
            case.case,
            described.text("DailySnapshotTime"),
            created.text("DailySnapshotTime")
        );
    }
}

#[tokio::test]
async fn the_default_group_a_cache_gets_is_the_one_of_the_vpc_it_is_placed_in() {
    // The axis: WHICH VPC the placement is in. A cache placed in a VPC of the
    // account's own must be guarded by THAT VPC's default group — the group is
    // the VPC's, so one of another VPC could not guard this cache's endpoint.
    let emulator = Emulator::new();
    let ec2 = emulator.ec2(REGION);
    let vpc = ec2
        .create_vpc()
        .cidr_block("10.40.0.0/16")
        .send()
        .await
        .expect("EC2 must create the VPC")
        .vpc()
        .and_then(::aws_sdk_ec2::types::Vpc::vpc_id)
        .expect("CreateVpc must report the VPC it minted")
        .to_string();
    let subnet = ec2
        .create_subnet()
        .vpc_id(&vpc)
        .cidr_block("10.40.1.0/24")
        .availability_zone(format!("{REGION}a"))
        .send()
        .await
        .expect("EC2 must create the subnet")
        .subnet()
        .and_then(::aws_sdk_ec2::types::Subnet::subnet_id)
        .expect("CreateSubnet must report the subnet it minted")
        .to_string();

    let created = create_cache(
        &emulator,
        "own-vpc-cache",
        &[
            ("Engine".to_string(), "valkey".to_string()),
            ("SubnetIds.SubnetId.1".to_string(), subnet.clone()),
        ],
    )
    .await;
    assert_eq!(
        created.status, 200,
        "the create must answer 200, got {} {}",
        created.status, created.body
    );
    let expected = vec![emulator.default_security_group_of(&vpc).await];
    let reported: Vec<String> = created
        .all("SecurityGroupIds/SecurityGroupId")
        .into_iter()
        .map(str::to_string)
        .collect();
    assert_eq!(
        reported, expected,
        "a cache placed in {vpc} must be guarded by that VPC's default group {expected:?}, got \
         {reported:?}"
    );
    // The same group comes back out of the store, and it is NOT the default
    // group of the Region's default VPC, which guards nothing of this placement.
    let default_vpc = emulator
        .account_subnets()
        .await
        .into_iter()
        .find(|candidate| candidate.default_for_az)
        .map(|candidate| candidate.vpc)
        .expect("the account must have a default subnet");
    let other_group = emulator.default_security_group_of(&default_vpc).await;
    assert_ne!(
        reported.first().map(String::as_str),
        Some(other_group.as_str()),
        "the group guarding the cache must be {vpc}'s own, not {default_vpc}'s {other_group}"
    );
    let described = describe_cache(&emulator, "own-vpc-cache").await;
    let read_back: Vec<String> = described
        .all("SecurityGroupIds/SecurityGroupId")
        .into_iter()
        .map(str::to_string)
        .collect();
    assert_eq!(
        read_back, expected,
        "the read must report the stored group {expected:?}, got {read_back:?}"
    );
}
