// SPDX-License-Identifier: CC0-1.0

//! What an Automated Reasoning policy's `definitionHash` is the digest OF, end-to-end over
//! the same `MultiServiceRouter` the emulator boots.
//!
//! The rules under test, each asserted over a SET of definitions rather than over one
//! recorded request:
//!
//! 1. **The hash is the hex SHA-512 of the canonical definition document**: the
//!    definition's members, and those of every element of its three lists, in the order the
//!    model declares them (`version`, `types`, `rules`, `variables`; a type as `name`,
//!    `description`, `values`, a type value as `value`, `description`; a rule as `id`,
//!    `expression`, `alternateExpression`; a variable as `name`, `type`, `description`), an
//!    absent list as the empty list, an absent optional member as null, rendered as compact
//!    JSON — and the document states the format token the DIGEST's canonical form uses. The
//!    document is spelled out here as BYTES rather than built by the same code the emulator
//!    builds it with, so the form the digest is taken over is pinned independently of how
//!    this crate happens to render a JSON object.
//! 2. **The token the digest's document states is not the token a definition is published
//!    under**: the definition an export reports states the member's own modelled `@default`
//!    (`AutomatedReasoningPolicyDefinition$version`), and the digest of the document stating
//!    THAT token is never the policy's `definitionHash` — the two are independent values, so
//!    the hash is the one the response carries rather than one re-derived from the published
//!    document.
//! 3. **The hash is a function of the definition's content alone**: two definitions holding
//!    the same types, rules and variables report one hash however their writers spelled the
//!    format token, and every operation that publishes the hash — the create, the read, the
//!    update's echo and the version cut from the draft — publishes that one value.
//! 4. **The hash a read publishes is the concurrency token a version is cut on**: the value
//!    the read reports cuts the next version, while the digest taken over the published
//!    document is refused `ConflictException` (400) in the one fixed sentence.
//!
//! Every expectation is derived from the model, cited where it is used
//! (`smithy_query(query="shape",shape="AutomatedReasoningPolicyDefinition")`,
//! `smithy_query(query="operation",operation="GetAutomatedReasoningPolicy,UpdateAutomatedReasoningPolicy,CreateAutomatedReasoningPolicyVersion,ExportAutomatedReasoningPolicyVersion")`).
//! The requests go through `crate::support` — the ONE harness every suite in this crate
//! drives the emulator with.

use serde_json::{json, Value};

use crate::support::{emulator, label, Emulator, Reply};

/// The format token the canonical document a `definitionHash` is taken over states.
const DIGEST_FORMAT_TOKEN: &str = "1.0";

/// The format token a definition is PUBLISHED under — the member's own modelled `@default`
/// (`smithy_query(query="shape",shape="AutomatedReasoningPolicyDefinition")`: `version`
/// targets `AutomatedReasoningPolicyFormatVersion`, `@default("1")`).
const PUBLISHED_FORMAT_TOKEN: &str = "1";

/// How many digits `AutomatedReasoningPolicyHash` carries (`@length(128, 128)`,
/// `^[0-9a-z]{128}$`).
const HASH_DIGITS: usize = 128;

/// The `ConflictException` sentence a `lastUpdatedDefinitionHash` that is not the draft's
/// current hash is refused with.
const STALE_DEFINITION_HASH: &str =
    "Provided definition hash does not match the DRAFT policy definition hash.";

async fn create(emu: &Emulator, body: Value) -> Reply {
    emu.call("POST", "/automated-reasoning-policies", Some(body))
        .await
}

async fn get(emu: &Emulator, policy_arn: &str) -> Reply {
    emu.call(
        "GET",
        &format!("/automated-reasoning-policies/{}", label(policy_arn)),
        None,
    )
    .await
}

async fn export(emu: &Emulator, policy_arn: &str) -> Reply {
    emu.call(
        "GET",
        &format!(
            "/automated-reasoning-policies/{}/export",
            label(policy_arn)
        ),
        None,
    )
    .await
}

async fn update(emu: &Emulator, policy_arn: &str, definition: Value) -> Reply {
    emu.call(
        "PATCH",
        &format!("/automated-reasoning-policies/{}", label(policy_arn)),
        Some(json!({ "policyDefinition": definition })),
    )
    .await
}

async fn cut_version(emu: &Emulator, policy_arn: &str, hash: &str) -> Reply {
    emu.call(
        "POST",
        &format!(
            "/automated-reasoning-policies/{}/versions",
            label(policy_arn)
        ),
        Some(json!({ "lastUpdatedDefinitionHash": hash })),
    )
    .await
}

