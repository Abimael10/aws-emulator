// SPDX-License-Identifier: CC0-1.0

//! The sentence an operation that places a resource in EC2 subnets is refused with when any
//! identifier it was given names no subnet of the account.
//!
//! THE RULE: the refusal (`InvalidParameterValue`, HTTP 400) enumerates the request's WHOLE
//! subnet list — every identifier it sent, in the order it sent them, the ones that resolved
//! included — between `Some input subnets in :[` and `] are invalid.`. The message is the
//! only place the refusal reports which subnets the service was asked about, so a client
//! that diffs the bracketed list against what it submitted, or counts its entries, reads its
//! own request back rather than a shorter list.
//!
//! The rows span what the rule generalises over: how many identifiers the request names, how
//! many of them resolve, where in the list the unresolvable ones sit, which form of EC2
//! subnet id they take, and whether the identifier was never minted or was minted and then
//! deleted. The accepting rows — the same lists with every identifier one the account HAS —
//! are what show a row is refused for resolution and not for its shape.
//!
//! Every operation this service has that takes a `SubnetIds` is driven: the create and the
//! modify of a cache subnet group, and the create of a serverless cache. Everything goes
//! over the wire through the same router the production emulator mounts, and the subnets a
//! row names are EC2's own — minted or deleted through EC2's API, never reached for in this
//! service's store.

use crate::cache_cluster_lifecycle::{account_subnet_ids, subnet_group_params, subnet_pairs};
use crate::harness::{assert_refusal, params, Emulator, Reply, REGION};

/// Identifiers of the EC2 subnet form that EC2 mints no subnet for, so each one reaches the
/// handlers' resolution and resolves to nothing. Both the 8-hex and the 17-hex form AWS has
/// minted subnet ids in appear, so the rule is not read off one spelling.
const NEVER_MINTED: [&str; 3] = [
    "subnet-0123456789abcdef0",
    "subnet-00000000deadbeef",
    "subnet-0000beef",
];

/// How this service words a request naming subnets the account does not have, over the whole
/// list the request sent.
fn refusal_sentence(submitted: &[String]) -> String {
    format!(
        "Some input subnets in :[{}] are invalid.",
        submitted.join(", ")
    )
}

/// Mint a subnet in the account's own VPC through EC2 — the owner of subnets — and delete it
/// again, answering with the identifier the account HELD and holds no longer.
async fn deleted_subnet(emulator: &Emulator) -> String {
    let ec2 = emulator.ec2(REGION);
    let vpc = ec2
        .create_vpc()
        .cidr_block("10.42.0.0/16")
        .send()
        .await
        .expect("EC2 must create the VPC this row's subnet is minted in")
        .vpc()
        .and_then(::aws_sdk_ec2::types::Vpc::vpc_id)
        .expect("CreateVpc must report the VPC it minted")
        .to_string();
    let subnet = ec2
        .create_subnet()
        .vpc_id(&vpc)
        .cidr_block("10.42.1.0/24")
        .availability_zone(format!("{REGION}a"))
        .send()
        .await
        .expect("EC2 must create the subnet this row then deletes")
        .subnet()
        .and_then(::aws_sdk_ec2::types::Subnet::subnet_id)
        .expect("CreateSubnet must report the subnet it minted")
        .to_string();
    ec2.delete_subnet()
        .subnet_id(&subnet)
        .send()
        .await
        .expect("EC2 must delete the subnet again");
    subnet
}

/// The lists every table below sends: the case, and the identifiers in the order the request
/// names them. `held` are subnets the account HAS (EC2's own answer), `gone` is one it held
/// and deleted.
fn rows(held: &[String], gone: &str) -> Vec<(&'static str, Vec<String>)> {
    assert!(
        held.len() >= 2,
        "the account must hold at least two subnets for these rows, got {held:?}"
    );
    vec![
        ("one id nothing minted, alone", vec![NEVER_MINTED[0].to_string()]),
        (
            "two ids nothing minted, of the two id forms",
            vec![NEVER_MINTED[1].to_string(), NEVER_MINTED[2].to_string()],
        ),
        (
            "a held id FIRST, an absent one after it",
            vec![held[0].clone(), NEVER_MINTED[0].to_string()],
        ),
        (
            "an absent id FIRST, a held one after it",
            vec![NEVER_MINTED[0].to_string(), held[0].clone()],
        ),
        (
            "an absent id BETWEEN two held ones",
            vec![held[0].clone(), NEVER_MINTED[1].to_string(), held[1].clone()],
        ),
        (
            "two absent ids among two held ones",
            vec![
                held[0].clone(),
                NEVER_MINTED[0].to_string(),
                held[1].clone(),
                NEVER_MINTED[2].to_string(),
            ],
        ),
        (
            "an id the account held and deleted, beside held ones",
            vec![held[0].clone(), gone.to_string(), held[1].clone()],
        ),
        (
            "an id minted and deleted, and one never minted",
            vec![gone.to_string(), NEVER_MINTED[1].to_string()],
        ),
    ]
}

/// Describe the cache subnet group `name` names.
async fn describe_group(emulator: &Emulator, name: &str) -> Reply {
    emulator
        .call(
            "DescribeCacheSubnetGroups",
            &params(&[("CacheSubnetGroupName", name)]),
        )
        .await
}

