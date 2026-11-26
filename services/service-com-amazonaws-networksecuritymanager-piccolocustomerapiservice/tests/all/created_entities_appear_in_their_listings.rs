// SPDX-License-Identifier: CC0-1.0

//! What a create makes visible to its type's listing: `CreateScope` then `ListScopes`,
//! `CreateTemplate` then `ListTemplates`.
//!
//! RULE under test: an entity a create persisted is a member of its own type's listing from
//! the next call on, its summary carrying the identifiers, name and type the create answered,
//! and the listing reports exactly the created set and nothing else, whether it is read in
//! one page or walked page by page through `nextToken`.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with a signed
//! restJson1 request — so what is asserted is what a client sees. The rows differ along what
//! the rule generalises over: which entity type was created, what the create submitted
//! (which resource types a scope's configuration selects and in which form, how many rules a
//! template associates, whether either is published, whether a description is carried) and
//! the page size the listing is walked with, including one row per page and the modelled
//! maximum.

use std::sync::Arc;

use service_com_amazonaws_networksecuritymanager_piccolocustomerapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// What a caller sees: the HTTP facts it branches on plus the JSON body.
#[derive(Debug)]
struct Reply {
    status: u16,
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
        // The wire behaviour under test is the operations', not SigV4's.
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
            // The credential scope is what routes the request to this service and names the
            // region its ARNs carry.
            .header(
                "authorization",
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20261021/us-east-1/\
                 network-security-manager/aws4_request, SignedHeaders=host, Signature=0",
            )
            .body(payload)
            .expect("build the request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let raw = response.into_body();
        let json = if raw.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&raw).unwrap_or(serde_json::Value::Null)
        };
        Reply { status, json }
    }

    async fn create(&self, collection: &str, body: serde_json::Value) -> Reply {
        self.call("POST", &format!("/{collection}"), Some(body)).await
    }

    /// Walk a listing to its end, returning every summary it reported in page order.
    ///
    /// `max_results` absent asks for the listing's own page size; present, it is what each
    /// page is cut to and `nextToken` is what resumes the walk.
    async fn list_all(
        &self,
        collection: &str,
        items_member: &str,
        max_results: Option<u32>,
    ) -> Vec<serde_json::Value> {
        let mut collected: Vec<serde_json::Value> = Vec::new();
        let mut next: Option<String> = None;
        // Generous: every page carries at least one row, so a terminating walk is far inside
        // this bound and a listing that never terminates fails instead of hanging.
        for _ in 0..512 {
            let mut query: Vec<String> = Vec::new();
            if let Some(limit) = max_results {
                query.push(format!("maxResults={limit}"));
            }
            if let Some(token) = &next {
                query.push(format!("nextToken={token}"));
            }
            let path = if query.is_empty() {
                format!("/{collection}")
            } else {
                format!("/{collection}?{}", query.join("&"))
            };
            let reply = self.call("GET", &path, None).await;
            assert_eq!(
                reply.status, 200,
                "case listing {collection} with maxResults={max_results:?}: expected 200, got {} {:?}",
                reply.status, reply.json
            );
            let page = reply.json[items_member].as_array().unwrap_or_else(|| {
                panic!(
                    "case listing {collection} with maxResults={max_results:?}: {items_member} is \
                     required, got {:?}",
                    reply.json
                )
            });
            collected.extend(page.iter().cloned());
            match reply.json["nextToken"].as_str() {
                Some(token) => next = Some(token.replace(':', "%3A")),
                None => return collected,
            }
        }
        panic!("case listing {collection}: the walk never terminated, collected {collected:?}");
    }
}

/// One created entity, as the create answered it: what its summary in the listing must say.
#[derive(Debug)]
struct Created {
    label: &'static str,
    id: String,
    arn: String,
    name: String,
    status: &'static str,
    /// The `firewallType` a template summary carries; a scope has no such member.
    firewall_type: Option<String>,
}

/// One row of the create table.
struct Row {
    label: &'static str,
    body: serde_json::Value,
    /// The `EntityStatus` the row's `isPublished` implies: `@default(true)` means an absent
    /// member is a published, ACTIVE resource.
    status: &'static str,
}

/// `count` distinct rule references, as a `CreateTemplate` body carries them: the model
/// documents `ruleIdentifier` as the rule's ARN, built with the same helper the handlers mint
/// ARNs with rather than from a literal.
fn rule_references(count: usize) -> serde_json::Value {
    serde_json::Value::Array(
        (0..count)
            .map(|index| {
                serde_json::json!({
                    "ruleIdentifier": engine::emulator_arn(
                        "network-security-manager",
                        "us-east-1",
                        &format!("rule/listedrule{index}"),
                    ),
                })
            })
            .collect(),
    )
}

