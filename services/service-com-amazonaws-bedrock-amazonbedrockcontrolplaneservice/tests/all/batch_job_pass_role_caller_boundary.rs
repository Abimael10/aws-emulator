// SPDX-License-Identifier: CC0-1.0

//! The ACCOUNT's pass-role boundary over the role a JOB-creating submission hands Amazon
//! Bedrock, driven over the real wire path (a restJson1 request through the multi-service
//! router into this service mounted beside IAM and STS on one engine), so what is asserted
//! is what a client sees.
//!
//! # The rule
//!
//! Handing Amazon Bedrock the role a job RUNS AS is a permission of the CALLING PRINCIPAL
//! rather than a property of the role: IAM evaluates `iam:PassRole` on that role's own ARN
//! against the policies in force on the principal the request speaks for, so a session whose
//! permissions do not carry that grant cannot submit such a job however well-formed the role
//! it names is. That decision is taken on EVERY submission that passes a role — the batch
//! inference job, the evaluation job and the customization job — and a pass the account
//! withholds is refused with the modelled `AccessDeniedException` (`@httpError(403)`)
//! carrying the one sentence that denial is stated with, with NO job written: the listing
//! that would report it reports nothing. A pass the account allows goes on to the
//! operation's own rules and mints the job.
//!
//! A refusal has to be GROUNDED in a document that was read, so a caller this emulator holds
//! no policies for (the account's own published credential, which is the position AWS account
//! root credentials are in) submits the job — the recorded success path, which
//! [`the_account_credential_passes_the_job_role_no_policy_bounds`] pins.
//!
//! # What the tables generalise over
//!
//! [`every_job_submission_needs_the_callers_pass_role_for_the_role_it_hands_over`] crosses
//! the DOCUMENT in force on the caller (none bearing the IAM action at all, `iam:PassRole` on
//! every resource, on exactly the role being passed, on this account's roles by a wildcard,
//! on another role only, allowed everywhere beside an explicit deny of that role, and the
//! whole `iam` namespace) with every OPERATION that makes such a pass
//! ([`crate::job_pass_role_source_arn::job_creates`], the same table the other half of this
//! decision is asserted over), so no row turns on which create it is.
//!
//! The role itself is held by this account's IAM and trusted to Amazon Bedrock throughout,
//! which is what makes every refusal here attributable to the caller's boundary and to
//! nothing about the role.

use serde_json::{json, Value};

use crate::control_plane_authorization::{given_role, trust, JOB_ROLE};
use crate::custom_model_authorization::{
    emulator, given_role_policy, given_session, ACCOUNT_KEY_ID, SESSION_KEY_ID,
};
use crate::job_pass_role_source_arn::{assert_refused, job_creates, job_listed};
use crate::support::{ACCOUNT, REGION};

/// A role of this account the caller may be allowed to pass INSTEAD of the job role, so a
/// grant scoped to another role is a readable grant that still covers nothing this request
/// asks for.
const OTHER_ROLE: &str = "BedrockSomethingElse";

/// A 12-digit account id that is NOT the calling account.
const OTHER_ACCOUNT: &str = "123456789012";

/// The ARN this account holds `name` under.
fn role_arn(name: &str) -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// One statement of an identity-based policy.
fn statement(effect: &str, action: &str, resource: &str) -> Value {
    json!({ "Effect": effect, "Action": action, "Resource": resource })
}

/// The document in force on the caller: every action of THIS service — so the operation's
/// own authorization is never what a row turns on — plus whatever the row says about the IAM
/// action that decides the pass.
fn caller_document(pass_role: &[Value]) -> Value {
    let mut statements = vec![statement("Allow", "bedrock:*", "*")];
    statements.extend(pass_role.iter().cloned());
    json!({ "Version": "2012-10-17", "Statement": statements })
}

/// One row: what the caller's policies say about `iam:PassRole` for the job role, and
/// whether the account lets this caller make that pass.
struct Row {
    case: &'static str,
    pass_role: Vec<Value>,
    passed: bool,
}

fn rows() -> Vec<Row> {
    let job_role = role_arn(JOB_ROLE);
    vec![
        Row {
            // The shape of a least-privilege policy for this service alone: it allows the
            // submission and says nothing about handing out a role.
            case: "a caller allowed this service's actions and no IAM action",
            pass_role: Vec::new(),
            passed: false,
        },
        Row {
            case: "a caller allowed iam:PassRole on every resource",
            pass_role: vec![statement("Allow", "iam:PassRole", "*")],
            passed: true,
        },
        Row {
            case: "a caller allowed iam:PassRole on exactly the role it passes",
            pass_role: vec![statement("Allow", "iam:PassRole", &job_role)],
            passed: true,
        },
        Row {
            case: "a caller allowed iam:PassRole on this account's roles by a wildcard",
            pass_role: vec![statement(
                "Allow",
                "iam:PassRole",
                &format!("arn:aws:iam::{ACCOUNT}:role/*"),
            )],
            passed: true,
        },
        Row {
            case: "a caller allowed iam:PassRole on another role only",
            pass_role: vec![statement("Allow", "iam:PassRole", &role_arn(OTHER_ROLE))],
            passed: false,
        },
        Row {
            case: "a caller allowed iam:PassRole on another ACCOUNT's roles only",
            pass_role: vec![statement(
                "Allow",
                "iam:PassRole",
                &format!("arn:aws:iam::{OTHER_ACCOUNT}:role/*"),
            )],
            passed: false,
        },
        Row {
            case: "a caller allowed iam:PassRole everywhere and denied it on that role",
            pass_role: vec![
                statement("Allow", "iam:PassRole", "*"),
                statement("Deny", "iam:PassRole", &job_role),
            ],
            passed: false,
        },
        Row {
            case: "a caller allowed the whole iam namespace",
            pass_role: vec![statement("Allow", "iam:*", "*")],
            passed: true,
        },
    ]
}

