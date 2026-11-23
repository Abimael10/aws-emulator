// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for ApiGatewayV2 `UntagResource`, read back through `GetTags`.
//!
//! The rule these cases assert, one level above the recorded case that filed the issue:
//!
//! > `UntagResource` (`DELETE /v2/tags/{ResourceArn}`, modelled HTTP 204) names its target
//! > by ARN: it reads the ARN's resource portion as the read path of one resource of this
//! > service whose modelled shape carries a `Tags` member, and removes exactly the
//! > `tagKeys` the request names from THAT record's own tag collection — the one the
//! > resource's own reader and `GetTags`, addressed by the same ARN, both report — leaving
//! > every other tag as it was, and succeeding for
//! > a key the record does not carry, since "the resource carries no tag under this key" is
//! > already the asked-for result. A well-formed ARN of such a resource that the account
//! > holds no record for is 404 `NotFoundException` with the one sentence a tag operation
//! > answers an unresolved resource ARN with, whatever kind the ARN named; a value that is
//! > no ARN of this service's namespace, or whose resource portion names a kind this
//! > service does not tag, is 400 `BadRequestException`; and neither refusal changes a
//! > stored tag.
//!
//! The rows vary what the rule generalises over: WHICH kind of resource the ARN names (all
//! six whose shape carries a tag collection — an api, a stage of an api, a VPC link, a
//! custom domain name, a portal and a portal product, which is also both ARN forms this
//! service mints: the account-less management form and the account-qualified one), how many
//! keys the request names (one, several, every one the record carries, the same one twice),
//! whether a named key is one the record carries at all, and the form of the keys (ASCII, a
//! key carrying the separators AWS allows in a tag key, a non-ASCII key). The recorded unit
//! that filed the issue — an untag of an api that had been deleted — is one row of the
//! not-found table.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::api_lifecycle::{as_str, call, emulator, error_code, REGION};
use crate::path_label::addressed;

/// The tags collection every tag operation is addressed under
/// (`smithy_query(query="operation", operation="UntagResource")`: `/v2/tags/{ResourceArn}`).
const TAGS: &str = "/v2/tags";

/// The emulator's canonical account id, the account segment of the ARNs this service mints
/// for its portal-plane resources.
const ACCOUNT_ID: &str = "000000000000";

/// The sentence this API answers a caller-supplied ARN it cannot read with. Spelled out
/// here rather than taken from the crate's constant: a test that asserts a handler's own
/// constant back at it cannot catch the wording drifting away from the service's.
const INVALID_ARN: &str = "Invalid ARN specified in the request";

/// The sentence this API answers a tag operation whose `ResourceArn` resolves to no
/// resource the account holds with — one sentence for every taggable kind, since a tag
/// operation addresses a RESOURCE rather than a typed identifier. Spelled out here for the
/// same reason [`INVALID_ARN`] is.
const RESOURCE_NOT_FOUND: &str = "Resource was not found";

/// One resource class the rule spans: the ARN that names the resource created for a row,
/// and the URI its own reader reports it at.
struct Target {
    kind: &'static str,
    arn: String,
    read_uri: String,
}

/// An ACM certificate ARN, the only source `DomainNameConfiguration$CertificateArn`
/// documents ("AWS Certificate Manager is the only supported source").
fn certificate_arn(name: &str) -> String {
    engine::emulator_arn("acm", REGION, &format!("certificate/{name}"))
}

/// A `PortalContent` carrying only its required members
/// (`smithy_query(query="shape", shape="PortalContent")`: `DisplayName` and `Theme`).
fn portal_content(display_name: &str) -> Value {
    json!({
        "displayName": display_name,
        "theme": { "customColors": {
            "accentColor": "#ff9900",
            "backgroundColor": "#ffffff",
            "errorValidationColor": "#d13212",
            "headerColor": "#232f3e",
            "navigationColor": "#16191f",
            "textColor": "#111111"
        } }
    })
}

