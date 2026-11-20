// SPDX-License-Identifier: CC0-1.0

//! The Availability Zone GROUP a zonal shift names, driven end to end over the wire.
//!
//! Every case here builds the production `MultiServiceRouter`, registers this service on
//! one shared engine and dispatches real `restJson1` requests, so what is pinned is the
//! HTTP behaviour rather than a handler called directly.
//!
//! RULE: every zonal-shift object ARC reports carries `azGroupId`, the zone group of the
//! Availability Zones of the Region the SHIFTED RESOURCE lives in — the Region field of
//! the resource's own ARN, since a `resourceIdentifier` is that ARN and nothing else. It
//! is one value per shift, so the create, every read and the cancel of one shift all
//! report the same group; a resource whose ARN names no Region names no group either, and
//! the member is then absent rather than filled with something else.
//!
//! The sites it is asserted at are every one this service reports a shift from:
//! `StartZonalShift`, `UpdateZonalShift`, `CancelZonalShift`, `ListZonalShifts`,
//! `GetManagedResource`, `ListManagedResources` and `StartPracticeRun`.
//!
//! The rows differ along what the rule generalises over: the REGION the resource lives in
//! (four commercial Regions, one in another partition), the partition itself, how the
//! resource reached ARC (seeded into this store, or registered by the service that owns
//! it), the KIND of resource, and an ARN with no Region field at all. The member's form —
//! the Region followed by its first zone group — is the one the EC2
//! `DescribeAvailabilityZones` `group-name` filter documents ("the zone group for the
//! Availability Zone (for example, us-east-1-zg-1)"), written out per row rather than
//! computed, so a row is an expectation and not a restatement of the implementation.
//!
//! The last test pins the LOCAL DECLARATION the member needs: the published model snapshot
//! declares no `azGroupId`, so the shapes that carry it are generated from the overlay
//! model `smithy-model/local-additions.json` (see `smithy-model/LOCAL-ADDITIONS.md`).
//! Everything that overlay restates of the snapshot is compared against it, shape by
//! shape, so a refreshed snapshot that declares the member, adds one the overlay does not
//! restate, or changes a target, value set or constraint fails here rather than silently
//! narrowing what this service reports.

use serde_json::{Map, Value};

use super::practice_run_configuration::get_managed_resource;
use super::practice_run_start::{given_configuration, start_practice_run};
use super::registered_managed_resources::register;
use super::zonal_shift_lifecycle::{call, emulator, encoded, seeded, start_body};

/// The published model snapshot this crate is generated from, embedded at compile time so
/// the local declaration is checked against the model rather than against a copy of it.
const SNAPSHOT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/smithy-model/com-amazonaws-arczonalshift-percdataplane.json"
));

/// The overlay model the local shift declarations are generated from, embedded for the
/// same reason: what it restates of the snapshot must still match it.
const OVERLAY: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/smithy-model/local-additions.json"
));

/// The member this crate declares locally, which the snapshot does not.
const LOCAL_MEMBER: &str = "azGroupId";

/// The shape the overlay adds for that member, which the snapshot declares nowhere.
const LOCAL_SHAPE: &str = "com.amazonaws.arczonalshift#AzGroupId";

/// The shapes the overlay restates in order to add [`LOCAL_MEMBER`] to them: every shape
/// this service reports a zonal shift through.
const SHIFT_SHAPES: &[&str] = &[
    "com.amazonaws.arczonalshift#ZonalShift",
    "com.amazonaws.arczonalshift#ZonalShiftSummary",
    "com.amazonaws.arczonalshift#ZonalShiftInResource",
    "com.amazonaws.arczonalshift#StartPracticeRunResponse",
];

/// One resource a shift is started for: how it reaches ARC, the ARN that names it, the
/// Availability Zones it is deployed in, and the zone group its Region's zones belong to.
struct Row {
    case: &'static str,
    arn: String,
    /// The zone traffic is moved away from, which must be one the resource is deployed in.
    away_from: String,
    /// `None` for a resource whose ARN names no Region, which names no zone group.
    az_group_id: Option<&'static str>,
}

