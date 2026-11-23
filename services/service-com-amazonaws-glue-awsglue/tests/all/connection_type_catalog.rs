// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the CONNECTION-TYPE CATALOG `DescribeConnectionType` reads: what the
//! service publishes about one connection type as opposed to another.
//!
//! The rule under test: `DescribeConnectionType` is a read of the service's own
//! connection-type catalog, so the answer is the entry recorded for the REQUESTED type —
//! that type's own data source (which its `Description` names) and the authentication types
//! a connection of it may be created with (which its `Capabilities` and its
//! `AuthenticationConfiguration` report, descriptors and all) — rather than one descriptor
//! synthesised for every type reached the same way. Two types reached the same way still
//! answer differently where the catalog records different things about them, and every type
//! a connection can be created with has an entry.
//!
//! The suites are tables whose rows differ along what the rule generalises over: which data
//! source the type reaches (a relational engine, a SaaS source read over REST, an Amazon Web
//! Services service, a record stream, a VPC path, a view validation), which credentials the
//! catalog records for it (an OAuth2 authorization server, an account's own user name and
//! password, a vendor API token, the caller's IAM identity, none at all), and how many of
//! those it records. The rows name types the issue did not cite (`SLACK`, `DATADOG`,
//! `SERVICENOW`, `ORACLE`, `TIMESTREAM`, …) beside the vendor OAuth2 source it did.
//!
//! The per-property descriptor fields the suites read (`Name`, `Description`, `Required`,
//! `PropertyTypes`, `AllowedValues`, `DefaultValue`) are the members of the modelled
//! `Property` structure, and the OAuth2 descriptors are keyed by the members of
//! `OAuth2PropertiesInput`, `OAuth2ClientApplication`, `AuthorizationCodeProperties` and
//! `OAuth2Credentials` — the properties a create of an OAuth2 connection sets
//! (`smithy_query shape=Property,OAuth2PropertiesInput,OAuth2ClientApplication,AuthorizationCodeProperties,OAuth2Credentials`).

use std::collections::BTreeSet;

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::ops::types::CONNECTION_TYPE_VALUES;
use service_com_amazonaws_glue_awsglue::state::CONNECTION_TYPE_CATALOG;
use smithy_server_runtime::MultiServiceRouter;

use crate::connection_type_description::map_keys;
use crate::wire::{emulator, ok, string_list};

/// One row of the catalog table: a connection type, the data source the service publishes
/// for it, and the authentication types it records for it.
struct CatalogRow {
    /// The type asked about.
    connection_type: &'static str,
    /// The data source the description must name.
    source: &'static str,
    /// The authentication types the catalog records, in the order they are reported.
    authentication_types: &'static [&'static str],
}

/// One row per kind of catalog entry: the data source and the credentials the service
/// publishes for that type.
const CATALOG_ROWS: &[CatalogRow] = &[
    // The vendor OAuth2 REST source the recorded call asked about.
    CatalogRow {
        connection_type: "ADOBEANALYTICS",
        source: "Adobe Analytics",
        authentication_types: &["OAUTH2"],
    },
    // Three more REST sources, each with the credentials its own vendor takes: an OAuth2
    // authorization server, an account's own credentials or that server, and a vendor API
    // token.
    CatalogRow {
        connection_type: "SLACK",
        source: "Slack",
        authentication_types: &["OAUTH2"],
    },
    CatalogRow {
        connection_type: "SERVICENOW",
        source: "ServiceNow",
        authentication_types: &["BASIC", "OAUTH2"],
    },
    CatalogRow {
        connection_type: "DATADOG",
        source: "Datadog",
        authentication_types: &["CUSTOM"],
    },
    CatalogRow {
        connection_type: "STRIPE",
        source: "Stripe",
        authentication_types: &["BASIC"],
    },
    // A relational engine, a document store, a record stream and a file transfer endpoint.
    CatalogRow {
        connection_type: "ORACLE",
        source: "Oracle Database",
        authentication_types: &["BASIC"],
    },
    CatalogRow {
        connection_type: "DOCUMENTDB",
        source: "Amazon DocumentDB",
        authentication_types: &["BASIC"],
    },
    CatalogRow {
        connection_type: "KAFKA",
        source: "Apache Kafka",
        authentication_types: &["BASIC", "CUSTOM", "IAM"],
    },
    CatalogRow {
        connection_type: "SFTP",
        source: "SFTP",
        authentication_types: &["BASIC"],
    },
    // An Amazon Web Services service, a VPC path that reaches no source at all, and the two
    // view validations.
    CatalogRow {
        connection_type: "TIMESTREAM",
        source: "Amazon Timestream",
        authentication_types: &["IAM"],
    },
    CatalogRow {
        connection_type: "NETWORK",
        source: "the resources of a VPC",
        authentication_types: &[],
    },
    CatalogRow {
        connection_type: "VIEW_VALIDATION_REDSHIFT",
        source: "Amazon Redshift",
        authentication_types: &["BASIC", "IAM"],
    },
    CatalogRow {
        connection_type: "VIEW_VALIDATION_ATHENA",
        source: "Amazon Athena",
        authentication_types: &["IAM"],
    },
];

