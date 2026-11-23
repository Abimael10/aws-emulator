// SPDX-License-Identifier: CC0-1.0

//! `ListSnapshotsInRecycleBin` over a bin that holds MANY entries: what the report narrows to,
//! how it pages the narrowed set, and that reporting it leaves the bin as it was.
//!
//! [`super::snapshots_in_recycle_bin`] drives the report for ONE entry and for the requests it
//! refuses; this suite is about the report as a REPORT, which is a question only a bin holding
//! several entries can answer. The rules under test are:
//!
//! 1. NARROWING. "The IDs of the snapshots to list. Omit this parameter to list all of the
//!    snapshots that are in the Recycle Bin" (`ListSnapshotsInRecycleBinRequest$SnapshotIds`):
//!    the report answers the entries the bin holds whose ids the request named, in `SnapshotId`
//!    order, and every entry it holds when the request names none — so the answer depends on
//!    the SET of ids named and not on the order or the multiplicity they were named in, and an
//!    entry the bin holds is never left out of a report that named it.
//! 2. PAGING IS OF THE NARROWED SET. "The maximum number of items to return for this request.
//!    To get the next page of items, make another request with the token returned in the
//!    output" (`MaxResults`), the token being null "when there are no more items to return"
//!    (`NextToken`): a page carries at most that many entries of the narrowed set, the token is
//!    there exactly while entries of that set follow, and the pages walked end to end are the
//!    narrowed set itself.
//! 3. THE REPORT IS A READ. Two identical reports answer identical entries, and the entries a
//!    report answers are exactly the ones a restore can still give back: restoring one of them
//!    drops that entry and leaves every other entry the bin holds reported as it was.
//!
//! Each reported entry is checked against the snapshot it was derived from — the source volume
//! and the description the bin kept, and the retention period the two reported instants span —
//! so a narrowing that reported the right ids with the wrong rows would fail too.
//!
//! The fixture's snapshots differ along the size of the volume they were taken of, that
//! volume's encryption, and whether they carry a description and tags, and the table's rows
//! differ along how many of the bin's entries the request names, in which order, with which
//! multiplicity, and at which page size — so no row is the specification.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use ec2::state::SNAPSHOT_RECYCLE_BIN_PAGE_SIZE_RANGE;

use super::ec2_query::{as_timestamp, element, indexed};
use super::snapshots_in_recycle_bin::{
    LONG_RETENTION, SnapshotRow, create_snapshot, describe, list_bin, recycle, restore,
};
use super::volume_lifecycle::p;
use super::wire::{child_text, document, set_items};

/// One snapshot the fixture retained: the id the bin holds it under, the volume it was taken
/// of, and the description it carried — the members a reported entry has to answer for it.
#[derive(Debug, Clone)]
struct Retained {
    snapshot_id: String,
    volume_id: String,
    description: Option<&'static str>,
}

/// One entry of the report, as the service rendered it — the timestamps as the text they
/// reached the wire as, so comparing two reports compares what a client would read.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportedEntry {
    snapshot_id: String,
    volume_id: Option<String>,
    description: Option<String>,
    entered: String,
    exits: String,
}

/// The snapshots the fixture retains, differing along everything the report's rules
/// generalise over: the size of the volume each was taken of, that volume's encryption, and
/// whether the snapshot carries a description, tags, or both.
fn retained_rows() -> Vec<SnapshotRow> {
    vec![
        SnapshotRow {
            case: "a snapshot of the smallest volume, with no description and no tags",
            volume_size: 1,
            encrypted: false,
            description: None,
            tags: &[],
        },
        SnapshotRow {
            case: "a described snapshot of an eight GiB volume",
            volume_size: 8,
            encrypted: false,
            description: Some("the nightly backup"),
            tags: &[],
        },
        SnapshotRow {
            case: "a tagged snapshot of an encrypted volume",
            volume_size: 4,
            encrypted: true,
            description: None,
            tags: &[("Name", "retained"), ("Team", "A")],
        },
        SnapshotRow {
            case: "a described and tagged snapshot of a 1024 GiB volume",
            volume_size: 1_024,
            encrypted: true,
            description: Some("the quarterly archive"),
            tags: &[("Name", "archive")],
        },
        SnapshotRow {
            case: "a described snapshot of a sixteen GiB volume",
            volume_size: 16,
            encrypted: false,
            description: Some("the pre-upgrade copy"),
            tags: &[("Stage", "upgrade")],
        },
        SnapshotRow {
            case: "an undescribed snapshot of a small encrypted volume",
            volume_size: 2,
            encrypted: true,
            description: None,
            tags: &[],
        },
    ]
}

