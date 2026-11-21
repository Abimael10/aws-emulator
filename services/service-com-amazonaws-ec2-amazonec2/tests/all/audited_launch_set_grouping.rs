// SPDX-License-Identifier: CC0-1.0

//! The SET/ITEMS grouping a record of an EC2 launch and of its terminate carries, over the
//! shapes of request that rule has to hold for.
//!
//! RULE: a record reports a call in the SERVING API's own document form, so for EC2 — whose
//! protocol is a query/XML dialect — a repeated member is held in one member whose elements
//! sit under `items`, a scalar member of the call stays where the call put it, and every
//! member is named as the model's `@xmlName` spells it
//! (`smithy_query(query="shape",shape="Reservation,Instance,InstanceStateChange,InstanceState,TagSpecification")`
//! -> `instancesSet`, `reservationId`, `subnetId`, `instanceState`, `currentState`,
//! `previousState`). A launch's own document describes the instances to launch as such a
//! list, which is why the members that describe them (`imageId`, `minCount`, `maxCount`) are
//! recorded inside `instancesSet.items[0]` while the launch-wide ones (`instanceType`,
//! `subnetId`) stay beside it.
//!
//! [`super::audited_call_documents`] states that rule over one launch of one instance with
//! one tag specification. The rows here span what it generalises over that the one launch
//! does not: how many instances a call asks for (`MinCount`/`MaxCount` equal, a range, and
//! more than one of each), how many tag specifications it carries (none, one, several, each
//! with a different number of tags), and how many instances a terminate names — and every
//! value a row expects is JOINED to what the call's own response reported rather than
//! written out, so the record is pinned to the call it is the record of.

use serde_json::{json, Value};

use super::audited_call_documents::{served, REGION};
use super::ec2_query::{call, element, emulator, indexed, tag_specifications};
use super::wire::{child, child_text, document, param, set_items};

/// The CIDR of the VPC these launches go into.
const VPC_CIDR: &str = "10.43.0.0/16";

/// The CIDR of the subnet inside it, wide enough for every instance the rows launch.
const SUBNET_CIDR: &str = "10.43.0.0/24";

/// The type every launch here asks for: one launch-wide member, recorded beside the
/// per-instance ones rather than inside them.
const INSTANCE_TYPE: &str = "t3.micro";

/// The state EC2 answers a just-launched instance in.
const LAUNCHED_STATE: &str = "pending";

/// The state EC2 answers a terminate with.
const TERMINATING_STATE: &str = "shutting-down";

/// The states a just-launched instance can be in when the terminate reaches it, and so the
/// ones its `previousState` can name: the one a launch answers, and the one the clock moves
/// it to on its own. Named as a SET rather than one of them, so what the record carries is
/// pinned to a state the instance could really have been in without the assertion depending
/// on how fast the host ran the two calls.
const RUNNABLE_STATES: &[&str] = &[LAUNCHED_STATE, "running"];

/// One tag specification a launch carries: the resource type it is for, and its tags.
type TagSpecification = (&'static str, &'static [(&'static str, &'static str)]);

/// One launch this suite records, and the terminate of the instances it answered.
struct LaunchCase {
    /// What this row is about, named in every assertion it makes.
    case: &'static str,
    /// The `MinCount` the launch asks for.
    min_count: i32,
    /// The `MaxCount` it asks for.
    max_count: i32,
    /// The tag specifications it carries, in the order it sends them.
    tag_specifications: &'static [TagSpecification],
}

/// The launches the rule is stated over: the counts and the tag specifications differ row by
/// row, which is what the recorded grouping has to hold across.
const LAUNCHES: &[LaunchCase] = &[
    LaunchCase {
        case: "one instance, one tag specification",
        min_count: 1,
        max_count: 1,
        tag_specifications: &[("instance", &[("Name", "audited")])],
    },
    LaunchCase {
        case: "a count range, two tag specifications of unequal length",
        min_count: 1,
        max_count: 2,
        tag_specifications: &[
            ("instance", &[("Name", "fleet"), ("Stage", "test")]),
            ("volume", &[("Name", "fleet-root")]),
        ],
    },
    LaunchCase {
        case: "three instances, no tag specification",
        min_count: 3,
        max_count: 3,
        tag_specifications: &[],
    },
];

