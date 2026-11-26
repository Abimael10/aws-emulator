// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `CreateTrafficMirrorSession`, driven over the wire the service
//! actually speaks — `ec2Query`, an `application/x-www-form-urlencoded` POST whose body
//! carries `Action=<Operation>`, answered with XML (README.md / OPERATIONS.md "Wire
//! protocol") — through the crate's shared [`super::ec2_query`] harness.
//!
//! The rules under test:
//!
//! 1. "Creates a Traffic Mirror session. A Traffic Mirror session actively copies packets
//!    from a Traffic Mirror source to a Traffic Mirror target"
//!    (`CreateTrafficMirrorSession`): the create MINTS a session from the three resources
//!    the request names, carries every member the request set, derives the owning account,
//!    and `DescribeTrafficMirrorSessions` reports the same record afterwards — so the answer
//!    is the store rather than the request object.
//! 2. The bounded integers are held to the ranges their members publish — `SessionNumber`
//!    "Valid values are 1-32766", `PacketLength` "Valid values are 1-8500", and
//!    `VirtualNetworkId` the 24-bit VXLAN network identifier of RFC 7348 — so each published
//!    bound is accepted and a value one past it is refused `InvalidParameterValue`, naming
//!    the value, the member and the bounds, with nothing stored.
//! 3. The source interface, the filter and the target have to be resources this account
//!    holds: each unresolvable id is refused with the code that id is resolved under, an id
//!    that is not a network interface id at all with the code for an id of the wrong shape,
//!    and nothing is stored.
//! 4. "If you do not specify a VirtualNetworkId, an account-wide unique ID is chosen"
//!    (`CreateTrafficMirrorSessionRequest$VirtualNetworkId`): a request that names none is
//!    given an identifier a VXLAN header carries that no other session of the account uses,
//!    and a request that names one keeps exactly that one.
//! 5. "For sessions with Network Load Balancer (NLB) Traffic Mirror targets the default
//!    PacketLength will be set to 8500", while "Do not specify this parameter when you want
//!    to mirror the entire packet" (`CreateTrafficMirrorSessionRequest$PacketLength`): the
//!    default is read off the TARGET the session mirrors to, and a session to any other kind
//!    of target carries no `PacketLength` at all.
//! 6. One `ClientToken` names one request: a repeat of the same create reports the session
//!    the first one minted and stores nothing, a repeat describing a different session is
//!    refused `IdempotentParameterMismatch`, and a create carrying no token is never a
//!    replay. `DryRun` answers `DryRunOperation` (HTTP 412) and stores nothing.
//!
//! Each table row is a distinct input along what the rule generalises over — which members
//! the request carries, which kind of target the session mirrors to, which bound is probed,
//! which named resource is unresolvable, which part of a repeated request changed — so no row
//! is the specification. What `DeleteTrafficMirrorSession` and
//! `DescribeTrafficMirrorSessions` do with a stored session is the subject of
//! [`super::traffic_mirror_sessions`].

use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, IDEMPOTENT_PARAMETER_MISMATCH,
    IDEMPOTENT_PARAMETER_MISMATCH_MESSAGE, INVALID_NETWORK_INTERFACE_ID_MALFORMED,
    INVALID_NETWORK_INTERFACE_ID_NOT_FOUND, INVALID_TRAFFIC_MIRROR_FILTER_ID_NOT_FOUND,
    INVALID_TRAFFIC_MIRROR_TARGET_ID_NOT_FOUND, NETWORK_LOAD_BALANCER_DEFAULT_PACKET_LENGTH,
    TRAFFIC_MIRROR_PACKET_LENGTH_RANGE, TRAFFIC_MIRROR_SESSION_ID_PREFIX,
    TRAFFIC_MIRROR_SESSION_NUMBER_RANGE, TRAFFIC_MIRROR_SESSION_TAGGABLE_RESOURCE,
    TRAFFIC_MIRROR_VIRTUAL_NETWORK_ID_RANGE,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, error_code, error_message};
use super::wire::{child_text, document, is_long_id, set_items};

/// The Region every call here is signed for. A Traffic Mirror session is named by an id
/// unique across this store and the modelled shape names no Region, so the Region only
/// selects the envelope.
const REGION: &str = "us-west-2";

/// The error code EC2 answers for a value its parameter layer cannot take.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// One request's form parameters, as the ec2Query harness takes them.
type Params = Vec<(String, String)>;

