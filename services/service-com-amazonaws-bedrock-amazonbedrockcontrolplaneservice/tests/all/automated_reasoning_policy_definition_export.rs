// SPDX-License-Identifier: CC0-1.0

//! `ExportAutomatedReasoningPolicyVersion` end-to-end: the definition a policy revision
//! holds is exported under the `AutomatedReasoningPolicyArn` that revision is addressed by,
//! over the same `MultiServiceRouter` the emulator boots.
//!
//! The rules under test, each asserted over a SET of inputs rather than over one recorded
//! request:
//!
//! 1. **The export reports the definition the addressed revision holds**: the unversioned
//!    ARN exports the working draft, for drafts differing in whether a definition is
//!    present, which of its lists are present and how many entries each carries — and the
//!    exported definition carries the format version this SERVICE writes definitions in,
//!    the member's own modelled default
//!    (`AutomatedReasoningPolicyDefinition$version`, `@default("1")`), whatever token the
//!    writing request spelled and for a draft holding no definition at all.
//! 2. **The exported definition is the one the revision's `definitionHash` is taken over**:
//!    a policy created from an exported definition reports the hash the exported revision
//!    reported, so the export loses nothing the hash depends on.
//! 3. **A version exports the snapshot it froze**: writing the draft after a version was
//!    cut moves what the unversioned ARN exports and leaves what the versioned ARN exports
//!    alone.
//! 4. **The export writes nothing**: the definition, hash and instants a read reports are
//!    the same before and after, however many times it is exported.
//! 5. **An ARN naming no revision is 404**: an id nothing minted and a minted id in another
//!    region are reported as a missing policy, while a version number the policy has not
//!    cut is reported as the missing version.
//! 6. **The format token is the service's own, and is not content**: whatever token the
//!    request that wrote the definition carried — a dotted release number, another whole
//!    number, an empty string, none at all — the definition read back carries the one
//!    format version this service writes definitions in, and two definitions differing only
//!    in the token their writers spelled report ONE `definitionHash`, because the digest is
//!    taken over the canonical form of the document rather than over the token its writer
//!    spelled (`automated_reasoning_policies.rs` pins the digest itself).
//!
//! Every expectation is derived from the model, cited per row
//! (`smithy_query(query="operation",operation="ExportAutomatedReasoningPolicyVersion")`,
//! `smithy_query(query="shape",shape="AutomatedReasoningPolicyDefinition")`). The requests
//! go through `crate::support` — the ONE harness every suite in this crate drives the
//! emulator with.

use serde_json::{json, Value};

use crate::support::{emulator, label, Emulator, Reply, ACCOUNT, REGION};

/// What an exported `AutomatedReasoningPolicyDefinition` states as the format it is written
/// in: the token `AutomatedReasoningPolicyDefinition$version` declares as its `@default`
/// (`smithy_query(query="shape",shape="AutomatedReasoningPolicyDefinition")`: target
/// `AutomatedReasoningPolicyFormatVersion`, a plain string, `@default("1")`), which is the
/// format this service holds every definition in whatever a writing request spelled.
const DEFAULT_FORMAT_VERSION: &str = "1";

/// What the refusal of an ARN naming a version the addressed policy has not cut says, as
/// `GetAutomatedReasoningPolicy` reports the same ARN.
const VERSION_NOT_FOUND: &str = "Policy version does not exist";

/// What the refusal of an ARN naming no policy of this account says, as every operation
/// addressed by an `AutomatedReasoningPolicyArn` reports one: a FIXED sentence, carrying
/// neither the ARN nor the account.
const POLICY_NOT_FOUND: &str = "Policy does not exist";

