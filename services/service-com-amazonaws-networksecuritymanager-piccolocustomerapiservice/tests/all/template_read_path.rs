// SPDX-License-Identifier: CC0-1.0

//! The Template read path, end to end over HTTP: `CreateTemplate`, `GetTemplate` and
//! `ListTemplates`.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with a signed
//! restJson1 request — so what is asserted is what a client sees, not what a handler
//! returns.
//!
//! RULE under test: the same one the deployment read path proves — a read resolves its
//! identifier member (the entity's ARN, as the model documents it, or its bare id)
//! against that entity's store and answers `200` with the persisted record's members on a
//! hit and the MODELLED `ResourceNotFoundException` (HTTP 404) on a miss, and a listing
//! reports exactly the records a create persisted, filtered by `status` when the caller
//! asks.

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

impl Reply {
    fn string_at(&self, member: &str) -> String {
        self.json[member]
            .as_str()
            .unwrap_or_else(|| panic!("{member} is required, got {:?}", self.json))
            .to_string()
    }
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

    async fn create_template(&self, body: serde_json::Value) -> Reply {
        self.call("POST", "/templates", Some(body)).await
    }

    async fn get_template(&self, identifier: &str) -> Reply {
        self.call(
            "GET",
            &format!("/templates/{}", identifier.replace('/', "%2F")),
            None,
        )
        .await
    }

    /// One `ListTemplates` page.
    async fn list_templates(&self, query: &str) -> Reply {
        let path = if query.is_empty() {
            "/templates".to_string()
        } else {
            format!("/templates?{query}")
        };
        self.call("GET", &path, None).await
    }
}

/// An ARN of one of this service's resource types, built from the same helper the
/// handlers mint ARNs with rather than from a hardcoded literal.
fn resource_arn(resource_type: &str, id: &str) -> String {
    engine::emulator_arn(
        "network-security-manager",
        "us-east-1",
        &format!("{resource_type}/{id}"),
    )
}

/// A `CreateTemplate` body naming `rules` rule ARNs.
fn template_body(name: &str, is_published: Option<bool>, rules: usize) -> serde_json::Value {
    let mut body = serde_json::json!({
        "templateName": name,
        "firewallType": "WAF",
        "associatedRuleList": (0..rules)
            .map(|index| serde_json::json!({
                "ruleIdentifier": resource_arn("rule", &format!("templaterule{index}")),
            }))
            .collect::<Vec<_>>(),
    });
    if let Some(published) = is_published {
        body["isPublished"] = serde_json::json!(published);
    }
    body
}

