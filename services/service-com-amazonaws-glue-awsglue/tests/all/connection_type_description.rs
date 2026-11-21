// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `DescribeConnectionType`: the full set of options the service
//! serves for one connection type, and the refusal of a name that is no connection type.
//!
//! The rule under test: the connection types this service describes are exactly the types a
//! connection can be CREATED with — the values of the modelled `ConnectionType` enum, the
//! key space of `ConnectionInput$ConnectionType` — each described with the capabilities,
//! properties, options, authentication and compute-environment configuration of the way
//! that type reaches its data source; a name outside that set is refused with 400
//! `InvalidInputException`, and the read mutates nothing.
//!
//! Each suite is a table whose rows differ along what the rule generalises over: which
//! connection type is asked for (the first suite asks for EVERY type the model declares),
//! which way of reaching a source that type is (relational engine, generic JDBC URL,
//! document store, record stream, file transfer, an Amazon Web Services service, a VPC
//! path, a supplied connector, a REST source, a view validation), and — for the refusal —
//! the value, length, casing and character set of a name no connection type has.
//!
//! The enum value lists the suites read (`CONNECTION_TYPE_VALUES`,
//! `CONNECTION_PROPERTY_KEY_VALUES`, `AUTHENTICATION_TYPE_VALUES`, `DATA_OPERATION_VALUES`,
//! `COMPUTE_ENVIRONMENT_VALUES`, `PROPERTY_TYPE_VALUES`, `O_AUTH2_GRANT_TYPE_VALUES`) are
//! the model's own, generated into this crate's types from the Smithy model, so the suites
//! restate no catalogue of their own where the model already carries one.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::ops::types::{
    AUTHENTICATION_TYPE_VALUES, COMPUTE_ENVIRONMENT_VALUES, CONNECTION_PROPERTY_KEY_VALUES,
    CONNECTION_TYPE_VALUES, DATA_OPERATION_VALUES, O_AUTH2_GRANT_TYPE_VALUES,
    PROPERTY_TYPE_VALUES,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{call, emulator, error_code, error_message, ok};

/// The wording a name that is no connection type is refused with: ONE sentence, naming
/// nothing of the request, so every such name is refused with the same words.
const EXPECTED_REFUSAL: &str = "The connection type is not available.";

/// The keys of a response's map member, or an empty set when the member is absent — an
/// awsJson map with nothing in it and an absent map say the same thing.
fn map_keys(body: &Value, member: &str) -> BTreeSet<String> {
    body.get(member)
        .and_then(Value::as_object)
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default()
}

