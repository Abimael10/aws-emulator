// SPDX-License-Identifier: CC0-1.0

//! `ListLinuxSubscriptionInstances` reports the RUNNING discovered instances.
//!
//! The rule under test: a report of discovered data is of the instances in the state the
//! model's own sentence for that report names — this one "lists the running Amazon EC2
//! instances that were discovered with commercial Linux subscriptions", and the `Instance`
//! it reports "details discovered information about a running instance using Linux
//! subscriptions" — so a record a pass holds in any other EC2 instance state is not an
//! element of it, and no `Filters` selection reaches one. Such a record stays in the store,
//! which is what keeps the subscription it uses reported by the sibling list with the count
//! of running instances that subscription has (`Subscription$InstanceCount`: "The total
//! amount of running instances using this subscription").
//!
//! The rows span what the rule generalises over: every `InstanceState$Name` EC2 publishes,
//! how many instances a subscription has in each of them, one subscription name licensed two
//! different ways, a subscription whose only instance is not running, and the `Filter$Name`
//! values addressing the instance state and the instance's own id under each `Operator`. The
//! two lists are also read against each other, because the instances this one reports are
//! exactly the ones the sibling counts.
//!
//! The records of the table are placed through the harness's seeding seam — discovery reads
//! the account's EC2 inventory and no operation of this service writes a discovered
//! instance, so a suite whose rule is about WHICH of those records a report is of states the
//! inventory a pass left. The last test drives the same rule end to end through EC2 itself:
//! an instance the account stops leaves the report while its subscription stays discovered.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

use service_com_amazonaws_licensemanagerlinuxsubscriptions_licensemanagerlinuxsubscriptions::state::discovery::{
    SUBSCRIPTION_TYPE_BYOS, SUBSCRIPTION_TYPE_INCLUDED, SUBSCRIPTION_TYPE_MARKETPLACE,
};
use service_com_amazonaws_licensemanagerlinuxsubscriptions_licensemanagerlinuxsubscriptions::state::{
    DiscoveredInstanceRecord, INSTANCE_STATUS_RUNNING,
};
use service_com_amazonaws_licensemanagerlinuxsubscriptions_licensemanagerlinuxsubscriptions::ComAmazonawsLicensemanagerlinuxsubscriptionsLicensemanagerlinuxsubscriptionsState as DiscoveryState;

use crate::discovery_from_ec2::{
    reported_instance_ids, reported_subscriptions, Fleet, ENABLED, RHEL, STATE_STOPPED,
};
use crate::harness::{
    call, enable_discovery, harness, seed_discovered_instance, Harness, Reply, ACCOUNT, REGION,
};

/// `POST /subscription/ListLinuxSubscriptionInstances` and its sibling list
/// (`OPERATIONS.md`).
const INSTANCES: &str = "/subscription/ListLinuxSubscriptionInstances";
const SUBSCRIPTIONS: &str = "/subscription/ListLinuxSubscriptions";

/// Every state an instance can be in, from EC2's own model
/// (`smithy_query(query="shape",shape="InstanceStateName")` on EC2's model: pending |
/// running | shutting-down | terminated | stopping | stopped) — the values
/// `Instance$Status` carries, because a discovered record reports the state the EC2
/// inventory reported for the instance.
///
/// A pass records no `terminated` instance, so no pass puts one in the store; a store
/// holding one is still a store the report holds no element for, which is what the table
/// asserts of every state other than `running`.
const INSTANCE_STATES: &[&str] = &[
    "pending",
    "running",
    "shutting-down",
    "terminated",
    "stopping",
    "stopped",
];

/// One subscription of the seeded inventory: the `(Subscription$Name, Subscription$Type)`
/// pair its instances carry, and the `Instance$Status` of each of those instances — one
/// record per entry.
struct Seeded {
    name: &'static str,
    subscription_type: &'static str,
    states: &'static [&'static str],
}

