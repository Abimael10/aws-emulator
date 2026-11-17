// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `DeleteTransitGatewayConnect`, driven over the wire the service speaks —
//! `ec2Query`, an `application/x-www-form-urlencoded` POST answered with XML (README.md /
//! OPERATIONS.md "Wire protocol") — together with the `CreateTransitGatewayConnect` that puts the
//! attachment there, the Connect peers that peer over it, and the
//! `DescribeTransitGatewayConnects` that reads it back.
//!
//! The rule under test: a delete that would take a resource away from a dependent the service
//! still holds is refused for the state that dependent leaves it in, naming the dependents, and
//! writes nothing — "you must first delete any Connect peers for the attachment"
//! (`DeleteTransitGatewayConnect`) — and once no dependent is left the delete records itself on
//! the STORED attachment the request names: the transient delete state of the attachment's own
//! catalog now, with the clock owing it the terminal one, every other member exactly as it was
//! stored. An id of the wrong form is refused as malformed before anything is resolved, an id
//! this account holds no Connect attachment under (one naming an attachment of another kind
//! included) as that resource's own not-found, an attachment whose state admits no delete for
//! that state, and a dry run reports the outcome without recording it.
//!
//! Each row of the refusal table is a distinct input along what the rule generalises over: which
//! non-deleted state of the peer's own catalog the dependent is in, and how many dependents the
//! attachment carries — including an attachment carrying both live peers and one whose deletion
//! has settled, which the refusal must not name.

use std::sync::Arc;
use std::time::Duration;

use ec2::ops::types::TransitGatewayVpcAttachment;
use ec2::state::{
    CONNECT_PEER_STATE_AVAILABLE, CONNECT_PEER_STATE_DELETED, CONNECT_PEER_STATE_DELETING,
    CONNECT_PEER_STATE_PENDING, ComAmazonawsEc2Amazonec2State as Ec2State, INCORRECT_STATE,
    INVALID_TRANSIT_GATEWAY_ATTACHMENT_ID_MALFORMED, INVALID_TRANSIT_GATEWAY_ATTACHMENT_ID_MESSAGE,
    INVALID_TRANSIT_GATEWAY_ATTACHMENT_ID_NOT_FOUND, PROTOCOL_VALUE_GRE,
    RESOURCE_TYPE_TRANSIT_GATEWAY_ATTACHMENT, TRANSIT_GATEWAY_ATTACHMENT_ID_PREFIX,
    TRANSIT_GATEWAY_ATTACHMENT_STATE_AVAILABLE, TRANSIT_GATEWAY_ATTACHMENT_STATE_DELETED,
    TRANSIT_GATEWAY_ATTACHMENT_STATE_DELETING, TRANSIT_GATEWAY_ATTACHMENT_STATE_PENDING,
    TransitGatewayAttachmentView,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, error_message, indexed};
