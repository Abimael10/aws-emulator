// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `DescribeSpotFleetRequestHistory`, driven over the wire the service
//! actually speaks — `ec2Query`, through the crate's shared [`super::ec2_query`] harness.
//!
//! The rule under test: a Spot Fleet request's history is the events the FLEET has been
//! through, each recorded at the instant it happened — one `fleetRequestChange` event for every
//! `BatchState` the request entered, whose `EventSubType` is that state word, and one
//! `instanceChange` event naming the instance for every one its placements launched
//! (`launched`) or its cancellation terminated (`terminated`). A report answers those events
//! from `StartTime` on, narrowed to the one `EventType` a request names, oldest first, cut into
//! `MaxResults`-sized pages a `NextToken` continues, echoing the fleet it is of and the window
//! it was read over, and carrying the instant it was evaluated at on the page that ends the
//! walk. Reporting is a READ: two reports a moment apart carry the same records.
//!
//! Each table row is a distinct input along what the rule generalises over — how much target
//! capacity the fleet asks for, how many pools it spreads it over, whether its cancellation
//! terminates its instances, which `StartTime` the window begins at, which `EventType` it
//! narrows to, which page size it is cut into and which malformed id form is sent — so no row
//! is the specification.

use std::collections::BTreeSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, EVENT_SUB_TYPE_LAUNCHED, EVENT_SUB_TYPE_TERMINATED,
    EVENT_TYPE_FLEET_REQUEST_CHANGE, EVENT_TYPE_INSTANCE_CHANGE,
    SPOT_FLEET_ALLOCATION_STRATEGY_DIVERSIFIED, SPOT_FLEET_HISTORY_AVAILABILITY,
    SPOT_FLEET_STATE_ACTIVE, SPOT_FLEET_STATE_CANCELLED, SPOT_FLEET_STATE_CANCELLED_RUNNING,
    SPOT_FLEET_STATE_CANCELLED_TERMINATING, SPOT_FLEET_STATE_SUBMITTED, SpotFleetHistoryEvent,
};
use ec2::ops::types::EVENT_TYPE_VALUES;
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{as_timestamp, call, error_code, error_message, indexed};
use super::rbin_retention_rules::poll_until;
use super::wire::{child, child_text, document};

/// The Region every call here is signed for. A Spot Fleet request and the events it has been
/// through are the account's, so the Region only selects the ec2Query envelope.
const REGION: &str = "us-west-2";

/// The ARN of the IAM role a Spot Fleet request grants the fleet. Its account is NOT this
/// emulator's: the role is the caller's.
const FLEET_ROLE: &str = "arn:aws:iam::111122223333:role/aws-ec2-spot-fleet-tagging-role";

/// A well-formed Spot Fleet request id no fleet of the account is stored under: the mint counts
/// from one, so a high counter value names nothing.
const UNKNOWN_FLEET_ID: &str = "sfr-0000ffff-0000-0000-0000-00000000ffff";

/// One request's form parameters, as the ec2Query harness takes them.
type Params = Vec<(String, String)>;

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// One `SpotFleetRequestConfig.<member>` parameter.
fn c(member: &str, value: impl Into<String>) -> (String, String) {
    p(&format!("SpotFleetRequestConfig.{member}"), value)
}

fn emulator() -> (MultiServiceRouter, std::sync::Arc<engine::InMemoryEngine>) {
    super::ec2_query::emulator()
}

/// An instant as the ISO 8601 UTC form `StartTime` is documented in ("in UTC format (for
/// example, YYYY-MM-DDTHH:MM:SSZ)").
fn iso(at_seconds: f64) -> String {
    ::aws_smithy_types::DateTime::from_secs_f64(at_seconds)
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .expect("a representable instant")
}

/// Now, in seconds since the Unix epoch — the clock the handler reads too.
fn now_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// An AMI of the published catalog a fresh account can already see: read out of the service's
/// OWN state rather than written here, because a launch specification must name an image the
/// account can resolve.
fn image_id() -> String {
    Ec2State::default()
        .images
        .keys()
        .next()
        .cloned()
        .expect("a fresh account must publish at least one image")
}

/// One row of the fleet table: a fleet configuration and how many instances it ends up running.
struct FleetCase {
    case: &'static str,
    /// `SpotFleetRequestConfig.TargetCapacity` — "the number of units to request".
    target_capacity: u32,
    /// `SpotFleetRequestConfig.AllocationStrategy`, or `None` for the documented default.
    allocation_strategy: Option<&'static str>,
    /// One launch specification per entry: the pool's instance type and what one of its
    /// instances provides (`WeightedCapacity`, left unset when `None`).
    pools: &'static [(&'static str, Option<f64>)],
    /// How many instances the fleet launches in total: each pool's share of the target capacity
    /// divided by what one of its instances provides, rounded up.
    expected_instances: usize,
}

