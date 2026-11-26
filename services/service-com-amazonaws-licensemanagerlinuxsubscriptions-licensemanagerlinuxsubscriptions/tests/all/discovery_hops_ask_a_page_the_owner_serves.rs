// SPDX-License-Identifier: CC0-1.0

//! Every hop of a discovery pass asks the owner of the inventory for a page size that owner
//! SERVES, however little the pass may still record.
//!
//! The rule under test: when this service pages another service's list, the page size it
//! asks for lies inside the range that list publishes for its page-size member — what a
//! pass may still RECORD bounds what it keeps off the page it is answered, not what it asks
//! for. A hop that asks for a page size outside that range is answered no page at all, and
//! a Region whose FIRST hop that is ends up discovered by no pass: the pass walked its whole
//! plan, so it records no point to resume at, the next pass starts the plan over, arrives
//! with the same budget left and asks the same refused question. Neither
//! `ListLinuxSubscriptionInstances` nor `ListLinuxSubscriptions` then reports that Region's
//! instances, however often either is called.
//!
//! The owner here is Amazon EC2, whose inventory read publishes its page sizes:
//! `DescribeInstances` declares no `@range` on `MaxResults`
//! (`smithy_query(query="doc",shape="DescribeInstancesRequest",member="MaxResults")` on
//! EC2's model documents only what the member is), so the bound is the one the EC2 API
//! Reference publishes for it — "Valid Range: Minimum value of 5. Maximum value of 1000." —
//! and EC2 refuses a request outside it instead of serving it.
//!
//! The rows span what the rule generalises over: HOW MUCH of the pass's record budget is
//! left when it hops into a later source Region — more than the smallest page the owner
//! serves, exactly that page, one record short of it, and a single record — driven by the
//! fleet an earlier Region of the same pass holds. Each row is asserted over the page sizes
//! the pass actually asked EC2 for, over the instance the later Region holds, and over the
//! subscription the sibling list counts it in.
//!
//! The instances and the AMIs are EC2's own, launched through EC2 itself on the one shared
//! engine (`tests/all/discovery_from_ec2.rs`). What this suite puts in front of EC2 is only
//! an OBSERVER of the inventory reads: every hop is answered by EC2's own handler with the
//! request exactly as it arrived, so what the rows assert is what the real service would
//! have answered.

use std::sync::{Arc, Mutex};

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use serde_json::{json, Value};
use smithy_server_runtime::{
    BoxFuture, ErasedOperation, ErasedServiceDefinition, OperationContext, Service,
};

use service_com_amazonaws_ec2_amazonec2 as ec2;
use service_com_amazonaws_licensemanagerlinuxsubscriptions_licensemanagerlinuxsubscriptions::state::discovery::{
    MAX_DISCOVERED_INSTANCES, SUBSCRIPTION_TYPE_INCLUDED,
};

use crate::discovery_from_ec2::{
    reported_instance_ids, reported_subscriptions, Fleet, DISABLED, ENABLED, RHEL,
};

/// The routing key of the inventory read this suite observes: EC2's `ec2Query` requests name
/// their operation in the `Action` field of the form-encoded body.
const DESCRIBE_INSTANCES: &str = "DescribeInstances";
/// The field that names it.
const ACTION_FIELD: &str = "Action";
/// The page-size field of that read (`DescribeInstancesRequest$MaxResults`), which is the
/// value every row is about.
const MAX_RESULTS_FIELD: &str = "MaxResults";

/// The SMALLEST page the owner's inventory read serves, and the LARGEST: the range the EC2
/// API Reference publishes for `DescribeInstancesRequest$MaxResults` ("Valid Range: Minimum
/// value of 5. Maximum value of 1000."), named here rather than read off the code under test
/// so a row asserts against what the owner publishes.
const SMALLEST_PAGE_SERVED: i64 = 5;
const LARGEST_PAGE_SERVED: i64 = 1_000;

/// The source Region a pass reads FIRST: a pass walks the configured Regions sorted
/// (`crate::state::DiscoveryPlan`), and this code sorts before the other one, so this is the
/// Region whose fleet spends the pass's record budget before it hops anywhere else.
const FIRST_REGION: &str = "eu-west-1";

/// The source Region the pass hops into AFTER that one, holding the single instance whose
/// discovery every row is asserted over.
const LATER_REGION: &str = "us-east-1";

/// One row: how many records of its budget the pass still has when it hops into
/// [`LATER_REGION`], which is what the fleet of [`FIRST_REGION`] is sized to leave.
struct Row {
    case: &'static str,
    budget_left: usize,
}

/// Which page sizes the pass asked EC2 for, and for which Region's inventory.
#[derive(Debug, Default)]
struct AskedPages {
    hops: Mutex<Vec<(String, i64)>>,
}