async fn create_policy(emu: &Emulator, body: Value) -> Reply {
    emu.call("POST", "/automated-reasoning-policies", Some(body))
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

async fn get_policy(emu: &Emulator, policy_arn: &str) -> Reply {
    emu.call(
        "GET",
        &format!("/automated-reasoning-policies/{}", label(policy_arn)),
        None,
    )
    .await
}

async fn update_policy(emu: &Emulator, policy_arn: &str, body: Value) -> Reply {
    emu.call(
        "PATCH",
        &format!("/automated-reasoning-policies/{}", label(policy_arn)),
        Some(body),
    )
    .await
}

async fn create_version(emu: &Emulator, policy_arn: &str, body: Value) -> Reply {
    emu.call(
        "POST",
        &format!(
            "/automated-reasoning-policies/{}/versions",
            label(policy_arn)
        ),
        Some(body),
    )
    .await
}

/// A policy created from `body`, as `(policyArn, definitionHash)`.
async fn policy(emu: &Emulator, body: Value) -> (String, String) {
    let created = create_policy(emu, body).await;
    assert_eq!(
        created.status, 200,
        "case setup: CreateAutomatedReasoningPolicy must answer 200, got {} {:?}",
        created.status, created.body
    );
    (created.text("policyArn"), created.text("definitionHash"))
}

/// One custom type, as `AutomatedReasoningPolicyDefinitionType` spells it (`name` and
/// `values[].value` are `^\p{L}[\p{L}\p{M}0-9_]*( [\p{L}\p{M}0-9_]+)*$`, `values` is
/// `@length(1, 150)`).
fn policy_type(index: usize) -> Value {
    json!({
        "name": format!("Type{index}"),
        "description": format!("the type {index} a response is validated against"),
        "values": [
            { "value": format!("Value{index}A"), "description": "the first value" },
            { "value": format!("Value{index}B") },
        ],
    })
}

/// One variable, as `AutomatedReasoningPolicyDefinitionVariable` spells it (every member
/// required).
fn policy_variable(index: usize) -> Value {
    json!({
        "name": format!("variable{index}"),
        "type": format!("Type{index}"),
        "description": format!("what variable {index} represents"),
    })
}

/// One rule, as `AutomatedReasoningPolicyDefinitionRule` spells it (`id` is
/// `^[A-Z][0-9A-Z]{11}$`); `alternate_expression` exercises the one OPTIONAL member a rule
/// carries.
fn policy_rule(index: usize, alternate: bool) -> Value {
    let mut rule = json!({
        "id": format!("R{index:011}"),
        "expression": format!("variable{index} == Value{index}A"),
    });
    if alternate {
        rule["alternateExpression"] = json!(format!("variable {index} must be the first value"));
    }
    rule
}

/// A definition carrying `types` types, `variables` variables and `rules` rules, with the
/// lists `present` names and no others, so a row can hold a definition whose lists are
/// absent rather than empty.
fn definition(present: &[&str], counts: (usize, usize, usize), format_version: bool) -> Value {
    let mut built = json!({});
    if format_version {
        built["version"] = json!(DEFAULT_FORMAT_VERSION);
    }
    if present.contains(&"types") {
        built["types"] = Value::Array((0..counts.0).map(policy_type).collect());
    }
    if present.contains(&"variables") {
        built["variables"] = Value::Array((0..counts.1).map(policy_variable).collect());
    }
    if present.contains(&"rules") {
        built["rules"] = Value::Array(
            (0..counts.2)
                .map(|index| policy_rule(index, index % 2 == 0))
                .collect(),
        );
    }
    built
}

/// A definition as its CONTENT: each of its three lists, an absent one read as the empty
/// list, and NOT the format token the document carries (asserted on its own, rule 6).
///
/// Two definitions that normalize alike hold the same rules, variables and types, which is
/// what "the export reports the definition the revision holds" is asserted over — a list
/// the request omitted and the same list sent empty are one definition, and so are two that
/// differ only in the format token the request spelled (rule 6).
fn content(definition: &Value) -> Value {
    let list = |name: &str| {
        definition
            .get(name)
            .cloned()
            .unwrap_or_else(|| json!([]))
    };
    json!({
        "types": list("types"),
        "variables": list("variables"),
        "rules": list("rules"),
    })
}

// ---------------------------------------------------------------------------------------
// 1 + 2. What the draft holds is what the export reports, and the export keeps everything
//        the revision's hash is taken over.
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn an_export_reports_the_definition_the_addressed_draft_holds() {
    // Rows differ along the CONTENT of the definition the draft holds: whether there is one
    // at all, which of its three lists the request carried, how many entries each carries,
    // and whether the request spelled the format version.
    struct Row {
        case: &'static str,
        definition: Option<Value>,
    }

    let rows = [
        Row {
            case: "a draft created with no definition at all",
            definition: None,
        },
        Row {
            case: "a definition whose every list is empty",
            definition: Some(definition(
                &["types", "variables", "rules"],
                (0, 0, 0),
                true,
            )),
        },
        Row {
            case: "a definition carrying no list and no format version",
            definition: Some(definition(&[], (0, 0, 0), false)),
        },
        Row {
            case: "a definition of one type only",
            definition: Some(definition(&["types"], (1, 0, 0), true)),
        },
        Row {
            case: "a definition of one rule only, with no format version",
            definition: Some(definition(&["rules"], (0, 0, 1), false)),
        },
        Row {
            case: "a definition of one type, variable and rule",
            definition: Some(definition(
                &["types", "variables", "rules"],
                (1, 1, 1),
                true,
            )),
        },
        Row {
            case: "a definition of three types, two variables and five rules",
            definition: Some(definition(
                &["types", "variables", "rules"],
                (3, 2, 5),
                true,
            )),
        },
    ];

    let emu = emulator();
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let mut body = json!({ "name": format!("exported policy {index}") });
        if let Some(definition) = &row.definition {
            body["policyDefinition"] = definition.clone();
        }
        let (policy_arn, draft_hash) = policy(&emu, body).await;

        let exported = export(&emu, &policy_arn).await;
        assert_eq!(
            exported.status, 200,
            "case {case:?}: ExportAutomatedReasoningPolicyVersion must answer 200, got {} {:?}",
            exported.status, exported.body
        );
        // `policyDefinition` is the operation's `@httpPayload`, so the BODY is the
        // definition itself.
        let expected = content(row.definition.as_ref().unwrap_or(&json!({})));
        assert_eq!(
            content(&exported.body),
            expected,
            "case {case:?}: the export must report the definition the draft holds, expected \
             {expected:?}, got {:?}",
            exported.body
        );
        // Whatever a writer spelled, a read reports the format THIS SERVICE writes
        // definitions in; a draft holding NO document stands for the empty one, stamped
        // alike.
        let expected_token = DEFAULT_FORMAT_VERSION;
        assert_eq!(
            exported.body.get("version").and_then(Value::as_str),
            Some(expected_token),
            "case {case:?}: the exported definition must carry the format token \
             {expected_token:?}, got {:?}",
            exported.body.get("version")
        );

        // 2. The export keeps everything the hash is a function of: a policy created from
        //    the exported definition reports the hash the exported revision reported.
        let (_, reimported_hash) = policy(
            &emu,
            json!({
                "name": format!("reimported policy {index}"),
                "policyDefinition": exported.body,
            }),
        )
        .await;
        assert_eq!(
            reimported_hash, draft_hash,
            "case {case:?}: a policy created from the exported definition must report the \
             exported revision's definitionHash {draft_hash:?}, got {reimported_hash:?}"
        );
    }
}