#[tokio::test]
async fn a_cache_subnet_group_create_lists_every_subnet_id_it_was_sent() {
    let emulator = Emulator::new();
    let held = account_subnet_ids(&emulator).await;
    let gone = deleted_subnet(&emulator).await;

    for (index, (case, submitted)) in rows(&held, &gone).into_iter().enumerate() {
        let name = format!("create-listed-{index}");
        let refused = emulator
            .call(
                "CreateCacheSubnetGroup",
                &subnet_group_params(&name, "the submitted-list suite", &submitted),
            )
            .await;
        assert_refusal(
            case,
            &refused,
            400,
            "InvalidParameterValue",
            &refusal_sentence(&submitted),
        );
        // A refused create materialises no group: the message is the whole answer.
        let described = describe_group(&emulator, &name).await;
        assert_eq!(
            described.error_code(),
            "CacheSubnetGroupNotFoundFault",
            "case {case:?}: a refused create must store no group, got {}",
            described.body
        );
    }

    // The contrast: the same shapes of list with every identifier one the account HAS are
    // accepted, so the rows above are refused for resolution and not for their shape.
    for (index, count) in [1usize, 2, 3].into_iter().enumerate() {
        let accepted: Vec<String> = held.iter().take(count).cloned().collect();
        let name = format!("create-accepted-{index}");
        let created = emulator
            .call(
                "CreateCacheSubnetGroup",
                &subnet_group_params(&name, "the submitted-list suite", &accepted),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {count} held subnets: must be accepted, got {} {}",
            created.status, created.body
        );
        let described = describe_group(&emulator, &name).await;
        assert_eq!(
            described.all("Subnets/Subnet/SubnetIdentifier"),
            accepted.iter().map(String::as_str).collect::<Vec<&str>>(),
            "case {count} held subnets: the stored group must hold them, got {}",
            described.body
        );
    }
}

#[tokio::test]
async fn a_cache_subnet_group_modify_lists_every_subnet_id_it_was_sent() {
    let emulator = Emulator::new();
    let held = account_subnet_ids(&emulator).await;
    let gone = deleted_subnet(&emulator).await;
    let group = "modify-listed";
    let created = emulator
        .call(
            "CreateCacheSubnetGroup",
            &subnet_group_params(group, "the group before the modify", &held[..1]),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "the group under test must be created, got {} {}",
        created.status, created.body
    );

    for (case, submitted) in rows(&held, &gone) {
        let mut pairs = params(&[("CacheSubnetGroupName", group)]);
        pairs.extend(subnet_pairs("SubnetIdentifier", &submitted));
        let refused = emulator.call("ModifyCacheSubnetGroup", &pairs).await;
        assert_refusal(
            case,
            &refused,
            400,
            "InvalidParameterValue",
            &refusal_sentence(&submitted),
        );
        // A refused modify replaces nothing: the group still holds what it was created with.
        let described = describe_group(&emulator, group).await;
        assert_eq!(
            described.all("Subnets/Subnet/SubnetIdentifier"),
            vec![held[0].as_str()],
            "case {case:?}: a refused modify must leave the group's subnets alone, got {}",
            described.body
        );
    }

    // The accepting contrast: held identifiers REPLACE the group's subnets.
    let mut pairs = params(&[("CacheSubnetGroupName", group)]);
    pairs.extend(subnet_pairs("SubnetIdentifier", &held[..2]));
    let modified = emulator.call("ModifyCacheSubnetGroup", &pairs).await;
    assert_eq!(
        modified.status, 200,
        "held subnets must be accepted by the modify, got {} {}",
        modified.status, modified.body
    );
    let described = describe_group(&emulator, group).await;
    assert_eq!(
        described.all("Subnets/Subnet/SubnetIdentifier"),
        held[..2].iter().map(String::as_str).collect::<Vec<&str>>(),
        "the modify must store the held subnets it was given, got {}",
        described.body
    );
}

#[tokio::test]
async fn a_serverless_cache_create_lists_every_subnet_id_it_was_sent() {
    let emulator = Emulator::new();
    let held = account_subnet_ids(&emulator).await;
    let gone = deleted_subnet(&emulator).await;

    for (index, (case, submitted)) in rows(&held, &gone).into_iter().enumerate() {
        let name = format!("serverless-listed-{index}");
        let mut pairs = params(&[("ServerlessCacheName", &name), ("Engine", "redis")]);
        pairs.extend(subnet_pairs("SubnetId", &submitted));
        let refused = emulator.call("CreateServerlessCache", &pairs).await;
        assert_refusal(
            case,
            &refused,
            400,
            "InvalidParameterValue",
            &refusal_sentence(&submitted),
        );
        let described = emulator
            .call(
                "DescribeServerlessCaches",
                &params(&[("ServerlessCacheName", &name)]),
            )
            .await;
        assert_eq!(
            described.error_code(),
            "ServerlessCacheNotFoundFault",
            "case {case:?}: a refused create must store no cache, got {}",
            described.body
        );
    }

    // The contrast: the same request with held identifiers is accepted and reports them.
    let accepted: Vec<String> = held.iter().take(2).cloned().collect();
    let mut pairs = params(&[
        ("ServerlessCacheName", "serverless-accepted"),
        ("Engine", "redis"),
    ]);
    pairs.extend(subnet_pairs("SubnetId", &accepted));
    let created = emulator.call("CreateServerlessCache", &pairs).await;
    assert_eq!(
        created.status, 200,
        "held subnets must be accepted by the serverless create, got {} {}",
        created.status, created.body
    );
    let described = emulator
        .call(
            "DescribeServerlessCaches",
            &params(&[("ServerlessCacheName", "serverless-accepted")]),
        )
        .await;
    assert_eq!(
        described.all("SubnetId"),
        accepted.iter().map(String::as_str).collect::<Vec<&str>>(),
        "the stored cache must hold the subnets it was given, got {}",
        described.body
    );
}
