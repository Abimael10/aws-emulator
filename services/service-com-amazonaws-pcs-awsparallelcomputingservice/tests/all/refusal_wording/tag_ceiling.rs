// SPDX-License-Identifier: CC0-1.0

//! The resource tag-count ceiling, asked end to end over the awsJson1_0 wire (`POST /`
//! with `X-Amz-Target: AWSParallelComputingService.<Operation>`) through the same router
//! the emulator serves from, on the harness [`crate::cluster_lifecycle`] builds.
//!
//! RULE under test: a PCS resource holds at most `MAX_TAGS_PER_RESOURCE` tags, and a call
//! that would leave it holding more is refused whole with the modelled
//! `ServiceQuotaExceededException` — the same status, the same sentence and the kind of
//! resource it is about — whichever path writes the tags: the `tags` a create carries, for
//! each of the three kinds a create mints, or a later `TagResource` that merges into a set
//! the resource already holds. Because the question is asked in one place
//! (`crate::state::tag_quota_violation`), one answer here is the answer on every path, and
//! a path that stopped asking it would show up as a row of this table.
//!
//! The rows span what the rule generalises over: which operation writes the tags, which
//! kind of resource it writes them to, and whether the tags arrive on the create or on a
//! merge into a set already at the ceiling — a merge of KEYS ALREADY HELD being the case
//! that does not grow the set and is therefore still served.
//!
//! Each test builds its own engine, so the state one test writes is invisible to the
//! others, and nothing here waits on the wall clock.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use service_com_amazonaws_pcs_awsparallelcomputingservice::service::wire::CLIENT_ERROR_STATUS;
use service_com_amazonaws_pcs_awsparallelcomputingservice::state::MAX_TAGS_PER_RESOURCE;
use smithy_server_runtime::MultiServiceRouter;

use crate::cluster_lifecycle::{
    call, create_cluster, create_compute_node_group, create_queue, emulator,
};

/// The modelled error a reached ceiling is refused with.
const QUOTA_ERROR: &str = "ServiceQuotaExceededException";

/// The `serviceCode` the modelled error carries (`@required`): this service's own code.
const SERVICE_CODE: &str = "pcs";

/// The cluster every nested resource in this suite is created inside.
const HOST_CLUSTER: &str = "tag-ceiling-host";

/// The compute node group the queues of this suite assign their jobs to.
const HOST_NODES: &str = "tag-ceiling-nodes";

/// The sentence the service states for a tag set past the ceiling. AWS words the rule
/// about the set the write would leave behind and names neither the keys nor the resource,
/// so the ceiling is the only variable part.
fn tag_ceiling_sentence() -> String {
    format!("The resultant tag set must not have more than {MAX_TAGS_PER_RESOURCE} user tags.")
}

/// `count` distinct tags, keyed by `prefix`.
fn bulk_tags(prefix: &str, count: usize) -> BTreeMap<String, String> {
    (0..count)
        .map(|index| (format!("{prefix}-{index:03}"), format!("value-{index:03}")))
        .collect()
}

/// One path that writes tags on a create: the operation, the kind of resource it mints and
/// how that resource is asked for again afterwards.
struct Row {
    /// How a failing assertion names this row.
    case: &'static str,
    /// The create under test.
    operation: &'static str,
    /// The member the created resource travels in, in the create's and the reader's output.
    resource: &'static str,
    /// The reader that answers whether the refused create persisted anything.
    reader: &'static str,
    /// The identifier member that reader takes for a resource nested in a cluster, or
    /// `None` for the cluster itself, which the reader names by `clusterIdentifier`.
    identifier: Option<&'static str>,
    /// The kind of resource the refusal reports in `resourceType`: the segment that names
    /// this kind in a PCS ARN.
    resource_type: &'static str,
    /// The create request for `name`, inside the host cluster, carrying `tags`.
    request: fn(&str, &str, &BTreeMap<String, String>) -> Value,
}

