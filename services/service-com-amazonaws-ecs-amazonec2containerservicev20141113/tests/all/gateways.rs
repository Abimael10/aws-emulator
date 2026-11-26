// SPDX-License-Identifier: CC0-1.0

//! The ECS GATEWAY control plane, driven end to end over HTTP: `CreateGateway` and the
//! `DescribeGateway` that reads what it wrote back.
//!
//! The rules under test:
//!
//! 1. A gateway is a persisted, ACCOUNT-scoped record whose configuration is RESOLVED from
//!    the request: every member the request leaves out takes the value its own model doc
//!    documents as the default (`scheme` internet-facing, `ipAddressType` dualstack,
//!    `maxAlbCount` 10, `httpDesync` defensive, `deletionProtection` true, `propagateTags`
//!    GATEWAY), the listener is the one the model allows (port 443, `HTTPS`), and a created
//!    gateway manages no load balancer yet (`currentAlbCount` 0).
//! 2. `accessType` is DERIVED from the resolved scheme — an `internet-facing` gateway is
//!    `PUBLIC`, an `internal` one `PRIVATE` — and never taken from the request.
//! 3. A created gateway is `ACTIVE` straight away, and is addressed afterwards by its name
//!    or by its ARN: both name the same record.
//! 4. A request that asks for a gateway Amazon ECS does not serve is refused with the
//!    modelled error of the class and that class's own sentence, and nothing is stored.
//! 5. A repeat of a create under the SAME `clientToken` is that create being made again:
//!    the stored gateway is reported back and nothing is rewritten; a repeat of the name
//!    without it is refused.
//!
//! Every case goes through the shared wire harness in [`crate::ecs_wire`], the same front
//! door a client uses, so nothing reaches into the state struct and a response that is not
//! computed from persisted state cannot pass.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    assert_millisecond_resolution, count, list, ok, refusal, service, string, ACCOUNT,
    SIGNED_REGION,
};
use crate::express_gateway_services::infrastructure_role;

/// `GatewayStatus` of a gateway that has just been created — `CreateGateway`: "the gateway is
/// returned with a status of ACTIVE".
const ACTIVE: &str = "ACTIVE";

/// The documented defaults a create applies, from the member docs of
/// `CreateGatewayRequest`/`HostSharedAlbConfiguration`/`GatewayElbNetworkConfiguration` ("The
/// default is `internet-facing`", "The default is `dualstack`", "The default is 10", "The
/// default is `defensive`", "The default is true", "The default is GATEWAY").
const DEFAULT_SCHEME: &str = "internet-facing";
const DEFAULT_IP_ADDRESS_TYPE: &str = "dualstack";
const DEFAULT_MAX_ALB_COUNT: i64 = 10;
const DEFAULT_HTTP_DESYNC: &str = "defensive";
const DEFAULT_PROPAGATE_TAGS: &str = "GATEWAY";

/// The ONE listener a gateway serves — `HostSharedAlbConfiguration$port`: "The port that the
/// load balancer listener uses is 443. The gateway supports only HTTPS at port 443."
const LISTENER_PORT: i64 = 443;
const LISTENER_PROTOCOL: &str = "HTTPS";

/// The sentence a request naming any other listener is refused with: the rule as the model
/// states it.
const LISTENER_REFUSAL: &str = "The gateway supports only HTTPS at port 443.";

/// The sentence a `name` that spells nothing is refused with.
const NAME_REQUIRED: &str = "GatewayName must be provided.";

/// The sentence an `infrastructureRoleArn` that names no role is refused with.
const ROLE_BLANK: &str = "InfrastructureRoleArn can not be blank.";

/// The fixed sentence a gateway this account holds none of is reported with.
const GATEWAY_NOT_FOUND: &str = "Gateway not found.";

/// The sentence a repeat of a gateway name that is not the same create is refused with.
const NOT_IDEMPOTENT: &str = "Creation of gateway was not idempotent.";

