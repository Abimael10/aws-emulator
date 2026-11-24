// SPDX-License-Identifier: CC0-1.0

//! What a `CreateChannel` request has to say about ITSELF, end to end over the restJson1
//! wire.
//!
//! Rule: MediaLive judges the request's own structure — the members it carries and the
//! resources they NAME — before it resolves the role the channel would run under against
//! IAM, and a channel that attaches no input at all describes nothing this service could
//! run, so such a create is refused with the operation's modelled `BadRequestException`
//! (400) and the fixed sentence "At least one inputAttachment must be specified". A channel
//! INGESTS through the inputs it attaches (`InputAttachments` is "List of input attachments
//! for channel"), which is why the list is not optional in practice however the model
//! declares it.
//!
//! The order matters as much as the refusals do, because the status class is what a client
//! branches on: a 400 says "the request is malformed, fix the payload" and the 422 of the
//! role resolution says "the request is well formed but a resource it references is
//! unusable, fix the IAM role". A request that is both structurally incomplete AND names an
//! unusable role is malformed, so it earns the 400 about the member the caller left out —
//! never the 422 about a member the caller did not get wrong.
//!
//! Every test drives the service the way a client does — through
//! `MultiServiceRouter::dispatch` with real HTTP requests — so what is asserted is the
//! emulator's wire behaviour, not an internal function's return value.
//!
//! The suites are TABLES spanning the class each rule generalises over: the request that
//! attaches nothing is one row of the first, and the recorded create that attaches nothing
//! while naming an unusable role is one row of the second.

use serde_json::{json, Value};
use service_com_amazonaws_medialive_medialive::state::{
    invalid_role_arn_message, not_found_message, MISSING_INPUT_ATTACHMENT_MESSAGE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::request_validation_refusals::{
    medialive_with_iam, role_trusting, MEDIALIVE_PRINCIPAL, OTHER_PRINCIPAL,
};
use crate::support::{attachable_input, channel_encoder_settings};
use crate::wire::{call, error_code, error_message, medialive, ACCOUNT};

/// An id of the shape this service mints for an input, that no `CreateInput` ever minted in
/// the account under test.
const UNMINTED_INPUT_ID: &str = "9999999";

/// How a request says it carries NO `inputAttachments` member at all: the harness fills the
/// member a client's own create would carry, and an explicit JSON `null` is how a suite
/// whose rule IS the absent member tells it to send none (`crate::wire`).
fn no_attachment_member() -> Value {
    Value::Null
}

/// A create body carrying everything a channel needs EXCEPT the attachments, which each row
/// says its own thing about.
fn channel_body(name: &str) -> Value {
    json!({ "name": name, "encoderSettings": channel_encoder_settings() })
}

/// One attachment of the input `id`.
fn attachment(name: &str, id: &str) -> Value {
    json!({ "inputAttachmentName": name, "inputId": id })
}

/// `CreateChannel` with `body`, sent as written.
async fn create(router: &MultiServiceRouter, body: Value) -> (u16, Value) {
    call(router, "POST", "/prod/channels", Some(body)).await
}

/// Every channel this account holds, as `ListChannels` reports them.
async fn listed(router: &MultiServiceRouter) -> Vec<Value> {
    let (status, body) = call(router, "GET", "/prod/channels", None).await;
    assert_eq!(status, 200, "ListChannels: got {status}: {body}");
    body["channels"].as_array().cloned().unwrap_or_default()
}

/// Assert `body` is the operation's modelled 400 carrying exactly `message`, and that the
/// refused create stored nothing.
async fn assert_refused(
    case: &str,
    router: &MultiServiceRouter,
    status: u16,
    body: &Value,
    message: &str,
) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected status 400, got {status}: {body}"
    );
    assert_eq!(
        error_code(status, body),
        "BadRequestException",
        "case {case:?}: expected the modelled BadRequestException, got {body}"
    );
    assert_eq!(
        error_message(body),
        message,
        "case {case:?}: expected the message {message:?}, got {:?}",
        error_message(body)
    );
    let held = listed(router).await;
    assert!(
        held.is_empty(),
        "case {case:?}: expected the refused create to store nothing, got {held:?}"
    );
}

