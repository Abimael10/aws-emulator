// SPDX-License-Identifier: CC0-1.0

//! How a write that takes a JOB DEFINITION refuses a definition that carries no value for a
//! member the job cannot run without, and in what order it judges those members.
//!
//! The rule under test: a job-definition write (`CreateJob`, `UpdateJob`) refuses such a
//! definition with the operation's modelled `InvalidInputException` (HTTP 400) whose message
//! names that member of the definition and states that it must not be null — fixed text plus
//! the member's own name, with nothing of the request interpolated — and, for a member whose
//! value is a STRING, that it must not be empty either, because one check covers both ways a
//! string carries no value. It judges the definition's members in one order, the command the
//! job runs before the role it runs under, so a definition missing both is refused about the
//! COMMAND. A definition carrying both is served, which is what makes the refusals above
//! refusals of the missing member and not of something else.
//!
//! The same rule one level INSIDE the definition, which the second half of this suite states:
//! a command that NAMES the command the job runs needs the script that command runs
//! ("Specifies the Amazon Simple Storage Service (Amazon S3) path to a script that runs a
//! job", `JobCommand$ScriptLocation`), so a definition naming a command and no script is
//! refused with the sentence naming the command it asked for — while a command that names no
//! command kind at all states no such requirement and is stored as sent.
//!
//! The rows differ along what the rule generalises over: which operation carried the
//! definition, which member carried no value, HOW it carried none (absent, the empty string,
//! a blank one), the command the definition named, and whether the other member was there.
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

/// The sentence a definition missing a STRUCTURE member — one it can only leave out — must be
/// refused with. Written here independently of the handler, so this suite pins the wording
/// and not the code.
fn missing_member(member: &str) -> String {
    format!("{member} should not be null.")
}

/// The sentence a definition missing a STRING member must be refused with: such a member can
/// be sent with nothing in it as well as left out, and the one check that covers both states
/// both. Written here independently of the handler, for the same reason.
fn missing_or_empty_member(member: &str) -> String {
    format!("{member} should not be null or empty.")
}

/// The sentence a definition whose command names `command_name` and no script must be refused
/// with: fixed text around the command the definition asked for, closing with no period.
/// Written here independently of the handler, for the same reason.
fn no_script_for(command_name: &str) -> String {
    format!("Script location cannot be null or empty for {command_name} command")
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

    // (case, the submitted JobUpdate, the sentence the refusal must carry)
    let rows: Vec<(&str, Value, String)> = vec![
        (
            "neither the command nor the role",
            json!({}),
            missing_member("Command"),
        ),
        (
            "no command, with a role",
            json!({ "Role": ROLE_ARN }),
            missing_member("Command"),
        ),
        (
            "no role, with a command",
            json!({ "Command": command() }),
            missing_or_empty_member("Role"),
        ),
        (
            "a blank role",
            json!({ "Role": "   ", "Command": command() }),
            missing_or_empty_member("Role"),
        ),
        (
            "an empty role",
            json!({ "Role": "", "Command": command() }),
            missing_or_empty_member("Role"),
        ),
    ];

    for (case, update, expected) in &rows {
        assert_refused(
            &router,
            case,
            "UpdateJob",
            &json!({ "JobName": STORED_JOB, "JobUpdate": update }),
            expected,
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
            &missing_or_empty_member("Role"),
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

/// A definition whose command NAMES the command the job runs and carries no script for it is
/// refused about the script, naming the command it asked for — on the create and on the
/// update, with nothing written either way.
///
/// The rows differ along the command the definition names (each of the four the model
/// documents, and two names it does not list) and along HOW the script carries no value
/// (absent, the empty string, a blank one).
#[tokio::test]
async fn a_definition_whose_command_names_no_script_names_that_command() {
    let router = emulator();
    given_job(&router, STORED_JOB).await;

    // (case, the command name the definition carries). `JobCommand$Name` closes no set, so
    // the requirement follows whatever the definition named.
    let commands: [(&str, &str); 6] = [
        ("the documented Apache Spark ETL command", "glueetl"),
        ("the documented Python shell command", "pythonshell"),
        (
            "the documented Apache Spark streaming command",
            "gluestreaming",
        ),
        ("the documented Ray command", "glueray"),
        ("a name the documentation does not list", "spark"),
        ("a documented name in another casing", "GlueETL"),
    ];

    // (case, the script the command carries, if any at all)
    let scripts: [(&str, Option<&str>); 3] = [
        ("no script member at all", None),
        ("an empty script location", Some("")),
        ("a blank script location", Some("   ")),
    ];

    for (index, (command_case, command_name)) in commands.iter().enumerate() {
        for (script_case, script) in &scripts {
            let case = format!("{command_case}, with {script_case}");
            let mut carried = json!({ "Name": command_name });
            if let Some(script) = script {
                carried["ScriptLocation"] = json!(script);
            }
            let name = format!("no-script-{index}");

            assert_refused(
                &router,
                &case,
                "CreateJob",
                &json!({ "Name": &name, "Role": ROLE_ARN, "Command": carried }),
                &no_script_for(command_name),
            )
            .await;
            // A refused create stored nothing, so the read that follows is a miss.
            let (status, body) = call(&router, "GetJob", &json!({ "JobName": &name })).await;
            assert_eq!(
                status, 400,
                "case {case}: a refused create must store no definition, got {status} with \
                 {body}"
            );

            assert_refused(
                &router,
                &case,
                "UpdateJob",
                &json!({
                    "JobName": STORED_JOB,
                    "JobUpdate": { "Role": ROLE_ARN, "Command": carried },
                }),
                &no_script_for(command_name),
            )
            .await;
            // And the refused update left the stored definition as the create wrote it (the
            // service fills a `PythonVersion` in beside what the create sent, so the two
            // members the create named are what is compared).
            let read = ok(&router, "GetJob", &json!({ "JobName": STORED_JOB })).await;
            let stored = &read["Job"]["Command"];
            for member in ["Name", "ScriptLocation"] {
                assert_eq!(
                    stored.get(member),
                    command().get(member),
                    "case {case}: a refused update must leave the stored {member} alone, got \
                     {read}"
                );
            }
        }
    }

    // A command that names no command kind at all states no such requirement: there is no
    // command to need a script, so the definition is stored as sent — which is what makes the
    // refusals above refusals of the missing script.
    for (index, carried) in [
        json!({ "ScriptLocation": "s3://etl/x.py" }),
        json!({ "PythonVersion": "3" }),
        json!({}),
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("unnamed-command-{index}");
        ok(
            &router,
            "CreateJob",
            &json!({ "Name": &name, "Role": ROLE_ARN, "Command": carried }),
        )
        .await;
        let read = ok(&router, "GetJob", &json!({ "JobName": &name })).await;
        assert!(
            read["Job"]["Command"].get("Name").is_none(),
            "case {index}: the stored command must name no command kind, got {read}"
        );
    }
}