impl FleetCase {
    fn params(&self) -> Params {
        let mut params = vec![
            c("IamFleetRole", FLEET_ROLE),
            c("TargetCapacity", self.target_capacity.to_string()),
        ];
        if let Some(strategy) = self.allocation_strategy {
            params.push(c("AllocationStrategy", strategy));
        }
        for (index, (instance_type, weight)) in self.pools.iter().enumerate() {
            let position = index + 1;
            params.push(c(
                &format!("LaunchSpecifications.{position}.ImageId"),
                image_id(),
            ));
            params.push(c(
                &format!("LaunchSpecifications.{position}.InstanceType"),
                *instance_type,
            ));
            if let Some(weight) = weight {
                params.push(c(
                    &format!("LaunchSpecifications.{position}.WeightedCapacity"),
                    weight.to_string(),
                ));
            }
        }
        params
    }
}

/// The rows of the fleet table. No row repeats another's combination of target capacity, pool
/// count, pool weight or instance types, so the number of `launched` events differs per row.
const FLEET_CASES: &[FleetCase] = &[
    FleetCase {
        case: "one unit in one pool, at the default weight",
        target_capacity: 1,
        allocation_strategy: None,
        pools: &[("t3.micro", None)],
        expected_instances: 1,
    },
    FleetCase {
        case: "three units in one pool, one unit per instance",
        target_capacity: 3,
        allocation_strategy: None,
        pools: &[("t3.small", Some(1.0))],
        expected_instances: 3,
    },
    FleetCase {
        case: "four units in one pool whose instance provides two",
        target_capacity: 4,
        allocation_strategy: None,
        pools: &[("m5.large", Some(2.0))],
        expected_instances: 2,
    },
    FleetCase {
        case: "two units spread across two pools by the diversified strategy",
        target_capacity: 2,
        allocation_strategy: Some(SPOT_FLEET_ALLOCATION_STRATEGY_DIVERSIFIED),
        pools: &[("c5.large", Some(1.0)), ("t3.medium", Some(1.0))],
        expected_instances: 2,
    },
];

/// Request one Spot Fleet and answer the `SpotFleetRequestId` the service minted.
async fn request_fleet(case: &str, router: &MultiServiceRouter, params: &[(String, String)]) -> String {
    let (status, body) = call(router, REGION, "RequestSpotFleet", params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: RequestSpotFleet must succeed, got {status}: {body}"
    );
    let parsed = document(case, &body);
    child_text(parsed.root_element(), "spotFleetRequestId")
        .unwrap_or_else(|| panic!("case {case:?}: no spotFleetRequestId in {body}"))
}

/// One `DescribeSpotFleetRequestHistory` call.
async fn describe(router: &MultiServiceRouter, params: &[(String, String)]) -> (u16, String) {
    call(router, REGION, "DescribeSpotFleetRequestHistory", params).await
}

/// The whole history of one fleet, read from the Unix epoch on so nothing the fleet has done is
/// outside the window.
async fn describe_history(router: &MultiServiceRouter, fleet_id: &str) -> (u16, String) {
    describe(
        router,
        &[p("SpotFleetRequestId", fleet_id), p("StartTime", iso(0.0))],
    )
    .await
}

/// One `<item>` of the `historyRecordSet` a response carries.
#[derive(Debug, Clone, PartialEq)]
struct Record {
    event_type: Option<String>,
    event_sub_type: Option<String>,
    event_description: Option<String>,
    instance_id: Option<String>,
    timestamp: Option<f64>,
}

/// Every history record a response reports, in the order it reported them.
fn records(case: &str, body: &str) -> Vec<Record> {
    let parsed = document(case, body);
    let Some(set) = child(parsed.root_element(), "historyRecordSet") else {
        return Vec::new();
    };
    set.children()
        .filter(|node| node.is_element() && node.tag_name().name() == "item")
        .map(|item| {
            let information = child(item, "eventInformation");
            Record {
                event_type: child_text(item, "eventType"),
                event_sub_type: information.and_then(|node| child_text(node, "eventSubType")),
                event_description: information
                    .and_then(|node| child_text(node, "eventDescription")),
                instance_id: information.and_then(|node| child_text(node, "instanceId")),
                timestamp: child_text(item, "timestamp")
                    .and_then(|text| as_timestamp(&text)),
            }
        })
        .collect()
}

/// The `(eventType, eventSubType)` pair of every record, in report order.
fn kinds(case: &str, body: &str) -> Vec<(String, String)> {
    records(case, body)
        .into_iter()
        .map(|record| {
            (
                record.event_type.unwrap_or_default(),
                record.event_sub_type.unwrap_or_default(),
            )
        })
        .collect()
}

