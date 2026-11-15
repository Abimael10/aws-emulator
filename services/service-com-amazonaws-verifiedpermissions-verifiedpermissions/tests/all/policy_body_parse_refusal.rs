// SPDX-License-Identifier: CC0-1.0

//! What a static policy whose Cedar text does not PARSE is refused with.
//!
//! RULE: the `fieldList` entry for a static policy body the Cedar parser rejects
//! carries the fixed classification of the failing element — `Failed to validate
//! policy body` — with the element itself identified by the entry's sibling
//! `path`, and never the parser's own diagnostic: that diagnostic names whichever
//! token happened to break the parse, so it differs for every malformed statement
//! of the same class and moves with the parser rather than with the API.
//!
//! The rows span the KIND of syntax error (a statement that is not Cedar at all, a
//! missing terminator, an unknown effect keyword, an unbalanced scope, a truncated
//! statement, a template slot where a static policy admits none, a condition that
//! is not an expression), the operation that submitted it (`CreatePolicy`,
//! `UpdatePolicy`) and the store's validation mode (`OFF`, and `STRICT` with a
//! schema, which is the other path that hands the statement to the parser).
//!
//! Two neighbouring classes keep their own wording, and the rows that pin them
//! say so: a statement past the size this emulator admits is refused by a bound
//! check rather than by a parse, and a statement that PARSES and is then refused
//! by the store's schema is reported one entry per reason the validator raised.
//!
//! Everything goes over the wire through the same `MultiServiceRouter` the
//! emulator binary serves, on the shared harness in [`crate::harness`].

use serde_json::{json, Value};
use service_com_amazonaws_verifiedpermissions_verifiedpermissions::ops::INVALID_INPUT_MESSAGE;
use service_com_amazonaws_verifiedpermissions_verifiedpermissions::state::cedar::MAX_STATEMENT_BYTES;

use crate::harness::{call, error_code, fault_message, field_list, ok, router, store_with_mode};
use crate::schema::photo_app_schema;

/// The classification a policy body that does not parse is refused with, as the
/// wire carries it. Written out here rather than imported on purpose: this suite
/// pins the sentence the API answers, so a change to the implementation constant
/// has to fail this test instead of moving with it.
const POLICY_BODY_REFUSAL: &str = "Failed to validate policy body";

/// The JSON pointer of the member a static policy's text lives at, which the
/// refusal's `path` names.
const STATIC_STATEMENT_PATH: &str = "/definition/static/statement";

/// A statement the schema below accepts, for the rows that need a policy to
/// already exist or a definition that is not itself at fault.
const CONFORMANT: &str = "permit(principal == PhotoApp::User::\"alice\", \
                          action == PhotoApp::Action::\"viewPhoto\", \
                          resource == PhotoApp::Photo::\"vacation.jpg\");";

/// Every statement a row submits as the policy body, each broken in a different
/// way, so what they share is only that the Cedar parser will not read them.
const UNPARSEABLE: &[(&str, &str)] = &[
    ("not Cedar at all", "this is not cedar"),
    (
        "no terminating semicolon",
        "permit(principal, action, resource)",
    ),
    (
        "an effect keyword Cedar does not have",
        "allow(principal, action, resource);",
    ),
    (
        "an unbalanced scope",
        "permit(principal == PhotoApp::User::\"alice\", action, resource;",
    ),
    ("a truncated statement", "permit(principal"),
    (
        // `?principal` is a template SLOT: a static policy admits none, so the
        // static parser refuses the text.
        "a template slot in a static policy",
        "permit(principal == ?principal, action, resource);",
    ),
    (
        "a condition that is not an expression",
        "permit(principal, action, resource) when { };",
    ),
    (
        "an unterminated string literal",
        "permit(principal == PhotoApp::User::\"alice, action, resource);",
    ),
];

/// A statement whose text is longer than [`MAX_STATEMENT_BYTES`], reached by
/// padding a string literal inside a condition, so the only thing wrong with it
/// is its size.
fn over_long_statement() -> String {
    format!(
        "permit(principal, action, resource) when {{ context.tag == \"{}\" }};",
        "x".repeat(MAX_STATEMENT_BYTES + 1)
    )
}

/// The `fieldList` of a 400 `ValidationException`, after checking the status and
/// the code — every row's refusal is the modelled one, so the shape check lives
/// in one place.
async fn refusal(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    operation: &str,
    request: &Value,
) -> Vec<(String, String)> {
    let (status, body) = call(router, operation, request).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} {body}"
    );
    let code = error_code(&body);
    assert_eq!(
        code, "ValidationException",
        "case {case:?}: expected a ValidationException, got {code:?} {body}"
    );
    let message = fault_message(&body);
    assert_eq!(
        message, INVALID_INPUT_MESSAGE,
        "case {case:?}: expected the fixed summary {INVALID_INPUT_MESSAGE:?}, got {message:?}"
    );
    let fields = field_list(&body);
    assert_eq!(
        fields.len(),
        1,
        "case {case:?}: expected one fieldList entry, got {fields:?}"
    );
    fields
}

