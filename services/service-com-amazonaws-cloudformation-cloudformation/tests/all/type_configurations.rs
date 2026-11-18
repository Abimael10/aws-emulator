// SPDX-License-Identifier: CC0-1.0

//! Activating a public third-party extension and configuring it, end to end over the
//! awsQuery wire.
//!
//! The rule every case here holds the service to: **`ActivateType` puts the activation of a
//! public extension in this account and Region's registry and answers the ARN this account
//! addresses it by — for a RESOURCE extension only when the request names an execution role;
//! `SetTypeConfiguration` persists the account-level configuration of the
//! extension THAT ARN names and answers the ARN of the configuration data; and
//! `BatchDescribeTypeConfigurations` reads that same configuration, alias and ARN back,
//! reporting an identifier the registry holds no configuration for as this batch's own error
//! entry rather than refusing the whole read.**
//!
//! The rows differ along what the rule generalises over: the kind of extension (a Hook, a
//! resource type), how the request names the public extension (`PublicTypeArn`, or
//! `Type` + `TypeName` + `PublisherId`), whether the activation takes an alias, whether the
//! configuration is set under an alias of its own or the default one, and how the read
//! identifies the configuration (by the type's ARN, by the configuration's own ARN, by kind
//! and name). The MODULE category, which holds no configuration at all, is the table in
//! `module_type_configurations.rs`.
//!
//! The second rule, about WHICH publisher a request may name: **CloudFormation does not hold
//! a request's `PublisherId` to the modelled third-party form — Amazon Web Services publishes
//! its own Hooks under a publisher id that form does not admit, so an activation naming that
//! publisher is served and the Hook is activated, configured and read back like any other —
//! while every value outside the form is still refused, in the words and under the code the
//! request boundary refuses a constrained member with.** Its rows differ along what that rule
//! generalises over: the operation whose request names a publisher (`ActivateType`,
//! `DescribeType`, `ListTypeVersions`, `DescribePublisher`, and the publisher `ListTypes`
//! filters by, which is the member nested inside `Filters`), and the FORM of the value — the
//! first-party id, ids at and inside both ends of the modelled form, one character short of
//! it, one past its length, a character the form does not admit, and a non-ASCII one.
//!
//! The recorded traces this suite was written for are one row of each: a Guard Hook activated
//! with an execution role, configured under the alias `default`, and read back; and that
//! activation naming Amazon Web Services' own Hook publisher.

use crate::harness::{xml_blocks, xml_unescape};
use crate::stack_lifecycle::{call, member, router, xml_text};
use service_com_amazonaws_cloudformation_cloudformation::state::registry;
use smithy_server_runtime::MultiServiceRouter;

/// The account and Region every ARN this suite asserts belongs to.
const ACCOUNT_ID: &str = "000000000000";
const REGION: &str = "us-east-1";

/// The publisher id a public third-party extension carries, within the modelled
/// `PublisherId` pattern `^[0-9a-zA-Z]{12,40}$`.
const PUBLISHER_ID: &str = "abcdef0123456789abcd";

/// The configuration a Hook is turned on by, as a client writes it.
const HOOK_CONFIGURATION: &str = r#"{"CloudFormationConfiguration":{"HookConfiguration":{"HookInvocationStatus":"ENABLED","TargetOperations":["STACK"],"FailureMode":"FAIL","Properties":{"ruleLocation":"s3://rules/guard.rules"}}}}"#;

/// The ARN of a public extension, as the modelled `ThirdPartyTypeArn` pattern spells one:
/// `arn:aws:cloudformation:<region>::type/<kind>/<publisher id>/<Type-Name>`.
fn public_type_arn(kind: &str, type_name: &str) -> String {
    format!(
        "arn:aws:cloudformation:{REGION}::type/{}/{PUBLISHER_ID}/{}",
        kind.to_ascii_lowercase(),
        type_name.replace("::", "-")
    )
}

