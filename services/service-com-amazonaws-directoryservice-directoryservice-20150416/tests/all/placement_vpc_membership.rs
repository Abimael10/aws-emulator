// SPDX-License-Identifier: CC0-1.0

//! WHICH VPC a directory's servers may be placed in, driven end to end over HTTP with the
//! owner of VPCs and subnets mounted beside this service on one engine.
//!
//! Every case goes through the same front door a client uses: `POST /` with
//! `X-Amz-Target: DirectoryService_20150416.<Operation>` and an `awsJson1_1` body, dispatched
//! by the multi-service router. The network each case runs against is built through EC2's own
//! API (`crate::directory_dns_addresses::network`), so every subnet a case names is one
//! `ec2:CreateSubnet` really minted and every VPC one `ec2:CreateVpc` really holds.
//!
//! The rule under test: `DirectoryVpcSettings` names ONE VPC — "the identifier of the VPC in
//! which to create the directory" — and its `SubnetIds` are "the identifiers of the subnets
//! for the directory servers", which are subnets OF that VPC. So a servable placement is one
//! the owner of subnets HAS a subnet for, for every identifier it names, in the VPC the request
//! names: a placement naming an identifier that owner holds no subnet for, or a subnet it
//! reports in another VPC, asks for servers on a network the directory is not being created on
//! and is refused with the resource-semantics failure class — `ClientException`, HTTP 400, with
//! the sentence of its own refusal (the invalid-subnets opener alone for an identifier the
//! account has no subnet for, the opener plus the ownership rule for a subnet of another VPC)
//! and the call's own request id — before anything is stored.
//!
//! It is asserted over a TABLE rather than one call, along what the rule generalises over:
//! every operation that names a VPC for a placement (both creates that make a directory, the
//! create that connects one, and the add that replicates one into another Region), WHY the
//! named subnets are not subnets of that VPC (the owner reports one of them in another VPC,
//! or holds no subnet for it at all), the position that subnet takes in the list, and the form
//! the identifier of a subnet the account does not hold takes — with the same operations on
//! subnets OF the named VPC as the positive control.
//!
//! The other two halves of the same placement rule are asserted here over the same real
//! network:
//!
//! * a settings block that names no TWO DISTINCT subnets cannot carry a directory's servers —
//!   they sit in two subnets in two different Availability Zones — and is refused with
//!   `InvalidParameterException` and the one FIXED sentence Directory Service states that
//!   rule in, before anything is stored;
//! * a placement that IS servable is REPORTED with the Availability Zones of the subnets it
//!   named, as the owner of subnets reports them — each zone once, however many of the
//!   placement's subnets sit in it — in `DirectoryVpcSettingsDescription$AvailabilityZones`
//!   and `DirectoryConnectSettingsDescription$AvailabilityZones`, "the list of Availability
//!   Zones that the directory is in".
//!
//! And the rule about HOW those refusals are WORDED, over a table of every unservable pick
//! crossed with every operation that places directory servers: a placement refusal states the
//! rule the pick broke in one FIXED sentence — the invalid-subnets opener and then that rule,
//! quoting nothing of the request — and the refusal envelope appends the call's own request id
//! once, which is the only part of the message that varies between two calls. Each of the
//! three placement rules carries its OWN sentence, so the sentence tells a client which pick
//! to change.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use crate::directory_dns_addresses::{ec2_client, network, ABSENT_SUBNET};
use crate::directory_lifecycle::{call, describe_one, ok, string, PASSWORD, REGION};

/// The sentence Directory Service refuses a placement whose subnets are not all in the VPC it
/// names with: fixed text, naming neither the subnets nor either VPC.
const SUBNETS_OUTSIDE_THE_VPC: &str =
    "Invalid subnet ID(s). Both subnets must belong to the specified VPC.";

/// The sentence Directory Service refuses a placement naming an identifier the account has no
/// subnet for with: the invalid-subnets opener ALONE, with no rule about a VPC or a zone a
/// subnet that is not there could satisfy — the shape the sibling refusal of a VPC the account
/// does not have ("Invalid VPC ID.") takes.
const UNHELD_SUBNET_IDS: &str = "Invalid subnet ID(s).";

/// What this service's refusal envelope appends to every diagnostic it words.
const REQUEST_ID_SEPARATOR: &str = " : RequestId: ";

/// One row: an operation that names a VPC for a placement, and the body it names it in.
struct PlacementCase {
    case: &'static str,
    operation: &'static str,
    body: fn(&str, &[String]) -> Value,
}