/// RULE: a job is submitted only where the account's IAM lets the CALLING principal pass the
/// role the submission hands Amazon Bedrock; any other answer is 403 `AccessDeniedException`
/// with the one sentence that denial is stated with, and writes no job.
#[tokio::test]
async fn every_job_submission_needs_the_callers_pass_role_for_the_role_it_hands_over() {
    let emu = emulator();
    given_session(&emu.engine);
    // The role every submission passes, held by this account and trusted to this service, so
    // these rows turn on the IDENTITY making the call rather than on the role it passes; and
    // one other role of this account, for the row whose grant is scoped to it.
    let trust_document = trust(None);
    given_role(&emu.engine, JOB_ROLE, Some(&trust_document));
    given_role(&emu.engine, OTHER_ROLE, Some(&trust_document));
    let job_role = role_arn(JOB_ROLE);

    for (row_index, row) in rows().iter().enumerate() {
        given_role_policy(&emu.engine, Some(&caller_document(&row.pass_role)));
        for (create_index, create) in job_creates().iter().enumerate() {
            let name = format!("pr{row_index}-{create_index}");
            let case = format!("{} / {:?}", create.case, row.case);
            let reply = emu
                .call_as(
                    SESSION_KEY_ID,
                    "POST",
                    create.path,
                    Some((create.body)(&name, &job_role)),
                )
                .await;

            if !row.passed {
                assert_refused(&case, &reply);
                assert!(
                    !job_listed(&emu, create, &case, &name).await,
                    "case {case}: a refused pass must leave no job behind"
                );
                continue;
            }

            assert_eq!(
                reply.status, create.success,
                "case {case}: a pass the account allows must be admitted, got {} {:?}",
                reply.status, reply.body
            );
            let arn = reply.text("jobArn");
            // The ARN is one of the job family this create mints, in this account and
            // Region: every job ARN of this service is built that way, whatever the family
            // spells after it.
            let prefix = format!("arn:aws:bedrock:{REGION}:{ACCOUNT}:{}/", create.family);
            assert!(
                arn.starts_with(&prefix),
                "case {case}: expected an ARN under {prefix:?}, got {arn:?}"
            );
            // The job the admitted pass was decided for is the job the create minted: the
            // operation's own read reports it, in a status it has just been put in.
            let read = emu
                .call_as(ACCOUNT_KEY_ID, "GET", &(create.read)(&arn), None)
                .await;
            assert_eq!(
                read.status, 200,
                "case {case}: the minted job must be readable back, got {} {:?}",
                read.status, read.body
            );
            let status = read.text("status");
            assert!(
                create.non_terminal.contains(&status.as_str()),
                "case {case}: expected one of {:?}, got {status:?}",
                create.non_terminal
            );
        }
    }
}

/// RULE (the grounding half): a refusal has to rest on a document that was READ, so a caller
/// this emulator holds no policies for submits the job — and the same caller whose policies
/// bear no IAM action at all is refused, so the difference is the pass and not the
/// submission.
#[tokio::test]
async fn the_account_credential_passes_the_job_role_no_policy_bounds() {
    let emu = emulator();
    given_session(&emu.engine);
    given_role(&emu.engine, JOB_ROLE, Some(&trust(None)));
    let job_role = role_arn(JOB_ROLE);

    for (create_index, create) in job_creates().iter().enumerate() {
        // The account's own published credential holds no IAM policies, which is the position
        // AWS account root credentials are in: the pass is admitted and the job is minted.
        let name = format!("acctcred{create_index}");
        let case = format!("{} / \"the account's own credential\"", create.case);
        let reply = emu
            .call_as(
                ACCOUNT_KEY_ID,
                "POST",
                create.path,
                Some((create.body)(&name, &job_role)),
            )
            .await;
        assert_eq!(
            reply.status, create.success,
            "case {case}: a caller no policy bounds must be served, got {} {:?}",
            reply.status, reply.body
        );

        // The SAME request from a principal whose documents this emulator DOES hold, and
        // which allow no IAM action, is refused and leaves nothing behind.
        given_role_policy(&emu.engine, Some(&caller_document(&[])));
        let bounded = format!("bounded{create_index}");
        let bounded_case = format!("{} / \"a caller allowed no IAM action\"", create.case);
        let refused = emu
            .call_as(
                SESSION_KEY_ID,
                "POST",
                create.path,
                Some((create.body)(&bounded, &job_role)),
            )
            .await;
        assert_refused(&bounded_case, &refused);
        assert!(
            !job_listed(&emu, create, &bounded_case, &bounded).await,
            "case {bounded_case}: a refused pass must leave no job behind"
        );
    }
}
