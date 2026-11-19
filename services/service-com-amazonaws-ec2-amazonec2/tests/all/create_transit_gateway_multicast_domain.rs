// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `CreateTransitGatewayMulticastDomain`, driven over the wire the
//! service actually speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST whose
//! body carries `Action=CreateTransitGatewayMulticastDomain`, answered with XML (README.md /
//! OPERATIONS.md "Wire protocol").
//!
//! The rule under test is the one every EC2 create of a resource OF a transit gateway
//! follows: it PERSISTS a new resource under a gateway the account holds and that is in
//! service, with every member the service owns derived by the service (a freshly minted id,
//! the ARN that carries it, the owning account, the transient state the create answers with
//! and the clock-owed move out of it) and every member the request chose stored as asked
//! (the options, each defaulting to off, and the tags), and it answers with exactly the
//! record it stored — while a request it cannot serve is refused with the code that names
//! what is wrong and NOTHING is written.
//!
//! Each table row is a distinct input along what that rule generalises over: which of the
//! three `enable`/`disable` options the request names and with which value, how many tags it
//! carries, how many domains one gateway holds, which state the named gateway is in, whether
//! the gateway id resolves at all, and whether an option value is one its enum publishes.
//! Every created domain is read back through `DescribeTransitGatewayMulticastDomains`, so a
//! create that reported a member the store does not hold would fail here.

use std::collections::BTreeSet;
use std::time::Duration;

use ec2::ops::types::{
    AUTO_ACCEPT_SHARED_ASSOCIATIONS_VALUE_VALUES, IGMPV2_SUPPORT_VALUE_VALUES,
    STATIC_SOURCES_SUPPORT_VALUE_VALUES, TRANSIT_GATEWAY_MULTICAST_DOMAIN_STATE_VALUES,
};
use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, DRY_RUN_OPERATION, INCORRECT_STATE,
    INVALID_TRANSIT_GATEWAY_ID_NOT_FOUND, MISSING_PARAMETER,
    RESOURCE_TYPE_TRANSIT_GATEWAY_MULTICAST_DOMAIN, TRANSIT_GATEWAY_MULTICAST_DOMAIN_ID_PREFIX,
    TRANSIT_GATEWAY_STATE_AVAILABLE, TRANSIT_GATEWAY_STATE_DELETING,
    TRANSIT_GATEWAY_STATE_MODIFYING,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{as_timestamp, call, element, error_code, error_message, indexed};
use super::wire::{Param, child, child_text, document, is_long_id, param, set_items};

/// The ec2Query `Action` under test.
const ACTION: &str = "CreateTransitGatewayMulticastDomain";

/// The Region every call here is signed for, and the Region a created domain's ARN names.
const REGION: &str = "us-east-1";

/// The HTTP status EC2 answers `DryRunOperation` with: the request was refused because a
/// precondition the CALLER set holds, not because it was malformed.
const DRY_RUN_STATUS: u16 = 412;

/// The HTTP status EC2 answers every other refusal here with.
const REFUSAL_STATUS: u16 = 400;

/// The code the request boundary refuses a value outside a modelled enum with, before any
/// handler of this service runs: the three `Options` members each target an enum, so a
/// spelling outside it never reaches the handler's own reader of the value, which stands as
/// the backstop behind this.
const BOUNDARY_ENUM_VIOLATION: &str = "ValidationError";

/// A well-formed transit gateway id this account holds no gateway for: the mint counts from
/// one, so the all-zero id is never issued.
const UNISSUED_GATEWAY_ID: &str = "tgw-00000000000000000";

/// Long enough past the span a clock-owed move takes that it is certainly overdue: the
/// deadline the poll below gives up at, generous because what is under test is the state a
/// resource reaches rather than how fast it reaches it.
fn move_deadline() -> Duration {
    Duration::from_secs(60)
}

/// How long the poll below waits between two reads of a state.
fn poll_interval() -> Duration {
    Duration::from_millis(50)
}

/// A member of `Igmpv2SupportValue`, from the modelled catalog
/// (`smithy_query(query="shape",shape="Igmpv2SupportValue")`, whose values are
/// `enable | disable`) rather than written out here.
fn igmpv2(name: &str) -> &'static str {
    catalog_word(IGMPV2_SUPPORT_VALUE_VALUES, "Igmpv2SupportValue", name)
}

/// A member of `StaticSourcesSupportValue`, from its own modelled catalog.
fn static_sources(name: &str) -> &'static str {
    catalog_word(
        STATIC_SOURCES_SUPPORT_VALUE_VALUES,
        "StaticSourcesSupportValue",
        name,
    )
}

