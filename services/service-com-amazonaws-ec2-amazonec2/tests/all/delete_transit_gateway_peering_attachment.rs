// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `DeleteTransitGatewayPeeringAttachment` and the describes it is read
//! back through, driven over the wire the service actually speaks: `ec2Query` — an
//! `application/x-www-form-urlencoded` POST whose body carries
//! `Action=DeleteTransitGatewayPeeringAttachment`, answered with XML (README.md /
//! OPERATIONS.md "Wire protocol").
//!
//! The rule under test: a delete of a transit gateway attachment WRITES THE STORED RECORD into
//! the transient `deleting` member of its own state catalog with the clock owing it the terminal
//! `deleted`, and answers the record as the store then holds it — every other member unchanged,
//! and a peering attachment's `Status`, which is the status OF the state it is in, moved with
//! that state — so both describes report what the delete answered. It refuses rather than
//! reporting a deletion it did not perform: a value that is not an attachment id at all with
//! that resource's `Malformed` code, an id no peering attachment of the account is held under —
//! including one naming an attachment of another kind — with its `NotFound` code in the wording
//! the transit gateway family answers for an attachment of its own that is not there, and an
//! attachment the lifecycle finished some OTHER way — rejected or failed — with `IncorrectState`.
//! A delete of an attachment whose deletion is already under way or done is what the caller
//! asked for, so it answers the record rather than refusing, and starts no second clock on it.
//!
//! Every table row is a distinct input along what that rule generalises over: which state the
//! attachment is in when the delete arrives (four non-terminal members of the catalog, plus one
//! row whose attachment `CreateTransitGatewayPeeringAttachment` itself made), whether it carries
//! an options block and which dynamic-routing value, whether the accepter end has an attachment
//! id of its own, whether the status carries a message, and how many tags it holds — plus one
//! refusal row per unresolvable id form and per state that admits no delete.
//!
//! Nothing here waits a fixed time for a state the clock owes: each look is taken again until
//! the describe reports the state, or a deadline far past the service's own transition span
//! passes.

use std::time::{Duration, Instant, SystemTime};

use ec2::ops::types::{
    PeeringAttachmentStatus, PeeringTgwInfo, TRANSIT_GATEWAY_ATTACHMENT_STATE_VALUES, Tag,
    TransitGatewayPeeringAttachment, TransitGatewayPeeringAttachmentOptions,
};
use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, INCORRECT_STATE,
    INVALID_TRANSIT_GATEWAY_ATTACHMENT_ID_MALFORMED,
    INVALID_TRANSIT_GATEWAY_ATTACHMENT_ID_MESSAGE,
    INVALID_TRANSIT_GATEWAY_ATTACHMENT_ID_NOT_FOUND, OPTION_DISABLE, OPTION_ENABLE,
    TRANSIT_GATEWAY_ATTACHMENT_RESOURCE_NAME, TRANSIT_GATEWAY_STATE_AVAILABLE, TRANSITION_DELAY,
    TransitGatewayAttachmentView, system_time_to_date_time,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, error_message, indexed};
use super::wire::{Param, child, child_text, document, param, set_items};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The ec2Query `Action` under test.
const ACTION: &str = "DeleteTransitGatewayPeeringAttachment";

/// The describe this delete is read back through.
const DESCRIBE: &str = "DescribeTransitGatewayPeeringAttachments";

/// The whole-family describe, which reports the same record.
const FAMILY_DESCRIBE: &str = "DescribeTransitGatewayAttachments";

/// The HTTP status EC2 answers `DryRunOperation` with.
const DRY_RUN_STATUS: u16 = 412;

/// The HTTP status EC2 answers a refused request with.
const REFUSAL_STATUS: u16 = 400;

/// A well-formed EC2 id suffix — 17 lowercase hex digits — that nothing in this store mints, so
/// an id built from it is well formed and names no resource.
const UNKNOWN_SUFFIX: &str = "00000000000000000";

/// The account the seeded peerings name as the owner of the peer gateway, so each is a complete
/// record of a peering that spans two accounts.
const PEER_ACCOUNT: &str = "210987654321";

/// The Region the seeded peerings place that peer gateway in.
const PEER_REGION: &str = "us-west-2";