/// One row of the activation-and-configuration table.
struct ExtensionCase {
    case: &'static str,
    /// The `Type` of extension: the kinds the modelled `ThirdPartyType` admits.
    kind: &'static str,
    /// The public extension's own name.
    type_name: &'static str,
    /// The alias the activation gives it, if any — which becomes the name this account
    /// addresses it by.
    alias: Option<&'static str>,
    /// Whether the activation names the public extension by its ARN (else by kind, name and
    /// publisher).
    by_public_arn: bool,
    /// The alias the configuration is set under, if any (else the default one).
    configuration_alias: Option<&'static str>,
    configuration: &'static str,
}

fn extension_cases() -> Vec<ExtensionCase> {
    vec![
        ExtensionCase {
            case: "a Hook activated by kind, name and publisher, configured under default",
            kind: "HOOK",
            type_name: "Example::Guard::Hook",
            alias: None,
            by_public_arn: false,
            configuration_alias: None,
            configuration: HOOK_CONFIGURATION,
        },
        ExtensionCase {
            case: "a Hook activated by its public ARN under an alias",
            kind: "HOOK",
            type_name: "Example::Other::Hook",
            alias: Some("Local::Guard::Hook"),
            by_public_arn: true,
            configuration_alias: Some("default"),
            configuration: HOOK_CONFIGURATION,
        },
        ExtensionCase {
            case: "a resource type configured under an alias of its own",
            kind: "RESOURCE",
            type_name: "Example::Store::Bucket",
            alias: None,
            by_public_arn: false,
            configuration_alias: Some("production"),
            configuration: r#"{"Credentials":{"ApiKey":"{{resolve:secretsmanager:key}}"}}"#,
        },
        ExtensionCase {
            case: "a resource type activated by its public ARN, configuration naming one member",
            kind: "RESOURCE",
            type_name: "Example::Network::Subnets",
            alias: None,
            by_public_arn: true,
            configuration_alias: Some("Alias2"),
            configuration: r#"{"Properties":{"Cidr":"10.0.0.0/16"}}"#,
        },
    ]
}

/// `ActivateType`, with the members a row varies.
async fn activate(
    router: &MultiServiceRouter,
    case: &ExtensionCase,
) -> (u16, String) {
    let mut members = vec![member(
        "ExecutionRoleArn",
        &format!("arn:aws:iam::{ACCOUNT_ID}:role/extension-role"),
    )];
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
    call(router, "ActivateType", &members).await
}

/// The `TypeConfigurationDetails` block one read answered with, as `(member, text)` pairs.
fn details(body: &str) -> Vec<(String, String)> {
    let block = xml_blocks(body, "TypeConfigurations")
        .into_iter()
        .next()
        .unwrap_or_default();
    ["Arn", "Alias", "Configuration", "TypeArn", "TypeName", "IsDefaultConfiguration"]
        .iter()
        .filter_map(|name| {
            Some(((*name).to_string(), xml_text(&block, name)?))
        })
        .collect()
}

/// What one read reports for `member`, or the empty string.
fn reported(read: &[(String, String)], member: &str) -> String {
    read.iter()
        .find(|(name, _)| name == member)
        .map(|(_, text)| text.clone())
        .unwrap_or_default()
}

