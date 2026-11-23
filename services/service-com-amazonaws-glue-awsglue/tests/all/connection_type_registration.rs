// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `RegisterConnectionType`: registering a CUSTOM, REST-based
//! connection type in this account, and reading it back.
//!
//! The rule under test: a registration persists this account's own connection type under the
//! name it states — the properties a connection of it is created with, the authentication
//! types it accepts with the properties each calls for, and the REST configuration its
//! requests and responses are interpreted by — answers that type's ARN, and is what
//! `DescribeConnectionType` of the type then reports; a name that does not carry the prefix
//! the member requires, or a name already registered, is refused with 400
//! `InvalidInputException` and nothing is stored.
//!
//! The rows of each suite differ along what the rule generalises over: which authentication
//! types the connector accepts and which OAuth2 grant it is configured with, how many
//! request parameters and entities the registration declares, whether it carries a
//! description and tags, and — for the refusals — the spelling, case and length of the name.
//!
//! The member documentation the suites read (`RegisterConnectionTypeRequest$ConnectionType`:
//! "must be prefixed with "REST-"", `$ConnectionProperties`: "defines the base URL and
//! additional request parameters needed during connection creation",
//! `DescribeConnectionTypeResponse$RestConfiguration`: "for REST based data source") comes
//! from `smithy_query(query="operation",operation="RegisterConnectionType,DescribeConnectionType")`
//! on this crate's Smithy model.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{assert_echoed, call, emulator, error_code, error_message, ok, ACCOUNT_ID, REGION};

/// The prefix the registered name must carry ("must be prefixed with "REST-" to indicate it
/// is a REST-based connector" — `RegisterConnectionTypeRequest$ConnectionType`).
const PREFIX: &str = "REST-";

/// The wording a name that is no connection type is refused with, which is also what a read
/// of a name nothing registered answers.
const NOT_AVAILABLE: &str = "The connection type is not available.";

/// The wording a registration under a name already registered is refused with.
const ALREADY_EXISTS: &str = "Connection type already exists.";

/// The key a create of a connection of a custom type states its base URL under: the member
/// of `ConnectionPropertiesConfiguration` that declares it (`Url`, "the base instance URL
/// for the endpoint that this connection type will connect to"), which is one key for every
/// custom type whatever display name its declaration carries.
const BASE_URL_KEY: &str = "URL";

/// The ARN a connection type registered under `name` is addressed by, as served in this
/// harness's Region.
fn expected_arn(name: &str) -> String {
    format!("arn:aws:glue:{REGION}:{ACCOUNT_ID}:connectionType/{name}")
}

/// Register one connection type and return the call's status and body.
async fn register(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(router, "RegisterConnectionType", body).await
}

/// Read one connection type's options.
async fn describe(router: &MultiServiceRouter, connection_type: &str) -> (u16, Value) {
    call(
        router,
        "DescribeConnectionType",
        &json!({ "ConnectionType": connection_type }),
    )
    .await
}

/// The keys of a map member of a response, or an empty set when the member is absent.
fn map_keys(body: &Value, member: &str) -> BTreeSet<String> {
    body.get(member)
        .and_then(Value::as_object)
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default()
}

/// The strings of a list member of a response, or an empty list when the member is absent.
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

/// A connector property declaration: the smallest `ConnectorProperty` the model admits is a
/// `Name`, a `Required` flag and a `PropertyType` (all three required).
fn connector_property(name: &str, required: bool, property_type: &str) -> Value {
    json!({ "Name": name, "Required": required, "PropertyType": property_type })
}

/// One row of the registration table: a whole registration request, and what the read of the
/// registered type must then report.
struct Row {
    /// What the row is about, named in every assertion it makes.
    case: &'static str,
    /// The name the type is registered under.
    connection_type: &'static str,
    /// The authentication types the connector accepts, in the order they are registered.
    authentication_types: &'static [&'static str],
    /// Whether the registration carries a `Description`.
    described: bool,
    /// The display names of the properties a connection of this type is created with: the
    /// base URL declaration's name followed by the additional request parameters' names.
    /// The base URL is reported under the key `URL` whatever this name is.
    connection_property_names: &'static [&'static str],
    /// The entities the REST configuration declares.
    entities: &'static [&'static str],
    /// The tags the registration assigns.
    tags: &'static [(&'static str, &'static str)],
}

