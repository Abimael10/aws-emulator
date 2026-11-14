// SPDX-License-Identifier: CC0-1.0

//! The WHOLE life of one legal hold — create, settle, enumerate, cancel,
//! enumerate again — driven as real HTTP through the router.
//!
//! The RULE under test: a legal hold is a stored resource whatever
//! recovery-point selection it was created with, including one that names every
//! vault with the entry no vault can be named by (the model types `VaultNames`
//! as a list of plain `string` with neither a `@pattern` nor a `@length`,
//! `smithy_query(query="shape", shape="VaultNames")`, unlike the patterned
//! `BackupVaultName`), so such a create is accepted, the hold it stores settles
//! from the non-terminal status it is returned in to `ACTIVE`, the account's
//! listing reports it — and only it — throughout, and a cancellation leaves the
//! record listed with the terminal `CANCELED` status and the reason the
//! cancellation stated ("This action returns a list of all active and previous
//! legal holds", `smithy_query` operation `ListLegalHolds`).
//!
//! The rows of the table differ along what the rule generalises over: the FORM
//! of the selection the hold was created with (every vault alone, every vault
//! beside a named one, every vault within a closed window, every vault beside a
//! protected resource, a named vault alone as the control) and the form of the
//! reason the cancellation states. None of them changes what the lifecycle does,
//! which is the point of the table.
//!
//! The sibling suites pin the pieces — `legal_hold.rs` the create/list/get
//! round trip and the settle, `legal_hold_cancel_record_retention.rs` what a
//! cancellation records, `legal_hold_vault_selection.rs` which selections are
//! accepted. This suite pins their COMPOSITION: that one client can walk a hold
//! from the create to the cancelled listing without a step refusing it.
//!
//! The request harness (`router`, `call`) is the one `vault_and_backup_job.rs`
//! exposes to this test binary, and the legal-hold calls and the
//! poll-until-in-force helper are the ones
//! `legal_hold_cancel_record_retention.rs` exposes, so this suite adds no second
//! copy of either.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use std::time::{Duration, SystemTime};

use super::legal_hold_cancel_record_retention::{
    cancel_legal_hold, get_legal_hold, hold_in_force, list_legal_holds, STATUS_ACTIVE,
    STATUS_CANCELED, STATUS_CREATING,
};
use super::vault_and_backup_job::{call, epoch_seconds, router, string_at, ACCOUNT, REGION};

/// The `VaultNames` entry that selects every vault (`crate::state`'s
/// `VAULT_NAMES_WILDCARD`): the `BackupVaultName` pattern
/// (`^[a-zA-Z0-9\-\_]{2,50}$`) admits no `*`, so no vault can carry it as a
/// name.
const EVERY_VAULT: &str = "*";

/// The status `CancelLegalHold` answers a cancellation it made with — the
/// operation's modelled success code (`@http` `code: 201`, `smithy_query`
/// operation `CancelLegalHold`), not the 200 a read answers with.
const CANCEL_SUCCESS_STATUS: u16 = 201;

/// One row of the lifecycle table: the selection the hold is created with, and
/// the reason its cancellation states.
struct LifecycleCase {
    name: &'static str,
    /// The `RecoveryPointSelection` as sent, which every read of the hold has to
    /// report back unchanged.
    selection: Value,
    /// Members the create carries beside the title, the description and the
    /// selection (tags, an idempotency token), as a JSON object.
    extras: Value,
    /// The `cancelDescription` the cancellation states, percent-encoded as it
    /// travels on the query string.
    encoded_reason: &'static str,
    /// That same reason decoded, which is what the cancelled hold must report.
    reason: &'static str,
}

impl LifecycleCase {
    /// The `CreateLegalHold` request body this row sends.
    fn request(&self) -> Value {
        let mut request = json!({
            "Title": format!("Lifecycle: {}", self.name),
            "Description": format!("the hold this case walks end to end: {}", self.name),
            "RecoveryPointSelection": self.selection.clone(),
        });
        for (member, value) in self.extras.as_object().into_iter().flatten() {
            request[member] = value.clone();
        }
        request
    }

    /// The query string this row's cancellation travels with.
    fn cancel_query(&self) -> String {
        format!("?cancelDescription={}", self.encoded_reason)
    }
}

/// An instant `ago` before now, as the epoch seconds the protocol serialises a
/// timestamp as — the shared harness's clock reader, so this suite adds no
/// second one.
fn seconds_ago(ago: Duration) -> f64 {
    epoch_seconds(SystemTime::now() - ago)
}

