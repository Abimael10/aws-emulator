// SPDX-License-Identifier: CC0-1.0

//! How the two VM Import/Export task families word the refusal of an id, driven over the
//! wire the service actually speaks — `ec2Query`, through the crate's shared
//! [`super::ec2_query`] harness.
//!
//! The rule under test: an operation that names ONE task refuses an id it cannot resolve by
//! the ID CLASS ITS OWN PARAMETER TAKES. `CancelImportTask` takes "the ID of the import
//! image or import snapshot task to be canceled" (`CancelImportTaskRequest$ImportTaskId`),
//! so a well-formed id of that class the account holds no task for is reported as that kind
//! of task missing; `CancelConversionTask` takes the id of a conversion task, "the import of
//! an instance or volume" (`CancelConversionTask`), so it reports a conversion task missing.
//! Neither ever reports a value as a missing task of the class it did not name. The two
//! classes are disjoint, so an id of one family is a value the other's parameter cannot read
//! at all: it is refused for its FORM, before any lookup. Both causes carry the one code
//! this family answers an id it cannot resolve with, HTTP 400 `InvalidConversionTaskId`.
//!
//! Rows differ along what the rule generalises over: which operation names the id, which
//! family's class the value belongs to, which minting prefix inside that family, and how
//! long the hexadecimal suffix is. The prefixes and the kinds they name are read from the
//! crate's own [`ec2::state::IMPORT_TASK_KINDS`] and
//! [`ec2::state::CONVERSION_TASK_ID_PREFIXES`], so a kind added to either family is covered
//! here without a new literal.
//!
//! The resolving leg is asserted against ids the service really issued: a task is started
//! through the operation that starts one — `ImportImage` and `ImportSnapshot` — and the
//! cancel of the OTHER family still cannot read that id, while the cancel of its own family
//! resolves it and moves it.

use ec2::state::{
    CONVERSION_TASK_ID_PARAMETER, CONVERSION_TASK_ID_PREFIXES,
    ComAmazonawsEc2Amazonec2State as Ec2State, IMPORT_TASK_KINDS, IMPORT_TASK_STATE_ACTIVE,
    IMPORT_TASK_STATE_DELETING, INVALID_CONVERSION_TASK_ID, conversion_task_id_is_well_formed,
    import_task_id_of_any_kind_is_well_formed,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, elements, error_code, error_message, member};

/// The Region every call here is signed for. Neither kind of task carries a Region member of
/// its own, so the Region only selects the ec2Query envelope.
const REGION: &str = "ap-southeast-2";

/// One request's form parameters, as the ec2Query harness takes them.
type Params = Vec<(String, String)>;

/// One `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

fn emulator() -> (MultiServiceRouter, std::sync::Arc<engine::InMemoryEngine>) {
    super::ec2_query::emulator()
}

/// Hexadecimal suffix lengths an import task id is read in: the long form EC2 mints today,
/// the short form it minted before long ids, and a length EC2 mints none in — the length of
/// an import task id's suffix being no part of what that parameter reads.
const IMPORT_SUFFIXES: [(&str, &str); 3] = [
    ("the long (17 hex digit) form", "0123456789abcdef0"),
    ("the short (8 hex digit) form", "0123abcd"),
    ("a single digit", "f"),
];

/// The suffix lengths a conversion task id is read in: the two forms EC2 mints a resource id
/// in, which is what its parameter reads.
const CONVERSION_SUFFIXES: [(&str, &str); 2] = [
    ("the long (17 hex digit) form", "0123456789abcdef0"),
    ("the short (8 hex digit) form", "0123abcd"),
];

/// A family of VM Import/Export task: the operation that cancels one, and the class of id
/// that operation's parameter takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    /// The IMPORT image and import snapshot tasks, which `CancelImportTask` cancels.
    Import,
    /// The legacy CONVERSION tasks, which `CancelConversionTask` cancels.
    Conversion,
}

