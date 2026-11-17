// SPDX-License-Identifier: CC0-1.0

//! WHICH segment of a deployment-parameter write's target is blamed when the
//! target does not resolve.
//!
//! The rule under test: a write resolves the resource its request's URI PATH
//! addresses before the resource a member of its BODY names, and the refusal
//! names the first of the two that does not resolve — the second is never
//! reached. `catalog` and `productId` are the operation's two `@httpLabel`
//! members (`POST /catalogs/{catalog}/products/{productId}/deployment-parameters`,
//! `smithy_query(query="operation",operation="PutDeploymentParameter")`) while
//! `agreementId` is carried in the body. So the blamed member is `ProductId`
//! whenever no agreement this account holds in the catalog covers the product the
//! path addresses — including a request naming neither a held product nor a held
//! agreement — and `AgreementId` only once that product resolves and the
//! agreement the body names is not one of those covering it.
//!
//! One order, for every request: nothing about the blamed member follows from
//! what else a request carries (tags, an idempotency token, an expiry), only from
//! what the account holds, which is what the shapes in the table below observe.
//!
//! The refusal is the modelled `ResourceNotFoundException` (HTTP 404), and its
//! message is one template: the fixed sentence
//! `"<Member>: Request references a resource which does not exist."`, whose only
//! variable part is the PascalCase name of that segment. No value from the
//! request — no id, no ARN — appears in it.
//!
//! Every case drives the real wire path through the harness the
//! `deployment_parameters` suite owns.

use serde_json::{json, Map, Value};
use service_com_amazonaws_marketplacedeployment_awsmpdeploymentparametersservice::state::MAX_TAGS_PER_RESOURCE;

use crate::deployment_parameters::{
    emulator, error_code, holds_agreement, holds_product, holds_target, message, provision, put,
    stored_parameters, CATALOG,
};

/// The modelled error and status an unresolved target carries.
const NOT_FOUND_CODE: &str = "ResourceNotFoundException";
const NOT_FOUND_STATUS: u16 = 404;

/// The one sentence the refusal states for the segment `member`.
fn not_found_sentence(member: &str) -> String {
    format!("{member}: Request references a resource which does not exist.")
}

/// One row: the target a request names, and the member its refusal must blame
/// (`None` when the whole target resolves and the write is served).
struct Row {
    case: &'static str,
    product_id: String,
    agreement_id: String,
    blamed: Option<&'static str>,
}

/// The agreements the account holds for the table below, as
/// `(agreementId, productIds)`. Written in the forms `ResourceId` admits
/// (`^[A-Za-z0-9_/-]+$`, `length 1..64`), including the one-character minimum and
/// the 64-character maximum, so the rule is exercised over the identifier forms it
/// generalises over and not only over one spelling.
fn held_agreements() -> Vec<(String, Vec<String>)> {
    vec![
        (
            "agmt-held-1".to_string(),
            vec!["prod-held-1".to_string(), "prod/shared/7".to_string()],
        ),
        (
            "agmt_held/2-x".to_string(),
            vec!["prod/shared/7".to_string()],
        ),
        ("a".to_string(), vec!["p".to_string()]),
        ("g".repeat(64), vec!["0".to_string(), "d".repeat(64)]),
    ]
}

/// A catalog OTHER than the one every request below names, holding one agreement
/// of its own: neither that agreement nor the product it covers is held in the
/// catalog the requests name.
const OTHER_CATALOG: &str = "AWSMarketplace-elsewhere";
/// The agreement that other catalog holds, and the product it covers.
const ELSEWHERE_AGREEMENT: &str = "agmt-elsewhere";
const ELSEWHERE_PRODUCT: &str = "prod-elsewhere-only";

