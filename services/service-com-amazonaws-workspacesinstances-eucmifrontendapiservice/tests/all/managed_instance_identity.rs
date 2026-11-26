// SPDX-License-Identifier: CC0-1.0

//! Which Amazon EC2 instance a WorkSpace Instance says it manages.
//!
//! RULE under test: `EC2ManagedInstance.InstanceId` is the identity of the one EC2 instance
//! launched for THAT WorkSpace Instance record — "Unique identifier of the managed EC2
//! instance" (`smithy_query(query="doc",shape="EC2ManagedInstance",member="InstanceId")`) —
//! so the value every view of the record reports is the id the service that OWNS instances
//! minted for that record's launch, whatever instances the account already held, and it does
//! not change from one read to the next.
//!
//! The model puts no `@pattern` and no `@length` on that member
//! (`smithy_query(query="shape",shape="EC2ManagedInstance")`), so no particular characters
//! are the requirement: the EQUALITY is. Every case here therefore establishes the managed
//! instance from the EC2 SIDE first — `ec2:DescribeInstances` over the account, matched on
//! the tags and the configuration the create was given, plus the one instance the account
//! gained across the create — and only then compares it with what this service reports.
//! Nothing reads either store directly.
//!
//! The rows span what the rule generalises over: the image, the instance type and the
//! network a launch names, the tag that identifies it, and how many unrelated instances the
//! account already holds when the launch is made (none, one, several) — so an id taken from
//! a counter of this service's own, or seeded ahead of the launch, cannot pass.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_workspacesinstances_eucmifrontendapiservice as wsinst;
use smithy_server_runtime::MultiServiceRouter;

use crate::ec2_managed_instance_propagation::ec2_client;
use crate::workspace_instance_lifecycle::{
    call, create, harness, ok, ACCOUNT_IMAGES, ACCOUNT_SUBNETS, POLL_DEADLINE,
};

/// The tag key each case marks its own WorkSpace Instance with, so the instance behind it
/// can be found from the EC2 side without reading anything this service stored.
const CASE_TAG: &str = "workspace-instance-case";

/// The tag key the instances launched DIRECTLY into the account carry: they belong to no
/// WorkSpace Instance, and no WorkSpace Instance may report one of them.
const DIRECT_TAG: &str = "launched-directly";

/// How long this suite waits between two looks while polling for a condition.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Every instance id Amazon EC2 holds for the account, sorted so two snapshots compare.
async fn held_instance_ids(ec2: &::aws_sdk_ec2::Client) -> Vec<String> {
    let answered = ec2
        .describe_instances()
        .send()
        .await
        .expect("ec2:DescribeInstances answers");
    let mut ids: Vec<String> = answered
        .reservations()
        .iter()
        .flat_map(::aws_sdk_ec2::types::Reservation::instances)
        .filter_map(|instance| instance.instance_id().map(str::to_string))
        .collect();
    ids.sort();
    ids
}

/// The instance Amazon EC2 holds for `instance_id`, or `None` when it holds none.
async fn described(
    ec2: &::aws_sdk_ec2::Client,
    instance_id: &str,
) -> Option<::aws_sdk_ec2::types::Instance> {
    let answered = ec2
        .describe_instances()
        .instance_ids(instance_id)
        .send()
        .await
        .ok()?;
    answered
        .reservations()
        .iter()
        .flat_map(::aws_sdk_ec2::types::Reservation::instances)
        .find(|instance| instance.instance_id() == Some(instance_id))
        .cloned()
}

/// The ids of every instance the account holds that carries `key`=`value`: the EC2-side
/// answer to "which instance is this WorkSpace Instance's", matched on the tags the create
/// was given.
async fn ids_tagged(ec2: &::aws_sdk_ec2::Client, key: &str, value: &str) -> Vec<String> {
    let answered = ec2
        .describe_instances()
        .send()
        .await
        .expect("ec2:DescribeInstances answers");
    let mut ids: Vec<String> = answered
        .reservations()
        .iter()
        .flat_map(::aws_sdk_ec2::types::Reservation::instances)
        .filter(|instance| {
            instance
                .tags()
                .iter()
                .any(|tag| tag.key() == Some(key) && tag.value() == Some(value))
        })
        .filter_map(|instance| instance.instance_id().map(str::to_string))
        .collect();
    ids.sort();
    ids
}

