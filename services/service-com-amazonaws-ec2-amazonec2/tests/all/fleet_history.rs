// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `DescribeFleetHistory`, driven over the wire the service actually
//! speaks — `ec2Query`, through the crate's shared [`super::ec2_query`] harness.
//!
//! The rule under test: an EC2 Fleet's history is the events the FLEET has been through, each
//! recorded at the instant it happened — one `fleet-change` event for every `FleetStateCode`
//! the fleet entered, whose `EventSubType` is that state word, and one `instance-change` event
//! naming the instance for every one the fleet placed (`launched`) or its deletion terminated
//! (`terminated`). A report answers those events from `StartTime` on, as far back as they are
//! available for and up to the instant it reports as evaluated — an event is published only
//! after the span the operation documents, so one recorded inside that span is in no report
//! yet — narrowed to the one `EventType` a request names, oldest first, cut into
//! `MaxResults`-sized pages a `NextToken` continues, and echoing the fleet it is of and the
//! window it was read over. Reporting is a READ: two reports a moment apart carry the same
//! records.
//!
//! Each table row is a distinct input along what the rule generalises over — which fleet type,
//! how much capacity of which kind the fleet asks for, how many pools it spreads it over,
//! whether its deletion terminates its instances, which `StartTime` the window begins at, which
//! `EventType` it narrows to, which page size it is cut into and which malformed id form is
//! sent — so no row is the specification.

use std::collections::BTreeSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ec2::ops::types::FLEET_EVENT_TYPE_VALUES;
use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, EVENT_SUB_TYPE_LAUNCHED, EVENT_SUB_TYPE_TERMINATED,
    FLEET_EVENT_TYPE_FLEET_CHANGE, FLEET_EVENT_TYPE_INSTANCE_CHANGE, FLEET_HISTORY_AVAILABILITY,
    FLEET_HISTORY_PUBLICATION_DELAY, FLEET_STATE_ACTIVE, FLEET_STATE_DELETED,
    FLEET_STATE_DELETED_RUNNING, FLEET_STATE_DELETED_TERMINATING, FLEET_STATE_SUBMITTED,
    FLEET_TYPE_INSTANT, FLEET_TYPE_MAINTAIN, FLEET_TYPE_REQUEST,
    SPOT_ALLOCATION_STRATEGY_DIVERSIFIED, fleet_instance_change_event,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{as_timestamp, call, element, error_code, error_message, indexed};
use super::rbin_retention_rules::poll_until;
use super::wire::{child, child_text, document};

/// The Region every call here is signed for. A fleet and the events it has been through are the
/// account's, so the Region only selects the ec2Query envelope.
const REGION: &str = "us-west-2";

/// A well-formed EC2 Fleet id no fleet of the account is stored under: the mint counts from one,
/// so a high counter value names nothing.
const UNKNOWN_FLEET_ID: &str = "fleet-0000ffff-0000-0000-0000-00000000ffff";

/// One request's form parameters, as the ec2Query harness takes them.
type Params = Vec<(String, String)>;

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

fn emulator() -> (MultiServiceRouter, std::sync::Arc<engine::InMemoryEngine>) {
    super::ec2_query::emulator()
}