/// The scope rows: they differ along which resource types the configuration selects
/// (`ScopeResourceType`'s values, `smithy_query(query="shape", shape="ScopeResourceType")`),
/// in which form each resource set selects them, whether the scope is published, and whether
/// a description is carried.
fn scope_rows() -> Vec<Row> {
    vec![
        Row {
            label: "every resource of one type",
            body: serde_json::json!({
                "scopeName": "eip-scope",
                "isPublished": true,
                "scopeConfiguration": {
                    "resourceScopes": { "AWS::EC2::EIP": { "includeAll": true } },
                },
            }),
            status: "ACTIVE",
        },
        Row {
            label: "a described draft selecting explicit ARNs",
            body: serde_json::json!({
                "scopeName": "distribution scope",
                "scopeDescription": "the edge distributions",
                "isPublished": false,
                "scopeConfiguration": {
                    "resourceScopes": {
                        "AWS::CloudFront::Distribution": {
                            "include": {
                                "explicitArns": [
                                    "arn:aws:cloudfront::000000000000:distribution/E1EXAMPLE",
                                ],
                            },
                        },
                    },
                },
            }),
            status: "DRAFT",
        },
        Row {
            label: "two resource types, publish state left to its modelled default",
            body: serde_json::json!({
                "scopeName": "Mixed.Scope_1",
                "scopeConfiguration": {
                    "resourceScopes": {
                        "AWS::ApiGateway::Stage": {
                            "exclude": {
                                "expression": { "criteria": { "tags": { "env": "prod" } } },
                            },
                        },
                        "AWS::ElasticLoadBalancing::LoadBalancer": { "includeAll": true },
                    },
                },
            }),
            status: "ACTIVE",
        },
        Row {
            label: "a tagged scope over application load balancers",
            body: serde_json::json!({
                "scopeName": "alb-scope",
                "isPublished": true,
                "tags": { "owner": "net-sec" },
                "scopeConfiguration": {
                    "resourceScopes": {
                        "AWS::ElasticLoadBalancingV2::LoadBalancer::application": {
                            "includeAll": true,
                        },
                    },
                },
            }),
            status: "ACTIVE",
        },
    ]
}

/// The template rows: they differ along how many rules the template associates, whether it is
/// published, and whether a description is carried.
fn template_rows() -> Vec<Row> {
    vec![
        Row {
            label: "one associated rule, published",
            body: serde_json::json!({
                "templateName": "edge-template",
                "firewallType": "WAF",
                "isPublished": true,
                "associatedRuleList": rule_references(1),
            }),
            status: "ACTIVE",
        },
        Row {
            label: "a described draft associating three rules",
            body: serde_json::json!({
                "templateName": "draft template",
                "templateDescription": "the staging rules",
                "firewallType": "WAF",
                "isPublished": false,
                "associatedRuleList": rule_references(3),
            }),
            status: "DRAFT",
        },
        Row {
            label: "publish state left to its modelled default",
            body: serde_json::json!({
                "templateName": "Defaulted.Template_1",
                "firewallType": "WAF",
                "associatedRuleList": rule_references(2),
            }),
            status: "ACTIVE",
        },
    ]
}

/// Create every row of a table, returning what each create answered.
async fn create_all(
    emulator: &Emulator,
    collection: &str,
    id_member: &str,
    arn_member: &str,
    name_member: &str,
    rows: Vec<Row>,
) -> Vec<Created> {
    let mut created = Vec::new();
    for row in rows {
        let label = row.label;
        let reply = emulator.create(collection, row.body.clone()).await;
        assert_eq!(
            reply.status, 201,
            "case {label}: creating in {collection} expected 201, got {} {:?}",
            reply.status, reply.json
        );
        assert_eq!(
            reply.json["status"].as_str(),
            Some(row.status),
            "case {label}: expected status {:?}, got {:?}",
            row.status,
            reply.json["status"]
        );
        created.push(Created {
            label,
            id: reply.string_at(id_member),
            arn: reply.string_at(arn_member),
            name: reply.string_at(name_member),
            status: row.status,
            firewall_type: reply.json["firewallType"].as_str().map(str::to_string),
        });
    }
    created
}