/// One instance a launch answered, as that launch's own response reported it.
struct LaunchedInstance {
    /// The id EC2 minted for it.
    instance_id: String,
    /// The subnet it was placed in.
    subnet_id: String,
    /// The state it was answered in.
    state: String,
}

/// One state change a terminate answered, as that terminate's own response reported it.
struct TerminatedInstance {
    /// The instance the change is about.
    instance_id: String,
    /// The state the terminate moved it to.
    current_state: String,
    /// The state it was in before.
    previous_state: String,
}

/// The value at the JSON pointer `path` of `document`, and `null` where the document holds
/// nothing there — an absent member being exactly what a row about the grouping asserts.
fn at(document: &Value, path: &str) -> Value {
    document.pointer(path).cloned().unwrap_or(Value::Null)
}

/// How many elements the value at `path` holds, or `None` when it is not a list at all.
fn items_at(document: &Value, path: &str) -> Option<usize> {
    at(document, path).as_array().map(Vec::len)
}

/// The instances a launch's response reported, in the order it reported them.
fn launched_instances(case: &str, body: &str) -> Vec<LaunchedInstance> {
    let parsed = document(case, body);
    set_items(parsed.root_element(), "instancesSet")
        .into_iter()
        .map(|item| LaunchedInstance {
            instance_id: child_text(item, "instanceId").unwrap_or_default(),
            subnet_id: child_text(item, "subnetId").unwrap_or_default(),
            state: child(item, "instanceState")
                .and_then(|state| child_text(state, "name"))
                .unwrap_or_default(),
        })
        .collect()
}

/// The state changes a terminate's response reported, in the order it reported them.
fn terminated_instances(case: &str, body: &str) -> Vec<TerminatedInstance> {
    let parsed = document(case, body);
    set_items(parsed.root_element(), "instancesSet")
        .into_iter()
        .map(|item| TerminatedInstance {
            instance_id: child_text(item, "instanceId").unwrap_or_default(),
            current_state: child(item, "currentState")
                .and_then(|state| child_text(state, "name"))
                .unwrap_or_default(),
            previous_state: child(item, "previousState")
                .and_then(|state| child_text(state, "name"))
                .unwrap_or_default(),
        })
        .collect()
}

