// SPDX-License-Identifier: CC0-1.0

//! The tool-definition fetch a gateway target's reported status is decided by.
//!
//! A target whose tool definitions are DISCOVERED from the endpoint it names
//! ([`target_discovers_tool_definitions`]) is only usable once the gateway has read
//! them from there, so the status such a target settles into is the outcome of that
//! fetch and not of the call that asked for it: the fetch opens an MCP session
//! against the endpoint, and a target whose endpoint does not complete the
//! handshake settles [`STATUS_FAILED`] carrying the reason in `statusReasons`. A
//! target whose tools are DECLARED in its own configuration has nothing to fetch,
//! so it settles [`STATUS_READY`] with no reason at all.
//!
//! The fetch is a real dial, and its destination is NOT taken from the caller's value
//! as written: it is dialled only when the endpoint addresses an MCP endpoint THIS
//! EMULATOR itself publishes and serves ([`mcp_endpoint_this_emulator_serves`] — a
//! gateway's own published endpoint, the only Model Context Protocol server this
//! emulator hosts), and then over the in-process transport every emulated service
//! reaches another through
//! ([`smithy_server_runtime::EmbeddedEmulator::http_connector`]), which dispatches the
//! request back into this same process without opening a socket or resolving a name.
//! Every other address — a host on the developer's network, a private address, a
//! link-local one, whatever path it carries — is never dialled at all and is reported as
//! the endpoint that answered nothing. The outcome is read off what answered: an
//! endpoint this emulator serves the protocol on completes the handshake, and anything
//! else does not. No tool list is invented for a fetch that did not complete — one would
//! name tools no target serves.
//!
//! Re-exported from [`crate::state`], so a handler names `crate::state::…` for these
//! exactly as it does for the target record they are about.

use ::serde_json::Value;
use ::smithy_server_runtime::OperationContext;

use super::*;

/// How long ONE dial of a target's endpoint is waited for before the endpoint has
/// answered nothing.
///
/// The transport is in-process, so an address this emulator serves answers within
/// it many times over; the bound is what keeps a create or a synchronization from
/// hanging on a transport that never completes.
const DIAL_TIMEOUT: Duration = Duration::from_secs(5);

/// The JSON-RPC method that opens an MCP session, which is the first thing a client
/// of an MCP server sends and what has to complete before any tool can be listed.
const METHOD_INITIALIZE: &str = "initialize";

/// The JSON-RPC version the dial declares (JSON-RPC 2.0 §4: "A String specifying the
/// version of the JSON-RPC protocol. MUST be exactly "2.0"").
const JSONRPC_VERSION: &str = "2.0";

/// The media type JSON-RPC travels as on this wire.
const JSON_CONTENT_TYPE: &str = "application/json";

/// The id the dial's one request carries, so the answer to it can be told from a
/// notification.
const DIAL_REQUEST_ID: i64 = 1;

/// Who the dial reports itself as in `initialize`'s `clientInfo`: the gateway making
/// the fetch, which is what the real client of a target's endpoint is.
const CLIENT_NAME: &str = "bedrock-agentcore-gateway";

/// The version reported beside [`CLIENT_NAME`], which the MCP `Implementation`
/// object requires. A gateway publishes no version of its own anywhere in this
/// service's model, so this is the version of the client making the fetch.
const CLIENT_VERSION: &str = "1.0.0";

/// How much of an endpoint a status reason quotes. `McpServerTargetConfiguration`'s
/// `endpoint` is constrained only by its `^https://.*` pattern while a `StatusReason`
/// is at most 2048 characters, so the caller's value is bounded before it is quoted
/// in one.
const MAX_QUOTED_ENDPOINT: usize = 512;

/// How much of an answer the dial reads. A JSON-RPC `initialize` result is a short
/// object, so anything past this cannot be one and nothing here interprets a target's
/// tools.
const MAX_READ_ANSWER_BYTES: usize = 64 * 1024;

/// The scheme an endpoint has to be written in to be dialled at all, which is also the
/// only one the member admits (`McpServerTargetConfiguration.endpoint`, pattern
/// `^https://.*`).
const ENDPOINT_SCHEME: &str = "https";

/// The lowest and the first-unsuccessful HTTP status an answer completes an exchange
/// with: the 2xx range, outside which the endpoint refused the exchange rather than
/// answering it.
const LOWEST_SUCCESS_STATUS: u16 = 200;
const FIRST_UNSUCCESSFUL_STATUS: u16 = 300;

