// SPDX-License-Identifier: CC0-1.0

//! What a `DeleteDomainName` leaves behind, end to end over HTTP.
//!
//! The rule asserted here: a delete removes the WHOLE custom-domain record — the API
//! association held on it goes with it — and releases the name, so after the delete every
//! operation addressed to that domain reports it absent, the name can be created again as a
//! NEW domain (its own endpoint, no association, only the configuration the new create
//! carried), and the slot the deleted domain occupied in the account's domain quota is free
//! again. The API the domain was mapped onto is a resource of its own and survives.
//!
//! The rows span the FORM of the domain name (the shapes the modelled `DomainName` pattern
//! admits, up to its modelled maximum length), whether the domain carried an API association
//! when it was deleted, and whether it carried a description and tags — so the rule cannot be
//! keyed to one kind of domain or to one history.
//!
//! The harness is shared rather than copied: `call`, `create_api`, `router` and `Answer`
//! come from the source-API association suite, the domain-name path, the maximum-length name
//! builder and the delete call from the update/delete suite, and the account quota from the
//! association suite that first pinned it. Only `create_domain` is this suite's own, because
//! it must answer a REFUSED create as well as an accepted one.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::state;
use smithy_server_runtime::MultiServiceRouter;

use crate::custom_domain_api_association::DOMAIN_NAMES_PER_REGION;
use crate::domain_name_update_delete::{
    delete_domain, max_length_domain, DOMAIN_NAMES_PATH,
};
use crate::source_api_association::{call, create_api, router, Answer};

/// How long a poll for a transition the clock owes may take before the test gives up —
/// generous, so a loaded host still sees the condition. Nothing is asserted about elapsed
/// time: only that the condition holds before the deadline.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long one poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// `GetDomainName`'s path for `domain`.
fn domain_path(domain: &str) -> String {
    format!("{DOMAIN_NAMES_PATH}/{domain}")
}

/// The path the three API-association operations share, for `domain`.
fn association_path(domain: &str) -> String {
    format!("{DOMAIN_NAMES_PATH}/{domain}/apiassociation")
}

/// The `message` an answer carries, or `""` when it carries none.
fn message(answer: &Answer) -> &str {
    answer.body["message"].as_str().unwrap_or_default()
}

/// Assert `answer` is the modelled `NotFoundException` (404) carrying `expected`.
fn assert_not_found(case: &str, answer: &Answer, expected: &str) {
    assert_eq!(
        (answer.status, answer.error_type.as_str()),
        (404, "NotFoundException"),
        "case {case}: expected 404 NotFoundException, got {answer:?}"
    );
    assert_eq!(
        message(answer),
        expected,
        "case {case}: expected the message {expected:?}, got {answer:?}"
    );
}

/// `CreateDomainName` for `domain`, carrying `description` and `tags` when the row has them.
async fn create_domain(
    router: &MultiServiceRouter,
    domain: &str,
    description: Option<&str>,
    tags: Option<Value>,
) -> Answer {
    let mut body = json!({
        "domainName": domain,
        "certificateArn": format!(
            "arn:aws:acm:us-east-1:000000000000:certificate/{}",
            domain.len()
        ),
    });
    if let Some(description) = description {
        body["description"] = json!(description);
    }
    if let Some(tags) = tags {
        body["tags"] = tags;
    }
    call(router, "POST", DOMAIN_NAMES_PATH, Some(body)).await
}

/// `GET /v1/domainnames/{domainName}/apiassociation`.
async fn get_association(router: &MultiServiceRouter, domain: &str) -> Answer {
    call(router, "GET", &association_path(domain), None).await
}

