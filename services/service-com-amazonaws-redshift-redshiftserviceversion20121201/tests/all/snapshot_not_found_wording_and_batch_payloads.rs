// SPDX-License-Identifier: CC0-1.0

//! What a cluster-snapshot operation says when it cannot find the snapshot the
//! request names, end to end over the awsQuery wire — as a refusal of a
//! one-snapshot call, and as the per-entry failure of a batch.
//!
//! Every case drives a real HTTP request through the same `MultiServiceRouter`
//! the production router builds (the harness is shared with
//! [`crate::cluster_lifecycle`]), so each assertion covers the whole walk:
//! form-urlencoded deserialize -> handler -> XML serialize.
//!
//! The RULES asserted here, each over a table rather than one request:
//!
//! * `ClusterSnapshotNotFoundFault` has one status (404) and one `awsQueryError`
//!   code (`ClusterSnapshotNotFound`), which the model fixes, but the fixed text
//!   of its MESSAGE is an attribute of the OPERATION — three sentences are in
//!   use across this surface — and the identifier the request named is its only
//!   variable part, whatever that identifier looks like.
//! * A batch snapshot operation reports what ONE entry got wrong as that entry's
//!   own `SnapshotErrorMessage`, whose `FailureCode` names the SHAPE of the fault
//!   the one-snapshot operation raises for the same condition (not the shorter
//!   `awsQueryError` code, which belongs to a refusal envelope and not to a value
//!   in a 200 body).
//! * `BatchDeleteClusterSnapshots` reports back the whole LOCATOR each failed
//!   entry sent — its `SnapshotIdentifier` and its `SnapshotClusterIdentifier` —
//!   so the modelled member is present on every entry, carrying no value when the
//!   entry named no cluster.
//! * `BatchModifyClusterSnapshots` lists what it acted on in `Resources` in
//!   request order and what it could not act on in `Errors` in the REVERSE of
//!   that order, the last identifier the request names first, each entry carrying
//!   `ModifyClusterSnapshot`'s own sentence as its `FailureReason`.
//!
//! The expected sentences, the failure codes and the batch limits are written out
//! literally here, so a change to the fixed text fails a test rather than passing
//! one computed from the same code under test.

use service_com_amazonaws_redshift_redshiftserviceversion20121201::state;

use crate::cluster_lifecycle::{all_text, element_blocks, Answer, Emulator};
use crate::copy_cluster_snapshot::seed_automated;

/// The account a snapshot-access request names, in the 12-digit form
/// `AccountWithRestoreAccess` takes.
const GRANTEE_ACCOUNT: &str = "123456789012";

/// The `SnapshotErrorMessage$FailureCode` of an entry naming no stored snapshot:
/// the name of the fault shape the one-snapshot path raises
/// (`smithy_query(query="shape", shape="ClusterSnapshotNotFoundFault")`).
const FAILURE_CODE_NOT_FOUND: &str = "ClusterSnapshotNotFoundFault";

/// Which sentence an operation words `ClusterSnapshotNotFoundFault` with.
///
/// Written out in this suite rather than read from the code under test, so a
/// change to any of the three fixed texts fails a case here.
#[derive(Debug, Clone, Copy)]
enum Wording {
    /// `ClusterSnapshot not found: <identifier>`
    ClusterSnapshotNotFoundThenIdentifier,
    /// `Snapshot <identifier> not found.`
    SnapshotThenNotFound,
    /// `Snapshot not found: <identifier>`
    SnapshotNotFoundThenIdentifier,
}

/// The sentence `wording` words for `identifier`.
fn expected_message(wording: Wording, identifier: &str) -> String {
    match wording {
        Wording::ClusterSnapshotNotFoundThenIdentifier => {
            format!("ClusterSnapshot not found: {identifier}")
        }
        Wording::SnapshotThenNotFound => format!("Snapshot {identifier} not found."),
        Wording::SnapshotNotFoundThenIdentifier => format!("Snapshot not found: {identifier}"),
    }
}

/// One row of the wording table: the operation, the input member its snapshot
/// identifier arrives on, the other members its model requires, and the sentence
/// it words the fault with.
type WordingCase = (
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
    Wording,
);

