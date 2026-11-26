// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for what `CreateOutboundExternalLink` writes, driven over the
//! same router the production emulator assembles.
//!
//! The rule under test is about the store rather than one recorded request: the
//! create of an outbound external link resolves the REQUESTER gateway its URI
//! addresses and refuses anything else with the service's not-found refusal, and
//! the `@idempotencyToken` it carries names ONE minted link per (operation,
//! token) — a retry stating the same parameters answers with the link the first
//! call minted and stores no second one, a retry stating ANY different parameter
//! is refused instead of silently answering the first call's link, and a token is
//! scoped to the operation it was presented to, so the same string presented to
//! another create mints that create's own resource.
//!
//! The tables span what the rule generalises over: for the replay, the form and
//! the length of `publicEndpoint` (scheme, host, port, path, up to the published
//! 255-character bound of `URL`), which members of `attributes` are stated and
//! whether any are, how many tags the link carries, and the sampling percentages
//! including both published bounds of that range
//! (`smithy_query(query="shape", shape="LinkApplicationLogSampling")`); for the
//! refusal, WHICH parameter the retry states differently — the endpoint's host or
//! only its path, an optional structure added or dropped, a scalar inside it
//! changed, a required structure's number changed, a tag added, a tag's key or
//! value changed, every tag dropped, and the gateway in the URI, which is a
//! parameter of this call like any other.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_rtbfabric_rtbfabric::ops::types::LINK_STATUS_VALUES;
use service_com_amazonaws_rtbfabric_rtbfabric::state::RESOURCE_NOT_FOUND_MESSAGE;
use service_com_amazonaws_rtbfabric_rtbfabric::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region the test signs for.
const REGION: &str = "us-west-2";

/// The operation under test, as the refusal below names it.
const OPERATION: &str = "CreateOutboundExternalLink";

/// A `GatewayId` matching the modelled `^rtb-gw-[a-z0-9-]{1,25}$` that no create
/// in this suite minted.
const UNMINTED_GATEWAY_ID: &str = "rtb-gw-nothing-here";

/// One answered call: the HTTP status, the wire error code (absent on a success)
/// and the parsed body.
struct Answer {
    status: u16,
    code: String,
    body: Value,
}

impl Answer {
    fn text(&self, member: &str) -> String {
        self.body[member].as_str().unwrap_or_default().to_string()
    }
}

/// The one emulated service, behind the same `MultiServiceRouter` the production
/// router assembles, so these calls take the real deserialize -> validate ->
/// handle -> serialize path.
struct Fabric {
    router: MultiServiceRouter,
}

impl Fabric {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    /// Send one restJson1 request and return what came back.
    async fn send(&self, method: &str, path: &str, body: Option<Value>) -> Answer {
        let bytes = match body {
            Some(body) => Bytes::from(serde_json::to_vec(&body).expect("serialize the body")),
            None => Bytes::new(),
        };
        let request = ::http::Request::builder()
            .method(method)
            .uri(format!("https://rtbfabric.{REGION}.amazonaws.com{path}"))
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/rtbfabric/aws4_request, \
                     SignedHeaders=host, Signature=00"
                ),
            )
            .body(bytes)
            .expect("build the request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let body = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        Answer { status, code, body }
    }

    /// A stored requester gateway, as `CreateRequesterGateway` mints one.
    async fn requester_gateway(&self, case: &str, token: &str) -> String {
        let answered = self
            .send(
                "POST",
                "/requester-gateway",
                Some(json!({
                    "vpcId": "vpc-0123456789abcdef0",
                    "subnetIds": ["subnet-0123456789abcdef0"],
                    "securityGroupIds": ["sg-0123456789abcdef0"],
                    "clientToken": token,
                })),
            )
            .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected CreateRequesterGateway to answer 200, got {} {:?}",
            answered.status, answered.body
        );
        answered.text("gatewayId")
    }

    /// A stored responder gateway of the `EXTERNAL` type, which is the kind that
    /// takes inbound external links.
    async fn responder_gateway(&self, case: &str, token: &str) -> String {
        let answered = self
            .send(
                "POST",
                "/responder-gateway",
                Some(json!({
                    "vpcId": "vpc-0123456789abcdef0",
                    "subnetIds": ["subnet-0123456789abcdef0"],
                    "securityGroupIds": ["sg-0123456789abcdef0"],
                    "port": 443,
                    "protocol": "HTTPS",
                    "listenerConfig": { "protocols": ["HTTPS"] },
                    "domainName": "responder.example.com",
                    "gatewayType": "EXTERNAL",
                    "clientToken": token,
                })),
            )
            .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected CreateResponderGateway to answer 200, got {} {:?}",
            answered.status, answered.body
        );
        answered.text("gatewayId")
    }

    /// `POST /requester-gateway/{gatewayId}/outbound-external-link`.
    async fn create_outbound(&self, gateway_id: &str, body: Value) -> Answer {
        self.send(
            "POST",
            &format!("/requester-gateway/{gateway_id}/outbound-external-link"),
            Some(body),
        )
        .await
    }

    /// `GET /requester-gateway/{gatewayId}/outbound-external-link/{linkId}`.
    async fn read_outbound(&self, gateway_id: &str, link_id: &str) -> Answer {
        self.send(
            "GET",
            &format!("/requester-gateway/{gateway_id}/outbound-external-link/{link_id}"),
            None,
        )
        .await
    }

    /// How many links a gateway of either kind counts, which is derived by
    /// scanning the links the store holds for it — an external link included.
    async fn links_counted(&self, case: &str, surface: &str, gateway_id: &str) -> i64 {
        let answered = self.send("GET", &format!("/{surface}/{gateway_id}"), None).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected the {surface} read to answer 200, got {} {:?}",
            answered.status, answered.body
        );
        answered.body["totalLinksCount"].as_i64().unwrap_or(-1)
    }
}

