// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of `DeleteCommand`: WHEN a command leaves the account, what
//! the delete reports in the meantime, and what every other command operation then sees.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is what an AWS SDK
//! client would see, not an internal function's return value.
//!
//! The rule under test: a delete's outcome is decided by how long the command has been
//! DEPRECATED. A command deprecated for at least the service's deletion timeout
//! (`COMMAND_DELETION_TIMEOUT`, "the maximum time out duration of 12 hours" of
//! `smithy_query(query="doc",shape="DeleteCommandResponse",member="statusCode")`) is removed
//! by the call itself and reported `204`; any other command — one never deprecated, or
//! deprecated for less than that timeout — is reported `202`, is deprecated by the delete
//! and enters `pendingDeletion` with its removal recorded for one timeout after its
//! deprecation. `statusCode` is `@httpResponseCode`, so the member a client reads and the
//! response's HTTP status are one value. Once the recorded instant is reached the command is
//! permanently removed: no reader reports it, the shared index the IoT Jobs data plane
//! resolves a `commandArn` against no longer holds it, and its id is free for a new create.
//! While the removal is only scheduled the command is still readable but no longer writable
//! — `UpdateCommand` is refused `ConflictException` (409) — and repeating the delete cannot
//! postpone the removal.
//!
//! The tables span what that rule generalises over: how long the command has been deprecated
//! (never, just now, one second short of the timeout, exactly the timeout, well past it),
//! whether the account holds the command at all, how the deprecation was made (through
//! `UpdateCommand` or carried by the record), which members a refused update names, and how
//! many commands the account holds when one of them is removed.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::{
    command_registry, service_state, COMMAND_DELETION_TIMEOUT,
};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";
/// The region the harness signs for.
const REGION: &str = "us-east-1";

/// A SigV4 `Authorization` header whose credential scope names the `iot` signing name and
/// the region: the scope is how the router picks the claiming service, and how the service
/// picks the region its ARNs are scoped to. The signature itself is never checked here —
/// the harness turns request authentication off, exactly as an unauthenticated emulator
/// runs.
fn signed_for_iot() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTIOT/20260101/{REGION}/iot/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: Option<String>,
    body: Value,
}

impl Reply {
    /// The `message` of an error body.
    fn message(&self) -> String {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// A boolean member of the body, or `None` when it is absent or not a boolean.
    fn flag(&self, name: &str) -> Option<bool> {
        self.body.get(name).and_then(Value::as_bool)
    }

    /// A string member of the body, or `None` when it is absent or not a string.
    fn text(&self, name: &str) -> Option<String> {
        self.body
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
    }
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it, reached
/// the way the production router reaches it. The engine is kept so a case can read the
/// SHARED command index beside the service's own store, and can age a command's deprecation
/// without waiting hours for the clock.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
    }