/// One `SnapshotErrorMessage` as a served body carries it.
///
/// Each member is `None` only when its element is ABSENT from the entry; an
/// element present with no text reads as `Some("")`, which is the difference an
/// awsQuery consumer expecting every modelled member sees.
#[derive(Debug, PartialEq, Eq)]
struct ReportedError {
    snapshot_identifier: Option<String>,
    snapshot_cluster_identifier: Option<String>,
    failure_code: Option<String>,
    failure_reason: Option<String>,
}

/// The `Errors` entries of a batch answer, in the order the body lists them.
fn reported_errors(answer: &Answer) -> Vec<ReportedError> {
    element_blocks(&answer.body, "SnapshotErrorMessage")
        .iter()
        .map(|block| ReportedError {
            snapshot_identifier: member(block, "SnapshotIdentifier"),
            snapshot_cluster_identifier: member(block, "SnapshotClusterIdentifier"),
            failure_code: member(block, "FailureCode"),
            failure_reason: member(block, "FailureReason"),
        })
        .collect()
}

/// The text of the `name` element of one entry: `None` when the element is
/// absent, `Some("")` when it is present with no value.
///
/// A scanner over this one wire format for the same reason
/// [`crate::cluster_lifecycle::all_text`] is one — and unlike that helper it has
/// to tell an EMPTY element from a missing one, in either spelling a serializer
/// may choose (`<X></X>` or `<X/>`). An element name is matched whole, so
/// `SnapshotIdentifier` cannot read `SnapshotClusterIdentifier`.
fn member(block: &str, name: &str) -> Option<String> {
    let open = format!("<{name}");
    let close = format!("</{name}>");
    let mut rest = block;
    while let Some(start) = rest.find(&open) {
        let after = &rest[start + open.len()..];
        let open_end = after.find('>')?;
        let attributes = &after[..open_end];
        if !(attributes.is_empty() || attributes.starts_with(' ') || attributes == "/") {
            // A longer element name that merely starts the same way.
            rest = after;
            continue;
        }
        if attributes.ends_with('/') {
            return Some(String::new());
        }
        let inner = &after[open_end + 1..];
        let end = inner.find(&close)?;
        return Some(inner[..end].to_string());
    }
    None
}

/// The `Resources` entries of a batch answer, in the order the body lists them.
///
/// `SnapshotIdentifierList`'s member is serialized as `<String>`, and the only
/// `SnapshotIdentifierList` in either batch output is `Resources`.
fn resources(answer: &Answer) -> Vec<String> {
    all_text(&answer.body, "String")
}

/// `BatchModifyClusterSnapshots` naming `identifiers`, with the extra parameters
/// `extra`.
async fn batch_modify(
    emulator: &Emulator,
    identifiers: &[&str],
    extra: &[(&str, &str)],
) -> Answer {
    // An awsQuery list member is sent as `<List>.<member xmlName>.<index>`, and
    // `SnapshotIdentifierList`'s member carries the xmlName `String`
    // (`smithy_query(query="shape", shape="SnapshotIdentifierList")`).
    let mut params: Vec<(String, String)> = identifiers
        .iter()
        .enumerate()
        .map(|(position, identifier)| {
            (
                format!("SnapshotIdentifierList.String.{}", position + 1),
                (*identifier).to_string(),
            )
        })
        .collect();
    params.extend(
        extra
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string())),
    );
    let borrowed: Vec<(&str, &str)> = params
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    emulator
        .call("BatchModifyClusterSnapshots", &borrowed)
        .await
}

/// `BatchDeleteClusterSnapshots` naming each `(SnapshotIdentifier,
/// SnapshotClusterIdentifier)` entry in order.
async fn batch_delete(emulator: &Emulator, entries: &[(&str, Option<&str>)]) -> Answer {
    // `DeleteClusterSnapshotMessageList`'s member carries the xmlName
    // `DeleteClusterSnapshotMessage`
    // (`smithy_query(query="shape", shape="DeleteClusterSnapshotMessageList")`).
    let mut params: Vec<(String, String)> = Vec::new();
    for (position, (identifier, cluster)) in entries.iter().enumerate() {
        let prefix = format!("Identifiers.DeleteClusterSnapshotMessage.{}", position + 1);
        params.push((
            format!("{prefix}.SnapshotIdentifier"),
            (*identifier).to_string(),
        ));
        if let Some(cluster) = cluster {
            params.push((
                format!("{prefix}.SnapshotClusterIdentifier"),
                (*cluster).to_string(),
            ));
        }
    }
    let borrowed: Vec<(&str, &str)> = params
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    emulator
        .call("BatchDeleteClusterSnapshots", &borrowed)
        .await
}

