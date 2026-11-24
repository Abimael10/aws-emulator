// SPDX-License-Identifier: CC0-1.0

//! How the cancels that act on the ONE task their request names answer a value that is no id
//! of the class their parameter takes, driven over the wire the service actually speaks —
//! `ec2Query`, through the crate's shared [`super::ec2_query`] harness.
//!
//! The rule under test: such an operation reads the FORM of the id it was given before it
//! looks anything up, and a value outside the form of every class that parameter admits is no
//! id the operation resolves at all — so it is refused HTTP 400 with EC2's resource-agnostic
//! code for one, `InvalidID`, and the sentence that quotes the value back: fixed text `The ID
//! '` … `' is not valid`, the only variable part being the value the REQUEST sent. The task
//! family's own `NotFound` code is reserved for an id whose class WAS read and that the
//! account holds no task under: it reports a task that is missing, which a value that is not
//! one of its ids never is.
//!
//! Read through both operations the model gives a single task id to — `CancelImportTask`,
//! whose `ImportTaskId` takes "the ID of the import image or import snapshot task to be
//! canceled", and `CancelExportTask`, whose `ExportTaskId` takes the id of an instance or
//! image export task — because the rule is about the CLASS of a parameter's id rather than
//! about one task family.
//!
//! Rows differ along what the rule generalises over: which operation reads the value, which
//! minting prefix of its class the value departs from, and HOW it departs (an uppercase
//! suffix, a digit outside hexadecimal, an empty suffix, a suffix of a length that class is
//! never minted in, no kind segment at all, the other cancel's own id, another resource's id,
//! and a value of no resource-id shape). The value a refusal quotes back is bounded, like
//! every echoed caller value in this crate, at [`ec2::state::MAX_ECHOED_RESOURCE_ID_CHARS`]
//! characters, which the last test here reads over both operations. Every row is cross-checked against the CRATE'S OWN
//! reader of the form, so a row claims to be outside it only where the handler agrees, and
//! the prefixes are read from the crate's own tables so a class added there is covered here
//! without a new literal. The codes and sentences this suite ASSERTS are written out as the
//! recorded responses give them, never read back out of the implementation.

use std::time::Instant;

use ec2::ops::cancel_export_task::{EXPORT_TASK_ID_PREFIXES, export_task_id_is_well_formed};
use ec2::ops::types::{ExportTask, ImportImageTask};
use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, EXPORT_TASK_STATE_ACTIVE, ExportTaskRecord,
    IMPORT_TASK_KINDS, IMPORT_TASK_STATE_ACTIVE, IMPORT_TASK_STATE_DELETING,
    ImportImageTaskRecord, MAX_ECHOED_RESOURCE_ID_CHARS,
    import_task_id_of_any_kind_is_well_formed,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, error_code, error_message, member};

/// The Region every call here is signed for. Neither kind of task carries a Region member of
/// its own, so the Region only selects the ec2Query envelope.
const REGION: &str = "eu-central-1";

/// The status every refusal here carries: a fault of the REQUEST.
const REFUSAL_STATUS: u16 = 400;

/// What a value that is no resource id of a class the operation resolves is refused with.
const INVALID_ID: &str = "InvalidID";

/// The import (conversion) task family's code for a task of a class it READ that the account
/// holds none of.
const INVALID_CONVERSION_TASK_ID: &str = "InvalidConversionTaskId";

/// The export task family's code for the same half of the rule.
const INVALID_EXPORT_TASK_ID_NOT_FOUND: &str = "InvalidExportTaskID.NotFound";

/// A hexadecimal suffix of the long id form EC2 mints today, for building an id a class
/// really could have been issued under.
const MINTED_SUFFIX: &str = "0123456789abcdef0";

/// One `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// A cancel that acts on the ONE task the request names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cancel {
    /// `CancelImportTask`, reading `ImportTaskId` — an import image or import snapshot task.
    ImportTask,
    /// `CancelExportTask`, reading `ExportTaskId` — an instance or image export task.
    ExportTask,
}

