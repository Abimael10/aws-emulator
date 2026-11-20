// SPDX-License-Identifier: CC0-1.0

//! IAM authorization of AWS Batch's BULK job operations, over its restJson1 wire.
//!
//! The rule under test: an operation that is the bulk version of a per-item one is
//! authorized item by item, against the SINGULAR per-item action and that item's own
//! resource ARN — there is no IAM action named after the bulk API at all, which the model
//! states for each of them ("This operation requires `batch:TerminateServiceJob` permission
//! for each service job in the request. There is no separate `batch:TerminateServiceJobs`
//! IAM action", and the same for `CancelJobs` and `TerminateJobs`). So a principal granted
//! exactly the per-item action may make the bulk call; a principal granted only the bulk
//! API's name may not; a grant scoped to one listed item's ARN does not cover the others;
//! and the refusal is IAM's standard identity-based denial naming the per-item action AND
//! the ARN of the item it was refused for. The decision is taken before the operation runs,
//! so a refused bulk call leaves every job it named exactly as it was.
//!
//! This is a CROSS-SERVICE rule and only a cross-service suite can pin it: the principals
//! are made with IAM's own operations over the same engine, so the documents Batch decides
//! on are the ones IAM wrote and the principal a refusal blames is the one STS reports for
//! the credential that signed. The harness — the router, the signed call and the IAM user
//! builder — is the one `batch_identity_authorization` already owns.
//!
//! The rows vary what the rule generalises over: the bulk OPERATION (the service-job
//! termination, and the container-job termination and cancellation, each with its own
//! per-item action and its own ARN family), how many items the request lists, whether an
//! item is given as a job id or already as an ARN, and what the caller's policy grants (the
//! per-item action, only the bulk API's name, the per-item action scoped to one of the
//! listed items, every action of this service, nothing about this service).

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_batch_awsbatchv20160810::state::{job_arn, service_job_arn};

use crate::batch_identity_authorization::{
    allow, batch, build_router, served, user, Principal, ACCESS_DENIED, ACCESS_DENIED_STATUS,
    ACCOUNT_KEY, ACTION_PREFIX, REGION, SAGEMAKER_TRAINING,
};

/// The `ServiceJobStatus` a terminated service job ends in, which no row below may reach:
/// a refused call terminates nothing.
const TERMINATED_STATUS: &str = "FAILED";

/// The resource one item of a bulk request addresses: the ARN it already spells, or the one
/// this service's own builder mints for the job id it gives.
fn item_resource(item: &str, arn_of: fn(&str, &str) -> String) -> String {
    if item.starts_with("arn:") {
        return item.to_string();
    }
    arn_of(REGION, item)
}

/// One bulk operation of this model: its path, the per-item action IAM evaluates for it, and
/// the ARN family the items it lists belong to.
struct BulkOperation {
    case: &'static str,
    path: &'static str,
    /// The operation's own name, after which IAM publishes no action.
    bulk_operation: &'static str,
    /// The per-item operation, whose action IAM evaluates once per listed item.
    item_operation: &'static str,
    arn_of: fn(&str, &str) -> String,
}

impl BulkOperation {
    /// The IAM action a request to this operation really asks for, once per item.
    fn item_action(&self) -> String {
        format!("{ACTION_PREFIX}:{}", self.item_operation)
    }

    /// The action named after the bulk API, which IAM does not publish.
    fn bulk_action(&self) -> String {
        format!("{ACTION_PREFIX}:{}", self.bulk_operation)
    }

    /// The body of a bulk call listing `items`.
    fn body(&self, items: &[String]) -> Value {
        json!({"jobs": items, "reason": "probe"})
    }
}

/// What a case expects of the answer.
#[derive(Debug, Clone, Copy)]
enum Expect {
    /// The caller's policies cover every listed item, so the request reaches the operation
    /// and whatever that operation answers is not the authorization refusal.
    Served,
    /// The request is refused for the item at this index of the list.
    RefusedFor(usize),
}

/// One case: the policy a fresh principal carries, which of the fixture's items the call
/// lists, and what the answer must be.
struct Case {
    label: &'static str,
    /// The document in force on the caller, built from the row's own action names and the
    /// items the call lists.
    policy: fn(&BulkOperation, &[String]) -> String,
    /// The indices into the row's item pool the call lists, in order.
    items: &'static [usize],
    expect: Expect,
}