/// RULE: a launch's record names the per-instance members of the call inside
/// `instancesSet.items`, the launch-wide ones beside it, its tag specifications under
/// `tagSpecificationSet.items`, and what the call answered under the same grouping — for
/// every count and tag specification a launch can carry.
#[tokio::test]
async fn a_launch_record_groups_the_call_the_way_ec2s_own_documents_do() {
    let (router, engine) = emulator();

    // The network every launch goes into, created through the API so the ids below are ones
    // this account really holds.
    let (vpc_body, _) = served(
        &router,
        &engine,
        "the network",
        "CreateVpc",
        &[param("CidrBlock", VPC_CIDR)],
    )
    .await;
    let vpc_id = element(&vpc_body, "vpcId")
        .unwrap_or_else(|| panic!("the create must answer a VPC id: {vpc_body}"));
    let (subnet_body, _) = served(
        &router,
        &engine,
        "the network",
        "CreateSubnet",
        &[param("VpcId", &vpc_id), param("CidrBlock", SUBNET_CIDR)],
    )
    .await;
    let subnet_id = element(&subnet_body, "subnetId")
        .unwrap_or_else(|| panic!("the create must answer a subnet id: {subnet_body}"));

    // An AMI of the published catalog, resolved the way a client does.
    let (images_status, images_body) = call(&router, REGION, "DescribeImages", &[]).await;
    assert_eq!(
        images_status, 200,
        "DescribeImages must be served, got {images_status} with body {images_body}"
    );
    let image_id = element(&images_body, "imageId")
        .unwrap_or_else(|| panic!("the catalog must publish an image: {images_body}"));

    for case in LAUNCHES {
        let label = case.case;
        let mut params = vec![
            param("ImageId", &image_id),
            param("InstanceType", INSTANCE_TYPE),
            param("MinCount", &case.min_count.to_string()),
            param("MaxCount", &case.max_count.to_string()),
            param("SubnetId", &subnet_id),
        ];
        params.extend(tag_specifications(case.tag_specifications));
        let (run_body, run_record) = served(&router, &engine, label, "RunInstances", &params).await;

        // The parameters: the per-instance launch specification is ONE element whatever the
        // count the launch asked for, and it holds the members that describe the instances
        // and nothing else.
        let requested = at(&run_record.document, "/requestParameters");
        assert_eq!(
            items_at(&requested, "/instancesSet/items"),
            Some(1),
            "case {label:?}: expected one launch specification in instancesSet.items, got \
             {requested}"
        );
        // The counts are the TEXT the request sent them as, which is the form this front
        // door records a call's parameters in by design: its module docs state that
        // `requestParameters` is the WIRE form of the call — "every value is the text the
        // request sent, where a document rendered from a built input would carry the
        // member's modelled type" (`src/service/management_events.rs`) — an ec2Query body
        // carrying no types at all. So a count is pinned as what was sent rather than
        // re-typed here.
        let expected_specification = json!({
            "imageId": image_id,
            "minCount": case.min_count.to_string(),
            "maxCount": case.max_count.to_string(),
        });
        let specification = at(&requested, "/instancesSet/items/0");
        assert_eq!(
            specification, expected_specification,
            "case {label:?}: expected the launch specification to be \
             {expected_specification}, got {specification}"
        );
        for (member, expected) in [
            ("instanceType", INSTANCE_TYPE.to_string()),
            ("subnetId", subnet_id.clone()),
        ] {
            let actual = at(&requested, &format!("/{member}"));
            assert_eq!(
                actual,
                json!(expected),
                "case {label:?}: expected the launch-wide {member} beside the launch \
                 specification to be {expected:?}, got {actual} in {requested}"
            );
        }

        // The tag specifications, each under its own element with its tags as key/value
        // pairs — and none recorded at all for a launch that carried none.
        if case.tag_specifications.is_empty() {
            let absent = at(&requested, "/tagSpecificationSet");
            assert_eq!(
                absent,
                Value::Null,
                "case {label:?}: a launch carrying no tag specification records none, got \
                 {absent} in {requested}"
            );
        } else {
            assert_eq!(
                items_at(&requested, "/tagSpecificationSet/items"),
                Some(case.tag_specifications.len()),
                "case {label:?}: expected {} tag specifications in \
                 tagSpecificationSet.items, got {requested}",
                case.tag_specifications.len()
            );
            for (position, (resource_type, tags)) in case.tag_specifications.iter().enumerate() {
                let expected_entry = json!({
                    "resourceType": resource_type,
                    "tags": tags
                        .iter()
                        .map(|(key, value)| json!({"key": key, "value": value}))
                        .collect::<Vec<Value>>(),
                });
                let actual = at(&requested, &format!("/tagSpecificationSet/items/{position}"));
                assert_eq!(
                    actual, expected_entry,
                    "case {label:?}: expected tag specification {position} to be \
                     {expected_entry}, got {actual}"
                );
            }
        }

        // What the launch answered, joined to the response the caller read. A launch answers
        // the LARGEST count it can between the two the request named, and this account holds
        // the capacity for all of them, so that is `MaxCount` instances.
        let launched = launched_instances(label, &run_body);
        assert_eq!(
            launched.len(),
            usize::try_from(case.max_count).unwrap_or_default(),
            "case {label:?}: expected a launch of {} to {} instances to answer {} of them, \
             got {} in {run_body}",
            case.min_count,
            case.max_count,
            case.max_count,
            launched.len()
        );
        let reservation_id = element(&run_body, "reservationId")
            .unwrap_or_else(|| panic!("case {label:?}: the launch must answer a reservation id: {run_body}"));
        let answered = at(&run_record.document, "/responseElements");
        let recorded_reservation = at(&answered, "/reservationId");
        assert_eq!(
            recorded_reservation,
            json!(reservation_id),
            "case {label:?}: expected the recorded reservationId to be {reservation_id:?}, \
             got {recorded_reservation} in {answered}"
        );
        assert_eq!(
            items_at(&answered, "/instancesSet/items"),
            Some(launched.len()),
            "case {label:?}: expected the record to carry the {} instances the launch \
             answered, got {answered}",
            launched.len()
        );
        for (position, instance) in launched.iter().enumerate() {
            assert_eq!(
                instance.state, LAUNCHED_STATE,
                "case {label:?}: expected the launch to answer instance {position} in \
                 {LAUNCHED_STATE:?}, got {:?} in {run_body}",
                instance.state
            );
            for (path, expected) in [
                (format!("/instancesSet/items/{position}/instanceId"), instance.instance_id.clone()),
                (format!("/instancesSet/items/{position}/subnetId"), instance.subnet_id.clone()),
                (
                    format!("/instancesSet/items/{position}/instanceState/name"),
                    instance.state.clone(),
                ),
            ] {
                let actual = at(&answered, &path);
                assert_eq!(
                    actual,
                    json!(expected),
                    "case {label:?}: expected {path} of the launch record to be \
                     {expected:?}, got {actual} in {answered}"
                );
            }
        }

        // The terminate of exactly those instances: a repeated member of ids on the way in,
        // a list of structures with nested structures on the way out.
        let instance_ids: Vec<&str> = launched
            .iter()
            .map(|instance| instance.instance_id.as_str())
            .collect();
        let (terminate_body, terminate_record) = served(
            &router,
            &engine,
            label,
            "TerminateInstances",
            &indexed("InstanceId", &instance_ids),
        )
        .await;
        let terminated = terminated_instances(label, &terminate_body);
        let named = at(&terminate_record.document, "/requestParameters");
        assert_eq!(
            items_at(&named, "/instancesSet/items"),
            Some(instance_ids.len()),
            "case {label:?}: expected the {} instances the terminate named in \
             instancesSet.items, got {named}",
            instance_ids.len()
        );
        for (position, instance_id) in instance_ids.iter().enumerate() {
            let expected_entry = json!({"instanceId": instance_id});
            let actual = at(&named, &format!("/instancesSet/items/{position}"));
            assert_eq!(
                actual, expected_entry,
                "case {label:?}: expected the terminate's named instance {position} to be \
                 {expected_entry}, got {actual}"
            );
        }
        let changed = at(&terminate_record.document, "/responseElements");
        assert_eq!(
            items_at(&changed, "/instancesSet/items"),
            Some(terminated.len()),
            "case {label:?}: expected the record to carry the {} state changes the \
             terminate answered, got {changed}",
            terminated.len()
        );
        for (position, change) in terminated.iter().enumerate() {
            assert_eq!(
                change.current_state, TERMINATING_STATE,
                "case {label:?}: expected the terminate to answer instance {position} in \
                 {TERMINATING_STATE:?}, got {:?} in {terminate_body}",
                change.current_state
            );
            assert!(
                RUNNABLE_STATES.contains(&change.previous_state.as_str()),
                "case {label:?}: expected the state instance {position} was in before the \
                 terminate to be one of {RUNNABLE_STATES:?}, got {:?} in {terminate_body}",
                change.previous_state
            );
            for (path, expected) in [
                (
                    format!("/instancesSet/items/{position}/instanceId"),
                    change.instance_id.clone(),
                ),
                (
                    format!("/instancesSet/items/{position}/currentState/name"),
                    change.current_state.clone(),
                ),
                (
                    format!("/instancesSet/items/{position}/previousState/name"),
                    change.previous_state.clone(),
                ),
            ] {
                let actual = at(&changed, &path);
                assert_eq!(
                    actual,
                    json!(expected),
                    "case {label:?}: expected {path} of the terminate record to be \
                     {expected:?}, got {actual} in {changed}"
                );
            }
        }
    }
}
