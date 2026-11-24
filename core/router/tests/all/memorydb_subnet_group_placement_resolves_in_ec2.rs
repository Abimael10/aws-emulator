// SPDX-License-Identifier: CC0-1.0

//! Amazon MemoryDB resolves the network a SUBNET GROUP places a cluster in against Amazon
//! EC2, which owns it — and reports what that service answered.
//!
//! RULE: a subnet is EC2's resource, so every `SubnetIds` entry a `CreateSubnetGroup` or
//! `UpdateSubnetGroup` names is resolved against the account's EC2 inventory before anything
//! is written, and the group reports only what that inventory says:
//!
//! * `Subnet$SupportedNetworkTypes` is the network types the subnet's own ADDRESS FAMILIES
//!   support — `ipv4` for a subnet with an IPv4 block, `ipv6` for one with an IPv6 block, and
//!   both for one addressed in both, which is what that member's documentation calls a
//!   dual-stack deployment — and `SubnetGroup$SupportedNetworkTypes` is the types EVERY
//!   subnet of the group supports, since a cluster is given addresses in whichever
//!   subnet of the group MemoryDB places a node in. Every answer carrying a group reports them:
//!   the create, the update and the describe alike.
//! * an identifier EC2 holds no subnet for is refused with the modelled
//!   `InvalidParameterValueException` (HTTP 400) naming the offending identifiers, and the
//!   request writes nothing.
//!
//! The suite lives beside the router because the network is EC2's: the VPCs and subnets below
//! are minted by the real EC2 operations on the SAME engine that serves MemoryDB, so every
//! identifier in an accepted request is one EC2 handed out and every refusal is grounded in
//! what EC2 actually answered.
//!
//! The rows differ along what the rules generalise over: how many subnets a group names and in
//! which order, which address families each of them is addressed in (IPv4-only, dual-stack, and
//! a group mixing the two), which VPC they belong to, whether the group was created or updated
//! over them, the FORM of an identifier that resolves to nothing (EC2's 17-hex and 8-hex id
//! forms), and where in the list it sits. An IPv6-ONLY subnet is not among the rows: this
//! emulator's EC2 answers `CreateSubnet` with an IPv6 block HTTP 501, so no such subnet of the
//! account exists to place a group over.
//!
//! The third rule here is about the page size that listing serves: a `MaxResults` outside its
//! window names no page and is refused with the same modelled `InvalidParameterValueException`
//! — the only error code this service's model exposes for a value it cannot serve — naming the
//! window's bounds.
//!
//! Sources: `smithy_query(query="shape", shape="SubnetGroup,Subnet,NetworkType")` and
//! `smithy_query(query="doc", shape="Subnet", member="SupportedNetworkTypes")` on the
//! memorydb model (`SupportedNetworkTypes` targets `NetworkTypeList` of the `NetworkType`
//! enum, and "can include 'ipv4', 'ipv6', or both"; the enum's `dual_stack` value is what
//! `Cluster$NetworkType` reports for a cluster addressed in both);
//! `smithy_query(query="shape", shape="InvalidParameterValueException")` (client error,
//! httpError 400).

use std::net::Ipv6Addr;
use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The EC2 API version a client sends in an ec2Query body.
const EC2_API_VERSION: &str = "2016-11-15";

/// The network types the model's `NetworkType` enum declares for an address family, spelled
/// out HERE rather than taken from the crate under test.
const IPV4: &str = "ipv4";
const IPV6: &str = "ipv6";

/// The bounds of the page-size window `DescribeSubnetGroups` serves, and the sentence a size
/// outside it is refused with — the service's own wording, spelled out here for the same
/// reason.
const PAGE_SIZE_MIN: i64 = 20;
const PAGE_SIZE_MAX: i64 = 100;
const PAGE_SIZE_MESSAGE: &str = "Invalid value for max records. Must be between 20 and 100";

/// The error code this service answers a value it cannot serve with.
const INVALID_VALUE: &str = "InvalidParameterValueException";

/// One emulator carrying Amazon EC2 and Amazon MemoryDB over ONE shared engine, as the
/// production router mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_memorydb_amazonmemorydb::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is what
/// routes the request here; the signature is never checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Call one MemoryDB operation over its `awsJson1_1` wire, answering `(status, body)`.
async fn memorydb(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("memory-db.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AmazonMemoryDB.{operation}"))
        .header("authorization", authorization("memorydb"))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, answered)
}

/// Call one MemoryDB operation that must be served, answering its body.
async fn served(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
    case: &str,
) -> Value {
    let (status, answered) = memorydb(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected {operation} to answer 200, got {status} with {answered}"
    );
    answered
}