/// The cases, spanning what the grant question generalises over. Each document is built per
/// row, because the action names and the ARNs under test are that row's own.
fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "a document saying nothing about this service",
            policy: |_, _| allow("sqs:*", "*"),
            items: &[0],
            expect: Expect::RefusedFor(0),
        },
        Case {
            label: "the SINGULAR per-item action on every resource",
            policy: |row, _| allow(&row.item_action(), "*"),
            items: &[0, 1],
            expect: Expect::Served,
        },
        Case {
            label: "only the bulk API's own name allowed, which is no IAM action",
            policy: |row, _| allow(&row.bulk_action(), "*"),
            items: &[0, 1],
            expect: Expect::RefusedFor(0),
        },
        Case {
            label: "the per-item action scoped to the FIRST listed item only",
            policy: |row, items| {
                allow(&row.item_action(), &item_resource(&items[0], row.arn_of))
            },
            items: &[0, 1],
            expect: Expect::RefusedFor(1),
        },
        Case {
            label: "the per-item action scoped to the SECOND listed item only",
            policy: |row, items| {
                allow(&row.item_action(), &item_resource(&items[1], row.arn_of))
            },
            items: &[0, 1],
            expect: Expect::RefusedFor(0),
        },
        Case {
            label: "every action of this service on every resource",
            policy: |_, _| allow(&format!("{ACTION_PREFIX}:*"), "*"),
            items: &[0, 1],
            expect: Expect::Served,
        },
        Case {
            label: "nothing granted, the one item given as an ARN rather than an id",
            policy: |_, _| allow("sqs:*", "*"),
            items: &[2],
            expect: Expect::RefusedFor(0),
        },
    ]
}

/// The `DescribeServiceJob` detail of `job_id`, read back as the account.
async fn service_job(router: &MultiServiceRouter, case: &str, job_id: &str) -> Value {
    served(
        router,
        case,
        "/v1/describeservicejob",
        &json!({"jobId": job_id}),
    )
    .await
}

/// One fresh service job on `queue_arn`, submitted as the account; answers its `jobId`.
async fn submit_service_job(
    router: &MultiServiceRouter,
    case: &str,
    queue_arn: &str,
    job_name: &str,
) -> String {
    let submitted = served(
        router,
        case,
        "/v1/submitservicejob",
        &json!({
            "jobName": job_name,
            "jobQueue": queue_arn,
            "serviceJobType": SAGEMAKER_TRAINING,
            "serviceRequestPayload": "{}",
        }),
    )
    .await;
    submitted["jobId"].as_str().unwrap_or_default().to_string()
}

