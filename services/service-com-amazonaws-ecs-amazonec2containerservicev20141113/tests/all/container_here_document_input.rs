// SPDX-License-Identifier: CC0-1.0

//! The redirections whose TEXT is written inside the command — a here-document and a
//! here-string — driven end to end through `RunTask`/`DescribeTasks`.
//!
//! RULE: a container runs the argument vector its definition states
//! (`ContainerDefinition$entryPoint` / `$command`) and `Container$exitCode` is "the exit code
//! returned from the container", so it is the status the program that vector names ended with.
//! A program hands a document it composed to a command through the shell's own forms rather
//! than through a file: `<<WORD` is "the command's standard input" written under the command
//! itself, `<<-WORD` is the same with the leading tabs stripped off each of its lines, and
//! `<<<WORD` is the one word followed by a line ending. Whether the text is EXPANDED is the
//! delimiter's own question — an unquoted delimiter expands the document's parameters, a
//! quoted one takes it literally — and the text reaches the command's standard input exactly
//! as a `< path` redirection's bytes do. So a program built from those runs to its end and
//! reports the status it chose.
//!
//! Why it matters beyond the one form: ONE form the container session performs none of
//! abandons the WHOLE run, and a run that claims no exit status leaves its task `RUNNING` with
//! no outcome a caller can read (`container_program_not_performed` pins that rule) — so a
//! one-shot workload that composes what it publishes in a here-document is a task that never
//! settles rather than a task that reports what it did.
//!
//! The rows differ along what the rule generalises over: WHICH form carries the text (a
//! document, a tab-stripped document, a here-string), whether the delimiter asks for
//! expansion, WHICH command reads it (`cat`, a text filter, a write into the container's own
//! filesystem), where the text ends up (a word of the program, a file, the program's own
//! output), and which of two input redirections of the same command stands — "the last open of
//! a descriptor is the one that stands", and a document stands on the descriptor a `< path`
//! does. Each row's program chooses a DIFFERENT status, so a row cannot pass on another row's
//! answer.
//!
//! The contrast is unchanged and asserted here too: a here-document handed to a command this
//! session has no program for still runs nothing of the program the image holds, so that
//! container claims no status at all.
//!
//! Nothing here reaches into a state struct: every value asserted is read back through
//! `DescribeTasks`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{cluster_with_capacity, list, ok, present_count, string};

/// The image every container below names. Nothing is fetched from it: what runs is the
/// `command` the revision states.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";

/// How often a poll re-reads the task it is waiting on.
const POLL_INTERVAL: Duration = Duration::from_millis(60);

/// How long a poll keeps trying before it gives up. Generous: a standalone task comes up, runs
/// the work it was placed for and then stops, and this suite runs beside every other test of
/// the workspace, which can slow each of those spans down. The deadline is only here so a
/// failure is a failure rather than a hang.
const POLL_BUDGET: Duration = Duration::from_secs(240);

/// `Task$stopCode` of a task whose essential container exited, spelled out here rather than
/// imported so a wrong constant in the implementation cannot make a test pass.
const ESSENTIAL_CONTAINER_EXITED: &str = "EssentialContainerExited";

/// One row: a shell program written round one of the forms, and the status it ends with.
struct Row {
    case: &'static str,
    /// The program the container's `command` hands the shell.
    program: &'static str,
    /// The status that program chooses, which is the container's `exitCode`.
    exit_code: i64,
}

