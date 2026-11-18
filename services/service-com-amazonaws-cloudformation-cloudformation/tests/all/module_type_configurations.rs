// SPDX-License-Identifier: CC0-1.0

//! The CONFIGURATION surface of an extension by CATEGORY, and what a batch read reports for a
//! configuration the registry does not hold — end to end over the awsQuery wire.
//!
//! The rule every case here holds the service to: **`SetTypeConfiguration` resolves the
//! extension the request identifies first — a name or an ARN this account and Region's
//! registry holds no record for is the modelled `TypeNotFoundException` (404) `The type
//! '<TypeName>' cannot be found.` — and then refuses the categories that hold no account-level
//! configuration: a MODULE, which is a package of template fragments with no handlers to hand
//! configuration data to, is the modelled `CFNRegistryException` (400)
//! `TypeConfiguration is not supported for MODULE types`, written before any state is touched,
//! while a resource type and a Hook take the write; and
//! `BatchDescribeTypeConfigurations` reports under `TypeConfigurations` exactly the
//! configurations the registry HOLDS, so an identifier it holds none for — an extension no
//! configuration was ever set for as much as one no record answers to — is one entry under
//! `Errors` with `ErrorCode` `404` and `ErrorMessage` `TypeConfiguration not found`.**
//!
//! The rows differ along what the rule generalises over: the extension CATEGORY the registry
//! holds the record under (`MODULE`, `RESOURCE`, `HOOK`), how the record got there (a private
//! registration of this account, a public extension this account activated), how the request
//! IDENTIFIES it (`TypeArn`, or `Type` and `TypeName`), whether a configuration alias is named,
//! and whether the registry holds a configuration under what the read names.
//!
//! `type_configurations.rs` is the table for the configuration an activated extension takes
//! and reads back; this one is the categories and the misses. `RegisterType` is driven end to
//! end by `extension_registrations.rs`, so the records below are seeded straight into the
//! store — the same device `type_deregistration.rs` and `type_deactivation.rs` use — and every
//! call still goes through the real request path.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use service_com_amazonaws_cloudformation_cloudformation::state::registry::{
    public_type_arn, registry_type, type_configuration_arn, visibility, StoredExtension,
    StoredTypeVersion, DEFAULT_CONFIGURATION_ALIAS,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, member, refusal, router_with_engine, store, xml_blocks, xml_text,
    REGION};

/// The publisher id the activated public records carry, within the modelled `PublisherId`
/// pattern `^[0-9a-zA-Z]{12,40}$`.
const PUBLISHER_ID: &str = "abcdef0123456789abcd";

/// A private MODULE registration. `TypeName`'s pattern admits the trailing `::MODULE` segment
/// a module's name carries.
const PRIVATE_MODULE: &str = "My::Sample::Thing::MODULE";

/// A public MODULE this account ACTIVATED: the other way a module record enters this
/// account and Region's registry.
const ACTIVATED_MODULE: &str = "My::Network::Subnets::MODULE";

/// A private RESOURCE and a private HOOK registration: the categories that DO hold a
/// configuration.
const PRIVATE_RESOURCE: &str = "My::Sample::Resource";
const PRIVATE_HOOK: &str = "My::Sample::Hook";

/// A name no record of any Region's registry carries.
const UNREGISTERED: &str = "My::Unregistered::Resource";

/// The version id every seeded record holds, which is its default version.
const VERSION_ID: &str = "00000001";

/// The configuration a case writes, as a client spells one.
const CONFIGURATION: &str = r#"{"Properties":{"Endpoint":"https://example.invalid/api"}}"#;

/// The refusal a configuration of a category that holds none is answered with: fixed text,
/// naming the category refused.
const NOT_SUPPORTED: &str = "TypeConfiguration is not supported for MODULE types";

/// The error code the registry's own modelled refusal carries on the wire
/// (`CFNRegistryException`'s `awsQueryError`), and the one a name no record answers to does.
const REGISTRY_REFUSAL_CODE: &str = "CFNRegistryException";
const NOT_FOUND_CODE: &str = "TypeNotFoundException";

/// The code and the message this batch reports for an identifier whose configuration the
/// registry does not hold.
const BATCH_NOT_FOUND: (&str, &str) = ("404", "TypeConfiguration not found");

/// An instant to register the seeded versions at, so nothing a case asserts comes from the
/// clock.
fn registered_at() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
}

/// One private registration of this account and Region, holding one version.
fn registration(type_kind: &str, type_name: &str) -> StoredExtension {
    let mut extension = StoredExtension::new(REGION, type_kind, type_name);
    let version = StoredTypeVersion::new(&extension.arn, VERSION_ID, registered_at());
    extension.default_version_id = Some(VERSION_ID.to_string());
    extension.versions = vec![version];
    extension
}

