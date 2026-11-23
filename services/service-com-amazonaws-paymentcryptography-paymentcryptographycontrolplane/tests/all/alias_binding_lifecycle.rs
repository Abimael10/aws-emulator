// SPDX-License-Identifier: CC0-1.0

//! The alias binding lifecycle, end to end over the wire: `CreateAlias`, `UpdateAlias`,
//! `GetAlias`, `DeleteAlias` and the `KeyIdentifier` forms that resolve through an alias.
//!
//! The rule under test: an alias is a name in the account and Region that binds AT MOST
//! ONE key, and every operation answers from the one stored binding — `UpdateAlias`
//! rebinds the name to the key the request names, or removes the binding when it names
//! none ("the key that you are updating or removing from the alias", `UpdateAliasInput`'s
//! own documentation), while the alias itself stays until `DeleteAlias` removes it; the
//! keys themselves are untouched by any of it.
//!
//! Every call goes through the real `MultiServiceRouter` (`POST /` with `X-Amz-Target:
//! PaymentCryptographyControlPlane.<Op>`, awsJson1_0) and the modelled-constraint walk,
//! through the same harness `key_lifecycle` uses, so what these tests assert is what a
//! signed SDK request observes.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::key_lifecycle::{call, error_code, ok_call, router, string, REGION};

/// A `CreateKey` body for one symmetric data-encryption key: the attribute set this
/// service's own documentation pairs with `AES_128`.
fn key_body() -> Value {
    json!({
        "KeyAttributes": {
            "KeyUsage": "TR31_D0_SYMMETRIC_DATA_ENCRYPTION_KEY",
            "KeyClass": "SYMMETRIC_KEY",
            "KeyAlgorithm": "AES_128",
            "KeyModesOfUse": {"Encrypt": true, "Decrypt": true},
        },
        "Exportable": true,
    })
}

/// Create one key and answer its `KeyArn`.
async fn fresh_key_arn(router: &MultiServiceRouter) -> String {
    let created = ok_call(router, "CreateKey", &key_body()).await;
    string(
        created.get("Key").expect("CreateKey answers a Key"),
        "KeyArn",
    )
    .to_string()
}

/// The `Alias` an answer carries.
pub(crate) fn alias_of(case: &str, answered: &Value) -> Value {
    answered
        .get("Alias")
        .unwrap_or_else(|| panic!("case {case}: no Alias in {answered}"))
        .clone()
}

/// Alias names spanning the class the modelled `AliasName` admits: its minimum and
/// maximum length, a nested name, and a name using every punctuation character the
/// pattern allows.
///
/// Source: `smithy_query(query="operation",operation="UpdateAlias")` — `AliasName`
/// carries `length 7..256` and `pattern ^alias/[a-zA-Z0-9/_-]+$`.
fn alias_names() -> Vec<String> {
    vec![
        "alias/u".to_string(),
        "alias/team/payments/pin_key-1".to_string(),
        "alias/UPPER-and-lower_09".to_string(),
        format!("alias/{}", "r".repeat(250)),
    ]
}

/// Every alias name `ListAliases` reports for one key, following `NextToken` to the
/// last page.
async fn aliases_bound_to(router: &MultiServiceRouter, key_arn: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut body = json!({"KeyArn": key_arn});
        if let Some(next) = &token {
            body["NextToken"] = json!(next);
        }
        let page = ok_call(router, "ListAliases", &body).await;
        let listed = page
            .get("Aliases")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("ListAliases answers an Aliases list, got {page}"))
            .clone();
        for entry in &listed {
            names.push(string(entry, "AliasName").to_string());
        }
        token = page
            .get("NextToken")
            .and_then(Value::as_str)
            .map(str::to_string);
        if token.is_none() {
            return names;
        }
    }
}

/// The `Key` record one key identifier reports, as a reader sees it.
async fn key_record(router: &MultiServiceRouter, key_identifier: &str) -> Value {
    let answered = ok_call(
        router,
        "GetKey",
        &json!({"KeyIdentifier": key_identifier}),
    )
    .await;
    answered
        .get("Key")
        .unwrap_or_else(|| panic!("GetKey answers a Key, got {answered}"))
        .clone()
}