/// The strings of a response's list member, or an empty list when the member is absent.
fn list_of(body: &Value, member: &str) -> Vec<String> {
    body.get(member)
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .map(|value| value.as_str().unwrap_or("<not a string>").to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// The names of the properties of a map member that the create MUST carry.
fn required_keys(body: &Value, member: &str) -> BTreeSet<String> {
    body.get(member)
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .filter(|(_, property)| property["Required"].as_bool() == Some(true))
                .map(|(key, _)| key.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Ask for one connection type's options.
async fn describe(router: &MultiServiceRouter, connection_type: &str) -> (u16, Value) {
    call(
        router,
        "DescribeConnectionType",
        &json!({ "ConnectionType": connection_type }),
    )
    .await
}

/// Assert that every property of `member` is well formed: keyed by its own name, carrying
/// at least one of the modelled property types, and — where it narrows itself to some data
/// operations — naming only modelled operations.
fn assert_properties(case: &str, member: &str, body: &Value, keyed_by_enum: bool) {
    let Some(properties) = body.get(member).and_then(Value::as_object) else {
        return;
    };
    for (key, property) in properties {
        assert_eq!(
            property["Name"].as_str(),
            Some(key.as_str()),
            "case {case}: {member} must key each property by its own Name, got {property}"
        );
        if keyed_by_enum {
            assert!(
                CONNECTION_PROPERTY_KEY_VALUES.contains(&key.as_str()),
                "case {case}: {member} must key properties by a modelled ConnectionPropertyKey, \
                 got {key:?}"
            );
        }
        let description = property["Description"].as_str().unwrap_or("");
        assert!(
            !description.is_empty(),
            "case {case}: {member} property {key:?} must describe itself, got {property}"
        );
        let property_types = list_of(property, "PropertyTypes");
        assert!(
            !property_types.is_empty(),
            "case {case}: {member} property {key:?} must state its PropertyTypes, got {property}"
        );
        for property_type in &property_types {
            assert!(
                PROPERTY_TYPE_VALUES.contains(&property_type.as_str()),
                "case {case}: {member} property {key:?} states PropertyType {property_type:?}, \
                 which the model does not declare"
            );
        }
        for scope in list_of(property, "DataOperationScopes") {
            assert!(
                DATA_OPERATION_VALUES.contains(&scope.as_str()),
                "case {case}: {member} property {key:?} is scoped to {scope:?}, which is no \
                 modelled DataOperation"
            );
        }
        if let Some(allowed) = property.get("AllowedValues").and_then(Value::as_array) {
            for value in allowed {
                assert!(
                    value["Value"].as_str().is_some_and(|value| !value.is_empty()),
                    "case {case}: {member} property {key:?} must state each allowed value, got \
                     {value}"
                );
            }
        }
    }
}

/// Assert everything that must hold of ANY connection type's description, whichever type it
/// is: it names itself, describes itself, states capabilities drawn from the modelled
/// enums, carries one configuration per compute environment it supports, and configures
/// exactly the authentication kinds it says it supports.
fn assert_description(connection_type: &str, body: &Value) {
    let case = connection_type;
    assert_eq!(
        body["ConnectionType"].as_str(),
        Some(connection_type),
        "case {case}: the description must name the type it was asked about, got {body}"
    );
    let description = body["Description"].as_str().unwrap_or("");
    assert!(
        !description.is_empty(),
        "case {case}: the description must say what the connection connects to, got {body}"
    );
    // `DescribeConnectionTypeResponse$Description` targets `Description`, length 0..1024
    // (from `smithy_query` on `DescribeConnectionType`).
    assert!(
        description.chars().count() <= 1024,
        "case {case}: Description must fit the modelled 1024 characters, got {} characters",
        description.chars().count()
    );

    let capabilities = body
        .get("Capabilities")
        .unwrap_or_else(|| panic!("case {case}: a description must state its Capabilities, got {body}"));
    let authentication_types = list_of(capabilities, "SupportedAuthenticationTypes");
    for value in &authentication_types {
        assert!(
            AUTHENTICATION_TYPE_VALUES.contains(&value.as_str()),
            "case {case}: SupportedAuthenticationTypes names {value:?}, which is no modelled \
             AuthenticationType"
        );
    }
    for value in list_of(capabilities, "SupportedDataOperations") {
        assert!(
            DATA_OPERATION_VALUES.contains(&value.as_str()),
            "case {case}: SupportedDataOperations names {value:?}, which is no modelled \
             DataOperation"
        );
    }
    let environments = list_of(capabilities, "SupportedComputeEnvironments");
    for value in &environments {
        assert!(
            COMPUTE_ENVIRONMENT_VALUES.contains(&value.as_str()),
            "case {case}: SupportedComputeEnvironments names {value:?}, which is no modelled \
             ComputeEnvironment"
        );
    }

    assert_properties(case, "ConnectionProperties", body, true);
    assert_properties(case, "ConnectionOptions", body, false);
    assert_properties(case, "PhysicalConnectionRequirements", body, false);

    // One configuration per environment the type supports, and no configuration for an
    // environment it does not.
    let configurations = map_keys(body, "ComputeEnvironmentConfigurations");
    assert_eq!(
        configurations,
        environments.iter().cloned().collect::<BTreeSet<String>>(),
        "case {case}: ComputeEnvironmentConfigurations must cover exactly the supported \
         compute environments, got {body}"
    );
    let options = map_keys(body, "ConnectionOptions");
    for environment in &environments {
        let configuration = &body["ComputeEnvironmentConfigurations"][environment];
        assert_eq!(
            configuration["ComputeEnvironment"].as_str(),
            Some(environment.as_str()),
            "case {case}: the {environment} configuration must name its own environment, got \
             {configuration}"
        );
        assert_eq!(
            list_of(configuration, "SupportedAuthenticationTypes"),
            authentication_types,
            "case {case}: the {environment} configuration must support the type's own \
             authentication kinds, got {configuration}"
        );
        assert_eq!(
            map_keys(configuration, "ConnectionOptions"),
            options,
            "case {case}: the {environment} configuration must carry the type's connection \
             options, got {configuration}"
        );
        assert_properties(
            &format!("{case} / {environment}"),
            "ConnectionOptions",
            configuration,
            false,
        );
    }

    // A type that authenticates somehow says HOW; a type that takes no credentials at all
    // reports no authentication configuration rather than an empty one.
    let authentication = body.get("AuthenticationConfiguration");
    if authentication_types.is_empty() {
        assert!(
            authentication.is_none(),
            "case {case}: a type that supports no authentication type must report no \
             AuthenticationConfiguration, got {body}"
        );
        assert!(
            !map_keys(body, "ConnectionProperties").contains("SECRET_ID"),
            "case {case}: a type with no credentials must not take a secret, got {body}"
        );
        return;
    }
    let authentication = authentication
        .unwrap_or_else(|| panic!("case {case}: a type with credentials must report how they are supplied, got {body}"));
    let allowed: Vec<String> = authentication["AuthenticationType"]["AllowedValues"]
        .as_array()
        .unwrap_or_else(|| {
            panic!("case {case}: the AuthenticationType property must state the kinds it admits, got {authentication}")
        })
        .iter()
        .map(|value| value["Value"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        allowed, authentication_types,
        "case {case}: the AuthenticationType property must admit exactly the supported \
         authentication types, got {authentication}"
    );

    for (kind, member) in [
        ("OAUTH2", "OAuth2Properties"),
        ("BASIC", "BasicAuthenticationProperties"),
        ("CUSTOM", "CustomAuthenticationProperties"),
    ] {
        let supported = authentication_types.iter().any(|value| value == kind);
        assert_eq!(
            authentication.get(member).is_some(),
            supported,
            "case {case}: {member} must be reported exactly when {kind} is supported, got \
             {authentication}"
        );
        assert_properties(case, member, authentication, false);
    }

    // Credentials that may live in Secrets Manager instead of on the connection are both a
    // property of the create and a member of the authentication configuration, so the two
    // can never disagree.
    assert_eq!(
        authentication.get("SecretArn").is_some(),
        map_keys(body, "ConnectionProperties").contains("SECRET_ID"),
        "case {case}: a secret-held credential must be reported both as SECRET_ID and as \
         SecretArn, got {body}"
    );

    if supports(&authentication_types, "OAUTH2") {
        let grant = &authentication["OAuth2Properties"]["OAuth2GrantType"];
        let grants: Vec<String> = grant["AllowedValues"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case}: the OAuth2 grant property must state the grants it admits, got {grant}"))
            .iter()
            .map(|value| value["Value"].as_str().unwrap_or_default().to_string())
            .collect();
        for value in &grants {
            assert!(
                O_AUTH2_GRANT_TYPE_VALUES.contains(&value.as_str()),
                "case {case}: the OAuth2 grant property admits {value:?}, which is no modelled \
                 OAuth2GrantType"
            );
        }
        assert!(
            !grants.is_empty(),
            "case {case}: the OAuth2 grant property must admit at least one grant, got {grant}"
        );
    }
}

/// Whether `kind` is one of the authentication types a description states.
fn supports(authentication_types: &[String], kind: &str) -> bool {
    authentication_types.iter().any(|value| value == kind)
}

/// EVERY connection type a connection can be created with is described, and every
/// description satisfies the invariants above.
///
/// The rows are the whole modelled `ConnectionType` enum
/// (`CONNECTION_TYPE_VALUES`), so the suite spans every connector kind the service serves
/// rather than the handful the families below are read through.
#[tokio::test]
async fn every_connection_type_a_connection_can_be_created_with_is_described() {
    let router = emulator();

    for connection_type in CONNECTION_TYPE_VALUES {
        let body = ok(
            &router,
            "DescribeConnectionType",
            &json!({ "ConnectionType": connection_type }),
        )
        .await;
        assert_description(connection_type, &body);
    }
}

/// One row of the family table below: a connection type and what the way it reaches its
/// source means for the description the service answers.
struct FamilyRow {
    /// The type asked about.
    connection_type: &'static str,
    /// The `ConnectionProperties` a create of it must carry.
    required_properties: &'static [&'static str],
    /// The authentication types it takes, in the order they are reported.
    authentication_types: &'static [&'static str],
    /// What a job may do with it.
    data_operations: &'static [&'static str],
    /// The compute environments that can use it.
    compute_environments: &'static [&'static str],
    /// Whether it is reached inside a VPC, so its physical placement is reported.
    in_vpc: bool,
}

/// One row per way of reaching a data source, plus a second row of two of those ways.
const FAMILY_ROWS: &[FamilyRow] = &[
    FamilyRow {
        connection_type: "MYSQL",
        required_properties: &["HOST", "PORT"],
        authentication_types: &["BASIC"],
        data_operations: &["READ", "WRITE"],
        compute_environments: &["SPARK", "PYTHON", "ATHENA"],
        in_vpc: true,
    },
    FamilyRow {
        connection_type: "POSTGRESQL",
        required_properties: &["HOST", "PORT"],
        authentication_types: &["BASIC"],
        data_operations: &["READ", "WRITE"],
        compute_environments: &["SPARK", "PYTHON", "ATHENA"],
        in_vpc: true,
    },
    FamilyRow {
        connection_type: "JDBC",
        required_properties: &["JDBC_CONNECTION_URL"],
        authentication_types: &["BASIC"],
        data_operations: &["READ", "WRITE"],
        compute_environments: &["SPARK", "PYTHON"],
        in_vpc: true,
    },
    FamilyRow {
        connection_type: "MONGODB",
        required_properties: &["CONNECTION_URL"],
        authentication_types: &["BASIC"],
        data_operations: &["READ", "WRITE"],
        compute_environments: &["SPARK"],
        in_vpc: true,
    },
    FamilyRow {
        connection_type: "KAFKA",
        required_properties: &["KAFKA_BOOTSTRAP_SERVERS"],
        authentication_types: &["BASIC", "CUSTOM", "IAM"],
        data_operations: &["READ", "WRITE"],
        compute_environments: &["SPARK"],
        in_vpc: true,
    },
    FamilyRow {
        connection_type: "SFTP",
        required_properties: &["HOST"],
        authentication_types: &["BASIC"],
        data_operations: &["READ", "WRITE"],
        compute_environments: &["SPARK"],
        in_vpc: true,
    },
    FamilyRow {
        connection_type: "DYNAMODB",
        required_properties: &[],
        authentication_types: &["IAM"],
        data_operations: &["READ", "WRITE"],
        compute_environments: &["SPARK", "PYTHON", "ATHENA"],
        in_vpc: false,
    },
    FamilyRow {
        connection_type: "NETWORK",
        required_properties: &[],
        authentication_types: &[],
        data_operations: &[],
        compute_environments: &["SPARK", "PYTHON"],
        in_vpc: true,
    },
    FamilyRow {
        connection_type: "MARKETPLACE",
        required_properties: &["CONNECTOR_URL", "CONNECTOR_TYPE", "CONNECTOR_CLASS_NAME"],
        authentication_types: &["BASIC", "CUSTOM", "IAM"],
        data_operations: &["READ", "WRITE"],
        compute_environments: &["SPARK", "PYTHON", "ATHENA"],
        in_vpc: true,
    },
    FamilyRow {
        connection_type: "SALESFORCE",
        required_properties: &["ENDPOINT"],
        authentication_types: &["BASIC", "OAUTH2", "CUSTOM"],
        data_operations: &["READ"],
        compute_environments: &["SPARK", "PYTHON"],
        in_vpc: false,
    },
    FamilyRow {
        connection_type: "HUBSPOT",
        required_properties: &["ENDPOINT"],
        authentication_types: &["BASIC", "OAUTH2", "CUSTOM"],
        data_operations: &["READ"],
        compute_environments: &["SPARK", "PYTHON"],
        in_vpc: false,
    },
    FamilyRow {
        connection_type: "VIEW_VALIDATION_ATHENA",
        required_properties: &[],
        authentication_types: &["IAM"],
        data_operations: &["READ"],
        compute_environments: &["ATHENA"],
        in_vpc: false,
    },
    FamilyRow {
        connection_type: "VIEW_VALIDATION_REDSHIFT",
        required_properties: &[],
        authentication_types: &["BASIC", "IAM"],
        data_operations: &["READ"],
        compute_environments: &[],
        in_vpc: false,
    },
];

/// What each WAY of reaching a data source is configured with, one row per family: the
/// properties a create of that type must carry, the credentials it takes, what a job may do
/// with it, which compute environments can use it, and whether it sits inside a VPC.
///
/// The rows name types the issue did not cite (`POSTGRESQL`, `HUBSPOT`, `SFTP`,
/// `VIEW_VALIDATION_ATHENA`, …) beside one per remaining family, and two rows of the same
/// family (`MYSQL`/`POSTGRESQL`, `SALESFORCE`/`HUBSPOT`) so the suite shows the
/// configuration follows the way the source is reached and not the individual type.
#[tokio::test]
async fn each_connection_type_is_described_with_the_way_it_reaches_its_source() {
    let router = emulator();

    for row in FAMILY_ROWS {
        let connection_type = row.connection_type;
        let body = ok(
            &router,
            "DescribeConnectionType",
            &json!({ "ConnectionType": connection_type }),
        )
        .await;
        assert_description(connection_type, &body);

        assert_eq!(
            required_keys(&body, "ConnectionProperties"),
            row.required_properties
                .iter()
                .map(|key| (*key).to_string())
                .collect::<BTreeSet<String>>(),
            "case {connection_type}: the properties a create must carry, got {body}"
        );
        assert_eq!(
            list_of(&body["Capabilities"], "SupportedAuthenticationTypes"),
            row.authentication_types,
            "case {connection_type}: the credentials this type takes, got {body}"
        );
        assert_eq!(
            list_of(&body["Capabilities"], "SupportedDataOperations"),
            row.data_operations,
            "case {connection_type}: what a job may do with this type, got {body}"
        );
        assert_eq!(
            list_of(&body["Capabilities"], "SupportedComputeEnvironments"),
            row.compute_environments,
            "case {connection_type}: which compute environments use this type, got {body}"
        );
        assert_eq!(
            body.get("PhysicalConnectionRequirements").is_some(),
            row.in_vpc,
            "case {connection_type}: a type reached inside a VPC reports where it sits, got \
             {body}"
        );
        // A VPC path is nothing BUT its placement, so every physical requirement of it is
        // required; a source that can also be reached without one leaves them optional.
        if row.in_vpc {
            let required = required_keys(&body, "PhysicalConnectionRequirements");
            let all = map_keys(&body, "PhysicalConnectionRequirements");
            let expected = if connection_type == "NETWORK" {
                all.clone()
            } else {
                BTreeSet::new()
            };
            assert_eq!(
                required, expected,
                "case {connection_type}: the physical requirements a create must carry, got \
                 {body}"
            );
        }
    }
}

/// A name that is no connection type the service serves is refused with 400
/// `InvalidInputException` and the service's own wording, however it is spelled — and the
/// refusal is NOT answered for a name the service does serve.
///
/// The rows differ along the axis the refusal generalises over: an unknown name of the
/// fixture's own shape, the lowercase and mixed-case spellings of types the service serves
/// (the modelled values are the uppercase ones), a served type with a trailing space, a
/// custom REST type nothing registered, a single character, and a name at the member's
/// documented 255-character ceiling (`DescribeConnectionTypeRequest$ConnectionType` targets
/// `NameString`, length 1..255, from `smithy_query` on `DescribeConnectionType`).
#[tokio::test]
async fn a_name_that_is_no_connection_type_is_refused() {
    let router = emulator();

    let names = vec![
        ("unknown name".to_string(), "NOT_A_CONNECTION_TYPE".to_string()),
        ("lowercase spelling".to_string(), "mysql".to_string()),
        ("mixed case spelling".to_string(), "Salesforce".to_string()),
        ("trailing space".to_string(), "JDBC ".to_string()),
        (
            "unregistered custom REST type".to_string(),
            "REST-custom-source".to_string(),
        ),
        ("single character".to_string(), "x".to_string()),
        ("at the 255 ceiling".to_string(), "T".repeat(255)),
    ];

    for (case, name) in &names {
        let (status, body) = describe(&router, name).await;
        assert_eq!(
            status, 400,
            "case {case:?} ({name:?}): expected 400, got {status} with {body}"
        );
        assert!(
            error_code(&body).ends_with("InvalidInputException"),
            "case {case:?} ({name:?}): expected InvalidInputException, got {}",
            error_code(&body)
        );
        assert_eq!(
            error_message(&body),
            EXPECTED_REFUSAL,
            "case {case:?} ({name:?}): expected Message {EXPECTED_REFUSAL:?}, got {:?}",
            error_message(&body)
        );
        assert!(
            !error_message(&body).contains(name.as_str()),
            "case {case:?} ({name:?}): the refusal must not carry the requested name, got {:?}",
            error_message(&body)
        );
    }

    // The same read of a type the service DOES serve is answered, so the refusals above are
    // about the requested name alone.
    let served = ok(
        &router,
        "DescribeConnectionType",
        &json!({ "ConnectionType": "MYSQL" }),
    )
    .await;
    assert_eq!(
        served["ConnectionType"].as_str(),
        Some("MYSQL"),
        "a served connection type must still be described, got {served}"
    );
}

/// Describing a connection type reports what the service itself ships, so the answer does
/// not depend on what this account holds and reading it leaves nothing changed: the same
/// read answers the same thing before and after a connection of that type is created, and
/// the connection created in between is still there afterwards.
#[tokio::test]
async fn describing_a_connection_type_changes_nothing_a_later_caller_sees() {
    let router = emulator();

    let before = ok(
        &router,
        "DescribeConnectionType",
        &json!({ "ConnectionType": "POSTGRESQL" }),
    )
    .await;

    ok(
        &router,
        "CreateConnection",
        &json!({
            "ConnectionInput": {
                "Name": "lake-postgres",
                "ConnectionType": "POSTGRESQL",
                "ConnectionProperties": { "HOST": "db.internal", "PORT": "5432" },
            },
        }),
    )
    .await;

    for round in 1..=3 {
        let again = ok(
            &router,
            "DescribeConnectionType",
            &json!({ "ConnectionType": "POSTGRESQL" }),
        )
        .await;
        assert_eq!(
            again, before,
            "case round {round}: the description of a connection type must not change, got \
             {again}"
        );
    }

    let connection = ok(
        &router,
        "GetConnection",
        &json!({ "Name": "lake-postgres" }),
    )
    .await;
    assert_eq!(
        connection["Connection"]["ConnectionType"].as_str(),
        Some("POSTGRESQL"),
        "the connection created of that type must still be reported, got {connection}"
    );
}
