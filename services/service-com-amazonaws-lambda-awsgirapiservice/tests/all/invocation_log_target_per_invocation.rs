// SPDX-License-Identifier: CC0-1.0

//! WHICH destination one invocation's log is delivered to, WHOSE role decides that delivery,
//! and WHICH of a handler's own records it carries.
//!
//! RULE: Lambda reads where a function logs, and at which level of detail, off the
//! `LoggingConfig` the function carries AT THE INSTANT the invocation is accepted, and makes the
//! writes of that one delivery as that function's own execution role — so three things follow
//! from one reading:
//!
//! * the destination is "the name of the Amazon CloudWatch log group the function sends logs
//!   to", and "by default, Lambda functions send logs to a default log group named
//!   `/aws/lambda/<function name>`"
//!   (`smithy_query(query="doc",shape="LoggingConfig",member="LogGroup")`), so a configuration
//!   change redirects the invocations that FOLLOW it and adds nothing to where the earlier ones
//!   went;
//! * the decision is per function and per invocation, so what one function's role withholds
//!   decides what that function leaves behind and nothing about another function's delivery;
//! * "Lambda only sends application logs at the selected level of detail and lower"
//!   (`smithy_query(query="doc",shape="LoggingConfig",member="ApplicationLogLevel")`), read off
//!   the `level` member of the RECORD — which is where a handler that wrote its own JSON object
//!   states it — and a record naming no level this reading places is sent at the level AWS
//!   documents for output written straight to the handler's standard streams, while the
//!   PLATFORM's records of the same run are a log of the other kind, filtered at the other level
//!   and rendered as the structured records the selected `LogFormat` states them in.
//!
//! The rows differ along what the rule generalises over: which FORM the destination took before
//! and after the change (the default group, a group the configuration names, and back to the
//! default), in which ORDER the delivered and the withheld function ran, and which LEVEL the
//! function filters its application records at (every level the records below are stated at, and
//! one above all of them) against records stating a level of each case — one the ladder places,
//! one it does not, and none at all.
//!
//! Every row drives real HTTP through the production `MultiServiceRouter` with Lambda,
//! CloudWatch Logs and IAM mounted on ONE engine — the harness
//! [`crate::invocation_log_delivery`] pins the delivery itself with, reused rather than copied —
//! so what is asserted is what a client reads back through CloudWatch Logs' own
//! `DescribeLogGroups` and `FilterLogEvents`, never a handler's Rust.
//!
//! Every test here RUNS handlers, so each takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::{allow, given_role};
use crate::event_source_mappings::{await_function_active, call};
use crate::invocation_log_delivery::{
    create_function_logging, emulator, groups, invocation_request_carrying, record_text, searched,
    structured_record, Created, FUNCTION_ERROR_HEADER, LOGS, LOG_WRITES, NODE_RUNTIME,
    PYTHON_RUNTIME,
};

/// Where `CreateFunction` is bound, and the collection `UpdateFunctionConfiguration` sits under.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The prefix of every DEFAULT function log group, as `LoggingConfig$LogGroup` documents it
/// ("a default log group named `/aws/lambda/<function name>`").
const DEFAULT_PREFIX: &str = "/aws/lambda/";

/// The record the plain-text framing closes a run with, which is one per run in a group.
const REPORT_PREFIX: &str = "REPORT RequestId: ";

/// The node handler every destination row and both twins run: it writes the marker the event
/// carries, so one function's records are told apart from another's and one invocation's from
/// the next.
const MARKING_HANDLER: &str = "exports.handler = async (event) => {\n    \
                               console.log('logged ' + event.marker);\n    return { logged: \
                               event.marker };\n};\n";

