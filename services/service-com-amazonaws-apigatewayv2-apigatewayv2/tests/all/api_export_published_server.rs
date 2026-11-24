// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the ADDRESS an exported definition publishes, and for the
//! refusal of a `Specification` this operation does not serve.
//!
//! Two rules, each one level above the recorded case (an HTTP api created through a client
//! configured with this emulator's own endpoint and then exported, and an export asking for
//! a specification nothing serves):
//!
//! > The one Server Object of an exported definition names the api's own PUBLISHED invoke
//! > address — the value `Api$ApiEndpoint` reports, which is an address the caller dials and
//! > this emulator answers on — followed by the `{basePath}` template it declares as a server
//! > variable, so a reader that resolves server variables reaches the configuration the
//! > document describes. The document and the member state one address, whichever endpoint
//! > the client that created or exported the api reached this emulator at, and the document
//! > also declares the import/export algorithm version it was written under.
//!
//! > A request naming a `Specification` outside the one value this operation serves is
//! > refused `400 BadRequestException` with API Gateway's own sentence for that refusal,
//! > which names the one specification version an export is written in and quotes nothing
//! > out of the request — so every unsupported spelling is refused with the same text.
//!
//! The first table's rows span what that rule generalises over: the endpoint the CREATE was
//! addressed at and the endpoint the EXPORT was addressed at (an IPv4 loopback literal with a
//! port, a loopback NAME on another port, a bracketed IPv6 literal, and this service's own
//! AWS host — the export must follow the api's published address and not the address the
//! export request itself arrived on), and the Region the api was created in (including the
//! emulator's own default, so no row passes on a hardcoded Region). The second table's rows
//! span the SHAPE of the rejected value: a later specification version, the served value in
//! another case, another specification family, the served value with a suffix, and a long
//! value — in both output types.

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::api_export::UNSUPPORTED_SPECIFICATION_MESSAGE;
use crate::api_lifecycle::{as_str, call_at_host, emulator, error_code};

/// The specification an export asks for: "OAS30, for OpenAPI 3.0, is the only supported
/// value" (`ExportApi$Specification`).
const OAS30: &str = "OAS30";

/// The extension an exported definition declares the import/export algorithm version under,
/// and the version this service exports under: "Currently, the only supported version is
/// 1.0" (`ExportApi$ExportVersion`).
const IMPORTEXPORT_VERSION_EXTENSION: &str = "x-amazon-apigateway-importexport-version";
const IMPORTEXPORT_VERSION: &str = "1.0";

/// WHERE the client that minted a row's api reached this emulator, which is what the address
/// the api publishes follows.
#[derive(Debug, PartialEq, Eq)]
enum ReachedAt {
    /// A client configured with one local endpoint URL, which resolves the emulator's own
    /// address and nothing else: the api is published on that same address, with its own id
    /// behind the reserved data-plane segment.
    TheEmulatorsOwnEndpoint,
    /// A client that resolves AWS's own hosts.
    AwsOwnHost,
}

/// One row: which endpoint the create and the export were addressed at, and the Region the
/// api was created in.
struct Row {
    case: &'static str,
    /// The `Host` of the control-plane calls that MINT the api.
    create_host: &'static str,
    create_reached: ReachedAt,
    /// The `Host` of the export request itself.
    export_host: &'static str,
    /// The Region every call of the row is signed for, and therefore the api's own.
    region: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the recorded case: created and exported through the emulator's loopback \
                   endpoint",
            create_host: "127.0.0.1:35807",
            create_reached: ReachedAt::TheEmulatorsOwnEndpoint,
            export_host: "127.0.0.1:35807",
            region: "us-west-2",
        },
        Row {
            case: "created on the loopback NAME on another port, exported through AWS's own \
                   host",
            create_host: "localhost:9297",
            create_reached: ReachedAt::TheEmulatorsOwnEndpoint,
            export_host: "apigateway.eu-west-1.amazonaws.com",
            region: "eu-west-1",
        },
        Row {
            case: "created through AWS's own host, exported through a bracketed IPv6 loopback \
                   literal",
            create_host: "apigateway.ap-southeast-2.amazonaws.com",
            create_reached: ReachedAt::AwsOwnHost,
            export_host: "[::1]:9297",
            region: "ap-southeast-2",
        },
        Row {
            case: "the emulator's own default Region, signed for explicitly",
            create_host: "127.0.0.1:9297",
            create_reached: ReachedAt::TheEmulatorsOwnEndpoint,
            export_host: "localhost:4566",
            region: "us-east-1",
        },
        Row {
            case: "created and exported through AWS's own host",
            create_host: "apigateway.eu-north-1.amazonaws.com",
            create_reached: ReachedAt::AwsOwnHost,
            export_host: "apigateway.eu-north-1.amazonaws.com",
            region: "eu-north-1",
        },
    ]
}

