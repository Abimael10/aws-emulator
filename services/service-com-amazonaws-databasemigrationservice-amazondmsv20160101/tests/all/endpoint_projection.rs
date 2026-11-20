// SPDX-License-Identifier: CC0-1.0

//! The `Endpoint` projection every DMS operation that answers with an endpoint goes
//! through: `CreateEndpoint`, `ModifyEndpoint`, `DescribeEndpoints` and `DeleteEndpoint`.
//!
//! Two rules are asserted here, each over a SET of inputs:
//!
//! 1. An `Endpoint` reports every member the stored endpoint determines —
//!    `EndpointType` in the uppercase form a response carries, the expanded
//!    `EngineDisplayName` for its `EngineName`, `IsReadOnly`, the encryption key (the
//!    region's default DMS key when the request named none) and the single engine settings
//!    structure `EngineName` selects, carrying the connection parameters the request
//!    supplied — and reports them identically on every such operation.
//! 2. An endpoint holds the LAST value DMS accepted for each of its members: a modify
//!    writes every member it supplies to the endpoint, leaves every member it omits as it
//!    was, and folds a settings bag it carries onto the stored one setting by setting
//!    (wholesale when the call asks for exact settings) — so the modify response, a later
//!    describe and the endpoint a delete echoes all report the endpoint as last modified.
//!
//! Each case drives the real wire path through the router, so what is asserted is what
//! a client sees.

use serde_json::{json, Value};

use crate::migration_task_slice::{error_code, text, Emulator, ACCOUNT, REGION};

/// A password every request that carries one uses, so a case can assert it appears
/// nowhere in any response.
const SECRET: &str = "never-echoed-secret";

/// One endpoint the projection is asserted over.
struct Case {
    /// What this row varies.
    name: &'static str,
    /// The `EngineName` the request carries.
    engine_name: &'static str,
    /// The `EndpointType` the request carries, in the lowercase request form.
    endpoint_type: &'static str,
    /// The `EngineDisplayName` the response must carry.
    display_name: &'static str,
    /// The one `Endpoint` member that must carry an engine settings structure.
    settings_member: &'static str,
    /// Request members beyond `EndpointIdentifier`/`EndpointType`/`EngineName`.
    extra: Value,
    /// The engine settings structure the response must report, member for member: a
    /// member this does not name must not be invented either.
    expected_settings: Value,
}