// ---------------------------------------------------------------------------------------
// 3 + 4. A version exports the snapshot it froze; exporting writes nothing.
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn a_version_exports_the_snapshot_it_froze_while_the_draft_moves_on() {
    // Rows: the definition the version is cut from, and the one written over the draft
    // after the cut — differing in which direction the content grows, so neither the frozen
    // nor the current definition is the larger one in every row.
    let rows: Vec<(&str, Value, Value)> = vec![
        (
            "one rule frozen, three rules written",
            definition(&["types", "variables", "rules"], (1, 1, 1), true),
            definition(&["types", "variables", "rules"], (3, 3, 3), true),
        ),
        (
            "three rules frozen, one rule written",
            definition(&["types", "variables", "rules"], (3, 3, 3), true),
            definition(&["types", "variables", "rules"], (1, 1, 1), true),
        ),
        (
            "an empty definition frozen, two rules written",
            definition(&["types", "variables", "rules"], (0, 0, 0), true),
            definition(&["types", "variables", "rules"], (2, 2, 2), true),
        ),
        (
            "rules frozen, types only written",
            definition(&["rules"], (0, 0, 2), true),
            definition(&["types"], (2, 0, 0), true),
        ),
    ];

    let emu = emulator();
    for (index, (case, frozen, written)) in rows.iter().enumerate() {
        let (policy_arn, draft_hash) = policy(
            &emu,
            json!({
                "name": format!("snapshot exported policy {index}"),
                "policyDefinition": frozen.clone(),
            }),
        )
        .await;

        let cut = create_version(
            &emu,
            &policy_arn,
            json!({ "lastUpdatedDefinitionHash": draft_hash }),
        )
        .await;
        assert_eq!(
            cut.status, 200,
            "case {case:?}: CreateAutomatedReasoningPolicyVersion must answer 200, got {} {:?}",
            cut.status, cut.body
        );
        let version_arn = cut.text("policyArn");

        let updated = update_policy(&emu, &policy_arn, json!({ "policyDefinition": written })).await;
        assert_eq!(
            updated.status, 200,
            "case {case:?}: UpdateAutomatedReasoningPolicy must answer 200, got {} {:?}",
            updated.status, updated.body
        );

        let frozen_export = export(&emu, &version_arn).await;
        assert_eq!(
            frozen_export.status, 200,
            "case {case:?}: the versioned ARN must export, got {} {:?}",
            frozen_export.status, frozen_export.body
        );
        assert_eq!(
            content(&frozen_export.body),
            content(frozen),
            "case {case:?}: the version exports the definition it froze, got {:?}",
            frozen_export.body
        );

        let draft_export = export(&emu, &policy_arn).await;
        assert_eq!(
            content(&draft_export.body),
            content(written),
            "case {case:?}: the unversioned ARN exports the draft as it now stands, got {:?}",
            draft_export.body
        );

        // 4. The export is a read: the revision a later call reads is the one it was before,
        //    however many exports ran.
        let before = get_policy(&emu, &version_arn).await;
        for round in 0..3 {
            let repeat = export(&emu, &version_arn).await;
            assert_eq!(
                repeat.body, frozen_export.body,
                "case {case:?} export {round}: repeated exports must report one definition"
            );
            let repeat_draft = export(&emu, &policy_arn).await;
            assert_eq!(
                repeat_draft.body, draft_export.body,
                "case {case:?} export {round}: exporting the draft must not move it"
            );
        }
        let after = get_policy(&emu, &version_arn).await;
        assert_eq!(
            after.body, before.body,
            "case {case:?}: exporting must leave the revision a read reports unchanged"
        );
    }
}