/// Create one resource of each taggable class, all carrying `tags`, and answer the ARN and
/// the read URI of each.
///
/// `label` keeps the names of one row's resources apart from another's. The api and the
/// stage, the VPC link and the custom domain name are named by the account-less management
/// ARN API Gateway documents for its own control-plane resources; the portal and the portal
/// product report an ARN of their own, which is read back from the create.
async fn taggable_resources(
    emulator: &MultiServiceRouter,
    label: &str,
    tags: &Value,
) -> Vec<Target> {
    let (status, api) = call(
        emulator,
        "POST",
        "/v2/apis",
        Some(json!({
            "name": format!("{label}-api"),
            "protocolType": "HTTP",
            "tags": tags
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {label:?}: expected 201 from CreateApi, got {status} {api}"
    );
    let api_id = as_str(&api, "apiId");

    // The stage that serves an api's root: a name carrying a character that is not
    // URI-safe, so the ARN naming it is percent-encoded the way an SDK client sends it.
    let stage_name = "$default";
    let (status, stage) = call(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/stages"),
        Some(json!({ "stageName": stage_name, "tags": tags })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {label:?}: expected 201 from CreateStage, got {status} {stage}"
    );

    let (status, link) = call(
        emulator,
        "POST",
        "/v2/vpclinks",
        Some(json!({
            "name": format!("{label}-link"),
            "subnetIds": ["subnet-0a1b2c3d"],
            "tags": tags
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {label:?}: expected 201 from CreateVpcLink, got {status} {link}"
    );
    let vpc_link_id = as_str(&link, "vpcLinkId");

    let domain = format!("{label}.example.com");
    let (status, domain_name) = call(
        emulator,
        "POST",
        "/v2/domainnames",
        Some(json!({
            "domainName": domain,
            "domainNameConfigurations": [{ "certificateArn": certificate_arn(label) }],
            "tags": tags
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {label:?}: expected 201 from CreateDomainName, got {status} {domain_name}"
    );

    let (status, portal) = call(
        emulator,
        "POST",
        "/v2/portals",
        Some(json!({
            "authorization": { "none": {} },
            "endpointConfiguration": { "none": {} },
            "portalContent": portal_content(&format!("{label} portal")),
            "tags": tags
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {label:?}: expected 201 from CreatePortal, got {status} {portal}"
    );
    let portal_id = as_str(&portal, "portalId");

    let (status, product) = call(
        emulator,
        "POST",
        "/v2/portalproducts",
        Some(json!({ "displayName": format!("{label} product"), "tags": tags })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {label:?}: expected 201 from CreatePortalProduct, got {status} {product}"
    );
    let product_id = as_str(&product, "portalProductId");

    vec![
        Target {
            kind: "an api",
            arn: format!("arn:aws:apigateway:{REGION}::/apis/{api_id}"),
            read_uri: format!("/v2/apis/{api_id}"),
        },
        Target {
            kind: "a stage of an api",
            arn: format!("arn:aws:apigateway:{REGION}::/apis/{api_id}/stages/{stage_name}"),
            read_uri: format!("/v2/apis/{api_id}/stages/{}", addressed(stage_name)),
        },
        Target {
            kind: "a VPC link",
            arn: format!("arn:aws:apigateway:{REGION}::/vpclinks/{vpc_link_id}"),
            read_uri: format!("/v2/vpclinks/{vpc_link_id}"),
        },
        Target {
            kind: "a custom domain name",
            arn: format!("arn:aws:apigateway:{REGION}::/domainnames/{domain}"),
            read_uri: format!("/v2/domainnames/{domain}"),
        },
        Target {
            kind: "a portal",
            arn: as_str(&portal, "portalArn"),
            read_uri: format!("/v2/portals/{portal_id}"),
        },
        Target {
            kind: "a portal product",
            arn: as_str(&product, "portalProductArn"),
            read_uri: format!("/v2/portalproducts/{product_id}"),
        },
    ]
}

/// The untag request for `arn` and `keys`: the ARN as the one path label of the operation's
/// URI and each key as one `tagKeys` query value, spelled the way an SDK client sends them.
fn untag_uri(arn: &str, keys: &[&str]) -> String {
    let query: Vec<String> = keys
        .iter()
        .map(|key| format!("tagKeys={}", addressed(key)))
        .collect();
    format!("{TAGS}/{}?{}", addressed(arn), query.join("&"))
}

/// The tag collection `target` reports, read BOTH ways and asserted to agree: through the
/// resource's own reader, and through `GetTags`, which is addressed by the very ARN the
/// untag is — one collection, so a removal cannot show up in one reader and not the other.
/// A reader that reports no `tags` member at all reports no tags.
async fn read_tags(emulator: &MultiServiceRouter, case: &str, target: &Target) -> Value {
    let (status, body) = call(emulator, "GET", &target.read_uri, None).await;
    assert_eq!(
        status,
        200,
        "case {case:?}: expected 200 reading {} back, got {status} {body}",
        target.read_uri
    );
    let reported = body.get("tags").cloned().unwrap_or_else(|| json!({}));

    let tags_uri = format!("{TAGS}/{}", addressed(&target.arn));
    let (status, body) = call(emulator, "GET", &tags_uri, None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from GetTags at {tags_uri}, got {status} {body}"
    );
    let through_get_tags = body.get("tags").cloned().unwrap_or_else(|| json!({}));
    assert_eq!(
        through_get_tags, reported,
        "case {case:?}: expected GetTags to report the tags the resource's own reader does"
    );
    reported
}

/// One removal row: the tags a resource is created with, the keys the untag names, and the
/// tags the resource must report afterwards.
struct Row {
    case: &'static str,
    /// The label the row's resources are named from — `label` of [`taggable_resources`].
    label: &'static str,
    tags: Value,
    remove: &'static [&'static str],
    remaining: Value,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one key of three, leaving the other two",
            label: "one-of-three",
            tags: json!({ "team": "payments", "tier": "gold", "env": "test" }),
            remove: &["tier"],
            remaining: json!({ "team": "payments", "env": "test" }),
        },
        Row {
            case: "several keys at once, leaving one",
            label: "several",
            tags: json!({ "team": "payments", "tier": "gold", "env": "test" }),
            remove: &["team", "env"],
            remaining: json!({ "tier": "gold" }),
        },
        Row {
            case: "every key the resource carries, leaving no tag at all",
            label: "every-key",
            tags: json!({ "only": "one" }),
            remove: &["only"],
            remaining: json!({}),
        },
        Row {
            case: "a key the resource does not carry, which changes nothing",
            label: "absent-key",
            tags: json!({ "team": "payments" }),
            remove: &["nosuchkey"],
            remaining: json!({ "team": "payments" }),
        },
        Row {
            case: "a key the resource carries and one it does not, in one request",
            label: "mixed-keys",
            tags: json!({ "team": "payments", "tier": "gold" }),
            remove: &["nosuchkey", "team"],
            remaining: json!({ "tier": "gold" }),
        },
        Row {
            // "A tag key can have a maximum of 128 characters, and can contain only
            // Unicode letters, digits, white space, or these symbols: _ . : / = + - @"
            // — the separators are part of a key, so a key carrying them is one key.
            case: "a key carrying the separators a tag key may contain",
            label: "separators",
            tags: json!({ "cost:center/eu": "42", "team": "payments" }),
            remove: &["cost:center/eu"],
            remaining: json!({ "team": "payments" }),
        },
        Row {
            // A tag key may carry any Unicode letter, so a non-ASCII key is one key too.
            case: "a non-ASCII key",
            label: "non-ascii",
            tags: json!({ "équipe": "paiements", "team": "payments" }),
            remove: &["équipe"],
            remaining: json!({ "team": "payments" }),
        },
        Row {
            // `tagKeys` is a list, so the same key may arrive twice; removing it is
            // stated as a result, so naming it twice removes it once and succeeds.
            case: "the same key named twice in one request",
            label: "repeated-key",
            tags: json!({ "team": "payments", "tier": "gold" }),
            remove: &["team", "team"],
            remaining: json!({ "tier": "gold" }),
        },
    ]
}

#[tokio::test]
async fn untag_resource_removes_the_named_keys_from_the_resource_its_arn_names() {
    let emulator = emulator();

    for row in rows() {
        for target in taggable_resources(&emulator, row.label, &row.tags).await {
            let case = format!("{} of {}", row.case, target.kind);

            // The record carries what it was created with, before anything is removed.
            assert_eq!(
                read_tags(&emulator, &case, &target).await,
                row.tags,
                "case {case:?}: expected the created tag set before the untag"
            );

            // The modelled 204, with a response structure the model declares no members
            // on — so there is nothing about the resource in the answer.
            let uri = untag_uri(&target.arn, row.remove);
            let (status, body) = call(&emulator, "DELETE", &uri, None).await;
            assert_eq!(
                status, 204,
                "case {case:?}: expected 204 from UntagResource at {uri}, got {status} {body}"
            );
            assert!(
                body.is_null() || body.as_object().is_some_and(serde_json::Map::is_empty),
                "case {case:?}: expected UntagResource to report no members, got {body}"
            );

            // Exactly the named keys are gone, from the record the resource's own reader
            // reports — and a second, identical untag is the same answer and the same
            // state, so the removal is idempotent.
            for round in 0..2 {
                assert_eq!(
                    read_tags(&emulator, &case, &target).await,
                    row.remaining,
                    "case {case:?} (round {round}): expected the tags left after removing \
                     {:?}",
                    row.remove
                );
                let (status, body) = call(&emulator, "DELETE", &uri, None).await;
                assert_eq!(
                    status, 204,
                    "case {case:?} (round {round}): expected a repeated UntagResource to be \
                     204, got {status} {body}"
                );
            }
        }
    }
}

#[tokio::test]
async fn untagging_one_resource_leaves_every_other_resource_tagged() {
    let emulator = emulator();
    let tags = json!({ "team": "payments", "tier": "gold" });

    // Two resources of every class, created from the same tag set.
    let first = taggable_resources(&emulator, "untouched-first", &tags).await;
    let second = taggable_resources(&emulator, "untouched-second", &tags).await;

    for target in &first {
        let case = format!("untagging {}", target.kind);
        let (status, body) = call(
            &emulator,
            "DELETE",
            &untag_uri(&target.arn, &["team"]),
            None,
        )
        .await;
        assert_eq!(
            status, 204,
            "case {case:?}: expected 204 from UntagResource, got {status} {body}"
        );
    }

    // Every sibling kept both its tags: the removal reached one record only.
    for target in &second {
        let case = format!("the sibling {} after the untags", target.kind);
        assert_eq!(
            read_tags(&emulator, &case, target).await,
            tags,
            "case {case:?}: expected the sibling's tag set to be untouched"
        );
    }
}

#[tokio::test]
async fn an_arn_that_names_no_held_resource_is_a_not_found_that_removes_nothing() {
    let emulator = emulator();
    let tags = json!({ "team": "payments" });
    let held = taggable_resources(&emulator, "still-held", &tags).await;

    // The api of this row is DELETED, so its ARN is well formed and names nothing the
    // account holds any more — the recorded case that filed the issue.
    let (status, deleted_api) = call(
        &emulator,
        "POST",
        "/v2/apis",
        Some(json!({ "name": "deleted-api", "protocolType": "HTTP", "tags": tags })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case \"the api that is then deleted\": expected 201 from CreateApi, got {status} {deleted_api}"
    );
    let deleted_api_id = as_str(&deleted_api, "apiId");
    let (status, body) = call(
        &emulator,
        "DELETE",
        &format!("/v2/apis/{deleted_api_id}"),
        None,
    )
    .await;
    assert_eq!(
        status, 204,
        "case \"the api that is then deleted\": expected 204 from DeleteApi, got {status} {body}"
    );

    // Every row earns the SAME sentence: a tag operation addresses its target by ARN, so
    // the refusal is about the resource the ARN named rather than about an identifier of a
    // particular kind. Spelled out rather than taken from the crate's constant, so wording
    // that drifts away from the service's is caught.
    let cases: Vec<(&str, String)> = vec![
        (
            "an api id nothing minted",
            format!("arn:aws:apigateway:{REGION}::/apis/nosuchapi00"),
        ),
        (
            "the ARN of an api that was deleted",
            format!("arn:aws:apigateway:{REGION}::/apis/{deleted_api_id}"),
        ),
        (
            "a stage of an api nothing minted",
            format!("arn:aws:apigateway:{REGION}::/apis/nosuchapi00/stages/prod"),
        ),
        (
            "a stage name the api holds no stage under",
            format!(
                "arn:aws:apigateway:{REGION}::/apis/{}/stages/nosuchstage",
                api_id_of(&held)
            ),
        ),
        (
            "a VPC link id nothing minted",
            format!("arn:aws:apigateway:{REGION}::/vpclinks/nosuchlink"),
        ),
        (
            "a domain name the account does not hold",
            format!("arn:aws:apigateway:{REGION}::/domainnames/absent.example.com"),
        ),
        (
            "a portal id nothing minted",
            format!("arn:aws:apigateway:{REGION}:{ACCOUNT_ID}:/portals/nosuchportal"),
        ),
        (
            "a portal product id nothing minted",
            format!("arn:aws:apigateway:{REGION}:{ACCOUNT_ID}:/portalproducts/nosuchprod0"),
        ),
    ];

    for (case, arn) in cases {
        // Both tag operations resolve the same ARN, so both refuse an unresolved one with
        // the same status, the same code and the same sentence.
        for (operation, method, uri) in [
            ("UntagResource", "DELETE", untag_uri(&arn, &["team"])),
            ("GetTags", "GET", format!("{TAGS}/{}", addressed(&arn))),
        ] {
            let (status, body) = call(&emulator, method, &uri, None).await;
            assert_eq!(
                status, 404,
                "case {case:?}: expected 404 from {operation}, got {status} {body}"
            );
            assert_eq!(
                error_code(&body),
                "NotFoundException",
                "case {case:?}: expected NotFoundException from {operation}, got {body}"
            );
            assert_eq!(
                as_str(&body, "message"),
                RESOURCE_NOT_FOUND,
                "case {case:?}: expected {operation} to answer the sentence this API answers \
                 a tag operation's unresolved resource ARN with, got {body}"
            );
        }
    }

    // A refused untag removed nothing: every held resource still carries its tag.
    for target in &held {
        let case = format!("the held {} after the refusals", target.kind);
        assert_eq!(
            read_tags(&emulator, &case, target).await,
            tags,
            "case {case:?}: expected a refused UntagResource to remove nothing"
        );
    }
}

/// The `ApiId` of the api among `targets` — the row's api is the first one.
fn api_id_of(targets: &[Target]) -> String {
    targets
        .first()
        .expect("the api is the first taggable resource of a row")
        .read_uri
        .rsplit('/')
        .next()
        .expect("the api's read URI ends in its id")
        .to_string()
}

#[tokio::test]
async fn a_value_that_names_no_taggable_resource_of_this_service_is_a_bad_request() {
    let emulator = emulator();
    let tags = json!({ "team": "payments" });
    let held = taggable_resources(&emulator, "bad-request", &tags).await;
    let api_id = api_id_of(&held);

    for (case, value) in [
        ("a value that is no ARN at all", "not-an-arn".to_string()),
        (
            "an ARN missing its resource field",
            format!("arn:aws:apigateway:{REGION}:{ACCOUNT_ID}"),
        ),
        (
            "an ARN whose resource field is empty",
            format!("arn:aws:apigateway:{REGION}::"),
        ),
        (
            "an ARN of another vendor's resource",
            "arn:aws:s3:::a-bucket".to_string(),
        ),
        (
            "an ARN of the other API Gateway control plane's REST api",
            format!("arn:aws:apigateway:{REGION}::/restapis/abcdef1234"),
        ),
        (
            "an api child this service does not tag",
            format!("arn:aws:apigateway:{REGION}::/apis/{api_id}/routes/abcdefg"),
        ),
        (
            "the apis collection rather than one api",
            format!("arn:aws:apigateway:{REGION}::/apis"),
        ),
        (
            "an api ARN with an empty id segment",
            format!("arn:aws:apigateway:{REGION}::/apis/"),
        ),
    ] {
        // Both tag operations read the same ARN, so both refuse a value that names no
        // taggable resource of this service the same way.
        for (operation, method, uri) in [
            ("UntagResource", "DELETE", untag_uri(&value, &["team"])),
            ("GetTags", "GET", format!("{TAGS}/{}", addressed(&value))),
        ] {
            let (status, body) = call(&emulator, method, &uri, None).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected 400 from {operation} for {value:?}, got {status} {body}"
            );
            assert_eq!(
                error_code(&body),
                "BadRequestException",
                "case {case:?}: expected BadRequestException from {operation}, got {body}"
            );
            assert_eq!(
                as_str(&body, "message"),
                INVALID_ARN,
                "case {case:?}: expected {operation} to answer the sentence this API answers \
                 an unreadable ARN with, got {body}"
            );
        }
    }

    // A refused untag removed nothing.
    for target in &held {
        let case = format!("the held {} after the refusals", target.kind);
        assert_eq!(
            read_tags(&emulator, &case, target).await,
            tags,
            "case {case:?}: expected a refused UntagResource to remove nothing"
        );
    }
}

#[tokio::test]
async fn the_tags_an_untag_leaves_are_the_ones_the_store_holds() {
    // The projection every reader of one resource serves comes from the stored record, so
    // the keys a removal left are a fact about the store and not about the answer: the
    // store is read directly here, beside the reader's own report.
    let engine: std::sync::Arc<engine::InMemoryEngine> =
        std::sync::Arc::new(engine::InMemoryEngine::new());
    let mut emulator = MultiServiceRouter::new();
    emulator.set_request_authentication(false);
    emulator.register_service(
        service_com_amazonaws_apigatewayv2_apigatewayv2::EmulatedService::new(
            std::sync::Arc::clone(&engine),
        ),
    );

    let (status, api) = call(
        &emulator,
        "POST",
        "/v2/apis",
        Some(json!({
            "name": "stored",
            "protocolType": "HTTP",
            "tags": { "team": "payments", "tier": "gold" }
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case \"the stored api\": expected 201 from CreateApi, got {status} {api}"
    );
    let api_id = as_str(&api, "apiId");

    let arn = format!("arn:aws:apigateway:{REGION}::/apis/{api_id}");
    let (status, body) = call(&emulator, "DELETE", &untag_uri(&arn, &["tier"]), None).await;
    assert_eq!(
        status, 204,
        "case \"the stored api\": expected 204 from UntagResource, got {status} {body}"
    );

    let stored: BTreeMap<String, String> = engine
        .state::<service_com_amazonaws_apigatewayv2_apigatewayv2::ComAmazonawsApigatewayv2Apigatewayv2State>()
        .lock()
        .apis
        .get(&api_id)
        .expect("the api the create persisted")
        .tags
        .clone();
    assert_eq!(
        stored,
        BTreeMap::from([("team".to_string(), "payments".to_string())]),
        "case \"the stored api\": expected the store itself to hold only the keys the untag \
         left, got {stored:?}"
    );
}