/// `Invoke` the function `name` with `marker` as its event, asking for the log tail, and answer
/// that tail. The invocation itself has to be clean — HTTP 200, no `FunctionError`, and the
/// handler's own answer — because what became of its log is never an invocation failure; what the
/// delivery left behind is asserted on the GROUP, not here.
async fn invoked(router: &MultiServiceRouter, case: &str, name: &str, marker: &str) -> String {
    let event = json!({ "marker": marker }).to_string();
    let response = router
        .dispatch(invocation_request_carrying(
            name,
            &[("x-amz-log-type", "Tail")],
            event.as_bytes(),
        ))
        .await;
    let (parts, body) = response.into_parts();
    assert_eq!(
        parts.status.as_u16(),
        200,
        "case {case:?}: expected HTTP 200 from Invoke of {name:?} with {marker:?}, got {} {}",
        parts.status,
        String::from_utf8_lossy(&body)
    );
    let failed = parts
        .headers
        .get(FUNCTION_ERROR_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    assert_eq!(
        failed, None,
        "case {case:?}: expected no FunctionError from the run of {marker:?}, got {failed:?} with \
         {}",
        String::from_utf8_lossy(&body)
    );
    let encoded = parts
        .headers
        .get("x-amz-log-result")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_else(|| panic!("case {case:?}: expected a log tail on the answer of {name:?}"))
        .to_string();
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(&encoded)
        .unwrap_or_else(|error| panic!("case {case:?}: the log tail is base64: {error}"));
    String::from_utf8(decoded)
        .unwrap_or_else(|error| panic!("case {case:?}: the log tail is text: {error}"))
}

/// `UpdateFunctionConfiguration` on `name`, replacing its whole `LoggingConfig` with `logging`,
/// and wait until the update has settled — which is the call a client repoints a function's log
/// group with. A read of the function then has to report `expected_group` as where it logs: the
/// destination the next invocation is delivered to is the one a client reads off the function.
async fn repointed(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    logging: &Value,
    expected_group: &str,
) {
    let updated = call(
        router,
        "PUT",
        &format!("{FUNCTIONS_URI}/{name}/configuration"),
        &json!({ "LoggingConfig": logging }),
    )
    .await;
    assert_eq!(
        updated.status,
        200,
        "case {case:?}: expected 200 from UpdateFunctionConfiguration on {name:?}, got {} {}",
        updated.status,
        updated.text()
    );
    await_function_active(router, case, name).await;
    // What a READ of the function reports is the destination the next invocation is delivered
    // to, so the update is observed where a client observes it before anything is invoked.
    let read = call(
        router,
        "GET",
        &format!("{FUNCTIONS_URI}/{name}/configuration"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: expected 200 from GetFunctionConfiguration on {name:?}, got {} {}",
        read.status,
        read.text()
    );
    assert_eq!(
        read.json()["LoggingConfig"]["LogGroup"],
        json!(expected_group),
        "case {case:?}: expected {name:?} to log to {expected_group:?} after the change, got {}",
        read.text()
    );
}

/// One row of the destination table: where the function logged before the change and where it
/// logs after it.
struct Redirect {
    case: &'static str,
    /// The function the row creates, which is also what names its default group.
    function: &'static str,
    /// The group its `LoggingConfig` names at creation, or `None` to take the default.
    before: Option<&'static str>,
    /// The `LoggingConfig` the update replaces it with.
    after: fn() -> Value,
    /// The group that configuration points it at, or `None` for the default again.
    after_group: Option<&'static str>,
}

/// The marker the invocation BEFORE the change carries, and the one the invocation after it
/// carries.
const BEFORE_MARKER: &str = "before-the-change";
const AFTER_MARKER: &str = "after-the-change";

fn redirect_rows() -> Vec<Redirect> {
    vec![
        Redirect {
            case: "a function on its default group, repointed at a group it names",
            function: "log-redirect-from-default",
            before: None,
            after: || json!({ "LogGroup": "/aws/lambda/redirected-elsewhere" }),
            after_group: Some("/aws/lambda/redirected-elsewhere"),
        },
        Redirect {
            case: "a function on a named group, repointed at another one outside the default \
                   prefix",
            function: "log-redirect-between-named",
            before: Some("/scenario/redirect/first"),
            after: || json!({ "LogGroup": "/scenario/redirect/second" }),
            after_group: Some("/scenario/redirect/second"),
        },
        Redirect {
            case: "a function on a named group, repointed at no group at all",
            function: "log-redirect-back-to-default",
            before: Some("/scenario/redirect/named"),
            after: || json!({}),
            after_group: None,
        },
    ]
}

/// RULE: an invocation's records go to the group the function's `LoggingConfig` names when that
/// invocation is accepted, so repointing the group splits a function's history at the change —
/// the records of the runs before it stay where they were and gain nothing, and the runs after it
/// are delivered to the new destination alone.
#[tokio::test]
async fn repointing_the_log_group_redirects_the_invocations_that_follow_it() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    for row in redirect_rows() {
        let case = row.case;
        // One emulator per row: the function, the role and the groups a row reads are its own.
        let router = emulator();
        let role = given_role(
            &router,
            case,
            &format!("{}-writer", row.function),
            Some(json!([allow(LOGS, &LOG_WRITES, "*")])),
        )
        .await;
        let default_group = format!("{DEFAULT_PREFIX}{}", row.function);
        let before_group = row.before.unwrap_or(&default_group).to_string();
        let after_group = row.after_group.unwrap_or(&default_group).to_string();
        assert_ne!(
            before_group, after_group,
            "case {case:?}: the row must repoint the function at another group"
        );
        create_function_logging(
            &router,
            &Created {
                case,
                name: row.function,
                source: MARKING_HANDLER,
                runtime: NODE_RUNTIME,
                role: &role,
                log_group: row.before,
            },
            None,
        )
        .await;

        // 1. One invocation under the configuration as created.
        let before_tail = invoked(&router, case, row.function, BEFORE_MARKER).await;
        assert!(
            before_tail.contains(BEFORE_MARKER),
            "case {case:?}: expected the first run's own log on its answer, got {before_tail:?}"
        );
        let held_before = searched(&router, case, &before_group).await;
        assert!(
            held_before
                .iter()
                .any(|message| message.contains(BEFORE_MARKER)),
            "case {case:?}: expected {before_group:?} to hold the first run's record, got \
             {held_before:?}"
        );

        // 2. The change, which a read of the function reports before anything else runs.
        repointed(
            &router,
            case,
            row.function,
            &(row.after)(),
            &after_group,
        )
        .await;

        // 3. One invocation under the configuration as changed.
        let after_tail = invoked(&router, case, row.function, AFTER_MARKER).await;
        assert!(
            after_tail.contains(AFTER_MARKER),
            "case {case:?}: expected the second run's own log on its answer, got {after_tail:?}"
        );

        // 4. The new destination holds the second run and nothing of the first.
        let held_after = searched(&router, case, &after_group).await;
        assert!(
            held_after
                .iter()
                .any(|message| message.contains(AFTER_MARKER)),
            "case {case:?}: expected {after_group:?} to hold the second run's record, got \
             {held_after:?}"
        );
        assert!(
            !held_after
                .iter()
                .any(|message| message.contains(BEFORE_MARKER)),
            "case {case:?}: expected nothing of the first run in {after_group:?}, got \
             {held_after:?}"
        );
        let after_reports = held_after
            .iter()
            .filter(|message| message.contains(REPORT_PREFIX))
            .count();
        assert_eq!(
            after_reports, 1,
            "case {case:?}: expected exactly one run reported in {after_group:?}, got \
             {held_after:?}"
        );

        // 5. The destination the first run went to is untouched by the second: the same records
        //    it held before the change, and no record of the run that followed it.
        let still_held = searched(&router, case, &before_group).await;
        assert_eq!(
            still_held, held_before,
            "case {case:?}: expected {before_group:?} to hold exactly what it held before the \
             second run, got {still_held:?} against {held_before:?}"
        );
        assert!(
            !still_held
                .iter()
                .any(|message| message.contains(AFTER_MARKER)),
            "case {case:?}: expected nothing of the second run in {before_group:?}, got \
             {still_held:?}"
        );
        let before_reports = still_held
            .iter()
            .filter(|message| message.contains(REPORT_PREFIX))
            .count();
        assert_eq!(
            before_reports, 1,
            "case {case:?}: expected exactly one run reported in {before_group:?}, got \
             {still_held:?}"
        );
    }
}

/// The markers the two twins' runs carry, one each.
const GRANTED_MARKER: &str = "granted-twins-run";
const WITHHELD_MARKER: &str = "withheld-twins-run";

/// RULE: the delivery of one invocation is judged against that function's own execution role, so
/// a function whose role carries no logs grant leaves no group of its own behind while its twin —
/// invoked in the same emulator, before it or after it — is delivered in full. A withheld write is
/// never an invocation failure: the run still answers and still reports its own log tail.
#[tokio::test]
async fn one_functions_withheld_delivery_leaves_its_twins_untouched() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    // (case, whether the function whose role withholds the writes is invoked FIRST)
    let orders: &[(&str, bool)] = &[
        ("the withheld twin invoked first", true),
        ("the granted twin invoked first", false),
    ];
    for (index, (case, withheld_first)) in orders.iter().enumerate() {
        let router = emulator();
        let granted_role = given_role(
            &router,
            case,
            &format!("twin-granted-{index}"),
            Some(json!([allow(LOGS, &LOG_WRITES, "*")])),
        )
        .await;
        // A role that carries a grant of its own but no logs grant: what it may do says nothing
        // about CloudWatch Logs, which is what leaves the group uncreated.
        let withheld_role = given_role(
            &router,
            case,
            &format!("twin-withheld-{index}"),
            Some(json!([allow("s3", &["GetObject"], "*")])),
        )
        .await;
        let granted = format!("twin-granted-{index}");
        let withheld = format!("twin-withheld-{index}");
        let granted_group = format!("{DEFAULT_PREFIX}{granted}");
        let withheld_group = format!("{DEFAULT_PREFIX}{withheld}");
        for (name, role) in [
            (granted.as_str(), granted_role.as_str()),
            (withheld.as_str(), withheld_role.as_str()),
        ] {
            create_function_logging(
                &router,
                &Created {
                    case,
                    name,
                    source: MARKING_HANDLER,
                    runtime: NODE_RUNTIME,
                    role,
                    log_group: None,
                },
                None,
            )
            .await;
        }

        let mut runs: Vec<(&str, &str)> = vec![
            (withheld.as_str(), WITHHELD_MARKER),
            (granted.as_str(), GRANTED_MARKER),
        ];
        if !*withheld_first {
            runs.reverse();
        }
        for (name, marker) in runs {
            // Whatever became of its log, each run answers and reports its own log tail.
            let tail = invoked(&router, case, name, marker).await;
            assert!(
                tail.contains(marker),
                "case {case:?}: expected the run of {name:?} to report its own log, got {tail:?}"
            );
        }

        // The granted twin is delivered in full: its own group, holding its own record and
        // exactly one report of the one run it made.
        let listed = groups(&router, case, &granted_group).await;
        assert_eq!(
            listed,
            vec![granted_group.clone()],
            "case {case:?}: expected the granted twin's group {granted_group:?}, got {listed:?}"
        );
        let held = searched(&router, case, &granted_group).await;
        assert!(
            held.iter().any(|message| message.contains(GRANTED_MARKER)),
            "case {case:?}: expected {granted_group:?} to hold the granted twin's record, got \
             {held:?}"
        );
        assert_eq!(
            held.iter()
                .filter(|message| message.contains(REPORT_PREFIX))
                .count(),
            1,
            "case {case:?}: expected exactly one run reported in {granted_group:?}, got {held:?}"
        );
        // ... and nothing of the twin whose role withheld the writes, whichever ran first.
        assert!(
            !held.iter().any(|message| message.contains(WITHHELD_MARKER)),
            "case {case:?}: expected nothing of the withheld twin in {granted_group:?}, got \
             {held:?}"
        );

        // The withheld twin leaves no group at all, so nothing of its run is there to read.
        let absent = groups(&router, case, &withheld_group).await;
        assert!(
            absent.is_empty(),
            "case {case:?}: expected no group {withheld_group:?} for a role short of the logs \
             grant, got {absent:?}"
        );
    }
}

