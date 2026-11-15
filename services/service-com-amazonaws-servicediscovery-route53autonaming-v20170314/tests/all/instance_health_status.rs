// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `GetInstancesHealthStatus`: the health status it reports comes
//! from the stored instance records, only a service that configures health checking has
//! statuses to report, the `Instances` list selects which of them are reported,
//! `MaxResults`/`NextToken` cut the report into pages, and a request naming something
//! that is not stored is refused with the error the model declares.
//!
//! Every request goes through `MultiServiceRouter::dispatch`, i.e. the same path the
//! emulator serves in production (`awsJson1_1`: `POST /` with `X-Amz-Target`), reusing
//! the harness in `cloud_map_core`.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use service_com_amazonaws_servicediscovery_route53autonaming_v20170314 as cloudmap;

use crate::cloud_map_core::{call, dns_namespace, emulator, failure, namespace, ok};

/// The `HealthStatus` values this API reports, from the model's `HealthStatus` enum
/// (`smithy_query(query="shape",shape="HealthStatus")` -> `HEALTHY`, `UNHEALTHY`,
/// `UNKNOWN`). Every one of them is exercised by the table below.
const HEALTH_STATUS_VALUES: [&str; 3] = ["HEALTHY", "UNHEALTHY", "UNKNOWN"];

/// The health check types the model's `HealthCheckType` enum publishes, from the crate's
/// own catalog rather than copied (`cloudmap::ops::types::HEALTH_CHECK_TYPE_VALUES`).
const HEALTH_CHECK_TYPES: &[&str] = cloudmap::ops::types::HEALTH_CHECK_TYPE_VALUES;

/// How this operation's refusal names the scope of a request that listed no `Instances`,
/// from the documentation of `GetInstancesHealthStatusRequest$Instances`: "If you omit
/// Instances, Cloud Map returns the health status for all the instances that are
/// associated with the specified service."
const ALL_INSTANCES: &str = "All instances";

/// Create a service with a custom health check in `namespace_id` and return its id.
async fn custom_health_service(
    router: &smithy_server_runtime::MultiServiceRouter,
    namespace_id: &str,
    name: &str,
) -> String {
    service_with(
        router,
        namespace_id,
        name,
        json!({ "HealthCheckCustomConfig": { "FailureThreshold": 1 } }),
    )
    .await
}

/// Create a service in `namespace_id` from the extra request members a row needs, and
/// return its id.
async fn service_with(
    router: &smithy_server_runtime::MultiServiceRouter,
    namespace_id: &str,
    name: &str,
    extra: Value,
) -> String {
    let mut body = json!({ "Name": name, "NamespaceId": namespace_id });
    if let Value::Object(members) = extra {
        for (key, value) in members {
            body[key.as_str()] = value;
        }
    }
    let created = ok(router, "CreateService", body).await;
    created["Service"]["Id"]
        .as_str()
        .expect("CreateService returns the service it stored")
        .to_string()
}

/// The HTTP status, wire error code and message one refused request answers with.
/// `cloud_map_core::failure` answers the first two; the message matters here because
/// this operation's `InstanceNotFound` names the service and the scope it was asked
/// about.
async fn refusal(
    router: &smithy_server_runtime::MultiServiceRouter,
    body: Value,
) -> (u16, String, String) {
    let (status, json) = call(router, "GetInstancesHealthStatus", body).await;
    let code = json["__type"]
        .as_str()
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string();
    // The awsJson error envelope carries the message as `message` or `Message`.
    let message = json["message"]
        .as_str()
        .or_else(|| json["Message"].as_str())
        .unwrap_or_default()
        .to_string();
    assert_ne!(status, 200, "the read must fail, got {json}");
    (status, code, message)
}

/// The ARN `GetService` reports for a stored service.
async fn service_arn(
    router: &smithy_server_runtime::MultiServiceRouter,
    service_id: &str,
) -> String {
    ok(router, "GetService", json!({ "Id": service_id })).await["Service"]["Arn"]
        .as_str()
        .expect("GetService reports the ARN it minted")
        .to_string()
}