/// The deadline the polls below give up at: a large multiple of the service's own transition
/// span, so the assertion is about the state the clock owes and never about how fast the host
/// runs.
fn move_deadline() -> Duration {
    TRANSITION_DELAY * 60
}

/// How long one look waits before looking again — a fraction of that span, so a poll inside the
/// deadline above costs many looks rather than one.
fn poll_interval() -> Duration {
    TRANSITION_DELAY / 10
}

/// The value `name` names in the modelled `TransitGatewayAttachmentState` catalog
/// (`smithy_query(query="shape",shape="TransitGatewayAttachmentState")`, re-exported from
/// `ec2::ops::types`), or a panic — so every state written into a fixture or asserted on comes
/// from the model rather than from this file.
fn attachment_state(name: &str) -> &'static str {
    TRANSIT_GATEWAY_ATTACHMENT_STATE_VALUES
        .iter()
        .find(|value| **value == name)
        .unwrap_or_else(|| panic!("{name:?} is no TransitGatewayAttachmentState member"))
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

/// One reported `TransitGatewayPeeringAttachment`, read out of the XML by the document rather
/// than by string surgery: the two `PeeringTgwInfo` ends carry identically-named members, so
/// each is read inside its own element.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reported {
    attachment_id: String,
    accepter_attachment_id: Option<String>,
    requester: (String, String, String),
    accepter: (String, String, String),
    state: String,
    status_code: Option<String>,
    status_message: Option<String>,
    dynamic_routing: Option<String>,
    creation_time: String,
    tags: Vec<(String, String)>,
}

/// Every peering attachment a response carries, in document order: the elements that have a
/// `requesterTgwInfo` child, whether the response is the delete's single
/// `transitGatewayPeeringAttachment` or a describe's set.
fn reported_all(case: &str, body: &str) -> Vec<Reported> {
    let parsed = document(case, body);
    parsed
        .descendants()
        .filter(|node| node.is_element() && child(*node, "requesterTgwInfo").is_some())
        .map(|item| {
            let end = |name: &str| {
                child(item, name)
                    .map(|info| {
                        (
                            child_text(info, "transitGatewayId").unwrap_or_default(),
                            child_text(info, "ownerId").unwrap_or_default(),
                            child_text(info, "region").unwrap_or_default(),
                        )
                    })
                    .unwrap_or_default()
            };
            Reported {
                attachment_id: child_text(item, "transitGatewayAttachmentId").unwrap_or_default(),
                accepter_attachment_id: child_text(
                    item,
                    "accepterTransitGatewayAttachmentId",
                ),
                requester: end("requesterTgwInfo"),
                accepter: end("accepterTgwInfo"),
                state: child_text(item, "state").unwrap_or_default(),
                status_code: child(item, "status").and_then(|status| child_text(status, "code")),
                status_message: child(item, "status")
                    .and_then(|status| child_text(status, "message")),
                dynamic_routing: child(item, "options")
                    .and_then(|options| child_text(options, "dynamicRouting")),
                creation_time: child_text(item, "creationTime").unwrap_or_default(),
                tags: set_items(item, "tagSet")
                    .into_iter()
                    .map(|tag| {
                        (
                            child_text(tag, "key").unwrap_or_default(),
                            child_text(tag, "value").unwrap_or_default(),
                        )
                    })
                    .collect(),
            }
        })
        .collect()
}

/// The one peering attachment a response carries, or a panic naming the case.
fn reported(case: &str, body: &str) -> Reported {
    let mut all = reported_all(case, body);
    assert_eq!(
        all.len(),
        1,
        "case {case:?}: expected exactly one peering attachment, got {all:?} in {body}"
    );
    all.remove(0)
}

/// The one peering attachment the describe reports for `attachment_id`.
async fn described(case: &str, router: &MultiServiceRouter, attachment_id: &str) -> Reported {
    let body = ok_body(
        case,
        router,
        DESCRIBE,
        &indexed("TransitGatewayAttachmentIds", &[attachment_id]),
    )
    .await;
    reported(case, &body)
}