/// The python handler whose own records are JSON objects it wrote itself: one at a level the
/// ladder places, one at a level it does not, and one stating no level at all. `print()` reaches
/// the invocation's log as it was written, so each line below is one delivered record.
const SELF_STRUCTURED_HANDLER: &str = concat!(
    "LINES = (\n",
    "    '{\"level\": \"debug\", \"msg\": \"self-debug\"}',\n",
    "    '{\"msg\": \"self-levelless\"}',\n",
    "    '{\"level\": \"bogus\", \"msg\": \"self-bogus\"}',\n",
    "    '{\"level\": \"warn\", \"msg\": \"self-warn\"}',\n",
    "    '{\"level\": \"error\", \"msg\": \"self-error\"}',\n",
    ")\n",
    "\n",
    "\n",
    "def lambda_handler(event, context):\n",
    "    for line in LINES:\n",
    "        print(line)\n",
    "    return {'ok': True}\n",
);

/// The `msg` of each record that handler writes, in the order it writes them.
const SELF_RECORDS: [&str; 5] = [
    "self-debug",
    "self-levelless",
    "self-bogus",
    "self-warn",
    "self-error",
];

/// One row of the level table: the level the function filters its application records at, and the
/// records of the handler above that reach the group under it.
struct Filtered {
    case: &'static str,
    /// The `ApplicationLogLevel` the function names.
    level: &'static str,
    /// The `msg` of every record the group must hold, in any order.
    delivered: &'static [&'static str],
}

