// SPDX-License-Identifier: CC0-1.0

//! A discovery pass over SEVERAL source Regions resumes at the earliest Region whose
//! inventory it left unread, whatever it read after that Region.
//!
//! The rule under test: a pass records the FIRST point of its walk at which it left
//! inventory unread, so the next pass begins there and reads that remainder — a Region whose
//! inventory a pass covered only in part is picked up at the cursor that remainder begins at
//! however many Regions of the plan the pass went on to finish. A pass that keeps only the
//! outcome of the Region it happened to read LAST throws the remainder away: the next pass
//! then starts the plan afresh and spends its budget walking a prefix of that Region it has
//! already recorded, which is the one thing the recorded stopping point exists to prevent.
//!
//! The rows span what the rule generalises over: WHERE in the walk the Region whose
//! inventory is left part-read sits — first, in the middle, last, and nowhere at all — with
//! every Region after it read to the end of its own inventory, which is the case that makes
//! the recorded stopping point of the part-read Region the one a later outcome can overwrite.
//! Each row is asserted over both operations that materialize a pass
//! (`ListLinuxSubscriptionInstances` and `ListLinuxSubscriptions`), which report from the one
//! store a pass fills.
//!
//! The instances and the AMIs are EC2's own, launched through EC2 itself on the one shared
//! engine (`tests/all/discovery_from_ec2.rs`), and the inventory walk is answered a page at a
//! time with one hop of one Region's walk refused — the seam
//! `tests/all/discovery_partial_inventory_read.rs` already puts in front of EC2, used here
//! over more than one Region.

use std::sync::Arc;

use serde_json::json;

use service_com_amazonaws_licensemanagerlinuxsubscriptions_licensemanagerlinuxsubscriptions::state::discovery::SUBSCRIPTION_TYPE_INCLUDED;
use service_com_amazonaws_licensemanagerlinuxsubscriptions_licensemanagerlinuxsubscriptions::state::DiscoveryResume;
use service_com_amazonaws_licensemanagerlinuxsubscriptions_licensemanagerlinuxsubscriptions::ComAmazonawsLicensemanagerlinuxsubscriptionsLicensemanagerlinuxsubscriptionsState as DiscoveryState;

use crate::discovery_from_ec2::{
    reported_instance_ids, reported_subscriptions, Fleet, ENABLED, RHEL,
};
use crate::discovery_partial_inventory_read::{InventoryHop, InventoryHops, PagedEc2};

/// The source Regions every row configures, in the order the FIRST pass walks them: a pass's
/// plan is the configured set sorted, rotated to where the previous pass stopped
/// (`crate::state::DiscoveryPlan`), and the first pass has nowhere to rotate to.
///
/// Three of them, so a row can leave the part-read Region at the beginning, in the middle or
/// at the end of that walk.
const WALK: [&str; 3] = ["ap-south-1", "us-east-1", "us-west-2"];

/// How many items each answered page of a Region's inventory carries. The page size EC2's
/// own inventory read admits at its smallest (`DescribeInstancesRequest$MaxResults`), which
/// is what lets a small fleet span several pages.
const PAGE: usize = 5;

/// The hop of the part-read Region's walk that is refused: its SECOND, so the first page of
/// that Region is answered and recorded and the rest of its inventory is the remainder a
/// later pass has to pick up.
const REFUSED_HOP: usize = 2;

/// How many instances the part-read Region holds: more than [`PAGE`], so the refused hop
/// really does leave inventory behind it.
const PART_READ_FLEET: usize = 12;

/// How many instances every other Region holds: one short page, so its walk reaches the end
/// of its inventory in a single hop and the pass goes on with budget to spare.
const WHOLE_FLEET: usize = 2;

/// One row: where in the walk the Region whose inventory is left part-read sits.
struct Row {
    case: &'static str,
    /// The position in [`WALK`] of the Region one hop of whose walk is refused; `None` cuts
    /// no Region's walk short.
    part_read: Option<usize>,
}

/// The instances of `region` the row's pass must have recorded, in the order EC2 pages them.
fn expected_from(region_fleet: &[String], part_read: bool) -> Vec<String> {
    match part_read {
        true => region_fleet[..PAGE].to_vec(),
        false => region_fleet.to_vec(),
    }
}