impl AskedPages {
    /// Record one hop of `region`'s inventory walk, asked with page size `page_size`.
    fn record(&self, region: String, page_size: i64) {
        self.hops
            .lock()
            .expect("the hop log is never held across a panic")
            .push((region, page_size));
    }

    /// Forget the hops recorded so far, so a row reads only its own report's.
    fn forget(&self) {
        self.hops
            .lock()
            .expect("the hop log is never held across a panic")
            .clear();
    }

    /// The hops recorded since the last [`AskedPages::forget`].
    fn hops(&self) -> Vec<(String, i64)> {
        self.hops
            .lock()
            .expect("the hop log is never held across a panic")
            .clone()
    }
}

/// EC2, with every inventory read that carries a page size recorded on its way in.
///
/// Its identity, protocol and operations are EC2's own — the definition is the real
/// service's — and the observed read is answered by EC2's own handler with the request
/// unchanged, so nothing here decides what a hop is answered.
struct ObservedEc2 {
    inner: ec2::EmulatedService<engine::InMemoryEngine>,
    read: InventoryRead,
}

impl Service for ObservedEc2 {
    fn definition(&self) -> &dyn ErasedServiceDefinition {
        self.inner.definition()
    }

    fn can_handle(&self, parts: &http::request::Parts) -> bool {
        self.inner.can_handle(parts)
    }

    fn override_operation(&self, req: &RtRequest) -> Option<&dyn ErasedOperation> {
        let action = form_field(req, ACTION_FIELD);
        (action.as_deref() == Some(DESCRIBE_INSTANCES))
            .then_some(&self.read as &dyn ErasedOperation)
    }
}

/// One inventory read: its page size recorded when it carries one, then answered by EC2.
struct InventoryRead {
    asked: Arc<AskedPages>,
}

impl ErasedOperation for InventoryRead {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        // A read carrying no page size is one of the row's own setup reads (an instance
        // described by id), not a hop of the pass, which always names the page it wants.
        if let Some(page_size) = form_field(req, MAX_RESULTS_FIELD)
            .as_deref()
            .and_then(|value| value.parse::<i64>().ok())
        {
            self.asked.record(hop_region(req), page_size);
        }
        Box::pin(async move {
            let describe = svc
                .definition()
                .routing_table()
                .get(DESCRIBE_INSTANCES)
                .expect("EC2 models the inventory read");
            describe.run(svc, req, ctx).await
        })
    }
}

/// The source Region whose inventory walk `req` is a hop of: the pass asks each Region
/// through that Region's own endpoint (`ec2.<region>.amazonaws.com`), so the host of the
/// request names it.
fn hop_region(req: &RtRequest) -> String {
    let uri = req.uri();
    uri.split('/')
        .find(|part| part.starts_with("ec2."))
        .and_then(|host| host.split('.').nth(1))
        .unwrap_or_else(|| panic!("an inventory hop addressed to ec2.<region>., got {uri:?}"))
        .to_string()
}

/// The value of the form-encoded field `name` in `req`'s body, if it carries one.
fn form_field(req: &RtRequest, name: &str) -> Option<String> {
    let body = req.body().bytes()?;
    String::from_utf8_lossy(body)
        .split('&')
        .filter_map(|field| field.split_once('='))
        .find(|(field, _)| *field == name)
        .map(|(_, value)| value.to_string())
}

/// A discovery-list request for the instances of `region` alone, so a row reads the one
/// instance it is about rather than the whole fleet that spent the budget.
fn only_in(region: &str) -> Value {
    json!({ "Filters": [{ "Name": "Region", "Values": [region], "Operator": "Equal" }] })
}