#[tokio::test]
async fn an_activated_extension_takes_a_configuration_and_reads_it_back() {
    for case in extension_cases() {
        let router = router();
        let (code, body) = activate(&router, &case).await;
        assert_eq!(
            code, 200,
            "case {:?}: expected ActivateType to succeed, got {code} {body}",
            case.case
        );
        let activated_arn = xml_text(&body, "Arn").unwrap_or_default();
        let local_name = case.alias.unwrap_or(case.type_name);
        let expected_arn = format!(
            "arn:aws:cloudformation:{REGION}:{ACCOUNT_ID}:type/{}/{}",
            case.kind.to_ascii_lowercase(),
            local_name.replace("::", "-")
        );
        assert_eq!(
            activated_arn, expected_arn,
            "case {:?}: expected the activation's ARN to name this account, Region, kind and \
             local name, got {activated_arn:?}",
            case.case
        );

        // The activated extension is in this account and Region's registry, under the name
        // this account addresses it by.
        let (code, described) = call(
            &router,
            "DescribeType",
            &[member("Arn", &activated_arn)],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {:?}: expected DescribeType to answer for the activation, got {code} \
             {described}",
            case.case
        );
        assert_eq!(
            xml_text(&described, "TypeName").as_deref(),
            Some(local_name),
            "case {:?}: expected the activation to be held under its local name, got \
             {described}",
            case.case
        );
        assert_eq!(
            xml_text(&described, "IsActivated").as_deref(),
            Some("true"),
            "case {:?}: expected the extension to be reported activated, got {described}",
            case.case
        );

        // Its configuration is persisted against that ARN, and the write answers the ARN of
        // the configuration data.
        let mut members = vec![
            member("TypeArn", &activated_arn),
            member("Configuration", case.configuration),
        ];
        if let Some(alias) = case.configuration_alias {
            members.push(member("ConfigurationAlias", alias));
        }
        let (code, body) = call(&router, "SetTypeConfiguration", &members).await;
        assert_eq!(
            code, 200,
            "case {:?}: expected SetTypeConfiguration to succeed for the ARN ActivateType \
             answered, got {code} {body}",
            case.case
        );
        let configuration_arn = xml_text(&body, "ConfigurationArn").unwrap_or_default();
        let alias = case
            .configuration_alias
            .unwrap_or(registry::DEFAULT_CONFIGURATION_ALIAS);
        let expected_configuration_arn = format!(
            "arn:aws:cloudformation:{REGION}:{ACCOUNT_ID}:type-configuration/{}/{}/{alias}",
            case.kind.to_ascii_lowercase(),
            local_name.replace("::", "-")
        );
        assert_eq!(
            configuration_arn, expected_configuration_arn,
            "case {:?}: expected the configuration's own ARN, got {configuration_arn:?}",
            case.case
        );

        // ...and the read reports exactly what the write wrote, however the identifier names
        // it: by the type's ARN and alias, by the configuration's own ARN, and by kind and
        // name.
        let identifiers: Vec<Vec<(String, String)>> = vec![
            vec![
                member("TypeConfigurationIdentifiers.member.1.TypeArn", &activated_arn),
                member(
                    "TypeConfigurationIdentifiers.member.1.TypeConfigurationAlias",
                    alias,
                ),
            ],
            vec![member(
                "TypeConfigurationIdentifiers.member.1.TypeConfigurationArn",
                &configuration_arn,
            )],
            vec![
                member("TypeConfigurationIdentifiers.member.1.Type", case.kind),
                member("TypeConfigurationIdentifiers.member.1.TypeName", local_name),
                member(
                    "TypeConfigurationIdentifiers.member.1.TypeConfigurationAlias",
                    alias,
                ),
            ],
        ];
        for identifier in identifiers {
            let (code, body) =
                call(&router, "BatchDescribeTypeConfigurations", &identifier).await;
            assert_eq!(
                code, 200,
                "case {:?}: expected BatchDescribeTypeConfigurations to answer, got {code} \
                 {body}",
                case.case
            );
            let read = details(&body);
            assert_eq!(
                reported(&read, "Configuration"),
                case.configuration,
                "case {:?} ({identifier:?}): expected the configuration that was set, got \
                 {read:?}",
                case.case
            );
            assert_eq!(
                reported(&read, "Alias"),
                alias,
                "case {:?}: expected the alias the configuration was set under, got {read:?}",
                case.case
            );
            assert_eq!(
                reported(&read, "Arn"),
                configuration_arn,
                "case {:?}: expected the configuration's own ARN, got {read:?}",
                case.case
            );
            assert_eq!(
                reported(&read, "TypeArn"),
                activated_arn,
                "case {:?}: expected the extension's ARN, got {read:?}",
                case.case
            );
            assert_eq!(
                reported(&read, "TypeName"),
                local_name,
                "case {:?}: expected the extension's local name, got {read:?}",
                case.case
            );
            assert_eq!(
                reported(&read, "IsDefaultConfiguration"),
                (alias == registry::DEFAULT_CONFIGURATION_ALIAS).to_string(),
                "case {:?}: expected the default-configuration flag to follow the alias, got \
                 {read:?}",
                case.case
            );
        }

        // Setting the configuration again under the same alias REPLACES it, under the same
        // configuration ARN.
        let revised = r#"{"Properties":{"revised":"true"}}"#;
        let mut members = vec![
            member("TypeArn", &activated_arn),
            member("Configuration", revised),
        ];
        if let Some(alias) = case.configuration_alias {
            members.push(member("ConfigurationAlias", alias));
        }
        let (code, body) = call(&router, "SetTypeConfiguration", &members).await;
        assert_eq!(
            code, 200,
            "case {:?}: expected a second SetTypeConfiguration to succeed, got {code} {body}",
            case.case
        );
        assert_eq!(
            xml_text(&body, "ConfigurationArn").unwrap_or_default(),
            configuration_arn,
            "case {:?}: expected the replacement to keep the configuration's ARN, got {body}",
            case.case
        );
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
            reported(&details(&body), "Configuration"),
            revised,
            "case {:?}: expected the read to report the configuration last written, got {body}",
            case.case
        );
    }
}