/// The inventory a previous pass left behind, spanning the forms the rule covers.
const FIXTURE: &[Seeded] = &[
    // One instance in EVERY state EC2 publishes: the report is of the one that is running.
    Seeded {
        name: "Red Hat Enterprise Linux",
        subscription_type: SUBSCRIPTION_TYPE_INCLUDED,
        states: INSTANCE_STATES,
    },
    // The same subscription name licensed another way, with its single running instance.
    Seeded {
        name: "Red Hat Enterprise Linux",
        subscription_type: SUBSCRIPTION_TYPE_BYOS,
        states: &["running"],
    },
    // Several running instances beside one that is not: the report and the count are the
    // same two instances.
    Seeded {
        name: "SUSE Linux",
        subscription_type: SUBSCRIPTION_TYPE_INCLUDED,
        states: &["running", "running", "stopped"],
    },
    // A subscription whose only instance is not running: still discovered, so the sibling
    // list reports it with a count of zero, and the instance report holds nothing for it.
    Seeded {
        name: "Ubuntu Pro",
        subscription_type: SUBSCRIPTION_TYPE_MARKETPLACE,
        states: &["stopped"],
    },
];

/// One seeded record, as a case names it: its instance id, the state it is held in, and the
/// subscription name it uses.
struct Held {
    instance_id: String,
    status: &'static str,
    subscription_name: &'static str,
}

/// Place [`FIXTURE`] in the store and answer what was placed.
///
/// The account's settings are the ones that make those records discovered data — discovery
/// enabled for the Region they were found in — because what the two lists report is the
/// inventory of the Regions the settings discover now.
async fn discover(h: &Harness) -> Vec<Held> {
    enable_discovery(h, &[REGION]).await;
    let mut held = Vec::new();
    for (subscription, index) in FIXTURE.iter().zip(0_usize..) {
        for (status, position) in subscription.states.iter().zip(0_usize..) {
            let instance_id = format!("i-{index:02}{position:02}");
            seed_discovered_instance(
                h,
                DiscoveredInstanceRecord {
                    instance_id: instance_id.clone(),
                    account_id: ACCOUNT.to_string(),
                    ami_id: "ami-00000000000000001".to_string(),
                    instance_type: "t3.micro".to_string(),
                    status: (*status).to_string(),
                    region: REGION.to_string(),
                    usage_operation: "RunInstances:0010".to_string(),
                    subscription_name: subscription.name.to_string(),
                    subscription_type: subscription.subscription_type.to_string(),
                    ..DiscoveredInstanceRecord::default()
                },
            );
            held.push(Held {
                instance_id,
                status,
                subscription_name: subscription.name,
            });
        }
    }
    held
}

/// The instance ids of `held` that are RUNNING, in the instance-id order the report uses.
fn running_ids(held: &[Held]) -> Vec<String> {
    let mut ids: Vec<String> = held
        .iter()
        .filter(|record| record.status == INSTANCE_STATUS_RUNNING)
        .map(|record| record.instance_id.clone())
        .collect();
    ids.sort();
    ids
}

/// A string member of a reported entry.
fn member(entry: &Value, name: &str, case: &str) -> String {
    entry
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no {name} in {entry:?}"))
        .to_string()
}

/// The entries a list body reports under `key`.
fn entries(reply: &Reply, key: &str, case: &str) -> Vec<Value> {
    reply
        .ok(case)
        .get(key)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: no {key} in {:?}", reply.body))
        .clone()
}

/// The `InstanceID`s an instance-list body reports.
fn instance_ids(reply: &Reply, case: &str) -> Vec<String> {
    entries(reply, "Instances", case)
        .iter()
        .map(|entry| member(entry, "InstanceID", case))
        .collect()
}

/// The `Instance$Status` the store holds each discovered record in, read through the same
/// seam the handlers use: what the report withholds is held there all the same.
fn stored_statuses(h: &Harness) -> BTreeMap<String, String> {
    h.engine
        .state::<DiscoveryState>()
        .lock()
        .discovered_instances
        .iter()
        .map(|(instance_id, record)| (instance_id.clone(), record.status.clone()))
        .collect()
}

/// One `Filter`, with its `Operator` left out when `operator` is `None` (the equality an
/// absent one means).
fn filtered(name: &str, operator: Option<&str>, values: &[String]) -> Value {
    let mut filter = json!({ "Name": name, "Values": values });
    if let Some(operator) = operator {
        filter["Operator"] = json!(operator);
    }
    json!({ "Filters": [filter] })
}