/// The whole request a row registers, built so the members a row varies are the only ones
/// that differ between rows.
fn request_for(row: &Row) -> Value {
    let mut connection_properties = json!({
        "Url": {
            "Name": row.connection_property_names[0],
            "Required": true,
            "PropertyType": "USER_INPUT",
            "DefaultValue": "https://api.example.test",
            "PropertyLocation": "PATH",
        },
    });
    let additional: Vec<Value> = row.connection_property_names[1..]
        .iter()
        .enumerate()
        .map(|(index, name)| {
            json!({
                "Name": name,
                "KeyOverride": format!("x_{index}"),
                "Required": index % 2 == 0,
                "PropertyType": "USER_INPUT",
                "PropertyLocation": "QUERY_PARAM",
                "AllowedValues": ["one", "two"],
            })
        })
        .collect();
    if !additional.is_empty() {
        connection_properties["AdditionalRequestParameters"] = json!(additional);
    }

    let mut authentication = json!({ "AuthenticationTypes": row.authentication_types });
    if row.authentication_types.contains(&"BASIC") {
        authentication["BasicAuthenticationProperties"] = json!({
            "Username": connector_property("Username", true, "USER_INPUT"),
            "Password": connector_property("Password", true, "SECRET"),
        });
    }
    if row.authentication_types.contains(&"CUSTOM") {
        authentication["CustomAuthenticationProperties"] = json!({
            "AuthenticationParameters": [
                connector_property("apiKey", true, "SECRET"),
                connector_property("tenant", false, "USER_INPUT"),
            ],
        });
    }
    if row.authentication_types.contains(&"OAUTH2") {
        // One grant per registration, as `ConnectorOAuth2Properties$OAuth2GrantType` is
        // required: the rows below register two different ones.
        authentication["OAuth2Properties"] = if row.case.contains("JWT") {
            json!({
                "OAuth2GrantType": "JWT_BEARER",
                "JWTBearerProperties": {
                    "TokenUrl": connector_property("TokenUrl", true, "USER_INPUT"),
                    "JwtToken": connector_property("JwtToken", true, "SECRET"),
                },
            })
        } else {
            json!({
                "OAuth2GrantType": "CLIENT_CREDENTIALS",
                "ClientCredentialsProperties": {
                    "TokenUrl": connector_property("TokenUrl", true, "USER_INPUT"),
                    "ClientId": connector_property("ClientId", true, "USER_INPUT"),
                    "ClientSecret": connector_property("ClientSecret", true, "SECRET"),
                },
            })
        };
    }

    let entity_configurations: serde_json::Map<String, Value> = row
        .entities
        .iter()
        .map(|entity| {
            (
                (*entity).to_string(),
                json!({
                    "SourceConfiguration": {
                        "RequestMethod": "GET",
                        "RequestPath": format!("/v1/{entity}"),
                        "ResponseConfiguration": { "ResultPath": "$.data" },
                    },
                    "Schema": {
                        "id": { "Name": "id", "FieldDataType": "STRING", "IsQueryable": true },
                    },
                }),
            )
        })
        .collect();
    let mut rest_configuration = json!({
        "GlobalSourceConfiguration": {
            "RequestMethod": "GET",
            "RequestPath": "/v1",
            "ResponseConfiguration": { "ResultPath": "$.records", "ErrorPath": "$.errors" },
        },
        "ValidationEndpointConfiguration": {
            "RequestMethod": "GET",
            "RequestPath": "/v1/ping",
            "ResponseConfiguration": { "ResultPath": "$.status" },
        },
    });
    if !entity_configurations.is_empty() {
        rest_configuration["EntityConfigurations"] = Value::Object(entity_configurations);
    }

    let mut request = json!({
        "ConnectionType": row.connection_type,
        "IntegrationType": "REST",
        "ConnectionProperties": connection_properties,
        "ConnectorAuthenticationConfiguration": authentication,
        "RestConfiguration": rest_configuration,
    });
    if row.described {
        request["Description"] = json!(format!("the {} data source", row.connection_type));
    }
    if !row.tags.is_empty() {
        request["Tags"] = Value::Object(
            row.tags
                .iter()
                .map(|(key, value)| ((*key).to_string(), json!(value)))
                .collect(),
        );
    }
    request
}