use super::wire::{Param, child, child_text, document, param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The HTTP status EC2 answers `DryRunOperation` with.
const DRY_RUN_STATUS: u16 = 412;

/// The IPv4 CIDR block the fixture's transit gateway carries, which a Connect peer's unnamed
/// `TransitGatewayAddress` is assigned out of.
const GATEWAY_IPV4_BLOCK: &str = "10.100.0.0/24";

/// Long enough past the span a clock-owed move takes that it is certainly overdue: the deadline
/// the polls below give up at, generous because what is under test is the state a resource
/// reaches rather than how fast it reaches it.
const MOVE_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll below waits between two reads of a resource's state.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The emulator, one transit gateway with a CIDR block a Connect peer can be addressed out of,
/// and one VPC attachment on it in service for a Connect attachment to tunnel over.
struct Fixture {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
    /// The transport attachment every Connect attachment of this suite is created from, which is
    /// also an id of an attachment of ANOTHER kind for the refusal table.
    transport_id: String,
}

impl Fixture {
    /// A fresh emulator holding nothing but the gateway and the transport attachment.
    ///
    /// The transit gateway is created over the wire, so the gateway a Connect attachment reports
    /// is the one that operation really stored; the transport VPC attachment is seeded straight
    /// into the store in the one state a Connect create admits, the way the Connect create and
    /// Connect peer suites beside this one seed theirs.
    async fn new() -> Self {
        let (router, engine) = super::ec2_query::emulator();
        let (status, body) = call(
            &router,
            REGION,
            "CreateTransitGateway",
            &[param(
                "Options.TransitGatewayCidrBlocks.1",
                GATEWAY_IPV4_BLOCK,
            )],
        )
        .await;
        assert_eq!(
            status, 200,
            "the fixture's transit gateway must be created, got {status} with body {body}"
        );
        let transit_gateway_id = element(&body, "transitGatewayId")
            .unwrap_or_else(|| panic!("no transitGatewayId in {body}"));
        let transport_id = engine
            .state::<Ec2State>()
            .lock()
            .insert_transit_gateway_attachment(TransitGatewayAttachmentView::Vpc(
                TransitGatewayVpcAttachment {
                    transit_gateway_id: Some(transit_gateway_id),
                    vpc_id: Some("vpc-0123456789abcdef0".to_string()),
                    state: Some(TRANSIT_GATEWAY_ATTACHMENT_STATE_AVAILABLE.to_string()),
                    ..Default::default()
                },
            ))
            .expect("the attachment store has room for one transport attachment");
        Self {
            router,
            engine,
            transport_id,
        }
    }

    /// Create one Connect attachment over the transport attachment, with `tags` applied, and
    /// answer it as the create reported it.
    async fn create_connect(&self, case: &str, tags: &[(&str, &str)]) -> Connect {
        let mut params = vec![
            param("TransportTransitGatewayAttachmentId", &self.transport_id),
            param("Options.Protocol", PROTOCOL_VALUE_GRE),
        ];
        if !tags.is_empty() {
            params.push(param(
                "TagSpecification.1.ResourceType",
                RESOURCE_TYPE_TRANSIT_GATEWAY_ATTACHMENT,
            ));
            for (position, (key, value)) in tags.iter().enumerate() {
                let index = position + 1;
                params.push(param(&format!("TagSpecification.1.Tag.{index}.Key"), key));
                params.push(param(
                    &format!("TagSpecification.1.Tag.{index}.Value"),
                    value,
                ));
            }
        }
        let (status, body) = call(
            &self.router,
            REGION,
            "CreateTransitGatewayConnect",
            &params,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the Connect attachment must be created, got {status} with body {body}"
        );
        connect_of(case, &body)
    }

    /// Create one Connect peer over `attachment_id`, peering over `inside` from `peer_address`,
    /// and answer the id the store minted for it.
    async fn create_peer(
        &self,
        case: &str,
        attachment_id: &str,
        inside: &str,
        peer_address: &str,
    ) -> String {
        let mut params = vec![
            param("TransitGatewayAttachmentId", attachment_id),
            param("PeerAddress", peer_address),
        ];
        params.extend(indexed("InsideCidrBlocks", &[inside]));
        let (status, body) = call(
            &self.router,
            REGION,
            "CreateTransitGatewayConnectPeer",
            &params,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the Connect peer must be created, got {status} with body {body}"
        );
        element(&body, "transitGatewayConnectPeerId")
            .unwrap_or_else(|| panic!("case {case:?}: no transitGatewayConnectPeerId in {body}"))
    }

    /// Put the stored Connect peer `peer_id` into `state` with nothing owed to the clock, so a
    /// test of a peer in one state of its catalog does not depend on when a clock-owed move falls
    /// due. The peer itself was created by the operation that creates one.
    fn force_peer_state(&self, case: &str, peer_id: &str, state: &str) {
        let handle = self.engine.state::<Ec2State>();
        let mut store = handle.lock();
        let record = store
            .transit_gateway_connect_peers
            .get_mut(peer_id)
            .unwrap_or_else(|| panic!("case {case:?}: the store must hold the peer {peer_id}"));
        record.view.state = Some(state.to_string());
        record.pending = None;
    }

    /// The state the STORE holds for the Connect peer `peer_id`.
    fn stored_peer_state(&self, peer_id: &str) -> Option<String> {
        self.engine
            .state::<Ec2State>()
            .lock()
            .transit_gateway_connect_peers
            .get(peer_id)
            .and_then(|record| record.view.state.clone())
    }

    /// The state the STORE holds for the attachment `attachment_id`.
    fn stored_attachment_state(&self, attachment_id: &str) -> Option<String> {
        self.engine
            .state::<Ec2State>()
            .lock()
            .transit_gateway_attachments
            .get(attachment_id)
            .and_then(|record| record.view.state().map(str::to_string))
    }
}

/// Every member of a reported Connect attachment EXCEPT its state — what a delete must leave
/// exactly as the store held it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Rest {
    attachment_id: String,
    transport_id: String,
    transit_gateway_id: String,
    creation_time: String,
    protocol: String,
    tags: Vec<(String, String)>,
}