/// Whether `candidate` is a `LinkId`: `^link-[a-z0-9-]{1,25}$`, length 6..30
/// (`smithy_query(query="shape", shape="LinkId")`).
fn is_link_id(candidate: &str) -> bool {
    let Some(suffix) = candidate.strip_prefix("link-") else {
        return false;
    };
    (1..=25).contains(&suffix.len())
        && (6..=30).contains(&candidate.len())
        && suffix
            .chars()
            .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-')
}

/// `LinkLogSettings` whose sampling percentages are `error` and `filter`, each
/// inside the modelled 0..100 range.
fn log_settings_at(error: f64, filter: f64) -> Value {
    json!({ "applicationLogs": { "sampling": { "errorLog": error, "filterLog": filter } } })
}

/// The members of one create body besides its `clientToken`.
struct LinkBody {
    /// A `URL`: len 0..255, `^(https|http)://.+$`.
    public_endpoint: String,
    /// A `LinkAttributes`, or `Value::Null` for a link that states none.
    attributes: Value,
    /// A `LinkLogSettings`, which the create requires.
    log_settings: Value,
    /// A `TagsMap`, or `Value::Null` for a link that carries no tags.
    tags: Value,
}

impl LinkBody {
    fn json(&self, token: &str) -> Value {
        let mut body = json!({
            "clientToken": token,
            "publicEndpoint": self.public_endpoint,
            "logSettings": self.log_settings,
        });
        let object = body.as_object_mut().expect("a JSON object body");
        if !self.attributes.is_null() {
            object.insert("attributes".to_string(), self.attributes.clone());
        }
        if !self.tags.is_null() {
            object.insert("tags".to_string(), self.tags.clone());
        }
        body
    }
}

/// One row of the replay table: a create whose retry states exactly the same
/// parameters.
struct ReplayRow {
    case: &'static str,
    body: LinkBody,
}

