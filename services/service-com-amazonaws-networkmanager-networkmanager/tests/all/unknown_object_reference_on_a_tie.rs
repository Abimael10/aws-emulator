// SPDX-License-Identifier: CC0-1.0

//! The refusal a DEVICE or LINK reference gets when it names no such object of the global
//! network a new tie is stored under, over EVERY operation of this service that stores one.
//!
//! The rule under test: an operation that stores a new record judges the references its
//! input carries — not only the parent the record hangs off, but every resource the record
//! is tied to — as INPUT, before anything is stored. So a `DeviceId` or `LinkId` that names
//! no device or link of the global network the request resolved is a FIELD-validation
//! failure on the member that carried it: HTTP 400 with
//! `x-amzn-errortype: ValidationException`, `Reason` = `FieldValidationFailed`, and one
//! `Fields` entry whose `Name` is that member in this service's own lowerCamelCase field
//! naming and whose `Message` is the top-level `Message` — "Device not found." /
//! "Link not found.", the "<Kind> not found." prose the recorded refusals of the same two
//! members answer. The 404 `ResourceNotFoundException` is what an operation addressed AT an
//! existing resource answers instead — a read, an update, a delete, a disassociation — and
//! `crate::resource_not_found_message` owns that half.
//!
//! The rows span what the rule generalises over:
//!
//! * the OPERATION that stores the tie — all four of this service: `AssociateLink`,
//!   `AssociateCustomerGateway`, `AssociateConnectPeer` and
//!   `AssociateTransitGatewayConnectPeer` (`smithy_query query="digest"` lists no other
//!   operation that ties a device to something);
//! * WHICH member carries the unresolvable reference, `DeviceId` or `LinkId` — the other
//!   one of the pair naming a live object every time, so each row is refused on one member
//!   only;
//! * HOW the reference fails to resolve: an id of the minted shape that was never minted,
//!   one minted and then deleted, and a live device or link stored under ANOTHER global
//!   network (addressable there and nowhere else).
//!
//! `crate::unknown_parent_reference` pins the same refusal for the PARENT a create hangs a
//! new resource off, and `crate::unresolvable_reference_field_naming` pins that one member
//! reports one field name and one prose whichever operation refused it; what this suite adds
//! is the half where the unresolvable reference is an object the new record is TIED to.
//!
//! Every call goes through the real `MultiServiceRouter`, and nothing reaches into
//! `crate::state`: each refusal is read back for through the association's own implemented
//! reader. The harness is the one `global_network_lifecycle` defines and the fixture is the
//! one `connect_peer_association_lifecycle` builds, so the Connect peers the rows need are
//! minted exactly as that suite mints them.

use serde_json::{json, Value};

use crate::connect_peer_association_lifecycle::fixture as peer_fixture;
use crate::customer_gateway_association_lifecycle::{plain_cgw_arn, PLAIN_REGION};
use crate::device_lifecycle::create_device_ok;
use crate::global_network_lifecycle::{error_code, Emulator};
use crate::link_lifecycle::{create_link_ok, plain_link};
use crate::transit_gateway_connect_peer_association_lifecycle::{
    connect_peer_arn, register_gateway,
};
use crate::transit_gateway_peering::{own_account, transit_gateway_arn};

/// The request members the rule covers, in the lowerCamelCase spelling this service names
/// its own fields in (`crate::unresolvable_reference_field_naming` owns that spelling).
pub(crate) const FIELD_DEVICE_ID: &str = "deviceId";
/// See [`FIELD_DEVICE_ID`].
pub(crate) const FIELD_LINK_ID: &str = "linkId";

/// The prose a device reference that resolves to nothing carries: the "<Kind> not found."
/// text the recorded refusals of this member answer.
pub(crate) const DEVICE_NOT_FOUND: &str = "Device not found.";
/// See [`DEVICE_NOT_FOUND`]: the same prose for the link kind, which the recorded
/// `AssociateLink` on a deleted link answers.
pub(crate) const LINK_NOT_FOUND: &str = "Link not found.";

/// The `ValidationException$Reason` a refusal of one input MEMBER carries.
const REASON_FIELD_VALIDATION_FAILED: &str = "FieldValidationFailed";

/// An id of the shape this service mints that was never minted.
const UNMINTED_DEVICE: &str = "device-0000000000000beef";
/// See [`UNMINTED_DEVICE`].
const UNMINTED_LINK: &str = "link-0000000000000beef";

/// The four operations of this service that store a tie whose input names a device, each
/// with the collection it POSTs to and the member its reader lists ties under.
#[derive(Clone, Copy)]
enum Tie {
    Link,
    CustomerGateway,
    ConnectPeer,
    TransitGatewayConnectPeer,
}

const TIES: [Tie; 4] = [
    Tie::Link,
    Tie::CustomerGateway,
    Tie::ConnectPeer,
    Tie::TransitGatewayConnectPeer,
];