/// Every operation of this service that places directory servers in a VPC of its own request.
fn placement_rows() -> Vec<PlacementCase> {
    vec![
        PlacementCase {
            case: "a Simple AD create",
            operation: "CreateDirectory",
            body: |vpc, subnets| {
                json!({
                    "Name": "membership-simple.example.com",
                    "Password": PASSWORD,
                    "Size": "Small",
                    "VpcSettings": { "VpcId": vpc, "SubnetIds": subnets }
                })
            },
        },
        PlacementCase {
            case: "a Managed Microsoft AD create",
            operation: "CreateMicrosoftAD",
            body: |vpc, subnets| {
                json!({
                    "Name": "membership-managed.example.com",
                    "Password": PASSWORD,
                    "VpcSettings": { "VpcId": vpc, "SubnetIds": subnets }
                })
            },
        },
        PlacementCase {
            case: "an AD Connector create",
            operation: "ConnectDirectory",
            body: |vpc, subnets| {
                json!({
                    "Name": "membership-connected.example.com",
                    "Password": "Sim2Real-Connect-Pw1!",
                    "Size": "Small",
                    "ConnectSettings": {
                        "VpcId": vpc,
                        "SubnetIds": subnets,
                        "CustomerDnsIps": ["10.0.0.10", "10.0.0.11"],
                        "CustomerUserName": "ad-connector-svc"
                    }
                })
            },
        },
    ]
}

/// Assert `operation` with `body` is refused with the modelled `ClientException` and
/// `sentence`, whose only variable part is the call's own request id.
async fn refused_as_unservable_placement(
    router: &MultiServiceRouter,
    operation: &str,
    body: Value,
    sentence: &str,
    case: &str,
) {
    let (status, parsed) = call(router, operation, body).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected {operation} to refuse the placement, got {status} with \
         {parsed}"
    );
    assert_eq!(
        parsed["__type"],
        json!("ClientException"),
        "case {case:?}: expected the resource-semantics refusal, got {parsed}"
    );
    let reported = string(&parsed, "RequestId", case);
    let stated = parsed["Message"].as_str().unwrap_or_default();
    assert_eq!(
        stated,
        format!("{sentence}{REQUEST_ID_SEPARATOR}{reported}"),
        "case {case:?}: expected the placement sentence, got {parsed}"
    );
}

/// How many directories the store holds, read back through `DescribeDirectories`.
async fn stored_count(router: &MultiServiceRouter, case: &str) -> usize {
    let answer = ok(router, "DescribeDirectories", json!({}), case).await;
    answer["DirectoryDescriptions"]
        .as_array()
        .map(Vec::len)
        .unwrap_or_default()
}

/// RULE: a placement naming a subnet of another VPC is refused with `ClientException`, and
/// stores nothing.
#[tokio::test]
async fn a_placement_naming_a_subnet_of_another_vpc_is_refused() {
    let network = network().await;
    let router = &network.router;

    // The foreign subnet is one of the account's DEFAULT subnets — minted by the owner of
    // subnets in the account's default VPC, which is not the VPC this network's client
    // subnets were cut out of — and it sits in a different zone from the client subnet beside
    // it, so what refuses a row is the VPC membership and nothing else.
    let inside = network.client[0].id.clone();
    let outside = network.default[1].id.clone();
    // (case, the subnets the placement names) — the rows differ along WHERE the foreign
    // subnet sits in the list, and whether every entry is foreign.
    let picks: Vec<(&str, Vec<String>)> = vec![
        (
            "the foreign subnet named second",
            vec![inside.clone(), outside.clone()],
        ),
        (
            "the foreign subnet named first",
            vec![outside.clone(), inside.clone()],
        ),
        (
            "both subnets foreign",
            vec![
                network.default[0].id.clone(),
                network.default[1].id.clone(),
            ],
        ),
    ];

    let before = stored_count(router, "before any refused placement").await;
    for row in placement_rows() {
        for (pick, subnets) in &picks {
            let case = format!("{}: {pick}", row.case);
            refused_as_unservable_placement(
                router,
                row.operation,
                (row.body)(&network.vpc, subnets),
                SUBNETS_OUTSIDE_THE_VPC,
                &case,
            )
            .await;
        }
    }
    assert_eq!(
        stored_count(router, "after every refused placement").await,
        before,
        "expected a refused placement to store nothing"
    );
}

/// RULE (the positive control for the same rule): the same operations on subnets OF the VPC
/// they name are SERVED, so what refuses the rows above is the membership and not the
/// placement itself.
#[tokio::test]
async fn a_placement_naming_subnets_of_the_named_vpc_is_served() {
    let network = network().await;
    let router = &network.router;
    let subnets = vec![network.client[0].id.clone(), network.client[1].id.clone()];

    for row in placement_rows() {
        let case = format!("{}: subnets of the named VPC", row.case);
        let answer = ok(router, row.operation, (row.body)(&network.vpc, &subnets), &case).await;
        assert!(
            answer["DirectoryId"].as_str().is_some(),
            "case {case:?}: expected the placement to be served, got {answer}"
        );
    }
}