/// An AMI of the published catalog a fresh account can already see: read out of the service's
/// OWN state rather than written here, because a launch template must name an image the account
/// can resolve.
fn image_id() -> String {
    Ec2State::default()
        .images
        .keys()
        .next()
        .cloned()
        .expect("a fresh account must publish at least one image")
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

/// One row of the fleet table: a fleet configuration and how many instances it ends up running.
struct FleetCase {
    case: &'static str,
    /// `Type` — one of the modelled `FleetType` members.
    fleet_type: &'static str,
    /// `TargetCapacitySpecification.TotalTargetCapacity` — "the number of units to request".
    total_target_capacity: i32,
    /// Extra parameters beside the launch template configuration, the type and the capacity.
    extra: fn() -> Params,
    /// How many instances the fleet places, which is how many `launched` events it records.
    expected_instances: usize,
}

impl FleetCase {
    /// Whether this fleet is one AWS takes on ASYNCHRONOUSLY, so the clock owes it `active`:
    /// "`instant` - The EC2 Fleet places a synchronous one-time request for your desired
    /// capacity" (`FleetData$Type`), and every other type places an asynchronous one.
    fn asynchronous(&self) -> bool {
        self.fleet_type != FLEET_TYPE_INSTANT
    }

    /// The `FleetStateCode` members this fleet enters while it is being created, in order: the
    /// state it is stored in, plus the `active` the clock owes an asynchronous one.
    fn expected_states(&self) -> Vec<String> {
        if self.asynchronous() {
            vec![
                FLEET_STATE_SUBMITTED.to_string(),
                FLEET_STATE_ACTIVE.to_string(),
            ]
        } else {
            vec![FLEET_STATE_ACTIVE.to_string()]
        }
    }
}

/// The rows of the fleet table. No row repeats another's combination of fleet type, capacity
/// type, pool count or target capacity, so the events each one records differ per row.
const FLEET_CASES: &[FleetCase] = &[
    FleetCase {
        case: "a maintain fleet of one Spot unit",
        fleet_type: FLEET_TYPE_MAINTAIN,
        total_target_capacity: 1,
        extra: Vec::new,
        expected_instances: 1,
    },
    FleetCase {
        case: "a request fleet of three Spot units in one pool",
        fleet_type: FLEET_TYPE_REQUEST,
        total_target_capacity: 3,
        extra: Vec::new,
        expected_instances: 3,
    },
    FleetCase {
        case: "a maintain fleet of one On-Demand and one Spot unit",
        fleet_type: FLEET_TYPE_MAINTAIN,
        total_target_capacity: 2,
        extra: || {
            vec![
                p("TargetCapacitySpecification.OnDemandTargetCapacity", "1"),
                p("TargetCapacitySpecification.SpotTargetCapacity", "1"),
            ]
        },
        expected_instances: 2,
    },
    FleetCase {
        case: "an instant fleet of two On-Demand units",
        fleet_type: FLEET_TYPE_INSTANT,
        total_target_capacity: 2,
        extra: || vec![p("TargetCapacitySpecification.OnDemandTargetCapacity", "2")],
        expected_instances: 2,
    },
    FleetCase {
        case: "a request fleet spreading two units across two pools",
        fleet_type: FLEET_TYPE_REQUEST,
        total_target_capacity: 2,
        extra: || {
            vec![
                p(
                    "SpotOptions.AllocationStrategy",
                    SPOT_ALLOCATION_STRATEGY_DIVERSIFIED,
                ),
                p("LaunchTemplateConfigs.1.Overrides.1.InstanceType", "t3.small"),
                p("LaunchTemplateConfigs.1.Overrides.2.InstanceType", "c5.large"),
            ]
        },
        expected_instances: 2,
    },
    FleetCase {
        case: "a maintain fleet asking for no capacity yet",
        fleet_type: FLEET_TYPE_MAINTAIN,
        total_target_capacity: 0,
        extra: Vec::new,
        expected_instances: 0,
    },
];

/// Create one launch template naming `instance_type` and answer its `LaunchTemplateId`.
async fn launch_template(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, body) = call(
        router,
        REGION,
        "CreateLaunchTemplate",
        &[
            p("LaunchTemplateName", name),
            p("LaunchTemplateData.ImageId", image_id()),
            p("LaunchTemplateData.InstanceType", "t3.micro"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateLaunchTemplate must succeed, got {status}: {body}"
    );
    element(&body, "launchTemplateId")
        .unwrap_or_else(|| panic!("case {case:?}: no launchTemplateId in {body}"))
}

/// Create the fleet one row describes and answer the `FleetId` the service minted.
async fn create_fleet(router: &MultiServiceRouter, row: &FleetCase, name: &str) -> String {
    let template_id = launch_template(router, row.case, name).await;
    let mut params = vec![
        p(
            "LaunchTemplateConfigs.1.LaunchTemplateSpecification.LaunchTemplateId",
            template_id,
        ),
        p("Type", row.fleet_type),
        p(
            "TargetCapacitySpecification.TotalTargetCapacity",
            row.total_target_capacity.to_string(),
        ),
    ];
    params.extend((row.extra)());
    let (status, body) = call(router, REGION, "CreateFleet", &params).await;
    assert_eq!(
        status, 200,
        "case {:?}: CreateFleet must succeed, got {status}: {body}",
        row.case
    );
    element(&body, "fleetId").unwrap_or_else(|| panic!("case {:?}: no fleetId in {body}", row.case))
}

/// One `DescribeFleetHistory` call.
async fn describe(router: &MultiServiceRouter, params: &[(String, String)]) -> (u16, String) {
    call(router, REGION, "DescribeFleetHistory", params).await
}

/// The whole history of one fleet, read from the Unix epoch on so nothing the fleet has done is
/// outside the window.
async fn describe_history(router: &MultiServiceRouter, fleet_id: &str) -> (u16, String) {
    describe(
        router,
        &[p("FleetId", fleet_id), p("StartTime", iso(0.0))],
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
                timestamp: child_text(item, "timestamp").and_then(|text| as_timestamp(&text)),
            }
        })
        .collect()
}