#[tokio::test]
async fn an_inventory_hop_asks_for_a_page_the_owner_serves() {
    let rows = [
        Row {
            case: "more budget left than the smallest page the owner serves",
            budget_left: 10,
        },
        Row {
            case: "exactly the smallest page the owner serves left",
            budget_left: SMALLEST_PAGE_SERVED as usize,
        },
        Row {
            case: "one record short of the smallest page the owner serves",
            budget_left: SMALLEST_PAGE_SERVED as usize - 1,
        },
        Row {
            case: "a single record of budget left",
            budget_left: 1,
        },
    ];

    let asked = Arc::new(AskedPages::default());
    let fleet = Fleet::with_ec2({
        let asked = Arc::clone(&asked);
        move |inner| ObservedEc2 {
            inner,
            read: InventoryRead { asked },
        }
    });
    let image_id = fleet.publish_image(RHEL);
    // The one instance of the later Region, up and running before any row reads a list: both
    // discovery lists report the RUNNING instances.
    let later_id = fleet
        .launch(LATER_REGION, &image_id, 1)
        .await
        .first()
        .cloned()
        .expect("EC2 answers the launch with its instance id");

    // The fleet of the first Region GROWS from row to row, each row spending more of the
    // pass's record budget there than the row before it — so one fleet serves every row and
    // no row re-launches what the previous one already had.
    let batch = usize::try_from(ec2::state::MAX_INSTANCES_PER_RUN).expect("a positive batch");
    let mut in_first_region: usize = 0;
    for row in rows {
        let case = row.case;
        let fleet_size = MAX_DISCOVERED_INSTANCES - row.budget_left;
        assert!(
            fleet_size >= in_first_region,
            "case {case:?}: the rows grow one fleet, so each must spend at least as much \
             budget as the row before it, got {fleet_size} after {in_first_region}"
        );
        let mut launched_last: Option<String> = None;
        while in_first_region < fleet_size {
            let launch = (fleet_size - in_first_region).min(batch);
            launched_last = fleet
                .launch_now(FIRST_REGION, &image_id, launch)
                .await
                .last()
                .cloned();
            in_first_region += launch;
        }
        // EC2's own clock is given the span it owes those launches, waited out ONCE for the
        // whole fleet: the launch made last is the last one due, and EC2 settles every
        // launch due by the instant it is asked.
        if let Some(instance_id) = &launched_last {
            fleet.settled_running(FIRST_REGION, instance_id).await;
        }

        // Every row's pass starts from an EMPTY discovered store, so no row can be shown a
        // record an earlier row's pass wrote: discovery holds data only for the Regions the
        // settings name, so turning it off and reporting once drops what was discovered.
        // Which also makes each row the account's FIRST pass over this inventory — the case
        // the rule is about, because a Region a pass establishes nothing about keeps whatever
        // an earlier pass found there.
        fleet
            .set_discovery(DISABLED, &[FIRST_REGION, LATER_REGION])
            .await;
        let dropped = reported_instance_ids(&fleet.instances(&json!({})).await, case);
        assert!(
            dropped.is_empty(),
            "case {case:?}: an account that discovers no Region has no discovered instance, \
             got {dropped:?}"
        );
        fleet
            .set_discovery(ENABLED, &[FIRST_REGION, LATER_REGION])
            .await;

        // ONE report, which materializes ONE pass: it reads the first Region's fleet to the
        // end of its inventory, spending the budget down to this row's remainder, and then
        // hops into the later Region.
        asked.forget();
        let reported = reported_instance_ids(&fleet.instances(&only_in(LATER_REGION)).await, case);
        assert_eq!(
            reported,
            vec![later_id.clone()],
            "case {case:?}: the pass must discover the later Region's instance with \
             {} records of budget left, got {reported:?}",
            row.budget_left
        );

        // Which it can only have done by asking EC2 for a page EC2 serves. Every hop of the
        // pass is asserted, including the later Region's own: a hop asking outside the
        // published range is refused, and a Region whose first hop is refused is discovered
        // by no pass at all.
        let hops = asked.hops();
        let into_later: Vec<i64> = hops
            .iter()
            .filter(|(region, _)| region == LATER_REGION)
            .map(|(_, page_size)| *page_size)
            .collect();
        assert!(
            !into_later.is_empty(),
            "case {case:?}: the pass must hop into {LATER_REGION}, it asked {hops:?}"
        );
        for (region, page_size) in &hops {
            assert!(
                (SMALLEST_PAGE_SERVED..=LARGEST_PAGE_SERVED).contains(page_size),
                "case {case:?}: the hop into {region} must ask for a page between \
                 {SMALLEST_PAGE_SERVED} and {LARGEST_PAGE_SERVED}, it asked for {page_size}"
            );
        }

        // And every later report reports it as well, rather than only the one pass that
        // happened to reach it: a pass that walks its whole plan records no point to resume
        // at, so the next pass starts the plan over and arrives at the later Region with the
        // same budget left as this one did.
        let again = reported_instance_ids(&fleet.instances(&only_in(LATER_REGION)).await, case);
        assert_eq!(
            again,
            vec![later_id.clone()],
            "case {case:?}: every later report must discover the later Region's instance \
             too, got {again:?}"
        );

        // And the sibling list reports it too: both lists report from the one store the pass
        // fills, so the subscription counts every running instance of both Regions.
        let counted = i64::try_from(fleet_size + 1).expect("a small instance count");
        let subscriptions = reported_subscriptions(&fleet.subscriptions(&json!({})).await, case);
        assert_eq!(
            subscriptions,
            vec![(
                RHEL.platform_details.to_string(),
                SUBSCRIPTION_TYPE_INCLUDED.to_string(),
                counted,
            )],
            "case {case:?}: the subscription must count the running instances of both source \
             Regions ({counted}), got {subscriptions:?}"
        );
    }
}
