// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for `ExportApi`: the api's configuration, serialized.
//!
//! The rule these cases assert:
//!
//! > `ExportApi` answers the api's CURRENTLY PERSISTED configuration — the one the named
//! > stage serves, or the api's latest when the request names no stage — as an OpenAPI 3.0
//! > document in the requested output type, and writes nothing. The document is the
//! > inverse of the definition this service's own reader reads, so every value in it comes
//! > from the store and an exported definition re-imported describes the same api.
//!
//! Three tests drive one table of api configurations, so each row is asserted as a
//! document, as a re-import and in both output types. The rows span what the rule
//! generalises over: which optional api members are configured (description, version, CORS,
//! the default endpoint turned off), how many routes a path carries and how many paths the
//! api has, the route-key method (named methods and the catch-all), whether a route is wired
//! to an integration at all, and which integration members the backend carries. The further
//! tests span the other axes: the output type, `IncludeExtensions`, the stage a request
//! names (deployed and undeployed), the catch-all route key — which names no path, so it is
//! exported under the key itself and is the one key this service's own reader does not read
//! back, which is why it has a test of its own rather than a row of the round-tripped table
//! — and every value outside the catalogs the operation serves.
//!
//! The last three tests assert what the document says about the API ITSELF rather than
//! about its routes:
//!
//! > The one Server Object names the api's own endpoint with the base path TEMPLATED as a
//! > `basePath` server variable, whose default is the base path the exported configuration
//! > is served under — a named stage's own name, and nothing for the stage that serves the
//! > api's root and for the api's latest configuration — so substituting the variable
//! > yields the address that configuration answers on. With API Gateway extensions
//! > included, the document also states what no standard OpenAPI member can: the
//! > import/export algorithm version it was written under, and one `tags` entry per tag the
//! > api carries, naming the tag's key and carrying its value under the API Gateway
//! > tag-value extension. And `info.version` is the api's CURRENT `Version` — whatever a
//! > patch last left there, and a value of the api's own once the member is carried no
//! > longer.
//!
//! Their rows span how many tags an api carries and which characters their keys and values
//! use, which stage a request names, and how the api's `Version` was last written (set at
//! the create, patched, and cleared); the `IncludeExtensions` axis of that rule is one more
//! row set in the extensions test above.

use std::collections::BTreeMap;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::api_lifecycle::{as_str, call, emulator, error_code, REGION};

/// The specification an export asks for: "OAS30, for OpenAPI 3.0, is the only supported
/// value" (`ExportApi$Specification`).
const OAS30: &str = "OAS30";

/// The document-level extensions and the operation-level one an export carries, each named
/// as this service's own definition reader reads it.
const INTEGRATION_EXTENSION: &str = "x-amazon-apigateway-integration";
const CORS_EXTENSION: &str = "x-amazon-apigateway-cors";
const ENDPOINT_CONFIGURATION_EXTENSION: &str = "x-amazon-apigateway-endpoint-configuration";

/// The key a path item declares an operation of `method` under.
///
/// The catalog is the OpenAPI Path Item Object's operation fields plus API Gateway's own
/// `x-amazon-apigateway-any-method` extension for the catch-all `ANY` a route key may name
/// — the same catalog this service's definition reader reads an operation from, and the
/// methods `Route$RouteKey` admits.
fn operation_key(method: &str) -> String {
    if method == "ANY" {
        "x-amazon-apigateway-any-method".to_string()
    } else {
        method.to_ascii_lowercase()
    }
}

/// One route of a case: the `CreateRoute` body, and the `CreateIntegration` body of the
/// backend it is wired to (`None` for a route wired to nothing).
struct RouteSpec {
    route: Value,
    integration: Option<Value>,
}

impl RouteSpec {
    /// A route wired to one integration.
    fn wired(route_key: &str, integration: Value) -> Self {
        Self {
            route: json!({ "routeKey": route_key }),
            integration: Some(integration),
        }
    }

    /// A route wired to nothing.
    fn bare(route: Value) -> Self {
        Self {
            route,
            integration: None,
        }
    }

    /// The route key this route is created under.
    fn route_key(&self) -> String {
        as_str(&self.route, "routeKey")
    }
}

/// One api configuration a case exports.
struct Row {
    case: &'static str,
    /// The `CreateApi` body.
    api: Value,
    routes: Vec<RouteSpec>,
}

/// The configuration rows, spanning the axes named in this file's own doc.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an api describing itself fully, two methods on one path and a second \
                   path behind a proxy",
            api: json!({
                "name": "orders-api",
                "protocolType": "HTTP",
                "description": "the orders service",
                "version": "2024-07-01",
                "tags": { "team": "platform", "env": "test" },
                "corsConfiguration": {
                    "allowOrigins": ["https://shop.example.com"],
                    "allowMethods": ["GET", "POST"],
                    "allowHeaders": ["content-type"],
                    "exposeHeaders": ["x-request-id"],
                    "allowCredentials": true,
                    "maxAge": 600
                },
                "disableExecuteApiEndpoint": true
            }),
            routes: vec![
                RouteSpec::wired(
                    "GET /orders",
                    json!({
                        "integrationType": "HTTP_PROXY",
                        "integrationUri": "https://upstream.example.com/orders",
                        "integrationMethod": "GET",
                        "payloadFormatVersion": "1.0",
                        "timeoutInMillis": 15_000,
                        "tlsConfig": { "serverNameToVerify": "upstream.example.com" }
                    }),
                ),
                RouteSpec::wired(
                    "POST /orders",
                    json!({
                        "integrationType": "AWS_PROXY",
                        "integrationUri":
                            "arn:aws:lambda:us-east-1:000000000000:function:place-order",
                        "payloadFormatVersion": "2.0",
                        "requestParameters": {
                            "overwrite:header.x-origin": "$request.header.origin"
                        }
                    }),
                ),
                RouteSpec::wired(
                    "ANY /orders/{id}",
                    json!({
                        "integrationType": "HTTP_PROXY",
                        "integrationUri": "https://upstream.example.com/orders/{id}",
                        "integrationMethod": "ANY",
                        "payloadFormatVersion": "1.0"
                    }),
                ),
            ],
        },
        Row {
            case: "an api with nothing optional configured and one route",
            api: json!({ "name": "minimal-api", "protocolType": "HTTP" }),
            routes: vec![RouteSpec::wired(
                "GET /",
                json!({
                    "integrationType": "HTTP_PROXY",
                    "integrationUri": "https://upstream.example.com/",
                    "payloadFormatVersion": "1.0"
                }),
            )],
        },
        Row {
            case: "an api whose route carries an operation name and is wired to nothing",
            api: json!({
                "name": "unwired-api",
                "protocolType": "HTTP",
                "description": "no backend yet"
            }),
            routes: vec![RouteSpec::bare(json!({
                "routeKey": "DELETE /drafts/{draftId}",
                "operationName": "discardDraft"
            }))],
        },
        Row {
            case: "an api holding no route at all",
            api: json!({
                "name": "empty-api",
                "protocolType": "HTTP",
                "version": "0.1"
            }),
            routes: Vec::new(),
        },
    ]
}

