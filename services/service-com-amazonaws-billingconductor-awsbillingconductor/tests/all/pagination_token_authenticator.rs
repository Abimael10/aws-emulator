// SPDX-License-Identifier: CC0-1.0

//! What tells a `NextToken` this service ISSUED from one a caller assembled itself.
//!
//! RULE under test: the position a continuation token names travels INSIDE it, so a read
//! that keeps any value whose shape it can reproduce honours a cut point the service never
//! offered. A `NextToken` therefore carries an authenticator over its own cursor, computed
//! under a secret the issuing store drew from the system's randomness and never reveals: a
//! value carrying any other authenticator over that same payload — one a caller computed
//! from bytes it already knows, one another store minted — names no page at all and is
//! refused with the modelled 400 `ValidationException`, `Reason` `CANNOT_PARSE` and `Fields`
//! naming `NextToken`, rather than being served as a cut point.
//!
//! The rows span what the rule generalises over rather than one reported call: the paginated
//! read (five of them, covering all three shapes of scope this service mints under — a
//! listing of the account's own collection answered for one month, a listing read within one
//! parent resource, and one read within a parent AND answered for one month — and both
//! classes of cursor, a stored ARN and a minted fixed-width ordinal); per read, the cut point
//! the assembled value names (the one the listing itself offered, one sorting above every
//! element of the sequence, and one sorting below all of them); and, per cut point, how the
//! caller authenticated it (an eight-byte digest of the payload, a full digest of it, an
//! all-zero authenticator, an all-ones authenticator, and no authenticator at all). The
//! payload of each assembled value is the issued token's own, read back out of it, so what is
//! refused differs from a token this service minted in the authenticator and in nothing else.
//!
//! [`a_next_token_one_store_minted_is_no_cursor_in_another_store`] pins the other half: two
//! stores holding the same resources mint DIFFERENT tokens for the same cut point, and each
//! refuses the other's, which is what the per-store secret buys.
//!
//! Every assertion goes over the service's real `restJson1` wire through
//! [`smithy_server_runtime::MultiServiceRouter::dispatch`], on the shared harness of
//! `tests/all/payer_account_gate.rs` — this service's control plane answers the payer
//! account alone.

use serde_json::{json, Value};
use service_common::page_tokens::AUTHENTICATOR_BYTES;
use service_com_amazonaws_billingconductor_awsbillingconductor::state::{
    page_scope_in_period, page_scope_within,
};

use crate::pagination_token_opacity::period;
use crate::payer_account_gate::{error_code, Emulator};

/// The byte `crate::state::encode_next_token` writes between the scope a token was minted
/// for and the cursor it carries — the layout a caller reads straight off an issued token,
/// which is why only the authenticator can tell a minted token from an assembled one.
const PAYLOAD_SEPARATOR: char = '\u{1f}';

/// A bound on the pages a walk may take before it is called non-terminating: far above the
/// rows this fixture holds, so reaching it means the paging never ended.
const MAX_PAGES: usize = 16;

/// One paginated read of this service, with the scope its tokens are minted under.
struct Read {
    /// The modelled operation, which is also the case name.
    operation: &'static str,
    /// The route the model binds it to.
    path: &'static str,
    /// The output member its page travels in.
    member: &'static str,
    /// The request, minus `NextToken`: the members that pick the sequence being read.
    body: Value,
    /// The scope this read mints under, built by the crate's own scope builders rather
    /// than re-spelled here — what a caller that assembles a value has to reproduce.
    scope: String,
}

/// The linked account id of the `index`th billing group: twelve digits, as `AccountId`'s
/// modelled `^[0-9]{12}$` requires, and distinct per group so no account is claimed twice.
fn linked_account(index: usize) -> String {
    format!("{:012}", 300_000_000_000_u64 + index as u64)
}

/// Send one setup call that must succeed, and return its body.
async fn setup(em: &Emulator, method: &str, path: &str, body: &Value, label: &str) -> Value {
    let (status, response, _) = em.call(method, path, Some(body)).await;
    assert_eq!(
        status, 200,
        "setup {label}: expected HTTP 200 from {path}, got {status} with {response}"
    );
    response
}