/// Look at the attachment AGAIN until the describe reports it in `wanted`, or a deadline far
/// past the service's own transition span passes — the condition is waited for, never a fixed
/// time, so nothing here depends on how fast the host runs.
async fn poll_until_state(
    case: &str,
    router: &MultiServiceRouter,
    attachment_id: &str,
    wanted: &str,
) -> Reported {
    let deadline = Instant::now() + move_deadline();
    loop {
        let seen = described(case, router, attachment_id).await;
        if seen.state == wanted {
            return seen;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {attachment_id} to reach {wanted:?}, got {state:?}",
            state = seen.state
        );
        tokio::time::sleep(poll_interval()).await;
    }
}

/// The state `DescribeTransitGateways` reports for one gateway right now.
async fn described_gateway_state(
    case: &str,
    router: &MultiServiceRouter,
    transit_gateway_id: &str,
) -> String {
    let body = ok_body(
        case,
        router,
        "DescribeTransitGateways",
        &indexed("TransitGatewayIds", &[transit_gateway_id]),
    )
    .await;
    element(&body, "state").unwrap_or_default()
}

/// Look again until `DescribeTransitGateways` reports the gateway in service, which is what a
/// peering of it needs — the condition, not a slept span.
async fn poll_until_gateway_available(
    case: &str,
    router: &MultiServiceRouter,
    transit_gateway_id: &str,
) {
    let deadline = Instant::now() + move_deadline();
    loop {
        let state = described_gateway_state(case, router, transit_gateway_id).await;
        if state == TRANSIT_GATEWAY_STATE_AVAILABLE {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {transit_gateway_id} to reach \
             {TRANSIT_GATEWAY_STATE_AVAILABLE:?}, got {state:?}"
        );
        tokio::time::sleep(poll_interval()).await;
    }
}

/// The emulator to drive: the router to call and the engine to read and seed the store through.
struct Fixture {
    router: MultiServiceRouter,
    engine: std::sync::Arc<engine::InMemoryEngine>,
}

/// What one seeded peering attachment carries beside its state, so a row can differ along what
/// the rule generalises over without changing the delete it drives.
struct Seed {
    /// The gateway the peering was requested from, which this account owns.
    requester_gateway: &'static str,
    /// The gateway the peering was requested of, which the peer account owns.
    accepter_gateway: &'static str,
    /// "Indicates whether dynamic routing is enabled or disabled"
    /// (`TransitGatewayPeeringAttachmentOptions$DynamicRouting`) — `None` for an attachment
    /// carrying no options block at all.
    dynamic_routing: Option<&'static str>,
    /// The accepter's own attachment id, which a peering only carries once the other side has
    /// one.
    accepter_attachment_id: Option<&'static str>,
    /// The message beside the status code, for a row whose status carries one.
    status_message: Option<&'static str>,
    /// The tags it holds, as (key, value) pairs — empty for an untagged attachment.
    tags: &'static [(&'static str, &'static str)],
}

impl Fixture {
    async fn new() -> Self {
        let (router, engine) = super::ec2_query::emulator();
        Self { router, engine }
    }

    /// Seed one peering attachment in `state` carrying `seed`, straight through the store's own
    /// writer — so a row can put the attachment in a state no implemented operation leaves one
    /// in. Answers the `TransitGatewayAttachmentId` the store minted.
    fn seed(&self, state: &str, seed: &Seed) -> String {
        let side = |transit_gateway_id: &str, owner_id: &str, region: &str| {
            Some(PeeringTgwInfo {
                transit_gateway_id: Some(transit_gateway_id.to_string()),
                core_network_id: None,
                owner_id: Some(owner_id.to_string()),
                region: Some(region.to_string()),
            })
        };
        let tags: Vec<Tag> = seed
            .tags
            .iter()
            .map(|(key, value)| Tag {
                key: Some((*key).to_string()),
                value: Some((*value).to_string()),
            })
            .collect();
        self.engine
            .state::<Ec2State>()
            .lock()
            .insert_transit_gateway_attachment(TransitGatewayAttachmentView::Peering(
                TransitGatewayPeeringAttachment {
                    // The store mints the id; what a writer puts here is replaced by it.
                    transit_gateway_attachment_id: None,
                    accepter_transit_gateway_attachment_id: seed
                        .accepter_attachment_id
                        .map(str::to_string),
                    requester_tgw_info: side(
                        seed.requester_gateway,
                        engine::emulator_account_id(),
                        REGION,
                    ),
                    accepter_tgw_info: side(seed.accepter_gateway, PEER_ACCOUNT, PEER_REGION),
                    options: seed
                        .dynamic_routing
                        .map(|routing| TransitGatewayPeeringAttachmentOptions {
                            dynamic_routing: Some(routing.to_string()),
                        }),
                    // "The status of the transit gateway peering attachment" is the status OF
                    // the state it is in, which is how every writer of this store sets it.
                    status: Some(PeeringAttachmentStatus {
                        code: Some(state.to_string()),
                        message: seed.status_message.map(str::to_string),
                    }),
                    state: Some(state.to_string()),
                    creation_time: Some(system_time_to_date_time(SystemTime::now())),
                    tags: (!tags.is_empty()).then_some(tags),
                },
            ))
            .expect("the attachment store has room for one seeded peering attachment")
    }