/// The `Status` map `GetInstancesHealthStatus` answers with, as an ordered map.
fn status_map(reported: &Value) -> BTreeMap<String, String> {
    reported["Status"]
        .as_object()
        .expect("GetInstancesHealthStatus returns a Status map")
        .iter()
        .map(|(instance_id, status)| {
            (
                instance_id.clone(),
                status
                    .as_str()
                    .expect("a HealthStatus is a string")
                    .to_string(),
            )
        })
        .collect()
}

/// Register `instance_id` in `service_id`, with the given extra attributes.
async fn register(
    router: &smithy_server_runtime::MultiServiceRouter,
    service_id: &str,
    instance_id: &str,
    attributes: Value,
) {
    let mut body = json!({ "ServiceId": service_id, "InstanceId": instance_id });
    body["Attributes"] = attributes;
    ok(router, "RegisterInstance", body).await;
}

/// The report is computed from the stored records: a custom health check's instance
/// carries the status its registration stored (`AWS_INIT_HEALTH_STATUS`, defaulting to
/// `HEALTHY`), while an instance of a service checked by Route 53 is `UNKNOWN` until that
/// check has reported on it.
#[tokio::test]
async fn health_status_is_reported_from_the_stored_instance_records() {
    let router = emulator();
    let (_, namespace_id) = namespace(&router, "health-status-ns").await;
    let custom_id = custom_health_service(&router, &namespace_id, "custom-svc").await;
    let checked_id = service_with(
        &router,
        &namespace_id,
        "route53-svc",
        json!({ "HealthCheckConfig": { "Type": "HTTP", "ResourcePath": "/ping" } }),
    )
    .await;

    // (case, service, instance, registration attributes, expected HealthStatus)
    let rows: [(&str, &str, &str, Value, &str); 4] = [
        (
            "custom health check, registered HEALTHY",
            custom_id.as_str(),
            "i-healthy",
            json!({ "AWS_INIT_HEALTH_STATUS": "HEALTHY", "AWS_INSTANCE_IPV4": "10.0.0.1" }),
            "HEALTHY",
        ),
        (
            "custom health check, registered UNHEALTHY",
            custom_id.as_str(),
            "i-unhealthy",
            json!({ "AWS_INIT_HEALTH_STATUS": "UNHEALTHY", "AWS_INSTANCE_IPV4": "10.0.0.2" }),
            "UNHEALTHY",
        ),
        (
            "custom health check, no initial status named",
            custom_id.as_str(),
            "i-default",
            json!({ "AWS_INSTANCE_IPV4": "10.0.0.3" }),
            "HEALTHY",
        ),
        (
            "a Route 53 health check that has not reported yet",
            checked_id.as_str(),
            "i-probed",
            json!({ "AWS_INSTANCE_IPV4": "10.0.0.4" }),
            "UNKNOWN",
        ),
    ];

    for (_, service_id, instance_id, attributes, _) in &rows {
        register(&router, service_id, instance_id, attributes.clone()).await;
    }

    // Every row, asked for one instance at a time.
    for (case, service_id, instance_id, _, expected) in &rows {
        let reported = ok(
            &router,
            "GetInstancesHealthStatus",
            json!({ "ServiceId": service_id, "Instances": [instance_id] }),
        )
        .await;
        let status = status_map(&reported);
        assert_eq!(
            status,
            BTreeMap::from([((*instance_id).to_string(), (*expected).to_string())]),
            "case {case:?}: expected only {instance_id}={expected}, got {status:?}"
        );
    }

    // Every status in the model's enum is covered by the table above.
    let covered: BTreeSet<&str> = rows.iter().map(|(_, _, _, _, status)| *status).collect();
    let mut missing: Vec<&str> = HEALTH_STATUS_VALUES
        .iter()
        .copied()
        .filter(|status| !covered.contains(status))
        .collect();
    missing.sort_unstable();
    assert!(
        missing.is_empty(),
        "the table must cover every modelled HealthStatus, missing {missing:?}"
    );

    // Omitting `Instances` reports every instance of the named service, and only those.
    let whole_custom = status_map(
        &ok(
            &router,
            "GetInstancesHealthStatus",
            json!({ "ServiceId": custom_id }),
        )
        .await,
    );
    assert_eq!(
        whole_custom,
        BTreeMap::from([
            ("i-default".to_string(), "HEALTHY".to_string()),
            ("i-healthy".to_string(), "HEALTHY".to_string()),
            ("i-unhealthy".to_string(), "UNHEALTHY".to_string()),
        ]),
        "case \"omitted Instances, custom service\": got {whole_custom:?}"
    );
    let whole_checked = status_map(
        &ok(
            &router,
            "GetInstancesHealthStatus",
            json!({ "ServiceId": checked_id }),
        )
        .await,
    );
    assert_eq!(
        whole_checked,
        BTreeMap::from([("i-probed".to_string(), "UNKNOWN".to_string())]),
        "case \"omitted Instances, Route 53 checked service\": got {whole_checked:?}"
    );

    // A health-checked service with no instances reports an empty map rather than
    // failing.
    let empty_id = custom_health_service(&router, &namespace_id, "empty-svc").await;
    let empty = status_map(
        &ok(
            &router,
            "GetInstancesHealthStatus",
            json!({ "ServiceId": empty_id }),
        )
        .await,
    );
    assert!(
        empty.is_empty(),
        "case \"service with no instances\": expected an empty Status, got {empty:?}"
    );
}

