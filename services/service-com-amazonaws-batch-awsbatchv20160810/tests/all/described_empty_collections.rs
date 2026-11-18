// SPDX-License-Identifier: CC0-1.0

//! What a READ reports for the modelled COLLECTIONS of a compute environment and of a job,
//! end to end over the wire.
//!
//! RULE under test: a `Describe*` reader MATERIALISES the collections of the document it
//! reports — a modelled list or map the resource holds nothing in comes back PRESENT and
//! EMPTY rather than omitted — because "absent" and "empty" are different answers.
//! `ComputeEnvironmentDetail$computeResources` targets `ComputeResource`, whose
//! `instanceTypes` is a `StringList` and not an optional scalar, so a client reads
//! `computeResources["instanceTypes"] == []` of an environment whose compute type is a
//! Fargate one and concludes that no instance types — and therefore no vCPU floors — are
//! configured. `JobDetail$attempts` is "a list of job attempts that are associated with this
//! job", so a job that has made none reports `[]` and `len(job["attempts"])` is the evidence
//! that nothing has run yet. The same holds for the rest of each document: the `tags` map of
//! a compute environment's resources, and a job's `dependsOn` and `parameters`.
//!
//! An absent STRUCTURE is not an empty one, which the unmanaged-environment case pins: an
//! environment Batch manages no resources for reports no `computeResources` document at all,
//! and nothing invents one.
//!
//! The rows differ along what the rule generalises over: which compute type an environment
//! has, which collections its create states and how many entries each carries, which
//! collections a submit states, and where in its life a job is read — just submitted, held
//! in `RUNNABLE` by a queue with no capacity, and placed and ended.

use serde_json::{json, Value};

use service_com_amazonaws_batch_awsbatchv20160810::ops::types::C_R_TYPE_VALUES;
use service_com_amazonaws_batch_awsbatchv20160810::state;

use crate::array_jobs_and_placement::managed_compute_environment;
use crate::job_lifecycle::{
    compute_environment, container_properties, describe_job, job_definition, job_queue,
    until_status, Batch,
};

/// The LIST members of `ComputeResource`, from
/// `smithy_query(query = "shape", shape = "ComputeResource")`: the four a described
/// environment materialises. `capacityTags` is deliberately not among them — its own length
/// constraint publishes a minimum of one entry, so the empty map is not a value that member
/// can take — and `tags` is a map, asserted beside these.
const COMPUTE_RESOURCE_LISTS: &[&str] = &[
    "instanceTypes",
    "subnets",
    "securityGroupIds",
    "ec2Configuration",
];

/// The members of `ContainerDetail` the job definition and the submit's overrides state
/// here, from `smithy_query(query = "shape", shape = "ContainerDetail")`: the effective
/// container is reported as the record holds it, which is what the rows below check.
const CONTAINER_LISTS: &[&str] = &["command", "resourceRequirements"];

/// One row of the compute-environment table: an environment of `compute_type` created with
/// `resources` as its `computeResources`, whose described collections must be exactly what
/// the create stated and empty everywhere else.
struct Environment {
    case: &'static str,
    compute_type: &'static str,
    /// The `computeResources` members the create states beside `type` and `maxvCpus`.
    resources: fn() -> Value,
}

/// One row per modelled `CRType`, differing in which collections the create states and in
/// how many entries each of them carries.
const ENVIRONMENTS: &[Environment] = &[
    Environment {
        case: "a Fargate environment states no EC2-only collection",
        compute_type: "FARGATE",
        resources: || json!({"subnets": ["subnet-1111"], "securityGroupIds": ["sg-1111"]}),
    },
    Environment {
        case: "a Fargate Spot environment states two subnets",
        compute_type: "FARGATE_SPOT",
        resources: || {
            json!({
                "subnets": ["subnet-1111", "subnet-2222"],
                "securityGroupIds": ["sg-1111"],
            })
        },
    },
    Environment {
        case: "an EC2 environment states its instance types and its capacity tags",
        compute_type: "EC2",
        resources: || {
            json!({
                "instanceTypes": ["optimal"],
                "subnets": ["subnet-1111"],
                "securityGroupIds": ["sg-1111"],
                "tags": {"Name": "batch-instance"},
            })
        },
    },
    Environment {
        case: "a Spot environment states two instance families and an ec2Configuration",
        compute_type: "SPOT",
        resources: || {
            json!({
                "instanceTypes": ["c5", "m5"],
                "ec2Configuration": [{"imageType": "ECS_AL2023"}],
                "subnets": ["subnet-1111"],
                "securityGroupIds": ["sg-1111"],
                "bidPercentage": 60,
            })
        },
    },
    Environment {
        case: "an ECS Managed Instances environment states only the two required members",
        compute_type: "ECS_MANAGED_INSTANCES",
        resources: || json!({}),
    },
];