/// The table. Each row differs from a resolving target in exactly one way, so what
/// it observes is the precedence rule: which segment fails to resolve, and in which
/// form.
fn table() -> Vec<Row> {
    let long_agreement = "g".repeat(64);
    let long_product = "d".repeat(64);
    vec![
        // No agreement of this catalog covers the product the path addresses, so
        // the product is the segment blamed — whatever the agreement is.
        Row {
            case: "neither the product nor the agreement is held",
            product_id: "prod-unknown".to_string(),
            agreement_id: "agmt-unknown".to_string(),
            blamed: Some("ProductId"),
        },
        Row {
            case: "a product nothing covers, under an agreement the account holds",
            product_id: "prod-unknown".to_string(),
            agreement_id: "agmt-held-1".to_string(),
            blamed: Some("ProductId"),
        },
        Row {
            case: "a product differing from a covered one only in case",
            product_id: "PROD-HELD-1".to_string(),
            agreement_id: "agmt-held-1".to_string(),
            blamed: Some("ProductId"),
        },
        Row {
            case: "segments held in another catalog only",
            product_id: ELSEWHERE_PRODUCT.to_string(),
            agreement_id: ELSEWHERE_AGREEMENT.to_string(),
            blamed: Some("ProductId"),
        },
        Row {
            case: "a one-character product nothing covers, under a one-character held agreement",
            product_id: "q".to_string(),
            agreement_id: "a".to_string(),
            blamed: Some("ProductId"),
        },
        Row {
            case: "a product nothing covers, under a 64-character held agreement",
            product_id: "prod-unknown".to_string(),
            agreement_id: long_agreement.clone(),
            blamed: Some("ProductId"),
        },
        // The product resolves: now the agreement the body names is what does not.
        Row {
            case: "a held product, under an agreement the account does not hold",
            product_id: "prod-held-1".to_string(),
            agreement_id: "agmt-unknown".to_string(),
            blamed: Some("AgreementId"),
        },
        Row {
            case: "a product two held agreements cover, under an unknown agreement",
            product_id: "prod/shared/7".to_string(),
            agreement_id: "agmt-unknown".to_string(),
            blamed: Some("AgreementId"),
        },
        Row {
            case: "an agreement differing from a held one only in case",
            product_id: "prod-held-1".to_string(),
            agreement_id: "AGMT-HELD-1".to_string(),
            blamed: Some("AgreementId"),
        },
        Row {
            case: "a held product, under a held agreement that covers another product only",
            product_id: "prod-held-1".to_string(),
            agreement_id: "agmt_held/2-x".to_string(),
            blamed: Some("AgreementId"),
        },
        Row {
            case: "a one-character held product, under an agreement nothing holds",
            product_id: "p".to_string(),
            agreement_id: "b".to_string(),
            blamed: Some("AgreementId"),
        },
        Row {
            case: "a 64-character held product, under a 64-character agreement nothing holds",
            product_id: long_product.clone(),
            agreement_id: "h".repeat(64),
            blamed: Some("AgreementId"),
        },
        Row {
            case: "a 64-character held product, under a held agreement that misses it",
            product_id: long_product.clone(),
            agreement_id: "a".to_string(),
            blamed: Some("AgreementId"),
        },
        // The controls: every segment resolves, so the write is served.
        Row {
            case: "a held agreement covering the product addressed",
            product_id: "prod-held-1".to_string(),
            agreement_id: "agmt-held-1".to_string(),
            blamed: None,
        },
        Row {
            case: "a shared product under the second agreement that covers it",
            product_id: "prod/shared/7".to_string(),
            agreement_id: "agmt_held/2-x".to_string(),
            blamed: None,
        },
        Row {
            case: "the one-character target",
            product_id: "p".to_string(),
            agreement_id: "a".to_string(),
            blamed: None,
        },
        Row {
            case: "the 64-character target",
            product_id: long_product,
            agreement_id: long_agreement,
            blamed: None,
        },
    ]
}

/// The request shapes each row is tried in. The blamed member follows the store
/// alone, so these differ along everything else a create may carry — no `tags`
/// member, an empty one, one entry, and entries up to the modelled maximum — and
/// every shape of one row must be answered identically.
struct Shape {
    case: &'static str,
    tags: Option<Map<String, Value>>,
}

