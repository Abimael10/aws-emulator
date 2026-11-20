// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the AUTHORIZATION SERVICE a domain configuration names,
//! driven against the `CreateDomainConfiguration` / `DescribeDomainConfiguration` /
//! `UpdateDomainConfiguration` helpers of the parent suite plus `CreateAuthorizer` and
//! `DeleteAuthorizer`, so every authorizer a case names is one this account's own API
//! created.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is what an AWS SDK
//! client would see, not an internal function's return value.
//!
//! The rule under test: `authorizerConfig` is "An object that specifies the authorization
//! service for a domain" (`smithy_query(query="operation",operation="CreateDomainConfiguration")`),
//! and that service is one of this account's OWN custom authorizers — the records
//! `CreateAuthorizer` writes — so a configuration may only name one the account holds: a
//! device connecting under a configuration naming an authorizer nobody created would reach
//! no authorization service at all. A name the account holds none under is refused with the
//! modelled `InvalidRequestException` (HTTP 400), the create writes nothing (so the read
//! that follows is a miss) and the update leaves the record as it was.

use super::*;

/// RULE: a configuration may only name a custom authorizer the account holds, and a
/// create that names another writes nothing.
#[tokio::test]
async fn a_create_may_only_name_an_authorizer_the_account_holds() {
    let emulator = Emulator::new();
    create_authorizer(&emulator, "held", "domain-authorizer").await;
    create_authorizer(&emulator, "removed", "removed-authorizer").await;
    delete_authorizer(&emulator, "removed", "removed-authorizer").await;

    // The rows span the ways a named authorizer can be one the account does not hold:
    // never created, created and deleted again, and one differing from a held name only
    // in case, since the name is the authorizer's identity byte for byte.
    let rows = [
        ("never created", "no-such-authorizer"),
        ("created and deleted", "removed-authorizer"),
        ("differing in case", "Domain-Authorizer"),
    ];
    for (index, (case, authorizer_name)) in rows.iter().enumerate() {
        let name = format!("authorizer-miss-{index}");
        let refused = emulator
            .create_configuration(
                &name,
                &json!({
                    "authorizerConfig": { "defaultAuthorizerName": authorizer_name },
                }),
            )
            .await;
        assert_eq!(
            refused.status,
            400,
            "case {case}: expected 400, got {} body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.error_code(),
            "InvalidRequestException",
            "case {case}: got {}",
            refused.text()
        );
        assert_eq!(
            refused.error_message(),
            format!("Custom authorizer does not exist, AuthorizerName: {authorizer_name}"),
            "case {case}: got {}",
            refused.error_message()
        );
        // Nothing was written, which is what makes the read that follows a miss.
        let read = emulator.describe_configuration(&name).await;
        assert_eq!(
            read.status,
            404,
            "case {case}: a refused create must write nothing, got {} body {}",
            read.status,
            read.text()
        );
    }

    // An authorizer the account DOES hold is accepted, with and without the override
    // flag, and the reader reports the configuration it was given.
    for (index, allow_override) in [None, Some(false), Some(true)].into_iter().enumerate() {
        let case = format!("a held authorizer with allowAuthorizerOverride {allow_override:?}");
        let name = format!("authorizer-held-{index}");
        let mut authorizer_config = json!({ "defaultAuthorizerName": "domain-authorizer" });
        if let Some(allow_override) = allow_override {
            authorizer_config["allowAuthorizerOverride"] = json!(allow_override);
        }
        let created = emulator
            .create_configuration(&name, &json!({ "authorizerConfig": authorizer_config }))
            .await;
        assert_eq!(
            created.status,
            200,
            "case {case}: expected 200, got {} body {}",
            created.status,
            created.text()
        );
        let described = emulator.describe_configuration(&name).await.json();
        assert_eq!(
            described["authorizerConfig"], authorizer_config,
            "case {case}: the read must report the authorizer configuration, got {described}"
        );
    }
}

/// RULE: an update names an authorization service the same way a create does, and a
/// refused update leaves the record as it was; `removeAuthorizerConfig` takes it off.
#[tokio::test]
async fn an_update_resolves_the_authorizer_it_names_and_can_remove_it() {
    let emulator = Emulator::new();
    create_authorizer(&emulator, "update", "first-authorizer").await;
    create_authorizer(&emulator, "update", "second-authorizer").await;

    let name = "authorizer.updated";
    let created = emulator
        .create_configuration(
            name,
            &json!({
                "authorizerConfig": { "defaultAuthorizerName": "first-authorizer" },
            }),
        )
        .await;
    assert_eq!(
        created.status,
        200,
        "case the configuration to update: expected 200, got {} body {}",
        created.status,
        created.text()
    );

    // An update naming an authorizer the account does not hold is refused and changes
    // nothing.
    let before = emulator.describe_configuration(name).await.json();
    let refused = emulator
        .update_configuration(
            name,
            &json!({ "authorizerConfig": { "defaultAuthorizerName": "no-such-authorizer" } }),
        )
        .await;
    assert_eq!(
        refused.status,
        400,
        "case an update naming no authorizer of the account: expected 400, got {} body {}",
        refused.status,
        refused.text()
    );
    assert_eq!(
        refused.error_message(),
        "Custom authorizer does not exist, AuthorizerName: no-such-authorizer",
        "case an update naming no authorizer of the account: got {}",
        refused.error_message()
    );
    assert_eq!(
        emulator.describe_configuration(name).await.json(),
        before,
        "case an update naming no authorizer of the account: the record must be unchanged"
    );

    // One naming an authorizer it holds replaces the configuration ...
    let updated = emulator
        .update_configuration(
            name,
            &json!({ "authorizerConfig": { "defaultAuthorizerName": "second-authorizer" } }),
        )
        .await;
    assert_eq!(
        updated.status,
        200,
        "case an update naming a held authorizer: expected 200, got {} body {}",
        updated.status,
        updated.text()
    );
    assert_eq!(
        emulator.describe_configuration(name).await.json()["authorizerConfig"],
        json!({ "defaultAuthorizerName": "second-authorizer" }),
        "case an update naming a held authorizer: the read must report it"
    );

    // ... and `removeAuthorizerConfig` "Removes the authorization configuration from a
    // domain", so the member is reported no more.
    let removed = emulator
        .update_configuration(name, &json!({ "removeAuthorizerConfig": true }))
        .await;
    assert_eq!(
        removed.status,
        200,
        "case removing the authorizer configuration: expected 200, got {} body {}",
        removed.status,
        removed.text()
    );
    let described = emulator.describe_configuration(name).await.json();
    assert_eq!(
        described["authorizerConfig"],
        Value::Null,
        "case removing the authorizer configuration: got {described}"
    );

    // A request that both removes it and supplies one states two outcomes for the same
    // member, so neither silently wins.
    let contradictory = emulator
        .update_configuration(
            name,
            &json!({
                "removeAuthorizerConfig": true,
                "authorizerConfig": { "defaultAuthorizerName": "first-authorizer" },
            }),
        )
        .await;
    assert_eq!(
        contradictory.status,
        400,
        "case removing and setting the authorizer configuration at once: expected 400, got {} \
         body {}",
        contradictory.status,
        contradictory.text()
    );
    assert_eq!(
        emulator.describe_configuration(name).await.json()["authorizerConfig"],
        Value::Null,
        "case removing and setting the authorizer configuration at once: nothing changes"
    );
}

