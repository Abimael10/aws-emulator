// SPDX-License-Identifier: CC0-1.0

//! Two rules the registry holds every ACTIVATED public extension to, end to end over the
//! awsQuery wire.
//!
//! 1. **Activating a public RESOURCE extension requires an execution role: a request that
//!    names no `ExecutionRoleArn` is refused with the registry's own modelled
//!    `CFNRegistryException` (400) carrying the execution-role sentence and writes nothing,
//!    while a MODULE and a Hook — which declare no handlers CloudFormation invokes on the
//!    activating account's behalf — activate without one.**
//!
//! 2. **The registry holds an extension's configuration document as ASCII: every codepoint
//!    outside ASCII reads back replaced by one `?`, and nothing else about the document
//!    changes — every delimiter, member name and nesting level survives, so the document a
//!    `BatchDescribeTypeConfigurations` reports parses to the same tree the write submitted
//!    modulo that substitution.**
//!
//! 3. **A registry read reports an extension's `ConfigurationSchema` — the document declaring
//!    the account-level configuration its publisher gave it — for every extension that takes
//!    one and for none that does not, before anything has been activated or configured; so a
//!    client that discovers the public registry, reads that document and builds a payload
//!    from it can activate the extension and set and read back its configuration, naming the
//!    extension by any ARN the registry handed it for it.**
//!
//! The tables span what the rules generalise over: for the first, each kind the modelled
//! `ThirdPartyType` admits, both ways a request can name the public extension
//! (`PublicTypeArn`, or `Type` + `TypeName` + `PublisherId`), and with and without an alias;
//! for the second, nesting depth, every JSON value kind, and codepoints on both sides of the
//! ASCII boundary — the last ASCII codepoint, a Latin-1 letter, a two-byte, a three-byte and
//! a four-byte encoding — each inside a value, inside a member NAME and inside a nested
//! array; for the third, every extension the public registry publishes (read from
//! `crate::state::registry::PUBLIC_EXTENSION_CATALOG`, so a catalogue entry added later is
//! covered without editing the suite) — which spans both configurable kinds, two publishers,
//! and configurations of different depth and property count — beside the extensions Amazon
//! Web Services publishes for its own resource types, which take no account-level
//! configuration at all.
//!
//! `type_configurations.rs` is the suite for what an activation and a configuration WRITE and
//! read back; this one is the two rules above.

use crate::stack_lifecycle::{call, member, router, xml_blocks, xml_text, ACCOUNT_ID, REGION};
use service_com_amazonaws_cloudformation_cloudformation::state::registry;
use smithy_server_runtime::MultiServiceRouter;

/// The publisher id a public third-party extension is named by, within the modelled
/// `PublisherId` pattern `^[0-9a-zA-Z]{12,40}$`.
const PUBLISHER_ID: &str = "abcdef0123456789abcd";

/// The execution role an activation names, inside the modelled `RoleARN2` pattern
/// `^arn:.+:iam::[0-9]{12}:role/.+$`.
const EXECUTION_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/extension-execution";

/// The refusal AWS answers an activation of a public RESOURCE extension that names no
/// execution role. Entirely fixed text: no part of it comes from the request or the store.
const NO_EXECUTION_ROLE: &str = "Please provide execution role to activate the Resource type";

/// The ARN of a public extension, as the modelled `ThirdPartyTypeArn` pattern spells one:
/// `arn:aws:cloudformation:<region>::type/<kind>/<publisher id>/<Type-Name>`.
fn public_type_arn(kind: &str, type_name: &str) -> String {
    format!(
        "arn:aws:cloudformation:{REGION}::type/{}/{PUBLISHER_ID}/{}",
        kind.to_ascii_lowercase(),
        type_name.replace("::", "-")
    )
}

/// The ARN this account addresses an activation by: `type/<kind>/<Local-Name>` in this
/// account and Region.
fn activated_arn(kind: &str, local_name: &str) -> String {
    format!(
        "arn:aws:cloudformation:{REGION}:{ACCOUNT_ID}:type/{}/{}",
        kind.to_ascii_lowercase(),
        local_name.replace("::", "-")
    )
}

