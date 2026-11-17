// SPDX-License-Identifier: CC0-1.0

//! What a service's `DnsConfig` may declare, and what a registration against it must
//! carry.
//!
//! Two rules, both stated on `DnsRecord$Type` and `RegisterInstanceRequest$Attributes`:
//!
//! * a `CNAME` record is served only under the `WEIGHTED` routing policy, and never
//!   beside a `HealthCheckConfig` — a `CNAME` answer is a domain NAME, so one query can
//!   be answered with only one of them;
//! * every record a service declares names the instance attribute Route 53 answers a
//!   query from, and a registration that carries none of them is refused with
//!   `InvalidInput` rather than stored as an instance no record could represent.
//!
//! Both are asserted over a TABLE: every `RecordType` the model publishes, each routing
//! policy, and registrations that carry each attribute, none of them, the documented alias
//! alternative, and unrelated attributes only.
//!
//! Every request goes through `MultiServiceRouter::dispatch`, the same path the emulator
//! serves in production; the harness lives in `cloud_map_core`.

use serde_json::{json, Value};
use service_com_amazonaws_servicediscovery_route53autonaming_v20170314 as cloudmap;
use smithy_server_runtime::MultiServiceRouter;

use crate::cloud_map_core::{dns_namespace, emulator, ok, refusal};

/// The `RecordType` values the model publishes, which is the catalog every row below is
/// drawn from (`smithy_query(query="shape", shape="RecordType")`).
const RECORD_TYPES: &[&str] = cloudmap::ops::types::RECORD_TYPE_VALUES;

/// The `RoutingPolicy` values the model publishes
/// (`smithy_query(query="shape", shape="RoutingPolicy")`).
const ROUTING_POLICIES: &[&str] = cloudmap::ops::types::ROUTING_POLICY_VALUES;

/// The refusal `DnsRecord$Type` states for a `CNAME` record under any other policy.
const CNAME_NEEDS_WEIGHTED: &str =
    "WEIGHTED routing policy must be used with DNS Record Type CNAME";

/// Dispatch one operation and report its status, wire error code and message text — the
/// shared `refusal` with an absent message read as the empty one, so every row below
/// compares three strings whether the call was served or refused.
async fn answered(
    router: &MultiServiceRouter,
    operation: &str,
    body: Value,
) -> (u16, String, String) {
    let (status, code, message) = refusal(router, operation, body).await;
    (status, code, message.unwrap_or_default())
}

/// Create a service with `dns_config` in `namespace_id`, answering how the call was
/// answered.
async fn create_service(
    router: &MultiServiceRouter,
    namespace_id: &str,
    name: &str,
    dns_config: Value,
    extra: Value,
) -> (u16, String, String) {
    let mut body = json!({ "Name": name, "NamespaceId": namespace_id });
    if !dns_config.is_null() {
        body["DnsConfig"] = dns_config;
    }
    if let Value::Object(members) = extra {
        for (key, value) in members {
            body[key.as_str()] = value;
        }
    }
    answered(router, "CreateService", body).await
}

/// One `DnsRecord`.
fn record(record_type: &str) -> Value {
    json!({ "Type": record_type, "TTL": 60 })
}