/// How long a gateway name may be — `CreateGatewayRequest$name`: "The name must be unique
/// within your account and can be up to 255 characters."
const MAX_NAME_LENGTH: usize = 255;

/// The refusal a name outside the character set and the length a named resource of this
/// service is spelled in earns: the rule, then the value the request spelled.
fn name_pattern_refusal(name: &str) -> String {
    format!(r"Name must match ^[a-zA-Z0-9\-_]{{1,255}}$, but was {name}")
}

/// The ARN a gateway of `name` carries, as a client builds one to address it by.
fn gateway_arn(name: &str) -> String {
    format!("arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:gateway/{name}")
}

/// A `CreateGateway` request naming `name` and the `hostSharedAlb` configuration
/// `configuration`, which is the smallest request this operation accepts.
fn create_request(name: &str, configuration: Value) -> Value {
    json!({
        "name": name,
        "infrastructureRoleArn": infrastructure_role(),
        "configuration": { "hostSharedAlb": configuration },
    })
}

/// Describe one gateway and return it, addressed by name or by ARN.
async fn describe_gateway(router: &MultiServiceRouter, reference: &str, case: &str) -> Value {
    let body = ok(
        router,
        "DescribeGateway",
        json!({ "gateway": reference }),
        case,
    )
    .await;
    body["gateway"].clone()
}

/// The `hostSharedAlb` configuration a gateway reports, asserting the union carries that
/// variant.
fn host_shared_alb(gateway: &Value, case: &str) -> Value {
    let configuration = gateway["configuration"]["hostSharedAlb"].clone();
    assert!(
        configuration.is_object(),
        "case {case:?}: expected a hostSharedAlb configuration, got {gateway}"
    );
    configuration
}

/// One row of the create table: which members the request names, and what the gateway it
/// creates reports for the ones it does not.
#[derive(Debug)]
struct CreateCase {
    case: &'static str,
    name: String,
    /// The `hostSharedAlb` configuration the request carries.
    configuration: Value,
    deletion_protection: Option<bool>,
    propagate_tags: Option<&'static str>,
    tags: &'static [(&'static str, &'static str)],
    /// What the stored gateway reports for the four configuration members with a documented
    /// default, the access type it derives, and the two request members with one.
    scheme: &'static str,
    ip_address_type: &'static str,
    max_alb_count: i64,
    http_desync: &'static str,
    access_type: &'static str,
    reports_deletion_protection: bool,
    reports_propagate_tags: &'static str,
}

