// SPDX-License-Identifier: CC0-1.0

//! How EVERY operation that names an import task answers an id, driven over the wire the
//! service actually speaks — `ec2Query`, through the crate's shared [`super::ec2_query`]
//! harness.
//!
//! The rule under test: an operation that names an import task refuses an id it cannot
//! resolve to a task with HTTP 400, by the half of the rule the value falls in. An id whose
//! KIND the operation reads that the account holds no such task under is refused
//! `InvalidConversionTaskId`, the import (conversion) task family's own code, with the
//! refusal spelling that kind: the family's code reports a task of this family that is
//! MISSING. A value that names no kind the operation reads at all is no id of this family,
//! so it is refused for its FORM, before the store is reached, by the refusal THAT
//! operation's validator words — the single-task `CancelImportTask` answers EC2's
//! resource-agnostic `InvalidID` / "The ID '<value>' is not valid", as its recorded refusal
//! carries, while the describes that select rows by `ImportTaskIds` answer the family code
//! with the service's unreadable-id sentence. The one further qualification is a describe's
//! list semantics: it reports "a list of zero or more" tasks, so an id of the kind THAT
//! describe reports which names no task selects no row rather than failing the call.
//!
//! The kind of an import task id is the one its PREFIX names, followed by a hexadecimal
//! suffix of any length: the length of the suffix is no part of what the parameter admits.
//!
//! Rows differ along what the rule generalises over: which operation names the id, which
//! kind the value names, how a value departs from naming one (an uppercase suffix, a digit
//! outside hexadecimal, an empty suffix, an unknown kind segment, another resource's id, a
//! value of no resource-id shape), and how long the hexadecimal suffix is — the lengths EC2
//! mints and lengths it mints none in. The prefixes and the kinds they name are read from
//! the crate's own [`ec2::state::IMPORT_TASK_KINDS`], so a kind added there is covered here
//! without a new literal.
//!
//! The resolving leg is asserted against ids the service really issued: a task of each kind
//! is started through the operation that starts it — `ImportImage` and `ImportSnapshot` —
//! then read back through its describe and cancelled through `CancelImportTask`.

use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, IMPORT_IMAGE_TASK_ID_PREFIX_WITH_HYPHEN,
    IMPORT_SNAPSHOT_TASK_ID_PREFIX_WITH_HYPHEN, IMPORT_TASK_KINDS, IMPORT_TASK_STATE_ACTIVE,
    IMPORT_TASK_STATE_DELETING, INVALID_CONVERSION_TASK_ID, MAX_ECHOED_RESOURCE_ID_CHARS,
    import_task_id_is_well_formed, import_task_id_of_any_kind_is_well_formed,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements, error_code, member, unescaped};

/// The Region every call here is signed for. An import task carries no Region member, so
/// the Region only selects the ec2Query envelope.
const REGION: &str = "eu-west-1";

/// What a value the single-task cancel cannot read as an import task id of either kind is
/// refused with: EC2's resource-agnostic code for an id of no kind the operation resolves,
/// as that operation's recorded refusal carries it.
const INVALID_ID: &str = "InvalidID";

/// One request's form parameters, as the ec2Query harness takes them.
type Params = Vec<(String, String)>;

/// One `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

fn emulator() -> (MultiServiceRouter, std::sync::Arc<engine::InMemoryEngine>) {
    super::ec2_query::emulator()
}

/// The hexadecimal suffix lengths under test: the long form EC2 mints today, the short form
/// it minted before long ids, and lengths either side of both — which this parameter admits
/// too, the suffix's length being no part of what it reads.
const SUFFIX_LENGTHS: [(&str, &str); 5] = [
    ("the long (17 hex digit) form", "0123456789abcdef0"),
    ("the short (8 hex digit) form", "0123abcd"),
    ("a digit short of the short form", "0123abc"),
    ("a digit past the long form", "0123456789abcdef01"),
    ("a single digit", "f"),
];