/// The text of one scalar member of the response envelope.
fn envelope(case: &str, body: &str, name: &str) -> Option<String> {
    let parsed = document(case, body);
    child_text(parsed.root_element(), name)
}

/// The instance ids the records of sub type `event_sub_type` name.
fn named_instances(case: &str, body: &str, event_sub_type: &str) -> BTreeSet<String> {
    records(case, body)
        .into_iter()
        .filter(|record| record.event_sub_type.as_deref() == Some(event_sub_type))
        .filter_map(|record| record.instance_id)
        .collect()
}

/// The instances one stored fleet's placements have launched, by the Spot Instance request
/// store — the account's own record of what the fleet launched, which the `launched` events must
/// name.
fn fleet_launched_instances(engine: &engine::InMemoryEngine, fleet_id: &str) -> BTreeSet<String> {
    let state = engine.state::<Ec2State>();
    let store = state.lock();
    let Some(record) = store.spot_fleet_requests.get(fleet_id) else {
        return BTreeSet::new();
    };
    record
        .placements
        .iter()
        .filter_map(|placement| {
            store
                .spot_instance_requests
                .get(&placement.spot_instance_request_id)
                .and_then(|request| request.view.instance_id.clone())
        })
        .collect()
}

/// How many events the store holds for one fleet.
fn stored_history_len(engine: &engine::InMemoryEngine, fleet_id: &str) -> usize {
    let state = engine.state::<Ec2State>();
    let store = state.lock();
    store
        .spot_fleet_requests
        .get(fleet_id)
        .map_or(0, |record| record.history.len())
}

/// Look again until the fleet's history holds every event its launch owes: the `active` the
/// clock moves it to, and one `launched` per instance its placements launch.
async fn poll_until_launched(
    case: &str,
    router: &MultiServiceRouter,
    fleet_id: &str,
    expected_instances: usize,
) -> String {
    poll_until(
        case,
        "the fleet to become active with its instances launched",
        || async {
            let (status, body) = describe_history(router, fleet_id).await;
            if status != 200 {
                return false;
            }
            let reported = kinds(case, &body);
            reported.iter().any(|(event_type, sub_type)| {
                event_type == EVENT_TYPE_FLEET_REQUEST_CHANGE
                    && sub_type == SPOT_FLEET_STATE_ACTIVE
            })
                && reported
                    .iter()
                    .filter(|(_, sub_type)| sub_type == EVENT_SUB_TYPE_LAUNCHED)
                    .count()
                    == expected_instances
        },
    )
    .await;
    // The events waited for are the clock's, and the history only grows, so the report read
    // back here is the one the condition held on.
    let (status, body) = describe_history(router, fleet_id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status}: {body}"
    );
    body
}

/// Look again until the fleet's history holds a `fleetRequestChange` event of sub type `wanted`.
async fn poll_until_state_event(
    case: &str,
    router: &MultiServiceRouter,
    fleet_id: &str,
    wanted: &str,
) -> String {
    poll_until(
        case,
        "the fleet to record the state it was moving to",
        || async {
            let (status, body) = describe_history(router, fleet_id).await;
            status == 200
                && kinds(case, &body).iter().any(|(event_type, sub_type)| {
                    event_type == EVENT_TYPE_FLEET_REQUEST_CHANGE && sub_type == wanted
                })
        },
    )
    .await;
    let (status, body) = describe_history(router, fleet_id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status}: {body}"
    );
    body
}

