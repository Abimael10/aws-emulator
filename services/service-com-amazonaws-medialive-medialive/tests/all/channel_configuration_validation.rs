// SPDX-License-Identifier: CC0-1.0

//! The channel CONFIGURATION a write accepts, end to end over the restJson1
//! wire.
//!
//! Rule: an operation that writes a channel's configuration (`CreateChannel`,
//! `UpdateChannel`) validates the WHOLE configuration before anything is
//! stored, and refuses one it could not run with `UnprocessableEntityException`
//! (422) carrying one `ValidationErrors` entry per problem — each naming the
//! element path the problem was found at — leaving the store exactly as it was.
//! Two problems make a configuration unrunnable: it declares no encoder settings
//! (what the channel encodes), and an `InputAttachments` entry names no input at
//! all.
//!
//! The input an attachment DOES name is resolved as a fact about the REQUEST, so
//! an entry naming an id no input of this account answers to is refused with the
//! operation's modelled `BadRequestException` (400) naming that input, not as one
//! more entry of the 422 — the suite below is about that rule too, and
//! `crate::channel_create_structure` is about where it sits in the order a create
//! judges a request in.
//!
//! Every test drives the service the way a client does — through
//! `MultiServiceRouter::dispatch` with real HTTP requests — so what is asserted
//! is the emulator's wire behaviour, not an internal function's return value.
//!
//! The suites are tables: the empty-body create and the one attachment naming an
//! unminted id are ONE row each, and the other rows are what the rule
//! generalises over — which member is missing, how many problems one request
//! carries, where in the attachment list the bad entry sits, what makes the
//! reference unusable (never minted, named at all) and
//! whether the request asked to be performed or only checked.