/// Rule: a create that attaches no input is refused with the modelled 400 and the fixed
/// sentence, and nothing is stored for it.
///
/// The rows span the ways a request attaches nothing (the member absent, and the member
/// present as the empty list), what else the request carries beside it (a role, a class and
/// a maintenance window, tags, a destination), and whether the request asked to be performed
/// or only CHECKED — a `DryRun` create is answered every refusal the real create would
/// answer. Each row gets an account of its own, so "nothing was stored" is about that row's
/// own create.
#[tokio::test]
async fn a_create_that_attaches_no_input_is_refused() {
    // (case, the attachments the request carries, the rest of the body it carries)
    let table: [(&str, Value, Value); 6] = [
        ("no attachments member at all", no_attachment_member(), json!({})),
        ("an empty attachments list", json!([]), json!({})),
        (
            "no attachments member beside a role",
            no_attachment_member(),
            json!({ "roleArn": format!("arn:aws:iam::{ACCOUNT}:role/medialive-runner") }),
        ),
        (
            "an empty attachments list beside a class and a maintenance window",
            json!([]),
            json!({
                "channelClass": "SINGLE_PIPELINE",
                "maintenance": { "maintenanceDay": "SUNDAY", "maintenanceStartTime": "02:00" },
            }),
        ),
        (
            "no attachments member beside tags and a destination",
            no_attachment_member(),
            json!({
                "tags": { "owner": "broadcast" },
                "destinations": [{ "id": "out" }],
            }),
        ),
        (
            "a checked-only create that attaches nothing",
            json!([]),
            json!({ "dryRun": true }),
        ),
    ];

    for (case, attachments, rest) in table {
        let router = medialive();
        let mut body = channel_body("attaches-nothing");
        body["inputAttachments"] = attachments;
        for (member, value) in rest.as_object().cloned().unwrap_or_default() {
            body[member] = value;
        }

        let (status, refused) = create(&router, body).await;
        assert_refused(
            case,
            &router,
            status,
            &refused,
            MISSING_INPUT_ATTACHMENT_MESSAGE,
        )
        .await;
    }
}

/// Rule (the other side of the same rule): the SAME request with one attachment of an input
/// this account holds is created, and the stored channel attaches it.
///
/// The rows span the attachment counts a create is accepted with, including two entries on
/// the same input, which is what makes the rule "at least one" rather than "exactly one".
#[tokio::test]
async fn a_create_that_attaches_an_input_is_accepted() {
    // (case, how many attachments the request carries, how many inputs they name)
    let table: [(&str, usize, usize); 3] = [
        ("one attachment", 1, 1),
        ("two attachments on two inputs", 2, 2),
        ("two attachments on one input", 2, 1),
    ];

    for (case, entries, inputs) in table {
        let router = medialive();
        let held: Vec<String> = {
            let mut minted = Vec::new();
            for index in 0..inputs {
                minted.push(attachable_input(&router, &format!("feed-{index}")).await);
            }
            minted
        };
        let attachments: Vec<Value> = (0..entries)
            .map(|index| attachment(&format!("feed-{index}"), &held[index % inputs]))
            .collect();
        let mut body = channel_body("attaches-an-input");
        body["inputAttachments"] = json!(attachments);

        let (status, created) = create(&router, body).await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected 201, got {status}: {created}"
        );
        let named: Vec<String> = created["channel"]["inputAttachments"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|entry| entry["inputId"].as_str().unwrap_or_default().to_string())
            .collect();
        let expected: Vec<String> = attachments
            .iter()
            .map(|entry| entry["inputId"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            named, expected,
            "case {case:?}: expected the created channel to attach {expected:?}, got {named:?}"
        );
    }
}

