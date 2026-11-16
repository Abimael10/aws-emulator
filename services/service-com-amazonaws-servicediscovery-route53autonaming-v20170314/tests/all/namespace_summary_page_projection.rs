// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the `NamespaceSummary` projection `ListNamespaces` serves.
//!
//! The rule under test: a summary in a list page is the stored namespace record projected
//! onto the members the summary shape declares, so EVERY entry of EVERY page — whatever
//! filters selected it, whatever page size served it and however many entries that page
//! holds — carries the ownership member `ResourceOwner` (the account that holds the
//! resource, modelled `AWSAccountId` @length 12..12) and the structural member
//! `Properties`, whose `DnsProperties` names the namespace's hosted zone and its SOA,
//! reporting exactly what `GetNamespace` reports for the same record.
//!
//! `tests/all/namespace_properties.rs` pins that projection for an UNFILTERED page and
//! `tests/all/resource_ownership.rs` the account members of every read; the rows here
//! differ along the filter form (none, `TYPE` by equality and by prefix, `NAME`,
//! `HTTP_NAME`, `RESOURCE_OWNER` either way, two filters at once — the names, values and
//! conditions `NamespaceFilter` documents) and along the page size, because the
//! projection must not depend on how a page was selected or cut. It is the
//! `ListNamespaces` counterpart of `tests/all/service_summary_page_projection.rs`.
//!
//! Every request goes through `MultiServiceRouter::dispatch`, i.e. the same path the
//! emulator serves in production (`awsJson1_1`: `POST /` with `X-Amz-Target`), reusing
//! the one harness in `tests/all/cloud_map_core.rs`.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::cloud_map_core::{all_page_entries, emulator, entry_ids, ok};
use crate::namespace_properties::create_namespace;

/// A VPC id of the shape `CreatePrivateDnsNamespace$Vpc` takes (`ResourceId`, 0..64).
const VPC: &str = "vpc-0a1b2c3d4e5f60718";

/// The `NamespaceType` values, from `smithy_query(query="shape",
/// shape="NamespaceFilterName")`'s companion enum `NamespaceType`: a DNS namespace is
/// backed by a Route 53 hosted zone, an `HTTP` one is not.
const TYPE_HTTP: &str = "HTTP";
const TYPE_DNS_PUBLIC: &str = "DNS_PUBLIC";
const TYPE_DNS_PRIVATE: &str = "DNS_PRIVATE";

/// The prefix `BEGINS_WITH` selects both DNS types by.
const DNS_TYPE_PREFIX: &str = "DNS_";

/// The string a namespace read reports for `path`, which every namespace carries.
fn member(case: &str, read: &Value, path: &[&str]) -> String {
    let mut value = read;
    for step in path {
        value = &value[*step];
    }
    value
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: {} is reported, got {read}", path.join(".")))
        .to_string()
}

