// SPDX-License-Identifier: CC0-1.0

//! The referential rule this service's destructive scope operation follows: a scope
//! a monitor still references is not deletable.
//!
//! A monitor names the scope it watches by a `@required` ARN
//! (`CreateMonitor.scopeArn`), so every stored monitor is a live reference to one
//! scope. `DeleteScope` is refused while any such reference exists — the modelled
//! `ValidationException` (HTTP 400), answered BEFORE anything is written, so the
//! addressed scope and every monitor under it are exactly as they were — and the
//! same delete succeeds once the last referencing monitor is gone.
//!
//! The table spans what the rule generalizes over: how MANY monitors reference the
//! scope, what those monitors hold (local resource type, remote resources, tags),
//! which lifecycle status the clock has the scope and the monitors in, and how many
//! monitors are still left when the delete is retried. The recorded case — one
//! `ACTIVE` monitor over a `SUCCEEDED` scope — is one row of it.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::harness::{
    client_token, extra_region, targets, Emulator, ACCOUNT_ID, REGION, SIGNING_NAME,
};

/// How long a poll for a status the clock owes keeps looking before it gives up.
/// Deliberately far longer than the provisioning span the store settles a fresh
/// resource after, so a loaded host does not decide the outcome.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long one round of a poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// The whole message a delete of a referenced scope is refused with: wholly fixed,
/// with no part of it taken from the request.
const SCOPE_IN_USE: &str = "Scope can't be deleted as there's monitors associated with it";

/// One row of the referencing-monitor table.
struct Row {
    /// What this row varies, named in every assertion.
    case: &'static str,
    /// The monitors created under the row's scope: one `CreateMonitor` body each,
    /// minus the `scopeArn`, which is the scope this row mints.
    monitors: Vec<Value>,
    /// Whether the row waits for the terminal statuses the clock owes before the
    /// delete, so its scope is `SUCCEEDED` and its monitors `ACTIVE` rather than
    /// still `IN_PROGRESS` and `PENDING`.
    settled: bool,
}

/// A `CreateMonitor` body (without `scopeArn`) for `name`, over `local` as its one
/// local resource.
fn monitor(name: &str, local_type: &str, local_identifier: &str) -> Value {
    json!({
        "monitorName": name,
        "localResources": [{ "type": local_type, "identifier": local_identifier }]
    })
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one monitor, scope still IN_PROGRESS and monitor still PENDING",
            monitors: vec![monitor("ref-in-progress", "AWS::EC2::VPC", "vpc-0a1b2c3d")],
            settled: false,
        },
        Row {
            // The recorded shape: a Region-wide monitor over a settled scope.
            case: "one ACTIVE monitor over a SUCCEEDED scope",
            monitors: vec![monitor("ref-active", "AWS::Region", REGION)],
            settled: true,
        },
        Row {
            case: "three monitors, differing in local resource type, remote resources and tags",
            monitors: vec![
                monitor("ref-many-1", "AWS::EC2::Subnet", "subnet-0123456789abcdef0"),
                json!({
                    "monitorName": "ref-many-2",
                    "localResources": [{ "type": "AWS::EKS::Cluster", "identifier": "cluster-a" }],
                    "remoteResources": [{ "type": "AWS::AWSService", "identifier": "S3" }],
                    "tags": { "team": "net" }
                }),
                json!({
                    "monitorName": "ref_many.3-dotted",
                    "localResources": [
                        { "type": "AWS::AvailabilityZone", "identifier": "use1-az1" },
                        { "type": "AWS::EC2::VPC", "identifier": "vpc-ffffffff" }
                    ],
                    "clientToken": client_token(31)
                }),
            ],
            settled: true,
        },
        Row {
            case: "five monitors, so the delete stays refused while any one of them is left",
            monitors: (0..5)
                .map(|index| {
                    monitor(
                        &format!("ref-five-{index}"),
                        "AWS::EC2::VPC",
                        &format!("vpc-{index:08x}"),
                    )
                })
                .collect(),
            settled: false,
        },
    ]
}