/// The rows of the table, spanning the selection forms a hold over every vault
/// can take plus the named-vault control, and the reason forms a cancellation
/// can state.
fn lifecycle_cases() -> Vec<LifecycleCase> {
    vec![
        LifecycleCase {
            name: "every vault, nothing else",
            selection: json!({"VaultNames": [EVERY_VAULT]}),
            extras: json!({}),
            encoded_reason: "released",
            reason: "released",
        },
        LifecycleCase {
            name: "every vault within a closed window",
            selection: json!({
                "VaultNames": [EVERY_VAULT],
                "DateRange": {
                    "FromDate": seconds_ago(Duration::from_secs(3_600)),
                    "ToDate": seconds_ago(Duration::from_secs(1)),
                },
            }),
            extras: json!({"Tags": {"owner": "legal", "matter": "case-91"}}),
            encoded_reason: "window%20reviewed%2C%20hold%20lifted",
            reason: "window reviewed, hold lifted",
        },
        LifecycleCase {
            name: "every vault beside a named one",
            selection: json!({"VaultNames": [EVERY_VAULT, "audit-vault"]}),
            extras: json!({"IdempotencyToken": "lifecycle-every-vault-and-one"}),
            encoded_reason: "matter%20settled%3A%20no%20appeal.",
            reason: "matter settled: no appeal.",
        },
        LifecycleCase {
            name: "every vault beside a protected resource",
            selection: json!({
                "VaultNames": [EVERY_VAULT],
                "ResourceIdentifiers": [
                    "arn:aws:dynamodb:us-west-2:000000000000:table/ledger",
                ],
            }),
            extras: json!({}),
            encoded_reason: "the-ledger-is-no-longer-under-hold",
            reason: "the-ledger-is-no-longer-under-hold",
        },
        LifecycleCase {
            // The rule is that the model constrains these entries not at all, so
            // a row names a vault no vault could be called either: the
            // `BackupVaultName` pattern (`^[a-zA-Z0-9\-\_]{2,50}$`) admits
            // neither the spaces nor the punctuation below.
            name: "an entry that is no vault name at all",
            selection: json!({"VaultNames": ["not a vault name!", EVERY_VAULT]}),
            extras: json!({}),
            encoded_reason: "nothing%20was%20ever%20held",
            reason: "nothing was ever held",
        },
        LifecycleCase {
            name: "a named vault alone, the control",
            selection: json!({"VaultNames": ["audit-vault"]}),
            extras: json!({}),
            encoded_reason: "audit%20closed",
            reason: "audit closed",
        },
    ]
}

/// `POST /legal-holds` — `CreateLegalHold`'s HTTP binding.
async fn create_legal_hold(router: &MultiServiceRouter, request: &Value) -> (u16, Value) {
    call(router, "POST", "/legal-holds", Some(&request.to_string())).await
}

/// A recovery-point selection with every timestamp of its `DateRange` read as
/// the fractional epoch seconds the protocol serialises one as, so a comparison
/// is about the INSTANT rather than about whether the number was written with a
/// fractional part.
fn selection_instants(selection: Option<&Value>) -> (Option<&Value>, Vec<Option<f64>>) {
    let range = selection.and_then(|selection| selection.get("DateRange"));
    let bounds = ["FromDate", "ToDate"]
        .into_iter()
        .map(|member| {
            range
                .and_then(|range| range.get(member))
                .and_then(Value::as_f64)
        })
        .collect();
    let names = selection.and_then(|selection| selection.get("VaultNames"));
    (names, bounds)
}

/// Whether `answered` reports the selection `sent` was created with: the vault
/// names as sent, the resource identifiers as sent, and each `DateRange` bound
/// at the same instant.
fn reports_the_selection(sent: Option<&Value>, answered: Option<&Value>) -> bool {
    let identifiers = |selection: Option<&Value>| {
        selection
            .and_then(|selection| selection.get("ResourceIdentifiers"))
            .cloned()
    };
    identifiers(sent) == identifiers(answered)
        && selection_instants(sent) == selection_instants(answered)
}

/// The one entry the account's listing reports, with the case named when it
/// reports anything else.
fn the_only_listed_hold(case: &str, holds: &[Value]) -> Value {
    assert_eq!(
        holds.len(),
        1,
        "case {case:?}: expected exactly the one hold this account holds listed, got {holds:?}"
    );
    holds[0].clone()
}