/// One row of the execution-role table.
struct ActivationCase {
    case: &'static str,
    /// The kinds the modelled `ThirdPartyType` admits: `HOOK | MODULE | RESOURCE`
    /// (`smithy_query(query="shape", shape="ThirdPartyType")`).
    kind: &'static str,
    type_name: &'static str,
    /// The alias the activation gives it, if any.
    alias: Option<&'static str>,
    /// Whether the request names the public extension by its ARN.
    by_public_arn: bool,
    /// Whether an activation of this kind needs an execution role.
    needs_execution_role: bool,
}

fn activation_cases() -> Vec<ActivationCase> {
    vec![
        ActivationCase {
            case: "a resource type named by kind, name and publisher",
            kind: registry::registry_type::RESOURCE,
            type_name: "Example::Store::Bucket",
            alias: None,
            by_public_arn: false,
            needs_execution_role: true,
        },
        ActivationCase {
            case: "a resource type named by its public ARN, under an alias",
            kind: registry::registry_type::RESOURCE,
            type_name: "Example::Other::Table",
            alias: Some("Local::Other::Table"),
            by_public_arn: true,
            needs_execution_role: true,
        },
        ActivationCase {
            case: "a module, which declares no handlers",
            kind: registry::registry_type::MODULE,
            type_name: "Example::Network::Subnets::MODULE",
            alias: None,
            by_public_arn: true,
            needs_execution_role: false,
        },
        ActivationCase {
            case: "a Hook, invoked on the stack operation's own credentials",
            kind: registry::registry_type::HOOK,
            type_name: "Example::Guard::Hook",
            alias: Some("Local::Guard::Hook"),
            by_public_arn: false,
            needs_execution_role: false,
        },
    ]
}

/// The members one row's `ActivateType` request carries, without the execution role.
fn activation_members(case: &ActivationCase) -> Vec<(String, String)> {
    let mut members = Vec::new();
    if case.by_public_arn {
        members.push(member(
            "PublicTypeArn",
            &public_type_arn(case.kind, case.type_name),
        ));
    } else {
        members.push(member("Type", case.kind));
        members.push(member("TypeName", case.type_name));
        members.push(member("PublisherId", PUBLISHER_ID));
    }
    if let Some(alias) = case.alias {
        members.push(member("TypeNameAlias", alias));
    }
    members
}

#[tokio::test]
async fn an_activation_needs_an_execution_role_exactly_where_handlers_are_invoked() {
    for case in activation_cases() {
        // A fresh account per row, so each activation is the first one its registry saw.
        let router = router();
        let local_name = case.alias.unwrap_or(case.type_name);
        let expected_arn = activated_arn(case.kind, local_name);

        // Without the role.
        let members = activation_members(&case);
        let (status_code, body) = call(&router, "ActivateType", &members).await;
        if case.needs_execution_role {
            assert_eq!(
                status_code, 400,
                "case {:?}: expected the activation refused with 400, got {status_code} {body}",
                case.case
            );
            assert_eq!(
                xml_text(&body, "Code").as_deref(),
                Some("CFNRegistryException"),
                "case {:?}: expected CFNRegistryException, got {body}",
                case.case
            );
            assert_eq!(
                xml_text(&body, "Message").as_deref(),
                Some(NO_EXECUTION_ROLE),
                "case {:?}: expected {NO_EXECUTION_ROLE:?}, got {body}",
                case.case
            );
            assert_eq!(
                xml_text(&body, "Arn"),
                None,
                "case {:?}: expected no ARN in the refusal, got {body}",
                case.case
            );
            // Nothing was written: the extension is not in this account's registry.
            let (status_code, described) =
                call(&router, "DescribeType", &[member("Arn", &expected_arn)]).await;
            assert_eq!(
                status_code, 404,
                "case {:?}: expected the refused activation to write nothing, got \
                 {status_code} {described}",
                case.case
            );
        } else {
            assert_eq!(
                status_code, 200,
                "case {:?}: expected a kind that needs no execution role to activate, got \
                 {status_code} {body}",
                case.case
            );
            assert_eq!(
                xml_text(&body, "Arn").as_deref(),
                Some(expected_arn.as_str()),
                "case {:?}: expected the activation's own ARN, got {body}",
                case.case
            );
        }

        // With the role, every kind activates, and the activation is readable at the ARN it
        // answered.
        let mut with_role = activation_members(&case);
        with_role.push(member("ExecutionRoleArn", EXECUTION_ROLE_ARN));
        let (status_code, body) = call(&router, "ActivateType", &with_role).await;
        assert_eq!(
            status_code, 200,
            "case {:?}: expected the activation to succeed, got {status_code} {body}",
            case.case
        );
        assert_eq!(
            xml_text(&body, "Arn").as_deref(),
            Some(expected_arn.as_str()),
            "case {:?}: expected the activation's own ARN, got {body}",
            case.case
        );
        let (status_code, described) =
            call(&router, "DescribeType", &[member("Arn", &expected_arn)]).await;
        assert_eq!(
            status_code, 200,
            "case {:?}: expected the activation readable, got {status_code} {described}",
            case.case
        );
        assert_eq!(
            xml_text(&described, "IsActivated").as_deref(),
            Some("true"),
            "case {:?}: expected it reported activated, got {described}",
            case.case
        );
        assert_eq!(
            xml_text(&described, "TypeName").as_deref(),
            Some(local_name),
            "case {:?}: expected it held under its local name, got {described}",
            case.case
        );
    }
}