    /// Seed one attachment of ANOTHER kind — a VPC attachment — so a refusal row can name an id
    /// this operation cannot act on.
    fn seed_other_kind(&self) -> String {
        self.engine
            .state::<Ec2State>()
            .lock()
            .insert_transit_gateway_attachment(
                super::transit_gateway_acceptance::pending_vpc_attachment(),
            )
            .expect("the attachment store has room for one seeded VPC attachment")
    }

    /// Every attachment the store holds, by the id it is held under and the state it is in, as
    /// the store ACTUALLY holds them — what a refusal must leave untouched.
    fn stored_states(&self) -> Vec<(String, String)> {
        self.engine
            .state::<Ec2State>()
            .lock()
            .transit_gateway_attachments
            .iter()
            .map(|(attachment_id, record)| {
                (
                    attachment_id.clone(),
                    record.view.state().unwrap_or_default().to_string(),
                )
            })
            .collect()
    }

    /// The state and the owed move the store holds for `attachment_id`, read straight off the
    /// record rather than through a describe, so what the delete WROTE is asserted without
    /// depending on how much of the clock's span has passed.
    fn stored_move(&self, attachment_id: &str) -> (String, Option<String>) {
        let handle = self.engine.state::<Ec2State>();
        let store = handle.lock();
        let record = store
            .transit_gateway_attachments
            .get(attachment_id)
            .unwrap_or_else(|| panic!("the store must still hold {attachment_id}"));
        (
            record.view.state().unwrap_or_default().to_string(),
            record.pending.as_ref().map(|pending| pending.to.clone()),
        )
    }

    /// One `DeleteTransitGatewayPeeringAttachment` call naming `attachment_id`, with `extra`
    /// form fields beside it.
    async fn delete(&self, attachment_id: &str, extra: &[Param]) -> (u16, String) {
        let mut params = vec![param("TransitGatewayAttachmentId", attachment_id)];
        params.extend_from_slice(extra);
        invoke(&self.router, ACTION, &params).await
    }

    /// Two transit gateways of this account in service, and the peering attachment
    /// `CreateTransitGatewayPeeringAttachment` makes of them — the one row whose attachment no
    /// seeding wrote. Answers that attachment's id.
    async fn create_peering(&self, case: &str) -> String {
        let mut gateways = Vec::new();
        for _ in 0..2 {
            let body = ok_body(case, &self.router, "CreateTransitGateway", &[]).await;
            gateways.push(
                element(&body, "transitGatewayId")
                    .expect("a created transit gateway reports its own id"),
            );
        }
        for transit_gateway_id in &gateways {
            poll_until_gateway_available(case, &self.router, transit_gateway_id).await;
        }
        let body = ok_body(
            case,
            &self.router,
            "CreateTransitGatewayPeeringAttachment",
            &[
                param("TransitGatewayId", &gateways[0]),
                param("PeerTransitGatewayId", &gateways[1]),
                param("PeerAccountId", engine::emulator_account_id()),
                param("PeerRegion", REGION),
            ],
        )
        .await;
        reported(case, &body).attachment_id
    }
}

/// The peering the rows below seed: two gateways of the pair, with the members a row varies.
const GATEWAY_A: &str = "tgw-0000000000000000a";
const GATEWAY_B: &str = "tgw-0000000000000000b";