/// A public extension this account ACTIVATED, held under its own name.
fn activation(type_kind: &str, type_name: &str) -> StoredExtension {
    let mut extension = registration(type_kind, type_name);
    extension.visibility = visibility::PUBLIC.to_string();
    extension.default_version_id = None;
    extension.publisher_id = Some(PUBLISHER_ID.to_string());
    extension.original_type_name = Some(type_name.to_string());
    extension.original_type_arn = Some(public_type_arn(REGION, type_kind, type_name));
    extension.is_activated = Some(true);
    extension
}

/// A router over a store holding one record of each category.
fn router_with_records() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let (router, engine) = router_with_engine();
    {
        let state = store(&engine);
        state.lock().extensions = vec![
            registration(registry_type::MODULE, PRIVATE_MODULE),
            registration(registry_type::RESOURCE, PRIVATE_RESOURCE),
            registration(registry_type::HOOK, PRIVATE_HOOK),
            activation(registry_type::MODULE, ACTIVATED_MODULE),
        ];
    }
    (router, engine)
}

/// The ARN this account and Region's registry holds a record at.
fn record_arn(engine: &Arc<engine::InMemoryEngine>, type_name: &str) -> String {
    let state = store(engine);
    let locked = state.lock();
    locked
        .extensions
        .iter()
        .find(|extension| extension.type_name == type_name)
        .map(|extension| extension.arn.clone())
        .unwrap_or_else(|| panic!("the seeded registry holds a record for {type_name}"))
}

/// Every configuration the store holds right now, as `(configuration ARN, document)`: what a
/// case checks a refused write wrote nothing into.
fn held_configurations(engine: &Arc<engine::InMemoryEngine>) -> Vec<(String, String)> {
    let state = store(engine);
    let locked = state.lock();
    locked
        .extensions
        .iter()
        .flat_map(|extension| {
            extension
                .configurations
                .iter()
                .map(|held| (held.arn.clone(), held.configuration.clone()))
        })
        .collect()
}

/// How a request identifies the extension whose configuration it sets or reads.
#[derive(Clone, Copy, Debug)]
enum By {
    /// `TypeArn`, "the Amazon Resource Name (ARN) for the extension in this account and
    /// Region".
    Arn,
    /// `Type` and `TypeName`.
    Name,
}

/// The members one `SetTypeConfiguration` request carries for a record, identified `by`.
fn configuration_members(
    engine: &Arc<engine::InMemoryEngine>,
    by: By,
    type_kind: &str,
    type_name: &str,
    alias: Option<&str>,
) -> Vec<(String, String)> {
    let mut members = vec![member("Configuration", CONFIGURATION)];
    match by {
        By::Arn => members.push(member("TypeArn", &record_arn(engine, type_name))),
        By::Name => {
            members.push(member("Type", type_kind));
            members.push(member("TypeName", type_name));
        }
    }
    if let Some(alias) = alias {
        members.push(member("ConfigurationAlias", alias));
    }
    members
}

#[tokio::test]
async fn a_module_holds_no_configuration_and_the_write_is_refused() {
    let (router, engine) = router_with_records();

    // The rows: both module records the registry holds — a private registration and this
    // account's activation of a public module — identified both ways a request can identify
    // one, with and without a configuration alias.
    for (case, by, type_name, alias) in [
        ("a private module by its ARN", By::Arn, PRIVATE_MODULE, None),
        (
            "a private module by kind and name",
            By::Name,
            PRIVATE_MODULE,
            None,
        ),
        (
            "a private module under an alias of its own",
            By::Name,
            PRIVATE_MODULE,
            Some("Alias2"),
        ),
        (
            "an activated public module by its ARN",
            By::Arn,
            ACTIVATED_MODULE,
            None,
        ),
        (
            "an activated public module by kind and name",
            By::Name,
            ACTIVATED_MODULE,
            Some(DEFAULT_CONFIGURATION_ALIAS),
        ),
    ] {
        let members = configuration_members(
            &engine,
            by,
            registry_type::MODULE,
            type_name,
            alias,
        );
        let (status, body) = call(&router, "SetTypeConfiguration", &members).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the configuration of a module refused with 400, got \
             {status} {body}"
        );
        let (code, message) = refusal(&body);
        assert_eq!(
            (code.as_str(), message.as_str()),
            (REGISTRY_REFUSAL_CODE, NOT_SUPPORTED),
            "case {case:?}: expected the registry's own refusal {NOT_SUPPORTED:?}, got {body}"
        );
        assert_eq!(
            xml_text(&body, "ConfigurationArn"),
            None,
            "case {case:?}: expected no configuration ARN in the refusal, got {body}"
        );
        // Nothing was written: the refusal is answered before any state is touched, so no
        // configuration exists against any record.
        let held = held_configurations(&engine);
        assert!(
            held.is_empty(),
            "case {case:?}: expected a refused write to persist no configuration, got {held:?}"
        );
    }
}

