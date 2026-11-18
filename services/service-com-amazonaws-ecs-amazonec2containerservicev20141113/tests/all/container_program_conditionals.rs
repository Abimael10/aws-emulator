// SPDX-License-Identifier: CC0-1.0

//! The CONDITIONAL forms a container's program is written with, driven end to end through
//! `RunTask`/`DescribeTasks`.
//!
//! RULE: `Container$exitCode` is "the exit code returned from the container", so it is the
//! status the program the container's `command` names ended with. A program decides that
//! status with the forms a shell gives it and the utilities whose whole effect is inside its
//! own session: `test` (and its `[ ... ]` spelling) answers a question about the words it was
//! given, `if` runs the branch its condition chose, `case` runs the branch whose shell
//! pattern its word matched, `${#NAME}` is how many characters a variable's value has, and a
//! backslash escape stands for the character it escapes. A program built from those reports
//! the status it ended with — every one of them is answered inside the session, so there is
//! nothing outside this process for any of them to reach.
//!
//! What a container has no program for is unchanged by that: a question about a FILE needs a
//! filesystem this container does not have, and a `for` loop is a form it does not perform,
//! so a program that reaches one of those ran none of the program its image holds and claims
//! no status at all — which is the rule `container_program_not_performed` pins.
//!
//! The rows differ along what the rule generalises over: WHICH form decides the status (a
//! `test` string comparison, a `test` integer comparison, a negated `test`, an `if`, an
//! `if`/`elif`/`else`, a `case` with one pattern, a `case` with alternatives, a `case` whose
//! wildcard catches the value, a `case` matching one character, a length expansion, an
//! escaped quote inside a double-quoted word), and the status each program chooses is a
//! DIFFERENT number per row, so a row cannot pass on another row's answer.
//!
//! Nothing here reaches into a state struct: every status asserted is read back through
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

/// How long a poll keeps trying before it gives up. Generous: a standalone task comes up,
/// runs the work it was placed for and then stops, and this suite runs beside every other
/// test of the workspace, which can slow each of those spans down. The deadline is only here
/// so a failure is a failure rather than a hang.
const POLL_BUDGET: Duration = Duration::from_secs(240);

/// `Task$stopCode` of a task whose essential container exited, spelled out here rather than
/// imported so a wrong constant in the implementation cannot make a test pass.
const ESSENTIAL_CONTAINER_EXITED: &str = "EssentialContainerExited";

/// One row: a shell program, and the status it ends with.
struct Row {
    case: &'static str,
    /// The program the container's `command` hands the shell.
    program: &'static str,
    /// The status that program chooses, which is the container's `exitCode`.
    exit_code: i64,
}

const ROWS: &[Row] = &[
    Row {
        case: "a `[` string comparison that holds",
        program: "V=kept; [ \"$V\" = kept ] && exit 11; exit 61",
        exit_code: 11,
    },
    Row {
        case: "a `[` string comparison that does not hold",
        program: "V=kept; [ \"$V\" = other ] && exit 62; exit 12",
        exit_code: 12,
    },
    Row {
        case: "a `test` inequality, written as the utility's own name",
        program: "V=kept; test \"$V\" != other && exit 13; exit 63",
        exit_code: 13,
    },
    Row {
        case: "a `[` integer comparison",
        program: "[ 7 -gt 3 ] && exit 14; exit 64",
        exit_code: 14,
    },
    Row {
        case: "a negated `[` emptiness test over an unset variable",
        program: "[ ! -z \"$NOTHING\" ] && exit 65; exit 15",
        exit_code: 15,
    },
    Row {
        case: "an `if` whose condition holds",
        program: "V=yes; if [ \"$V\" = yes ]; then exit 16; fi; exit 66",
        exit_code: 16,
    },
    Row {
        case: "an `if` that falls through to its `elif`",
        program: "V=b; if [ \"$V\" = a ]; then exit 67; elif [ \"$V\" = b ]; then exit 17; \
                  else exit 68; fi",
        exit_code: 17,
    },
    Row {
        case: "an `if` that falls through to its `else`",
        program: "V=c; if [ \"$V\" = a ]; then exit 69; elif [ \"$V\" = b ]; then exit 70; \
                  else exit 18; fi",
        exit_code: 18,
    },
    Row {
        case: "a `case` whose pattern matches the whole word",
        program: "V=exact; case \"$V\" in exact) exit 19;; *) exit 71;; esac",
        exit_code: 19,
    },
    Row {
        case: "a `case` pattern with a wildcard on either side",
        program: "V=a-middle-b; case \"$V\" in *middle*) exit 20;; *) exit 72;; esac",
        exit_code: 20,
    },
    Row {
        case: "a `case` item whose alternatives name the word second",
        program: "V=second; case \"$V\" in first|second) exit 21;; *) exit 73;; esac",
        exit_code: 21,
    },
    Row {
        case: "a `case` the wildcard item catches",
        program: "V=neither; case \"$V\" in first) exit 74;; *) exit 22;; esac",
        exit_code: 22,
    },
    Row {
        case: "a `case` with one wildcard character, which matches one character",
        program: "V=ab; case \"$V\" in a?) exit 23;; *) exit 76;; esac",
        exit_code: 23,
    },
    Row {
        // POSIX: "the exit status of case shall be zero if no patterns are matched" — so the
        // `&&` after it runs, which is how this row tells that status from the branch's.
        case: "a `case` no pattern of which matches, whose own status is success",
        program: "V=nothing; case \"$V\" in first) exit 77;; esac && exit 30",
        exit_code: 30,
    },
    Row {
        case: "a length expansion of a variable the program set",
        program: "V=abcd; [ ${#V} -eq 4 ] && exit 24; exit 77",
        exit_code: 24,
    },
    Row {
        case: "a length expansion of a variable nothing set, which is zero",
        program: "[ ${#NOTHING} -eq 0 ] && exit 25; exit 78",
        exit_code: 25,
    },
    Row {
        case: "an escaped quote inside a double-quoted word",
        program: "V=\"a\\\"b\"; [ ${#V} -eq 3 ] && exit 26; exit 79",
        exit_code: 26,
    },
    Row {
        // The shape the recorded workloads take: a JSON document matched against a pattern
        // whose quoted part carries the quotes of the document.
        case: "a `case` pattern carrying escaped quotes, over a JSON document",
        program: "D='{\"K\":\"v\",\"L\":\"w\"}'; K=v; \
                  case \"$D\" in *\"\\\"K\\\":\\\"$K\\\"\"*) exit 27;; *) exit 80;; esac",
        exit_code: 27,
    },
    Row {
        case: "a `case` over a document whose braces are no pattern of their own",
        program: "D='{a,b}'; case \"$D\" in \"{a,b}\") exit 28;; *) exit 81;; esac",
        exit_code: 28,
    },
    Row {
        case: "a `[` question about a file the container's filesystem holds nothing at",
        program: "[ -f /etc/passwd ] && exit 31; exit 32",
        exit_code: 32,
    },
    Row {
        case: "a `case` branch that reads a path the container's filesystem holds nothing at",
        program: "V=a; case \"$V\" in a) cat /etc/passwd;; esac; exit 34",
        exit_code: 34,
    },
];