/// The rule, over the fleet table: the history is the events the fleet has been through, with
/// the state it entered or the instance that changed on each, oldest first.
#[tokio::test]
async fn the_history_is_the_events_the_fleet_has_been_through() {
    let (router, engine) = emulator();

    // Every row's fleet is requested first, so the clock's moves for all of them run together:
    // each fleet's events fall due on the clock, not on the order the rows are read in.
    let mut fleets: Vec<(&FleetCase, String)> = Vec::new();
    for row in FLEET_CASES {
        let fleet_id = request_fleet(row.case, &router, &row.params()).await;
        fleets.push((row, fleet_id));
    }

    // Straight after the request: the fleet has been submitted and nothing else has happened
    // yet, so its history is exactly that one event — and it names no instance, because "this
    // information is available only for instanceChange events".
    for (row, fleet_id) in &fleets {
        let case = row.case;
        let (status, body) = describe_history(&router, fleet_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a fleet that has only been submitted is still described, \
             got {status}: {body}"
        );
        let reported = records(case, &body);
        assert_eq!(
            kinds(case, &body),
            vec![(
                EVENT_TYPE_FLEET_REQUEST_CHANGE.to_string(),
                SPOT_FLEET_STATE_SUBMITTED.to_string()
            )],
            "case {case:?}: the first event of a fleet's history is its submission: {body}"
        );
        assert_eq!(
            reported.first().and_then(|record| record.instance_id.clone()),
            None,
            "case {case:?}: a fleetRequestChange event names no instance: {body}"
        );
        assert_eq!(
            envelope(case, &body, "spotFleetRequestId").as_deref(),
            Some(fleet_id.as_str()),
            "case {case:?}: the report names the fleet it is of: {body}"
        );
    }

    for (row, fleet_id) in &fleets {
        let case = row.case;
        let body = poll_until_launched(case, &router, fleet_id, row.expected_instances).await;
        let reported = records(case, &body);

        // One event per thing that happened: the submission, the `active` the clock moved the
        // fleet to, and one launch per instance the fleet's placements launched.
        assert_eq!(
            reported.len(),
            2 + row.expected_instances,
            "case {case:?}: expected {} event(s), got {reported:?}",
            2 + row.expected_instances
        );
        let state_events: Vec<String> = reported
            .iter()
            .filter(|record| record.event_type.as_deref() == Some(EVENT_TYPE_FLEET_REQUEST_CHANGE))
            .map(|record| record.event_sub_type.clone().unwrap_or_default())
            .collect();
        assert_eq!(
            state_events,
            vec![
                SPOT_FLEET_STATE_SUBMITTED.to_string(),
                SPOT_FLEET_STATE_ACTIVE.to_string()
            ],
            "case {case:?}: the states the request entered, in the order it entered them: {body}"
        );

        // The `launched` events name the instances the account's own stores say the fleet
        // launched — not a count, and not an id from anywhere else.
        assert_eq!(
            named_instances(case, &body, EVENT_SUB_TYPE_LAUNCHED),
            fleet_launched_instances(&engine, fleet_id),
            "case {case:?}: every launch names the instance it launched: {body}"
        );
        for record in &reported {
            match record.event_type.as_deref() {
                Some(EVENT_TYPE_INSTANCE_CHANGE) => assert!(
                    record.instance_id.is_some(),
                    "case {case:?}: an instanceChange event names its instance, got {record:?}"
                ),
                Some(EVENT_TYPE_FLEET_REQUEST_CHANGE) => assert_eq!(
                    record.instance_id, None,
                    "case {case:?}: a fleetRequestChange event names no instance, got {record:?}"
                ),
                other => panic!("case {case:?}: unexpected event type {other:?} in {body}"),
            }
            assert_eq!(
                record.event_description, None,
                "case {case:?}: this store holds no event text, so none is reported: {record:?}"
            );
            assert!(
                record.timestamp.is_some(),
                "case {case:?}: every event is reported with the instant it happened: {record:?}"
            );
        }

        // Oldest first, which is the order a page's token names a position in.
        let stamps: Vec<f64> = reported
            .iter()
            .map(|record| record.timestamp.unwrap_or_default())
            .collect();
        assert!(
            stamps.windows(2).all(|pair| pair[0] <= pair[1]),
            "case {case:?}: the history is reported oldest first, got {stamps:?}"
        );

        // "All records up to this time were retrieved": the page that ends the walk carries the
        // instant the history was evaluated at, which is never before an event it reports.
        assert_eq!(
            envelope(case, &body, "nextToken"),
            None,
            "case {case:?}: a page with nothing after it carries no token: {body}"
        );
        let evaluated = envelope(case, &body, "lastEvaluatedTime")
            .and_then(|text| as_timestamp(&text))
            .unwrap_or_else(|| panic!("case {case:?}: no lastEvaluatedTime in {body}"));
        let newest = stamps.iter().copied().fold(f64::MIN, f64::max);
        assert!(
            evaluated >= newest,
            "case {case:?}: expected a lastEvaluatedTime at or after {newest}, got {evaluated}"
        );

        // The window the report was read over is echoed as the request named it.
        assert_eq!(
            envelope(case, &body, "startTime").and_then(|text| as_timestamp(&text)),
            Some(0.0),
            "case {case:?}: the report echoes the StartTime it was read from: {body}"
        );
    }

    // One fleet's history never carries another fleet's instances: every instance of the account
    // was launched by exactly one of the fleets above.
    let mut seen: Vec<String> = Vec::new();
    for (_, fleet_id) in &fleets {
        let (_, body) = describe_history(&router, fleet_id).await;
        seen.extend(named_instances("every fleet", &body, EVENT_SUB_TYPE_LAUNCHED));
    }
    let unique: BTreeSet<String> = seen.iter().cloned().collect();
    assert_eq!(
        unique.len(),
        seen.len(),
        "no instance is reported in two fleets' histories, got {seen:?}"
    );
}