/// One row of the configuration round-trip table: a document to set, and what the registry
/// must report back for it.
struct ConfigurationCase {
    case: &'static str,
    /// What the client sends.
    sent: &'static str,
    /// What a read must report: the same document with every codepoint outside ASCII
    /// replaced by one `?`.
    expected: &'static str,
}

fn configuration_cases() -> Vec<ConfigurationCase> {
    vec![
        ConfigurationCase {
            case: "a flat, wholly ASCII document",
            sent: r#"{"ApiKey":"abc-123"}"#,
            expected: r#"{"ApiKey":"abc-123"}"#,
        },
        ConfigurationCase {
            case: "a nested document carrying every JSON value kind",
            sent: r#"{"Credentials":{"ApiKey":"k","Retries":3,"Ratio":0.5,"Enabled":true,"Absent":null,"Tags":["a","b"],"Nested":{"Deep":{"Deeper":"end"}}}}"#,
            expected: r#"{"Credentials":{"ApiKey":"k","Retries":3,"Ratio":0.5,"Enabled":true,"Absent":null,"Tags":["a","b"],"Nested":{"Deep":{"Deeper":"end"}}}}"#,
        },
        ConfigurationCase {
            case: "the last ASCII codepoint, which is held as it is",
            sent: "{\"Separator\":\"a\u{7f}b\"}",
            expected: "{\"Separator\":\"a\u{7f}b\"}",
        },
        ConfigurationCase {
            case: "a Latin-1 letter, the first codepoint past ASCII",
            sent: r#"{"Owner":"José"}"#,
            expected: r#"{"Owner":"Jos?"}"#,
        },
        ConfigurationCase {
            case: "a two-byte encoding outside Latin-1",
            sent: r#"{"Label":"Привет"}"#,
            expected: r#"{"Label":"??????"}"#,
        },
        ConfigurationCase {
            case: "a three-byte encoding inside a nested array",
            sent: r#"{"Labels":{"Names":["東京","Tokyo"]}}"#,
            expected: r#"{"Labels":{"Names":["??","Tokyo"]}}"#,
        },
        ConfigurationCase {
            case: "a four-byte encoding, one codepoint and so one question mark",
            sent: r#"{"Mood":"🙂"}"#,
            expected: r#"{"Mood":"?"}"#,
        },
        ConfigurationCase {
            case: "a non-ASCII member NAME, so the structure survives too",
            sent: r#"{"Région":{"Nom":"eu-ouest"}}"#,
            expected: r#"{"R?gion":{"Nom":"eu-ouest"}}"#,
        },
    ]
}

