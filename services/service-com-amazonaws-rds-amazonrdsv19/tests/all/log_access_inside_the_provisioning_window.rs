// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what the two DB log reads — `DescribeDBLogFiles` and
//! `DownloadDBLogFilePortion` — answer for an instance the Region DOES hold but which has
//! not finished being created.
//!
//! Every call goes over the wire: the service is mounted on a `MultiServiceRouter` exactly
//! as the production router mounts it (`crate::db_instances::emulator_with_engine`), each
//! request is a real awsQuery form POST carrying the SigV4 credential scope the router keys
//! on, and every assertion reads the XML the emulator answered with.
//!
//! The rule under test: a log read resolves the DB instance BEFORE anything of the file it
//! was asked for, so an instance inside its provisioning window is answered by that
//! instance's own `DBInstanceNotFoundFault` — 404, `DBInstanceNotFound`, worded about the
//! instance (`DBInstance <identifier> not found.`) rather than with the control plane's
//! not-found sentence an identifier that resolves to NOTHING gets — whichever other members
//! the request carries, and the refusal CLEARS once the transition the create owes has
//! settled, which is what makes the two sentences a retry signal: the instance-scoped one
//! says wait, the control-plane one says stop.
//!
//! What the sibling suites pin, and this one does not repeat: which sentence each miss gets
//! (`log_access_not_found_sentences`) and the same resolution-before-the-file order for an
//! identifier that resolves to no instance at all (`log_read_instance_resolution`). The rows
//! here differ along what is left: the engine families whose log file names differ, the
//! spelling the request carries the identifier in, which of the two reads is called, and the
//! request shapes — a file the instance would write, a file nothing writes, a page token
//! nothing issued, a narrowing member, a bounded page — none of which may change the answer
//! while the instance is still coming up.
//!
//! Nothing here waits on the clock: the provisioning window is held open through this
//! service's own store rather than by racing it, and the window is closed by making the
//! transition the create already owes due and then polling a REPORTING call through the one
//! poll loop this test target has (`crate::lifecycle_poll_loops::poll_until`), so what ends
//! a wait is the CONDITION.

use std::sync::Arc;
use std::time::Instant;

use service_com_amazonaws_rds_amazonrdsv19::state as st;
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{
    call, emulator_with_engine, expect_error, expect_ok, optional_text, text, texts, REGION,
};
use crate::lifecycle_poll_loops::poll_until;
use crate::log_access_not_found_sentences::hold_in_creating;

/// The HTTP status and the wire error `<Code>` the model binds `DBInstanceNotFoundFault` to
/// on both reads (`smithy_query(query="operation",
/// operation="DescribeDBLogFiles,DownloadDBLogFilePortion")`: httpError 404, `awsQueryError`
/// code `DBInstanceNotFound`).
const NOT_FOUND_STATUS: u16 = 404;
const NOT_FOUND_CODE: &str = "DBInstanceNotFound";

/// The compute class every instance below is ordered with.
const CLASS: &str = "db.t3.micro";

/// A log file name NO DB instance of this service writes, for the request shape that would
/// trip the log-file miss had the instance resolved.
const UNWRITTEN_LOG_FILE: &str = "error/not-a-log-file.log";

/// One row: the engine the instance runs, the identifier it is created under, and the
/// spelling the log read carries.
struct Row {
    case: &'static str,
    engine: &'static str,
    /// The identifier the create carries, which this service folds to lower case.
    created: &'static str,
    /// The spelling the log read carries, which is what the refusal must name.
    requested: &'static str,
}

/// The rows differ along the engine family (whose log file names differ) and the spelling
/// the read carries the identifier in.
const ROWS: &[Row] = &[
    Row {
        case: "a postgres instance still coming up",
        engine: "postgres",
        created: "emu-logwindow-pg",
        requested: "emu-logwindow-pg",
    },
    Row {
        case: "a mysql instance still coming up",
        engine: "mysql",
        created: "emu-logwindow-my",
        requested: "emu-logwindow-my",
    },
    Row {
        case: "a postgres instance still coming up, read in mixed case",
        engine: "postgres",
        created: "emu-logwindow-case",
        requested: "EMU-LogWindow-Case",
    },
];

