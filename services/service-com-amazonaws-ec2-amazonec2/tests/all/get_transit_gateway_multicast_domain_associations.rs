// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `GetTransitGatewayMulticastDomainAssociations`, driven over the wire
//! the service actually speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST whose
//! body carries `Action=GetTransitGatewayMulticastDomainAssociations`, answered with XML
//! (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: this report REPORTS, out of the store, exactly the subnet associations
//! the named multicast domain holds — one entry per (attachment, subnet), each member read off
//! the stored association — narrowed to the entries every filter the request names holds for,
//! and cut into pages of the size the request asked for in one stable order, with a token only
//! while a further entry follows; a domain the account does not hold is refused, while a
//! domain that simply holds no association reports an empty set.
//!
//! Each table row is a distinct input along what that generalises over: which of the five
//! documented filters the request names, with how many values, matched exactly or with EC2's
//! wildcards, two filters at once, how many associations the domain holds, how many
//! attachments they are spread over, which page size is asked for (each published bound and
//! one past it), and which page of the walk is being read. Every association is MADE over the
//! wire by `AssociateTransitGatewayMulticastDomain` and read back here, so a report of
//! anything the store does not hold fails the suite.

use std::collections::BTreeSet;
use std::time::Duration;

use ec2::ops::types::{
    TRANSIT_GATEWAY_ATTACHMENT_RESOURCE_TYPE_VALUES,
    TRANSIT_GATEWAY_MULITCAST_DOMAIN_ASSOCIATION_STATE_VALUES, TransitGatewayVpcAttachment,
};
use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, DRY_RUN_OPERATION, INVALID_MAX_RESULTS,
    INVALID_PAGINATION_TOKEN, INVALID_PARAMETER_VALUE,
    INVALID_TRANSIT_GATEWAY_MULTICAST_DOMAIN_ID_MALFORMED,
    INVALID_TRANSIT_GATEWAY_MULTICAST_DOMAIN_ID_NOT_FOUND,
    MULTICAST_ASSOCIATION_STATE_ASSOCIATED, TRANSIT_GATEWAY_ATTACHMENT_RESOURCE_TYPE_VPC,
    TRANSIT_GATEWAY_ATTACHMENT_STATE_AVAILABLE, TRANSIT_GATEWAY_MULTICAST_DOMAIN_ID_PREFIX,
    TransitGatewayAttachmentView, TransitGatewayMulticastDomainRecord,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, error_message, filters, indexed};
use super::wire::{Param, child, child_text, document, param, set_items};

/// The ec2Query `Action` under test.
const ACTION: &str = "GetTransitGatewayMulticastDomainAssociations";

/// The `Action` that MAKES the associations this one reports.
const ASSOCIATE: &str = "AssociateTransitGatewayMulticastDomain";

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The HTTP status EC2 answers `DryRunOperation` with: the request was refused because a
/// precondition the CALLER set holds, not because it was malformed.
const DRY_RUN_STATUS: u16 = 412;

/// The HTTP status EC2 answers every other refusal here with.
const REFUSAL_STATUS: u16 = 400;

/// A well-formed EC2 id suffix — 17 lowercase hex digits — that nothing in this store mints,
/// so an id built from it is well formed and names no resource.
const UNKNOWN_SUFFIX: &str = "00000000000000000";

/// The smallest page the model lets a request ask for: the minimum of the `@range` of
/// `TransitGatewayMaxResults`, the shape this operation's `MaxResults` targets
/// (`smithy_query(query="operation",operation="GetTransitGatewayMulticastDomainAssociations")`,
/// `range 5..1000`).
const PUBLISHED_MIN_MAX_RESULTS: u32 = 5;

/// The largest page that same published range allows.
const PUBLISHED_MAX_MAX_RESULTS: u32 = 1000;

/// How many subnets the fixture associates: one more than the smallest page the model
/// publishes, so a page of that size leaves exactly one association behind it.
const ASSOCIATED_SUBNETS: usize = PUBLISHED_MIN_MAX_RESULTS as usize + 1;

/// The deadline the poll below gives up at: generous, because what is under test is the state
/// an association reaches rather than how fast it reaches it.
fn move_deadline() -> Duration {
    Duration::from_secs(60)
}

/// How long the poll below waits between two reads.
fn poll_interval() -> Duration {
    Duration::from_millis(25)
}