/// One attachment to delete: the state it is in when the delete arrives, and what it carries.
struct Case {
    label: &'static str,
    /// The `TransitGatewayAttachmentState` member the attachment is in, or `None` for the row
    /// whose attachment `CreateTransitGatewayPeeringAttachment` itself makes.
    seeded_state: Option<&'static str>,
    seed: Seed,
}

/// Every non-terminal state a peering attachment can be in when a delete arrives admits it: the
/// record is written into `deleting` with the clock owing it `deleted`, the answer carries the
/// stored record with every member but the state and its status unchanged, and both describes
/// report the same.
#[tokio::test]
async fn a_delete_writes_the_attachment_into_deleting_and_owes_it_deleted() {
    let cases = [
        Case {
            label: "an attachment in service, dynamic routing on, two tags",
            seeded_state: Some("available"),
            seed: Seed {
                requester_gateway: GATEWAY_A,
                accepter_gateway: GATEWAY_B,
                dynamic_routing: Some(OPTION_ENABLE),
                accepter_attachment_id: Some("tgw-attach-0123456789abcdef0"),
                status_message: None,
                tags: &[("Name", "north"), ("Team", "network")],
            },
        },
        Case {
            label: "a request still being initiated, no options block, no tags",
            seeded_state: Some("initiatingRequest"),
            seed: Seed {
                requester_gateway: GATEWAY_A,
                accepter_gateway: GATEWAY_B,
                dynamic_routing: None,
                accepter_attachment_id: None,
                status_message: None,
                tags: &[],
            },
        },
        Case {
            label: "an attachment waiting for acceptance, dynamic routing off, a status message",
            seeded_state: Some("pendingAcceptance"),
            seed: Seed {
                requester_gateway: GATEWAY_B,
                accepter_gateway: GATEWAY_A,
                dynamic_routing: Some(OPTION_DISABLE),
                accepter_attachment_id: None,
                status_message: Some("Pending acceptance by the accepter account"),
                tags: &[("Name", "south")],
            },
        },
        Case {
            label: "an attachment still pending, one tag",
            seeded_state: Some("pending"),
            seed: Seed {
                requester_gateway: GATEWAY_A,
                accepter_gateway: GATEWAY_B,
                dynamic_routing: Some(OPTION_ENABLE),
                accepter_attachment_id: None,
                status_message: None,
                tags: &[("Stage", "test")],
            },
        },
        Case {
            label: "an attachment being modified, no tags",
            seeded_state: Some("modifying"),
            seed: Seed {
                requester_gateway: GATEWAY_B,
                accepter_gateway: GATEWAY_A,
                dynamic_routing: None,
                accepter_attachment_id: Some("tgw-attach-0123456789abcdef1"),
                status_message: None,
                tags: &[],
            },
        },
        Case {
            label: "an attachment the create itself made",
            seeded_state: None,
            seed: Seed {
                requester_gateway: GATEWAY_A,
                accepter_gateway: GATEWAY_B,
                dynamic_routing: None,
                accepter_attachment_id: None,
                status_message: None,
                tags: &[],
            },
        },
    ];

    for case in &cases {
        let label = case.label;
        let fixture = Fixture::new().await;
        let attachment_id = match case.seeded_state {
            Some(state) => fixture.seed(attachment_state(state), &case.seed),
            None => fixture.create_peering(label).await,
        };

        let before = described(label, &fixture.router, &attachment_id).await;

        let (status, body) = fixture.delete(&attachment_id, &[]).await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected HTTP 200, got {status} with body {body}"
        );
        let answered = reported(label, &body);
        let deleting = attachment_state("deleting");
        assert_eq!(
            answered.state, deleting,
            "case {label:?}: expected the answer to carry the transient delete state, got \
             {answered:?}"
        );
        // "The status of the transit gateway peering attachment" is the status OF the state it
        // is in, so the delete moves it with the state rather than leaving it naming the
        // previous one.
        assert_eq!(
            answered.status_code.as_deref(),
            Some(deleting),
            "case {label:?}: expected the status to name the state the attachment is now in, \
             got {:?}",
            answered.status_code
        );
        // Every member but the state and its status is the one the store already held: the
        // delete reports the record, it does not rebuild it.
        assert_eq!(
            Reported {
                state: before.state.clone(),
                status_code: before.status_code.clone(),
                status_message: before.status_message.clone(),
                ..answered.clone()
            },
            before,
            "case {label:?}: expected every member but the state and its status to be the \
             stored one, got {answered:?} where {before:?} was held"
        );

        // What the delete WROTE, read straight off the record: the transient state now, the
        // terminal one owed. No span has to have passed for this to hold.
        let (stored_state, owed) = fixture.stored_move(&attachment_id);
        assert_eq!(
            stored_state, deleting,
            "case {label:?}: expected the store to hold the transient delete state, got \
             {stored_state:?}"
        );
        assert_eq!(
            owed.as_deref(),
            Some(attachment_state("deleted")),
            "case {label:?}: expected the clock to owe the terminal delete state, got {owed:?}"
        );

        // And the move the clock owed lands, on both describes.
        let settled = poll_until_state(
            label,
            &fixture.router,
            &attachment_id,
            attachment_state("deleted"),
        )
        .await;
        assert_eq!(
            settled.status_code.as_deref(),
            Some(attachment_state("deleted")),
            "case {label:?}: expected the settled status to name the terminal state, got {:?}",
            settled.status_code
        );
        assert_eq!(
            Reported {
                state: before.state.clone(),
                status_code: before.status_code.clone(),
                status_message: before.status_message.clone(),
                ..settled.clone()
            },
            before,
            "case {label:?}: expected the settled record to differ from the stored one only in \
             its state and that state's status, got {settled:?}"
        );
        let family = ok_body(
            label,
            &fixture.router,
            FAMILY_DESCRIBE,
            &indexed("TransitGatewayAttachmentIds", &[&attachment_id]),
        )
        .await;
        let family_state = element(&family, "state");
        assert_eq!(
            family_state.as_deref(),
            Some(attachment_state("deleted")),
            "case {label:?}: expected {FAMILY_DESCRIBE} to report the deleted state, got \
             {family_state:?} in {family}"
        );
    }
}