#[tokio::test]
async fn every_listed_summary_reports_the_ownership_and_properties_of_its_namespace() {
    let router = emulator();
    // `engine::EMULATOR_ACCOUNT_ID` is the account every emulated resource belongs to;
    // the test names the constant rather than a literal, as the ARNs do.
    let account = json!(engine::EMULATOR_ACCOUNT_ID);

    // One namespace of every modelled type, with two of one type so a filtered page
    // still holds several entries: an SOA TTL stated on one DNS namespace and left
    // unstated on the other, so the structural member differs from row to row.
    let first_http = create_namespace(
        &router,
        "CreateHttpNamespace",
        json!({ "Name": "nsproj-http-a" }),
    )
    .await;
    let second_http = create_namespace(
        &router,
        "CreateHttpNamespace",
        json!({ "Name": "nsproj-http-b", "Description": "the API-only namespace" }),
    )
    .await;
    let public_dns = create_namespace(
        &router,
        "CreatePublicDnsNamespace",
        json!({
            "Name": "nsproj-public.example.com",
            "Properties": { "DnsProperties": { "SOA": { "TTL": 60 } } },
        }),
    )
    .await;
    let private_dns = create_namespace(
        &router,
        "CreatePrivateDnsNamespace",
        json!({ "Name": "nsproj-private.example.com", "Vpc": VPC }),
    )
    .await;

    // A service in one namespace, so `ServiceCount` is derived rather than zero
    // everywhere and the summary has to agree with the per-namespace read on it too.
    ok(
        &router,
        "CreateService",
        json!({ "Name": "nsproj-svc", "NamespaceId": public_dns }),
    )
    .await;

    // (case, id, the type the namespace reports)
    let namespaces: Vec<(&str, &str, &str)> = vec![
        ("an HTTP namespace", first_http.as_str(), TYPE_HTTP),
        (
            "a second HTTP namespace, this one described",
            second_http.as_str(),
            TYPE_HTTP,
        ),
        (
            "a public DNS namespace with an SOA TTL of its own",
            public_dns.as_str(),
            TYPE_DNS_PUBLIC,
        ),
        (
            "a private DNS namespace given no properties",
            private_dns.as_str(),
            TYPE_DNS_PRIVATE,
        ),
    ];

    // What `GetNamespace` reports for each record: a summary must agree with it member
    // for member on every member `NamespaceSummary` declares.
    let mut expected: Vec<(String, String, &str, Value)> = Vec::new();
    for (case, namespace_id, namespace_type) in &namespaces {
        let read = ok(&router, "GetNamespace", json!({ "Id": namespace_id })).await;
        let namespace = read["Namespace"].clone();
        assert_eq!(
            namespace["Type"],
            json!(namespace_type),
            "case {case:?}: expected the namespace to report type {namespace_type}, got {namespace}"
        );
        expected.push((
            namespace_id.to_string(),
            case.to_string(),
            namespace_type,
            namespace,
        ));
    }

    // The row of `expected` a namespace id names, so the filter values below are read from
    // the record they belong to rather than from a position in the vector.
    fn row<'a>(expected: &'a [(String, String, &str, Value)], namespace_id: &str) -> &'a Value {
        &expected
            .iter()
            .find(|(id, _, _, _)| id == namespace_id)
            .unwrap_or_else(|| panic!("{namespace_id} is one of the created namespaces"))
            .3
    }

    let http_name_of_public = member(
        "a public DNS namespace",
        row(&expected, &public_dns),
        &["Properties", "HttpProperties", "HttpName"],
    );
    let name_of_first_http = member("an HTTP namespace", row(&expected, &first_http), &["Name"]);

    // The described namespace reports the description it was created with, so the
    // member-for-member comparison below covers a `Description` that is there as well as the
    // absent ones.
    let description = row(&expected, &second_http)["Description"].clone();
    assert!(
        description.is_string(),
        "a namespace created with a Description reports it, got {description:?}"
    );

    let every_namespace: BTreeSet<String> = namespaces
        .iter()
        .map(|(_, id, _)| (*id).to_string())
        .collect();
    let http_namespaces: BTreeSet<String> =
        BTreeSet::from([first_http.clone(), second_http.clone()]);
    let dns_namespaces: BTreeSet<String> = BTreeSet::from([public_dns.clone(), private_dns.clone()]);

    // (case, the Filters the request carries, the namespaces that request must list)
    let selections: Vec<(&str, Value, BTreeSet<String>)> = vec![
        ("no filter at all", Value::Null, every_namespace.clone()),
        (
            "TYPE EQ, which selects one namespace type",
            json!([{ "Name": "TYPE", "Values": [TYPE_HTTP], "Condition": "EQ" }]),
            http_namespaces.clone(),
        ),
        (
            "TYPE BEGINS_WITH, which selects both DNS types at once",
            json!([{ "Name": "TYPE", "Values": [DNS_TYPE_PREFIX], "Condition": "BEGINS_WITH" }]),
            dns_namespaces.clone(),
        ),
        (
            "NAME EQ, naming one namespace",
            json!([{ "Name": "NAME", "Values": [name_of_first_http.clone()] }]),
            BTreeSet::from([first_http.clone()]),
        ),
        (
            "NAME BEGINS_WITH, naming a prefix two namespaces share",
            json!([{ "Name": "NAME", "Values": ["nsproj-http"], "Condition": "BEGINS_WITH" }]),
            http_namespaces.clone(),
        ),
        (
            "HTTP_NAME EQ, the name a namespace is discovered under",
            json!([{ "Name": "HTTP_NAME", "Values": [http_name_of_public.clone()] }]),
            BTreeSet::from([public_dns.clone()]),
        ),
        (
            "RESOURCE_OWNER SELF, the owner of every namespace this emulator holds",
            json!([{ "Name": "RESOURCE_OWNER", "Values": ["SELF"], "Condition": "EQ" }]),
            every_namespace.clone(),
        ),
        (
            "RESOURCE_OWNER OTHER_ACCOUNTS, which no namespace here was shared in from",
            json!([{ "Name": "RESOURCE_OWNER", "Values": ["OTHER_ACCOUNTS"] }]),
            BTreeSet::new(),
        ),
        (
            "two filters at once, which a namespace must match both of",
            json!([{ "Name": "TYPE", "Values": [TYPE_HTTP] },
                   { "Name": "RESOURCE_OWNER", "Values": ["SELF"] }]),
            http_namespaces.clone(),
        ),
    ];

    for (selection, filters, listed_namespaces) in selections {
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
                all_page_entries(&router, &case, "ListNamespaces", "Namespaces", body).await;

            assert_eq!(
                entry_ids(&case, &summaries),
                listed_namespaces,
                "case {case:?}: expected the listing to serve {listed_namespaces:?}, got \
                 {summaries:?}"
            );

            for summary in &summaries {
                let namespace_id = summary["Id"].as_str().unwrap_or_default().to_string();
                let (_, row, namespace_type, namespace) = expected
                    .iter()
                    .find(|(id, _, _, _)| *id == namespace_id)
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: {namespace_id} is one of the created namespaces")
                    });
                let case = format!("{case}, {row}");

                let owner = summary["ResourceOwner"].clone();
                assert_eq!(
                    owner, account,
                    "case {case:?}: expected NamespaceSummary.ResourceOwner of {namespace_id} to \
                     be the owning account {account}, got {owner:?} in {summary}"
                );

                let dns = summary["Properties"]["DnsProperties"].clone();
                assert!(
                    dns.is_object(),
                    "case {case:?}: expected NamespaceSummary.Properties.DnsProperties of \
                     {namespace_id} to be a structure, got {dns:?} in {summary}"
                );
                // `SOA$TTL` is modelled @required, so the SOA a namespace reports always
                // carries a TTL for purposes of negative caching.
                assert!(
                    dns["SOA"]["TTL"].is_i64(),
                    "case {case:?}: expected the SOA of {namespace_id} to carry a TTL, got \
                     {dns:?}"
                );
                // Only a DNS namespace is backed by a Route 53 hosted zone; an HTTP one
                // reports the structure without naming a zone. The expectation comes from the
                // type the row CREATED, not from the type the answer reports.
                let has_zone = *namespace_type != TYPE_HTTP;
                assert_eq!(
                    dns["HostedZoneId"].is_string(),
                    has_zone,
                    "case {case:?}: expected a hosted zone reported for {namespace_id}: \
                     {has_zone}, got {dns:?}"
                );

                for name in [
                    "Arn",
                    "ResourceOwner",
                    "Name",
                    "Type",
                    "Description",
                    "ServiceCount",
                    "Properties",
                    "CreateDate",
                ] {
                    assert_eq!(
                        summary[name], namespace[name],
                        "case {case:?}: expected NamespaceSummary.{name} of {namespace_id} to be \
                         what GetNamespace reports ({}), got {:?}",
                        namespace[name], summary[name]
                    );
                }
            }
        }
    }
}