#[tokio::test]
async fn a_pass_resumes_the_earliest_region_whose_inventory_it_left_unread() {
    let rows = [
        Row {
            case: "the first Region of the walk is left part-read and the later ones finish",
            part_read: Some(0),
        },
        Row {
            case: "a middle Region of the walk is left part-read and a later one finishes",
            part_read: Some(1),
        },
        Row {
            case: "the last Region of the walk is left part-read",
            part_read: Some(2),
        },
        Row {
            case: "every Region's inventory is read to its end",
            part_read: None,
        },
    ];

    for row in rows {
        let case = row.case;
        let part_read = row.part_read.map(|position| WALK[position]);
        let hops = Arc::new(InventoryHops::new(
            PAGE,
            part_read.map(|region| (region, REFUSED_HOP)),
        ));
        let fleet = Fleet::with_ec2({
            let hops = Arc::clone(&hops);
            move |inner| PagedEc2 {
                inner,
                hop: InventoryHop { hops },
            }
        });
        let image_id = fleet.publish_image(RHEL);
        // Launch order IS instance-id order, which is the order EC2 pages a Region's
        // inventory in, so the sorted ids of a Region are its pages in the order a walk
        // reads them.
        let mut launched: Vec<(&str, Vec<String>)> = Vec::new();
        for region in WALK {
            let count = match Some(region) == part_read {
                true => PART_READ_FLEET,
                false => WHOLE_FLEET,
            };
            launched.push((region, fleet.launch_now(region, &image_id, count).await));
        }
        // Both lists report the RUNNING instances, so EC2's own clock is given the span it
        // owes the launches — in every Region at once, the launches having all been made
        // already. These reads are the row's own, not the pass's hops, which is why the
        // paging and the refusal are armed only afterwards.
        for (region, ids) in &mut launched {
            ids.sort();
            for instance_id in ids.iter() {
                fleet.settled_running(region, instance_id).await;
            }
        }
        hops.arm();
        fleet.set_discovery(ENABLED, &WALK).await;

        // ONE report, which materializes ONE pass: it answers the first page of the
        // part-read Region, is refused the next, and goes on to read every Region after it
        // to the end of its inventory.
        let reported = {
            let mut reported = reported_instance_ids(&fleet.instances(&json!({})).await, case);
            reported.sort();
            reported
        };
        let expected = {
            let mut expected: Vec<String> = launched
                .iter()
                .flat_map(|(region, ids)| expected_from(ids, Some(*region) == part_read))
                .collect();
            expected.sort();
            expected
        };
        assert_eq!(
            reported, expected,
            "case {case:?}: the pass must record the {} instances of the pages EC2 answered, \
             got {reported:?}",
            expected.len()
        );
        // The hops that pass made: one per Region whose inventory fits a page, plus the
        // refused one of the part-read Region. A walk that was not cut short where the row
        // says would show up here as another count.
        let expected_hops = WALK.len() + usize::from(part_read.is_some());
        assert_eq!(
            hops.seen(),
            expected_hops,
            "case {case:?}: the pass must make {expected_hops} inventory hops, it made {}",
            hops.seen()
        );

        // What it recorded about where it stopped: the Region it left a remainder inside,
        // at the cursor that remainder begins at — never the outcome of a Region it read
        // afterwards, which would leave that remainder unreachable.
        let stopped = fleet
            .h
            .engine
            .state::<DiscoveryState>()
            .lock()
            .discovery_resume
            .clone();
        match part_read {
            Some(region) => {
                let inside = matches!(
                    &stopped,
                    Some(DiscoveryResume::Inside(at, cursor)) if at == region && cursor.is_some()
                );
                assert!(
                    inside,
                    "case {case:?}: {region} still holds unread inventory, so the pass must \
                     stop inside it at the cursor that remainder begins at, got {stopped:?}"
                );
            }
            None => assert!(
                stopped.is_none(),
                "case {case:?}: a pass that left no unread inventory records no stopping \
                 point, got {stopped:?}"
            ),
        }

        // And the remainder is read: the next pass — materialized here by the OTHER
        // operation that reports discovered data, which fills the same store — takes the
        // part-read Region up where this one stopped, so the whole inventory of every
        // configured Region is discovered.
        let total: usize = launched.iter().map(|(_, ids)| ids.len()).sum();
        let counted = vec![(
            RHEL.platform_details.to_string(),
            SUBSCRIPTION_TYPE_INCLUDED.to_string(),
            i64::try_from(total).expect("a small instance count"),
        )];
        let subscriptions = reported_subscriptions(&fleet.subscriptions(&json!({})).await, case);
        assert_eq!(
            subscriptions, counted,
            "case {case:?}: the subscription must count every running instance of the \
             configured Regions, got {subscriptions:?}"
        );

        // Which is the whole inventory, instance by instance — and a pass that got through
        // its plan leaves no stopping point behind.
        let later = {
            let mut later = reported_instance_ids(&fleet.instances(&json!({})).await, case);
            later.sort();
            later
        };
        let whole = {
            let mut whole: Vec<String> = launched
                .iter()
                .flat_map(|(_, ids)| ids.iter().cloned())
                .collect();
            whole.sort();
            whole
        };
        assert_eq!(
            later, whole,
            "case {case:?}: a later pass must discover the whole inventory of every \
             configured Region, got {later:?}"
        );
        let settled = fleet
            .h
            .engine
            .state::<DiscoveryState>()
            .lock()
            .discovery_resume
            .clone();
        assert!(
            settled.is_none(),
            "case {case:?}: a pass that read its whole plan records no stopping point, got \
             {settled:?}"
        );
    }
}