impl Cancel {
    /// Both operations the rule is read through.
    const ALL: [Self; 2] = [Self::ImportTask, Self::ExportTask];

    /// The ec2Query `Action` this cancel is called under.
    fn action(self) -> &'static str {
        match self {
            Self::ImportTask => "CancelImportTask",
            Self::ExportTask => "CancelExportTask",
        }
    }

    /// The member this cancel names the one task in.
    fn parameter(self) -> &'static str {
        match self {
            Self::ImportTask => "ImportTaskId",
            Self::ExportTask => "ExportTaskId",
        }
    }

    /// Every minting prefix of the id class this parameter takes, each carrying its trailing
    /// hyphen, read from the crate's own tables.
    fn id_prefixes(self) -> Vec<String> {
        match self {
            Self::ImportTask => IMPORT_TASK_KINDS
                .iter()
                .map(|(prefix, _)| (*prefix).to_string())
                .collect(),
            Self::ExportTask => EXPORT_TASK_ID_PREFIXES
                .iter()
                .map(|prefix| format!("{prefix}-"))
                .collect(),
        }
    }

    /// Whether this parameter reads `value` as an id of its own class, through the CRATE'S
    /// OWN reader of that form — the one the handler asks.
    fn reads(self, value: &str) -> bool {
        match self {
            Self::ImportTask => import_task_id_of_any_kind_is_well_formed(value),
            Self::ExportTask => export_task_id_is_well_formed(value),
        }
    }

    /// The code this family answers for an id of its class the account holds no task under —
    /// the half of the rule `InvalidID` is NOT for.
    fn not_found_code(self) -> &'static str {
        match self {
            Self::ImportTask => INVALID_CONVERSION_TASK_ID,
            Self::ExportTask => INVALID_EXPORT_TASK_ID_NOT_FOUND,
        }
    }

    /// Suffix lengths outside the form of this class, as `(case, suffix)` pairs. An export
    /// task id is read in the 8 and 17 digit forms EC2 mints a resource id in, so a length
    /// either side of them is outside its form; an import task id's suffix length is no part
    /// of what that parameter reads, so it has no such row.
    fn unminted_suffixes(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::ImportTask => &[],
            Self::ExportTask => &[
                ("a suffix a digit short of the short form", "0123abc"),
                ("a suffix a digit past the long form", "0123456789abcdef01"),
            ],
        }
    }

    /// The word this family's ids begin with, taken from its own first prefix: the segment
    /// before the kind (`import`, `export`).
    fn family_word(self) -> String {
        self.id_prefixes()
            .first()
            .and_then(|prefix| prefix.split('-').next().map(str::to_string))
            .unwrap_or_else(|| panic!("{} must declare an id prefix", self.action()))
    }

    /// An id of this class the account holds no task under: its first prefix and a suffix of
    /// the length EC2 mints ids in today.
    fn unknown_id(self) -> String {
        format!(
            "{}{MINTED_SUFFIX}",
            self.id_prefixes()
                .first()
                .cloned()
                .unwrap_or_else(|| panic!("{} must declare an id prefix", self.action()))
        )
    }

    /// Make this call naming `value` as the one task, and nothing else.
    async fn invoke(self, router: &MultiServiceRouter, value: &str) -> (u16, String) {
        call(
            router,
            REGION,
            self.action(),
            &[p(self.parameter(), value)],
        )
        .await
    }
}

/// One value a cancel cannot read as an id of its class.
#[derive(Debug, Clone)]
struct Row {
    /// The operation that reads it.
    cancel: Cancel,
    /// What this row varies, for an assertion to name.
    case: String,
    /// The value the request sends.
    value: String,
}

