// SPDX-License-Identifier: CC0-1.0

//! How this service quotes a `@pattern` requirement when the boundary refuses a value
//! outside it.
//!
//! RULE under test: a value outside a member's `@pattern` is refused with the standard
//! constraint report — `N validation error(s) detected: Value '<value>' at '<member>'
//! failed to satisfy constraint: Member must satisfy regular expression pattern:
//! <pattern>` — and the `<pattern>` it quotes is the expression WITHOUT the whole-value
//! anchors the model wraps it in: the leading `^` and the trailing `$` are how Smithy turns
//! a find into a whole-string match, not part of the character grammar the service
//! advertises. The CHECK still runs the model's pattern as written; only the rendering
//! drops the pair.
//!
//! The rule belongs to the RENDERING, so it holds for every `@pattern` of this model: the
//! rows below span five different expressions — a bare character-class run, one whose body
//! is a parenthesised group, one written with hex escapes, one with repetition counts, and
//! one carrying no escapes at all — reached through four different operations and three
//! different bindings (a body member, a nested body member and a query parameter).

use std::sync::Arc;

use service_com_amazonaws_networksecuritymanager_piccolocustomerapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// What a caller sees: the HTTP facts it branches on plus the JSON body.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: Option<String>,
    json: serde_json::Value,
}

/// The emulator under test: this one service mounted on the production router.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router }
    }

    async fn call(&self, method: &str, path: &str, body: Option<serde_json::Value>) -> Reply {
        let payload = match &body {
            Some(value) => bytes::Bytes::from(serde_json::to_vec(value).expect("serialize body")),
            None => bytes::Bytes::new(),
        };
        let request = http::Request::builder()
            .method(method)
            .uri(path)
            .header("host", "network-security-manager.us-east-1.amazonaws.com")
            .header("content-type", "application/json")
            .header(
                "authorization",
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20261021/us-east-1/\
                 network-security-manager/aws4_request, SignedHeaders=host, Signature=0",
            )
            .body(payload)
            .expect("build the request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let raw = response.into_body();
        let json = if raw.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&raw).unwrap_or(serde_json::Value::Null)
        };
        Reply {
            status,
            error_code,
            json,
        }
    }
}

/// `RuleName`'s / `PolicyName`'s / `ScopeName`'s `@pattern` as the MODEL declares it
/// (`smithy_query(query="shape", shape="RuleName")`), and the expression inside its
/// whole-value anchors.
const NAME_DECLARED: &str = r"^[a-zA-Z0-9][a-zA-Z0-9 _.:/=+\-@]*$";
const NAME_STATED: &str = r"[a-zA-Z0-9][a-zA-Z0-9 _.:/=+\-@]*";

/// `Description`'s `@pattern` (`smithy_query(query="shape", shape="Description")`).
const DESCRIPTION_DECLARED: &str = r"^[a-zA-Z0-9 _.:/=+\-@]*$";
const DESCRIPTION_STATED: &str = r"[a-zA-Z0-9 _.:/=+\-@]*";

/// `IdempotencyToken`'s `@pattern`
/// (`smithy_query(query="shape", shape="IdempotencyToken")`).
const CLIENT_TOKEN_DECLARED: &str = r"^[\x21-\x7E]+$";
const CLIENT_TOKEN_STATED: &str = r"[\x21-\x7E]+";

/// `NextToken`'s `@pattern` (`smithy_query(query="shape", shape="NextToken")`).
const NEXT_TOKEN_DECLARED: &str = r"^[0-9A-Za-z:/+=_-]+$";
const NEXT_TOKEN_STATED: &str = r"[0-9A-Za-z:/+=_-]+";

/// `UpdateToken`'s `@pattern` (`smithy_query(query="shape", shape="UpdateToken")`) — the
/// one of these expressions whose body opens with a parenthesised group.
const UPDATE_TOKEN_DECLARED: &str = r"^([0-9a-f]{8})-([0-9a-f]{4}-){3}([0-9a-f]{12})$";
const UPDATE_TOKEN_STATED: &str = r"([0-9a-f]{8})-([0-9a-f]{4}-){3}([0-9a-f]{12})";