/// What creating a row's api left in the store.
struct Created {
    api_id: String,
    api_endpoint: String,
    /// One entry per route created, in the row's order: the route as the service REPORTS
    /// it, and the integration it is wired to as the service reports that. Asserting the
    /// export against these is what pins the document to the STORED configuration rather
    /// than to the request that wrote it — a member the service resolved itself (a Lambda
    /// proxy's invoke method, a defaulted timeout) is part of the configuration too.
    routes: Vec<(Value, Option<Value>)>,
}

/// Create the row's api, one integration per wired route and the routes themselves.
async fn create(emulator: &MultiServiceRouter, row: &Row) -> Created {
    let case = row.case;
    let (status, api) = call(emulator, "POST", "/v2/apis", Some(row.api.clone())).await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateApi, got {status} {api}"
    );
    let api_id = as_str(&api, "apiId");
    let mut created_routes = Vec::new();

    for spec in &row.routes {
        let mut route = spec.route.clone();
        let mut backend = None;
        if let Some(integration) = &spec.integration {
            let (status, created) = call(
                emulator,
                "POST",
                &format!("/v2/apis/{api_id}/integrations"),
                Some(integration.clone()),
            )
            .await;
            assert_eq!(
                status, 201,
                "case {case:?}: expected 201 from CreateIntegration, got {status} {created}"
            );
            let integration_id = as_str(&created, "integrationId");
            route["target"] = json!(format!("integrations/{integration_id}"));
            backend = Some(created);
        }
        let (status, created) = call(
            emulator,
            "POST",
            &format!("/v2/apis/{api_id}/routes"),
            Some(route),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected 201 from CreateRoute, got {status} {created}"
        );
        created_routes.push((created, backend));
    }

    Created {
        api_endpoint: as_str(&api, "apiEndpoint"),
        api_id,
        routes: created_routes,
    }
}

/// Send one GET and answer `(status, body bytes)`.
///
/// An exported definition is a `@httpPayload` blob and the YAML output type is no JSON
/// document at all, so the answer is read as bytes rather than through the JSON helper
/// `crate::api_lifecycle::call` uses.
async fn get_bytes(emulator: &MultiServiceRouter, uri: &str) -> (u16, Vec<u8>) {
    let request = http::Request::builder()
        .method("GET")
        .uri(uri)
        .header("host", format!("apigateway.{REGION}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTAPIGWV2/20261021/{REGION}/apigateway/aws4_request, \
                 SignedHeaders=host;x-amz-date, Signature=0000000000000000000000000000000000000000000000000000000000000000"
            ),
        )
        .body(Bytes::new())
        .expect("build the request");

    let response = emulator.dispatch(request).await;
    let status = response.status().as_u16();
    (status, response.into_body().to_vec())
}

/// Export `api_id` with the query the case asks for: `(status, body as text)`.
async fn export_text(
    emulator: &MultiServiceRouter,
    api_id: &str,
    specification: &str,
    query: &str,
) -> (u16, String) {
    let (status, bytes) =
        get_bytes(emulator, &format!("/v2/apis/{api_id}/exports/{specification}?{query}")).await;
    (status, String::from_utf8_lossy(&bytes).to_string())
}

/// The exported JSON document of `api_id`, asserted to have been served.
async fn exported(emulator: &MultiServiceRouter, case: &str, api_id: &str, query: &str) -> Value {
    let (status, text) = export_text(emulator, api_id, OAS30, query).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from ExportApi, got {status} {text}"
    );
    serde_json::from_str(&text).unwrap_or_else(|error| {
        panic!("case {case:?}: the JSON output type must parse as JSON, got {error}: {text}")
    })
}

/// A member of `document` as text, or the empty string when it is absent.
fn text_at(document: &Value, path: &[&str]) -> String {
    let mut node = document;
    for member in path {
        node = match node.get(member) {
            Some(found) => found,
            None => return String::new(),
        };
    }
    node.as_str().unwrap_or_default().to_string()
}