/// The request shapes `DescribeDBLogFiles` is read with: the plain read, a narrowing member,
/// a bounded page, and a page token nothing issued.
const DESCRIBE_SHAPES: &[(&str, &[(&str, &str)])] = &[
    ("the plain read", &[]),
    ("a narrowing file name", &[("FilenameContains", "error")]),
    ("a narrowing size", &[("FileSize", "0")]),
    ("a bounded page", &[("MaxRecords", "20")]),
    ("a marker no prior request issued", &[("Marker", "nope")]),
];

/// One request shape a download is read with: the `LogFileName` it names — `None` for the
/// file an instance of the row's engine writes — and the members beyond it.
struct DownloadShape {
    shape: &'static str,
    log_file: Option<&'static str>,
    extra: &'static [(&'static str, &'static str)],
}

/// The shapes span the other members: the file the engine writes, a file nothing writes, a
/// page token nothing issued, and a bounded portion.
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
        extra: &[("Marker", "nope")],
    },
    DownloadShape {
        shape: "a bounded number of lines",
        log_file: None,
        extra: &[("NumberOfLines", "1")],
    },
];

/// The sentence a read that FOUND the instance but cannot reach into it yet answers with,
/// spelled out here rather than taken from the code under test.
fn instance_scoped_sentence(requested: &str) -> String {
    format!("DBInstance {requested} not found.")
}

/// `CreateDBInstance` for one row, in [`REGION`].
async fn create_instance(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
    engine: &str,
) {
    expect_ok(
        case,
        call(
            router,
            "CreateDBInstance",
            &[
                ("DBInstanceIdentifier", identifier),
                ("Engine", engine),
                ("DBInstanceClass", CLASS),
                ("AllocatedStorage", "20"),
                ("MasterUsername", "emuadmin"),
                ("MasterUserPassword", "a-strong-password-1"),
            ],
        )
        .await,
    );
}

/// Make the transition the create already owes `identifier` DUE, so the next call
/// materialises it.
///
/// This changes no transition and invents none: it is the same `creating` -> `available`
/// change the create staged, brought to the instant this test is at, which is how the
/// provisioning window is closed without waiting out a span.
fn close_the_provisioning_window(
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    identifier: &str,
) {
    let state = st::service_state(&**engine).expect("this service's store");
    let mut store = state.lock();
    let record = store
        .db_instances
        .get_mut(&(REGION.to_string(), identifier.to_string()))
        .unwrap_or_else(|| panic!("case {case:?}: the create must have stored {identifier}"));
    let pending = record
        .pending_status
        .as_mut()
        .unwrap_or_else(|| panic!("case {case:?}: the create must owe {identifier} a transition"));
    pending.due_at = Instant::now();
}

/// Assert one refused read: the modelled fault, the instance-scoped sentence naming the
/// spelling the request carried, and nothing of any log in the body.
fn expect_instance_scoped_refusal(case: &str, body: &str, requested: &str) {
    let expected = instance_scoped_sentence(requested);
    let message = text(body, "Message");
    assert_eq!(
        message, expected,
        "case {case:?}: expected the sentence {expected:?}, got {message:?} in {body}"
    );
    assert_eq!(
        text(body, "Type"),
        "Sender",
        "case {case:?}: the refusal is the caller's, got {body}"
    );
    assert!(
        texts(body, "LogFileName").is_empty() && texts(body, "LogFileData").is_empty(),
        "case {case:?}: no file and no portion may be reported, got {body}"
    );
}

