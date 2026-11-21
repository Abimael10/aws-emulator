// SPDX-License-Identifier: CC0-1.0

//! What `UpdateDomainName` PERSISTS, end to end over HTTP.
//!
//! The rule asserted here: an update writes the one settable member its input carries
//! (`description`) onto the STORED custom-domain record and touches nothing else, so
//!
//! 1. every reader of that record — `GetDomainName` and `ListDomainNames` — reports what the
//!    LAST accepted update stored, through any number of successive updates, and repeating an
//!    update answers the same configuration again (it is idempotent);
//! 2. an update addresses the ONE domain its path names, so the other domains this account
//!    holds keep the configuration they were created with;
//! 3. state the update's input does not address survives it — including the API association
//!    the domain carries, which is read through its own operation rather than out of the
//!    `DomainNameConfig`.
//!
//! The rows differ along the form of the domain name, the number of successive updates and
//! the form of each new `description` (absent, empty, multi-line and multi-byte, the modelled
//! maximum length), so neither claim can be keyed to one request or one kind of value. The
//! sibling suite `domain_name_update_delete` asserts the single-update replacement and the
//! refusals; this one asserts that the write is durable, scoped to one domain and narrow.
//!
//! The harness (`call`, `router`, `Answer`, `create_api`) is the source-API association
//! suite's, and the domain path and maximum-length domain builder the update/delete suite's —
//! reused rather than copied.

use serde_json::{json, Value};
use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::state;
use smithy_server_runtime::MultiServiceRouter;

use crate::domain_name_update_delete::{max_length_domain, DOMAIN_NAMES_PATH};
use crate::source_api_association::{call, create_api, router, Answer};

/// The largest `description` the model admits: the `description` member of
/// `UpdateDomainNameRequest` targets `Description`, which is `@length` 0..255. Source:
/// `smithy_query(query="operation", operation="UpdateDomainName")`.
const DESCRIPTION_MAX_LEN: usize = 255;

/// The members of `DomainNameConfig` that no `UpdateDomainName` input addresses, so each one
/// must survive every update. Source: `smithy_query(query="shape", shape="DomainNameConfig")`
/// against `UpdateDomainName`'s input, whose only settable member is `description`.
const UNADDRESSED_MEMBERS: &[&str] = &[
    "domainName",
    "certificateArn",
    "appsyncDomainName",
    "hostedZoneId",
    "domainNameArn",
    "tags",
];

/// The association statuses a mapping being deployed and a deployed mapping carry. Source:
/// the `AssociationStatus` enum, `smithy_query(query="shape", shape="AssociationStatus")`.
const ASSOCIATION_STATUSES: &[&str] = &["PROCESSING", "SUCCESS", "FAILED"];

/// A description of exactly `length` characters, built from newline-bearing and multi-byte
/// text so a row exercises content as well as size. `chars`, because the model measures
/// `@length` in characters.
fn description(length: usize) -> String {
    let mut text = String::new();
    while text.chars().count() < length {
        text.push_str("the payments domain — 支払い\n");
    }
    text.chars().take(length).collect()
}

/// `GET /v1/domainnames/{domainName}`.
async fn get_domain(router: &MultiServiceRouter, domain: &str) -> Answer {
    call(router, "GET", &format!("{DOMAIN_NAMES_PATH}/{domain}"), None).await
}

/// `POST /v1/domainnames/{domainName}`, carrying `description` when the row names one.
async fn update_domain(
    router: &MultiServiceRouter,
    domain: &str,
    new_description: Option<&str>,
) -> Answer {
    let mut body = json!({});
    if let Some(text) = new_description {
        body["description"] = json!(text);
    }
    call(
        router,
        "POST",
        &format!("{DOMAIN_NAMES_PATH}/{domain}"),
        Some(body),
    )
    .await
}

/// The `DomainNameConfig` `ListDomainNames` reports for `domain`, or `Value::Null` when the
/// listing does not hold it.
async fn listed_config(router: &MultiServiceRouter, domain: &str) -> Value {
    let listed = call(router, "GET", DOMAIN_NAMES_PATH, None).await;
    listed.body["domainNameConfigs"]
        .as_array()
        .and_then(|entries| {
            entries
                .iter()
                .find(|entry| entry["domainName"] == json!(domain))
                .cloned()
        })
        .unwrap_or(Value::Null)
}

/// Create one custom domain and answer the `DomainNameConfig` the create stored.
async fn create_domain(
    router: &MultiServiceRouter,
    domain: &str,
    created_description: Option<&str>,
    tags: Option<Value>,
) -> Value {
    let mut body = json!({
        "domainName": domain,
        "certificateArn": format!(
            "arn:aws:acm:us-east-1:000000000000:certificate/{}",
            domain.len()
        ),
    });
    if let Some(text) = created_description {
        body["description"] = json!(text);
    }
    if let Some(tags) = tags {
        body["tags"] = tags;
    }
    let answer = call(router, "POST", DOMAIN_NAMES_PATH, Some(body)).await;
    assert_eq!(
        answer.status, 200,
        "case {domain:?}: expected CreateDomainName 200, got {answer:?}"
    );
    answer.body["domainNameConfig"].clone()
}

