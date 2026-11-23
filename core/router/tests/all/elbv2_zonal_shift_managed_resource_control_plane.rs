// SPDX-License-Identifier: CC0-1.0

//! The ARC control plane reached THROUGH a load balancer its own service registered, across
//! the three services that span: Amazon EC2 over ec2Query, Elastic Load Balancing v2 over
//! awsQuery and ARC Zonal Shift over restJson1, all served by ONE router over ONE engine.
//!
//! RULE: a resource an owning service has registered with ARC — here a load balancer whose
//! `zonal_shift.config.enabled` attribute is `true` — is a managed resource for EVERY ARC
//! operation that takes a `resourceIdentifier`, not only for `GetManagedResource`: it is one
//! of `ListManagedResources`'s items, with the zones its owner recorded and the weights and
//! shifts ARC computes for it; the practice run configuration and the zonal autoshift status
//! are ARC's OWN records about it, so writing one is served for such a resource and every
//! later read of it — the resource read and the listing alike — reports what was written and
//! the status derived from it. The owner withdrawing the registration takes the resource out
//! of both reads again.
//!
//! The registration half of the rule — `GetManagedResource` answering 200 with one weight
//! per Availability Zone, and a shift's whole life on the load balancer — is
//! `elbv2_zonal_shift_managed_resource.rs`, whose helpers this suite reuses so both assert
//! over the same hand-off. The ARC-only half, driven on a registry written directly, is
//! `services/service-com-amazonaws-arczonalshift-percdataplane/tests/all/registered_managed_resources.rs`.
//!
//! The rows differ along what the rule generalises over: the TYPE and SCHEME of the load
//! balancer (both the families the attribute's documentation lists it for) and how many
//! Availability Zones it spans — so neither the listing nor a configuration write is pinned
//! on one shape of resource.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::elbv2_zonal_shift_managed_resource::{
    arc, ec2_network, elbv2_ok, emulator, error_code, expected_weights, get_managed_resource,
    set_zonal_shift, weights,
};
use crate::query_wire::{element, form_encode};

/// The two sentences ARC states when zonal autoshift is asked to be `ENABLED` for a
/// resource that has no practice run configuration, and the `ConflictExceptionReason` it
/// names for it — both pinned here as LITERALS rather than through the constants the crate
/// exports, so prose composed from the request or the store fails the assertion. The text is
/// fixed for the class: the requirement is the same for every resource and every caller,
/// which already named the resource, so the refusal carries no variable part. The reason is
/// a value of the modelled enum (`smithy_query(query="shape",
/// shape="ConflictExceptionReason")`).
const PRACTICE_CONFIGURATION_REQUIRED_SENTENCE: &str =
    "Resource does not have a practice run configuration. Cannot enable zonal autoshift.";
const PRACTICE_CONFIGURATION_MISSING_REASON: &str = "PracticeConfigurationDoesNotExist";

/// The two sentences ARC states when a practice run configuration is asked to be deleted
/// while zonal autoshift is `ENABLED` for the resource — the status named as the token
/// `ZonalAutoshiftStatus` enumerates, then the action it blocks, about "the requested
/// resource" — and the modelled reason it carries. Literals for the same reason.
const AUTOSHIFT_ENABLED_SENTENCE: &str = "Zonal autoshift is ENABLED. Cannot delete a \
     practice run configuration for the requested resource.";
const AUTOSHIFT_ENABLED_REASON: &str = "AutoShiftEnabled";

/// One load balancer the rule is asserted over.
struct Row {
    case: &'static str,
    name: &'static str,
    /// The `Type` of the load balancer, from `LoadBalancerTypeEnum`.
    lb_type: &'static str,
    scheme: &'static str,
    /// The Availability Zones its subnets are created in — one subnet per zone.
    zones: &'static [&'static str],
    /// The second octet of the VPC CIDR, so each row builds its own network.
    octet: u8,
}