/// `Instances` selects exactly the instances named, whatever their number, order or
/// repetition — and the service may be named by its ARN as well as by its id, because
/// the member is modelled as `Arn`.
#[tokio::test]
async fn an_instances_list_reports_exactly_the_distinct_instances_it_names() {
    let router = emulator();
    let (_, namespace_id) = namespace(&router, "select-ns").await;
    let service_id = custom_health_service(&router, &namespace_id, "select-svc").await;
    let service_arn = ok(&router, "GetService", json!({ "Id": service_id }))
        .await["Service"]["Arn"]
        .as_str()
        .expect("GetService reports the ARN it minted")
        .to_string();

    for index in 0..4 {
        let initial = if index % 2 == 0 { "HEALTHY" } else { "UNHEALTHY" };
        register(
            &router,
            &service_id,
            &format!("i-{index}"),
            json!({ "AWS_INIT_HEALTH_STATUS": initial }),
        )
        .await;
    }

    // (case, the service reference, the Instances list, the ids expected back)
    let rows: [(&str, &str, Vec<&str>, Vec<&str>); 5] = [
        ("one id", service_id.as_str(), vec!["i-1"], vec!["i-1"]),
        (
            "two ids, requested out of order",
            service_id.as_str(),
            vec!["i-2", "i-0"],
            vec!["i-0", "i-2"],
        ),
        (
            "a repeated id names one instance",
            service_id.as_str(),
            vec!["i-3", "i-3"],
            vec!["i-3"],
        ),
        (
            "every id",
            service_id.as_str(),
            vec!["i-0", "i-1", "i-2", "i-3"],
            vec!["i-0", "i-1", "i-2", "i-3"],
        ),
        (
            "the service named by its ARN",
            service_arn.as_str(),
            vec!["i-0", "i-1"],
            vec!["i-0", "i-1"],
        ),
    ];

    for (case, reference, requested, expected) in &rows {
        let reported = ok(
            &router,
            "GetInstancesHealthStatus",
            json!({ "ServiceId": reference, "Instances": requested }),
        )
        .await;
        let status = status_map(&reported);
        let reported_ids: Vec<&str> = status.keys().map(String::as_str).collect();
        assert_eq!(
            reported_ids, *expected,
            "case {case:?}: expected {expected:?}, got {status:?}"
        );
        for (instance_id, reported_status) in &status {
            let expected_status = if instance_id
                .rsplit('-')
                .next()
                .and_then(|index| index.parse::<u32>().ok())
                .is_some_and(|index| index % 2 == 0)
            {
                "HEALTHY"
            } else {
                "UNHEALTHY"
            };
            assert_eq!(
                reported_status, expected_status,
                "case {case:?}: {instance_id} was registered {expected_status}, \
                 got {reported_status}"
            );
        }
    }
}