/// The `description` a `DomainNameConfig` should carry for `text`: the member is absent from
/// the body when the stored record carries none.
fn expected_description(text: Option<&str>) -> Value {
    text.map_or(Value::Null, |text| json!(text))
}

/// The description the independence sweep gives the row at `index`: distinct per row and
/// well inside the modelled `@length`, whatever the row's domain name is.
fn sweep_description(index: usize) -> String {
    format!("the description row {index} stored")
}

/// One row of the persistence table: a domain and the successive descriptions it is updated
/// with, in order.
struct Row {
    label: &'static str,
    domain: String,
    /// The description the domain is CREATED with.
    created: Option<String>,
    /// The descriptions the successive updates carry, each `None` for an update that omits
    /// the member.
    updates: Vec<Option<String>>,
    tags: Option<Value>,
}

/// The persistence table: the rows differ along the form of the domain name, the description
/// the create stored, how MANY updates follow, and the form of each new description.
fn rows() -> Vec<Row> {
    vec![
        Row {
            label: "one update onto a domain created without a description",
            domain: "p0.example".to_string(),
            created: None,
            updates: vec![Some("the payments domain".to_string())],
            tags: None,
        },
        Row {
            label: "three updates ending on the empty description",
            domain: "api.p1.example.com".to_string(),
            created: Some("the first description".to_string()),
            updates: vec![
                Some(description(32)),
                Some(description(DESCRIPTION_MAX_LEN)),
                Some(String::new()),
            ],
            tags: Some(json!({"stage": "beta"})),
        },
        Row {
            label: "an update that drops the description, then one that restores it",
            domain: "api-7.p2-x.example.com".to_string(),
            created: Some("the description the first update drops".to_string()),
            updates: vec![None, Some("the description the second update stores".to_string())],
            tags: Some(json!({"team": "payments", "stage": "prod"})),
        },
        Row {
            label: "two updates that each omit the member on a wildcard domain",
            domain: "*.p3.example.com".to_string(),
            created: Some("not empty".to_string()),
            updates: vec![None, None],
            tags: None,
        },
        Row {
            label: "the modelled maximum description on a prefixed wildcard domain",
            domain: "*dev.p4.example.com".to_string(),
            created: Some(String::new()),
            updates: vec![Some(description(DESCRIPTION_MAX_LEN))],
            tags: Some(json!({"stage": "dev"})),
        },
        Row {
            label: "four updates on a domain of the modelled maximum length",
            domain: max_length_domain("p5.example.com"),
            created: None,
            updates: vec![
                Some(description(1)),
                Some(description(128)),
                None,
                Some(description(DESCRIPTION_MAX_LEN - 1)),
            ],
            tags: None,
        },
    ]
}

/// RULE 1: through any number of successive updates, every reader of the domain record —
/// `GetDomainName` and `ListDomainNames` — reports what the last accepted update stored, and
/// repeating an update answers the same configuration again.
#[tokio::test]
async fn every_reader_reports_what_the_last_update_stored() {
    let router = router();

    for row in rows() {
        let Row {
            label,
            domain,
            created,
            updates,
            tags,
        } = row;
        let created_config = create_domain(&router, &domain, created.as_deref(), tags).await;

        for (step, new_description) in updates.iter().enumerate() {
            let updated = update_domain(&router, &domain, new_description.as_deref()).await;
            assert_eq!(
                updated.status, 200,
                "case {label:?} step {step}: expected UpdateDomainName 200, got {updated:?}"
            );
            let stored = updated.body["domainNameConfig"].clone();
            let wanted = expected_description(new_description.as_deref());
            assert_eq!(
                stored["description"], wanted,
                "case {label:?} step {step}: expected the description the update carried, \
                 got {:?}",
                stored["description"]
            );

            // The members the input does not address keep the values the create stored.
            for member in UNADDRESSED_MEMBERS {
                assert_eq!(
                    stored[member], created_config[member],
                    "case {label:?} step {step}: expected {member} to survive the update as \
                     {:?}, got {:?}",
                    created_config[member], stored[member]
                );
            }

            // Both readers answer from the one stored record.
            let read = get_domain(&router, &domain).await;
            assert_eq!(
                read.status, 200,
                "case {label:?} step {step}: expected GetDomainName 200, got {read:?}"
            );
            assert_eq!(
                read.body["domainNameConfig"], stored,
                "case {label:?} step {step}: expected the read to report what the update \
                 stored, got {:?}",
                read.body["domainNameConfig"]
            );
            let listed = listed_config(&router, &domain).await;
            assert_eq!(
                listed, stored,
                "case {label:?} step {step}: expected the listing to report what the update \
                 stored, got {listed:?}"
            );

            // Repeating the SAME update is idempotent: the stored record, and so every
            // reader's answer, is unchanged.
            let repeated = update_domain(&router, &domain, new_description.as_deref()).await;
            assert_eq!(
                repeated.status, 200,
                "case {label:?} step {step}: expected the repeated update to answer 200, \
                 got {repeated:?}"
            );
            assert_eq!(
                repeated.body["domainNameConfig"], stored,
                "case {label:?} step {step}: expected the repeated update to store the same \
                 configuration, got {:?}",
                repeated.body["domainNameConfig"]
            );
        }
    }
}