/// The `FleetStateCode` words the `fleet-change` records name, in report order.
fn state_events(case: &str, body: &str) -> Vec<String> {
    records(case, body)
        .into_iter()
        .filter(|record| record.event_type.as_deref() == Some(FLEET_EVENT_TYPE_FLEET_CHANGE))
        .map(|record| record.event_sub_type.unwrap_or_default())
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

/// The instances one stored fleet placed, by the fleet store — the account's own record of what
/// the fleet launched, which the `launched` events must name.
fn fleet_placed_instances(engine: &engine::InMemoryEngine, fleet_id: &str) -> BTreeSet<String> {
    let state = engine.state::<Ec2State>();
    let store = state.lock();
    store
        .fleets
        .get(fleet_id)
        .map(|record| {
            record
                .instances
                .iter()
                .map(|placed| placed.instance_id.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// How many events the store holds for one fleet.
fn stored_history_len(engine: &engine::InMemoryEngine, fleet_id: &str) -> usize {
    let state = engine.state::<Ec2State>();
    let store = state.lock();
    store
        .fleets
        .get(fleet_id)
        .map_or(0, |record| record.history.len())
}

/// The `FleetStateCode` words the STORE holds `fleet-change` events for, in record order: what
/// the fleet has been through, before the publication delay decides which of those a report may
/// carry yet.
fn stored_state_events(engine: &engine::InMemoryEngine, fleet_id: &str) -> Vec<String> {
    let state = engine.state::<Ec2State>();
    let store = state.lock();
    store
        .fleets
        .get(fleet_id)
        .map(|record| {
            record
                .history
                .iter()
                .filter(|event| event.event_type == FLEET_EVENT_TYPE_FLEET_CHANGE)
                .map(|event| event.event_sub_type.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Move every event the store holds for one fleet back by `by`.
///
/// "EC2 Fleet events are delayed by up to 30 seconds before they can be described"
/// (`DescribeFleetHistory`), so the events the service's own writers have just recorded are not
/// describable yet. The events stay the ones those writers recorded — only the instants they
/// happened at move — so a case can read the report the published delay lets a caller see
/// without the test waiting out that delay.
fn age_history(engine: &engine::InMemoryEngine, fleet_id: &str, by: Duration) {
    let state = engine.state::<Ec2State>();
    let mut store = state.lock();
    let record = store
        .fleets
        .get_mut(fleet_id)
        .unwrap_or_else(|| panic!("the store must hold the fleet {fleet_id}"));
    for event in &mut record.history {
        event.at -= by;
    }
}

/// Enough older than the published delay that every aged event is describable, by a margin no
/// clock reading inside one call can close.
fn past_the_publication_delay() -> Duration {
    FLEET_HISTORY_PUBLICATION_DELAY + Duration::from_secs(1)
}

/// Look again until the STORE holds a `fleet-change` event of sub type `wanted` for the fleet.
///
/// Each look drives the clock the way any caller would — through a report of the history, which
/// materialises the moves that have fallen due — and asks the store rather than the report,
/// because an event the clock has just recorded is not describable for the published delay.
async fn poll_until_stored_state(
    case: &str,
    router: &MultiServiceRouter,
    engine: &engine::InMemoryEngine,
    fleet_id: &str,
    wanted: &str,
) {
    poll_until(
        case,
        "the fleet to record the state it was moving to",
        || async {
            let (status, body) = describe_history(router, fleet_id).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 while waiting, got {status}: {body}"
            );
            stored_state_events(engine, fleet_id)
                .iter()
                .any(|state| state == wanted)
        },
    )
    .await;
}

/// The report of one fleet once the state `wanted` has been recorded and every event it holds
/// has been published.
async fn history_once(
    case: &str,
    router: &MultiServiceRouter,
    engine: &engine::InMemoryEngine,
    fleet_id: &str,
    wanted: &str,
) -> String {
    poll_until_stored_state(case, router, engine, fleet_id, wanted).await;
    age_history(engine, fleet_id, past_the_publication_delay());
    let (status, body) = describe_history(router, fleet_id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status}: {body}"
    );
    body
}

/// The report of a fleet once it has reached `active`: the state every row's fleet holds its
/// capacity in, whether the clock moved it there or the create did.
async fn history_once_active(
    case: &str,
    router: &MultiServiceRouter,
    engine: &engine::InMemoryEngine,
    fleet_id: &str,
) -> String {
    history_once(case, router, engine, fleet_id, FLEET_STATE_ACTIVE).await
}

/// The rule, over the fleet table: the history is the events the fleet has been through, with
/// the state it entered or the instance that changed on each, oldest first.
#[tokio::test]
async fn the_history_is_the_events_the_fleet_has_been_through() {
    let (router, engine) = emulator();

    // Every row's fleet is created first, so the clock's moves for all of them run together:
    // each fleet's events fall due on the clock, not on the order the rows are read in.
    let mut fleets: Vec<(&FleetCase, String)> = Vec::new();
    for (index, row) in FLEET_CASES.iter().enumerate() {
        let fleet_id = create_fleet(&router, row, &format!("fleet-history-{index}")).await;
        fleets.push((row, fleet_id));
    }

    // Straight after the create: the fleet has entered the state it was stored in and placed the
    // capacity it could, so the store holds that state event first and one launch per instance —
    // but "EC2 Fleet events are delayed by up to 30 seconds before they can be described", so a
    // report made now carries none of them yet and says it was evaluated only up to an instant
    // before they happened.
    for (row, fleet_id) in &fleets {
        let case = row.case;
        let first_state = if row.asynchronous() {
            FLEET_STATE_SUBMITTED
        } else {
            FLEET_STATE_ACTIVE
        };
        assert_eq!(
            stored_state_events(&engine, fleet_id)
                .first()
                .map(String::as_str),
            Some(first_state),
            "case {case:?}: the first event of a fleet's history is the state it was created in"
        );
        let recorded = stored_history_len(&engine, fleet_id);
        // The create's own events: the state it put the fleet in and one launch per instance it
        // placed. The clock may have added the `active` of an asynchronous fleet by now, so this
        // is the floor rather than the count.
        let least_recorded = 1 + row.expected_instances;
        assert!(
            recorded >= least_recorded,
            "case {case:?}: expected at least {least_recorded} recorded event(s), got {recorded}"
        );
        let (status, body) = describe_history(&router, fleet_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a fleet that has only just been created is still described, \
             got {status}: {body}"
        );
        assert_eq!(
            records(case, &body),
            Vec::new(),
            "case {case:?}: an event recorded inside the publication delay is in no report \
             yet: {body}"
        );
        let evaluated = envelope(case, &body, "lastEvaluatedTime")
            .and_then(|text| as_timestamp(&text))
            .unwrap_or_else(|| panic!("case {case:?}: no lastEvaluatedTime in {body}"));
        let published_by = now_seconds() - FLEET_HISTORY_PUBLICATION_DELAY.as_secs_f64();
        assert!(
            evaluated <= published_by,
            "case {case:?}: expected a lastEvaluatedTime at or before {published_by}, \
             got {evaluated}"
        );
        assert_eq!(
            envelope(case, &body, "fleetId").as_deref(),
            Some(fleet_id.as_str()),
            "case {case:?}: the report names the fleet it is of: {body}"
        );
    }

    for (row, fleet_id) in &fleets {
        let case = row.case;
        let body = history_once_active(case, &router, &engine, fleet_id).await;
        let reported = records(case, &body);

        // One event per thing that happened: the states the fleet entered, and one launch per
        // instance it placed.
        let expected_events = row.expected_states().len() + row.expected_instances;
        assert_eq!(
            reported.len(),
            expected_events,
            "case {case:?}: expected {expected_events} event(s), got {reported:?}"
        );
        assert_eq!(
            state_events(case, &body),
            row.expected_states(),
            "case {case:?}: the states the fleet entered, in the order it entered them: {body}"
        );
        // The `launched` events name the instances the account's own store says the fleet
        // placed — not a count, and not an id from anywhere else.
        assert_eq!(
            named_instances(case, &body, EVENT_SUB_TYPE_LAUNCHED),
            fleet_placed_instances(&engine, fleet_id),
            "case {case:?}: every launch names the instance it launched: {body}"
        );
        for record in &reported {
            match record.event_type.as_deref() {
                Some(FLEET_EVENT_TYPE_INSTANCE_CHANGE) => assert!(
                    record.instance_id.is_some(),
                    "case {case:?}: an instance-change event names its instance, got {record:?}"
                ),
                Some(FLEET_EVENT_TYPE_FLEET_CHANGE) => assert_eq!(
                    record.instance_id, None,
                    "case {case:?}: a fleet-change event names no instance, got {record:?}"
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
    // was placed by exactly one of the fleets above.
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
    let row = &FLEET_CASES[1];
    let case = row.case;
    let (router, engine) = emulator();
    let fleet_id = create_fleet(&router, row, "fleet-history-window").await;
    let whole = history_once_active(case, &router, &engine, &fleet_id).await;
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
        let params = vec![p("FleetId", fleet_id.clone()), p("StartTime", iso(start))];
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

    // Each member of the modelled `FleetEventType` catalog, read off the generated catalog
    // rather than written out here: a type the fleet has events of reports exactly those, and
    // one it has none of reports an empty history rather than everything.
    for event_type in FLEET_EVENT_TYPE_VALUES {
        let params = vec![
            p("FleetId", fleet_id.clone()),
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
    for event_type in ["fleetChange", "FLEET-CHANGE", "instanceChange", "instance-change "] {
        let params = vec![
            p("FleetId", fleet_id.clone()),
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

/// "EC2 Fleet events are available for 48 hours": an event older than that is outside the window
/// however early a `StartTime` asks for it, and one inside it is reported.
#[tokio::test]
async fn an_event_older_than_the_availability_window_is_not_reported() {
    let row = &FLEET_CASES[0];
    let case = row.case;
    let (router, engine) = emulator();
    let fleet_id = create_fleet(&router, row, "fleet-history-availability").await;
    history_once_active(case, &router, &engine, &fleet_id).await;

    // Each side of the published window, and one event seeded at each: the store is written
    // directly, because waiting out 48 hours is not something a test does.
    let aged: &[(&str, Duration, &str)] = &[
        (
            "an hour old",
            Duration::from_secs(3_600),
            "i-0aaaa1111aaaa1111",
        ),
        (
            "an hour inside the window",
            FLEET_HISTORY_AVAILABILITY - Duration::from_secs(3_600),
            "i-0bbbb2222bbbb2222",
        ),
        (
            "an hour past the window",
            FLEET_HISTORY_AVAILABILITY + Duration::from_secs(3_600),
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
            .fleets
            .get_mut(&fleet_id)
            .expect("the fleet must be stored");
        for (_, age, instance_id) in aged {
            record.history.push(fleet_instance_change_event(
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
        let available = *age < FLEET_HISTORY_AVAILABILITY;
        assert_eq!(
            reported.contains(*instance_id),
            available,
            "case {window:?}: an event {age:?} old is {} the window: {body}",
            if available { "inside" } else { "outside" }
        );
    }
}

/// A deletion is a change of the fleet's state, and the instances it terminates are instance
/// changes of their own — while a deletion that leaves them running terminates nothing and never
/// reaches `deleted`.
#[tokio::test]
async fn a_deletion_records_the_states_and_the_instances_it_ended() {
    let (router, engine) = emulator();
    let terminating = FleetCase {
        case: "deleted with its instances",
        fleet_type: FLEET_TYPE_MAINTAIN,
        total_target_capacity: 2,
        extra: Vec::new,
        expected_instances: 2,
    };
    let running = FleetCase {
        case: "deleted with its instances left running",
        fleet_type: FLEET_TYPE_REQUEST,
        total_target_capacity: 1,
        extra: Vec::new,
        expected_instances: 1,
    };

    let mut fleets: Vec<(&FleetCase, bool, String)> = Vec::new();
    for (index, (row, terminate)) in [(&terminating, true), (&running, false)]
        .into_iter()
        .enumerate()
    {
        let fleet_id = create_fleet(&router, row, &format!("fleet-history-delete-{index}")).await;
        fleets.push((row, terminate, fleet_id));
    }
    for (row, _, fleet_id) in &fleets {
        history_once_active(row.case, &router, &engine, fleet_id).await;
    }

    for (row, terminate, fleet_id) in &fleets {
        let case = row.case;
        let placed = fleet_placed_instances(&engine, fleet_id);
        let mut params = indexed("FleetId", &[fleet_id.as_str()]);
        params.push(p(
            "TerminateInstances",
            if *terminate { "true" } else { "false" },
        ));
        let (status, body) = call(&router, REGION, "DeleteFleets", &params).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the deletion must succeed, got {status}: {body}"
        );

        // The state the deletion put the fleet in is one event of its history, and the instances
        // it terminated are one `instance-change` each — a deletion that leaves them running
        // terminates none, so it records none.
        let expected_state = if *terminate {
            FLEET_STATE_DELETED_TERMINATING
        } else {
            FLEET_STATE_DELETED_RUNNING
        };
        let body = history_once(case, &router, &engine, fleet_id, expected_state).await;
        let expected_terminations: BTreeSet<String> = if *terminate {
            placed.clone()
        } else {
            BTreeSet::new()
        };
        assert_eq!(
            named_instances(case, &body, EVENT_SUB_TYPE_TERMINATED),
            expected_terminations,
            "case {case:?}: the terminations are the instances the deletion ended: {body}"
        );

        // The states the fleet entered, in the order it entered them: the deletion follows the
        // `active` it was holding capacity in.
        assert_eq!(
            state_events(case, &body),
            vec![
                FLEET_STATE_SUBMITTED.to_string(),
                FLEET_STATE_ACTIVE.to_string(),
                expected_state.to_string()
            ],
            "case {case:?}: expected the states up to the deletion: {body}"
        );
    }

    // A fleet deleted WITH its instances reaches `deleted` once they are gone, which is one
    // further event; the one whose instances were left running has none to wait for, so it is
    // still `deleted_running` and records no `deleted` at all.
    let (_, _, terminated_fleet) = &fleets[0];
    let (_, _, running_fleet) = &fleets[1];
    let body = history_once(
        terminating.case,
        &router,
        &engine,
        terminated_fleet,
        FLEET_STATE_DELETED,
    )
    .await;
    assert_eq!(
        state_events(terminating.case, &body)
            .iter()
            .filter(|state| *state == FLEET_STATE_DELETED)
            .count(),
        1,
        "case {:?}: the fleet reaches deleted once: {body}",
        terminating.case
    );
    let (status, body) = describe_history(&router, running_fleet).await;
    assert_eq!(status, 200, "case {:?}: got {status}: {body}", running.case);
    assert!(
        !stored_state_events(&engine, running_fleet)
            .iter()
            .any(|state| *state == FLEET_STATE_DELETED),
        "case {:?}: a fleet whose instances still run has not been deleted, got {:?}",
        running.case,
        stored_state_events(&engine, running_fleet)
    );
    assert!(
        !state_events(running.case, &body)
            .iter()
            .any(|state| *state == FLEET_STATE_DELETED),
        "case {:?}: a fleet whose instances still run has not been deleted: {body}",
        running.case
    );
}

/// `MaxResults` cuts the history into pages a `NextToken` continues, oldest event first, and a
/// token this report did not mint is refused.
#[tokio::test]
async fn the_history_pages_in_event_order() {
    let row = &FLEET_CASES[1];
    let case = row.case;
    let other = &FLEET_CASES[0];
    let (router, engine) = emulator();
    let fleet_id = create_fleet(&router, row, "fleet-history-paging").await;
    let second = create_fleet(&router, other, "fleet-history-paging-other").await;
    let whole = history_once_active(case, &router, &engine, &fleet_id).await;
    history_once_active(other.case, &router, &engine, &second).await;
    let everything = records(case, &whole);
    let expected_events = row.expected_states().len() + row.expected_instances;
    assert_eq!(
        everything.len(),
        expected_events,
        "case {case:?}: the whole history is {expected_events} events: {whole}"
    );

    // Each page size the history can be cut into, and the pages it answers.
    let mut tokens: Vec<String> = Vec::new();
    for page_size in 1..=everything.len() + 1 {
        let mut walked: Vec<Record> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0usize;
        loop {
            let mut params = vec![
                p("FleetId", fleet_id.clone()),
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

    // This operation's `MaxResults` targets a plain `Integer` — the model publishes no range for
    // it — so the only bound is that a page of none is not a page: a value above the largest page
    // the service serves is answered as that page rather than refused.
    for (max_results, expected_status) in [(-1, 400), (0, 400), (1, 200), (1000, 200), (1001, 200)]
    {
        let params = vec![
            p("FleetId", fleet_id.clone()),
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
            p("FleetId", fleet_id.clone()),
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
        p("FleetId", second.clone()),
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

/// An id that is not an EC2 Fleet id at all is refused BEFORE any lookup, a well-formed id of no
/// fleet is refused as not found, a dry run answers only for a request that would have succeeded,
/// and reporting is a READ.
#[tokio::test]
async fn ids_are_refused_in_order_and_the_report_is_a_read() {
    let row = &FLEET_CASES[0];
    let (router, engine) = emulator();
    let fleet_id = create_fleet(&router, row, "fleet-history-ids").await;
    let held = history_once_active(row.case, &router, &engine, &fleet_id).await;

    // The malformed forms: not a UUID at all, another resource's prefix, the `<prefix>-<hex>`
    // form every other EC2 resource id takes, a UUID group of the wrong length, uppercase
    // hexadecimal, and a stored id with something after its last group.
    for named_id in [
        "fleet-not-a-uuid".to_string(),
        "sfr-0000ffff-0000-0000-0000-00000000ffff".to_string(),
        "fleet-00000000000000001".to_string(),
        "fleet-0000ffff-0000-0000-0000-00000000fff".to_string(),
        "fleet-0000FFFF-0000-0000-0000-00000000ffff".to_string(),
        format!("{fleet_id}-tail"),
    ] {
        let params = vec![p("FleetId", named_id.clone()), p("StartTime", iso(0.0))];
        let (status, body) = describe(&router, &params).await;
        assert_eq!(
            status, 400,
            "case {named_id:?}: expected HTTP 400, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidFleetId.Malformed",
            "case {named_id:?}: expected the malformed code, got {body}"
        );
    }

    // A well-formed id of no fleet of the account: not an empty history, and not "malformed".
    for named_id in [
        UNKNOWN_FLEET_ID,
        "fleet-00000000-0000-0000-0000-000000000000",
        "fleet-abcdef01-0000-0000-0000-0123456789ab",
    ] {
        let params = vec![p("FleetId", named_id), p("StartTime", iso(0.0))];
        let (status, body) = describe(&router, &params).await;
        assert_eq!(
            status, 400,
            "case {named_id:?}: expected HTTP 400, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidFleetId.NotFound",
            "case {named_id:?}: expected the not-found code, got {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("The FleetId(s) {named_id} do not exist."),
            "case {named_id:?}: expected AWS's wording for an id of no fleet, got {body}"
        );
    }

    // A dry run of a request that would have succeeded reports that, and reports nothing else.
    let params = vec![
        p("FleetId", fleet_id.clone()),
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
        (UNKNOWN_FLEET_ID, "InvalidFleetId.NotFound"),
        ("fleet-not-a-uuid", "InvalidFleetId.Malformed"),
    ] {
        let params = vec![
            p("FleetId", named_id),
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
