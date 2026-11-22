// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the DB instance a DB LOG FILE READ resolves before it serves
//! anything: `DescribeDBLogFiles` and `DownloadDBLogFilePortion`.
//!
//! Every call goes over the wire: the service is mounted on a `MultiServiceRouter` exactly
//! as the production router mounts it (`crate::db_instances::emulator`), each request is a
//! real awsQuery form POST carrying the SigV4 credential scope the router keys on, and
//! every assertion reads the XML the emulator answered with.
//!
//! The rule under test: a DB log file read resolves the `DBInstanceIdentifier` it was given
//! against the DB instances the account holds IN THE REGION the request was signed for, and
//! when that identifier holds none it answers the modelled `DBInstanceNotFoundFault` — 404
//! with the query-protocol `Sender`/`DBInstanceNotFound` envelope and the sentence that
//! error class carries — instead of a log of its own. The refusal does not depend on the
//! other members: a log file name that names no file of any instance and a `Marker` no
//! prior request issued are answered by the instance's absence just the same, because
//! nothing of the file is reached until the instance is.
//!
//! The rows below span what the rule generalises over — HOW the identifier comes to resolve
//! to no instance of the region (never created, created and deleted, held in another region
//! only) and the SPELLING the request carries (plain, digit-bearing, a single letter, mixed
//! case) — and the request shapes each row is read with span the other members the rule
//! holds regardless of (a file the instance writes, a file nothing writes, a page token
//! nothing issued, a narrowing member). The last test is the contrast: an instance the
//! region does hold is served, through either spelling of its identifier, so the refusal is
//! the resolution's answer and not a blanket one.
//!
//! Nothing here waits on the clock: a transition this service owes is waited for by polling
//! a REPORTING call through the one poll loop this test target has
//! (`crate::lifecycle_poll_loops::poll_until`), so what ends a wait is the CONDITION.

use service_com_amazonaws_rds_amazonrdsv19::state::engine_log;
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{
    call, call_in, emulator, expect_error, expect_ok, optional_text, text, texts, REGION,
};
use crate::lifecycle_poll_loops::{poll_until, read_instance, read_instance_in};

/// The wire error `<Code>` the model binds `DBInstanceNotFoundFault` to
/// (`awsQueryError` code `DBInstanceNotFound`, `httpResponseCode` 404).
const DB_INSTANCE_NOT_FOUND: &str = "DBInstanceNotFound";

/// A second region of the partition, for the row whose instance is held in another region
/// only: Amazon RDS is regional, so an instance of one region resolves in no other.
const OTHER_REGION: &str = "us-east-1";

/// The engine every instance below runs, and the log file such an instance writes
/// (`crate::state::engine_log::log_file_name`).
const ENGINE: &str = "mysql";

/// A log file name NO DB instance of this service writes, for the request shape that trips
/// the log-file miss as well as the instance miss.
const UNWRITTEN_LOG_FILE: &str = "error/not-a-log-file.log";

/// How the identifier a row sends comes to hold no DB instance in the region the read is
/// signed for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Resolution {
    /// No instance of that identifier was ever created.
    NeverCreated,
    /// An instance of that identifier was created, deleted, and its removal — the
    /// transition the delete owes — has settled.
    Deleted,
    /// An instance of that identifier exists, in another region only.
    AnotherRegionOnly,
}

/// One row of the table: the identifier a read sends and why it resolves to nothing.
struct Row {
    case: &'static str,
    /// The spelling the REQUEST carries, which is what the refusal names.
    requested: &'static str,
    resolution: Resolution,
}

/// The rows differ along what the rule generalises over: how the identifier comes to
/// resolve to nothing, and the spelling the request carries.
const ROWS: &[Row] = &[
    Row {
        case: "a plain identifier never created",
        requested: "emu-logres-absent",
        resolution: Resolution::NeverCreated,
    },
    Row {
        case: "a digit-bearing identifier never created",
        requested: "emu-logres-absent-42",
        resolution: Resolution::NeverCreated,
    },
    Row {
        case: "a single-letter identifier never created",
        requested: "q",
        resolution: Resolution::NeverCreated,
    },
    Row {
        case: "a mixed-case spelling never created",
        requested: "EMU-LogRes-Absent",
        resolution: Resolution::NeverCreated,
    },
    Row {
        case: "an identifier whose instance was deleted",
        requested: "emu-logres-deleted",
        resolution: Resolution::Deleted,
    },
    Row {
        case: "an instance held in another region only",
        requested: "emu-logres-elsewhere",
        resolution: Resolution::AnotherRegionOnly,
    },
];