/// Map `api_id` onto `domain` and wait for the mapping to reach the terminal status the
/// clock owes it, then answer the `ApiAssociation` the read reports.
///
/// Polls for the CONDITION rather than waiting a fixed time: the handler stores the instant
/// the deployment is due and the read materialises it, so the loop looks again until the
/// status is terminal or [`POLL_DEADLINE`] passes.
async fn associate_and_settle(
    router: &MultiServiceRouter,
    domain: &str,
    api_id: &str,
    case: &str,
) -> Value {
    let associated = call(
        router,
        "POST",
        &association_path(domain),
        Some(json!({"apiId": api_id})),
    )
    .await;
    assert_eq!(
        associated.status, 200,
        "case {case}: expected AssociateApi 200, got {associated:?}"
    );

    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let read = get_association(router, domain).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected GetApiAssociation 200 while the deployment is owed, got \
             {read:?}"
        );
        if read.body["apiAssociation"]["associationStatus"] == json!(state::ASSOCIATION_SUCCESS) {
            return read.body["apiAssociation"].clone();
        }
        assert!(
            Instant::now() < deadline,
            "case {case}: expected the mapping to reach {:?}, got {read:?}",
            state::ASSOCIATION_SUCCESS
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The names `ListDomainNames` reports.
async fn listed_domains(router: &MultiServiceRouter) -> Vec<String> {
    let listed = call(router, "GET", DOMAIN_NAMES_PATH, None).await;
    assert_eq!(
        listed.status, 200,
        "case the listing: expected ListDomainNames 200, got {listed:?}"
    );
    listed.body["domainNameConfigs"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry["domainName"].as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// One row of the delete table.
struct Row {
    label: &'static str,
    domain: String,
    description: Option<String>,
    tags: Option<Value>,
    /// Whether the domain is mapped onto an API before it is deleted.
    associated: bool,
}

/// The delete table: the rows differ along the form of the domain name (label count, digits
/// and hyphens, both wildcard forms the modelled pattern admits, the modelled maximum
/// length), along whether the domain carries an API association when the delete arrives, and
/// along the description and tags it holds.
fn rows() -> Vec<Row> {
    vec![
        Row {
            label: "a two-label domain carrying an association",
            domain: "d0.example".to_string(),
            description: Some("the orders domain".to_string()),
            tags: Some(json!({"team": "orders"})),
            associated: true,
        },
        Row {
            label: "a three-label domain carrying no association",
            domain: "api.d1.example.com".to_string(),
            description: None,
            tags: None,
            associated: false,
        },
        Row {
            label: "a hyphenated, digit-bearing domain carrying an association",
            domain: "api-7.d2-x.example.com".to_string(),
            description: Some(String::new()),
            tags: Some(json!({"team": "checkout", "tier": "edge"})),
            associated: true,
        },
        Row {
            label: "a wildcard domain carrying no association",
            domain: "*.d3.example.com".to_string(),
            description: Some("every subdomain".to_string()),
            tags: None,
            associated: false,
        },
        Row {
            label: "a prefixed wildcard domain carrying an association",
            domain: "*dev.d4.example.com".to_string(),
            description: None,
            tags: Some(json!({"stage": "dev"})),
            associated: true,
        },
        Row {
            label: "a domain of the modelled maximum length carrying no association",
            domain: max_length_domain("d5.example.com"),
            description: Some("the longest domain".to_string()),
            tags: None,
            associated: false,
        },
    ]
}

/// RULE, first part: the delete removes the whole record — the association held on it
/// included — so every operation addressed to the domain afterwards reports it absent, in
/// that operation's own wording, while the API the domain was mapped onto survives.
#[tokio::test]
async fn a_delete_takes_the_api_association_with_the_domain() {
    let router = router();

    for (index, row) in rows().iter().enumerate() {
        let case = row.label;
        let created = create_domain(
            &router,
            &row.domain,
            row.description.as_deref(),
            row.tags.clone(),
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected CreateDomainName 200, got {created:?}"
        );

        let api = create_api(
            &router,
            &format!("Domain_release_{index}"),
            state::GRAPHQL_API_TYPE,
        )
        .await;
        if row.associated {
            let association = associate_and_settle(&router, &row.domain, &api.id, case).await;
            assert_eq!(
                association["apiId"], json!(api.id),
                "case {case:?}: expected the mapping to name the API it was given, got \
                 {association:?}"
            );
        } else {
            // A domain that was never mapped reports the absent ASSOCIATION, which is a
            // different condition from the absent domain the delete produces below.
            let before = get_association(&router, &row.domain).await;
            assert_not_found(
                &format!("{case:?}: GetApiAssociation before any AssociateApi"),
                &before,
                state::API_ASSOCIATION_NOT_FOUND_MESSAGE,
            );
        }

        let removed = delete_domain(&router, &row.domain).await;
        assert_eq!(
            removed.status, 200,
            "case {case:?}: expected DeleteDomainName 200, got {removed:?}"
        );

        // Every operation addressed to the deleted domain reports it absent, in its own
        // wording: the association that was held ON the domain is gone with it, so the read
        // of it is the absent-DOMAIN refusal and not the absent-association one.
        for (operation, expected, answer) in [
            (
                "GetDomainName",
                state::DOMAIN_NAME_NOT_FOUND_READ_MESSAGE,
                call(&router, "GET", &domain_path(&row.domain), None).await,
            ),
            (
                "UpdateDomainName",
                state::DOMAIN_NAME_NOT_FOUND_READ_MESSAGE,
                call(
                    &router,
                    "POST",
                    &domain_path(&row.domain),
                    Some(json!({"description": "after the delete"})),
                )
                .await,
            ),
            (
                "GetApiAssociation",
                state::DOMAIN_NAME_NOT_FOUND_READ_MESSAGE,
                get_association(&router, &row.domain).await,
            ),
            (
                "AssociateApi",
                state::DOMAIN_NAME_NOT_FOUND_MESSAGE,
                call(
                    &router,
                    "POST",
                    &association_path(&row.domain),
                    Some(json!({"apiId": api.id})),
                )
                .await,
            ),
            (
                "DisassociateApi",
                state::DOMAIN_NAME_NOT_FOUND_MESSAGE,
                call(&router, "DELETE", &association_path(&row.domain), None).await,
            ),
            (
                "DeleteDomainName",
                state::DOMAIN_NAME_NOT_FOUND_MESSAGE,
                delete_domain(&router, &row.domain).await,
            ),
        ] {
            assert_not_found(
                &format!("{case:?}: {operation} after the delete"),
                &answer,
                expected,
            );
        }

        // The listing no longer carries it either.
        let names = listed_domains(&router).await;
        assert!(
            !names.contains(&row.domain),
            "case {case:?}: expected the listing not to carry the deleted domain, got {names:?}"
        );

        // The API is a resource of its own: deleting the domain it was mapped onto leaves it
        // where it was.
        let api_read = call(&router, "GET", &format!("/v1/apis/{}", api.id), None).await;
        assert_eq!(
            api_read.status, 200,
            "case {case:?}: expected the associated API to survive the delete, got {api_read:?}"
        );
    }
}

/// RULE, second part: the delete releases the NAME, so a create under it is a new domain —
/// its own endpoint, no association, and only the configuration the new create carried.
#[tokio::test]
async fn a_deleted_name_is_created_again_as_a_new_domain() {
    let router = router();

    for (index, row) in rows().iter().enumerate() {
        let case = row.label;
        let created = create_domain(
            &router,
            &row.domain,
            row.description.as_deref(),
            row.tags.clone(),
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected CreateDomainName 200, got {created:?}"
        );
        let first = created.body["domainNameConfig"].clone();

        if row.associated {
            let api = create_api(
                &router,
                &format!("Domain_recreate_{index}"),
                state::GRAPHQL_API_TYPE,
            )
            .await;
            associate_and_settle(&router, &row.domain, &api.id, case).await;
        }

        let removed = delete_domain(&router, &row.domain).await;
        assert_eq!(
            removed.status, 200,
            "case {case:?}: expected DeleteDomainName 200, got {removed:?}"
        );

        // The second create is accepted — the name is free — and carries a configuration of
        // its own, not the deleted domain's.
        let again = create_domain(&router, &row.domain, Some("the second domain"), None).await;
        assert_eq!(
            again.status, 200,
            "case {case:?}: expected the name to be free after the delete, got {again:?}"
        );
        let second = again.body["domainNameConfig"].clone();
        assert_eq!(
            second["description"],
            json!("the second domain"),
            "case {case:?}: expected the new create's description, got {second:?}"
        );
        // The tag map is REPORTED and empty: the recreated domain carries only what the
        // second create asked for, so the deleted domain's tags are not inherited.
        assert_eq!(
            second["tags"].as_object().map(serde_json::Map::len),
            Some(0),
            "case {case:?}: expected the recreated domain to carry only the new create's tags, \
             got {second:?}"
        );
        let first_endpoint = first["appsyncDomainName"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the first create reports an endpoint"));
        let second_endpoint = second["appsyncDomainName"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the second create reports an endpoint"));
        assert_ne!(
            second_endpoint, first_endpoint,
            "case {case:?}: expected the recreated domain to be served through an endpoint of \
             its own, got {second:?}"
        );
        // The ARN names the domain, so it is the one value the two creates share.
        let first_arn = first["domainNameArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the first create reports an ARN"));
        let second_arn = second["domainNameArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the second create reports an ARN"));
        assert!(
            second_arn == first_arn && second_arn.ends_with(&state::domain_name_resource(&row.domain)),
            "case {case:?}: expected the ARN to name the domain, got {second_arn:?} against \
             {first_arn:?}"
        );

        // The read reports the new domain, and the recreated domain carries NO association —
        // whatever the deleted one carried.
        let read = call(&router, "GET", &domain_path(&row.domain), None).await;
        assert_eq!(
            (read.status, read.body["domainNameConfig"].clone()),
            (200, second),
            "case {case:?}: expected the read to report what the second create stored, got \
             {read:?}"
        );
        let association = get_association(&router, &row.domain).await;
        assert_not_found(
            &format!("{case:?}: GetApiAssociation on the recreated domain"),
            &association,
            state::API_ASSOCIATION_NOT_FOUND_MESSAGE,
        );
    }
}

/// RULE, third part: the delete frees the slot the domain occupied in the account's quota,
/// so a create refused at the cap is accepted once a domain has been deleted.
#[tokio::test]
async fn a_delete_releases_the_account_quota_slot() {
    let router = router();

    for index in 0..DOMAIN_NAMES_PER_REGION {
        let domain = format!("slot-{index}.example.com");
        let answer = create_domain(&router, &domain, None, None).await;
        assert_eq!(
            answer.status, 200,
            "case \"domain {index} inside the quota\": expected CreateDomainName 200, got \
             {answer:?}"
        );
    }

    // At the cap the create is refused, and the refusal names the cap.
    let past = "slot-past.example.com";
    let refused = create_domain(&router, past, None, None).await;
    assert_eq!(
        (refused.status, refused.error_type.as_str()),
        (400, "BadRequestException"),
        "case \"one past the quota\": expected 400 BadRequestException, got {refused:?}"
    );
    assert!(
        message(&refused).contains(&DOMAIN_NAMES_PER_REGION.to_string()),
        "case \"one past the quota\": expected the refusal to state the quota \
         {DOMAIN_NAMES_PER_REGION}, got {refused:?}"
    );

    // Deleting one domain frees exactly one slot: the create that was refused is accepted,
    // and the next one is refused again.
    let removed = delete_domain(&router, "slot-0.example.com").await;
    assert_eq!(
        removed.status, 200,
        "case \"the freed slot\": expected DeleteDomainName 200, got {removed:?}"
    );
    let accepted = create_domain(&router, past, None, None).await;
    assert_eq!(
        accepted.status, 200,
        "case \"the freed slot\": expected the create to be accepted, got {accepted:?}"
    );
    let read = call(&router, "GET", &domain_path(past), None).await;
    assert_eq!(
        read.status, 200,
        "case \"the freed slot\": expected the new domain to read back, got {read:?}"
    );
    let refused_again = create_domain(&router, "slot-past-2.example.com", None, None).await;
    assert_eq!(
        (refused_again.status, refused_again.error_type.as_str()),
        (400, "BadRequestException"),
        "case \"the quota after the freed slot was taken\": expected 400 BadRequestException, \
         got {refused_again:?}"
    );

    // The deleted domain is gone, and the one the refusals named was never stored.
    let names = listed_domains(&router).await;
    assert!(
        !names.contains(&"slot-0.example.com".to_string())
            && !names.contains(&"slot-past-2.example.com".to_string()),
        "case \"the listing after the deletes\": expected neither the deleted nor the refused \
         domain, got {names:?}"
    );
}