/// A legal hold walks its whole life whatever its selection names: the create is
/// accepted and echoes the selection as sent, the clock puts the hold in force,
/// the account's listing reports it alone, and the cancellation leaves it listed
/// with the terminal status and the reason it stated.
#[tokio::test]
async fn a_legal_hold_walks_create_settle_list_and_cancel() {
    let cases = lifecycle_cases();

    // Every row gets its OWN router, so each account holds exactly one hold and
    // "the listing reports it alone" is a claim about that hold. The creates all
    // happen before any row waits, so the activations the clock owes them fall
    // due together rather than one after another.
    let mut created: Vec<(MultiServiceRouter, Value, Value)> = Vec::with_capacity(cases.len());
    for case in &cases {
        // One router per row, so the account each row works in holds exactly one
        // legal hold.
        let router = router();
        let request = case.request();
        let (status, answer) = create_legal_hold(&router, &request).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected CreateLegalHold 200, got {status} {answer:?}",
            case.name
        );
        created.push((router, request, answer));
    }

    for (case, (router, request, answer)) in cases.iter().zip(&created) {
        let case_name = case.name;

        // The create's own answer: the selection as sent, the request's title
        // and description, a minted id, ARN and creation instant, and the
        // non-terminal status a fresh hold is returned in.
        for member in ["Title", "Description"] {
            assert_eq!(
                answer.get(member),
                request.get(member),
                "case {case_name:?}: {member} must report what the create was given, \
                 got {answer:?}"
            );
        }
        assert!(
            reports_the_selection(
                request.get("RecoveryPointSelection"),
                answer.get("RecoveryPointSelection")
            ),
            "case {case_name:?}: the create must echo the selection as sent, got {answer:?}"
        );
        let id = string_at(answer, "LegalHoldId");
        assert!(
            !id.is_empty(),
            "case {case_name:?}: the create must mint a LegalHoldId, got {answer:?}"
        );
        assert_eq!(
            string_at(answer, "LegalHoldArn"),
            format!("arn:aws:backup:{REGION}:{ACCOUNT}:legal-hold:{id}"),
            "case {case_name:?}: expected the hold's own ARN, got {answer:?}"
        );
        assert_eq!(
            string_at(answer, "Status"),
            STATUS_CREATING,
            "case {case_name:?}: a fresh hold is still being placed, got {answer:?}"
        );
        assert!(
            answer["CreationDate"].is_number(),
            "case {case_name:?}: the create must report a CreationDate, got {answer:?}"
        );

        // The activation the clock owes the hold, waited for by STATUS.
        hold_in_force(router, case_name, &id).await;
        let in_force = get_legal_hold(router, case_name, &id).await;
        assert!(
            reports_the_selection(
                request.get("RecoveryPointSelection"),
                in_force.get("RecoveryPointSelection")
            ),
            "case {case_name:?}: coming into force must not rewrite the selection, \
             got {in_force:?}"
        );
        for member in ["CancellationDate", "CancelDescription", "RetainRecordUntil"] {
            assert!(
                in_force.get(member).is_none(),
                "case {case_name:?}: a hold nobody cancelled has no {member}, got {in_force:?}"
            );
        }

        // The listing reports that hold, and only that hold, while it is active.
        let listed = the_only_listed_hold(case_name, &list_legal_holds(router, case_name).await);
        for member in ["Title", "Description", "LegalHoldId", "LegalHoldArn"] {
            assert_eq!(
                listed.get(member),
                answer.get(member),
                "case {case_name:?}: the listed {member} differs from the created one, \
                 got {listed:?}"
            );
        }
        assert_eq!(
            string_at(&listed, "Status"),
            STATUS_ACTIVE,
            "case {case_name:?}: a hold in force is listed as {STATUS_ACTIVE:?}, got {listed:?}"
        );
        assert!(
            listed.get("CancellationDate").is_none(),
            "case {case_name:?}: a hold nobody cancelled has no CancellationDate, got {listed:?}"
        );

        // The cancellation, which the hold in force admits.
        let (status, errortype, cancelled) =
            cancel_legal_hold(router, &id, &case.cancel_query()).await;
        assert_eq!(
            (status, errortype.as_str()),
            (CANCEL_SUCCESS_STATUS, "<unset>"),
            "case {case_name:?}: expected CancelLegalHold {CANCEL_SUCCESS_STATUS}, \
             got {status} {cancelled:?}"
        );

        // The record is KEPT: the hold is still the one entry the listing
        // reports, now in the terminal status, and its own read reports the
        // reason the cancellation stated beside the selection it was created
        // with.
        let after = the_only_listed_hold(case_name, &list_legal_holds(router, case_name).await);
        assert_eq!(
            (
                after.get("LegalHoldId"),
                string_at(&after, "Status").as_str()
            ),
            (answer.get("LegalHoldId"), STATUS_CANCELED),
            "case {case_name:?}: a cancelled hold stays listed as {STATUS_CANCELED:?}, \
             got {after:?}"
        );
        assert!(
            after["CancellationDate"].is_number(),
            "case {case_name:?}: a cancelled hold reports when it was cancelled, got {after:?}"
        );

        let read = get_legal_hold(router, case_name, &id).await;
        assert_eq!(
            (
                string_at(&read, "Status").as_str(),
                string_at(&read, "CancelDescription").as_str()
            ),
            (STATUS_CANCELED, case.reason),
            "case {case_name:?}: the cancelled hold reports the status and the reason stated, \
             got {read:?}"
        );
        assert!(
            reports_the_selection(
                request.get("RecoveryPointSelection"),
                read.get("RecoveryPointSelection")
            ),
            "case {case_name:?}: a cancellation must not rewrite the selection, got {read:?}"
        );
    }
}