/// RULE: the add that replicates a directory into another Region resolves the membership the
/// same way — a replica placed in a subnet of another VPC is refused in the same class, with
/// the same sentence.
#[tokio::test]
async fn a_replica_naming_a_subnet_of_another_vpc_is_refused() {
    let network = network().await;
    let router = &network.router;
    let case = "a replica of a Managed Microsoft AD directory";

    let answer = ok(
        router,
        "CreateMicrosoftAD",
        json!({
            "Name": "membership-replicated.example.com",
            "Password": PASSWORD,
            "VpcSettings": {
                "VpcId": &network.vpc,
                "SubnetIds": [&network.client[0].id, &network.client[1].id]
            }
        }),
        case,
    )
    .await;
    let directory_id = string(&answer, "DirectoryId", case);

    refused_as_unservable_placement(
        router,
        "AddRegion",
        json!({
            "DirectoryId": &directory_id,
            "RegionName": REGION,
            "VPCSettings": {
                "VpcId": &network.vpc,
                "SubnetIds": [&network.client[0].id, &network.default[1].id]
            }
        }),
        SUBNETS_OUTSIDE_THE_VPC,
        case,
    )
    .await;
}

/// The FIXED sentence every create that places directory servers in a VPC refuses a settings
/// block naming no two DISTINCT subnets with: the rule the pick broke, quoting neither the
/// subnet ids nor how many were sent.
const SUBNET_PAIR_REFUSAL: &str = "Invalid subnet ID(s). They must correspond to two subnets \
                                   in different Availability Zones.";

/// A create body placing a directory named `name` in `subnets` of `vpc`, for `operation`.
///
/// One builder for the three creates, so a row differs only in the placement it names: the
/// name is a parameter here (unlike [`placement_rows`], whose bodies name one directory each),
/// because the tables below send many placements through the same operation.
fn create_body(operation: &str, name: &str, vpc: &str, subnets: &[String]) -> Value {
    match operation {
        "CreateDirectory" => json!({
            "Name": name,
            "Password": PASSWORD,
            "Size": "Small",
            "VpcSettings": { "VpcId": vpc, "SubnetIds": subnets }
        }),
        "CreateMicrosoftAD" => json!({
            "Name": name,
            "Password": PASSWORD,
            "VpcSettings": { "VpcId": vpc, "SubnetIds": subnets }
        }),
        "ConnectDirectory" => json!({
            "Name": name,
            "Password": "Sim2Real-Connect-Pw1!",
            "Size": "Small",
            "ConnectSettings": {
                "VpcId": vpc,
                "SubnetIds": subnets,
                "CustomerDnsIps": ["10.0.0.10", "10.0.0.11"],
                "CustomerUserName": "ad-connector-svc"
            }
        }),
        other => panic!("no create body for {other}"),
    }
}

/// The Availability Zones the OWNER of subnets reports for `subnet_ids`, each named once, in
/// zone order.
///
/// ASKED, never written here: the zones come back from EC2 over the same in-process seam this
/// service's own placement resolution reaches it through, so a case cannot pass against a zone
/// name this suite made up.
async fn zones_reported_for(
    router: &MultiServiceRouter,
    subnet_ids: &[String],
    case: &str,
) -> Vec<String> {
    let ec2 = ec2_client(&EmbeddedEmulator::from_router(router.clone()));
    let described = ec2
        .describe_subnets()
        .set_subnet_ids(Some(subnet_ids.to_vec()))
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: EC2 must report the subnets {subnet_ids:?}: {error:?}")
        });
    let mut zones: Vec<String> = described
        .subnets()
        .iter()
        .filter_map(|subnet| subnet.availability_zone().map(str::to_string))
        .collect();
    zones.sort();
    zones.dedup();
    zones
}

/// The `AvailabilityZones` of a described placement, sorted — the member has to be THERE, so a
/// description that omits it fails the case rather than reading as an empty list.
fn reported_zones(settings: &Value, case: &str) -> Vec<String> {
    let entries = settings["AvailabilityZones"].as_array().unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected AvailabilityZones on the described placement, got \
             {settings}"
        )
    });
    let mut zones: Vec<String> = entries
        .iter()
        .map(|zone| {
            zone.as_str()
                .unwrap_or_else(|| panic!("case {case:?}: expected a zone name, got {zone}"))
                .to_string()
        })
        .collect();
    zones.sort();
    zones
}

