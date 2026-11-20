// SPDX-License-Identifier: CC0-1.0

//! The CAPACITY a consumable resource bounds, end to end over the wire.
//!
//! RULES under test (`smithy_query(query="shape", shape="ConsumableResourceSummary")`):
//!
//! * `inUseQuantity` is "the amount of the consumable resource that is currently in use" and
//!   `availableQuantity` is what is left of `totalQuantity` ("the total amount of the
//!   consumable resource that is available") once that is taken off — so while a job that
//!   claims a quantity through `consumableResourceProperties.consumableResourceList` is
//!   running, the resource reports that quantity in use and that much less available;
//! * the quantity is a CONCURRENCY bound, so a second claimant of a quantity that is all in
//!   use is not admitted onto capacity: it waits where a job waits to be placed until the
//!   holder releases it;
//! * what releasing MEANS is the resource's `resourceType`, which "indicates whether the
//!   resource is available to be re-used after a job completes": a `REPLENISHABLE` resource
//!   is, so its whole total is offered again, while a `NON_REPLENISHABLE` one is not — the
//!   quantity a completed job consumed is gone from its total as well;
//! * `DeleteConsumableResource` is refused while any of the quantity is in use, with the
//!   modelled `ClientException`, and the resource stays exactly as it was.
//!
//! The rows span what the rules generalise over: the resource's type, the total it holds, the
//! quantity a job claims of it, how many claimants there are, and whether a claim names the
//! resource by name or by ARN.

use std::time::Duration;

use serde_json::{json, Value};

use service_com_amazonaws_batch_awsbatchv20160810::state;

use crate::array_jobs_and_placement::managed_compute_environment;
use crate::job_lifecycle::{
    container_properties, describe_job, error_code, error_message, job_queue, until_status, Batch,
};
use crate::jobs_by_consumable_resource::resource_arn;

/// How many times a loop looks for a condition before giving up.
///
/// A job's whole life here is a handful of clock-owed moves, each falling due
/// `state::JOB_PHASE_MILLIS` after the one before it, so this many looks at [`poll_gap`]
/// apart is many spans of headroom on a loaded machine — and a loop ends on the CONDITION,
/// never on a deadline of its own.
const POLLS: usize = 3_000;

/// How long one look waits before the next: far below one span of the clock, so a condition
/// that holds for only one span is still observed on a machine whose other work keeps
/// interrupting this one.
fn poll_gap() -> Duration {
    Duration::from_millis(10)
}

/// Several spans of the clock: what a case waits through to show that a job it expects to
/// STAY where it is really does.
fn several_spans() -> Duration {
    let span = u64::try_from(state::JOB_PHASE_MILLIS).unwrap_or(500);
    Duration::from_millis(span.saturating_mul(3))
}

/// `DescribeConsumableResource` for `reference`.
async fn describe_resource(batch: &Batch, reference: &str) -> Value {
    batch
        .ok(
            "/v1/describeconsumableresource",
            json!({"consumableResource": reference}),
        )
        .await
}

/// Create a consumable resource called `name` of `resource_type` holding `total`.
async fn create_resource(batch: &Batch, name: &str, resource_type: &str, total: i64) {
    batch
        .ok(
            "/v1/createconsumableresource",
            json!({
                "consumableResourceName": name,
                "resourceType": resource_type,
                "totalQuantity": total,
            }),
        )
        .await;
}

/// Register a job definition called `name` whose jobs claim `quantity` of `reference`, and
/// answer its ARN.
async fn claiming_definition(
    batch: &Batch,
    name: &str,
    reference: &str,
    quantity: i64,
) -> String {
    let registered = batch
        .ok(
            "/v1/registerjobdefinition",
            json!({
                "jobDefinitionName": name,
                "type": "container",
                "containerProperties": container_properties(),
                "consumableResourceProperties": {
                    "consumableResourceList": [
                        {"consumableResource": reference, "quantity": quantity},
                    ],
                },
            }),
        )
        .await;
    registered["jobDefinitionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {name}: expected a jobDefinitionArn, got {registered}"))
        .to_string()
}

/// Submit one job called `job_name` through `definition` and answer its id.
async fn submit(batch: &Batch, queue_arn: &str, definition: &str, job_name: &str) -> String {
    let answered = batch
        .ok(
            "/v1/submitjob",
            json!({
                "jobName": job_name,
                "jobQueue": queue_arn,
                "jobDefinition": definition,
            }),
        )
        .await;
    answered["jobId"]
        .as_str()
        .unwrap_or_else(|| panic!("case submit {job_name}: expected a jobId, got {answered}"))
        .to_string()
}

