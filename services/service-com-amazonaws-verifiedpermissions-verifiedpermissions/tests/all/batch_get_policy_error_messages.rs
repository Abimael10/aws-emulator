// SPDX-License-Identifier: CC0-1.0

//! What a `BatchGetPolicy` per-item miss SAYS.
//!
//! RULE: a request item whose reference names nothing is reported in `errors`
//! with the `BatchGetPolicyErrorCode` for which reference missed, and the item's
//! `message` is the FIXED sentence naming that resource kind — the same for every
//! request, because `BatchGetPolicyErrorItem` models `code`, `policyStoreId` and
//! `policyId` as required members of its own, so the classification and the
//! references the item asked about are already reported there and nothing of the
//! request belongs in the text.
//!
//! The rows span the three codes the model lists (`POLICY_STORE_ALIAS_NOT_FOUND`,
//! `POLICY_STORE_NOT_FOUND`, `POLICY_NOT_FOUND`), the FORM each reference took (a
//! store id, an alias reference, a policy id, a `name/<policyName>` reference)
//! and, for each code, a SECOND item whose reference differs — which is what
//! shows the sentence is a constant and not a template filled from the request.
//!
//! Everything goes over the wire through the same `MultiServiceRouter` the
//! emulator binary serves, on the shared harness in [`crate::harness`].

use serde_json::{json, Value};

use crate::harness::{call, ok, router, store_with_mode};

/// The prefix every policy store alias name carries, from
/// `CreatePolicyStoreAliasInput$aliasName`.
const ALIAS_PREFIX: &str = "policy-store-alias/";

/// One request item, and what the batch's `errors` entry for it must carry.
struct Item {
    case: &'static str,
    /// The item's `policyStoreId` member.
    store_reference: String,
    /// The item's `policyId` member.
    policy_reference: String,
    /// The `BatchGetPolicyErrorCode` the miss must be reported under.
    code: &'static str,
    /// The whole sentence the item's `message` must be, written out here rather
    /// than derived from the code under test.
    message: &'static str,
}

/// A Cedar statement for the one policy the fixture stores, so a row can miss the
/// POLICY inside a store that really exists.
const STATEMENT: &str = "permit(principal, action, resource);";

