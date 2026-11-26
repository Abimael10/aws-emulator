// SPDX-License-Identifier: CC0-1.0

//! Which SENTENCE the two log-access operations — `DescribeDBLogFiles` and
//! `DownloadDBLogFilePortion` — word `DBInstanceNotFoundFault` with.
//!
//! The rule under test: both operations answer ONE fault for a `DBInstanceIdentifier` they
//! cannot read a log out of — code `DBInstanceNotFound`, HTTP 404 — so the sentence is the
//! only thing that tells a caller which miss it hit, and it tells them apart. An identifier
//! that resolves to an instance the service HOLDS but cannot serve log access for yet, one
//! that has not finished being created, is answered about that instance:
//! `DBInstance <identifier> not found.`. An identifier that resolves to NO instance in the
//! Region the request was signed for is answered with this front door's control-plane
//! not-found sentence instead: `DBInstance not found: <identifier>`. Either way the
//! identifier is the one the REQUEST carried, in the spelling it carried it in.
//!
//! Why it matters: a client waiting for an instance to come up retries the first sentence and
//! must stop on the second, and the code and the status give it nothing to branch on.
//!
//! The rows span what the rule generalises over: which of the two operations is called, the
//! two engine families whose log file names differ, an identifier that was never created,
//! one created in ANOTHER Region (Amazon RDS is regional, so that is a miss here), an
//! instance of this Region that is still `creating`, and the spelling the request writes the
//! identifier in — a stored identifier is folded to lower case, so a mixed-case request
//! proves the sentence echoes what was SENT rather than the stored key.
//!
//! The still-creating rows hold the instance in `creating` through this service's own store
//! rather than by racing the clock, so nothing here depends on how fast the host runs; the
//! control test that reads a READY instance's logs waits for the condition through the shared
//! poll loop.

use std::sync::Arc;
use std::time::{Duration, Instant};

use service_com_amazonaws_rds_amazonrdsv19::state as st;
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{
    call_in, emulator_with_engine, expect_error, expect_ok, optional_text, texts, REGION,
};
use crate::lifecycle_poll_loops::{poll_until, read_instance};

/// Another Region, for the row whose instance exists but not where the request is signed for.
const OTHER_REGION: &str = "eu-west-1";

/// The HTTP status and the `awsQueryError` code both operations answer this fault with
/// (`smithy_query(query="operation", operation="DescribeDBLogFiles")`:
/// `DBInstanceNotFoundFault`, httpError 404, code `DBInstanceNotFound`).
const NOT_FOUND_STATUS: u16 = 404;
const NOT_FOUND_CODE: &str = "DBInstanceNotFound";

/// The compute class and storage every instance below is ordered with.
const CLASS: &str = "db.t3.micro";
const STORAGE: &str = "20";

/// The two operations that reach a DB instance's LOG FILES, which is the class this rule
/// holds for.
const LOG_ACCESS_CALLS: [&str; 2] = ["DescribeDBLogFiles", "DownloadDBLogFilePortion"];

/// How a row makes its identifier one the log-access calls cannot serve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Miss {
    /// No instance of that identifier was ever created, in any Region.
    NeverCreated,
    /// An instance of that identifier exists in [`OTHER_REGION`] and not in [`REGION`].
    AnotherRegion,
    /// An instance of that identifier exists in [`REGION`] and has not finished being
    /// created.
    StillCreating,
}

/// One row: the identifier the request carries, how the identifier misses, and the instance
/// to create for it.
struct MissCase {
    case: &'static str,
    /// The identifier the log-access request writes, in the spelling it writes it in.
    requested: &'static str,
    /// The identifier the instance (where the row creates one) is created under.
    created: &'static str,
    engine: &'static str,
    miss: Miss,
}

const MISS_CASES: &[MissCase] = &[
    MissCase {
        case: "an identifier that was never created",
        requested: "emu-logmiss-absent",
        created: "",
        engine: "postgres",
        miss: Miss::NeverCreated,
    },
    MissCase {
        case: "an identifier that was never created, written in mixed case",
        requested: "Emu-LogMiss-Absent-Two",
        created: "",
        engine: "postgres",
        miss: Miss::NeverCreated,
    },
    MissCase {
        case: "an instance of another Region",
        requested: "emu-logmiss-elsewhere",
        created: "emu-logmiss-elsewhere",
        engine: "mysql",
        miss: Miss::AnotherRegion,
    },
    MissCase {
        case: "a postgres instance that has not finished being created",
        requested: "emu-logmiss-creating",
        created: "emu-logmiss-creating",
        engine: "postgres",
        miss: Miss::StillCreating,
    },
    MissCase {
        case: "a mysql instance that has not finished being created",
        requested: "emu-logmiss-creating-my",
        created: "emu-logmiss-creating-my",
        engine: "mysql",
        miss: Miss::StillCreating,
    },
    MissCase {
        case: "an instance that has not finished being created, named in mixed case",
        requested: "EMU-LOGMISS-CREATING-CASE",
        created: "emu-logmiss-creating-case",
        engine: "postgres",
        miss: Miss::StillCreating,
    },
];

/// The sentence the control plane of this front door words an unresolvable DB instance with,
/// spelled out here rather than taken from the code under test.
fn control_plane_sentence(requested: &str) -> String {
    format!("DBInstance not found: {requested}")
}

/// The sentence a call that FOUND the instance but cannot reach into it yet answers with,
/// likewise spelled out here.
fn instance_scoped_sentence(requested: &str) -> String {
    format!("DBInstance {requested} not found.")
}