// ---------------------------------------------------------------------------------------
// 5. The ARN the export is addressed by.
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn an_arn_that_names_no_revision_is_refused() {
    let emu = emulator();
    let (policy_arn, draft_hash) = policy(
        &emu,
        json!({
            "name": "addressed exported policy",
            "policyDefinition": definition(&["types", "variables", "rules"], (1, 1, 1), true),
        }),
    )
    .await;
    let cut = create_version(
        &emu,
        &policy_arn,
        json!({ "lastUpdatedDefinitionHash": draft_hash }),
    )
    .await;
    assert_eq!(
        cut.status, 200,
        "case setup: the first cut must answer 200, got {} {:?}",
        cut.status, cut.body
    );

    let prefix = format!("arn:aws:bedrock:{REGION}:{ACCOUNT}:automated-reasoning-policy/");
    let id = policy_arn
        .strip_prefix(&prefix)
        .unwrap_or_else(|| panic!("case setup: ARN {policy_arn:?} is not under {prefix:?}"))
        .to_string();

    // (case, the ARN the export is addressed by, the message it is refused with). Rows: an
    // id nothing minted and this id in another region name no policy; a version number this
    // policy has not cut names a revision of a policy that IS there, which is reported as
    // the missing version.
    let rows: Vec<(&str, String, String)> = vec![
        (
            "an id nothing minted",
            format!("{prefix}00000000000z"),
            POLICY_NOT_FOUND.to_string(),
        ),
        (
            "the minted id in another region",
            format!("arn:aws:bedrock:eu-west-1:{ACCOUNT}:automated-reasoning-policy/{id}"),
            POLICY_NOT_FOUND.to_string(),
        ),
        (
            "the version right past the last one cut",
            format!("{policy_arn}:2"),
            VERSION_NOT_FOUND.to_string(),
        ),
        (
            "a version number far past the last one cut",
            format!("{policy_arn}:99"),
            VERSION_NOT_FOUND.to_string(),
        ),
    ];

    for (case, arn, message) in &rows {
        let refused = export(&emu, arn).await;
        assert_eq!(
            refused.status, 404,
            "case {case:?}: {arn:?} must be refused with 404, got {} {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error.as_deref(),
            Some("ResourceNotFoundException"),
            "case {case:?}: expected ResourceNotFoundException, got {:?}",
            refused.error
        );
        assert_eq!(
            refused.body.get("message").and_then(Value::as_str),
            Some(message.as_str()),
            "case {case:?}: expected the message {message:?}, got {:?}",
            refused.body
        );
    }

    // The version that WAS cut exports, so the rows above are refused for the revision they
    // name and not for the operation.
    let served = export(&emu, &format!("{policy_arn}:1")).await;
    assert_eq!(
        served.status, 200,
        "case the version that was cut: must export, got {} {:?}",
        served.status, served.body
    );
}