#[tokio::test]
async fn a_created_gateway_is_stored_with_the_documented_defaults() {
    // The rows differ along WHICH members the request names — none beyond the required ones,
    // every one of them, the scheme alone (the member the access type is derived from), an
    // `ipAddressType` the enum publishes beyond the default, and a name at the published
    // length bound.
    let cases = [
        CreateCase {
            case: "minimal",
            name: "edge".to_string(),
            configuration: json!({}),
            deletion_protection: None,
            propagate_tags: None,
            tags: &[],
            scheme: DEFAULT_SCHEME,
            ip_address_type: DEFAULT_IP_ADDRESS_TYPE,
            max_alb_count: DEFAULT_MAX_ALB_COUNT,
            http_desync: DEFAULT_HTTP_DESYNC,
            access_type: "PUBLIC",
            reports_deletion_protection: true,
            reports_propagate_tags: DEFAULT_PROPAGATE_TAGS,
        },
        CreateCase {
            case: "every-member-named",
            name: "store_front-1".to_string(),
            configuration: json!({
                "networkConfiguration": {
                    "inboundSecurityGroup": "sg-0123456789abcdef0",
                    "subnets": ["subnet-0123456789abcdef0", "subnet-0fedcba9876543210"],
                    "ipAddressType": "ipv4",
                },
                "scheme": "internal",
                "webACLArn": format!("arn:aws:wafv2:{SIGNED_REGION}:{ACCOUNT}:regional/webacl/gw/1"),
                "accessLogs": { "destination": { "s3": { "bucket": "gateway-logs", "prefix": "edge" } } },
                "port": LISTENER_PORT,
                "protocol": LISTENER_PROTOCOL,
                "maxAlbCount": 3,
                "httpDesync": "strictest",
                "testTrafficRules": { "header": { "name": "x-canary", "values": ["on", "off"] } },
            }),
            deletion_protection: Some(false),
            propagate_tags: Some("NONE"),
            tags: &[("team", "payments"), ("env", "prod")],
            scheme: "internal",
            ip_address_type: "ipv4",
            max_alb_count: 3,
            http_desync: "strictest",
            access_type: "PRIVATE",
            reports_deletion_protection: false,
            reports_propagate_tags: "NONE",
        },
        CreateCase {
            case: "internal-scheme-alone",
            name: "private-gw".to_string(),
            configuration: json!({ "scheme": "internal" }),
            deletion_protection: None,
            propagate_tags: None,
            tags: &[],
            scheme: "internal",
            ip_address_type: DEFAULT_IP_ADDRESS_TYPE,
            max_alb_count: DEFAULT_MAX_ALB_COUNT,
            http_desync: DEFAULT_HTTP_DESYNC,
            access_type: "PRIVATE",
            reports_deletion_protection: true,
            reports_propagate_tags: DEFAULT_PROPAGATE_TAGS,
        },
        CreateCase {
            case: "dualstack-without-public-ipv4",
            name: "dualstack-gw".to_string(),
            configuration: json!({
                "networkConfiguration": { "ipAddressType": "dualstack-without-public-ipv4" },
                "maxAlbCount": 1,
            }),
            deletion_protection: Some(true),
            propagate_tags: Some("GATEWAY"),
            tags: &[("owner", "platform")],
            scheme: DEFAULT_SCHEME,
            ip_address_type: "dualstack-without-public-ipv4",
            max_alb_count: 1,
            http_desync: DEFAULT_HTTP_DESYNC,
            access_type: "PUBLIC",
            reports_deletion_protection: true,
            reports_propagate_tags: DEFAULT_PROPAGATE_TAGS,
        },
        CreateCase {
            case: "a-name-at-the-published-length-bound",
            name: "g".repeat(MAX_NAME_LENGTH),
            configuration: json!({ "httpDesync": "monitor" }),
            deletion_protection: None,
            propagate_tags: None,
            tags: &[],
            scheme: DEFAULT_SCHEME,
            ip_address_type: DEFAULT_IP_ADDRESS_TYPE,
            max_alb_count: DEFAULT_MAX_ALB_COUNT,
            http_desync: "monitor",
            access_type: "PUBLIC",
            reports_deletion_protection: true,
            reports_propagate_tags: DEFAULT_PROPAGATE_TAGS,
        },
    ];

    for row in cases {
        let case = row.case;
        let router = service();

        let mut request = create_request(&row.name, row.configuration.clone());
        if let Some(protection) = row.deletion_protection {
            request["deletionProtection"] = json!(protection);
        }
        if let Some(propagate) = row.propagate_tags {
            request["propagateTags"] = json!(propagate);
        }
        if !row.tags.is_empty() {
            request["tags"] = json!(row
                .tags
                .iter()
                .map(|(key, value)| json!({ "key": key, "value": value }))
                .collect::<Vec<Value>>());
        }

        let created = ok(&router, "CreateGateway", request, case).await;
        assert_eq!(
            string(&created, "gatewayArn", case),
            gateway_arn(&row.name),
            "case {case:?}: expected the gateway's own ARN, got {created}"
        );
        assert_eq!(
            string(&created, "name", case),
            row.name,
            "case {case:?}: expected the name the request asked for, got {created}"
        );
        assert_eq!(
            string(&created, "status", case),
            ACTIVE,
            "case {case:?}: expected a synchronous create to answer {ACTIVE}, got {created}"
        );

        // The same record, read back through the describe, by NAME and by ARN: the two
        // documented forms of the `gateway` member name one gateway.
        for reference in [row.name.clone(), gateway_arn(&row.name)] {
            let gateway = describe_gateway(&router, &reference, case).await;
            let case = &format!("{case}/{reference}");
            assert_eq!(
                string(&gateway, "gatewayArn", case),
                gateway_arn(&row.name),
                "case {case:?}: expected the gateway's own ARN, got {gateway}"
            );
            assert_eq!(
                string(&gateway, "name", case),
                row.name,
                "case {case:?}: expected the stored name, got {gateway}"
            );
            assert_eq!(
                string(&gateway, "status", case),
                ACTIVE,
                "case {case:?}: expected {ACTIVE}, got {gateway}"
            );
            assert_eq!(
                string(&gateway, "infrastructureRoleArn", case),
                infrastructure_role(),
                "case {case:?}: expected the role the create handed over, got {gateway}"
            );
            assert_eq!(
                gateway["deletionProtection"],
                json!(row.reports_deletion_protection),
                "case {case:?}: expected deletionProtection {}, got {gateway}",
                row.reports_deletion_protection
            );
            assert_eq!(
                string(&gateway, "propagateTags", case),
                row.reports_propagate_tags,
                "case {case:?}: expected propagateTags {}, got {gateway}",
                row.reports_propagate_tags
            );
            assert_eq!(
                string(&gateway, "accessType", case),
                row.access_type,
                "case {case:?}: expected the access type derived from scheme {}, got {gateway}",
                row.scheme
            );
            let reported_tags: Vec<(String, String)> = list(&gateway, "tags", case)
                .iter()
                .map(|tag| (string(tag, "key", case), string(tag, "value", case)))
                .collect();
            let expected_tags: Vec<(String, String)> = row
                .tags
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect();
            assert_eq!(
                reported_tags, expected_tags,
                "case {case:?}: expected the tags the create carried, got {gateway}"
            );
            assert_millisecond_resolution(case, "createdAt", &gateway["createdAt"]);
            assert_millisecond_resolution(case, "updatedAt", &gateway["updatedAt"]);
            assert_eq!(
                gateway["createdAt"], gateway["updatedAt"],
                "case {case:?}: expected a gateway nothing has updated to report one instant \
                 for both, got {gateway}"
            );

            // The configuration the gateway RUNS: the defaults filled in, the listener the
            // model allows, and no load balancer allocated yet.
            let configuration = host_shared_alb(&gateway, case);
            assert_eq!(
                string(&configuration, "scheme", case),
                row.scheme,
                "case {case:?}: expected scheme {}, got {configuration}",
                row.scheme
            );
            assert_eq!(
                string(&configuration["networkConfiguration"], "ipAddressType", case),
                row.ip_address_type,
                "case {case:?}: expected ipAddressType {}, got {configuration}",
                row.ip_address_type
            );
            assert_eq!(
                count(&configuration, "maxAlbCount", case),
                row.max_alb_count,
                "case {case:?}: expected maxAlbCount {}, got {configuration}",
                row.max_alb_count
            );
            assert_eq!(
                string(&configuration, "httpDesync", case),
                row.http_desync,
                "case {case:?}: expected httpDesync {}, got {configuration}",
                row.http_desync
            );
            assert_eq!(
                count(&configuration, "port", case),
                LISTENER_PORT,
                "case {case:?}: expected the one listener port, got {configuration}"
            );
            assert_eq!(
                string(&configuration, "protocol", case),
                LISTENER_PROTOCOL,
                "case {case:?}: expected the one listener protocol, got {configuration}"
            );
            assert_eq!(
                configuration["currentAlbCount"],
                json!(0),
                "case {case:?}: expected a gateway nothing uses to have allocated no load \
                 balancer, got {configuration}"
            );

            // Every other member the request spelled is reported back as it was spelled:
            // these configure the infrastructure the gateway manages and carry no default.
            for member in ["webACLArn", "accessLogs", "testTrafficRules"] {
                assert_eq!(
                    configuration[member], row.configuration[member],
                    "case {case:?}: expected {member} as the request spelled it, got \
                     {configuration}"
                );
            }
            for member in ["inboundSecurityGroup", "subnets"] {
                assert_eq!(
                    configuration["networkConfiguration"][member],
                    row.configuration["networkConfiguration"][member],
                    "case {case:?}: expected {member} as the request spelled it, got \
                     {configuration}"
                );
            }
        }
    }
}