/// Rule: the request's own structure and the resources it NAMES are judged before the role
/// it names is resolved against IAM, so a request that is both malformed and names an
/// unusable role earns the 400 about the request — and only a request this service accepts
/// as a request reaches the 422 about the role.
///
/// The rows span what is wrong with the request beside the role (nothing, no attachment at
/// all, an attachment naming an input the account does not hold), the role the request names
/// (one IAM does not hold, one it holds that trusts another service, a value that is not the
/// ARN of a role at all, and one this service may assume), and so every outcome the pair can
/// have: the 400 about the member the caller left out, the 400 about the member it
/// misspelled, the 400 about the input it named, the 422 about the role, and the create that
/// is served.
#[tokio::test]
async fn the_request_is_judged_before_the_role_it_names_is_resolved() {
    /// What the row's `inputAttachments` says: nothing at all, an input the account holds,
    /// or an id no input of the account answers to.
    #[derive(Clone, Copy)]
    enum Attaches {
        Nothing,
        AnInputTheAccountHolds,
        AnInputTheAccountDoesNotHold,
    }
    /// Which role the row names.
    #[derive(Clone, Copy)]
    enum Names {
        ARoleIamDoesNotHold,
        ARoleTrustingSomebodyElse,
        NoRoleArnAtAll,
        AnAssumableRole,
    }
    /// What the service answers: the refusal class and the member it is about, or the
    /// create it serves.
    #[derive(Clone, Copy)]
    enum Answers {
        /// The modelled 400 about the attachment list the request left out.
        BadRequestAboutTheMissingAttachment,
        /// The modelled 400 about the role ARN the request misspelled.
        BadRequestAboutTheMalformedRole,
        /// The modelled 400 about the input the request named.
        BadRequestAboutTheUnknownInput,
        /// The modelled 422 about the role the request names.
        Unprocessable,
        /// The create this service serves.
        Created,
    }

    let (router, iam) = medialive_with_iam();
    let assumable = role_trusting(&iam, "medialive-runner", MEDIALIVE_PRINCIPAL).await;
    let somebody_elses = role_trusting(&iam, "lambda-runner", OTHER_PRINCIPAL).await;
    let unheld = format!("arn:aws:iam::{ACCOUNT}:role/not-provisioned");
    // Not the ARN of an IAM role at all, so it names no role to resolve.
    let not_an_arn = "MediaLiveAccessRole";

    let table: [(&str, Attaches, Names, Answers); 7] = [
        (
            "no attachment and a role IAM does not hold",
            Attaches::Nothing,
            Names::ARoleIamDoesNotHold,
            Answers::BadRequestAboutTheMissingAttachment,
        ),
        (
            "no attachment and a role trusting somebody else",
            Attaches::Nothing,
            Names::ARoleTrustingSomebodyElse,
            Answers::BadRequestAboutTheMissingAttachment,
        ),
        (
            "no attachment and a value that is not a role ARN",
            Attaches::Nothing,
            Names::NoRoleArnAtAll,
            Answers::BadRequestAboutTheMalformedRole,
        ),
        (
            "an attachment naming no input of this account, and an unusable role",
            Attaches::AnInputTheAccountDoesNotHold,
            Names::ARoleTrustingSomebodyElse,
            Answers::BadRequestAboutTheUnknownInput,
        ),
        (
            "an attachment this account holds and a role trusting somebody else",
            Attaches::AnInputTheAccountHolds,
            Names::ARoleTrustingSomebodyElse,
            Answers::Unprocessable,
        ),
        (
            "an attachment this account holds and a role IAM does not hold",
            Attaches::AnInputTheAccountHolds,
            Names::ARoleIamDoesNotHold,
            Answers::Unprocessable,
        ),
        (
            "an attachment this account holds and an assumable role",
            Attaches::AnInputTheAccountHolds,
            Names::AnAssumableRole,
            Answers::Created,
        ),
    ];

    for (case, attaches, names, answers) in table {
        let mut body = channel_body(case);
        body["inputAttachments"] = match attaches {
            Attaches::Nothing => no_attachment_member(),
            Attaches::AnInputTheAccountHolds => {
                let held = attachable_input(&router, "feed").await;
                json!([attachment("feed", &held)])
            }
            Attaches::AnInputTheAccountDoesNotHold => {
                json!([attachment("ghost", UNMINTED_INPUT_ID)])
            }
        };
        body["roleArn"] = match names {
            Names::ARoleIamDoesNotHold => json!(unheld),
            Names::ARoleTrustingSomebodyElse => json!(somebody_elses),
            Names::NoRoleArnAtAll => json!(not_an_arn),
            Names::AnAssumableRole => json!(assumable),
        };

        let (status, answered) = create(&router, body).await;
        match answers {
            Answers::BadRequestAboutTheMissingAttachment
            | Answers::BadRequestAboutTheMalformedRole
            | Answers::BadRequestAboutTheUnknownInput => {
                let message = match answers {
                    Answers::BadRequestAboutTheMalformedRole => {
                        invalid_role_arn_message(not_an_arn)
                    }
                    Answers::BadRequestAboutTheUnknownInput => {
                        not_found_message("Input", UNMINTED_INPUT_ID)
                    }
                    _ => MISSING_INPUT_ATTACHMENT_MESSAGE.to_string(),
                };
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected status 400, got {status}: {answered}"
                );
                assert_eq!(
                    error_code(status, &answered),
                    "BadRequestException",
                    "case {case:?}: expected the modelled BadRequestException, got {answered}"
                );
                assert_eq!(
                    error_message(&answered),
                    message,
                    "case {case:?}: expected the message {message:?}, got {:?}",
                    error_message(&answered)
                );
            }
            Answers::Unprocessable => {
                assert_eq!(
                    status, 422,
                    "case {case:?}: expected status 422, got {status}: {answered}"
                );
                assert_eq!(
                    error_code(status, &answered),
                    "UnprocessableEntityException",
                    "case {case:?}: expected the modelled UnprocessableEntityException, got \
                     {answered}"
                );
                let problems = answered["validationErrors"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                assert_eq!(
                    problems.iter().map(|problem| problem["elementPath"].clone()).collect::<Vec<_>>(),
                    vec![json!("roleArn")],
                    "case {case:?}: expected the 422 to report the role, got {answered}"
                );
            }
            Answers::Created => {
                assert_eq!(
                    status, 201,
                    "case {case:?}: expected 201, got {status}: {answered}"
                );
            }
        }
    }

    // Exactly one channel was created across the table: the row this service served.
    let held = listed(&router).await;
    assert_eq!(
        held.len(),
        1,
        "expected only the served create to have stored a channel, got {held:?}"
    );
}