/// One row per axis the rule generalises over: the authentication kinds the connector
/// accepts (basic, custom, each OAuth2 grant, several at once, none at all), whether it is
/// described, how many request parameters it declares (one, two, three) and how many
/// entities (none, one, two), and whether it is tagged.
const ROWS: &[Row] = &[
    Row {
        case: "basic auth, one property, no entity, untagged",
        connection_type: "REST-basic-source",
        authentication_types: &["BASIC"],
        described: false,
        connection_property_names: &["ENDPOINT"],
        entities: &[],
        tags: &[],
    },
    Row {
        case: "OAuth2 client credentials, two properties, one entity, described",
        connection_type: "REST-oauth-source",
        authentication_types: &["OAUTH2"],
        described: true,
        connection_property_names: &["ENDPOINT", "ENDPOINT_TYPE"],
        entities: &["Accounts"],
        tags: &[("team", "data")],
    },
    Row {
        case: "OAuth2 JWT bearer grant, three properties, two entities",
        connection_type: "REST-jwt-source",
        authentication_types: &["OAUTH2"],
        described: true,
        connection_property_names: &["ENDPOINT", "ENDPOINT_TYPE", "INSTANCE_ID"],
        entities: &["Accounts", "Contacts"],
        tags: &[("team", "data"), ("tier", "gold")],
    },
    Row {
        case: "custom auth alone",
        connection_type: "REST-custom-auth-source",
        authentication_types: &["CUSTOM"],
        described: true,
        connection_property_names: &["ENDPOINT", "TENANT"],
        entities: &["Invoices"],
        tags: &[],
    },
    Row {
        case: "three authentication kinds at once",
        connection_type: "REST-multi-auth-source",
        authentication_types: &["BASIC", "OAUTH2", "CUSTOM"],
        described: true,
        connection_property_names: &["ENDPOINT"],
        entities: &["Orders"],
        tags: &[("owner", "platform")],
    },
    Row {
        case: "no authentication type at all",
        connection_type: "REST-open-source",
        authentication_types: &[],
        described: false,
        connection_property_names: &["ENDPOINT", "REGION"],
        entities: &[],
        tags: &[],
    },
];

/// Every registration is persisted and reported back: the call answers the type's ARN, and
/// the read of that type answers the registration — its description, the authentication types
/// it accepts with the properties each of them declared, the properties a connection of it is
/// created with, and its REST configuration.
#[tokio::test]
async fn a_registered_connection_type_is_described_as_it_was_registered() {
    let router = emulator();

    for row in ROWS {
        let case = row.case;
        let request = request_for(row);
        let (status, body) = register(&router, &request).await;
        assert_eq!(
            status, 200,
            "case {case}: RegisterConnectionType must answer 200, got {status} with {body}"
        );
        assert_eq!(
            body["ConnectionTypeArn"].as_str(),
            Some(expected_arn(row.connection_type).as_str()),
            "case {case}: the registration must answer the type's own ARN, got {body}"
        );

        let (status, read) = describe(&router, row.connection_type).await;
        assert_eq!(
            status, 200,
            "case {case}: the registered type must be described, got {status} with {read}"
        );
        assert_eq!(
            read["ConnectionType"].as_str(),
            Some(row.connection_type),
            "case {case}: the read must name the registered type, got {read}"
        );
        assert_eq!(
            read.get("Description").and_then(Value::as_str),
            request.get("Description").and_then(Value::as_str),
            "case {case}: the read must report the registered Description, got {read}"
        );
        assert_eq!(
            list_of(&read["Capabilities"], "SupportedAuthenticationTypes"),
            row.authentication_types,
            "case {case}: the read must report the registered authentication types, got {read}"
        );

        // "Defines the base URL and additional request parameters needed during connection
        // creation for this connection type": those declarations are the properties a
        // connection of the type is created with. A request parameter is keyed by its own
        // name; the base URL is keyed by the key a create states it under (`URL`), whatever
        // display name its declaration carries.
        assert_eq!(
            map_keys(&read, "ConnectionProperties"),
            std::iter::once(BASE_URL_KEY.to_string())
                .chain(
                    row.connection_property_names[1..]
                        .iter()
                        .map(|name| (*name).to_string())
                )
                .collect::<BTreeSet<String>>(),
            "case {case}: the read must report the declared connection properties, got {read}"
        );
        let declared_url = &request["ConnectionProperties"]["Url"];
        let reported_url = &read["ConnectionProperties"][BASE_URL_KEY];
        for member in ["Name", "Required", "DefaultValue", "PropertyLocation"] {
            assert_eq!(
                reported_url.get(member),
                declared_url.get(member),
                "case {case}: the read must carry the declared {member} of the base URL \
                 property, got {reported_url}"
            );
        }
        assert_eq!(
            list_of(reported_url, "PropertyTypes"),
            vec![declared_url["PropertyType"].as_str().unwrap_or_default()],
            "case {case}: a declared property's type must be reported, got {reported_url}"
        );
        for (index, name) in row.connection_property_names[1..].iter().enumerate() {
            let declared = &request["ConnectionProperties"]["AdditionalRequestParameters"][index];
            let reported = &read["ConnectionProperties"][*name];
            assert_eq!(
                reported.get("KeyOverride"),
                declared.get("KeyOverride"),
                "case {case}: the key a parameter is sent under must be reported, got {reported}"
            );
            assert_eq!(
                reported.get("Required"),
                declared.get("Required"),
                "case {case}: whether parameter {name} is required must be reported, got \
                 {reported}"
            );
            assert_eq!(
                reported
                    .get("AllowedValues")
                    .and_then(Value::as_array)
                    .map(|values| values
                        .iter()
                        .map(|value| value["Value"].as_str().unwrap_or_default().to_string())
                        .collect::<Vec<String>>()),
                Some(vec!["one".to_string(), "two".to_string()]),
                "case {case}: the values parameter {name} admits must be reported, got {reported}"
            );
        }

        // "HTTP request and response configuration, validation endpoint, and entity
        // configurations for REST based data source": the one member only a registered type
        // carries, reported as it was registered.
        assert_echoed(
            case,
            &request["RestConfiguration"],
            read.get("RestConfiguration").unwrap_or(&Value::Null),
        );

        assert_authentication(row, &request, &read);
    }
}