/// Send one setup call that must succeed, and return the `Arn` it answered with.
async fn setup_arn(em: &Emulator, method: &str, path: &str, body: &Value, label: &str) -> String {
    let response = setup(em, method, path, body, label).await;
    response
        .get("Arn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("setup {label}: expected an Arn member, got {response}"))
        .to_string()
}

/// Populate `em` and answer the reads this suite drives against it: one pricing plan, two
/// pricing rules, two billing groups each with their own linked account, three custom line
/// items in the first group, a percentage item two resources are associated to, and an item
/// carrying two generations.
///
/// Written as a function OF the emulator so the cross-store case can stand the same
/// resources up twice: the ids this service mints come from a counter per store, so two
/// stores populated in the same order hold the same ARNs and mint tokens over the same
/// payload.
async fn fixture(em: &Emulator) -> Vec<Read> {
    let plan = setup_arn(
        em,
        "POST",
        "/create-pricing-plan",
        &json!({ "Name": "AuthenticatedTokenPlan" }),
        "the pricing plan",
    )
    .await;

    for index in 0..2 {
        setup_arn(
            em,
            "POST",
            "/create-pricing-rule",
            &json!({
                "Name": format!("AuthenticatedTokenRule{index}"),
                "Scope": "GLOBAL",
                "Type": "DISCOUNT",
                "ModifierPercentage": 5.0,
            }),
            &format!("pricing rule {index}"),
        )
        .await;
    }

    let mut groups: Vec<String> = Vec::new();
    for index in 0..2 {
        let account = linked_account(index);
        groups.push(
            setup_arn(
                em,
                "POST",
                "/create-billing-group",
                &json!({
                    "Name": format!("AuthenticatedTokenGroup{index}"),
                    "AccountGrouping": { "LinkedAccountIds": [account] },
                    "ComputationPreference": { "PricingPlanArn": plan },
                    "PrimaryAccountId": account,
                }),
                &format!("billing group {index}"),
            )
            .await,
        );
    }

    let mut items: Vec<String> = Vec::new();
    for index in 0..3 {
        items.push(
            setup_arn(
                em,
                "POST",
                "/create-custom-line-item",
                &json!({
                    "Name": format!("AuthenticatedTokenItem{index}"),
                    "Description": "an item of the authenticated-token fixture",
                    "BillingGroupArn": groups[0],
                    "ChargeDetails": { "Type": "FEE", "Flat": { "ChargeValue": 1.0 } },
                }),
                &format!("custom line item {index}"),
            )
            .await,
        );
    }

    // Resources are associated to a PERCENTAGE item only, so the association listing needs
    // one of those, with more than one resource on it so a one-row page leaves a remainder.
    let percentage = setup_arn(
        em,
        "POST",
        "/create-custom-line-item",
        &json!({
            "Name": "AuthenticatedTokenPercentage",
            "Description": "a percentage item of the authenticated-token fixture",
            "BillingGroupArn": groups[0],
            "ChargeDetails": { "Type": "FEE", "Percentage": { "PercentageValue": 10.0 } },
        }),
        "the percentage custom line item",
    )
    .await;
    let associated = vec![groups[0].clone(), items[0].clone(), items[1].clone()];
    let response = setup(
        em,
        "PUT",
        "/batch-associate-resources-to-custom-line-item",
        &json!({ "TargetArn": percentage, "ResourceArns": associated }),
        "the group and two items on the percentage item",
    )
    .await;
    assert_eq!(
        response
            .get("SuccessfullyAssociatedResources")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or_default(),
        associated.len(),
        "setup associations: expected every resource associated, got {response}"
    );

    // An item with TWO generations: created in the previous billing period and written
    // again from the current one, which opens a second generation rather than replacing
    // the first.
    let versioned = setup_arn(
        em,
        "POST",
        "/create-custom-line-item",
        &json!({
            "Name": "AuthenticatedTokenVersioned",
            "Description": "a versioned item of the authenticated-token fixture",
            "BillingGroupArn": groups[0],
            "ChargeDetails": { "Type": "FEE", "Flat": { "ChargeValue": 2.0 } },
            "BillingPeriodRange": { "InclusiveStartBillingPeriod": period(-1) },
        }),
        "the versioned custom line item",
    )
    .await;
    setup(
        em,
        "POST",
        "/update-custom-line-item",
        &json!({
            "Arn": versioned,
            "Description": "the second generation of the versioned item",
            "BillingPeriodRange": { "InclusiveStartBillingPeriod": period(0) },
        }),
        "the second generation of the versioned item",
    )
    .await;

    let current = period(0);
    vec![
        Read {
            operation: "ListCustomLineItems",
            path: "/list-custom-line-items",
            member: "CustomLineItems",
            body: json!({}),
            scope: page_scope_in_period("ListCustomLineItems", None, &current),
        },
        Read {
            operation: "ListBillingGroups",
            path: "/list-billing-groups",
            member: "BillingGroups",
            body: json!({}),
            scope: page_scope_in_period("ListBillingGroups", None, &current),
        },
        Read {
            operation: "ListPricingRules",
            path: "/list-pricing-rules",
            member: "PricingRules",
            body: json!({}),
            scope: page_scope_in_period("ListPricingRules", None, &current),
        },
        Read {
            operation: "ListCustomLineItemVersions",
            path: "/list-custom-line-item-versions",
            member: "CustomLineItemVersions",
            body: json!({ "Arn": versioned }),
            scope: page_scope_within("ListCustomLineItemVersions", &versioned),
        },
        Read {
            operation: "ListResourcesAssociatedToCustomLineItem",
            path: "/list-resources-associated-to-custom-line-item",
            member: "AssociatedResources",
            body: json!({ "Arn": percentage }),
            scope: page_scope_in_period(
                "ListResourcesAssociatedToCustomLineItem",
                Some(&percentage),
                &current,
            ),
        },
    ]
}

/// `read`'s request with the extra members `extra` carries written onto it.
fn request(body: &Value, extra: Value) -> Value {
    let mut request = body.clone();
    for (member, value) in extra.as_object().cloned().unwrap_or_default() {
        request[member] = value;
    }
    request
}

/// The elements of a page, or an empty list when the response carries none.
fn page(body: &Value, member: &str) -> Vec<Value> {
    body.get(member)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Every element `read` reports with no page bound of its own — the sequence a refused value
/// must leave untouched.
async fn whole_listing(em: &Emulator, read: &Read) -> Vec<Value> {
    let (status, body, _) = em.call("POST", read.path, Some(&read.body)).await;
    assert_eq!(
        status, 200,
        "case {}: expected the unpaged listing to be served, got {status} with {body}",
        read.operation
    );
    let rows = page(&body, read.member);
    assert!(
        rows.len() > 1,
        "case {}: this fixture must hold more than one row for a one-row page to mint a \
         token, got {rows:?}",
        read.operation
    );
    rows
}

/// The `NextToken` a one-row page of `read` hands back.
async fn minted_token(em: &Emulator, read: &Read) -> String {
    let body = request(&read.body, json!({ "MaxResults": 1 }));
    let (status, response, _) = em.call("POST", read.path, Some(&body)).await;
    assert_eq!(
        status, 200,
        "case {}: expected a first page, got {status} with {response}",
        read.operation
    );
    response
        .get("NextToken")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "case {}: expected a NextToken for a one-row page of a longer sequence, got \
                 {response}",
                read.operation
            )
        })
        .to_string()
}

