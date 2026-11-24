// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what an entry of a paginated Cloud Map listing reports.
//!
//! The rule under test: a listing entry is the stored record projected onto the members
//! its summary shape declares, so EVERY entry of EVERY page carries each of those
//! members the record has — the ownership members `ResourceOwner` / `CreatedByAccount`
//! and the structural member `DnsConfig` among them — with exactly the value the
//! per-resource read reports for the same record, however many entries the page holds
//! and whatever page size cut it.
//!
//! The rows span the listings this service paginates (`ListNamespaces`, `ListServices`,
//! `ListInstances`, `ListOperations`), the optional configuration a record may carry
//! (none, a description, a custom health check, DNS records under either routing policy,
//! a Route 53 health check) and the page size (one entry at a time, two at a time, and
//! the documented default), because the projection must not depend on which listing
//! served the record or on how the page was cut.
//!
//! `tests/all/service_summary_page_projection.rs` pins the same rule for `ListServices`
//! across the filter forms; this suite widens it to every listing, so a summary that
//! drops a member its shape declares is caught wherever it is projected.
//!
//! Every request goes through `MultiServiceRouter::dispatch`, i.e. the same path the
//! emulator serves in production (`awsJson1_1`: `POST /` with `X-Amz-Target`), reusing
//! the one harness in `tests/all/cloud_map_core.rs`.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloud_map_core::{all_page_entries, dns_namespace, emulator, entry_ids, namespace, ok};

/// The `NamespaceSummary` members every namespace has, whatever its type: the shape's
/// members minus `Description`, which only a namespace created with one carries
/// (`smithy_query(query="shape",shape="NamespaceSummary")`).
const NAMESPACE_SUMMARY_MEMBERS: &[&str] = &[
    "Id",
    "Arn",
    "ResourceOwner",
    "Name",
    "Type",
    "ServiceCount",
    "Properties",
    "CreateDate",
];

/// The `ServiceSummary` members every service has, whatever it was created with: the
/// shape's members minus `Description`, `HealthCheckConfig` and
/// `HealthCheckCustomConfig`, which only a service created with them carries
/// (`smithy_query(query="shape",shape="ServiceSummary")`).
const SERVICE_SUMMARY_MEMBERS: &[&str] = &[
    "Id",
    "Arn",
    "ResourceOwner",
    "Name",
    "Type",
    "InstanceCount",
    "DnsConfig",
    "CreateDate",
    "CreatedByAccount",
];

/// Every `InstanceSummary` member (`smithy_query(query="shape",shape="InstanceSummary")`).
const INSTANCE_SUMMARY_MEMBERS: &[&str] = &["Id", "Attributes", "CreatedByAccount"];

/// Every `OperationSummary` member (`smithy_query(query="shape",shape="OperationSummary")`).
const OPERATION_SUMMARY_MEMBERS: &[&str] = &["Id", "Status"];

/// The `OperationStatus` enumeration, which `OperationSummary$Status` targets:
/// "SUBMITTED ... PENDING ... SUCCESS ... FAIL"
/// (`smithy_query(query="shape",shape="OperationStatus")`).
const OPERATION_STATUS_VALUES: &[&str] = &["SUBMITTED", "PENDING", "SUCCESS", "FAIL"];

/// The page sizes every listing below is served at: one entry at a time and two at a
/// time, which cut the listing into several pages, and the documented default, which
/// serves every record in one page.
const PAGE_SIZES: [Option<i32>; 3] = [Some(1), Some(2), None];

/// One listing this suite drives, with the records its entries must project.
struct Listing<'a> {
    /// What the row is, for the assertion messages.
    case: &'a str,
    /// The paginated operation that serves the listing.
    operation: &'a str,
    /// The output member holding one page of it.
    member: &'a str,
    /// The request body every page of it is asked for with, minus `MaxResults`.
    body: Value,
    /// The summary members every one of these records carries.
    declared: &'a [&'a str],
    /// What the per-resource read reports for each record, keyed by its id.
    records: BTreeMap<String, Value>,
    /// The members of the summary that name an account.
    account_members: &'a [&'a str],
}

/// Assert that one listing entry is the stored record projected onto its summary shape:
/// every member the shape declares for a record like this one is present, and every
/// member the entry carries holds exactly what the per-resource read reports for it.
fn assert_projects_record(case: &str, entry: &Value, record: &Value, declared: &[&str]) {
    for member in declared {
        assert!(
            !entry[member].is_null(),
            "case {case:?}: expected the summary to carry {member}, got {entry}"
        );
    }
    let members = entry
        .as_object()
        .unwrap_or_else(|| panic!("case {case:?}: a listing entry is a structure, got {entry}"));
    for (member, got) in members {
        assert_eq!(
            got, &record[member],
            "case {case:?}: expected the summary's {member} to be what the per-resource read \
             reports ({}), got {got:?}",
            record[member]
        );
    }
}