/// The `TypeConfigurationDetails` member `name` one read reported.
fn detail(body: &str, name: &str) -> Option<String> {
    let block = xml_blocks(body, "TypeConfigurations").into_iter().next()?;
    xml_text(&block, name)
}

/// Activate the Hook every configuration row is set on, and answer the ARN this account
/// addresses it by.
async fn activate_a_hook(router: &MultiServiceRouter) -> String {
    let (status_code, body) = call(
        router,
        "ActivateType",
        &[
            member("Type", registry::registry_type::HOOK),
            member("TypeName", "Example::Guard::Hook"),
            member("PublisherId", PUBLISHER_ID),
        ],
    )
    .await;
    assert_eq!(
        status_code, 200,
        "case \"activating the extension to configure\": expected 200, got {status_code} {body}"
    );
    xml_text(&body, "Arn").expect("the activation answers an ARN")
}

#[tokio::test]
async fn a_stored_configuration_is_held_as_ascii_with_its_structure_intact() {
    for ConfigurationCase {
        case,
        sent,
        expected,
    } in configuration_cases()
    {
        // A fresh account per row, so each document is the first one its extension was
        // configured with.
        let router = router();
        let activated = activate_a_hook(&router).await;

        let (status_code, body) = call(
            &router,
            "SetTypeConfiguration",
            &[
                member("TypeArn", &activated),
                member("Configuration", sent),
            ],
        )
        .await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected SetTypeConfiguration to succeed, got {status_code} {body}"
        );
        let configuration_arn = xml_text(&body, "ConfigurationArn").unwrap_or_default();

        let (status_code, body) = call(
            &router,
            "BatchDescribeTypeConfigurations",
            &[member(
                "TypeConfigurationIdentifiers.member.1.TypeConfigurationArn",
                &configuration_arn,
            )],
        )
        .await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected the batch read to answer, got {status_code} {body}"
        );
        let reported = detail(&body, "Configuration");
        assert_eq!(
            reported.as_deref(),
            Some(expected),
            "case {case:?}: expected {expected:?}, got {reported:?}"
        );
        let reported = reported.unwrap_or_default();
        assert!(
            reported.is_ascii(),
            "case {case:?}: expected the stored document to be ASCII, got {reported:?}"
        );
        // Structure intact: the document still parses, and to the same tree the expected
        // text does.
        let read: ::serde_json::Value =
            ::serde_json::from_str(&reported).unwrap_or_else(|error| {
                panic!("case {case:?}: expected JSON, got {reported:?} ({error})")
            });
        let wanted: ::serde_json::Value = ::serde_json::from_str(expected)
            .unwrap_or_else(|error| panic!("case {case:?}: {expected:?} is not JSON ({error})"));
        assert_eq!(
            read, wanted,
            "case {case:?}: expected the document's structure intact, got {read:?}"
        );
        // One `?` per CODEPOINT outside ASCII, not per byte: the document keeps its length
        // in characters.
        assert_eq!(
            reported.chars().count(),
            sent.chars().count(),
            "case {case:?}: expected one character per submitted codepoint, got {reported:?}"
        );
        assert_eq!(
            detail(&body, "Alias").as_deref(),
            Some(registry::DEFAULT_CONFIGURATION_ALIAS),
            "case {case:?}: expected the default alias, got {body}"
        );
        assert_eq!(
            detail(&body, "TypeArn").as_deref(),
            Some(activated.as_str()),
            "case {case:?}: expected the activation's ARN, got {body}"
        );
    }
}

