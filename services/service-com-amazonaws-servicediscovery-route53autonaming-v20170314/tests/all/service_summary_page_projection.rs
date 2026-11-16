// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the `ServiceSummary` projection `ListServices` serves.
//!
//! The rule under test: a `ServiceSummary` is the whole stored service record projected
//! onto the members the shape declares, so EVERY entry of EVERY page — whatever filters
//! selected it, whatever page size served it and however many entries that page holds —
//! carries the ownership members `ResourceOwner` and `CreatedByAccount` (the account
//! that holds the resource, modelled `AWSAccountId` @length 12..12) and the structural
//! member `DnsConfig`, reporting exactly what `GetService` reports for the same record,
//! with `DnsConfig` present and naming no record for a service that registers none.
//!
//! `tests/all/resource_ownership.rs` pins that rule for UNFILTERED pages; the rows here
//! differ along the filter form (none, `NAMESPACE_ID` by id, `NAMESPACE_ID` by ARN,
//! `RESOURCE_OWNER` either way, two filters at once — the names and values
//! `ServiceFilter` documents) and the page size, because the projection must not depend
//! on how a page was selected or cut.
//!
//! Every request goes through `MultiServiceRouter::dispatch`, the same path the emulator
//! serves in production, reusing the harness in `cloud_map_core.rs`.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::cloud_map_core::{all_page_entries, dns_namespace, emulator, entry_ids, namespace, ok};
use super::service_listing::namespace_arn;

/// Create a service, optionally with a `DnsConfig`, and return its id.
async fn service_with(
    router: &MultiServiceRouter,
    namespace_id: &str,
    name: &str,
    dns_config: &Value,
) -> String {
    let mut body = json!({ "Name": name, "NamespaceId": namespace_id });
    if !dns_config.is_null() {
        body["DnsConfig"] = dns_config.clone();
    }
    let created = ok(router, "CreateService", body).await;
    created["Service"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateService returns the service it stored, got {created}"))
        .to_string()
}

#[tokio::test]
async fn every_listed_summary_reports_the_ownership_and_record_members_of_its_service() {
    let router = emulator();
    // `engine::EMULATOR_ACCOUNT_ID` is the account every emulated resource belongs to;
    // the test names the constant rather than a literal, as the ARNs do.
    let account = json!(engine::EMULATOR_ACCOUNT_ID);
    assert_eq!(
        engine::EMULATOR_ACCOUNT_ID.len(),
        12,
        "AWSAccountId is modelled @length(min 12, max 12), got {}",
        engine::EMULATOR_ACCOUNT_ID
    );

    let (_, http_namespace) = namespace(&router, "projection-http-ns").await;
    let dns_namespace_id = dns_namespace(&router, "projection-dns-ns").await;

    // Three services in one namespace and two in another, so every page below holds more
    // than one entry and a filtered page still holds several.
    let mut http_services = BTreeSet::new();
    for index in 0..3 {
        http_services.insert(
            service_with(
                &router,
                &http_namespace,
                &format!("projection-http-svc-{index}"),
                &Value::Null,
            )
            .await,
        );
    }
    let mut dns_services = BTreeSet::new();
    // One service that registers records and one that registers none, so the row that
    // reports an empty `DnsConfig` and the row that reports a populated one are both in
    // every page.
    for (index, dns_config) in [
        Value::Null,
        json!({ "DnsRecords": [{ "Type": "A", "TTL": 60 }, { "Type": "AAAA", "TTL": 300 }] }),
    ]
    .into_iter()
    .enumerate()
    {
        dns_services.insert(
            service_with(
                &router,
                &dns_namespace_id,
                &format!("projection-dns-svc-{index}"),
                &dns_config,
            )
            .await,
        );
    }
    let every_service: BTreeSet<String> = http_services.union(&dns_services).cloned().collect();

    // What `GetService` reports for each record: the summary must agree with it member
    // for member on the three members under test.
    let mut expected: Vec<(String, Value)> = Vec::new();
    for service_id in &every_service {
        let read = ok(&router, "GetService", json!({ "Id": service_id })).await;
        expected.push((service_id.clone(), read["Service"].clone()));
    }

    let http_arn = namespace_arn(&router, &http_namespace).await;

    // (case, the Filters the request carries, the services that request must list)
    let selections: Vec<(&str, Value, BTreeSet<String>)> = vec![
        ("no filter at all", Value::Null, every_service.clone()),
        (
            "NAMESPACE_ID naming a namespace by id",
            json!([{ "Name": "NAMESPACE_ID", "Values": [dns_namespace_id.clone()],
                     "Condition": "EQ" }]),
            dns_services.clone(),
        ),
        (
            "NAMESPACE_ID naming a namespace by ARN",
            json!([{ "Name": "NAMESPACE_ID", "Values": [http_arn.clone()] }]),
            http_services.clone(),
        ),
        (
            "RESOURCE_OWNER SELF, the owner of every namespace this emulator holds",
            json!([{ "Name": "RESOURCE_OWNER", "Values": ["SELF"], "Condition": "EQ" }]),
            every_service.clone(),
        ),
        (
            "RESOURCE_OWNER OTHER_ACCOUNTS, which no namespace here was shared in from",
            json!([{ "Name": "RESOURCE_OWNER", "Values": ["OTHER_ACCOUNTS"] }]),
            BTreeSet::new(),
        ),
        (
            "two filters at once, which a service must match both of",
            json!([{ "Name": "NAMESPACE_ID", "Values": [http_namespace.clone()] },
                   { "Name": "RESOURCE_OWNER", "Values": ["SELF"] }]),
            http_services.clone(),
        ),
    ];

    for (selection, filters, listed_services) in selections {
        // The page sizes: one entry at a time, two at a time, and the documented default
        // of up to 100 — so the same projection is checked on a cut page and a whole one.
        for max_results in [Some(1), Some(2), None] {
            let case = format!("{selection}, MaxResults {max_results:?}");
            let mut body = json!({});
            if !filters.is_null() {
                body["Filters"] = filters.clone();
            }
            if let Some(max) = max_results {
                body["MaxResults"] = json!(max);
            }
            let summaries =
                all_page_entries(&router, &case, "ListServices", "Services", body).await;

            assert_eq!(
                entry_ids(&case, &summaries),
                listed_services,
                "case {case:?}: expected the listing to serve {listed_services:?}, got \
                 {summaries:?}"
            );

            for summary in &summaries {
                let service_id = summary["Id"].as_str().unwrap_or_default().to_string();
                let (_, service) = expected
                    .iter()
                    .find(|(id, _)| *id == service_id)
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: {service_id} is one of the created services")
                    });

                for member in ["ResourceOwner", "CreatedByAccount"] {
                    let got = summary[member].clone();
                    assert_eq!(
                        got, account,
                        "case {case:?}: expected ServiceSummary.{member} of {service_id} to be \
                         the owning account {account}, got {got:?} in {summary}"
                    );
                }
                let dns_config = summary["DnsConfig"].clone();
                assert!(
                    dns_config.is_object(),
                    "case {case:?}: expected ServiceSummary.DnsConfig of {service_id} to be a \
                     structure, got {dns_config:?} in {summary}"
                );
                for member in ["ResourceOwner", "CreatedByAccount", "DnsConfig", "Arn"] {
                    assert_eq!(
                        summary[member], service[member],
                        "case {case:?}: expected ServiceSummary.{member} of {service_id} to be \
                         what GetService reports ({}), got {:?}",
                        service[member], summary[member]
                    );
                }
            }
        }
    }
}
