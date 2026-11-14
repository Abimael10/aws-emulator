// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `ModifyTransitGatewayVpcAttachment` and the
//! `DescribeTransitGatewayVpcAttachments` it reads back through, driven over the wire the
//! service actually speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST whose body
//! carries `Action=ModifyTransitGatewayVpcAttachment`, answered with XML (README.md /
//! OPERATIONS.md "Wire protocol").
//!
//! The rule under test is the one a modification of a stored resource follows: it APPLIES THE
//! MEMBERS THE REQUEST CARRIES to the record it names and leaves every other member exactly as
//! it was — here the subnets the request adds and removes and each of the four attachment
//! options — leaves the attachment in the transient `modifying` state with the clock owing it
//! `available`, and answers the stored record, which is what a later describe reports.
//!
//! Every table row below is a distinct input along what that generalises over: how many subnets
//! the attachment starts with, which options it was created with, how many subnets the request
//! adds and removes (none, one, two), whether it swaps one subnet for another in a single call,
//! whether it names a subnet the attachment already carries, and which of the four option
//! members it carries (none, one, two, all four).
//!
//! Every refusal row carries EC2's own error code and sentence, and changes nothing: an id that
//! is not an attachment id at all, a well-formed id naming no VPC attachment, an attachment of
//! another kind, an attachment that is not in service yet, a value that is no subnet id in
//! either list, a subnet id naming nothing, a subnet of another VPC, a subnet of a zone the
//! attachment is already served in, the removal of a subnet it does not carry, the removal of
//! every subnet it has, a request that asks for no change at all, and a dry run — which asks
//! about permission rather than about the named attachment, so it answers the same whether the
//! attachment is there or not.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ec2::ops::types::{PeeringTgwInfo, TransitGatewayPeeringAttachment};
use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, DUPLICATE_SUBNETS_IN_SAME_ZONE, DRY_RUN_OPERATION,
    INVALID_PARAMETER_VALUE, INVALID_SUBNET_ID_MALFORMED, INVALID_SUBNET_ID_NOT_FOUND,
    INVALID_TRANSIT_GATEWAY_ATTACHMENT_ID_MALFORMED,
    INVALID_TRANSIT_GATEWAY_ATTACHMENT_ID_MESSAGE,
    INVALID_TRANSIT_GATEWAY_ATTACHMENT_ID_NOT_FOUND, MISSING_PARAMETER, OPTION_DISABLE,
    OPTION_ENABLE,
    TRANSIT_GATEWAY_ATTACHMENT_STATE_AVAILABLE, TRANSIT_GATEWAY_ATTACHMENT_STATE_MODIFYING,
    TRANSIT_GATEWAY_ATTACHMENT_STATE_PENDING, TRANSITION_DELAY, TransitGatewayAttachmentView,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, error_message, indexed};
use super::wire::{Param, child, child_text, document, param, set_items};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The HTTP status EC2 answers `DryRunOperation` with.
const DRY_RUN_STATUS: u16 = 412;

/// The ec2Query `Action` under test.
const ACTION: &str = "ModifyTransitGatewayVpcAttachment";

/// A well-formed EC2 id suffix — 17 lowercase hex digits — that nothing in this store mints, so
/// an id built from it is well formed and names no resource.
const UNKNOWN_SUFFIX: &str = "00000000000000000";

/// The Availability Zones of [`REGION`] the fixture puts its subnets in — one subnet per zone,
/// which is what an attachment admits.
const ZONES: [&str; 3] = ["us-east-1a", "us-east-1b", "us-east-1c"];

/// How long a poll below keeps looking for a move the clock owes before giving up: a generous
/// multiple of the service's own transition span, so a loaded machine polls more often rather
/// than failing.
fn poll_deadline() -> Duration {
    TRANSITION_DELAY * 60
}

/// How long a poll waits before looking again — a fraction of the span, inside the loop.
fn poll_interval() -> Duration {
    TRANSITION_DELAY / 10
}