/// RULE: a key can carry many aliases while each alias binds at most one key at a
/// time, so an update moves ONLY the binding of the name its request carries — every
/// other alias keeps the key it held, and no key record changes ("These operations
/// don't affect the underlying key", `CreateAlias`' own documentation).
///
/// The rows differ along how many aliases the key carries and which of them is rebound:
/// the only one, the first of several, a middle one, the last.
#[tokio::test]
async fn update_alias_moves_only_the_binding_it_names() {
    let router = router();
    // (how many aliases the source key carries, the index of the one rebound)
    let rows = [(1usize, 0usize), (2, 0), (2, 1), (5, 2), (5, 4)];
    for (fan_out, rebound) in rows {
        let case = format!("{fan_out} alias(es) on one key, rebinding #{rebound}");
        let source = fresh_key_arn(&router).await;
        let target = fresh_key_arn(&router).await;
        let names: Vec<String> = (0..fan_out)
            .map(|index| format!("alias/fan-{fan_out}-{rebound}-{index}"))
            .collect();
        for name in &names {
            ok_call(
                &router,
                "CreateAlias",
                &json!({"AliasName": name, "KeyArn": source}),
            )
            .await;
        }
        // The two key records as they stand before the update, to compare after it.
        let before = [
            key_record(&router, &source).await,
            key_record(&router, &target).await,
        ];

        let updated = ok_call(
            &router,
            "UpdateAlias",
            &json!({"AliasName": names[rebound], "KeyArn": target}),
        )
        .await;
        let reported = alias_of(&case, &updated);
        assert_eq!(
            (string(&reported, "AliasName"), string(&reported, "KeyArn")),
            (names[rebound].as_str(), target.as_str()),
            "case {case}: the update must answer the binding it stored"
        );

        // Every alias answers from its own stored binding: one moved, the rest did not.
        for (index, name) in names.iter().enumerate() {
            let expected = if index == rebound { &target } else { &source };
            let read = ok_call(&router, "GetAlias", &json!({"AliasName": name})).await;
            assert_eq!(
                string(&alias_of(&case, &read), "KeyArn"),
                expected.as_str(),
                "case {case}: alias {name} must bind {expected}, got {read}"
            );
        }

        // The list answers from the same bindings: the key the alias moved to carries
        // exactly that one name, and the key it moved off keeps all the others.
        assert_eq!(
            aliases_bound_to(&router, &target).await,
            vec![names[rebound].clone()],
            "case {case}: the target key must carry only the rebound alias"
        );
        let kept: Vec<String> = names
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != rebound)
            .map(|(_, name)| name.clone())
            .collect();
        assert_eq!(
            aliases_bound_to(&router, &source).await,
            kept,
            "case {case}: the source key must keep every alias that was not rebound"
        );

        // Rebinding an alias changes neither key record.
        for (key_arn, was) in [(&source, &before[0]), (&target, &before[1])] {
            assert_eq!(
                &key_record(&router, key_arn).await,
                was,
                "case {case}: the record of key {key_arn} must be unchanged by the update"
            );
        }
    }
}