#[tokio::test]
async fn the_instance_report_holds_exactly_the_running_discovered_instances() {
    let h = harness();
    let held = discover(&h).await;
    let expected = running_ids(&held);

    let case = "a discovered inventory spanning every instance state";
    let reply = call(&h, INSTANCES, &json!({})).await;
    let listed = instance_ids(&reply, case);
    assert_eq!(
        listed, expected,
        "case {case:?}: expected {expected:?}, got {listed:?}"
    );
    for entry in entries(&reply, "Instances", case) {
        let status = member(&entry, "Status", case);
        assert_eq!(
            status, INSTANCE_STATUS_RUNNING,
            "case {case:?}: every reported instance is a running one, got {status:?} for \
             {entry:?}"
        );
    }

    // What the report withholds is still DISCOVERED: the store holds every record in the
    // state the inventory reported for it, which is what a later pass refreshes and what
    // keeps its subscription reported below.
    let stored = stored_statuses(&h);
    for record in &held {
        let instance_id = &record.instance_id;
        let status = stored.get(instance_id).map(String::as_str);
        assert_eq!(
            status,
            Some(record.status),
            "case {case:?}: {instance_id} must stay in the store as {:?}, the store holds \
             {stored:?}",
            record.status
        );
    }
}

#[tokio::test]
async fn the_subscription_counts_are_the_instances_the_report_lists() {
    let h = harness();
    let held = discover(&h).await;

    let case = "the two lists over one discovered inventory";
    let instances = call(&h, INSTANCES, &json!({})).await;
    let subscriptions = reported_subscriptions(&call(&h, SUBSCRIPTIONS, &json!({})).await, case);

    // Every subscription the store holds is reported, whether or not any of its instances is
    // running: the two members say what was discovered and how much of it is running.
    let reported_names: BTreeSet<String> = subscriptions
        .iter()
        .map(|(name, _, _)| name.clone())
        .collect();
    let discovered_names: BTreeSet<String> = held
        .iter()
        .map(|record| record.subscription_name.to_string())
        .collect();
    assert_eq!(
        reported_names, discovered_names,
        "case {case:?}: expected the discovered subscriptions {discovered_names:?}, got \
         {reported_names:?}"
    );

    // And for every subscription name, the instances the report lists under it are as many
    // as the `InstanceCount`s reported for it — computed from the two answers, so neither
    // side restates the table.
    let mut counted: BTreeMap<String, i64> = BTreeMap::new();
    for (name, _, count) in &subscriptions {
        *counted.entry(name.clone()).or_default() += count;
    }
    let mut listed: BTreeMap<String, i64> = BTreeMap::new();
    for entry in entries(&instances, "Instances", case) {
        *listed
            .entry(member(&entry, "SubscriptionName", case))
            .or_default() += 1;
    }
    for (name, count) in &counted {
        let reported = listed.get(name).copied().unwrap_or_default();
        assert_eq!(
            *count, reported,
            "case {case:?}: {name} counts {count} running instance(s), of which the report \
             lists {reported}"
        );
    }
}

/// One filter case: the `Filter$Name`, its `Operator` (`None` is an absent one), the
/// `Values` it compares against, and the instance ids it must select, in the report's order.
type FilterCase = (
    &'static str,
    Option<&'static str>,
    Vec<String>,
    Vec<String>,
);

