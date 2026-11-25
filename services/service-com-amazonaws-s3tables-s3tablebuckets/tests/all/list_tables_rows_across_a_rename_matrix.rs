// SPDX-License-Identifier: CC0-1.0

//! The WIRE FORM of a `ListTables` summary row over a SEQUENCE of renames — a matrix in
//! which some moves are carried out and some are refused — read again after every step.
//!
//! RULE 1 — a `TableSummary`'s two instants go on the JSON wire as RFC 3339 date-time
//! STRINGS, never as the epoch-seconds number the `restJson1` protocol defaults to:
//! `smithy_query(query="shape", shape="TableSummary")` reports
//! `"timestampFormat": "date-time"` on both `createdAt` and `modifiedAt`, and both are
//! `@required`.
//!
//! RULE 2 — the id segment after `/table/` in a row's `tableARN` is the id this service
//! MINTED for the table when it was created: a canonical hyphenated UUID (8-4-4-4-12,
//! lowercase), which identifies the record for its whole life, so a listing row still
//! carries that exact ARN however often the table has been renamed since. The model
//! constrains the charset only (`TableARN`'s pattern admits the hyphens), so the form is
//! recorded behaviour, asserted through the one [`assert_canonical_uuid`] every suite
//! shares.
//!
//! RULE 3 — a record's instants move only with a write to THAT record: a row's `createdAt`
//! never moves at all, its `modifiedAt` never moves backwards, a carried-out rename of the
//! record is the only step that may move it, and a refused rename moves neither instant of
//! any row.
//!
//! `wire_forms.rs` states RULES 1 and 2 over the create-and-read paths,
//! `list_tables_row_wire_forms.rs` over the ways ONE record is written, and
//! `list_tables_rows_after_refused_writes.rs` over ONE call that writes nothing. What this
//! suite adds is the dimension none of them walks: a listing read repeatedly along a
//! SEQUENCE of renames, where what stands between two listings is a whole history of moves
//! and refusals rather than a single call. The cases span the orders the two kinds of step
//! come in (a refusal before the move it blocked, a move before the refusal it causes), a
//! name freed by one rename and taken by another, a cross-namespace move onto a name the
//! destination namespace already holds, every class of refusal a rename takes, one, two and
//! three tables, and three regions — and every step's listing is read through four views
//! (the whole bucket, each namespace, and one row per page).

use std::collections::BTreeMap;

use serde_json::{json, Value};
use service_com_amazonaws_s3tables_s3tablebuckets::state::{
    NOTHING_TO_RENAME_MESSAGE, TABLE_ALREADY_EXISTS_MESSAGE, TABLE_NOT_FOUND_MESSAGE,
    TABLE_VERSION_TOKEN_MISMATCH_MESSAGE,
};

use crate::harness::{timestamp_seconds, Emulator};
use crate::wire_forms::{assert_canonical_uuid, create_table_request, table_id_of};

/// The two namespaces every case's bucket holds: the one its tables are created in and
/// the one a cross-namespace move goes to. Both satisfy `NamespaceName`'s `^[0-9a-z_]*$`.
const SOURCE_NAMESPACE: &str = "matrix_source";
const TARGET_NAMESPACE: &str = "matrix_target";

/// A table name no case's bucket holds, which the "absent table" step addresses.
const ABSENT_TABLE: &str = "never_created";

/// A table created at the start of a case, labelled by an `id` that stays with the record
/// however it is renamed, so a step and an assertion can name the identity rather than a
/// name that moves.
#[derive(Debug, Clone, Copy)]
struct Created {
    id: &'static str,
    namespace: &'static str,
    name: &'static str,
    /// Whether the create describes the table's metadata, which is what gives the record a
    /// metadata document from the start.
    metadata: bool,
}