/// A Traffic Mirror session as a response reported it: every member of the modelled
/// `TrafficMirrorSession` shape
/// (`smithy_query(query="shape",shape="TrafficMirrorSession")`), so a create and a describe
/// of one session are compared member for member.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Reported {
    id: Option<String>,
    traffic_mirror_target_id: Option<String>,
    traffic_mirror_filter_id: Option<String>,
    network_interface_id: Option<String>,
    owner_id: Option<String>,
    packet_length: Option<String>,
    session_number: Option<String>,
    virtual_network_id: Option<String>,
    description: Option<String>,
    tags: Vec<(String, String)>,
}

/// Every Traffic Mirror session a response body reports, in document order: the element that
/// carries a `trafficMirrorSessionId` child, whether it is the create's own
/// `<trafficMirrorSession>` or one `<item>` of the describe's `<trafficMirrorSessionSet>`.
fn reported(case: &str, body: &str) -> Vec<Reported> {
    let parsed = document(case, body);
    parsed
        .descendants()
        .filter(|node| node.is_element() && child_text(*node, "trafficMirrorSessionId").is_some())
        .map(|node| Reported {
            id: child_text(node, "trafficMirrorSessionId"),
            traffic_mirror_target_id: child_text(node, "trafficMirrorTargetId"),
            traffic_mirror_filter_id: child_text(node, "trafficMirrorFilterId"),
            network_interface_id: child_text(node, "networkInterfaceId"),
            owner_id: child_text(node, "ownerId"),
            packet_length: child_text(node, "packetLength"),
            session_number: child_text(node, "sessionNumber"),
            virtual_network_id: child_text(node, "virtualNetworkId"),
            description: child_text(node, "description"),
            tags: set_items(node, "tagSet")
                .into_iter()
                .map(|item| {
                    (
                        child_text(item, "key").unwrap_or_default(),
                        child_text(item, "value").unwrap_or_default(),
                    )
                })
                .collect(),
        })
        .collect()
}

/// The one session a body reports, or a panic naming the case.
fn one(case: &str, body: &str) -> Reported {
    let mut found = reported(case, body);
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: expected exactly one Traffic Mirror session in {body}"
    );
    found.remove(0)
}

async fn create(router: &MultiServiceRouter, params: &[(String, String)]) -> (u16, String) {
    call(router, REGION, "CreateTrafficMirrorSession", params).await
}

async fn describe(router: &MultiServiceRouter, params: &[(String, String)]) -> (u16, String) {
    call(router, REGION, "DescribeTrafficMirrorSessions", params).await
}

/// The session ids the store holds, in its own order — the fact a create or a refusal is
/// checked against, read through the engine rather than through the operation under test.
fn stored_ids(engine: &engine::InMemoryEngine) -> Vec<String> {
    engine
        .state::<Ec2State>()
        .lock()
        .traffic_mirror_sessions
        .keys()
        .cloned()
        .collect()
}

/// The resources of this account a session mirrors between, each created through the
/// operation that mints it, so nothing under test is a value this suite invented.
struct Fixtures {
    /// The source network interface, from `CreateNetworkInterface`.
    network_interface_id: String,
    /// The filter the session mirrors by, from `CreateTrafficMirrorFilter`.
    traffic_mirror_filter_id: String,
    /// A target whose destination is a network interface, from
    /// `CreateTrafficMirrorTarget`.
    interface_target_id: String,
    /// A target whose destination is a Network Load Balancer, from
    /// `CreateTrafficMirrorTarget`.
    load_balancer_target_id: String,
}

/// A Network Load Balancer ARN. A load balancer is a resource of ANOTHER service, which this
/// store holds none of, so `CreateTrafficMirrorTarget` holds the value to the FORM of a load
/// balancer ARN and no further.
fn network_load_balancer_arn() -> String {
    format!(
        "arn:aws:elasticloadbalancing:{REGION}:{account}:loadbalancer/net/mirror/0123456789abcdef",
        account = engine::emulator_account_id(),
    )
}