/// One row of the zone-reporting table: the create under test, the member whose description
/// carries the placement, and the subnets the directory's servers are placed in.
struct ZoneCase {
    case: &'static str,
    operation: &'static str,
    settings: &'static str,
    subnets: Vec<String>,
}

/// RULE: a directory's placement is REPORTED with the Availability Zones of the subnets it
/// named — the zones the owner of subnets holds for them, each named once however many of the
/// placement's subnets sit in it — so a client reads the zones the directory's servers occupy
/// off `DirectoryVpcSettingsDescription$AvailabilityZones` (or the connect-settings member for
/// an AD Connector) rather than off a zone list written anywhere in this service.
///
/// The rows differ along what the rule generalises over: the create that made the directory
/// and therefore which settings member reports the placement, how many subnets the placement
/// names, how many zones those subnets span, the order the create names them in, and whether
/// they are subnets a client cut or ones the account holds by default.
#[tokio::test]
async fn a_described_placement_reports_the_zones_of_the_subnets_it_names() {
    let network = network().await;
    let router = &network.router;
    let client = &network.client;
    let default = &network.default;

    let rows = [
        ZoneCase {
            case: "a Managed Microsoft AD in two subnets in two zones",
            operation: "CreateMicrosoftAD",
            settings: "VpcSettings",
            subnets: vec![client[0].id.clone(), client[1].id.clone()],
        },
        ZoneCase {
            // Three subnets, two of which share a zone: the zones are a SET, so the list is
            // shorter than the subnet list.
            case: "a Managed Microsoft AD whose three subnets span two zones",
            operation: "CreateMicrosoftAD",
            settings: "VpcSettings",
            subnets: vec![
                client[0].id.clone(),
                client[1].id.clone(),
                client[3].id.clone(),
            ],
        },
        ZoneCase {
            case: "a Simple AD whose two subnets are named in reverse zone order",
            operation: "CreateDirectory",
            settings: "VpcSettings",
            subnets: vec![client[2].id.clone(), client[1].id.clone()],
        },
        ZoneCase {
            case: "an AD Connector in two subnets in two zones",
            operation: "ConnectDirectory",
            settings: "ConnectSettings",
            subnets: vec![client[1].id.clone(), client[2].id.clone()],
        },
        ZoneCase {
            case: "a Managed Microsoft AD in the account's own default subnets",
            operation: "CreateMicrosoftAD",
            settings: "VpcSettings",
            subnets: vec![default[0].id.clone(), default[1].id.clone()],
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        // The VPC a row names is the VPC its own subnets sit in, as EC2 reports it: a
        // directory's servers are placed in subnets OF the VPC the create names.
        let placement_vpc = vpc_of(&network, &row.subnets[0], case);
        let expected = zones_reported_for(router, &row.subnets, case).await;
        assert!(
            expected.len() >= 2,
            "case {case:?}: the row must place the servers across at least two zones, EC2 \
             reports {expected:?}"
        );

        let name = format!("zones{index}.example.com");
        let answer = ok(
            router,
            row.operation,
            create_body(row.operation, &name, &placement_vpc, &row.subnets),
            case,
        )
        .await;
        let directory_id = string(&answer, "DirectoryId", case);

        let described = describe_one(router, &directory_id, case).await;
        let settings = &described[row.settings];
        assert_eq!(
            settings["VpcId"],
            json!(&placement_vpc),
            "case {case:?}: expected the placement to report the VPC it was made in, got \
             {described}"
        );
        assert_eq!(
            settings["SubnetIds"],
            json!(&row.subnets),
            "case {case:?}: expected the subnets the create named, got {described}"
        );

        let zones = reported_zones(settings, case);
        assert_eq!(
            zones, expected,
            "case {case:?}: expected the zones EC2 reports for {:?}, got {zones:?}",
            row.subnets
        );
        assert_eq!(
            zones.iter().collect::<BTreeSet<_>>().len(),
            zones.len(),
            "case {case:?}: expected each zone named once, got {zones:?}"
        );
        assert!(
            zones.len() <= row.subnets.len(),
            "case {case:?}: expected no more zones than subnets, got {zones:?} for {:?}",
            row.subnets
        );

        // Reading again reports the same zones: a describe derives them, it does not assign
        // them.
        let again = describe_one(router, &directory_id, case).await;
        assert_eq!(
            reported_zones(&again[row.settings], case),
            zones,
            "case {case:?}: expected a second read to report the same zones, got {again}"
        );
    }
}

/// RULE: a directory's servers sit in two subnets in two different Availability Zones, so a
/// settings block that names no TWO DISTINCT subnets cannot carry them and is refused — by
/// every create that places servers in a VPC, with `InvalidParameterException` and the one
/// FIXED sentence Directory Service states that rule in, before anything is stored.
///
/// The rows differ along what the rule generalises over: how many entries the list carries
/// (one, the same one twice, the same one three times), which subnet of the account it names,
/// and whether that subnet is one a client cut or one the account holds by default. Every
/// subnet named here is one EC2 really minted, so no row can pass by naming an identifier the
/// account has no subnet for.
#[tokio::test]
async fn a_placement_naming_no_two_distinct_subnets_is_refused_with_the_stated_rule() {
    let network = network().await;
    let router = &network.router;
    let picks: Vec<(&str, Vec<String>)> = vec![
        ("one subnet of the client's VPC", vec![network.client[0].id.clone()]),
        (
            "one subnet of the account's default VPC",
            vec![network.default[0].id.clone()],
        ),
        (
            "the same subnet twice",
            vec![network.client[1].id.clone(), network.client[1].id.clone()],
        ),
        (
            "the same subnet three times",
            vec![
                network.client[2].id.clone(),
                network.client[2].id.clone(),
                network.client[2].id.clone(),
            ],
        ),
    ];

    let before = stored_count(router, "before any refused placement").await;
    for row in placement_rows() {
        for (pick, subnets) in &picks {
            let case = format!("{}: {pick}", row.case);
            // The VPC is the one those subnets really sit in, so what refuses the row is the
            // subnet pick and nothing else.
            let placement_vpc = vpc_of(&network, &subnets[0], &case);
            let (status, parsed) = call(
                router,
                row.operation,
                create_body(row.operation, "pair.example.com", &placement_vpc, subnets),
            )
            .await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected the placement to be refused, got {status} with {parsed}"
            );
            assert_eq!(
                parsed["__type"],
                json!("InvalidParameterException"),
                "case {case:?}: expected the modelled parameter refusal, got {parsed}"
            );
            let reported = string(&parsed, "RequestId", &case);
            let stated = parsed["Message"].as_str().unwrap_or_default();
            assert_eq!(
                stated,
                format!("{SUBNET_PAIR_REFUSAL}{REQUEST_ID_SEPARATOR}{reported}"),
                "case {case:?}: expected the stated subnet-pair rule, got {parsed}"
            );
            // The sentence states the rule and quotes nothing of the request: not a subnet,
            // not the VPC, not the count.
            for quoted in [subnets[0].as_str(), placement_vpc.as_str()] {
                assert!(
                    !stated.contains(quoted),
                    "case {case:?}: expected the sentence to quote nothing of the request, \
                     found {quoted:?} in {stated:?}"
                );
            }
        }
    }
    assert_eq!(
        stored_count(router, "after every refused placement").await,
        before,
        "expected a refused placement to store nothing"
    );
}