/// Every operation that answers `ClusterSnapshotNotFoundFault` answers it in ITS
/// OWN words, for every form of identifier.
#[tokio::test]
async fn the_snapshot_not_found_refusal_is_worded_per_operation() {
    let emulator = Emulator::new();

    // The operations, each paired with the sentence recorded for it. Three
    // sentences are in use, so the table is what shows the wording to be an
    // attribute of the operation rather than of the service or of the fault.
    let operations: [WordingCase; 8] = [
        (
            "DeleteClusterSnapshot",
            "SnapshotIdentifier",
            &[],
            Wording::ClusterSnapshotNotFoundThenIdentifier,
        ),
        (
            "ModifyClusterSnapshot",
            "SnapshotIdentifier",
            &[("ManualSnapshotRetentionPeriod", "7")],
            Wording::ClusterSnapshotNotFoundThenIdentifier,
        ),
        (
            "CopyClusterSnapshot",
            "SourceSnapshotIdentifier",
            &[("TargetSnapshotIdentifier", "wording-copy-target")],
            Wording::ClusterSnapshotNotFoundThenIdentifier,
        ),
        (
            "AuthorizeSnapshotAccess",
            "SnapshotIdentifier",
            &[("AccountWithRestoreAccess", GRANTEE_ACCOUNT)],
            Wording::SnapshotThenNotFound,
        ),
        (
            "RevokeSnapshotAccess",
            "SnapshotIdentifier",
            &[("AccountWithRestoreAccess", GRANTEE_ACCOUNT)],
            Wording::SnapshotThenNotFound,
        ),
        (
            "DescribeClusterSnapshots",
            "SnapshotIdentifier",
            &[],
            Wording::SnapshotThenNotFound,
        ),
        (
            "RestoreFromClusterSnapshot",
            "SnapshotIdentifier",
            &[("ClusterIdentifier", "wording-restored-cluster")],
            Wording::SnapshotNotFoundThenIdentifier,
        ),
        (
            "GetReservedNodeExchangeConfigurationOptions",
            "SnapshotIdentifier",
            &[("ActionType", state::EXCHANGE_ACTION_RESTORE_CLUSTER)],
            Wording::SnapshotNotFoundThenIdentifier,
        ),
    ];

    // The identifier rows differ along the FORM and the LENGTH of the name the
    // request names, which no sentence depends on.
    let longest = "s".repeat(255);
    let identifiers: [(&str, &str); 4] = [
        ("a plain name", "wording-absent-snapshot"),
        ("a single character", "s"),
        ("a name carrying digits and dashes", "snap-2024-07-01-0001"),
        ("a name of the longest form the service admits", &longest),
    ];

    for (action, member_name, extra, wording) in operations {
        for (form, identifier) in identifiers {
            let case = format!("{action} naming {form}");
            let mut params: Vec<(&str, &str)> = vec![(member_name, identifier)];
            params.extend_from_slice(extra);
            let answer = emulator.call(action, &params).await;
            assert_eq!(
                (answer.status, answer.error_code().as_deref()),
                (404, Some("ClusterSnapshotNotFound")),
                "case {case}: expected 404 ClusterSnapshotNotFound, got {} / {:?} with body {}",
                answer.status,
                answer.error_code(),
                answer.body
            );
            let expected = expected_message(wording, identifier);
            assert_eq!(
                answer.text("Message").as_deref(),
                Some(expected.as_str()),
                "case {case}: expected the refusal {expected:?}, got {:?}",
                answer.text("Message")
            );
        }
    }
}