/// Both families the attribute's documentation lists it for — "supported by both Application
/// Load Balancers and Network Load Balancers" — across two zone counts and both schemes.
const ROWS: &[Row] = &[
    Row {
        case: "an internal Network Load Balancer across two zones",
        name: "zs-control-nlb",
        lb_type: "network",
        scheme: "internal",
        zones: &["us-west-2a", "us-west-2b"],
        octet: 211,
    },
    Row {
        case: "an internet-facing Application Load Balancer across three zones",
        name: "zs-control-alb",
        lb_type: "application",
        scheme: "internet-facing",
        zones: &["us-west-2a", "us-west-2b", "us-west-2c"],
        octet: 212,
    },
];

/// A router whose ELBv2 store holds `row`'s load balancer with zonal-shift integration
/// turned on, answered with the load balancer's ARN and the Availability Zone IDs EC2
/// reports for the subnets it is placed in — never a value this suite made up.
async fn registered_load_balancer(row: &Row) -> (MultiServiceRouter, String, Vec<String>) {
    let case = row.case;
    let router = emulator();
    let subnets = ec2_network(&router, row.octet, row.zones).await;
    let zone_ids: Vec<String> = subnets.iter().map(|subnet| subnet.zone_id.clone()).collect();
    assert_eq!(
        zone_ids.len(),
        row.zones.len(),
        "setup {case:?}: expected one zone ID per subnet, got {zone_ids:?}"
    );

    let mut params: Vec<(&str, &str)> = vec![
        ("Name", row.name),
        ("Type", row.lb_type),
        ("Scheme", row.scheme),
    ];
    let subnet_keys: Vec<String> = (1..=subnets.len())
        .map(|index| format!("Subnets.member.{index}"))
        .collect();
    for (key, subnet) in subnet_keys.iter().zip(&subnets) {
        params.push((key.as_str(), subnet.id.as_str()));
    }
    let created = elbv2_ok(&router, case, "CreateLoadBalancer", &params).await;
    let lb_arn = element(&created, "LoadBalancerArn");
    set_zonal_shift(&router, case, &lb_arn, "true").await;
    (router, lb_arn, zone_ids)
}

/// The `ListManagedResources` item for `arn`, walking every page the operation hands out, or
/// `None` when no page carries it.
///
/// The walk is BOUNDED: a handler that reissued a token it already handed out would
/// otherwise hang this suite instead of failing it, so the pages are capped far above the
/// few a store holding this suite's resources can fill, and the cap panics naming the page
/// it stopped at.
async fn listed(router: &MultiServiceRouter, arn: &str) -> Option<Value> {
    const MAX_PAGES: usize = 64;
    let mut query = String::new();
    for page in 1..=MAX_PAGES {
        let (status, body) = arc(router, "GET", &format!("/managedresources{query}"), None).await;
        assert_eq!(
            status, 200,
            "ListManagedResources{query}: expected 200, got {status} {body}"
        );
        for item in body["items"].as_array().cloned().unwrap_or_default() {
            if item["arn"].as_str() == Some(arn) {
                return Some(item);
            }
        }
        match body["nextToken"].as_str() {
            Some(token) => {
                assert!(
                    page < MAX_PAGES,
                    "ListManagedResources still hands out a token at page {page}, the last \
                     answer being {body}"
                );
                query = format!("?nextToken={}", form_encode(token));
            }
            None => return None,
        }
    }
    unreachable!("every page either answers, returns or trips the page cap above")
}

/// The `ListManagedResources` item for `arn`, which must be there.
async fn listed_item(router: &MultiServiceRouter, case: &str, arn: &str) -> Value {
    listed(router, arn).await.unwrap_or_else(|| {
        panic!("case {case:?}: expected ListManagedResources to report {arn}, it reports none")
    })
}

/// The Availability Zones a listing item reports.
fn listed_zones(item: &Value) -> Vec<String> {
    item["availabilityZones"]
        .as_array()
        .unwrap_or_else(|| panic!("availabilityZones is required, got {item}"))
        .iter()
        .map(|zone| zone.as_str().unwrap_or_default().to_string())
        .collect()
}