/// Which `versionToken` a rename step presents.
#[derive(Debug, Clone, Copy)]
enum Token {
    /// The subject's own current token, as `GetTable` reports it.
    Current,
    /// Another table's current token: a real token of this service's own minting that is
    /// not the subject's, which is what a stale view of the record presents.
    Of(&'static str),
    /// No `versionToken` member at all.
    Omitted,
}

/// Which table a rename step addresses.
#[derive(Debug, Clone, Copy)]
enum Subject {
    /// The record labelled by this id, at whatever coordinates it sits at now.
    Record(&'static str),
    /// [`ABSENT_TABLE`] in [`SOURCE_NAMESPACE`]: a table the namespace does not hold.
    Absent,
}

/// The answer a rename the store rules out is owed: the status, the error code and the
/// fixed sentence of its class.
#[derive(Debug, Clone, Copy)]
struct Refusal {
    status: u16,
    code: &'static str,
    message: &'static str,
}

/// Coordinates another table already occupies — at the source namespace or at the
/// destination one — are the conflict with a previous write `ConflictException` names.
const OCCUPIED: Refusal = Refusal {
    status: 409,
    code: "ConflictException",
    message: TABLE_ALREADY_EXISTS_MESSAGE,
};

/// A request naming neither new coordinate describes no move at all.
const NO_MOVE: Refusal = Refusal {
    status: 400,
    code: "BadRequestException",
    message: NOTHING_TO_RENAME_MESSAGE,
};

/// A table the namespace does not hold.
const ABSENT: Refusal = Refusal {
    status: 404,
    code: "NotFoundException",
    message: TABLE_NOT_FOUND_MESSAGE,
};

/// A `versionToken` that is not the addressed table's current one.
const STALE_VIEW: Refusal = Refusal {
    status: 409,
    code: "ConflictException",
    message: TABLE_VERSION_TOKEN_MISMATCH_MESSAGE,
};

/// One step of the matrix: a rename, and the answer it is owed.
#[derive(Debug, Clone, Copy)]
struct Step {
    label: &'static str,
    subject: Subject,
    new_namespace: Option<&'static str>,
    new_name: Option<&'static str>,
    token: Token,
    /// The refusal the step is owed, or `None` for a rename that is carried out — which
    /// this service answers 204 with, declaring no output members.
    refusal: Option<Refusal>,
}

/// One case: the tables a bucket starts with and the sequence of renames applied to them.
#[derive(Debug, Clone, Copy)]
struct Case {
    label: &'static str,
    region: &'static str,
    bucket: &'static str,
    created: &'static [Created],
    steps: &'static [Step],
}

/// A rename that is carried out.
const fn carried_out(
    label: &'static str,
    subject: Subject,
    new_namespace: Option<&'static str>,
    new_name: Option<&'static str>,
    token: Token,
) -> Step {
    Step {
        label,
        subject,
        new_namespace,
        new_name,
        token,
        refusal: None,
    }
}

/// A rename the store rules out, refused as its class.
const fn refused(
    label: &'static str,
    subject: Subject,
    new_namespace: Option<&'static str>,
    new_name: Option<&'static str>,
    token: Token,
    refusal: Refusal,
) -> Step {
    Step {
        label,
        subject,
        new_namespace,
        new_name,
        token,
        refusal: Some(refusal),
    }
}

/// The matrix: four sequences of renames over buckets holding one, two and three tables.
const CASES: &[Case] = &[
    Case {
        label: "a refusal, then the move it blocked, then the name it freed",
        region: "us-west-2",
        bucket: "lt-matrix-conflict-then-move",
        created: &[
            Created {
                id: "one",
                namespace: SOURCE_NAMESPACE,
                name: "tbl_one",
                metadata: true,
            },
            Created {
                id: "two",
                namespace: SOURCE_NAMESPACE,
                name: "tbl_two",
                metadata: false,
            },
        ],
        steps: &[
            refused(
                "onto the name the sibling occupies",
                Subject::Record("one"),
                None,
                Some("tbl_two"),
                Token::Current,
                OCCUPIED,
            ),
            carried_out(
                "onto a free name",
                Subject::Record("one"),
                None,
                Some("tbl_three"),
                Token::Current,
            ),
            carried_out(
                "of the sibling onto the name the previous rename freed",
                Subject::Record("two"),
                None,
                Some("tbl_one"),
                Token::Current,
            ),
            refused(
                "back onto the name the sibling now occupies",
                Subject::Record("one"),
                None,
                Some("tbl_one"),
                Token::Current,
                OCCUPIED,
            ),
        ],
    },
    Case {
        label: "a cross-namespace move onto a name the destination holds, then one that fits",
        region: "us-east-1",
        bucket: "lt-matrix-cross-namespace",
        created: &[
            // The same name in both namespaces, which is what makes the first move below a
            // conflict at the destination rather than at the source.
            Created {
                id: "source_a",
                namespace: SOURCE_NAMESPACE,
                name: "a",
                metadata: false,
            },
            Created {
                id: "target_a",
                namespace: TARGET_NAMESPACE,
                name: "a",
                metadata: true,
            },
            Created {
                id: "long",
                namespace: SOURCE_NAMESPACE,
                name: "a_very_long_table_name_0123456789",
                metadata: false,
            },
        ],
        steps: &[
            refused(
                "keeping a name the destination namespace holds",
                Subject::Record("source_a"),
                Some(TARGET_NAMESPACE),
                None,
                Token::Current,
                OCCUPIED,
            ),
            carried_out(
                "to the destination namespace under a free name",
                Subject::Record("source_a"),
                Some(TARGET_NAMESPACE),
                Some("a2"),
                Token::Current,
            ),
            carried_out(
                "of the destination's own table into the namespace the first one left",
                Subject::Record("target_a"),
                Some(SOURCE_NAMESPACE),
                None,
                Token::Current,
            ),
            refused(
                "of the long-named table onto the name that move took",
                Subject::Record("long"),
                None,
                Some("a"),
                Token::Omitted,
                OCCUPIED,
            ),
        ],
    },
    Case {
        label: "every class a rename is refused with, then the move that is carried out",
        region: "eu-west-1",
        bucket: "lt-matrix-refusal-classes",
        created: &[
            Created {
                id: "subject",
                namespace: SOURCE_NAMESPACE,
                name: "tbl_one",
                metadata: true,
            },
            Created {
                id: "sibling",
                namespace: SOURCE_NAMESPACE,
                name: "t0",
                metadata: false,
            },
        ],
        steps: &[
            refused(
                "naming neither new coordinate",
                Subject::Record("subject"),
                None,
                None,
                Token::Omitted,
                NO_MOVE,
            ),
            refused(
                "of a table the namespace does not hold",
                Subject::Absent,
                None,
                Some("free_name"),
                Token::Omitted,
                ABSENT,
            ),
            refused(
                "presenting the sibling's version token",
                Subject::Record("subject"),
                None,
                Some("free_name"),
                Token::Of("sibling"),
                STALE_VIEW,
            ),
            carried_out(
                "presenting the subject's own token",
                Subject::Record("subject"),
                None,
                Some("free_name"),
                Token::Current,
            ),
        ],
    },
    Case {
        label: "one table renamed away and back, with no token presented",
        region: "us-west-2",
        bucket: "lt-matrix-round-trip",
        created: &[Created {
            id: "only",
            namespace: SOURCE_NAMESPACE,
            name: "t",
            metadata: false,
        }],
        steps: &[
            carried_out(
                "to a second name",
                Subject::Record("only"),
                None,
                Some("t2"),
                Token::Omitted,
            ),
            carried_out(
                "to the destination namespace",
                Subject::Record("only"),
                Some(TARGET_NAMESPACE),
                None,
                Token::Omitted,
            ),
            carried_out(
                "back to the coordinates it started at",
                Subject::Record("only"),
                Some(SOURCE_NAMESPACE),
                Some("t"),
                Token::Omitted,
            ),
        ],
    },
];

/// What is known about one record as the matrix runs: where it sits now, the ARN its
/// create minted, and the two instants the last listing reported for it.
#[derive(Debug, Clone)]
struct Record {
    namespace: String,
    name: String,
    /// The `tableARN` `CreateTable` answered with — the ARN every later row must carry.
    arn: String,
    /// The `createdAt` TEXT the first listing reported, which no step may move. Empty
    /// until that first listing has been read.
    created_at: String,
    /// The `modifiedAt` TEXT the last listing reported, which only a write to this record
    /// may move.
    modified_at: String,
}

/// The row of `rows` whose `tableARN` is `arn`: a listing row is correlated with a record
/// by the ARN the create minted for it, not by a name a rename moves.
fn row_for_arn<'a>(rows: &'a [Value], arn: &str, case: &str) -> &'a Value {
    rows.iter()
        .find(|row| row.get("tableARN").and_then(Value::as_str) == Some(arn))
        .unwrap_or_else(|| {
            panic!("case {case:?}: no listing row carries the minted ARN {arn:?}: {rows:?}")
        })
}

