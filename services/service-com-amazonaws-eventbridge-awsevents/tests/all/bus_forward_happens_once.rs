// SPDX-License-Identifier: CC0-1.0

//! A bus-to-bus forward happens AT MOST ONCE per event, end to end over the wire
//! through the real `MultiServiceRouter` with Amazon SQS, IAM and CloudWatch
//! mounted beside EventBridge — so what each bus did with the event is asserted
//! where a client observes it: on the queue each bus's own rule delivers to.
//!
//! The RULE asserted here: a rule whose target names another event bus publishes
//! the event onto that bus, where that bus's rules match it and deliver it to
//! their targets — but the copy that arrived BY that forward is not forwarded on,
//! so a bus target of the receiving bus's own rule publishes nothing. Buses
//! therefore cannot be chained: whatever the shape of the mesh a client wires, one
//! published event crosses at most one bus-to-bus link, every bus sees it at most
//! once, and the copy that does cross carries the event the publisher published —
//! its `source`, `detail-type`, `detail` and `resources` unchanged.
//!
//! The table spans the class rather than the one recorded case: the rows differ
//! along the SHAPE of the mesh (a chain of three buses, a longer chain, a ring of
//! two buses that name each other, one bus forwarding to two others), along WHICH
//! bus the event is published on (the head of a chain, its middle), and so along
//! how many links each copy would have to cross to reach a bus.
//!
//! The harness (the router with EventBridge, SQS, IAM and CloudWatch on one
//! engine) is `tests/all/target_dispatch.rs`'s, and the bus and rule helpers are
//! `tests/all/bus_forward_authorization.rs`'s, which every suite that NAMES the
//! bus a rule is on shares.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_eventbridge_awsevents::state::SERVICE_PRINCIPAL;

use crate::bus_forward_authorization::{bus_arn, given_bus, given_rule};
use crate::target_dispatch::{
    drain, emulator, events, given_queue, given_role, Emulator, DETAIL_TYPE, SOURCE,
};

/// The role every forward below is made under: its own inline policy allows the
/// publication, so no row turns on the identity half of the decision.
const FORWARDING_ROLE: &str = "forward-once-role";
const PUT_EVENTS_ACTION: &str = "events:PutEvents";

/// How long a delivery is waited for before a sink is read as not having
/// received: generous, because what is asserted is WHICH sinks received rather
/// than how fast the host is.
const DELIVERY_BUDGET: Duration = Duration::from_secs(30);

/// How often the sinks are read again while waiting for that condition.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// One mesh of buses: which buses exist, which of them forwards to which (by
/// index), the bus the event is published on, and how many copies each bus's own
/// sink must end up holding.
struct Row {
    case: &'static str,
    buses: &'static [&'static str],
    /// `(from, to)` pairs, each a bus target on `from`'s rule naming `to`.
    links: &'static [(usize, usize)],
    published_on: usize,
    /// One count per bus of `buses`, in that order.
    copies: &'static [usize],
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a chain of three buses, published on the head",
            buses: &["chain-a", "chain-b", "chain-c"],
            links: &[(0, 1), (1, 2)],
            published_on: 0,
            // The publication reaches the first bus's rules, one forward reaches
            // the second, and the third is never reached: the copy on the second
            // bus arrived by a forward, so that bus does not forward it on.
            copies: &[1, 1, 0],
        },
        Row {
            case: "a chain of four buses, published on the head",
            buses: &["long-a", "long-b", "long-c", "long-d"],
            links: &[(0, 1), (1, 2), (2, 3)],
            published_on: 0,
            copies: &[1, 1, 0, 0],
        },
        Row {
            case: "a chain of three buses, published on the middle one",
            buses: &["mid-a", "mid-b", "mid-c"],
            links: &[(0, 1), (1, 2)],
            published_on: 1,
            // A publication is not a forward, so the bus it is made on forwards
            // it once — onto the third bus, which then forwards nothing. The
            // first bus holds no copy: nothing forwards onto it.
            copies: &[0, 1, 1],
        },
        Row {
            case: "a ring of two buses naming each other",
            buses: &["ring-a", "ring-b"],
            links: &[(0, 1), (1, 0)],
            published_on: 0,
            // The forward onto the second bus happens; the copy there is not
            // forwarded back, so the first bus sees the event once — the ring
            // cannot carry it round.
            copies: &[1, 1],
        },
        Row {
            case: "one bus forwarding onto two others",
            buses: &["fan-a", "fan-b", "fan-c"],
            links: &[(0, 1), (0, 2)],
            published_on: 0,
            // Both links are a FIRST hop for the event, so each of the two
            // destination buses receives one copy.
            copies: &[1, 1, 1],
        },
        Row {
            case: "one bus forwarding onto two others, each forwarding on again",
            buses: &["wide-a", "wide-b", "wide-c", "wide-d"],
            links: &[(0, 1), (0, 2), (1, 3), (2, 3)],
            published_on: 0,
            // The fourth bus is reached only from a bus that received the event
            // by a forward, so neither of the two links onto it publishes.
            copies: &[1, 1, 1, 0],
        },
    ]
}

/// The detail every publication below carries, naming the row so a delivered
/// envelope is read as the event the row published.
fn detail(case: &str) -> Value {
    json!({ "nonce": case, "orderId": "A-1" })
}