// ---------------------------------------------------------------------------------------
// The canonical document, spelled out as the BYTES the digest is taken over.
// ---------------------------------------------------------------------------------------

/// One JSON value as a scalar of the canonical document: an absent member as `null`, a
/// present one as the JSON it is written as (which escapes the text the same way the wire
/// does).
fn scalar(value: Value) -> String {
    value.to_string()
}

/// One member of `element`, or `null` when the element omits it.
fn member(element: &Value, name: &str) -> Value {
    element.get(name).cloned().unwrap_or(Value::Null)
}

/// A compact JSON object of `members`, in exactly the order given.
fn object(members: &[(&str, String)]) -> String {
    let body: Vec<String> = members
        .iter()
        .map(|(name, value)| format!("\"{name}\":{value}"))
        .collect();
    format!("{{{}}}", body.join(","))
}

/// A compact JSON array of `elements`, in exactly the order given.
fn array(elements: &[String]) -> String {
    format!("[{}]", elements.join(","))
}

/// The canonical document for `definition`, stating `format_token`, as the bytes a digest is
/// taken over.
fn canonical_text(definition: Option<&Value>, format_token: &str) -> String {
    let list = |name: &str| -> Vec<Value> {
        definition
            .and_then(|definition| definition.get(name))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let types: Vec<String> = list("types")
        .iter()
        .map(|declared| {
            let values: Vec<String> = member(declared, "values")
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|value| {
                    object(&[
                        ("value", scalar(member(value, "value"))),
                        ("description", scalar(member(value, "description"))),
                    ])
                })
                .collect();
            object(&[
                ("name", scalar(member(declared, "name"))),
                ("description", scalar(member(declared, "description"))),
                ("values", array(&values)),
            ])
        })
        .collect();
    let rules: Vec<String> = list("rules")
        .iter()
        .map(|rule| {
            object(&[
                ("id", scalar(member(rule, "id"))),
                ("expression", scalar(member(rule, "expression"))),
                (
                    "alternateExpression",
                    scalar(member(rule, "alternateExpression")),
                ),
            ])
        })
        .collect();
    let variables: Vec<String> = list("variables")
        .iter()
        .map(|variable| {
            object(&[
                ("name", scalar(member(variable, "name"))),
                ("type", scalar(member(variable, "type"))),
                ("description", scalar(member(variable, "description"))),
            ])
        })
        .collect();
    object(&[
        ("version", scalar(json!(format_token))),
        ("types", array(&types)),
        ("rules", array(&rules)),
        ("variables", array(&variables)),
    ])
}

/// The hash a canonical document hashes to: the SHA-512 of its bytes, hex-encoded, whose 128
/// lowercase hex digits are exactly what `AutomatedReasoningPolicyHash` admits.
fn digest_of(canonical: &str) -> String {
    use sha2::{Digest, Sha512};

    hex::encode(Sha512::digest(canonical.as_bytes()))
}

/// The hash the canonical document for `definition` must report.
fn expected_hash(definition: Option<&Value>) -> String {
    digest_of(&canonical_text(definition, DIGEST_FORMAT_TOKEN))
}

/// The hash the document a definition is PUBLISHED as would report — the value a client that
/// re-derived the hash from the served document would hold, which is not the policy's.
fn published_document_hash(definition: Option<&Value>) -> String {
    digest_of(&canonical_text(definition, PUBLISHED_FORMAT_TOKEN))
}

/// Assert `hash` is one `AutomatedReasoningPolicyHash` admits.
fn assert_hash_shape(case: &str, hash: &str) {
    assert!(
        hash.len() == HASH_DIGITS
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit()),
        "case {case:?}: definitionHash must be {HASH_DIGITS} digits of [0-9a-z], got {hash:?} \
         ({} chars)",
        hash.len()
    );
}

// ---------------------------------------------------------------------------------------
// The definition shapes a row is built from.
// ---------------------------------------------------------------------------------------

/// One custom type (`values` is `@length(1, 150)`; the second value carries no description,
/// the one OPTIONAL member a type value has).
fn policy_type(index: usize) -> Value {
    json!({
        "name": format!("Type{index}"),
        "description": format!("the type {index} a response is checked against"),
        "values": [
            { "value": format!("Value{index}A"), "description": "the first value" },
            { "value": format!("Value{index}B") },
        ],
    })
}