#[tokio::test]
async fn a_batch_item_miss_carries_the_fixed_sentence_for_its_code() {
    let router = router();
    let live_store = store_with_mode(&router, "OFF").await;
    let stored = ok(
        &router,
        "CreatePolicy",
        &json!({
            "policyStoreId": live_store,
            "definition": { "static": { "statement": STATEMENT } },
            "name": "batch-message-policy",
        }),
    )
    .await;
    let stored_policy_id = stored["policyId"]
        .as_str()
        .expect("policyId is required on the CreatePolicy response")
        .to_string();

    // An ACTIVE alias of that store, so the hit row below reaches the store
    // through an alias and the alias rows are about an alias that really is
    // missing rather than about the prefix.
    let active_alias = format!("{ALIAS_PREFIX}batch-message-alias");
    ok(
        &router,
        "CreatePolicyStoreAlias",
        &json!({ "aliasName": active_alias, "policyStoreId": live_store }),
    )
    .await;

    const POLICY_MISSING: &str = "Policy not found";
    const STORE_MISSING: &str = "Policy store not found";
    const ALIAS_MISSING: &str = "Policy store alias not found";

    let items = [
        Item {
            case: "a live store and a policy id it does not hold",
            store_reference: live_store.clone(),
            policy_reference: "SPDOESNOTEXIST000000".to_string(),
            code: "POLICY_NOT_FOUND",
            message: POLICY_MISSING,
        },
        Item {
            case: "a live store and another policy id it does not hold",
            store_reference: live_store.clone(),
            policy_reference: "SPNOTTHEREEITHER0001".to_string(),
            code: "POLICY_NOT_FOUND",
            message: POLICY_MISSING,
        },
        Item {
            // The model documents a policy NAME as usable in place of the id,
            // prefixed with `name/`: another form of the same missing reference.
            case: "a live store and a name/ reference naming no policy",
            store_reference: live_store.clone(),
            policy_reference: "name/no-such-policy".to_string(),
            code: "POLICY_NOT_FOUND",
            message: POLICY_MISSING,
        },
        Item {
            case: "the live store reached through its alias, and no such policy",
            store_reference: active_alias.clone(),
            policy_reference: "SPDOESNOTEXIST000000".to_string(),
            code: "POLICY_NOT_FOUND",
            message: POLICY_MISSING,
        },
        Item {
            case: "a store id that was never minted",
            store_reference: "PSNONEXISTENT00000000".to_string(),
            policy_reference: stored_policy_id.clone(),
            code: "POLICY_STORE_NOT_FOUND",
            message: STORE_MISSING,
        },
        Item {
            case: "another store id that was never minted",
            store_reference: "PS-other_1/2".to_string(),
            policy_reference: "SPDOESNOTEXIST000000".to_string(),
            code: "POLICY_STORE_NOT_FOUND",
            message: STORE_MISSING,
        },
        Item {
            case: "an alias reference naming no alias",
            store_reference: format!("{ALIAS_PREFIX}no-such-alias"),
            policy_reference: stored_policy_id.clone(),
            code: "POLICY_STORE_ALIAS_NOT_FOUND",
            message: ALIAS_MISSING,
        },
        Item {
            case: "another alias reference naming no alias",
            store_reference: format!("{ALIAS_PREFIX}also-no-such-alias"),
            policy_reference: "name/no-such-policy".to_string(),
            code: "POLICY_STORE_ALIAS_NOT_FOUND",
            message: ALIAS_MISSING,
        },
    ];

    // One call carrying every row, plus a HIT, so the misses are reported beside
    // an item that resolved — a miss must not cost another item its answer.
    let mut requests: Vec<Value> = items
        .iter()
        .map(|item| {
            json!({
                "policyStoreId": item.store_reference,
                "policyId": item.policy_reference,
            })
        })
        .collect();
    requests.push(json!({
        "policyStoreId": live_store,
        "policyId": stored_policy_id,
    }));

    let (status, body) = call(&router, "BatchGetPolicy", &json!({ "requests": requests })).await;
    assert_eq!(
        status, 200,
        "a batch read answers 200 whatever its items resolve to, got {status} {body}"
    );
    let errors = body["errors"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        errors.len(),
        items.len(),
        "every missing item must be reported in errors, got {body}"
    );
    assert_eq!(
        body["results"].as_array().map(Vec::len).unwrap_or_default(),
        1,
        "the one item that resolves must still be reported in results, got {body}"
    );

    for (item, error) in items.iter().zip(&errors) {
        let Item {
            case,
            store_reference,
            policy_reference,
            code,
            message: expected,
        } = item;

        let answered_code = error["code"].as_str().unwrap_or_default();
        assert_eq!(
            answered_code, *code,
            "case {case:?}: expected code {code:?}, got {error}"
        );
        let message = error["message"].as_str().unwrap_or_default();
        assert_eq!(
            message, *expected,
            "case {case:?}: expected the message {expected:?}, got {message:?}"
        );
        assert!(
            !message.contains(store_reference.as_str())
                && !message.contains(policy_reference.as_str()),
            "case {case:?}: the message must carry neither reference \
             ({store_reference:?}, {policy_reference:?}), got {message:?}"
        );

        // The references the item asked about are reported in the members that
        // model them, exactly as the item sent them, so the constant sentence
        // withholds nothing a client needs to pair the error with its item.
        assert_eq!(
            error["policyStoreId"].as_str().unwrap_or_default(),
            store_reference.as_str(),
            "case {case:?}: expected policyStoreId {store_reference:?}, got {error}"
        );
        assert_eq!(
            error["policyId"].as_str().unwrap_or_default(),
            policy_reference.as_str(),
            "case {case:?}: expected policyId {policy_reference:?}, got {error}"
        );
    }

    // One sentence per code across every row: the point of the rule is that a
    // client can key on it.
    for code in [
        "POLICY_NOT_FOUND",
        "POLICY_STORE_NOT_FOUND",
        "POLICY_STORE_ALIAS_NOT_FOUND",
    ] {
        let messages: std::collections::BTreeSet<&str> = errors
            .iter()
            .filter(|error| error["code"].as_str() == Some(code))
            .map(|error| error["message"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(
            messages.len(),
            1,
            "case {code:?}: every item under one code must carry ONE sentence, got {messages:?}"
        );
    }
}
