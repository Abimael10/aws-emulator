// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the configuration a START and a CREATE record on the resource they
//! begin, and that every later read of it reports back: the arguments a job run was started
//! with (`JobRun$ExecutionArguments`) and the timeout a session was created with
//! (`Session$Timeout`).
//!
//! The rule under test: what a caller supplied for ONE run or ONE session is kept on that
//! record and reported by every read of it, so a client that configured a resource can read
//! the configuration back and confirm it — the run's own `Arguments` as sent, and the
//! session's own timeout beside the idle timeout that defaults to it. Neither member is
//! dropped for a caller that supplied nothing: `ExecutionArguments` is reported whatever it
//! holds, so a run started with no arguments carries the empty map there (where `Arguments`,
//! which the job definition's own arguments are no part of either, is absent), and a session
//! created with no timeout reports the span the service filled in.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.
//!
//! The suites are TABLES whose rows differ along what the rule generalises over: how many
//! arguments a start supplies and whether they collide with the job definition's own
//! (`DefaultArguments`, `NonOverridableArguments`), which reader reports the run, and what
//! the create named for the two session timeouts — each published bound included.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::DEFAULT_SESSION_TIMEOUT_MINUTES;

use crate::job_runs::{
    emulator_with_engine, far_future, get_run, job_body, set_due_instant, start_run,
};
use crate::wire::{emulator, ok};

/// An IAM role ARN of the form the modelled `OrchestrationRoleArn` pattern requires
/// (`^arn:aws[^:]*:iam::[0-9]*:role/.+$`).
const SESSION_ROLE: &str = "arn:aws:iam::000000000000:role/GlueSessionRole";

/// The map members of `member` on `reported`, as a comparable map. The member must be there:
/// an argument map is reported whatever it holds.
fn argument_map(case: &str, reported: &Value, member: &str) -> BTreeMap<String, String> {
    reported
        .get(member)
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("case {case:?}: expected the {member} member, got {reported}"))
        .iter()
        .map(|(name, value)| {
            (
                name.clone(),
                value
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: {member} holds strings, got {value}")
                    })
                    .to_string(),
            )
        })
        .collect()
}