/// The tags EC2 reports on an instance, as a map.
fn instance_tags(instance: &::aws_sdk_ec2::types::Instance) -> BTreeMap<String, String> {
    instance
        .tags()
        .iter()
        .filter_map(|tag| {
            Some((
                tag.key()?.to_string(),
                tag.value().unwrap_or_default().to_string(),
            ))
        })
        .collect()
}

/// Launch `count` instances straight into the account's own Amazon EC2 — nothing to do with
/// any WorkSpace Instance — and answer their ids.
async fn launch_unrelated(ec2: &::aws_sdk_ec2::Client, case: &str, count: usize) -> Vec<String> {
    let mut ids = Vec::new();
    for _ in 0..count {
        let launched = ec2
            .run_instances()
            .image_id(ACCOUNT_IMAGES[0])
            .instance_type(::aws_sdk_ec2::types::InstanceType::from("t3.micro"))
            .min_count(1)
            .max_count(1)
            .tag_specifications(
                ::aws_sdk_ec2::types::TagSpecification::builder()
                    .resource_type(::aws_sdk_ec2::types::ResourceType::Instance)
                    .tags(
                        ::aws_sdk_ec2::types::Tag::builder()
                            .key(DIRECT_TAG)
                            .value("yes")
                            .build(),
                    )
                    .build(),
            )
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case {case:?}: ec2:RunInstances answers, got {error:?}")
            });
        let id = launched
            .instances()
            .first()
            .and_then(|instance| instance.instance_id().map(str::to_string))
            .unwrap_or_else(|| panic!("case {case:?}: ec2:RunInstances reported no instance"));
        ids.push(id);
    }
    ids
}

/// The `EC2ManagedInstance.InstanceId` and the `ProvisionState` `GetWorkspaceInstance`
/// reports for `id`, with the whole response beside them.
async fn reported(router: &MultiServiceRouter, id: &str) -> (Option<String>, String, Value) {
    let got = ok(
        router,
        "GetWorkspaceInstance",
        json!({ "WorkspaceInstanceId": id }),
    )
    .await;
    let managed = got["EC2ManagedInstance"]["InstanceId"]
        .as_str()
        .map(str::to_string);
    let state = got["ProvisionState"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    (managed, state, got)
}

/// One launch the rule is asserted on.
struct Row {
    case: &'static str,
    /// The value this row marks its WorkSpace Instance with under [`CASE_TAG`]: inside the
    /// modelled tag-value pattern `([\p{L}\p{Z}\p{N}_.:/=+\-@]*)`
    /// (`smithy_query(query="shape",shape="TagValue")`), which a sentence with punctuation
    /// is not.
    marker: &'static str,
    /// The `ManagedInstance` the create carries.
    managed_instance: Value,
    /// The `ImageId` and `InstanceType` the launched instance must report back, which is the
    /// EC2-side evidence that the id resolves to THIS row's launch.
    image_id: &'static str,
    instance_type: &'static str,
    /// How many unrelated instances are launched straight into the account before this row's
    /// create, so the account's instances are already there when the launch is made.
    unrelated_first: usize,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a create into an account that already holds two instances",
            marker: "two-held-already",
            managed_instance: json!({ "ImageId": ACCOUNT_IMAGES[1], "InstanceType": "t3.medium" }),
            image_id: ACCOUNT_IMAGES[1],
            instance_type: "t3.medium",
            unrelated_first: 2,
        },
        Row {
            case: "another image and instance type, in a subnet of the account",
            marker: "another-image-in-a-subnet",
            managed_instance: json!({
                "ImageId": ACCOUNT_IMAGES[2],
                "InstanceType": "m5.large",
                "SubnetId": ACCOUNT_SUBNETS[0],
            }),
            image_id: ACCOUNT_IMAGES[2],
            instance_type: "m5.large",
            unrelated_first: 1,
        },
        Row {
            case: "a create with no unrelated launch in between",
            marker: "no-unrelated-launch-between",
            managed_instance: json!({
                "ImageId": ACCOUNT_IMAGES[3],
                "InstanceType": "c5.large",
                "SubnetId": ACCOUNT_SUBNETS[1],
            }),
            image_id: ACCOUNT_IMAGES[3],
            instance_type: "c5.large",
            unrelated_first: 0,
        },
        Row {
            case: "a create after three more unrelated launches",
            marker: "after-three-more",
            managed_instance: json!({ "ImageId": ACCOUNT_IMAGES[4], "InstanceType": "t3.large" }),
            image_id: ACCOUNT_IMAGES[4],
            instance_type: "t3.large",
            unrelated_first: 3,
        },
    ]
}