#[tokio::test]
async fn the_categories_that_hold_a_configuration_still_take_the_write() {
    let (router, engine) = router_with_records();

    // The positive control the category check must not swallow, and the discriminator ahead
    // of it: a record of a category that holds a configuration takes the write, and a name no
    // record answers to is the registry's not-found rather than the category refusal.
    for (case, type_kind, type_name, alias) in [
        (
            "a private resource type under the default alias",
            registry_type::RESOURCE,
            PRIVATE_RESOURCE,
            None,
        ),
        (
            "a private Hook under an alias of its own",
            registry_type::HOOK,
            PRIVATE_HOOK,
            Some("Production"),
        ),
    ] {
        let members =
            configuration_members(&engine, By::Name, type_kind, type_name, alias);
        let (status, body) = call(&router, "SetTypeConfiguration", &members).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the configuration accepted, got {status} {body}"
        );
        let expected = type_configuration_arn(
            REGION,
            type_kind,
            type_name,
            alias.unwrap_or(DEFAULT_CONFIGURATION_ALIAS),
        );
        assert_eq!(
            xml_text(&body, "ConfigurationArn").as_deref(),
            Some(expected.as_str()),
            "case {case:?}: expected the ARN of the configuration data, got {body}"
        );
    }

    // The discriminator: an unregistered name is refused as not found, whatever category the
    // request names it under — including MODULE, which resolves to no record at all.
    for (case, type_kind) in [
        ("a resource type no registry holds", registry_type::RESOURCE),
        ("a module no registry holds", registry_type::MODULE),
    ] {
        let (status, body) = call(
            &router,
            "SetTypeConfiguration",
            &[
                member("Type", type_kind),
                member("TypeName", UNREGISTERED),
                member("Configuration", CONFIGURATION),
            ],
        )
        .await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected an unregistered extension refused with 404, got {status} \
             {body}"
        );
        let (code, message) = refusal(&body);
        assert_eq!(
            (code.as_str(), message.as_str()),
            (
                NOT_FOUND_CODE,
                format!("The type '{UNREGISTERED}' cannot be found.").as_str()
            ),
            "case {case:?}: expected the registry's not-found refusal, got {body}"
        );
    }
}

/// One `BatchDescribeTypeConfigurations` read, by one identifier.
async fn batch_read(
    router: &MultiServiceRouter,
    identifier: &[(String, String)],
) -> (u16, String) {
    call(router, "BatchDescribeTypeConfigurations", identifier).await
}

/// The members of one identifier of a batch read.
fn identifier(key: &str, value: &str) -> (String, String) {
    member(&format!("TypeConfigurationIdentifiers.member.1.{key}"), value)
}

/// What the batch answered: the configurations it reported, and its error entries.
fn reported(body: &str) -> (Vec<String>, Vec<String>) {
    let configurations = xml_blocks(body, "TypeConfigurations")
        .into_iter()
        .next()
        .unwrap_or_default();
    let errors = xml_blocks(body, "Errors").into_iter().next().unwrap_or_default();
    (
        xml_blocks(&configurations, "member"),
        xml_blocks(&errors, "member"),
    )
}