impl Tie {
    /// The operation's name, as every assertion message below spells it.
    fn name(self) -> &'static str {
        match self {
            Self::Link => "AssociateLink",
            Self::CustomerGateway => "AssociateCustomerGateway",
            Self::ConnectPeer => "AssociateConnectPeer",
            Self::TransitGatewayConnectPeer => "AssociateTransitGatewayConnectPeer",
        }
    }

    /// The collection under the global network in the path that the associate POSTs to and
    /// its reader GETs from (`smithy_query query="digest"`).
    fn collection(self) -> &'static str {
        match self {
            Self::Link => "link-associations",
            Self::CustomerGateway => "customer-gateway-associations",
            Self::ConnectPeer => "connect-peer-associations",
            Self::TransitGatewayConnectPeer => "transit-gateway-connect-peer-associations",
        }
    }

    /// The member the reader lists the stored ties under.
    fn list_member(self) -> &'static str {
        match self {
            Self::Link => "LinkAssociations",
            Self::CustomerGateway => "CustomerGatewayAssociations",
            Self::ConnectPeer => "ConnectPeerAssociations",
            Self::TransitGatewayConnectPeer => "TransitGatewayConnectPeerAssociations",
        }
    }

    /// The request this operation sends: the resource it ties (`subject`, which every row
    /// names resolvably), the device and the link.
    fn request(self, subject: &str, device: &str, link: &str) -> Value {
        match self {
            Self::Link => json!({ "DeviceId": device, "LinkId": link }),
            Self::CustomerGateway => json!({
                "CustomerGatewayArn": subject,
                "DeviceId": device,
                "LinkId": link,
            }),
            Self::ConnectPeer => json!({
                "ConnectPeerId": subject,
                "DeviceId": device,
                "LinkId": link,
            }),
            Self::TransitGatewayConnectPeer => json!({
                "TransitGatewayConnectPeerArn": subject,
                "DeviceId": device,
                "LinkId": link,
            }),
        }
    }
}

/// One row: the operation, the pair it names, and the member and prose its refusal owes.
struct Row {
    case: String,
    tie: Tie,
    subject: String,
    device: String,
    link: String,
    field: &'static str,
    message: &'static str,
}

/// The subjects the ties are stored against, one per operation and one per use, so a row
/// that is ACCEPTED never collides with a tie another row already holds. `index` makes each
/// gateway and peer ARN distinct.
fn subject(tie: Tie, peers: &[String], index: usize) -> String {
    match tie {
        Tie::Link => String::new(),
        Tie::CustomerGateway => plain_cgw_arn(&format!("{:017x}", index + 1)),
        Tie::ConnectPeer => peers[index % peers.len()].clone(),
        Tie::TransitGatewayConnectPeer => {
            connect_peer_arn("aws", PLAIN_REGION, &own_account(), index + 1, 17)
        }
    }
}

/// What one collection lists right now. `query` narrows the read, which
/// `GetLinkAssociations` requires: "either the device ID or the link ID must be specified"
/// (`smithy_query query="doc" shape="GetLinkAssociations"`).
async fn listed(emulator: &Emulator, case: &str, network: &str, tie: Tie, query: &str) -> Vec<Value> {
    let uri = if query.is_empty() {
        format!("/global-networks/{network}/{}", tie.collection())
    } else {
        format!("/global-networks/{network}/{}?{query}", tie.collection())
    };
    let (status, body) = emulator.get(&uri).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the {} reader expected 200, got {status} with {body}",
        tie.name()
    );
    body[tie.list_member()]
        .as_array()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected a {} array in {body}",
                tie.list_member()
            )
        })
        .clone()
}