/// The payload an issued token carries behind its authenticator — the bytes a caller reads
/// off it by decoding the hex, and the ones it would have to reproduce to assemble a value
/// of its own.
fn payload_of(token: &str, case: &str) -> String {
    let bytes = ::hex::decode(token)
        .unwrap_or_else(|error| panic!("case {case:?}: an issued token is hex, got {error}"));
    assert!(
        bytes.len() > AUTHENTICATOR_BYTES,
        "case {case:?}: expected a token longer than its {AUTHENTICATOR_BYTES}-byte \
         authenticator, got {} bytes",
        bytes.len()
    );
    String::from_utf8(bytes[AUTHENTICATOR_BYTES..].to_vec()).unwrap_or_else(|error| {
        panic!("case {case:?}: expected a text payload behind the authenticator, got {error}")
    })
}

/// The cut point an issued token of `read` names, read out of its own payload.
fn issued_cursor(read: &Read, token: &str) -> String {
    let payload = payload_of(token, read.operation);
    let rest = payload.strip_prefix(read.scope.as_str()).unwrap_or_else(|| {
        panic!(
            "case {}: expected the token to be minted under the scope the crate builds \
             ({:?}), got the payload {payload:?}",
            read.operation, read.scope
        )
    });
    rest.strip_prefix(PAYLOAD_SEPARATOR)
        .unwrap_or_else(|| {
            panic!(
                "case {}: expected the cursor to follow the scope, got the payload \
                 {payload:?}",
                read.operation
            )
        })
        .to_string()
}