/// RULE: a created template is read back, by its ARN and by its bare id, exactly as the
/// create returned it; and the listing — with and without the `status` filter — reports
/// exactly the templates that were created.
#[tokio::test]
async fn a_created_template_is_read_back_and_listed() {
    let emulator = Emulator::new();

    // The rows differ along publish state (including the defaulted absence) and how many
    // rules the template associates (`AssociatedRuleList` is `@length(1..50)`).
    let rows: Vec<(&str, Option<bool>, usize, &str)> = vec![
        ("published-by-default", None, 1, "ACTIVE"),
        ("explicitly published", Some(true), 3, "ACTIVE"),
        ("draft.template", Some(false), 2, "DRAFT"),
    ];

    let mut created: Vec<(String, String, &str)> = Vec::new();
    for (name, is_published, rules, expected_status) in rows {
        let reply = emulator
            .create_template(template_body(name, is_published, rules))
            .await;
        assert_eq!(
            reply.status, 201,
            "case {name}: expected 201, got {} {:?}",
            reply.status, reply.json
        );
        let template_id = reply.string_at("templateId");
        assert_eq!(
            reply.string_at("templateArn"),
            resource_arn("template", &template_id),
            "case {name}: unexpected templateArn"
        );
        assert_eq!(
            reply.string_at("status"),
            expected_status,
            "case {name}: unexpected status"
        );
        assert_eq!(
            reply.json["associatedRuleList"].as_array().map(Vec::len),
            Some(rules),
            "case {name}: unexpected associatedRuleList, got {:?}",
            reply.json["associatedRuleList"]
        );

        for (form, identifier) in [
            ("its ARN", reply.string_at("templateArn")),
            ("its bare id", template_id.clone()),
        ] {
            let read = emulator.get_template(&identifier).await;
            assert_eq!(
                read.status, 200,
                "case {name} read by {form}: expected 200, got {} {:?}",
                read.status, read.json
            );
            for member in ["templateId", "templateArn", "templateName", "status", "version"] {
                assert_eq!(
                    read.json[member], reply.json[member],
                    "case {name} read by {form}: {member} must match the create's, got {:?}",
                    read.json[member]
                );
            }
            assert_eq!(
                read.json["associatedRuleList"], reply.json["associatedRuleList"],
                "case {name} read by {form}: unexpected associatedRuleList"
            );
        }
        created.push((template_id, name.to_string(), expected_status));
    }

    // Every listing reports exactly the created templates that match it.
    for (query, label) in [("", "no filter"), ("status=ACTIVE", "status=ACTIVE"), ("status=DRAFT", "status=DRAFT")] {
        let reply = emulator.list_templates(query).await;
        assert_eq!(
            reply.status, 200,
            "case listing with {label}: expected 200, got {} {:?}",
            reply.status, reply.json
        );
        let listed: Vec<String> = reply.json["templates"]
            .as_array()
            .unwrap_or_else(|| {
                panic!(
                    "case listing with {label}: templates is required, got {:?}",
                    reply.json
                )
            })
            .iter()
            .map(|summary| {
                summary["templateId"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case listing with {label}: templateId is required, got {summary:?}")
                    })
                    .to_string()
            })
            .collect();
        let mut wanted: Vec<String> = created
            .iter()
            .filter(|(_, _, status)| query.is_empty() || query == format!("status={status}"))
            .map(|(id, _, _)| id.clone())
            .collect();
        wanted.sort();
        let mut actual = listed.clone();
        actual.sort();
        assert_eq!(
            actual, wanted,
            "case listing with {label}: unexpected templates"
        );
    }

    // One row per page: the token resumes where the previous page stopped.
    let first_page = emulator.list_templates("maxResults=1").await;
    assert_eq!(
        first_page.json["templates"].as_array().map(Vec::len),
        Some(1),
        "case the first page of one: expected one summary, got {:?}",
        first_page.json["templates"]
    );
    let token = first_page.json["nextToken"]
        .as_str()
        .unwrap_or_else(|| panic!("case the first page of one: a further page exists, so a nextToken must be stated, got {:?}", first_page.json))
        .to_string();
    let second_page = emulator
        .list_templates(&format!("maxResults=1&nextToken={token}"))
        .await;
    assert_eq!(
        second_page.status, 200,
        "case the second page: expected 200, got {} {:?}",
        second_page.status, second_page.json
    );
    assert_ne!(
        second_page.json["templates"][0]["templateId"], first_page.json["templates"][0]["templateId"],
        "case the second page: it must resume after the first page's row"
    );
}

/// RULE: an identifier no stored template answers to is refused with the modelled
/// `ResourceNotFoundException` — HTTP 404 and that error code — whatever form it takes.
#[tokio::test]
async fn an_unknown_template_identifier_is_refused_as_not_found() {
    let emulator = Emulator::new();
    let live = emulator
        .create_template(template_body("live-template", None, 1))
        .await;
    assert_eq!(
        live.status, 201,
        "case the live template: expected 201, got {} {:?}",
        live.status, live.json
    );

    // The rows differ along the FORM of the identifier: an ARN nothing minted, a bare id
    // nothing minted, an ARN of another resource type, and the one-character minimum of
    // `TemplateIdentifier`'s `@length(1..1010)`.
    let rows: Vec<(&str, String)> = vec![
        (
            "a template ARN that was never minted",
            resource_arn("template", "nevermintedtemplate1"),
        ),
        ("a bare id that was never minted", "nosuchtemplate1".to_string()),
        ("an ARN of another resource type", resource_arn("rule", "notatemplate1")),
        ("the shortest identifier the model admits", "x".to_string()),
    ];

    for (label, identifier) in rows {
        let reply = emulator.get_template(&identifier).await;
        assert_eq!(
            reply.status, 404,
            "case {label:?}: expected 404, got {} {:?}",
            reply.status, reply.json
        );
        assert_eq!(
            reply.error_code.as_deref(),
            Some("ResourceNotFoundException"),
            "case {label:?}: unexpected x-amzn-errortype, got {:?}",
            reply.error_code
        );
        assert_eq!(
            reply.json["resourceType"].as_str(),
            Some("Template"),
            "case {label:?}: unexpected resourceType, got {:?}",
            reply.json["resourceType"]
        );
    }

    // The refused reads left the store alone.
    assert_eq!(
        emulator
            .get_template(&live.string_at("templateId"))
            .await
            .status,
        200,
        "case after every refused read: the live template must still read back"
    );
}