/// A member of `AutoAcceptSharedAssociationsValue`, from its own modelled catalog.
fn auto_accept(name: &str) -> &'static str {
    catalog_word(
        AUTO_ACCEPT_SHARED_ASSOCIATIONS_VALUE_VALUES,
        "AutoAcceptSharedAssociationsValue",
        name,
    )
}

/// A member of `TransitGatewayMulticastDomainState`, from its own modelled catalog (whose
/// values are `pending | available | deleting | deleted`).
fn domain_state(name: &str) -> &'static str {
    catalog_word(
        TRANSIT_GATEWAY_MULTICAST_DOMAIN_STATE_VALUES,
        "TransitGatewayMulticastDomainState",
        name,
    )
}

/// `name` as the modelled catalog `values` publishes it, or a panic: nothing here writes an
/// enum member out by hand, so a model that renames one fails the suite rather than passing
/// it against a stale spelling.
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
async fn ok_body(
    case: &str,
    router: &MultiServiceRouter,
    action: &str,
    params: &[Param],
) -> String {
    let (status, body) = invoke(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// A transit gateway of this account that is IN SERVICE: created over the wire, then polled
/// through `DescribeTransitGateways` until the clock has moved it to `available`.
///
/// Polled rather than slept for, so a loaded machine cannot read the gateway before its move
/// has been applied.
async fn gateway_in_service(router: &MultiServiceRouter) -> String {
    let transit_gateway_id = create_transit_gateway(router).await;
    let deadline = tokio::time::Instant::now() + move_deadline();
    loop {
        let reported = reported_gateway_state(router, &transit_gateway_id).await;
        if reported == TRANSIT_GATEWAY_STATE_AVAILABLE {
            return transit_gateway_id;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the fixture's transit gateway {transit_gateway_id} must reach \
             {TRANSIT_GATEWAY_STATE_AVAILABLE:?}, last read {reported:?}"
        );
        tokio::time::sleep(poll_interval()).await;
    }
}

/// One `CreateTransitGateway` call, as the id of the gateway it created. The gateway comes up
/// in the transient `pending` state, with the clock owing it `available`.
async fn create_transit_gateway(router: &MultiServiceRouter) -> String {
    let body = ok_body("the fixture's gateway", router, "CreateTransitGateway", &[]).await;
    element(&body, "transitGatewayId").unwrap_or_else(|| panic!("no transitGatewayId in {body}"))
}

/// The state `DescribeTransitGateways` reports for one gateway.
async fn reported_gateway_state(router: &MultiServiceRouter, transit_gateway_id: &str) -> String {
    let body = ok_body(
        "the gateway's state",
        router,
        "DescribeTransitGateways",
        &indexed("TransitGatewayIds", &[transit_gateway_id]),
    )
    .await;
    let parsed = document("the gateway's state", &body);
    parsed
        .descendants()
        .find(|node| {
            node.is_element()
                && child_text(*node, "transitGatewayId").as_deref() == Some(transit_gateway_id)
        })
        .and_then(|gateway| child_text(gateway, "state"))
        .unwrap_or_else(|| panic!("no state for {transit_gateway_id} in {body}"))
}

/// Put one stored transit gateway in `state`, so a row about a gateway that is NOT in service
/// is decided by the state the store holds rather than by a race with the clock.
///
/// The clock-owed move is dropped with it: only a `pending` or `modifying` gateway is owed
/// `available`, and nothing here is owed anything.
fn force_gateway_state(engine: &engine::InMemoryEngine, transit_gateway_id: &str, state: &str) {
    let store = engine.state::<Ec2State>();
    let mut guard = store.lock();
    guard.transit_gateway_available_at.remove(transit_gateway_id);
    guard
        .transit_gateways
        .get_mut(transit_gateway_id)
        .unwrap_or_else(|| panic!("the store holds the gateway {transit_gateway_id}"))
        .state = Some(state.to_string());
}

/// The form fields of one `CreateTransitGatewayMulticastDomain` call: the gateway, the option
/// members the row names and the tags it asks for.
fn create_params(
    transit_gateway_id: &str,
    options: &[(&str, &str)],
    tags: &[(&str, &str)],
) -> Vec<Param> {
    let mut params = vec![param("TransitGatewayId", transit_gateway_id)];
    for (member, value) in options {
        params.push(param(&format!("Options.{member}"), value));
    }
    if !tags.is_empty() {
        params.push(param(
            "TagSpecification.1.ResourceType",
            RESOURCE_TYPE_TRANSIT_GATEWAY_MULTICAST_DOMAIN,
        ));
        for (index, (key, value)) in tags.iter().enumerate() {
            params.push(param(
                &format!("TagSpecification.1.Tag.{}.Key", index + 1),
                key,
            ));
            params.push(param(
                &format!("TagSpecification.1.Tag.{}.Value", index + 1),
                value,
            ));
        }
    }
    params
}

/// One reported `TransitGatewayMulticastDomain`, read out of the XML by the document rather
/// than by string surgery: the `options` and `tagSet` children carry members of their own.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reported {
    domain_id: String,
    transit_gateway_id: String,
    arn: Option<String>,
    owner_id: String,
    options: Vec<(String, String)>,
    state: String,
    creation_time: Option<String>,
    tags: Vec<(String, String)>,
}

/// The one multicast domain a response carries — the element that has a
/// `transitGatewayMulticastDomainId` child, whether the body is a create's single
/// `transitGatewayMulticastDomain` or a describe's set narrowed to one id.
fn reported(case: &str, body: &str) -> Reported {
    let parsed = document(case, body);
    let mut found: Vec<Reported> = parsed
        .descendants()
        .filter(|node| {
            node.is_element() && child(*node, "transitGatewayMulticastDomainId").is_some()
        })
        .map(|item| Reported {
            domain_id: child_text(item, "transitGatewayMulticastDomainId").unwrap_or_default(),
            transit_gateway_id: child_text(item, "transitGatewayId").unwrap_or_default(),
            arn: child_text(item, "transitGatewayMulticastDomainArn"),
            owner_id: child_text(item, "ownerId").unwrap_or_default(),
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
            state: child_text(item, "state").unwrap_or_default(),
            creation_time: child_text(item, "creationTime"),
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
        found.len(),
        1,
        "case {case:?}: expected exactly one multicast domain, got {found:?} in {body}"
    );
    found.remove(0)
}

/// The one domain `DescribeTransitGatewayMulticastDomains` reports for `domain_id`.
async fn described(case: &str, router: &MultiServiceRouter, domain_id: &str) -> Reported {
    let body = ok_body(
        case,
        router,
        "DescribeTransitGatewayMulticastDomains",
        &indexed("TransitGatewayMulticastDomainIds", &[domain_id]),
    )
    .await;
    reported(case, &body)
}

/// Every multicast domain id the store holds right now — what a refusal must leave unchanged.
fn stored_ids(engine: &engine::InMemoryEngine) -> BTreeSet<String> {
    engine
        .state::<Ec2State>()
        .lock()
        .transit_gateway_multicast_domains
        .keys()
        .cloned()
        .collect()
}

/// One served create: the option members it names and the tags it asks for, beside what the
/// domain must then report for each of the three options.
struct Served {
    label: &'static str,
    options: Vec<(&'static str, &'static str)>,
    tags: Vec<(&'static str, &'static str)>,
    expected_options: Vec<(&'static str, &'static str)>,
}

/// The served rows: every option member on its own, all three together, all three turned off
/// by name, and a request that names none at all — with no tags, one tag and two.
fn served_cases() -> Vec<Served> {
    let off = || {
        vec![
            ("igmpv2Support", igmpv2("disable")),
            ("staticSourcesSupport", static_sources("disable")),
            ("autoAcceptSharedAssociations", auto_accept("disable")),
        ]
    };
    vec![
        Served {
            label: "no options and no tags",
            options: Vec::new(),
            tags: Vec::new(),
            expected_options: off(),
        },
        Served {
            label: "IGMPv2 turned on, one tag",
            options: vec![("Igmpv2Support", igmpv2("enable"))],
            tags: vec![("Name", "multicast")],
            expected_options: vec![
                ("igmpv2Support", igmpv2("enable")),
                ("staticSourcesSupport", static_sources("disable")),
                ("autoAcceptSharedAssociations", auto_accept("disable")),
            ],
        },
        Served {
            label: "static sources turned on",
            options: vec![("StaticSourcesSupport", static_sources("enable"))],
            tags: Vec::new(),
            expected_options: vec![
                ("igmpv2Support", igmpv2("disable")),
                ("staticSourcesSupport", static_sources("enable")),
                ("autoAcceptSharedAssociations", auto_accept("disable")),
            ],
        },
        Served {
            label: "shared associations accepted automatically, two tags",
            options: vec![(
                "AutoAcceptSharedAssociations",
                auto_accept("enable"),
            )],
            tags: vec![("Name", "shared"), ("Stage", "test")],
            expected_options: vec![
                ("igmpv2Support", igmpv2("disable")),
                ("staticSourcesSupport", static_sources("disable")),
                ("autoAcceptSharedAssociations", auto_accept("enable")),
            ],
        },
        Served {
            label: "every option turned on, two tags",
            options: vec![
                ("Igmpv2Support", igmpv2("enable")),
                ("StaticSourcesSupport", static_sources("enable")),
                ("AutoAcceptSharedAssociations", auto_accept("enable")),
            ],
            tags: vec![("Team", "network"), ("Zone", "all")],
            expected_options: vec![
                ("igmpv2Support", igmpv2("enable")),
                ("staticSourcesSupport", static_sources("enable")),
                ("autoAcceptSharedAssociations", auto_accept("enable")),
            ],
        },
        Served {
            label: "every option turned off by name, one tag",
            options: vec![
                ("Igmpv2Support", igmpv2("disable")),
                ("StaticSourcesSupport", static_sources("disable")),
                ("AutoAcceptSharedAssociations", auto_accept("disable")),
            ],
            tags: vec![("Name", "quiet")],
            expected_options: off(),
        },
    ]
}

/// A create on a gateway in service persists the domain it reports: a freshly minted id, the
/// ARN that carries it, this account as the owner, the options the request asked for (each
/// absent member defaulting to off), the tags it asked for, the transient `pending` state and
/// a creation time — and every member reads back identically through
/// `DescribeTransitGatewayMulticastDomains`.
///
/// One row per [`served_cases`] entry, all on ONE gateway, so no single request is the
/// specification and a gateway holding several domains is covered too.
#[tokio::test]
async fn a_created_domain_is_persisted_and_reads_back() {
    let (router, engine) = super::ec2_query::emulator();
    let transit_gateway_id = gateway_in_service(&router).await;

    let mut minted: Vec<String> = Vec::new();
    for case in served_cases() {
        let label = case.label;
        let body = ok_body(
            label,
            &router,
            ACTION,
            &create_params(&transit_gateway_id, &case.options, &case.tags),
        )
        .await;
        let created = reported(label, &body);

        assert!(
            is_long_id(TRANSIT_GATEWAY_MULTICAST_DOMAIN_ID_PREFIX, &created.domain_id),
            "case {label:?}: expected a {TRANSIT_GATEWAY_MULTICAST_DOMAIN_ID_PREFIX}- id of \
             17 hex digits, got {:?}",
            created.domain_id
        );
        assert_eq!(
            created.transit_gateway_id, transit_gateway_id,
            "case {label:?}: expected the domain under {transit_gateway_id}, got {:?}",
            created.transit_gateway_id
        );
        let expected_arn = engine::emulator_arn(
            "ec2",
            REGION,
            &format!(
                "{RESOURCE_TYPE_TRANSIT_GATEWAY_MULTICAST_DOMAIN}/{}",
                created.domain_id
            ),
        );
        assert_eq!(
            created.arn.as_deref(),
            Some(expected_arn.as_str()),
            "case {label:?}: expected the ARN {expected_arn:?}, got {:?}",
            created.arn
        );
        assert_eq!(
            created.owner_id,
            engine::emulator_account_id(),
            "case {label:?}: expected this account as the owner, got {:?}",
            created.owner_id
        );
        let expected_options: Vec<(String, String)> = case
            .expected_options
            .iter()
            .map(|(member, value)| ((*member).to_string(), (*value).to_string()))
            .collect();
        let mut reported_options = created.options.clone();
        reported_options.sort();
        let mut wanted_options = expected_options.clone();
        wanted_options.sort();
        assert_eq!(
            reported_options, wanted_options,
            "case {label:?}: expected the options {wanted_options:?}, got {reported_options:?}"
        );
        assert_eq!(
            created.state,
            domain_state("pending"),
            "case {label:?}: a domain a create answers with is still coming up, got {:?}",
            created.state
        );
        let creation_time = created
            .creation_time
            .clone()
            .unwrap_or_else(|| panic!("case {label:?}: a created domain carries a creationTime"));
        assert!(
            as_timestamp(&creation_time).is_some(),
            "case {label:?}: expected an ISO 8601 creationTime, got {creation_time:?}"
        );
        let expected_tags: Vec<(String, String)> = case
            .tags
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        assert_eq!(
            created.tags, expected_tags,
            "case {label:?}: expected the tags {expected_tags:?}, got {:?}",
            created.tags
        );

        // The round trip: what the create answered is what the store holds, so a describe
        // narrowed to the new id reports the same domain member for member. Only the state
        // may have moved on, the clock owing this domain `available`.
        let read_back = described(label, &router, &created.domain_id).await;
        assert_eq!(
            Reported {
                state: created.state.clone(),
                ..read_back.clone()
            },
            created,
            "case {label:?}: the describe must report the domain the create stored, got \
             {read_back:?}"
        );

        minted.push(created.domain_id);
    }

    let unique: BTreeSet<&String> = minted.iter().collect();
    assert_eq!(
        unique.len(),
        minted.len(),
        "every create mints its own id, got {minted:?}"
    );
    assert_eq!(
        stored_ids(&engine),
        minted.iter().cloned().collect::<BTreeSet<String>>(),
        "the store must hold exactly the domains the creates reported, got {minted:?}"
    );
}

/// A domain a create answers as `pending` reaches `available` on the clock, whether or not
/// anybody calls: polled through the describe for the STATE rather than for a span, so what
/// is asserted is the state it reaches.
#[tokio::test]
async fn a_created_domain_reaches_available_on_the_clock() {
    let (router, _engine) = super::ec2_query::emulator();
    let transit_gateway_id = gateway_in_service(&router).await;
    let case = "the domain's own move";

    let body = ok_body(
        case,
        &router,
        ACTION,
        &create_params(&transit_gateway_id, &[], &[]),
    )
    .await;
    let created = reported(case, &body);
    assert_eq!(
        created.state,
        domain_state("pending"),
        "case {case:?}: the create answers the transient state, got {:?}",
        created.state
    );

    let deadline = tokio::time::Instant::now() + move_deadline();
    loop {
        let read_back = described(case, &router, &created.domain_id).await;
        if read_back.state == domain_state("available") {
            break;
        }
        assert_eq!(
            read_back.state,
            domain_state("pending"),
            "case {case:?}: a domain coming up is `pending` until it is `available`, got {:?}",
            read_back.state
        );
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: the domain {} must reach {:?}",
            created.domain_id,
            domain_state("available")
        );
        tokio::time::sleep(poll_interval()).await;
    }
}

/// Which gateway a refusal row names.
#[derive(Debug, Clone, Copy)]
enum Named {
    /// A gateway of this account that is in service.
    InService,
    /// A gateway of this account a modification is still being applied to.
    Modifying,
    /// A gateway of this account a delete has started on.
    Deleting,
    /// A well-formed transit gateway id this account holds no gateway for.
    UnissuedId,
    /// No `TransitGatewayId` member at all.
    Absent,
}

/// One refusal row: the gateway it names, the extra members it carries, and the code and
/// sentence EC2 answers it with.
struct Refusal {
    label: &'static str,
    named: Named,
    extra: Vec<(&'static str, String)>,
    status: u16,
    code: &'static str,
}

/// A request this operation cannot serve is refused with the code that names what is wrong,
/// and NOTHING is written: an id the account holds no gateway for with the gateway's own
/// `NotFound`, a gateway that is not in service with `IncorrectState`, an option value no
/// enum publishes with the boundary's own refusal, a request that carries no gateway id with
/// `MissingParameter`, and a dry run with `DryRunOperation`.
///
/// One row per kind, each a distinct input along what the rule generalises over: which state
/// the gateway is in, which option carries the value no enum publishes, and whether the
/// request would otherwise have succeeded.
#[tokio::test]
async fn a_request_this_operation_cannot_serve_is_refused_and_stores_nothing() {
    let (router, engine) = super::ec2_query::emulator();
    let in_service = gateway_in_service(&router).await;
    let modifying = gateway_in_service(&router).await;
    force_gateway_state(&engine, &modifying, TRANSIT_GATEWAY_STATE_MODIFYING);
    let deleting = gateway_in_service(&router).await;
    force_gateway_state(&engine, &deleting, TRANSIT_GATEWAY_STATE_DELETING);
    let before = stored_ids(&engine);

    let refusals = vec![
        Refusal {
            label: "an id this account holds no gateway for",
            named: Named::UnissuedId,
            extra: Vec::new(),
            status: REFUSAL_STATUS,
            code: INVALID_TRANSIT_GATEWAY_ID_NOT_FOUND,
        },
        Refusal {
            label: "a gateway a modification is still being applied to",
            named: Named::Modifying,
            extra: Vec::new(),
            status: REFUSAL_STATUS,
            code: INCORRECT_STATE,
        },
        Refusal {
            label: "a gateway a delete has started on, with tags",
            named: Named::Deleting,
            extra: vec![
                (
                    "TagSpecification.1.ResourceType",
                    RESOURCE_TYPE_TRANSIT_GATEWAY_MULTICAST_DOMAIN.to_string(),
                ),
                ("TagSpecification.1.Tag.1.Key", "Name".to_string()),
                ("TagSpecification.1.Tag.1.Value", "doomed".to_string()),
            ],
            status: REFUSAL_STATUS,
            code: INCORRECT_STATE,
        },
        Refusal {
            label: "an IGMPv2 support value no enum publishes",
            named: Named::InService,
            extra: vec![("Options.Igmpv2Support", "yes".to_string())],
            status: REFUSAL_STATUS,
            code: BOUNDARY_ENUM_VIOLATION,
        },
        Refusal {
            label: "a static sources support value no enum publishes",
            named: Named::InService,
            extra: vec![("Options.StaticSourcesSupport", "ENABLE".to_string())],
            status: REFUSAL_STATUS,
            code: BOUNDARY_ENUM_VIOLATION,
        },
        Refusal {
            label: "an auto-accept value no enum publishes",
            named: Named::InService,
            extra: vec![(
                "Options.AutoAcceptSharedAssociations",
                "enabled".to_string(),
            )],
            status: REFUSAL_STATUS,
            code: BOUNDARY_ENUM_VIOLATION,
        },
        Refusal {
            label: "no gateway id at all",
            named: Named::Absent,
            extra: Vec::new(),
            status: REFUSAL_STATUS,
            code: MISSING_PARAMETER,
        },
        Refusal {
            label: "a dry run of a request that would have succeeded",
            named: Named::InService,
            extra: vec![("DryRun", "true".to_string())],
            status: DRY_RUN_STATUS,
            code: DRY_RUN_OPERATION,
        },
    ];

    for refusal in &refusals {
        let label = refusal.label;
        let mut params: Vec<Param> = match refusal.named {
            Named::InService => vec![param("TransitGatewayId", &in_service)],
            Named::Modifying => vec![param("TransitGatewayId", &modifying)],
            Named::Deleting => vec![param("TransitGatewayId", &deleting)],
            Named::UnissuedId => vec![param("TransitGatewayId", UNISSUED_GATEWAY_ID)],
            Named::Absent => Vec::new(),
        };
        params.extend(
            refusal
                .extra
                .iter()
                .map(|(name, value)| param(name, value)),
        );

        let (status, body) = invoke(&router, ACTION, &params).await;
        assert_eq!(
            status, refusal.status,
            "case {label:?}: expected HTTP {}, got {status} with body {body}",
            refusal.status
        );
        let code = error_code(&body);
        assert_eq!(
            code, refusal.code,
            "case {label:?}: expected the code {:?}, got {code:?} with body {body}",
            refusal.code
        );
        assert!(
            !error_message(&body).is_empty(),
            "case {label:?}: a refusal says what is wrong, got {body}"
        );
    }

    // The refusals above include ones that get as far as resolving the gateway, so what they
    // must leave behind is exactly the domains the store held before any of them ran: none.
    let after = stored_ids(&engine);
    assert_eq!(
        after, before,
        "a refused request stores nothing, so the store must still hold {before:?}, got \
         {after:?}"
    );
    assert!(
        after.is_empty(),
        "no row above asked for a domain the service could serve, got {after:?}"
    );
}

/// The id an unknown gateway is refused by is the one the REQUEST named, and the sentence is
/// the transit gateway family's own — the refusal a client reads to tell a dangling reference
/// from a gateway it cannot use.
#[tokio::test]
async fn an_unknown_gateway_is_named_in_its_own_refusal() {
    let (router, _engine) = super::ec2_query::emulator();
    let case = "an id this account holds no gateway for";

    let (status, body) = invoke(
        &router,
        ACTION,
        &create_params(UNISSUED_GATEWAY_ID, &[], &[]),
    )
    .await;
    assert_eq!(
        status, REFUSAL_STATUS,
        "case {case:?}: expected HTTP {REFUSAL_STATUS}, got {status} with body {body}"
    );
    let expected = format!("Transit Gateway {UNISSUED_GATEWAY_ID} was deleted or does not exist.");
    let message = error_message(&body);
    assert_eq!(
        message, expected,
        "case {case:?}: expected the sentence {expected:?}, got {message:?}"
    );
}