// ---------------------------------------------------------------------------
// RULE: each placement rule is stated in its OWN fixed sentence, by every operation that
// places directory servers, with the call's request id as the message's only variable part.
// ---------------------------------------------------------------------------

/// The FIXED sentence a pick the owner of subnets reports in ONE Availability Zone is refused
/// with: the same documented rule as [`SUBNET_PAIR_REFUSAL`] — a directory's servers sit in two
/// subnets in two different zones — stated in its own sentence and answered in its own error
/// class, because the two refusals are recorded apart.
const SUBNETS_IN_ONE_ZONE: &str =
    "Invalid subnet ID(s). The two subnets must be in different Availability Zones.";

/// The opener every one of this service's placement sentences names the invalid part of the
/// request with.
const INVALID_SUBNETS_OPENER: &str = "Invalid subnet ID(s). ";

/// The modelled refusal a defect of the REQUEST is reported as.
const REQUEST_REFUSAL: &str = "InvalidParameterException";

/// The modelled refusal a defect of the account's NETWORK is reported as.
const NETWORK_REFUSAL: &str = "ClientException";

/// One row of the wording table: a subnet pick no placement can carry, and the refusal every
/// operation that places directory servers has to answer it with.
struct PickCase {
    /// What is wrong with the pick, for the assertion messages.
    case: String,
    /// The VPC the placement names.
    vpc: String,
    /// The subnets the placement names.
    subnets: Vec<String>,
    /// The `__type` the refusal reports.
    code: &'static str,
    /// The sentence the refusal states the broken rule in.
    sentence: &'static str,
}