/// Publish one event onto the bus `bus`, carrying the row's own detail.
async fn publish(emulator: &Emulator, case: &str, bus: &str) {
    let (status, body) = events(
        emulator,
        "PutEvents",
        &json!({
            "Entries": [{
                "EventBusName": bus,
                "Source": SOURCE,
                "DetailType": DETAIL_TYPE,
                "Detail": detail(case).to_string(),
            }],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutEvents answered {status} {body}"
    );
    assert_eq!(
        body["FailedEntryCount"],
        json!(0),
        "case {case:?}: PutEvents reported a failed entry: {body}"
    );
}

/// Everything the queue at `url` holds once it holds `want` messages, so a
/// delivery still in flight is read again rather than read as missing; gives up
/// after [`DELIVERY_BUDGET`] so a delivery that never arrives fails the
/// assertion that follows instead of hanging.
async fn held_once(emulator: &Emulator, url: &str, want: usize) -> Vec<Value> {
    let deadline = Instant::now() + DELIVERY_BUDGET;
    let mut held: Vec<Value> = Vec::new();
    loop {
        held.extend(drain(emulator, url).await);
        if held.len() >= want || Instant::now() >= deadline {
            return held;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The `detail` of each message of `messages`, with the envelope members a
/// forward must carry over asserted on every one of them.
fn details(case: &str, bus: &str, messages: &[Value]) -> Vec<Value> {
    messages
        .iter()
        .map(|message| {
            let text = message["Body"].as_str().unwrap_or_default();
            let envelope: Value = serde_json::from_str(text).unwrap_or_else(|error| {
                panic!("case {case:?}: a body delivered on {bus} is not JSON ({error}): {text:?}")
            });
            assert_eq!(
                envelope["source"],
                json!(SOURCE),
                "case {case:?}: the copy delivered on {bus} carries another source: {envelope}"
            );
            assert_eq!(
                envelope["detail-type"],
                json!(DETAIL_TYPE),
                "case {case:?}: the copy delivered on {bus} carries another detail-type: \
                 {envelope}"
            );
            assert_eq!(
                envelope["resources"],
                json!([]),
                "case {case:?}: the copy delivered on {bus} carries other resources: {envelope}"
            );
            envelope["detail"].clone()
        })
        .collect()
}

/// One published event crosses at most one bus-to-bus link, whatever mesh of bus
/// targets the buses are wired into.
#[tokio::test]
async fn a_forwarded_event_is_never_forwarded_on() {
    for row in rows() {
        assert_eq!(
            row.buses.len(),
            row.copies.len(),
            "case {:?}: the table states one expected count per bus",
            row.case
        );
        let case = row.case;
        let emulator = emulator();
        let role_arn = given_role(
            &emulator,
            FORWARDING_ROLE,
            SERVICE_PRINCIPAL,
            Some(PUT_EVENTS_ACTION),
        )
        .await;

        // Every bus of the mesh, each with the sink its own rule delivers to, so
        // what reached a bus is read off that bus's own queue.
        let mut sinks: BTreeMap<&str, String> = BTreeMap::new();
        let mut sink_arns: BTreeMap<&str, String> = BTreeMap::new();
        for bus in row.buses {
            given_bus(&emulator, case, bus).await;
            let (url, arn) = given_queue(&emulator, &format!("{bus}-sink")).await;
            sinks.insert(bus, url);
            sink_arns.insert(bus, arn);
        }
        // One rule per bus: its sink, plus a bus target for every link out of it.
        for (index, bus) in row.buses.iter().enumerate() {
            let mut targets = vec![json!({
                "Id": "sink",
                "Arn": sink_arns.get(bus).expect("the sink of this bus was created"),
            })];
            for (link, (from, to)) in row.links.iter().enumerate() {
                if *from != index {
                    continue;
                }
                targets.push(json!({
                    "Id": format!("bus-{link}"),
                    "Arn": bus_arn(row.buses[*to]),
                    "RoleArn": &role_arn,
                }));
            }
            given_rule(
                &emulator,
                case,
                bus,
                &format!("{bus}-rule"),
                Value::Array(targets),
            )
            .await;
        }

        publish(&emulator, case, row.buses[row.published_on]).await;

        // The sinks that must receive are read FIRST, each until it holds what the
        // row states: once every one of them has, the routing and the delivery of
        // this publication are done, so a sink the row says receives nothing is
        // read as empty rather than as not yet delivered to.
        let mut held: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
        for (bus, want) in row.buses.iter().zip(row.copies) {
            if *want == 0 {
                continue;
            }
            let url = sinks.get(bus).expect("the sink of this bus was created");
            held.insert(bus, held_once(&emulator, url, *want).await);
        }
        // Only then every sink again, the ones the row expects nothing on
        // included: a copy a further hop would have delivered is read here rather
        // than left on the queue.
        for bus in row.buses {
            let url = sinks.get(bus).expect("the sink of this bus was created");
            let extra = drain(&emulator, url).await;
            held.entry(bus).or_default().extend(extra);
        }

        for (bus, want) in row.buses.iter().zip(row.copies) {
            let messages = held.remove(bus).unwrap_or_default();
            let delivered = details(case, bus, &messages);
            let expected: Vec<Value> = (0..*want).map(|_| detail(case)).collect();
            assert_eq!(
                delivered, expected,
                "case {case:?}: expected {want} copy/copies of the event on {bus}, got \
                 {delivered:?}"
            );
        }
    }
}