/// The instant a date-time TEXT this suite recorded stands for, read through the one
/// reading of the modelled wire form the harness states.
fn seconds_of(text: &str, case: &str) -> f64 {
    timestamp_seconds(&json!({ "modifiedAt": text }), "modifiedAt", case)
}

/// Create the case's bucket, its two namespaces and its tables, and answer the bucket's
/// ARN beside one [`Record`] per created table, keyed by its label.
async fn set_up(emulator: &Emulator, case: &Case) -> (String, BTreeMap<&'static str, Record>) {
    let label = case.label;
    let bucket = emulator
        .create_table_bucket(case.region, &json!({ "name": case.bucket }))
        .await;
    assert_eq!(
        bucket.status, 200,
        "case {label:?}: CreateTableBucket: {bucket:?}"
    );
    let bucket_arn = bucket.text("arn");
    for namespace in [SOURCE_NAMESPACE, TARGET_NAMESPACE] {
        let created = emulator
            .create_namespace(case.region, &bucket_arn, &json!({ "namespace": [namespace] }))
            .await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: CreateNamespace {namespace:?}: {created:?}"
        );
    }

    let mut records = BTreeMap::new();
    for created in case.created {
        let reply = emulator
            .create_table(
                case.region,
                &bucket_arn,
                created.namespace,
                &create_table_request(created.name, created.metadata),
            )
            .await;
        assert_eq!(
            reply.status, 200,
            "case {label:?}: CreateTable {id:?}: {reply:?}",
            id = created.id
        );
        let arn = reply.text("tableARN");
        assert_canonical_uuid(
            &table_id_of(&arn, label),
            "the id segment of the ARN CreateTable reported",
            label,
        );
        records.insert(
            created.id,
            Record {
                namespace: created.namespace.to_string(),
                name: created.name.to_string(),
                arn,
                created_at: String::new(),
                modified_at: String::new(),
            },
        );
    }
    (bucket_arn, records)
}