/// The classes of cut point a caller can name once it has read a payload: the one the
/// listing itself offered, one sorting above every element of the sequence (which would
/// answer an empty page), and one sorting below all of them (which would restart the walk).
///
/// Every key this service pages over is an ARN or a fixed-width decimal ordinal, so `zzzz`
/// sorts above all of them and the empty value below.
fn cut_points(offered: &str) -> Vec<(&'static str, String)> {
    vec![
        ("the cut point the listing offered", offered.to_string()),
        ("a cut point above every element", "zzzz".to_string()),
        ("a cut point below every element", String::new()),
    ]
}

/// The values a caller can assemble for `payload`: the payload behind each authenticator it
/// can compute for itself from bytes it already holds, rendered the way the minter renders
/// one.
fn assembled(payload: &str) -> Vec<(&'static str, String)> {
    use ::sha2::Digest as _;
    let digest = ::sha2::Sha256::digest(payload.as_bytes());
    let with = |authenticator: &[u8]| ::hex::encode([authenticator, payload.as_bytes()].concat());
    vec![
        (
            "an eight-byte digest of the payload",
            with(&digest[..8.min(digest.len())]),
        ),
        ("a full digest of the payload", with(&digest)),
        (
            "an all-zero authenticator",
            with(&[0_u8; AUTHENTICATOR_BYTES]),
        ),
        (
            "an all-ones authenticator",
            with(&[0xff_u8; AUTHENTICATOR_BYTES]),
        ),
        ("no authenticator at all", with(&[])),
    ]
}

/// Assert `read`, asked to resume from the `NextToken` in `body`, is refused as a value this
/// service did not mint: the modelled 400 `ValidationException` with `Reason` `CANNOT_PARSE`,
/// `Fields` naming `NextToken`, a `Message` that states the member — and no page at all.
async fn assert_refused(em: &Emulator, read: &Read, case: &str, body: &Value) {
    let (status, response, error_type) = em.call("POST", read.path, Some(body)).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400 for a value this service did not mint, got {status} \
         with {response}"
    );
    assert_eq!(
        error_code(&response),
        "ValidationException",
        "case {case:?}: expected the modelled ValidationException, got {response}"
    );
    assert_eq!(
        error_type
            .as_deref()
            .map(|value| value.rsplit('#').next().unwrap_or_default().to_string()),
        Some("ValidationException".to_string()),
        "case {case:?}: expected the error shape in x-amzn-errortype, got {error_type:?}"
    );
    assert_eq!(
        response.get("Reason").and_then(Value::as_str),
        Some("CANNOT_PARSE"),
        "case {case:?}: expected Reason CANNOT_PARSE, got {response}"
    );
    let fields: Vec<String> = page(&response, "Fields")
        .iter()
        .filter_map(|field| field.get("Name").and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    assert_eq!(
        fields,
        vec!["NextToken".to_string()],
        "case {case:?}: expected Fields to name NextToken, got {response}"
    );
    let message = response
        .get("Message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        message.contains("NextToken"),
        "case {case:?}: expected the message to state the member, got {message:?}"
    );
    assert!(
        response.get(read.member).is_none(),
        "case {case:?}: the refusal must carry no {} page, got {response}",
        read.member
    );
    assert!(
        response.get("NextToken").is_none(),
        "case {case:?}: the refusal must carry no NextToken, got {response}"
    );
}