/// The url of the document's one Server Object, or the empty string when it has none.
fn served_url(document: &Value) -> String {
    document
        .pointer("/servers/0/url")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The default of the document's one Server Object's `basePath` variable, or `None` when
/// the document declares no such variable.
fn served_base_path(document: &Value) -> Option<String> {
    document
        .pointer("/servers/0/variables/basePath/default")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The address a reader of the document reaches by substituting the `basePath` variable
/// into the server url — the invoke address the exported configuration answers on.
fn resolved_url(document: &Value) -> String {
    served_url(document).replace("{basePath}", &served_base_path(document).unwrap_or_default())
}

#[tokio::test]
async fn an_exported_definition_states_the_apis_configuration() {
    let emulator = emulator();

    for row in rows() {
        let case = row.case;
        let created = create(&emulator, &row).await;
        let document = exported(&emulator, case, &created.api_id, "outputType=JSON").await;

        // The document declares the specification it was asked for.
        let declared = text_at(&document, &["openapi"]);
        assert!(
            declared.starts_with("3.0."),
            "case {case:?}: an {OAS30} export must declare an OpenAPI 3.0 version, got \
             {declared:?} in {document}"
        );

        // `info` carries the api's own name, description and version.
        assert_eq!(
            text_at(&document, &["info", "title"]),
            as_str(&row.api, "name"),
            "case {case:?}: info.title must be the api's Name, got {document}"
        );
        let described = row.api.get("description").and_then(Value::as_str);
        assert_eq!(
            document.pointer("/info/description").and_then(Value::as_str),
            described,
            "case {case:?}: info.description must be the api's Description, got {document}"
        );
        if let Some(version) = row.api.get("version").and_then(Value::as_str) {
            assert_eq!(
                text_at(&document, &["info", "version"]),
                version,
                "case {case:?}: info.version must be the api's Version, got {document}"
            );
        } else {
            assert!(
                !text_at(&document, &["info", "version"]).is_empty(),
                "case {case:?}: the Info Object requires a version, got {document}"
            );
        }

        // The server the exported configuration is called at is the api's own endpoint,
        // with the base path it is served under left as a substitutable variable.
        assert_eq!(
            served_url(&document),
            format!("{}/{{basePath}}", created.api_endpoint),
            "case {case:?}: servers[0].url must be the api's ApiEndpoint under a templated \
             base path, got {document}"
        );
        assert_eq!(
            served_base_path(&document).as_deref(),
            Some(""),
            "case {case:?}: an export naming no stage is served at the api's root, so its \
             basePath variable defaults to the empty string, got {document}"
        );

        // One path item per path the route keys name, each declaring the operation of
        // every route of that path — and nothing else.
        let mut expected: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for spec in &row.routes {
            let route_key = spec.route_key();
            let (method, path) = route_key
                .split_once(' ')
                .expect("every route key of these rows names a method and a path");
            expected
                .entry(path.to_string())
                .or_default()
                .push(operation_key(method));
        }
        let paths = document
            .get("paths")
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("case {case:?}: the document must carry paths: {document}"));
        let served: BTreeMap<String, Vec<String>> = paths
            .iter()
            .map(|(path, item)| {
                let mut keys: Vec<String> = item
                    .as_object()
                    .map(|members| members.keys().cloned().collect())
                    .unwrap_or_default();
                keys.sort();
                (path.clone(), keys)
            })
            .collect();
        let mut wanted = expected.clone();
        for keys in wanted.values_mut() {
            keys.sort();
        }
        assert_eq!(
            served, wanted,
            "case {case:?}: the exported paths must be the api's route keys, got {document}"
        );

        // Each route's backend, as the extension the definition reader reads one from —
        // compared against what the service REPORTS for the route and its integration, so
        // a member the service resolved itself has to be stated too.
        for (route, backend) in &created.routes {
            let route_key = as_str(route, "routeKey");
            let (method, path) = route_key.split_once(' ').expect("a method and a path");
            let operation = document
                .pointer(&format!(
                    "/paths/{}/{}",
                    path.replace('~', "~0").replace('/', "~1"),
                    operation_key(method)
                ))
                .unwrap_or_else(|| {
                    panic!("case {case:?}: {route_key:?} must be exported: {document}")
                });
            // The OpenAPI Operation Object requires a responses map, and an HTTP api's
            // route answers under one default response.
            assert!(
                operation.pointer("/responses/default/description").is_some(),
                "case {case:?}: {route_key:?} must declare its default response, got \
                 {operation}"
            );
            assert_eq!(
                operation.get("operationId").and_then(Value::as_str),
                route.get("operationName").and_then(Value::as_str),
                "case {case:?}: {route_key:?} must carry the route's OperationName, got \
                 {operation}"
            );
            match backend {
                None => assert!(
                    operation.get(INTEGRATION_EXTENSION).is_none(),
                    "case {case:?}: {route_key:?} is wired to nothing, so it must carry no \
                     integration extension, got {operation}"
                ),
                Some(integration) => {
                    let extension = operation.get(INTEGRATION_EXTENSION).unwrap_or_else(|| {
                        panic!(
                            "case {case:?}: {route_key:?} must carry its integration: \
                             {operation}"
                        )
                    });
                    // The extension spells the type in lower case; every other member is
                    // the value the integration holds.
                    assert_eq!(
                        text_at(extension, &["type"]),
                        as_str(integration, "integrationType").to_ascii_lowercase(),
                        "case {case:?}: {route_key:?} must state its integration type, got \
                         {extension}"
                    );
                    for (member, key) in [
                        ("integrationUri", "uri"),
                        ("integrationMethod", "httpMethod"),
                        ("integrationSubtype", "integrationSubtype"),
                        ("payloadFormatVersion", "payloadFormatVersion"),
                        ("connectionType", "connectionType"),
                        ("credentialsArn", "credentials"),
                    ] {
                        assert_eq!(
                            extension.get(key).and_then(Value::as_str),
                            integration.get(member).and_then(Value::as_str),
                            "case {case:?}: {route_key:?} must state {key} as the \
                             integration's {member}, got {extension}"
                        );
                    }
                    assert_eq!(
                        extension.get("timeoutInMillis"),
                        integration.get("timeoutInMillis"),
                        "case {case:?}: {route_key:?} must state its timeout, got \
                         {extension}"
                    );
                    for (member, key) in [
                        ("requestParameters", "requestParameters"),
                        ("responseParameters", "responseParameters"),
                        ("tlsConfig", "tlsConfig"),
                    ] {
                        let held = integration
                            .get(member)
                            .filter(|value| !value.as_object().is_some_and(serde_json::Map::is_empty));
                        assert_eq!(
                            extension.get(key),
                            held,
                            "case {case:?}: {route_key:?} must state {key} as the \
                             integration's {member}, got {extension}"
                        );
                    }
                }
            }
        }

        // The api's own extensions: the CORS configuration it holds, and the default
        // endpoint only when it is turned off (the member's documented default is false).
        assert_eq!(
            document.get(CORS_EXTENSION).cloned(),
            row.api.get("corsConfiguration").cloned(),
            "case {case:?}: the document must state the api's CorsConfiguration, got \
             {document}"
        );
        let disabled = row
            .api
            .get("disableExecuteApiEndpoint")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        assert_eq!(
            document
                .pointer(&format!(
                    "/{}/disableExecuteApiEndpoint",
                    ENDPOINT_CONFIGURATION_EXTENSION
                ))
                .and_then(Value::as_bool),
            disabled.then_some(true),
            "case {case:?}: the endpoint configuration must state a disabled default \
             endpoint and nothing else, got {document}"
        );

        // A report, not a write: the api is unchanged by having been exported.
        let (status, again) = call(
            &emulator,
            "GET",
            &format!("/v2/apis/{}", created.api_id),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the api must still be readable after an export, got {status} \
             {again}"
        );
        assert_eq!(
            as_str(&again, "name"),
            as_str(&row.api, "name"),
            "case {case:?}: an export must change nothing about the api, got {again}"
        );
    }
}

#[tokio::test]
async fn an_exported_definition_reimports_as_the_same_api() {
    let emulator = emulator();

    for row in rows() {
        let case = row.case;
        let created = create(&emulator, &row).await;
        let (status, text) = export_text(&emulator, &created.api_id, OAS30, "outputType=JSON").await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from ExportApi, got {status} {text}"
        );

        // The round trip: the exported definition is read back by this service's own
        // importer, which is what makes every value in it one a reader resolves.
        let (status, imported) = call(
            &emulator,
            "PUT",
            "/v2/apis",
            Some(json!({ "body": text.clone() })),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: an exported definition must import, got {status} {imported} \
             from {text}"
        );
        let reimported_id = as_str(&imported, "apiId");
        assert_ne!(
            reimported_id, created.api_id,
            "case {case:?}: an import mints its own api, got {imported}"
        );

        let document: Value = serde_json::from_str(&text).expect("the export parses as JSON");
        for (member, expected) in [
            ("name", text_at(&document, &["info", "title"])),
            ("description", text_at(&document, &["info", "description"])),
            ("version", text_at(&document, &["info", "version"])),
        ] {
            let reported = imported
                .get(member)
                .and_then(Value::as_str)
                .unwrap_or_default();
            assert_eq!(
                reported, expected,
                "case {case:?}: the re-imported api's {member} must be the document's own, \
                 got {imported}"
            );
        }
        assert_eq!(
            imported.get("corsConfiguration").cloned(),
            document.get(CORS_EXTENSION).cloned(),
            "case {case:?}: the re-imported api's CORS configuration must be the \
             document's own, got {imported}"
        );
        assert_eq!(
            imported
                .get("disableExecuteApiEndpoint")
                .and_then(Value::as_bool),
            Some(
                document
                    .pointer(&format!(
                        "/{}/disableExecuteApiEndpoint",
                        ENDPOINT_CONFIGURATION_EXTENSION
                    ))
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            ),
            "case {case:?}: the re-imported api's default endpoint must follow the \
             document, got {imported}"
        );

        // The routes the document declares, and the backend each one names.
        let (status, routes) = call(
            &emulator,
            "GET",
            &format!("/v2/apis/{reimported_id}/routes"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from GetRoutes, got {status} {routes}"
        );
        let mut reimported_keys: Vec<String> = routes
            .get("items")
            .and_then(Value::as_array)
            .unwrap_or(&Vec::new())
            .iter()
            .map(|route| as_str(route, "routeKey"))
            .collect();
        reimported_keys.sort();
        let mut expected_keys: Vec<String> = row.routes.iter().map(RouteSpec::route_key).collect();
        expected_keys.sort();
        assert_eq!(
            reimported_keys, expected_keys,
            "case {case:?}: the re-imported api must hold the same route keys, got {routes}"
        );

        let (status, integrations) = call(
            &emulator,
            "GET",
            &format!("/v2/apis/{reimported_id}/integrations"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from GetIntegrations, got {status} {integrations}"
        );
        let mut reimported_backends: Vec<(String, String)> = integrations
            .get("items")
            .and_then(Value::as_array)
            .unwrap_or(&Vec::new())
            .iter()
            .map(|integration| {
                (
                    as_str(integration, "integrationType"),
                    as_str(integration, "integrationUri"),
                )
            })
            .collect();
        reimported_backends.sort();
        let mut expected_backends: Vec<(String, String)> = row
            .routes
            .iter()
            .filter_map(|spec| spec.integration.as_ref())
            .map(|integration| {
                (
                    as_str(integration, "integrationType"),
                    as_str(integration, "integrationUri"),
                )
            })
            .collect();
        expected_backends.sort();
        assert_eq!(
            reimported_backends, expected_backends,
            "case {case:?}: the re-imported api must front the same backends, got \
             {integrations}"
        );
    }
}

#[tokio::test]
async fn the_two_output_types_carry_one_document() {
    let emulator = emulator();

    for row in rows() {
        let case = row.case;
        let created = create(&emulator, &row).await;
        let (json_status, json_text) =
            export_text(&emulator, &created.api_id, OAS30, "outputType=JSON").await;
        let (yaml_status, yaml_text) =
            export_text(&emulator, &created.api_id, OAS30, "outputType=YAML").await;
        assert_eq!(
            (json_status, yaml_status),
            (200, 200),
            "case {case:?}: both output types must be served, got {json_status} and \
             {yaml_status}"
        );
        assert_ne!(
            yaml_text, json_text,
            "case {case:?}: the YAML output type must be YAML, not the JSON spelling \
             handed back: {yaml_text}"
        );

        // YAML 1.2 is a superset of JSON, so loading both spellings through the same
        // reader is what says they are ONE document.
        let as_yaml = |text: &str| {
            yaml_rust2::YamlLoader::load_from_str(text).unwrap_or_else(|error| {
                panic!("case {case:?}: an exported definition must parse: {error}: {text}")
            })
        };
        assert_eq!(
            as_yaml(&yaml_text),
            as_yaml(&json_text),
            "case {case:?}: the YAML export must carry the same document as the JSON one, \
             got {yaml_text}"
        );
    }
}

#[tokio::test]
async fn the_catch_all_route_is_exported_under_its_own_key() {
    let emulator = emulator();

    // "If you don't specify a routeKey, a default route of $default is created. The
    // $default route acts as a catch-all for any request made to your API"
    // (`CreateApi$RouteKey`): that key names neither a method nor a path, so the document
    // carries it as a path item under the key itself, with the catch-all method key. Both
    // sides of the axis: an api holding ONLY the catch-all, and one holding it beside a
    // route of the `{Method} {Path}` form.
    for (case, keys) in [
        ("an api whose only route is the catch-all", vec!["$default"]),
        (
            "an api holding the catch-all beside a path route",
            vec!["$default", "GET /orders"],
        ),
    ] {
        let (status, api) = call(
            &emulator,
            "POST",
            "/v2/apis",
            Some(json!({ "name": format!("catch-all-{}", keys.len()), "protocolType": "HTTP" })),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected 201 from CreateApi, got {status} {api}"
        );
        let api_id = as_str(&api, "apiId");
        for key in &keys {
            wire_route(
                &emulator,
                case,
                &api_id,
                key,
                "https://upstream.example.com/anything",
            )
            .await;
        }

        let document = exported(&emulator, case, &api_id, "outputType=JSON").await;
        let served: Vec<String> = document
            .get("paths")
            .and_then(Value::as_object)
            .map(|paths| paths.keys().cloned().collect())
            .unwrap_or_default();
        let mut expected: Vec<String> = keys
            .iter()
            .map(|key| match key.split_once(' ') {
                Some((_, path)) => path.to_string(),
                None => (*key).to_string(),
            })
            .collect();
        expected.sort();
        assert_eq!(
            served, expected,
            "case {case:?}: every route the api serves must be exported, got {document}"
        );

        // The catch-all answers every method, so its operation is declared under the key a
        // definition spells the catch-all method with, carrying the route's own backend.
        let catch_all = document
            .pointer("/paths/$default/x-amazon-apigateway-any-method")
            .unwrap_or_else(|| {
                panic!("case {case:?}: the catch-all must declare its operation: {document}")
            });
        assert_eq!(
            catch_all
                .pointer("/responses/default/description")
                .and_then(Value::as_str),
            Some("Default response for $default"),
            "case {case:?}: the catch-all's default response must name the route it answers \
             for, got {catch_all}"
        );
        assert_eq!(
            catch_all
                .pointer(&format!("/{INTEGRATION_EXTENSION}/uri"))
                .and_then(Value::as_str),
            Some("https://upstream.example.com/anything"),
            "case {case:?}: the catch-all must state the backend it fronts, got {catch_all}"
        );
    }
}

#[tokio::test]
async fn an_export_without_extensions_carries_no_api_gateway_extension() {
    let emulator = emulator();

    // "Specifies whether to include API Gateway extensions in the exported API
    // definition. API Gateway extensions are included by default"
    // (`ExportApi$IncludeExtensions`): both spellings of the default, and the one that
    // turns them off.
    for (case, query, extensions) in [
        ("the member is left out, so extensions are included", "", true),
        (
            "the member is sent true",
            "&includeExtensions=true",
            true,
        ),
        (
            "the member is sent false",
            "&includeExtensions=false",
            false,
        ),
    ] {
        let row = &rows()[0];
        let created = create(&emulator, row).await;
        let document = exported(
            &emulator,
            case,
            &created.api_id,
            &format!("outputType=JSON{query}"),
        )
        .await;

        assert_eq!(
            document.get(CORS_EXTENSION).is_some(),
            extensions,
            "case {case:?}: the CORS extension must be present only with extensions on, \
             got {document}"
        );
        assert_eq!(
            document.get(ENDPOINT_CONFIGURATION_EXTENSION).is_some(),
            extensions,
            "case {case:?}: the endpoint configuration extension must be present only \
             with extensions on, got {document}"
        );
        assert_eq!(
            document
                .pointer("/paths/~1orders/get")
                .and_then(|operation| operation.get(INTEGRATION_EXTENSION))
                .is_some(),
            extensions,
            "case {case:?}: the integration extension must be present only with \
             extensions on, got {document}"
        );
        // The api's tag set travels as `tags` entries whose VALUE is an API Gateway
        // extension, and the import/export algorithm version is an extension itself, so
        // both follow the same switch.
        assert_eq!(
            document.get(TAGS_MEMBER).is_some(),
            extensions,
            "case {case:?}: a tag's value travels in an API Gateway extension, so the \
             {TAGS_MEMBER} array must be present only with extensions on, got {document}"
        );
        assert_eq!(
            document.get(IMPORTEXPORT_VERSION_EXTENSION).is_some(),
            extensions,
            "case {case:?}: the import/export version extension must be present only with \
             extensions on, got {document}"
        );
        // The address the configuration is served at is no extension either, so the
        // templated server variable is stated whatever the switch says.
        assert_eq!(
            served_base_path(&document).as_deref(),
            Some(""),
            "case {case:?}: the server variable describes the address, not an extension, \
             got {document}"
        );
        // The document itself is still served either way: the paths are the api's
        // configuration, not an extension.
        assert!(
            document.pointer("/paths/~1orders/get").is_some(),
            "case {case:?}: the api's routes are exported whatever IncludeExtensions \
             says, got {document}"
        );
    }
}

#[tokio::test]
async fn a_stage_export_states_the_configuration_that_stage_serves() {
    let emulator = emulator();

    // Both spellings of a stage: the one that serves the api's root, which is reached with
    // no stage segment, and a named one, which is reached under its name.
    for (case, stage_name, suffix) in [
        ("a named stage", "prod", "/prod"),
        ("the stage that serves the api's root", "$default", ""),
    ] {
        let (status, api) = call(
            &emulator,
            "POST",
            "/v2/apis",
            Some(json!({ "name": format!("staged-{}", stage_name.trim_start_matches('$')), "protocolType": "HTTP" })),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected 201 from CreateApi, got {status} {api}"
        );
        let api_id = as_str(&api, "apiId");
        let api_endpoint = as_str(&api, "apiEndpoint");

        let wire = |route_key: &'static str, upstream: &'static str| {
            wire_route(&emulator, case, &api_id, route_key, upstream)
        };

        wire("GET /deployed", "https://upstream.example.com/deployed").await;

        let (status, stage) = call(
            &emulator,
            "POST",
            &format!("/v2/apis/{api_id}/stages"),
            Some(json!({ "stageName": stage_name })),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected 201 from CreateStage, got {status} {stage}"
        );
        let (status, deployment) = call(
            &emulator,
            "POST",
            &format!("/v2/apis/{api_id}/deployments"),
            Some(json!({ "stageName": stage_name })),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected 201 from CreateDeployment, got {status} {deployment}"
        );

        // The api's configuration MOVES after the release; the stage stays on what it
        // froze, which is what its export has to state.
        wire("GET /later", "https://upstream.example.com/later").await;

        let latest = exported(&emulator, case, &api_id, "outputType=JSON").await;
        let paths: Vec<String> = latest
            .get("paths")
            .and_then(Value::as_object)
            .map(|paths| paths.keys().cloned().collect())
            .unwrap_or_default();
        assert_eq!(
            paths,
            vec!["/deployed".to_string(), "/later".to_string()],
            "case {case:?}: an export naming no stage must state the api's latest \
             configuration, got {latest}"
        );

        let staged = exported(
            &emulator,
            case,
            &api_id,
            &format!(
                "outputType=JSON&stageName={}",
                crate::path_label::addressed(stage_name)
            ),
        )
        .await;
        let staged_paths: Vec<String> = staged
            .get("paths")
            .and_then(Value::as_object)
            .map(|paths| paths.keys().cloned().collect())
            .unwrap_or_default();
        assert_eq!(
            staged_paths,
            vec!["/deployed".to_string()],
            "case {case:?}: a stage export must state the configuration that stage \
             deployed, got {staged}"
        );
        assert_eq!(
            served_url(&staged),
            format!("{api_endpoint}/{{basePath}}"),
            "case {case:?}: a stage export must name the api's endpoint under a templated \
             base path, got {staged}"
        );
        assert_eq!(
            served_base_path(&staged).as_deref(),
            Some(suffix.trim_start_matches('/')),
            "case {case:?}: the basePath variable must default to the base path the stage \
             is served under, got {staged}"
        );
        // Substituting the variable yields the stage's own address — the empty base path
        // of the stage that serves the api's root leaves only the url's own separator.
        assert_eq!(
            resolved_url(&staged).trim_end_matches('/'),
            format!("{api_endpoint}{suffix}"),
            "case {case:?}: a stage export must resolve to the stage's own address, got \
             {staged}"
        );

        // And the other side of "the configuration that stage SERVES": a stage the api
        // holds but has released no deployment to serves no route at all, so its export
        // carries none — never the api's current configuration, which that stage does not
        // answer for.
        let undeployed = "staging";
        let (status, stage) = call(
            &emulator,
            "POST",
            &format!("/v2/apis/{api_id}/stages"),
            Some(json!({ "stageName": undeployed })),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected 201 from CreateStage, got {status} {stage}"
        );
        let empty = exported(
            &emulator,
            case,
            &api_id,
            &format!("outputType=JSON&stageName={undeployed}"),
        )
        .await;
        assert_eq!(
            empty.get("paths"),
            Some(&json!({})),
            "case {case:?}: a stage with no deployment serves no route, so its export must \
             carry no path, got {empty}"
        );
        assert_eq!(
            text_at(&empty, &["info", "title"]),
            text_at(&staged, &["info", "title"]),
            "case {case:?}: the api an undeployed stage belongs to is still the exported \
             api, got {empty}"
        );
    }
}

/// Wire one route of `api_id` to a fresh proxy integration fronting `upstream`.
async fn wire_route(
    emulator: &MultiServiceRouter,
    case: &str,
    api_id: &str,
    route_key: &str,
    upstream: &str,
) {
    let (status, integration) = call(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/integrations"),
        Some(json!({
            "integrationType": "HTTP_PROXY",
            "integrationUri": upstream,
            "payloadFormatVersion": "1.0"
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateIntegration, got {status} {integration}"
    );
    let target = format!("integrations/{}", as_str(&integration, "integrationId"));
    let (status, route) = call(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/routes"),
        Some(json!({ "routeKey": route_key, "target": target })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateRoute, got {status} {route}"
    );
}

#[tokio::test]
async fn an_export_request_outside_what_the_operation_serves_is_refused() {
    let emulator = emulator();

    let (status, api) = call(
        &emulator,
        "POST",
        "/v2/apis",
        Some(json!({ "name": "refusal-api", "protocolType": "HTTP" })),
    )
    .await;
    assert_eq!(
        status, 201,
        "expected 201 from CreateApi, got {status} {api}"
    );
    let api_id = as_str(&api, "apiId");

    let (status, websocket) = call(
        &emulator,
        "POST",
        "/v2/apis",
        Some(json!({
            "name": "socket-api",
            "protocolType": "WEBSOCKET",
            "routeSelectionExpression": "$request.body.action"
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "expected 201 from CreateApi, got {status} {websocket}"
    );
    let websocket_id = as_str(&websocket, "apiId");

    let absent = "nosuchapi0";
    let account = engine::emulator_account_id();

    // Each value outside a catalog this operation serves, and each label that addresses
    // nothing: the refusal names the member and the values it admits.
    let rows: Vec<(&str, String, String, u16, &str, String)> = vec![
        (
            "a specification this operation does not serve",
            api_id.clone(),
            "OAS31?outputType=JSON".to_string(),
            400,
            "BadRequestException",
            "Invalid value for Specification: 'OAS31'. Valid values are: OAS30.".to_string(),
        ),
        (
            "the one specification it serves, spelled in lower case",
            api_id.clone(),
            "oas30?outputType=JSON".to_string(),
            400,
            "BadRequestException",
            "Invalid value for Specification: 'oas30'. Valid values are: OAS30.".to_string(),
        ),
        (
            "an output type this operation does not serve",
            api_id.clone(),
            format!("{OAS30}?outputType=XML"),
            400,
            "BadRequestException",
            "Invalid value for OutputType: 'XML'. Valid values are: JSON, YAML.".to_string(),
        ),
        (
            "an output type it serves, spelled in lower case",
            api_id.clone(),
            format!("{OAS30}?outputType=yaml"),
            400,
            "BadRequestException",
            "Invalid value for OutputType: 'yaml'. Valid values are: JSON, YAML.".to_string(),
        ),
        (
            "an export algorithm version other than the only supported one",
            api_id.clone(),
            format!("{OAS30}?outputType=JSON&exportVersion=2.0"),
            400,
            "BadRequestException",
            "Invalid value for ExportVersion: '2.0'. Valid values are: 1.0.".to_string(),
        ),
        (
            "an api of the protocol an OpenAPI document describes no operation of",
            websocket_id.clone(),
            format!("{OAS30}?outputType=JSON"),
            400,
            "BadRequestException",
            "Currently, Exports are not permitted for APIs with a protocol type of \
             WEBSOCKET"
                .to_string(),
        ),
        (
            "an api id the account does not hold",
            absent.to_string(),
            format!("{OAS30}?outputType=JSON"),
            404,
            "NotFoundException",
            format!("Invalid API identifier specified {account}:{absent}"),
        ),
        (
            "a stage name the api does not hold",
            api_id.clone(),
            format!("{OAS30}?outputType=JSON&stageName=nosuchstage"),
            404,
            "NotFoundException",
            "Invalid stage identifier specified".to_string(),
        ),
    ];

    for (case, target, specification, expected_status, expected_code, expected_message) in rows {
        let (status, body) = get_bytes(
            &emulator,
            &format!("/v2/apis/{target}/exports/{specification}"),
        )
        .await;
        let text = String::from_utf8_lossy(&body).to_string();
        let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected {expected_status} from ExportApi, got {status} {text}"
        );
        assert_eq!(
            error_code(&parsed),
            expected_code,
            "case {case:?}: expected {expected_code}, got {text}"
        );
        assert_eq!(
            as_str(&parsed, "message"),
            expected_message,
            "case {case:?}: the refusal must say what the caller has to fix, got {text}"
        );
    }
}

// ---------------------------------------------------------------------------
// What the document says about the API ITSELF: its address, its tag set, the
// import/export algorithm version, and the version `info` states
// ---------------------------------------------------------------------------

/// The extension an exported definition declares the import/export algorithm version
/// under, and the version this service exports under: "Currently, the only supported
/// version is 1.0" (`ExportApi$ExportVersion`).
const IMPORTEXPORT_VERSION_EXTENSION: &str = "x-amazon-apigateway-importexport-version";
const IMPORTEXPORT_VERSION: &str = "1.0";

/// The root member a document states an api's tag set under, and the two members one entry
/// of it carries: the OpenAPI Tag Object's own `name`, and the API Gateway extension the
/// tag's VALUE travels in.
const TAGS_MEMBER: &str = "tags";
const TAG_NAME: &str = "name";
const TAG_VALUE_EXTENSION: &str = "x-amazon-apigateway-tag-value";

/// The reserved name of the stage that serves an api's root, which is reached with no stage
/// segment at all (`CreateStage$StageName`).
const DEFAULT_STAGE_NAME: &str = "$default";

/// Create one HTTP api from `request` and answer `(apiId, apiEndpoint)`.
async fn created_api(emulator: &MultiServiceRouter, case: &str, request: Value) -> (String, String) {
    let (status, api) = call(emulator, "POST", "/v2/apis", Some(request)).await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateApi, got {status} {api}"
    );
    (as_str(&api, "apiId"), as_str(&api, "apiEndpoint"))
}

/// The api's tag set as `GetApi` reports it: the state the document has to agree with.
async fn reported_tags(emulator: &MultiServiceRouter, case: &str, api_id: &str) -> Value {
    let (status, api) = call(emulator, "GET", &format!("/v2/apis/{api_id}"), None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from GetApi, got {status} {api}"
    );
    api.get(TAGS_MEMBER).cloned().unwrap_or(json!({}))
}

/// The document's `tags` array as sorted `(name, value)` pairs; `None` when the document
/// carries no `tags` member at all.
fn document_tags(document: &Value) -> Option<Vec<(String, String)>> {
    let entries = document.get(TAGS_MEMBER)?.as_array()?;
    let mut pairs: Vec<(String, String)> = entries
        .iter()
        .map(|entry| {
            (
                entry
                    .get(TAG_NAME)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                entry
                    .get(TAG_VALUE_EXTENSION)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect();
    pairs.sort();
    Some(pairs)
}

/// A tag map as sorted `(key, value)` pairs — of what a row created, or of what `GetApi`
/// reports.
fn reported_pairs(tags: &Value) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = tags
        .as_object()
        .map(|members| {
            members
                .iter()
                .map(|(key, value)| (key.clone(), value.as_str().unwrap_or_default().to_string()))
                .collect()
        })
        .unwrap_or_default();
    pairs.sort();
    pairs
}

#[tokio::test]
async fn an_exported_document_states_the_apis_tag_set_and_importexport_version() {
    let emulator = emulator();

    // The rows span how many tags an api carries and which characters a tag key and value
    // use: the namespace punctuation an AWS tag key carries, a value that is the empty
    // string, one outside ASCII, and a long one.
    for (case, tags) in [
        ("an api carrying no tag at all", json!({})),
        ("an api carrying one tag", json!({ "team": "platform" })),
        (
            "an api carrying several tags, spanning the characters a key and a value use",
            json!({
                "env": "test",
                "cost.center:api": "cc-00042",
                "owner": "",
                "説明": "注文",
                "long": "v".repeat(256)
            }),
        ),
    ] {
        let mut request = json!({ "name": "tagged-api", "protocolType": "HTTP" });
        request[TAGS_MEMBER] = tags.clone();
        let (api_id, _) = created_api(&emulator, case, request).await;
        wire_route(
            &emulator,
            case,
            &api_id,
            "GET /orders",
            "https://upstream.example.com/orders",
        )
        .await;

        let document = exported(&emulator, case, &api_id, "outputType=JSON").await;

        // The document declares the algorithm it was written by, whatever the api holds.
        assert_eq!(
            document
                .get(IMPORTEXPORT_VERSION_EXTENSION)
                .and_then(Value::as_str),
            Some(IMPORTEXPORT_VERSION),
            "case {case:?}: the document must declare the import/export version, got \
             {document}"
        );

        // The tag set the api was CREATED with is what the document has to state, and the
        // operation that owns tags has to report the same set.
        let expected = reported_pairs(&tags);
        if expected.is_empty() {
            assert!(
                document.get(TAGS_MEMBER).is_none(),
                "case {case:?}: an untagged api has no tag to state, so the document must \
                 carry no {TAGS_MEMBER} member, got {document}"
            );
        } else {
            assert_eq!(
                document_tags(&document),
                Some(expected.clone()),
                "case {case:?}: the document must state the api's own tags as {TAGS_MEMBER} \
                 entries, expected {expected:?}, got {document}"
            );
        }
        let held = reported_tags(&emulator, case, &api_id).await;
        assert_eq!(
            reported_pairs(&held),
            expected,
            "case {case:?}: GetApi must report the same tag set the document states, got \
             {held}"
        );

        // A report, not a write: the tag set a later reader sees is the one just stated.
        let after = reported_tags(&emulator, case, &api_id).await;
        assert_eq!(
            after, held,
            "case {case:?}: exporting an api must not change its tags, got {after}"
        );
    }
}


#[tokio::test]
async fn the_exported_server_templates_the_base_path_it_is_served_under() {
    let emulator = emulator();

    let case = "the base path a configuration is served under";
    let (api_id, api_endpoint) = created_api(
        &emulator,
        case,
        json!({ "name": "served-api", "protocolType": "HTTP" }),
    )
    .await;
    wire_route(
        &emulator,
        case,
        &api_id,
        "GET /orders",
        "https://upstream.example.com/orders",
    )
    .await;

    // Every stage of the api is deployed, so each export below states a configuration that
    // stage really serves.
    for stage_name in [DEFAULT_STAGE_NAME, "prod", "v1-beta"] {
        let (status, stage) = call(
            &emulator,
            "POST",
            &format!("/v2/apis/{api_id}/stages"),
            Some(json!({ "stageName": stage_name })),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {stage_name:?}: expected 201 from CreateStage, got {status} {stage}"
        );
        let (status, deployment) = call(
            &emulator,
            "POST",
            &format!("/v2/apis/{api_id}/deployments"),
            Some(json!({ "stageName": stage_name })),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {stage_name:?}: expected 201 from CreateDeployment, got {status} \
             {deployment}"
        );
    }

    // The rows span which base path the exported configuration is served under: the api's
    // latest configuration, which no stage name selects; the stage that serves the api's
    // root; and named stages, whose own name is that base path.
    for (case, named, base_path) in [
        ("no stage named, so the api's latest configuration", None, ""),
        (
            "the stage that serves the api's root",
            Some(DEFAULT_STAGE_NAME),
            "",
        ),
        ("a named stage", Some("prod"), "prod"),
        ("a second named stage", Some("v1-beta"), "v1-beta"),
    ] {
        let query = match named {
            Some(stage_name) => format!(
                "outputType=JSON&stageName={}",
                crate::path_label::addressed(stage_name)
            ),
            None => "outputType=JSON".to_string(),
        };
        let document = exported(&emulator, case, &api_id, &query).await;

        // One server, naming the api's endpoint with the base path templated.
        assert_eq!(
            document
                .get("servers")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(1),
            "case {case:?}: an exported document names one server, got {document}"
        );
        assert_eq!(
            served_url(&document),
            format!("{api_endpoint}/{{basePath}}"),
            "case {case:?}: servers[0].url must be the api's ApiEndpoint under the \
             templated base path, got {document}"
        );
        assert_eq!(
            served_base_path(&document).as_deref(),
            Some(base_path),
            "case {case:?}: the basePath variable must default to the base path the \
             exported configuration is served under, got {document}"
        );
        // And substituting it yields the address that configuration answers on: the api's
        // endpoint, followed by the stage name when a named stage was asked for.
        assert_eq!(
            resolved_url(&document).trim_end_matches('/'),
            if base_path.is_empty() {
                api_endpoint.clone()
            } else {
                format!("{api_endpoint}/{base_path}")
            },
            "case {case:?}: the resolved server url must be the invoke address, got \
             {document}"
        );
    }
}

#[tokio::test]
async fn the_exported_info_version_follows_the_apis_current_version() {
    let emulator = emulator();

    let case = "the api's current version";
    let (api_id, _) = created_api(
        &emulator,
        case,
        json!({ "name": "versioned-api", "protocolType": "HTTP", "version": "v1" }),
    )
    .await;
    wire_route(
        &emulator,
        case,
        &api_id,
        "GET /orders",
        "https://upstream.example.com/orders",
    )
    .await;

    // Each row patches `Version` and states what the api then carries: a value replaces the
    // stored one, and the empty string clears the member, after which the api reports none.
    let patched_versions = ["v1", "2024-07-01", "v2.1.0-rc.1"];
    for (case, patched, held) in [
        ("the version the create set", None, Some("v1")),
        (
            "a version a patch replaced it with",
            Some("2024-07-01"),
            Some("2024-07-01"),
        ),
        (
            "a version a patch replaced again",
            Some("v2.1.0-rc.1"),
            Some("v2.1.0-rc.1"),
        ),
        ("a version a patch cleared", Some(""), None),
    ] {
        if let Some(version) = patched {
            let (status, updated) = call(
                &emulator,
                "PATCH",
                &format!("/v2/apis/{api_id}"),
                Some(json!({ "version": version })),
            )
            .await;
            assert!(
                (200..300).contains(&status),
                "case {case:?}: a patch of Version must be accepted, got {status} {updated}"
            );
        }

        // What the api CARRIES, read back through the operation that owns it.
        let (status, api) = call(&emulator, "GET", &format!("/v2/apis/{api_id}"), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from GetApi, got {status} {api}"
        );
        assert_eq!(
            api.get("version").and_then(Value::as_str),
            held,
            "case {case:?}: GetApi must report the version the patch left, got {api}"
        );

        let document = exported(&emulator, case, &api_id, "outputType=JSON").await;
        let stated = text_at(&document, &["info", "version"]);
        match held {
            Some(version) => assert_eq!(
                stated, version,
                "case {case:?}: info.version must be the api's current Version, got \
                 {document}"
            ),
            // The Info Object requires a version, so an api carrying none is exported under
            // a value of its own — never one it carries no longer, and never empty. What it
            // states is the api's own creation instant, to the second, with the zone it is
            // stated in named after it.
            None => {
                let created = as_str(&api, "createdDate");
                let (date, after_date) = created
                    .split_once('T')
                    .unwrap_or_else(|| panic!("case {case:?}: GetApi must report a createdDate, got {api}"));
                let seconds = after_date
                    .split(['.', 'Z'])
                    .next()
                    .unwrap_or_default()
                    .to_string();
                assert_eq!(
                    stated,
                    format!("{date} {seconds}UTC"),
                    "case {case:?}: an api carrying no Version is exported under its own \
                     creation instant, got {document}"
                );
                assert!(
                    !patched_versions.contains(&stated.as_str()),
                    "case {case:?}: info.version must not report a Version the api carries \
                     no longer, got {stated:?} in {document}"
                );
            }
        }
    }
}