#[tokio::test]
async fn an_identifier_that_resolves_to_nothing_is_this_batch_s_own_error_entry() {
    let router = router();
    let case = &extension_cases()[0];
    let (code, body) = activate(&router, case).await;
    assert_eq!(code, 200, "expected ActivateType to succeed, got {code} {body}");
    let activated_arn = xml_text(&body, "Arn").unwrap_or_default();
    let (code, body) = call(
        &router,
        "SetTypeConfiguration",
        &[
            member("TypeArn", &activated_arn),
            member("Configuration", HOOK_CONFIGURATION),
        ],
    )
    .await;
    assert_eq!(code, 200, "expected SetTypeConfiguration to succeed, got {code} {body}");

    // One identifier the registry answers to, one it does not: the read reports the first
    // and names the second under `Errors`, with the status of the modelled not-found
    // refusal as its `ErrorCode`.
    let unknown = format!("arn:aws:cloudformation:{REGION}:{ACCOUNT_ID}:type/hook/No-Such-Hook");
    let (code, body) = call(
        &router,
        "BatchDescribeTypeConfigurations",
        &[
            member("TypeConfigurationIdentifiers.member.1.TypeArn", &activated_arn),
            member("TypeConfigurationIdentifiers.member.2.TypeArn", &unknown),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "expected the batch to succeed with an error entry, got {code} {body}"
    );
    assert_eq!(
        reported(&details(&body), "TypeArn"),
        activated_arn,
        "expected the resolvable identifier to be reported, got {body}"
    );
    let errors = xml_blocks(&body, "Errors").into_iter().next().unwrap_or_default();
    assert_eq!(
        xml_text(&errors, "ErrorCode").as_deref(),
        Some("404"),
        "expected the unresolvable identifier to be reported with the not-found code, got \
         {body}"
    );
    assert_eq!(
        xml_text(&errors, "TypeArn").as_deref(),
        Some(unknown.as_str()),
        "expected the error entry to carry the identifier it came from, got {body}"
    );
}

#[tokio::test]
async fn an_extension_no_configuration_was_set_for_is_this_batch_s_own_error_entry() {
    let router = router();
    let case = &extension_cases()[0];
    let (code, body) = activate(&router, case).await;
    assert_eq!(code, 200, "expected ActivateType to succeed, got {code} {body}");
    let activated_arn = xml_text(&body, "Arn").unwrap_or_default();

    let (code, body) = call(
        &router,
        "BatchDescribeTypeConfigurations",
        &[member(
            "TypeConfigurationIdentifiers.member.1.TypeArn",
            &activated_arn,
        )],
    )
    .await;
    assert_eq!(code, 200, "expected the batch to succeed, got {code} {body}");
    // The extension is in the registry and no configuration of it was ever set, so there is
    // no configuration to report: the read reports the miss through the batch's own error
    // channel instead, which is what tells a client it still has to set one.
    assert!(
        xml_blocks(&body, "TypeConfigurations")
            .first()
            .is_some_and(String::is_empty),
        "expected no configuration reported for an extension none was set for, got {body}"
    );
    let errors = xml_blocks(&body, "Errors")
        .into_iter()
        .next()
        .unwrap_or_default();
    assert_eq!(
        xml_blocks(&errors, "member").len(),
        1,
        "expected exactly one error entry, got {body}"
    );
    assert_eq!(
        (
            xml_text(&errors, "ErrorCode").unwrap_or_default().as_str(),
            xml_text(&errors, "ErrorMessage").unwrap_or_default().as_str(),
        ),
        ("404", "TypeConfiguration not found"),
        "expected the registry's own wording for a configuration it does not hold, got {body}"
    );
    assert_eq!(
        xml_text(&errors, "TypeArn").as_deref(),
        Some(activated_arn.as_str()),
        "expected the error entry to carry the identifier it came from, got {body}"
    );
}

#[tokio::test]
async fn a_type_arn_no_extension_answers_to_is_refused_as_not_found() {
    let router = router();
    let unknown = format!("arn:aws:cloudformation:{REGION}:{ACCOUNT_ID}:type/hook/Never-Activated");
    let (code, body) = call(
        &router,
        "SetTypeConfiguration",
        &[
            member("TypeArn", &unknown),
            member("Configuration", HOOK_CONFIGURATION),
        ],
    )
    .await;
    assert_eq!(
        code, 404,
        "expected a configuration of an extension the registry does not hold to be refused, \
         got {code} {body}"
    );
    assert_eq!(
        xml_text(&body, "Code").as_deref(),
        Some("TypeNotFoundException"),
        "expected the modelled not-found error, got {body}"
    );
}

/// Every Hook Amazon Web Services publishes to the public registry, read from the catalogue
/// rather than listed here, so one published later is covered without editing this suite.
fn amazon_published_hooks() -> Vec<&'static registry::PublishedExtension> {
    registry::PUBLIC_EXTENSION_CATALOG
        .iter()
        .filter(|published| {
            published.publisher_name == registry::AWS_PUBLISHER_NAME
                && published.type_kind == registry::registry_type::HOOK
        })
        .collect()
}

