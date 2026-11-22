// SPDX-License-Identifier: CC0-1.0

//! The expression a `@pattern` clause of this service quotes for the shape a caller-CHOSEN
//! name targets — the patch group a patch-compliance read is asked about, and the default
//! node name a hybrid activation carries, which the model declares the same text for.
//!
//! One rule is pinned here: a clause quotes the expression this service PUBLISHES for the
//! member's shape, which for that text is the model's own
//! (`smithy_query(query="shape",shape="PatchGroup")`:
//! `^([\p{L}\p{Z}\p{N}_.:/=+\-@]*)$`), the whole-value pair included — so the sentence
//! states the match the value was refused for instead of a run the rejected value contains.
//! What is published belongs to the SHAPE, so the rows span every operation that carries it
//! and both members that target the text, with the rejected character varied over the
//! punctuation the class leaves out.
//!
//! Two counterweights keep the rule from being read as "every expression keeps its pair":
//! a value the expression ADMITS is served rather than refused (the characters the class
//! names, letters outside ASCII and a separator among them), and a member whose shape this
//! service publishes without the pair is still quoted without it.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` ([`router`], [`call`]) dispatches `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` through the multi-service router.

use serde_json::{json, Value};

use crate::parameter_store::{call, error_code, error_message, router};

/// The expression this service publishes for the shape a caller-chosen name targets, as the
/// model declares it (`smithy_query(query="shape",shape="PatchGroup")`, which
/// `DefaultInstanceName` carries too), anchors included.
const CHOSEN_NAME_EXPRESSION: &str = r"^([\p{L}\p{Z}\p{N}_.:/=+\-@]*)$";

/// A `BaselineId` the modelled expression and length band admit, so a row that breaks the
/// patch-group expression breaks nothing else.
const A_BASELINE_ID: &str = "pb-0123456789abcdef0123";

/// The greatest length the shape admits (`@length` 1..256), which the longest served row
/// is built to.
const MAX_NAME: usize = 256;

/// The clause one broken `@pattern` is stated as, quoting the value the request carried.
fn pattern_clause(value: &str, path: &str, expression: &str) -> String {
    format!(
        "1 validation error detected: Value '{value}' at '{path}' failed to satisfy \
         constraint: Member must satisfy regular expression pattern: {expression}"
    )
}

/// Refuse `body` under `operation` and return the message, asserting the status and the
/// code every boundary refusal of this service carries.
async fn refusal(case: &str, operation: &str, body: &Value) -> String {
    let router = router();
    let (status, response) = call(&router, operation, body).await;
    assert_eq!(
        status, 400,
        "case {case:?}: a value outside the expression is refused with 400, got {status} \
         {response}"
    );
    assert_eq!(
        error_code(&response),
        "ValidationException",
        "case {case:?}: the refusal is a ValidationException, got {response}"
    );
    error_message(&response)
}