/// One successful call of `action`, as its body.
async fn ok_body(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// The transit gateway `CreateTransitGateway` makes, with no options at all.
async fn create_transit_gateway(router: &MultiServiceRouter) -> String {
    let body = ok_body("the gateway", router, "CreateTransitGateway", &[]).await;
    element(&body, "transitGatewayId")
        .unwrap_or_else(|| panic!("CreateTransitGateway answered no transitGatewayId: {body}"))
}

/// The VPC `CreateVpc` makes for `cidr_block`.
async fn create_vpc(router: &MultiServiceRouter, cidr_block: &str) -> String {
    let body = ok_body(
        "the network",
        router,
        "CreateVpc",
        &[param("CidrBlock", cidr_block)],
    )
    .await;
    element(&body, "vpcId").unwrap_or_else(|| panic!("CreateVpc answered no vpcId: {body}"))
}

/// The subnet `CreateSubnet` makes in `vpc_id`, in `zone`.
async fn create_subnet(
    router: &MultiServiceRouter,
    vpc_id: &str,
    cidr_block: &str,
    zone: &str,
) -> String {
    let body = ok_body(
        "the network",
        router,
        "CreateSubnet",
        &[
            param("VpcId", vpc_id),
            param("CidrBlock", cidr_block),
            param("AvailabilityZone", zone),
        ],
    )
    .await;
    element(&body, "subnetId").unwrap_or_else(|| panic!("CreateSubnet answered no subnetId: {body}"))
}

/// The four option members of a request, each as the value it asks for — `None` leaves the
/// member out, which is what leaves the stored value as it was.
#[derive(Debug, Clone, Copy, Default)]
struct RequestedOptions {
    dns_support: Option<&'static str>,
    security_group_referencing_support: Option<&'static str>,
    ipv6_support: Option<&'static str>,
    appliance_mode_support: Option<&'static str>,
}

impl RequestedOptions {
    /// The form fields these options are sent as — the member names
    /// `ModifyTransitGatewayVpcAttachmentRequestOptions` and
    /// `CreateTransitGatewayVpcAttachmentRequestOptions` share
    /// (`smithy_query(query="shape",shape="ModifyTransitGatewayVpcAttachmentRequestOptions")`).
    fn params(self) -> Vec<Param> {
        let mut params = Vec::new();
        for (name, value) in [
            ("Options.DnsSupport", self.dns_support),
            (
                "Options.SecurityGroupReferencingSupport",
                self.security_group_referencing_support,
            ),
            ("Options.Ipv6Support", self.ipv6_support),
            ("Options.ApplianceModeSupport", self.appliance_mode_support),
        ] {
            if let Some(value) = value {
                params.push(param(name, value));
            }
        }
        params
    }
}

/// One reported `TransitGatewayVpcAttachment`, read out of the XML by the document rather than
/// by string surgery — `state` appears on the attachment and the option members inside its
/// `options`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reported {
    attachment_id: String,
    transit_gateway_id: String,
    vpc_id: String,
    vpc_owner_id: String,
    state: String,
    subnet_ids: Vec<String>,
    creation_time: String,
    options: Vec<(String, String)>,
    tags: Vec<(String, String)>,
}

impl Reported {
    /// This report with its `state` blanked, so two reports can be compared on every member the
    /// clock does not move.
    fn without_state(&self) -> Self {
        Self {
            state: String::new(),
            ..self.clone()
        }
    }

    /// The value of the option member `name`, or a panic naming the case.
    fn option(&self, case: &str, name: &str) -> String {
        self.options
            .iter()
            .find(|(member, _)| member == name)
            .map(|(_, value)| value.clone())
            .unwrap_or_else(|| {
                panic!("case {case:?}: the report carries no {name:?} option: {self:?}")
            })
    }
}

/// The text of every `item` of the `name` set of `node`, in document order.
fn item_texts(node: roxmltree::Node<'_, '_>, name: &str) -> Vec<String> {
    set_items(node, name)
        .into_iter()
        .map(|entry| entry.text().unwrap_or_default().to_string())
        .collect()
}