/// The statuses of a job that is LIVE past its queue — admitted onto capacity and not
/// finished with it. Source: the `JobStatus` enum (`smithy_query(query="shape",
/// shape="JobStatus")`), whose order is SUBMITTED, PENDING, RUNNABLE, STARTING, RUNNING,
/// SUCCEEDED, FAILED.
const LIVE_PAST_THE_QUEUE: [&str; 2] = [state::JOB_STARTING, state::JOB_RUNNING];

/// The terminal pair of that enum: a job no later call can move again, and so no claimant of
/// anything.
const TERMINAL: [&str; 2] = [state::JOB_SUCCEEDED, state::JOB_FAILED];

/// One row of the accounting table: the resource a job claims from and what it claims.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    resource_type: &'static str,
    /// What the resource holds.
    total: i64,
    /// What the job's requirement asks for.
    claim: i64,
    /// Whether the requirement names the resource by its ARN rather than by its name.
    by_arn: bool,
}

/// The rows: both documented resource types, a resource whose whole total one job takes and
/// one that keeps quantity spare, and both identifier forms.
const ROWS: &[Row] = &[
    Row {
        case: "replenishable, the whole total claimed by name",
        resource_type: state::RESOURCE_TYPE_REPLENISHABLE,
        total: 1,
        claim: 1,
        by_arn: false,
    },
    Row {
        case: "replenishable, part of the total claimed by ARN",
        resource_type: state::RESOURCE_TYPE_REPLENISHABLE,
        total: 10,
        claim: 4,
        by_arn: true,
    },
    Row {
        case: "non-replenishable, the whole total claimed by ARN",
        resource_type: state::RESOURCE_TYPE_NON_REPLENISHABLE,
        total: 1,
        claim: 1,
        by_arn: true,
    },
    Row {
        case: "non-replenishable, part of the total claimed by name",
        resource_type: state::RESOURCE_TYPE_NON_REPLENISHABLE,
        total: 7,
        claim: 3,
        by_arn: false,
    },
];

/// RULE: a job that has been admitted onto capacity HOLDS the quantity it claimed —
/// `inUseQuantity` reports it and `availableQuantity` is the total less it — and gives it
/// back when the job goes terminal: the whole total is offered again by a `REPLENISHABLE`
/// resource, while a `NON_REPLENISHABLE` one loses the consumed quantity from its total too.
#[tokio::test]
async fn a_running_job_holds_the_quantity_it_claimed_and_releases_it_at_the_end() {
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let batch = Batch::new();
        let environment =
            managed_compute_environment(&batch, &format!("ce-hold{index}"), state::STATE_ENABLED)
                .await;
        let queue_arn = job_queue(&batch, &format!("q-hold{index}"), &environment).await;
        let name = format!("cr-hold{index}");
        create_resource(&batch, &name, row.resource_type, row.total).await;
        let reference = if row.by_arn {
            resource_arn(&name)
        } else {
            name.clone()
        };
        let definition =
            claiming_definition(&batch, &format!("jd-hold{index}"), &reference, row.claim).await;

        // Before anything is admitted, nothing of the resource is in use.
        let before = describe_resource(&batch, &name).await;
        assert_eq!(
            (
                before["totalQuantity"].as_i64(),
                before["inUseQuantity"].as_i64(),
                before["availableQuantity"].as_i64()
            ),
            (Some(row.total), Some(0), Some(row.total)),
            "case {case}: a resource nothing claims holds its whole total, got {before}"
        );

        // The hold is OBSERVED while a holder is LIVE: the loop looks for the quantity in use
        // rather than waiting a fixed time for it, and — where the resource can take another
        // holder — submits a fresh one as soon as the one it was watching is done, so what it
        // waits for is the CONDITION and not one window of the clock.
        let replenishable = row.resource_type == state::RESOURCE_TYPE_REPLENISHABLE;
        let mut holder = submit(&batch, &queue_arn, &definition, "holder").await;
        let mut holders = vec![holder.clone()];
        let mut held: Option<Value> = None;
        for _ in 0..POLLS {
            let described = describe_resource(&batch, &name).await;
            match described["inUseQuantity"].as_i64() {
                Some(0) => {}
                Some(quantity) if quantity == row.claim => {
                    held = Some(described);
                    break;
                }
                _ => panic!(
                    "case {case}: a job claiming {} put something else in use, got {described}",
                    row.claim
                ),
            }
            if replenishable {
                let detail = describe_job(&batch, &holder).await;
                if TERMINAL.contains(&detail["status"].as_str().unwrap_or_default()) {
                    holder = submit(&batch, &queue_arn, &definition, "holder").await;
                    holders.push(holder.clone());
                }
            }
            tokio::time::sleep(poll_gap()).await;
        }
        match held {
            Some(held) => {
                assert_eq!(
                    held["availableQuantity"].as_i64(),
                    Some(row.total - row.claim),
                    "case {case}: availableQuantity is the total less what is in use, got {held}"
                );
                assert_eq!(
                    held["totalQuantity"].as_i64(),
                    Some(row.total),
                    "case {case}: holding a quantity does not change the total, got {held}"
                );
            }
            // A NON_REPLENISHABLE resource can take no second holder — the first consumes what
            // it claimed, which is the rule this row is about — so the hold of the one holder
            // it has is proved by the total it leaves behind, asserted below.
            None => assert!(
                !replenishable,
                "case {case}: a replenishable resource was never observed holding the \
                 quantity its claimant took"
            ),
        }

        // ... and given back once the holder is terminal, whatever it ended as.
        let mut last = Value::Null;
        for id in &holders {
            last = until_terminal(&batch, case, id).await;
        }
        let after = describe_resource(&batch, &name).await;
        let total_after = if replenishable {
            row.total
        } else {
            row.total - row.claim
        };
        assert_eq!(
            (
                after["totalQuantity"].as_i64(),
                after["inUseQuantity"].as_i64(),
                after["availableQuantity"].as_i64()
            ),
            (Some(total_after), Some(0), Some(total_after)),
            "case {case}: a {} resource offers {total_after} again once the job that held \
             {} is {}, got {after}",
            row.resource_type,
            row.claim,
            last["status"].as_str().unwrap_or_default(),
        );

        // A read changes nothing: the same answer twice.
        let again = describe_resource(&batch, &name).await;
        assert_eq!(
            again["inUseQuantity"], after["inUseQuantity"],
            "case {case}: a second describe must report the same quantity, got {again}"
        );
    }
}