/// RULE: an alias exists only while the registry holds its name, so the binding an
/// update moves is the one the registry holds at that moment: a name that was deleted
/// can no longer be updated, and a name registered again starts from the binding its
/// create stored, never from the one the earlier registration carried.
///
/// The rows differ along the form of the name: a flat one and a nested one.
#[tokio::test]
async fn update_alias_follows_the_registry_across_delete_and_recreate() {
    let router = router();
    let first = fresh_key_arn(&router).await;
    let second = fresh_key_arn(&router).await;
    for name in ["alias/recreated", "alias/team/recreated-2"] {
        ok_call(
            &router,
            "CreateAlias",
            &json!({"AliasName": name, "KeyArn": first}),
        )
        .await;
        let updated = ok_call(
            &router,
            "UpdateAlias",
            &json!({"AliasName": name, "KeyArn": second}),
        )
        .await;
        assert_eq!(
            string(&alias_of(name, &updated), "KeyArn"),
            second,
            "case {name}: the update must store the key it named"
        );

        ok_call(&router, "DeleteAlias", &json!({"AliasName": name})).await;

        // A name the registry no longer holds names nothing to update, and the refused
        // update registers nothing.
        let (status, parsed) = call(
            &router,
            "UpdateAlias",
            &json!({"AliasName": name, "KeyArn": first}),
        )
        .await;
        assert_eq!(
            (status, error_code(&parsed).as_str()),
            (400, "ResourceNotFoundException"),
            "case {name}: a deleted alias cannot be updated, got {status} {parsed}"
        );
        assert_eq!(
            parsed.get("ResourceId").and_then(Value::as_str),
            Some(name),
            "case {name}: the refusal must name the alias asked for, got {parsed}"
        );
        let (status, parsed) = call(&router, "GetAlias", &json!({"AliasName": name})).await;
        assert_eq!(
            (status, error_code(&parsed).as_str()),
            (400, "ResourceNotFoundException"),
            "case {name}: the refused update must not register the name, got {status} {parsed}"
        );
        assert!(
            !aliases_bound_to(&router, &second).await.contains(&name.to_string()),
            "case {name}: a deleted alias must no longer be listed for the key it bound"
        );

        // Registered again, the name starts from the binding this create stored.
        ok_call(
            &router,
            "CreateAlias",
            &json!({"AliasName": name, "KeyArn": first}),
        )
        .await;
        let read = ok_call(&router, "GetAlias", &json!({"AliasName": name})).await;
        assert_eq!(
            string(&alias_of(name, &read), "KeyArn"),
            first,
            "case {name}: the recreated alias must bind the key its create named"
        );
        // And it is updatable again, including the update that removes the binding.
        let unbound = ok_call(&router, "UpdateAlias", &json!({"AliasName": name})).await;
        assert!(
            alias_of(name, &unbound).get("KeyArn").is_none(),
            "case {name}: an update naming no key leaves the alias unbound, got {unbound}"
        );
        let listed_for_first = aliases_bound_to(&router, &first).await;
        assert!(
            !listed_for_first.contains(&name.to_string()),
            "case {name}: an unbound alias is listed for no key, got {listed_for_first:?}"
        );

        // The keys the name bound along the way are all still there.
        for key_arn in [&first, &second] {
            key_record(&router, key_arn).await;
        }
    }
}

/// RULE: `UpdateAlias` moves the one stored binding, and every reader — `GetAlias`,
/// `ListAliases` and a `KeyIdentifier` naming the alias — answers from it.
///
/// The rows differ along the form of the alias name; each row drives the whole class of
/// updates: a rebind to another key, a rebind to the key the name already holds, and an
/// update naming no key at all, which removes the binding.
#[tokio::test]
async fn update_alias_moves_the_one_stored_binding() {
    let router = router();
    for name in alias_names() {
        let case = format!("{} chars: {name}", name.len());
        let first = fresh_key_arn(&router).await;
        let second = fresh_key_arn(&router).await;
        ok_call(
            &router,
            "CreateAlias",
            &json!({"AliasName": name, "KeyArn": first}),
        )
        .await;

        for (step, key_arn) in [("another key", &second), ("the same key", &second)] {
            let case = format!("{case}, rebound to {step}");
            let updated = ok_call(
                &router,
                "UpdateAlias",
                &json!({"AliasName": name, "KeyArn": key_arn}),
            )
            .await;
            let reported = alias_of(&case, &updated);
            assert_eq!(
                (string(&reported, "AliasName"), string(&reported, "KeyArn")),
                (name.as_str(), key_arn.as_str()),
                "case {case}: the update must answer the binding it stored"
            );
            // Read back through the implemented reader: the stored binding is the one
            // every later caller sees.
            let read = ok_call(&router, "GetAlias", &json!({"AliasName": name})).await;
            assert_eq!(
                alias_of(&case, &read),
                reported,
                "case {case}: GetAlias must report the updated binding"
            );
            // And resolving a key through the alias reaches the key it now binds.
            let resolved = ok_call(&router, "GetKey", &json!({"KeyIdentifier": name})).await;
            assert_eq!(
                string(resolved.get("Key").expect("a Key"), "KeyArn"),
                key_arn.as_str(),
                "case {case}: the alias must resolve to the key it is bound to"
            );
        }

        // An update naming no key removes the binding and keeps the alias.
        let case = format!("{case}, unbound");
        let unbound = ok_call(&router, "UpdateAlias", &json!({"AliasName": name})).await;
        let reported = alias_of(&case, &unbound);
        assert_eq!(
            string(&reported, "AliasName"),
            name,
            "case {case}: the alias must still be named"
        );
        assert!(
            reported.get("KeyArn").is_none(),
            "case {case}: an unbound alias reports no KeyArn, got {reported}"
        );
        let read = ok_call(&router, "GetAlias", &json!({"AliasName": name})).await;
        assert_eq!(
            alias_of(&case, &read),
            reported,
            "case {case}: GetAlias must report the alias as unbound"
        );
        // An alias bound to nothing resolves to no key.
        let (status, parsed) = call(&router, "GetKey", &json!({"KeyIdentifier": name})).await;
        assert_eq!(
            (status, error_code(&parsed).as_str()),
            (400, "ResourceNotFoundException"),
            "case {case}: an unbound alias resolves to no key, got {status} {parsed}"
        );

        // The keys themselves are untouched by any of it: both still answer by their own
        // ARNs.
        for key_arn in [&first, &second] {
            ok_call(&router, "GetKey", &json!({"KeyIdentifier": key_arn})).await;
        }
    }
}