/// The wire error code of a failure body (`__type`, with any namespace prefix dropped).
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The `message` of a failure body, under either spelling of the member.
fn error_message(body: &Value, case: &str) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a message in {body}"))
        .to_string()
}

/// One ec2Query action that must succeed, answered with its XML body.
async fn ec2(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version={EC2_API_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ec2.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("ec2"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with {text}"
    );
    text
}

/// One subnet EC2 minted: the identifier it handed out, the zone it put it in, and the
/// network types its address families support.
#[derive(Debug, Clone)]
struct Subnet {
    id: String,
    zone: String,
    network_types: Vec<&'static str>,
}

/// The `/64` block at offset `index` inside the `/56` an Amazon-provided VPC association
/// carries, which is the size a subnet's IPv6 block has.
fn subnet_ipv6_block(vpc_block: &str, index: u16) -> String {
    let (address, _) = vpc_block
        .split_once('/')
        .unwrap_or_else(|| panic!("setup: the VPC's IPv6 block {vpc_block:?} names no prefix"));
    let parsed: Ipv6Addr = address
        .parse()
        .unwrap_or_else(|_| panic!("setup: {address:?} is no IPv6 address"));
    let mut segments = parsed.segments();
    segments[3] += index;
    format!("{}/64", Ipv6Addr::from(segments))
}

/// Create one VPC with one subnet per entry of `zones`, through EC2's own operations, and
/// associate an IPv6 block with the first `dual_stack` of them.
///
/// The VPC takes an Amazon-provided IPv6 block when any of its subnets is to be dual-stack,
/// because a subnet's block is cut out of its VPC's.
async fn ec2_network(
    router: &MultiServiceRouter,
    second_octet: u8,
    zones: &[&str],
    dual_stack: usize,
) -> (String, Vec<Subnet>) {
    let cidr = format!("10.{second_octet}.0.0/16");
    let mut params: Vec<(&str, &str)> = vec![("CidrBlock", cidr.as_str())];
    if dual_stack > 0 {
        params.push(("AmazonProvidedIpv6CidrBlock", "true"));
    }
    let created = ec2(router, "CreateVpc", &params).await;
    let vpc_id = element(&created, "vpcId");
    let vpc_ipv6_block = (dual_stack > 0).then(|| element(&created, "ipv6CidrBlock"));

    let mut subnets = Vec::new();
    for (index, zone) in zones.iter().enumerate() {
        let subnet_cidr = format!("10.{second_octet}.{}.0/24", index + 1);
        let subnet_id = element(
            &ec2(
                router,
                "CreateSubnet",
                &[
                    ("VpcId", vpc_id.as_str()),
                    ("CidrBlock", subnet_cidr.as_str()),
                    ("AvailabilityZone", zone),
                ],
            )
            .await,
            "subnetId",
        );
        let mut network_types = vec![IPV4];
        if index < dual_stack {
            let block = subnet_ipv6_block(
                vpc_ipv6_block
                    .as_deref()
                    .expect("setup: a dual-stack subnet needs its VPC's IPv6 block"),
                u16::try_from(index).unwrap_or_default(),
            );
            ec2(
                router,
                "AssociateSubnetCidrBlock",
                &[
                    ("SubnetId", subnet_id.as_str()),
                    ("Ipv6CidrBlock", block.as_str()),
                ],
            )
            .await;
            network_types = vec![IPV4, IPV6];
        }
        subnets.push(Subnet {
            id: subnet_id,
            zone: (*zone).to_string(),
            network_types,
        });
    }
    (vpc_id, subnets)
}

/// What the service must report for a group placed over `subnets`, in the order they were
/// named: each subnet's identifier, zone and own network types, and the group's own types —
/// the ones every one of its subnets supports.
fn expected_members(subnets: &[&Subnet]) -> (Value, Value) {
    let reported: Vec<Value> = subnets
        .iter()
        .map(|subnet| {
            json!({
                "Identifier": subnet.id,
                "AvailabilityZone": { "Name": subnet.zone },
                "SupportedNetworkTypes": subnet.network_types,
            })
        })
        .collect();
    let shared: Vec<&str> = subnets
        .first()
        .map(|first| {
            first
                .network_types
                .iter()
                .copied()
                .filter(|network_type| {
                    subnets
                        .iter()
                        .all(|subnet| subnet.network_types.contains(network_type))
                })
                .collect()
        })
        .unwrap_or_default();
    (Value::Array(reported), json!(shared))
}

/// Assert that `group` reports exactly the placement `subnets` says it has.
fn assert_placement(group: &Value, subnets: &[&Subnet], vpc_id: &str, case: &str) {
    let (expected_subnets, expected_types) = expected_members(subnets);
    assert_eq!(
        group["Subnets"], expected_subnets,
        "case {case:?}: expected each subnet to carry the zone and the network types EC2's \
         record of it gives, got {group}"
    );
    assert_eq!(
        group["SupportedNetworkTypes"], expected_types,
        "case {case:?}: expected the group to support the types every one of its subnets \
         supports, got {group}"
    );
    assert_eq!(
        group["VpcId"],
        json!(vpc_id),
        "case {case:?}: expected the VPC EC2 reports for those subnets, got {group}"
    );
}

/// Values of EC2's two published subnet-id forms that name no subnet of this account.
///
/// `SubnetIds` carries no modelled pattern, and MemoryDB holds an identifier to the EC2 form
/// (`subnet-` plus 8 or 17 lower-case hex digits) before resolving it, so these are
/// well-formed values that simply resolve to nothing.
fn absent_subnet_ids() -> Vec<(&'static str, &'static str)> {
    vec![
        ("EC2's 17-hex id form", "subnet-0f1e2d3c4b5a69780"),
        ("EC2's older 8-hex id form", "subnet-1a2b3c4d"),
        ("the all-zero id form", "subnet-00000000000000000"),
    ]
}

/// The sentence MemoryDB refuses `SubnetIds` that name no subnet with.
fn absent_message(subnet_ids: &[&str]) -> String {
    format!(
        "The provided input subnet ids are invalid : [{}].",
        subnet_ids.join(", ")
    )
}

/// The names `DescribeSubnetGroups` reports for the whole listing.
async fn listed_names(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let listing = served(router, "DescribeSubnetGroups", &json!({}), case).await;
    listing["SubnetGroups"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|group| group["Name"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// A created group reports the network types EC2's record of each subnet supports, and so do
/// the describe and the update that follow — one placement, reported the same way wherever it
/// is answered from.
#[tokio::test]
async fn a_subnet_group_reports_the_network_types_ec2_holds_for_its_subnets() {
    let router = emulator();
    // An IPv4-only VPC, and one whose first two subnets are dual-stack while its third is
    // IPv4-only — which is what lets a group MIX the two capabilities inside one VPC.
    let (ipv4_vpc, ipv4_subnets) =
        ec2_network(&router, 70, &["us-west-2a", "us-west-2b", "us-west-2c"], 0).await;
    let (dual_vpc, dual_subnets) =
        ec2_network(&router, 71, &["us-west-2a", "us-west-2b", "us-west-2c"], 2).await;

    // (case, the group's name, the VPC, the subnets it is placed over, in request order)
    let rows: Vec<(&str, &str, &str, Vec<&Subnet>)> = vec![
        (
            "one IPv4-only subnet",
            "ipv4-one",
            &ipv4_vpc,
            vec![&ipv4_subnets[0]],
        ),
        (
            "three IPv4-only subnets, in an order that is not sorted",
            "ipv4-three",
            &ipv4_vpc,
            vec![&ipv4_subnets[2], &ipv4_subnets[0], &ipv4_subnets[1]],
        ),
        (
            "one dual-stack subnet",
            "dual-one",
            &dual_vpc,
            vec![&dual_subnets[0]],
        ),
        (
            "two dual-stack subnets",
            "dual-two",
            &dual_vpc,
            vec![&dual_subnets[0], &dual_subnets[1]],
        ),
        (
            "a dual-stack subnet beside an IPv4-only one of the same VPC",
            "dual-mixed",
            &dual_vpc,
            vec![&dual_subnets[0], &dual_subnets[2]],
        ),
        (
            "an IPv4-only subnet named first, then a dual-stack one",
            "mixed-ipv4-first",
            &dual_vpc,
            vec![&dual_subnets[2], &dual_subnets[1]],
        ),
    ];

    for (case, name, vpc_id, subnets) in &rows {
        let subnet_ids: Vec<&str> = subnets.iter().map(|subnet| subnet.id.as_str()).collect();
        let created = served(
            &router,
            "CreateSubnetGroup",
            &json!({ "SubnetGroupName": name, "SubnetIds": subnet_ids }),
            case,
        )
        .await;
        assert_placement(&created["SubnetGroup"], subnets, vpc_id, case);

        // The round trip: the describe answers the same placement from state.
        let described = served(
            &router,
            "DescribeSubnetGroups",
            &json!({ "SubnetGroupName": name }),
            case,
        )
        .await;
        assert_eq!(
            described["SubnetGroups"][0], created["SubnetGroup"],
            "case {case:?}: a describe must report the group the create persisted, got \
             {described}"
        );
    }

    // AN UPDATE reports the types of the subnets it moves the group ONTO, not the ones it had:
    // the IPv4-only group below becomes a dual-stack one, and both readers agree afterwards.
    let moved = vec![&dual_subnets[0], &dual_subnets[1]];
    let moved_ids: Vec<&str> = moved.iter().map(|subnet| subnet.id.as_str()).collect();
    let case = "an update onto two dual-stack subnets";
    let updated = served(
        &router,
        "UpdateSubnetGroup",
        &json!({ "SubnetGroupName": "dual-mixed", "SubnetIds": moved_ids }),
        case,
    )
    .await;
    assert_placement(&updated["SubnetGroup"], &moved, &dual_vpc, case);
    let described = served(
        &router,
        "DescribeSubnetGroups",
        &json!({ "SubnetGroupName": "dual-mixed" }),
        case,
    )
    .await;
    assert_eq!(
        described["SubnetGroups"][0], updated["SubnetGroup"],
        "case {case:?}: a describe must report the placement the update persisted, got \
         {described}"
    );

    // And the whole listing reports every one of them, each still carrying its own types.
    let listing = served(&router, "DescribeSubnetGroups", &json!({}), "the full listing").await;
    for group in listing["SubnetGroups"].as_array().cloned().unwrap_or_default() {
        let name = group["Name"].as_str().unwrap_or_default().to_string();
        let subnets = group["Subnets"].as_array().cloned().unwrap_or_default();
        assert!(
            !subnets.is_empty(),
            "case {name:?}: the listing must report the group's subnets, got {group}"
        );
        for subnet in &subnets {
            assert!(
                subnet["SupportedNetworkTypes"]
                    .as_array()
                    .is_some_and(|types| types.contains(&json!(IPV4))),
                "case {name:?}: every subnet of this listing is addressed in IPv4, so it must \
                 report that type, got {subnet}"
            );
        }
        assert!(
            group["SupportedNetworkTypes"]
                .as_array()
                .is_some_and(|types| types.contains(&json!(IPV4))),
            "case {name:?}: the listing must report the group's own network types, got {group}"
        );
    }
}

/// A `SubnetIds` entry EC2 holds no subnet for is refused with the modelled invalid-value
/// error naming the offending identifiers, and the request writes nothing — on the create and
/// on the update alike.
#[tokio::test]
async fn subnet_ids_that_name_no_subnet_are_refused_and_write_nothing() {
    let router = emulator();
    let (vpc_id, subnets) = ec2_network(&router, 72, &["us-west-2a", "us-west-2b"], 0).await;
    let held: Vec<&str> = subnets.iter().map(|subnet| subnet.id.as_str()).collect();

    // A group over EC2's own subnets, so the refusals below can be checked against state that
    // an accepted request really left.
    let case = "a group over EC2's own subnets";
    let created = served(
        &router,
        "CreateSubnetGroup",
        &json!({ "SubnetGroupName": "resolved-subnets", "SubnetIds": held }),
        case,
    )
    .await;
    assert_placement(
        &created["SubnetGroup"],
        &subnets.iter().collect::<Vec<&Subnet>>(),
        &vpc_id,
        case,
    );

    // (case, the name the request would create, the SubnetIds it sends, the ids the refusal
    // must name). The rows span the id FORMS that resolve to nothing, where in the list one
    // sits, and how many of them there are.
    let mut rows: Vec<(String, String, Vec<String>, Vec<String>)> = Vec::new();
    for (index, (form, absent)) in absent_subnet_ids().into_iter().enumerate() {
        rows.push((
            format!("one subnet EC2 does not hold, in {form}"),
            format!("phantom-only-{index}"),
            vec![absent.to_string()],
            vec![absent.to_string()],
        ));
        rows.push((
            format!("one subnet EC2 holds and then one it does not, in {form}"),
            format!("phantom-last-{index}"),
            vec![held[0].to_string(), absent.to_string()],
            vec![absent.to_string()],
        ));
        rows.push((
            format!("the FIRST of three, in {form}"),
            format!("phantom-first-{index}"),
            vec![
                absent.to_string(),
                held[0].to_string(),
                held[1].to_string(),
            ],
            vec![absent.to_string()],
        ));
    }
    let both: Vec<String> = absent_subnet_ids()
        .iter()
        .map(|(_, absent)| (*absent).to_string())
        .collect();
    rows.push((
        "every subnet of the request resolving to nothing".to_string(),
        "phantom-all".to_string(),
        both.clone(),
        both,
    ));

    for (case, name, subnet_ids, named) in &rows {
        let (status, refused) = memorydb(
            &router,
            "CreateSubnetGroup",
            &json!({ "SubnetGroupName": name, "SubnetIds": subnet_ids }),
        )
        .await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, INVALID_VALUE),
            "case {case:?}: expected the modelled invalid-value refusal, got {status} with \
             {refused}"
        );
        let expected: Vec<&str> = named.iter().map(String::as_str).collect();
        assert_eq!(
            error_message(&refused, case),
            absent_message(&expected),
            "case {case:?}: expected the sentence naming the identifiers that resolve to \
             nothing, got {refused}"
        );
        // Nothing was written: the group the request named is in no listing, and the read of
        // it is the modelled not-found fault.
        let names = listed_names(&router, case).await;
        assert_eq!(
            names,
            vec!["resolved-subnets".to_string()],
            "case {case:?}: a refused create must leave the listing as it was"
        );
        let (status, answered) = memorydb(
            &router,
            "DescribeSubnetGroups",
            &json!({ "SubnetGroupName": name }),
        )
        .await;
        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, "SubnetGroupNotFoundFault"),
            "case {case:?}: a refused create must store no group, got {status} with {answered}"
        );

        // THE UPDATE follows the same rule, and leaves the group it names exactly as it was.
        let (status, refused) = memorydb(
            &router,
            "UpdateSubnetGroup",
            &json!({ "SubnetGroupName": "resolved-subnets", "SubnetIds": subnet_ids }),
        )
        .await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, INVALID_VALUE),
            "case {case:?}: an update naming a subnet EC2 does not hold must be refused the \
             same way, got {status} with {refused}"
        );
        assert_eq!(
            error_message(&refused, case),
            absent_message(&expected),
            "case {case:?}: expected the same sentence from the update, got {refused}"
        );
        let described = served(
            &router,
            "DescribeSubnetGroups",
            &json!({ "SubnetGroupName": "resolved-subnets" }),
            case,
        )
        .await;
        assert_eq!(
            described["SubnetGroups"][0], created["SubnetGroup"],
            "case {case:?}: a refused update must leave the group's placement alone, got \
             {described}"
        );
    }
}