/// Rule, first half: while the instance is inside its provisioning window both reads answer
/// its own `DBInstanceNotFoundFault`, worded about that instance, whichever other members
/// the request carries — the file is never reached, because the instance is not.
#[tokio::test]
async fn a_log_read_of_an_instance_still_coming_up_names_that_instance() {
    for row in ROWS {
        // A store per row, so one row's instance cannot resolve another's identifier.
        let (router, engine) = emulator_with_engine();
        create_instance(&router, row.case, row.created, row.engine).await;
        hold_in_creating(&engine, row.case, row.created);
        let written_log_file = st::engine_log::log_file_name(row.engine);

        for (shape, extra) in DESCRIBE_SHAPES {
            let case = format!("DescribeDBLogFiles, {}, {shape}", row.case);
            let mut params = vec![("DBInstanceIdentifier", row.requested)];
            params.extend_from_slice(extra);
            let body = expect_error(
                &case,
                call(&router, "DescribeDBLogFiles", &params).await,
                NOT_FOUND_STATUS,
                NOT_FOUND_CODE,
            );
            expect_instance_scoped_refusal(&case, &body, row.requested);
        }

        for download in DOWNLOAD_SHAPES {
            let case = format!("DownloadDBLogFilePortion, {}, {}", row.case, download.shape);
            let mut params = vec![
                ("DBInstanceIdentifier", row.requested),
                (
                    "LogFileName",
                    download.log_file.unwrap_or(written_log_file),
                ),
            ];
            params.extend_from_slice(download.extra);
            let body = expect_error(
                &case,
                call(&router, "DownloadDBLogFilePortion", &params).await,
                NOT_FOUND_STATUS,
                NOT_FOUND_CODE,
            );
            expect_instance_scoped_refusal(&case, &body, row.requested);
        }
    }
}

/// Rule, second half: the same reads that were refused are SERVED once the transition the
/// create owes has settled, so the instance-scoped sentence is a refusal a client clears by
/// waiting — which is the whole reason it is worded differently from the control plane's
/// not-found sentence.
#[tokio::test]
async fn the_refusal_clears_once_the_instance_has_finished_being_created() {
    for row in ROWS {
        let (router, engine) = emulator_with_engine();
        create_instance(&router, row.case, row.created, row.engine).await;
        hold_in_creating(&engine, row.case, row.created);
        let written_log_file = st::engine_log::log_file_name(row.engine);

        let refused = expect_error(
            row.case,
            call(
                &router,
                "DescribeDBLogFiles",
                &[("DBInstanceIdentifier", row.requested)],
            )
            .await,
            NOT_FOUND_STATUS,
            NOT_FOUND_CODE,
        );
        expect_instance_scoped_refusal(row.case, &refused, row.requested);

        close_the_provisioning_window(&engine, row.case, row.created);
        // Wait for the CONDITION a client waits on — the read it was refused answering —
        // through the shared poll loop over a reporting call.
        let (listed, _polls) = poll_until(
            row.case,
            "the log listing the instance was refused",
            || async {
                call(
                    &router,
                    "DescribeDBLogFiles",
                    &[("DBInstanceIdentifier", row.requested)],
                )
                .await
            },
            |status, _| status == 200,
        )
        .await;
        let files = texts(&listed, "LogFileName");
        assert!(
            files.iter().any(|name| name == written_log_file),
            "case {:?}: expected the instance's own log {written_log_file:?} to be listed, got \
             {files:?}",
            row.case
        );

        let served = expect_ok(
            row.case,
            call(
                &router,
                "DownloadDBLogFilePortion",
                &[
                    ("DBInstanceIdentifier", row.requested),
                    ("LogFileName", written_log_file),
                ],
            )
            .await,
        );
        let data = optional_text(&served, "LogFileData").unwrap_or_default();
        assert!(
            !data.is_empty(),
            "case {:?}: expected the lines this service wrote for the instance, got {served}",
            row.case
        );
    }
}