const ROWS: &[Row] = &[
    Row {
        case: "a here-document is the lines written under the command",
        program: "D=$(cat <<EOF\nalpha\nbeta\ngamma\nEOF\n); \
                  case \"$D\" in alpha*gamma) exit 41;; *) exit 91;; esac",
        exit_code: 41,
    },
    Row {
        case: "an unquoted delimiter expands the document's parameters",
        program: "V=expanded; D=$(cat <<EOF\nvalue=$V\nEOF\n); \
                  [ \"$D\" = value=expanded ] && exit 42; exit 92",
        exit_code: 42,
    },
    Row {
        case: "a quoted delimiter takes the document literally",
        program: "V=expanded; D=$(cat <<'EOF'\nvalue=$V\nEOF\n); \
                  [ \"$D\" = 'value=$V' ] && exit 43; exit 93",
        exit_code: 43,
    },
    Row {
        case: "`<<-` strips the leading tabs off each of the document's lines",
        program: "D=$(cat <<-EOF\n\tindented\n\tlines\nEOF\n); \
                  case \"$D\" in indented*lines) exit 44;; *) exit 94;; esac",
        exit_code: 44,
    },
    Row {
        case: "a here-string is the one word it was written with",
        program: "W='one word'; D=$(cat <<<\"$W\"); [ \"$D\" = 'one word' ] && exit 45; \
                  exit 95",
        exit_code: 45,
    },
    Row {
        case: "a here-document read by a text filter rather than by `cat`",
        program: "D=$(tail -n 1 <<EOF\nalpha\nomega\nEOF\n); [ \"$D\" = omega ] && exit 46; \
                  exit 96",
        exit_code: 46,
    },
    Row {
        case: "a here-document written into the container's own filesystem and read back",
        program: "cat > /tmp/doc <<EOF\nwritten\nEOF\nD=$(cat /tmp/doc); \
                  [ \"$D\" = written ] && exit 47; exit 97",
        exit_code: 47,
    },
    Row {
        // The shape a one-shot publisher takes: the body it publishes is composed in a
        // document, reported on the container's own output, and the program ends with a
        // status of its own either way.
        case: "a publisher composing what it reports in a document",
        program: "BODY=$(cat <<EOF\nline one\nline two\nEOF\n); echo \"OUTCOME=OK\"; \
                  case \"$BODY\" in 'line one'*'line two') exit 48;; *) exit 98;; esac",
        exit_code: 48,
    },
    Row {
        // "The last open of a descriptor is the one that stands": a document stands on the
        // descriptor a `< path` does, so whichever was written LAST is what the command reads.
        case: "a file redirection written after a document is the one that stands",
        program: "echo fromfile > /tmp/src; D=$(cat <<EOF < /tmp/src\nfromdoc\nEOF\n); \
                  [ \"$D\" = fromfile ] && exit 51; exit 101",
        exit_code: 51,
    },
    Row {
        case: "a document written after a file redirection is the one that stands",
        program: "echo fromfile > /tmp/src; D=$(cat < /tmp/src <<EOF\nfromdoc\nEOF\n); \
                  [ \"$D\" = fromdoc ] && exit 52; exit 102",
        exit_code: 52,
    },
];

/// The rows whose program reaches a command this container has no program for, so it claims NO
/// status — the contrast that keeps the forms above from being read as "every program now
/// runs".
const UNPERFORMED: &[Row] = &[
    Row {
        case: "a document handed to a command this container has no program for",
        program: "definitely-not-a-utility <<EOF\nalpha\nEOF\nexit 49",
        exit_code: 0,
    },
];

/// Register one revision of `family` whose single essential container runs `program` through a
/// shell.
async fn register(router: &MultiServiceRouter, family: &str, program: &str, case: &str) {
    ok(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": family,
            "containerDefinitions": [{
                "name": "app",
                "image": IMAGE,
                "memory": 128,
                "essential": true,
                "command": ["sh", "-c", program],
            }],
        }),
        case,
    )
    .await;
}

/// `RunTask` one task of `family` in `cluster`, and answer its ARN.
async fn run_task(router: &MultiServiceRouter, cluster: &str, family: &str, case: &str) -> String {
    let body = ok(
        router,
        "RunTask",
        json!({ "cluster": cluster, "taskDefinition": family }),
        case,
    )
    .await;
    let tasks = list(&body, "tasks", case);
    let task = tasks
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: expected a task back, got {body}"));
    string(task, "taskArn", case)
}