/// Every row of the listing `query` asks for, following `continuationToken` to the end so
/// a paged view reports the whole listing.
async fn rows(
    emulator: &Emulator,
    case: &Case,
    bucket_arn: &str,
    query: &[(&str, String)],
    view: &str,
) -> Vec<Value> {
    let mut collected = Vec::new();
    let mut token: Option<String> = None;
    // One page per table of the bucket is the worst case; the extra pages are headroom for
    // the last, empty page a cursor can end on.
    let pages = case.created.len() + 3;
    for page in 0..pages {
        let mut asked: Vec<(&str, String)> = query.to_vec();
        if let Some(token) = &token {
            asked.push(("continuationToken", token.clone()));
        }
        let reply = emulator.list_tables(case.region, bucket_arn, &asked).await;
        assert_eq!(
            reply.status, 200,
            "case {view:?}: ListTables (page {page}): {reply:?}"
        );
        collected.extend(reply.list("tables"));
        token = reply.optional_text("continuationToken");
        if token.is_none() {
            return collected;
        }
    }
    panic!("case {view:?}: the listing never ran out of pages");
}

/// One table's current `versionToken`, as `GetTable` reports it at the coordinates it sits
/// at now.
async fn version_token(emulator: &Emulator, case: &Case, bucket_arn: &str, at: &Record) -> String {
    let label = case.label;
    let read = emulator
        .get_table(case.region, bucket_arn, &at.namespace, &at.name)
        .await;
    assert_eq!(
        read.status, 200,
        "case {label:?}: GetTable {name:?}: {read:?}",
        name = at.name
    );
    read.text("versionToken")
}