/// What one attempted fetch of a target's tool definitions achieved, as the status
/// the target settles into and the reasons that status reports.
///
/// Every operation that writes a target's configuration or refreshes what it serves
/// settles the target from one of these, so a target's status cannot mean one thing
/// after a create and another after a synchronization.
#[derive(Debug, Clone)]
pub struct ToolDefinitionFetch {
    /// The `TargetStatus` the target settles into once the clock has made the
    /// transition due.
    pub settled_status: &'static str,
    /// Why it settles there, for a status that HAS a reason; empty for one that has
    /// none.
    pub settled_status_reasons: Vec<String>,
}

impl ToolDefinitionFetch {
    /// The target is usable: its tools are either declared in its own configuration
    /// or were read from the endpoint it names, so there is nothing to report.
    fn completed() -> Self {
        Self {
            settled_status: STATUS_READY,
            settled_status_reasons: Vec::new(),
        }
    }

    /// The fetch did not complete, so the target serves no tools and reports why.
    fn failed(reason: String) -> Self {
        Self {
            settled_status: STATUS_FAILED,
            settled_status_reasons: vec![reason],
        }
    }
}

/// Fetch the tool definitions `configuration` calls for, and report the status the
/// target settles into.
///
/// A configuration whose tools live in the configuration itself is fetched from
/// nothing and settles ready; a configuration whose tools are discovered from an
/// endpoint is fetched by dialling that endpoint, and settles into what the dial
/// achieved.
///
/// Nothing is written here: the answer is the outcome, which the caller stores on the
/// target it is settling.
pub async fn fetch_tool_definitions(
    ctx: &OperationContext<'_>,
    configuration: &TargetConfiguration,
) -> ToolDefinitionFetch {
    let Some(endpoint) = discovered_tool_endpoint(configuration) else {
        return ToolDefinitionFetch::completed();
    };
    match dial_mcp_server(ctx, endpoint).await {
        Ok(()) => ToolDefinitionFetch::completed(),
        Err(reason) => ToolDefinitionFetch::failed(reason),
    }
}

/// The endpoint a target's tool definitions are fetched FROM, for a target whose
/// tools are discovered there rather than declared in its own configuration, and
/// `None` for every other target shape.
///
/// Which shapes those are is the one reading
/// [`target_discovers_tool_definitions`] states for the whole crate, so the targets
/// a create fetches for are exactly the targets a synchronization fetches for.
///
/// Public because an operation that drops the store guard to make the fetch has to
/// tell afterwards whether the stored target still names the endpoint that was
/// fetched: an outcome belongs to the configuration it was fetched for.
pub fn discovered_tool_endpoint(configuration: &TargetConfiguration) -> Option<&str> {
    if !target_discovers_tool_definitions(configuration) {
        return None;
    }
    match configuration {
        TargetConfiguration::Mcp(McpTargetConfiguration::mcpServer(server)) => {
            Some(server.endpoint.as_str())
        }
        _ => None,
    }
}

