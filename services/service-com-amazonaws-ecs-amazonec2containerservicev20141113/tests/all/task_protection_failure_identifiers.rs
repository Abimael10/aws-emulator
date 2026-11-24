// SPDX-License-Identifier: CC0-1.0

//! How the TASK-PROTECTION operations NAME a `tasks` entry they could not resolve, driven
//! end to end over the shared wire harness in [`crate::ecs_wire`].
//!
//! `Failure$arn` is "The Amazon Resource Name (ARN) of the failed resource", and the
//! `tasks` member of both operations is "A list of up to 10 task IDs or full ARN entries",
//! so one entry can be spelled three ways: the task id, the cluster-less task ARN of that
//! id, and the cluster-qualified ARN a stored task of that id carries. What each operation
//! reports for an entry that resolves to NO task of the cluster is recorded behavior, and
//! the two surfaces differ:
//!
//! * `UpdateTaskProtection` names the task by its OWN ID alone —
//!   `arn:aws:ecs:<region>:<account>:task/<id>` — whichever of the three forms the entry
//!   took: the cluster segment of a task ARN names the cluster a task is registered in, and
//!   a task that resolves to nothing is registered nowhere;
//! * `GetTaskProtection` ECHOES the entry as the caller sent it: the value itself when it
//!   is already an ARN, and that id alone under the task resource type when it is an id.
//!
//! Either way the entry is attributable to the task the caller asked about, and neither
//! operation INSERTS a cluster into the ARN of a task the store does not hold.
//!
//! The rows differ along what the rule generalises over: the FORM of the entry (bare id,
//! cluster-less ARN, cluster-qualified ARN), the id's alphabet and length, how many
//! unresolvable entries are asked about at once, and whether a task the store DOES hold is
//! asked about beside them — the contrast row, since a stored task's failure keeps the
//! cluster-qualified ARN that task really carries. The write is driven with protection both
//! ON and OFF, because `protectionEnabled` decides what a resolvable entry gets and not how
//! an unresolvable one is named.
//!
//! Nothing here reaches into the state struct: every value asserted is read off the wire.

use serde_json::json;

use crate::ecs_wire::{
    cluster_with_capacity, list, ok, register_definition, service, string, ACCOUNT, SIGNED_REGION,
};

/// The cluster every request below names.
const CLUSTER: &str = "protected";

/// The task-definition family the stored task of the contrast row runs.
const FAMILY: &str = "protectedapp";

/// The `Failure$reason` for an identifier that names nothing, from the API failure reasons:
/// "MISSING — the described resource could not be found".
const MISSING: &str = "MISSING";

/// The `Failure$reason` for a task these operations cannot act on: "This operation is only
/// supported for tasks belonging to an Amazon ECS service. Invoking this operation for a
/// standalone task will result in an TASK_NOT_VALID failure."
const TASK_NOT_VALID: &str = "TASK_NOT_VALID";

/// The ARN of a task in the signed region under the resource path `path`.
fn task_arn(path: &str) -> String {
    format!("arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:task/{path}")
}

/// The `(arn, reason)` pair of every failure a protection response reports, in order.
fn failures(body: &serde_json::Value, case: &str) -> Vec<(String, String)> {
    list(body, "failures", case)
        .iter()
        .map(|failure| {
            (
                string(failure, "arn", case),
                string(failure, "reason", case),
            )
        })
        .collect()
}

/// One row of the table: the entries a request names, and the failures each of the two
/// operations answers with, in the request's order.
struct Row {
    case: &'static str,
    named: Vec<String>,
    /// What `UpdateTaskProtection` reports: `(arn, reason)` per failure.
    written: Vec<(String, String)>,
    /// What `GetTaskProtection` reports for the same entries.
    read: Vec<(String, String)>,
}

/// The `MISSING` failure an entry that resolves to nothing earns under the task ARN `arn`.
fn missing(arn: String) -> (String, String) {
    (arn, MISSING.to_string())
}