/// The ARN of an Amazon CloudWatch alarm in the load balancer's own Region and account —
/// what a `ControlCondition` names an alarm by (`ControlCondition$alarmIdentifier`: "The
/// Amazon Resource Name (ARN) for an Amazon CloudWatch alarm that you specify as a control
/// condition for a practice run") — so no partition, Region or account here is invented.
fn alarm_arn(load_balancer_arn: &str, alarm: &str) -> String {
    // `arn:<partition>:<service>:<region>:<account>:<resource>`, so the fields the alarm
    // ARN borrows are the 2nd, 4th and 5th of at least six.
    let fields: Vec<&str> = load_balancer_arn.split(':').collect();
    assert!(
        fields.len() >= 6,
        "expected an ARN of at least six colon-separated fields, got {load_balancer_arn:?}"
    );
    let (partition, region, account) = (fields[1], fields[3], fields[4]);
    format!("arn:{partition}:cloudwatch:{region}:{account}:alarm:{alarm}")
}

/// `CreatePracticeRunConfiguration` for `arn`, carrying the one member the model requires:
/// `outcomeAlarms`, whose `@length` is 1..10, so one alarm is the smallest configuration ARC
/// accepts (`smithy_query(query="operation", operation="CreatePracticeRunConfiguration")`).
async fn create_practice_run_configuration(
    router: &MultiServiceRouter,
    arn: &str,
    alarm: &str,
) -> (u16, Value) {
    arc(
        router,
        "POST",
        "/configuration",
        Some(json!({
            "resourceIdentifier": arn,
            "outcomeAlarms": [{
                "type": "CLOUDWATCH",
                "alarmIdentifier": alarm_arn(arn, alarm),
            }],
        })),
    )
    .await
}

/// `DeletePracticeRunConfiguration` for `arn`: `DELETE /configuration/{resourceIdentifier}`
/// (`smithy_query(query="operation", operation="DeletePracticeRunConfiguration")`).
async fn delete_practice_run_configuration(
    router: &MultiServiceRouter,
    arn: &str,
) -> (u16, Value) {
    arc(
        router,
        "DELETE",
        &format!("/configuration/{}", form_encode(arn)),
        None,
    )
    .await
}

/// `UpdateZonalAutoshiftConfiguration` for `arn`: `PUT
/// /managedresources/{resourceIdentifier}` (`smithy_query(query="operation",
/// operation="UpdateZonalAutoshiftConfiguration")`).
async fn set_zonal_autoshift(
    router: &MultiServiceRouter,
    arn: &str,
    wanted: &str,
) -> (u16, Value) {
    arc(
        router,
        "PUT",
        &format!("/managedresources/{}", form_encode(arn)),
        Some(json!({ "zonalAutoshiftStatus": wanted })),
    )
    .await
}