/// A load balancer ARN in `partition`/`region`, of the `kind` Elastic Load Balancing
/// writes (`app` for an Application Load Balancer, `net` for a Network one).
fn load_balancer(partition: &str, region: &str, kind: &str, name: &str) -> String {
    format!(
        "arn:{partition}:elasticloadbalancing:{region}:000000000000:loadbalancer/{kind}/{name}\
         /1a2b3c4d5e6f7890"
    )
}

/// The rows every wire test below runs: resources in several Regions and partitions, one
/// of them this store's own seeded resource, plus an ARN with no Region field.
fn rows(engine: &engine::InMemoryEngine) -> Vec<Row> {
    // The resource this store was seeded with, so the rule is asserted for a resource ARC
    // holds itself as well as for the registered ones. Its Region is the seed Region, read
    // off the ARN rather than named here.
    let (seeded_arn, seeded_zones) = seeded(engine)
        .into_iter()
        .next()
        .expect("the store seeds at least one managed resource");
    let seeded_away_from = seeded_zones
        .first()
        .expect("a seeded resource is deployed in at least one zone")
        .clone();
    assert!(
        seeded_arn.contains(":us-east-1:"),
        "the seeded resources live in us-east-1, got {seeded_arn}"
    );

    let mut rows = vec![Row {
        case: "a resource this store seeds, us-east-1",
        arn: seeded_arn,
        away_from: seeded_away_from,
        az_group_id: Some("us-east-1-zg-1"),
    }];
    for (case, arn, zones, az_group_id) in [
        (
            "a Network Load Balancer another service registered, us-west-2",
            load_balancer("aws", "us-west-2", "net", "zg-nlb"),
            vec!["usw2-az1", "usw2-az2"],
            Some("us-west-2-zg-1"),
        ),
        (
            "an Application Load Balancer, eu-central-1",
            load_balancer("aws", "eu-central-1", "app", "zg-alb"),
            vec!["euc1-az1", "euc1-az2", "euc1-az3"],
            Some("eu-central-1-zg-1"),
        ),
        (
            "an Auto Scaling group, ap-northeast-1",
            "arn:aws:autoscaling:ap-northeast-1:000000000000:autoScalingGroup:\
             4e2b6e5a-9c1d-4f3a-8b7e-0d6c5a4b3e21:autoScalingGroupName/zg-asg"
                .to_string(),
            vec!["apne1-az1", "apne1-az4"],
            Some("ap-northeast-1-zg-1"),
        ),
        (
            // Another partition: the group is still the Region's own.
            "a load balancer in another partition, us-gov-west-1",
            load_balancer("aws-us-gov", "us-gov-west-1", "net", "zg-gov-nlb"),
            vec!["usgw1-az1", "usgw1-az2"],
            Some("us-gov-west-1-zg-1"),
        ),
        (
            // An ARN AWS writes for a partition-global resource: no Region field, so no
            // Region, so no zone group to name.
            "a resource whose ARN names no Region",
            load_balancer("aws", "", "net", "zg-regionless-nlb"),
            vec!["usw2-az1"],
            None,
        ),
    ] {
        register(engine, &arn, "zg-resource", &zones);
        rows.push(Row {
            case,
            arn,
            away_from: zones[0].to_string(),
            az_group_id,
        });
    }
    rows
}

/// The `azGroupId` a body carries: `Some(value)` for the member present as a string,
/// `None` for the member absent.
fn az_group_id(body: &Value) -> Option<String> {
    match &body[LOCAL_MEMBER] {
        Value::Null => None,
        Value::String(value) => Some(value.clone()),
        other => panic!("{LOCAL_MEMBER} must be a string or absent, got {other}"),
    }
}