    async fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://iot.{REGION}.amazonaws.com{path}"))
            .header("authorization", signed_for_iot())
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply {
            status,
            error_code,
            body,
        }
    }

    async fn create_command(&self, command_id: &str, body: &Value) -> Reply {
        self.call("PUT", &format!("/commands/{command_id}"), Some(body))
            .await
    }

    async fn get_command(&self, command_id: &str) -> Reply {
        self.call("GET", &format!("/commands/{command_id}"), None).await
    }

    async fn update_command(&self, command_id: &str, body: &Value) -> Reply {
        self.call("PATCH", &format!("/commands/{command_id}"), Some(body))
            .await
    }

    async fn delete_command(&self, command_id: &str) -> Reply {
        self.call("DELETE", &format!("/commands/{command_id}"), None)
            .await
    }

    async fn list_commands(&self) -> Reply {
        self.call("GET", "/commands", None).await
    }

    /// Whether the SHARED command index holds `command_arn` — the question
    /// `StartCommandExecution` asks before it enqueues anything.
    fn index_holds(&self, command_arn: &str) -> bool {
        command_registry(&*self.engine)
            .expect("the shared IoT command index")
            .lock()
            .contains(command_arn)
    }

    /// Whether the SHARED command index reports `command_arn` as deprecated — the other
    /// fact the data plane decides a `StartCommandExecution` on.
    fn index_deprecated(&self, command_arn: &str) -> Option<bool> {
        command_registry(&*self.engine)
            .expect("the shared IoT command index")
            .lock()
            .facts(command_arn)
            .map(|facts| facts.deprecated)
    }

    /// Move the instant a stored command was DEPRECATED back by `age`, so a case can
    /// observe both sides of the 12-hour deletion timeout without waiting for it. Nothing a
    /// request can reach does this; it stands in for hours of clock.
    fn age_deprecation(&self, command_id: &str, age: Duration) {
        let state = service_state(&*self.engine).expect("the IoT service store");
        let mut store = state.lock();
        let command = store
            .commands
            .get_mut(command_id)
            .unwrap_or_else(|| panic!("the store holds the command {command_id:?}"));
        let deprecated_at = command
            .deprecated_at
            .unwrap_or_else(|| panic!("the command {command_id:?} carries a deprecation"));
        command.deprecated_at = Some(
            deprecated_at
                .checked_sub(age)
                .expect("an instant that far back is representable"),
        );
    }

    /// Record a deprecation made `age` ago directly on the stored command, the way a record
    /// that has been sitting deprecated for hours carries one.
    fn deprecated_since(&self, command_id: &str, age: Duration) {
        let state = service_state(&*self.engine).expect("the IoT service store");
        let mut store = state.lock();
        let command = store
            .commands
            .get_mut(command_id)
            .unwrap_or_else(|| panic!("the store holds the command {command_id:?}"));
        command.set_deprecated(
            true,
            SystemTime::now()
                .checked_sub(age)
                .expect("an instant that far back is representable"),
        );
    }

    /// The instants the stored command carries: when it was deprecated, and when its
    /// scheduled removal falls due.
    fn deletion_instants(&self, command_id: &str) -> (Option<SystemTime>, Option<SystemTime>) {
        let state = service_state(&*self.engine).expect("the IoT service store");
        let store = state.lock();
        let command = store
            .commands
            .get(command_id)
            .unwrap_or_else(|| panic!("the store holds the command {command_id:?}"));
        (command.deprecated_at, command.deletion_due_at)
    }

    /// Bring the scheduled removal of a stored command forward to an instant the clock has
    /// already passed, which is what the next call must materialise.
    fn make_removal_due(&self, command_id: &str) {
        let state = service_state(&*self.engine).expect("the IoT service store");
        let mut store = state.lock();
        let command = store
            .commands
            .get_mut(command_id)
            .unwrap_or_else(|| panic!("the store holds the command {command_id:?}"));
        assert!(
            command.deletion_due_at.is_some(),
            "case {command_id:?}: a removal must be scheduled before it can fall due, got \
             {command:?}"
        );
        command.deletion_due_at = Some(
            SystemTime::now()
                .checked_sub(Duration::from_secs(1))
                .expect("an instant one second back is representable"),
        );
    }

    /// How many commands the service's own store holds.
    fn stored_commands(&self) -> usize {
        let state = service_state(&*self.engine).expect("the IoT service store");
        let held = state.lock().commands.len();
        held
    }
}

/// The ARN a command created in [`REGION`] must carry.
fn expected_arn(command_id: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:command/{command_id}")
}

/// A `CreateCommandRequest` body: the members are not what this suite is about, so one
/// static command serves every case.
fn create_body() -> Value {
    json!({
        "displayName": "Reboot the device",
        // A blob is base64 on the wire: this is `{"cmd":"reboot"}`.
        "payload": {
            "content": "eyJjbWQiOiJyZWJvb3QifQ==",
            "contentType": "application/json",
        },
    })
}

/// How long a command has been deprecated when the delete arrives.
#[derive(Debug, Clone, Copy)]
enum Deprecation {
    /// The command was never deprecated.
    Never,
    /// It was deprecated through `UpdateCommand`, so the deprecation is this instant.
    JustNow,
    /// It carries a deprecation made this long ago.
    Ago(Duration),
}

/// One delete: a label, how long the command has been deprecated, whether the account holds
/// it at all, the `statusCode` the delete must report, and whether the command is gone once
/// the call returns.
struct Case {
    label: &'static str,
    command_id: &'static str,
    deprecation: Deprecation,
    created: bool,
    status: u16,
    removed: bool,
}