fn replay_rows() -> Vec<ReplayRow> {
    vec![
        ReplayRow {
            case: "an https endpoint, no attributes, no tags, sampling at both bounds",
            body: LinkBody {
                public_endpoint: "https://bidder.example.com/rtb".to_string(),
                attributes: Value::Null,
                log_settings: log_settings_at(100.0, 0.0),
                tags: Value::Null,
            },
        },
        ReplayRow {
            case: "an http endpoint with a port, a customer-provided id, one tag",
            body: LinkBody {
                public_endpoint: "http://198.51.100.10:8080/bid".to_string(),
                attributes: json!({ "customerProvidedId": "exchange-west-1" }),
                log_settings: log_settings_at(12.5, 37.25),
                tags: json!({ "team": "rtb" }),
            },
        },
        ReplayRow {
            // `ResponderErrorMaskingForHttpCode`: `httpCode` is
            // `^DEFAULT|4XX|5XX|\d{3}$`, `action` is NO_BID or PASSTHROUGH and
            // `loggingTypes` holds members of NONE, METRIC, RESPONSE
            // (`smithy_query(query="shape",
            // shape="ResponderErrorMaskingForHttpCode")`).
            case: "error masking for two HTTP codes, three tags",
            body: LinkBody {
                public_endpoint: "https://a.example.com/".to_string(),
                attributes: json!({
                    "customerProvidedId": "exchange_east-2",
                    "responderErrorMasking": [
                        {
                            "httpCode": "5XX",
                            "action": "NO_BID",
                            "loggingTypes": ["METRIC"],
                            "responseLoggingPercentage": 42.5,
                        },
                        {
                            "httpCode": "503",
                            "action": "PASSTHROUGH",
                            "loggingTypes": ["METRIC", "RESPONSE"],
                        },
                    ],
                }),
                log_settings: log_settings_at(0.0, 100.0),
                tags: json!({ "team": "rtb", "env": "test", "owner": "fabric" }),
            },
        },
        ReplayRow {
            // The published upper bound of `URL`'s length: 25 + 230 = 255.
            case: "an endpoint exactly at the published length bound",
            body: LinkBody {
                public_endpoint: format!("https://long.example.com/{}", "a".repeat(230)),
                attributes: json!({ "customerProvidedId": "a" }),
                log_settings: log_settings_at(50.0, 50.0),
                tags: Value::Null,
            },
        },
    ]
}

#[tokio::test]
async fn a_replayed_client_token_answers_with_the_link_the_first_call_minted() {
    for row in replay_rows() {
        let case = row.case;
        let fabric = Fabric::new();
        let gateway_id = fabric.requester_gateway(case, "token-requester").await;
        let token = "token-outbound-link";

        let first = fabric
            .create_outbound(&gateway_id, row.body.json(token))
            .await;
        assert_eq!(
            first.status, 200,
            "case {case:?}: expected {OPERATION} to answer 200, got {} {:?}",
            first.status, first.body
        );
        let link_id = first.text("linkId");
        assert!(
            is_link_id(&link_id),
            "case {case:?}: expected a minted linkId matching the modelled pattern, got {link_id:?}"
        );

        // The retry states the same parameters, so it is answered from the record
        // the first call wrote rather than minting a second link.
        let retry = fabric
            .create_outbound(&gateway_id, row.body.json(token))
            .await;
        assert_eq!(
            retry.status, 200,
            "case {case:?}: expected the retry to answer 200, got {} {:?}",
            retry.status, retry.body
        );
        assert_eq!(
            (retry.text("linkId"), retry.text("gatewayId")),
            (link_id.clone(), gateway_id.clone()),
            "case {case:?}: expected the retry to report the link the first call minted, got {:?}",
            retry.body
        );
        assert!(
            LINK_STATUS_VALUES.contains(&retry.text("status").as_str()),
            "case {case:?}: expected a modelled LinkStatus on the retry, got {:?}",
            retry.body
        );

        // One link, not two: the gateway's own reader counts what the store
        // holds for it.
        assert_eq!(
            fabric
                .links_counted(case, "requester-gateway", &gateway_id)
                .await,
            1,
            "case {case:?}: expected the retry to have stored no second link, got {:?}",
            retry.body
        );
        let read = fabric.read_outbound(&gateway_id, &link_id).await;
        assert_eq!(
            (read.status, read.text("publicEndpoint")),
            (200, row.body.public_endpoint.clone()),
            "case {case:?}: expected the stored link to read back unchanged, got {} {:?}",
            read.status,
            read.body
        );
    }
}

/// One row of the mismatch table: how the retry's request differs from the call
/// the token was first presented with.
struct MismatchRow {
    case: &'static str,
    /// Applied to the retry's body, which starts as a copy of the first call's.
    mutate: fn(&mut Value),
    /// Whether the retry addresses a SECOND requester gateway instead of the one
    /// the first call opened the link from.
    other_gateway: bool,
}

/// The call every mismatch row retries: every optional member stated, so a row
/// can drop one as well as change one.
fn mismatch_base() -> LinkBody {
    LinkBody {
        public_endpoint: "https://bidder.example.com/rtb".to_string(),
        attributes: json!({
            "customerProvidedId": "exchange-west-1",
            "responderErrorMasking": [{
                "httpCode": "5XX",
                "action": "NO_BID",
                "loggingTypes": ["METRIC"],
                "responseLoggingPercentage": 42.5,
            }],
        }),
        log_settings: log_settings_at(100.0, 0.0),
        tags: json!({ "team": "rtb", "env": "test" }),
    }
}