impl Family {
    /// The `Action` that cancels a task of this family.
    fn action(self) -> &'static str {
        match self {
            Self::Import => "CancelImportTask",
            Self::Conversion => "CancelConversionTask",
        }
    }

    /// The member that operation names the one task in.
    fn parameter(self) -> &'static str {
        match self {
            Self::Import => "ImportTaskId",
            Self::Conversion => "ConversionTaskId",
        }
    }

    /// The OTHER family — the one whose ids this family's parameter does not take.
    fn other(self) -> Self {
        match self {
            Self::Import => Self::Conversion,
            Self::Conversion => Self::Import,
        }
    }

    /// Whether this family's parameter reads `id` as an id of its own class, through the
    /// crate's own reader of that class's form.
    fn reads(self, id: &str) -> bool {
        match self {
            Self::Import => import_task_id_of_any_kind_is_well_formed(id),
            Self::Conversion => conversion_task_id_is_well_formed(id),
        }
    }

    /// The sentence this family refuses a value its parameter cannot read as an id of its
    /// class with: the import tasks quote the value back, the conversion tasks name the
    /// parameter beside it.
    fn unreadable_message(self, id: &str) -> String {
        match self {
            Self::Import => format!("Invalid id: \"{id}\""),
            Self::Conversion => {
                format!("Parameter {CONVERSION_TASK_ID_PARAMETER} ({id}) has an invalid format.")
            }
        }
    }

    /// How a refusal naming a task of this family spells the CLASS of task it names: the
    /// modelled resource type of each import task kind, or the conversion task the legacy
    /// family's sentence names. What no refusal of the other family may carry.
    fn class_words(self) -> Vec<String> {
        match self {
            Self::Import => IMPORT_TASK_KINDS
                .iter()
                .map(|(_, resource_type)| format!("{resource_type} ID"))
                .collect(),
            Self::Conversion => vec!["conversion task".to_string()],
        }
    }
}

/// One `CancelImportTask` or `CancelConversionTask` call naming `task_id` and nothing else.
async fn cancel(router: &MultiServiceRouter, family: Family, task_id: &str) -> (u16, String) {
    call(
        router,
        REGION,
        family.action(),
        &[p(family.parameter(), task_id)],
    )
    .await
}

/// One id of a task class, and the sentence the family that class belongs to words a task it
/// holds none of under that id.
#[derive(Debug, Clone)]
struct Row {
    /// The family whose parameter takes an id of this class.
    family: Family,
    /// What this row varies, for an assertion to name.
    case: String,
    /// The id itself.
    id: String,
    /// The sentence `family` answers this id with when the account holds no such task.
    not_found: String,
}

/// Every id class under test: each family's minting prefixes crossed with the suffix lengths
/// its parameter reads, generated from the crate's own prefix tables.
fn class_rows() -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    for (prefix, resource_type) in IMPORT_TASK_KINDS {
        for (length, suffix) in IMPORT_SUFFIXES {
            let id = format!("{prefix}{suffix}");
            rows.push(Row {
                family: Family::Import,
                case: format!("an {prefix} id in {length}"),
                not_found: format!("The {resource_type} ID '{id}' does not exist"),
                id,
            });
        }
    }
    for prefix in CONVERSION_TASK_ID_PREFIXES {
        for (length, suffix) in CONVERSION_SUFFIXES {
            let id = format!("{prefix}-{suffix}");
            rows.push(Row {
                family: Family::Conversion,
                case: format!("an {prefix}- id in {length}"),
                not_found: format!("The conversion task '{id}' does not exist"),
                id,
            });
        }
    }
    rows
}

/// How many tasks of either family the store holds, over every store a cancel of either
/// could have written to.
fn stored_tasks(engine: &engine::InMemoryEngine) -> usize {
    let store = engine.state::<Ec2State>();
    let guard = store.lock();
    guard.import_image_tasks.len() + guard.import_snapshot_tasks.len() + guard.conversion_tasks.len()
}

/// Each family's own class: a well-formed id of it that names no task is reported as a
/// missing task of THAT class, worded for the kind the id names and never for the other
/// family's class — the two families' sentences do not cross.
#[tokio::test]
async fn a_cancel_names_a_missing_task_by_the_id_class_its_own_parameter_takes() {
    let (router, engine) = emulator();

    for row in class_rows() {
        let case = &row.case;
        assert!(
            row.family.reads(&row.id),
            "case {case:?}: {} must read {} as an id of its own class",
            row.family.action(),
            row.id
        );
        assert!(
            !row.family.other().reads(&row.id),
            "case {case:?}: the two task classes are disjoint, so {} must not read {}",
            row.family.other().action(),
            row.id
        );

        let (status, body) = cancel(&router, row.family, &row.id).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            INVALID_CONVERSION_TASK_ID,
            "case {case:?}: expected {INVALID_CONVERSION_TASK_ID}, got body {body}"
        );
        let message = error_message(&body);
        assert_eq!(
            message, row.not_found,
            "case {case:?}: expected the refusal {:?}, got {message:?}",
            row.not_found
        );
        for words in row.family.other().class_words() {
            assert!(
                !message.contains(&words),
                "case {case:?}: the refusal must not name the {words:?} class, got {message:?}"
            );
        }
    }

    assert_eq!(
        stored_tasks(&engine),
        0,
        "no call here starts an import, so the account still holds no task of either family"
    );
}