/// Assert that the credentials the read reports are the ones the registration declared: one
/// descriptor map per authentication kind the registration configured, keyed by the names
/// that registration gave its properties.
fn assert_authentication(row: &Row, request: &Value, read: &Value) {
    let case = row.case;
    let configuration = read.get("AuthenticationConfiguration");
    if row.authentication_types.is_empty() {
        assert!(
            configuration.is_none(),
            "case {case}: a type that accepts no authentication type must report no \
             AuthenticationConfiguration, got {read}"
        );
        return;
    }
    let configuration = configuration.unwrap_or_else(|| {
        panic!("case {case}: a type with credentials must report how they are supplied, got {read}")
    });
    let admitted: Vec<String> = configuration["AuthenticationType"]["AllowedValues"]
        .as_array()
        .unwrap_or_else(|| {
            panic!("case {case}: the AuthenticationType property must state the kinds it admits, got {configuration}")
        })
        .iter()
        .map(|value| value["Value"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        admitted, row.authentication_types,
        "case {case}: the kinds admitted must be the registered ones, got {configuration}"
    );

    let declared = &request["ConnectorAuthenticationConfiguration"];
    if row.authentication_types.contains(&"BASIC") {
        assert_eq!(
            map_keys(configuration, "BasicAuthenticationProperties"),
            ["Password", "Username"]
                .iter()
                .map(|key| (*key).to_string())
                .collect::<BTreeSet<String>>(),
            "case {case}: the basic credentials declared must be reported, got {configuration}"
        );
        assert_eq!(
            list_of(
                &configuration["BasicAuthenticationProperties"]["Password"],
                "PropertyTypes"
            ),
            vec!["SECRET".to_string()],
            "case {case}: a credential declared as a secret must be reported as one, got \
             {configuration}"
        );
    } else {
        assert!(
            configuration.get("BasicAuthenticationProperties").is_none(),
            "case {case}: a type that declared no basic credentials must report none, got \
             {configuration}"
        );
    }
    if row.authentication_types.contains(&"CUSTOM") {
        assert_eq!(
            map_keys(configuration, "CustomAuthenticationProperties"),
            ["apiKey", "tenant"]
                .iter()
                .map(|key| (*key).to_string())
                .collect::<BTreeSet<String>>(),
            "case {case}: the custom credentials declared must be reported, got {configuration}"
        );
    } else {
        assert!(
            configuration.get("CustomAuthenticationProperties").is_none(),
            "case {case}: a type that declared no custom credentials must report none, got \
             {configuration}"
        );
    }
    if row.authentication_types.contains(&"OAUTH2") {
        let reported = map_keys(configuration, "OAuth2Properties");
        let grant = declared["OAuth2Properties"]["OAuth2GrantType"]
            .as_str()
            .unwrap_or_default();
        let mut expected: BTreeSet<String> = ["OAuth2GrantType".to_string()].into_iter().collect();
        if grant == "JWT_BEARER" {
            expected.extend(["TokenUrl".to_string(), "JwtToken".to_string()]);
        } else {
            expected.extend([
                "TokenUrl".to_string(),
                "ClientId".to_string(),
                "ClientSecret".to_string(),
            ]);
        }
        assert_eq!(
            reported, expected,
            "case {case}: the OAuth2 properties of the registered {grant} grant must be \
             reported, got {configuration}"
        );
        assert_eq!(
            configuration["OAuth2Properties"]["OAuth2GrantType"]["AllowedValues"][0]["Value"]
                .as_str(),
            Some(grant),
            "case {case}: the grant property must admit the registered grant, got {configuration}"
        );
    } else {
        assert!(
            configuration.get("OAuth2Properties").is_none(),
            "case {case}: a type that declared no OAuth2 properties must report none, got \
             {configuration}"
        );
    }
}

/// A name that does not carry the prefix the member requires registers nothing: the call is
/// refused with 400 `InvalidInputException`, and a read of that name is still refused as a
/// type the service does not have.
///
/// The rows differ along the spelling of the name: the prefix in the wrong case, with the
/// wrong separator, as the bare word, offset by a space, a name of a connector the service
/// itself ships, a single character and a name at the member's documented 255-character
/// ceiling (`RegisterConnectionTypeRequest$ConnectionType` targets `NameString`, length
/// 1..255).
#[tokio::test]
async fn a_name_without_the_required_prefix_registers_nothing() {
    let router = emulator();

    let names = vec![
        ("no prefix".to_string(), "MyConnector".to_string()),
        ("lowercase prefix".to_string(), "rest-source".to_string()),
        ("mixed case prefix".to_string(), "Rest-source".to_string()),
        ("underscore separator".to_string(), "REST_source".to_string()),
        ("bare word".to_string(), "REST".to_string()),
        ("leading space".to_string(), " REST-source".to_string()),
        ("a shipped connector".to_string(), "SALESFORCE".to_string()),
        ("single character".to_string(), "x".to_string()),
        ("at the 255 ceiling".to_string(), "T".repeat(255)),
    ];

    for (case, name) in &names {
        let mut request = request_for(&ROWS[0]);
        request["ConnectionType"] = json!(name);
        let (status, body) = register(&router, &request).await;
        assert_eq!(
            status, 400,
            "case {case:?} ({name:?}): expected 400, got {status} with {body}"
        );
        assert!(
            error_code(&body).ends_with("InvalidInputException"),
            "case {case:?} ({name:?}): expected InvalidInputException, got {}",
            error_code(&body)
        );
        assert!(
            error_message(&body).contains(PREFIX),
            "case {case:?} ({name:?}): the refusal must state the prefix a name must carry, \
             got {:?}",
            error_message(&body)
        );

        // Nothing was registered under that name: a read of it is either refused as a type
        // the service does not have, or — for the name of a connector the service itself
        // ships — answers that shipped connector, which carries no registered REST
        // configuration.
        let (status, read) = describe(&router, name).await;
        if status == 200 {
            assert!(
                read.get("RestConfiguration").is_none(),
                "case {case:?} ({name:?}): a refused registration must leave no registered \
                 configuration to read, got {read}"
            );
        } else {
            assert_eq!(
                status, 400,
                "case {case:?} ({name:?}): a refused registration must leave nothing to read, \
                 got {status} with {read}"
            );
            assert_eq!(
                error_message(&read),
                NOT_AVAILABLE,
                "case {case:?} ({name:?}): expected Message {NOT_AVAILABLE:?}, got {:?}",
                error_message(&read)
            );
        }
    }

    // The same registration under a name that does carry the prefix is accepted, so the
    // refusals above are about the name alone.
    let mut request = request_for(&ROWS[0]);
    request["ConnectionType"] = json!("REST-accepted");
    let body = ok(&router, "RegisterConnectionType", &request).await;
    assert_eq!(
        body["ConnectionTypeArn"].as_str(),
        Some(expected_arn("REST-accepted").as_str()),
        "a name carrying the prefix must still be registered, got {body}"
    );
}

/// A second registration under a name already registered is refused, and the configuration
/// already stored is left exactly as it was — the refused call replaces nothing.
#[tokio::test]
async fn a_name_already_registered_is_refused_and_changes_nothing() {
    let router = emulator();

    let first = request_for(&ROWS[1]);
    let name = ROWS[1].connection_type;
    ok(&router, "RegisterConnectionType", &first).await;
    let before = ok(
        &router,
        "DescribeConnectionType",
        &json!({ "ConnectionType": name }),
    )
    .await;

    // A SECOND registration of the same name, configured differently in every member a
    // registration carries.
    let mut second = request_for(&ROWS[4]);
    second["ConnectionType"] = json!(name);
    let (status, body) = register(&router, &second).await;
    assert_eq!(
        status, 400,
        "a name already registered must be refused, got {status} with {body}"
    );
    assert!(
        error_code(&body).ends_with("InvalidInputException"),
        "expected InvalidInputException, got {}",
        error_code(&body)
    );
    assert_eq!(
        error_message(&body),
        ALREADY_EXISTS,
        "expected Message {ALREADY_EXISTS:?}, got {:?}",
        error_message(&body)
    );

    let after = ok(
        &router,
        "DescribeConnectionType",
        &json!({ "ConnectionType": name }),
    )
    .await;
    assert_eq!(
        after, before,
        "case refused duplicate: the registered configuration must be unchanged, got {after}"
    );
}

/// The tags a registration assigns are the type's own: the tagging surface reads them back
/// through the ARN the registration answered, and a tag written there afterwards is reported
/// by the same read.
#[tokio::test]
async fn the_tags_a_registration_assigns_are_read_back_by_its_arn() {
    let router = emulator();

    for row in ROWS {
        let case = row.case;
        let body = ok(&router, "RegisterConnectionType", &request_for(row)).await;
        let arn = body["ConnectionTypeArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: the registration must answer an ARN, got {body}"))
            .to_string();

        let tags = ok(&router, "GetTags", &json!({ "ResourceArn": arn })).await;
        let expected: Value = Value::Object(
            row.tags
                .iter()
                .map(|(key, value)| ((*key).to_string(), json!(value)))
                .collect(),
        );
        assert_eq!(
            tags["Tags"], expected,
            "case {case}: the tags the registration assigned must be read back, got {tags}"
        );
    }

    // A tag written after the registration lands on the same record.
    let arn = expected_arn(ROWS[0].connection_type);
    ok(
        &router,
        "TagResource",
        &json!({ "ResourceArn": arn, "TagsToAdd": { "stage": "test" } }),
    )
    .await;
    let tags = ok(&router, "GetTags", &json!({ "ResourceArn": arn })).await;
    assert_eq!(
        tags["Tags"]["stage"].as_str(),
        Some("test"),
        "a tag written on a registered connection type must be reported, got {tags}"
    );
}