#[tokio::test]
async fn an_unresolvable_task_protection_entry_is_named_by_the_task_it_asked_about() {
    let router = service();
    cluster_with_capacity(&router, CLUSTER, "fixture").await;
    register_definition(&router, FAMILY, "fixture").await;
    let run = ok(
        &router,
        "RunTask",
        json!({ "cluster": CLUSTER, "taskDefinition": FAMILY }),
        "fixture",
    )
    .await;
    // A task the store HOLDS, carrying the cluster-qualified ARN a stored task has. It was
    // started by `RunTask` rather than by a service, so these operations cannot act on it —
    // which is the reason its own failure entry carries.
    let stored = string(&list(&run, "tasks", "fixture")[0], "taskArn", "fixture");
    let stored_id = stored
        .rsplit('/')
        .next()
        .expect("a task ARN ends in its id")
        .to_string();

    // Rows: the case, the entries the request names, and the failures expected back IN THE
    // REQUEST'S ORDER — once as `UpdateTaskProtection` names them (the task's own id) and
    // once as `GetTaskProtection` does (the entry as sent).
    let hex = "9".repeat(32);
    let uuid = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_string();
    let rows = [
        Row {
            case: "a bare 32-hex id that names nothing",
            named: vec![hex.clone()],
            written: vec![missing(task_arn(&hex))],
            read: vec![missing(task_arn(&hex))],
        },
        Row {
            case: "a bare uuid-shaped id",
            named: vec![uuid.clone()],
            written: vec![missing(task_arn(&uuid))],
            read: vec![missing(task_arn(&uuid))],
        },
        Row {
            case: "a short id",
            named: vec!["ghost".to_string()],
            written: vec![missing(task_arn("ghost"))],
            read: vec![missing(task_arn("ghost"))],
        },
        Row {
            case: "a cluster-less task ARN",
            named: vec![task_arn(&"8".repeat(32))],
            written: vec![missing(task_arn(&"8".repeat(32)))],
            read: vec![missing(task_arn(&"8".repeat(32)))],
        },
        Row {
            case: "a task ARN qualified by this cluster",
            named: vec![task_arn(&format!("{CLUSTER}/{}", "7".repeat(32)))],
            written: vec![missing(task_arn(&"7".repeat(32)))],
            read: vec![missing(task_arn(&format!(
                "{CLUSTER}/{}",
                "7".repeat(32)
            )))],
        },
        Row {
            case: "three unresolvable entries at once, spelled three ways",
            named: vec![
                "a".repeat(32),
                task_arn(&"b".repeat(32)),
                task_arn(&format!("{CLUSTER}/{}", "c".repeat(32))),
            ],
            written: vec![
                missing(task_arn(&"a".repeat(32))),
                missing(task_arn(&"b".repeat(32))),
                missing(task_arn(&"c".repeat(32))),
            ],
            read: vec![
                missing(task_arn(&"a".repeat(32))),
                missing(task_arn(&"b".repeat(32))),
                missing(task_arn(&format!("{CLUSTER}/{}", "c".repeat(32)))),
            ],
        },
        Row {
            case: "the stored task beside an unresolvable entry",
            named: vec![stored_id.clone(), "d".repeat(32)],
            written: vec![
                (stored.clone(), TASK_NOT_VALID.to_string()),
                missing(task_arn(&"d".repeat(32))),
            ],
            read: vec![
                (stored.clone(), TASK_NOT_VALID.to_string()),
                missing(task_arn(&"d".repeat(32))),
            ],
        },
    ];

    for row in rows {
        let Row {
            case,
            named,
            written,
            read,
        } = row;
        for protection_enabled in [true, false] {
            let case = format!("{case} with protectionEnabled {protection_enabled}");
            let case = case.as_str();
            let body = ok(
                &router,
                "UpdateTaskProtection",
                json!({
                    "cluster": CLUSTER,
                    "tasks": named.clone(),
                    "protectionEnabled": protection_enabled,
                }),
                case,
            )
            .await;
            let reported = failures(&body, case);
            assert_eq!(
                reported, written,
                "case {case:?}: expected the failures {written:?}, got {reported:?} in {body}"
            );
            assert!(
                list(&body, "protectedTasks", case).is_empty(),
                "case {case:?}: no entry of this row is a task protection can be written \
                 for, got {body}"
            );
        }

        let body = ok(
            &router,
            "GetTaskProtection",
            json!({ "cluster": CLUSTER, "tasks": named.clone() }),
            case,
        )
        .await;
        let reported = failures(&body, case);
        assert_eq!(
            reported, read,
            "case {case:?}: expected the read to echo the failures {read:?}, got {reported:?} \
             in {body}"
        );
    }

    // The entries above left no protection behind, so the stored task is still one no
    // protection was written for: the failure channel is what these rows are about, and it
    // moves nothing a later caller reads.
    let read_back = ok(
        &router,
        "GetTaskProtection",
        json!({ "cluster": CLUSTER, "tasks": [stored_id] }),
        "read back",
    )
    .await;
    assert_eq!(
        failures(&read_back, "read back"),
        vec![(stored, TASK_NOT_VALID.to_string())],
        "case \"read back\": expected the stored task still reported as one this operation \
         cannot act on, got {read_back}"
    );
}