/// One variable (every member of a variable is `@required`).
fn policy_variable(index: usize) -> Value {
    json!({
        "name": format!("variable{index}"),
        "type": format!("Type{index}"),
        "description": format!("what variable {index} stands for"),
    })
}

/// One rule (`id` is `^[A-Z][0-9A-Z]{11}$`), carrying `alternateExpression` — its one
/// OPTIONAL member — only when `alternate`.
fn policy_rule(index: usize, alternate: bool) -> Value {
    let mut rule = json!({
        "id": format!("R{index:011}"),
        "expression": format!("variable{index} == Value{index}A"),
    });
    if alternate {
        rule["alternateExpression"] = json!(format!("variable {index} is the first value"));
    }
    rule
}

/// A definition carrying `count` of each list, under the format token `token`.
fn definition(count: usize, token: &str) -> Value {
    json!({
        "version": token,
        "types": (0..count).map(policy_type).collect::<Vec<_>>(),
        "variables": (0..count).map(policy_variable).collect::<Vec<_>>(),
        "rules": (0..count)
            .map(|index| policy_rule(index, index % 2 == 0))
            .collect::<Vec<_>>(),
    })
}

/// The rows every assertion about the digest is made over: a definition absent altogether,
/// the empty definition, each list alone, optional members present and absent, several
/// entries per list, text needing JSON escaping, and three rows of ONE content written under
/// three different format tokens.
fn rows() -> Vec<(&'static str, Option<Value>)> {
    vec![
        ("no definition at all", None),
        (
            "the empty definition",
            Some(json!({ "version": "1", "types": [], "variables": [], "rules": [] })),
        ),
        ("a definition carrying no list at all", Some(json!({}))),
        (
            "one type only",
            Some(json!({ "version": "1", "types": [policy_type(0)] })),
        ),
        (
            "one rule only, carrying no alternate expression",
            Some(json!({ "version": "1", "rules": [policy_rule(0, false)] })),
        ),
        (
            "one variable only",
            Some(json!({ "version": "1", "variables": [policy_variable(0)] })),
        ),
        ("three of each", Some(definition(3, "1"))),
        (
            "three of each under a dotted writer token",
            Some(definition(3, "1.0")),
        ),
        (
            "three of each under a writer token of its own",
            Some(definition(3, "7.4.1")),
        ),
        (
            "text needing JSON escaping",
            Some(json!({
                "version": "1",
                "variables": [{
                    "name": "variable0",
                    "type": "Type0",
                    "description": "a \"quoted\" description,\nsecond line\tand a \\ backslash",
                }],
            })),
        ),
    ]
}

// ---------------------------------------------------------------------------------------
// 1 + 2 + 3. What the digest is taken over, and what it is NOT taken over.
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn the_definition_hash_is_the_digest_of_the_canonical_document_under_the_digest_token() {
    let emu = emulator();
    let mut reported: Vec<(String, String)> = Vec::new();
    for (index, (case, definition)) in rows().into_iter().enumerate() {
        let mut body = json!({ "name": format!("digest-row-{index}") });
        if let Some(definition) = &definition {
            body["policyDefinition"] = definition.clone();
        }
        let created = create(&emu, body).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreateAutomatedReasoningPolicy must answer 200, got {} {:?}",
            created.status, created.body
        );
        let policy_arn = created.text("policyArn");
        let hash = created.text("definitionHash");
        assert_hash_shape(case, &hash);

        assert_eq!(
            hash,
            expected_hash(definition.as_ref()),
            "case {case:?}: the create must report the digest of {:?}, got {hash:?}",
            canonical_text(definition.as_ref(), DIGEST_FORMAT_TOKEN)
        );

        // The token the digest's document states is not the token the definition is
        // published under, so the digest of the published document is another value.
        assert_ne!(
            hash,
            published_document_hash(definition.as_ref()),
            "case {case:?}: the digest is taken over the document stating \
             {DIGEST_FORMAT_TOKEN:?}, not the one stating {PUBLISHED_FORMAT_TOKEN:?}"
        );

        let read = get(&emu, &policy_arn).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: GetAutomatedReasoningPolicy must answer 200, got {} {:?}",
            read.status, read.body
        );
        assert_eq!(
            read.text("definitionHash"),
            hash,
            "case {case:?}: the read must publish the digest the create did, got {:?}",
            read.get("definitionHash")
        );

        // What the definition is PUBLISHED as states the modelled default, whatever token
        // its writer spelled and whatever token the digest's document states.
        let exported = export(&emu, &policy_arn).await;
        assert_eq!(
            exported.status, 200,
            "case {case:?}: ExportAutomatedReasoningPolicyVersion must answer 200, got {} {:?}",
            exported.status, exported.body
        );
        assert_eq!(
            exported.text("version"),
            PUBLISHED_FORMAT_TOKEN,
            "case {case:?}: the published definition states the modelled default, got {:?}",
            exported.get("version")
        );

        reported.push((case.to_string(), hash));
    }

    // Three rows hold the same types, rules and variables under three different writer
    // tokens, so the digest — a function of the CONTENT — is one value across them.
    let same: Vec<&(String, String)> = reported
        .iter()
        .filter(|(case, _)| case.starts_with("three of each"))
        .collect();
    assert_eq!(
        same.len(),
        3,
        "case setup: the three rows of one content must all have been created, got {same:?}"
    );
    for (case, hash) in &same {
        assert_eq!(
            hash, &same[0].1,
            "case {case:?}: one content is one digest however its writer spelled the format \
             token, got {hash:?} beside {:?}",
            same[0].1
        );
    }
}