/// Create a service and return its id, passing on whatever optional configuration the
/// row carries.
async fn service_with(
    router: &MultiServiceRouter,
    namespace_id: &str,
    name: &str,
    configuration: &Value,
) -> String {
    let mut body = json!({ "Name": name, "NamespaceId": namespace_id });
    for (member, value) in configuration
        .as_object()
        .map(|members| members.iter().collect::<Vec<_>>())
        .unwrap_or_default()
    {
        body[member] = value.clone();
    }
    let created = ok(router, "CreateService", body).await;
    created["Service"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateService returns the service it stored, got {created}"))
        .to_string()
}

#[tokio::test]
async fn every_listed_entry_projects_the_record_its_per_resource_read_reports() {
    let router = emulator();
    // `engine::EMULATOR_ACCOUNT_ID` is the account every emulated resource belongs to;
    // the test names the constant rather than a literal, as the ARNs do.
    let account = json!(engine::EMULATOR_ACCOUNT_ID);

    let (_, http_namespace) = namespace(&router, "listing-projection-http").await;
    let dns_namespace_id = dns_namespace(&router, "listing-projection-dns").await;

    // The services, one row per optional configuration a service may carry, spread over
    // the two namespaces so no page holds a single entry.
    let services: Vec<(&str, &str, Value)> = vec![
        (
            "a service with no optional configuration at all",
            http_namespace.as_str(),
            json!({}),
        ),
        (
            "a service carrying a description and a custom health check",
            http_namespace.as_str(),
            json!({
                "Description": "a service with a third-party health checker",
                "HealthCheckCustomConfig": { "FailureThreshold": 1 },
            }),
        ),
        (
            "a service registering an A record under a Route 53 health check",
            dns_namespace_id.as_str(),
            json!({
                "DnsConfig": { "DnsRecords": [{ "Type": "A", "TTL": 60 }] },
                "HealthCheckConfig": { "Type": "HTTP", "ResourcePath": "/health",
                                       "FailureThreshold": 2 },
            }),
        ),
        (
            "a service registering an SRV record under a weighted routing policy",
            dns_namespace_id.as_str(),
            json!({
                "DnsConfig": { "RoutingPolicy": "WEIGHTED",
                               "DnsRecords": [{ "Type": "SRV", "TTL": 10 }] },
            }),
        ),
    ];
    let mut service_ids = Vec::new();
    for (index, (_, namespace_id, configuration)) in services.iter().enumerate() {
        service_ids.push(
            service_with(
                &router,
                namespace_id,
                &format!("listing-projection-svc-{index}"),
                configuration,
            )
            .await,
        );
    }
    let instance_service = service_ids[0].clone();

    // Three instances on one service, so the instance listing is served over several
    // pages too, each carrying a different attribute set.
    let instances: Vec<(&str, Value)> = vec![
        ("an IPv4 instance", json!({ "AWS_INSTANCE_IPV4": "10.0.0.4" })),
        (
            "an IPv6 instance",
            json!({ "AWS_INSTANCE_IPV6": "2001:db8::4" }),
        ),
        (
            "an instance carrying an IPv4 address, a port and a custom attribute",
            json!({ "AWS_INSTANCE_IPV4": "10.0.0.5", "AWS_INSTANCE_PORT": "8080",
                    "stage": "test" }),
        ),
    ];
    let mut instance_ids = Vec::new();
    for (index, (_, attributes)) in instances.iter().enumerate() {
        let instance_id = format!("i-listing-projection-{index}");
        ok(
            &router,
            "RegisterInstance",
            json!({
                "ServiceId": instance_service,
                "InstanceId": instance_id,
                "Attributes": attributes,
            }),
        )
        .await;
        instance_ids.push(instance_id);
    }

    // What each per-resource read reports, gathered AFTER every create, so the records a
    // listing is compared against are the records as they now stand.
    let mut namespace_records: BTreeMap<String, Value> = BTreeMap::new();
    for namespace_id in [http_namespace.as_str(), dns_namespace_id.as_str()] {
        let read = ok(&router, "GetNamespace", json!({ "Id": namespace_id })).await;
        namespace_records.insert(namespace_id.to_string(), read["Namespace"].clone());
    }
    let mut service_records: BTreeMap<String, Value> = BTreeMap::new();
    for service_id in &service_ids {
        let read = ok(&router, "GetService", json!({ "Id": service_id })).await;
        service_records.insert(service_id.clone(), read["Service"].clone());
    }
    let mut instance_records: BTreeMap<String, Value> = BTreeMap::new();
    for instance_id in &instance_ids {
        let read = ok(
            &router,
            "GetInstance",
            json!({ "ServiceId": instance_service, "InstanceId": instance_id }),
        )
        .await;
        instance_records.insert(instance_id.clone(), read["Instance"].clone());
    }

    let listings = vec![
        Listing {
            case: "the namespaces this emulator holds",
            operation: "ListNamespaces",
            member: "Namespaces",
            body: json!({}),
            declared: NAMESPACE_SUMMARY_MEMBERS,
            records: namespace_records,
            account_members: &["ResourceOwner"],
        },
        Listing {
            case: "the services this emulator holds",
            operation: "ListServices",
            member: "Services",
            body: json!({}),
            declared: SERVICE_SUMMARY_MEMBERS,
            records: service_records,
            account_members: &["ResourceOwner", "CreatedByAccount"],
        },
        Listing {
            case: "the instances of one service",
            operation: "ListInstances",
            member: "Instances",
            body: json!({ "ServiceId": instance_service }),
            declared: INSTANCE_SUMMARY_MEMBERS,
            records: instance_records,
            account_members: &["CreatedByAccount"],
        },
    ];

    for listing in listings {
        let Listing {
            case: listed,
            operation,
            member,
            body,
            declared,
            records,
            account_members,
        } = listing;
        let expected_ids: BTreeSet<String> = records.keys().cloned().collect();
        for max_results in PAGE_SIZES {
            let case = format!("{listed} by {operation}, MaxResults {max_results:?}");
            let mut request = body.clone();
            if let Some(max) = max_results {
                request["MaxResults"] = json!(max);
            }
            let entries = all_page_entries(&router, &case, operation, member, request).await;

            assert_eq!(
                entry_ids(&case, &entries),
                expected_ids,
                "case {case:?}: expected the listing to serve {expected_ids:?}, got {entries:?}"
            );
            assert!(
                entries.len() > 1,
                "case {case:?}: the rule is about every entry of a multi-entry listing, got \
                 {entries:?}"
            );

            for entry in &entries {
                let id = entry["Id"].as_str().unwrap_or_default().to_string();
                let record = records.get(&id).unwrap_or_else(|| {
                    panic!("case {case:?}: {id} is one of the records this test created")
                });
                assert_projects_record(&case, entry, record, declared);
                for account_member in account_members {
                    let got = entry[account_member].clone();
                    assert_eq!(
                        got, account,
                        "case {case:?}: expected {account_member} of {id} to be the owning \
                         account {account}, got {got:?} in {entry}"
                    );
                    assert_eq!(
                        got.as_str().unwrap_or_default().len(),
                        12,
                        "case {case:?}: AWSAccountId is modelled @length(min 12, max 12), got \
                         {got:?}"
                    );
                }
            }
        }
    }
}

/// Every `ListOperations` entry names one filed operation and the status that operation
/// reports, which is one of the modelled `OperationStatus` values.
///
/// The status itself is compared against the enumeration rather than against one
/// `GetOperation` answer, because an operation the clock settles between the two reads
/// legitimately reports a later status in the second one.
#[tokio::test]
async fn every_listed_operation_names_its_operation_and_a_modelled_status() {
    let router = emulator();

    // Several operations of different types, so every page below holds more than one
    // entry: two namespace creates, a service create (which files none) and two instance
    // registrations.
    let (create_http, http_namespace) = namespace(&router, "listing-operations-http").await;
    let dns_namespace_id = dns_namespace(&router, "listing-operations-dns").await;
    let service_id = service_with(&router, &http_namespace, "listing-operations-svc", &json!({}))
        .await;
    let mut filed = vec![create_http];
    for index in 0..2 {
        let registered = ok(
            &router,
            "RegisterInstance",
            json!({
                "ServiceId": service_id,
                "InstanceId": format!("i-listing-operations-{index}"),
                "Attributes": { "AWS_INSTANCE_IPV4": format!("10.0.1.{index}") },
            }),
        )
        .await;
        filed.push(
            registered["OperationId"]
                .as_str()
                .unwrap_or_else(|| panic!("RegisterInstance files an operation, got {registered}"))
                .to_string(),
        );
    }
    assert!(
        !dns_namespace_id.is_empty(),
        "the DNS namespace create filed its own operation, got an empty namespace id"
    );

    for max_results in PAGE_SIZES {
        let case = format!("the operations this emulator filed, MaxResults {max_results:?}");
        let mut request = json!({});
        if let Some(max) = max_results {
            request["MaxResults"] = json!(max);
        }
        let entries = all_page_entries(&router, &case, "ListOperations", "Operations", request).await;

        for operation_id in &filed {
            assert!(
                entries
                    .iter()
                    .any(|entry| entry["Id"] == json!(operation_id)),
                "case {case:?}: expected {operation_id} to be listed, got {entries:?}"
            );
        }
        assert!(
            entries.len() > 1,
            "case {case:?}: the rule is about every entry of a multi-entry listing, got {entries:?}"
        );

        for entry in &entries {
            for member in OPERATION_SUMMARY_MEMBERS {
                assert!(
                    !entry[member].is_null(),
                    "case {case:?}: expected the summary to carry {member}, got {entry}"
                );
            }
            let status = entry["Status"].as_str().unwrap_or_default();
            assert!(
                OPERATION_STATUS_VALUES.contains(&status),
                "case {case:?}: expected a modelled OperationStatus of \
                 {OPERATION_STATUS_VALUES:?}, got {status:?} in {entry}"
            );
            let read = ok(
                &router,
                "GetOperation",
                json!({ "OperationId": entry["Id"] }),
            )
            .await;
            assert_eq!(
                read["Operation"]["Id"], entry["Id"],
                "case {case:?}: expected the operation read to be the one the summary names, \
                 got {read}"
            );
        }
    }
}