/// An operation that names ONE import task id, and how it names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Caller {
    /// `CancelImportTask`, which acts on the one task it names, of either kind.
    Cancel,
    /// `DescribeImportImageTasks`, which reports the import IMAGE tasks it names.
    DescribeImageTasks,
    /// `DescribeImportSnapshotTasks`, which reports the import SNAPSHOT tasks it names.
    DescribeSnapshotTasks,
}

impl Caller {
    /// Every operation the rule covers.
    const ALL: [Self; 3] = [
        Self::Cancel,
        Self::DescribeImageTasks,
        Self::DescribeSnapshotTasks,
    ];

    /// The `Action` this operation is called under.
    fn action(self) -> &'static str {
        match self {
            Self::Cancel => "CancelImportTask",
            Self::DescribeImageTasks => "DescribeImportImageTasks",
            Self::DescribeSnapshotTasks => "DescribeImportSnapshotTasks",
        }
    }

    /// The form fields that name `import_task_id` to this operation: the cancel names one
    /// task in a scalar member, a describe names them in an `ImportTaskId.N` list.
    fn params(self, import_task_id: &str) -> Params {
        match self {
            Self::Cancel => vec![p("ImportTaskId", import_task_id)],
            Self::DescribeImageTasks | Self::DescribeSnapshotTasks => {
                vec![p("ImportTaskId.1", import_task_id)]
            }
        }
    }

    /// The minting prefix of the task kind this operation REPORTS, or `None` for the cancel,
    /// which acts on a task of either kind.
    fn reported_prefix(self) -> Option<&'static str> {
        match self {
            Self::Cancel => None,
            Self::DescribeImageTasks => Some(IMPORT_IMAGE_TASK_ID_PREFIX_WITH_HYPHEN),
            Self::DescribeSnapshotTasks => Some(IMPORT_SNAPSHOT_TASK_ID_PREFIX_WITH_HYPHEN),
        }
    }

    /// Whether this operation can read `import_task_id` as an id of a task it reports: a
    /// describe reads only its own kind's ids, the cancel reads either kind's.
    fn can_read(self, import_task_id: &str) -> bool {
        match self.reported_prefix() {
            Some(prefix) => import_task_id_is_well_formed(prefix, import_task_id),
            None => import_task_id_of_any_kind_is_well_formed(import_task_id),
        }
    }

    /// The `(code, message)` this operation refuses a value it cannot read as an id of a
    /// task it names with — the half of the rule that is a fact of the OPERATION: the cancel
    /// of the one task it names reports the value as no id it resolves, while a describe
    /// answers the task family's own code with the service's unreadable-id sentence.
    fn unreadable_refusal(self, import_task_id: &str) -> (&'static str, String) {
        match self {
            Self::Cancel => (
                INVALID_ID,
                format!("The ID '{import_task_id}' is not valid"),
            ),
            Self::DescribeImageTasks | Self::DescribeSnapshotTasks => (
                INVALID_CONVERSION_TASK_ID,
                format!("Invalid id: \"{import_task_id}\""),
            ),
        }
    }
}

/// One call of `caller` naming `import_task_id` and nothing else.
async fn name_task(
    router: &MultiServiceRouter,
    caller: Caller,
    import_task_id: &str,
) -> (u16, String) {
    call(
        router,
        REGION,
        caller.action(),
        &caller.params(import_task_id),
    )
    .await
}

/// Values that name NO kind of import task: each row departs in one way from an id of a
/// kind, generated over EVERY minting prefix the crate declares so no kind's reading is
/// asserted only through another's, plus the values that carry no kind segment at all.
fn unreadable_ids() -> Vec<(String, String)> {
    let departures = [
        ("an uppercase suffix", "0123ABCD"),
        ("a suffix outside hexadecimal", "0123abcz"),
        ("a long suffix outside hexadecimal", "0123456789abcdefz"),
        ("an empty suffix", ""),
    ];
    let mut rows: Vec<(String, String)> = Vec::new();
    for (prefix, _) in IMPORT_TASK_KINDS {
        for (case, suffix) in departures {
            rows.push((
                format!("{prefix} with {case}"),
                format!("{prefix}{suffix}"),
            ));
        }
    }
    for (case, id) in [
        ("an import id with no kind segment", "import-0123abcd"),
        ("a kind EC2 starts no import task under", "import-disk-0123abcd"),
        ("another resource's id", "snap-0123456789abcdef0"),
        ("a value of no resource-id shape", "the-last-import"),
    ] {
        rows.push((case.to_string(), id.to_string()));
    }
    rows
}