/// A `CNAME` record is created only under `WEIGHTED`, and every other record type is
/// created under either policy — the whole published `RecordType` × `RoutingPolicy`
/// product, plus the rows that state no policy at all.
#[tokio::test]
async fn a_cname_service_is_created_only_under_the_weighted_routing_policy() {
    let router = emulator();
    let namespace_id = dns_namespace(&router, "cname-rules").await;

    assert!(
        RECORD_TYPES.contains(&"CNAME") && ROUTING_POLICIES.contains(&"WEIGHTED"),
        "the model's catalogs must carry the values this rule is about, got {RECORD_TYPES:?} \
         and {ROUTING_POLICIES:?}"
    );

    let mut index = 0_usize;
    for record_type in RECORD_TYPES {
        // `None` is the row that states no `RoutingPolicy`, where the service applies its
        // own default — the policy the records would really be served under.
        for policy in ROUTING_POLICIES.iter().map(Some).chain([None]) {
            index += 1;
            let case = format!("{record_type} under {policy:?}");
            let mut dns_config = json!({ "DnsRecords": [record(record_type)] });
            if let Some(policy) = policy {
                dns_config["RoutingPolicy"] = json!(policy);
            }
            let refused = *record_type == "CNAME" && policy != Some(&"WEIGHTED");
            let (status, code, message) = create_service(
                &router,
                &namespace_id,
                &format!("cname-rules-{index}"),
                dns_config,
                Value::Null,
            )
            .await;
            if refused {
                assert_eq!(
                    (status, code.as_str(), message.as_str()),
                    (400, "InvalidInput", CNAME_NEEDS_WEIGHTED),
                    "case {case:?}: expected 400 InvalidInput {CNAME_NEEDS_WEIGHTED:?}, got \
                     {status} {code} {message:?}"
                );
            } else {
                assert_eq!(
                    status, 200,
                    "case {case:?}: this configuration is one Cloud Map serves, got {status} \
                     {code} {message:?}"
                );
            }
        }
    }
}