/// The one `ComputeEnvironmentDetail` `DescribeComputeEnvironments` reports for `reference`.
async fn describe_environment(batch: &Batch, reference: &str) -> Value {
    let body = batch
        .ok(
            "/v1/describecomputeenvironments",
            json!({"computeEnvironments": [reference]}),
        )
        .await;
    let listed = body["computeEnvironments"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        listed.len(),
        1,
        "case describe {reference}: expected exactly one ComputeEnvironmentDetail, got {body}"
    );
    listed[0].clone()
}

/// RULE: every modelled collection of a described environment's `computeResources` is
/// present — carrying what the create stated, and EMPTY where the create stated nothing.
#[tokio::test]
async fn a_described_environment_reports_every_collection_of_its_compute_resources() {
    assert_eq!(
        ENVIRONMENTS.len(),
        C_R_TYPE_VALUES.len(),
        "case the CRType table: every modelled compute type needs a row, got {:?}",
        C_R_TYPE_VALUES
    );
    let batch = Batch::new();
    for (index, row) in ENVIRONMENTS.iter().enumerate() {
        let case = row.case;
        let name = format!("ce-collections-{index}");
        let mut resources = (row.resources)();
        resources["type"] = json!(row.compute_type);
        resources["maxvCpus"] = json!(16);
        batch
            .ok(
                "/v1/createcomputeenvironment",
                json!({
                    "computeEnvironmentName": &name,
                    "type": "MANAGED",
                    "state": "ENABLED",
                    "computeResources": &resources,
                }),
            )
            .await;

        let detail = describe_environment(&batch, &name).await;
        let described = detail
            .get("computeResources")
            .unwrap_or_else(|| panic!("case {case}: expected computeResources, got {detail}"));
        for member in COMPUTE_RESOURCE_LISTS {
            // What the create stated for this member, or the empty list where it stated
            // nothing: the answer is derived from the request, never from a fixed table.
            let wanted = resources.get(member).cloned().unwrap_or_else(|| json!([]));
            let actual = described
                .get(member)
                .unwrap_or_else(|| panic!("case {case}: expected {member}, got {described}"));
            assert_eq!(
                actual, &wanted,
                "case {case}: expected {member} {wanted}, got {actual}"
            );
        }
        let wanted_tags = resources.get("tags").cloned().unwrap_or_else(|| json!({}));
        let tags = described
            .get("tags")
            .unwrap_or_else(|| panic!("case {case}: expected the tags map, got {described}"));
        assert_eq!(
            tags, &wanted_tags,
            "case {case}: expected tags {wanted_tags}, got {tags}"
        );
        // The scalars the create stated are untouched by the projection.
        assert_eq!(
            described.get("maxvCpus"),
            Some(&json!(16)),
            "case {case}: expected the stated maxvCpus, got {described}"
        );
        assert_eq!(
            described.get("type").and_then(Value::as_str),
            Some(row.compute_type),
            "case {case}: expected the stated compute type, got {described}"
        );
    }
}

/// RULE: an absent STRUCTURE is not an empty one — an environment whose resources Batch does
/// not manage reports no `computeResources` document, and so no collections of one.
#[tokio::test]
async fn an_unmanaged_environment_reports_no_compute_resources_document() {
    let batch = Batch::new();
    compute_environment(&batch, "ce-unmanaged-collections").await;
    let detail = describe_environment(&batch, "ce-unmanaged-collections").await;
    assert!(
        detail.get("computeResources").is_none_or(Value::is_null),
        "case an UNMANAGED environment: nothing invents compute resources for it, got {detail}"
    );
    // Its own `tags` map is materialised all the same: that member belongs to the
    // environment, which exists.
    assert_eq!(
        detail.get("tags"),
        Some(&json!({})),
        "case an UNMANAGED environment: expected the materialised tags map, got {detail}"
    );
}

/// One row of the job table: a job submitted to a queue that does or does not offer the
/// scheduler capacity, read where `wait_for` says, and the collections it must then report.
struct Job {
    case: String,
    /// Whether the job goes on the queue over MANAGED, ENABLED capacity (so the clock places
    /// it) or on the one over capacity nothing can place a job on.
    placed: bool,
    /// Members merged into the submit beside `jobName`, `jobQueue` and `jobDefinition`.
    extra: Value,
    /// The status to wait for before reading, or `None` to read straight after the submit.
    wait_for: Option<&'static str>,
    /// How many `AttemptDetail`s the job must report: one per placement it has had.
    attempts: usize,
}

/// The array at `member` of `document`, which must be PRESENT.
fn list<'doc>(case: &str, document: &'doc Value, member: &str) -> &'doc Vec<Value> {
    document
        .get(member)
        .and_then(Value::as_array)
        .unwrap_or_else(|| {
            panic!("case {case}: expected {member} to be a present list, got {document}")
        })
}