/// The engine display names and settings structures the rows below expect come from
/// `Endpoint$EngineDisplayName` ("The expanded name for the engine name. For example,
/// if the EngineName parameter is \"aurora\", this value would be \"Amazon Aurora
/// MySQL\"") and from the per-engine settings members `Endpoint` models, over the
/// `EngineName` values `CreateEndpoint$EngineName` publishes.
fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "mysql source, connection parameters at top level",
            engine_name: "mysql",
            endpoint_type: "source",
            display_name: "MySQL",
            settings_member: "MySQLSettings",
            extra: json!({
                "ServerName": "db.example.internal",
                "Port": 3306,
                "DatabaseName": "hr",
                "Username": "reader",
                "Password": SECRET,
            }),
            expected_settings: json!({
                "ServerName": "db.example.internal",
                "Port": 3306,
                "DatabaseName": "hr",
                "Username": "reader",
                "AuthenticationMethod": "password",
            }),
        },
        Case {
            name: "mysql source, no connection parameters at all",
            engine_name: "mysql",
            endpoint_type: "source",
            display_name: "MySQL",
            settings_member: "MySQLSettings",
            extra: json!({}),
            expected_settings: json!({ "AuthenticationMethod": "password" }),
        },
        Case {
            name: "mysql target, parameters in the settings member, iam authentication",
            engine_name: "mysql",
            endpoint_type: "target",
            display_name: "MySQL",
            settings_member: "MySQLSettings",
            extra: json!({
                "ServerName": "top-level.example.internal",
                "MySQLSettings": {
                    "ServerName": "settings.example.internal",
                    "Port": 3307,
                    "Username": "writer",
                    "Password": SECRET,
                    // `MySQLAuthenticationMethod` models password | iam.
                    "AuthenticationMethod": "iam",
                    "ServerTimezone": "US/Pacific",
                },
            }),
            expected_settings: json!({
                // What the settings member carries wins over the top-level member.
                "ServerName": "settings.example.internal",
                "Port": 3307,
                "Username": "writer",
                "AuthenticationMethod": "iam",
                "ServerTimezone": "US/Pacific",
            }),
        },
        Case {
            // The example the `EngineDisplayName` doc itself gives, and an engine whose
            // settings structure is MySQL's.
            name: "aurora source",
            engine_name: "aurora",
            endpoint_type: "source",
            display_name: "Amazon Aurora MySQL",
            settings_member: "MySQLSettings",
            extra: json!({ "ServerName": "aurora.example.internal", "Port": 3306 }),
            expected_settings: json!({
                "ServerName": "aurora.example.internal",
                "Port": 3306,
                "AuthenticationMethod": "password",
            }),
        },
        Case {
            name: "mariadb target",
            engine_name: "mariadb",
            endpoint_type: "target",
            display_name: "MariaDB",
            settings_member: "MySQLSettings",
            extra: json!({ "ServerName": "maria.example.internal", "Username": "loader" }),
            expected_settings: json!({
                "ServerName": "maria.example.internal",
                "Username": "loader",
                "AuthenticationMethod": "password",
            }),
        },
        Case {
            name: "postgres target",
            engine_name: "postgres",
            endpoint_type: "target",
            display_name: "PostgreSQL",
            settings_member: "PostgreSQLSettings",
            extra: json!({
                "ServerName": "warehouse.example.internal",
                "Port": 5432,
                "DatabaseName": "analytics",
                "Username": "etl",
                "Password": SECRET,
            }),
            expected_settings: json!({
                "ServerName": "warehouse.example.internal",
                "Port": 5432,
                "DatabaseName": "analytics",
                "Username": "etl",
                "AuthenticationMethod": "password",
            }),
        },
        Case {
            name: "oracle source",
            engine_name: "oracle",
            endpoint_type: "source",
            display_name: "Oracle",
            settings_member: "OracleSettings",
            extra: json!({
                "ServerName": "ora.example.internal",
                "Port": 1521,
                "DatabaseName": "ORCL",
                "Username": "dms",
                "Password": SECRET,
            }),
            expected_settings: json!({
                "ServerName": "ora.example.internal",
                "Port": 1521,
                "DatabaseName": "ORCL",
                "Username": "dms",
                "AuthenticationMethod": "password",
            }),
        },
        Case {
            name: "sqlserver source, caller-named encryption key",
            engine_name: "sqlserver",
            endpoint_type: "source",
            display_name: "Microsoft SQL Server",
            settings_member: "MicrosoftSQLServerSettings",
            extra: json!({
                "ServerName": "mssql.example.internal",
                "Port": 1433,
                "KmsKeyId": "arn:aws:kms:us-east-1:000000000000:key/caller-named-key",
            }),
            expected_settings: json!({
                "ServerName": "mssql.example.internal",
                "Port": 1433,
                "AuthenticationMethod": "password",
            }),
        },
        Case {
            name: "db2 source",
            engine_name: "db2",
            endpoint_type: "source",
            display_name: "IBM Db2 LUW",
            settings_member: "IBMDb2Settings",
            extra: json!({
                "ServerName": "db2.example.internal",
                "Port": 50000,
                "DatabaseName": "SAMPLE",
                "Username": "db2inst1",
            }),
            expected_settings: json!({
                "ServerName": "db2.example.internal",
                "Port": 50000,
                "DatabaseName": "SAMPLE",
                "Username": "db2inst1",
            }),
        },
        Case {
            name: "mongodb source",
            engine_name: "mongodb",
            endpoint_type: "source",
            display_name: "MongoDB",
            settings_member: "MongoDbSettings",
            extra: json!({
                "ServerName": "mongo.example.internal",
                "Port": 27017,
                "DatabaseName": "catalog",
                "Username": "reader",
                // `AuthMechanismValue` models default | mongodb_cr | scram_sha_1.
                "MongoDbSettings": { "AuthMechanism": "scram_sha_1", "Password": SECRET },
            }),
            expected_settings: json!({
                "ServerName": "mongo.example.internal",
                "Port": 27017,
                "DatabaseName": "catalog",
                "Username": "reader",
                "AuthMechanism": "scram_sha_1",
            }),
        },
        Case {
            // An engine whose settings structure is nothing like a database
            // connection: it is reported as the request supplied it.
            name: "s3 target",
            engine_name: "s3",
            endpoint_type: "target",
            display_name: "Amazon S3",
            settings_member: "S3Settings",
            extra: json!({
                "S3Settings": { "BucketName": "landing-zone", "BucketFolder": "cdc" },
            }),
            expected_settings: json!({ "BucketName": "landing-zone", "BucketFolder": "cdc" }),
        },
        Case {
            name: "dynamodb target",
            engine_name: "dynamodb",
            endpoint_type: "target",
            display_name: "Amazon DynamoDB",
            settings_member: "DynamoDbSettings",
            extra: json!({
                "DynamoDbSettings": {
                    "ServiceAccessRoleArn": "arn:aws:iam::000000000000:role/dms-dynamodb",
                },
            }),
            expected_settings: json!({
                "ServiceAccessRoleArn": "arn:aws:iam::000000000000:role/dms-dynamodb",
            }),
        },
        Case {
            // `NeptuneSettings` models required members, so it is reported exactly as
            // the request supplied it and the endpoint's own connection parameters are
            // not merged into it.
            name: "neptune target",
            engine_name: "neptune",
            endpoint_type: "target",
            display_name: "Amazon Neptune",
            settings_member: "NeptuneSettings",
            extra: json!({
                "ServerName": "ignored-by-neptune.example.internal",
                "NeptuneSettings": {
                    "S3BucketName": "neptune-staging",
                    "S3BucketFolder": "bulk",
                    "MaxFileSize": 1024,
                },
            }),
            expected_settings: json!({
                "S3BucketName": "neptune-staging",
                "S3BucketFolder": "bulk",
                "MaxFileSize": 1024,
            }),
        },
    ]
}