/// Every way a value departs from the form of the class its cancel's parameter takes: each
/// minting prefix of that class crossed with the departures of a suffix, plus the values that
/// carry no kind segment of the class at all.
fn unreadable_rows() -> Vec<Row> {
    let suffix_departures = [
        ("an uppercase suffix", "0123ABCD"),
        ("a suffix outside hexadecimal", "0123abcz"),
        ("a long suffix outside hexadecimal", "0123456789abcdefz"),
        ("an empty suffix", ""),
    ];
    let mut rows: Vec<Row> = Vec::new();
    for cancel in Cancel::ALL {
        for prefix in cancel.id_prefixes() {
            for (case, suffix) in suffix_departures
                .iter()
                .chain(cancel.unminted_suffixes().iter())
            {
                rows.push(Row {
                    cancel,
                    case: format!("{prefix} with {case}"),
                    value: format!("{prefix}{suffix}"),
                });
            }
        }
        let family = cancel.family_word();
        for (case, value) in [
            (
                "the family word with no kind segment".to_string(),
                format!("{family}-fffffff0"),
            ),
            ("the bare family word".to_string(), family.clone()),
            (
                "the other cancel's own id".to_string(),
                cancel_other(cancel).unknown_id(),
            ),
            (
                "another resource's id".to_string(),
                format!("snap-{MINTED_SUFFIX}"),
            ),
            (
                "a value of no resource-id shape".to_string(),
                "the-last-task".to_string(),
            ),
        ] {
            rows.push(Row {
                cancel,
                case,
                value,
            });
        }
    }
    rows
}

/// The OTHER cancel — whose class of ids this one's parameter does not take.
fn cancel_other(cancel: Cancel) -> Cancel {
    match cancel {
        Cancel::ImportTask => Cancel::ExportTask,
        Cancel::ExportTask => Cancel::ImportTask,
    }
}

/// How many tasks of either family the store holds, over every store a cancel here could
/// have written to.
fn stored_tasks(engine: &engine::InMemoryEngine) -> usize {
    let store = engine.state::<Ec2State>();
    let guard = store.lock();
    guard.import_image_tasks.len() + guard.import_snapshot_tasks.len() + guard.export_tasks.len()
}

/// Plant a task of `cancel`'s own family under `key` — a record a LOOKUP would resolve, so a
/// refusal that still names the value can only have read the form first.
fn plant_task(engine: &engine::InMemoryEngine, cancel: Cancel, key: &str) {
    let store = engine.state::<Ec2State>();
    let mut guard = store.lock();
    let now = Instant::now();
    match cancel {
        Cancel::ImportTask => {
            guard.import_image_tasks.insert(
                key.to_string(),
                ImportImageTaskRecord {
                    view: ImportImageTask {
                        import_task_id: Some(key.to_string()),
                        status: Some(IMPORT_TASK_STATE_ACTIVE.to_string()),
                        ..ImportImageTask::default()
                    },
                    started_at: now,
                    // Far enough out that nothing the clock owes this planted task can fall
                    // due while the test runs.
                    completed_at: now + std::time::Duration::from_secs(3_600),
                },
            );
        }
        Cancel::ExportTask => {
            guard.export_tasks.insert(
                key.to_string(),
                ExportTaskRecord {
                    view: ExportTask {
                        export_task_id: Some(key.to_string()),
                        state: Some(EXPORT_TASK_STATE_ACTIVE.to_string()),
                        ..ExportTask::default()
                    },
                    cancelled_at: None,
                },
            );
        }
    }
}

/// The state a planted task of `cancel`'s family is in under `key`, straight off the store —
/// `None` when the store holds no such record.
fn planted_state(
    engine: &engine::InMemoryEngine,
    cancel: Cancel,
    key: &str,
) -> Option<String> {
    let store = engine.state::<Ec2State>();
    let guard = store.lock();
    match cancel {
        Cancel::ImportTask => guard
            .import_image_tasks
            .get(key)
            .and_then(|record| record.view.status.clone()),
        Cancel::ExportTask => guard
            .export_tasks
            .get(key)
            .and_then(|record| record.view.state.clone()),
    }
}