/// An emulator whose Recycle Bin holds one entry per [`retained_rows`] row, each retained for
/// [`LONG_RETENTION`] so nothing in this suite reaches the end of a retention period.
///
/// Retained through the state primitive a covered `DeleteSnapshot` writes the bin through, the
/// way [`super::snapshots_in_recycle_bin`] does — the delete path itself is that suite's own
/// case, so each case here is one call away from the report it is about.
async fn bin_holding_several_entries(case: &str) -> (MultiServiceRouter, Vec<Retained>) {
    let (router, engine) = super::ec2_query::emulator();
    let mut retained: Vec<Retained> = Vec::new();
    for row in retained_rows() {
        let (snapshot_id, volume_id) = create_snapshot(&router, &row).await;
        recycle(&engine, case, &snapshot_id, LONG_RETENTION);
        retained.push(Retained {
            snapshot_id,
            volume_id,
            description: row.description,
        });
    }
    // The fixture is only a fixture if the bin really took every one of them in.
    let (status, body) = list_bin(&router, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from the report of the seeded bin, got {status} / \
         {body}"
    );
    assert_eq!(
        reported_ids(case, &body).len(),
        retained.len(),
        "case {case:?}: expected the bin to hold all {} retained snapshots, got {body}",
        retained.len()
    );
    (router, retained)
}

/// The entries a report body carries, in the order it reported them.
fn reported_entries(case: &str, body: &str) -> Vec<ReportedEntry> {
    let parsed = document(case, body);
    set_items(parsed.root_element(), "snapshotSet")
        .into_iter()
        .map(|item| {
            let required = |name: &str| {
                child_text(item, name).unwrap_or_else(|| {
                    panic!("case {case:?}: no <{name}> on a reported entry in {body}")
                })
            };
            ReportedEntry {
                snapshot_id: required("snapshotId"),
                volume_id: child_text(item, "volumeId"),
                description: child_text(item, "description"),
                entered: required("recycleBinEnterTime"),
                exits: required("recycleBinExitTime"),
            }
        })
        .collect()
}

/// The ids a report body carries, in the order it reported them.
fn reported_ids(case: &str, body: &str) -> Vec<String> {
    reported_entries(case, body)
        .into_iter()
        .map(|entry| entry.snapshot_id)
        .collect()
}

/// The `SnapshotId.N` form fields naming `selection`, and nothing at all for a request that
/// names no id — the two halves of "omit this parameter to list all of the snapshots that are
/// in the Recycle Bin".
fn named_ids(selection: Option<&[&str]>) -> Vec<(String, String)> {
    match selection {
        None => Vec::new(),
        Some(ids) => indexed("SnapshotId", ids),
    }
}

/// The ids `selection` names, as the report orders them: ascending `SnapshotId`, each once.
fn expected_order(selection: &[&str]) -> Vec<String> {
    let mut expected: Vec<String> = selection.iter().map(|id| (*id).to_string()).collect();
    expected.sort();
    expected.dedup();
    expected
}

/// Check every reported entry against the snapshot it was derived from, and the span between
/// the two instants it reports against the retention period the bin took it in for.
fn assert_entries_match_their_snapshots(case: &str, entries: &[ReportedEntry], bin: &[Retained]) {
    for entry in entries {
        let source = bin
            .iter()
            .find(|candidate| candidate.snapshot_id == entry.snapshot_id)
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: the report answered {:?}, which the bin never held",
                    entry.snapshot_id
                )
            });
        assert_eq!(
            entry.volume_id.as_deref(),
            Some(source.volume_id.as_str()),
            "case {case:?}: expected the entry for {} to report its source volume {}, got {:?}",
            entry.snapshot_id,
            source.volume_id,
            entry.volume_id
        );
        assert_eq!(
            entry.description.as_deref(),
            source.description,
            "case {case:?}: expected the entry for {} to report the snapshot's description \
             {:?}, got {:?}",
            entry.snapshot_id,
            source.description,
            entry.description
        );
        let instant = |name: &str, text: &str| {
            as_timestamp(text).unwrap_or_else(|| {
                panic!("case {case:?}: {name} {text:?} is not a timestamp")
            })
        };
        let span = instant("recycleBinExitTime", &entry.exits)
            - instant("recycleBinEnterTime", &entry.entered);
        assert!(
            (span - LONG_RETENTION.as_secs_f64()).abs() < 1.0,
            "case {case:?}: expected the entry for {} to span the retention period of {:?}, \
             got {span} second(s)",
            entry.snapshot_id,
            LONG_RETENTION
        );
    }
}