// ---------------------------------------------------------------------------------------
// 6. The format token an exported definition carries is the service's, and is not content.
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn an_exported_definition_carries_the_services_own_format_token() {
    // Rows differ along the format token the WRITING request spelled for one and the same
    // definition content: `AutomatedReasoningPolicyFormatVersion` is a plain string with no
    // enum, pattern or length constraint
    // (`smithy_query(query="shape",shape="AutomatedReasoningPolicyFormatVersion")`), so any
    // of these reaches the handler — and none of them is a format this service publishes, so
    // the document read back states the service's own format whichever row wrote it.
    let rows: Vec<(&str, Option<&str>)> = vec![
        ("no format version at all", None),
        ("the shape's own default", Some("1")),
        ("a dotted release number", Some("1.0")),
        ("a longer dotted release number", Some("1.0.0")),
        ("a whole number past it", Some("2")),
        ("a whole number below it", Some("0")),
        ("a prefixed token", Some("v1")),
        ("an empty token", Some("")),
    ];

    let emu = emulator();
    // One content for every row, so the only thing a row varies is the format token.
    let written = definition(&["types", "variables", "rules"], (2, 2, 2), false);
    let mut hashes: Vec<(&str, String)> = Vec::new();

    for (index, (case, token)) in rows.iter().enumerate() {
        let mut sent = written.clone();
        if let Some(token) = token {
            sent["version"] = json!(token);
        }
        let (policy_arn, draft_hash) = policy(
            &emu,
            json!({
                "name": format!("format stamped policy {index}"),
                "policyDefinition": sent.clone(),
            }),
        )
        .await;

        // The draft reads back carrying the format this service writes definitions in,
        // whatever the writer spelled.
        let expected_token = DEFAULT_FORMAT_VERSION;
        let exported = export(&emu, &policy_arn).await;
        assert_eq!(
            exported.status, 200,
            "case {case:?}: ExportAutomatedReasoningPolicyVersion must answer 200, got {} {:?}",
            exported.status, exported.body
        );
        assert_eq!(
            exported.body.get("version").and_then(Value::as_str),
            Some(expected_token),
            "case {case:?}: the exported definition must carry the format token \
             {expected_token:?}, got {:?}",
            exported.body.get("version")
        );
        assert_eq!(
            content(&exported.body),
            content(&sent),
            "case {case:?}: the export must report the definition the draft holds, got {:?}",
            exported.body
        );

        // A version cut from that draft freezes the same document, so the snapshot a
        // versioned ARN exports is stamped alike.
        let cut = create_version(
            &emu,
            &policy_arn,
            json!({ "lastUpdatedDefinitionHash": draft_hash.clone() }),
        )
        .await;
        assert_eq!(
            cut.status, 200,
            "case {case:?}: CreateAutomatedReasoningPolicyVersion must answer 200, got {} {:?}",
            cut.status, cut.body
        );
        let frozen = export(&emu, &cut.text("policyArn")).await;
        assert_eq!(
            frozen.body.get("version").and_then(Value::as_str),
            Some(expected_token),
            "case {case:?}: the frozen snapshot must carry the format token \
             {expected_token:?}, got {:?}",
            frozen.body.get("version")
        );

        // And the exported document feeds straight back in: a policy created from it reports
        // the hash the exported revision reported.
        let (_, reimported_hash) = policy(
            &emu,
            json!({
                "name": format!("format stamped reimport {index}"),
                "policyDefinition": exported.body.clone(),
            }),
        )
        .await;
        assert_eq!(
            reimported_hash, draft_hash,
            "case {case:?}: a policy created from the exported definition must report the \
             exported revision's definitionHash {draft_hash:?}, got {reimported_hash:?}"
        );

        hashes.push((case, draft_hash));
    }

    // The format token is not content: one definition written under any of these tokens is
    // one definition, so every row reports the same `definitionHash`.
    let (first_case, first_hash) = hashes
        .first()
        .cloned()
        .expect("case setup: the table must carry rows");
    for (case, hash) in &hashes {
        assert_eq!(
            hash, &first_hash,
            "case {case:?}: a definition differing from {first_case:?} only in the format \
             token the request spelled must report its definitionHash {first_hash:?}, got \
             {hash:?}"
        );
    }
}