/// Create the filter, the target and the source interface a session needs, through the
/// operations that mint them.
async fn fixtures(
    router: &MultiServiceRouter,
    engine: &engine::InMemoryEngine,
    case: &str,
) -> Fixtures {
    let subnet_id = {
        let store = engine.state::<Ec2State>();
        let store = store.lock();
        let vpc_id = store
            .vpcs
            .values()
            .find(|vpc| vpc.is_default == Some(true))
            .and_then(|vpc| vpc.vpc_id.clone())
            .unwrap_or_else(|| panic!("case {case:?}: a fresh account has its default VPC"));
        store
            .subnets
            .values()
            .find(|subnet| subnet.vpc_id.as_deref() == Some(vpc_id.as_str()))
            .and_then(|subnet| subnet.subnet_id.clone())
            .unwrap_or_else(|| {
                panic!("case {case:?}: a fresh account has the default VPC's subnets")
            })
    };

    let mut interfaces: Vec<String> = Vec::new();
    for which in ["the source", "the target's destination"] {
        let (status, body) = call(
            router,
            REGION,
            "CreateNetworkInterface",
            &[p("SubnetId", subnet_id.clone())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateNetworkInterface must succeed for {which}, got {status} with \
             body {body}"
        );
        interfaces.push(
            super::ec2_query::element(&body, "networkInterfaceId")
                .unwrap_or_else(|| panic!("case {case:?}: no <networkInterfaceId> in {body}")),
        );
    }

    let (status, body) = call(router, REGION, "CreateTrafficMirrorFilter", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateTrafficMirrorFilter must succeed, got {status} with body {body}"
    );
    let traffic_mirror_filter_id = super::ec2_query::element(&body, "trafficMirrorFilterId")
        .unwrap_or_else(|| panic!("case {case:?}: no <trafficMirrorFilterId> in {body}"));

    let (status, body) = call(
        router,
        REGION,
        "CreateTrafficMirrorTarget",
        &[p("NetworkInterfaceId", interfaces[1].clone())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateTrafficMirrorTarget must succeed for an interface target, got \
         {status} with body {body}"
    );
    let interface_target_id = super::ec2_query::element(&body, "trafficMirrorTargetId")
        .unwrap_or_else(|| panic!("case {case:?}: no <trafficMirrorTargetId> in {body}"));

    let (status, body) = call(
        router,
        REGION,
        "CreateTrafficMirrorTarget",
        &[p("NetworkLoadBalancerArn", network_load_balancer_arn())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateTrafficMirrorTarget must succeed for a load balancer target, got \
         {status} with body {body}"
    );
    let load_balancer_target_id = super::ec2_query::element(&body, "trafficMirrorTargetId")
        .unwrap_or_else(|| panic!("case {case:?}: no <trafficMirrorTargetId> in {body}"));

    Fixtures {
        network_interface_id: interfaces[0].clone(),
        traffic_mirror_filter_id,
        interface_target_id,
        load_balancer_target_id,
    }
}

/// Which of the two targets a row mirrors to: the kind of destination is what decides the
/// default `PacketLength`, so it is part of every create table here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TargetKind {
    /// A target whose destination is a network interface.
    Interface,
    /// A target whose destination is a Network Load Balancer.
    LoadBalancer,
}

/// One row of the create table.
#[derive(Clone, Debug)]
struct CreateRow {
    case: &'static str,
    target: TargetKind,
    session_number: i32,
    packet_length: Option<i32>,
    virtual_network_id: Option<i32>,
    description: Option<&'static str>,
    tags: &'static [(&'static str, &'static str)],
}

/// The rows every create table here is driven over: sessions differing along the kind of
/// target they mirror to, the session number (at both published bounds), whether they name a
/// packet length (at both published bounds) and a virtual network id (at the widest the VXLAN
/// header carries), and the caller-chosen text they carry — none at all, a description alone,
/// one tag alone, and a description holding punctuation and non-ASCII text beside two tags.
const CREATE_ROWS: &[CreateRow] = &[
    CreateRow {
        case: "a session carrying only what it mirrors between",
        target: TargetKind::Interface,
        session_number: 1,
        packet_length: None,
        virtual_network_id: None,
        description: None,
        tags: &[],
    },
    CreateRow {
        case: "a session to a load balancer target, naming no packet length",
        target: TargetKind::LoadBalancer,
        session_number: 2,
        packet_length: None,
        virtual_network_id: Some(4_242),
        description: Some("web tier mirror"),
        tags: &[("Team", "platform")],
    },
    CreateRow {
        case: "a session at the lowest packet length and the highest session number",
        target: TargetKind::Interface,
        session_number: 32_766,
        packet_length: Some(1),
        virtual_network_id: Some(16_777_215),
        description: Some("mirror: ingress/egress — grün"),
        tags: &[("Team", "network"), ("Env", "staging")],
    },
    CreateRow {
        case: "a session at the highest packet length, to a load balancer target",
        target: TargetKind::LoadBalancer,
        session_number: 3,
        packet_length: Some(8_500),
        virtual_network_id: Some(0),
        description: Some("d"),
        tags: &[("Only", "")],
    },
    CreateRow {
        case: "a session to an interface target with a packet length and no text",
        target: TargetKind::Interface,
        session_number: 4,
        packet_length: Some(100),
        virtual_network_id: None,
        description: None,
        tags: &[],
    },
];

/// The target id `row` mirrors to, among the account's resources in `built`.
fn target_id(row: &CreateRow, built: &Fixtures) -> String {
    match row.target {
        TargetKind::Interface => built.interface_target_id.clone(),
        TargetKind::LoadBalancer => built.load_balancer_target_id.clone(),
    }
}

/// The form parameters one [`CreateRow`] is sent as.
fn create_params(row: &CreateRow, built: &Fixtures) -> Params {
    let mut params = vec![
        p("NetworkInterfaceId", built.network_interface_id.clone()),
        p("TrafficMirrorFilterId", built.traffic_mirror_filter_id.clone()),
        p("TrafficMirrorTargetId", target_id(row, built)),
        p("SessionNumber", row.session_number.to_string()),
    ];
    if let Some(packet_length) = row.packet_length {
        params.push(p("PacketLength", packet_length.to_string()));
    }
    if let Some(virtual_network_id) = row.virtual_network_id {
        params.push(p("VirtualNetworkId", virtual_network_id.to_string()));
    }
    if let Some(description) = row.description {
        params.push(p("Description", description));
    }
    if !row.tags.is_empty() {
        params.push(p(
            "TagSpecification.1.ResourceType",
            TRAFFIC_MIRROR_SESSION_TAGGABLE_RESOURCE,
        ));
        for (index, (key, value)) in row.tags.iter().enumerate() {
            params.push(p(&format!("TagSpecification.1.Tag.{}.Key", index + 1), *key));
            params.push(p(
                &format!("TagSpecification.1.Tag.{}.Value", index + 1),
                *value,
            ));
        }
    }
    params
}

/// The `PacketLength` the row's session is reported with: the one it named, or for a load
/// balancer target the default the member documents, and nothing at all otherwise.
fn expected_packet_length(row: &CreateRow) -> Option<String> {
    match (row.packet_length, row.target) {
        (Some(packet_length), _) => Some(packet_length.to_string()),
        (None, TargetKind::LoadBalancer) => {
            Some(NETWORK_LOAD_BALANCER_DEFAULT_PACKET_LENGTH.to_string())
        }
        (None, TargetKind::Interface) => None,
    }
}

/// RULE 1, 4 and 5: the create mints a session from the resources the request names, carries
/// every member it set, derives the owner and an identifier for a request that named none,
/// and `DescribeTrafficMirrorSessions` reports the same record member for member.
#[tokio::test]
async fn every_session_is_created_and_read_back() {
    let (router, engine) = super::ec2_query::emulator();
    let built = fixtures(&router, &engine, "the create table").await;

    let mut minted: Vec<String> = Vec::new();
    let mut virtual_network_ids: Vec<String> = Vec::new();
    for row in CREATE_ROWS {
        let case = row.case;
        let (status, body) = create(&router, &create_params(row, &built)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateTrafficMirrorSession, got {status} \
             with body {body}"
        );
        let created = one(case, &body);
        let id = created.id.clone().unwrap_or_default();
        assert!(
            is_long_id(TRAFFIC_MIRROR_SESSION_ID_PREFIX, &id),
            "case {case:?}: expected a minted Traffic Mirror session id, got {id:?}"
        );

        let virtual_network_id = created.virtual_network_id.clone().unwrap_or_else(|| {
            panic!(
                "case {case:?}: a session carries a virtual network id whether the request \
                 named one or not, got {body}"
            )
        });
        let expected = Reported {
            id: Some(id.clone()),
            traffic_mirror_target_id: Some(target_id(row, &built)),
            traffic_mirror_filter_id: Some(built.traffic_mirror_filter_id.clone()),
            network_interface_id: Some(built.network_interface_id.clone()),
            owner_id: Some(engine::emulator_account_id().to_string()),
            packet_length: expected_packet_length(row),
            session_number: Some(row.session_number.to_string()),
            virtual_network_id: Some(virtual_network_id.clone()),
            description: row.description.map(str::to_string),
            tags: row
                .tags
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect(),
        };
        assert_eq!(
            created, expected,
            "case {case:?}: the create must report the session the request describes, got {body}"
        );

        // RULE 4: the identifier a request names is kept exactly, and one the service chose
        // is inside the width a VXLAN header carries and is not one another session uses.
        let (lowest, widest) = TRAFFIC_MIRROR_VIRTUAL_NETWORK_ID_RANGE;
        match row.virtual_network_id {
            Some(named) => assert_eq!(
                virtual_network_id,
                named.to_string(),
                "case {case:?}: the virtual network id the request named must be kept, got \
                 {virtual_network_id:?}"
            ),
            None => {
                let chosen: i32 = virtual_network_id.parse().unwrap_or_else(|_| {
                    panic!(
                        "case {case:?}: expected a numeric virtual network id, got \
                         {virtual_network_id:?}"
                    )
                });
                assert!(
                    (lowest..=widest).contains(&chosen),
                    "case {case:?}: expected a virtual network id in {lowest}..={widest}, got \
                     {chosen}"
                );
            }
        }
        assert!(
            !virtual_network_ids.contains(&virtual_network_id),
            "case {case:?}: a virtual network id is unique across the account's sessions, \
             {virtual_network_id:?} is already held by one of {virtual_network_ids:?}"
        );
        virtual_network_ids.push(virtual_network_id);

        // RULE 1: the record read back through the describe is the record the create
        // reported.
        let (status, body) = describe(&router, &[p("TrafficMirrorSessionId.1", id.clone())]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeTrafficMirrorSessions, got {status} \
             with body {body}"
        );
        assert_eq!(
            one(case, &body),
            expected,
            "case {case:?}: the describe must report the session the create stored, got {body}"
        );

        minted.push(id);
    }

    assert_eq!(
        stored_ids(&engine),
        {
            let mut sorted = minted.clone();
            sorted.sort();
            sorted
        },
        "the store must hold exactly the sessions the table created, it holds {:?}",
        stored_ids(&engine)
    );
}

/// RULE 2: every bounded member is accepted AT each published bound and refused one past it,
/// with EC2's wording for a value its parameter layer cannot take and nothing stored.
///
/// The rows are each bounded member at its lowest and highest published value, and one below
/// and one above each of those — the published bounds themselves are driven by
/// [`every_session_is_created_and_read_back`], whose rows sit at them.
#[tokio::test]
async fn every_bounded_member_is_held_to_its_published_range() {
    let (router, engine) = super::ec2_query::emulator();
    let built = fixtures(&router, &engine, "the range table").await;

    let (lowest_session, highest_session) = TRAFFIC_MIRROR_SESSION_NUMBER_RANGE;
    let (lowest_length, highest_length) = TRAFFIC_MIRROR_PACKET_LENGTH_RANGE;
    let (lowest_network, widest_network) = TRAFFIC_MIRROR_VIRTUAL_NETWORK_ID_RANGE;

    // (case, member, the value one past the bound, the bounds the member publishes)
    let rows: Vec<(String, &str, i64, (i32, i32))> = vec![
        (
            "a session number below the lowest".to_string(),
            "SessionNumber",
            i64::from(lowest_session) - 1,
            TRAFFIC_MIRROR_SESSION_NUMBER_RANGE,
        ),
        (
            "a session number above the highest".to_string(),
            "SessionNumber",
            i64::from(highest_session) + 1,
            TRAFFIC_MIRROR_SESSION_NUMBER_RANGE,
        ),
        (
            "a packet length below the lowest".to_string(),
            "PacketLength",
            i64::from(lowest_length) - 1,
            TRAFFIC_MIRROR_PACKET_LENGTH_RANGE,
        ),
        (
            "a packet length above the highest".to_string(),
            "PacketLength",
            i64::from(highest_length) + 1,
            TRAFFIC_MIRROR_PACKET_LENGTH_RANGE,
        ),
        (
            "a virtual network id below the lowest".to_string(),
            "VirtualNetworkId",
            i64::from(lowest_network) - 1,
            TRAFFIC_MIRROR_VIRTUAL_NETWORK_ID_RANGE,
        ),
        (
            "a virtual network id wider than a VXLAN header carries".to_string(),
            "VirtualNetworkId",
            i64::from(widest_network) + 1,
            TRAFFIC_MIRROR_VIRTUAL_NETWORK_ID_RANGE,
        ),
    ];

    for (case, member, value, (min, max)) in rows {
        let mut params = create_params(&CREATE_ROWS[0], &built);
        params.retain(|(key, _)| key != member);
        params.push(p(member, value.to_string()));

        let before = stored_ids(&engine);
        let (status, body) = create(&router, &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            INVALID_PARAMETER_VALUE,
            "case {case:?}: expected {INVALID_PARAMETER_VALUE}, got {body}"
        );
        // The member is named as the wire names it, which is its lowerCamelCase spelling.
        let wire_member = format!(
            "{first}{rest}",
            first = member[..1].to_lowercase(),
            rest = &member[1..],
        );
        assert_eq!(
            error_message(&body),
            format!(
                "Value ({value}) for parameter {wire_member} is invalid. Valid values are from \
                 {min} to {max}."
            ),
            "case {case:?}: expected the message to name the value, the member and the bounds, \
             got {body}"
        );
        assert_eq!(
            stored_ids(&engine),
            before,
            "case {case:?}: a refused create must store nothing, the store holds {:?}",
            stored_ids(&engine)
        );
    }
}

/// RULE 3: the source interface, the filter and the target each have to be a resource this
/// account holds, and an unresolvable one is refused with the code its own id is resolved
/// under — never stored as a session that mirrors from or to nothing.
///
/// The rows differ along WHICH named resource cannot be resolved and along the SHAPE of the
/// value: the long 17-hex form this store mints, the short 8-hex form of EC2's older ids, the
/// id of another resource kind, and text that is no network interface id at all.
#[tokio::test]
async fn every_named_resource_must_be_one_the_account_holds() {
    let (router, engine) = super::ec2_query::emulator();
    let built = fixtures(&router, &engine, "the resolution table").await;

    let rows: Vec<(&str, &str, &str, &str)> = vec![
        (
            "a source interface of the minted form that names none",
            "NetworkInterfaceId",
            "eni-0123456789abcdef0",
            INVALID_NETWORK_INTERFACE_ID_NOT_FOUND,
        ),
        (
            "a source interface in EC2's short id form that names none",
            "NetworkInterfaceId",
            "eni-00000000",
            INVALID_NETWORK_INTERFACE_ID_NOT_FOUND,
        ),
        (
            "a source interface that is not a network interface id at all",
            "NetworkInterfaceId",
            "not-an-id-at-all",
            INVALID_NETWORK_INTERFACE_ID_MALFORMED,
        ),
        (
            "a source interface naming another resource kind",
            "NetworkInterfaceId",
            "vpc-0123456789abcdef0",
            INVALID_NETWORK_INTERFACE_ID_MALFORMED,
        ),
        (
            "a filter of the minted form that names none",
            "TrafficMirrorFilterId",
            "tmf-0123456789abcdef0",
            INVALID_TRAFFIC_MIRROR_FILTER_ID_NOT_FOUND,
        ),
        (
            "a filter that is no id at all",
            "TrafficMirrorFilterId",
            "not-an-id-at-all",
            INVALID_TRAFFIC_MIRROR_FILTER_ID_NOT_FOUND,
        ),
        (
            "a target of the minted form that names none",
            "TrafficMirrorTargetId",
            "tmt-0123456789abcdef0",
            INVALID_TRAFFIC_MIRROR_TARGET_ID_NOT_FOUND,
        ),
        (
            "a target naming a filter instead",
            "TrafficMirrorTargetId",
            "tmf-0123456789abcdef0",
            INVALID_TRAFFIC_MIRROR_TARGET_ID_NOT_FOUND,
        ),
    ];

    for (case, member, value, code) in rows {
        let mut params = create_params(&CREATE_ROWS[0], &built);
        params.retain(|(key, _)| key != member);
        params.push(p(member, value));

        let before = stored_ids(&engine);
        let (status, body) = create(&router, &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            code,
            "case {case:?}: expected {code}, got {body}"
        );
        assert!(
            error_message(&body).contains(value),
            "case {case:?}: expected the message to name the id the request gave, got {}",
            error_message(&body)
        );
        assert_eq!(
            stored_ids(&engine),
            before,
            "case {case:?}: a refused create must store nothing, the store holds {:?}",
            stored_ids(&engine)
        );
    }
}

/// RULE 6: one `ClientToken` names one request — a repeat reports the session the first
/// create minted and stores nothing, while two creates carrying no token mint two sessions.
#[tokio::test]
async fn one_client_token_names_one_session() {
    let (router, engine) = super::ec2_query::emulator();
    let built = fixtures(&router, &engine, "the idempotency table").await;

    // The rows differ along which members the request carries, so idempotency is a fact of
    // the token rather than of one shape of session.
    for (n, row) in CREATE_ROWS.iter().enumerate() {
        let case = row.case;
        let mut params = create_params(row, &built);
        params.push(p("ClientToken", format!("token-{n}")));

        let (status, body) = create(&router, &params).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with body {body}"
        );
        let first = one(case, &body);
        assert_eq!(
            super::ec2_query::element(&body, "clientToken").as_deref(),
            Some(format!("token-{n}").as_str()),
            "case {case:?}: the create must echo the token the request carried, got {body}"
        );
        let held = stored_ids(&engine);

        let (status, body) = create(&router, &params).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from the replay, got {status} with body {body}"
        );
        assert_eq!(
            one(case, &body),
            first,
            "case {case:?}: the replay must report the session the token names, got {body}"
        );
        assert_eq!(
            stored_ids(&engine),
            held,
            "case {case:?}: a replay must store nothing, the store holds {:?}",
            stored_ids(&engine)
        );
    }

    // No token at all is never a replay: the same request twice mints two sessions.
    let params = create_params(&CREATE_ROWS[0], &built);
    let held = stored_ids(&engine).len();
    for round in 1..=2 {
        let (status, body) = create(&router, &params).await;
        assert_eq!(
            status, 200,
            "case \"no token, round {round}\": expected HTTP 200, got {status} with body {body}"
        );
    }
    assert_eq!(
        stored_ids(&engine).len(),
        held + 2,
        "a create carrying no token is never a replay, the store holds {:?}",
        stored_ids(&engine)
    );
}