#[tokio::test]
async fn every_tie_refuses_an_unresolvable_device_or_link_on_the_member_that_carried_it() {
    let emulator = Emulator::new();
    // A global network holding a core network with its Connect peers, a device at a site,
    // one with no site, and that site's links — the sited device and `link` already tied by
    // `AssociateLink`, which is the precondition the peer associates put on a named link.
    let here = peer_fixture(&emulator, "unresolvable object references", 0).await;
    // A transit gateway of the Region the rows' Connect peer ARNs name, so
    // `AssociateTransitGatewayConnectPeer` reaches the reference check and, for the pair
    // that resolves, past it.
    register_gateway(
        &emulator,
        &here.global_network,
        &transit_gateway_arn("aws", PLAIN_REGION, &own_account(), 1, 17),
    )
    .await;

    // A device and a link of this global network that are then DELETED: ids this store did
    // mint, naming nothing now.
    let site = emulator.create_site(&here.global_network, "deletions").await;
    let gone_device =
        create_device_ok(&emulator, &here.global_network, json!({ "SiteId": site })).await;
    let gone_link =
        create_link_ok(&emulator, &here.global_network, &site, &plain_link()).await;
    for (what, uri) in [
        (
            "DeleteDevice",
            format!(
                "/global-networks/{}/devices/{gone_device}",
                here.global_network
            ),
        ),
        (
            "DeleteLink",
            format!(
                "/global-networks/{}/links/{gone_link}",
                here.global_network
            ),
        ),
    ] {
        let (status, body) = emulator.delete(&uri).await;
        assert_eq!(
            status, 200,
            "{what}: expected 200 while preparing a deleted reference, got {status} with {body}"
        );
    }

    // Another global network with its own sited device and link, live but addressable only
    // there.
    let elsewhere = emulator.create_global_network("elsewhere").await;
    let foreign_site = emulator.create_site(&elsewhere, "foreign-hq").await;
    let foreign_device =
        create_device_ok(&emulator, &elsewhere, json!({ "SiteId": foreign_site })).await;
    let foreign_link =
        create_link_ok(&emulator, &elsewhere, &foreign_site, &plain_link()).await;

    // How a reference fails to resolve, for each of the two members.
    let unresolvable_devices = [
        ("a device that was never minted", UNMINTED_DEVICE.to_string()),
        ("a device that was deleted", gone_device.clone()),
        (
            "a device of another global network",
            foreign_device.clone(),
        ),
    ];
    let unresolvable_links = [
        ("a link that was never minted", UNMINTED_LINK.to_string()),
        ("a link that was deleted", gone_link.clone()),
        ("a link of another global network", foreign_link.clone()),
    ];

    let mut rows: Vec<Row> = Vec::new();
    let mut index = 0usize;
    for tie in TIES {
        for (form, device) in &unresolvable_devices {
            rows.push(Row {
                case: format!("{} with {form}", tie.name()),
                tie,
                subject: subject(tie, &here.peers, index),
                device: device.clone(),
                link: here.link.clone(),
                field: FIELD_DEVICE_ID,
                message: DEVICE_NOT_FOUND,
            });
            index += 1;
        }
        for (form, link) in &unresolvable_links {
            rows.push(Row {
                case: format!("{} with {form}", tie.name()),
                tie,
                subject: subject(tie, &here.peers, index),
                device: here.sited_device.clone(),
                link: link.clone(),
                field: FIELD_LINK_ID,
                message: LINK_NOT_FOUND,
            });
            index += 1;
        }
    }

    for row in &rows {
        let Row {
            case,
            tie,
            subject,
            device,
            link,
            field,
            message,
        } = row;
        let body = tie.request(subject, device, link);
        let (status, refused) = emulator
            .post(
                &format!(
                    "/global-networks/{}/{}",
                    here.global_network,
                    tie.collection()
                ),
                body.clone(),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: {body} expected 400, got {status} with {refused}"
        );
        assert_eq!(
            error_code(&refused),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {refused}"
        );
        assert_eq!(
            refused["Reason"].as_str(),
            Some(REASON_FIELD_VALIDATION_FAILED),
            "case {case:?}: expected Reason {REASON_FIELD_VALIDATION_FAILED}, got {refused}"
        );
        assert_eq!(
            refused["Message"].as_str(),
            Some(*message),
            "case {case:?}: expected Message {message:?}, got {refused}"
        );
        assert_eq!(
            refused["Fields"],
            json!([{ "Name": field, "Message": message }]),
            "case {case:?}: expected one Fields entry naming {field:?}, got {refused}"
        );
    }

    // Not one of those requests stored anything: the only tie this global network holds is
    // the device/link pair the fixture made, and no associate stored a tie of its own.
    let held = listed(
        &emulator,
        "the link associations after the refusals",
        &here.global_network,
        Tie::Link,
        &format!("deviceId={}", here.sited_device),
    )
    .await;
    assert_eq!(
        held.len(),
        1,
        "a refused associate stores nothing, so the one pair the fixture tied must be the only link association, got {held:?}"
    );
    assert_eq!(
        (
            held[0]["DeviceId"].as_str(),
            held[0]["LinkId"].as_str()
        ),
        (
            Some(here.sited_device.as_str()),
            Some(here.link.as_str())
        ),
        "the one link association must be the pair the fixture tied, got {held:?}"
    );
    for tie in [
        Tie::CustomerGateway,
        Tie::ConnectPeer,
        Tie::TransitGatewayConnectPeer,
    ] {
        let case = format!("the {} ties after the refusals", tie.name());
        let held = listed(&emulator, &case, &here.global_network, tie, "").await;
        assert!(
            held.is_empty(),
            "case {case:?}: a refused associate stores nothing, got {held:?}"
        );
    }

    // And the rule is the reference check and nothing wider: the same request against a
    // device and a link that DO resolve is accepted by every one of the four operations.
    for (position, tie) in TIES.iter().enumerate() {
        let case = format!("{} with references that resolve", tie.name());
        let body = tie.request(
            &subject(*tie, &here.peers, position),
            &here.sited_device,
            &here.link,
        );
        let (status, accepted) = emulator
            .post(
                &format!(
                    "/global-networks/{}/{}",
                    here.global_network,
                    tie.collection()
                ),
                body.clone(),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: {body} expected 200, got {status} with {accepted}"
        );
    }
}