/// Look again until the scope reports its terminal status, so a row that is about a
/// SUCCEEDED scope waits for the CONDITION rather than for a span of the clock.
async fn await_scope_succeeded(emulator: &Emulator, scope_id: &str, case: &str) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let read = emulator.get_scope(scope_id).await;
        if read.text("status") == "SUCCEEDED" {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the scope never reached SUCCEEDED; last status {:?} with body {}",
            read.text("status"),
            read.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The monitor counterpart: look again until the monitor reports `ACTIVE`.
async fn await_monitor_active(emulator: &Emulator, name: &str, case: &str) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let read = emulator.get_monitor(name).await;
        if read.text("monitorStatus") == "ACTIVE" {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: monitor {name:?} never reached ACTIVE; last status {:?} with body {}",
            read.text("monitorStatus"),
            read.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Assert `answer` is the modelled in-use refusal, whole: status, code and message.
fn assert_in_use(case: &str, answer: &crate::harness::Answer) {
    assert_eq!(
        answer.status, 400,
        "case {case:?}: expected HTTP 400 for a scope still referenced, got {} with body {}",
        answer.status, answer.body
    );
    assert!(
        answer.is_error("ValidationException"),
        "case {case:?}: expected ValidationException, got {:?} / body {}",
        answer.error_type,
        answer.body
    );
    assert_eq!(
        answer.sentence(),
        SCOPE_IN_USE,
        "case {case:?}: expected the fixed in-use wording, got {:?}",
        answer.sentence()
    );
}

/// The rule, end to end: while monitors reference the scope every `DeleteScope` is
/// refused and changes nothing, and the delete succeeds exactly once the last
/// reference is gone.
#[tokio::test]
async fn delete_scope_is_refused_while_a_monitor_references_the_scope() {
    for row in rows() {
        // A store per row: the account holds one scope per Region, and each row is
        // about one scope's references.
        let emulator = Emulator::new();
        let created = emulator
            .create_scope(json!({ "targets": targets(&[("123456789012", "us-east-1")]) }))
            .await;
        assert_eq!(
            created.status, 200,
            "case {:?}: the scope must be created, got {} with body {}",
            row.case, created.status, created.body
        );
        let scope_id = created.text("scopeId");
        let scope_arn = created.text("scopeArn");

        let mut names: Vec<String> = Vec::new();
        for body in &row.monitors {
            let mut body = body.clone();
            body["scopeArn"] = Value::String(scope_arn.clone());
            let answer = emulator.create_monitor(body.clone()).await;
            assert_eq!(
                answer.status, 200,
                "case {:?}: the monitor {} must be created, got {} with body {}",
                row.case,
                body["monitorName"],
                answer.status,
                answer.body
            );
            names.push(answer.text("monitorName"));
        }

        // A settled row waits for the terminal statuses the clock owes — the
        // recorded case's ACTIVE monitor over a SUCCEEDED scope — while an unsettled
        // row deletes straight away, so the table covers both lifecycle phases.
        if row.settled {
            await_scope_succeeded(&emulator, &scope_id, row.case).await;
            for name in &names {
                await_monitor_active(&emulator, name, row.case).await;
            }
        }
        // What the rule forbids is a status the DELETE moved the scope to
        // (`DEACTIVATING`/`DEACTIVATED`) or the scope being gone; an unsettled row
        // may have been settled by the clock in the meantime, which is the store's
        // own doing and not the delete's.
        let live_statuses: &[&str] = if row.settled {
            &["SUCCEEDED"]
        } else {
            &["IN_PROGRESS", "SUCCEEDED"]
        };

        // The refusal, once per still-referencing monitor: each round deletes one
        // monitor, and while any is left the delete is refused and the scope is
        // untouched.
        while !names.is_empty() {
            let case = format!("{} ({} reference(s) left)", row.case, names.len());
            let refused = emulator.delete_scope(&scope_id).await;
            assert_in_use(&case, &refused);

            // Nothing was written: the scope is still there, in the status the clock
            // has it in, and still resolves its own ARN.
            let read = emulator.get_scope(&scope_id).await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: the refused delete must leave the scope readable, got {} with \
                 body {}",
                read.status, read.body
            );
            assert!(
                live_statuses.contains(&read.text("status").as_str()),
                "case {case:?}: the refused delete must leave the scope in one of \
                 {live_statuses:?}, got {:?}",
                read.text("status")
            );
            assert_eq!(
                read.text("scopeArn"),
                scope_arn,
                "case {case:?}: the scope must still carry its minted ARN, got {:?}",
                read.text("scopeArn")
            );

            // And every monitor still references it, so none of them was cascaded.
            for name in &names {
                let monitor = emulator.get_monitor(name).await;
                assert_eq!(
                    monitor.status, 200,
                    "case {case:?}: monitor {name:?} must survive the refused delete, got {} \
                     with body {}",
                    monitor.status, monitor.body
                );
                assert_eq!(
                    monitor.text("monitorArn"),
                    format!("arn:aws:{SIGNING_NAME}:{REGION}:{ACCOUNT_ID}:monitor/{name}"),
                    "case {case:?}: monitor {name:?} must read back as itself, got {:?}",
                    monitor.text("monitorArn")
                );
            }

            let gone = names.remove(0);
            assert_eq!(
                emulator.delete_monitor(&gone).await.status,
                200,
                "case {case:?}: monitor {gone:?} must delete"
            );
        }

        // The last reference is gone, so the same delete succeeds and removes the
        // scope: the next reader is answered the modelled 404.
        let deleted = emulator.delete_scope(&scope_id).await;
        assert_eq!(
            deleted.status, 200,
            "case {:?}: with no monitor left the delete must succeed, got {} with body {}",
            row.case, deleted.status, deleted.body
        );
        let read = emulator.get_scope(&scope_id).await;
        assert_eq!(
            read.status, 404,
            "case {:?}: the deleted scope must be gone, got {} with body {}",
            row.case, read.status, read.body
        );
        assert!(
            read.is_error("ResourceNotFoundException"),
            "case {:?}: expected ResourceNotFoundException, got {:?} / body {}",
            row.case,
            read.error_type,
            read.body
        );
    }
}

/// The guard is about the monitors of THIS scope: a monitor under another scope is
/// not a reference to the one being deleted, and the scope with no monitors of its
/// own deletes while the other one's monitors stay live.
#[tokio::test]
async fn only_the_addressed_scopes_own_monitors_block_its_delete() {
    let emulator = Emulator::new();

    // Two scopes in one store, each in its own Region (the account's scope quota is
    // per Region).
    let watched = emulator
        .create_scope(json!({ "targets": targets(&[("123456789012", "us-east-1")]) }))
        .await;
    let free = emulator
        .create_scope_in(
            extra_region(0),
            json!({ "targets": targets(&[("210987654321", "eu-west-1")]) }),
        )
        .await;
    for (case, answer) in [("the watched scope", &watched), ("the free scope", &free)] {
        assert_eq!(
            answer.status, 200,
            "case {case:?}: the scope must be created, got {} with body {}",
            answer.status, answer.body
        );
    }

    let watched_arn = watched.text("scopeArn");
    let mut body = monitor("cross-scope-probe", "AWS::EC2::VPC", "vpc-0abc");
    body["scopeArn"] = Value::String(watched_arn.clone());
    let created = emulator.create_monitor(body).await;
    assert_eq!(
        created.status, 200,
        "the monitor under the watched scope must be created, got {} with body {}",
        created.status, created.body
    );

    // The scope nothing references deletes, even though the store holds a monitor.
    let free_id = free.text("scopeId");
    let deleted = emulator.delete_scope(&free_id).await;
    assert_eq!(
        deleted.status, 200,
        "a scope no monitor references must delete, got {} with body {}",
        deleted.status, deleted.body
    );

    // The referenced one does not.
    let watched_id = watched.text("scopeId");
    assert_in_use(
        "the scope the monitor references",
        &emulator.delete_scope(&watched_id).await,
    );
    let read = emulator.get_scope(&watched_id).await;
    assert_eq!(
        read.status, 200,
        "the referenced scope must survive, got {} with body {}",
        read.status, read.body
    );

    // The monitor is still under the ARN it was created with, which still resolves:
    // a fresh monitor naming that same scope is accepted.
    let mut again = monitor("cross-scope-second", "AWS::Region", REGION);
    again["scopeArn"] = Value::String(watched_arn.clone());
    let second = emulator.create_monitor(again).await;
    assert_eq!(
        second.status, 200,
        "the refused delete must leave the scope ARN resolvable, got {} with body {}",
        second.status, second.body
    );
}