#[tokio::test]
async fn the_reported_managed_instance_is_the_one_ec2_holds_for_that_record() {
    let (router, _engine) = harness();
    let ec2 = ec2_client(&router);

    let mut unrelated: Vec<String> = Vec::new();
    let mut reported_ids: Vec<String> = Vec::new();

    for row in rows() {
        let case = row.case;
        let marker = row.marker;
        unrelated.extend(launch_unrelated(&ec2, case, row.unrelated_first).await);

        // What the account held before the create: the launch the create makes is the only
        // instance it may add.
        let before = held_instance_ids(&ec2).await;

        let id = create(
            &router,
            json!({
                "ManagedInstance": row.managed_instance,
                "Tags": [{ "Key": CASE_TAG, "Value": marker }],
            }),
        )
        .await;

        // The EC2 side first: exactly one instance of the account carries this case's tag,
        // and the account gained exactly that one instance across the create.
        let tagged = ids_tagged(&ec2, CASE_TAG, marker).await;
        assert_eq!(
            tagged.len(),
            1,
            "case {case:?}: expected exactly one instance of the account to carry \
             {CASE_TAG}={marker}, got {tagged:?}"
        );
        let after = held_instance_ids(&ec2).await;
        let gained: Vec<String> = after
            .iter()
            .filter(|held| !before.contains(held))
            .cloned()
            .collect();
        assert_eq!(
            gained, tagged,
            "case {case:?}: expected the create to add exactly the instance carrying its \
             tag, the account gained {gained:?}"
        );
        let launched = tagged[0].clone();

        // Only now, what this service reports — it has to be that same instance.
        let (managed, state, got) = reported(&router, &id).await;
        let managed = managed.unwrap_or_else(|| {
            panic!("case {case:?}: expected an EC2ManagedInstance.InstanceId, got {got}")
        });
        assert_eq!(
            managed, launched,
            "case {case:?}: expected the instance EC2 holds for this record ({launched}), got \
             {managed:?} in state {state:?}"
        );
        assert!(
            !unrelated.contains(&managed),
            "case {case:?}: expected an instance of its own, got the directly launched \
             {managed:?}"
        );
        assert!(
            !reported_ids.contains(&managed),
            "case {case:?}: expected an instance of its own, got the already-reported \
             {managed:?}"
        );

        // And that instance is this row's launch, by the configuration the create was given.
        let instance = described(&ec2, &managed).await.unwrap_or_else(|| {
            panic!("case {case:?}: ec2:DescribeInstances resolves no instance {managed:?}")
        });
        assert_eq!(
            instance.image_id(),
            Some(row.image_id),
            "case {case:?}: expected the image the create named, got {instance:?}"
        );
        assert_eq!(
            instance
                .instance_type()
                .map(::aws_sdk_ec2::types::InstanceType::as_str),
            Some(row.instance_type),
            "case {case:?}: expected the instance type the create named, got {instance:?}"
        );
        let carried = instance_tags(&instance);
        assert_eq!(
            carried.get(CASE_TAG).map(String::as_str),
            Some(marker),
            "case {case:?}: expected the launched instance to carry {CASE_TAG}={marker}, got \
             {carried:?}"
        );
        assert!(
            !carried.contains_key(DIRECT_TAG),
            "case {case:?}: expected the WorkSpace Instance's own launch, got an instance \
             launched directly: {carried:?}"
        );

        reported_ids.push(managed);
    }

    // Every record reports an instance of its own, and none of them is one of the account's
    // other instances: the ids are per-record identities, not positions in a counter.
    let mut distinct = reported_ids.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        reported_ids.len(),
        "case the whole table: expected one managed instance per record, got {reported_ids:?}"
    );
}