#[tokio::test]
async fn a_registered_load_balancer_is_one_of_the_listings_items_with_the_shifts_it_carries() {
    for row in ROWS {
        let case = row.case;
        let (router, lb_arn, zone_ids) = registered_load_balancer(row).await;

        let item = listed_item(&router, case, &lb_arn).await;
        assert_eq!(
            (item["arn"].as_str(), item["name"].as_str()),
            (Some(lb_arn.as_str()), Some(row.name)),
            "case {case:?}: expected the load balancer's own ARN and name, got {item}"
        );
        assert_eq!(
            listed_zones(&item),
            zone_ids,
            "case {case:?}: expected the zones EC2 put it in, got {item}"
        );
        assert_eq!(
            weights(&item),
            expected_weights(&zone_ids, None),
            "case {case:?}: expected weight 1 in each of {zone_ids:?}, got {:?}",
            weights(&item)
        );
        assert_eq!(
            (
                item["zonalShifts"].as_array().map(Vec::len),
                item["practiceRunStatus"].as_str(),
                item["zonalAutoshiftStatus"].as_str(),
            ),
            (Some(0), Some("DISABLED"), Some("DISABLED")),
            "case {case:?}: a resource nothing is configured for carries no shift and \
             neither status, got {item}"
        );

        // A shift started on it is reported through the LISTING as well: the weights it
        // moves and the shift itself are ARC's own, computed for a resource it did not
        // create.
        let away_from = zone_ids[zone_ids.len() - 1].clone();
        let (status, started) = arc(
            &router,
            "POST",
            "/zonalshifts",
            Some(json!({
                "resourceIdentifier": lb_arn,
                "awayFrom": away_from,
                "expiresIn": "1h",
                "comment": "shifting away from one zone",
            })),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: StartZonalShift must answer 201, got {status} {started}"
        );
        let shift_id = started["zonalShiftId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no zonalShiftId in {started}"))
            .to_string();

        let item = listed_item(&router, case, &lb_arn).await;
        assert_eq!(
            weights(&item),
            expected_weights(&zone_ids, Some(&away_from)),
            "case {case:?}: expected weight 0 for {away_from} and 1 elsewhere, got {:?}",
            weights(&item)
        );
        let in_item = item["zonalShifts"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: zonalShifts is required, got {item}"));
        assert_eq!(
            in_item
                .iter()
                .map(|shift| shift["zonalShiftId"].as_str())
                .collect::<Vec<_>>(),
            vec![Some(shift_id.as_str())],
            "case {case:?}: exactly the one shift in progress is listed, got {in_item:?}"
        );

        // Canceled, the listing reports no shift and the weights it moved are back.
        let (status, canceled) = arc(
            &router,
            "DELETE",
            &format!("/zonalshifts/{shift_id}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CancelZonalShift must answer 200, got {status} {canceled}"
        );
        let item = listed_item(&router, case, &lb_arn).await;
        assert_eq!(
            (weights(&item), item["zonalShifts"].as_array().map(Vec::len)),
            (expected_weights(&zone_ids, None), Some(0)),
            "case {case:?}: a canceled shift moves no weight and is no shift in progress, \
             got {item}"
        );

        // The owner withdrawing the registration takes it out of the listing too.
        set_zonal_shift(&router, case, &lb_arn, "false").await;
        let withdrawn = listed(&router, &lb_arn).await;
        assert!(
            withdrawn.is_none(),
            "case {case:?}: the integration turned off takes it out of the listing, got \
             {withdrawn:?}"
        );
    }
}

#[tokio::test]
async fn arcs_own_records_about_a_registered_load_balancer_are_written_and_read_back() {
    for row in ROWS {
        let case = row.case;
        let (router, lb_arn, zone_ids) = registered_load_balancer(row).await;

        // Zonal autoshift cannot be enabled for a resource ARC holds no practice run
        // configuration for: the refusal is a conflict about the configuration, not a
        // refusal of the resource.
        let (status, refused) = set_zonal_autoshift(&router, &lb_arn, "ENABLED").await;
        assert_eq!(
            (
                status,
                error_code(&refused).as_str(),
                refused["reason"].as_str(),
                refused["message"].as_str(),
            ),
            (
                409,
                "ConflictException",
                Some(PRACTICE_CONFIGURATION_MISSING_REASON),
                Some(PRACTICE_CONFIGURATION_REQUIRED_SENTENCE),
            ),
            "case {case:?}: expected the conflict ARC answers without a practice run \
             configuration, got {status} {refused}"
        );

        // A practice run configuration for it IS served, and the answer names the load
        // balancer its owner registered.
        let (status, created) = create_practice_run_configuration(&router, &lb_arn, "outcome").await;
        assert_eq!(
            status, 201,
            "case {case:?}: CreatePracticeRunConfiguration must answer 201, got {status} \
             {created}"
        );
        assert_eq!(
            (
                created["arn"].as_str(),
                created["name"].as_str(),
                created["zonalAutoshiftStatus"].as_str(),
            ),
            (Some(lb_arn.as_str()), Some(row.name), Some("DISABLED")),
            "case {case:?}: expected the load balancer's ARN and name with its current \
             autoshift status, got {created}"
        );
        let expected_alarms = json!([{
            "type": "CLOUDWATCH",
            "alarmIdentifier": alarm_arn(&lb_arn, "outcome"),
        }]);
        assert_eq!(
            created["practiceRunConfiguration"]["outcomeAlarms"], expected_alarms,
            "case {case:?}: expected the alarm the call asked ARC to hold, got {created}"
        );

        // Read back through the resource read and the listing: PERSISTED, not echoed.
        let (status, view) = get_managed_resource(&router, &lb_arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetManagedResource must answer 200, got {status} {view}"
        );
        assert_eq!(
            view["practiceRunConfiguration"], created["practiceRunConfiguration"],
            "case {case:?}: the read must report what the create answered, got {view}"
        );
        assert_eq!(
            listed_item(&router, case, &lb_arn).await["practiceRunStatus"].as_str(),
            Some("ENABLED"),
            "case {case:?}: a resource that has a configuration reports ENABLED"
        );

        // With one held, zonal autoshift is enabled for it, and both reads report it.
        let (status, enabled) = set_zonal_autoshift(&router, &lb_arn, "ENABLED").await;
        assert_eq!(
            (
                status,
                enabled["resourceIdentifier"].as_str(),
                enabled["zonalAutoshiftStatus"].as_str(),
            ),
            (200, Some(lb_arn.as_str()), Some("ENABLED")),
            "case {case:?}: expected the resource enabled for zonal autoshift, got {status} \
             {enabled}"
        );
        let (_, view) = get_managed_resource(&router, &lb_arn).await;
        assert_eq!(
            view["zonalAutoshiftStatus"].as_str(),
            Some("ENABLED"),
            "case {case:?}: GetManagedResource must report the status that was set, got {view}"
        );
        assert_eq!(
            listed_item(&router, case, &lb_arn).await["zonalAutoshiftStatus"].as_str(),
            Some("ENABLED"),
            "case {case:?}: ListManagedResources must report the status that was set"
        );

        // While autoshift is enabled the configuration it depends on cannot be deleted, and
        // the refused call leaves it held.
        let (status, refused) = delete_practice_run_configuration(&router, &lb_arn).await;
        assert_eq!(
            (
                status,
                error_code(&refused).as_str(),
                refused["reason"].as_str(),
                refused["message"].as_str(),
            ),
            (
                409,
                "ConflictException",
                Some(AUTOSHIFT_ENABLED_REASON),
                Some(AUTOSHIFT_ENABLED_SENTENCE),
            ),
            "case {case:?}: expected the conflict ARC answers while autoshift is enabled, \
             got {status} {refused}"
        );
        let (_, view) = get_managed_resource(&router, &lb_arn).await;
        assert_eq!(
            view["practiceRunConfiguration"], created["practiceRunConfiguration"],
            "case {case:?}: a refused delete must leave the configuration held, got {view}"
        );

        // Disabled again, the same delete is served, and both reads report the resource
        // carrying neither record any more — while it stays the managed resource its owner
        // registered, with the zones and weights that come from the owner.
        let (status, disabled) = set_zonal_autoshift(&router, &lb_arn, "DISABLED").await;
        assert_eq!(
            (status, disabled["zonalAutoshiftStatus"].as_str()),
            (200, Some("DISABLED")),
            "case {case:?}: expected the resource disabled for zonal autoshift, got {status} \
             {disabled}"
        );
        let (status, deleted) = delete_practice_run_configuration(&router, &lb_arn).await;
        assert_eq!(
            (status, deleted["arn"].as_str(), deleted["name"].as_str()),
            (200, Some(lb_arn.as_str()), Some(row.name)),
            "case {case:?}: DeletePracticeRunConfiguration must answer 200 for the load \
             balancer, got {status} {deleted}"
        );
        let (status, view) = get_managed_resource(&router, &lb_arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the resource stays managed after the delete, got {status} {view}"
        );
        assert!(
            view["practiceRunConfiguration"].is_null(),
            "case {case:?}: the deleted configuration is held by no read, got {view}"
        );
        assert_eq!(
            weights(&view),
            expected_weights(&zone_ids, None),
            "case {case:?}: expected the owner's zones at full weight, got {:?}",
            weights(&view)
        );
        let item = listed_item(&router, case, &lb_arn).await;
        assert_eq!(
            (
                item["practiceRunStatus"].as_str(),
                item["zonalAutoshiftStatus"].as_str(),
            ),
            (Some("DISABLED"), Some("DISABLED")),
            "case {case:?}: both statuses are back to what ARC reports for a resource \
             nothing is configured for, got {item}"
        );
    }
}