/// The execution role an activation names, inside the modelled `RoleARN2` pattern
/// `^arn:.+:iam::[0-9]{12}:role/.+$`.
fn execution_role_arn() -> String {
    format!("arn:aws:iam::{ACCOUNT_ID}:role/hook-execution")
}

#[tokio::test]
async fn an_amazon_published_hook_activates_under_its_first_party_publisher_id() {
    let published = amazon_published_hooks();
    assert!(
        !published.is_empty(),
        "the public registry must publish at least one Hook of Amazon Web Services' own for a \
         client to activate, got {:?}",
        registry::PUBLIC_EXTENSION_CATALOG
            .iter()
            .map(|entry| entry.type_name)
            .collect::<Vec<_>>()
    );

    // The rows: every such Hook, activated under its own name and under an alias — the two
    // local names an activation can be addressed by.
    for entry in published {
        for alias in [None, Some("Local::Guard::Hook")] {
            let case = format!(
                "{} activated as {}",
                entry.type_name,
                alias.unwrap_or(entry.type_name)
            );
            // A fresh account per row, so each activation is the first one its registry saw.
            let router = router();
            let mut members = vec![
                member("Type", entry.type_kind),
                member("TypeName", entry.type_name),
                member("PublisherId", registry::AWS_HOOKS_PUBLISHER_ID),
                member("ExecutionRoleArn", &execution_role_arn()),
            ];
            if let Some(alias) = alias {
                members.push(member("TypeNameAlias", alias));
            }
            let (code, body) = call(&router, "ActivateType", &members).await;
            assert_eq!(
                code, 200,
                "case {case:?}: expected the activation to be served, got {code} {body}"
            );
            let activated_arn = xml_text(&body, "Arn").unwrap_or_default();
            let local_name = alias.unwrap_or(entry.type_name);
            let expected_arn = format!(
                "arn:aws:cloudformation:{REGION}:{ACCOUNT_ID}:type/{}/{}",
                entry.type_kind.to_ascii_lowercase(),
                local_name.replace("::", "-")
            );
            assert_eq!(
                activated_arn, expected_arn,
                "case {case:?}: expected the ARN this account addresses the activation by, got \
                 {activated_arn:?}"
            );

            // The activation is in this account and Region's registry, under the local name,
            // naming the public extension it was made from.
            let (code, described) =
                call(&router, "DescribeType", &[member("Arn", &activated_arn)]).await;
            assert_eq!(
                code, 200,
                "case {case:?}: expected DescribeType to answer for the activation, got {code} \
                 {described}"
            );
            for (tag, expected) in [
                ("TypeName", local_name),
                ("IsActivated", "true"),
                ("PublisherId", registry::AWS_HOOKS_PUBLISHER_ID),
                ("OriginalTypeName", entry.type_name),
            ] {
                let actual = xml_text(&described, tag);
                assert_eq!(
                    actual.as_deref(),
                    Some(expected),
                    "case {case:?}: expected <{tag}> to be {expected:?}, got {actual:?} from \
                     {described}"
                );
            }

            // ...and it takes an account-level configuration and reads it back, which is what
            // activating a Hook is for.
            let (code, body) = call(
                &router,
                "SetTypeConfiguration",
                &[
                    member("TypeArn", &activated_arn),
                    member("Configuration", HOOK_CONFIGURATION),
                ],
            )
            .await;
            assert_eq!(
                code, 200,
                "case {case:?}: expected the configuration of the activated Hook to be \
                 persisted, got {code} {body}"
            );
            let configuration_arn = xml_text(&body, "ConfigurationArn").unwrap_or_default();
            let (code, body) = call(
                &router,
                "BatchDescribeTypeConfigurations",
                &[member(
                    "TypeConfigurationIdentifiers.member.1.TypeConfigurationArn",
                    &configuration_arn,
                )],
            )
            .await;
            assert_eq!(
                code, 200,
                "case {case:?}: expected the configuration read to answer, got {code} {body}"
            );
            let read = details(&body);
            assert_eq!(
                reported(&read, "Configuration"),
                HOOK_CONFIGURATION,
                "case {case:?}: expected the configuration that was set, got {read:?}"
            );
            assert_eq!(
                reported(&read, "TypeArn"),
                activated_arn,
                "case {case:?}: expected the activated Hook's own ARN, got {read:?}"
            );
        }
    }
}