/// THE RULE: a `NextToken` whose authenticator the caller computed itself names no page, so
/// every paginated read refuses it rather than resuming from the cut point it carries — and
/// the token the read really did mint still resumes the listing.
#[tokio::test]
async fn a_next_token_a_caller_authenticated_itself_names_no_page() {
    let em = Emulator::payer();
    let reads = fixture(&em).await;

    for read in &reads {
        let before = whole_listing(&em, read).await;
        let issued = minted_token(&em, read).await;
        let offered = issued_cursor(read, &issued);

        for (point, cursor) in cut_points(&offered) {
            let payload = format!("{}{PAYLOAD_SEPARATOR}{cursor}", read.scope);
            for (authenticator, token) in assembled(&payload) {
                let case = format!(
                    "{} given {point} behind {authenticator}",
                    read.operation
                );
                assert_ne!(
                    token, issued,
                    "case {case:?}: an assembled value must differ from the issued token"
                );
                let body = request(&read.body, json!({ "NextToken": token }));
                assert_refused(&em, read, &case, &body).await;
            }
        }

        // The refusals changed nothing, and the token this read DID mint still resumes it:
        // a paged walk reports the whole sequence once, in the unpaged order.
        let after = whole_listing(&em, read).await;
        assert_eq!(
            after, before,
            "case {}: a refused token must change nothing a later caller reads, got {after:?}",
            read.operation
        );

        let mut walked: Vec<Value> = Vec::new();
        let mut next: Option<String> = None;
        let mut pages = 0;
        loop {
            let mut extra = json!({ "MaxResults": 1 });
            if let Some(token) = &next {
                extra["NextToken"] = json!(token);
            }
            let body = request(&read.body, extra);
            let (status, response, _) = em.call("POST", read.path, Some(&body)).await;
            assert_eq!(
                status, 200,
                "case {}: expected page {pages} to be served from the token it minted, got \
                 {status} with {response}",
                read.operation
            );
            walked.extend(page(&response, read.member));
            next = response
                .get("NextToken")
                .and_then(Value::as_str)
                .map(str::to_string);
            pages += 1;
            assert!(
                pages <= MAX_PAGES,
                "case {}: expected the walk to end, got {pages} pages",
                read.operation
            );
            if next.is_none() {
                break;
            }
        }
        assert_eq!(
            walked, before,
            "case {}: expected the paged walk to report every element once in the unpaged \
             order, got {walked:?}",
            read.operation
        );
    }
}

/// THE RULE, the other half: the authenticator is a secret of ONE store, so two stores
/// holding the same resources mint different tokens for the same cut point and neither
/// store's token names a position in the other.
#[tokio::test]
async fn a_next_token_one_store_minted_is_no_cursor_in_another_store() {
    let first = Emulator::payer();
    let second = Emulator::payer();
    let reads = fixture(&first).await;
    let mirrored = fixture(&second).await;

    for (read, mirror) in reads.iter().zip(mirrored.iter()) {
        assert_eq!(
            read.scope, mirror.scope,
            "case {}: two stores populated alike must page the same sequence, got the scopes \
             {:?} and {:?}",
            read.operation, read.scope, mirror.scope
        );

        let ours = minted_token(&first, read).await;
        let theirs = minted_token(&second, mirror).await;
        assert_eq!(
            payload_of(&ours, read.operation),
            payload_of(&theirs, read.operation),
            "case {}: the two stores must name the same cut point, so what differs is the \
             authenticator alone",
            read.operation
        );
        assert_ne!(
            ours, theirs,
            "case {}: two stores must authenticate one cut point differently, got {ours:?} \
             from both",
            read.operation
        );

        let case = format!("{} given the other store's token", read.operation);
        let body = request(&read.body, json!({ "NextToken": theirs }));
        assert_refused(&first, read, &case, &body).await;
        let body = request(&mirror.body, json!({ "NextToken": ours }));
        assert_refused(&second, mirror, &case, &body).await;

        // Each store still serves its own.
        let body = request(&read.body, json!({ "MaxResults": 1, "NextToken": ours }));
        let (status, response, _) = first.call("POST", read.path, Some(&body)).await;
        assert_eq!(
            status, 200,
            "case {}: expected the store's own token to resume its listing, got {status} with \
             {response}",
            read.operation
        );
    }
}