/// A member of a modelled enum's catalog, or a panic: nothing here writes an enum member out
/// by hand, so a model that renames one fails the suite rather than passing against a stale
/// spelling.
fn catalog_word(values: &[&'static str], shape: &str, name: &str) -> &'static str {
    values
        .iter()
        .find(|value| **value == name)
        .unwrap_or_else(|| panic!("{name:?} is no {shape} member"))
}

/// One call of `action` signed for the Region above.
async fn invoke(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// One successful call of `action`, as its body.
async fn ok_body(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = invoke(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// A well-formed id of the kind `prefix` names that no resource of this store has.
fn unknown_id(prefix: &str) -> String {
    format!("{prefix}-{UNKNOWN_SUFFIX}")
}

/// One association as this report answers it, read out of the response tree so a member of an
/// association is never confused with one of the same name elsewhere in the body.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Reported {
    /// `TransitGatewayMulticastDomainAssociation$Subnet` / `SubnetAssociation$SubnetId`.
    subnet_id: Option<String>,
    /// `SubnetAssociation$State`.
    state: Option<String>,
    /// `$TransitGatewayAttachmentId`.
    attachment_id: Option<String>,
    /// `$ResourceId`.
    resource_id: Option<String>,
    /// `$ResourceType`.
    resource_type: Option<String>,
    /// `$ResourceOwnerId`.
    resource_owner_id: Option<String>,
}

/// The associations and the `NextToken` one call of [`ACTION`] answered.
struct Page {
    /// The entries of `multicastDomainAssociations`, in the order the body reported them.
    associations: Vec<Reported>,
    /// The envelope's `nextToken`, absent when the report carried none.
    next_token: Option<String>,
}

impl Page {
    /// The subnet ids this page reported, in order.
    fn subnet_ids(&self) -> Vec<String> {
        self.associations
            .iter()
            .map(|association| association.subnet_id.clone().unwrap_or_default())
            .collect()
    }
}

/// One successful call of [`ACTION`], as the page it answered.
async fn reported_page(case: &str, router: &MultiServiceRouter, params: &[Param]) -> Page {
    let body = ok_body(case, router, ACTION, params).await;
    let parsed = document(case, &body);
    let root = parsed.root_element();
    let associations = set_items(root, "multicastDomainAssociations")
        .into_iter()
        .map(|item| {
            let subnet = child(item, "subnet");
            Reported {
                subnet_id: subnet.and_then(|subnet| child_text(subnet, "subnetId")),
                state: subnet.and_then(|subnet| child_text(subnet, "state")),
                attachment_id: child_text(item, "transitGatewayAttachmentId"),
                resource_id: child_text(item, "resourceId"),
                resource_type: child_text(item, "resourceType"),
                resource_owner_id: child_text(item, "resourceOwnerId"),
            }
        })
        .collect();
    Page {
        associations,
        next_token: child_text(root, "nextToken"),
    }
}

/// The request members of one report of `domain_id`, with `extra` beside them.
fn report_params(domain_id: &str, extra: Vec<Param>) -> Vec<Param> {
    let mut params = vec![param("TransitGatewayMulticastDomainId", domain_id)];
    params.extend(extra);
    params
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

/// The subnet `CreateSubnet` makes in `vpc_id`.
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

/// An `available` VPC attachment of `vpc_id`, as it is stored.
fn vpc_attachment(vpc_id: &str) -> TransitGatewayAttachmentView {
    TransitGatewayAttachmentView::Vpc(TransitGatewayVpcAttachment {
        transit_gateway_id: Some("tgw-0123456789abcdef0".to_string()),
        vpc_id: Some(vpc_id.to_string()),
        // The VPC is one this account created over the wire, so the attachment names this
        // account as the owner of what it attaches.
        vpc_owner_id: Some(engine::emulator_account_id().to_string()),
        state: Some(TRANSIT_GATEWAY_ATTACHMENT_STATE_AVAILABLE.to_string()),
        ..Default::default()
    })
}

/// A stored multicast domain holding no association of its own.
fn empty_domain() -> TransitGatewayMulticastDomainRecord {
    TransitGatewayMulticastDomainRecord {
        // The store mints the id; what a writer puts here is replaced by it.
        transit_gateway_multicast_domain_id: String::new(),
        transit_gateway_id: "tgw-0123456789abcdef0".to_string(),
        // Seeded rather than created, so the facts a create records carry no value here.
        transit_gateway_multicast_domain_arn: None,
        owner_id: engine::emulator_account_id().to_string(),
        options: None,
        // The `TransitGatewayMulticastDomainState` enum's `available` member.
        state: "available".to_string(),
        creation_time: None,
        tags: None,
        associations: Vec::new(),
        pending: None,
    }
}

/// The network, the domains and the associations every row is read against.
struct Fixture {
    /// The domain the associations below were made in.
    domain_id: String,
    /// A second domain of the same account, holding no association at all.
    other_domain_id: String,
    /// The VPC the associated subnets belong to.
    vpc_id: String,
    /// A second VPC of the account, which no association names.
    other_vpc_id: String,
    /// The attachment the first four subnets are associated through.
    attachment_id: String,
    /// The attachment the remaining subnets are associated through.
    second_attachment_id: String,
    /// Every associated subnet, in the order the associations were made.
    subnets: Vec<String>,
    /// How many of [`Self::subnets`] were associated through [`Self::attachment_id`].
    through_first_attachment: usize,
}

impl Fixture {
    /// The subnets associated through [`Self::attachment_id`].
    fn first_attachment_subnets(&self) -> Vec<String> {
        self.subnets[..self.through_first_attachment].to_vec()
    }

    /// The subnets associated through [`Self::second_attachment_id`].
    fn second_attachment_subnets(&self) -> Vec<String> {
        self.subnets[self.through_first_attachment..].to_vec()
    }

    /// Every associated subnet, in the order this report pages them: ascending subnet id.
    fn subnets_in_report_order(&self) -> Vec<String> {
        let mut ordered = self.subnets.clone();
        ordered.sort();
        ordered
    }
}

/// Seed [`Fixture`]: the VPCs and their subnets over the wire, the attachments and the two
/// domains through the store's own writers, then the associations over the wire — so every
/// association this suite reports was MADE by the operation that makes them.
async fn fixture(router: &MultiServiceRouter, engine: &engine::InMemoryEngine) -> Fixture {
    let vpc_id = create_vpc(router, "10.90.0.0/16").await;
    let other_vpc_id = create_vpc(router, "10.91.0.0/16").await;
    let mut subnets = Vec::new();
    for index in 0..ASSOCIATED_SUBNETS {
        subnets.push(
            create_subnet(
                router,
                &vpc_id,
                &format!("10.90.{}.0/24", index + 1),
                "us-east-1a",
            )
            .await,
        );
    }

    // The attachments and the domains are seeded through the store's own writers, inside a
    // scope of their own so the store's guard is released before the next call is awaited.
    let (attachment_id, second_attachment_id, domain_id, other_domain_id) = {
        let store = engine.state::<Ec2State>();
        let mut guard = store.lock();
        let attachment_id = guard
            .insert_transit_gateway_attachment(vpc_attachment(&vpc_id))
            .expect("the store holds the seeded attachment");
        let second_attachment_id = guard
            .insert_transit_gateway_attachment(vpc_attachment(&vpc_id))
            .expect("the store holds the seeded attachment");
        let domain_id = guard
            .insert_transit_gateway_multicast_domain(empty_domain())
            .expect("the store holds the seeded multicast domain");
        let other_domain_id = guard
            .insert_transit_gateway_multicast_domain(empty_domain())
            .expect("the store holds the second seeded multicast domain");
        (
            attachment_id,
            second_attachment_id,
            domain_id,
            other_domain_id,
        )
    };

    // Spread over two attachments, so the attachment filter has something to narrow and the
    // report is not of one attachment's associations by construction.
    let through_first_attachment = ASSOCIATED_SUBNETS - 2;
    for (attachment, named) in [
        (&attachment_id, &subnets[..through_first_attachment]),
        (&second_attachment_id, &subnets[through_first_attachment..]),
    ] {
        let mut params = vec![
            param("TransitGatewayMulticastDomainId", &domain_id),
            param("TransitGatewayAttachmentId", attachment),
        ];
        let named: Vec<&str> = named.iter().map(String::as_str).collect();
        params.extend(indexed("SubnetIds", &named));
        ok_body("the fixture's associations", router, ASSOCIATE, &params).await;
    }

    Fixture {
        domain_id,
        other_domain_id,
        vpc_id,
        other_vpc_id,
        attachment_id,
        second_attachment_id,
        subnets,
        through_first_attachment,
    }
}

/// The associations one domain holds right now, as `(subnet id, attachment id, state)` read
/// straight out of the store.
fn stored_associations(
    engine: &engine::InMemoryEngine,
    domain_id: &str,
) -> BTreeSet<(String, String, String)> {
    engine
        .state::<Ec2State>()
        .lock()
        .transit_gateway_multicast_domains
        .get(domain_id)
        .into_iter()
        .flat_map(|domain| domain.associations.iter())
        .map(|association| {
            (
                association.subnet_id.clone(),
                association.transit_gateway_attachment_id.clone(),
                association.state.clone(),
            )
        })
        .collect()
}

/// The rule's reporting half: every association the domain holds is reported, each member off
/// the stored record, in ascending subnet-id order and with no token when one page holds them
/// all — while a domain of the same account holding no association reports an empty set rather
/// than a refusal.
#[tokio::test]
async fn the_report_answers_exactly_the_associations_the_domain_holds() {
    let case = "every association the domain holds";
    let (router, engine) = super::ec2_query::emulator();
    let seeded = fixture(&router, &engine).await;

    let page = reported_page(case, &router, &report_params(&seeded.domain_id, vec![])).await;
    assert_eq!(
        page.subnet_ids(),
        seeded.subnets_in_report_order(),
        "case {case:?}: expected every associated subnet once, in ascending subnet-id order"
    );
    assert_eq!(
        page.next_token, None,
        "case {case:?}: one page holds every association, so no token is due, got {token:?}",
        token = page.next_token
    );

    // Every member of every entry is the stored association's, not the request's: the
    // attachment each subnet was associated through, the resource that attachment attaches,
    // that resource's type and the account that owns it.
    let first = seeded.first_attachment_subnets();
    let resource_type = catalog_word(
        TRANSIT_GATEWAY_ATTACHMENT_RESOURCE_TYPE_VALUES,
        "TransitGatewayAttachmentResourceType",
        TRANSIT_GATEWAY_ATTACHMENT_RESOURCE_TYPE_VPC,
    );
    for association in &page.associations {
        let subnet_id = association.subnet_id.clone().unwrap_or_default();
        let expected_attachment = if first.contains(&subnet_id) {
            &seeded.attachment_id
        } else {
            &seeded.second_attachment_id
        };
        assert_eq!(
            association.attachment_id.as_deref(),
            Some(expected_attachment.as_str()),
            "case {case:?}: {subnet_id} must be reported under the attachment it was \
             associated through, got {association:?}"
        );
        assert_eq!(
            association.resource_id.as_deref(),
            Some(seeded.vpc_id.as_str()),
            "case {case:?}: expected the resource the attachment attaches, got {association:?}"
        );
        assert_eq!(
            association.resource_type.as_deref(),
            Some(resource_type),
            "case {case:?}: expected the attached resource's type, got {association:?}"
        );
        assert_eq!(
            association.resource_owner_id.as_deref(),
            Some(engine::emulator_account_id()),
            "case {case:?}: expected the account that owns the attached resource, got \
             {association:?}"
        );
        let held = association.state.clone().unwrap_or_default();
        assert!(
            TRANSIT_GATEWAY_MULITCAST_DOMAIN_ASSOCIATION_STATE_VALUES.contains(&held.as_str()),
            "case {case:?}: {held:?} is no member of \
             TransitGatewayMulitcastDomainAssociationState, got {association:?}"
        );
    }

    // What was reported is what the store holds, down to each association's state.
    let reported: BTreeSet<(String, String, String)> = page
        .associations
        .iter()
        .map(|association| {
            (
                association.subnet_id.clone().unwrap_or_default(),
                association.attachment_id.clone().unwrap_or_default(),
                association.state.clone().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        reported,
        stored_associations(&engine, &seeded.domain_id),
        "case {case:?}: the report must answer exactly what the store holds"
    );

    // A domain nothing is associated with is a 200 carrying no association, not a refusal:
    // that is what tells a client an existing domain apart from one it cannot see.
    let empty_case = "a domain of the account holding no association";
    let empty = reported_page(
        empty_case,
        &router,
        &report_params(&seeded.other_domain_id, vec![]),
    )
    .await;
    assert!(
        empty.associations.is_empty() && empty.next_token.is_none(),
        "case {empty_case:?}: expected an empty report, got {held:?} with token {token:?}",
        held = empty.associations,
        token = empty.next_token
    );

    // Reading the report changes nothing a later caller sees: the same call answers the same
    // associations, and the store still holds them.
    let again = reported_page(case, &router, &report_params(&seeded.domain_id, vec![])).await;
    assert_eq!(
        again.subnet_ids(),
        seeded.subnets_in_report_order(),
        "case {case:?}: a second read must answer the same associations"
    );
}

/// One row of the filter table: the `Filters` a request names, and the subnets the report must
/// then answer.
struct FilterRow {
    /// What distinguishes this row.
    case: &'static str,
    /// The filter entries, built from the seeded fixture.
    filters: fn(&Fixture) -> Vec<Param>,
    /// The subnets the report must answer, in any order.
    expected: fn(&Fixture) -> Vec<String>,
}

/// The rows: each of the five documented filter names, one value and several, an exact value
/// and EC2's wildcards, a value nothing carries, and two filters at once — which are joined
/// with an AND.
fn filter_rows() -> Vec<FilterRow> {
    vec![
        FilterRow {
            case: "subnet-id naming one associated subnet",
            filters: |seeded| filters(&[("subnet-id", &[seeded.subnets[0].as_str()])]),
            expected: |seeded| vec![seeded.subnets[0].clone()],
        },
        FilterRow {
            case: "subnet-id naming two associated subnets, which are joined with an OR",
            filters: |seeded| {
                filters(&[(
                    "subnet-id",
                    &[seeded.subnets[0].as_str(), seeded.subnets[2].as_str()],
                )])
            },
            expected: |seeded| vec![seeded.subnets[0].clone(), seeded.subnets[2].clone()],
        },
        FilterRow {
            case: "subnet-id naming a subnet no association of this domain carries",
            filters: |_| {
                let unknown = unknown_id("subnet");
                filters(&[("subnet-id", &[unknown.as_str()])])
            },
            expected: |_| Vec::new(),
        },
        FilterRow {
            case: "subnet-id as a wildcard every subnet id matches",
            filters: |_| filters(&[("subnet-id", &["subnet-*"])]),
            expected: |seeded| seeded.subnets.clone(),
        },
        FilterRow {
            case: "transit-gateway-attachment-id naming the first attachment",
            filters: |seeded| {
                filters(&[(
                    "transit-gateway-attachment-id",
                    &[seeded.attachment_id.as_str()],
                )])
            },
            expected: |seeded| seeded.first_attachment_subnets(),
        },
        FilterRow {
            case: "transit-gateway-attachment-id naming the second attachment",
            filters: |seeded| {
                filters(&[(
                    "transit-gateway-attachment-id",
                    &[seeded.second_attachment_id.as_str()],
                )])
            },
            expected: |seeded| seeded.second_attachment_subnets(),
        },
        FilterRow {
            case: "transit-gateway-attachment-id naming an attachment with no association here",
            filters: |_| {
                let unknown = unknown_id("tgw-attach");
                filters(&[("transit-gateway-attachment-id", &[unknown.as_str()])])
            },
            expected: |_| Vec::new(),
        },
        FilterRow {
            case: "resource-id naming the VPC the attachments attach",
            filters: |seeded| filters(&[("resource-id", &[seeded.vpc_id.as_str()])]),
            expected: |seeded| seeded.subnets.clone(),
        },
        FilterRow {
            case: "resource-id naming another VPC of the account",
            filters: |seeded| filters(&[("resource-id", &[seeded.other_vpc_id.as_str()])]),
            expected: |_| Vec::new(),
        },
        FilterRow {
            case: "resource-type naming the type the attached resource is",
            filters: |_| {
                let vpc = catalog_word(
                    TRANSIT_GATEWAY_ATTACHMENT_RESOURCE_TYPE_VALUES,
                    "TransitGatewayAttachmentResourceType",
                    TRANSIT_GATEWAY_ATTACHMENT_RESOURCE_TYPE_VPC,
                );
                filters(&[("resource-type", &[vpc])])
            },
            expected: |seeded| seeded.subnets.clone(),
        },
        FilterRow {
            case: "resource-type naming another type the catalog publishes",
            filters: |_| {
                // A member of `TransitGatewayAttachmentResourceType` that is not the type
                // every association here carries, read off the modelled catalog.
                let other = catalog_word(
                    TRANSIT_GATEWAY_ATTACHMENT_RESOURCE_TYPE_VALUES,
                    "TransitGatewayAttachmentResourceType",
                    "vpn",
                );
                filters(&[("resource-type", &[other])])
            },
            expected: |_| Vec::new(),
        },
        FilterRow {
            case: "a subnet of the first attachment and that attachment, joined with an AND",
            filters: |seeded| {
                filters(&[
                    ("subnet-id", &[seeded.subnets[0].as_str()]),
                    (
                        "transit-gateway-attachment-id",
                        &[seeded.attachment_id.as_str()],
                    ),
                ])
            },
            expected: |seeded| vec![seeded.subnets[0].clone()],
        },
        FilterRow {
            case: "a subnet of the first attachment and the SECOND attachment, which no \
                   association satisfies at once",
            filters: |seeded| {
                filters(&[
                    ("subnet-id", &[seeded.subnets[0].as_str()]),
                    (
                        "transit-gateway-attachment-id",
                        &[seeded.second_attachment_id.as_str()],
                    ),
                ])
            },
            expected: |_| Vec::new(),
        },
    ]
}

/// The rule's filtering half, over every row: the report answers the associations that carry
/// the filter's value and no others, every filter having to hold.
#[tokio::test]
async fn each_documented_filter_narrows_the_report_to_what_carries_its_value() {
    let (router, engine) = super::ec2_query::emulator();
    let seeded = fixture(&router, &engine).await;

    for row in filter_rows() {
        let case = row.case;
        let page = reported_page(
            case,
            &router,
            &report_params(&seeded.domain_id, (row.filters)(&seeded)),
        )
        .await;
        let mut reported = page.subnet_ids();
        reported.sort();
        let mut expected = (row.expected)(&seeded);
        expected.sort();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the associations carrying the filter's value, got \
             {reported:?}"
        );
    }
}

/// The state filter, through the move the clock owes a new association: the filter selects on
/// the SUBNET association's own state, so the report narrows to `associated` once the move the
/// associate owed has fallen due, answers nothing for a state no association is in, and
/// answers every association for a wildcard both spellings match.
///
/// The move is POLLED rather than waited for, so nothing here depends on how fast the host
/// runs.
#[tokio::test]
async fn the_state_filter_selects_on_the_state_each_association_is_in() {
    let (router, engine) = super::ec2_query::emulator();
    let seeded = fixture(&router, &engine).await;
    let associated = catalog_word(
        TRANSIT_GATEWAY_MULITCAST_DOMAIN_ASSOCIATION_STATE_VALUES,
        "TransitGatewayMulitcastDomainAssociationState",
        MULTICAST_ASSOCIATION_STATE_ASSOCIATED,
    );

    let case = "state naming the terminal state the clock owes a new association";
    let deadline = tokio::time::Instant::now() + move_deadline();
    let settled = loop {
        let page = reported_page(
            case,
            &router,
            &report_params(
                &seeded.domain_id,
                filters(&[("state", &[associated])]),
            ),
        )
        .await;
        if page.associations.len() == seeded.subnets.len() {
            break page;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: every association must reach {associated:?}, last read {held:?}",
            held = page.subnet_ids()
        );
        tokio::time::sleep(poll_interval()).await;
    };
    let mut reported = settled.subnet_ids();
    reported.sort();
    assert_eq!(
        reported,
        seeded.subnets_in_report_order(),
        "case {case:?}: expected every association once it is in {associated:?}"
    );

    // A state no association of this domain is in selects nothing — the filter narrows, it
    // never widens.
    let none_case = "state naming a state no association of this domain is in";
    let disassociated = catalog_word(
        TRANSIT_GATEWAY_MULITCAST_DOMAIN_ASSOCIATION_STATE_VALUES,
        "TransitGatewayMulitcastDomainAssociationState",
        "disassociated",
    );
    let page = reported_page(
        none_case,
        &router,
        &report_params(
            &seeded.domain_id,
            filters(&[("state", &[disassociated])]),
        ),
    )
    .await;
    assert!(
        page.associations.is_empty(),
        "case {none_case:?}: expected no association, got {held:?}",
        held = page.subnet_ids()
    );

    // A wildcard both the transient and the terminal spelling match answers every association
    // whichever of the two each one is in, so this row does not depend on the move above.
    let wildcard_case = "state as a wildcard both association spellings match";
    let page = reported_page(
        wildcard_case,
        &router,
        &report_params(&seeded.domain_id, filters(&[("state", &["associat*"])])),
    )
    .await;
    let mut reported = page.subnet_ids();
    reported.sort();
    assert_eq!(
        reported,
        seeded.subnets_in_report_order(),
        "case {wildcard_case:?}: expected every association, got {reported:?}"
    );
}

/// The paging half: a page carries at most the associations the request asked for, a token is
/// due only while one follows, following that token answers the rest exactly once, and a token
/// is read only against the domain it was minted for.
#[tokio::test]
async fn the_report_pages_in_one_order_under_a_token_scoped_to_its_domain() {
    let (router, engine) = super::ec2_query::emulator();
    let seeded = fixture(&router, &engine).await;
    let ordered = seeded.subnets_in_report_order();

    let case = "a page size one below the number of associations held";
    let limit = usize::try_from(PUBLISHED_MIN_MAX_RESULTS).expect("the published floor fits");
    let first = reported_page(
        case,
        &router,
        &report_params(
            &seeded.domain_id,
            vec![param("MaxResults", &PUBLISHED_MIN_MAX_RESULTS.to_string())],
        ),
    )
    .await;
    assert_eq!(
        first.subnet_ids(),
        ordered[..limit].to_vec(),
        "case {case:?}: expected the first {limit} associations in report order"
    );
    let token = first.next_token.clone().unwrap_or_else(|| {
        panic!("case {case:?}: an association follows this page, so a token is due")
    });

    let next_case = "the page that token names";
    let second = reported_page(
        next_case,
        &router,
        &report_params(
            &seeded.domain_id,
            vec![
                param("MaxResults", &PUBLISHED_MIN_MAX_RESULTS.to_string()),
                param("NextToken", &token),
            ],
        ),
    )
    .await;
    assert_eq!(
        second.subnet_ids(),
        ordered[limit..].to_vec(),
        "case {next_case:?}: expected the associations the first page left behind"
    );
    assert_eq!(
        second.next_token, None,
        "case {next_case:?}: nothing follows this page, so no token is due, got {held:?}",
        held = second.next_token
    );
    // The walk covered every association exactly once.
    let walked: Vec<String> = first
        .subnet_ids()
        .into_iter()
        .chain(second.subnet_ids())
        .collect();
    assert_eq!(
        walked, ordered,
        "case {next_case:?}: the walk must cover every association exactly once"
    );

    // Each published bound of the page size is served, and a page larger than the set holds
    // everything and carries no token.
    for page_size in [PUBLISHED_MIN_MAX_RESULTS, PUBLISHED_MAX_MAX_RESULTS] {
        let bound_case = format!("a page size of {page_size}, a bound the model publishes");
        let page = reported_page(
            &bound_case,
            &router,
            &report_params(
                &seeded.domain_id,
                vec![param("MaxResults", &page_size.to_string())],
            ),
        )
        .await;
        let held = usize::try_from(page_size)
            .expect("a published page size fits")
            .min(ordered.len());
        assert_eq!(
            page.subnet_ids(),
            ordered[..held].to_vec(),
            "case {bound_case:?}: expected the first {held} associations"
        );
    }

    // The token names a position inside ONE domain's report, so presenting it back against
    // another domain is refused rather than resumed from.
    let scoped_case = "a token minted for another domain's report";
    let (status, body) = invoke(
        &router,
        ACTION,
        &report_params(&seeded.other_domain_id, vec![param("NextToken", &token)]),
    )
    .await;
    assert_eq!(
        status, REFUSAL_STATUS,
        "case {scoped_case:?}: expected HTTP {REFUSAL_STATUS}, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        INVALID_PAGINATION_TOKEN,
        "case {scoped_case:?}: expected the pagination-token code, got {body}"
    );
}

/// One row of the refusal table: a request that cannot be served, and the code it is refused
/// with.
struct Refusal {
    /// What distinguishes this row.
    case: &'static str,
    /// The request, built from the seeded fixture.
    params: fn(&Fixture) -> Vec<Param>,
    /// The HTTP status the refusal carries.
    status: u16,
    /// The code the refusal carries.
    code: &'static str,
}

/// The rows: a domain id that resolves to nothing, a value that is not a domain id at all, a
/// filter name this operation does not document, a page size one past each published bound, a
/// token this service did not mint, and an authorized dry run.
fn refusals() -> Vec<Refusal> {
    vec![
        Refusal {
            case: "a well-formed domain id this account holds no domain for",
            params: |_| {
                report_params(
                    &unknown_id(TRANSIT_GATEWAY_MULTICAST_DOMAIN_ID_PREFIX),
                    vec![],
                )
            },
            status: REFUSAL_STATUS,
            code: INVALID_TRANSIT_GATEWAY_MULTICAST_DOMAIN_ID_NOT_FOUND,
        },
        Refusal {
            case: "a value that is not a multicast domain id at all",
            params: |_| report_params("tgw-mcast-domain-not-hex", vec![]),
            status: REFUSAL_STATUS,
            code: INVALID_TRANSIT_GATEWAY_MULTICAST_DOMAIN_ID_MALFORMED,
        },
        Refusal {
            case: "a filter name this operation does not document",
            params: |seeded| {
                report_params(&seeded.domain_id, filters(&[("subnet", &["subnet-*"])]))
            },
            status: REFUSAL_STATUS,
            code: INVALID_PARAMETER_VALUE,
        },
        Refusal {
            case: "a filter this operation documents, misspelled",
            params: |seeded| {
                report_params(&seeded.domain_id, filters(&[("Subnet-Id", &["subnet-*"])]))
            },
            status: REFUSAL_STATUS,
            code: INVALID_PARAMETER_VALUE,
        },
        Refusal {
            case: "a page size one below the smallest the model publishes",
            params: |seeded| {
                report_params(
                    &seeded.domain_id,
                    vec![param(
                        "MaxResults",
                        &(PUBLISHED_MIN_MAX_RESULTS - 1).to_string(),
                    )],
                )
            },
            status: REFUSAL_STATUS,
            code: INVALID_MAX_RESULTS,
        },
        Refusal {
            case: "a page size one above the largest the model publishes",
            params: |seeded| {
                report_params(
                    &seeded.domain_id,
                    vec![param(
                        "MaxResults",
                        &(PUBLISHED_MAX_MAX_RESULTS + 1).to_string(),
                    )],
                )
            },
            status: REFUSAL_STATUS,
            code: INVALID_MAX_RESULTS,
        },
        Refusal {
            case: "a token this service did not mint",
            params: |seeded| {
                report_params(&seeded.domain_id, vec![param("NextToken", "not-a-token")])
            },
            status: REFUSAL_STATUS,
            code: INVALID_PAGINATION_TOKEN,
        },
        Refusal {
            case: "an authorized dry run of a request that would have succeeded",
            params: |seeded| {
                report_params(&seeded.domain_id, vec![param("DryRun", "true")])
            },
            status: DRY_RUN_STATUS,
            code: DRY_RUN_OPERATION,
        },
        Refusal {
            case: "an authorized dry run naming a domain that does not exist",
            params: |_| {
                report_params(
                    &unknown_id(TRANSIT_GATEWAY_MULTICAST_DOMAIN_ID_PREFIX),
                    vec![param("DryRun", "true")],
                )
            },
            // The flag asks about PERMISSION, not about the resources the request names, so
            // the answer does not depend on whether they exist.
            status: DRY_RUN_STATUS,
            code: DRY_RUN_OPERATION,
        },
    ]
}

/// The rule's refusal half, over every row: the request is refused with the code that names
/// what is wrong, it reports no association, and the store is left exactly as it was.
#[tokio::test]
async fn a_report_that_cannot_be_served_is_refused_and_answers_no_association() {
    let (router, engine) = super::ec2_query::emulator();
    let seeded = fixture(&router, &engine).await;
    let held = stored_associations(&engine, &seeded.domain_id);

    for refusal in refusals() {
        let case = refusal.case;
        let (status, body) = invoke(&router, ACTION, &(refusal.params)(&seeded)).await;
        assert_eq!(
            status, refusal.status,
            "case {case:?}: expected HTTP {expected}, got {status} with body {body}",
            expected = refusal.status
        );
        assert_eq!(
            error_code(&body),
            refusal.code,
            "case {case:?}: expected the code {expected:?}, got {body}",
            expected = refusal.code
        );
        assert!(
            !body.contains("<multicastDomainAssociations>"),
            "case {case:?}: a refused report must answer no association, got {body}"
        );
        assert_eq!(
            stored_associations(&engine, &seeded.domain_id),
            held,
            "case {case:?}: a read must leave the store exactly as it was"
        );
    }
}

/// The wording of the refusal for a multicast domain id that resolves to nothing: the one the
/// transit gateway family answers for a resource of it that was deleted or never existed,
/// asserted on two ids the store never minted, so the sentence is built from the request
/// rather than from any one recording.
#[tokio::test]
async fn an_unknown_multicast_domain_is_refused_in_the_family_wording() {
    let (router, _engine) = super::ec2_query::emulator();
    for domain_id in [
        unknown_id(TRANSIT_GATEWAY_MULTICAST_DOMAIN_ID_PREFIX),
        format!("{TRANSIT_GATEWAY_MULTICAST_DOMAIN_ID_PREFIX}-0123456789abcdef0"),
    ] {
        let case = domain_id.clone();
        let (status, body) = invoke(&router, ACTION, &report_params(&domain_id, vec![])).await;
        assert_eq!(
            status, REFUSAL_STATUS,
            "case {case:?}: expected HTTP {REFUSAL_STATUS}, got {status} with body {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("Transit Gateway Multicast Domain {domain_id} was deleted or does not exist."),
            "case {case:?}: expected the family's own wording, got {body}"
        );
    }
}