/// Assert that `body` reports exactly `expected` for the member.
fn assert_group(case: &str, site: &str, body: &Value, expected: Option<&str>) {
    let actual = az_group_id(body);
    assert_eq!(
        actual.as_deref(),
        expected,
        "case {case:?}: {site} must report {LOCAL_MEMBER} {expected:?}, got {actual:?} in {body}"
    );
}

/// `ListZonalShifts` items for one resource.
async fn shifts_of(
    router: &smithy_server_runtime::MultiServiceRouter,
    arn: &str,
) -> Vec<Value> {
    let (status, body) = call(
        router,
        "GET",
        &format!("/zonalshifts?resourceIdentifier={}", encoded(arn)),
        "",
    )
    .await;
    assert_eq!(status, 200, "ListZonalShifts for {arn}: got {status} {body}");
    body["items"].as_array().cloned().unwrap_or_default()
}

/// The `ListManagedResources` entry for one resource, walking the pages it hands out.
async fn listed_resource(
    router: &smithy_server_runtime::MultiServiceRouter,
    arn: &str,
) -> Value {
    let mut query = String::new();
    loop {
        let (status, body) = call(router, "GET", &format!("/managedresources{query}"), "").await;
        assert_eq!(
            status, 200,
            "ListManagedResources{query}: got {status} {body}"
        );
        let items = body["items"].as_array().cloned().unwrap_or_default();
        if let Some(found) = items.into_iter().find(|item| item["arn"] == arn) {
            return found;
        }
        let Some(token) = body["nextToken"].as_str() else {
            panic!("ListManagedResources never reported {arn}");
        };
        query = format!("?nextToken={}", encoded(token));
    }
}