/// The clause the request boundary states for a value outside a member's `@pattern`, and the
/// one it states for a value past its `@length` maximum — the two requirements a publisher id
/// can break, each naming the member path the walk spells.
fn pattern_clause(path: &str, value: &str) -> String {
    format!(
        "Value '{value}' at '{path}' failed to satisfy constraint: Member must satisfy regular \
         expression pattern: {}",
        registry::PUBLISHER_ID_PATTERN
    )
}

fn max_length_clause(path: &str, value: &str) -> String {
    format!(
        "Value '{value}' at '{path}' failed to satisfy constraint: Member must have length less \
         than or equal to {}",
        registry::PUBLISHER_ID_MAX_LEN
    )
}

/// The envelope those clauses are stated in, as the boundary's own renderer builds it.
fn validation_envelope(clauses: &[String]) -> String {
    match clauses {
        [one] => format!("1 validation error detected: {one}"),
        many => format!(
            "{} validation errors detected: {}",
            many.len(),
            many.join("; ")
        ),
    }
}

/// The members one operation's request carries to name `publisher_id`, with whatever else it
/// needs to identify an extension at all.
fn naming_the_publisher(
    action: &str,
    type_kind: &str,
    type_name: &str,
    publisher_id: &str,
) -> Vec<(String, String)> {
    match action {
        "DescribePublisher" => vec![member("PublisherId", publisher_id)],
        "ListTypes" => vec![
            member("Visibility", registry::visibility::PUBLIC),
            member("Filters.PublisherId", publisher_id),
        ],
        "ActivateType" => vec![
            member("Type", type_kind),
            member("TypeName", type_name),
            member("PublisherId", publisher_id),
            member("ExecutionRoleArn", &execution_role_arn()),
        ],
        _ => vec![
            member("Type", type_kind),
            member("TypeName", type_name),
            member("PublisherId", publisher_id),
        ],
    }
}