fn mismatch_rows() -> Vec<MismatchRow> {
    vec![
        MismatchRow {
            case: "the endpoint names another host",
            mutate: |body| body["publicEndpoint"] = json!("https://other.example.com/rtb"),
            other_gateway: false,
        },
        MismatchRow {
            case: "the endpoint differs only in its path",
            mutate: |body| body["publicEndpoint"] = json!("https://bidder.example.com/rtb/v2"),
            other_gateway: false,
        },
        MismatchRow {
            case: "the retry states no attributes at all",
            mutate: |body| {
                body.as_object_mut()
                    .expect("a JSON object body")
                    .remove("attributes");
            },
            other_gateway: false,
        },
        MismatchRow {
            case: "a scalar inside attributes changed",
            mutate: |body| body["attributes"]["customerProvidedId"] = json!("exchange-east-1"),
            other_gateway: false,
        },
        MismatchRow {
            case: "the error-masking action changed",
            mutate: |body| body["attributes"]["responderErrorMasking"][0]["action"] = json!("PASSTHROUGH"),
            other_gateway: false,
        },
        MismatchRow {
            case: "one more error-masking entry",
            mutate: |body| {
                let entries = body["attributes"]["responderErrorMasking"]
                    .as_array_mut()
                    .expect("a JSON array of masking entries");
                entries.push(json!({
                    "httpCode": "DEFAULT",
                    "action": "NO_BID",
                    "loggingTypes": ["NONE"],
                }));
            },
            other_gateway: false,
        },
        MismatchRow {
            case: "a log sampling percentage changed",
            mutate: |body| {
                body["logSettings"]["applicationLogs"]["sampling"]["errorLog"] = json!(99.5);
            },
            other_gateway: false,
        },
        MismatchRow {
            case: "one more tag",
            mutate: |body| body["tags"]["owner"] = json!("fabric"),
            other_gateway: false,
        },
        MismatchRow {
            case: "a tag's value changed",
            mutate: |body| body["tags"]["env"] = json!("prod"),
            other_gateway: false,
        },
        MismatchRow {
            case: "a tag's key changed",
            mutate: |body| {
                let tags = body["tags"].as_object_mut().expect("a JSON object of tags");
                tags.remove("env");
                tags.insert("stage".to_string(), json!("test"));
            },
            other_gateway: false,
        },
        MismatchRow {
            case: "the retry carries no tags",
            mutate: |body| {
                body.as_object_mut()
                    .expect("a JSON object body")
                    .remove("tags");
            },
            other_gateway: false,
        },
        MismatchRow {
            case: "the same body, from another requester gateway",
            mutate: |_| {},
            other_gateway: true,
        },
    ]
}