/// Ids whose kind EC2 reads, over every kind crossed with every hexadecimal suffix length
/// under test — the lengths EC2 mints and lengths it mints none in, because the length of
/// the suffix is no part of what the parameter admits. Each row carries the modelled
/// resource type of the kind a refusal naming it must spell.
fn readable_ids() -> Vec<(String, String, &'static str)> {
    let mut rows: Vec<(String, String, &'static str)> = Vec::new();
    for (prefix, resource_type) in IMPORT_TASK_KINDS {
        for (case, suffix) in SUFFIX_LENGTHS {
            rows.push((
                format!("{prefix} in {case}"),
                format!("{prefix}{suffix}"),
                resource_type,
            ));
        }
    }
    rows
}

/// How many import tasks of both kinds the store holds.
fn stored_tasks(engine: &engine::InMemoryEngine) -> usize {
    let store = engine.state::<Ec2State>();
    let guard = store.lock();
    guard.import_image_tasks.len() + guard.import_snapshot_tasks.len()
}

/// Every `importTaskId` a response reports.
fn reported_ids(body: &str) -> Vec<String> {
    elements(body, "importTaskId")
}

/// The rule, over every operation that names an import task crossed with every way an id
/// can fail to name a task of the account: an id whose kind the operation reads that names
/// no task is HTTP 400 `InvalidConversionTaskId` worded as the task of that kind that is not
/// there, while a value whose kind it cannot read is refused for its form — the cancel's
/// `InvalidID` or a describe's family-coded unreadable-id sentence — except where a describe
/// answers an id of its own kind with no row at all.
#[tokio::test]
async fn an_import_task_id_that_names_no_task_is_answered_the_same_way_everywhere() {
    let (router, engine) = emulator();

    let mut rows: Vec<(String, String, Option<&'static str>)> = Vec::new();
    for (case, id) in unreadable_ids() {
        assert!(
            !import_task_id_of_any_kind_is_well_formed(&id),
            "case {case:?}: {id} must name no kind of import task"
        );
        rows.push((case, id, None));
    }
    for (case, id, resource_type) in readable_ids() {
        assert!(
            import_task_id_of_any_kind_is_well_formed(&id),
            "case {case:?}: {id} must name a kind of import task"
        );
        rows.push((case, id, Some(resource_type)));
    }

    for (row, import_task_id, resource_type) in &rows {
        for caller in Caller::ALL {
            let case = format!("{} naming {row}", caller.action());
            let (status, body) = name_task(&router, caller, import_task_id).await;
            // A describe answers "a list of zero or more" tasks, so an id of its own kind
            // that names none selects no row instead of failing the call.
            if caller.reported_prefix().is_some() && caller.can_read(import_task_id) {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected HTTP 200, got {status} with body {body}"
                );
                let found = reported_ids(&body);
                assert!(
                    found.is_empty(),
                    "case {case:?}: expected no task reported, got {found:?}"
                );
                continue;
            }
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} with body {body}"
            );
            // The two causes are worded — and coded — apart: an id whose kind the operation
            // reads that names no task is reported as the task of that kind that is not
            // there, under the family's own code, while a value whose kind it cannot read is
            // refused for its form by the refusal that operation words. The XML escapes the
            // quotes around the id, so the texts are compared.
            let message = element(&body, "Message").map(|message| unescaped(&message));
            let (expected_code, expected) =
                match resource_type.filter(|_| caller.can_read(import_task_id)) {
                    Some(resource_type) => (
                        INVALID_CONVERSION_TASK_ID,
                        format!("The {resource_type} ID '{import_task_id}' does not exist"),
                    ),
                    None => caller.unreadable_refusal(import_task_id),
                };
            assert_eq!(
                error_code(&body),
                expected_code,
                "case {case:?}: expected {expected_code}, got body {body}"
            );
            assert_eq!(
                message,
                Some(expected.clone()),
                "case {case:?}: expected the message {expected:?}, got {message:?}"
            );
        }
    }

    assert_eq!(
        stored_tasks(&engine),
        0,
        "no call here starts an import, so the account still holds no import task"
    );
}