/// `MaxResults` caps a page and `NextToken` resumes after it: walking the pages reports
/// every instance exactly once, and only a truncated page carries a token. The declared
/// range is 1..100, so each bound is accepted and one past each bound is refused before
/// the handler runs.
#[tokio::test]
async fn max_results_cuts_pages_and_next_token_resumes_after_the_last_one() {
    let router = emulator();
    let (_, namespace_id) = namespace(&router, "page-ns").await;
    let service_id = custom_health_service(&router, &namespace_id, "page-svc").await;

    const STORED: usize = 7;
    let mut everything: BTreeMap<String, String> = BTreeMap::new();
    for index in 0..STORED {
        let instance_id = format!("i-{index}");
        let initial = if index % 3 == 0 { "UNHEALTHY" } else { "HEALTHY" };
        register(
            &router,
            &service_id,
            &instance_id,
            json!({ "AWS_INIT_HEALTH_STATUS": initial }),
        )
        .await;
        everything.insert(instance_id, initial.to_string());
    }

    for page_size in [1_usize, 2, 3, 7, 100] {
        let case = format!("MaxResults={page_size}");
        let mut collected: BTreeMap<String, String> = BTreeMap::new();
        let mut pages = 0_usize;
        let mut next_token: Option<String> = None;
        loop {
            let mut body = json!({ "ServiceId": service_id, "MaxResults": page_size });
            if let Some(token) = &next_token {
                body["NextToken"] = json!(token);
            }
            let reported = ok(&router, "GetInstancesHealthStatus", body).await;
            let page = status_map(&reported);
            pages += 1;
            assert!(
                page.len() <= page_size,
                "case {case:?}: a page holds at most {page_size} entries, got {page:?}"
            );
            for (instance_id, status) in page {
                assert!(
                    collected.insert(instance_id.clone(), status).is_none(),
                    "case {case:?}: {instance_id} was reported on two pages"
                );
            }
            next_token = reported["NextToken"].as_str().map(str::to_string);
            if next_token.is_none() {
                break;
            }
            assert!(
                pages <= STORED + 1,
                "case {case:?}: the paging loop must terminate, got {pages} pages"
            );
        }
        assert_eq!(
            collected, everything,
            "case {case:?}: paging must report every stored instance exactly once"
        );
        assert_eq!(
            pages,
            STORED.div_ceil(page_size),
            "case {case:?}: expected {} pages for {STORED} instances",
            STORED.div_ceil(page_size)
        );
    }

    // No `MaxResults` reports up to the documented default of 100, i.e. everything here.
    let all = status_map(
        &ok(
            &router,
            "GetInstancesHealthStatus",
            json!({ "ServiceId": service_id }),
        )
        .await,
    );
    assert_eq!(
        all, everything,
        "case \"no MaxResults\": every stored instance is reported, got {all:?}"
    );

    // The published range's bounds, and one past each of them. An explicitly empty
    // `Instances` list is outside the modelled `length min 1` and is refused too, rather
    // than answered with an empty report a caller cannot tell from an empty service.
    for (case, body, expected_status) in [
        (
            "the low bound",
            json!({ "ServiceId": service_id, "MaxResults": 1 }),
            200_u16,
        ),
        (
            "the high bound",
            json!({ "ServiceId": service_id, "MaxResults": 100 }),
            200,
        ),
        (
            "one below the low bound",
            json!({ "ServiceId": service_id, "MaxResults": 0 }),
            400,
        ),
        (
            "one above the high bound",
            json!({ "ServiceId": service_id, "MaxResults": 101 }),
            400,
        ),
        (
            "an empty Instances list",
            json!({ "ServiceId": service_id, "Instances": [] }),
            400,
        ),
    ] {
        let (status, reported) = call(&router, "GetInstancesHealthStatus", body).await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected HTTP {expected_status}, got {status} {reported}"
        );
    }
}