/// `StartTime` bounds the window and `EventType` narrows the events: "the type of events to
/// describe. By default, all events are described".
#[tokio::test]
async fn the_window_and_the_event_type_narrow_the_report() {
    let case = "three instances in one pool";
    let (router, _engine) = emulator();
    let row = FleetCase {
        case,
        target_capacity: 3,
        allocation_strategy: None,
        pools: &[("t3.large", Some(1.0))],
        expected_instances: 3,
    };
    let fleet_id = request_fleet(case, &router, &row.params()).await;
    let whole = poll_until_launched(case, &router, &fleet_id, row.expected_instances).await;
    let everything = records(case, &whole);
    let stamps: Vec<f64> = everything
        .iter()
        .map(|record| record.timestamp.unwrap_or_default())
        .collect();
    let oldest = stamps.iter().copied().fold(f64::MAX, f64::min);
    let newest = stamps.iter().copied().fold(f64::MIN, f64::max);

    // Each window, as a `StartTime` and the events that lie at or after it. The bounds are
    // chosen a whole second off an event's own instant, so no row turns on how a rendered
    // timestamp rounds.
    for (window, start) in [
        ("the Unix epoch", 0.0),
        ("a second before the first event", oldest - 1.0),
        ("a second after the first event", oldest + 1.0),
        ("a second after the last event", newest + 1.0),
        ("a day from now", now_seconds() + 86_400.0),
    ] {
        let params = vec![
            p("SpotFleetRequestId", fleet_id.clone()),
            p("StartTime", iso(start)),
        ];
        let (status, body) = describe(&router, &params).await;
        assert_eq!(
            status, 200,
            "case {window:?}: expected HTTP 200, got {status}: {body}"
        );
        let expected: Vec<Record> = everything
            .iter()
            .filter(|record| record.timestamp.unwrap_or_default() >= start)
            .cloned()
            .collect();
        assert_eq!(
            records(case, &body),
            expected,
            "case {window:?}: the report is the events at or after {start}: {body}"
        );
    }

    // Each member of the modelled `EventType` catalog, read off the generated catalog rather
    // than written out here: a type the fleet has events of reports exactly those, and one it
    // has none of reports an empty history rather than everything.
    for event_type in EVENT_TYPE_VALUES {
        let params = vec![
            p("SpotFleetRequestId", fleet_id.clone()),
            p("StartTime", iso(0.0)),
            p("EventType", *event_type),
        ];
        let (status, body) = describe(&router, &params).await;
        assert_eq!(
            status, 200,
            "case {event_type:?}: expected HTTP 200, got {status}: {body}"
        );
        let expected: Vec<Record> = everything
            .iter()
            .filter(|record| record.event_type.as_deref() == Some(*event_type))
            .cloned()
            .collect();
        assert_eq!(
            records(case, &body),
            expected,
            "case {event_type:?}: the report is this type's events alone: {body}"
        );
    }

    // A value outside the modelled catalog is refused before the handler runs, rather than read
    // as "every event".
    for event_type in ["fleetChange", "FLEETREQUESTCHANGE ", "instance-change"] {
        let params = vec![
            p("SpotFleetRequestId", fleet_id.clone()),
            p("StartTime", iso(0.0)),
            p("EventType", event_type.to_string()),
        ];
        let (status, body) = describe(&router, &params).await;
        assert_eq!(
            status, 400,
            "case {event_type:?}: an event type outside the catalog is refused, \
             got {status}: {body}"
        );
    }
}

/// "Spot Fleet events are available for 48 hours": an event older than that is outside the
/// window however early a `StartTime` asks for it, and one inside it is reported.
#[tokio::test]
async fn an_event_older_than_the_availability_window_is_not_reported() {
    let case = "one instance in one pool";
    let (router, engine) = emulator();
    let row = &FLEET_CASES[0];
    let fleet_id = request_fleet(case, &router, &row.params()).await;
    poll_until_launched(case, &router, &fleet_id, row.expected_instances).await;

    // Each side of the published window, and one event seeded at each: the store is written
    // directly, because waiting out 48 hours is not something a test does.
    let aged: &[(&str, Duration, &str)] = &[
        ("an hour old", Duration::from_secs(3_600), "i-0aaaa1111aaaa1111"),
        (
            "an hour inside the window",
            SPOT_FLEET_HISTORY_AVAILABILITY - Duration::from_secs(3_600),
            "i-0bbbb2222bbbb2222",
        ),
        (
            "an hour past the window",
            SPOT_FLEET_HISTORY_AVAILABILITY + Duration::from_secs(3_600),
            "i-0cccc3333cccc3333",
        ),
        (
            "a week old",
            Duration::from_secs(7 * 24 * 60 * 60),
            "i-0dddd4444dddd4444",
        ),
    ];
    {
        let state = engine.state::<Ec2State>();
        let mut store = state.lock();
        let record = store
            .spot_fleet_requests
            .get_mut(&fleet_id)
            .expect("the fleet must be stored");
        for (_, age, instance_id) in aged {
            record.history.push(SpotFleetHistoryEvent::instance_change(
                EVENT_SUB_TYPE_TERMINATED,
                instance_id,
                SystemTime::now() - *age,
            ));
        }
    }

    let (status, body) = describe_history(&router, &fleet_id).await;
    assert_eq!(status, 200, "case {case:?}: got {status}: {body}");
    let reported = named_instances(case, &body, EVENT_SUB_TYPE_TERMINATED);
    for (window, age, instance_id) in aged {
        let available = *age < SPOT_FLEET_HISTORY_AVAILABILITY;
        assert_eq!(
            reported.contains(*instance_id),
            available,
            "case {window:?}: an event {age:?} old is {} the window: {body}",
            if available { "inside" } else { "outside" }
        );
    }
}