/// RULE: an update of a name the account does not hold, or one naming a key it does not
/// hold, is the not-found class naming the identifier the request asked for — and writes
/// nothing.
#[tokio::test]
async fn update_alias_refuses_an_identifier_the_account_does_not_hold() {
    let router = router();
    let key_arn = fresh_key_arn(&router).await;
    let absent_key = format!(
        "arn:aws:payment-cryptography:{REGION}:{}:key/neverminted00000",
        engine::emulator_account_id()
    );
    // (case, the request, the identifier the refusal must name)
    let rows: Vec<(&str, Value, String)> = vec![
        (
            "an alias that was never created",
            json!({"AliasName": "alias/never-created", "KeyArn": key_arn}),
            "alias/never-created".to_string(),
        ),
        (
            "an alias that was never created, naming no key",
            json!({"AliasName": "alias/never-created"}),
            "alias/never-created".to_string(),
        ),
        (
            "a key that was never minted",
            json!({"AliasName": "alias/exists", "KeyArn": absent_key}),
            absent_key.clone(),
        ),
    ];
    ok_call(
        &router,
        "CreateAlias",
        &json!({"AliasName": "alias/exists", "KeyArn": key_arn}),
    )
    .await;

    for (case, body, resource_id) in rows {
        let (status, parsed) = call(&router, "UpdateAlias", &body).await;
        assert_eq!(
            (status, error_code(&parsed).as_str()),
            (400, "ResourceNotFoundException"),
            "case {case}: expected the not-found class, got {status} {parsed}"
        );
        assert_eq!(
            parsed.get("ResourceId").and_then(Value::as_str),
            Some(resource_id.as_str()),
            "case {case}: expected ResourceId {resource_id:?}, got {parsed}"
        );
    }

    // The refused updates stored nothing: the one alias that exists still binds the key
    // it was created with, and the name that does not exist still does not.
    let read = ok_call(&router, "GetAlias", &json!({"AliasName": "alias/exists"})).await;
    assert_eq!(
        string(&alias_of("after the refusals", &read), "KeyArn"),
        key_arn,
        "a refused update must leave the stored binding alone"
    );
    let (status, parsed) = call(
        &router,
        "GetAlias",
        &json!({"AliasName": "alias/never-created"}),
    )
    .await;
    assert_eq!(
        (status, error_code(&parsed).as_str()),
        (400, "ResourceNotFoundException"),
        "a refused update must not create the alias it named, got {status} {parsed}"
    );
}