/// A `MaxResults` outside the window `DescribeSubnetGroups` serves is refused with the
/// modelled invalid-value error naming that window's bounds; a size inside it is served and
/// bounds the page.
#[tokio::test]
async fn an_out_of_window_page_size_is_the_modelled_invalid_value_refusal() {
    let router = emulator();
    let (_, subnets) = ec2_network(&router, 73, &["us-west-2a"], 0).await;
    served(
        &router,
        "CreateSubnetGroup",
        &json!({ "SubnetGroupName": "paged-subnets", "SubnetIds": [subnets[0].id] }),
        "the seed",
    )
    .await;

    // (page size, served?) — each bound of the window, one inside and one outside each bound,
    // and the values no window admits.
    let rows: &[(i64, bool)] = &[
        (PAGE_SIZE_MIN, true),
        (PAGE_SIZE_MIN + 1, true),
        (PAGE_SIZE_MAX - 1, true),
        (PAGE_SIZE_MAX, true),
        (PAGE_SIZE_MIN - 1, false),
        (PAGE_SIZE_MAX + 1, false),
        (1, false),
        (0, false),
        (-1, false),
        (i64::from(i16::MAX), false),
    ];

    for (page_size, serves) in rows.iter().copied() {
        let case = format!("MaxResults={page_size}");
        let (status, answered) = memorydb(
            &router,
            "DescribeSubnetGroups",
            &json!({ "MaxResults": page_size }),
        )
        .await;
        if serves {
            assert_eq!(
                status, 200,
                "case {case:?}: this listing's window serves this size, got {status} with \
                 {answered}"
            );
            let page = answered["SubnetGroups"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            assert!(
                i64::try_from(page.len()).unwrap_or(i64::MAX) <= page_size,
                "case {case:?}: a page holds at most MaxResults items, got {answered}"
            );
        } else {
            assert_eq!(
                (status, error_code(&answered).as_str()),
                (400, INVALID_VALUE),
                "case {case:?}: expected the modelled invalid-value refusal, got {status} \
                 with {answered}"
            );
            assert_eq!(
                error_message(&answered, &case),
                PAGE_SIZE_MESSAGE,
                "case {case:?}: expected this listing's own window bounds, got {answered}"
            );
        }
    }
}