use serde_json::{json, Value};
use service_com_amazonaws_medialive_medialive::state::{
    missing_reference_id_message, not_found_message, required_channel_configuration_message,
    CHANNEL_CONTAINS_ERRORS_MESSAGE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::channel_lifecycle::wait_for_the_clock;
use crate::support::{attachable_input, channel_encoder_settings};
use crate::wire::{call, error_code, error_message, medialive};

/// An id of the shape this service mints for an input, that no `CreateInput`
/// ever minted in the account under test.
const UNMINTED_INPUT_ID: &str = "9999999";

/// One problem a refusal reports: the element path it was found at, and the
/// message that says what it is.
type Problem = (String, String);

/// One row of a refusal table: the case, the request body, and the problems the
/// refusal must report, in order.
type RefusalRow = (&'static str, Value, Vec<Problem>);

/// One row of the update-refusal table: the case, a builder of the request body
/// from the id of the input the row's own channel holds, and the problems the
/// refusal must report.
type UpdateRefusalRow = (&'static str, fn(&str) -> Value, Vec<Problem>);

/// `CreateChannel` with `body`.
async fn create(router: &MultiServiceRouter, body: Value) -> (u16, Value) {
    call(router, "POST", "/prod/channels", Some(body)).await
}

/// `UpdateChannel` on `id` with `body`.
async fn update(router: &MultiServiceRouter, id: &str, body: Value) -> (u16, Value) {
    call(router, "PUT", &format!("/prod/channels/{id}"), Some(body)).await
}

/// `DescribeChannel` on `id`.
async fn describe(router: &MultiServiceRouter, id: &str) -> (u16, Value) {
    call(router, "GET", &format!("/prod/channels/{id}"), None).await
}

/// Every channel this account holds, as `ListChannels` reports them.
async fn listed(router: &MultiServiceRouter) -> Vec<Value> {
    let (status, body) = call(router, "GET", "/prod/channels", None).await;
    assert_eq!(status, 200, "ListChannels: got {status}: {body}");
    body["channels"].as_array().cloned().unwrap_or_default()
}

/// One attachment entry: the attachment name, and the `InputId` it names (none
/// at all when `input_id` is `None`).
fn attachment(name: &str, input_id: Option<&str>) -> Value {
    match input_id {
        Some(id) => json!({ "inputAttachmentName": name, "inputId": id }),
        None => json!({ "inputAttachmentName": name }),
    }
}

/// The `(elementPath, errorMessage)` pairs a refusal reported, in the order it
/// reported them.
fn reported_problems(body: &Value) -> Vec<Problem> {
    body["validationErrors"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|problem| {
            (
                problem["elementPath"].as_str().unwrap_or_default().to_string(),
                problem["errorMessage"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// Assert `body` is the modelled 422 refusal carrying exactly `expected`, one
/// entry per problem, in that order.
fn assert_unprocessable(
    case: &str,
    status: u16,
    body: &Value,
    expected: &[Problem],
) {
    assert_eq!(
        status, 422,
        "case {case:?}: expected status 422, got {status}: {body}"
    );
    assert_eq!(
        error_code(status, body),
        "UnprocessableEntityException",
        "case {case:?}: expected UnprocessableEntityException, got {body}"
    );
    assert_eq!(
        error_message(body),
        CHANNEL_CONTAINS_ERRORS_MESSAGE,
        "case {case:?}: expected the message {CHANNEL_CONTAINS_ERRORS_MESSAGE:?}, got {:?}",
        error_message(body)
    );
    assert_eq!(
        reported_problems(body),
        expected,
        "case {case:?}: expected the problems {expected:?}, got {:?}",
        body["validationErrors"]
    );
}

/// The problem a configuration that declares no encoder settings carries.
fn missing_encoder_settings() -> Problem {
    (
        "encoderSettings".to_string(),
        required_channel_configuration_message("encoderSettings"),
    )
}

/// The problem an attachment at `index` naming no input at all carries.
fn nameless_attachment(index: usize) -> Problem {
    (
        format!("inputAttachments/{index}/inputId"),
        missing_reference_id_message("inputAttachments"),
    )
}

/// Assert `body` is the modelled 400 refusal naming the input `id` the request
/// named and that this account holds no input for.
fn assert_unknown_input(case: &str, status: u16, body: &Value, id: &str) {
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
        not_found_message("Input", id),
        "case {case:?}: expected the refusal to name the input, got {:?}",
        error_message(body)
    );
}

/// Rule: `CreateChannel` refuses a configuration it could not run with 422 and
/// one `ValidationErrors` entry per problem, and creates NO channel.
///
/// The table spans what makes a configuration unrunnable and how many problems
/// one request carries: a body with no configuration member, a body that names
/// only members outside the configuration, an attachment that names no input at
/// all at each position of the list, and a request carrying both classes of
/// problem at once (whose entries come in the order the configuration declares
/// its members). Every row names the channel, because a create with no name is
/// refused for THAT before any configuration is read
/// (`crate::request_validation_refusals`), and every row that declares no
/// attachment of its own is sent the one a client's own create carries
/// (`crate::wire`), because a create that attaches nothing is refused for THAT
/// first (`crate::channel_create_structure`).
#[tokio::test]
async fn a_create_refuses_a_configuration_it_could_not_run() {
    // Rows are built against one input this account really holds, so a row can
    // say "this entry resolves" beside one that does not.
    let router = medialive();
    let held = attachable_input(&router, "held").await;

    let table: Vec<RefusalRow> = vec![
        (
            "no configuration member at all",
            json!({ "name": "no-configuration" }),
            vec![missing_encoder_settings()],
        ),
        (
            "only members outside the configuration",
            json!({ "name": "nameless-configuration", "logLevel": "INFO", "tags": { "a": "b" } }),
            vec![missing_encoder_settings()],
        ),
        (
            "a resolvable attachment and a nameless one after it",
            json!({
                "name": "one-of-each",
                "encoderSettings": channel_encoder_settings(),
                "inputAttachments": [
                    attachment("held", Some(&held)),
                    attachment("nameless", None),
                ],
            }),
            vec![nameless_attachment(1)],
        ),
        (
            "two attachments naming no input",
            json!({
                "name": "two-nameless",
                "encoderSettings": channel_encoder_settings(),
                "inputAttachments": [
                    attachment("nameless", None),
                    attachment("empty", Some("")),
                ],
            }),
            vec![nameless_attachment(0), nameless_attachment(1)],
        ),
        (
            "an attachment naming no input at all",
            json!({
                "name": "nameless-attachment",
                "encoderSettings": channel_encoder_settings(),
                "inputAttachments": [attachment("a", None)],
            }),
            vec![nameless_attachment(0)],
        ),
        (
            "no encoder settings and an attachment naming no input",
            json!({
                "name": "two-problems",
                "inputAttachments": [attachment("a", None)],
            }),
            vec![missing_encoder_settings(), nameless_attachment(0)],
        ),
    ];

    let before = listed(&router).await.len();
    for (case, body, expected) in table {
        let (status, refused) = create(&router, body).await;
        assert_unprocessable(case, status, &refused, &expected);
        assert_eq!(
            refused.get("channel"),
            None,
            "case {case:?}: expected no channel in the refusal, got {refused}"
        );

        // Nothing was stored for the refusal: the account holds exactly the
        // channels it held before it.
        let held_now = listed(&router).await;
        assert_eq!(
            held_now.len(),
            before,
            "case {case:?}: expected the refused create to store nothing, got {held_now:?}"
        );
    }
}

/// Rule (the other side of the same rule): a configuration this service can run
/// is created, and the channel reads back through `DescribeChannel` carrying
/// exactly the configuration the request declared.
///
/// The table spans the attachment counts the rule generalises over — one
/// resolvable entry, two entries resolving to two different inputs, and two
/// entries resolving to the same one. A create that attaches NOTHING is refused
/// for that (`crate::channel_create_structure`), so it is not one of these rows.
///
/// Each row gets an account of its own: an input is attached to at most one
/// channel at a time, so a row's inputs are free only while no earlier row's
/// channel still holds them.
#[tokio::test]
async fn a_create_accepts_a_configuration_it_can_run() {
    // Each entry is the attachment's name and WHICH of the row's two inputs it
    // names, so the ids are the ones that row's own account minted.
    let table: Vec<(&str, Vec<(&str, usize)>)> = vec![
        ("one resolvable attachment", vec![("a", 0)]),
        ("two attachments on two inputs", vec![("a", 0), ("b", 1)]),
        ("two attachments on one input", vec![("a", 0), ("b", 0)]),
    ];

    for (case, entries) in table {
        let router = medialive();
        let inputs = [
            attachable_input(&router, "first").await,
            attachable_input(&router, "second").await,
        ];
        let attachments: Vec<Value> = entries
            .iter()
            .map(|(name, input)| attachment(name, Some(&inputs[*input])))
            .collect();
        let body = json!({
            "name": case,
            "encoderSettings": channel_encoder_settings(),
            "inputAttachments": attachments.clone(),
        });
        let (status, created) = create(&router, body).await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected 201, got {status}: {created}"
        );
        let id = created["channel"]["id"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        let (status, read) = describe(&router, &id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeChannel: got {status}: {read}"
        );
        // The stored configuration is the declared one COMPLETED, through the
        // one helper that says what the service completes.
        assert_eq!(
            read["encoderSettings"],
            crate::support::stored_channel_member("encoderSettings", &channel_encoder_settings()),
            "case {case:?}: expected the declared encoder settings, got {:?}",
            read["encoderSettings"]
        );
        assert_eq!(
            read["inputAttachments"],
            crate::support::stored_channel_member("inputAttachments", &json!(attachments)),
            "case {case:?}: expected the declared attachments, got {:?}",
            read["inputAttachments"]
        );
    }
}

/// Rule: a reference resolves against the inputs this account HOLDS, so an
/// attachment naming an id nothing ever minted names no input — a fact about the
/// REQUEST, refused with the operation's modelled `BadRequestException` (400)
/// naming that input, and never a channel — and a `DryRun` request, which asks
/// for the write to be CHECKED rather than performed, earns exactly the same
/// refusal.
///
/// The rows span the WRITE (the create, and the update of a channel that already
/// holds an attachment), the position the unresolvable entry sits at, and whether
/// the request asked to be performed or only checked.
///
/// An id this account DOES hold a record for and whose delete has been accepted
/// is a different fact — that input exists and is identified, it is simply in a
/// state no channel can ingest through — so it is refused in the same class with
/// a message naming that state; `crate::cross_field_refusals` is the suite about
/// that rule.
#[tokio::test]
async fn a_reference_resolves_against_the_inputs_the_account_holds() {
    // (case, whether the request asked to be checked only, the attachments it
    //  carries, built from the input the row's own account holds)
    type Row = (&'static str, bool, fn(&str) -> Value);
    let table: Vec<Row> = vec![
        (
            "a create naming an id nothing minted",
            false,
            |_held| json!([attachment("ghost", Some(UNMINTED_INPUT_ID))]),
        ),
        (
            "a create naming an id nothing minted, checked only",
            true,
            |_held| json!([attachment("ghost", Some(UNMINTED_INPUT_ID))]),
        ),
        (
            "a create whose second entry names an id nothing minted",
            false,
            |held| {
                json!([
                    attachment("held", Some(held)),
                    attachment("ghost", Some(UNMINTED_INPUT_ID)),
                ])
            },
        ),
    ];

    for (case, dry_run, build) in table {
        let router = medialive();
        let held = attachable_input(&router, "held").await;
        let (status, refused) = create(
            &router,
            json!({
                "name": case,
                "dryRun": dry_run,
                "encoderSettings": channel_encoder_settings(),
                "inputAttachments": build(&held),
            }),
        )
        .await;
        assert_unknown_input(case, status, &refused, UNMINTED_INPUT_ID);
        assert!(
            listed(&router).await.is_empty(),
            "case {case:?}: expected no channel to have been created, got {:?}",
            listed(&router).await
        );
    }
}

/// Rule (the same rule, through the other write): an update whose attachment
/// names an id nothing minted earns the same modelled 400 naming that input, and
/// the stored channel keeps the attachments it had.
///
/// The rows span the position the unresolvable entry sits at and whether the
/// update asked to be performed or only checked.
#[tokio::test]
async fn an_update_naming_an_input_the_account_does_not_hold_is_refused() {
    // (case, the update body, built from the input the row's own channel holds)
    type Row = (&'static str, fn(&str) -> Value);
    let table: Vec<Row> = vec![
        (
            "an attachment naming an id nothing minted",
            |_held| json!({ "inputAttachments": [attachment("ghost", Some(UNMINTED_INPUT_ID))] }),
        ),
        (
            "an unresolvable attachment after a resolvable one",
            |held| {
                json!({ "inputAttachments": [
                    attachment("held", Some(held)),
                    attachment("ghost", Some(UNMINTED_INPUT_ID)),
                ] })
            },
        ),
        (
            "a checked-only update whose attachment does not resolve",
            |_held| {
                json!({
                    "dryRun": true,
                    "inputAttachments": [attachment("ghost", Some(UNMINTED_INPUT_ID))],
                })
            },
        ),
    ];

    for (case, build) in table {
        let router = medialive();
        let held = attachable_input(&router, "held").await;
        let (status, created) = create(
            &router,
            json!({
                "name": "baseline",
                "encoderSettings": channel_encoder_settings(),
                "inputAttachments": [attachment("held", Some(&held))],
            }),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: seeding the channel: got {status}: {created}"
        );
        let id = created["channel"]["id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        // An update is accepted from a channel that has stopped, which is the
        // `IDLE` the create's transient state settles into.
        wait_for_the_clock();

        let (status, refused) = update(&router, &id, build(&held)).await;
        assert_unknown_input(case, status, &refused, UNMINTED_INPUT_ID);

        let (status, read) = describe(&router, &id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeChannel: got {status}: {read}"
        );
        assert_eq!(
            read["inputAttachments"],
            crate::support::stored_channel_member(
                "inputAttachments",
                &json!([attachment("held", Some(&held))])
            ),
            "case {case:?}: expected the refused update to leave the attachments alone, got {:?}",
            read["inputAttachments"]
        );
    }
}

/// Rule: `UpdateChannel` validates the configuration the update WOULD leave on
/// the channel — the members its request names over the ones the channel already
/// had — and a refused update leaves the stored channel exactly as it was.
///
/// The table spans the update shapes the rule generalises over: an entry naming
/// no input at either position of the list, and a `DryRun` update, which is
/// checked by the same rule.
///
/// Each row seeds its baseline channel in an account of its own, because an
/// input is attached to at most one channel at a time.
#[tokio::test]
async fn an_update_refuses_a_configuration_it_could_not_run() {
    // Each row builds its request from the id of the input ITS OWN account
    // minted and its channel already holds, so no row depends on another's.
    let table: Vec<UpdateRefusalRow> = vec![
        (
            "an attachment naming no input at all",
            |_held| json!({ "inputAttachments": [attachment("nameless", None)] }),
            vec![nameless_attachment(0)],
        ),
        (
            "a nameless attachment after a resolvable one",
            |held| {
                json!({ "inputAttachments": [
                    attachment("held", Some(held)),
                    attachment("nameless", None),
                ] })
            },
            vec![nameless_attachment(1)],
        ),
        (
            "a checked-only update whose attachment names no input",
            |_held| {
                json!({
                    "dryRun": true,
                    "inputAttachments": [attachment("nameless", None)],
                })
            },
            vec![nameless_attachment(0)],
        ),
    ];

    for (case, build, expected) in table {
        let router = medialive();
        let held = attachable_input(&router, "held").await;
        let baseline = json!({
            "name": "baseline",
            "encoderSettings": channel_encoder_settings(),
            "inputAttachments": [attachment("held", Some(&held))],
        });
        let request = build(&held);
        let (status, created) = create(&router, baseline.clone()).await;
        assert_eq!(
            status, 201,
            "case {case:?}: seeding the channel: got {status}: {created}"
        );
        let id = created["channel"]["id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        // An update is accepted from a channel that has stopped, which is the
        // `IDLE` the create's transient state settles into.
        wait_for_the_clock();

        let (status, refused) = update(&router, &id, request).await;
        assert_unprocessable(case, status, &refused, &expected);

        let (status, read) = describe(&router, &id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeChannel: got {status}: {read}"
        );
        assert_eq!(
            read["inputAttachments"],
            crate::support::stored_channel_member(
                "inputAttachments",
                &json!([attachment("held", Some(&held))])
            ),
            "case {case:?}: expected the refused update to leave the attachments alone, got {:?}",
            read["inputAttachments"]
        );
    }
}

/// Rule (the other side of the update): an update whose configuration this
/// service can run is applied, and the channel reads back carrying it — an
/// update that names no attachment at all keeps the ones the channel had, which
/// is what makes the check one on the configuration the update WOULD leave
/// rather than on the request alone.
///
/// Each row seeds its baseline channel in an account of its own, because an
/// input is attached to at most one channel at a time.
#[tokio::test]
async fn an_update_accepts_a_configuration_it_can_run() {
    // Each row builds its request, and the attachments it expects to read back,
    // from the two inputs ITS OWN account minted: `held`, which the baseline
    // channel attaches, and `other`, which nothing attaches.
    type Row = (&'static str, fn(&str, &str) -> (Value, Value));
    let table: Vec<Row> = vec![
        (
            "no attachment member at all",
            |held, _other| {
                (
                    json!({ "name": "renamed" }),
                    json!([attachment("held", Some(held))]),
                )
            },
        ),
        (
            "another resolvable attachment",
            |_held, other| {
                (
                    json!({ "inputAttachments": [attachment("other", Some(other))] }),
                    json!([attachment("other", Some(other))]),
                )
            },
        ),
        (
            "an empty attachment list",
            |_held, _other| (json!({ "inputAttachments": [] }), json!([])),
        ),
    ];

    for (case, build) in table {
        let router = medialive();
        let held = attachable_input(&router, "held").await;
        let other = attachable_input(&router, "other").await;
        let (request, expected) = build(&held, &other);
        let (status, created) = create(
            &router,
            json!({
                "name": "baseline",
                "encoderSettings": channel_encoder_settings(),
                "inputAttachments": [attachment("held", Some(&held))],
            }),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: seeding the channel: got {status}: {created}"
        );
        let id = created["channel"]["id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        wait_for_the_clock();

        let (status, updated) = update(&router, &id, request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status}: {updated}"
        );

        let (status, read) = describe(&router, &id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeChannel: got {status}: {read}"
        );
        let expected = crate::support::stored_channel_member("inputAttachments", &expected);
        assert_eq!(
            read["inputAttachments"], expected,
            "case {case:?}: expected the attachments {expected:?}, got {:?}",
            read["inputAttachments"]
        );
    }
}
