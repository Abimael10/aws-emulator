// SPDX-License-Identifier: CC0-1.0

//! What `BatchGetInvoiceProfile` answers for the REQUESTER'S OWN account, driven end to
//! end over this service's wire protocol (`POST /` with
//! `X-Amz-Target: Invoicing.BatchGetInvoiceProfile` and an awsJson1_0 body, through the
//! same `MultiServiceRouter` the production router uses).
//!
//! The rule under test: the requester may be answered for exactly those accounts this
//! service's state names as LINKED accounts of its organization, and every other
//! well-formed account id refuses the request WHOLE with the modelled 403
//! `AccessDeniedException`, whose message names the requester and the account it was
//! refused. The requester's own account is no exception — the requester is that
//! organization's MANAGEMENT account, which is not one of its own organization's linked
//! accounts — so it is answered when a stored configuration names it as a linked account
//! and refused when none does, on exactly the terms any other id is.
//!
//! The rows span what the rule generalises over: whether a configuration names the own
//! account and in which of the ways this service's state can name one (nowhere, as an
//! invoice unit's `InvoiceReceiver`, as a rule's `LinkedAccounts` entry, in a rule a later
//! update superseded, only in a unit that was deleted again, only as a
//! `BillSourceAccounts` entry), the shape of the batch it is asked about in (alone,
//! repeated, first, last, among others, at the modelled list maximum) and which other
//! account ids travel beside it.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::multi_service_router::MultiServiceRouter;

/// The modelled maximum of `AccountIdList` (`@length(1..1000)`,
/// `smithy_query(query="operation", operation="BatchGetInvoiceProfile")`), so the rule is
/// asserted at the largest batch a request may carry.
const MAX_ACCOUNT_IDS: usize = 1000;

/// A router carrying only this service, over one shared engine — the same assembly the
/// production router performs, minus the other services.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signing is the router's concern, not this slice's: drive the operations unsigned.
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_invoicing_invoicing::EmulatedService::new(
        engine,
    ));
    router
}