/// A cancellation is a change of the request's status, and the instances it terminates are
/// instance changes of their own — while a cancellation that leaves them running terminates
/// nothing and never reaches `cancelled`.
#[tokio::test]
async fn a_cancellation_records_the_states_and_the_instances_it_ended() {
    let (router, engine) = emulator();
    let terminating = FleetCase {
        case: "cancelled with its instances",
        target_capacity: 2,
        allocation_strategy: None,
        pools: &[("c5.xlarge", Some(1.0))],
        expected_instances: 2,
    };
    let running = FleetCase {
        case: "cancelled with its instances left running",
        target_capacity: 1,
        allocation_strategy: None,
        pools: &[("m5.xlarge", Some(1.0))],
        expected_instances: 1,
    };

    let mut fleets: Vec<(&FleetCase, bool, String)> = Vec::new();
    for (row, terminate) in [(&terminating, true), (&running, false)] {
        let fleet_id = request_fleet(row.case, &router, &row.params()).await;
        fleets.push((row, terminate, fleet_id));
    }
    for (row, _, fleet_id) in &fleets {
        poll_until_launched(row.case, &router, fleet_id, row.expected_instances).await;
    }

    for (row, terminate, fleet_id) in &fleets {
        let case = row.case;
        let launched = fleet_launched_instances(&engine, fleet_id);
        let mut params = indexed("SpotFleetRequestId", &[fleet_id.as_str()]);
        params.push(p("TerminateInstances", if *terminate { "true" } else { "false" }));
        let (status, body) = call(&router, REGION, "CancelSpotFleetRequests", &params).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the cancellation must succeed, got {status}: {body}"
        );

        // The state the cancellation put the request in is one event of its history, and the
        // instances it terminated are one `instanceChange` each — a cancellation that leaves
        // them running terminates none, so it records none.
        let expected_state = if *terminate {
            SPOT_FLEET_STATE_CANCELLED_TERMINATING
        } else {
            SPOT_FLEET_STATE_CANCELLED_RUNNING
        };
        let body = poll_until_state_event(case, &router, fleet_id, expected_state).await;
        let expected_terminations: BTreeSet<String> = if *terminate {
            launched.clone()
        } else {
            BTreeSet::new()
        };
        assert_eq!(
            named_instances(case, &body, EVENT_SUB_TYPE_TERMINATED),
            expected_terminations,
            "case {case:?}: the terminations are the instances the cancellation ended: {body}"
        );

        // The states the request entered, in the order it entered them: the cancellation
        // follows the `active` it was holding capacity in.
        let state_events: Vec<String> = records(case, &body)
            .into_iter()
            .filter(|record| record.event_type.as_deref() == Some(EVENT_TYPE_FLEET_REQUEST_CHANGE))
            .map(|record| record.event_sub_type.unwrap_or_default())
            .collect();
        assert_eq!(
            state_events,
            vec![
                SPOT_FLEET_STATE_SUBMITTED.to_string(),
                SPOT_FLEET_STATE_ACTIVE.to_string(),
                expected_state.to_string()
            ],
            "case {case:?}: expected the states up to the cancellation: {body}"
        );
    }

    // A fleet cancelled WITH its instances reaches `cancelled` once they are gone, which is one
    // further event; the one whose instances were left running has none to wait for, so it is
    // still `cancelled_running` and records no `cancelled` at all.
    let (_, _, terminated_fleet) = &fleets[0];
    let (_, _, running_fleet) = &fleets[1];
    let body = poll_until_state_event(
        terminating.case,
        &router,
        terminated_fleet,
        SPOT_FLEET_STATE_CANCELLED,
    )
    .await;
    let state_events = kinds(terminating.case, &body);
    assert_eq!(
        state_events
            .iter()
            .filter(|(_, sub_type)| sub_type == SPOT_FLEET_STATE_CANCELLED)
            .count(),
        1,
        "case {:?}: the fleet reaches cancelled once: {body}",
        terminating.case
    );
    let (status, body) = describe_history(&router, running_fleet).await;
    assert_eq!(
        status, 200,
        "case {:?}: got {status}: {body}",
        running.case
    );
    assert!(
        !kinds(running.case, &body)
            .iter()
            .any(|(_, sub_type)| sub_type == SPOT_FLEET_STATE_CANCELLED),
        "case {:?}: a request whose instances still run has not been cancelled: {body}",
        running.case
    );
}