/// RULES 1, 2 and 3 over four views of the listing, against what the records are expected
/// to carry. `written` names the record a carried-out rename has just written, the only one
/// whose `modifiedAt` may have moved. Answers the two instants each record's row reported,
/// so the next step can be measured against them.
async fn assert_views(
    emulator: &Emulator,
    case: &Case,
    bucket_arn: &str,
    records: &BTreeMap<&'static str, Record>,
    written: Option<&'static str>,
    after: &str,
) -> BTreeMap<&'static str, (String, String)> {
    let views: Vec<(String, Vec<(&str, String)>)> = vec![
        ("the whole bucket".to_string(), Vec::new()),
        (
            format!("namespace {SOURCE_NAMESPACE:?}"),
            vec![("namespace", SOURCE_NAMESPACE.to_string())],
        ),
        (
            format!("namespace {TARGET_NAMESPACE:?}"),
            vec![("namespace", TARGET_NAMESPACE.to_string())],
        ),
        (
            "one row per page".to_string(),
            vec![("maxTables", "1".to_string())],
        ),
    ];

    let mut reported: BTreeMap<&'static str, (String, String)> = BTreeMap::new();
    for (view, query) in views {
        let view_label = format!("{} / {after} / {view}", case.label);
        let listed = rows(emulator, case, bucket_arn, &query, &view_label).await;
        for (&id, record) in records {
            // A namespace-narrowed view reports the records that sit in that namespace and
            // no others.
            let narrowed = query
                .iter()
                .find(|(member, _)| *member == "namespace")
                .map(|(_, namespace)| namespace.as_str());
            if narrowed.is_some_and(|namespace| namespace != record.namespace) {
                let absent = listed
                    .iter()
                    .all(|row| row.get("tableARN").and_then(Value::as_str) != Some(&record.arn));
                assert!(
                    absent,
                    "case {view_label:?}: record {id:?} sits in {namespace:?}, so this view \
                     must not report it, got {listed:?}",
                    namespace = record.namespace
                );
                continue;
            }

            let row_label = format!("{view_label} / record {id:?}");
            let row = row_for_arn(&listed, &record.arn, &row_label);

            // The row stands for the record at the coordinates the matrix has left it at.
            assert_eq!(
                row.get("name").and_then(Value::as_str),
                Some(record.name.as_str()),
                "case {row_label:?}: expected the row's name to be {name:?}, got {row}",
                name = record.name
            );
            assert_eq!(
                row.get("namespace").and_then(Value::as_array),
                Some(&vec![Value::from(record.namespace.clone())]),
                "case {row_label:?}: expected the row's namespace to be [{namespace:?}], got \
                 {row}",
                namespace = record.namespace
            );

            // RULE 1: both instants are JSON STRINGS in the modelled date-time format —
            // `timestamp_seconds` accepts that form and no other.
            for member in ["createdAt", "modifiedAt"] {
                let actual = row.get(member);
                assert!(
                    actual.is_some_and(Value::is_string),
                    "case {row_label:?}: expected {member} to be an RFC 3339 date-time \
                     string, got {actual:?}"
                );
            }
            let created_seconds = timestamp_seconds(row, "createdAt", &row_label);
            let modified_seconds = timestamp_seconds(row, "modifiedAt", &row_label);
            assert!(
                modified_seconds >= created_seconds,
                "case {row_label:?}: the row was last modified at {modified_seconds}, before \
                 it was created at {created_seconds}"
            );

            // RULE 2: the row carries the ARN the create minted — the lookup above matched
            // the row on it — and that ARN's id segment is a canonical hyphenated UUID.
            assert_canonical_uuid(
                &table_id_of(&record.arn, &row_label),
                "the id segment of a listing row's tableARN",
                &row_label,
            );

            let created_text = row
                .get("createdAt")
                .and_then(Value::as_str)
                .expect("createdAt was just read as a string")
                .to_string();
            let modified_text = row
                .get("modifiedAt")
                .and_then(Value::as_str)
                .expect("modifiedAt was just read as a string")
                .to_string();

            // RULE 3, over the step just taken: `createdAt` never moves, and `modifiedAt`
            // moves only for the record a carried-out rename wrote — and then never
            // backwards.
            if !record.created_at.is_empty() {
                assert_eq!(
                    created_text, record.created_at,
                    "case {row_label:?}: the row reports createdAt as {created_text:?}, not \
                     the {expected:?} every earlier listing reported",
                    expected = record.created_at
                );
                if written == Some(id) {
                    let previous = seconds_of(&record.modified_at, &row_label);
                    assert!(
                        modified_seconds >= previous,
                        "case {row_label:?}: the rename moved modifiedAt back to \
                         {modified_seconds} from {previous}"
                    );
                } else {
                    assert_eq!(
                        modified_text, record.modified_at,
                        "case {row_label:?}: the row reports modifiedAt as {modified_text:?} \
                         after a step that wrote nothing to this record, not the {expected:?} \
                         the previous listing reported",
                        expected = record.modified_at
                    );
                }
            }

            // Every view of ONE listing reports one record identically.
            if let Some((seen_created, seen_modified)) = reported.get(id) {
                assert_eq!(
                    (&created_text, &modified_text),
                    (seen_created, seen_modified),
                    "case {row_label:?}: this view reports the record's instants as \
                     ({created_text:?}, {modified_text:?}), another view of the same listing \
                     reported ({seen_created:?}, {seen_modified:?})"
                );
            } else {
                reported.insert(id, (created_text, modified_text));
            }
        }
    }
    reported
}