/// The members `action` needs beyond `DBInstanceIdentifier`.
fn log_access_members(action: &str, engine: &str) -> Vec<(&'static str, String)> {
    match action {
        // A download names the file it reads a portion of; the published name of this
        // engine's server log is the one a client would have listed first
        // (`crate::state::engine_log::log_file_name`).
        "DownloadDBLogFilePortion" => {
            vec![("LogFileName", st::engine_log::log_file_name(engine).to_string())]
        }
        _ => Vec::new(),
    }
}

/// `CreateDBInstance` for one row, in `region`.
async fn create_instance(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    identifier: &str,
    engine: &str,
) {
    expect_ok(
        case,
        call_in(
            router,
            region,
            "CreateDBInstance",
            &[
                ("DBInstanceIdentifier", identifier),
                ("Engine", engine),
                ("DBInstanceClass", CLASS),
                ("AllocatedStorage", STORAGE),
                ("MasterUsername", "emuadmin"),
                ("MasterUserPassword", "a-strong-password-1"),
            ],
        )
        .await,
    );
}

/// Hold the instance `identifier` in `creating` for as long as this suite runs, by pushing
/// the transition this service owes it well past the end of the test.
///
/// The transition itself is untouched — it is still owed, and still the one the create staged
/// — so the instance is in exactly the state a caller sees inside the provisioning window,
/// without the test racing the clock for it.
///
/// `pub(crate)` so the sibling suites of this one test target hold an instance inside its
/// provisioning window through this helper rather than growing a second copy of it.
pub(crate) fn hold_in_creating(
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    identifier: &str,
) {
    /// Longer than any test run, so the status cannot settle under the assertions below.
    const BEYOND_THIS_SUITE: Duration = Duration::from_secs(3_600);

    let state = st::service_state(&**engine).expect("this service's store");
    let mut store = state.lock();
    let record = store
        .db_instances
        .get_mut(&(REGION.to_string(), identifier.to_string()))
        .unwrap_or_else(|| panic!("case {case:?}: the create must have stored {identifier}"));
    assert_eq!(
        record.status, "creating",
        "case {case:?}: an instance just created is still creating"
    );
    record.pending_status = Some(st::PendingStatus {
        change: st::PendingChange::Status("available".to_string()),
        due_at: Instant::now() + BEYOND_THIS_SUITE,
    });
}

/// Rule: one fault, two sentences — the instance-scoped one for an identifier that resolves
/// to an instance still being created, and the control plane's not-found one for an
/// identifier that resolves to no instance in the request's Region.
#[tokio::test]
async fn log_access_tells_an_unready_instance_from_one_that_does_not_exist() {
    for row in MISS_CASES {
        // A store per row, so one row's instance cannot resolve another's identifier.
        let (router, engine) = emulator_with_engine();
        match row.miss {
            Miss::NeverCreated => {}
            Miss::AnotherRegion => {
                create_instance(&router, OTHER_REGION, row.case, row.created, row.engine).await;
            }
            Miss::StillCreating => {
                create_instance(&router, REGION, row.case, row.created, row.engine).await;
                hold_in_creating(&engine, row.case, row.created);
            }
        }
        let expected = if row.miss == Miss::StillCreating {
            instance_scoped_sentence(row.requested)
        } else {
            control_plane_sentence(row.requested)
        };

        for action in LOG_ACCESS_CALLS {
            let case = format!("{action} for {}", row.case);
            let mut members = vec![("DBInstanceIdentifier", row.requested.to_string())];
            members.extend(log_access_members(action, row.engine));
            let body = expect_error(
                &case,
                call_in(&router, REGION, action, &members).await,
                NOT_FOUND_STATUS,
                NOT_FOUND_CODE,
            );
            let reported = optional_text(&body, "Message");
            assert_eq!(
                reported.as_deref(),
                Some(expected.as_str()),
                "case {case:?}: expected the sentence {expected:?}, got {reported:?} in {body}"
            );
        }
    }
}

/// The other side of the same rule: an identifier that resolves to an instance log access CAN
/// reach is served its log files, so neither sentence is the answer for a resolvable one.
#[tokio::test]
async fn log_access_serves_an_instance_that_has_finished_being_created() {
    const IDENTIFIER: &str = "emu-logmiss-ready";
    const ENGINE: &str = "postgres";
    let case = "a postgres instance that has come up";
    let (router, _engine) = emulator_with_engine();
    create_instance(&router, REGION, case, IDENTIFIER, ENGINE).await;

    // Wait for the CONDITION the client waits on — the instance reporting itself available —
    // through the shared poll loop over a reporting call.
    poll_until(
        case,
        "the instance reaching available",
        || read_instance(&router, IDENTIFIER),
        |status, body| {
            status == 200 && optional_text(body, "DBInstanceStatus").as_deref() == Some("available")
        },
    )
    .await;

    let listed = expect_ok(
        case,
        call_in(
            &router,
            REGION,
            "DescribeDBLogFiles",
            &[("DBInstanceIdentifier", IDENTIFIER)],
        )
        .await,
    );
    let files = texts(&listed, "LogFileName");
    let published = st::engine_log::log_file_name(ENGINE);
    assert!(
        files.iter().any(|name| name == published),
        "case {case:?}: expected the instance's own log {published:?} to be listed, got \
         {files:?}"
    );

    let portion = expect_ok(
        case,
        call_in(
            &router,
            REGION,
            "DownloadDBLogFilePortion",
            &[
                ("DBInstanceIdentifier", IDENTIFIER),
                ("LogFileName", published),
            ],
        )
        .await,
    );
    let data = optional_text(&portion, "LogFileData").unwrap_or_default();
    assert!(
        !data.is_empty(),
        "case {case:?}: expected the lines this service wrote for the instance, got {portion}"
    );
}