/// `MaxResults` cuts the history into pages a `NextToken` continues, oldest event first, and a
/// token this report did not mint is refused.
#[tokio::test]
async fn the_history_pages_in_event_order() {
    let case = "three instances in one pool";
    let (router, _engine) = emulator();
    let row = FleetCase {
        case,
        target_capacity: 3,
        allocation_strategy: None,
        pools: &[("r5.large", Some(1.0))],
        expected_instances: 3,
    };
    let other = FleetCase {
        case: "another fleet of the account",
        target_capacity: 1,
        allocation_strategy: None,
        pools: &[("t3.nano", Some(1.0))],
        expected_instances: 1,
    };
    let fleet_id = request_fleet(case, &router, &row.params()).await;
    let second = request_fleet(other.case, &router, &other.params()).await;
    let whole = poll_until_launched(case, &router, &fleet_id, row.expected_instances).await;
    poll_until_launched(other.case, &router, &second, other.expected_instances).await;
    let everything = records(case, &whole);
    assert_eq!(
        everything.len(),
        5,
        "case {case:?}: the whole history is five events: {whole}"
    );

    // Each page size the history can be cut into, and the pages it answers.
    let mut tokens: Vec<String> = Vec::new();
    for page_size in [1usize, 2, 3, 4, 5, 6] {
        let mut walked: Vec<Record> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0usize;
        loop {
            let mut params = vec![
                p("SpotFleetRequestId", fleet_id.clone()),
                p("StartTime", iso(0.0)),
                p("MaxResults", page_size.to_string()),
            ];
            if let Some(token) = token.clone() {
                params.push(p("NextToken", token));
            }
            let (status, body) = describe(&router, &params).await;
            assert_eq!(
                status, 200,
                "case {case:?}: MaxResults={page_size} must succeed, got {status}: {body}"
            );
            let page = records(case, &body);
            assert!(
                page.len() <= page_size,
                "case {case:?}: a page of at most {page_size}, got {page:?}"
            );
            walked.extend(page);
            pages += 1;
            assert!(
                pages <= 1 + everything.len(),
                "case {case:?}: MaxResults={page_size} must terminate, walked {walked:?}"
            );
            match envelope(case, &body, "nextToken") {
                None => {
                    assert!(
                        envelope(case, &body, "lastEvaluatedTime").is_some(),
                        "case {case:?}: the page that ends the walk reports up to when it \
                         retrieved: {body}"
                    );
                    break;
                }
                Some(next) => {
                    assert_eq!(
                        envelope(case, &body, "lastEvaluatedTime"),
                        None,
                        "case {case:?}: a page with more to come reports no \
                         lastEvaluatedTime: {body}"
                    );
                    tokens.push(next.clone());
                    token = Some(next);
                }
            }
        }
        assert_eq!(
            walked, everything,
            "case {case:?}: MaxResults={page_size} walks the whole history in event order"
        );
    }

    // The published bounds of `DescribeSpotFleetRequestHistoryMaxResults` (`range 1..1000`) and
    // one past each: a value outside them is refused by the modelled range.
    for (max_results, expected_status) in [(0, 400), (1, 200), (1000, 200), (1001, 400)] {
        let params = vec![
            p("SpotFleetRequestId", fleet_id.clone()),
            p("StartTime", iso(0.0)),
            p("MaxResults", max_results.to_string()),
        ];
        let (status, body) = describe(&router, &params).await;
        assert_eq!(
            status, expected_status,
            "case MaxResults={max_results}: expected HTTP {expected_status}, got {status}: {body}"
        );
    }

    // A token that is not one this report issued is refused rather than read as a position.
    for token in ["not-a-token", "aW52YWxpZA", ""] {
        let params = vec![
            p("SpotFleetRequestId", fleet_id.clone()),
            p("StartTime", iso(0.0)),
            p("NextToken", token.to_string()),
        ];
        let (status, body) = describe(&router, &params).await;
        assert_eq!(
            status, 400,
            "case {token:?}: an unreadable token is refused, got {status}: {body}"
        );
    }

    // A token minted while paging ONE fleet's history names no position in another's, so it is
    // refused there rather than resumed from.
    let borrowed = tokens
        .first()
        .cloned()
        .expect("paging the history must have issued a token");
    let params = vec![
        p("SpotFleetRequestId", second.clone()),
        p("StartTime", iso(0.0)),
        p("NextToken", borrowed),
    ];
    let (status, body) = describe(&router, &params).await;
    assert_eq!(
        status, 400,
        "case {:?}: another fleet's token is refused, got {status}: {body}",
        other.case
    );
}