/// Open an MCP session against `endpoint` and report whether the handshake
/// completed.
///
/// `Err` carries the `statusReasons` entry the target settles with: what the dial
/// observed, in the sentence the failed fetch is reported in.
///
/// The destination is NOT the caller's value as written. It is dialled only when it
/// addresses an MCP endpoint this emulator itself publishes and serves
/// ([`mcp_endpoint_this_emulator_serves`]), and then over the IN-PROCESS transport
/// ([`smithy_server_runtime::EmbeddedEmulator::http_connector`]), which dispatches the
/// request back into this same process: no socket is opened and no name is resolved,
/// so no address a request body names is ever reached over the network and this
/// emulator's network position is not available to a caller. Every other address is
/// reported as the endpoint that answered nothing, which it did.
///
/// The dial carries NO credential, which is what a fetch of a caller-named endpoint is:
/// it holds nothing of the caller's and nothing of this service's, so it observes
/// exactly what the endpoint serves to everyone — an endpoint of this emulator that
/// authorizes nothing answers it, and one that requires a credential answers its own
/// refusal, which is reported as the answer it is.
async fn dial_mcp_server(ctx: &OperationContext<'_>, endpoint: &str) -> Result<(), String> {
    use ::aws_smithy_runtime_api::client::http::HttpConnector as _;

    // The one destination check: an endpoint this emulator does not serve the protocol
    // on is not dialled, so the value a request body carries never reaches a transport.
    let Some(address) = mcp_endpoint_this_emulator_serves(endpoint) else {
        return Err(endpoint_answered_nothing(endpoint));
    };
    let payload = ::serde_json::json!({
        "jsonrpc": JSONRPC_VERSION,
        "id": DIAL_REQUEST_ID,
        "method": METHOD_INITIALIZE,
        "params": {
            // The revision the MCP surfaces of this emulator speak, declared by the
            // client as the revision it asks for.
            "protocolVersion": crate::service::mcp::MCP_PROTOCOL_VERSION,
            // The fetch lists tools and nothing else, so it offers no capability of
            // its own for the server to call back on.
            "capabilities": {},
            "clientInfo": {"name": CLIENT_NAME, "version": CLIENT_VERSION},
        },
    });
    let body = ::serde_json::to_vec(&payload).unwrap_or_default();
    // The transport routes on the address the request carries, which is the endpoint
    // the check above resolved — rebuilt from its parts, so what is dialled is that
    // address and not the text around it.
    let built = ::http::Request::post(&address)
        .header(::http::header::CONTENT_TYPE, JSON_CONTENT_TYPE)
        .header(::http::header::ACCEPT, JSON_CONTENT_TYPE)
        .body(::aws_smithy_types::body::SdkBody::from(body));
    let Ok(request) = built else {
        return Err(endpoint_answered_nothing(endpoint));
    };
    let Ok(request) = ::aws_smithy_runtime_api::http::Request::try_from(request) else {
        return Err(endpoint_answered_nothing(endpoint));
    };
    let dialled = ::tokio::time::timeout(
        DIAL_TIMEOUT,
        ctx.callback().http_connector().call(request),
    );
    let Ok(Ok(answer)) = dialled.await else {
        ::tracing::debug!(
            endpoint = %endpoint,
            "a gateway target's tool-definition fetch reached no endpoint"
        );
        return Err(endpoint_answered_nothing(endpoint));
    };
    let status = answer.status().as_u16();
    let Some(bytes) = answer.into_body().bytes().map(<[u8]>::to_vec) else {
        // An answer whose body this reader cannot see carries no `initialize` result
        // to read, so the handshake did not complete — the in-process transport
        // buffers every answer, so nothing here reaches this today, and it is worth a
        // line if a transport ever does.
        ::tracing::debug!(
            endpoint = %endpoint,
            status,
            "a gateway target's tool-definition fetch could not read the answer's body"
        );
        return Err(handshake_did_not_complete(endpoint, status));
    };
    let read = &bytes[..bytes.len().min(MAX_READ_ANSWER_BYTES)];
    // The transport's OWN refusal of a call that reached no service is not an
    // endpoint's answer: nothing here serves that address.
    if ::service_common::in_process_refusal::is_transport_refusal(read) {
        return Err(endpoint_answered_nothing(endpoint));
    }
    if !(LOWEST_SUCCESS_STATUS..FIRST_UNSUCCESSFUL_STATUS).contains(&status)
        || !completed_initialize(read)
    {
        return Err(handshake_did_not_complete(endpoint, status));
    }
    Ok(())
}

/// The address to dial for `endpoint` when it addresses an MCP endpoint THIS EMULATOR
/// publishes and serves, and `None` for every other value.
///
/// A gateway's own published endpoint is the only Model Context Protocol server this
/// emulator hosts, and [`published_gateway_url`] publishes it in two forms, which
/// [`crate::service::mcp`] serves on:
///
/// * the gateway's OWN endpoint host, where the gateway is named in the host — admitted
///   when that host resolves back to a gateway id and a Region
///   ([`gateway_endpoint_identity`], the inverse of the host this service publishes);
/// * an address of THIS EMULATOR's own, where the gateway is named in the PATH under the
///   gateway plane's reserved prefix — admitted only when the authority names the
///   interface this emulator's own listeners answer on, read by the tree's one reading of
///   that question
///   (`service_common::data_plane_addressing::host_names_the_bound_interface`). The path
///   alone admits nothing: a reserved path under SOMEONE ELSE's host is not an address
///   this emulator publishes.
///
/// So the destination of a fetch is one of those two forms, written in the scheme the
/// member admits, and nothing else: a host on the developer's network, a private
/// address and a link-local one match neither form, under any
/// path, and are never dialled.
///
/// The answer is rebuilt from the parsed parts rather than handed back as written, so
/// the address that reaches the transport is the one this reading resolved.
fn mcp_endpoint_this_emulator_serves(endpoint: &str) -> Option<String> {
    let uri = endpoint.parse::<::http::Uri>().ok()?;
    if uri.scheme_str() != Some(ENDPOINT_SCHEME) {
        return None;
    }
    let authority = uri.authority()?.as_str();
    let path = uri.path();
    let serves_gateway = gateway_endpoint_identity(authority).is_some()
        || (::service_common::data_plane_addressing::host_names_the_bound_interface(authority)
            && gateway_plane_path(path).is_some_and(|rest| !rest.is_empty()));
    if !serves_gateway {
        return None;
    }
    Some(format!("{ENDPOINT_SCHEME}://{authority}{path}"))
}