/// RULE 6, the other side of a token: a token names ONE request, so a repeat of it describing
/// a DIFFERENT session is refused `IdempotentParameterMismatch` and stores nothing.
///
/// The rows differ along WHICH part of the request the repeat changed: the target it mirrors
/// to, the session number, the packet length, the virtual network id, the description, a
/// tag's value and an added tag. The last row changes nothing but the ORDER two tags are
/// listed in, which is not a difference: "Tag keys must be unique per resource", so the set
/// is the parameter.
#[tokio::test]
async fn a_token_reused_for_another_request_is_refused() {
    let (router, engine) = super::ec2_query::emulator();
    let built = fixtures(&router, &engine, "the mismatch table").await;

    // The request the token names: an interface target with every member set and two tags.
    let naming_request = |token: &str| -> Params {
        vec![
            p("NetworkInterfaceId", built.network_interface_id.clone()),
            p("TrafficMirrorFilterId", built.traffic_mirror_filter_id.clone()),
            p("TrafficMirrorTargetId", built.interface_target_id.clone()),
            p("SessionNumber", "7"),
            p("PacketLength", "200"),
            p("VirtualNetworkId", "4242"),
            p("Description", "the request the token names"),
            p(
                "TagSpecification.1.ResourceType",
                TRAFFIC_MIRROR_SESSION_TAGGABLE_RESOURCE,
            ),
            p("TagSpecification.1.Tag.1.Key", "Team"),
            p("TagSpecification.1.Tag.1.Value", "network"),
            p("TagSpecification.1.Tag.2.Key", "Env"),
            p("TagSpecification.1.Tag.2.Value", "staging"),
            p("ClientToken", token.to_string()),
        ]
    };

    /// `params` with `member` replaced by `value`.
    fn changed(params: &[(String, String)], member: &str, value: &str) -> Params {
        let mut changed: Params = params
            .iter()
            .filter(|(key, _)| key != member)
            .cloned()
            .collect();
        changed.push(p(member, value));
        changed
    }

    let token = "the-one-request";
    let (status, body) = create(&router, &naming_request(token)).await;
    assert_eq!(
        status, 200,
        "the request the token names must succeed, got {status} with body {body}"
    );
    let named = one("the request the token names", &body);
    let held = stored_ids(&engine);

    let rows: Vec<(&str, Params)> = vec![
        (
            "another target",
            changed(
                &naming_request(token),
                "TrafficMirrorTargetId",
                &built.load_balancer_target_id,
            ),
        ),
        (
            "another session number",
            changed(&naming_request(token), "SessionNumber", "8"),
        ),
        (
            "another packet length",
            changed(&naming_request(token), "PacketLength", "201"),
        ),
        (
            "another virtual network id",
            changed(&naming_request(token), "VirtualNetworkId", "4243"),
        ),
        (
            "another description",
            changed(&naming_request(token), "Description", "another request"),
        ),
        (
            "another tag value",
            changed(&naming_request(token), "TagSpecification.1.Tag.2.Value", "prod"),
        ),
        (
            "one more tag",
            {
                let mut params = naming_request(token);
                params.push(p("TagSpecification.1.Tag.3.Key", "Owner"));
                params.push(p("TagSpecification.1.Tag.3.Value", "network"));
                params
            },
        ),
    ];

    for (case, params) in rows {
        let (status, body) = create(&router, &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            IDEMPOTENT_PARAMETER_MISMATCH,
            "case {case:?}: expected {IDEMPOTENT_PARAMETER_MISMATCH}, got {body}"
        );
        assert_eq!(
            error_message(&body),
            IDEMPOTENT_PARAMETER_MISMATCH_MESSAGE,
            "case {case:?}: expected EC2's own sentence for a reused token, got {body}"
        );
        assert_eq!(
            stored_ids(&engine),
            held,
            "case {case:?}: a refused replay must store nothing, the store holds {:?}",
            stored_ids(&engine)
        );
    }

    // The same parameters with the two tags listed the other way round are the same request.
    let mut reordered = naming_request(token);
    reordered.retain(|(key, _)| !key.starts_with("TagSpecification.1.Tag."));
    reordered.push(p("TagSpecification.1.Tag.1.Key", "Env"));
    reordered.push(p("TagSpecification.1.Tag.1.Value", "staging"));
    reordered.push(p("TagSpecification.1.Tag.2.Key", "Team"));
    reordered.push(p("TagSpecification.1.Tag.2.Value", "network"));
    let (status, body) = create(&router, &reordered).await;
    assert_eq!(
        status, 200,
        "tags listed in another order are the same parameters, got {status} with body {body}"
    );
    assert_eq!(
        one("tags in the other order", &body).id,
        named.id,
        "a replay differing only in tag order must report the session the token named: {body}"
    );
}