/// The table spans the dimension the rule is about — how long the command has been
/// deprecated — on both sides of the published bound and ON it, plus the delete of a command
/// the account does not hold, which the model gives no not-found error to answer.
fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "a command that was never deprecated",
            command_id: "never-deprecated",
            deprecation: Deprecation::Never,
            created: true,
            status: 202,
            removed: false,
        },
        Case {
            label: "a command deprecated by this caller a moment ago",
            command_id: "deprecated_just_now",
            deprecation: Deprecation::JustNow,
            created: true,
            status: 202,
            removed: false,
        },
        Case {
            label: "a command deprecated one second short of the timeout",
            command_id: "deprecated-just-short",
            deprecation: Deprecation::Ago(
                COMMAND_DELETION_TIMEOUT.saturating_sub(Duration::from_secs(1)),
            ),
            created: true,
            status: 202,
            removed: false,
        },
        Case {
            label: "a command deprecated exactly the timeout ago",
            command_id: "deprecated-at-the-bound",
            deprecation: Deprecation::Ago(COMMAND_DELETION_TIMEOUT),
            created: true,
            status: 204,
            removed: true,
        },
        Case {
            label: "a command deprecated one second past the timeout",
            command_id: "deprecated-past-the-bound",
            deprecation: Deprecation::Ago(COMMAND_DELETION_TIMEOUT + Duration::from_secs(1)),
            created: true,
            status: 204,
            removed: true,
        },
        Case {
            label: "a command deprecated a week ago",
            command_id: "deprecated_long_ago",
            deprecation: Deprecation::Ago(Duration::from_secs(7 * 24 * 60 * 60)),
            created: true,
            status: 204,
            removed: true,
        },
        Case {
            label: "an id the account holds no command under",
            command_id: "never-created",
            deprecation: Deprecation::Never,
            created: false,
            status: 204,
            removed: true,
        },
    ]
}

/// The delete reports what it DID — 204 for a command it removed, 202 for one whose removal
/// it only scheduled — and the command is gone from both stores exactly when it reported a
/// removal.
#[tokio::test]
async fn a_delete_removes_only_a_command_deprecated_past_the_timeout() {
    for case in cases() {
        let emulator = Emulator::new();
        let arn = expected_arn(case.command_id);

        if case.created {
            let created = emulator.create_command(case.command_id, &create_body()).await;
            assert_eq!(
                created.status, 200,
                "case {:?}: the command must be created first, got {created:?}",
                case.label
            );
            match case.deprecation {
                Deprecation::Never => {}
                Deprecation::JustNow => {
                    let updated = emulator
                        .update_command(case.command_id, &json!({ "deprecated": true }))
                        .await;
                    assert_eq!(
                        updated.status, 200,
                        "case {:?}: deprecating it must succeed, got {updated:?}",
                        case.label
                    );
                }
                Deprecation::Ago(age) => emulator.deprecated_since(case.command_id, age),
            }
        }

        let deleted = emulator.delete_command(case.command_id).await;
        assert_eq!(
            deleted.status, case.status,
            "case {:?}: expected HTTP {}, got {deleted:?}",
            case.label, case.status
        );

        // `pendingDeletion` and the reader's answer are the same fact seen twice: a removed
        // command is reported by nobody, a scheduled one is still reported, pending.
        let read = emulator.get_command(case.command_id).await;
        if case.removed {
            assert_eq!(
                (read.status, read.error_code.as_deref()),
                (404, Some("ResourceNotFoundException")),
                "case {:?}: a removed command is reported by nobody, got {read:?}",
                case.label
            );
            assert!(
                !emulator.index_holds(&arn),
                "case {:?}: a removed command must leave the shared index, which still holds \
                 {arn:?}",
                case.label
            );
            assert_eq!(
                emulator.stored_commands(),
                0,
                "case {:?}: a removed command leaves no record, got {} stored",
                case.label,
                emulator.stored_commands()
            );
        } else {
            assert_eq!(
                read.status, 200,
                "case {:?}: a command whose removal is only scheduled is still readable, got \
                 {read:?}",
                case.label
            );
            assert_eq!(
                (read.flag("pendingDeletion"), read.flag("deprecated")),
                (Some(true), Some(true)),
                "case {:?}: a scheduled removal puts the command in pendingDeletion and \
                 deprecates it, got {read:?}",
                case.label
            );
            assert_eq!(
                (emulator.index_holds(&arn), emulator.index_deprecated(&arn)),
                (true, Some(true)),
                "case {:?}: the shared index must still hold {arn:?}, deprecated, got {:?}",
                case.label,
                emulator.index_deprecated(&arn)
            );
        }
    }
}