/// Whether an answer is the `initialize` RESULT an MCP server completes the handshake
/// with: a JSON-RPC result carrying the protocol revision the server speaks, which
/// the protocol requires it to answer with and which is what tells an MCP server from
/// anything else serving that address.
fn completed_initialize(answer: &[u8]) -> bool {
    let Ok(message) = ::serde_json::from_slice::<Value>(answer) else {
        return false;
    };
    message
        .get("result")
        .and_then(|result| result.get("protocolVersion"))
        .and_then(Value::as_str)
        .is_some_and(|revision| !revision.is_empty())
}

/// Why a fetch that NOTHING answered leaves the target failed: the endpoint it was
/// made against, what was asked of it, and the two things the caller checks.
fn endpoint_answered_nothing(endpoint: &str) -> String {
    format!(
        "The MCP server at {} did not respond to the tool discovery request. Verify that the \
         endpoint is reachable from the gateway and serves the Model Context Protocol, then \
         synchronize the target again.",
        quoted_endpoint(endpoint)
    )
}

/// Why a fetch something ANSWERED, without being an MCP server, leaves the target
/// failed: the endpoint, the exchange that did not complete, the status that answer
/// carried, and what the caller checks.
fn handshake_did_not_complete(endpoint: &str, status: u16) -> String {
    format!(
        "The MCP server at {} did not complete the MCP initialize handshake: the tool discovery \
         request was answered with HTTP {status}. Verify that the endpoint serves the Model \
         Context Protocol, then synchronize the target again.",
        quoted_endpoint(endpoint)
    )
}