/// Ask for one connection type's catalog entry, which must be answered.
async fn describe(router: &MultiServiceRouter, connection_type: &str) -> Value {
    ok(
        router,
        "DescribeConnectionType",
        &json!({ "ConnectionType": connection_type }),
    )
    .await
}

/// The authentication types a description says it supports.
fn supported_authentication_types(body: &Value) -> Vec<String> {
    string_list(&body["Capabilities"], "SupportedAuthenticationTypes")
}

/// The values the `AuthenticationType` property of a description admits.
fn admitted_authentication_types(body: &Value) -> Vec<String> {
    body["AuthenticationConfiguration"]["AuthenticationType"]["AllowedValues"]
        .as_array()
        .map(|values| {
            values
                .iter()
                .map(|value| value["Value"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Each type is described from its OWN catalog entry: the description names the data source
/// recorded for it, and the credentials it reports — both in `Capabilities` and in the
/// `AuthenticationConfiguration` descriptors — are the ones recorded for it and no others.
#[tokio::test]
async fn each_connection_type_is_described_from_its_own_catalog_entry() {
    let router = emulator();

    for row in CATALOG_ROWS {
        let case = row.connection_type;
        let body = describe(&router, case).await;

        let description = body["Description"].as_str().unwrap_or_default();
        assert!(
            description.contains(row.source),
            "case {case}: the description must name the data source the catalog records \
             ({:?}), got {description:?}",
            row.source
        );
        assert_eq!(
            supported_authentication_types(&body),
            row.authentication_types,
            "case {case}: the credentials the catalog records for this type, got {body}"
        );
        assert_eq!(
            admitted_authentication_types(&body),
            row.authentication_types,
            "case {case}: the AuthenticationType property must admit exactly the recorded \
             credentials, got {body}"
        );

        // The descriptors of each kind of credential are reported exactly where that kind is
        // one the catalog records for this type.
        let authentication = body.get("AuthenticationConfiguration");
        for (kind, member) in [
            ("OAUTH2", "OAuth2Properties"),
            ("BASIC", "BasicAuthenticationProperties"),
            ("CUSTOM", "CustomAuthenticationProperties"),
        ] {
            let recorded = row.authentication_types.contains(&kind);
            let reported = authentication
                .and_then(|configuration| configuration.get(member))
                .is_some();
            assert_eq!(
                reported, recorded,
                "case {case}: {member} must be reported exactly when {kind} is recorded, got \
                 {body}"
            );
        }

        // Each admitted authentication type describes itself, which is what the modelled
        // `AllowedValue$Description` carries.
        if let Some(values) = body["AuthenticationConfiguration"]["AuthenticationType"]
            ["AllowedValues"]
            .as_array()
        {
            for value in values {
                assert!(
                    value["Description"]
                        .as_str()
                        .is_some_and(|description| !description.is_empty()),
                    "case {case}: each admitted authentication type must describe itself, got \
                     {value}"
                );
            }
        }
    }
}

/// Two types reached the same way are still described from their own entries: REST sources
/// whose vendors take different credentials differ in what they report, and engines reached
/// the same way share their property keys while naming their own sources.
#[tokio::test]
async fn types_reached_the_same_way_differ_where_their_catalog_entries_differ() {
    let router = emulator();

    // Four REST sources, one per kind of credential their vendors take.
    let adobe = describe(&router, "ADOBEANALYTICS").await;
    let servicenow = describe(&router, "SERVICENOW").await;
    let datadog = describe(&router, "DATADOG").await;
    let stripe = describe(&router, "STRIPE").await;

    for (case, first, second) in [
        ("ADOBEANALYTICS / SERVICENOW", &adobe, &servicenow),
        ("SERVICENOW / DATADOG", &servicenow, &datadog),
        ("DATADOG / STRIPE", &datadog, &stripe),
    ] {
        assert_ne!(
            supported_authentication_types(first),
            supported_authentication_types(second),
            "case {case}: two REST sources whose vendors take different credentials must not \
             report the same ones, got {:?} and {:?}",
            supported_authentication_types(first),
            supported_authentication_types(second)
        );
        assert_ne!(
            first["Description"], second["Description"],
            "case {case}: each type's description must name its own data source, got {:?} and \
             {:?}",
            first["Description"], second["Description"]
        );
    }

    // Two engines reached the same way: the keys a create sets are the way's, the described
    // source is the type's own.
    let mysql = describe(&router, "MYSQL").await;
    let postgres = describe(&router, "POSTGRESQL").await;
    assert_eq!(
        map_keys(&mysql, "ConnectionProperties"),
        map_keys(&postgres, "ConnectionProperties"),
        "case MYSQL / POSTGRESQL: two engines reached over JDBC are configured with the same \
         keys, got {mysql} and {postgres}"
    );
    assert_ne!(
        mysql["Description"], postgres["Description"],
        "case MYSQL / POSTGRESQL: each engine's description must name its own source, got {:?} \
         and {:?}",
        mysql["Description"], postgres["Description"]
    );
}

/// A type whose catalog entry records OAuth2 is described with the OAuth2 property
/// descriptors a create of it sets: the grant it is obtained by, the provider's token URL,
/// the authorization-code leg (the URL the service supplies, the code and the redirect URI
/// the caller carries back, the scope asked for) and the client application, each naming
/// itself and stating whether a create must carry it.
#[tokio::test]
async fn an_oauth2_connection_type_is_described_with_the_oauth2_property_descriptors() {
    let router = emulator();

    // The members of the create's own OAuth2 shapes, plus the two the service supplies for
    // the authorization-code leg.
    let expected: BTreeSet<String> = [
        "OAuth2GrantType",
        "TokenUrl",
        "AuthorizationCodeUrl",
        "AuthorizationCode",
        "RedirectUri",
        "Scope",
        "UserManagedClientApplicationClientId",
        "UserManagedClientApplicationClientSecret",
        "AWSManagedClientApplicationReference",
    ]
    .iter()
    .map(|key| (*key).to_string())
    .collect();

    // One vendor OAuth2 source the issue cited, one it did not, and one that takes an
    // account's own credentials as well.
    for connection_type in ["ADOBEANALYTICS", "SALESFORCE", "SERVICENOW"] {
        let case = connection_type;
        let body = describe(&router, connection_type).await;
        let properties = &body["AuthenticationConfiguration"]["OAuth2Properties"];
        assert_eq!(
            map_keys(&body["AuthenticationConfiguration"], "OAuth2Properties"),
            expected,
            "case {case}: the OAuth2 descriptors a create of this type sets, got {properties}"
        );

        assert_eq!(
            properties["OAuth2GrantType"]["Required"].as_bool(),
            Some(true),
            "case {case}: a create must state the grant the token is obtained by, got \
             {properties}"
        );
        // The URL the user is sent to is the SERVICE's to supply, not the caller's: it is
        // reported as read-only, so a client does not try to set it.
        assert_eq!(
            string_list(&properties["AuthorizationCodeUrl"], "PropertyTypes"),
            vec!["READ_ONLY".to_string()],
            "case {case}: the authorization-code URL is supplied by the service, got \
             {properties}"
        );
        // The authorization code belongs to one grant, so it is conditional rather than
        // required of every create.
        assert_eq!(
            properties["AuthorizationCode"]["Required"].as_bool(),
            Some(false),
            "case {case}: the authorization code is required only of the AUTHORIZATION_CODE \
             grant, got {properties}"
        );
        assert!(
            string_list(&properties["AuthorizationCode"], "PropertyTypes")
                .iter()
                .any(|property_type| property_type == "SECRET"),
            "case {case}: the authorization code is a credential, got {properties}"
        );
    }
}

/// EVERY type a connection can be created with has exactly one catalog entry, and no entry
/// describes a type the model does not declare: the read is answered for each value of the
/// modelled `ConnectionType` enum, and each answer names the type it was asked about.
#[tokio::test]
async fn every_connection_type_a_connection_can_be_created_with_has_one_catalog_entry() {
    let router = emulator();

    for connection_type in CONNECTION_TYPE_VALUES {
        let body = describe(&router, connection_type).await;
        assert_eq!(
            body["ConnectionType"].as_str(),
            Some(*connection_type),
            "case {connection_type}: the entry must name the type it was asked about, got {body}"
        );
        let description = body["Description"].as_str().unwrap_or_default();
        assert!(
            !description.is_empty(),
            "case {connection_type}: every entry must say what the connection connects to, got \
             {body}"
        );
        // A description written out of the catalog names the data source, not the enum token
        // the request used — except where the token IS what the catalog calls the source
        // (`JDBC`, `SFTP`, `CMDB`).
        if !["JDBC", "SFTP", "CMDB"].contains(connection_type) {
            assert!(
                !description.contains(connection_type),
                "case {connection_type}: the description must name the data source rather than \
                 the requested token, got {description:?}"
            );
        }
    }

    // The converse, read off the catalog itself: it holds ONE row per type and no row for a
    // type the model does not declare, so no row can be shadowed by a second one carrying
    // the same name. The lookup answers the first row of a name, which is why a duplicate
    // would otherwise go unseen.
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for entry in CONNECTION_TYPE_CATALOG {
        assert!(
            CONNECTION_TYPE_VALUES.contains(&entry.connection_type),
            "case {}: the catalog holds a row for a type the model does not declare",
            entry.connection_type
        );
        assert!(
            seen.insert(entry.connection_type),
            "case {}: the catalog holds a second row for a type it already records",
            entry.connection_type
        );
    }
    assert_eq!(
        seen.len(),
        CONNECTION_TYPE_VALUES.len(),
        "the catalog must record every type a connection can be created with, got {} row(s) \
         for {} modelled type(s)",
        seen.len(),
        CONNECTION_TYPE_VALUES.len()
    );
}