/// A scheduled removal falls due ONE timeout after the command's deprecation — the delete's
/// own instant for a command that carried none — and repeating the delete cannot postpone it.
#[tokio::test]
async fn a_scheduled_removal_is_counted_from_the_deprecation_and_cannot_be_postponed() {
    // (case, how long the command had been deprecated when the delete arrived)
    let rows: [(&str, Option<Duration>); 4] = [
        ("never deprecated", None),
        ("deprecated a second ago", Some(Duration::from_secs(1))),
        ("deprecated an hour ago", Some(Duration::from_secs(3_600))),
        (
            "deprecated a second short of the timeout",
            Some(COMMAND_DELETION_TIMEOUT.saturating_sub(Duration::from_secs(1))),
        ),
    ];

    for (index, (case, age)) in rows.into_iter().enumerate() {
        let emulator = Emulator::new();
        let command_id = format!("scheduled-{index}");
        let created = emulator.create_command(&command_id, &create_body()).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: the command must be created first, got {created:?}"
        );
        if let Some(age) = age {
            emulator.deprecated_since(&command_id, age);
        }

        let deleted = emulator.delete_command(&command_id).await;
        assert_eq!(
            deleted.status, 202,
            "case {case:?}: a command not yet deprecated past the timeout is only scheduled, \
             got {deleted:?}"
        );
        let (deprecated_at, due) = emulator.deletion_instants(&command_id);
        let deprecated_at =
            deprecated_at.unwrap_or_else(|| panic!("case {case:?}: the delete deprecates it"));
        assert_eq!(
            due,
            deprecated_at.checked_add(COMMAND_DELETION_TIMEOUT),
            "case {case:?}: the removal falls due one timeout after the deprecation, got {due:?}"
        );

        // A second delete re-reports the schedule and keeps the instant it recorded: a
        // caller cannot hold a command in the account by deleting it again. The deprecation
        // it already carries is not restarted either.
        let repeated = emulator.delete_command(&command_id).await;
        assert_eq!(
            repeated.status, 202,
            "case {case:?}: repeating the delete re-reports the scheduled removal, got \
             {repeated:?}"
        );
        assert_eq!(
            emulator.deletion_instants(&command_id),
            (Some(deprecated_at), due),
            "case {case:?}: repeating the delete must not postpone the removal, got {:?}",
            emulator.deletion_instants(&command_id)
        );
    }
}

/// Once the recorded instant is reached the command is permanently removed: no reader
/// reports it, the shared index no longer holds it, its id is free for a new create, and
/// every command whose removal is not yet due is untouched.
#[tokio::test]
async fn a_command_whose_scheduled_removal_is_due_is_gone_from_every_reader() {
    // (case, the command whose removal is made due, how many commands the account holds)
    let rows: [(&str, usize); 3] = [
        ("the only command in the account", 1),
        ("one of two commands", 2),
        ("one of five commands", 5),
    ];

    for (case, held) in rows {
        let emulator = Emulator::new();
        let ids: Vec<String> = (0..held).map(|ordinal| format!("due-{ordinal}")).collect();
        for command_id in &ids {
            let created = emulator.create_command(command_id, &create_body()).await;
            assert_eq!(
                created.status, 200,
                "case {case:?}: creating {command_id:?} must succeed, got {created:?}"
            );
            let deleted = emulator.delete_command(command_id).await;
            assert_eq!(
                deleted.status, 202,
                "case {case:?}: deleting {command_id:?} schedules its removal, got {deleted:?}"
            );
        }

        // Only the FIRST command's removal is brought forward; every other one stays
        // scheduled, so this also proves the materialisation removes what is due and
        // nothing else.
        let removed_id = ids[0].clone();
        emulator.make_removal_due(&removed_id);
        let removed_arn = expected_arn(&removed_id);

        let read = emulator.get_command(&removed_id).await;
        assert_eq!(
            (read.status, read.error_code.as_deref()),
            (404, Some("ResourceNotFoundException")),
            "case {case:?}: a command past its removal instant is reported by nobody, got \
             {read:?}"
        );
        assert!(
            !emulator.index_holds(&removed_arn),
            "case {case:?}: the shared index must not hold {removed_arn:?} any more"
        );

        let listed = emulator.list_commands().await;
        assert_eq!(
            listed.status, 200,
            "case {case:?}: the listing must succeed, got {listed:?}"
        );
        let listed_ids: Vec<String> = listed
            .body
            .get("commands")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| row.get("commandId").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            !listed_ids.contains(&removed_id),
            "case {case:?}: the listing must not report {removed_id:?}, got {listed_ids:?}"
        );
        assert_eq!(
            listed_ids.len(),
            held - 1,
            "case {case:?}: every command whose removal is not due is still listed, got \
             {listed_ids:?}"
        );

        // A command the account no longer holds does not hold its id: the next create takes
        // it rather than conflicting with a record that is gone.
        let recreated = emulator.create_command(&removed_id, &create_body()).await;
        assert_eq!(
            recreated.status, 200,
            "case {case:?}: the id of a removed command is free for a new create, got \
             {recreated:?}"
        );
        let reread = emulator.get_command(&removed_id).await;
        assert_eq!(
            (reread.status, reread.flag("pendingDeletion"), reread.flag("deprecated")),
            (200, Some(false), Some(false)),
            "case {case:?}: the command created under that id is a fresh one, got {reread:?}"
        );
        assert!(
            emulator.index_holds(&removed_arn),
            "case {case:?}: the new command is registered in the shared index as {removed_arn:?}"
        );
    }
}