/// Setting a configuration again under the same alias replaces it, and the replacement is
/// held as ASCII too: the coercion is a property of the write, not of the first one.
#[tokio::test]
async fn a_replacement_configuration_is_coerced_as_well() {
    let router = router();
    let activated = activate_a_hook(&router).await;

    for (case, sent, expected) in [
        ("the first write", r#"{"Owner":"ascii"}"#, r#"{"Owner":"ascii"}"#),
        (
            "the replacement",
            r#"{"Owner":"naïve","Extra":"東"}"#,
            r#"{"Owner":"na?ve","Extra":"?"}"#,
        ),
    ] {
        let (status_code, body) = call(
            &router,
            "SetTypeConfiguration",
            &[
                member("TypeArn", &activated),
                member("Configuration", sent),
            ],
        )
        .await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected the write to succeed, got {status_code} {body}"
        );
        let configuration_arn = xml_text(&body, "ConfigurationArn").unwrap_or_default();
        let (_, body) = call(
            &router,
            "BatchDescribeTypeConfigurations",
            &[member(
                "TypeConfigurationIdentifiers.member.1.TypeConfigurationArn",
                &configuration_arn,
            )],
        )
        .await;
        assert_eq!(
            detail(&body, "Configuration").as_deref(),
            Some(expected),
            "case {case:?}: expected {expected:?}, got {body}"
        );
    }
}

/// The category a client narrows the public registry to when it looks for extensions
/// published by somebody other than Amazon: `Filters$Category` `THIRD_PARTY`, "extensions
/// available for use from publishers other than Amazon"
/// (`smithy_query(query="shape", shape="Category")`).
const THIRD_PARTY: &str = "THIRD_PARTY";

/// The alias the configuration of a discovered extension is set under, inside the modelled
/// `TypeConfigurationAlias` pattern `^[a-zA-Z0-9]{1,256}$`.
const DISCOVERY_ALIAS: &str = "Discovery";

/// The upper bound the model puts on the document `ConfigurationSchema` carries
/// (`@length(1..60000)`), whose lower bound makes an empty document no document at all.
const MAX_CONFIGURATION_SCHEMA: usize = 60_000;

/// Whether the configuration schema `document` declares the property PATH `path`: `A.B` is
/// the `B` of `A`, which a JSON schema spells as a `properties` member inside a `properties`
/// member.
fn declares(document: &::serde_json::Value, path: &str) -> bool {
    let mut node = document;
    for segment in path.split('.') {
        let declared = node
            .get("properties")
            .and_then(::serde_json::Value::as_object)
            .and_then(|properties| properties.get(segment));
        match declared {
            Some(next) => node = next,
            None => return false,
        }
    }
    true
}

/// The configuration payload a client builds from the `ConfigurationSchema` it read: one
/// string per leaf property the document declares, nested exactly as the document nests
/// them — which is what makes the write below derived from the registry's own answer rather
/// than written out per row.
fn payload_for(name: &str, document: &::serde_json::Value) -> ::serde_json::Value {
    match document
        .get("properties")
        .and_then(::serde_json::Value::as_object)
    {
        None => ::serde_json::Value::String(format!("{name}-set-by-the-client")),
        Some(properties) => ::serde_json::Value::Object(
            properties
                .iter()
                .map(|(member, declared)| (member.clone(), payload_for(member, declared)))
                .collect(),
        ),
    }
}

/// `DescribeType` for a read that must succeed, as the response body.
async fn described(case: &str, router: &MultiServiceRouter, members: &[(String, String)]) -> String {
    let (status_code, body) = call(router, "DescribeType", members).await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: expected the extension readable, got {status_code} {body}"
    );
    body
}