/// RULE: every `@pattern` refusal quotes the expression without its whole-value anchors.
#[tokio::test]
async fn a_pattern_refusal_quotes_the_expression_without_its_anchors() {
    let emulator = Emulator::new();

    /// One row: the case, the request, the member the clause names, the value it echoes,
    /// and the pattern as the model declares it and as the service states it.
    struct Row {
        label: &'static str,
        method: &'static str,
        path: &'static str,
        body: Option<serde_json::Value>,
        member: &'static str,
        value: &'static str,
        declared: &'static str,
        stated: &'static str,
    }

    let rows = vec![
        Row {
            label: "CreateRule.ruleName, the recorded case",
            method: "POST",
            path: "/rules",
            body: Some(serde_json::json!({
                "ruleName": "_sim2real-probe-nsm-bad-name",
                "firewallType": "WAF",
                "ruleType": "CONFIGURATION",
                "configuration": { "DefaultAction": "ALLOW" },
            })),
            member: "ruleName",
            value: "_sim2real-probe-nsm-bad-name",
            declared: NAME_DECLARED,
            stated: NAME_STATED,
        },
        Row {
            label: "CreatePolicy.policyName, the same expression on another operation",
            method: "POST",
            path: "/policies",
            body: Some(serde_json::json!({
                "policyName": " leading-space",
                "priority": 1,
                "firewallType": "SHIELD_ADVANCED",
                "policyConfiguration": {
                    "remediationEnabled": true,
                    "resourcesCleanUp": true,
                },
            })),
            member: "policyName",
            value: " leading-space",
            declared: NAME_DECLARED,
            stated: NAME_STATED,
        },
        Row {
            label: "CreateScope.scopeName",
            method: "POST",
            path: "/scopes",
            body: Some(serde_json::json!({
                "scopeName": "-starts-with-a-dash",
                "scopeConfiguration": {
                    "resourceScopes": { "AWS::EC2::EIP": { "includeAll": true } },
                },
            })),
            member: "scopeName",
            value: "-starts-with-a-dash",
            declared: NAME_DECLARED,
            stated: NAME_STATED,
        },
        Row {
            label: "CreateRule.ruleDescription, a different expression",
            method: "POST",
            path: "/rules",
            body: Some(serde_json::json!({
                "ruleName": "pattern-probe",
                "ruleDescription": "not allowed!",
                "firewallType": "WAF",
                "ruleType": "CONFIGURATION",
                "configuration": { "DefaultAction": "ALLOW" },
            })),
            member: "ruleDescription",
            value: "not allowed!",
            declared: DESCRIPTION_DECLARED,
            stated: DESCRIPTION_STATED,
        },
        Row {
            label: "CreateRule.clientToken, an expression written with hex escapes",
            method: "POST",
            path: "/rules",
            body: Some(serde_json::json!({
                "clientToken": "token with spaces",
                "ruleName": "pattern-probe",
                "firewallType": "WAF",
                "ruleType": "CONFIGURATION",
                "configuration": { "DefaultAction": "ALLOW" },
            })),
            member: "clientToken",
            value: "token with spaces",
            declared: CLIENT_TOKEN_DECLARED,
            stated: CLIENT_TOKEN_STATED,
        },
        Row {
            label: "ListPolicies.nextToken, a query parameter",
            method: "GET",
            path: "/policies?nextToken=not%20a%20token",
            body: None,
            member: "nextToken",
            value: "not a token",
            declared: NEXT_TOKEN_DECLARED,
            stated: NEXT_TOKEN_STATED,
        },
        Row {
            label: "UpdatePolicy.updateToken, an expression opening with a group",
            method: "PATCH",
            path: "/policies/0123456789abcdef",
            body: Some(serde_json::json!({
                "updateToken": "zzzzzzzz-zzzz-zzzz-zzzz-zzzzzzzzzzzz",
                "isPublished": true,
            })),
            member: "updateToken",
            value: "zzzzzzzz-zzzz-zzzz-zzzz-zzzzzzzzzzzz",
            declared: UPDATE_TOKEN_DECLARED,
            stated: UPDATE_TOKEN_STATED,
        },
    ];

    for row in rows {
        let label = row.label;
        let reply = emulator.call(row.method, row.path, row.body.clone()).await;
        assert_eq!(
            reply.status, 400,
            "case {label}: expected 400, got {} {:?}",
            reply.status, reply.json
        );
        assert_eq!(
            reply.error_code.as_deref(),
            Some("ValidationException"),
            "case {label}: unexpected x-amzn-errortype, got {:?}",
            reply.error_code
        );
        let message = reply.json["message"]
            .as_str()
            .unwrap_or_else(|| panic!("case {label}: a refusal states a message, got {:?}", reply.json))
            .to_string();
        assert_eq!(
            message,
            format!(
                "1 validation error detected: Value '{}' at '{}' failed to satisfy \
                 constraint: Member must satisfy regular expression pattern: {}",
                row.value, row.member, row.stated
            ),
            "case {label}: unexpected message"
        );
        // The anchored spelling is what the model declares and what a renderer that kept
        // the pair would quote, so its absence is the rule under test.
        assert!(
            !message.contains(row.declared),
            "case {label}: the quoted expression must carry no whole-value anchors, got \
             {message:?}"
        );
    }
}

/// RULE: only the `@pattern` rendering moves — a refusal of another constraint class keeps
/// the framework's own wording, including the value it echoes.
#[tokio::test]
async fn a_refusal_of_another_constraint_class_states_the_frameworks_requirement() {
    let emulator = Emulator::new();

    // The rows differ along the constraint class broken: a `@range` minimum, a `@range`
    // maximum, an absent `@required` member and a value outside an enumerated type.
    let rows: Vec<(&str, &str, &str, Option<serde_json::Value>, &str)> = vec![
        (
            "maxResults below its @range minimum",
            "GET",
            "/policies?maxResults=0",
            None,
            "Member must have value greater than or equal to 1",
        ),
        (
            "maxResults above its @range maximum",
            "GET",
            "/policies?maxResults=101",
            None,
            "Member must have value less than or equal to 100",
        ),
        (
            "an absent @required member",
            "POST",
            "/scopes",
            Some(serde_json::json!({
                "scopeConfiguration": {
                    "resourceScopes": { "AWS::EC2::EIP": { "includeAll": true } },
                },
            })),
            "Member must not be null",
        ),
        (
            "a value outside an enumerated type",
            "GET",
            "/policies?status=BOGUS",
            None,
            "Member must satisfy enum value set:",
        ),
    ];

    for (label, method, path, body, requirement) in rows {
        let reply = emulator.call(method, path, body).await;
        assert_eq!(
            reply.status, 400,
            "case {label}: expected 400, got {} {:?}",
            reply.status, reply.json
        );
        let message = reply.json["message"]
            .as_str()
            .unwrap_or_else(|| panic!("case {label}: a refusal states a message, got {:?}", reply.json))
            .to_string();
        assert!(
            message.starts_with("1 validation error detected: "),
            "case {label}: the framework's envelope must be kept, got {message:?}"
        );
        assert!(
            message.contains(requirement),
            "case {label}: expected the requirement {requirement:?}, got {message:?}"
        );
        assert!(
            !message.contains("regular expression pattern"),
            "case {label}: this refusal states no pattern, got {message:?}"
        );
    }
}