/// RULE 1: the report answers the entries whose ids the request named, in `SnapshotId` order,
/// and every entry the bin holds when the request names none — the same answer whatever order
/// or multiplicity the ids were named in.
#[tokio::test]
async fn the_recycle_bin_report_answers_the_set_of_ids_the_request_names() {
    let case = "a Recycle Bin holding six retained snapshots";
    let (router, bin) = bin_holding_several_entries(case).await;
    let held: Vec<&str> = bin.iter().map(|entry| entry.snapshot_id.as_str()).collect();
    let mut ascending = held.clone();
    ascending.sort_unstable();

    // (case, the ids the request names — `None` naming none at all)
    let rows: Vec<(&str, Option<Vec<&str>>)> = vec![
        ("no SnapshotIds at all", None),
        ("one of the entries", Some(vec![held[2]])),
        (
            "three entries, named in the order the bin reports them",
            Some(vec![ascending[0], ascending[2], ascending[4]]),
        ),
        (
            "the same three entries, named in the opposite order",
            Some(vec![ascending[4], ascending[2], ascending[0]]),
        ),
        (
            "two entries, one of them named twice",
            Some(vec![held[1], held[4], held[1]]),
        ),
        ("every entry, named explicitly", Some(held.clone())),
    ];

    for (row_case, selection) in rows {
        let (status, body) = list_bin(&router, &named_ids(selection.as_deref())).await;
        assert_eq!(
            status, 200,
            "case {row_case:?}: expected HTTP 200 from the report, got {status} / {body}"
        );
        let expected = match &selection {
            None => ascending.iter().map(|id| (*id).to_string()).collect(),
            Some(ids) => expected_order(ids),
        };
        let entries = reported_entries(row_case, &body);
        let reported: Vec<String> = entries
            .iter()
            .map(|entry| entry.snapshot_id.clone())
            .collect();
        assert_eq!(
            reported, expected,
            "case {row_case:?}: expected the named entries in SnapshotId order, got {body}"
        );
        assert!(
            element(&body, "nextToken").is_none(),
            "case {row_case:?}: a report that fits in one page carries no token, got {body}"
        );
        assert_entries_match_their_snapshots(row_case, &entries, &bin);
    }
}

/// RULE 2: `MaxResults` pages the set the request narrowed the report to — each page is at
/// most that many entries, a token follows exactly while entries of that set remain, and the
/// walk is the narrowed set.
///
/// The page sizes are the published bounds of
/// `ec2::state::SNAPSHOT_RECYCLE_BIN_PAGE_SIZE_RANGE` (the modelled `@range` of
/// `ListSnapshotsInRecycleBinMaxResults`), so the walk is probed at the smallest page the
/// report serves and at the largest.
#[tokio::test]
async fn the_recycle_bin_report_pages_the_set_the_request_narrowed_it_to() {
    let case = "a Recycle Bin holding six retained snapshots";
    let (router, bin) = bin_holding_several_entries(case).await;
    let held: Vec<&str> = bin.iter().map(|entry| entry.snapshot_id.as_str()).collect();
    let (smallest_page, largest_page) = SNAPSHOT_RECYCLE_BIN_PAGE_SIZE_RANGE;

    // (case, the ids named — `None` naming none at all, the page size asked for). The
    // narrowed rows are what this suite is about: the walk of the WHOLE bin at the smallest
    // page is [`super::snapshots_in_recycle_bin`]'s own case, so it appears here only at the
    // other published bound.
    let rows: Vec<(&str, Option<Vec<&str>>, i32)> = vec![
        (
            "the whole bin at the largest page the report serves",
            None,
            largest_page,
        ),
        (
            "a narrowing of six entries at the smallest page, which overflows it",
            Some(held.clone()),
            smallest_page,
        ),
        (
            "a narrowing of four entries at the smallest page",
            Some(vec![held[0], held[1], held[2], held[3]]),
            smallest_page,
        ),
        (
            "a narrowing of five entries at the smallest page, which fills it exactly",
            Some(vec![held[0], held[1], held[2], held[3], held[4]]),
            smallest_page,
        ),
        (
            "a narrowing of one entry at the smallest page",
            Some(vec![held[5]]),
            smallest_page,
        ),
    ];

    for (row_case, selection, page_size) in rows {
        let expected: Vec<String> = match &selection {
            None => expected_order(&held),
            Some(ids) => expected_order(ids),
        };
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        // One page per expected entry is more pages than any page size here can need, so a
        // walk that never ends fails the case instead of looping.
        for page_number in 1..=expected.len() + 1 {
            let mut params = named_ids(selection.as_deref());
            params.push(p("MaxResults", page_size.to_string()));
            if let Some(cursor) = &token {
                params.push(p("NextToken", cursor.as_str()));
            }
            let (status, body) = list_bin(&router, &params).await;
            assert_eq!(
                status, 200,
                "case {row_case:?} (page {page_number}): expected HTTP 200, got {status} / \
                 {body}"
            );
            let entries = reported_entries(row_case, &body);
            assert!(
                entries.len() <= page_size as usize,
                "case {row_case:?} (page {page_number}): expected at most {page_size} entries, \
                 got {}",
                entries.len()
            );
            assert_entries_match_their_snapshots(row_case, &entries, &bin);
            walked.extend(entries.into_iter().map(|entry| entry.snapshot_id));
            token = element(&body, "nextToken");
            let left = expected.len().saturating_sub(walked.len());
            assert_eq!(
                token.is_some(),
                left > 0,
                "case {row_case:?} (page {page_number}): expected a token exactly while \
                 entries follow ({left} left), got {body}"
            );
            if token.is_none() {
                break;
            }
        }
        assert_eq!(
            walked, expected,
            "case {row_case:?}: expected the paged walk to be the narrowed set in SnapshotId \
             order, got {walked:?}"
        );
    }
}

