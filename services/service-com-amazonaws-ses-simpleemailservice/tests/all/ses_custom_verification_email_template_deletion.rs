// SPDX-License-Identifier: CC0-1.0

//! `DeleteCustomVerificationEmailTemplate`, driven end to end over the awsQuery wire:
//! form-urlencoded body in, XML out, one shared engine behind every call, with the
//! effect proved through the readers of the same store
//! (`GetCustomVerificationEmailTemplate`, `ListCustomVerificationEmailTemplates`,
//! `SendCustomVerificationEmail`).
//!
//! What is asserted is the RULE the operation follows over a SET of inputs, not one
//! recorded request:
//!
//! * a delete removes the record held under EXACTLY the name the request carries, so
//!   every later reader of this account's store stops finding it and every other
//!   template it holds is untouched;
//! * the name is matched byte for byte, so a name differing in case, in surrounding
//!   whitespace or in any other byte removes nothing;
//! * a delete of a name this account holds no template under is a SUCCESS: the
//!   operation models no error at all (`smithy_query(query="operation",
//!   operation="DeleteCustomVerificationEmailTemplate")` lists none), so deleting
//!   twice, or deleting a name never created, is answered exactly like the first
//!   delete of a stored one;
//! * the name a delete frees is free again, and so is the slot it held in the
//!   account's bounded template store.

use crate::harness::{
    call, element, elements, emulator_and_engine, error_envelope, field, settle_verifications,
    xml_text, Field,
};
use service_com_amazonaws_ses_simpleemailservice::state;
use service_com_amazonaws_ses_simpleemailservice::state::MAX_CUSTOM_VERIFICATION_EMAIL_TEMPLATES;
use smithy_server_runtime::MultiServiceRouter;
use std::sync::Arc;

/// The sender every template in this suite is sent from, verified on each store below.
const SENDER: &str = "verification@example.com";

/// The recipient a send names — a legal address, so a send reaches the template lookup
/// rather than being refused for the address.
const RECIPIENT: &str = "recipient@example.net";

/// The code SES answers for a custom verification email template name the account holds
/// nothing under (the model's `awsQueryError` code for
/// `CustomVerificationEmailTemplateDoesNotExistException`).
const DOES_NOT_EXIST: &str = "CustomVerificationEmailTemplateDoesNotExist";

/// A router whose account may send and whose sending identity is verified, so every
/// call below reaches the template store.
async fn sending_account() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let (router, engine) = emulator_and_engine();
    // `SendCustomVerificationEmail` reads the account's sandbox state before anything
    // the request names, so this account is taken out of the sandbox.
    let store = state::service_state(&*engine).expect("this service has a store on the engine");
    store.lock().account.production_access_granted = true;
    let (status, body) = call(
        &router,
        "VerifyEmailIdentity",
        &[field("EmailAddress", SENDER)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case seeding {SENDER:?}: expected 200, got {status} with {body}"
    );
    // Asking for the verification only STARTS it; a template's own sender has to be a
    // verified identity, so the store is settled past the span the clock owes that
    // record rather than slept for.
    settle_verifications(&engine);
    (router, engine)
}

/// The six members a create carries, each derived from `name` so one template's stored
/// values are never mistaken for another's.
fn create_params(name: &str) -> Vec<Field> {
    vec![
        field("TemplateName", name),
        field("FromEmailAddress", SENDER),
        field("TemplateSubject", format!("Subject of {name}")),
        field(
            "TemplateContent",
            format!("<html><body>Content of {name}</body></html>"),
        ),
        field("SuccessRedirectionURL", "https://example.com/ok"),
        field("FailureRedirectionURL", "https://example.com/no"),
    ]
}

/// `CreateCustomVerificationEmailTemplate`, asserted to land.
async fn seed(router: &MultiServiceRouter, case: &str, name: &str) {
    let (status, body) = call(
        router,
        "CreateCustomVerificationEmailTemplate",
        &create_params(name),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 seeding {name:?}, got {status} with {body}"
    );
}