// ---------------------------------------------------------------------------------------
// 3 + 4. The hash moves with the definition, and is the token a version is cut on.
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn the_hash_a_read_publishes_is_the_token_that_cuts_the_next_version() {
    // Rows differ along the definition the draft is written to: from none to a populated
    // one, from a populated one to another, and from a populated one back to the empty one.
    let rows: Vec<(&str, Option<Value>, Value)> = vec![
        ("written from no definition", None, definition(1, "1")),
        (
            "rewritten to another definition",
            Some(definition(1, "1")),
            definition(2, "1"),
        ),
        (
            "rewritten to the empty definition",
            Some(definition(2, "1")),
            json!({ "version": "1", "types": [], "variables": [], "rules": [] }),
        ),
    ];

    let emu = emulator();
    for (index, (case, before, after)) in rows.into_iter().enumerate() {
        let mut body = json!({ "name": format!("token-row-{index}") });
        if let Some(before) = &before {
            body["policyDefinition"] = before.clone();
        }
        let created = create(&emu, body).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreateAutomatedReasoningPolicy must answer 200, got {} {:?}",
            created.status, created.body
        );
        let policy_arn = created.text("policyArn");

        let updated = update(&emu, &policy_arn, after.clone()).await;
        assert_eq!(
            updated.status, 200,
            "case {case:?}: UpdateAutomatedReasoningPolicy must answer 200, got {} {:?}",
            updated.status, updated.body
        );
        let echoed = updated.text("definitionHash");
        assert_eq!(
            echoed,
            expected_hash(Some(&after)),
            "case {case:?}: the update echoes the digest of the definition it stored, got {:?}",
            updated.get("definitionHash")
        );

        let read = get(&emu, &policy_arn).await;
        assert_eq!(
            read.text("definitionHash"),
            echoed,
            "case {case:?}: a read after the write publishes the echoed digest, got {:?}",
            read.get("definitionHash")
        );

        // A client that re-derived the hash from the document it was served holds another
        // value, and that value is not the draft's concurrency token.
        let refused = cut_version(&emu, &policy_arn, &published_document_hash(Some(&after))).await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: a hash that is not the draft's is refused 400, got {} {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error.as_deref(),
            Some("ConflictException"),
            "case {case:?}: the refusal is a ConflictException, got {:?}",
            refused.error
        );
        assert_eq!(
            refused.text("message"),
            STALE_DEFINITION_HASH,
            "case {case:?}: the refusal carries the one fixed sentence, got {:?}",
            refused.get("message")
        );

        // The hash the READ published is the one that cuts the version.
        let cut = cut_version(&emu, &policy_arn, &read.text("definitionHash")).await;
        assert_eq!(
            cut.status, 200,
            "case {case:?}: the digest a read publishes must cut a version, got {} {:?}",
            cut.status, cut.body
        );
        assert_eq!(
            cut.text("version"),
            "1",
            "case {case:?}: the first version a policy cuts is 1, got {:?}",
            cut.get("version")
        );
        assert_eq!(
            cut.text("policyArn"),
            format!("{policy_arn}:1"),
            "case {case:?}: the version is reported under the versioned ARN, got {:?}",
            cut.get("policyArn")
        );
        assert_eq!(
            cut.text("definitionHash"),
            echoed,
            "case {case:?}: the version freezes the draft's digest, got {:?}",
            cut.get("definitionHash")
        );
    }
}