/// RULE: every modelled collection of a described job is present — `attempts` carrying one
/// entry per placement and EMPTY for a job nothing placed, and `dependsOn` and `parameters`
/// carrying what the submit stated and empty where it stated nothing.
#[tokio::test]
async fn a_described_job_reports_every_collection_of_its_document() {
    let batch = Batch::new();
    let placing = managed_compute_environment(&batch, "ce-jobcoll", state::STATE_ENABLED).await;
    let placing_queue = job_queue(&batch, "q-jobcoll-placed", &placing).await;
    let resting = compute_environment(&batch, "ce-jobcoll-resting").await;
    let resting_queue = job_queue(&batch, "q-jobcoll-resting", &resting).await;
    let (definition, _) = job_definition(&batch, "jd-jobcoll", container_properties()).await;

    // A job to depend on, so one row can state a `dependsOn` the service accepts.
    let dependency = batch
        .ok(
            "/v1/submitjob",
            json!({
                "jobName": "jobcoll-dependency",
                "jobQueue": &resting_queue,
                "jobDefinition": &definition,
            }),
        )
        .await["jobId"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    let rows = [
        Job {
            case: "a job just submitted".to_string(),
            placed: false,
            extra: json!({}),
            wait_for: None,
            attempts: 0,
        },
        Job {
            case: "a job held in RUNNABLE by a queue with no capacity".to_string(),
            placed: false,
            extra: json!({}),
            wait_for: Some(state::JOB_RUNNABLE),
            attempts: 0,
        },
        Job {
            case: "a job the clock placed and ran".to_string(),
            placed: true,
            extra: json!({}),
            wait_for: Some(state::JOB_SUCCEEDED),
            attempts: 1,
        },
        Job {
            case: "a job submitted with parameters, a dependency and an environment override"
                .to_string(),
            placed: false,
            extra: json!({
                "parameters": {"shard": "7"},
                "dependsOn": [{"jobId": &dependency}],
                "containerOverrides": {"environment": [{"name": "MODE", "value": "diagnose"}]},
            }),
            wait_for: None,
            attempts: 0,
        },
        Job {
            case: "a placed job submitted with two parameters".to_string(),
            placed: true,
            extra: json!({"parameters": {"shard": "1", "size": "2"}}),
            wait_for: Some(state::JOB_SUCCEEDED),
            attempts: 1,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case.as_str();
        let queue = if row.placed {
            &placing_queue
        } else {
            &resting_queue
        };
        let mut request = json!({
            "jobName": format!("jobcoll-{index}"),
            "jobQueue": queue,
            "jobDefinition": &definition,
        });
        for (member, value) in row.extra.as_object().cloned().unwrap_or_default() {
            request[member] = value;
        }
        let job_id = batch.ok("/v1/submitjob", request).await["jobId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: expected a jobId"))
            .to_string();

        let detail = match row.wait_for {
            Some(status) => until_status(&batch, case, &job_id, status).await,
            None => describe_job(&batch, &job_id).await,
        };

        assert_eq!(
            list(case, &detail, "attempts").len(),
            row.attempts,
            "case {case}: expected {} attempt(s) as a present list, got {detail}",
            row.attempts
        );
        let wanted_dependencies = row
            .extra
            .get("dependsOn")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        assert_eq!(
            list(case, &detail, "dependsOn").len(),
            wanted_dependencies,
            "case {case}: expected {wanted_dependencies} dependency entries, got {detail}"
        );
        let wanted_parameters = row
            .extra
            .get("parameters")
            .cloned()
            .unwrap_or_else(|| json!({}));
        assert_eq!(
            detail.get("parameters"),
            Some(&wanted_parameters),
            "case {case}: expected parameters {wanted_parameters}, got {detail}"
        );
        assert_eq!(
            detail.get("tags"),
            Some(&json!({})),
            "case {case}: expected the materialised tags map, got {detail}"
        );

        let container = detail
            .get("container")
            .unwrap_or_else(|| panic!("case {case}: expected the job's container, got {detail}"));
        for member in CONTAINER_LISTS {
            list(case, container, member);
        }
        // What the job definition and the submit's overrides state is reported as stated.
        assert_eq!(
            container.get("command"),
            Some(&json!(["echo", "hi"])),
            "case {case}: expected the definition's command, got {container}"
        );
        assert_eq!(
            list(case, container, "resourceRequirements").len(),
            2,
            "case {case}: expected the definition's two resource requirements, got {container}"
        );
        let wanted_environment = row.extra.pointer("/containerOverrides/environment");
        assert_eq!(
            container.get("environment"),
            wanted_environment,
            "case {case}: expected the submitted environment override {wanted_environment:?}, \
             got {container}"
        );
    }
}