/// Send one GET to `host`, signed for `region`, and answer `(status, body text)`.
///
/// An exported definition is a `@httpPayload` blob, so the answer is read as bytes rather
/// than through the JSON helper the rest of the suite's calls use.
async fn get_text(
    emulator: &MultiServiceRouter,
    host: &str,
    region: &str,
    uri: &str,
) -> (u16, String) {
    let request = http::Request::builder()
        .method("GET")
        .uri(uri)
        .header("host", host)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTAPIGWV2/20261021/{region}/apigateway/aws4_request, \
                 SignedHeaders=host;x-amz-date, Signature=0000000000000000000000000000000000000000000000000000000000000000"
            ),
        )
        .body(Bytes::new())
        .expect("build the request");

    let response = emulator.dispatch(request).await;
    let status = response.status().as_u16();
    let body = response.into_body().to_vec();
    (status, String::from_utf8_lossy(&body).to_string())
}

/// Create one HTTP api with a route for `row`, through the endpoint that row's client
/// reaches, and answer `(apiId, the apiEndpoint the create published)`.
async fn created_api(emulator: &MultiServiceRouter, row: &Row) -> (String, String) {
    let (status, api) = call_at_host(
        emulator,
        row.create_host,
        row.region,
        "POST",
        "/v2/apis",
        Some(json!({ "name": "published-export-api", "protocolType": "HTTP" })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {:?}: expected 201 from CreateApi, got {status} {api}",
        row.case
    );
    let api_id = as_str(&api, "apiId");

    let (status, integration) = call_at_host(
        emulator,
        row.create_host,
        row.region,
        "POST",
        &format!("/v2/apis/{api_id}/integrations"),
        Some(json!({
            "integrationType": "HTTP_PROXY",
            "integrationMethod": "GET",
            "integrationUri": "https://upstream.example.com/orders",
            "payloadFormatVersion": "1.0"
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {:?}: expected 201 from CreateIntegration, got {status} {integration}",
        row.case
    );
    let target = format!("integrations/{}", as_str(&integration, "integrationId"));

    let (status, route) = call_at_host(
        emulator,
        row.create_host,
        row.region,
        "POST",
        &format!("/v2/apis/{api_id}/routes"),
        Some(json!({ "routeKey": "GET /orders", "target": target })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {:?}: expected 201 from CreateRoute, got {status} {route}",
        row.case
    );

    (api_id, as_str(&api, "apiEndpoint"))
}

#[tokio::test]
async fn an_exported_document_publishes_the_apis_own_invoke_address() {
    let emulator = emulator();

    for row in rows() {
        let case = row.case;
        let (api_id, published) = created_api(&emulator, &row).await;

        let (status, text) = get_text(
            &emulator,
            row.export_host,
            row.region,
            &format!("/v2/apis/{api_id}/exports/{OAS30}?outputType=JSON&includeExtensions=true"),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from ExportApi, got {status} {text}"
        );
        let document: Value = serde_json::from_str(&text).unwrap_or_else(|error| {
            panic!("case {case:?}: the JSON output type must parse as JSON, got {error}: {text}")
        });

        // One server, naming the address the api publishes with the base path templated —
        // the address a reader of the document dials, not the endpoint this export request
        // happened to arrive on.
        let servers = document
            .get("servers")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            servers.len(),
            1,
            "case {case:?}: an exported document names one server, got {document}"
        );
        let expected = format!("{published}/{{basePath}}");
        let url = document
            .pointer("/servers/0/url")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert_eq!(
            url, expected,
            "case {case:?}: servers[0].url must be the address the api publishes under the \
             templated base path, expected {expected:?}, got {url:?}"
        );

        // A client that resolves nothing but this emulator's own endpoint is given a url it
        // can dial: the endpoint it reached, with the api behind it.
        if row.create_reached == ReachedAt::TheEmulatorsOwnEndpoint {
            assert!(
                url.starts_with(&format!("https://{}/", row.create_host)),
                "case {case:?}: a client configured with this emulator's own endpoint must be \
                 given a server url on that endpoint, got {url:?}"
            );
        }

        // The template the url carries is DECLARED, so a reader that resolves server
        // variables substitutes it; the api's latest configuration is served under no base
        // path at all, so its default is the empty string.
        assert_eq!(
            document
                .pointer("/servers/0/variables/basePath/default")
                .and_then(Value::as_str),
            Some(""),
            "case {case:?}: the server must declare the basePath variable the url templates, \
             got {document}"
        );

        // And the document says which algorithm wrote it.
        assert_eq!(
            document
                .get(IMPORTEXPORT_VERSION_EXTENSION)
                .and_then(Value::as_str),
            Some(IMPORTEXPORT_VERSION),
            "case {case:?}: the document must declare the import/export version, got \
             {document}"
        );

        // One address, whichever operation states it, before and after the export: a report
        // changes nothing a later caller sees.
        let (status, api) = call_at_host(
            &emulator,
            row.create_host,
            row.region,
            "GET",
            &format!("/v2/apis/{api_id}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from GetApi, got {status} {api}"
        );
        assert_eq!(
            as_str(&api, "apiEndpoint"),
            published,
            "case {case:?}: GetApi must report the address the document states, got {api}"
        );
    }
}

#[tokio::test]
async fn a_specification_this_operation_does_not_serve_is_refused_with_one_sentence() {
    let emulator = emulator();

    let row = Row {
        case: "an api to export",
        create_host: "apigateway.eu-west-1.amazonaws.com",
        create_reached: ReachedAt::AwsOwnHost,
        export_host: "apigateway.eu-west-1.amazonaws.com",
        region: "eu-west-1",
    };
    let (api_id, _) = created_api(&emulator, &row).await;

    // The rows span the shape of the rejected value — a later specification version, the
    // served value in another case, another specification family, the served value with a
    // suffix, and a long value — and both output types, so the specification gate is what
    // answers whichever output type was asked for.
    for (case, specification, output_type) in [
        ("a later specification version", "OAS31".to_string(), "JSON"),
        (
            "the served value spelled in lower case",
            "oas30".to_string(),
            "JSON",
        ),
        (
            "another specification family",
            "SWAGGER".to_string(),
            "YAML",
        ),
        (
            "the served value with a suffix",
            "OAS30.1".to_string(),
            "JSON",
        ),
        ("a long value", "X".repeat(128), "YAML"),
    ] {
        let (status, text) = get_text(
            &emulator,
            row.export_host,
            row.region,
            &format!("/v2/apis/{api_id}/exports/{specification}?outputType={output_type}"),
        )
        .await;
        let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400 from ExportApi, got {status} {text}"
        );
        assert_eq!(
            error_code(&parsed),
            "BadRequestException",
            "case {case:?}: expected BadRequestException, got {text}"
        );
        let message = as_str(&parsed, "message");
        assert_eq!(
            message, UNSUPPORTED_SPECIFICATION_MESSAGE,
            "case {case:?}: the refusal must carry the one sentence for an unsupported \
             specification, got {message:?}"
        );
        // Fixed text: nothing of the request is quoted back into it.
        assert!(
            !message.contains(specification.as_str()),
            "case {case:?}: the sentence quotes nothing out of the request, got {message:?}"
        );
    }

    // The one value it DOES serve is still served, so the gate refuses what is outside the
    // catalog and nothing else.
    let (status, text) = get_text(
        &emulator,
        row.export_host,
        row.region,
        &format!("/v2/apis/{api_id}/exports/{OAS30}?outputType=JSON"),
    )
    .await;
    assert_eq!(
        status, 200,
        "the one served specification must still export, got {status} {text}"
    );
}