// ---------------------------------------------------------------------------------------
// 7. The format token survives an export/import round trip, on every revision.
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn the_exported_format_token_is_the_modelled_default_on_every_revision() {
    // The token is a property of the SERVICE's document format rather than of a revision,
    // so every revision an export can be addressed by reports the same one. The rows differ
    // along the revision addressed and along what wrote it: a draft nothing wrote a
    // definition into, a draft written by the create, a draft the update rewrote, a version
    // cut from such a draft, and the draft a re-import of an exported document wrote — and
    // the writing requests spell the token differently (none at all, a dotted release
    // number, a whole number past the default), so no row's answer can come from an echo.
    struct Row {
        case: &'static str,
        /// The token the CREATE's definition spelled; `None` for a create carrying no
        /// definition at all.
        created_with: Option<Option<&'static str>>,
        /// The token the UPDATE's definition spelled, when a row rewrites the draft.
        updated_with: Option<&'static str>,
    }

    let rows = [
        Row {
            case: "a draft created with no definition",
            created_with: None,
            updated_with: None,
        },
        Row {
            case: "a draft created with no format token",
            created_with: Some(None),
            updated_with: None,
        },
        Row {
            case: "a draft created under a dotted release number",
            created_with: Some(Some("1.0")),
            updated_with: None,
        },
        Row {
            case: "a draft rewritten under a whole number past the default",
            created_with: Some(Some("1.0")),
            updated_with: Some("2"),
        },
        Row {
            case: "a draft rewritten under no format token",
            created_with: Some(Some("9.9.9")),
            updated_with: Some(""),
        },
    ];

    let emu = emulator();
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let mut body = json!({ "name": format!("token stamped policy {index}") });
        if let Some(token) = row.created_with {
            let mut written = definition(&["types", "variables", "rules"], (1, 1, 1), false);
            if let Some(token) = token {
                written["version"] = json!(token);
            }
            body["policyDefinition"] = written;
        }
        let (policy_arn, created_hash) = policy(&emu, body).await;

        let mut draft_hash = created_hash;
        if let Some(token) = row.updated_with {
            let mut rewritten = definition(&["rules"], (0, 0, 2), false);
            rewritten["version"] = json!(token);
            let updated = update_policy(
                &emu,
                &policy_arn,
                json!({ "policyDefinition": rewritten }),
            )
            .await;
            assert_eq!(
                updated.status, 200,
                "case {case:?}: UpdateAutomatedReasoningPolicy must answer 200, got {} {:?}",
                updated.status, updated.body
            );
            draft_hash = updated.text("definitionHash");
        }

        // The draft, the version cut from it, and the draft a re-import of the exported
        // document wrote: three revisions, one token.
        let exported = export(&emu, &policy_arn).await;
        assert_eq!(
            exported.status, 200,
            "case {case:?}: the draft must export, got {} {:?}",
            exported.status, exported.body
        );
        let cut = create_version(
            &emu,
            &policy_arn,
            json!({ "lastUpdatedDefinitionHash": draft_hash.clone() }),
        )
        .await;
        assert_eq!(
            cut.status, 200,
            "case {case:?}: CreateAutomatedReasoningPolicyVersion must answer 200, got {} {:?}",
            cut.status, cut.body
        );
        let frozen = export(&emu, &cut.text("policyArn")).await;
        let (reimported_arn, _) = policy(
            &emu,
            json!({
                "name": format!("token stamped reimport {index}"),
                "policyDefinition": exported.body.clone(),
            }),
        )
        .await;
        let reimported = export(&emu, &reimported_arn).await;

        for (revision, answer) in [
            ("the draft", &exported),
            ("the version it cut", &frozen),
            ("the re-imported draft", &reimported),
        ] {
            assert_eq!(
                answer.body.get("version").and_then(Value::as_str),
                Some(DEFAULT_FORMAT_VERSION),
                "case {case:?}/{revision}: the exported definition must state the format \
                 version this service writes definitions in ({DEFAULT_FORMAT_VERSION:?}), got \
                 {:?}",
                answer.body.get("version")
            );
        }
        // And the re-import is a round trip: the document the export handed out describes the
        // same definition, so it reports the hash the exported revision reported.
        assert_eq!(
            content(&reimported.body),
            content(&exported.body),
            "case {case:?}: the re-imported draft must hold the definition the export \
             reported, got {:?}",
            reimported.body
        );
    }
}