/// One reported Connect attachment: the state a call answers, beside everything else it reports.
#[derive(Debug, Clone)]
struct Connect {
    state: String,
    rest: Rest,
}

/// The one Connect attachment a response carries, read out of the XML document rather than by
/// string surgery.
fn connect_of(case: &str, body: &str) -> Connect {
    let parsed = document(case, body);
    let item = parsed
        .descendants()
        .find(|node| {
            node.is_element() && child_text(*node, "transportTransitGatewayAttachmentId").is_some()
        })
        .unwrap_or_else(|| panic!("case {case:?}: no Connect attachment in {body}"));
    Connect {
        state: child_text(item, "state").unwrap_or_default(),
        rest: Rest {
            attachment_id: child_text(item, "transitGatewayAttachmentId").unwrap_or_default(),
            transport_id: child_text(item, "transportTransitGatewayAttachmentId")
                .unwrap_or_default(),
            transit_gateway_id: child_text(item, "transitGatewayId").unwrap_or_default(),
            creation_time: child_text(item, "creationTime").unwrap_or_default(),
            protocol: child(item, "options")
                .and_then(|options| child_text(options, "protocol"))
                .unwrap_or_default(),
            tags: child(item, "tagSet")
                .map(|tag_set| {
                    tag_set
                        .children()
                        .filter(|node| node.is_element() && node.tag_name().name() == "item")
                        .map(|tag| {
                            (
                                child_text(tag, "key").unwrap_or_default(),
                                child_text(tag, "value").unwrap_or_default(),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
        },
    }
}

/// One `DeleteTransitGatewayConnect` call.
async fn delete_connect(router: &MultiServiceRouter, params: &[Param]) -> (u16, String) {
    call(router, REGION, "DeleteTransitGatewayConnect", params).await
}

/// The Connect attachment `attachment_id` as `DescribeTransitGatewayConnects` reports it now.
async fn described(case: &str, router: &MultiServiceRouter, attachment_id: &str) -> Connect {
    let (status, body) = call(
        router,
        REGION,
        "DescribeTransitGatewayConnects",
        &indexed("TransitGatewayAttachmentIds", &[attachment_id]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the describe of {attachment_id} must succeed, got {status} with body \
         {body}"
    );
    connect_of(case, &body)
}

/// Look at the attachment AGAIN until the describe reports it in `wanted`, or a deadline far past
/// the span the clock owes a move passes.
///
/// Polls for that STATE rather than waiting out a span, so neither a loaded nor a fast host can be
/// the reason this reads the attachment before the move has been applied.
async fn polled_state(
    case: &str,
    router: &MultiServiceRouter,
    attachment_id: &str,
    wanted: &str,
) -> Connect {
    let deadline = tokio::time::Instant::now() + MOVE_DEADLINE;
    loop {
        let connect = described(case, router, attachment_id).await;
        if connect.state == wanted {
            return connect;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: the Connect attachment {attachment_id} must reach {wanted:?}, last \
             read {actual:?}",
            actual = connect.state,
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The sentence EC2 refuses a delete of a Connect attachment that still has Connect peers with.
fn peers_in_the_way(attachment_id: &str, peer_ids: &[&str]) -> String {
    format!(
        "Connect {attachment_id} has non-deleted connect peers: {peers}.",
        peers = peer_ids.join(", "),
    )
}

/// One peer the refusal table puts on the attachment: the state of the peer's own catalog it is
/// in when the delete arrives, and whether the refusal must name it.
struct PeerSpec {
    /// The state to leave the peer in, or `None` to leave it exactly as its create stored it.
    forced: Option<&'static str>,
    /// Whether the refusal names this peer: every state but the terminal `deleted` does.
    in_the_way: bool,
}

/// One row of the refusal table.
struct Blocked {
    name: &'static str,
    peers: Vec<PeerSpec>,
}

#[tokio::test]
async fn a_connect_attachment_whose_peers_are_not_deleted_cannot_be_deleted() {
    // Each row differs along what the rule generalises over: which non-deleted state of
    // `TransitGatewayConnectPeerState` the dependent is in, how many dependents there are, and
    // whether a dependent whose deletion HAS settled is among them — one that carries nothing any
    // more, so it is not in the way and is not named.
    let rows = [
        Blocked {
            name: "one peer exactly as its create stored it",
            peers: vec![PeerSpec {
                forced: None,
                in_the_way: true,
            }],
        },
        Blocked {
            name: "one peer in service",
            peers: vec![PeerSpec {
                forced: Some(CONNECT_PEER_STATE_AVAILABLE),
                in_the_way: true,
            }],
        },
        Blocked {
            name: "one peer whose own deletion has not settled",
            peers: vec![PeerSpec {
                forced: Some(CONNECT_PEER_STATE_DELETING),
                in_the_way: true,
            }],
        },
        Blocked {
            name: "two peers, one pending and one in service",
            peers: vec![
                PeerSpec {
                    forced: Some(CONNECT_PEER_STATE_PENDING),
                    in_the_way: true,
                },
                PeerSpec {
                    forced: Some(CONNECT_PEER_STATE_AVAILABLE),
                    in_the_way: true,
                },
            ],
        },
        Blocked {
            name: "three peers, the middle one already deleted",
            peers: vec![
                PeerSpec {
                    forced: Some(CONNECT_PEER_STATE_AVAILABLE),
                    in_the_way: true,
                },
                PeerSpec {
                    forced: Some(CONNECT_PEER_STATE_DELETED),
                    in_the_way: false,
                },
                PeerSpec {
                    forced: Some(CONNECT_PEER_STATE_AVAILABLE),
                    in_the_way: true,
                },
            ],
        },
    ];

    for row in rows {
        let name = row.name;
        let fixture = Fixture::new().await;
        let attachment = fixture.create_connect(name, &[]).await;
        let attachment_id = attachment.rest.attachment_id.clone();
        let before = polled_state(
            name,
            &fixture.router,
            &attachment_id,
            TRANSIT_GATEWAY_ATTACHMENT_STATE_AVAILABLE,
        )
        .await;

        let mut peers = Vec::new();
        for (position, spec) in row.peers.iter().enumerate() {
            let inside = format!("169.254.{block}.0/29", block = 6 + position);
            let peer_address = format!("192.0.2.{address}", address = 10 + position);
            let peer_id = fixture
                .create_peer(name, &attachment_id, &inside, &peer_address)
                .await;
            if let Some(state) = spec.forced {
                fixture.force_peer_state(name, &peer_id, state);
            }
            peers.push((peer_id, spec.in_the_way));
        }
        let named: Vec<&str> = peers
            .iter()
            .filter(|(_, in_the_way)| *in_the_way)
            .map(|(peer_id, _)| peer_id.as_str())
            .collect();

        let (status, body) = delete_connect(
            &fixture.router,
            &[param("TransitGatewayAttachmentId", &attachment_id)],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {name:?}: expected HTTP 400 while a peer is in the way, got {status} with body \
             {body}"
        );
        assert_eq!(
            error_code(&body),
            INCORRECT_STATE,
            "case {name:?}: expected {INCORRECT_STATE}, got body {body}"
        );
        let expected = peers_in_the_way(&attachment_id, &named);
        assert_eq!(
            error_message(&body),
            expected,
            "case {name:?}: expected the sentence {expected:?}, got body {body}"
        );

        // The refusal wrote nothing: the attachment is still in service and every peer is still
        // in the state the row put it in.
        let after = described(name, &fixture.router, &attachment_id).await;
        assert_eq!(
            after.state, TRANSIT_GATEWAY_ATTACHMENT_STATE_AVAILABLE,
            "case {name:?}: a refused delete must leave the attachment in service, got \
             {actual:?}",
            actual = after.state,
        );
        assert_eq!(
            after.rest, before.rest,
            "case {name:?}: a refused delete must leave the attachment as it was, got {actual:?}",
            actual = after.rest,
        );
        for (position, (peer_id, _)) in peers.iter().enumerate() {
            let wanted = row.peers[position]
                .forced
                .unwrap_or(CONNECT_PEER_STATE_PENDING);
            let held = fixture.stored_peer_state(peer_id);
            assert!(
                held.as_deref() == Some(wanted)
                    // A peer left exactly as its create stored it is owed `available` by the
                    // clock, so either state of that move is the one the store holds.
                    || (row.peers[position].forced.is_none()
                        && held.as_deref() == Some(CONNECT_PEER_STATE_AVAILABLE)),
                "case {name:?}: the peer {peer_id} must be untouched by the refusal, got \
                 {held:?}"
            );
        }
    }
}

#[tokio::test]
async fn the_delete_records_the_deletion_once_no_peer_is_left() {
    /// One row of the success table: whether the attachment ever carried a peer, and the tags it
    /// was created with.
    struct Case<'a> {
        name: &'a str,
        /// Whether a Connect peer is created over the attachment and deleted again before the
        /// attachment's own delete.
        had_a_peer: bool,
        tags: &'a [(&'a str, &'a str)],
    }
    // Each row differs along what the rule generalises over: whether a dependent was ever there
    // and has since settled its own deletion, and how many tags the attachment carries.
    let cases = [
        Case {
            name: "an attachment that never carried a peer",
            had_a_peer: false,
            tags: &[],
        },
        Case {
            name: "an attachment whose only peer has been deleted",
            had_a_peer: true,
            tags: &[],
        },
        Case {
            name: "a tagged attachment whose only peer has been deleted",
            had_a_peer: true,
            tags: &[("Name", "connect"), ("Team", "network")],
        },
    ];

    for case in cases {
        let name = case.name;
        let fixture = Fixture::new().await;
        let created = fixture.create_connect(name, case.tags).await;
        let attachment_id = created.rest.attachment_id.clone();
        assert!(
            attachment_id.starts_with(TRANSIT_GATEWAY_ATTACHMENT_ID_PREFIX),
            "case {name:?}: expected a transit gateway attachment id, got {attachment_id:?}"
        );
        assert_eq!(
            created.state, TRANSIT_GATEWAY_ATTACHMENT_STATE_PENDING,
            "case {name:?}: a created Connect attachment is answered in the state its create \
             stored, got {actual:?}",
            actual = created.state,
        );

        if case.had_a_peer {
            // A Connect peer can only be created on an attachment in service, so the clock-owed
            // move is polled for before the peer is created.
            polled_state(
                name,
                &fixture.router,
                &attachment_id,
                TRANSIT_GATEWAY_ATTACHMENT_STATE_AVAILABLE,
            )
            .await;
            let peer_id = fixture
                .create_peer(name, &attachment_id, "169.254.6.0/29", "192.0.2.10")
                .await;
            let (status, body) = call(
                &fixture.router,
                REGION,
                "DeleteTransitGatewayConnectPeer",
                &[param("TransitGatewayConnectPeerId", &peer_id)],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {name:?}: the peer's own delete must succeed, got {status} with body {body}"
            );
            // The peer's deletion is owed its terminal state by the clock: it is READ until the
            // store reports it, rather than waited out.
            let deadline = tokio::time::Instant::now() + MOVE_DEADLINE;
            while fixture.stored_peer_state(&peer_id).as_deref() != Some(CONNECT_PEER_STATE_DELETED)
            {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "case {name:?}: the peer {peer_id} must reach {CONNECT_PEER_STATE_DELETED:?}, \
                     last read {actual:?}",
                    actual = fixture.stored_peer_state(&peer_id),
                );
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        }

        let before = described(name, &fixture.router, &attachment_id).await;
        let (status, body) = delete_connect(
            &fixture.router,
            &[param("TransitGatewayAttachmentId", &attachment_id)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected HTTP 200 from the delete, got {status} with body {body}"
        );
        let deleted = connect_of(name, &body);
        assert_eq!(
            deleted.state, TRANSIT_GATEWAY_ATTACHMENT_STATE_DELETING,
            "case {name:?}: expected the delete to answer \
             {TRANSIT_GATEWAY_ATTACHMENT_STATE_DELETING:?}, got {actual:?}",
            actual = deleted.state,
        );
        // "Information about the deleted Connect attachment": the record as it was stored, with
        // only the state this call changes.
        assert_eq!(
            deleted.rest, before.rest,
            "case {name:?}: expected the delete to answer the stored attachment {expected:?}, got \
             {actual:?}",
            expected = before.rest,
            actual = deleted.rest,
        );
        assert_eq!(
            deleted.rest, created.rest,
            "case {name:?}: expected the delete to answer what the create stored {expected:?}, \
             got {actual:?}",
            expected = created.rest,
            actual = deleted.rest,
        );
        let read = described(name, &fixture.router, &attachment_id).await;
        assert_eq!(
            read.state, TRANSIT_GATEWAY_ATTACHMENT_STATE_DELETING,
            "case {name:?}: expected the describe to report \
             {TRANSIT_GATEWAY_ATTACHMENT_STATE_DELETING:?} right after the delete, got {actual:?}",
            actual = read.state,
        );
        assert_eq!(
            fixture.stored_attachment_state(&attachment_id).as_deref(),
            Some(TRANSIT_GATEWAY_ATTACHMENT_STATE_DELETING),
            "case {name:?}: the stored attachment must be the one the describe reported"
        );

        // Nothing is called to make it happen: the clock owes the terminal state, so the
        // attachment is READ until it reports it.
        let settled = polled_state(
            name,
            &fixture.router,
            &attachment_id,
            TRANSIT_GATEWAY_ATTACHMENT_STATE_DELETED,
        )
        .await;
        assert_eq!(
            settled.rest, before.rest,
            "case {name:?}: a settled deletion changes only the state, got {actual:?}",
            actual = settled.rest,
        );
    }
}

#[tokio::test]
async fn an_id_the_service_cannot_resolve_is_refused_and_records_nothing() {
    /// One refused delete.
    struct Refusal {
        label: String,
        attachment_id: String,
        code: &'static str,
        message: String,
    }

    let fixture = Fixture::new().await;
    let live = fixture
        .create_connect("the attachment a refused delete must not touch", &[])
        .await;
    let live_id = live.rest.attachment_id.clone();

    // Both published id forms are 8 or 17 lowercase hexadecimal digits after the kind's prefix
    // (`crate::state::prefixed_id_is_well_formed`), so each malformed row below is a value outside
    // that form — a bare prefix, the wrong digit count, a non-hexadecimal digit, an uppercase
    // suffix, another resource kind's prefix — while the not-found rows are inside it with no
    // Connect attachment behind them, the last of them naming an attachment of ANOTHER kind.
    let mut refusals: Vec<Refusal> = [
        ("the bare prefix", "tgw-attach"),
        (
            "sixteen hexadecimal digits, one short of the long form",
            "tgw-attach-0123456789abcdef",
        ),
        (
            "a digit outside hexadecimal",
            "tgw-attach-0123456789abcdefg",
        ),
        ("an uppercase suffix", "tgw-attach-0123456789ABCDEF0"),
        (
            "another resource kind's id",
            "tgw-connect-peer-0123456789abcdef0",
        ),
    ]
    .into_iter()
    .map(|(label, attachment_id)| Refusal {
        label: label.to_string(),
        attachment_id: attachment_id.to_string(),
        code: INVALID_TRANSIT_GATEWAY_ATTACHMENT_ID_MALFORMED,
        message: INVALID_TRANSIT_GATEWAY_ATTACHMENT_ID_MESSAGE.to_string(),
    })
    .collect();
    for (label, attachment_id) in [
        (
            "a well-formed long id no attachment is stored for",
            "tgw-attach-0123456789abcdef0".to_string(),
        ),
        (
            "a well-formed short id no attachment is stored for",
            "tgw-attach-0123abcd".to_string(),
        ),
        (
            "the id of an attachment of another kind",
            fixture.transport_id.clone(),
        ),
    ] {
        refusals.push(Refusal {
            label: label.to_string(),
            message: format!(
                "Transit Gateway Connect {attachment_id} was deleted or does not exist."
            ),
            attachment_id,
            code: INVALID_TRANSIT_GATEWAY_ATTACHMENT_ID_NOT_FOUND,
        });
    }

    for refusal in refusals {
        let Refusal {
            label,
            attachment_id,
            code,
            message,
        } = refusal;
        let (status, body) = delete_connect(
            &fixture.router,
            &[param("TransitGatewayAttachmentId", &attachment_id)],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {label:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            code,
            "case {label:?}: expected {code}, got body {body}"
        );
        assert_eq!(
            error_message(&body),
            message,
            "case {label:?}: expected the sentence {message:?}, got body {body}"
        );
        assert!(
            matches!(
                fixture.stored_attachment_state(&live_id).as_deref(),
                Some(
                    TRANSIT_GATEWAY_ATTACHMENT_STATE_PENDING
                        | TRANSIT_GATEWAY_ATTACHMENT_STATE_AVAILABLE
                )
            ),
            "case {label:?}: a refused delete must leave the stored attachment live, got \
             {actual:?}",
            actual = fixture.stored_attachment_state(&live_id),
        );
        // The attachment of another kind the table names is not deleted by being named either.
        assert_eq!(
            fixture
                .stored_attachment_state(&fixture.transport_id)
                .as_deref(),
            Some(TRANSIT_GATEWAY_ATTACHMENT_STATE_AVAILABLE),
            "case {label:?}: the transport attachment must be untouched"
        );
    }
}

#[tokio::test]
async fn a_dry_run_reports_the_outcome_and_records_nothing() {
    let case = "DryRun on a delete that would have succeeded";
    let fixture = Fixture::new().await;
    let attachment_id = fixture.create_connect(case, &[]).await.rest.attachment_id;
    let before = polled_state(
        case,
        &fixture.router,
        &attachment_id,
        TRANSIT_GATEWAY_ATTACHMENT_STATE_AVAILABLE,
    )
    .await;

    let (status, body) = delete_connect(
        &fixture.router,
        &[
            param("TransitGatewayAttachmentId", &attachment_id),
            param("DryRun", "true"),
        ],
    )
    .await;
    assert_eq!(
        status, DRY_RUN_STATUS,
        "case {case:?}: expected HTTP {DRY_RUN_STATUS}, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "DryRunOperation",
        "case {case:?}: expected DryRunOperation, got body {body}"
    );
    let after = described(case, &fixture.router, &attachment_id).await;
    assert_eq!(
        after.state, before.state,
        "case {case:?}: a dry run must not move the attachment, got {actual:?}",
        actual = after.state,
    );
    assert_eq!(
        after.rest, before.rest,
        "case {case:?}: a dry run must leave the stored attachment as it was, got {actual:?}",
        actual = after.rest,
    );
}

#[tokio::test]
async fn a_second_delete_is_refused_in_both_delete_states() {
    // An attachment a delete has already been asked for has nothing left to delete, whether that
    // delete is still being carried out or has settled: one row per delete state of the
    // attachment's own catalog.
    for wanted in [
        TRANSIT_GATEWAY_ATTACHMENT_STATE_DELETING,
        TRANSIT_GATEWAY_ATTACHMENT_STATE_DELETED,
    ] {
        let case = format!("a second delete of an attachment in {wanted:?}");
        let fixture = Fixture::new().await;
        let attachment_id = fixture.create_connect(&case, &[]).await.rest.attachment_id;
        let (status, body) = delete_connect(
            &fixture.router,
            &[param("TransitGatewayAttachmentId", &attachment_id)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the first delete must succeed, got {status} with body {body}"
        );
        let before = polled_state(&case, &fixture.router, &attachment_id, wanted).await;

        let (status, body) = delete_connect(
            &fixture.router,
            &[param("TransitGatewayAttachmentId", &attachment_id)],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            INCORRECT_STATE,
            "case {case:?}: expected {INCORRECT_STATE}, got body {body}"
        );
        let expected = format!("{attachment_id} is in invalid state");
        assert_eq!(
            error_message(&body),
            expected,
            "case {case:?}: expected the sentence {expected:?}, got body {body}"
        );
        let after = described(&case, &fixture.router, &attachment_id).await;
        assert_eq!(
            after.rest, before.rest,
            "case {case:?}: a refused second delete must leave the attachment as it was, got \
             {actual:?}",
            actual = after.rest,
        );
    }
}