/// The suffix's length being no part of what the parameter reads, what bounds a refusal is
/// the echo: an id longer than the service quotes a caller's value back is named only as far
/// as that bound, over every kind.
#[tokio::test]
async fn a_long_import_task_id_is_echoed_only_as_far_as_a_refusal_quotes_one() {
    let (router, engine) = emulator();
    for (prefix, resource_type) in IMPORT_TASK_KINDS {
        let case = format!("a {prefix} id far longer than the echo bound");
        let import_task_id = format!("{prefix}{}", "f".repeat(MAX_ECHOED_RESOURCE_ID_CHARS * 3));
        let (status, body) = name_task(&router, Caller::Cancel, &import_task_id).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            INVALID_CONVERSION_TASK_ID,
            "case {case:?}: expected {INVALID_CONVERSION_TASK_ID}, got body {body}"
        );
        let echoed: String = import_task_id
            .chars()
            .take(MAX_ECHOED_RESOURCE_ID_CHARS)
            .collect();
        let message = element(&body, "Message").map(|message| unescaped(&message));
        assert_eq!(
            message,
            Some(format!(
                "The {resource_type} ID '{echoed}' does not exist"
            )),
            "case {case:?}: expected the message to name only the echoed id, got {message:?}"
        );
    }

    assert_eq!(
        stored_tasks(&engine),
        0,
        "a refused cancel stores nothing, so the account still holds no import task"
    );
}

/// The resolving leg of the same rule: an id the service MINTED is read by every operation
/// that names it — the describe of its kind reports exactly that task, and the cancel moves
/// it — so the refusals above are about ids that name no task rather than about any id of a
/// task the account holds.
#[tokio::test]
async fn an_id_the_service_minted_resolves_through_every_operation_that_names_it() {
    let (router, engine) = emulator();

    // `(case, the Action that starts the task, its disk container fields, the describe that
    // reports it)`: one task of each kind, started through the operation that starts it.
    let starts: Vec<(&str, &str, Params, Caller)> = vec![
        (
            "an import image task",
            "ImportImage",
            vec![
                p("DiskContainer.1.Format", "VMDK"),
                p("DiskContainer.1.Url", "s3://disk-images/appliance.vmdk"),
            ],
            Caller::DescribeImageTasks,
        ),
        (
            "an import snapshot task",
            "ImportSnapshot",
            vec![
                p("DiskContainer.Format", "RAW"),
                p("DiskContainer.UserBucket.S3Bucket", "disk-images"),
                p("DiskContainer.UserBucket.S3Key", "golden/volume.raw"),
            ],
            Caller::DescribeSnapshotTasks,
        ),
    ];

    for (case, action, container, describe) in starts {
        let (status, body) = call(&router, REGION, action, &container).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
        );
        let minted = reported_ids(&body)
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("case {case:?}: no importTaskId in {body}"));
        assert!(
            describe.can_read(&minted),
            "case {case:?}: {action} must mint an id its describe can read, got {minted}"
        );

        let (status, body) = name_task(&router, describe, &minted).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from {}, got {status} with body {body}",
            describe.action()
        );
        assert_eq!(
            reported_ids(&body),
            vec![minted.clone()],
            "case {case:?}: expected the describe to report only {minted}, got body {body}"
        );

        let (status, body) = name_task(&router, Caller::Cancel, &minted).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CancelImportTask, got {status} with body \
             {body}"
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

        // The same id still resolves after the cancel: it names the task the describe
        // reports, now working through its cancellation.
        let (status, body) = name_task(&router, describe, &minted).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from the describe after the cancel, got \
             {status} with body {body}"
        );
        assert_eq!(
            reported_ids(&body),
            vec![minted.clone()],
            "case {case:?}: expected {minted} still reported after the cancel, got body \
             {body}"
        );
    }

    assert_eq!(
        stored_tasks(&engine),
        2,
        "each start stores the one task it minted"
    );
}