/// Every unservable pick the wording table sends through every placement operation, with the
/// refusal each one has to get.
///
/// The rows differ along what the rule generalises over: WHICH of the three placement rules the
/// pick breaks, and how it breaks it — the number of entries the list carries, the repetition
/// in it, the order the subnets are named in, and whether a named subnet is one a client cut or
/// one the account holds by default.
///
/// Built over the network EC2 holds: the zones the one-zone rows rest on are ASKED for here, so
/// a row whose subnets no longer share a zone fails loudly instead of passing for another
/// reason.
async fn unservable_picks(network: &crate::directory_dns_addresses::Network) -> Vec<PickCase> {
    let router = &network.router;
    let client = &network.client;
    let default = &network.default;
    let vpc = network.vpc.clone();

    // A pair of subnets of the named VPC that EC2 reports in ONE zone, and one it reports in
    // two: the zones come back from the owner of subnets, never from this suite.
    let same_zone = vec![client[0].id.clone(), client[3].id.clone()];
    let zones = zones_reported_for(router, &same_zone, "the one-zone pick").await;
    assert_eq!(
        zones.len(),
        1,
        "setup: the one-zone rows need a pair EC2 reports in one zone, got {zones:?}"
    );

    let mut rows = vec![
        PickCase {
            case: "one subnet of the named VPC".to_string(),
            vpc: vpc.clone(),
            subnets: vec![client[0].id.clone()],
            code: REQUEST_REFUSAL,
            sentence: SUBNET_PAIR_REFUSAL,
        },
        PickCase {
            case: "the same subnet twice".to_string(),
            vpc: vpc.clone(),
            subnets: vec![client[1].id.clone(), client[1].id.clone()],
            code: REQUEST_REFUSAL,
            sentence: SUBNET_PAIR_REFUSAL,
        },
        PickCase {
            case: "the same subnet three times".to_string(),
            vpc: vpc.clone(),
            subnets: vec![
                client[2].id.clone(),
                client[2].id.clone(),
                client[2].id.clone(),
            ],
            code: REQUEST_REFUSAL,
            sentence: SUBNET_PAIR_REFUSAL,
        },
        PickCase {
            case: format!("two subnets EC2 reports in {}", zones[0]),
            vpc: vpc.clone(),
            subnets: same_zone.clone(),
            code: NETWORK_REFUSAL,
            sentence: SUBNETS_IN_ONE_ZONE,
        },
        PickCase {
            case: "the same one-zone pair in the other order".to_string(),
            vpc: vpc.clone(),
            subnets: vec![same_zone[1].clone(), same_zone[0].clone()],
            code: NETWORK_REFUSAL,
            sentence: SUBNETS_IN_ONE_ZONE,
        },
    ];

    // The membership rule: the placement names the VPC the client cut its subnets out of, and a
    // subnet of ANOTHER VPC in either position, or only subnets of that other VPC.
    for (where_foreign, picked) in [
        (
            "a subnet of another VPC named second",
            vec![client[0].id.clone(), default[1].id.clone()],
        ),
        (
            "a subnet of another VPC named first",
            vec![default[1].id.clone(), client[0].id.clone()],
        ),
        (
            "both subnets of another VPC",
            vec![default[0].id.clone(), default[1].id.clone()],
        ),
    ] {
        rows.push(PickCase {
            case: where_foreign.to_string(),
            vpc: vpc.clone(),
            subnets: picked,
            code: NETWORK_REFUSAL,
            sentence: SUBNETS_OUTSIDE_THE_VPC,
        });
    }
    rows
}

/// Assert `status`/`parsed` is the refusal `row` has to get, and return the message with the
/// call's own request id taken back out of it.
fn stated_sentence(status: u16, parsed: &Value, row: &PickCase, case: &str) -> String {
    assert_eq!(
        status, 400,
        "case {case:?}: expected the placement to be refused, got {status} with {parsed}"
    );
    assert_eq!(
        parsed["__type"],
        json!(row.code),
        "case {case:?}: expected the modelled {} refusal, got {parsed}",
        row.code
    );
    let request_id = string(parsed, "RequestId", case);
    let message = string(parsed, "Message", case);
    assert_eq!(
        message,
        format!("{}{REQUEST_ID_SEPARATOR}{request_id}", row.sentence),
        "case {case:?}: expected the sentence the broken rule is stated in, got {message:?}"
    );
    // The sentence states the rule and quotes nothing of the request: not a subnet, not the
    // VPC, not how many subnets were named.
    for quoted in [row.subnets[0].as_str(), row.vpc.as_str()] {
        assert!(
            !message.contains(quoted),
            "case {case:?}: expected the sentence to quote nothing of the request, found \
             {quoted:?} in {message:?}"
        );
    }
    message.replace(&request_id, "")
}