/// An id that is not a Spot Fleet request id at all is refused BEFORE any lookup, a well-formed
/// id of no fleet is refused as not found, a dry run answers only for a request that would have
/// succeeded, and reporting is a READ.
#[tokio::test]
async fn ids_are_refused_in_order_and_the_report_is_a_read() {
    let (router, engine) = emulator();
    let row = &FLEET_CASES[0];
    let fleet_id = request_fleet(row.case, &router, &row.params()).await;
    let held = poll_until_launched(row.case, &router, &fleet_id, row.expected_instances).await;

    // The malformed forms: not a UUID at all, another resource's prefix, the `<prefix>-<hex>`
    // form every other EC2 resource id takes, a UUID group of the wrong length, uppercase
    // hexadecimal, and a stored id with something after its last group.
    for named_id in [
        "sfr-not-a-uuid".to_string(),
        "fleet-0000ffff-0000-0000-0000-00000000ffff".to_string(),
        "sfr-00000000000000001".to_string(),
        "sfr-0000ffff-0000-0000-0000-00000000fff".to_string(),
        "sfr-0000FFFF-0000-0000-0000-00000000ffff".to_string(),
        format!("{fleet_id}-tail"),
    ] {
        let params = vec![p("SpotFleetRequestId", named_id.clone()), p("StartTime", iso(0.0))];
        let (status, body) = describe(&router, &params).await;
        assert_eq!(
            status, 400,
            "case {named_id:?}: expected HTTP 400, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidSpotFleetRequestId.Malformed",
            "case {named_id:?}: expected the malformed code, got {body}"
        );
    }

    // A well-formed id of no fleet of the account: not an empty history, and not "malformed".
    for named_id in [
        UNKNOWN_FLEET_ID,
        "sfr-00000000-0000-0000-0000-000000000000",
        "sfr-abcdef01-0000-0000-0000-0123456789ab",
    ] {
        let params = vec![p("SpotFleetRequestId", named_id), p("StartTime", iso(0.0))];
        let (status, body) = describe(&router, &params).await;
        assert_eq!(
            status, 400,
            "case {named_id:?}: expected HTTP 400, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidSpotFleetRequestId.NotFound",
            "case {named_id:?}: expected the not-found code, got {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("The SpotFleetRequestId(s) {named_id} do not exist."),
            "case {named_id:?}: expected AWS's wording for an id of no fleet, got {body}"
        );
    }

    // A dry run of a request that would have succeeded reports that, and reports nothing else.
    let params = vec![
        p("SpotFleetRequestId", fleet_id.clone()),
        p("StartTime", iso(0.0)),
        p("DryRun", "true"),
    ];
    let (status, body) = describe(&router, &params).await;
    assert_eq!(
        status, 412,
        "a dry run is refused with 412, got {status}: {body}"
    );
    assert_eq!(
        error_code(&body),
        "DryRunOperation",
        "expected DryRunOperation, got {body}"
    );

    // A dry run of a request that would have been refused is refused with THAT reason.
    for (named_id, expected_code) in [
        (UNKNOWN_FLEET_ID, "InvalidSpotFleetRequestId.NotFound"),
        ("sfr-not-a-uuid", "InvalidSpotFleetRequestId.Malformed"),
    ] {
        let params = vec![
            p("SpotFleetRequestId", named_id),
            p("StartTime", iso(0.0)),
            p("DryRun", "true"),
        ];
        let (status, body) = describe(&router, &params).await;
        assert_eq!(
            status, 400,
            "case {named_id:?}: a dry run of a doomed request is refused with its own reason, \
             got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            expected_code,
            "case {named_id:?}: expected {expected_code}, got {body}"
        );
    }

    // Reporting is a read: the second report carries the records the first did, and the store
    // holds exactly the events it held before anybody asked.
    let before = stored_history_len(&engine, &fleet_id);
    let (status, again) = describe_history(&router, &fleet_id).await;
    assert_eq!(status, 200, "case {:?}: got {status}: {again}", row.case);
    assert_eq!(
        records(row.case, &again),
        records(row.case, &held),
        "case {:?}: two reports of one history answer the same",
        row.case
    );
    assert_eq!(
        stored_history_len(&engine, &fleet_id),
        before,
        "case {:?}: reporting the history recorded nothing",
        row.case
    );
}