/// The other half of the same rule: an id of the OTHER family's class is a value this
/// parameter cannot read, so it is refused for its FORM — never as a missing task of the
/// class the operation names, and never worded for the class the value belongs to.
#[tokio::test]
async fn a_cancel_refuses_an_id_of_the_other_task_family_as_a_value_it_cannot_read() {
    let (router, engine) = emulator();

    for row in class_rows() {
        let caller = row.family.other();
        let case = format!("{} naming {}", caller.action(), row.case);
        let (status, body) = cancel(&router, caller, &row.id).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            INVALID_CONVERSION_TASK_ID,
            "case {case:?}: expected {INVALID_CONVERSION_TASK_ID}, got body {body}"
        );
        let message = error_message(&body);
        let expected = caller.unreadable_message(&row.id);
        assert_eq!(
            message, expected,
            "case {case:?}: expected the format refusal {expected:?}, got {message:?}"
        );
        assert!(
            !message.contains("does not exist"),
            "case {case:?}: a value this parameter cannot read is not a task that is not \
             there, got {message:?}"
        );
        for words in row.family.class_words() {
            assert!(
                !message.contains(&words),
                "case {case:?}: the refusal must not name the {words:?} class, got {message:?}"
            );
        }
    }

    assert_eq!(
        stored_tasks(&engine),
        0,
        "a refused cancel stores nothing, so the account still holds no task of either family"
    );
}

/// The resolving leg, over ids the service really ISSUED: an import task id is read only by
/// the family that minted it — the conversion cancel cannot read it and leaves it alone,
/// while the cancel of its own family resolves it and moves it out of `active`.
#[tokio::test]
async fn an_id_the_service_minted_is_read_only_by_the_family_that_minted_it() {
    let (router, engine) = emulator();

    // `(case, the Action that starts the task, its disk container fields)`: one task of each
    // import kind, started through the operation that starts it.
    let starts: Vec<(&str, &str, Params)> = vec![
        (
            "an import image task",
            "ImportImage",
            vec![
                p("DiskContainer.1.Format", "VMDK"),
                p("DiskContainer.1.Url", "s3://disk-images/appliance.vmdk"),
            ],
        ),
        (
            "an import snapshot task",
            "ImportSnapshot",
            vec![
                p("DiskContainer.Format", "RAW"),
                p("DiskContainer.UserBucket.S3Bucket", "disk-images"),
                p("DiskContainer.UserBucket.S3Key", "golden/volume.raw"),
            ],
        ),
    ];

    for (case, action, container) in starts {
        let (status, body) = call(&router, REGION, action, &container).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
        );
        let minted = elements(&body, "importTaskId")
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("case {case:?}: no importTaskId in {body}"));
        assert!(
            Family::Import.reads(&minted),
            "case {case:?}: {action} must mint an id of the import task class, got {minted}"
        );
        assert!(
            !Family::Conversion.reads(&minted),
            "case {case:?}: {minted} is no conversion task id"
        );

        // The conversion cancel cannot read a minted import task id at all, so it reports
        // the value rather than a conversion task that is not there.
        let (status, body) = cancel(&router, Family::Conversion, &minted).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 from {}, got {status} with body {body}",
            Family::Conversion.action()
        );
        assert_eq!(
            error_code(&body),
            INVALID_CONVERSION_TASK_ID,
            "case {case:?}: expected {INVALID_CONVERSION_TASK_ID}, got body {body}"
        );
        let message = error_message(&body);
        let expected = Family::Conversion.unreadable_message(&minted);
        assert_eq!(
            message, expected,
            "case {case:?}: expected the format refusal {expected:?}, got {message:?}"
        );

        // And it left the task alone: the cancel of its own family still finds it `active`.
        let (status, body) = cancel(&router, Family::Import, &minted).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from {}, got {status} with body {body}",
            Family::Import.action()
        );
        assert_eq!(
            member(&body, "previousState").as_deref(),
            Some(IMPORT_TASK_STATE_ACTIVE),
            "case {case:?}: expected the cancel to report the state the task was in, got \
             body {body}"
        );
        assert_eq!(
            member(&body, "state").as_deref(),
            Some(IMPORT_TASK_STATE_DELETING),
            "case {case:?}: expected the cancel to report the state it moved the task to, \
             got body {body}"
        );
    }

    assert_eq!(
        stored_tasks(&engine),
        2,
        "each start stores the one import task it minted, and no conversion task was stored"
    );
}