/// RULE: every operation that places a directory's servers in a VPC refuses an unservable pick
/// with the one FIXED sentence the rule that pick broke is stated in, followed by the call's own
/// request id — and stores nothing. The three rules carry three DIFFERENT sentences, all opened
/// by the same invalid-subnets clause, so the sentence is what tells a client which pick to
/// change.
#[tokio::test]
async fn every_placement_states_the_rule_its_pick_broke_in_one_fixed_sentence() {
    let network = network().await;
    let router = &network.router;
    let rows = unservable_picks(&network).await;

    // The directory the add below replicates: placed in a pair EC2 reports in two zones of the
    // VPC it names, so what refuses each row of that table is the row's own pick. The placement
    // an add asks for is resolved before the Region and stage gates, which is why the rows reach
    // the same decision a create's do.
    let replicated = ok(
        router,
        "CreateMicrosoftAD",
        json!({
            "Name": "wording-replicated.example.com",
            "Password": PASSWORD,
            "Edition": "Enterprise",
            "VpcSettings": {
                "VpcId": &network.vpc,
                "SubnetIds": [&network.client[0].id, &network.client[1].id]
            }
        }),
        "the directory AddRegion replicates",
    )
    .await;
    let replicated = string(&replicated, "DirectoryId", "the directory AddRegion replicates");

    let before = stored_count(router, "before any refused placement").await;
    for (index, row) in rows.iter().enumerate() {
        for operation in [
            "CreateDirectory",
            "CreateMicrosoftAD",
            "ConnectDirectory",
            "AddRegion",
        ] {
            let case = format!("{operation} with {}", row.case);
            let body = if operation == "AddRegion" {
                json!({
                    "DirectoryId": &replicated,
                    "RegionName": REGION,
                    "VPCSettings": { "VpcId": &row.vpc, "SubnetIds": &row.subnets },
                })
            } else {
                create_body(
                    operation,
                    &format!("wording{index}.example.com"),
                    &row.vpc,
                    &row.subnets,
                )
            };
            let (status, parsed) = call(router, operation, body).await;
            let stated = stated_sentence(status, &parsed, row, &case);
            // One sentence per rule: with the call's own request id taken out, nothing of the
            // message differs between two calls that broke the same rule, whichever operation
            // they called and however they broke it.
            assert_eq!(
                stated,
                format!("{}{REQUEST_ID_SEPARATOR}", row.sentence),
                "case {case:?}: expected the rule's one fixed sentence, got {stated:?}"
            );
        }
    }

    // The three rules are DISTINGUISHABLE, and each names the subnet pick as the invalid part of
    // the request: a client that branches on the message can tell a pick it has to re-zone from
    // one it has to re-pick inside the right VPC.
    let sentences = [
        SUBNET_PAIR_REFUSAL,
        SUBNETS_IN_ONE_ZONE,
        SUBNETS_OUTSIDE_THE_VPC,
    ];
    for (position, sentence) in sentences.iter().enumerate() {
        assert!(
            sentence.starts_with(INVALID_SUBNETS_OPENER),
            "case \"the placement sentences\": expected every one to open with \
             {INVALID_SUBNETS_OPENER:?}, got {sentence:?}"
        );
        for other in &sentences[position + 1..] {
            assert_ne!(
                sentence, other,
                "case \"the placement sentences\": expected one sentence per rule, got \
                 {sentence:?} twice"
            );
        }
    }

    assert_eq!(
        stored_count(router, "after every refused placement").await,
        before,
        "case \"after every refused placement\": expected a refused placement to store nothing"
    );
}

/// The VPC the owner of subnets reports `subnet_id` in, as the account's network holds it.
fn vpc_of(
    network: &crate::directory_dns_addresses::Network,
    subnet_id: &str,
    case: &str,
) -> String {
    network
        .client
        .iter()
        .chain(network.default.iter())
        .find(|subnet| subnet.id == subnet_id)
        .map(|subnet| subnet.vpc.clone())
        .unwrap_or_else(|| panic!("case {case:?}: the account must hold the subnet {subnet_id}"))
}

/// Well-formed `SubnetId`s the account has NO subnet for, as `(id, form)`.
///
/// Both forms the modelled pattern admits — `^(subnet-[0-9a-f]{8}|subnet-[0-9a-f]{17})$`,
/// `smithy_query(query="shape",shape="SubnetId")` — with different digits, so no row below can
/// pass by being refused for the SHAPE of its identifier instead of for the network it names.
/// One of them is the identifier the sibling suites use for a subnet EC2 mints nothing for.
const ABSENT_SUBNETS: [(&str, &str); 3] = [
    ("subnet-0a1b2c3d", "the eight-digit form"),
    ("subnet-00000000000000000", "a seventeen-digit id of all zeros"),
    (ABSENT_SUBNET, "a seventeen-digit id of mixed digits"),
];