/// The one VPC attachment a response carries, or a panic naming the case: the element that has
/// both a `transitGatewayAttachmentId` and a `vpcOwnerId` child, whether the response is this
/// modify's single `transitGatewayVpcAttachment` or a describe's set.
fn reported(case: &str, body: &str) -> Reported {
    let parsed = document(case, body);
    let mut all: Vec<Reported> = parsed
        .descendants()
        .filter(|node| {
            node.is_element()
                && child(*node, "transitGatewayAttachmentId").is_some()
                && child(*node, "vpcOwnerId").is_some()
        })
        .map(|item| Reported {
            attachment_id: child_text(item, "transitGatewayAttachmentId").unwrap_or_default(),
            transit_gateway_id: child_text(item, "transitGatewayId").unwrap_or_default(),
            vpc_id: child_text(item, "vpcId").unwrap_or_default(),
            vpc_owner_id: child_text(item, "vpcOwnerId").unwrap_or_default(),
            state: child_text(item, "state").unwrap_or_default(),
            subnet_ids: item_texts(item, "subnetIds"),
            creation_time: child_text(item, "creationTime").unwrap_or_default(),
            options: child(item, "options")
                .map(|options| {
                    options
                        .children()
                        .filter(roxmltree::Node::is_element)
                        .map(|member| {
                            (
                                member.tag_name().name().to_string(),
                                member.text().unwrap_or_default().to_string(),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
            tags: set_items(item, "tagSet")
                .into_iter()
                .map(|tag| {
                    (
                        child_text(tag, "key").unwrap_or_default(),
                        child_text(tag, "value").unwrap_or_default(),
                    )
                })
                .collect(),
        })
        .collect();
    assert_eq!(
        all.len(),
        1,
        "case {case:?}: expected exactly one VPC attachment, got {all:?} in {body}"
    );
    all.remove(0)
}

/// The one VPC attachment `DescribeTransitGatewayVpcAttachments` reports for `attachment_id`.
async fn described(case: &str, router: &MultiServiceRouter, attachment_id: &str) -> Reported {
    let body = ok_body(
        case,
        router,
        "DescribeTransitGatewayVpcAttachments",
        &indexed("TransitGatewayAttachmentIds", &[attachment_id]),
    )
    .await;
    reported(case, &body)
}

/// The attachment once the clock has carried it to `wanted` — a wait on the CONDITION rather
/// than on a span, so a loaded machine only looks more often.
async fn poll_until_state(
    case: &str,
    router: &MultiServiceRouter,
    attachment_id: &str,
    wanted: &str,
) -> Reported {
    let deadline = Instant::now() + poll_deadline();
    loop {
        let report = described(case, router, attachment_id).await;
        if report.state == wanted {
            return report;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {attachment_id} to reach {wanted:?}, it is still \
             {:?} past the deadline",
            report.state
        );
        tokio::time::sleep(poll_interval()).await;
    }
}

/// One `ModifyTransitGatewayVpcAttachment` call carrying `params`.
async fn modify(router: &MultiServiceRouter, params: &[Param]) -> (u16, String) {
    call(router, REGION, ACTION, params).await
}

/// The emulator, one VPC with one subnet per zone of [`ZONES`] plus a SECOND subnet in the first
/// zone, and another VPC with a subnet of its own — so a row can add a subnet of a zone the
/// attachment is already served in, and one of a VPC the attachment does not attach.
struct Fixture {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
    /// The VPC every attachment here attaches.
    vpc_id: String,
    /// Its subnets, one per zone of [`ZONES`], in that order.
    subnets: Vec<String>,
    /// A second subnet of that VPC in `ZONES[0]`, whose zone the first subnet already takes.
    crowded_subnet: String,
    /// A subnet of ANOTHER VPC, which no attachment of `vpc_id` can reach.
    foreign_subnet: String,
}

impl Fixture {
    async fn new() -> Self {
        let (router, engine) = super::ec2_query::emulator();
        let vpc_id = create_vpc(&router, "10.80.0.0/16").await;
        let mut subnets = Vec::new();
        for (index, zone) in ZONES.into_iter().enumerate() {
            subnets.push(create_subnet(&router, &vpc_id, &format!("10.80.{index}.0/24"), zone).await);
        }
        let crowded_subnet = create_subnet(&router, &vpc_id, "10.80.9.0/24", ZONES[0]).await;
        let other_vpc = create_vpc(&router, "10.81.0.0/16").await;
        let foreign_subnet = create_subnet(&router, &other_vpc, "10.81.0.0/24", ZONES[0]).await;
        Self {
            router,
            engine,
            vpc_id,
            subnets,
            crowded_subnet,
            foreign_subnet,
        }
    }

    /// The subnet of the fixture's VPC in the zone at `index`.
    fn subnet(&self, index: usize) -> &str {
        &self.subnets[index]
    }

    /// A VPC attachment of the fixture's VPC to a transit gateway of its own, created over the
    /// wire with `subnets` and `options`, and NOT yet waited on — so a caller can either see it
    /// `pending` or wait for it.
    async fn attach(&self, zones: &[usize], options: RequestedOptions) -> String {
        let transit_gateway_id = create_transit_gateway(&self.router).await;
        let named: Vec<&str> = zones.iter().map(|index| self.subnet(*index)).collect();
        let mut params = vec![
            param("TransitGatewayId", &transit_gateway_id),
            param("VpcId", &self.vpc_id),
        ];
        params.extend(indexed("SubnetIds", &named));
        params.extend(options.params());
        let body = ok_body(
            "the attachment",
            &self.router,
            "CreateTransitGatewayVpcAttachment",
            &params,
        )
        .await;
        reported("the attachment", &body).attachment_id
    }

    /// A VPC attachment in service: created, then waited on until the clock has carried it to
    /// `available`, which is the state a modification is admitted in.
    async fn available_attachment(&self, zones: &[usize], options: RequestedOptions) -> String {
        let attachment_id = self.attach(zones, options).await;
        poll_until_state(
            "the attachment",
            &self.router,
            &attachment_id,
            TRANSIT_GATEWAY_ATTACHMENT_STATE_AVAILABLE,
        )
        .await;
        attachment_id
    }

    /// Seed a transit gateway PEERING attachment straight through the store's own writer, for
    /// the row that names an attachment of another kind.
    fn seed_peering_attachment(&self) -> String {
        self.engine
            .state::<Ec2State>()
            .lock()
            .insert_transit_gateway_attachment(TransitGatewayAttachmentView::Peering(
                TransitGatewayPeeringAttachment {
                    requester_tgw_info: Some(PeeringTgwInfo {
                        transit_gateway_id: Some(format!("tgw-{UNKNOWN_SUFFIX}")),
                        ..Default::default()
                    }),
                    state: Some(TRANSIT_GATEWAY_ATTACHMENT_STATE_AVAILABLE.to_string()),
                    ..Default::default()
                },
            ))
            .expect("the attachment store has room for one seeded attachment")
    }
}

/// One modification to drive: the subnets the attachment starts with and the options it was
/// created with, then the zone indexes the request adds, the ones it removes, and the options it
/// asks for.
struct Case {
    label: &'static str,
    start: &'static [usize],
    created: RequestedOptions,
    add: &'static [usize],
    remove: &'static [usize],
    options: RequestedOptions,
}

/// A modification applies the members the request carries to the stored attachment and leaves
/// every other member as it was: its subnets after the removals and then the additions, each
/// option the request names, and nothing else — all of it readable back through
/// `DescribeTransitGatewayVpcAttachments`.
#[tokio::test]
async fn a_modification_applies_the_members_the_request_carries() {
    let cases = [
        Case {
            label: "one option on a single-subnet attachment, no subnet change",
            start: &[0],
            created: RequestedOptions::default(),
            add: &[],
            remove: &[],
            options: RequestedOptions {
                dns_support: Some(OPTION_DISABLE),
                ..RequestedOptions::default()
            },
        },
        Case {
            label: "one subnet added, no options",
            start: &[0],
            created: RequestedOptions::default(),
            add: &[1],
            remove: &[],
            options: RequestedOptions::default(),
        },
        Case {
            label: "two subnets added at once, two options",
            start: &[0],
            created: RequestedOptions::default(),
            add: &[1, 2],
            remove: &[],
            options: RequestedOptions {
                ipv6_support: Some(OPTION_ENABLE),
                appliance_mode_support: Some(OPTION_ENABLE),
                ..RequestedOptions::default()
            },
        },
        Case {
            label: "one subnet removed from three",
            start: &[0, 1, 2],
            created: RequestedOptions::default(),
            add: &[],
            remove: &[1],
            options: RequestedOptions::default(),
        },
        Case {
            label: "two subnets removed from three",
            start: &[0, 1, 2],
            created: RequestedOptions::default(),
            add: &[],
            remove: &[0, 2],
            options: RequestedOptions::default(),
        },
        Case {
            label: "one subnet swapped for another in one call",
            start: &[0, 1],
            created: RequestedOptions::default(),
            add: &[2],
            remove: &[1],
            options: RequestedOptions::default(),
        },
        Case {
            label: "a subnet the attachment already carries, named again",
            start: &[0, 1],
            created: RequestedOptions::default(),
            add: &[1],
            remove: &[],
            options: RequestedOptions::default(),
        },
        Case {
            label: "every option asked for, on an attachment created with them reversed",
            start: &[0],
            created: RequestedOptions {
                dns_support: Some(OPTION_DISABLE),
                security_group_referencing_support: Some(OPTION_DISABLE),
                ipv6_support: Some(OPTION_ENABLE),
                appliance_mode_support: Some(OPTION_ENABLE),
            },
            add: &[2],
            remove: &[],
            options: RequestedOptions {
                dns_support: Some(OPTION_ENABLE),
                security_group_referencing_support: Some(OPTION_ENABLE),
                ipv6_support: Some(OPTION_DISABLE),
                appliance_mode_support: Some(OPTION_DISABLE),
            },
        },
        Case {
            label: "one option that is already the attachment's value, nothing else",
            start: &[0, 2],
            created: RequestedOptions {
                ipv6_support: Some(OPTION_ENABLE),
                ..RequestedOptions::default()
            },
            add: &[],
            remove: &[],
            options: RequestedOptions {
                ipv6_support: Some(OPTION_ENABLE),
                ..RequestedOptions::default()
            },
        },
    ];

    let fixture = Fixture::new().await;
    for case in &cases {
        let label = case.label;
        let attachment_id = fixture.available_attachment(case.start, case.created).await;
        let before = described(label, &fixture.router, &attachment_id).await;

        let added: Vec<&str> = case.add.iter().map(|index| fixture.subnet(*index)).collect();
        let removed: Vec<&str> = case
            .remove
            .iter()
            .map(|index| fixture.subnet(*index))
            .collect();
        let mut params = vec![param("TransitGatewayAttachmentId", &attachment_id)];
        params.extend(indexed("AddSubnetIds", &added));
        params.extend(indexed("RemoveSubnetIds", &removed));
        params.extend(case.options.params());
        let (status, body) = modify(&fixture.router, &params).await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected HTTP 200, got {status} with body {body}"
        );
        let got = reported(label, &body);

        // The subnets the attachment held that the request does not remove, in the order it held
        // them, then the ones it adds that are not already there, in the order named.
        let mut expected_subnets: Vec<String> = before
            .subnet_ids
            .iter()
            .filter(|subnet_id| !removed.iter().any(|gone| gone == subnet_id))
            .cloned()
            .collect();
        for subnet_id in &added {
            if !expected_subnets.iter().any(|held| held == *subnet_id) {
                expected_subnets.push((*subnet_id).to_string());
            }
        }
        assert_eq!(
            got.subnet_ids, expected_subnets,
            "case {label:?}: expected the subnets {expected_subnets:?}, got {:?}",
            got.subnet_ids
        );

        // Each option member: the value the request asked for, else the value the attachment
        // already carried.
        for (member, requested) in [
            ("dnsSupport", case.options.dns_support),
            (
                "securityGroupReferencingSupport",
                case.options.security_group_referencing_support,
            ),
            ("ipv6Support", case.options.ipv6_support),
            ("applianceModeSupport", case.options.appliance_mode_support),
        ] {
            let expected = requested
                .map(str::to_string)
                .unwrap_or_else(|| before.option(label, member));
            let actual = got.option(label, member);
            assert_eq!(
                actual, expected,
                "case {label:?}: expected {member} {expected:?}, got {actual:?}"
            );
        }

        assert_eq!(
            got.state, TRANSIT_GATEWAY_ATTACHMENT_STATE_MODIFYING,
            "case {label:?}: expected the transient \
             {TRANSIT_GATEWAY_ATTACHMENT_STATE_MODIFYING:?}, got {:?}",
            got.state
        );
        for (member, expected, actual) in [
            ("transitGatewayAttachmentId", &before.attachment_id, &got.attachment_id),
            ("transitGatewayId", &before.transit_gateway_id, &got.transit_gateway_id),
            ("vpcId", &before.vpc_id, &got.vpc_id),
            ("vpcOwnerId", &before.vpc_owner_id, &got.vpc_owner_id),
            ("creationTime", &before.creation_time, &got.creation_time),
        ] {
            assert_eq!(
                actual, expected,
                "case {label:?}: the modification must leave {member} as it was, expected \
                 {expected:?}, got {actual:?}"
            );
        }
        assert_eq!(
            got.tags, before.tags,
            "case {label:?}: the modification must leave the tags as they were, expected {:?}, \
             got {:?}",
            before.tags, got.tags
        );

        // What the modify answered is the record the store holds: the describe reports every
        // member identically, its state being the only one the clock moves on from here.
        let read_back = described(label, &fixture.router, &attachment_id).await;
        assert_eq!(
            read_back.without_state(),
            got.without_state(),
            "case {label:?}: expected the describe to report the modified attachment, got \
             {read_back:?}"
        );
        assert!(
            read_back.state == TRANSIT_GATEWAY_ATTACHMENT_STATE_MODIFYING
                || read_back.state == TRANSIT_GATEWAY_ATTACHMENT_STATE_AVAILABLE,
            "case {label:?}: expected the describe to report the modification in progress or \
             done, got {:?}",
            read_back.state
        );
    }
}

/// The clock owes the modified attachment `available`: it is `modifying` in the answer to the
/// call and in service once the span has passed, with nothing slept for in between.
#[tokio::test]
async fn the_clock_owes_the_modified_attachment_available() {
    let case = "modifying -> available";
    let fixture = Fixture::new().await;
    let attachment_id = fixture
        .available_attachment(&[0], RequestedOptions::default())
        .await;

    let (status, body) = modify(
        &fixture.router,
        &[
            param("TransitGatewayAttachmentId", &attachment_id),
            param("AddSubnetIds.1", fixture.subnet(1)),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with body {body}"
    );
    let straight_after = reported(case, &body).state;
    assert_eq!(
        straight_after, TRANSIT_GATEWAY_ATTACHMENT_STATE_MODIFYING,
        "case {case:?}: expected {TRANSIT_GATEWAY_ATTACHMENT_STATE_MODIFYING:?} in the answer to \
         the call, got {straight_after:?}"
    );

    let settled = poll_until_state(
        case,
        &fixture.router,
        &attachment_id,
        TRANSIT_GATEWAY_ATTACHMENT_STATE_AVAILABLE,
    )
    .await;
    assert_eq!(
        settled.subnet_ids,
        vec![
            fixture.subnet(0).to_string(),
            fixture.subnet(1).to_string()
        ],
        "case {case:?}: expected the added subnet to outlive the move to \
         {TRANSIT_GATEWAY_ATTACHMENT_STATE_AVAILABLE:?}, got {:?}",
        settled.subnet_ids
    );
}

/// One refusal to drive: the parameters, and the code and sentence EC2 answers them with.
struct Refusal {
    label: String,
    params: Vec<Param>,
    status: u16,
    code: String,
    message: String,
}

/// Every refusal of this modification carries EC2's own code and sentence, and leaves the
/// attachment exactly as it was.
#[tokio::test]
async fn the_modification_refuses_what_it_cannot_apply() {
    let fixture = Fixture::new().await;
    // The attachment every row that needs a live one points at: in service, carrying the subnets
    // of the first two zones.
    let attachment_id = fixture
        .available_attachment(&[0, 1], RequestedOptions::default())
        .await;
    let attachment = param("TransitGatewayAttachmentId", &attachment_id);
    let before = described("the live attachment", &fixture.router, &attachment_id).await;
    let mut refusals: Vec<Refusal> = Vec::new();

    // Not a transit gateway attachment id at all: the wrong resource's prefix, no prefix, a digit
    // count outside the two the id form allows, upper-case hex, and an empty value.
    for malformed in [
        "vpc-0123456789abcdef0",
        "not-an-attachment-id",
        "tgw-attach-0123456789ab",
        "tgw-attach-0123456789ABCDEF0",
        "tgw-attach-",
    ] {
        refusals.push(Refusal {
            label: format!("malformed attachment id {malformed:?}"),
            params: vec![param("TransitGatewayAttachmentId", malformed)],
            status: 400,
            code: INVALID_TRANSIT_GATEWAY_ATTACHMENT_ID_MALFORMED.to_string(),
            message: INVALID_TRANSIT_GATEWAY_ATTACHMENT_ID_MESSAGE.to_string(),
        });
    }

    // Well formed, and no VPC attachment of this account holds it — including the id of an
    // attachment of ANOTHER KIND, which names no VPC attachment this operation can modify.
    let unknown_attachment = format!("tgw-attach-{UNKNOWN_SUFFIX}");
    let peering = fixture.seed_peering_attachment();
    for (label, named) in [
        ("an unknown attachment id", unknown_attachment.clone()),
        ("a peering attachment's id", peering.clone()),
    ] {
        refusals.push(Refusal {
            label: label.to_string(),
            params: vec![
                param("TransitGatewayAttachmentId", &named),
                param("Options.DnsSupport", OPTION_DISABLE),
            ],
            status: 400,
            code: INVALID_TRANSIT_GATEWAY_ATTACHMENT_ID_NOT_FOUND.to_string(),
            message: format!(
                "Transit Gateway VPC Attachment {named} was deleted or does not exist."
            ),
        });
    }

    // Not a subnet id at all, in either of the two lists.
    for member in ["AddSubnetIds", "RemoveSubnetIds"] {
        for malformed in ["vpc-0123456789abcdef0", "subnet-0123456789ab", "subnet-"] {
            refusals.push(Refusal {
                label: format!("malformed subnet id {malformed:?} in {member}"),
                params: vec![
                    attachment.clone(),
                    param(&format!("{member}.1"), malformed),
                ],
                status: 400,
                code: INVALID_SUBNET_ID_MALFORMED.to_string(),
                message: format!("The subnet ID '{malformed}' is malformed"),
            });
        }
    }

    // Well formed, but no subnet of the account holds it.
    let unknown_subnet = format!("subnet-{UNKNOWN_SUFFIX}");
    refusals.push(Refusal {
        label: "an unknown subnet added".to_string(),
        params: vec![attachment.clone(), param("AddSubnetIds.1", &unknown_subnet)],
        status: 400,
        code: INVALID_SUBNET_ID_NOT_FOUND.to_string(),
        message: format!("Subnet {unknown_subnet} was deleted or does not exist."),
    });

    // A subnet that exists, of a VPC this attachment does not attach.
    refusals.push(Refusal {
        label: "a subnet of another VPC added".to_string(),
        params: vec![
            attachment.clone(),
            param("AddSubnetIds.1", &fixture.foreign_subnet),
        ],
        status: 400,
        code: INVALID_SUBNET_ID_NOT_FOUND.to_string(),
        message: format!(
            "Subnet {subnet_id} was deleted or does not exist.",
            subnet_id = fixture.foreign_subnet
        ),
    });

    // A second subnet of a zone the attachment is already served in.
    refusals.push(Refusal {
        label: "a subnet of a zone already served".to_string(),
        params: vec![
            attachment.clone(),
            param("AddSubnetIds.1", &fixture.crowded_subnet),
        ],
        status: 400,
        code: DUPLICATE_SUBNETS_IN_SAME_ZONE.to_string(),
        message: format!(
            "The subnets {held} and {requested} are both in the Availability Zone {zone}; you \
             can specify only one subnet per Availability Zone",
            held = fixture.subnet(0),
            requested = fixture.crowded_subnet,
            zone = ZONES[0],
        ),
    });

    // A subnet the attachment does not carry has no address in it to give back.
    let unattached = fixture.subnet(2).to_string();
    refusals.push(Refusal {
        label: "a subnet the attachment does not carry, removed".to_string(),
        params: vec![attachment.clone(), param("RemoveSubnetIds.1", &unattached)],
        status: 400,
        code: INVALID_PARAMETER_VALUE.to_string(),
        message: format!(
            "Value ({unattached}) for parameter RemoveSubnetIds is invalid. Subnet {unattached} \
             is not attached to transit gateway attachment {attachment_id}."
        ),
    });

    // Every subnet the attachment has, removed: an attachment reaches its VPC through an address
    // in a subnet of it, so it cannot be left with none.
    let all_subnets: Vec<&str> = before.subnet_ids.iter().map(String::as_str).collect();
    let mut remove_everything = vec![attachment.clone()];
    remove_everything.extend(indexed("RemoveSubnetIds", &all_subnets));
    refusals.push(Refusal {
        label: "every subnet removed".to_string(),
        params: remove_everything,
        status: 400,
        code: INVALID_PARAMETER_VALUE.to_string(),
        message: format!(
            "Value ({removed}) for parameter RemoveSubnetIds is invalid. You must specify at \
             least one subnet.",
            removed = all_subnets.join(", "),
        ),
    });

    // A request that carries no modification at all: every member but the id and the dry run IS
    // the modification, so one that names none is told which parameters carry a change rather
    // than answered with an untouched attachment.
    refusals.push(Refusal {
        label: "a request that asks for nothing".to_string(),
        params: vec![attachment.clone()],
        status: 400,
        code: MISSING_PARAMETER.to_string(),
        message: "The request must contain at least one of the parameters addSubnetIds, \
                  removeSubnetIds and options"
            .to_string(),
    });

    // A request that WOULD have worked, asked as a dry run: the permission answer, and nothing
    // written. The flag asks about permission rather than about the named resource, so an
    // attachment this account does not hold earns the same answer.
    for (label, named) in [
        ("a dry run of a request that would have worked", &attachment_id),
        ("a dry run naming an attachment nobody holds", &unknown_attachment),
    ] {
        refusals.push(Refusal {
            label: label.to_string(),
            params: vec![
                param("TransitGatewayAttachmentId", named),
                param("AddSubnetIds.1", fixture.subnet(2)),
                param("DryRun", "true"),
            ],
            status: DRY_RUN_STATUS,
            code: DRY_RUN_OPERATION.to_string(),
            message: "Request would have succeeded, but DryRun flag is set.".to_string(),
        });
    }

    for refusal in &refusals {
        let label = &refusal.label;
        let (status, body) = modify(&fixture.router, &refusal.params).await;
        assert_eq!(
            status, refusal.status,
            "case {label:?}: expected HTTP {}, got {status} with body {body}",
            refusal.status
        );
        let code = error_code(&body);
        assert_eq!(
            code, refusal.code,
            "case {label:?}: expected the code {:?}, got {code:?} in {body}",
            refusal.code
        );
        let message = error_message(&body);
        assert_eq!(
            message, refusal.message,
            "case {label:?}: expected the sentence {:?}, got {message:?}",
            refusal.message
        );
        let after = described(label, &fixture.router, &attachment_id).await;
        assert_eq!(
            after, before,
            "case {label:?}: the refusal must leave the attachment as it was, expected \
             {before:?}, got {after:?}"
        );
    }
}

/// An attachment that is not in service yet cannot be modified: the create leaves it in the
/// non-terminal `pending` state, and a modification of it is refused for the state it is in,
/// with nothing written.
#[tokio::test]
async fn an_attachment_that_is_not_in_service_is_refused() {
    let case = "a pending attachment";
    let fixture = Fixture::new().await;
    let attachment_id = fixture.attach(&[0], RequestedOptions::default()).await;
    let before = described(case, &fixture.router, &attachment_id).await;
    assert_eq!(
        before.state, TRANSIT_GATEWAY_ATTACHMENT_STATE_PENDING,
        "case {case:?}: expected the create to leave it \
         {TRANSIT_GATEWAY_ATTACHMENT_STATE_PENDING:?}, got {:?}",
        before.state
    );

    let (status, body) = modify(
        &fixture.router,
        &[
            param("TransitGatewayAttachmentId", &attachment_id),
            param("Options.DnsSupport", OPTION_DISABLE),
        ],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with body {body}"
    );
    let code = error_code(&body);
    assert_eq!(
        code,
        ec2::state::INCORRECT_STATE,
        "case {case:?}: expected the code {:?}, got {code:?} in {body}",
        ec2::state::INCORRECT_STATE
    );
    let message = error_message(&body);
    assert_eq!(
        message,
        format!("{attachment_id} is in invalid state"),
        "case {case:?}: expected the attachment family's sentence, got {message:?}"
    );

    // Nothing was written: the attachment reaches `available` carrying the options it was
    // created with, the refused change nowhere in it.
    let settled = poll_until_state(
        case,
        &fixture.router,
        &attachment_id,
        TRANSIT_GATEWAY_ATTACHMENT_STATE_AVAILABLE,
    )
    .await;
    assert_eq!(
        settled.option(case, "dnsSupport"),
        OPTION_ENABLE,
        "case {case:?}: the refused modification must not have been applied, got {settled:?}"
    );
}