/// RULE: a claimant of a quantity that is all in use is NOT admitted onto capacity — it waits
/// where a job waits to be placed — and is admitted once the holder gives the quantity back.
#[tokio::test]
async fn a_claimant_waits_while_the_quantity_it_needs_is_held() {
    let batch = Batch::new();
    let environment =
        managed_compute_environment(&batch, "ce-gate", state::STATE_ENABLED).await;
    let queue_arn = job_queue(&batch, "q-gate", &environment).await;
    // One unit, claimed whole by each of two jobs: only one of them can hold it.
    create_resource(&batch, "cr-gate", state::RESOURCE_TYPE_REPLENISHABLE, 1).await;
    let definition = claiming_definition(&batch, "jd-gate", "cr-gate", 1).await;

    let mut claimants = vec![
        submit(&batch, &queue_arn, &definition, "claimant-one").await,
        submit(&batch, &queue_arn, &definition, "claimant-two").await,
    ];

    // Exactly ONE claimant is LIVE on the unit at a time: the one holding it is in a status
    // past the queue, and every other is either still waiting for it or already done with it.
    // A terminal job claims nothing any more, so the count is of the LIVE admitted statuses —
    // otherwise it would depend on which of the consecutive hold windows the loop looked in.
    // Where every claimant has finished without the loop having looked inside a window, two
    // more are submitted, so what it waits for is the CONDITION and not a window of time.
    let mut witnessed = false;
    for _ in 0..POLLS {
        let described = describe_resource(&batch, "cr-gate").await;
        let mut statuses: Vec<Value> = Vec::with_capacity(claimants.len());
        for id in &claimants {
            statuses.push(describe_job(&batch, id).await);
        }
        if described["inUseQuantity"].as_i64() == Some(1) {
            assert_eq!(
                described["availableQuantity"].as_i64(),
                Some(0),
                "case the gate: the single unit in use leaves none available, got {described}"
            );
            let live_on_the_unit = statuses
                .iter()
                .filter(|detail| {
                    LIVE_PAST_THE_QUEUE.contains(&detail["status"].as_str().unwrap_or_default())
                })
                .count();
            assert_eq!(
                live_on_the_unit, 1,
                "case the gate: exactly one claimant of the single unit is live on it, got \
                 {statuses:?}"
            );
            witnessed = true;
            break;
        }
        let all_done = statuses.iter().all(|detail| {
            TERMINAL.contains(&detail["status"].as_str().unwrap_or_default())
        });
        if all_done {
            let next = claimants.len();
            claimants.push(submit(&batch, &queue_arn, &definition, &format!("claimant-{next}")).await);
            claimants
                .push(submit(&batch, &queue_arn, &definition, &format!("claimant-{}", next + 1)).await);
        }
        tokio::time::sleep(poll_gap()).await;
    }
    assert!(
        witnessed,
        "case the gate: the single unit was never observed in use"
    );

    // Every claimant gets its turn: the next is admitted once the one before it releases the
    // unit, and the resource is whole again at the end.
    for (position, id) in claimants.iter().enumerate() {
        let detail = until_terminal(&batch, &format!("claimant {position}"), id).await;
        assert!(
            detail["startedAt"].as_i64().unwrap_or_default() > 0,
            "case claimant {position}: a job that was admitted and ran reports when it \
             started, got {detail}"
        );
    }
    let after = describe_resource(&batch, "cr-gate").await;
    assert_eq!(
        (
            after["inUseQuantity"].as_i64(),
            after["availableQuantity"].as_i64()
        ),
        (Some(0), Some(1)),
        "case the gate: a replenishable unit is offered again once both jobs are done, got \
         {after}"
    );
}