/// One task, as `DescribeTasks` reports it.
async fn describe(router: &MultiServiceRouter, cluster: &str, arn: &str, case: &str) -> Value {
    let body = ok(
        router,
        "DescribeTasks",
        json!({ "cluster": cluster, "tasks": [arn] }),
        case,
    )
    .await;
    list(&body, "tasks", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected the task back, got {body}"))
}

/// Read one task again until its `lastStatus` is `STOPPED`, and answer it.
///
/// Waiting for the CONDITION rather than for a span: a standalone task is stopped once the
/// essential container it was placed to run has exited, which is what makes that container's
/// run certainly finished.
async fn stopped_task(router: &MultiServiceRouter, cluster: &str, arn: &str, case: &str) -> Value {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let task = describe(router, cluster, arn, case).await;
        if string(&task, "lastStatus", case) == "STOPPED" {
            return task;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the task to stop within the poll budget, got {task}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Read one task again until its `lastStatus` is `RUNNING`, and answer it.
async fn running_task(router: &MultiServiceRouter, cluster: &str, arn: &str, case: &str) -> Value {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let task = describe(router, cluster, arn, case).await;
        if string(&task, "lastStatus", case) == "RUNNING" {
            return task;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the task to reach RUNNING within the poll budget, got \
             {task}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The one container of a task's record.
fn container_of(task: &Value, case: &str) -> Value {
    list(task, "containers", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected a container, got {task}"))
}

/// RULE: the text a here-document or a here-string carries IS the command's standard input, so
/// a program that hands a document it composed to a command runs to its end and reports the
/// status it chose.
#[tokio::test]
async fn a_program_handing_a_document_to_a_command_reports_the_status_it_chose() {
    let router = crate::ecs_wire::service();
    let cluster = "heredocs";
    cluster_with_capacity(&router, cluster, "here-document programs").await;

    // Every row's task is launched FIRST and polled afterwards: the runs happen in parallel in
    // the one emulator, so the suite waits for the longest of them rather than for their sum.
    let mut launched: Vec<(&Row, String)> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let family = format!("heredoc{index}");
        register(&router, &family, row.program, row.case).await;
        let arn = run_task(&router, cluster, &family, row.case).await;
        launched.push((row, arn));
    }

    for (row, arn) in &launched {
        let case = row.case;
        let task = stopped_task(&router, cluster, arn, case).await;
        let reported = present_count(&container_of(&task, case), "exitCode", case);
        assert_eq!(
            reported,
            Some(row.exit_code),
            "case {case:?}: expected exitCode {:?}, got {reported:?} in {task}",
            row.exit_code
        );
        assert_eq!(
            string(&task, "stopCode", case),
            ESSENTIAL_CONTAINER_EXITED,
            "case {case:?}: expected the stop of a task whose container ran and exited, got \
             {task}"
        );
    }
}

/// RULE, the other side: the form carrying the text changes nothing about the COMMAND that
/// reads it, so a document handed to a command this container has no program for still runs
/// none of the program the image holds and claims no status at all.
#[tokio::test]
async fn a_document_handed_to_an_unperformed_command_still_claims_no_status() {
    let router = crate::ecs_wire::service();
    let cluster = "heredocsunperformed";
    cluster_with_capacity(&router, cluster, "unperformed here-document programs").await;

    let mut launched: Vec<(&Row, String)> = Vec::new();
    for (index, row) in UNPERFORMED.iter().enumerate() {
        let family = format!("heredocunperformed{index}");
        register(&router, &family, row.program, row.case).await;
        let arn = run_task(&router, cluster, &family, row.case).await;
        launched.push((row, arn));
    }

    for (row, arn) in &launched {
        let case = row.case;
        let task = running_task(&router, cluster, arn, case).await;
        let reported = present_count(&container_of(&task, case), "exitCode", case);
        assert_eq!(
            reported, None,
            "case {case:?}: expected no exitCode at all, got {reported:?} in {task}"
        );
    }
}