/// The engine settings members an `Endpoint` body carries.
fn settings_members(endpoint: &Value) -> Vec<String> {
    endpoint
        .as_object()
        .map(|members| {
            members
                .keys()
                .filter(|name| name.ends_with("Settings"))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// Assert the engine settings structure reports exactly `expected` — every member it
/// must carry, and no member the projection would have had to invent.
fn assert_settings(case: &str, member: &str, actual: &Value, expected: &Value) {
    assert_eq!(
        actual, expected,
        "case {case:?}: expected {member} to report exactly {expected}, got {actual}"
    );
}

/// Whether `key_id` has the form of a KMS key id: five hyphen-separated hex groups of
/// 8-4-4-4-12 characters.
fn is_key_id(key_id: &str) -> bool {
    let groups: Vec<&str> = key_id.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .map(|group| group.len())
            .eq([8, 4, 4, 4, 12])
        && groups
            .iter()
            .all(|group| group.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

#[tokio::test]
async fn endpoint_projection_reports_every_member_the_record_determines() {
    let emulator = Emulator::new();
    let default_key_prefix = format!("arn:aws:kms:{REGION}:{ACCOUNT}:key/");
    let mut default_keys: Vec<String> = Vec::new();

    for (index, case) in cases().into_iter().enumerate() {
        let identifier = format!("ep-{index}");
        let mut request = json!({
            "EndpointIdentifier": identifier,
            "EndpointType": case.endpoint_type,
            "EngineName": case.engine_name,
        });
        for (key, value) in case
            .extra
            .as_object()
            .expect("the table's request members are objects")
        {
            request[key] = value.clone();
        }

        let created = emulator.ok(case.name, "CreateEndpoint", &request).await;
        let endpoint = created
            .get("Endpoint")
            .unwrap_or(&Value::Null)
            .clone();

        // EndpointType comes back uppercased, whatever case the request used.
        assert_eq!(
            text(&endpoint, "/EndpointType"),
            case.endpoint_type.to_uppercase(),
            "case {:?}: expected EndpointType uppercased, got {endpoint}",
            case.name
        );
        assert_eq!(
            text(&endpoint, "/EngineDisplayName"),
            case.display_name,
            "case {:?}: expected the expanded engine name, got {endpoint}",
            case.name
        );
        assert_eq!(
            endpoint.get("IsReadOnly").and_then(Value::as_bool),
            Some(false),
            "case {:?}: expected an endpoint the caller created to report IsReadOnly \
             false, got {endpoint}",
            case.name
        );

        // The encryption key: the caller's when the request named one, otherwise the
        // region's default DMS key, which every endpoint in the region shares.
        let kms_key_id = text(&endpoint, "/KmsKeyId");
        match case.extra.get("KmsKeyId").and_then(Value::as_str) {
            Some(named) => assert_eq!(
                kms_key_id, named,
                "case {:?}: expected the named KMS key, got {endpoint}",
                case.name
            ),
            None => {
                let key_id = kms_key_id
                    .strip_prefix(&default_key_prefix)
                    .unwrap_or_default();
                assert!(
                    is_key_id(key_id),
                    "case {:?}: expected the region's default DMS key under \
                     {default_key_prefix}, got {kms_key_id:?}",
                    case.name
                );
                default_keys.push(kms_key_id.clone());
            }
        }

        // Exactly one engine settings structure, the one EngineName selects.
        assert_eq!(
            settings_members(&endpoint),
            vec![case.settings_member.to_string()],
            "case {:?}: expected only {} on the endpoint, got {endpoint}",
            case.name,
            case.settings_member
        );
        assert_settings(
            case.name,
            case.settings_member,
            endpoint
                .get(case.settings_member)
                .unwrap_or(&Value::Null),
            &case.expected_settings,
        );

        // No password a request carried is ever reported back.
        assert!(
            !created.to_string().contains(SECRET),
            "case {:?}: a response must never echo a password, got {created}",
            case.name
        );

        // The describe path answers from the same projection, member for member.
        let described = emulator
            .ok(
                case.name,
                "DescribeEndpoints",
                &json!({
                    "Filters": [{ "Name": "endpoint-id", "Values": [identifier] }],
                }),
            )
            .await;
        assert_eq!(
            described.pointer("/Endpoints/0"),
            Some(&endpoint),
            "case {:?}: expected DescribeEndpoints to report the endpoint CreateEndpoint \
             returned, got {described}",
            case.name
        );
    }

    // One default key per region, so two endpoints in the same region report the same
    // one and a read repeated later reports it again.
    assert!(
        default_keys.len() > 1 && default_keys.windows(2).all(|pair| pair[0] == pair[1]),
        "expected every endpoint in {REGION} to share the region's default DMS key, got \
         {default_keys:?}"
    );
}

/// One `ModifyEndpoint` case: what the endpoint was created with, what the modify
/// supplies, and the `Endpoint` members the modify has to change.
struct ModifyCase {
    /// What this row varies.
    name: &'static str,
    /// Request members `CreateEndpoint` carries beyond `EndpointIdentifier`.
    create: Value,
    /// Request members `ModifyEndpoint` carries beyond `EndpointArn`.
    modify: Value,
    /// The `Endpoint` members whose value the modify has to change, each given in FULL
    /// (an engine settings member as the whole structure it must then report); `null`
    /// means the member has to be absent afterwards. Every member this does not name has
    /// to report exactly what the create reported.
    expected: Value,
}

/// The rows below vary along what the rule generalises over: which kind of member the
/// modify supplies (a string, an integer, an enum, an engine settings bag), where the
/// endpoint carried it (its own member or the bag's), whether the call asks for exact
/// settings (`ExactSettings`), and which engine the endpoint runs — one row per shape of
/// change rather than one per endpoint.
fn modify_cases() -> Vec<ModifyCase> {
    vec![
        ModifyCase {
            name: "a connection parameter the endpoint carries at the top level",
            create: json!({
                "EndpointType": "source",
                "EngineName": "mysql",
                "ServerName": "a.example.internal",
                "Port": 3306,
                "DatabaseName": "alpha",
            }),
            modify: json!({ "DatabaseName": "beta" }),
            expected: json!({
                "DatabaseName": "beta",
                // The projection reports the endpoint's own value for a parameter the
                // settings bag does not carry, so the bag follows the record.
                "MySQLSettings": {
                    "ServerName": "a.example.internal",
                    "Port": 3306,
                    "DatabaseName": "beta",
                    "AuthenticationMethod": "password",
                },
            }),
        },
        ModifyCase {
            name: "a settings bag folds onto the stored one, setting by setting",
            create: json!({
                "EndpointType": "source",
                "EngineName": "mysql",
                "ServerName": "b.example.internal",
                "Port": 3306,
                "MySQLSettings": { "DatabaseName": "hrdb", "ServerTimezone": "UTC" },
            }),
            modify: json!({
                "MySQLSettings": {
                    "DatabaseName": "hrdb-2",
                    "MaxFileSize": 2048,
                    "Password": SECRET,
                },
            }),
            expected: json!({
                "MySQLSettings": {
                    // Replaced: the call names it.
                    "DatabaseName": "hrdb-2",
                    // Created: the call names a setting the endpoint did not carry.
                    "MaxFileSize": 2048,
                    // Kept: the call names no value for it.
                    "ServerTimezone": "UTC",
                    "ServerName": "b.example.internal",
                    "Port": 3306,
                    "AuthenticationMethod": "password",
                },
            }),
        },
        ModifyCase {
            name: "ExactSettings replaces the stored bag with the call's own settings",
            create: json!({
                "EndpointType": "target",
                "EngineName": "mysql",
                "ServerName": "c.example.internal",
                "Port": 3306,
                "MySQLSettings": {
                    "DatabaseName": "ledger",
                    "ServerTimezone": "UTC",
                    "AfterConnectScript": "SET time_zone='UTC'",
                },
            }),
            modify: json!({
                "MySQLSettings": { "DatabaseName": "ledger-2" },
                "ExactSettings": true,
            }),
            expected: json!({
                "MySQLSettings": {
                    "DatabaseName": "ledger-2",
                    // The settings the endpoint held and the call did not name are gone;
                    // the endpoint's own connection parameters still fill the bag the
                    // projection reports.
                    "ServerName": "c.example.internal",
                    "Port": 3306,
                    "AuthenticationMethod": "password",
                },
            }),
        },
        ModifyCase {
            name: "an integer, a string and an enum member in one call",
            create: json!({
                "EndpointType": "target",
                "EngineName": "postgres",
                "ServerName": "d.example.internal",
                "Port": 5432,
                "DatabaseName": "analytics",
            }),
            modify: json!({ "Port": 5433, "Username": "etl", "SslMode": "require" }),
            expected: json!({
                "Port": 5433,
                "Username": "etl",
                "SslMode": "require",
                "PostgreSQLSettings": {
                    "ServerName": "d.example.internal",
                    "Port": 5433,
                    "DatabaseName": "analytics",
                    "Username": "etl",
                    "AuthenticationMethod": "password",
                },
            }),
        },
        ModifyCase {
            // "To reset this parameter, pass the empty string ("") as an argument"
            // (`ExtraConnectionAttributes` doc).
            name: "the empty string resets ExtraConnectionAttributes",
            create: json!({
                "EndpointType": "source",
                "EngineName": "oracle",
                "ServerName": "e.example.internal",
                "Port": 1521,
                "ExtraConnectionAttributes": "addSupplementalLogging=Y",
            }),
            modify: json!({ "ExtraConnectionAttributes": "" }),
            expected: json!({ "ExtraConnectionAttributes": Value::Null }),
        },
        ModifyCase {
            name: "a settings bag whose engine is not a database",
            create: json!({
                "EndpointType": "target",
                "EngineName": "s3",
                "S3Settings": { "BucketName": "landing", "BucketFolder": "cdc" },
            }),
            modify: json!({
                "S3Settings": { "BucketFolder": "full-load", "CsvDelimiter": "|" },
            }),
            expected: json!({
                "S3Settings": {
                    "BucketName": "landing",
                    "BucketFolder": "full-load",
                    "CsvDelimiter": "|",
                },
            }),
        },
        ModifyCase {
            name: "the engine the endpoint runs, and the identifier it is known by",
            create: json!({
                "EndpointType": "source",
                "EngineName": "mysql",
                "ServerName": "g.example.internal",
                "Port": 3306,
            }),
            modify: json!({
                "EngineName": "mariadb",
                "EndpointIdentifier": "renamed-endpoint",
            }),
            expected: json!({
                "EngineName": "mariadb",
                "EngineDisplayName": "MariaDB",
                "EndpointIdentifier": "renamed-endpoint",
            }),
        },
        ModifyCase {
            name: "a bag the call does not mention stays, and follows the endpoint",
            create: json!({
                "EndpointType": "source",
                "EngineName": "mongodb",
                "ServerName": "h1.example.internal",
                "Port": 27017,
                // `AuthMechanismValue` models default | mongodb_cr | scram_sha_1.
                "MongoDbSettings": { "AuthMechanism": "scram_sha_1", "NestingLevel": "one" },
            }),
            modify: json!({ "ServerName": "h2.example.internal" }),
            expected: json!({
                "ServerName": "h2.example.internal",
                "MongoDbSettings": {
                    "AuthMechanism": "scram_sha_1",
                    "NestingLevel": "one",
                    "ServerName": "h2.example.internal",
                    "Port": 27017,
                },
            }),
        },
        ModifyCase {
            name: "ExactSettings with no bag in the call leaves the stored bag alone",
            create: json!({
                "EndpointType": "target",
                "EngineName": "redshift",
                "ServerName": "i.example.internal",
                "Port": 5439,
                "RedshiftSettings": { "BucketName": "stage", "DateFormat": "auto" },
            }),
            modify: json!({ "Username": "loader", "ExactSettings": true }),
            expected: json!({
                "Username": "loader",
                "RedshiftSettings": {
                    "BucketName": "stage",
                    "DateFormat": "auto",
                    "ServerName": "i.example.internal",
                    "Port": 5439,
                    "Username": "loader",
                },
            }),
        },
    ]
}

/// `base` with every member of `overrides` replaced, and every member whose override is
/// `null` removed. A member `overrides` does not name keeps its value, which is what lets
/// a case assert what the modify changed AND what it left alone in one comparison.
fn with_members(base: &Value, overrides: &Value) -> Value {
    let mut members = base
        .as_object()
        .cloned()
        .expect("an Endpoint body is an object");
    for (name, value) in overrides
        .as_object()
        .expect("the table's expectations are objects")
    {
        if value.is_null() {
            members.remove(name);
        } else {
            members.insert(name.clone(), value.clone());
        }
    }
    Value::Object(members)
}

/// `body` with every member of `extra` added to it.
fn with_request_members(mut body: Value, extra: &Value) -> Value {
    for (name, value) in extra
        .as_object()
        .expect("the table's request members are objects")
    {
        body[name] = value.clone();
    }
    body
}

#[tokio::test]
async fn a_modified_endpoint_reports_the_values_it_last_accepted() {
    let emulator = Emulator::new();

    for (index, case) in modify_cases().into_iter().enumerate() {
        let identifier = format!("mod-ep-{index}");
        let created = emulator
            .ok(
                case.name,
                "CreateEndpoint",
                &with_request_members(json!({ "EndpointIdentifier": identifier }), &case.create),
            )
            .await;
        let before = created.get("Endpoint").cloned().unwrap_or(Value::Null);
        let arn = text(&before, "/EndpointArn");

        let modified = emulator
            .ok(
                case.name,
                "ModifyEndpoint",
                // Every modify carries a password as well, so each case also asserts that
                // a secret a reconfiguration sent is persisted nowhere and echoed nowhere.
                &with_request_members(
                    json!({ "EndpointArn": arn, "Password": SECRET }),
                    &case.modify,
                ),
            )
            .await;
        let after = modified.get("Endpoint").cloned().unwrap_or(Value::Null);
        assert!(
            !modified.to_string().contains(SECRET),
            "case {:?}: a response must never echo a password, got {modified}",
            case.name
        );

        // "The modified endpoint": every member the call supplied carries the value it
        // supplied, and every other member still carries the one the create reported.
        let expected = with_members(&before, &case.expected);
        assert_eq!(
            after, expected,
            "case {:?}: expected the modify to answer with {expected}, got {after}",
            case.name
        );

        // The endpoint is the resource that changed, not just the response: a later read
        // of it reports the same projection.
        let described = emulator
            .ok(
                case.name,
                "DescribeEndpoints",
                &json!({ "Filters": [{ "Name": "endpoint-arn", "Values": [arn] }] }),
            )
            .await;
        assert_eq!(
            described.pointer("/Endpoints/0"),
            Some(&after),
            "case {:?}: expected DescribeEndpoints to report the endpoint as last \
             modified, got {described}",
            case.name
        );
        assert!(
            !described.to_string().contains(SECRET),
            "case {:?}: a password must not be reachable through a later read, got \
             {described}",
            case.name
        );

        // "The endpoint that was deleted" is the endpoint as last modified, with the only
        // difference the delete itself makes: the status it carries on its way out.
        let deleted = emulator
            .ok(
                case.name,
                "DeleteEndpoint",
                &json!({ "EndpointArn": arn }),
            )
            .await;
        let echoed = deleted.get("Endpoint").cloned().unwrap_or(Value::Null);
        let expected_echo = with_members(&after, &json!({ "Status": "deleting" }));
        assert_eq!(
            echoed, expected_echo,
            "case {:?}: expected the delete to echo {expected_echo}, got {echoed}",
            case.name
        );
    }
}

#[tokio::test]
async fn a_modify_that_is_refused_leaves_the_endpoint_as_it_was() {
    let emulator = Emulator::new();

    let held = emulator
        .ok(
            "the endpoint a modify is refused on",
            "CreateEndpoint",
            &json!({
                "EndpointIdentifier": "modify-refusals",
                "EndpointType": "source",
                "EngineName": "mysql",
                "ServerName": "refusals.example.internal",
                "Port": 3306,
                "DatabaseName": "before",
            }),
        )
        .await;
    let endpoint = held.get("Endpoint").cloned().unwrap_or(Value::Null);
    let arn = text(&endpoint, "/EndpointArn");
    let taken = emulator.endpoint("already-taken", "target", "postgres").await;

    // An `EndpointArn` that names no endpoint the account holds, over the forms a value
    // can take: a well-formed ARN for an endpoint that does not exist, one in another
    // region, and a value that is not an ARN at all.
    for (case, absent) in [
        (
            "an endpoint of this account that does not exist",
            format!("arn:aws:dms:{REGION}:{ACCOUNT}:endpoint:NOSUCHENDPOINT0000000000000"),
        ),
        (
            "an endpoint of another region",
            format!("arn:aws:dms:eu-west-1:{ACCOUNT}:endpoint:OTHERREGION000000000000000"),
        ),
        ("a value that is not an ARN", "not-an-arn".to_string()),
    ] {
        let (status, response) = emulator
            .call(
                "ModifyEndpoint",
                &json!({ "EndpointArn": absent, "DatabaseName": "after" }),
            )
            .await;
        let expected = format!("Endpoint with arn {absent} does not exist.");
        assert_eq!(
            (status, error_code(&response), text(&response, "/message")),
            (
                400,
                "InvalidParameterValueException".to_string(),
                expected.clone()
            ),
            "case {case:?}: expected 400 InvalidParameterValueException {expected:?}, got \
             {status} {response}"
        );
    }

    // A rename onto an identifier another endpoint of the account holds is refused, and
    // the refusal keeps the other endpoint's ARN out of its text.
    let (status, response) = emulator
        .call(
            "ModifyEndpoint",
            &json!({ "EndpointArn": arn, "EndpointIdentifier": "already-taken" }),
        )
        .await;
    let expected = r#"ReplicationEndpoint "already-taken" already in use."#.to_string();
    assert_eq!(
        (status, error_code(&response), text(&response, "/message")),
        (
            400,
            "ResourceAlreadyExistsFault".to_string(),
            expected.clone()
        ),
        "expected 400 ResourceAlreadyExistsFault {expected:?}, got {status} {response}"
    );
    assert!(
        !text(&response, "/message").contains(&taken),
        "the refusal must not disclose the holding endpoint's ARN ({taken}), got {response}"
    );

    // An `EndpointIdentifier` the member's own grammar refuses is refused before anything
    // is written, over each way the grammar can be broken.
    for (case, candidate) in [
        ("a leading digit", "1endpoint"),
        ("a trailing hyphen", "endpoint-"),
        ("two consecutive hyphens", "end--point"),
        ("a character outside the alphabet", "end_point"),
        ("the empty identifier", ""),
    ] {
        let (status, response) = emulator
            .call(
                "ModifyEndpoint",
                &json!({ "EndpointArn": arn, "EndpointIdentifier": candidate }),
            )
            .await;
        assert_eq!(
            (status, error_code(&response).contains("ValidationException")),
            (400, true),
            "case {case:?}: expected 400 ValidationException, got {status} {response}"
        );
    }

    // Nothing any of those refusals carried reached the endpoint.
    let described = emulator
        .ok(
            "the endpoint after every refusal",
            "DescribeEndpoints",
            &json!({ "Filters": [{ "Name": "endpoint-arn", "Values": [arn] }] }),
        )
        .await;
    assert_eq!(
        described.pointer("/Endpoints/0"),
        Some(&endpoint),
        "expected a refused modify to leave the endpoint as it was, got {described}"
    );

    // A rename that IS accepted moves the identifier: the endpoint answers to the new
    // one, and the one it released is free for another endpoint to take.
    let renamed = emulator
        .ok(
            "an accepted rename",
            "ModifyEndpoint",
            &json!({ "EndpointArn": arn, "EndpointIdentifier": "modify-refusals-2" }),
        )
        .await;
    assert_eq!(
        text(&renamed, "/Endpoint/EndpointIdentifier"),
        "modify-refusals-2",
        "expected the rename to be reported, got {renamed}"
    );
    assert_eq!(
        text(&renamed, "/Endpoint/EndpointArn"),
        arn,
        "expected a rename to leave the endpoint's ARN alone, got {renamed}"
    );
    let reused = emulator
        .ok(
            "the released identifier",
            "CreateEndpoint",
            &json!({
                "EndpointIdentifier": "modify-refusals",
                "EndpointType": "target",
                "EngineName": "mysql",
            }),
        )
        .await;
    assert_ne!(
        text(&reused, "/Endpoint/EndpointArn"),
        arn,
        "expected the released identifier to name a NEW endpoint, got {reused}"
    );
    let described = emulator
        .ok(
            "both endpoints after the rename",
            "DescribeEndpoints",
            &json!({ "Filters": [{ "Name": "endpoint-id", "Values": ["modify-refusals-2"] }] }),
        )
        .await;
    assert_eq!(
        text(described.pointer("/Endpoints/0").unwrap_or(&Value::Null), "/EndpointArn"),
        arn,
        "expected the renamed endpoint to answer to its new identifier, got {described}"
    );
}

/// One engine's settings bag, carried in full by the create and then named one setting at
/// a time by the modify.
struct FoldCase {
    /// The `EngineName` that selects this bag.
    engine_name: &'static str,
    /// The `Endpoint` member the bag is reported under.
    settings_member: &'static str,
    /// The bag the create carries: every member of that structure whose value a request
    /// chooses freely — the ones this service's model types as a plain string, number,
    /// boolean or number list — from the structure's own definition in the model
    /// (`smithy_query(query="shape", shape="<Settings>")`). A member the model types as an
    /// enum is left out, since its value has to come from that enum, and so is every
    /// secret member, which is persisted nowhere.
    created: Value,
    /// What the modify names: one setting with a new value, plus the members the model
    /// makes required, which a request cannot omit.
    names: Value,
}

/// One row per settings structure `ModifyEndpoint` models, so a member missing from the
/// fold — which would silently drop the endpoint's stored value for that setting — is
/// caught for every engine rather than for the few the cases above happen to carry.
fn fold_cases() -> Vec<FoldCase> {
    vec![
        FoldCase {
            engine_name: "oracle",
            settings_member: "OracleSettings",
            created: json!({
                "AddSupplementalLogging": true,
                "ArchivedLogDestId": 1,
                "AdditionalArchivedLogDestId": 2,
                "ExtraArchivedLogDestIds": [3, 4],
                "AllowSelectNestedTables": true,
                "ParallelAsmReadThreads": 4,
                "ReadAheadBlocks": 2000,
                "AccessAlternateDirectly": false,
                "UseAlternateFolderForOnline": false,
                "OraclePathPrefix": "/oracle",
                "UsePathPrefix": "/use",
                "ReplacePathPrefix": false,
                "EnableHomogenousTablespace": true,
                "DirectPathNoLog": true,
                "ArchivedLogsOnly": false,
                "AsmServer": "asm.example.internal",
                "AsmUser": "asm-user",
                "DatabaseName": "ORCL",
                "DirectPathParallelLoad": true,
                "FailTasksOnLobTruncation": true,
                "NumberDatatypeScale": 2,
                "Port": 1521,
                "ReadTableSpaceName": true,
                "RetryInterval": 5,
                "SecurityDbEncryptionName": "encryption-name",
                "ServerName": "ora.example.internal",
                "SpatialDataOptionToGeoJsonFunctionName": "to_geojson",
                "StandbyDelayTime": 10,
                "Username": "dms",
                "UseBFile": true,
                "UseDirectPathFullLoad": true,
                "UseLogminerReader": false,
                "SecretsManagerAccessRoleArn": "arn:aws:iam::000000000000:role/dms-secrets",
                "SecretsManagerSecretId": "dms/oracle",
                "SecretsManagerOracleAsmAccessRoleArn": "arn:aws:iam::000000000000:role/dms-asm",
                "SecretsManagerOracleAsmSecretId": "dms/oracle-asm",
                "TrimSpaceInChar": true,
                "ConvertTimestampWithZoneToUtc": true,
                "OpenTransactionWindow": 60,
            }),
            names: json!({ "RetryInterval": 60 }),
        },
        FoldCase {
            engine_name: "kafka",
            settings_member: "KafkaSettings",
            created: json!({
                "Broker": "kafka.example.internal:9092",
                "Topic": "changes",
                "IncludeTransactionDetails": true,
                "IncludePartitionValue": true,
                "PartitionIncludeSchemaTable": true,
                "IncludeTableAlterOperations": true,
                "IncludeControlDetails": true,
                "MessageMaxBytes": 1_000_000,
                "IncludeNullAndEmpty": true,
                "SslClientCertificateArn": "arn:aws:acm:us-east-1:000000000000:certificate/client",
                "SslClientKeyArn": "arn:aws:acm:us-east-1:000000000000:certificate/key",
                "SslCaCertificateArn": "arn:aws:acm:us-east-1:000000000000:certificate/ca",
                "SaslUsername": "kafka-user",
                "NoHexPrefix": true,
                "UseLargeIntegerValue": true,
            }),
            names: json!({ "Topic": "changes-v2" }),
        },
        FoldCase {
            engine_name: "redshift",
            settings_member: "RedshiftSettings",
            created: json!({
                "AcceptAnyDate": true,
                "AfterConnectScript": "SET statement_timeout TO 0",
                "BucketFolder": "stage",
                "BucketName": "redshift-stage",
                "CaseSensitiveNames": true,
                "CompUpdate": false,
                "ConnectionTimeout": 30,
                "DatabaseName": "warehouse",
                "DateFormat": "auto",
                "EmptyAsNull": true,
                "ExplicitIds": true,
                "FileTransferUploadStreams": 10,
                "LoadTimeout": 1000,
                "MaxFileSize": 32_000,
                "Port": 5439,
                "RemoveQuotes": true,
                "ReplaceInvalidChars": "?",
                "ReplaceChars": "?",
                "ServerName": "redshift.example.internal",
                "ServerSideEncryptionKmsKeyId": "alias/aws/s3",
                "TimeFormat": "auto",
                "TrimBlanks": true,
                "TruncateColumns": true,
                "Username": "loader",
                "WriteBufferSize": 1000,
                "SecretsManagerAccessRoleArn": "arn:aws:iam::000000000000:role/dms-secrets",
                "SecretsManagerSecretId": "dms/redshift",
                "MapBooleanAsBoolean": true,
            }),
            names: json!({ "LoadTimeout": 2000 }),
        },
        FoldCase {
            engine_name: "postgres",
            settings_member: "PostgreSQLSettings",
            created: json!({
                "AfterConnectScript": "SET session_replication_role = replica",
                "CaptureDdls": true,
                "MaxFileSize": 32_000,
                "DatabaseName": "analytics",
                "DdlArtifactsSchema": "dms",
                "ExecuteTimeout": 60,
                "FailTasksOnLobTruncation": true,
                "HeartbeatEnable": true,
                "HeartbeatSchema": "heartbeat",
                "HeartbeatFrequency": 5,
                "Port": 5432,
                "ServerName": "pg.example.internal",
                "Username": "etl",
                "SlotName": "dms_slot",
                "SecretsManagerAccessRoleArn": "arn:aws:iam::000000000000:role/dms-secrets",
                "SecretsManagerSecretId": "dms/postgres",
                "TrimSpaceInChar": true,
                "MapBooleanAsBoolean": true,
                "MapJsonbAsClob": true,
                "BabelfishDatabaseName": "babelfish",
                "DisableUnicodeSourceFilter": true,
            }),
            names: json!({ "HeartbeatFrequency": 15 }),
        },
        FoldCase {
            engine_name: "s3",
            settings_member: "S3Settings",
            created: json!({
                "ExternalTableDefinition": "{\"TableCount\":1}",
                "CsvRowDelimiter": "\\n",
                "CsvDelimiter": ",",
                "BucketFolder": "cdc",
                "BucketName": "landing",
                "ServerSideEncryptionKmsKeyId": "alias/aws/s3",
                "DictPageSizeLimit": 1_000_000,
                "RowGroupLength": 10_000,
                "DataPageSize": 1_000_000,
                "EnableStatistics": true,
                "IncludeOpForFullLoad": true,
                "CdcInsertsOnly": false,
                "TimestampColumnName": "loaded_at",
                "ParquetTimestampInMillisecond": true,
                "CdcInsertsAndUpdates": false,
                "DatePartitionEnabled": false,
                "UseCsvNoSupValue": true,
                "CsvNoSupValue": "NOSUP",
                "PreserveTransactions": false,
                "CdcPath": "cdc-path",
                "UseTaskStartTimeForFullLoadTimestamp": true,
                "AddColumnName": true,
                "CdcMaxBatchInterval": 60,
                "CdcMinFileSize": 32_000,
                "CsvNullValue": "NULL",
                "IgnoreHeaderRows": 1,
                "MaxFileSize": 1_048_576,
                "Rfc4180": true,
                "DatePartitionTimezone": "UTC",
                "AddTrailingPaddingCharacter": false,
                "ExpectedBucketOwner": "000000000000",
                "GlueCatalogGeneration": false,
            }),
            names: json!({ "BucketFolder": "full-load" }),
        },
        FoldCase {
            engine_name: "sqlserver",
            settings_member: "MicrosoftSQLServerSettings",
            created: json!({
                "Port": 1433,
                "BcpPacketSize": 16_384,
                "DatabaseName": "sales",
                "ControlTablesFileGroup": "dms",
                "QuerySingleAlwaysOnNode": true,
                "ReadBackupOnly": true,
                "ServerName": "mssql.example.internal",
                "Username": "dms",
                "UseBcpFullLoad": true,
                "UseThirdPartyBackupDevice": false,
                "SecretsManagerAccessRoleArn": "arn:aws:iam::000000000000:role/dms-secrets",
                "SecretsManagerSecretId": "dms/sqlserver",
                "TrimSpaceInChar": true,
                "ForceLobLookup": true,
            }),
            names: json!({ "BcpPacketSize": 32_768 }),
        },
        FoldCase {
            engine_name: "mysql",
            settings_member: "MySQLSettings",
            created: json!({
                "AfterConnectScript": "SET time_zone='UTC'",
                "CleanSourceMetadataOnMismatch": true,
                "DatabaseName": "hrdb",
                "EventsPollInterval": 5,
                "MaxFileSize": 32_000,
                "ParallelLoadThreads": 4,
                "Port": 3306,
                "ServerName": "mysql.example.internal",
                "ServerTimezone": "UTC",
                "Username": "reader",
                "SecretsManagerAccessRoleArn": "arn:aws:iam::000000000000:role/dms-secrets",
                "SecretsManagerSecretId": "dms/mysql",
                "ExecuteTimeout": 60,
            }),
            names: json!({ "EventsPollInterval": 10 }),
        },
        FoldCase {
            engine_name: "gcp-mysql",
            settings_member: "GcpMySQLSettings",
            created: json!({
                "AfterConnectScript": "SET time_zone='UTC'",
                "CleanSourceMetadataOnMismatch": true,
                "DatabaseName": "gcpdb",
                "EventsPollInterval": 5,
                "MaxFileSize": 32_000,
                "ParallelLoadThreads": 4,
                "Port": 3306,
                "ServerName": "gcp.example.internal",
                "ServerTimezone": "UTC",
                "Username": "reader",
                "SecretsManagerAccessRoleArn": "arn:aws:iam::000000000000:role/dms-secrets",
                "SecretsManagerSecretId": "dms/gcp-mysql",
            }),
            names: json!({ "ParallelLoadThreads": 8 }),
        },
        FoldCase {
            engine_name: "mongodb",
            settings_member: "MongoDbSettings",
            created: json!({
                "Username": "reader",
                "ServerName": "mongo.example.internal",
                "Port": 27017,
                "DatabaseName": "catalog",
                "ExtractDocId": "false",
                "DocsToInvestigate": "1000",
                "AuthSource": "admin",
                "KmsKeyId": "alias/aws/dms",
                "SecretsManagerAccessRoleArn": "arn:aws:iam::000000000000:role/dms-secrets",
                "SecretsManagerSecretId": "dms/mongodb",
                "UseUpdateLookUp": true,
                "ReplicateShardCollections": true,
            }),
            names: json!({ "AuthSource": "catalog" }),
        },
        FoldCase {
            engine_name: "docdb",
            settings_member: "DocDbSettings",
            created: json!({
                "Username": "reader",
                "ServerName": "docdb.example.internal",
                "Port": 27017,
                "DatabaseName": "catalog",
                "ExtractDocId": false,
                "DocsToInvestigate": 1000,
                "KmsKeyId": "alias/aws/dms",
                "SecretsManagerAccessRoleArn": "arn:aws:iam::000000000000:role/dms-secrets",
                "SecretsManagerSecretId": "dms/docdb",
                "UseUpdateLookUp": true,
                "ReplicateShardCollections": true,
            }),
            names: json!({ "DocsToInvestigate": 2000 }),
        },
        FoldCase {
            engine_name: "db2",
            settings_member: "IBMDb2Settings",
            created: json!({
                "DatabaseName": "SAMPLE",
                "Port": 50_000,
                "ServerName": "db2.example.internal",
                "SetDataCaptureChanges": true,
                "CurrentLsn": "00000000000000000000000000000001",
                "MaxKBytesPerRead": 64,
                "Username": "db2inst1",
                "SecretsManagerAccessRoleArn": "arn:aws:iam::000000000000:role/dms-secrets",
                "SecretsManagerSecretId": "dms/db2",
                "LoadTimeout": 1000,
                "WriteBufferSize": 1000,
                "MaxFileSize": 32_000,
                "KeepCsvFiles": true,
            }),
            names: json!({ "MaxKBytesPerRead": 128 }),
        },
        FoldCase {
            engine_name: "sybase",
            settings_member: "SybaseSettings",
            created: json!({
                "DatabaseName": "ase",
                "Port": 5000,
                "ServerName": "sybase.example.internal",
                "Username": "dms",
                "SecretsManagerAccessRoleArn": "arn:aws:iam::000000000000:role/dms-secrets",
                "SecretsManagerSecretId": "dms/sybase",
            }),
            names: json!({ "DatabaseName": "ase-2" }),
        },
        FoldCase {
            engine_name: "kinesis",
            settings_member: "KinesisSettings",
            created: json!({
                "StreamArn": "arn:aws:kinesis:us-east-1:000000000000:stream/changes",
                "IncludeTransactionDetails": true,
                "IncludePartitionValue": true,
                "PartitionIncludeSchemaTable": true,
                "IncludeTableAlterOperations": true,
                "IncludeControlDetails": true,
                "IncludeNullAndEmpty": true,
                "NoHexPrefix": true,
                "UseLargeIntegerValue": true,
            }),
            names: json!({
                "StreamArn": "arn:aws:kinesis:us-east-1:000000000000:stream/changes-v2",
            }),
        },
        FoldCase {
            engine_name: "dms-transfer",
            settings_member: "DmsTransferSettings",
            created: json!({ "BucketName": "transfer-zone" }),
            names: json!({ "BucketName": "transfer-zone-2" }),
        },
        FoldCase {
            engine_name: "dynamodb",
            settings_member: "DynamoDbSettings",
            // `ServiceAccessRoleArn` is the structure's only member, and the model makes
            // it required, so a bag the call carries names every setting there is.
            created: json!({ "ServiceAccessRoleArn": "arn:aws:iam::000000000000:role/dms-ddb" }),
            names: json!({ "ServiceAccessRoleArn": "arn:aws:iam::000000000000:role/dms-ddb-2" }),
        },
        FoldCase {
            engine_name: "opensearch",
            settings_member: "ElasticsearchSettings",
            created: json!({
                "ServiceAccessRoleArn": "arn:aws:iam::000000000000:role/dms-opensearch",
                "EndpointUri": "search.example.internal",
                "FullLoadErrorPercentage": 10,
                "ErrorRetryDuration": 300,
                "UseNewMappingType": true,
            }),
            // The two required members travel with every call that carries this bag.
            names: json!({
                "ServiceAccessRoleArn": "arn:aws:iam::000000000000:role/dms-opensearch",
                "EndpointUri": "search.example.internal",
                "ErrorRetryDuration": 600,
            }),
        },
        FoldCase {
            engine_name: "neptune",
            settings_member: "NeptuneSettings",
            created: json!({
                "ServiceAccessRoleArn": "arn:aws:iam::000000000000:role/dms-neptune",
                "S3BucketName": "neptune-staging",
                "S3BucketFolder": "bulk",
                "ErrorRetryDuration": 300,
                "MaxFileSize": 1024,
                "MaxRetryCount": 5,
                "IamAuthEnabled": false,
            }),
            names: json!({
                "S3BucketName": "neptune-staging",
                "S3BucketFolder": "bulk",
                "MaxRetryCount": 10,
            }),
        },
        FoldCase {
            engine_name: "redis",
            settings_member: "RedisSettings",
            created: json!({
                "ServerName": "redis.example.internal",
                "Port": 6379,
                "AuthUserName": "redis-user",
                "SslCaCertificateArn": "arn:aws:acm:us-east-1:000000000000:certificate/ca",
            }),
            names: json!({
                "ServerName": "redis.example.internal",
                "Port": 6380,
            }),
        },
        FoldCase {
            engine_name: "timestream",
            settings_member: "TimestreamSettings",
            created: json!({
                "DatabaseName": "metrics",
                "MemoryDuration": 10,
                "MagneticDuration": 200,
                "CdcInsertsAndUpdates": true,
                "EnableMagneticStoreWrites": true,
            }),
            names: json!({
                "DatabaseName": "metrics",
                "MemoryDuration": 20,
                "MagneticDuration": 200,
            }),
        },
    ]
}

#[tokio::test]
async fn a_fold_keeps_every_setting_the_call_does_not_name() {
    let emulator = Emulator::new();

    for (index, case) in fold_cases().into_iter().enumerate() {
        let identifier = format!("fold-ep-{index}");
        let mut create = json!({
            "EndpointIdentifier": identifier,
            "EndpointType": "target",
            "EngineName": case.engine_name,
        });
        create[case.settings_member] = case.created;
        let created = emulator
            .ok(case.settings_member, "CreateEndpoint", &create)
            .await;
        let before = created
            .pointer(&format!("/Endpoint/{}", case.settings_member))
            .cloned()
            .unwrap_or(Value::Null);
        let arn = text(&created, "/Endpoint/EndpointArn");

        let mut modify = json!({ "EndpointArn": arn });
        modify[case.settings_member] = case.names.clone();
        let modified = emulator
            .ok(case.settings_member, "ModifyEndpoint", &modify)
            .await;
        let after = modified
            .pointer(&format!("/Endpoint/{}", case.settings_member))
            .cloned()
            .unwrap_or(Value::Null);

        // The settings the call names carry its values; every other setting the endpoint
        // held survives the fold.
        let expected = with_members(&before, &case.names);
        assert_eq!(
            after, expected,
            "case {:?}: expected the fold to report {expected}, got {after}",
            case.settings_member
        );
    }
}