#[tokio::test]
async fn every_site_that_reports_a_shift_names_its_resources_zone_group() {
    let (router, engine) = emulator();
    for row in rows(&engine) {
        let case = row.case;
        let away_from = row.away_from.as_str();

        // The create.
        let (status, started) = call(
            &router,
            "POST",
            "/zonalshifts",
            &start_body(&row.arn, away_from, "30m", "zone group"),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: StartZonalShift answers 201, got {status} {started}"
        );
        assert_group(case, "the StartZonalShift body", &started, row.az_group_id);
        let shift_id = started["zonalShiftId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a minted zonalShiftId, got {started}"))
            .to_string();

        // The read of the resource the shift is for.
        let (status, view) = get_managed_resource(&router, &row.arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetManagedResource answers 200, got {status} {view}"
        );
        let in_resource = view["zonalShifts"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: zonalShifts is required, got {view}"));
        assert_eq!(
            in_resource.len(),
            1,
            "case {case:?}: the one shift in progress is listed, got {in_resource:?}"
        );
        assert_group(
            case,
            "the GetManagedResource zonalShifts entry",
            &in_resource[0],
            row.az_group_id,
        );

        // The list of resources, which reports the shifts in progress for each.
        let item = listed_resource(&router, &row.arn).await;
        let in_item = item["zonalShifts"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: zonalShifts is required, got {item}"));
        assert_eq!(
            in_item.len(),
            1,
            "case {case:?}: the one shift in progress is listed, got {in_item:?}"
        );
        assert_group(
            case,
            "the ListManagedResources zonalShifts entry",
            &in_item[0],
            row.az_group_id,
        );

        // The list of shifts.
        let listed = shifts_of(&router, &row.arn).await;
        assert_eq!(
            listed.len(),
            1,
            "case {case:?}: ListZonalShifts reports the one shift, got {listed:?}"
        );
        assert_group(
            case,
            "the ListZonalShifts items entry",
            &listed[0],
            row.az_group_id,
        );

        // The update: it changes the comment and the expiry, never the resource, so the
        // group it reports is the one the create reported.
        let (status, updated) = call(
            &router,
            "PATCH",
            &format!("/zonalshifts/{shift_id}"),
            &serde_json::json!({ "comment": "still the same resource" }).to_string(),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: UpdateZonalShift answers 200, got {status} {updated}"
        );
        assert_group(case, "the UpdateZonalShift body", &updated, row.az_group_id);

        // The cancel.
        let (status, canceled) = call(
            &router,
            "DELETE",
            &format!("/zonalshifts/{shift_id}"),
            "",
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CancelZonalShift answers 200, got {status} {canceled}"
        );
        assert_eq!(
            canceled["status"], "CANCELED",
            "case {case:?}: the canceled shift reports CANCELED, got {canceled}"
        );
        assert_group(case, "the CancelZonalShift body", &canceled, row.az_group_id);

        // A completed shift stays in the list, and keeps the group it was reported with.
        let listed = shifts_of(&router, &row.arn).await;
        assert_group(
            case,
            "the ListZonalShifts entry of the canceled shift",
            &listed[0],
            row.az_group_id,
        );
    }
}

#[tokio::test]
async fn one_shift_is_reported_with_one_group_wherever_it_is_read() {
    // The invariant behind the rows above, asserted without naming a value at all: the
    // five sites that report one shift cannot disagree about its group.
    let (router, engine) = emulator();
    for row in rows(&engine) {
        let case = row.case;
        let (status, started) = call(
            &router,
            "POST",
            "/zonalshifts",
            &start_body(&row.arn, &row.away_from, "1h", "one group per shift"),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: StartZonalShift answers 201, got {status} {started}"
        );
        let shift_id = started["zonalShiftId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a minted zonalShiftId, got {started}"))
            .to_string();

        let (_, view) = get_managed_resource(&router, &row.arn).await;
        let listed = shifts_of(&router, &row.arn).await;
        let (_, canceled) = call(
            &router,
            "DELETE",
            &format!("/zonalshifts/{shift_id}"),
            "",
        )
        .await;

        let reported: Vec<Option<String>> = vec![
            az_group_id(&started),
            az_group_id(&view["zonalShifts"][0]),
            az_group_id(&listed[0]),
            az_group_id(&canceled),
        ];
        assert!(
            reported.windows(2).all(|pair| pair[0] == pair[1]),
            "case {case:?}: every site must report one group for the shift, got {reported:?}"
        );
    }
}

#[tokio::test]
async fn a_practice_run_names_the_zone_group_of_its_resource() {
    // A practice run IS a zonal shift — `ListZonalShifts` reports it beside the
    // customer-initiated ones — so its own response names the same group.
    let (router, engine) = emulator();
    for row in rows(&engine) {
        let case = row.case;
        given_configuration(&router, &row.arn, &[]).await;
        let (status, started) =
            start_practice_run(&router, &row.arn, &row.away_from, "practice group").await;
        assert_eq!(
            status, 200,
            "case {case:?}: StartPracticeRun answers 200, got {status} {started}"
        );
        assert_group(case, "the StartPracticeRun body", &started, row.az_group_id);

        let listed = shifts_of(&router, &row.arn).await;
        assert_eq!(
            listed.len(),
            1,
            "case {case:?}: the practice run is the one shift listed, got {listed:?}"
        );
        assert_eq!(
            listed[0]["shiftType"], "PRACTICE_RUN",
            "case {case:?}: the shift listed is the practice run, got {}",
            listed[0]
        );
        assert_group(
            case,
            "the ListZonalShifts entry of the practice run",
            &listed[0],
            row.az_group_id,
        );
    }
}

/// The `shapes` object of one model document.
fn model_shapes(model: &str) -> Value {
    let parsed: Value = serde_json::from_str(model).expect("a model document must be JSON");
    Value::Object(
        parsed["shapes"]
            .as_object()
            .expect("a model document must carry a shapes object")
            .clone(),
    )
}