/// The rows whose program reaches a form this container performs nothing of, so the
/// container claims NO status — the contrast that keeps the catalog from being read as "every
/// shell form now runs".
const UNPERFORMED: &[Row] = &[
    Row {
        case: "a `case` pattern carrying a bracket expression, a construct it does not read",
        program: "V=b; case \"$V\" in [abc]) exit 31;; *) exit 32;; esac",
        exit_code: 0,
    },
    Row {
        case: "a `for` loop, a compound form this container does not perform",
        program: "for name in one two; do echo $name; done; exit 33",
        exit_code: 0,
    },
];

/// Register one revision of `family` whose single essential container runs `program` through
/// a shell.
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

/// Read one task again until its `lastStatus` is `STOPPED`, and answer it.
///
/// Waiting for the CONDITION rather than for a span: a standalone task is stopped once the
/// work its containers were placed to do is over, which is what makes its container's run
/// certainly finished.
async fn stopped_task(router: &MultiServiceRouter, cluster: &str, arn: &str, case: &str) -> Value {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let body = ok(
            router,
            "DescribeTasks",
            json!({ "cluster": cluster, "tasks": [arn] }),
            case,
        )
        .await;
        let task = list(&body, "tasks", case)
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("case {case:?}: expected the task back, got {body}"));
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

/// The one container of a stopped task's record.
fn container_of(task: &Value, case: &str) -> Value {
    list(task, "containers", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected a container, got {task}"))
}

/// RULE: a container's `exitCode` is the status its program ended with, and a program decides
/// that status with the conditional forms and session-local utilities a shell gives it.
#[tokio::test]
async fn a_conditional_program_reports_the_status_it_chose() {
    let router = crate::ecs_wire::service();
    let cluster = "conditionals";
    cluster_with_capacity(&router, cluster, "conditional programs").await;

    // Every row's task is launched FIRST and polled afterwards: the runs happen in parallel
    // in the one emulator, so the suite waits for the longest of them rather than for their
    // sum.
    let mut launched: Vec<(&Row, String)> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let family = format!("conditional{index}");
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

/// RULE, the other side: a program that reaches a form or a utility this container performs
/// nothing of ran none of the program its image holds, so it claims no status at all.
#[tokio::test]
async fn a_program_reaching_an_unperformed_form_still_claims_no_status() {
    let router = crate::ecs_wire::service();
    let cluster = "unperformed";
    cluster_with_capacity(&router, cluster, "unperformed programs").await;

    let mut launched: Vec<(&Row, String)> = Vec::new();
    for (index, row) in UNPERFORMED.iter().enumerate() {
        let family = format!("unperformed{index}");
        register(&router, &family, row.program, row.case).await;
        let arn = run_task(&router, cluster, &family, row.case).await;
        launched.push((row, arn));
    }

    for (row, arn) in &launched {
        let case = row.case;
        let task = stopped_task(&router, cluster, arn, case).await;
        let reported = present_count(&container_of(&task, case), "exitCode", case);
        assert_eq!(
            reported, None,
            "case {case:?}: expected no exitCode at all, got {reported:?} in {task}"
        );
    }
}