/// Every per-entry failure of a batch delete reports the whole locator its entry
/// sent, so the modelled `SnapshotClusterIdentifier` is present even when the
/// entry named no cluster.
#[tokio::test]
async fn a_batch_delete_failure_reports_the_locator_its_entry_sent() {
    let emulator = Emulator::new();
    let cluster = "locator-source-cluster";
    let created = emulator.create_minimal(cluster).await;
    assert_eq!(
        created.status, 200,
        "case seed cluster {cluster}: expected 200, got {} with body {}",
        created.status, created.body
    );
    // A snapshot Amazon Redshift took on its own schedule is one no delete
    // admits, so its entry is a failure that DID locate a snapshot — written
    // straight into the shared store, which no implemented operation mints.
    seed_automated(&emulator, cluster, "locator-automated", state::STATUS_AVAILABLE);
    emulator.advance_clock(state::PROVISIONING_SPAN);

    // The rows differ along what the entry's locator carries: an identifier
    // alone, an identifier paired with a cluster, no identifier at all, and an
    // identifier that resolves to a snapshot the delete rule does not admit.
    let entries: [(&str, Option<&str>); 4] = [
        ("no-such-snapshot", None),
        ("another-absent-snapshot", Some(cluster)),
        ("", None),
        ("locator-automated", None),
    ];
    let answer = batch_delete(&emulator, &entries).await;
    assert_eq!(
        answer.status, 200,
        "case a list of entries that all fail: expected 200, got {} with body {}",
        answer.status, answer.body
    );
    assert_eq!(
        resources(&answer),
        Vec::<String>::new(),
        "case a list of entries that all fail: expected nothing reported as deleted, got {:?}",
        resources(&answer)
    );

    // (the entry, the identifier reported back, the cluster reported back, the
    // failure code) — the cluster is the one the ENTRY named, and the one the
    // located snapshot was taken of where the entry named none.
    let expected: [(&str, &str, &str, &str); 4] = [
        ("an identifier alone", "no-such-snapshot", "", FAILURE_CODE_NOT_FOUND),
        (
            "an identifier paired with a cluster",
            "another-absent-snapshot",
            cluster,
            FAILURE_CODE_NOT_FOUND,
        ),
        ("an entry naming nothing at all", "", "", FAILURE_CODE_NOT_FOUND),
        (
            "a snapshot the delete rule does not admit",
            "locator-automated",
            cluster,
            "InvalidClusterSnapshotStateFault",
        ),
    ];
    let reported = reported_errors(&answer);
    assert_eq!(
        reported.len(),
        expected.len(),
        "case a list of entries that all fail: expected {} failures, got {reported:?} with body {}",
        expected.len(),
        answer.body
    );
    for (position, (case, identifier, cluster_identifier, code)) in expected.iter().enumerate() {
        let entry = &reported[position];
        assert_eq!(
            entry.snapshot_identifier.as_deref(),
            Some(*identifier),
            "case {case}: expected the failure to name {identifier:?}, got {:?}",
            entry.snapshot_identifier
        );
        assert_eq!(
            entry.snapshot_cluster_identifier.as_deref(),
            Some(*cluster_identifier),
            "case {case}: expected SnapshotClusterIdentifier present as \
             {cluster_identifier:?}, got {:?} with body {}",
            entry.snapshot_cluster_identifier,
            answer.body
        );
        assert_eq!(
            entry.failure_code.as_deref(),
            Some(*code),
            "case {case}: expected failure code {code:?}, got {:?}",
            entry.failure_code
        );
        assert!(
            entry
                .failure_reason
                .as_deref()
                .is_some_and(|reason| !reason.is_empty()),
            "case {case}: expected a failure reason, got {:?}",
            entry.failure_reason
        );
    }
}

/// A batch modify names the fault SHAPE and words its reason as
/// `ModifyClusterSnapshot` does, and lists its failures from the LAST entry the
/// request named.
#[tokio::test]
async fn a_batch_modify_reports_its_failures_youngest_first() {
    let emulator = Emulator::new();

    // The rows differ along the LENGTH of the list, every entry of which names a
    // snapshot no store holds, so the whole list comes back as failures.
    let rows: [(&str, &[&str]); 4] = [
        ("one unknown snapshot", &["absent-one"]),
        ("two unknown snapshots", &["absent-one", "absent-two"]),
        (
            "three unknown snapshots",
            &["absent-one", "absent-two", "absent-three"],
        ),
        (
            "four unknown snapshots, named out of alphabetical order",
            &["absent-d", "absent-b", "absent-c", "absent-a"],
        ),
    ];

    for (case, identifiers) in rows {
        let answer = batch_modify(
            &emulator,
            identifiers,
            &[("ManualSnapshotRetentionPeriod", "7")],
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected 200, got {} with body {}",
            answer.status, answer.body
        );
        assert_eq!(
            resources(&answer),
            Vec::<String>::new(),
            "case {case}: expected nothing reported as modified, got {:?}",
            resources(&answer)
        );

        // The failures are the request's own list turned around: the last
        // identifier it named heads `Errors`.
        let expected: Vec<ReportedError> = identifiers
            .iter()
            .rev()
            .map(|identifier| ReportedError {
                snapshot_identifier: Some((*identifier).to_string()),
                snapshot_cluster_identifier: None,
                failure_code: Some(FAILURE_CODE_NOT_FOUND.to_string()),
                failure_reason: Some(expected_message(
                    Wording::ClusterSnapshotNotFoundThenIdentifier,
                    identifier,
                )),
            })
            .collect();
        let reported = reported_errors(&answer);
        assert_eq!(
            reported, expected,
            "case {case}: expected the failures {expected:?}, got {reported:?} with body {}",
            answer.body
        );
    }
}

