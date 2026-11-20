// SPDX-License-Identifier: CC0-1.0

//! How a write that takes a JOB DEFINITION refuses a definition that carries no value for a
//! member the job cannot run without, and in what order it judges those members.
//!
//! The rule under test: a job-definition write (`CreateJob`, `UpdateJob`) refuses such a
//! definition with the operation's modelled `InvalidInputException` (HTTP 400) whose message
//! names that member of the definition and states that it must not be null — fixed text plus
//! the member's own name, with nothing of the request interpolated — and it judges the
//! definition's members in one order, the command the job runs before the role it runs
//! under, so a definition missing both is refused about the COMMAND. A definition carrying
//! both is served, which is what makes the refusals above refusals of the missing member and
//! not of something else.
//!
//! The rows differ along what the rule generalises over: which operation carried the
//! definition, which member carried no value, HOW it carried none (absent, the empty string,
//! a blank one), and whether the other member was there.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{call, emulator, error_code, error_message, ok};

/// The role ARN every definition that names one carries.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/sim2real-glue-job-role";

/// The job the updates below address, stored before each of them runs.
const STORED_JOB: &str = "member-refusals";

/// The `JobCommand` a definition that names one carries.
fn command() -> Value {
    json!({ "Name": "glueetl", "ScriptLocation": "s3://etl/x.py" })
}

/// The sentence a definition missing `member` must be refused with. Written here
/// independently of the handler, so this suite pins the wording and not the code.
fn missing_member(member: &str) -> String {
    format!("{member} should not be null.")
}

/// Store one job definition the updates below overwrite.
async fn given_job(router: &MultiServiceRouter, name: &str) {
    ok(
        router,
        "CreateJob",
        &json!({ "Name": name, "Role": ROLE_ARN, "Command": command() }),
    )
    .await;
}

/// Assert that `operation` refused `request` with the modelled input refusal carrying
/// exactly `expected`.
async fn assert_refused(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    request: &Value,
    expected: &str,
) {
    let (status, body) = call(router, operation, request).await;
    assert_eq!(
        status, 400,
        "case {case}: a definition missing a member the job needs must be refused, got \
         {status} with {body}"
    );
    assert!(
        error_code(&body).ends_with("InvalidInputException"),
        "case {case}: expected InvalidInputException, got {} in {body}",
        error_code(&body)
    );
    assert_eq!(
        error_message(&body),
        expected,
        "case {case}: unexpected wording for {request}"
    );
}

/// An `UpdateJob` whose `JobUpdate` carries no value for a member the job needs is refused
/// about that member, and a definition missing both the command and the role is refused
/// about the command — the member judged first.
#[tokio::test]
async fn an_update_naming_no_command_or_role_names_the_missing_member() {
    let router = emulator();
    given_job(&router, STORED_JOB).await;

    // (case, the submitted JobUpdate, the member the refusal must name)
    let rows: Vec<(&str, Value, &str)> = vec![
        (
            "neither the command nor the role",
            json!({}),
            "Command",
        ),
        (
            "no command, with a role",
            json!({ "Role": ROLE_ARN }),
            "Command",
        ),
        (
            "no role, with a command",
            json!({ "Command": command() }),
            "Role",
        ),
        (
            "a blank role",
            json!({ "Role": "   ", "Command": command() }),
            "Role",
        ),
        (
            "an empty role",
            json!({ "Role": "", "Command": command() }),
            "Role",
        ),
    ];

    for (case, update, member) in &rows {
        assert_refused(
            &router,
            case,
            "UpdateJob",
            &json!({ "JobName": STORED_JOB, "JobUpdate": update }),
            &missing_member(member),
        )
        .await;
    }

    // The stored definition is still the one the create stored, so none of the refusals
    // above wrote anything.
    let read = ok(&router, "GetJob", &json!({ "JobName": STORED_JOB })).await;
    assert_eq!(
        read["Job"]["Role"],
        json!(ROLE_ARN),
        "a refused update must leave the stored definition alone, got {read}"
    );

    // And the definition carrying both members is served, so the refusals above are about
    // the member each row left out.
    ok(
        &router,
        "UpdateJob",
        &json!({
            "JobName": STORED_JOB,
            "JobUpdate": { "Role": ROLE_ARN, "Command": command() },
        }),
    )
    .await;
}

/// The same rule on the CREATE: a definition whose role carries no value is refused about
/// that member, in the same sentence, and stores nothing.
#[tokio::test]
async fn a_create_whose_role_carries_no_value_names_that_member() {
    let router = emulator();

    // (case, the role the create submits). `CreateJob$Role` is `@required`, so a role that
    // is absent altogether is refused at the request boundary; these are the values that
    // reach the handler carrying no role.
    let rows: [(&str, &str); 3] = [
        ("a blank role", "   "),
        ("a role of one space", " "),
        ("a role of tabs", "\t\t"),
    ];

    for (index, (case, role)) in rows.iter().enumerate() {
        let name = format!("create-{index}");
        assert_refused(
            &router,
            case,
            "CreateJob",
            &json!({ "Name": name, "Role": role, "Command": command() }),
            &missing_member("Role"),
        )
        .await;

        // A refused create stored nothing, so the read that follows is a miss.
        let (status, body) = call(&router, "GetJob", &json!({ "JobName": name })).await;
        assert_eq!(
            status, 400,
            "case {case}: a refused create must store no definition, got {status} with {body}"
        );
    }

    // The same create with a role that names one is served, so the refusals above are
    // about the blank value and not about the request shape.
    ok(
        &router,
        "CreateJob",
        &json!({ "Name": "created", "Role": ROLE_ARN, "Command": command() }),
    )
    .await;
}