/// The platform records of a COLD run that every row's delivery carries whatever its application
/// level is: the record of the environment's initialization, the one that closes the handler's run
/// and the one that reports what the run spent. The first two state the DETAIL of how the run was
/// served, so the rows select the system level that admits every platform record
/// ([`crate::invocation_system_record_levels`] owns which level admits which record); what this
/// rule needs of them is that the level over the APPLICATION records withholds none of them.
const PLATFORM_RECORDS: [&str; 3] = [
    "platform.initStart",
    "platform.runtimeDone",
    "platform.report",
];

/// What the PLAIN-TEXT framing of the same run would open its records with — none of which a
/// function on the structured format is delivered, because the format is of the whole log.
const TEXT_FRAMING: [&str; 4] = [
    "INIT_START Runtime Version: ",
    "START RequestId: ",
    "END RequestId: ",
    REPORT_PREFIX,
];

/// The rows: every level the handler's own records are stated at, plus one above all of them.
///
/// A record stating no level the ladder places — none at all, or one the handler made up — is
/// sent at the level AWS documents for output a handler wrote straight to its standard streams,
/// which is the level a function's application logs carry by default
/// (`state::DEFAULT_LOG_LEVEL`, `INFO`): so the two of them travel with the `INFO` records and
/// are withheld with them.
fn filtered_rows() -> Vec<Filtered> {
    vec![
        Filtered {
            case: "the most detailed level keeps every record",
            level: "DEBUG",
            delivered: &SELF_RECORDS,
        },
        Filtered {
            case: "a level above the levelless records withholds them with the debug one",
            level: "WARN",
            delivered: &["self-warn", "self-error"],
        },
        Filtered {
            case: "a level at the one the levelless records count as keeps them",
            level: "INFO",
            delivered: &[
                "self-levelless",
                "self-bogus",
                "self-warn",
                "self-error",
            ],
        },
        Filtered {
            case: "a level above every record but one keeps that one alone",
            level: "ERROR",
            delivered: &["self-error"],
        },
    ]
}