/// RULE: a bulk operation is authorized once per listed item, against the per-item action
/// and that item's own ARN; the refusal names both, and the refused call changes nothing.
#[tokio::test]
async fn a_bulk_batch_call_is_authorized_per_item_against_the_singular_action() {
    let router = build_router();

    // The queue the suite's service jobs are submitted to. A FRESH service job is submitted
    // per case below, so a refused bulk termination can be shown to leave the job it named
    // un-terminated — and a SERVED one is free to end its own. The other ids a case lists
    // need not exist: the decision is taken from the REQUEST, ahead of any lookup, which is
    // what keeps a refusal from telling an unauthorized caller whether a resource exists.
    served(
        &router,
        "fixture",
        "/v1/createserviceenvironment",
        &json!({
            "serviceEnvironmentName": "se-bulk-authz",
            "serviceEnvironmentType": SAGEMAKER_TRAINING,
            "capacityLimits": [{"maxCapacity": 10, "capacityUnit": "NUM_INSTANCES"}],
        }),
    )
    .await;
    let queue = served(
        &router,
        "fixture",
        "/v1/createjobqueue",
        &json!({
            "jobQueueName": "q-bulk-authz",
            "priority": 1,
            "jobQueueType": SAGEMAKER_TRAINING,
            "serviceEnvironmentOrder": [{"order": 1, "serviceEnvironment": "se-bulk-authz"}],
        }),
    )
    .await;
    let queue_arn = queue["jobQueueArn"].as_str().unwrap_or_default().to_string();
    // A second job id, and the same one spelled as an ARN: the two forms an item can take.
    let second_id = "1b2c3d4e-5f60-4712-8899-aabbccddeeff".to_string();

    let rows: Vec<BulkOperation> = vec![
        BulkOperation {
            case: "a bulk service-job termination",
            path: "/v1/terminateservicejobs",
            bulk_operation: "TerminateServiceJobs",
            item_operation: "TerminateServiceJob",
            arn_of: service_job_arn,
        },
        BulkOperation {
            case: "a bulk container-job termination",
            path: "/v1/terminatejobs",
            bulk_operation: "TerminateJobs",
            item_operation: "TerminateJob",
            arn_of: job_arn,
        },
        BulkOperation {
            case: "a bulk container-job cancellation",
            path: "/v1/canceljobs",
            bulk_operation: "CancelJobs",
            item_operation: "CancelJob",
            arn_of: job_arn,
        },
    ];

    for (row_index, row) in rows.iter().enumerate() {
        for (case_index, case) in cases().iter().enumerate() {
            let label = format!("{} with {}", row.case, case.label);
            // This case's own live service job, so what a refused call did (nothing) is read
            // off a job no other case has touched.
            let live_job = submit_service_job(
                &router,
                &label,
                &queue_arn,
                &format!("sj-bulk-{row_index}-{case_index}"),
            )
            .await;
            // What this case lists: its own live service job, a second id, and that second
            // item spelled as an ARN of this row's own family.
            let pool: Vec<String> = vec![
                live_job.clone(),
                second_id.clone(),
                item_resource(&second_id, row.arn_of),
            ];
            let items: Vec<String> = case.items.iter().map(|index| pool[*index].clone()).collect();
            let principal: Principal = user(
                &router,
                &format!("u-bulk-{row_index}-{case_index}"),
                &(case.policy)(row, &items),
            )
            .await;

            let reply = batch(&router, &principal.access_key_id, row.path, &row.body(&items)).await;

            match case.expect {
                Expect::Served => {
                    assert_ne!(
                        reply.status, ACCESS_DENIED_STATUS,
                        "case {label:?}: a caller holding the per-item action must reach the \
                         operation, got {reply:?}"
                    );
                    assert_ne!(
                        reply.error_code.as_deref(),
                        Some(ACCESS_DENIED),
                        "case {label:?}: a caller holding the per-item action must reach the \
                         operation, got {reply:?}"
                    );
                }
                Expect::RefusedFor(index) => {
                    let resource = item_resource(&items[index], row.arn_of);
                    let action = row.item_action();
                    assert_eq!(
                        reply.status, ACCESS_DENIED_STATUS,
                        "case {label:?}: expected HTTP {ACCESS_DENIED_STATUS}, got {reply:?}"
                    );
                    assert_eq!(
                        reply.error_code.as_deref(),
                        Some(ACCESS_DENIED),
                        "case {label:?}: expected the {ACCESS_DENIED} code, got {reply:?}"
                    );
                    let message = reply.message();
                    let expected = format!(
                        "User: {} is not authorized to perform: {action} on resource: {resource} \
                         because no identity-based policy allows the {action} action",
                        principal.arn
                    );
                    assert_eq!(
                        message, expected,
                        "case {label:?}: expected IAM's sentence {expected:?}, got {message:?}"
                    );
                    // The action token is the SINGULAR one: the bulk API's name appears
                    // nowhere in the denial, because IAM publishes no such action.
                    assert!(
                        !message.contains(&row.bulk_action()),
                        "case {label:?}: the denial names no action after the bulk API, got \
                         {message:?}"
                    );
                    // And the refusal was taken ahead of the operation: the job the call
                    // named is neither terminated nor given a reason. Its STATUS is the
                    // clock's to move, so what is asserted is that it is not the terminated
                    // one.
                    let after = service_job(&router, &label, &live_job).await;
                    assert_ne!(
                        after["isTerminated"],
                        json!(true),
                        "case {label:?}: a refused bulk call terminates nothing, got {after}"
                    );
                    assert_ne!(
                        after["status"],
                        json!(TERMINATED_STATUS),
                        "case {label:?}: a refused bulk call fails no job, got {after}"
                    );
                    assert_eq!(
                        after["statusReason"],
                        Value::Null,
                        "case {label:?}: a refused bulk call records no reason, got {after}"
                    );
                }
            }
        }
    }

    // The account's own credential, which holds no IAM policies, still reaches every bulk
    // operation: the gate refuses nothing it was not asked to.
    for (row_index, row) in rows.iter().enumerate() {
        let live_job = submit_service_job(
            &router,
            row.case,
            &queue_arn,
            &format!("sj-bulk-account-{row_index}"),
        )
        .await;
        let reply = batch(
            &router,
            ACCOUNT_KEY,
            row.path,
            &row.body(std::slice::from_ref(&live_job)),
        )
        .await;
        assert_ne!(
            reply.status, ACCESS_DENIED_STATUS,
            "case {:?}: the account's own credential is not refused, got {reply:?}",
            row.case
        );
    }
}