/// `CreateDBInstance` in `region` with the three members it requires.
async fn create_instance(router: &MultiServiceRouter, region: &str, identifier: &str) {
    expect_ok(
        &format!("create {identifier} in {region}"),
        call_in(
            router,
            region,
            "CreateDBInstance",
            &[
                ("DBInstanceIdentifier", identifier),
                ("Engine", ENGINE),
                ("DBInstanceClass", "db.m5.large"),
            ],
        )
        .await,
    );
}

/// Poll `DescribeDBInstances` in `region` until the instance reports `available`: the
/// transition the create owes, materialised by a reporting call and nothing else.
async fn poll_until_available(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    identifier: &str,
) {
    poll_until(
        case,
        &format!("{identifier} reaching available in {region}"),
        || read_instance_in(router, region, identifier),
        |status, body| {
            status == 200 && optional_text(body, "DBInstanceStatus").as_deref() == Some("available")
        },
    )
    .await;
}

/// Put the row's identifier in the state the row describes.
async fn arrange(router: &MultiServiceRouter, row: &Row) {
    match row.resolution {
        Resolution::NeverCreated => {}
        Resolution::Deleted => {
            create_instance(router, REGION, row.requested).await;
            poll_until_available(router, row.case, REGION, row.requested).await;
            expect_ok(
                row.case,
                call(
                    router,
                    "DeleteDBInstance",
                    &[
                        ("DBInstanceIdentifier", row.requested),
                        ("SkipFinalSnapshot", "true"),
                    ],
                )
                .await,
            );
            // The removal is the transition the accepted delete OWES, so the read below is
            // made once the instance has actually left the store — waited for by polling
            // the report, never by sleeping.
            poll_until(
                row.case,
                &format!("{} leaving the store", row.requested),
                || read_instance(router, row.requested),
                |status, _| status == 404,
            )
            .await;
        }
        Resolution::AnotherRegionOnly => {
            create_instance(router, OTHER_REGION, row.requested).await;
            poll_until_available(router, row.case, OTHER_REGION, row.requested).await;
        }
    }
}

/// One request shape a download is read with: the `LogFileName` it names — `None` for the
/// file an instance of [`ENGINE`] writes — and the members beyond it that the rule holds
/// regardless of.
struct DownloadShape {
    shape: &'static str,
    log_file: Option<&'static str>,
    extra: &'static [(&'static str, &'static str)],
}

/// The shapes span the other members: the file such an instance writes, a file nothing
/// writes, a page token nothing issued, and a bounded portion.
const DOWNLOAD_SHAPES: &[DownloadShape] = &[
    DownloadShape {
        shape: "the file the engine writes",
        log_file: None,
        extra: &[],
    },
    DownloadShape {
        shape: "a file nothing writes",
        log_file: Some(UNWRITTEN_LOG_FILE),
        extra: &[],
    },
    DownloadShape {
        shape: "a marker no prior request issued",
        log_file: None,
        extra: &[("Marker", "not-a-marker")],
    },
    DownloadShape {
        shape: "a bounded number of lines",
        log_file: None,
        extra: &[("NumberOfLines", "1")],
    },
];

/// The request shapes `DescribeDBLogFiles` is read with.
const DESCRIBE_SHAPES: &[(&str, &[(&str, &str)])] = &[
    ("the plain read", &[]),
    ("a narrowing file name", &[("FilenameContains", "error")]),
    ("a bounded page", &[("MaxRecords", "20")]),
];