#[tokio::test]
async fn a_configuration_the_registry_does_not_hold_is_an_error_entry_not_an_empty_one() {
    let (router, engine) = router_with_records();

    // The rows: every record the registry holds, none of which has been configured, named by
    // each identifier form — and one identifier no record answers to at all, which is the
    // same miss reported the same way.
    let unknown_configuration = type_configuration_arn(
        REGION,
        registry_type::RESOURCE,
        UNREGISTERED,
        DEFAULT_CONFIGURATION_ALIAS,
    );
    let rows: Vec<(&str, Vec<(String, String)>)> = vec![
        (
            "a private resource type by its ARN",
            vec![identifier("TypeArn", &record_arn(&engine, PRIVATE_RESOURCE))],
        ),
        (
            "a private Hook by kind and name",
            vec![
                identifier("Type", registry_type::HOOK),
                identifier("TypeName", PRIVATE_HOOK),
            ],
        ),
        (
            "a private module by its ARN",
            vec![identifier("TypeArn", &record_arn(&engine, PRIVATE_MODULE))],
        ),
        (
            "a private resource type under an alias",
            vec![
                identifier("TypeArn", &record_arn(&engine, PRIVATE_RESOURCE)),
                identifier("TypeConfigurationAlias", "Production"),
            ],
        ),
        (
            "a configuration ARN no record answers to",
            vec![identifier("TypeConfigurationArn", &unknown_configuration)],
        ),
        (
            "a name no registry holds",
            vec![
                identifier("Type", registry_type::RESOURCE),
                identifier("TypeName", UNREGISTERED),
            ],
        ),
    ];

    for (case, members) in &rows {
        let (status, body) = batch_read(&router, members).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the batch to succeed, got {status} {body}"
        );
        let (configurations, errors) = reported(&body);
        assert!(
            configurations.is_empty(),
            "case {case:?}: expected no configuration reported for one the registry does not \
             hold, got {configurations:?}"
        );
        assert_eq!(
            errors.len(),
            1,
            "case {case:?}: expected exactly one error entry, got {body}"
        );
        let entry = errors.first().cloned().unwrap_or_default();
        assert_eq!(
            (
                xml_text(&entry, "ErrorCode").unwrap_or_default().as_str(),
                xml_text(&entry, "ErrorMessage").unwrap_or_default().as_str(),
            ),
            BATCH_NOT_FOUND,
            "case {case:?}: expected the registry's own wording for a configuration it does \
             not hold, got {entry:?}"
        );
    }
}

#[tokio::test]
async fn the_batch_reports_the_alias_that_was_written_and_misses_the_ones_that_were_not() {
    let (router, engine) = router_with_records();
    let type_arn = record_arn(&engine, PRIVATE_RESOURCE);

    // One configuration, written under one alias: the read under THAT alias reports it, and
    // the read under any other is still the batch's error entry — which is how a client tells
    // a configuration it set from one it never did.
    let (status, body) = call(
        &router,
        "SetTypeConfiguration",
        &[
            member("TypeArn", &type_arn),
            member("ConfigurationAlias", "Production"),
            member("Configuration", CONFIGURATION),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected the configuration accepted, got {status} {body}"
    );
    let configuration_arn = xml_text(&body, "ConfigurationArn").unwrap_or_default();

    for (case, members, holds_it) in [
        (
            "the alias the configuration was written under",
            vec![
                identifier("TypeArn", &type_arn),
                identifier("TypeConfigurationAlias", "Production"),
            ],
            true,
        ),
        (
            "the configuration's own ARN",
            vec![identifier("TypeConfigurationArn", &configuration_arn)],
            true,
        ),
        (
            "the default alias, which was never written",
            vec![identifier("TypeArn", &type_arn)],
            false,
        ),
        (
            "another alias of the same extension",
            vec![
                identifier("TypeArn", &type_arn),
                identifier("TypeConfigurationAlias", "Staging"),
            ],
            false,
        ),
    ] {
        let (status, body) = batch_read(&router, &members).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the batch to succeed, got {status} {body}"
        );
        let (configurations, errors) = reported(&body);
        if holds_it {
            assert_eq!(
                configurations.len(),
                1,
                "case {case:?}: expected the configuration that was written, got {body}"
            );
            assert!(
                errors.is_empty(),
                "case {case:?}: expected no error entry, got {errors:?}"
            );
            let detail = configurations.first().cloned().unwrap_or_default();
            assert_eq!(
                xml_text(&detail, "Alias").as_deref(),
                Some("Production"),
                "case {case:?}: expected the alias it was set under, got {detail:?}"
            );
            assert_eq!(
                xml_text(&detail, "Arn").as_deref(),
                Some(configuration_arn.as_str()),
                "case {case:?}: expected the configuration's own ARN, got {detail:?}"
            );
            assert_eq!(
                xml_text(&detail, "IsDefaultConfiguration").as_deref(),
                Some("false"),
                "case {case:?}: expected a configuration set under an alias of its own, got \
                 {detail:?}"
            );
            assert!(
                xml_text(&detail, "LastUpdated").is_some(),
                "case {case:?}: expected the instant of the write, got {detail:?}"
            );
        } else {
            assert!(
                configurations.is_empty(),
                "case {case:?}: expected no configuration under an alias none was written \
                 under, got {configurations:?}"
            );
            assert_eq!(
                errors.len(),
                1,
                "case {case:?}: expected exactly one error entry, got {body}"
            );
            let entry = errors.first().cloned().unwrap_or_default();
            assert_eq!(
                (
                    xml_text(&entry, "ErrorCode").unwrap_or_default().as_str(),
                    xml_text(&entry, "ErrorMessage").unwrap_or_default().as_str(),
                ),
                BATCH_NOT_FOUND,
                "case {case:?}: expected the registry's own wording for a configuration it \
                 does not hold, got {entry:?}"
            );
        }
    }
}