/// RULE: a claim that does not fit the resource AT ALL leaves the job waiting to be placed —
/// the quantity it asks for is never available, so nothing admits it — until a call publishes
/// that quantity; a job claiming nothing of the resource is admitted as usual.
#[tokio::test]
async fn a_claim_larger_than_the_resource_is_never_admitted() {
    let batch = Batch::new();
    let environment =
        managed_compute_environment(&batch, "ce-oversize", state::STATE_ENABLED).await;
    let queue_arn = job_queue(&batch, "q-oversize", &environment).await;
    create_resource(&batch, "cr-oversize", state::RESOURCE_TYPE_REPLENISHABLE, 2).await;
    // The rows differ along the claim: one past what the resource holds, far past it, and
    // the documented floor of 0, which asks for nothing and so always fits.
    let rows: &[(&str, i64, bool)] = &[
        ("one past the total", 3, false),
        ("far past the total", 1_000_000, false),
        ("the documented floor of zero", 0, true),
    ];
    for (index, (case, claim, admitted)) in rows.iter().enumerate() {
        let definition = claiming_definition(
            &batch,
            &format!("jd-oversize{index}"),
            "cr-oversize",
            *claim,
        )
        .await;
        let job_id = submit(&batch, &queue_arn, &definition, &format!("over{index}")).await;
        if *admitted {
            let detail = until_terminal(&batch, case, &job_id).await;
            assert!(
                detail["startedAt"].as_i64().unwrap_or_default() > 0,
                "case {case}: a job claiming nothing is admitted and runs, got {detail}"
            );
            continue;
        }
        // It comes to rest where a job waits to be placed, and STAYS there over further
        // spans of the clock.
        let detail = until_status(&batch, case, &job_id, state::JOB_RUNNABLE).await;
        assert_eq!(
            detail["startedAt"],
            Value::Null,
            "case {case}: a job nothing admitted never started, got {detail}"
        );
        tokio::time::sleep(several_spans()).await;
        let still = describe_job(&batch, &job_id).await;
        assert_eq!(
            still["status"].as_str(),
            Some(state::JOB_RUNNABLE),
            "case {case}: a claim the resource cannot fit keeps the job waiting, got {still}"
        );
        let resource = describe_resource(&batch, "cr-oversize").await;
        assert_eq!(
            resource["inUseQuantity"].as_i64(),
            Some(0),
            "case {case}: a job that was never admitted holds nothing, got {resource}"
        );

        // ... until a call PUBLISHES the quantity it asks for: raising the total is what
        // admits it, from that instant, and it then holds exactly what it claimed.
        batch
            .ok(
                "/v1/updateconsumableresource",
                json!({
                    "consumableResource": "cr-oversize",
                    "operation": state::UPDATE_OPERATION_SET,
                    "quantity": claim,
                }),
            )
            .await;
        let running = until_terminal(&batch, case, &job_id).await;
        assert!(
            running["startedAt"].as_i64().unwrap_or_default() > 0,
            "case {case}: the claimant is admitted once its quantity is published, got \
             {running}"
        );
        // And the resource is whole again afterwards, since it is replenishable.
        let resource = describe_resource(&batch, "cr-oversize").await;
        assert_eq!(
            (
                resource["totalQuantity"].as_i64(),
                resource["inUseQuantity"].as_i64()
            ),
            (Some(*claim), Some(0)),
            "case {case}: the published quantity is given back at the end, got {resource}"
        );
        // Put the resource back to what the rows above expect of it.
        batch
            .ok(
                "/v1/updateconsumableresource",
                json!({
                    "consumableResource": "cr-oversize",
                    "operation": state::UPDATE_OPERATION_SET,
                    "quantity": 2,
                }),
            )
            .await;
    }
}