fn shapes() -> Vec<Shape> {
    /// `count` well-formed tag pairs, in the entry forms the modelled `TagsMap`
    /// admits.
    fn tags(count: usize) -> Map<String, Value> {
        (0..count)
            .map(|index| (format!("key-{index:02}"), json!("v")))
            .collect()
    }
    vec![
        Shape {
            case: "no tags member at all",
            tags: None,
        },
        Shape {
            case: "a tags member carrying no entry",
            tags: Some(Map::new()),
        },
        Shape {
            case: "one tag",
            tags: Some(tags(1)),
        },
        Shape {
            case: "three tags",
            tags: Some(tags(3)),
        },
        Shape {
            // `TagsMap` is `@length max 50`
            // (`smithy_query(query="shape",shape="TagsMap")`), so this is the most
            // a request may carry and still reach the target check at all.
            case: "tags exactly at the modelled maximum",
            tags: Some(tags(MAX_TAGS_PER_RESOURCE)),
        },
    ]
}

/// The body one row is written with in one shape.
fn body(agreement_id: &str, shape: &Shape) -> Value {
    let mut body = json!({
        "agreementId": agreement_id,
        "deploymentParameter": { "name": "DbPassword", "secretString": "s3cr3t-value-1" },
    });
    if let Some(tags) = shape.tags.clone() {
        body.as_object_mut()
            .expect("an object body")
            .insert("tags".to_string(), Value::Object(tags));
    }
    body
}

#[tokio::test]
async fn the_refusal_blames_the_product_the_path_addresses_before_the_agreement_the_body_names() {
    let (router, engine) = emulator();
    for (agreement_id, product_ids) in held_agreements() {
        let products: Vec<&str> = product_ids.iter().map(String::as_str).collect();
        provision(&engine, CATALOG, &agreement_id, &products);
    }
    // Held in a catalog of its own, so neither it nor the product it covers is
    // held in the catalog every request below names.
    provision(
        &engine,
        OTHER_CATALOG,
        ELSEWHERE_AGREEMENT,
        &[ELSEWHERE_PRODUCT],
    );

    let mut served_targets = 0usize;
    for row in table() {
        let case = row.case;
        assert_eq!(
            holds_target(&engine, CATALOG, &row.product_id, &row.agreement_id),
            row.blamed.is_none(),
            "case {case:?}: the row must name a target the account holds exactly when it \
             expects to be served, got holds_target {}",
            holds_target(&engine, CATALOG, &row.product_id, &row.agreement_id)
        );
        // The row's own precondition: a row that expects `ProductId` names a
        // product no agreement of this catalog covers, and every other row names
        // one that is covered.
        assert_eq!(
            holds_product(&engine, CATALOG, &row.product_id),
            row.blamed != Some("ProductId"),
            "case {case:?}: the row must name a product the catalog covers exactly when it \
             expects another segment to be blamed, got holds_product {}",
            holds_product(&engine, CATALOG, &row.product_id)
        );

        if row.blamed.is_none() {
            served_targets += 1;
        }
        for shape in shapes() {
            let case = format!("{case} with {}", shape.case);
            let (status, answer) = put(
                &router,
                CATALOG,
                &row.product_id,
                &body(&row.agreement_id, &shape),
            )
            .await;

            let Some(member) = row.blamed else {
                assert_eq!(
                    status, 200,
                    "case {case:?}: a target the account holds must be written, \
                     got {status} {answer:?}"
                );
                continue;
            };

            assert_eq!(
                status, NOT_FOUND_STATUS,
                "case {case:?}: expected HTTP {NOT_FOUND_STATUS}, got {status} {answer:?}"
            );
            assert_eq!(
                error_code(&answer),
                NOT_FOUND_CODE,
                "case {case:?}: expected {NOT_FOUND_CODE}, got {answer:?}"
            );
            let reported = message(&answer);
            assert_eq!(
                reported,
                not_found_sentence(member),
                "case {case:?}: expected the sentence for {member}, got {reported:?}"
            );
            // The template's only variable part is the member, so nothing the
            // request sent may appear in it. Checked for the identifiers long
            // enough for the check to mean anything: a one-character id is a letter
            // the sentence's own fixed text carries anyway, and the equality above
            // already pins that text.
            let carried: Vec<&str> = [row.product_id.as_str(), row.agreement_id.as_str(), CATALOG]
                .into_iter()
                .filter(|value| value.len() > 2)
                .collect();
            for value in carried {
                assert!(
                    !reported.contains(value),
                    "case {case:?}: the refusal must carry no value from the request, \
                     got {reported:?} carrying {value:?}"
                );
            }
            assert!(
                answer.get("resourceArn").is_none()
                    && answer.get("deploymentParameterId").is_none(),
                "case {case:?}: a refused write must mint nothing, got {answer:?}"
            );
        }
    }

    // Every shape of one resolved target writes the SAME identity, so the store
    // holds one record per served target and the refused rows left nothing.
    let stored = stored_parameters(&engine);
    assert_eq!(
        stored, served_targets,
        "expected one record per resolved target ({served_targets}), got {stored} stored"
    );
}