/// `DeleteCustomVerificationEmailTemplate`.
async fn delete(router: &MultiServiceRouter, name: &str) -> (u16, String) {
    call(
        router,
        "DeleteCustomVerificationEmailTemplate",
        &[field("TemplateName", name)],
    )
    .await
}

/// `DeleteCustomVerificationEmailTemplate`, asserted to be answered as a success: the
/// modelled 200 with this operation's own result element and no error envelope in it.
async fn delete_ok(router: &MultiServiceRouter, case: &str, name: &str) {
    let (status, body) = delete(router, name).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 deleting {name:?}, got {status} with {body}"
    );
    assert_eq!(
        error_envelope(&body),
        (None, None),
        "case {case:?}: expected no error envelope deleting {name:?}, got {body}"
    );
    assert!(
        body.contains("<DeleteCustomVerificationEmailTemplateResponse"),
        "case {case:?}: expected this operation's awsQuery envelope, got {body}"
    );
}

/// `GetCustomVerificationEmailTemplate`.
async fn get(router: &MultiServiceRouter, name: &str) -> (u16, String) {
    call(
        router,
        "GetCustomVerificationEmailTemplate",
        &[field("TemplateName", name)],
    )
    .await
}

/// Assert the store HOLDS `name`, with the members a create of it wrote.
async fn assert_stored(router: &MultiServiceRouter, case: &str, name: &str) {
    let (status, body) = get(router, name).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 reading {name:?} back, got {status} with {body}"
    );
    for (member, want) in [
        ("TemplateName", name.to_string()),
        ("FromEmailAddress", SENDER.to_string()),
        ("TemplateSubject", format!("Subject of {name}")),
        (
            "TemplateContent",
            format!("<html><body>Content of {name}</body></html>"),
        ),
    ] {
        let got = element(&body, member)
            .map(|raw| xml_text(&raw))
            .unwrap_or_default();
        assert_eq!(
            got, want,
            "case {case:?}: expected {member} {want:?} for {name:?}, got {got:?}"
        );
    }
}

/// Assert the store holds NOTHING under `name`: the read states the modelled absence.
async fn assert_absent(router: &MultiServiceRouter, case: &str, name: &str) {
    let (status, body) = get(router, name).await;
    assert_eq!(
        (status, error_envelope(&body).0),
        (400, Some(DOES_NOT_EXIST.to_string())),
        "case {case:?}: expected the modelled absence for {name:?}, got {status} with {body}"
    );
}

/// Every template name `ListCustomVerificationEmailTemplates` reports, following each
/// `NextToken` until none comes back.
async fn listed_names(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    // Bounded above every page count this suite can ask for, so a listing that never
    // terminates fails here instead of looping forever.
    for _ in 0..1_000 {
        let mut params: Vec<Field> = Vec::new();
        if let Some(token) = token.as_deref() {
            params.push(field("NextToken", token));
        }
        let (status, body) = call(router, "ListCustomVerificationEmailTemplates", &params).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 listing templates, got {status} with {body}"
        );
        let list = element(&body, "CustomVerificationEmailTemplates").unwrap_or_default();
        for member in elements(&list, "member") {
            names.push(
                element(&member, "TemplateName")
                    .map(|raw| xml_text(&raw))
                    .unwrap_or_default(),
            );
        }
        token = element(&body, "NextToken");
        if token.is_none() {
            return names;
        }
    }
    panic!("case {case:?}: the listing never ran out of pages");
}

/// A response body with the per-call request id taken out of it, so two answers that
/// differ only in that id compare equal.
fn without_request_id(body: &str) -> String {
    const OPEN: &str = "<RequestId>";
    const CLOSE: &str = "</RequestId>";
    match (body.find(OPEN), body.find(CLOSE)) {
        (Some(start), Some(end)) if start + OPEN.len() <= end => {
            format!("{}{}", &body[..start + OPEN.len()], &body[end..])
        }
        _ => body.to_string(),
    }
}

/// `SendCustomVerificationEmail` naming `name`.
async fn send(router: &MultiServiceRouter, name: &str) -> (u16, String) {
    call(
        router,
        "SendCustomVerificationEmail",
        &[
            field("EmailAddress", RECIPIENT),
            field("TemplateName", name),
        ],
    )
    .await
}