/// RULE: `DeleteConsumableResource` is REFUSED while any of the resource's quantity is in
/// use — the modelled `ClientException` with the fixed detail
/// (`state::CONSUMABLE_RESOURCE_IN_USE_MESSAGE`) — and the resource is left exactly as it
/// was; once nothing holds it any more the delete succeeds.
#[tokio::test]
async fn deleting_a_resource_in_use_is_refused_and_removes_nothing() {
    // The rows differ along the resource type, the quantity in use (one unit of one, and
    // part of a larger total) and the identifier form the delete names it in.
    let rows: &[(&str, &str, i64, i64, bool)] = &[
        (
            "the single unit of a replenishable resource, by name",
            state::RESOURCE_TYPE_REPLENISHABLE,
            1,
            1,
            false,
        ),
        (
            "part of a non-replenishable total, by ARN",
            state::RESOURCE_TYPE_NON_REPLENISHABLE,
            10,
            3,
            true,
        ),
    ];
    for (index, (case, resource_type, total, in_use, by_arn)) in rows.iter().enumerate() {
        let batch = Batch::new();
        let name = format!("cr-delete{index}");
        create_resource(&batch, &name, resource_type, *total).await;
        // The quantity is put in use the way an admitted job puts it there; what this case is
        // about is the delete, so the hold is stated rather than raced for.
        {
            let store = batch.store();
            let mut store = store.lock();
            let resource = store
                .consumable_resources
                .get_mut(&name)
                .unwrap_or_else(|| panic!("case {case}: the created resource is stored"));
            resource.in_use_quantity = *in_use;
        }
        let reference = if *by_arn {
            resource_arn(&name)
        } else {
            name.clone()
        };

        let (status, body) = batch
            .call(
                "/v1/deleteconsumableresource",
                json!({"consumableResource": &reference}),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case}: expected the modelled ClientException, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "ClientException",
            "case {case}: expected ClientException, got {body}"
        );
        assert_eq!(
            error_message(&body),
            state::CONSUMABLE_RESOURCE_IN_USE_MESSAGE,
            "case {case}: expected the in-use detail, got {body}"
        );

        // The resource is still there, still holding exactly what it held.
        let described = describe_resource(&batch, &reference).await;
        assert_eq!(
            (
                described["totalQuantity"].as_i64(),
                described["inUseQuantity"].as_i64()
            ),
            (Some(*total), Some(*in_use)),
            "case {case}: the refused delete left the resource alone, got {described}"
        );

        // Once nothing holds it, the same delete succeeds and the resource is gone.
        {
            let store = batch.store();
            let mut store = store.lock();
            if let Some(resource) = store.consumable_resources.get_mut(&name) {
                resource.in_use_quantity = 0;
            }
        }
        let (status, body) = batch
            .call(
                "/v1/deleteconsumableresource",
                json!({"consumableResource": &reference}),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case}: a resource nothing holds is deleted, got {status} {body}"
        );
        let (status, body) = batch
            .call(
                "/v1/describeconsumableresource",
                json!({"consumableResource": &reference}),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case}: the deleted resource resolves to nothing, got {status} {body}"
        );
    }
}

/// Poll `job_id` until it reports a terminal `JobStatus`, and answer its last `JobDetail`.
async fn until_terminal(batch: &Batch, case: &str, job_id: &str) -> Value {
    for _ in 0..POLLS {
        let detail = describe_job(batch, job_id).await;
        let status = detail["status"].as_str().unwrap_or_default();
        if status == state::JOB_SUCCEEDED || status == state::JOB_FAILED {
            return detail;
        }
        tokio::time::sleep(poll_gap()).await;
    }
    let detail = describe_job(batch, job_id).await;
    panic!("case {case}: expected the job to become terminal, got {detail}");
}