/// The expectation a row states for an argument map, as the same comparable map.
fn expected_map(case: &str, expected: &Value) -> BTreeMap<String, String> {
    expected
        .as_object()
        .unwrap_or_else(|| panic!("case {case:?}: every row's expectation is a map"))
        .iter()
        .map(|(name, value)| {
            (
                name.clone(),
                value
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: an argument value is a string, got {value}")
                    })
                    .to_string(),
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The arguments a run was started with
// ---------------------------------------------------------------------------

/// One row: what the job definition carries, what the start supplies, and the map every read
/// of the run must then report for it.
struct RunArguments {
    /// What this row varies, named in every assertion it drives.
    case: &'static str,
    /// The job's `DefaultArguments`, or `Value::Null` for a job that names none.
    defaults: Value,
    /// The job's `NonOverridableArguments`, or `Value::Null` for a job that names none.
    locked: Value,
    /// The `Arguments` the start supplies, or `Value::Null` for a start that names none.
    supplied: Value,
    /// What `ExecutionArguments` must report: the start's own arguments, as sent, and the
    /// empty map for a start that sent none.
    execution_arguments: Value,
}

/// The rows: every combination of the three argument sets, and the counts in between.
fn run_argument_rows() -> Vec<RunArguments> {
    vec![
        RunArguments {
            case: "a start that supplied nothing, for a job that names nothing",
            defaults: Value::Null,
            locked: Value::Null,
            supplied: Value::Null,
            execution_arguments: json!({}),
        },
        RunArguments {
            case: "a start that supplied an empty map",
            defaults: Value::Null,
            locked: Value::Null,
            supplied: json!({}),
            execution_arguments: json!({}),
        },
        RunArguments {
            case: "one argument, for a job that names nothing",
            defaults: Value::Null,
            locked: Value::Null,
            supplied: json!({ "--my-custom-arg": "hello-world" }),
            execution_arguments: json!({ "--my-custom-arg": "hello-world" }),
        },
        RunArguments {
            case: "a start that supplied nothing, for a job carrying defaults",
            defaults: json!({ "--job-bookmark-option": "job-bookmark-enable" }),
            locked: Value::Null,
            supplied: Value::Null,
            // The job's defaults are not the run's own arguments: nothing was supplied for
            // this run, so the arguments it was started with are none.
            execution_arguments: json!({}),
        },
        RunArguments {
            case: "an argument that collides with one of the job's defaults",
            defaults: json!({ "--input": "s3://lake/in/", "--enable-metrics": "true" }),
            locked: Value::Null,
            supplied: json!({ "--input": "s3://lake/other/" }),
            execution_arguments: json!({ "--input": "s3://lake/other/" }),
        },
        RunArguments {
            case: "an argument the job does not let a run override",
            defaults: Value::Null,
            locked: json!({ "--locked": "yes" }),
            supplied: json!({ "--locked": "no" }),
            // What the start ASKED for is what the run is reported as started with, whether
            // or not the job let its script run with it.
            execution_arguments: json!({ "--locked": "no" }),
        },
        RunArguments {
            case: "several arguments against defaults and a locked argument",
            defaults: json!({ "--a": "1", "--b": "2", "--c": "3" }),
            locked: json!({ "--c": "locked" }),
            supplied: json!({ "--b": "overridden", "--d": "4", "--e": "5" }),
            execution_arguments: json!({ "--b": "overridden", "--d": "4", "--e": "5" }),
        },
        RunArguments {
            case: "an argument whose value is the empty string",
            defaults: Value::Null,
            locked: Value::Null,
            supplied: json!({ "--flag": "" }),
            execution_arguments: json!({ "--flag": "" }),
        },
    ]
}

/// Every read of a run reports the arguments its START supplied under `ExecutionArguments`,
/// which is there whatever it holds — the empty map for a start that supplied none, where
/// `Arguments` is absent — and the job definition's own arguments are no part of either.
#[tokio::test]
async fn a_run_reports_the_arguments_its_start_supplied() {
    let (router, engine) = emulator_with_engine();

    for (index, row) in run_argument_rows().into_iter().enumerate() {
        let case = row.case;
        let name = format!("started-with-arguments-{index}");
        let mut job = json!({});
        if !row.defaults.is_null() {
            job["DefaultArguments"] = row.defaults.clone();
        }
        if !row.locked.is_null() {
            job["NonOverridableArguments"] = row.locked.clone();
        }
        ok(&router, "CreateJob", &job_body(&name, job)).await;

        let mut start = json!({ "JobName": name });
        if !row.supplied.is_null() {
            start["Arguments"] = row.supplied.clone();
        }
        let run_id = start_run(&router, &start).await;
        // The run is held where its start put it, so this suite reads the record the start
        // wrote rather than waiting on the lifecycle another suite pins.
        set_due_instant(&engine, &name, far_future());

        let run = get_run(&router, &name, &run_id).await;
        assert_eq!(
            argument_map(case, &run, "ExecutionArguments"),
            expected_map(case, &row.execution_arguments),
            "case {case:?}: expected ExecutionArguments {}, got {run}",
            row.execution_arguments
        );
        // `Arguments` carries the same arguments, and is the member that is absent when the
        // start supplied none: read as "absent is none", the two agree on every row.
        let reported = match run.get("Arguments") {
            None | Some(Value::Null) => BTreeMap::new(),
            Some(_) => argument_map(case, &run, "Arguments"),
        };
        assert_eq!(
            reported,
            expected_map(case, &row.execution_arguments),
            "case {case:?}: expected Arguments {}, got {run}",
            row.execution_arguments
        );
        if row.supplied.as_object().is_some_and(|sent| !sent.is_empty()) {
            assert!(
                run.get("Arguments").is_some_and(|sent| !sent.is_null()),
                "case {case:?}: a run started with arguments reports them, got {run}"
            );
        }

        // Every reader of a run serves one projection: the history read reports the same run
        // the single read does, this member included.
        let history = ok(&router, "GetJobRuns", &json!({ "JobName": name })).await;
        let listed = history
            .get("JobRuns")
            .and_then(Value::as_array)
            .and_then(|runs| runs.first())
            .cloned()
            .unwrap_or_else(|| {
                panic!("case {case:?}: GetJobRuns must report the run, got {history}")
            });
        assert_eq!(
            listed.get("ExecutionArguments"),
            run.get("ExecutionArguments"),
            "case {case:?}: the history must report the single read's ExecutionArguments, got \
             {listed} against {run}"
        );

        // A read changes nothing: the same member comes back on the next read of the run.
        let again = get_run(&router, &name, &run_id).await;
        assert_eq!(
            again.get("ExecutionArguments"),
            run.get("ExecutionArguments"),
            "case {case:?}: a second read must report the same ExecutionArguments, got {again}"
        );
    }
}

// ---------------------------------------------------------------------------
// The timeout a session was created with
// ---------------------------------------------------------------------------

/// One row: what the create names for the two session timeouts, and what a read of the
/// session must then report for each.
struct SessionTimeouts {
    /// What this row varies, named in every assertion it drives.
    case: &'static str,
    /// The `Timeout` the create names, or `None` for a create that names none.
    timeout: Option<i64>,
    /// The `IdleTimeout` the create names, or `None` for a create that names none.
    idle_timeout: Option<i64>,
}

impl SessionTimeouts {
    /// The `Timeout` a read must report: the create's, else the service's own default.
    fn expected_timeout(&self) -> i64 {
        self.timeout
            .unwrap_or_else(|| i64::from(DEFAULT_SESSION_TIMEOUT_MINUTES))
    }

    /// The `IdleTimeout` a read must report: the create's, else the value of `Timeout`
    /// ("Default for Spark ETL jobs is value of Timeout").
    fn expected_idle_timeout(&self) -> i64 {
        self.idle_timeout.unwrap_or_else(|| self.expected_timeout())
    }
}

/// The rows: the two timeouts named together, one at a time and not at all, at the modelled
/// floor (`Timeout` is `range 1..`), at the recorded order of magnitude, and far above it.
fn session_timeout_rows() -> Vec<SessionTimeouts> {
    vec![
        SessionTimeouts {
            case: "neither timeout named",
            timeout: None,
            idle_timeout: None,
        },
        SessionTimeouts {
            case: "a timeout at the modelled floor",
            timeout: Some(1),
            idle_timeout: None,
        },
        SessionTimeouts {
            case: "a small timeout, no idle timeout",
            timeout: Some(5),
            idle_timeout: None,
        },
        SessionTimeouts {
            case: "both timeouts named, the idle one shorter",
            timeout: Some(60),
            idle_timeout: Some(30),
        },
        SessionTimeouts {
            case: "both timeouts named, the idle one longer",
            timeout: Some(45),
            idle_timeout: Some(120),
        },
        SessionTimeouts {
            case: "an idle timeout alone",
            timeout: None,
            idle_timeout: Some(10),
        },
        SessionTimeouts {
            case: "a timeout of a whole week in minutes",
            timeout: Some(10_080),
            idle_timeout: Some(1),
        },
    ]
}

/// A session reports the timeout it was created with, and the create and every later read
/// report the same figure — beside the idle timeout, which is its own member.
#[tokio::test]
async fn a_session_reports_the_timeout_it_was_created_with() {
    let router = emulator();

    for (index, row) in session_timeout_rows().into_iter().enumerate() {
        let case = row.case;
        let id = format!("session-timeout-{index}");
        let mut request = json!({
            "Id": id,
            "Role": SESSION_ROLE,
            "Command": { "Name": "glueetl" },
        });
        if let Some(timeout) = row.timeout {
            request["Timeout"] = json!(timeout);
        }
        if let Some(idle_timeout) = row.idle_timeout {
            request["IdleTimeout"] = json!(idle_timeout);
        }

        let created = ok(&router, "CreateSession", &request).await;
        let created = created
            .get("Session")
            .cloned()
            .unwrap_or_else(|| {
                panic!("case {case:?}: CreateSession must report a Session, got {created}")
            });
        let read = ok(&router, "GetSession", &json!({ "Id": id })).await;
        let read = read
            .get("Session")
            .cloned()
            .unwrap_or_else(|| {
                panic!("case {case:?}: GetSession must report a Session, got {read}")
            });

        let expected = row.expected_timeout();
        assert_eq!(
            read.get("Timeout").and_then(Value::as_i64),
            Some(expected),
            "case {case:?}: expected a Timeout of {expected}, got {read}"
        );
        assert_eq!(
            created.get("Timeout"),
            read.get("Timeout"),
            "case {case:?}: the create and the read must report one Timeout, got {created} \
             against {read}"
        );
        let expected_idle = row.expected_idle_timeout();
        assert_eq!(
            read.get("IdleTimeout").and_then(Value::as_i64),
            Some(expected_idle),
            "case {case:?}: expected an IdleTimeout of {expected_idle}, got {read}"
        );

        // A read changes nothing: the next read of the session reports the same figure.
        let again = ok(&router, "GetSession", &json!({ "Id": id })).await;
        assert_eq!(
            again.get("Session").and_then(|session| session.get("Timeout")),
            read.get("Timeout"),
            "case {case:?}: a second read must report the same Timeout, got {again}"
        );
    }
}