/// The seed the refusal rows and the second-delete row drive, which varies nothing: what they
/// assert is about the id and the state, not about what the attachment carries.
fn plain_seed() -> Seed {
    Seed {
        requester_gateway: GATEWAY_A,
        accepter_gateway: GATEWAY_B,
        dynamic_routing: Some(OPTION_ENABLE),
        accepter_attachment_id: None,
        status_message: None,
        tags: &[("Name", "plain")],
    }
}

/// A second delete of the same attachment is what the caller already asked for: it answers the
/// record as the store holds it, whether the first delete is still under way or has settled, and
/// starts no second clock — the attachment still reaches the terminal state the first delete owed
/// it and the store still holds exactly one record for it.
#[tokio::test]
async fn a_repeated_delete_answers_the_record_it_already_asked_for() {
    let fixture = Fixture::new().await;
    let attachment_id = fixture.seed(attachment_state("available"), &plain_seed());
    let (status, body) = fixture.delete(&attachment_id, &[]).await;
    assert_eq!(
        status, 200,
        "the first delete must succeed, got {status} with body {body}"
    );
    let held_before = fixture.stored_states().len();

    for (case, expected_state) in [
        ("straight after the first delete", "deleting"),
        ("once the move has landed", "deleted"),
    ] {
        if expected_state == "deleted" {
            poll_until_state(
                case,
                &fixture.router,
                &attachment_id,
                attachment_state("deleted"),
            )
            .await;
        }
        let (status, body) = fixture.delete(&attachment_id, &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with body {body}"
        );
        let answered = reported(case, &body);
        let wanted = attachment_state(expected_state);
        assert_eq!(
            answered.state, wanted,
            "case {case:?}: expected the record as the store holds it, in {wanted:?}, got \
             {answered:?}"
        );
        assert_eq!(
            answered.attachment_id, attachment_id,
            "case {case:?}: expected the answer to name the attachment the request did, got {:?}",
            answered.attachment_id
        );
        let held = fixture.stored_states().len();
        assert_eq!(
            held, held_before,
            "case {case:?}: a repeated delete stores no second record, got {held} where \
             {held_before} was held"
        );
    }

    // The first delete's own clock is the one that ran: the attachment is in the terminal state
    // it owed, with nothing further owed.
    let (stored_state, owed) = fixture.stored_move(&attachment_id);
    assert_eq!(
        stored_state,
        attachment_state("deleted"),
        "expected the attachment to hold the terminal delete state, got {stored_state:?}"
    );
    assert_eq!(
        owed, None,
        "expected no further owed move once the deletion has landed, got {owed:?}"
    );
}