#[tokio::test]
async fn the_public_registry_publishes_the_configuration_schema_of_each_configurable_extension() {
    let router = router();
    for published in registry::PUBLIC_EXTENSION_CATALOG {
        let case = published.type_name;
        let published_arn = registry::published_catalog_arn(REGION, published);

        // Discovery: the public registry reports the extension under the category a client
        // filters a third party's extensions by, with the ARN it hands the detailed read.
        let (status_code, body) = call(
            &router,
            "ListTypes",
            &[
                member("Visibility", registry::visibility::PUBLIC),
                member("Type", published.type_kind),
                member("Filters.Category", THIRD_PARTY),
                member("Filters.TypeNamePrefix", published.type_name),
            ],
        )
        .await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected the public listing to answer, got {status_code} {body}"
        );
        let summaries = xml_blocks(&body, "member");
        assert_eq!(
            summaries.len(),
            1,
            "case {case:?}: expected exactly one summary for the extension, got {summaries:?}"
        );
        assert_eq!(
            xml_text(&summaries[0], "TypeArn").as_deref(),
            Some(published_arn.as_str()),
            "case {case:?}: expected the published extension's own ARN, got {:?}",
            summaries[0]
        );

        // The read of that ARN carries the document the extension's configuration is set
        // against — nothing here has activated or configured it.
        let body = described(case, &router, &[member("Arn", &published_arn)]).await;
        assert_eq!(
            xml_text(&body, "IsActivated").as_deref(),
            Some("false"),
            "case {case:?}: expected an extension nothing activated, got {body}"
        );
        let Some(schema) = xml_text(&body, "ConfigurationSchema") else {
            panic!("case {case:?}: expected a ConfigurationSchema, got {body}");
        };
        assert!(
            !schema.is_empty() && schema.chars().count() <= MAX_CONFIGURATION_SCHEMA,
            "case {case:?}: expected a document inside the modelled length 1..{MAX_CONFIGURATION_SCHEMA}, \
             got {} characters: {schema:?}",
            schema.chars().count()
        );
        let document: ::serde_json::Value = ::serde_json::from_str(&schema)
            .unwrap_or_else(|error| panic!("case {case:?}: expected JSON, got {schema:?} ({error})"));
        assert_eq!(
            document.get("typeName").and_then(::serde_json::Value::as_str),
            Some(published.type_name),
            "case {case:?}: expected the document to name the extension it configures, got \
             {schema:?}"
        );
        for path in published.configuration_properties {
            assert!(
                declares(&document, path),
                "case {case:?}: expected the document to declare {path:?}, got {schema:?}"
            );
        }
    }

    // The other side of the rule: Amazon Web Services's own published resource types take no
    // account-level configuration, so a read of one reports no document rather than an empty
    // one. Taken from the registry's own catalogue of published resource types
    // (`state::registry::aws_published_resource_types`) rather than named here.
    for type_name in registry::aws_published_resource_types().take(3) {
        let body = described(
            type_name,
            &router,
            &[
                member("Type", registry::registry_type::RESOURCE),
                member("TypeName", type_name),
            ],
        )
        .await;
        assert_eq!(
            xml_text(&body, "ConfigurationSchema"),
            None,
            "case {type_name:?}: expected no configuration document for a type that takes \
             none, got {body}"
        );
    }
}