/// A command whose deletion is scheduled is readable but NOT writable: `UpdateCommand` is
/// refused `ConflictException` (409) with the sentence AWS words that conflict in, and
/// nothing it named is written.
#[tokio::test]
async fn an_update_of_a_command_pending_deletion_is_refused_and_writes_nothing() {
    // (case, the update body) — one row per shape of update the operation accepts:
    // `displayName`, `description`, the deprecation flag in both directions, and a request
    // naming every member at once
    // (`smithy_query(query="operation",operation="UpdateCommand")`).
    let rows: [(&str, Value); 5] = [
        ("a new display name", json!({ "displayName": "Renamed" })),
        ("a new description", json!({ "description": "changed" })),
        ("undeprecating it", json!({ "deprecated": false })),
        ("deprecating it again", json!({ "deprecated": true })),
        (
            "every member at once",
            json!({
                "displayName": "Renamed",
                "description": "changed",
                "deprecated": false,
            }),
        ),
    ];

    for (index, (case, body)) in rows.into_iter().enumerate() {
        let emulator = Emulator::new();
        let command_id = format!("pending-{index}");
        let created = emulator.create_command(&command_id, &create_body()).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: the command must be created first, got {created:?}"
        );
        let deleted = emulator.delete_command(&command_id).await;
        assert_eq!(
            deleted.status, 202,
            "case {case:?}: the delete must schedule the removal, got {deleted:?}"
        );
        let before = emulator.get_command(&command_id).await;

        let refused = emulator.update_command(&command_id, &body).await;
        assert_eq!(
            (refused.status, refused.error_code.as_deref()),
            (409, Some("ConflictException")),
            "case {case:?}: an update of a command pending deletion is a conflict, got \
             {refused:?}"
        );
        assert_eq!(
            refused.message(),
            format!("Failed to update command {command_id} as it's pending deletion."),
            "case {case:?}: expected the conflict AWS words for a pending deletion, got {:?}",
            refused.message()
        );

        let after = emulator.get_command(&command_id).await;
        assert_eq!(
            (
                after.text("displayName"),
                after.text("description"),
                after.flag("deprecated"),
                after.flag("pendingDeletion"),
            ),
            (
                before.text("displayName"),
                before.text("description"),
                before.flag("deprecated"),
                before.flag("pendingDeletion"),
            ),
            "case {case:?}: a refused update writes nothing, got {after:?} instead of \
             {before:?}"
        );
    }
}

/// The deprecation a command already carries is what the timeout is counted from, so an
/// `UpdateCommand` that re-states a deprecation the command has had for hours does not
/// restart it: the delete that follows removes the command instead of scheduling it.
#[tokio::test]
async fn restating_a_deprecation_does_not_restart_the_deletion_timeout() {
    // (case, how long the command has been deprecated, the delete's status code)
    let rows: [(&str, Duration, u16); 2] = [
        (
            "deprecated an hour ago",
            Duration::from_secs(3_600),
            202,
        ),
        (
            "deprecated a day ago",
            Duration::from_secs(24 * 60 * 60),
            204,
        ),
    ];

    for (index, (case, age, status)) in rows.into_iter().enumerate() {
        let emulator = Emulator::new();
        let command_id = format!("restated-{index}");
        let created = emulator.create_command(&command_id, &create_body()).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: the command must be created first, got {created:?}"
        );

        // Deprecated through the wire, then aged: the record now carries a deprecation made
        // `age` ago, exactly as one that has been sitting deprecated does.
        let deprecated = emulator
            .update_command(&command_id, &json!({ "deprecated": true }))
            .await;
        assert_eq!(
            deprecated.status, 200,
            "case {case:?}: deprecating it must succeed, got {deprecated:?}"
        );
        emulator.age_deprecation(&command_id, age);
        let (aged_at, _) = emulator.deletion_instants(&command_id);

        let restated = emulator
            .update_command(&command_id, &json!({ "deprecated": true }))
            .await;
        assert_eq!(
            restated.status, 200,
            "case {case:?}: re-stating the deprecation must succeed, got {restated:?}"
        );
        assert_eq!(
            emulator.deletion_instants(&command_id).0,
            aged_at,
            "case {case:?}: re-stating a deprecation keeps the instant it was made, got {:?}",
            emulator.deletion_instants(&command_id).0
        );

        let deleted = emulator.delete_command(&command_id).await;
        assert_eq!(
            deleted.status, status,
            "case {case:?}: expected HTTP {status} from the delete, got {deleted:?}"
        );
    }
}