#[tokio::test]
async fn a_log_read_refuses_an_identifier_that_holds_no_db_instance_in_the_region() {
    let router = emulator();
    let written_log_file = engine_log::log_file_name(ENGINE);

    for row in ROWS {
        arrange(&router, row).await;
        // The one sentence this service words the class with, naming the spelling the
        // request carried.
        let expected_message = format!("DBInstance {} not found.", row.requested);

        for (shape, extra) in DESCRIBE_SHAPES {
            let case = format!("DescribeDBLogFiles, {}, {shape}", row.case);
            let mut params = vec![("DBInstanceIdentifier", row.requested)];
            params.extend_from_slice(extra);
            let body = expect_error(
                &case,
                call(&router, "DescribeDBLogFiles", &params).await,
                404,
                DB_INSTANCE_NOT_FOUND,
            );
            let message = text(&body, "Message");
            assert_eq!(
                message, expected_message,
                "case {case:?}: expected {expected_message:?}, got {message:?} in {body}"
            );
            assert_eq!(
                text(&body, "Type"),
                "Sender",
                "case {case:?}: the refusal is the caller's, got {body}"
            );
            assert!(
                texts(&body, "LogFileName").is_empty(),
                "case {case:?}: no file of any instance may be reported, got {body}"
            );
        }

        for row_shape in DOWNLOAD_SHAPES {
            let DownloadShape {
                shape,
                log_file,
                extra,
            } = row_shape;
            let case = format!("DownloadDBLogFilePortion, {}, {shape}", row.case);
            let mut params = vec![
                ("DBInstanceIdentifier", row.requested),
                ("LogFileName", log_file.unwrap_or(written_log_file)),
            ];
            params.extend_from_slice(extra);
            let body = expect_error(
                &case,
                call(&router, "DownloadDBLogFilePortion", &params).await,
                404,
                DB_INSTANCE_NOT_FOUND,
            );
            let message = text(&body, "Message");
            assert_eq!(
                message, expected_message,
                "case {case:?}: expected {expected_message:?}, got {message:?} in {body}"
            );
            assert_eq!(
                text(&body, "Type"),
                "Sender",
                "case {case:?}: the refusal is the caller's, got {body}"
            );
            assert!(
                texts(&body, "LogFileData").is_empty(),
                "case {case:?}: no portion of any log may be served, got {body}"
            );
        }
    }
}

#[tokio::test]
async fn a_log_read_serves_the_instance_the_region_does_hold() {
    let router = emulator();
    let written_log_file = engine_log::log_file_name(ENGINE);
    let identifier = "emu-logres-held";
    create_instance(&router, REGION, identifier).await;
    poll_until_available(&router, "the held instance", REGION, identifier).await;

    // Amazon RDS folds a DB instance identifier to lower case ("DBInstanceIdentifier …
    // Stored as a lowercase string"), so either spelling of the one the create carried
    // resolves to the same instance — which is what makes the refusal above the answer of
    // the RESOLUTION rather than of the spelling.
    for (case, spelling) in [
        ("the spelling the create carried", identifier),
        ("an upper-case spelling of it", "EMU-LOGRES-HELD"),
    ] {
        let listed = expect_ok(
            case,
            call(
                &router,
                "DescribeDBLogFiles",
                &[("DBInstanceIdentifier", spelling)],
            )
            .await,
        );
        let files = texts(&listed, "LogFileName");
        assert!(
            files.contains(&written_log_file.to_string()),
            "case {case:?}: expected {written_log_file:?} among the instance's files, got \
             {files:?}"
        );
        let served = expect_ok(
            case,
            call(
                &router,
                "DownloadDBLogFilePortion",
                &[
                    ("DBInstanceIdentifier", spelling),
                    ("LogFileName", written_log_file),
                ],
            )
            .await,
        );
        let data = text(&served, "LogFileData");
        assert!(
            data.contains(identifier),
            "case {case:?}: the portion is this instance's own record, got {data:?}"
        );
    }

    // A file this instance does not write is the OTHER miss of the same read, and it is
    // reached only because the instance resolved: a different error class entirely.
    let refused = expect_error(
        "a file the held instance does not write",
        call(
            &router,
            "DownloadDBLogFilePortion",
            &[
                ("DBInstanceIdentifier", identifier),
                ("LogFileName", UNWRITTEN_LOG_FILE),
            ],
        )
        .await,
        404,
        "DBLogFileNotFoundFault",
    );
    assert!(
        text(&refused, "Message").contains(UNWRITTEN_LOG_FILE),
        "the file miss names the file the request asked for, got {refused}"
    );
}