/// Assert that every created entity is listed exactly once, with the members the create
/// answered, and that the listing reports nothing else.
fn assert_membership(
    listed: &[serde_json::Value],
    created: &[Created],
    page_label: &str,
    id_member: &str,
    arn_member: &str,
    name_member: &str,
) {
    let listed_ids: Vec<String> = listed
        .iter()
        .map(|summary| {
            summary[id_member]
                .as_str()
                .unwrap_or_else(|| panic!("case {page_label}: {id_member} is required, got {summary:?}"))
                .to_string()
        })
        .collect();
    assert_eq!(
        listed_ids.len(),
        created.len(),
        "case {page_label}: expected one row per create, got {listed_ids:?}"
    );

    for entity in created {
        let matching: Vec<&serde_json::Value> = listed
            .iter()
            .filter(|summary| summary[id_member].as_str() == Some(entity.id.as_str()))
            .collect();
        assert_eq!(
            matching.len(),
            1,
            "case {page_label}: {:?} ({}) must be listed exactly once, got {} row(s) among {listed_ids:?}",
            entity.label,
            entity.id,
            matching.len()
        );
        let summary = matching[0];
        assert_eq!(
            summary[name_member].as_str(),
            Some(entity.name.as_str()),
            "case {page_label}: {:?} must be listed under the submitted {name_member}, got {:?}",
            entity.label,
            summary[name_member]
        );
        assert_eq!(
            summary[arn_member].as_str(),
            Some(entity.arn.as_str()),
            "case {page_label}: {:?} must be listed with the ARN the create answered, got {:?}",
            entity.label,
            summary[arn_member]
        );
        assert_eq!(
            summary["status"].as_str(),
            Some(entity.status),
            "case {page_label}: {:?} must be listed with its own status, got {:?}",
            entity.label,
            summary["status"]
        );
        assert_eq!(
            summary["version"].as_str(),
            Some("1"),
            "case {page_label}: {:?} was created once, so it is listed at version 1, got {:?}",
            entity.label,
            summary["version"]
        );
        if let Some(firewall_type) = &entity.firewall_type {
            assert_eq!(
                summary["firewallType"].as_str(),
                Some(firewall_type.as_str()),
                "case {page_label}: {:?} must be listed with the submitted firewallType, got {:?}",
                entity.label,
                summary["firewallType"]
            );
        }
    }
}

/// RULE: a created entity is a member of its type's listing with exactly the members the
/// create answered, however the listing is paged.
#[tokio::test]
async fn a_created_entity_is_listed_by_its_id_with_what_the_create_answered() {
    let emulator = Emulator::new();

    let scopes = create_all(
        &emulator,
        "scopes",
        "scopeId",
        "scopeArn",
        "scopeName",
        scope_rows(),
    )
    .await;
    let templates = create_all(
        &emulator,
        "templates",
        "templateId",
        "templateArn",
        "templateName",
        template_rows(),
    )
    .await;

    // The page sizes: the listing's own default, one row per page, a page shorter than the
    // row count, the exact count, and `MaxResults`' modelled maximum
    // (`smithy_query(query="operation", operation="ListScopes")`: range 1..100).
    for max_results in [None, Some(1), Some(2), Some(4), Some(100)] {
        let listed = emulator.list_all("scopes", "scopes", max_results).await;
        assert_membership(
            &listed,
            &scopes,
            &format!("ListScopes maxResults={max_results:?}"),
            "scopeId",
            "scopeArn",
            "scopeName",
        );

        let listed = emulator
            .list_all("templates", "templates", max_results)
            .await;
        assert_membership(
            &listed,
            &templates,
            &format!("ListTemplates maxResults={max_results:?}"),
            "templateId",
            "templateArn",
            "templateName",
        );
    }
}

/// RULE: each listing reports its OWN entity type out of the store — a created scope is a
/// member of `ListScopes` only, and a created template of `ListTemplates` only.
#[tokio::test]
async fn a_listing_reports_only_its_own_entity_type() {
    let emulator = Emulator::new();

    let scopes = create_all(
        &emulator,
        "scopes",
        "scopeId",
        "scopeArn",
        "scopeName",
        scope_rows(),
    )
    .await;
    let templates = create_all(
        &emulator,
        "templates",
        "templateId",
        "templateArn",
        "templateName",
        template_rows(),
    )
    .await;

    let listed_scopes = emulator.list_all("scopes", "scopes", None).await;
    let listed_templates = emulator.list_all("templates", "templates", None).await;

    // Rows: each created entity against the listing of the OTHER type, by id and by ARN.
    for (label, listing, foreign) in [
        ("ListScopes", &listed_scopes, &templates),
        ("ListTemplates", &listed_templates, &scopes),
    ] {
        for entity in foreign {
            let mentions: Vec<&serde_json::Value> = listing
                .iter()
                .filter(|summary| {
                    summary.to_string().contains(&entity.id)
                        || summary.to_string().contains(&entity.arn)
                })
                .collect();
            assert!(
                mentions.is_empty(),
                "case {label}: it must not report {:?} ({}), got {mentions:?}",
                entity.label,
                entity.id
            );
        }
    }

    assert_eq!(
        listed_scopes.len(),
        scopes.len(),
        "ListScopes must report one row per created scope, got {listed_scopes:?}"
    );
    assert_eq!(
        listed_templates.len(),
        templates.len(),
        "ListTemplates must report one row per created template, got {listed_templates:?}"
    );
}