/// Every modelled refusal, over a table of requests that name something this service
/// does not hold.
#[tokio::test]
async fn requests_naming_something_unstored_answer_their_modelled_errors() {
    let router = emulator();
    let (_, namespace_id) = namespace(&router, "refuse-ns").await;
    let service_id = custom_health_service(&router, &namespace_id, "refuse-svc").await;
    register(&router, &service_id, "i-1", json!({ "stage": "prod" })).await;

    // A token for another scope is well formed but was not issued by this operation.
    let foreign_token = hex::encode(format!("ListInstances{}i-1", '\u{1f}'));

    for (case, body, expected_status, expected_code) in [
        (
            // This read reports whether instances have a status, so a service it
            // cannot resolve has none to report: the refusal is an instance miss, not
            // a service miss.
            "a service id nothing was created under",
            json!({ "ServiceId": "srv-nosuchservice" }),
            400,
            "InstanceNotFound",
        ),
        (
            "an instance that is not registered with the service",
            json!({ "ServiceId": service_id, "Instances": ["i-9"] }),
            400,
            "InstanceNotFound",
        ),
        (
            "one known and one unknown instance",
            json!({ "ServiceId": service_id, "Instances": ["i-1", "i-9"] }),
            400,
            "InstanceNotFound",
        ),
        (
            "a NextToken that is not hex at all",
            json!({ "ServiceId": service_id, "NextToken": "not-a-token" }),
            400,
            "InvalidInput",
        ),
        (
            "a well-formed NextToken from another paginated read",
            json!({ "ServiceId": service_id, "NextToken": foreign_token }),
            400,
            "InvalidInput",
        ),
    ] {
        let (status, code) = failure(&router, "GetInstancesHealthStatus", body).await;
        assert_eq!(
            (status, code.as_str()),
            (expected_status, expected_code),
            "case {case:?}: expected HTTP {expected_status} {expected_code}, got {status} {code}"
        );
    }
}

/// Reporting health status is a read: it changes nothing a later caller sees.
#[tokio::test]
async fn reporting_health_status_changes_nothing_a_later_caller_sees() {
    let router = emulator();
    let (_, namespace_id) = namespace(&router, "readonly-ns").await;
    let service_id = custom_health_service(&router, &namespace_id, "readonly-svc").await;
    for index in 0..3 {
        register(
            &router,
            &service_id,
            &format!("i-{index}"),
            json!({ "AWS_INSTANCE_IPV4": format!("10.0.0.{index}") }),
        )
        .await;
    }

    let before = ok(
        &router,
        "GetInstancesHealthStatus",
        json!({ "ServiceId": service_id }),
    )
    .await;
    let revision_before = ok(
        &router,
        "DiscoverInstances",
        json!({ "NamespaceName": "readonly-ns", "ServiceName": "readonly-svc" }),
    )
    .await["InstancesRevision"]
        .clone();

    // Read it again, and through a paged walk, which also decodes a cursor.
    for _ in 0..3 {
        ok(
            &router,
            "GetInstancesHealthStatus",
            json!({ "ServiceId": service_id, "MaxResults": 1 }),
        )
        .await;
    }
    let after = ok(
        &router,
        "GetInstancesHealthStatus",
        json!({ "ServiceId": service_id }),
    )
    .await;
    let revision_after = ok(
        &router,
        "DiscoverInstances",
        json!({ "NamespaceName": "readonly-ns", "ServiceName": "readonly-svc" }),
    )
    .await["InstancesRevision"]
        .clone();

    assert_eq!(
        status_map(&before),
        status_map(&after),
        "case \"repeated reads\": the report must not change, got {after}"
    );
    assert_eq!(
        revision_before, revision_after,
        "case \"repeated reads\": InstancesRevision must not move, got {revision_after}"
    );
    let instance_count = ok(&router, "GetService", json!({ "Id": service_id })).await["Service"]
        ["InstanceCount"]
        .clone();
    assert_eq!(
        instance_count,
        json!(3),
        "case \"repeated reads\": the service still holds its 3 instances, got {instance_count}"
    );
}