#[tokio::test]
async fn the_blamed_segment_follows_what_the_account_holds_and_nothing_else() {
    // The same request, answered four ways as the store gains the segments it
    // names: the product the URI path addresses first, then the agreement the body
    // names. Nothing about the request changes between the calls, so the blamed
    // segment is a fact of what the account holds.
    let (router, engine) = emulator();
    let body = json!({
        "agreementId": "agmt-staged",
        "deploymentParameter": { "name": "Staged", "secretString": "s" },
    });

    let (status, answer) = put(&router, CATALOG, "prod-staged", &body).await;
    assert_eq!(
        status, NOT_FOUND_STATUS,
        "nothing is held yet, so the write must be refused, got {status} {answer:?}"
    );
    assert_eq!(
        message(&answer),
        not_found_sentence("ProductId"),
        "with the product covered by nothing the refusal must blame the product, got {answer:?}"
    );

    // The agreement the body names is held now, but it covers ANOTHER product: the
    // product the path addresses is still covered by nothing, so it stays the
    // blamed segment.
    provision(&engine, CATALOG, "agmt-staged", &["prod-elsewhere"]);
    assert!(
        holds_agreement(&engine, CATALOG, "agmt-staged"),
        "the agreement the body names must be held for this stage"
    );
    assert!(
        !holds_product(&engine, CATALOG, "prod-staged"),
        "no agreement of this catalog may cover the product yet"
    );
    let (status, answer) = put(&router, CATALOG, "prod-staged", &body).await;
    assert_eq!(
        status, NOT_FOUND_STATUS,
        "the product is still covered by nothing, so the write must be refused, \
         got {status} {answer:?}"
    );
    assert_eq!(
        message(&answer),
        not_found_sentence("ProductId"),
        "holding the agreement named does not resolve the product the path addresses, \
         got {answer:?}"
    );

    // The product is covered now, but by another agreement: the product resolves,
    // so the agreement the body names is what does not.
    provision(&engine, CATALOG, "agmt-other", &["prod-staged"]);
    assert!(
        holds_product(&engine, CATALOG, "prod-staged"),
        "the product the path addresses must be covered for this stage"
    );
    let (status, answer) = put(&router, CATALOG, "prod-staged", &body).await;
    assert_eq!(
        status, NOT_FOUND_STATUS,
        "the agreement named does not cover the product, so the write must be refused, \
         got {status} {answer:?}"
    );
    assert_eq!(
        message(&answer),
        not_found_sentence("AgreementId"),
        "with the product covered the refusal must blame the agreement, got {answer:?}"
    );

    // And once the agreement the body names covers the product too, the write is
    // served.
    provision(&engine, CATALOG, "agmt-staged", &["prod-staged"]);
    let (status, answer) = put(&router, CATALOG, "prod-staged", &body).await;
    assert_eq!(
        status, 200,
        "the resolved target must be written, got {status} {answer:?}"
    );
    let arn = answer.get("resourceArn").and_then(Value::as_str);
    assert!(
        arn.is_some_and(|arn| !arn.is_empty()),
        "the served write must report the ARN it minted, got {answer:?}"
    );
    assert_eq!(
        stored_parameters(&engine),
        1,
        "exactly the served write may have stored a record"
    );
}