/// One row of the refusal table: the call, and the modelled error and sentence it earns.
#[derive(Debug)]
struct RefusalCase {
    case: &'static str,
    operation: &'static str,
    request: Value,
    code: &'static str,
    message: String,
}

#[tokio::test]
async fn a_gateway_request_the_service_cannot_serve_is_refused_and_stores_nothing() {
    let over_long_name = "g".repeat(MAX_NAME_LENGTH + 1);
    let cases = [
        RefusalCase {
            case: "a name that spells nothing",
            operation: "CreateGateway",
            request: create_request("", json!({})),
            code: "InvalidParameterException",
            message: NAME_REQUIRED.to_string(),
        },
        RefusalCase {
            case: "a name of nothing but whitespace",
            operation: "CreateGateway",
            request: create_request("   ", json!({})),
            code: "InvalidParameterException",
            message: NAME_REQUIRED.to_string(),
        },
        RefusalCase {
            case: "a name outside the character set",
            operation: "CreateGateway",
            request: create_request("bad name!", json!({})),
            code: "InvalidParameterException",
            message: name_pattern_refusal("bad name!"),
        },
        RefusalCase {
            case: "a name one character past the published bound",
            operation: "CreateGateway",
            request: create_request(&over_long_name, json!({})),
            code: "InvalidParameterException",
            message: name_pattern_refusal(&over_long_name),
        },
        RefusalCase {
            case: "an infrastructure role that names no role",
            operation: "CreateGateway",
            request: json!({
                "name": "edge",
                "infrastructureRoleArn": "",
                "configuration": { "hostSharedAlb": {} },
            }),
            code: "InvalidParameterException",
            message: ROLE_BLANK.to_string(),
        },
        RefusalCase {
            case: "a listener port the gateway does not serve",
            operation: "CreateGateway",
            request: create_request("edge", json!({ "port": 8080 })),
            code: "InvalidParameterException",
            message: LISTENER_REFUSAL.to_string(),
        },
        RefusalCase {
            case: "a listener protocol the gateway does not serve",
            operation: "CreateGateway",
            request: create_request("edge", json!({ "protocol": "HTTP" })),
            code: "InvalidParameterException",
            message: LISTENER_REFUSAL.to_string(),
        },
        RefusalCase {
            case: "a tag key in the namespace reserved for Amazon Web Services",
            operation: "CreateGateway",
            request: {
                let mut request = create_request("edge", json!({}));
                request["tags"] = json!([{ "key": "aws:created-by", "value": "me" }]);
                request
            },
            code: "InvalidParameterException",
            message: "Tag key must not start with aws:".to_string(),
        },
        RefusalCase {
            case: "a gateway this account holds none of",
            operation: "DescribeGateway",
            request: json!({ "gateway": "nothing-holds-this" }),
            code: "GatewayNotFoundException",
            message: GATEWAY_NOT_FOUND.to_string(),
        },
        RefusalCase {
            case: "an ARN naming another Region's gateway",
            operation: "DescribeGateway",
            request: json!({
                "gateway": format!("arn:aws:ecs:eu-west-1:{ACCOUNT}:gateway/edge"),
            }),
            code: "InvalidParameterException",
            message: "Invalid Region in ARN".to_string(),
        },
        RefusalCase {
            case: "an ARN naming another account's gateway",
            operation: "DescribeGateway",
            request: json!({
                "gateway": format!("arn:aws:ecs:{SIGNED_REGION}:210987654321:gateway/edge"),
            }),
            code: "InvalidParameterException",
            message: "AccountIDs mismatch".to_string(),
        },
    ];

    for row in cases {
        let case = row.case;
        let router = service();
        let (code, message) = refusal(&router, row.operation, row.request.clone(), case).await;
        assert_eq!(
            code, row.code,
            "case {case:?}: expected a {} from {}, got {code} with {message:?}",
            row.code, row.operation
        );
        assert_eq!(
            message, row.message,
            "case {case:?}: expected the refusal {:?}, got {message:?}",
            row.message
        );

        // A refused create stores nothing: the name it asked for is still free, so the
        // describe of it reports the class's missing-gateway sentence.
        if row.operation == "CreateGateway" {
            let name = row.request["name"].as_str().unwrap_or_default().to_string();
            let (code, message) =
                refusal(&router, "DescribeGateway", json!({ "gateway": name }), case).await;
            assert_eq!(
                (code.as_str(), message.as_str()),
                ("GatewayNotFoundException", GATEWAY_NOT_FOUND),
                "case {case:?}: expected a refused create to have stored nothing, got {code} \
                 with {message:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_repeat_under_one_client_token_is_the_same_create() {
    let case = "a repeated create";
    let router = service();
    let name = "edge";
    let token = "0f8fad5b-d9cb-469f-a165-70867728950e";

    let mut first = create_request(name, json!({ "maxAlbCount": 3 }));
    first["clientToken"] = json!(token);
    let created = ok(&router, "CreateGateway", first, case).await;
    assert_eq!(
        string(&created, "gatewayArn", case),
        gateway_arn(name),
        "case {case:?}: expected the gateway's own ARN, got {created}"
    );

    // "If this token matches a previous request, the service ignores the request, but does
    // not return an error" — so the repeat is answered with the stored gateway even though it
    // asks for a different configuration, and the stored gateway is NOT rewritten.
    let mut repeat = create_request(name, json!({ "maxAlbCount": 7 }));
    repeat["clientToken"] = json!(token);
    repeat["deletionProtection"] = json!(false);
    let repeated = ok(&router, "CreateGateway", repeat, case).await;
    assert_eq!(
        repeated, created,
        "case {case:?}: expected the repeat to answer the stored gateway, got {repeated}"
    );

    let gateway = describe_gateway(&router, name, case).await;
    assert_eq!(
        count(&host_shared_alb(&gateway, case), "maxAlbCount", case),
        3,
        "case {case:?}: expected the stored configuration to stand, got {gateway}"
    );
    assert_eq!(
        gateway["deletionProtection"],
        json!(true),
        "case {case:?}: expected the stored deletion protection to stand, got {gateway}"
    );

    // A call that is NOT that create being made again — a different token, or none at all —
    // asks for a second gateway under a name this account has taken, and is refused.
    for (row, token) in [
        ("a different token", Some("8a2d6ef3-1c64-4a0d-9a44-5c8a3f2b7e11")),
        ("no token at all", None),
    ] {
        let mut other = create_request(name, json!({ "maxAlbCount": 9 }));
        if let Some(token) = token {
            other["clientToken"] = json!(token);
        }
        let (code, message) = refusal(&router, "CreateGateway", other, row).await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            ("InvalidParameterException", NOT_IDEMPOTENT),
            "case {row:?}: expected the taken name to be refused, got {code} with {message:?}"
        );
    }

    let gateway = describe_gateway(&router, &gateway_arn(name), case).await;
    assert_eq!(
        count(&host_shared_alb(&gateway, case), "maxAlbCount", case),
        3,
        "case {case:?}: expected the refusals to have left the gateway alone, got {gateway}"
    );
}