/// RULE 3: reporting the bin changes nothing a later caller sees — the same request answers
/// the same entries — and the entries it answers are the ones a restore can still give back:
/// a restored id leaves the report, every other entry stays as it was, and the restored
/// snapshot is the account's again.
#[tokio::test]
async fn reporting_the_recycle_bin_leaves_every_entry_it_reported() {
    let case = "a Recycle Bin holding six retained snapshots";
    let (router, bin) = bin_holding_several_entries(case).await;
    let held: Vec<&str> = bin.iter().map(|entry| entry.snapshot_id.as_str()).collect();

    // The same request, twice: a read answers the same entries both times.
    for (row_case, selection) in [
        ("the whole bin, reported twice", None),
        (
            "a narrowing of three entries, reported twice",
            Some(vec![held[0], held[2], held[4]]),
        ),
    ] {
        let params = named_ids(selection.as_deref());
        let (first_status, first_body) = list_bin(&router, &params).await;
        let (second_status, second_body) = list_bin(&router, &params).await;
        assert_eq!(
            (first_status, second_status),
            (200, 200),
            "case {row_case:?}: expected both reports to answer HTTP 200, got {first_status} \
             then {second_status} with {first_body} then {second_body}"
        );
        assert_eq!(
            reported_entries(row_case, &first_body),
            reported_entries(row_case, &second_body),
            "case {row_case:?}: expected a second report to answer the same entries, got \
             {first_body} then {second_body}"
        );
    }

    // Every entry the report answered can still be given back, and giving one back is the
    // only thing that takes it out of the report.
    let (status, body) = list_bin(&router, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from the report, got {status} / {body}"
    );
    let before = reported_entries(case, &body);
    let restored = held[3];
    let (status, body) = restore(&router, restored, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 restoring the reported {restored}, got {status} / \
         {body}"
    );
    let (status, body) = list_bin(&router, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from the report after the restore, got {status} / \
         {body}"
    );
    let after = reported_entries(case, &body);
    let expected: Vec<ReportedEntry> = before
        .iter()
        .filter(|entry| entry.snapshot_id != restored)
        .cloned()
        .collect();
    assert_eq!(
        after, expected,
        "case {case:?}: expected the report to drop {restored} and leave every other entry as \
         it was, got {body}"
    );
    let (status, body) = describe(&router, restored).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the restored {restored} to be the account's again, got \
         {status} / {body}"
    );
    // The account's again means the account's OWN report answers that snapshot, taken of the
    // volume the bin had been reporting for it — not merely that the describe answered.
    assert_eq!(
        element(&body, "snapshotId").as_deref(),
        Some(restored),
        "case {case:?}: expected DescribeSnapshots to report {restored}, got {body}"
    );
    let source = before
        .iter()
        .find(|entry| entry.snapshot_id == restored)
        .unwrap_or_else(|| panic!("case {case:?}: the report never answered {restored}"));
    assert_eq!(
        element(&body, "volumeId"),
        source.volume_id,
        "case {case:?}: expected the restored snapshot to report the source volume the bin \
         had been reporting for it, got {body}"
    );
}