/// A caller-supplied endpoint as a status reason quotes it: bounded to
/// [`MAX_QUOTED_ENDPOINT`] characters, with anything that is not printable text
/// dropped, so the reason stays one readable line inside `StatusReason`'s length.
fn quoted_endpoint(endpoint: &str) -> String {
    endpoint
        .chars()
        .filter(|character| character.is_ascii_graphic() || *character == ' ')
        .take(MAX_QUOTED_ENDPOINT)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::types::{
        ApiSchemaConfiguration, McpServerTargetConfiguration, McpToolSchemaConfiguration,
    };

    /// An MCP server target pointed at `endpoint`, with no static tool schema, so its
    /// tools are discovered from that endpoint.
    fn mcp_server(endpoint: &str) -> TargetConfiguration {
        TargetConfiguration::Mcp(McpTargetConfiguration::mcpServer(
            McpServerTargetConfiguration {
                endpoint: endpoint.to_string(),
                ..McpServerTargetConfiguration::default()
            },
        ))
    }

    #[test]
    fn only_a_target_whose_tools_are_discovered_has_an_endpoint_to_fetch_from() {
        // The rows differ along the configuration shape: the one shape whose tools are
        // discovered, that shape pinned to a static list, and a shape whose tools are
        // declared in the configuration.
        let pinned = TargetConfiguration::Mcp(McpTargetConfiguration::mcpServer(
            McpServerTargetConfiguration {
                endpoint: "https://pinned.invalid/mcp".to_string(),
                mcp_tool_schema: Some(McpToolSchemaConfiguration::inlinePayload(
                    "{\"tools\":[]}".to_string(),
                )),
                ..McpServerTargetConfiguration::default()
            },
        ));
        let declared = TargetConfiguration::Mcp(McpTargetConfiguration::openApiSchema(
            ApiSchemaConfiguration::inlinePayload("{\"openapi\":\"3.0.0\"}".to_string()),
        ));
        let rows: Vec<(&str, TargetConfiguration, Option<&str>)> = vec![
            (
                "an MCP server whose tools are discovered",
                mcp_server("https://tools.invalid/mcp"),
                Some("https://tools.invalid/mcp"),
            ),
            (
                "an MCP server pinned to a static tool list",
                pinned,
                None,
            ),
            (
                "a target declaring its tools in its configuration",
                declared,
                None,
            ),
        ];
        for (case, configuration, expected) in &rows {
            let actual = discovered_tool_endpoint(configuration);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected the fetched endpoint {expected:?}, got {actual:?}"
            );
        }
    }

    #[test]
    fn only_an_mcp_endpoint_this_emulator_serves_is_dialled() {
        // The rows differ along what the endpoint ADDRESSES: both forms a gateway's
        // endpoint is published in, a gateway host in another Region, and the addresses
        // a fetch must never reach — a host on the developer's network, a private-range
        // address and another process on this machine, each on its own and again
        // carrying the reserved path of this emulator's own gateway plane, plus a
        // published form written in another scheme.
        const PLANE_PATH: &str = "/_aws/bedrock-agentcore-gateway/mygateway-0123456789/mcp";
        let gateway_host = |region: &str| {
            format!("mygateway-0123456789.gateway.bedrock-agentcore.{region}.amazonaws.com")
        };
        let rows: Vec<(&str, String, Option<String>)> = vec![
            (
                "a gateway's own published endpoint",
                format!("https://{}/mcp", gateway_host("us-east-1")),
                Some(format!("https://{}/mcp", gateway_host("us-east-1"))),
            ),
            (
                "the same gateway in another Region",
                format!("https://{}/mcp", gateway_host("eu-west-1")),
                Some(format!("https://{}/mcp", gateway_host("eu-west-1"))),
            ),
            (
                "this emulator's own endpoint with the gateway in the path",
                format!("https://127.0.0.1:9297{PLANE_PATH}"),
                Some(format!("https://127.0.0.1:9297{PLANE_PATH}")),
            ),
            (
                "a host on the developer's network",
                "https://tools.example.com/mcp".to_string(),
                None,
            ),
            (
                "that host carrying this emulator's own reserved path",
                format!("https://tools.example.com{PLANE_PATH}"),
                None,
            ),
            (
                "a private-range address",
                "https://10.1.2.3/mcp".to_string(),
                None,
            ),
            (
                "a private-range address carrying the reserved path",
                format!("https://10.1.2.3{PLANE_PATH}"),
                None,
            ),
            (
                "another process on this machine, outside the gateway plane",
                "https://127.0.0.1:8080/metrics".to_string(),
                None,
            ),
            (
                "a gateway endpoint written in another scheme",
                format!("http://{}/mcp", gateway_host("us-east-1")),
                None,
            ),
            ("a value that is not a URL at all", "https://".to_string(), None),
        ];
        for (case, endpoint, expected) in &rows {
            let actual = mcp_endpoint_this_emulator_serves(endpoint);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected the dialled address {expected:?}, got {actual:?}"
            );
        }
    }

    #[test]
    fn the_initialize_result_is_told_from_every_other_answer() {
        // The rows differ along what answered: the result an MCP server completes the
        // handshake with, a result missing the revision, an empty revision, a JSON-RPC
        // error, another service's own error document, and a body that is not JSON.
        let rows: [(&str, &[u8], bool); 6] = [
            (
                "the initialize result of an MCP server",
                br#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26"}}"#,
                true,
            ),
            (
                "a result carrying no protocol revision",
                br#"{"jsonrpc":"2.0","id":1,"result":{"capabilities":{}}}"#,
                false,
            ),
            (
                "a result carrying an empty revision",
                br#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":""}}"#,
                false,
            ),
            (
                "a JSON-RPC error",
                br#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"Method not found"}}"#,
                false,
            ),
            (
                "an AWS error envelope",
                br#"{"__type":"AccessDeniedException","message":"denied"}"#,
                false,
            ),
            ("a body that is not JSON at all", b"<html>hello</html>", false),
        ];
        for (case, answer, expected) in &rows {
            let actual = completed_initialize(answer);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected completed_initialize {expected}, got {actual}"
            );
        }
    }

    #[test]
    fn a_status_reason_stays_inside_the_modelled_length() {
        // The rows differ along the endpoint the reason quotes: a plain one, one far
        // longer than a reason can carry, and one carrying characters a single-line
        // reason cannot.
        let rows: Vec<(&str, String)> = vec![
            ("a plain endpoint", "https://tools.invalid/mcp".to_string()),
            (
                "an endpoint longer than a reason can carry",
                format!("https://{}.invalid/mcp", "p".repeat(4_000)),
            ),
            (
                "an endpoint carrying a newline",
                "https://tools.invalid/mcp\nX-Injected: 1".to_string(),
            ),
        ];
        for (case, endpoint) in &rows {
            for reason in [
                endpoint_answered_nothing(endpoint),
                handshake_did_not_complete(endpoint, 404),
            ] {
                assert!(
                    reason.chars().count() <= MAX_STATUS_REASON_CHARS,
                    "case {case:?}: a StatusReason is at most {MAX_STATUS_REASON_CHARS} \
                     characters, got {}",
                    reason.chars().count()
                );
                assert!(
                    !reason.contains('\n'),
                    "case {case:?}: a reason is one line, got {reason:?}"
                );
            }
        }
    }

    /// `StatusReason`'s own length bound (`smithy_query shape=StatusReason`: length
    /// max 2048), which every reason this module words stays inside.
    const MAX_STATUS_REASON_CHARS: usize = 2048;
}