/// The rule: a value that is no id of the class the parameter takes is refused `InvalidID`
/// with the sentence quoting the value back, over every operation crossed with every way a
/// value leaves that form — and nothing is written.
#[tokio::test]
async fn a_cancel_refuses_a_value_that_is_no_id_of_its_class_as_an_invalid_id() {
    let (router, engine) = emulator();

    for row in unreadable_rows() {
        let Row {
            cancel,
            case,
            value,
        } = &row;
        assert!(
            !cancel.reads(value),
            "case {case:?}: {} must not read {value:?} as an id of its own class",
            cancel.action()
        );
        let case = format!("{} naming {case}", cancel.action());
        let (status, body) = cancel.invoke(&router, value).await;
        assert_eq!(
            status, REFUSAL_STATUS,
            "case {case:?}: expected HTTP {REFUSAL_STATUS}, got {status} with body {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, INVALID_ID,
            "case {case:?}: expected {INVALID_ID}, got {code:?} with body {body}"
        );
        let message = error_message(&body);
        let expected = format!("The ID '{value}' is not valid");
        assert_eq!(
            message, expected,
            "case {case:?}: expected the refusal {expected:?}, got {message:?}"
        );
        assert!(
            !message.contains("does not exist"),
            "case {case:?}: a value that is no id of this class is not a task that is not \
             there, got {message:?}"
        );
    }

    assert_eq!(
        stored_tasks(&engine),
        0,
        "a refused cancel stores nothing, so the account still holds no task of either family"
    );
}

/// The other half of the rule: an id whose class the parameter DOES read, which the account
/// holds no task under, is refused with that family's own code instead — so `InvalidID` is
/// the answer for a value that is no id of the class, never for a task that is missing.
#[tokio::test]
async fn an_id_of_the_class_that_names_no_task_is_refused_with_the_family_code() {
    let (router, engine) = emulator();

    for cancel in Cancel::ALL {
        for prefix in cancel.id_prefixes() {
            let value = format!("{prefix}{MINTED_SUFFIX}");
            let case = format!("{} naming the unheld {value}", cancel.action());
            assert!(
                cancel.reads(&value),
                "case {case:?}: {value} must read as an id of this parameter's class"
            );
            let (status, body) = cancel.invoke(&router, &value).await;
            assert_eq!(
                status, REFUSAL_STATUS,
                "case {case:?}: expected HTTP {REFUSAL_STATUS}, got {status} with body {body}"
            );
            let code = error_code(&body);
            assert_eq!(
                code,
                cancel.not_found_code(),
                "case {case:?}: expected {}, got {code:?} with body {body}",
                cancel.not_found_code()
            );
        }
    }

    assert_eq!(
        stored_tasks(&engine),
        0,
        "no call here starts a task, so the account still holds none"
    );
}

/// The ORDER the rule depends on: the form is read BEFORE the store. A record of the cancel's
/// own family is planted under the unreadable value — one a lookup would resolve and cancel —
/// and the answer is still the invalid-id refusal, with the planted record left exactly as it
/// was.
#[tokio::test]
async fn the_form_is_read_before_the_store_reaches_a_planted_record() {
    let (router, engine) = emulator();

    for cancel in Cancel::ALL {
        // One departure per operation is enough for the ORDER: which values leave the form is
        // the table above's rule.
        let value = format!("{}-fffffff0", cancel.family_word());
        let case = format!("{} naming the planted {value}", cancel.action());
        assert!(
            !cancel.reads(&value),
            "case {case:?}: {value} must not read as an id of this parameter's class"
        );
        plant_task(&engine, cancel, &value);
        let planted_before = planted_state(&engine, cancel, &value);
        assert!(
            planted_before.is_some(),
            "case {case:?}: the planted record must be in the store, got {planted_before:?}"
        );

        let (status, body) = cancel.invoke(&router, &value).await;
        assert_eq!(
            status, REFUSAL_STATUS,
            "case {case:?}: expected HTTP {REFUSAL_STATUS}, got {status} with body {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, INVALID_ID,
            "case {case:?}: expected {INVALID_ID}, got {code:?} with body {body}"
        );
        let message = error_message(&body);
        let expected = format!("The ID '{value}' is not valid");
        assert_eq!(
            message, expected,
            "case {case:?}: expected the refusal {expected:?}, got {message:?}"
        );
        assert_eq!(
            planted_state(&engine, cancel, &value),
            planted_before,
            "case {case:?}: the refusal must leave the planted record as it was"
        );
    }
}