#[tokio::test]
async fn no_filter_selects_a_record_the_report_withholds() {
    let h = harness();
    let held = discover(&h).await;
    let running = running_ids(&held);
    let nothing: Vec<String> = Vec::new();
    let running_state = INSTANCE_STATUS_RUNNING.to_string();

    // (the `Filter$Name`, its `Operator` — `None` is an absent one, its `Values`, the ids it
    // must select in the report's order).
    let mut cases: Vec<FilterCase> = vec![
        (
            "Status",
            Some("Equal"),
            vec![running_state.clone()],
            running.clone(),
        ),
        ("Status", None, vec![running_state.clone()], running.clone()),
        // A value every reported state contains, and none of the withheld records could be
        // reached by either way of asking.
        (
            "Status",
            Some("Contains"),
            vec!["ing".to_string()],
            running.clone(),
        ),
        (
            "Status",
            Some("NotEqual"),
            vec![running_state],
            nothing.clone(),
        ),
    ];
    for state in INSTANCE_STATES
        .iter()
        .filter(|state| **state != INSTANCE_STATUS_RUNNING)
    {
        let values = vec![(*state).to_string()];
        cases.push(("Status", Some("Equal"), values.clone(), nothing.clone()));
        // Every record the report holds differs from a state no reported record is in.
        cases.push(("Status", Some("NotEqual"), values, running.clone()));
    }
    // Nor by its own id, which is the one value that names exactly one record.
    for record in held
        .iter()
        .filter(|record| record.status != INSTANCE_STATUS_RUNNING)
    {
        cases.push((
            "InstanceID",
            Some("Equal"),
            vec![record.instance_id.clone()],
            nothing.clone(),
        ));
    }

    for (name, operator, values, expected) in cases {
        let case = &format!("{name} {operator:?} {values:?}");
        let reply = call(&h, INSTANCES, &filtered(name, operator, &values)).await;
        let listed = instance_ids(&reply, case);
        assert_eq!(
            listed, expected,
            "case {case:?}: expected {expected:?}, got {listed:?}"
        );
    }
}

#[tokio::test]
async fn an_instance_the_account_stops_leaves_the_report_and_stays_discovered() {
    let fleet = Fleet::new();
    fleet.set_discovery(ENABLED, &[REGION]).await;
    let image_id = fleet.publish_image(RHEL);
    // Launched through EC2 itself and reported by it as running, so what the report lists is
    // the state EC2 holds the instance in rather than one this suite wrote.
    let mut ids = fleet.launch(REGION, &image_id, 3).await;
    ids.sort();
    let (name, subscription_type) = RHEL
        .subscription
        .expect("the AMI row bills a Linux subscription");

    let case = "three instances of one AMI, all running";
    let listed = reported_instance_ids(&fleet.instances(&json!({})).await, case);
    assert_eq!(
        listed, ids,
        "case {case:?}: expected {ids:?}, got {listed:?}"
    );
    let actual = reported_subscriptions(&fleet.subscriptions(&json!({})).await, case);
    let expected = vec![(name.to_string(), subscription_type.to_string(), 3)];
    assert_eq!(
        actual, expected,
        "case {case:?}: expected {expected:?}, got {actual:?}"
    );

    // One of them is stopped through EC2, so the account runs two of the three.
    fleet.stop(REGION, &ids[1]).await;

    let case = "one of the three stopped through EC2";
    let listed = reported_instance_ids(&fleet.instances(&json!({})).await, case);
    let expected_ids = vec![ids[0].clone(), ids[2].clone()];
    assert_eq!(
        listed, expected_ids,
        "case {case:?}: expected {expected_ids:?}, got {listed:?}"
    );
    let stored = stored_statuses(&fleet.h);
    let stopped = stored.get(&ids[1]).map(String::as_str);
    assert_eq!(
        stopped,
        Some(STATE_STOPPED),
        "case {case:?}: {} must stay discovered as {STATE_STOPPED:?}, the store holds \
         {stored:?}",
        ids[1]
    );
    let actual = reported_subscriptions(&fleet.subscriptions(&json!({})).await, case);
    let expected = vec![(name.to_string(), subscription_type.to_string(), 2)];
    assert_eq!(
        actual, expected,
        "case {case:?}: the subscription stays discovered, counting its running instances, \
         expected {expected:?}, got {actual:?}"
    );

    // And the state it is held in reaches no caller through the filter addressing that
    // member either.
    let selection = json!({
        "Filters": [{ "Name": "Status", "Values": [STATE_STOPPED], "Operator": "Equal" }]
    });
    let listed = reported_instance_ids(&fleet.instances(&selection).await, case);
    assert!(
        listed.is_empty(),
        "case {case:?}: a Status filter must reach no withheld record, got {listed:?}"
    );
}