/// RULE: Lambda sends an application record only at the selected level of detail and lower, read
/// off the `level` member of the record ITSELF — so a handler that wrote its own JSON object is
/// filtered by the level that object states, and one stating no level this reading places travels
/// at the level AWS documents for a write to the handler's standard streams. The PLATFORM's own
/// records of the run are a log of the other kind, filtered at the other level and rendered in the
/// format the whole log is in, so the structured format delivers them as structured records beside
/// the handler's and leaves none of the framing as text.
#[tokio::test]
async fn a_handlers_own_json_record_is_filtered_by_the_level_it_states() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    for row in filtered_rows() {
        let case = row.case;
        let router = emulator();
        let name = format!("self-structured-{}", row.level.to_lowercase());
        let role = given_role(
            &router,
            case,
            &format!("{name}-writer"),
            Some(json!([allow(LOGS, &LOG_WRITES, "*")])),
        )
        .await;
        let group = format!("{DEFAULT_PREFIX}{name}");
        create_function_logging(
            &router,
            &Created {
                case,
                name: &name,
                source: SELF_STRUCTURED_HANDLER,
                runtime: PYTHON_RUNTIME,
                role: &role,
                log_group: None,
            },
            // A level is a feature of the structured format, which is the format this handler's
            // own records are already in. The system level is the one that admits every platform
            // record, so what reaches the group of those is this level's doing and not the other
            // one's.
            Some(&json!({
                "LogFormat": "JSON",
                "ApplicationLogLevel": row.level,
                "SystemLogLevel": "DEBUG",
            })),
        )
        .await;

        // The run itself writes every one of its records, whatever the filter admits: the tail
        // the invocation reports is the log the run WROTE.
        let tail = invoked(&router, case, &name, "self-structured").await;
        for written in SELF_RECORDS {
            assert!(
                tail.contains(written),
                "case {case:?} at {}: expected the run to write {written:?}, got {tail:?}",
                row.level
            );
        }

        // What the GROUP holds is what Lambda sent: the records at the selected level and lower,
        // each as the JSON object the handler wrote.
        let held = searched(&router, case, &group).await;
        let carried: Vec<String> = held
            .iter()
            .filter_map(|message| structured_record(record_text(message)))
            .filter_map(|members| {
                members
                    .get("msg")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect();
        for written in SELF_RECORDS {
            let expected = row.delivered.contains(&written);
            let actual = carried.iter().any(|msg| msg.as_str() == written);
            assert_eq!(
                actual, expected,
                "case {case:?} at {}: expected {written:?} delivered = {expected}, got {actual} \
                 with {carried:?}",
                row.level
            );
        }
        // Every delivered record of the handler's own is one the filter admits, and the group
        // holds no second copy of any of them.
        assert_eq!(
            carried.len(),
            row.delivered.len(),
            "case {case:?} at {}: expected {} of the handler's records delivered, got {carried:?}",
            row.level,
            row.delivered.len()
        );
        // The PLATFORM's own records of the same run are in that group too, as the structured
        // records the selected format states them in — the level over the APPLICATION records
        // withholds none of them, and under this format none of the framing is left as text.
        let platform: Vec<String> = held
            .iter()
            .filter_map(|message| structured_record(record_text(message)))
            .filter_map(|members| {
                members
                    .get("type")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect();
        for kind in PLATFORM_RECORDS {
            assert_eq!(
                platform.iter().filter(|named| *named == kind).count(),
                1,
                "case {case:?} at {}: expected exactly one {kind:?} record of the run in \
                 {group:?}, got {held:?}",
                row.level
            );
        }
        for framing in TEXT_FRAMING {
            assert!(
                !held.iter().any(|message| message.starts_with(framing)),
                "case {case:?} at {}: expected no {framing:?} text line under the structured \
                 format, got {held:?}",
                row.level
            );
        }
    }
}