/// RULE 6, the probe: `DryRun` asks whether the create WOULD have worked, so it is answered
/// `DryRunOperation` (HTTP 412) with nothing stored, and a dry run that would have been
/// refused is refused with that reason instead.
#[tokio::test]
async fn a_dry_run_creates_nothing() {
    let (router, engine) = super::ec2_query::emulator();
    let built = fixtures(&router, &engine, "the dry run table").await;

    for row in CREATE_ROWS {
        let case = row.case;
        let before = stored_ids(&engine);
        let mut params = create_params(row, &built);
        params.push(p("DryRun", "true"));

        let (status, body) = create(&router, &params).await;
        assert_eq!(
            status, 412,
            "case {case:?}: expected HTTP 412 for a dry run, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "DryRunOperation",
            "case {case:?}: expected DryRunOperation, got {body}"
        );
        assert_eq!(
            stored_ids(&engine),
            before,
            "case {case:?}: a dry run must store nothing, the store holds {:?}",
            stored_ids(&engine)
        );
    }

    // A dry run of a request that would have been refused is refused with that reason: the
    // flag asks whether the call WOULD have worked.
    let mut params = create_params(&CREATE_ROWS[0], &built);
    params.retain(|(key, _)| key != "SessionNumber");
    params.push(p("SessionNumber", "0"));
    params.push(p("DryRun", "true"));
    let (status, body) = create(&router, &params).await;
    assert_eq!(
        status, 400,
        "case \"a dry run that would have been refused\": expected HTTP 400, got {status} with \
         body {body}"
    );
    assert_eq!(
        error_code(&body),
        INVALID_PARAMETER_VALUE,
        "case \"a dry run that would have been refused\": expected {INVALID_PARAMETER_VALUE}, \
         got {body}"
    );
}