/// A `CreateCluster` request carrying `tags`. A cluster is not nested, so the host cluster
/// is not its parent.
fn cluster_request(_host: &str, name: &str, tags: &BTreeMap<String, String>) -> Value {
    let mut request = create_cluster(name, "SMALL");
    request["tags"] = json!(tags);
    request
}

/// A `CreateComputeNodeGroup` request inside `host`, carrying `tags`.
fn compute_node_group_request(host: &str, name: &str, tags: &BTreeMap<String, String>) -> Value {
    let mut request = create_compute_node_group(host, name);
    request["tags"] = json!(tags);
    request
}

/// A `CreateQueue` request inside `host`, carrying `tags`.
fn queue_request(host: &str, name: &str, tags: &BTreeMap<String, String>) -> Value {
    let mut request = create_queue(host, name, &[HOST_NODES]);
    request["tags"] = json!(tags);
    request
}

/// Every path that writes tags on a create: the three kinds a create mints.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a cluster create",
            operation: "CreateCluster",
            resource: "cluster",
            reader: "GetCluster",
            identifier: None,
            resource_type: "cluster",
            request: cluster_request,
        },
        Row {
            case: "a compute node group create",
            operation: "CreateComputeNodeGroup",
            resource: "computeNodeGroup",
            reader: "GetComputeNodeGroup",
            identifier: Some("computeNodeGroupIdentifier"),
            resource_type: "computenodegroup",
            request: compute_node_group_request,
        },
        Row {
            case: "a queue create",
            operation: "CreateQueue",
            resource: "queue",
            reader: "GetQueue",
            identifier: Some("queueIdentifier"),
            resource_type: "queue",
            request: queue_request,
        },
    ]
}

/// The reader request of `row` for the resource called `name` inside `host`.
fn read_request(row: &Row, host: &str, name: &str) -> Value {
    match row.identifier {
        None => json!({"clusterIdentifier": name}),
        Some(member) => json!({"clusterIdentifier": host, member: name}),
    }
}

/// Every assertion a tag-ceiling refusal must satisfy, over one row: one status, the
/// modelled error, the service's own sentence, and the kind of resource and the service it
/// is about in the error's modelled members.
fn assert_tag_ceiling_refusal(case: &str, status: u16, body: &Value, expected_resource_type: &str) {
    assert_eq!(
        status, CLIENT_ERROR_STATUS,
        "case {case}: expected the client-error status {CLIENT_ERROR_STATUS}, got {status} {body}"
    );
    assert_eq!(
        body["__type"].as_str(),
        Some(QUOTA_ERROR),
        "case {case}: expected {QUOTA_ERROR}, got {body}"
    );
    let expected_message = tag_ceiling_sentence();
    assert_eq!(
        body["message"].as_str(),
        Some(expected_message.as_str()),
        "case {case}: expected the sentence {expected_message:?}, got {body}"
    );
    assert_eq!(
        body["resourceType"].as_str(),
        Some(expected_resource_type),
        "case {case}: expected the kind of resource in resourceType, got {body}"
    );
    assert_eq!(
        body["serviceCode"].as_str(),
        Some(SERVICE_CODE),
        "case {case}: expected serviceCode {SERVICE_CODE:?}, got {body}"
    );
}

/// The tags `ListTagsForResource` reports for `arn`.
async fn listed_tags(router: &MultiServiceRouter, case: &str, arn: &str) -> BTreeMap<String, String> {
    let (status, body) = call(router, "ListTagsForResource", &json!({"resourceArn": arn})).await;
    assert_eq!(
        status, 200,
        "case {case}: expected ListTagsForResource to answer 200, got {status} {body}"
    );
    body["tags"]
        .as_object()
        .unwrap_or_else(|| panic!("case {case}: expected a tags map, got {body}"))
        .iter()
        .map(|(key, value)| (key.clone(), value.as_str().unwrap_or_default().to_string()))
        .collect()
}