#[tokio::test]
async fn the_reported_managed_instance_does_not_change_from_one_read_to_the_next() {
    let (router, _engine) = harness();
    let ec2 = ec2_client(&router);
    // The account already holds instances, and gains another one between the two creates
    // below, so a reader that reports anything but its own record's launch reports a
    // neighbour's instance.
    launch_unrelated(&ec2, "the account's own instances", 2).await;

    let first = create(
        &router,
        json!({
            "ManagedInstance": { "ImageId": ACCOUNT_IMAGES[1], "InstanceType": "t3.medium" },
            "Tags": [{ "Key": CASE_TAG, "Value": "the-first-record" }],
        }),
    )
    .await;
    let (managed_first, _state, got) = reported(&router, &first).await;
    let managed_first = managed_first
        .unwrap_or_else(|| panic!("case the first record: no managed instance in {got}"));

    launch_unrelated(&ec2, "an instance launched between the two creates", 1).await;

    let second = create(
        &router,
        json!({
            "ManagedInstance": { "ImageId": ACCOUNT_IMAGES[2], "InstanceType": "m5.large" },
            "Tags": [{ "Key": CASE_TAG, "Value": "the-second-record" }],
        }),
    )
    .await;
    let (managed_second, _state, got) = reported(&router, &second).await;
    let managed_second = managed_second
        .unwrap_or_else(|| panic!("case the second record: no managed instance in {got}"));
    assert_ne!(
        managed_first, managed_second,
        "case two records: expected an instance each, both report {managed_first:?}"
    );

    // Read the first record until the clock has taken it out of the transient state it was
    // created in: every read in between, and the one that finds the terminal state, reports
    // the same managed instance. The loop waits for the CONDITION, never for a span.
    let deadline = std::time::Instant::now() + POLL_DEADLINE;
    let mut reads = 0_u32;
    loop {
        let (status, payload) = call(
            &router,
            "GetWorkspaceInstance",
            json!({ "WorkspaceInstanceId": &first }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case read {reads}: expected HTTP 200, got {status} with {payload}"
        );
        assert_eq!(
            payload["EC2ManagedInstance"]["InstanceId"].as_str(),
            Some(managed_first.as_str()),
            "case read {reads}: expected the record's own managed instance, got {payload}"
        );
        reads += 1;
        if payload["ProvisionState"].as_str() == Some(wsinst::state::PROVISION_STATE_ALLOCATED) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case read {reads}: expected the record to reach \
             {} inside the poll deadline, last read {payload}",
            wsinst::state::PROVISION_STATE_ALLOCATED
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    // The listing is the same view of the same identity, and both instances are instances
    // Amazon EC2 really holds.
    let listed = ok(&router, "ListWorkspaceInstances", json!({})).await;
    for (id, expected) in [(&first, &managed_first), (&second, &managed_second)] {
        let in_listing = listed["WorkspaceInstances"]
            .as_array()
            .unwrap_or_else(|| panic!("case {id}: no WorkspaceInstances in {listed}"))
            .iter()
            .find(|entry| entry["WorkspaceInstanceId"].as_str() == Some(id.as_str()))
            .and_then(|entry| entry["EC2ManagedInstance"]["InstanceId"].as_str())
            .map(str::to_string);
        assert_eq!(
            in_listing.as_ref(),
            Some(expected),
            "case {id}: expected the listing to report the same managed instance, got {listed}"
        );
        assert!(
            described(&ec2, expected).await.is_some(),
            "case {id}: expected Amazon EC2 to hold {expected}, it resolves no such instance"
        );
    }
}