/// The resolving leg, so the rule is a refusal of values that are no ids rather than of every
/// id: an id the service itself MINTED still cancels the task it names, and the response
/// reports the state it moved that task to.
#[tokio::test]
async fn an_id_the_service_minted_still_cancels_the_task_it_names() {
    let (router, engine) = emulator();

    let (status, body) = call(
        &router,
        REGION,
        "ImportImage",
        &[
            p("DiskContainer.1.Format", "VMDK"),
            p("DiskContainer.1.Url", "s3://disk-images/appliance.vmdk"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from ImportImage, got {status} with body {body}"
    );
    let minted = member(&body, "importTaskId")
        .unwrap_or_else(|| panic!("no importTaskId in {body}"));
    assert!(
        Cancel::ImportTask.reads(&minted),
        "ImportImage must mint an id of the class CancelImportTask reads, got {minted}"
    );

    let (status, body) = Cancel::ImportTask.invoke(&router, &minted).await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from CancelImportTask for {minted}, got {status} with body {body}"
    );
    assert_eq!(
        member(&body, "previousState").as_deref(),
        Some(IMPORT_TASK_STATE_ACTIVE),
        "expected the cancel to report the state the task was in, got body {body}"
    );
    assert_eq!(
        member(&body, "state").as_deref(),
        Some(IMPORT_TASK_STATE_DELETING),
        "expected the cancel to report the state it moved the task to, got body {body}"
    );

    // And the export half of the same leg, against a task seeded in the store the export
    // cancel reads ([`super::fpga_image_tasks_and_volume_io::seed_export_task`]).
    let seeded = super::fpga_image_tasks_and_volume_io::seed_export_task(
        &engine,
        EXPORT_TASK_STATE_ACTIVE,
    );
    assert!(
        Cancel::ExportTask.reads(&seeded),
        "the seeded export task must carry an id of the class CancelExportTask reads, got \
         {seeded}"
    );
    let (status, body) = Cancel::ExportTask.invoke(&router, &seeded).await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from CancelExportTask for {seeded}, got {status} with body {body}"
    );
}

/// The echo a refusal quotes a caller's value back in is BOUNDED, whatever the value's
/// length: a value far longer than any id EC2 issues is named only as far as the crate's one
/// bound on an echoed resource id, over both operations.
#[tokio::test]
async fn the_refusal_quotes_an_overlong_value_back_only_as_far_as_the_echo_bound() {
    let (router, engine) = emulator();

    for cancel in Cancel::ALL {
        // Outside every class's form (an uppercase suffix), so the row is the invalid-id
        // refusal, and far longer than the echo bound.
        let value = format!(
            "{}{}",
            cancel.family_word(),
            "A".repeat(MAX_ECHOED_RESOURCE_ID_CHARS * 3)
        );
        let case = format!("{} naming a value far past the echo bound", cancel.action());
        assert!(
            !cancel.reads(&value),
            "case {case:?}: the value must not read as an id of this parameter's class"
        );
        let (status, body) = cancel.invoke(&router, &value).await;
        assert_eq!(
            status, REFUSAL_STATUS,
            "case {case:?}: expected HTTP {REFUSAL_STATUS}, got {status} with body {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, INVALID_ID,
            "case {case:?}: expected {INVALID_ID}, got {code:?} with body {body}"
        );
        let echoed: String = value.chars().take(MAX_ECHOED_RESOURCE_ID_CHARS).collect();
        let message = error_message(&body);
        let expected = format!("The ID '{echoed}' is not valid");
        assert_eq!(
            message, expected,
            "case {case:?}: expected the refusal to name only the echoed value, got {message:?}"
        );
    }

    assert_eq!(
        stored_tasks(&engine),
        0,
        "a refused cancel stores nothing, so the account still holds no task of either family"
    );
}

/// The emulator every call here is dispatched through.
fn emulator() -> (MultiServiceRouter, std::sync::Arc<engine::InMemoryEngine>) {
    super::ec2_query::emulator()
}