/// One refusal to drive: the id it names, and the code and sentence EC2 answers it with.
struct Refusal {
    label: String,
    attachment_id: String,
    code: String,
    message: String,
}

/// Every refusal of this delete carries EC2's own code and sentence, and writes nothing.
#[tokio::test]
async fn the_delete_refuses_what_it_cannot_delete() {
    let fixture = Fixture::new().await;
    // One live attachment the refusals must leave exactly as it is.
    let live = fixture.seed(attachment_state("available"), &plain_seed());
    let mut refusals: Vec<Refusal> = Vec::new();

    // Not a transit gateway attachment id at all: the wrong resource's prefix, no prefix, a
    // digit count outside the id form, upper-case hex and an empty suffix. The sentence is
    // entirely fixed — this refusal interpolates nothing a caller sent.
    for malformed in [
        "tgw-0123456789abcdef0",
        "not-an-attachment-id",
        "tgw-attach-0123456789ab",
        "tgw-attach-0123456789ABCDEF0",
        "tgw-attach-",
    ] {
        refusals.push(Refusal {
            label: format!("malformed attachment id {malformed:?}"),
            attachment_id: malformed.to_string(),
            code: INVALID_TRANSIT_GATEWAY_ATTACHMENT_ID_MALFORMED.to_string(),
            message: INVALID_TRANSIT_GATEWAY_ATTACHMENT_ID_MESSAGE.to_string(),
        });
    }

    // Well formed, but no attachment of the account is held under it. The sentence is the one
    // the transit gateway family answers for an attachment of its own that is not there, naming
    // the attachment this action's subject is.
    let unknown = format!("tgw-attach-{UNKNOWN_SUFFIX}");
    refusals.push(Refusal {
        label: "an attachment id naming nothing".to_string(),
        attachment_id: unknown.clone(),
        code: INVALID_TRANSIT_GATEWAY_ATTACHMENT_ID_NOT_FOUND.to_string(),
        message: format!(
            "{TRANSIT_GATEWAY_ATTACHMENT_RESOURCE_NAME} {unknown} was deleted or does not exist."
        ),
    });

    // An attachment of ANOTHER kind: every kind shares one id space, so this id names no
    // attachment THIS operation can delete.
    let other_kind = fixture.seed_other_kind();
    refusals.push(Refusal {
        label: "an attachment of another kind".to_string(),
        attachment_id: other_kind.clone(),
        code: INVALID_TRANSIT_GATEWAY_ATTACHMENT_ID_NOT_FOUND.to_string(),
        message: format!(
            "{TRANSIT_GATEWAY_ATTACHMENT_RESOURCE_NAME} {other_kind} was deleted or does not \
             exist."
        ),
    });

    // Every state in which the lifecycle finished with the attachment some OTHER way than by
    // deleting it, so there is nothing left for a delete to take away and nothing it asked for
    // has happened: the owner of the attached resource refused it, or it could not be made — and
    // the transient states on the way to each of those ends.
    for state in ["rejecting", "rejected", "failing", "failed"] {
        let attachment_id = fixture.seed(attachment_state(state), &plain_seed());
        refusals.push(Refusal {
            label: format!("an attachment in {state:?}"),
            attachment_id: attachment_id.clone(),
            code: INCORRECT_STATE.to_string(),
            message: format!("{attachment_id} is in invalid state"),
        });
    }

    let before = fixture.stored_states();
    for refusal in &refusals {
        let label = &refusal.label;
        let (status, body) = fixture.delete(&refusal.attachment_id, &[]).await;
        assert_eq!(
            status, REFUSAL_STATUS,
            "case {label:?}: expected HTTP {REFUSAL_STATUS}, got {status} with body {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, refusal.code,
            "case {label:?}: expected the code {:?}, got {code:?} with body {body}",
            refusal.code
        );
        let message = error_message(&body);
        assert_eq!(
            message, refusal.message,
            "case {label:?}: expected the message {:?}, got {message:?}",
            refusal.message
        );
        let now = fixture.stored_states();
        assert_eq!(
            now, before,
            "case {label:?}: a refusal must write nothing, got {now:?} where {before:?} was held"
        );
    }

    // The live attachment is untouched by every refusal above, and its own delete still works.
    let (status, body) = fixture.delete(&live, &[]).await;
    assert_eq!(
        status, 200,
        "the live attachment must still be deletable, got {status} with body {body}"
    );
}