/// RULE 2: an update addresses the ONE domain its path names, so every other domain this
/// account holds keeps the configuration it was created with.
#[tokio::test]
async fn an_update_leaves_the_other_domains_this_account_holds() {
    let router = router();

    let table = rows();
    // What each domain's record is expected to hold at this point in the sweep: the create's
    // answer until that domain's own update, the update's answer afterwards.
    let mut expected: Vec<(String, Value)> = Vec::with_capacity(table.len());
    for row in &table {
        let config = create_domain(
            &router,
            &row.domain,
            row.created.as_deref(),
            row.tags.clone(),
        )
        .await;
        expected.push((row.domain.clone(), config));
    }

    for (index, row) in table.iter().enumerate() {
        // Short enough for the modelled `@length` whatever the domain's own length is, and
        // distinct per row, so no update is a no-op write and no two rows store the same
        // value.
        let new_description = sweep_description(index);
        let updated = update_domain(&router, &row.domain, Some(&new_description)).await;
        assert_eq!(
            updated.status, 200,
            "case {:?}: expected UpdateDomainName 200, got {updated:?}",
            row.label
        );
        for (domain, config) in expected.iter_mut() {
            if domain == &row.domain {
                *config = updated.body["domainNameConfig"].clone();
            }
        }

        // Every domain the update did NOT name still reads as it did before the call.
        for (domain, config) in &expected {
            if domain == &row.domain {
                continue;
            }
            let read = get_domain(&router, domain).await;
            assert_eq!(
                read.body["domainNameConfig"], *config,
                "case {:?}: expected {domain} to be untouched by the update as {config:?}, \
                 got {:?}",
                row.label, read.body["domainNameConfig"]
            );
        }
    }

    // After the whole sweep, each domain holds its OWN last update — no row overwrote
    // another's record.
    for (index, row) in table.iter().enumerate() {
        let read = get_domain(&router, &row.domain).await;
        assert_eq!(
            read.body["domainNameConfig"]["description"],
            json!(sweep_description(index)),
            "case {:?}: expected the domain's own last update, got {:?}",
            row.label,
            read.body["domainNameConfig"]
        );
    }
}

/// RULE 3: state the update's input does not address survives it, including the API
/// association the domain carries — which is read through its own operation and so cannot be
/// covered by comparing `DomainNameConfig` bodies.
#[tokio::test]
async fn an_update_keeps_the_api_association_the_domain_carries() {
    let router = router();

    let domain = "associated.p6.example.com";
    create_domain(&router, domain, Some("the association's domain"), None).await;
    let api = create_api(&router, "Payments_domain", state::GRAPHQL_API_TYPE).await;

    let association_path = format!("{DOMAIN_NAMES_PATH}/{domain}/apiassociation");
    let associated = call(
        &router,
        "POST",
        &association_path,
        Some(json!({"apiId": api.id})),
    )
    .await;
    assert_eq!(
        associated.status, 200,
        "case \"the association is created\": expected AssociateApi 200, got {associated:?}"
    );

    // Each update carries a different description, so none of them is a no-op write.
    for (step, new_description) in [
        Some("the first update".to_string()),
        None,
        Some(description(DESCRIPTION_MAX_LEN)),
    ]
    .into_iter()
    .enumerate()
    {
        let updated = update_domain(&router, domain, new_description.as_deref()).await;
        assert_eq!(
            updated.status, 200,
            "case \"an update beside an association\" step {step}: expected 200, got {updated:?}"
        );

        let read = call(&router, "GET", &association_path, None).await;
        assert_eq!(
            read.status, 200,
            "case \"an update beside an association\" step {step}: expected GetApiAssociation \
             200 after the update, got {read:?}"
        );
        let association = &read.body["apiAssociation"];
        assert_eq!(
            association["apiId"],
            json!(api.id),
            "case \"an update beside an association\" step {step}: expected the associated \
             API to survive the update, got {association:?}"
        );
        assert_eq!(
            association["domainName"],
            json!(domain),
            "case \"an update beside an association\" step {step}: expected the association \
             to name its domain, got {association:?}"
        );
        // The status is the clock's to move, not the update's: it must still be one the
        // model declares.
        let status = association["associationStatus"].as_str().unwrap_or_default();
        assert!(
            ASSOCIATION_STATUSES.contains(&status),
            "case \"an update beside an association\" step {step}: expected a modelled \
             association status, got {association:?}"
        );
    }
}