/// THE RULE: every operation carrying the shape quotes the published expression, anchors
/// included, whichever character of the request put the value outside it.
#[tokio::test]
async fn a_chosen_name_outside_its_expression_quotes_the_published_expression() {
    // The punctuation the class leaves out: it admits letters, separators and numbers plus
    // `_ . : / = + - @`, so each of these puts a value outside it.
    let rejected: &[(&str, &str)] = &[
        ("a number sign", "group#bad"),
        ("an exclamation mark", "group!"),
        ("a percent sign", "100%patched"),
        ("a star", "group*"),
        ("a comma", "one,two"),
        ("a question mark", "which?"),
        ("a dollar sign", "$group"),
        ("a backslash", r"domain\group"),
        ("a quotation mark", "\"quoted\""),
        ("a semicolon", "group;drop"),
        ("an ampersand", "a&b"),
        ("a tilde", "~group"),
        ("a bracket", "group[1]"),
        ("one character, the whole value", "!"),
    ];

    // (case, the operation, the member the chosen name travels in, the path a clause names
    // it at, the rest of its request)
    let carriers: &[(&str, &str, &str, &str, Value)] = &[
        (
            "the patch-group compliance read",
            "DescribePatchGroupState",
            "PatchGroup",
            "patchGroup",
            json!({}),
        ),
        (
            "the per-node compliance read of the same group",
            "DescribeInstancePatchStatesForPatchGroup",
            "PatchGroup",
            "patchGroup",
            json!({}),
        ),
        (
            "the baseline a group resolves to",
            "GetPatchBaselineForPatchGroup",
            "PatchGroup",
            "patchGroup",
            json!({ "OperatingSystem": "AMAZON_LINUX_2" }),
        ),
        (
            "the registration of a baseline for a group",
            "RegisterPatchBaselineForPatchGroup",
            "PatchGroup",
            "patchGroup",
            json!({ "BaselineId": A_BASELINE_ID }),
        ),
        (
            "the deregistration of the same pair",
            "DeregisterPatchBaselineForPatchGroup",
            "PatchGroup",
            "patchGroup",
            json!({ "BaselineId": A_BASELINE_ID }),
        ),
        (
            // `CreateActivation$DefaultInstanceName` targets another shape the model
            // declares the same expression for, so the rendering holds there too.
            "the default node name of a hybrid activation",
            "CreateActivation",
            "DefaultInstanceName",
            "defaultInstanceName",
            json!({ "IamRole": "SSMServiceRole" }),
        ),
    ];

    for (carrier, operation, member, path, rest) in carriers {
        for (rejected_case, value) in rejected {
            let case = format!("{carrier}, {rejected_case}");
            let mut body = rest.clone();
            body[*member] = json!(value);
            let stated = refusal(&case, operation, &body).await;
            let expected = pattern_clause(value, path, CHOSEN_NAME_EXPRESSION);
            assert_eq!(
                stated, expected,
                "case {case:?}: expected {expected:?}, got {stated:?}"
            );
        }
    }
}

/// THE FIRST COUNTERWEIGHT: a value the expression admits is SERVED — the characters the
/// class names, including letters outside ASCII, a separator and the punctuation it lists —
/// so stating the pair never widens what is refused.
#[tokio::test]
async fn a_chosen_name_the_expression_admits_is_served() {
    let router = router();

    let admitted: Vec<(&str, String)> = vec![
        ("a plain name", "patchgroup-1".to_string()),
        ("every listed punctuation mark", "a_b.c:d/e=f+g-h@i".to_string()),
        ("a separator between words", "web servers".to_string()),
        ("letters with diacritics", "gruppeÜberwachung".to_string()),
        ("letters outside the Latin script", "パッチグループ".to_string()),
        ("digits alone", "2024".to_string()),
        ("one character, the band's floor", "g".to_string()),
        ("the band's ceiling", "g".repeat(MAX_NAME)),
    ];

    for (case, name) in admitted {
        let (status, response) = call(
            &router,
            "DescribePatchGroupState",
            &json!({ "PatchGroup": name }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a name the expression admits is served, got {status} {response}"
        );
        assert_eq!(
            response["Instances"],
            json!(0),
            "case {case:?}: a group no node has been patched in holds no node, got {response}"
        );

        let (status, response) = call(
            &router,
            "CreateActivation",
            &json!({ "IamRole": "SSMServiceRole", "DefaultInstanceName": name }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the same name is admitted as a default node name, got {status} \
             {response}"
        );
    }
}

/// THE SECOND COUNTERWEIGHT: what a clause quotes is reference data about the SHAPE, so a
/// member whose shape this service publishes without the whole-value pair is still quoted
/// without it.
#[tokio::test]
async fn an_expression_published_without_the_pair_keeps_being_quoted_without_it() {
    // (case, the operation, the member the value travels in, the path a clause names it at,
    // the value, the expression the clause quotes)
    let cases: &[(&str, &str, &str, &str, &str, &str)] = &[
        (
            "an inventory deletion id that is not a UUID",
            "DescribeInventoryDeletions",
            "DeletionId",
            "deletionId",
            "not-a-deletion-id",
            "[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}",
        ),
        (
            "an activation id that is not a UUID",
            "DeleteActivation",
            "ActivationId",
            "activationId",
            "not-an-activation",
            "[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}",
        ),
    ];

    for (case, operation, member, path, value, expression) in cases {
        let body = json!({ (*member): value });
        let stated = refusal(case, operation, &body).await;
        let expected = pattern_clause(value, path, expression);
        assert_eq!(
            stated, expected,
            "case {case:?}: expected {expected:?}, got {stated:?}"
        );
    }
}
