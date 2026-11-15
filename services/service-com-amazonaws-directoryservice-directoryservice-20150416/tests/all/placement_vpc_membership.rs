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
//! for the directory servers", which are subnets OF that VPC. A placement naming a subnet the
//! owner of subnets reports in another VPC therefore asks for servers in a network the
//! directory is not being created in, and is refused with the resource-semantics failure
//! class — `ClientException`, HTTP 400, with this service's placement sentence and the call's
//! own request id — before anything is stored. A subnet EC2 reports nothing for leaves its
//! membership unknown and refuses nothing, which is the same reading every other half of the
//! placement rule takes.
//!
//! It is asserted over a TABLE rather than one call, along what the rule generalises over:
//! every operation that names a VPC for a placement (both creates that make a directory, the
//! create that connects one, and the add that replicates one into another Region), and the
//! position the foreign subnet takes in the list — with the same operations on subnets OF the
//! named VPC as the positive control.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::directory_dns_addresses::network;
use crate::directory_lifecycle::{call, ok, string, PASSWORD, REGION};

/// The sentence Directory Service refuses a placement whose subnets are not all in the VPC it
/// names with: fixed text, naming neither the subnets nor either VPC.
const SUBNETS_OUTSIDE_THE_VPC: &str =
    "Invalid subnet ID(s). The subnets must belong to the VPC specified.";

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

/// Assert `operation` with `body` is refused with the modelled `ClientException` and the
/// placement sentence, whose only variable part is the call's own request id.
async fn refused_as_outside_the_vpc(
    router: &MultiServiceRouter,
    operation: &str,
    body: Value,
    case: &str,
) {
    let (status, parsed) = call(router, operation, body).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected {operation} to refuse the foreign subnet, got {status} with \
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
        format!("{SUBNETS_OUTSIDE_THE_VPC}{REQUEST_ID_SEPARATOR}{reported}"),
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
            refused_as_outside_the_vpc(router, row.operation, (row.body)(&network.vpc, subnets), &case)
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

    refused_as_outside_the_vpc(
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
        case,
    )
    .await;
}