/// An instance has a health status only where its service configures health checking: a
/// service that configures neither kind of check has no instance whose health anything
/// tracks, so every scope of this read is refused with `InstanceNotFound` naming that
/// service and the scope asked about, while a service carrying either configuration is
/// served.
///
/// The table spans both health configurations the model declares for a service
/// (`CreateServiceRequest$HealthCheckConfig` with every `HealthCheckType` the model's
/// enum publishes, and `CreateServiceRequest$HealthCheckCustomConfig`), the absence of
/// both, and both ways a request names its scope (the ids it lists, and the whole
/// service when it lists none).
#[tokio::test]
async fn only_a_service_that_configures_health_checking_reports_a_status() {
    let router = emulator();
    let (_, namespace_id) = namespace(&router, "gated-ns").await;

    // (case, the health configuration the service is created with, whether the read is
    // served)
    let mut rows: Vec<(String, Value, bool)> = vec![
        (
            "no health configuration at all".to_string(),
            json!({}),
            false,
        ),
        (
            "a custom health check".to_string(),
            json!({ "HealthCheckCustomConfig": { "FailureThreshold": 1 } }),
            true,
        ),
        (
            "a custom health check with no failure threshold".to_string(),
            json!({ "HealthCheckCustomConfig": {} }),
            true,
        ),
    ];
    // Every check type the model publishes, so the rule is not asserted on one of them.
    for check_type in HEALTH_CHECK_TYPES {
        let mut configuration = json!({ "Type": check_type });
        if *check_type != "TCP" {
            configuration["ResourcePath"] = json!("/ping");
        }
        rows.push((
            format!("a Route 53 {check_type} health check"),
            json!({ "HealthCheckConfig": configuration }),
            true,
        ));
    }

    for (index, (case, health, served)) in rows.iter().enumerate() {
        let service_id = service_with(
            &router,
            &namespace_id,
            &format!("gated-svc-{index}"),
            health.clone(),
        )
        .await;
        let arn = service_arn(&router, &service_id).await;
        register(&router, &service_id, "i-1", json!({ "stage": "prod" })).await;

        // The two scopes a request can ask about, each with the way this operation's
        // refusal names it.
        let scopes: [(Value, &str); 2] = [
            (json!({ "ServiceId": service_id }), ALL_INSTANCES),
            (
                json!({ "ServiceId": service_id, "Instances": ["i-1"] }),
                "i-1",
            ),
        ];
        for (body, scope) in scopes {
            if *served {
                let reported = status_map(&ok(&router, "GetInstancesHealthStatus", body).await);
                assert!(
                    reported.contains_key("i-1"),
                    "case {case:?} for scope {scope:?}: a health-checked service reports its \
                     instance, got {reported:?}"
                );
                continue;
            }
            let (status, code, message) = refusal(&router, body).await;
            assert_eq!(
                (status, code.as_str()),
                (400, "InstanceNotFound"),
                "case {case:?} for scope {scope:?}: expected 400 InstanceNotFound, \
                 got {status} {code}"
            );
            assert_eq!(
                message,
                format!(
                    "Instance(s) not found for service '{arn}' when trying to retreive: {scope}"
                ),
                "case {case:?} for scope {scope:?}: unexpected message {message:?}"
            );
        }
    }
}

/// Adding health checking to a service makes the read it was refused for serve, and
/// deleting it again makes the read refused once more: the refusal follows the stored
/// configuration rather than the service's age or its instances.
#[tokio::test]
async fn the_refusal_follows_the_stored_health_configuration() {
    let router = emulator();
    let dns_id = dns_namespace(&router, "gated-dns").await;
    let service_id = service_with(
        &router,
        &dns_id,
        "gated-updatable-svc",
        json!({ "DnsConfig": { "DnsRecords": [{ "Type": "A", "TTL": 60 }] } }),
    )
    .await;
    register(&router, &service_id, "i-1", json!({ "AWS_INSTANCE_IPV4": "10.0.0.1" })).await;

    let (status, code, _) = refusal(&router, json!({ "ServiceId": service_id })).await;
    assert_eq!(
        (status, code.as_str()),
        (400, "InstanceNotFound"),
        "case \"created with no health check\": expected 400 InstanceNotFound, got {status} {code}"
    );

    ok(
        &router,
        "UpdateService",
        json!({
            "Id": service_id,
            "Service": {
                "DnsConfig": { "DnsRecords": [{ "Type": "A", "TTL": 60 }] },
                "HealthCheckConfig": { "Type": "HTTP", "ResourcePath": "/ping" },
            },
        }),
    )
    .await;
    let reported = status_map(
        &ok(
            &router,
            "GetInstancesHealthStatus",
            json!({ "ServiceId": service_id }),
        )
        .await,
    );
    assert!(
        reported.contains_key("i-1"),
        "case \"health check added\": the read is served, got {reported:?}"
    );

    ok(
        &router,
        "UpdateService",
        json!({
            "Id": service_id,
            "Service": { "DnsConfig": { "DnsRecords": [{ "Type": "A", "TTL": 60 }] } },
        }),
    )
    .await;
    let (status, code, _) = refusal(&router, json!({ "ServiceId": service_id })).await;
    assert_eq!(
        (status, code.as_str()),
        (400, "InstanceNotFound"),
        "case \"health check deleted\": expected 400 InstanceNotFound, got {status} {code}"
    );
}