/// The host cluster and the compute node group this suite's queues assign jobs to. The
/// request that follows a create observes the resource settled, so there is nothing to
/// wait for; neither create carries `tags`, which is the write that has no tags to count.
async fn host(router: &MultiServiceRouter) -> String {
    let (status, created) = call(
        router,
        "CreateCluster",
        &create_cluster(HOST_CLUSTER, "SMALL"),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected the host cluster to be created, got {status} {created}"
    );
    let cluster = created["cluster"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    let (status, group) = call(
        router,
        "CreateComputeNodeGroup",
        &create_compute_node_group(&cluster, HOST_NODES),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected the host compute node group to be created, got {status} {group}"
    );
    cluster
}

#[tokio::test]
async fn a_create_at_the_resource_tag_ceiling_stores_every_tag_and_one_past_it_is_refused() {
    let router = emulator();
    let cluster = host(&router).await;

    for row in rows() {
        // At the ceiling the create is served and the resource holds every tag it asked
        // for: the ceiling bounds the resulting set, it does not shrink it.
        let at_the_ceiling = bulk_tags("created", MAX_TAGS_PER_RESOURCE);
        let name = "tag-ceiling-at";
        let (status, body) = call(
            &router,
            row.operation,
            &(row.request)(&cluster, name, &at_the_ceiling),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {} at the ceiling: expected HTTP 200 for {MAX_TAGS_PER_RESOURCE} tags, got \
             {status} {body}",
            row.case
        );
        let arn = body[row.resource]["arn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {} at the ceiling: no ARN in {body}", row.case))
            .to_string();
        let stored = listed_tags(&router, row.case, &arn).await;
        assert_eq!(
            stored, at_the_ceiling,
            "case {} at the ceiling: expected the create's {MAX_TAGS_PER_RESOURCE} tags stored, \
             got {stored:?}",
            row.case
        );

        // One tag past the ceiling earns the one refusal, and the resource the create
        // named does not exist: a refused create persists nothing.
        let past_the_ceiling = bulk_tags("created", MAX_TAGS_PER_RESOURCE + 1);
        let refused_name = "tag-ceiling-past";
        let (status, refused) = call(
            &router,
            row.operation,
            &(row.request)(&cluster, refused_name, &past_the_ceiling),
        )
        .await;
        assert_tag_ceiling_refusal(row.case, status, &refused, row.resource_type);
        assert_eq!(
            refused["resourceId"].as_str(),
            Some(refused_name),
            "case {}: expected the refusal to report the resource the request named, got {refused}",
            row.case
        );

        let (status, missing) = call(
            &router,
            row.reader,
            &read_request(&row, &cluster, refused_name),
        )
        .await;
        assert_eq!(
            status, CLIENT_ERROR_STATUS,
            "case {}: expected the refused create to have persisted nothing, got {status} \
             {missing}",
            row.case
        );

        // A later TagResource on the resource that IS at the ceiling asks the same
        // question of the set the write would LEAVE BEHIND: one new key is past it and is
        // refused, while re-writing keys the resource already holds grows nothing and is
        // served.
        let (status, refused) = call(
            &router,
            "TagResource",
            &json!({"resourceArn": &arn, "tags": {"one-key-too-many": "yes"}}),
        )
        .await;
        assert_tag_ceiling_refusal(
            &format!("{} then a TagResource adding a key", row.case),
            status,
            &refused,
            row.resource_type,
        );

        let mut edits = BTreeMap::new();
        for (key, value) in at_the_ceiling.iter().take(2) {
            edits.insert(key.clone(), format!("{value}-edited"));
        }
        let (status, body) = call(
            &router,
            "TagResource",
            &json!({"resourceArn": &arn, "tags": &edits}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {} then a TagResource editing keys already held: expected HTTP 200, got \
             {status} {body}",
            row.case
        );
        let mut expected = at_the_ceiling.clone();
        expected.extend(edits);
        let after = listed_tags(&router, row.case, &arn).await;
        assert_eq!(
            after, expected,
            "case {} then a TagResource editing keys already held: expected the edits and no \
             growth, got {after:?}",
            row.case
        );
    }
}