#[tokio::test]
async fn a_publisher_id_is_held_to_the_modelled_form_except_amazons_own() {
    let hook = amazon_published_hooks()
        .first()
        .copied()
        .expect("the public registry publishes a Hook of Amazon Web Services' own");

    // The forms a publisher id can take: Amazon's own, ids at and inside both ends of the
    // modelled form, one character short of it, one past its length, a character the form does
    // not admit, and a non-ASCII one. `served` is a value CloudFormation answers for; the
    // others carry the requirements their value breaks, in the order the boundary's walk
    // reports them (length, then pattern).
    struct Row {
        case: &'static str,
        publisher_id: String,
        /// Which requirements the value breaks: `max_length` before `pattern`, as the
        /// boundary states them; empty for a value the service serves.
        max_length: bool,
        pattern: bool,
    }
    let rows = vec![
        Row {
            case: "the publisher id Amazon Web Services' own Hooks are named by",
            publisher_id: registry::AWS_HOOKS_PUBLISHER_ID.to_string(),
            max_length: false,
            pattern: false,
        },
        Row {
            case: "an id inside the modelled form",
            publisher_id: PUBLISHER_ID.to_string(),
            max_length: false,
            pattern: false,
        },
        Row {
            case: "the shortest id the modelled form admits",
            publisher_id: "a1b2c3d4e5f6".to_string(),
            max_length: false,
            pattern: false,
        },
        Row {
            case: "the longest id the modelled form admits",
            publisher_id: "a".repeat(registry::PUBLISHER_ID_MAX_LEN),
            max_length: false,
            pattern: false,
        },
        Row {
            case: "one character short of the shortest the form admits",
            publisher_id: "a1b2c3d4e5f".to_string(),
            max_length: false,
            pattern: true,
        },
        Row {
            case: "one character past the longest the form admits",
            publisher_id: "a".repeat(registry::PUBLISHER_ID_MAX_LEN + 1),
            max_length: true,
            pattern: true,
        },
        Row {
            case: "a hyphen inside an id of the admitted length, which is not Amazon's own",
            publisher_id: "a1b2c3-d4e5f6".to_string(),
            max_length: false,
            pattern: true,
        },
        Row {
            case: "a non-ASCII letter inside an id of the admitted length",
            publisher_id: "a1b2c3d4e5fé".to_string(),
            max_length: false,
            pattern: true,
        },
    ];

    for action in [
        "ActivateType",
        "DescribeType",
        "ListTypeVersions",
        "DescribePublisher",
        "ListTypes",
    ] {
        // The member path the clause names: the operation's own member, and the one nested
        // inside the filters `ListTypes` narrows by.
        let path = if action == "ListTypes" {
            registry::FILTERS_PUBLISHER_ID_PATH
        } else {
            registry::PUBLISHER_ID_PATH
        };
        for row in &rows {
            let case = format!("{action} with {}", row.case);
            // A fresh account per row: an activation is a WRITE, and two rows activating the
            // same Hook under two publishers would be the alias clash rather than this rule.
            let router = router();
            let members =
                naming_the_publisher(action, hook.type_kind, hook.type_name, &row.publisher_id);
            let (code, body) = call(&router, action, &members).await;
            let mut clauses = Vec::new();
            if row.max_length {
                clauses.push(max_length_clause(path, &row.publisher_id));
            }
            if row.pattern {
                clauses.push(pattern_clause(path, &row.publisher_id));
            }
            if clauses.is_empty() {
                // A value the service serves is never answered with the boundary's refusal —
                // an error class none of these operations even models. What it IS answered
                // with is each operation's own business: the activation is performed, while a
                // read of a publisher that published nothing here is the registry's own
                // not-found.
                assert_ne!(
                    xml_text(&body, "Code").as_deref(),
                    Some("ValidationError"),
                    "case {case:?}: expected the value to be served rather than refused as \
                     invalid, got {code} {body}"
                );
                if action == "ActivateType" {
                    assert_eq!(
                        code, 200,
                        "case {case:?}: expected the activation to be performed, got {code} \
                         {body}"
                    );
                }
                continue;
            }
            assert_eq!(
                code, 400,
                "case {case:?}: expected the refusal of a value outside the modelled form, got \
                 {code} {body}"
            );
            assert_eq!(
                xml_text(&body, "Code").as_deref(),
                Some("ValidationError"),
                "case {case:?}: expected the code this service refuses a request with, got \
                 {body}"
            );
            let message = xml_text(&body, "Message").map(|message| xml_unescape(&message));
            assert_eq!(
                message.as_deref(),
                Some(validation_envelope(&clauses).as_str()),
                "case {case:?}: expected the boundary's own sentence, got {message:?}"
            );
        }
    }
}