/// A dry run of a request that would have succeeded is refused with `DryRunOperation` and writes
/// nothing; the id's own form is read before the permission check.
#[tokio::test]
async fn a_dry_run_writes_nothing() {
    let case = "DryRun";
    let fixture = Fixture::new().await;
    let attachment_id = fixture.seed(attachment_state("available"), &plain_seed());
    let before = fixture.stored_states();

    let (status, body) = fixture
        .delete(&attachment_id, &[param("DryRun", "true")])
        .await;
    assert_eq!(
        status, DRY_RUN_STATUS,
        "case {case:?}: expected HTTP {DRY_RUN_STATUS}, got {status} with body {body}"
    );
    let now = fixture.stored_states();
    assert_eq!(
        now, before,
        "case {case:?}: a dry run must write nothing, got {now:?} where {before:?} was held"
    );

    // The id's own form precedes the permission answer, so a dry run naming something that is
    // no attachment id is refused for the request instead.
    let (status, body) = fixture
        .delete("not-an-attachment-id", &[param("DryRun", "true")])
        .await;
    assert_eq!(
        status, REFUSAL_STATUS,
        "case {case:?}: a malformed id must be refused before the dry run, got {status} with \
         body {body}"
    );
    let code = error_code(&body);
    assert_eq!(
        code, INVALID_TRANSIT_GATEWAY_ATTACHMENT_ID_MALFORMED,
        "case {case:?}: expected the code \
         {INVALID_TRANSIT_GATEWAY_ATTACHMENT_ID_MALFORMED:?}, got {code:?}"
    );
}

/// A peering this delete has finished with joins nothing: the same pair of gateways can be
/// peered again, and the acceptance of the deleted attachment is refused for its state.
#[tokio::test]
async fn a_deleted_peering_joins_neither_of_its_gateways() {
    let case = "a deleted peering joins nothing";
    let fixture = Fixture::new().await;
    let attachment_id = fixture.create_peering(case).await;
    // The requester and accepter ends the create recorded, which the replacement must peer
    // again.
    let created = described(case, &fixture.router, &attachment_id).await;

    let (status, body) = fixture.delete(&attachment_id, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the delete must succeed, got {status} with body {body}"
    );
    poll_until_state(
        case,
        &fixture.router,
        &attachment_id,
        attachment_state("deleted"),
    )
    .await;

    // An attachment the delete has finished with admits no acceptance either: the acceptance
    // rule asks for `pendingAcceptance`, which this attachment has left for good.
    let (status, body) = invoke(
        &fixture.router,
        "AcceptTransitGatewayPeeringAttachment",
        &[param("TransitGatewayAttachmentId", &attachment_id)],
    )
    .await;
    assert_eq!(
        status, REFUSAL_STATUS,
        "case {case:?}: a deleted attachment must admit no acceptance, got {status} with body \
         {body}"
    );
    let code = error_code(&body);
    assert_eq!(
        code, INCORRECT_STATE,
        "case {case:?}: expected the code {INCORRECT_STATE:?}, got {code:?} with body {body}"
    );

    // The same pair of gateways is a new peering, not a duplicate of the deleted one.
    let again = ok_body(
        case,
        &fixture.router,
        "CreateTransitGatewayPeeringAttachment",
        &[
            param("TransitGatewayId", &created.requester.0),
            param("PeerTransitGatewayId", &created.accepter.0),
            param("PeerAccountId", engine::emulator_account_id()),
            param("PeerRegion", REGION),
        ],
    )
    .await;
    let replacement = reported(case, &again).attachment_id;
    assert_ne!(
        replacement, attachment_id,
        "case {case:?}: expected a freshly minted attachment, got the deleted one"
    );

    let (status, body) = fixture.delete(&replacement, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the replacement must be deletable, got {status} with body {body}"
    );
}