/// Assert the OWNER of subnets really holds no subnet for `subnet_id`, so a row that names it
/// is refused for the network it names and not because this suite says so.
async fn the_account_holds_no_subnet(router: &MultiServiceRouter, subnet_id: &str, case: &str) {
    let ec2 = ec2_client(&EmbeddedEmulator::from_router(router.clone()));
    let reported: Vec<String> = ec2
        .describe_subnets()
        .filters(
            ::aws_sdk_ec2::types::Filter::builder()
                .name("subnet-id")
                .values(subnet_id)
                .build(),
        )
        .send()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: EC2 must answer DescribeSubnets: {error:?}"))
        .subnets()
        .iter()
        .filter_map(|subnet| subnet.subnet_id().map(str::to_string))
        .collect();
    assert!(
        reported.is_empty(),
        "case {case:?}: the account must hold no subnet for {subnet_id}, EC2 reports {reported:?}"
    );
}

/// RULE: a placement is servable only where the owner of subnets HAS a subnet for every
/// identifier it names — so one naming a well-formed identifier that owner holds no subnet for
/// names a network that does not exist, and is refused in the resource-semantics class with the
/// invalid-subnets opener alone, no rule about a VPC or a zone the missing subnet could satisfy.
/// Nothing is stored, and no directory is minted.
///
/// The rows differ along what the rule generalises over: the operation that places the
/// servers, the form of the identifier the account has no subnet for, where in the list it
/// sits, and whether the rest of the list is a subnet of the named VPC or absent too.
#[tokio::test]
async fn a_placement_naming_a_subnet_the_account_does_not_have_is_refused() {
    let network = network().await;
    let router = &network.router;
    let held = network.client[0].id.clone();

    // (case, the subnets the placement names)
    let mut picks: Vec<(String, Vec<String>)> = Vec::new();
    for (absent, form) in ABSENT_SUBNETS {
        the_account_holds_no_subnet(router, absent, form).await;
        picks.push((
            format!("{form}, named second beside a subnet of the VPC"),
            vec![held.clone(), absent.to_string()],
        ));
        picks.push((
            format!("{form}, named first beside a subnet of the VPC"),
            vec![absent.to_string(), held.clone()],
        ));
    }
    picks.push((
        "two identifiers the account has no subnet for".to_string(),
        vec![
            ABSENT_SUBNETS[0].0.to_string(),
            ABSENT_SUBNETS[1].0.to_string(),
        ],
    ));

    let before = stored_count(router, "before any refused placement").await;
    for row in placement_rows() {
        for (pick, subnets) in &picks {
            let case = format!("{}: {pick}", row.case);
            refused_as_unservable_placement(
                router,
                row.operation,
                (row.body)(&network.vpc, subnets),
                UNHELD_SUBNET_IDS,
                &case,
            )
            .await;
        }
    }
    assert_eq!(
        stored_count(router, "after every refused placement").await,
        before,
        "expected a refused placement to store nothing"
    );
}

/// RULE: the add that replicates a directory into another Region resolves the same way — a
/// replica placed in an identifier the account has no subnet for is refused in the same class,
/// with the same sentence, and the directory keeps the one Region it was created in.
#[tokio::test]
async fn a_replica_naming_a_subnet_the_account_does_not_have_is_refused() {
    let network = network().await;
    let router = &network.router;
    let case = "a replica of a Managed Microsoft AD directory";

    let answer = ok(
        router,
        "CreateMicrosoftAD",
        json!({
            "Name": "membership-unheld.example.com",
            "Password": PASSWORD,
            "Edition": "Enterprise",
            "VpcSettings": {
                "VpcId": &network.vpc,
                "SubnetIds": [&network.client[0].id, &network.client[1].id]
            }
        }),
        case,
    )
    .await;
    let directory_id = string(&answer, "DirectoryId", case);

    for (absent, form) in ABSENT_SUBNETS {
        refused_as_unservable_placement(
            router,
            "AddRegion",
            json!({
                "DirectoryId": &directory_id,
                "RegionName": REGION,
                "VPCSettings": {
                    "VpcId": &network.vpc,
                    "SubnetIds": [&network.client[0].id, absent]
                }
            }),
            UNHELD_SUBNET_IDS,
            &format!("{case}: {form}"),
        )
        .await;
    }

    // Nothing was recorded by any of those: the directory still holds the ONE Region it was
    // created in, read back through the operation that reports the inventory.
    let inventory = ok(
        router,
        "DescribeRegions",
        json!({ "DirectoryId": &directory_id }),
        case,
    )
    .await;
    let regions: Vec<String> = inventory["RegionsDescription"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|region| region["RegionName"].as_str().map(str::to_string))
        .collect();
    assert_eq!(
        regions,
        vec![REGION.to_string()],
        "case {case:?}: expected the directory to keep the one Region it was created in, got \
         {regions:?}"
    );
}