#[tokio::test]
async fn a_replayed_client_token_stating_a_different_parameter_is_refused() {
    let token = "token-outbound-link";
    for row in mismatch_rows() {
        let case = row.case;
        let fabric = Fabric::new();
        let gateway_id = fabric.requester_gateway(case, "token-requester").await;
        let other_gateway_id = fabric.requester_gateway(case, "token-requester-two").await;
        let base = mismatch_base();

        let first = fabric.create_outbound(&gateway_id, base.json(token)).await;
        assert_eq!(
            first.status, 200,
            "case {case:?}: expected {OPERATION} to answer 200, got {} {:?}",
            first.status, first.body
        );
        let link_id = first.text("linkId");

        let mut retry_body = base.json(token);
        (row.mutate)(&mut retry_body);
        let addressed = if row.other_gateway {
            &other_gateway_id
        } else {
            &gateway_id
        };
        let retry = fabric.create_outbound(addressed, retry_body).await;

        assert_eq!(
            (retry.status, retry.code.as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected a 400 ValidationException, got {} {:?} {:?}",
            retry.status,
            retry.code,
            retry.body
        );
        assert_eq!(
            retry.text("message"),
            format!(
                "clientToken {token} was already used for a {OPERATION} request with different parameters"
            ),
            "case {case:?}: expected the mismatch refusal to state the token and the operation, got {:?}",
            retry.body
        );

        // The refusal left the store as the first call did: one link on the
        // gateway it was opened from, none on the other, and the stored link
        // still reporting what the first call persisted.
        assert_eq!(
            (
                fabric
                    .links_counted(case, "requester-gateway", &gateway_id)
                    .await,
                fabric
                    .links_counted(case, "requester-gateway", &other_gateway_id)
                    .await,
            ),
            (1, 0),
            "case {case:?}: expected the refused retry to have stored nothing, got {:?}",
            retry.body
        );
        let read = fabric.read_outbound(&gateway_id, &link_id).await;
        assert_eq!(
            (read.status, read.text("publicEndpoint")),
            (200, base.public_endpoint.clone()),
            "case {case:?}: expected the first call's link to be untouched, got {} {:?}",
            read.status,
            read.body
        );
    }
}

#[tokio::test]
async fn a_client_token_is_scoped_to_the_operation_it_was_presented_to() {
    let case = "one token presented to both external-link creates";
    let fabric = Fabric::new();
    let token = "token-shared-between-operations";
    let requester_id = fabric.requester_gateway(case, "token-requester").await;
    let responder_id = fabric.responder_gateway(case, "token-responder").await;

    let outbound = fabric
        .create_outbound(&requester_id, mismatch_base().json(token))
        .await;
    assert_eq!(
        outbound.status, 200,
        "case {case:?}: expected {OPERATION} to answer 200, got {} {:?}",
        outbound.status, outbound.body
    );

    // The same string on ANOTHER create is that create's own token: it mints
    // that operation's resource instead of being read as a retry of this one.
    let inbound = fabric
        .send(
            "POST",
            &format!("/responder-gateway/{responder_id}/inbound-external-link"),
            Some(json!({
                "clientToken": token,
                "logSettings": log_settings_at(0.0, 100.0),
            })),
        )
        .await;
    assert_eq!(
        inbound.status, 200,
        "case {case:?}: expected CreateInboundExternalLink to answer 200, got {} {:?}",
        inbound.status, inbound.body
    );
    assert!(
        is_link_id(&inbound.text("linkId")) && inbound.text("linkId") != outbound.text("linkId"),
        "case {case:?}: expected the two creates to mint two links, got {:?} and {:?}",
        outbound.body,
        inbound.body
    );
    assert_eq!(
        (
            fabric
                .links_counted(case, "requester-gateway", &requester_id)
                .await,
            fabric
                .links_counted(case, "responder-gateway", &responder_id)
                .await,
        ),
        (1, 1),
        "case {case:?}: expected each gateway to hold the one link its own create minted"
    );
}

#[tokio::test]
async fn an_outbound_external_link_is_opened_only_from_a_requester_gateway() {
    let case = "a responder gateway's id on the outbound surface";
    let fabric = Fabric::new();
    let responder_id = fabric.responder_gateway(case, "token-responder").await;
    let token = "token-outbound-link";

    // An outbound external link is opened FROM a requester gateway, so an id of
    // the other kind resolves to nothing on this surface, exactly as an id
    // nothing minted does.
    for (case, gateway_id) in [
        (case, responder_id.as_str()),
        ("an id nothing minted", UNMINTED_GATEWAY_ID),
    ] {
        let refused = fabric
            .create_outbound(gateway_id, mismatch_base().json(token))
            .await;
        assert_eq!(
            (refused.status, refused.code.as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?}: expected a 404 ResourceNotFoundException, got {} {:?} {:?}",
            refused.status,
            refused.code,
            refused.body
        );
        assert_eq!(
            refused.text("message"),
            RESOURCE_NOT_FOUND_MESSAGE,
            "case {case:?}: expected this service's not-found message, got {:?}",
            refused.body
        );
    }
    assert_eq!(
        fabric
            .links_counted(case, "responder-gateway", &responder_id)
            .await,
        0,
        "case {case:?}: expected the refused create to have stored no link"
    );

    // A refused create wrote no client-token record either, so the same token is
    // free for the call that does resolve a requester gateway.
    let requester_id = fabric.requester_gateway(case, "token-requester").await;
    let created = fabric
        .create_outbound(&requester_id, mismatch_base().json(token))
        .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: expected the token to be free after the refusals, got {} {:?}",
        created.status, created.body
    );
    assert!(
        is_link_id(&created.text("linkId")),
        "case {case:?}: expected a minted linkId, got {:?}",
        created.body
    );
}