/// The names a delete is driven with, spanning the forms a template name arrives in:
/// plain, dotted, non-ASCII, carrying XML metacharacters, and a long one. Each is both
/// created and deleted, so the row varies the NAME and nothing else.
fn deletable_names() -> Vec<(&'static str, String)> {
    vec![
        ("a plain name", "verify-template".to_string()),
        (
            "a name carrying dots and digits",
            "verify.template.2".to_string(),
        ),
        ("a non-ASCII name", "vorlage-bestätigung".to_string()),
        (
            "a name carrying XML metacharacters",
            "tmpl<&>\"'delete".to_string(),
        ),
        ("a name of 512 characters", "delete-".repeat(64) + "x"),
        ("a name of one character", "x".to_string()),
    ]
}

#[tokio::test]
async fn a_delete_removes_the_named_template_and_every_reader_stops_finding_it() {
    let (router, _engine) = sending_account().await;

    // One bystander template on the store for the whole suite, so each row also
    // asserts that a delete removes the record it names and no other.
    const BYSTANDER: &str = "bystander-template";
    seed(&router, "bystander", BYSTANDER).await;

    for (case, name) in deletable_names() {
        seed(&router, case, &name).await;
        assert_stored(&router, case, &name).await;

        // The positive control before the delete: every reader of this store serves
        // the template, so what the delete changes is the record and not the reader.
        let listed = listed_names(&router, case).await;
        assert!(
            listed.contains(&name),
            "case {case:?}: expected the listing to report {name:?} before the delete, got \
             {listed:?}"
        );
        let (status, body) = send(&router, &name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected a send naming {name:?} to go through before the delete, \
             got {status} with {body}"
        );

        delete_ok(&router, case, &name).await;

        // Every reader of the same store stops finding it.
        assert_absent(&router, case, &name).await;
        let listed = listed_names(&router, case).await;
        assert!(
            !listed.contains(&name),
            "case {case:?}: expected the listing to omit {name:?} after the delete, got {listed:?}"
        );
        let (status, body) = send(&router, &name).await;
        assert_eq!(
            (status, error_envelope(&body).0),
            (400, Some(DOES_NOT_EXIST.to_string())),
            "case {case:?}: expected a send naming the deleted {name:?} to be refused, got \
             {status} with {body}"
        );

        // …and the template the delete did not name still stands.
        assert_stored(&router, case, BYSTANDER).await;
        let listed = listed_names(&router, case).await;
        assert!(
            listed.contains(&BYSTANDER.to_string()),
            "case {case:?}: expected the listing to keep reporting {BYSTANDER:?}, got {listed:?}"
        );
    }
}

#[tokio::test]
async fn a_delete_matches_the_stored_name_byte_for_byte() {
    let (router, _engine) = sending_account().await;

    const STORED: &str = "stored-template";
    seed(&router, "the stored template", STORED).await;

    // The rows differ in HOW the name the delete carries differs from the stored one:
    // its case, surrounding whitespace, a prefix of it, one character more, the empty
    // name, and a name never created. None of them names the stored record, so none of
    // them removes it — and each is still answered as a success.
    let rows: Vec<(&str, String)> = vec![
        ("another case of the stored name", STORED.to_uppercase()),
        (
            "the stored name with trailing whitespace",
            format!("{STORED} "),
        ),
        (
            "the stored name with leading whitespace",
            format!(" {STORED}"),
        ),
        ("a prefix of the stored name", "stored".to_string()),
        (
            "the stored name with one character more",
            format!("{STORED}."),
        ),
        ("the empty name", String::new()),
        ("a name never created", "no-such-template".to_string()),
        ("a non-ASCII name never created", "fehlt-bestätigung".to_string()),
    ];
    for (case, name) in &rows {
        delete_ok(&router, case, name).await;
        // Nothing was there to begin with, and nothing is there now.
        assert_absent(&router, case, name).await;
        // The record whose name only RESEMBLES the one deleted is untouched.
        assert_stored(&router, case, STORED).await;
    }

    // The exact name does remove it, which is what makes the rows above a statement
    // about matching and not about a delete that never removes anything.
    delete_ok(&router, "the stored name exactly", STORED).await;
    assert_absent(&router, "the stored name exactly", STORED).await;
}