/// The request body one step's rename carries, and the coordinates it is addressed at.
async fn rename_request(
    emulator: &Emulator,
    case: &Case,
    bucket_arn: &str,
    records: &BTreeMap<&'static str, Record>,
    step: &Step,
) -> (Record, Value) {
    let label = case.label;
    let addressed = match step.subject {
        Subject::Record(id) => records
            .get(id)
            .unwrap_or_else(|| panic!("case {label:?}: no record {id:?}"))
            .clone(),
        Subject::Absent => Record {
            namespace: SOURCE_NAMESPACE.to_string(),
            name: ABSENT_TABLE.to_string(),
            arn: String::new(),
            created_at: String::new(),
            modified_at: String::new(),
        },
    };

    let mut request = json!({});
    if let Some(namespace) = step.new_namespace {
        request["newNamespaceName"] = json!(namespace);
    }
    if let Some(name) = step.new_name {
        request["newName"] = json!(name);
    }
    let token = match step.token {
        Token::Current => Some(version_token(emulator, case, bucket_arn, &addressed).await),
        Token::Of(id) => {
            let other = records
                .get(id)
                .unwrap_or_else(|| panic!("case {label:?}: no record {id:?}"))
                .clone();
            Some(version_token(emulator, case, bucket_arn, &other).await)
        }
        Token::Omitted => None,
    };
    if let Some(token) = token {
        request["versionToken"] = json!(token);
    }
    (addressed, request)
}

/// RULES 1, 2 and 3: along a whole matrix of renames — carried out and refused — every
/// `ListTables` row keeps the modelled wire forms, the id its create minted and the
/// instants only a write to its own record moves.
#[tokio::test]
async fn every_listing_along_a_rename_matrix_keeps_the_modelled_row_forms() {
    for case in CASES {
        let label = case.label;
        let emulator = Emulator::new();
        let (bucket_arn, mut records) = set_up(&emulator, case).await;

        // The listing before any step runs: it settles the text each record's instants are
        // compared against from here on.
        let reported =
            assert_views(&emulator, case, &bucket_arn, &records, None, "the creates").await;
        for (id, record) in records.iter_mut() {
            let (created_at, modified_at) = reported
                .get(id)
                .unwrap_or_else(|| {
                    panic!("case {label:?}: the first listing has no row for {id:?}")
                })
                .clone();
            record.created_at = created_at;
            record.modified_at = modified_at;
        }

        for step in case.steps {
            let step_label = format!("{label} / a rename {}", step.label);
            let (addressed, request) =
                rename_request(&emulator, case, &bucket_arn, &records, step).await;

            let reply = emulator
                .rename_table(
                    case.region,
                    &bucket_arn,
                    &addressed.namespace,
                    &addressed.name,
                    &request,
                )
                .await;
            // A rename this service carries out is answered 204 with no output members; one
            // it rules out states its class.
            let expected_status = step.refusal.map_or(204, |refusal| refusal.status);
            assert_eq!(
                reply.status, expected_status,
                "case {step_label:?}: expected status {expected_status}, got {reply:?}"
            );
            let expected_code = step.refusal.map(|refusal| refusal.code);
            assert_eq!(
                reply.error_code.as_deref(),
                expected_code,
                "case {step_label:?}: expected error code {expected_code:?}, got {reply:?}"
            );
            if let Some(refusal) = step.refusal {
                assert_eq!(
                    reply.message(),
                    refusal.message,
                    "case {step_label:?}: expected message {expected:?}, got {actual:?}",
                    expected = refusal.message,
                    actual = reply.message()
                );
            }

            // A carried-out rename re-keys the record; a refused one leaves it where it was.
            let written = match (step.refusal, step.subject) {
                (None, Subject::Record(id)) => {
                    let record = records
                        .get_mut(id)
                        .unwrap_or_else(|| panic!("case {step_label:?}: no record {id:?}"));
                    if let Some(namespace) = step.new_namespace {
                        record.namespace = namespace.to_string();
                    }
                    if let Some(name) = step.new_name {
                        record.name = name.to_string();
                    }
                    Some(id)
                }
                _ => None,
            };

            let reported =
                assert_views(&emulator, case, &bucket_arn, &records, written, step.label).await;
            for (id, record) in records.iter_mut() {
                let (_, modified_at) = reported.get(id).unwrap_or_else(|| {
                    panic!("case {step_label:?}: the listing has no row for {id:?}")
                });
                record.modified_at = modified_at.clone();
            }
        }
    }
}