/// Registering a custom type adds to what the service serves rather than replacing it: the
/// connectors the service itself ships are still described as before, a name nothing
/// registered is still refused, and a registered type is described independently of them.
#[tokio::test]
async fn registering_a_custom_type_leaves_the_shipped_connectors_alone() {
    let router = emulator();

    let shipped_before = ok(
        &router,
        "DescribeConnectionType",
        &json!({ "ConnectionType": "SALESFORCE" }),
    )
    .await;

    ok(&router, "RegisterConnectionType", &request_for(&ROWS[1])).await;

    let shipped_after = ok(
        &router,
        "DescribeConnectionType",
        &json!({ "ConnectionType": "SALESFORCE" }),
    )
    .await;
    assert_eq!(
        shipped_after, shipped_before,
        "case shipped connector: a registration must not change what the service ships, got \
         {shipped_after}"
    );
    assert!(
        shipped_after.get("RestConfiguration").is_none(),
        "case shipped connector: a connector the service ships carries no registered REST \
         configuration, got {shipped_after}"
    );

    let (status, unregistered) = describe(&router, "REST-nothing-registered").await;
    assert_eq!(
        status, 400,
        "case unregistered name: expected 400, got {status} with {unregistered}"
    );
    assert_eq!(
        error_message(&unregistered),
        NOT_AVAILABLE,
        "case unregistered name: expected Message {NOT_AVAILABLE:?}, got {:?}",
        error_message(&unregistered)
    );
}