#[tokio::test]
async fn deleting_a_name_the_account_holds_nothing_under_is_answered_like_any_delete() {
    let (router, _engine) = sending_account().await;

    // The one answer, read off a delete that DOES remove a record: this operation
    // models no error, so every row below has to be answered the same way.
    const PRESENT: &str = "present-template";
    seed(&router, "the first delete", PRESENT).await;
    let (status, first) = delete(&router, PRESENT).await;
    assert_eq!(
        status, 200,
        "case the first delete: expected 200, got {status} with {first}"
    );

    // The rows differ in WHY nothing is there: the same name a second and a third
    // time, a name never created, and a name on an EMPTY store.
    let (empty, _empty_engine) = sending_account().await;
    let rows: Vec<(&str, &MultiServiceRouter, String)> = vec![
        ("the same name a second time", &router, PRESENT.to_string()),
        ("the same name a third time", &router, PRESENT.to_string()),
        (
            "a name never created",
            &router,
            "no-such-template".to_string(),
        ),
        ("a name on an empty store", &empty, PRESENT.to_string()),
        (
            "another name on an empty store",
            &empty,
            "no-such-template".to_string(),
        ),
    ];
    for (case, router, name) in &rows {
        let (status, body) = delete(router, name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 deleting {name:?}, got {status} with {body}"
        );
        assert_eq!(
            error_envelope(&body),
            (None, None),
            "case {case:?}: expected no error envelope, got {body}"
        );
        // Answered exactly like the delete that removed a record, apart from the
        // per-call request id every awsQuery response carries.
        assert_eq!(
            without_request_id(&body),
            without_request_id(&first),
            "case {case:?}: expected the same answer as a delete that removed a record, got \
             {body}"
        );
    }
}

#[tokio::test]
async fn a_delete_frees_the_name_and_the_slot_it_held() {
    let (router, _engine) = sending_account().await;

    const NAME: &str = "reusable-template";
    seed(&router, "the first create", NAME).await;
    delete_ok(&router, "the first delete", NAME).await;
    // The name is free again: the same name is created a second time and reads back,
    // which a store still holding the old record could not serve.
    seed(&router, "the second create", NAME).await;
    assert_stored(&router, "the second create", NAME).await;
    delete_ok(&router, "the second delete", NAME).await;
    assert_absent(&router, "the second delete", NAME).await;
}

#[tokio::test]
async fn a_delete_frees_a_slot_in_the_accounts_bounded_template_store() {
    let (router, _engine) = sending_account().await;

    // The store bounds this account's custom verification email templates
    // (`MAX_CUSTOM_VERIFICATION_EMAIL_TEMPLATES`), so a create past the bound is
    // refused. Filled to the bound here, the create that follows is refused and the
    // one after a delete lands: the slot a delete frees is usable again.
    for index in 0..MAX_CUSTOM_VERIFICATION_EMAIL_TEMPLATES {
        seed(&router, "filling the store", &format!("verify-{index:04}")).await;
    }
    let over = "verify-past-the-bound";
    let (status, body) = call(
        &router,
        "CreateCustomVerificationEmailTemplate",
        &create_params(over),
    )
    .await;
    assert_eq!(
        (status, error_envelope(&body).0),
        (400, Some("LimitExceeded".to_string())),
        "case a full store: expected the account's limit to refuse {over:?}, got {status} with \
         {body}"
    );

    delete_ok(&router, "a full store", "verify-0000").await;
    let (status, body) = call(
        &router,
        "CreateCustomVerificationEmailTemplate",
        &create_params(over),
    )
    .await;
    assert_eq!(
        status, 200,
        "case a slot freed by a delete: expected 200 creating {over:?}, got {status} with {body}"
    );
    assert_stored(&router, "a slot freed by a delete", over).await;
    assert_absent(&router, "a slot freed by a delete", "verify-0000").await;
}