/// "You can't specify both `CNAME` for `Type` and settings for `HealthCheckConfig`. If you
/// do, the request will fail with an `InvalidInput` error" — on the create, and on the
/// update that would add the health check to a stored `CNAME` service.
#[tokio::test]
async fn a_cname_record_and_a_health_check_cannot_be_configured_together() {
    let router = emulator();
    let namespace_id = dns_namespace(&router, "cname-health").await;
    let health_check = json!({ "HealthCheckConfig": { "Type": "HTTP", "ResourcePath": "/" } });

    // Rows differ along the record type the service declares: `CNAME` is the one type the
    // rule refuses, and every other published type takes the same health check.
    for (index, record_type) in RECORD_TYPES.iter().enumerate() {
        let mut dns_config = json!({ "DnsRecords": [record(record_type)] });
        if *record_type == "CNAME" {
            dns_config["RoutingPolicy"] = json!("WEIGHTED");
        }

        let case = format!("a {record_type} service created with a health check");
        let (status, code, message) = create_service(
            &router,
            &namespace_id,
            &format!("cname-health-{index}"),
            dns_config.clone(),
            health_check.clone(),
        )
        .await;
        if *record_type == "CNAME" {
            assert_eq!(
                (status, code.as_str()),
                (400, "InvalidInput"),
                "case {case:?}: expected 400 InvalidInput, got {status} {code} {message:?}"
            );
            assert!(
                message.contains("CNAME") && message.contains("HealthCheckConfig"),
                "case {case:?}: the refusal names both settings it cannot hold together, got \
                 {message:?}"
            );
        } else {
            assert_eq!(
                status, 200,
                "case {case:?}: a health check belongs to this record type, got {status} \
                 {code} {message:?}"
            );
        }

        // The same rule through `UpdateService`, which may add a health check to a service
        // created without one.
        let case = format!("a health check added to a stored {record_type} service");
        let created = ok(
            &router,
            "CreateService",
            json!({
                "Name": format!("cname-health-update-{index}"),
                "NamespaceId": namespace_id,
                "DnsConfig": dns_config,
            }),
        )
        .await;
        let service_id = created["Service"]["Id"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: CreateService answers the stored service"))
            .to_string();
        let (status, code, message) = answered(
            &router,
            "UpdateService",
            json!({
                "Id": service_id,
                "Service": { "DnsConfig": { "DnsRecords": [record(record_type)] },
                             "HealthCheckConfig": { "Type": "HTTP", "ResourcePath": "/" } },
            }),
        )
        .await;
        if *record_type == "CNAME" {
            assert_eq!(
                (status, code.as_str()),
                (400, "InvalidInput"),
                "case {case:?}: expected 400 InvalidInput, got {status} {code} {message:?}"
            );
            // The refused update stored nothing.
            let read = ok(&router, "GetService", json!({ "Id": service_id })).await;
            assert!(
                read["Service"]["HealthCheckConfig"].is_null(),
                "case {case:?}: a refused update adds no health check, got {read}"
            );
        } else {
            assert_eq!(
                status, 200,
                "case {case:?}: a health check belongs to this record type, got {status} \
                 {code} {message:?}"
            );
            let read = ok(&router, "GetService", json!({ "Id": service_id })).await;
            assert_eq!(
                read["Service"]["HealthCheckConfig"]["Type"],
                json!("HTTP"),
                "case {case:?}: the accepted update stored the health check, got {read}"
            );
        }
    }
}

/// Every record a service declares requires the attribute Route 53 answers its queries
/// from, and a registration missing one is refused with `InvalidInput` naming exactly the
/// attributes it is missing.
#[tokio::test]
async fn a_registration_must_carry_the_attributes_the_declared_records_require() {
    let router = emulator();
    let namespace_id = dns_namespace(&router, "required-attrs").await;

    // (the records the service declares, the attributes the registration carries, the
    //  attribute names the refusal must list — empty when the registration is accepted)
    let cases: Vec<(&str, Value, Value, Vec<&str>)> = vec![
        (
            "an A service with no attribute at all",
            json!([record("A")]),
            json!({}),
            vec!["AWS_INSTANCE_IPV4"],
        ),
        (
            "an A service with only an unrelated attribute",
            json!([record("A")]),
            json!({ "unrelated": "x" }),
            vec!["AWS_INSTANCE_IPV4"],
        ),
        (
            "an A service with the IPv4 address",
            json!([record("A")]),
            json!({ "AWS_INSTANCE_IPV4": "192.0.2.44" }),
            vec![],
        ),
        (
            "an A service with the IPv6 address instead",
            json!([record("A")]),
            json!({ "AWS_INSTANCE_IPV6": "2001:db8::1" }),
            vec!["AWS_INSTANCE_IPV4"],
        ),
        (
            "an AAAA service with no attribute",
            json!([record("AAAA")]),
            json!({}),
            vec!["AWS_INSTANCE_IPV6"],
        ),
        (
            "an AAAA service with the IPv6 address",
            json!([record("AAAA")]),
            json!({ "AWS_INSTANCE_IPV6": "2001:db8::1" }),
            vec![],
        ),
        (
            "an A and AAAA service with only the IPv4 address",
            json!([record("A"), record("AAAA")]),
            json!({ "AWS_INSTANCE_IPV4": "192.0.2.44" }),
            vec!["AWS_INSTANCE_IPV6"],
        ),
        (
            "an A and AAAA service with both addresses",
            json!([record("A"), record("AAAA")]),
            json!({ "AWS_INSTANCE_IPV4": "192.0.2.44", "AWS_INSTANCE_IPV6": "2001:db8::1" }),
            vec![],
        ),
        (
            "an A service with the documented alias alternative",
            json!([record("A")]),
            json!({ "AWS_ALIAS_DNS_NAME": "lb-1.us-west-2.elb.amazonaws.com" }),
            vec![],
        ),
        (
            "an A and AAAA service with the alias alternative",
            json!([record("A"), record("AAAA")]),
            json!({ "AWS_ALIAS_DNS_NAME": "lb-2.us-west-2.elb.amazonaws.com" }),
            vec![],
        ),
        (
            "a CNAME service with no attribute",
            json!([record("CNAME")]),
            json!({}),
            vec!["AWS_INSTANCE_CNAME"],
        ),
        (
            "a CNAME service with the domain name",
            json!([record("CNAME")]),
            json!({ "AWS_INSTANCE_CNAME": "example.com" }),
            vec![],
        ),
        (
            "an SRV service with no attribute, which is missing the port and an address",
            json!([record("SRV")]),
            json!({}),
            vec!["AWS_INSTANCE_IPV4", "AWS_INSTANCE_IPV6", "AWS_INSTANCE_PORT"],
        ),
        (
            "an SRV service with only the port",
            json!([record("SRV")]),
            json!({ "AWS_INSTANCE_PORT": "8080" }),
            vec!["AWS_INSTANCE_IPV4", "AWS_INSTANCE_IPV6"],
        ),
        (
            "an SRV service with the port and one address",
            json!([record("SRV")]),
            json!({ "AWS_INSTANCE_PORT": "8080", "AWS_INSTANCE_IPV4": "192.0.2.44" }),
            vec![],
        ),
        (
            "an SRV service with the port and the other address",
            json!([record("SRV")]),
            json!({ "AWS_INSTANCE_PORT": "8080", "AWS_INSTANCE_IPV6": "2001:db8::1" }),
            vec![],
        ),
        (
            "an SRV service with both addresses but no port",
            json!([record("SRV")]),
            json!({ "AWS_INSTANCE_IPV4": "192.0.2.44", "AWS_INSTANCE_IPV6": "2001:db8::1" }),
            vec!["AWS_INSTANCE_PORT"],
        ),
    ];

    for (index, (case, records, attributes, missing)) in cases.into_iter().enumerate() {
        let cnamed = records
            .as_array()
            .is_some_and(|records| records.iter().any(|record| record["Type"] == json!("CNAME")));
        let mut dns_config = json!({ "DnsRecords": records });
        if cnamed {
            dns_config["RoutingPolicy"] = json!("WEIGHTED");
        }
        let created = ok(
            &router,
            "CreateService",
            json!({
                "Name": format!("required-attrs-{index}"),
                "NamespaceId": namespace_id,
                "DnsConfig": dns_config,
            }),
        )
        .await;
        let service_id = created["Service"]["Id"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: CreateService answers the stored service"))
            .to_string();

        let body = json!({
            "ServiceId": service_id,
            "InstanceId": format!("i-{index}"),
            "Attributes": attributes,
        });
        let (status, code, message) = answered(&router, "RegisterInstance", body.clone()).await;
        if missing.is_empty() {
            assert_eq!(
                status, 200,
                "case {case:?}: every required attribute is there, got {status} {code} \
                 {message:?}"
            );
            // The instance is really registered, read back through the data plane.
            let read = ok(
                &router,
                "GetInstance",
                json!({ "ServiceId": service_id, "InstanceId": format!("i-{index}") }),
            )
            .await;
            assert_eq!(
                read["Instance"]["Id"],
                json!(format!("i-{index}")),
                "case {case:?}: the accepted registration reads back, got {read}"
            );
        } else {
            let expected = format!("Required attribute(s) {} are missing.", missing.join(", "));
            assert_eq!(
                (status, code.as_str(), message.as_str()),
                (400, "InvalidInput", expected.as_str()),
                "case {case:?}: expected 400 InvalidInput {expected:?}, got {status} {code} \
                 {message:?}"
            );
            // Nothing was stored for a refused registration.
            let (status, code, _) = answered(
                &router,
                "GetInstance",
                json!({ "ServiceId": service_id, "InstanceId": format!("i-{index}") }),
            )
            .await;
            assert_eq!(
                (status, code.as_str()),
                (400, "InstanceNotFound"),
                "case {case:?}: a refused registration stores no instance"
            );
        }
    }
}

/// A service that declares NO records requires no attribute at all: an HTTP service's
/// instances are found by `DiscoverInstances`, which answers whatever attributes they
/// carry.
#[tokio::test]
async fn a_service_with_no_records_requires_no_attribute() {
    let router = emulator();
    let (_, namespace_id) = crate::cloud_map_core::namespace(&router, "no-records-ns").await;
    let service_id =
        crate::cloud_map_core::service(&router, &namespace_id, "no-records-svc").await;

    // Rows differ along what the registration carries: nothing, a custom attribute, and
    // an address attribute no record uses.
    for (index, attributes) in [
        json!({}),
        json!({ "stage": "prod" }),
        json!({ "AWS_INSTANCE_IPV4": "192.0.2.44" }),
    ]
    .into_iter()
    .enumerate()
    {
        let instance_id = format!("i-no-records-{index}");
        let (status, code, message) = answered(
            &router,
            "RegisterInstance",
            json!({
                "ServiceId": service_id,
                "InstanceId": instance_id,
                "Attributes": attributes.clone(),
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {attributes}: a service declaring no record requires no attribute, got \
             {status} {code} {message:?}"
        );
        let read = ok(
            &router,
            "GetInstance",
            json!({ "ServiceId": service_id, "InstanceId": instance_id }),
        )
        .await;
        assert_eq!(
            read["Instance"]["Attributes"], attributes,
            "case {attributes}: the registration round-trips, got {read}"
        );
    }
}