/// What a mixed list acted on is reported in request order; what it could not act
/// on is reported in the reverse of it.
#[tokio::test]
async fn a_mixed_batch_modify_keeps_resources_in_request_order() {
    let emulator = Emulator::new();
    let cluster = "mixed-batch-source";
    let created = emulator.create_minimal(cluster).await;
    assert_eq!(
        created.status, 200,
        "case seed cluster {cluster}: expected 200, got {} with body {}",
        created.status, created.body
    );
    for snapshot in ["mixed-first", "mixed-second"] {
        let taken = emulator
            .call(
                "CreateClusterSnapshot",
                &[
                    ("SnapshotIdentifier", snapshot),
                    ("ClusterIdentifier", cluster),
                ],
            )
            .await;
        assert_eq!(
            taken.status, 200,
            "case seed snapshot {snapshot}: expected 200, got {} with body {}",
            taken.status, taken.body
        );
    }
    // The `creating` -> `available` transition is clock-owed: the clock is moved
    // past it rather than slept through, so nothing here depends on how fast the
    // host runs.
    emulator.advance_clock(state::PROVISIONING_SPAN);

    let answer = batch_modify(
        &emulator,
        &["absent-one", "mixed-first", "absent-two", "mixed-second"],
        &[("ManualSnapshotRetentionPeriod", "7")],
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case a mixed list: expected 200, got {} with body {}",
        answer.status, answer.body
    );
    assert_eq!(
        resources(&answer),
        vec!["mixed-first".to_string(), "mixed-second".to_string()],
        "case a mixed list: expected the modified snapshots in request order, got {:?} with \
         body {}",
        resources(&answer),
        answer.body
    );
    let reported: Vec<Option<String>> = reported_errors(&answer)
        .into_iter()
        .map(|entry| entry.snapshot_identifier)
        .collect();
    assert_eq!(
        reported,
        vec![
            Some("absent-two".to_string()),
            Some("absent-one".to_string())
        ],
        "case a mixed list: expected the failures from the last entry named, got {reported:?} \
         with body {}",
        answer.body
    );

    // ROUND TRIP: what the batch acted on is what a later read reports, and the
    // entries it failed on added nothing to the store.
    for snapshot in ["mixed-first", "mixed-second"] {
        let read = emulator
            .call(
                "DescribeClusterSnapshots",
                &[("SnapshotIdentifier", snapshot)],
            )
            .await;
        assert_eq!(
            read.status, 200,
            "case {snapshot} read back: expected 200, got {} with body {}",
            read.status, read.body
        );
        assert_eq!(
            read.text("ManualSnapshotRetentionPeriod").as_deref(),
            Some("7"),
            "case {snapshot} read back: expected the period the batch applied, got {:?}",
            read.text("ManualSnapshotRetentionPeriod")
        );
    }
    for absent in ["absent-one", "absent-two"] {
        let read = emulator
            .call(
                "DescribeClusterSnapshots",
                &[("SnapshotIdentifier", absent)],
            )
            .await;
        assert_eq!(
            (read.status, read.error_code().as_deref()),
            (404, Some("ClusterSnapshotNotFound")),
            "case {absent} read back: expected 404 ClusterSnapshotNotFound, got {} / {:?}",
            read.status,
            read.error_code()
        );
    }
}