/// `declaration` with every documentation trait dropped, recursively — and with a traits
/// object left empty by that dropped too, so a member the snapshot documents and nothing
/// else compares equal to the same member restated with no traits at all.
///
/// Documentation is prose: the overlay restates what codegen READS — targets, types, enum
/// values and constraint traits — so comparing declarations has to ignore the one trait
/// that carries no behaviour.
fn without_docs(declaration: &Value) -> Value {
    match declaration {
        Value::Object(members) => Value::Object(
            members
                .iter()
                .filter(|(key, _)| key.as_str() != "smithy.api#documentation")
                .map(|(key, value)| (key.clone(), without_docs(value)))
                .filter(|(key, value)| {
                    key.as_str() != "traits" || !value.as_object().is_some_and(Map::is_empty)
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(without_docs).collect()),
        other => other.clone(),
    }
}

#[test]
fn the_overlay_restates_the_snapshot_exactly_but_for_the_member_it_adds() {
    let snapshot = model_shapes(SNAPSHOT);
    let overlay = model_shapes(OVERLAY);
    let restated = overlay
        .as_object()
        .expect("the overlay carries a shapes object");

    for (shape_id, declared) in restated {
        if shape_id == LOCAL_SHAPE {
            assert!(
                snapshot[shape_id].is_null(),
                "the snapshot now declares {shape_id}, so the local declaration should give \
                 way to the modelled shape"
            );
            continue;
        }
        let modelled = &snapshot[shape_id];
        assert!(
            !modelled.is_null(),
            "the overlay restates {shape_id}, which the snapshot no longer declares"
        );
        assert_eq!(
            declared["type"], modelled["type"],
            "{shape_id} must be restated with the type the snapshot gives it, got {}",
            declared["type"]
        );

        // The traits the snapshot states for the shape itself — a constraint the boundary
        // validator enforces, the input/output marker — are what the overlay must restate
        // as written. A declaration whose only trait was documentation carries none here.
        let shape_traits = |declaration: &Value| -> Value {
            match without_docs(&declaration["traits"]) {
                Value::Object(traits) if traits.is_empty() => Value::Null,
                other => other,
            }
        };
        assert_eq!(
            shape_traits(declared),
            shape_traits(modelled),
            "{shape_id} must be restated with the traits the snapshot states for it, got {}",
            declared["traits"]
        );

        let modelled_members = modelled["members"].as_object().cloned().unwrap_or_default();
        let declared_members = declared["members"].as_object().cloned().unwrap_or_default();
        for (member, modelled_member) in &modelled_members {
            assert_eq!(
                without_docs(&declared_members[member]),
                without_docs(modelled_member),
                "member {member} of {shape_id} must be restated as the snapshot declares it, \
                 got {:?}",
                declared_members.get(member)
            );
        }
        let added: Vec<&String> = declared_members
            .keys()
            .filter(|member| !modelled_members.contains_key(*member))
            .collect();
        if SHIFT_SHAPES.contains(&shape_id.as_str()) {
            assert_eq!(
                added.iter().map(|member| member.as_str()).collect::<Vec<_>>(),
                vec![LOCAL_MEMBER],
                "{shape_id} may add {LOCAL_MEMBER} and nothing else, added {added:?}"
            );
        } else {
            assert!(
                added.is_empty(),
                "{shape_id} is restated for the shapes above to target, so it may add no \
                 member; added {added:?}"
            );
        }
    }

    // Every shape the local declarations are of is restated: a shift shape left out of
    // the overlay would silently lose the member.
    for shape_id in SHIFT_SHAPES {
        assert!(
            restated.contains_key(*shape_id),
            "the overlay must restate {shape_id}, the shape a local declaration is of"
        );
        assert!(
            snapshot[*shape_id]["members"]
                .as_object()
                .is_some_and(|members| !members.contains_key(LOCAL_MEMBER)),
            "the snapshot now declares {LOCAL_MEMBER} on {shape_id}, so the local declaration \
             should give way to the modelled member"
        );
    }
}