/// One awsJson1_0 request, returning the HTTP status and the parsed body.
async fn call(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("x-amz-target", format!("Invoicing.{operation}"))
        .header("content-type", "application/x-amz-json-1.0")
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a valid request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The same call, asserting it succeeded.
async fn ok(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let (status, parsed) = call(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {operation} {body}: expected HTTP 200, got {status} {parsed}"
    );
    parsed
}

/// The wire error code of a failure body (`__type` on the awsJson protocols).
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The explanatory text of a failure body, under either case of the message key.
fn error_text(body: &Value) -> String {
    body.get("Message")
        .or_else(|| body.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The requester's own account: the identity this emulator presents to a caller, read from
/// the one source of truth for it rather than spelled as a literal.
fn own_account() -> String {
    engine::emulator_account_id().to_string()
}

/// An account id of the modelled `AccountIdString` form (`@pattern ^\d{12}$`), derived from
/// a row index. The offset keeps every generated id distinct from the requester's own
/// account, which is all zeroes.
fn account(index: usize) -> String {
    format!("{:012}", 300 + index)
}

/// The message this service states for an account the requester may not be answered for:
/// built from the request — the requester's own account and the account it was refused.
fn not_authorized_message(refused: &str) -> String {
    format!(
        "Account {} is not authorized to access resources for account {refused}",
        own_account()
    )
}

/// `BatchGetInvoiceProfile` for `accounts`.
async fn batch_get(router: &MultiServiceRouter, accounts: &[String]) -> (u16, Value) {
    call(
        router,
        "BatchGetInvoiceProfile",
        &json!({"AccountIds": accounts}),
    )
    .await
}

/// The `AccountId` of each reported profile, in the order they were reported.
fn reported_accounts(body: &Value) -> Vec<String> {
    body.get("Profiles")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("expected a Profiles list in {body}"))
        .iter()
        .map(|profile| {
            profile
                .get("AccountId")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("expected an AccountId in {profile}"))
                .to_string()
        })
        .collect()
}

/// Create one invoice unit naming `receiver`, `linked` and `bill_source`, returning its ARN.
async fn create_unit(
    router: &MultiServiceRouter,
    name: &str,
    receiver: &str,
    linked: &[String],
    bill_source: &[String],
) -> String {
    let mut rule = serde_json::Map::new();
    if !linked.is_empty() {
        rule.insert("LinkedAccounts".to_string(), json!(linked));
    }
    if !bill_source.is_empty() {
        rule.insert("BillSourceAccounts".to_string(), json!(bill_source));
    }
    let created = ok(
        router,
        "CreateInvoiceUnit",
        &json!({
            "Name": name,
            "InvoiceReceiver": receiver,
            "TaxInheritanceDisabled": false,
            "Rule": Value::Object(rule),
            "ClientToken": format!("own-account-{name}"),
        }),
    )
    .await;
    created
        .get("InvoiceUnitArn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("expected an InvoiceUnitArn in {created}"))
        .to_string()
}

/// How this service's state names (or does not name) the requester's own account when it
/// asks about itself — the rows of the tables below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arrangement {
    /// A fresh emulator: no invoice unit at all, which is how a client first meets this
    /// operation.
    NothingAtAll,
    /// Invoice units naming other accounts only.
    UnitsNamingOthers,
    /// An invoice unit naming other accounts, which was deleted again — a delete takes the
    /// unit's accounts with it, so state holds a retired unit and no live membership.
    OnlyADeletedUnit,
    /// The own account named only as a `BillSourceAccounts` entry, which the model
    /// documents as possibly "a payer account from another organization".
    OwnAsBillSourceOnly,
    /// The own account as an invoice unit's `InvoiceReceiver` — "the Amazon Web Services
    /// account ID chosen to be the receiver of an invoice unit".
    OwnAsInvoiceReceiver,
    /// The own account as the only entry of a rule's `LinkedAccounts`.
    OwnAsSoleLinkedAccount,
    /// The own account as one entry among several of a rule's `LinkedAccounts`.
    OwnAsOneLinkedAccountOfSeveral,
    /// The own account in a rule a later `UpdateInvoiceUnit` superseded: it left that
    /// unit's rule, not the organization, and the configuration naming it is retained.
    OwnInASupersededRule,
}

impl Arrangement {
    /// Whether this arrangement leaves the requester's own account a LINKED account of the
    /// organization, which is what decides the answer.
    fn own_account_is_linked(self) -> bool {
        matches!(
            self,
            Arrangement::OwnAsInvoiceReceiver
                | Arrangement::OwnAsSoleLinkedAccount
                | Arrangement::OwnAsOneLinkedAccountOfSeveral
                | Arrangement::OwnInASupersededRule
        )
    }
}

/// Every row of the table, so the arrangement class is stated in one place.
const ARRANGEMENTS: [Arrangement; 8] = [
    Arrangement::NothingAtAll,
    Arrangement::UnitsNamingOthers,
    Arrangement::OnlyADeletedUnit,
    Arrangement::OwnAsBillSourceOnly,
    Arrangement::OwnAsInvoiceReceiver,
    Arrangement::OwnAsSoleLinkedAccount,
    Arrangement::OwnAsOneLinkedAccountOfSeveral,
    Arrangement::OwnInASupersededRule,
];

/// Apply `arrangement`, returning the OTHER account ids it leaves readable.
async fn arrange(router: &MultiServiceRouter, arrangement: Arrangement) -> Vec<String> {
    let own = own_account();
    match arrangement {
        Arrangement::NothingAtAll => Vec::new(),
        Arrangement::UnitsNamingOthers => {
            create_unit(router, "first-unit", &account(1), &[account(2)], &[]).await;
            create_unit(
                router,
                "second-unit",
                &account(3),
                &[account(4), account(5)],
                &[],
            )
            .await;
            vec![
                account(1),
                account(2),
                account(3),
                account(4),
                account(5),
            ]
        }
        Arrangement::OnlyADeletedUnit => {
            let arn = create_unit(router, "retired-unit", &account(1), &[account(2)], &[]).await;
            ok(
                router,
                "DeleteInvoiceUnit",
                &json!({"InvoiceUnitArn": arn, "ClientToken": "retired-unit-delete"}),
            )
            .await;
            Vec::new()
        }
        Arrangement::OwnAsBillSourceOnly => {
            create_unit(
                router,
                "bill-source-unit",
                &account(1),
                &[account(2)],
                std::slice::from_ref(&own),
            )
            .await;
            vec![account(1), account(2)]
        }
        Arrangement::OwnAsInvoiceReceiver => {
            create_unit(router, "own-receiver", &own, &[account(2)], &[]).await;
            vec![account(2)]
        }
        Arrangement::OwnAsSoleLinkedAccount => {
            create_unit(
                router,
                "own-sole-linked",
                &account(1),
                std::slice::from_ref(&own),
                &[],
            )
            .await;
            vec![account(1)]
        }
        Arrangement::OwnAsOneLinkedAccountOfSeveral => {
            create_unit(
                router,
                "own-one-of-several",
                &account(1),
                &[account(2), own.clone(), account(3)],
                &[],
            )
            .await;
            vec![account(1), account(2), account(3)]
        }
        Arrangement::OwnInASupersededRule => {
            let arn = create_unit(
                router,
                "own-superseded",
                &account(1),
                &[account(2), own.clone()],
                &[],
            )
            .await;
            ok(
                router,
                "UpdateInvoiceUnit",
                &json!({
                    "InvoiceUnitArn": arn,
                    "Rule": {"LinkedAccounts": [account(2)]},
                    "ClientToken": "own-superseded-update",
                }),
            )
            .await;
            vec![account(1), account(2)]
        }
    }
}

/// Assert one batch was refused WHOLE with the modelled 403 naming `refused`.
async fn assert_refused(
    router: &MultiServiceRouter,
    label: &str,
    requested: &[String],
    refused: &str,
) {
    let (status, body) = batch_get(router, requested).await;
    assert_eq!(
        status, 403,
        "case {label}: expected HTTP 403 for {} account ids, got {status} {body}",
        requested.len()
    );
    assert_eq!(
        error_code(&body),
        "AccessDeniedException",
        "case {label}: expected the modelled AccessDeniedException, got {body}"
    );
    let expected = not_authorized_message(refused);
    assert_eq!(
        error_text(&body),
        expected,
        "case {label}: expected the message {expected:?}, got {body}"
    );
    assert!(
        body.get("Profiles").is_none(),
        "case {label}: a refused batch reports no profiles, got {body}"
    );
}

/// Assert one batch was answered with a profile per account asked about, in request order.
async fn assert_reported(
    router: &MultiServiceRouter,
    label: &str,
    requested: &[String],
    expected: &[String],
) {
    let (status, body) = batch_get(router, requested).await;
    assert_eq!(
        status, 200,
        "case {label}: expected HTTP 200 for {} account ids, got {status} {body}",
        requested.len()
    );
    let actual = reported_accounts(&body);
    assert_eq!(
        actual, expected,
        "case {label}: expected profiles for {expected:?}, got {actual:?}"
    );
    // The profile this service holds reports the account it is about and invents nothing
    // else: every other member of `InvoiceProfile` is written through a plane this service
    // does not own.
    let profiles = body
        .get("Profiles")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {label}: expected a Profiles list in {body}"));
    for profile in profiles {
        assert_eq!(
            profile.as_object().map(|members| members.len()),
            Some(1),
            "case {label}: a profile reports the account it is about alone, got {profile}"
        );
    }
}

/// RULE: the requester's own account resolves to its invoice profile exactly when this
/// service's state names it as a LINKED account of the organization, and is refused with
/// the modelled 403 — like any other account id outside that membership — when nothing
/// does, whatever shape the batch takes.
#[tokio::test]
async fn the_own_account_answers_on_its_membership_whatever_shape_the_batch_takes() {
    let own = own_account();

    for arrangement in ARRANGEMENTS {
        let router = emulator();
        let readable = arrange(&router, arrangement).await;
        let own_is_readable = arrangement.own_account_is_linked();

        // The batch shapes the own account can be asked about in, each with the profiles a
        // readable own account must be answered with: one per account asked about, in the
        // order the request first names it.
        let mut shapes: Vec<(String, Vec<String>, Vec<String>)> = vec![
            ("alone".to_string(), vec![own.clone()], vec![own.clone()]),
            (
                // `AccountIdList` carries no `@uniqueItems`, so a repeat is admitted.
                "repeated".to_string(),
                vec![own.clone(), own.clone(), own.clone()],
                vec![own.clone()],
            ),
            (
                "at the modelled list maximum".to_string(),
                vec![own.clone(); MAX_ACCOUNT_IDS],
                vec![own.clone()],
            ),
        ];
        if !readable.is_empty() {
            let mut first = vec![own.clone()];
            first.extend(readable.iter().cloned());
            shapes.push(("first of a batch".to_string(), first.clone(), first));
            let mut last = readable.clone();
            last.push(own.clone());
            shapes.push(("last of a batch".to_string(), last.clone(), last));
            let mut between = vec![readable[0].clone(), own.clone()];
            between.extend(readable.iter().skip(1).cloned());
            shapes.push((
                "between linked accounts".to_string(),
                between.clone(),
                between,
            ));
        }

        for (shape, requested, expected) in shapes {
            let label = format!("{arrangement:?}, {shape}");
            if own_is_readable {
                assert_reported(&router, &label, &requested, &expected).await;
            } else {
                // Refused WHOLE on the first id in request order the requester may not
                // read, which is the own account wherever it sits in these shapes.
                assert_refused(&router, &label, &requested, &own).await;
            }
        }

        // Every account the arrangement DID place in the organization is readable, whether
        // or not the own account is: the answer is per account, not per caller.
        if !readable.is_empty() {
            let label = format!("{arrangement:?}, the accounts state names");
            assert_reported(&router, &label, &readable, &readable).await;
        }

        // An id the organization does not contain is refused WHOLE, wherever it sits
        // beside the own account, and the refusal names the requester and the first such
        // id in request order.
        let outsiders = [account(90), account(91), account(92)];
        for (shape, requested, refused) in [
            (
                "an outsider alone",
                vec![outsiders[0].clone()],
                outsiders[0].clone(),
            ),
            (
                "an outsider after the own account",
                vec![own.clone(), outsiders[1].clone()],
                if own_is_readable {
                    outsiders[1].clone()
                } else {
                    own.clone()
                },
            ),
            (
                "an outsider before the own account",
                vec![outsiders[2].clone(), own.clone()],
                outsiders[2].clone(),
            ),
            (
                "two outsiders around the own account",
                vec![outsiders[1].clone(), own.clone(), outsiders[2].clone()],
                outsiders[1].clone(),
            ),
        ] {
            let label = format!("{arrangement:?}, {shape}");
            assert_refused(&router, &label, &requested, &refused).await;
        }
    }
}

/// RULE, the state half: what admits the requester's own account is a stored configuration
/// naming it as a linked account and nothing else — so a create that names it turns the
/// very same refused request into an answer, a read (reported or refused) changes nothing a
/// later caller sees, and the refusal never teaches the service about the account.
#[tokio::test]
async fn only_a_stored_configuration_admits_the_own_account() {
    let router = emulator();
    let own = own_account();

    // Against a bare organization the own account is refused, and repeating the request
    // answers identically: a refused read added nothing.
    let first = batch_get(&router, std::slice::from_ref(&own)).await;
    let second = batch_get(&router, std::slice::from_ref(&own)).await;
    assert_eq!(
        first.0, 403,
        "case bare organization: expected HTTP 403, got {first:?}"
    );
    assert_eq!(
        first, second,
        "case repeated refusal: a refused read must not have admitted the account, got \
         {first:?} then {second:?}"
    );

    // A create naming it as a linked account is what admits it.
    create_unit(
        &router,
        "admits-own-account",
        &account(1),
        std::slice::from_ref(&own),
        &[],
    )
    .await;
    assert_reported(
        &router,
        "after a create names the own account",
        std::slice::from_ref(&own),
        std::slice::from_ref(&own),
    )
    .await;

    // Reading profiles changes nothing a later caller sees: the invoice units are what
    // they were, and the same requests answer the same way.
    let units = ok(&router, "ListInvoiceUnits", &json!({})).await;
    let reported = batch_get(&router, std::slice::from_ref(&own)).await;
    let refused = batch_get(&router, &[own.clone(), account(90)]).await;
    assert_eq!(
        reported,
        batch_get(&router, std::slice::from_ref(&own)).await,
        "case repeated read: the same request must answer identically"
    );
    assert_eq!(
        refused,
        batch_get(&router, &[own.clone(), account(90)]).await,
        "case repeated mixed batch: the same request must answer identically"
    );
    assert_eq!(
        units,
        ok(&router, "ListInvoiceUnits", &json!({})).await,
        "case state after reads: reading a profile must leave the invoice units alone"
    );
}