/// Every unparseable static policy body, submitted through both writers and under
/// both validation modes, is refused with the ONE classification at the member
/// that carries the text.
#[tokio::test]
async fn an_unparseable_policy_body_is_refused_with_one_classification() {
    let router = router();
    let off_store = store_with_mode(&router, "OFF").await;
    let strict_store = store_with_mode(&router, "STRICT").await;
    ok(
        &router,
        "PutSchema",
        &json!({
            "policyStoreId": strict_store,
            "definition": { "cedarJson": photo_app_schema() },
        }),
    )
    .await;

    // A policy in each store, so the `UpdatePolicy` rows replace the body of a
    // policy that really exists.
    let mut live_policy = std::collections::BTreeMap::new();
    for store in [&off_store, &strict_store] {
        let created = ok(
            &router,
            "CreatePolicy",
            &json!({
                "policyStoreId": store,
                "definition": { "static": { "statement": CONFORMANT } },
            }),
        )
        .await;
        live_policy.insert(
            store.clone(),
            created["policyId"]
                .as_str()
                .expect("policyId is required on the CreatePolicy response")
                .to_string(),
        );
    }

    for (kind, statement) in UNPARSEABLE {
        for (mode, store) in [("OFF", &off_store), ("STRICT", &strict_store)] {
            let create_case = format!("CreatePolicy, {kind}, a {mode} store");
            let fields = refusal(
                &router,
                &create_case,
                "CreatePolicy",
                &json!({
                    "policyStoreId": store,
                    "definition": { "static": { "statement": statement } },
                }),
            )
            .await;
            assert_eq!(
                fields[0],
                (
                    STATIC_STATEMENT_PATH.to_string(),
                    POLICY_BODY_REFUSAL.to_string()
                ),
                "case {create_case:?}: expected {STATIC_STATEMENT_PATH:?} refused with \
                 {POLICY_BODY_REFUSAL:?}, got {fields:?}"
            );

            let update_case = format!("UpdatePolicy, {kind}, a {mode} store");
            let fields = refusal(
                &router,
                &update_case,
                "UpdatePolicy",
                &json!({
                    "policyStoreId": store,
                    "policyId": live_policy[store.as_str()],
                    "definition": { "static": { "statement": statement } },
                }),
            )
            .await;
            assert_eq!(
                fields[0],
                (
                    STATIC_STATEMENT_PATH.to_string(),
                    POLICY_BODY_REFUSAL.to_string()
                ),
                "case {update_case:?}: expected {STATIC_STATEMENT_PATH:?} refused with \
                 {POLICY_BODY_REFUSAL:?}, got {fields:?}"
            );
        }
    }

    // The policy that PARSES is still stored, so the suite is not asserting about
    // a writer that refuses everything.
    let accepted = ok(
        &router,
        "CreatePolicy",
        &json!({
            "policyStoreId": strict_store,
            "definition": { "static": { "statement": CONFORMANT } },
            "name": "accepted-policy",
        }),
    )
    .await;
    assert_eq!(
        accepted["policyType"].as_str().unwrap_or_default(),
        "STATIC",
        "case \"a conformant statement\": the policy must be stored, got {accepted}"
    );
}

/// The neighbouring classes keep their own wording: a statement refused by this
/// emulator's SIZE bound states that bound, and a statement the store's SCHEMA
/// refuses after it parsed is reported at the validated member with the reasons
/// the validator raised — so the classification above identifies the PARSE class
/// and not every refusal of a policy body.
#[tokio::test]
async fn the_neighbouring_refusal_classes_keep_their_own_wording() {
    let router = router();
    let off_store = store_with_mode(&router, "OFF").await;
    let strict_store = store_with_mode(&router, "STRICT").await;
    ok(
        &router,
        "PutSchema",
        &json!({
            "policyStoreId": strict_store,
            "definition": { "cedarJson": photo_app_schema() },
        }),
    )
    .await;

    let case = "a statement longer than one policy may carry";
    let fields = refusal(
        &router,
        case,
        "CreatePolicy",
        &json!({
            "policyStoreId": off_store,
            "definition": { "static": { "statement": over_long_statement() } },
        }),
    )
    .await;
    assert_eq!(
        fields[0].0, STATIC_STATEMENT_PATH,
        "case {case:?}: the size refusal still names the statement, got {fields:?}"
    );
    assert_ne!(
        fields[0].1, POLICY_BODY_REFUSAL,
        "case {case:?}: a size refusal is not the parse classification, got {fields:?}"
    );
    assert!(
        fields[0].1.contains(&MAX_STATEMENT_BYTES.to_string()),
        "case {case:?}: the size refusal states the bound it exceeded, got {fields:?}"
    );

    // A statement that parses perfectly and names an action the schema does not
    // declare: refused by the schema, at the member whose CONTENT was validated.
    let case = "a statement the store's schema refuses";
    let (status, body) = call(
        &router,
        "CreatePolicy",
        &json!({
            "policyStoreId": strict_store,
            "definition": {
                "static": {
                    "statement": "permit(principal == PhotoApp::User::\"alice\", \
                                  action == PhotoApp::Action::\"undeclared\", \
                                  resource == PhotoApp::Photo::\"vacation.jpg\");",
                },
            },
        }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} {body}"
    );
    let fields = field_list(&body);
    assert!(
        !fields.is_empty()
            && fields
                .iter()
                .all(|(path, message)| path == "/definition" && message != POLICY_BODY_REFUSAL),
        "case {case:?}: a schema refusal names /definition with the validator's reasons, got \
         {fields:?}"
    );
}