#[tokio::test]
async fn a_discovered_extension_is_activated_and_configured_against_its_published_schema() {
    for published in registry::PUBLIC_EXTENSION_CATALOG {
        let case = published.type_name;
        // A fresh account per row, so each activation and configuration is the first one its
        // registry saw.
        let router = router();
        let published_arn = registry::published_catalog_arn(REGION, published);

        // What a client discovering the registry has in hand: the published document, and the
        // payload it builds from it.
        let body = described(case, &router, &[member("Arn", &published_arn)]).await;
        let schema = xml_text(&body, "ConfigurationSchema")
            .unwrap_or_else(|| panic!("case {case:?}: expected a ConfigurationSchema, got {body}"));
        let document: ::serde_json::Value = ::serde_json::from_str(&schema)
            .unwrap_or_else(|error| panic!("case {case:?}: expected JSON, got {schema:?} ({error})"));
        let payload = payload_for(published.type_name, &document).to_string();

        // The activation, naming the public extension the way its ARN allows: `PublicTypeArn`
        // is modelled `^arn:aws…:cloudformation:…::type/.+/[0-9a-zA-Z]{12,40}/.+$`, so it
        // carries the publisher's id — an extension Amazon Web Services published is assigned
        // none, and is named by its kind, its name and the publisher instead, the other form
        // `ActivateType` documents. A RESOURCE extension's handlers are invoked on this
        // account's behalf, so that one names an execution role.
        let publisher_id = registry::published_publisher_id(published.publisher_name);
        let mut members = if published_arn.contains(&publisher_id) {
            vec![member("PublicTypeArn", &published_arn)]
        } else {
            vec![
                member("Type", published.type_kind),
                member("TypeName", published.type_name),
                member("PublisherId", &publisher_id),
            ]
        };
        if published.type_kind == registry::registry_type::RESOURCE {
            members.push(member("ExecutionRoleArn", EXECUTION_ROLE_ARN));
        }
        let (status_code, body) = call(&router, "ActivateType", &members).await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected the activation accepted, got {status_code} {body}"
        );
        let expected_arn = activated_arn(published.type_kind, published.type_name);
        assert_eq!(
            xml_text(&body, "Arn").as_deref(),
            Some(expected_arn.as_str()),
            "case {case:?}: expected the activation's own ARN, got {body}"
        );

        // This account's copy is configured against the same document.
        let body = described(case, &router, &[member("Arn", &expected_arn)]).await;
        assert_eq!(
            xml_text(&body, "ConfigurationSchema").as_deref(),
            Some(schema.as_str()),
            "case {case:?}: expected the activation to carry the published document, got {body}"
        );
        let read_arn = xml_text(&body, "Arn")
            .unwrap_or_else(|| panic!("case {case:?}: expected an ARN, got {body}"));

        // The write, naming the extension by the local name it is held under.
        let (status_code, body) = call(
            &router,
            "SetTypeConfiguration",
            &[
                member("Type", published.type_kind),
                member("TypeName", published.type_name),
                member("ConfigurationAlias", DISCOVERY_ALIAS),
                member("Configuration", &payload),
            ],
        )
        .await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected the configuration accepted, got {status_code} {body}"
        );
        assert!(
            xml_text(&body, "ConfigurationArn").is_some(),
            "case {case:?}: expected the ARN of the configuration data, got {body}"
        );

        // The read back, by each ARN the registry handed the client for that extension: the
        // one the activation answered, and the one the detailed read reported.
        for (form, type_arn) in [
            ("the ARN the activation answered", &expected_arn),
            ("the ARN the detailed read reported", &read_arn),
        ] {
            let (status_code, body) = call(
                &router,
                "BatchDescribeTypeConfigurations",
                &[
                    member(
                        "TypeConfigurationIdentifiers.member.1.TypeArn",
                        type_arn,
                    ),
                    member(
                        "TypeConfigurationIdentifiers.member.1.TypeConfigurationAlias",
                        DISCOVERY_ALIAS,
                    ),
                ],
            )
            .await;
            assert_eq!(
                status_code, 200,
                "case {case:?} by {form}: expected the batch read to answer, got {status_code} \
                 {body}"
            );
            let errors = xml_blocks(&body, "Errors")
                .into_iter()
                .next()
                .unwrap_or_default();
            assert!(
                xml_blocks(&errors, "member").is_empty(),
                "case {case:?} by {form}: expected no per-item error, got {body}"
            );
            let configurations = xml_blocks(&body, "TypeConfigurations")
                .into_iter()
                .next()
                .unwrap_or_default();
            assert_eq!(
                xml_blocks(&configurations, "member").len(),
                1,
                "case {case:?} by {form}: expected exactly one configuration, got {body}"
            );
            assert_eq!(
                detail(&body, "Configuration").as_deref(),
                Some(payload.as_str()),
                "case {case:?} by {form}: expected the document the write submitted, got {body}"
            );
            assert_eq!(
                detail(&body, "Alias").as_deref(),
                Some(DISCOVERY_ALIAS),
                "case {case:?} by {form}: expected the alias it was set under, got {body}"
            );
            assert_eq!(
                detail(&body, "TypeArn").as_deref(),
                Some(expected_arn.as_str()),
                "case {case:?} by {form}: expected the extension's own ARN, got {body}"
            );
            assert_eq!(
                detail(&body, "IsDefaultConfiguration").as_deref(),
                Some("false"),
                "case {case:?} by {form}: expected a configuration set under an alias of its \
                 own, got {body}"
            );
        }
    }
}
