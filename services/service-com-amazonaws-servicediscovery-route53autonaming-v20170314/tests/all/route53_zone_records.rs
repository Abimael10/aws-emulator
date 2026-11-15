// SPDX-License-Identifier: CC0-1.0

//! The Route 53 zone a DNS namespace IS, and the record sets its services publish into it.
//!
//! RULE: a DNS namespace reports the id of a hosted zone Route 53 HOLDS — private and
//! associated with the requested VPC for a private namespace — and Cloud Map keeps that
//! zone's record sets in step with its own state: one record set per registered instance
//! per record the service's `DnsConfig` declares, at `<serviceName>.<namespaceName>.`,
//! identified by the instance id, carrying that record's TTL, the routing policy's own
//! member (`MultiValueAnswer` for `MULTIVALUE`, `Weight` for `WEIGHTED`) and the record
//! data read off the instance's attributes. An update of the service's TTLs rewrites those
//! record sets, a deregistration removes exactly the deregistered instance's, and deleting
//! the namespace deletes the zone.
//!
//! Every Cloud Map call goes through `MultiServiceRouter::dispatch` (`awsJson1_1`, the same
//! front door a client uses) and every Route 53 assertion is read back through Route 53's
//! own API with the AWS SDK over the emulator's in-process transport — so what is asserted
//! is what a DNS-based client would be told, not this crate's own state.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use super::cloud_map_core::{call, emulator, ok};

/// A VPC id of the shape `CreatePrivateDnsNamespace$Vpc` takes (`ResourceId`, 0..64), in
/// the long form Amazon EC2 issues today.
const VPC: &str = "vpc-0123456789abcdef0";

/// The Route 53 client every assertion here reads the zone through: the emulator's own
/// Route 53, reached in process, addressed as the global service it is.
fn route53(router: &MultiServiceRouter) -> ::aws_sdk_route53::Client {
    let callback = EmbeddedEmulator::from_router(router.clone());
    let config = ::aws_sdk_route53::Config::builder()
        .behavior_version(::aws_sdk_route53::config::BehaviorVersion::latest())
        .http_client(callback.http_client())
        .credentials_provider(callback.aws_credentials_provider())
        .region(::aws_sdk_route53::config::Region::new("us-east-1".to_string()))
        .endpoint_url("https://route53.amazonaws.com")
        .retry_config(::aws_sdk_route53::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_route53::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_route53::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_route53::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_route53::Client::from_conf(config)
}

/// One record set as a DNS client reads it back, with only the members this rule is about.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Observed {
    name: String,
    record_type: String,
    set_identifier: Option<String>,
    ttl: Option<i64>,
    multi_value_answer: Option<bool>,
    weight: Option<i64>,
    values: Vec<String>,
}

/// One expected record set, stated by hand from the rule rather than computed by the code
/// under test.
fn expected(
    name: &str,
    record_type: &str,
    set_identifier: Option<&str>,
    ttl: i64,
    multi_value_answer: Option<bool>,
    weight: Option<i64>,
    values: &[&str],
) -> Observed {
    Observed {
        name: name.to_string(),
        record_type: record_type.to_string(),
        set_identifier: set_identifier.map(str::to_string),
        ttl: Some(ttl),
        multi_value_answer,
        weight,
        values: values.iter().map(|value| (*value).to_string()).collect(),
    }
}

/// Every record set a zone holds BELOW its apex — the zone's own SOA and NS records are
/// Route 53's, not Cloud Map's, so they are not part of this rule.
async fn published(
    router: &MultiServiceRouter,
    zone_id: &str,
    zone_name: &str,
) -> BTreeSet<Observed> {
    let listed = route53(router)
        .list_resource_record_sets()
        .hosted_zone_id(zone_id)
        .send()
        .await
        .unwrap_or_else(|error| panic!("the zone {zone_id} must be readable: {error}"));
    let apex = format!("{}.", zone_name.trim_end_matches('.'));
    listed
        .resource_record_sets()
        .iter()
        .filter(|record| record.name() != apex)
        .map(|record| Observed {
            name: record.name().to_string(),
            record_type: record.r#type().as_str().to_string(),
            set_identifier: record.set_identifier().map(str::to_string),
            ttl: record.ttl(),
            multi_value_answer: record.multi_value_answer(),
            weight: record.weight(),
            values: record
                .resource_records()
                .iter()
                .map(|value| value.value().to_string())
                .collect(),
        })
        .collect()
}

/// Create a namespace of `operation` and return `(namespace id, hosted zone id)` as
/// `GetNamespace` reports them.
async fn namespace_with_zone(
    router: &MultiServiceRouter,
    operation: &str,
    body: Value,
) -> (String, String) {
    let created = ok(router, operation, body).await;
    let operation_id = created["OperationId"]
        .as_str()
        .unwrap_or_else(|| panic!("{operation} returns an OperationId, got {created}"))
        .to_string();
    let filed = ok(router, "GetOperation", json!({ "OperationId": operation_id })).await;
    let namespace_id = filed["Operation"]["Targets"]["NAMESPACE"]
        .as_str()
        .unwrap_or_else(|| panic!("the CREATE_NAMESPACE operation targets a namespace: {filed}"))
        .to_string();
    let read = ok(router, "GetNamespace", json!({ "Id": namespace_id })).await;
    let zone_id = read["Namespace"]["Properties"]["DnsProperties"]["HostedZoneId"]
        .as_str()
        .unwrap_or_else(|| panic!("a DNS namespace reports a HostedZoneId, got {read}"))
        .to_string();
    (namespace_id, zone_id)
}

/// Create a service with a `DnsConfig` and return its id.
async fn dns_service(
    router: &MultiServiceRouter,
    namespace_id: &str,
    name: &str,
    dns_config: Value,
) -> String {
    let created = ok(
        router,
        "CreateService",
        json!({ "Name": name, "NamespaceId": namespace_id, "DnsConfig": dns_config }),
    )
    .await;
    created["Service"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateService returns the service it stored, got {created}"))
        .to_string()
}

/// Register an instance.
async fn register(
    router: &MultiServiceRouter,
    service_id: &str,
    instance_id: &str,
    attributes: Value,
) {
    ok(
        router,
        "RegisterInstance",
        json!({
            "ServiceId": service_id,
            "InstanceId": instance_id,
            "Attributes": attributes,
        }),
    )
    .await;
}

/// One row of the publication table: a service configuration, the instances registered in
/// it, and the record sets the zone must then hold.
struct Row {
    case: &'static str,
    namespace: &'static str,
    service: &'static str,
    dns_config: Value,
    instances: Vec<(&'static str, Value)>,
    expected: Vec<Observed>,
}

/// A registered instance becomes record sets of the namespace's hosted zone, one per record
/// the service declares, with the members the service's routing policy decides.
///
/// The rows differ along what the rule generalises over: the record TYPE (the model's
/// `RecordType` value set — `A`, `AAAA`, `CNAME`, `SRV`
/// (`smithy_query(query="shape",shape="RecordType")`)), the number of records one service
/// declares, the TTL, the routing policy (`RoutingPolicy`: `MULTIVALUE`, `WEIGHTED`) and
/// the number of instances registered.
#[tokio::test]
async fn a_registered_instance_is_published_as_record_sets_of_the_namespace_zone() {
    let router = emulator();

    let rows: Vec<Row> = vec![
        Row {
            case: "MULTIVALUE A, two instances",
            namespace: "multivalue-a.example.com",
            service: "backend",
            dns_config: json!({ "RoutingPolicy": "MULTIVALUE",
                                "DnsRecords": [{ "Type": "A", "TTL": 60 }] }),
            instances: vec![
                ("i-11", json!({ "AWS_INSTANCE_IPV4": "10.0.0.11" })),
                ("i-12", json!({ "AWS_INSTANCE_IPV4": "10.0.0.12" })),
            ],
            expected: vec![
                expected(
                    "backend.multivalue-a.example.com.",
                    "A",
                    Some("i-11"),
                    60,
                    Some(true),
                    None,
                    &["10.0.0.11"],
                ),
                expected(
                    "backend.multivalue-a.example.com.",
                    "A",
                    Some("i-12"),
                    60,
                    Some(true),
                    None,
                    &["10.0.0.12"],
                ),
            ],
        },
        Row {
            case: "the service default routing policy with an AAAA record and a zero TTL",
            namespace: "default-aaaa.example.com",
            service: "api",
            dns_config: json!({ "DnsRecords": [{ "Type": "AAAA", "TTL": 0 }] }),
            instances: vec![(
                "i-v6",
                json!({ "AWS_INSTANCE_IPV6": "2001:0db8:85a3:0000:0000:abcd:0001:2345" }),
            )],
            expected: vec![expected(
                "api.default-aaaa.example.com.",
                "AAAA",
                Some("i-v6"),
                0,
                Some(true),
                None,
                &["2001:0db8:85a3:0000:0000:abcd:0001:2345"],
            )],
        },
        Row {
            // `DnsRecord$Type`: a `CNAME` service "must specify WEIGHTED for the value of
            // RoutingPolicy", and `DnsConfig$RoutingPolicy` says of weighted records that
            // "currently, all records have the same weight".
            case: "WEIGHTED CNAME",
            namespace: "weighted-cname.example.com",
            service: "edge",
            dns_config: json!({ "RoutingPolicy": "WEIGHTED",
                                "DnsRecords": [{ "Type": "CNAME", "TTL": 300 }] }),
            instances: vec![(
                "i-cname",
                json!({ "AWS_INSTANCE_CNAME": "elsewhere.example.org" }),
            )],
            expected: vec![expected(
                "edge.weighted-cname.example.com.",
                "CNAME",
                Some("i-cname"),
                300,
                None,
                Some(1),
                &["elsewhere.example.org"],
            )],
        },
        Row {
            // `DnsRecord$Type` on `SRV`: the data is `priority weight port
            // service-hostname`, priority and weight "both set to 1", the port from
            // `AWS_INSTANCE_PORT` and the service-hostname the instance id, the service
            // name and the namespace name — and "if you specify values for
            // AWS_INSTANCE_IPV4 … Cloud Map automatically creates A … records that have
            // the same name as the value of service-hostname".
            case: "two records at once: A and SRV",
            namespace: "srv-and-a.example.com",
            service: "checkout",
            dns_config: json!({ "DnsRecords": [{ "Type": "A", "TTL": 10 },
                                               { "Type": "SRV", "TTL": 10 }] }),
            instances: vec![(
                "i-srv",
                json!({ "AWS_INSTANCE_IPV4": "10.0.1.7", "AWS_INSTANCE_PORT": "8080" }),
            )],
            expected: vec![
                expected(
                    "checkout.srv-and-a.example.com.",
                    "A",
                    Some("i-srv"),
                    10,
                    Some(true),
                    None,
                    &["10.0.1.7"],
                ),
                expected(
                    "checkout.srv-and-a.example.com.",
                    "SRV",
                    Some("i-srv"),
                    10,
                    Some(true),
                    None,
                    &["1 1 8080 i-srv.checkout.srv-and-a.example.com"],
                ),
                expected(
                    "i-srv.checkout.srv-and-a.example.com.",
                    "A",
                    None,
                    10,
                    None,
                    None,
                    &["10.0.1.7"],
                ),
            ],
        },
    ];

    for row in rows {
        let case = row.case;
        let (namespace_id, zone_id) = namespace_with_zone(
            &router,
            "CreatePrivateDnsNamespace",
            json!({ "Name": row.namespace, "Vpc": VPC }),
        )
        .await;
        let service_id =
            dns_service(&router, &namespace_id, row.service, row.dns_config.clone()).await;
        for (instance_id, attributes) in &row.instances {
            register(&router, &service_id, instance_id, attributes.clone()).await;
        }

        let holds = published(&router, &zone_id, row.namespace).await;
        let wanted: BTreeSet<Observed> = row.expected.into_iter().collect();
        assert_eq!(
            holds, wanted,
            "case {case:?}: expected the zone {zone_id} to hold {wanted:#?}, got {holds:#?}"
        );
    }
}

/// A private namespace's zone is a PRIVATE hosted zone of the namespace's name, associated
/// with the VPC the create named — and it goes away with the namespace.
#[tokio::test]
async fn a_private_namespace_zone_is_private_vpc_associated_and_deleted_with_it() {
    let router = emulator();
    let name = "private-zone.example.com";
    let (namespace_id, zone_id) = namespace_with_zone(
        &router,
        "CreatePrivateDnsNamespace",
        json!({ "Name": name, "Vpc": VPC }),
    )
    .await;

    let read = route53(&router)
        .get_hosted_zone()
        .id(&zone_id)
        .send()
        .await
        .unwrap_or_else(|error| panic!("the reported zone {zone_id} must resolve: {error}"));
    let zone = read
        .hosted_zone()
        .expect("GetHostedZone answers with the hosted zone");
    assert_eq!(
        zone.name(),
        format!("{name}."),
        "the zone carries the namespace's name, got {:?}",
        zone.name()
    );
    assert_eq!(
        zone.config().map(|config| config.private_zone()),
        Some(true),
        "a private DNS namespace's zone is private, got {zone:?}"
    );
    let associated: Vec<&str> = read.vpcs().iter().filter_map(|vpc| vpc.vpc_id()).collect();
    assert_eq!(
        associated,
        vec![VPC],
        "the zone is associated with the VPC the namespace was created with, got {associated:?}"
    );

    // A zone Cloud Map created for a namespace is one Cloud Map MANAGES, which is what
    // `HostedZone$LinkedService` reports: this service's own principal, and the namespace
    // the zone belongs to.
    let namespace = ok(&router, "GetNamespace", json!({ "Id": namespace_id })).await;
    let namespace_arn = namespace["Namespace"]["Arn"]
        .as_str()
        .expect("a namespace reports its ARN")
        .to_string();
    let linked = zone.linked_service();
    assert_eq!(
        linked.and_then(|linked| linked.service_principal()),
        Some("servicediscovery.amazonaws.com"),
        "the zone is linked to Cloud Map as its managing service, got {linked:?}"
    );
    assert_eq!(
        linked.and_then(|linked| linked.description()),
        Some(namespace_arn.as_str()),
        "the linked service names the namespace the zone was created for, got {linked:?}"
    );

    // The namespace's teardown is the zone's: once the namespace is gone, Route 53 holds
    // no zone under the id it reported.
    ok(&router, "DeleteNamespace", json!({ "Id": namespace_id })).await;
    let after = route53(&router).get_hosted_zone().id(&zone_id).send().await;
    let error = after.expect_err("the zone of a deleted namespace must not resolve");
    assert!(
        error
            .as_service_error()
            .is_some_and(::aws_sdk_route53::operation::get_hosted_zone::GetHostedZoneError::is_no_such_hosted_zone),
        "expected NoSuchHostedZone for the zone of a deleted namespace, got {error:?}"
    );
}

/// `UpdateService`'s new TTL is rewritten onto the record sets of every registered
/// instance, and a record type the update leaves out stops being published.
#[tokio::test]
async fn an_update_of_the_service_rewrites_the_published_record_sets() {
    let router = emulator();
    let name = "ttl-rewrite.example.com";
    let (namespace_id, zone_id) = namespace_with_zone(
        &router,
        "CreatePrivateDnsNamespace",
        json!({ "Name": name, "Vpc": VPC }),
    )
    .await;
    let service_id = dns_service(
        &router,
        &namespace_id,
        "backend",
        json!({ "RoutingPolicy": "MULTIVALUE",
                "DnsRecords": [{ "Type": "A", "TTL": 60 }, { "Type": "SRV", "TTL": 60 }] }),
    )
    .await;
    register(
        &router,
        &service_id,
        "i-1",
        json!({ "AWS_INSTANCE_IPV4": "10.0.0.11", "AWS_INSTANCE_PORT": "9000" }),
    )
    .await;

    // (case, the DnsRecords the update states, the TTLs and types the zone must then hold)
    let cases: Vec<(&str, Value, Vec<Observed>)> = vec![
        (
            "a lower TTL on both records",
            json!([{ "Type": "A", "TTL": 30 }, { "Type": "SRV", "TTL": 30 }]),
            vec![
                expected(
                    "backend.ttl-rewrite.example.com.",
                    "A",
                    Some("i-1"),
                    30,
                    Some(true),
                    None,
                    &["10.0.0.11"],
                ),
                expected(
                    "backend.ttl-rewrite.example.com.",
                    "SRV",
                    Some("i-1"),
                    30,
                    Some(true),
                    None,
                    &["1 1 9000 i-1.backend.ttl-rewrite.example.com"],
                ),
                expected(
                    "i-1.backend.ttl-rewrite.example.com.",
                    "A",
                    None,
                    30,
                    None,
                    None,
                    &["10.0.0.11"],
                ),
            ],
        ),
        (
            "a higher TTL on the A record alone, which drops the SRV records",
            json!([{ "Type": "A", "TTL": 120 }]),
            vec![expected(
                "backend.ttl-rewrite.example.com.",
                "A",
                Some("i-1"),
                120,
                Some(true),
                None,
                &["10.0.0.11"],
            )],
        ),
    ];

    for (case, dns_records, wanted) in cases {
        ok(
            &router,
            "UpdateService",
            json!({ "Id": service_id, "Service": { "DnsConfig": { "DnsRecords": dns_records } } }),
        )
        .await;
        let holds = published(&router, &zone_id, name).await;
        let wanted: BTreeSet<Observed> = wanted.into_iter().collect();
        assert_eq!(
            holds, wanted,
            "case {case:?}: expected the zone to hold {wanted:#?}, got {holds:#?}"
        );
    }
}

/// `DeregisterInstance` removes exactly the deregistered instance's record sets and leaves
/// its siblings' where they are, whichever of several instances is deregistered.
#[tokio::test]
async fn deregistering_an_instance_removes_only_its_own_record_sets() {
    let router = emulator();
    let name = "deregister.example.com";
    let (namespace_id, zone_id) = namespace_with_zone(
        &router,
        "CreatePrivateDnsNamespace",
        json!({ "Name": name, "Vpc": VPC }),
    )
    .await;
    let service_id = dns_service(
        &router,
        &namespace_id,
        "backend",
        json!({ "RoutingPolicy": "MULTIVALUE",
                "DnsRecords": [{ "Type": "A", "TTL": 60 }] }),
    )
    .await;

    // Three instances, so a deregistration in the middle of the group is a row too.
    let registered = [
        ("i-1", "10.0.0.11"),
        ("i-2", "10.0.0.12"),
        ("i-3", "10.0.0.13"),
    ];
    for (instance_id, address) in registered {
        register(
            &router,
            &service_id,
            instance_id,
            json!({ "AWS_INSTANCE_IPV4": address }),
        )
        .await;
    }
    let record_of = |instance_id: &str, address: &str| {
        expected(
            "backend.deregister.example.com.",
            "A",
            Some(instance_id),
            60,
            Some(true),
            None,
            &[address],
        )
    };

    // Deregister the middle instance, then the first: each time, only the record sets of
    // the instances still registered remain.
    let mut surviving: Vec<(&str, &str)> = registered.to_vec();
    for deregistered in ["i-2", "i-1"] {
        ok(
            &router,
            "DeregisterInstance",
            json!({ "ServiceId": service_id, "InstanceId": deregistered }),
        )
        .await;
        surviving.retain(|(instance_id, _)| *instance_id != deregistered);

        let holds = published(&router, &zone_id, name).await;
        let wanted: BTreeSet<Observed> = surviving
            .iter()
            .map(|(instance_id, address)| record_of(instance_id, address))
            .collect();
        assert_eq!(
            holds, wanted,
            "case {deregistered:?} deregistered: expected {wanted:#?}, got {holds:#?}"
        );
    }

    // The last instance's record set goes with it, leaving the zone with nothing of this
    // service below its apex.
    ok(
        &router,
        "DeregisterInstance",
        json!({ "ServiceId": service_id, "InstanceId": "i-3" }),
    )
    .await;
    let holds = published(&router, &zone_id, name).await;
    assert!(
        holds.is_empty(),
        "with no instance registered the zone holds no record set of the service, got {holds:#?}"
    );

    // And a service of the same namespace is untouched by its sibling's reconciliations:
    // deregistering from one service never reaches another's records.
    let other = dns_service(
        &router,
        &namespace_id,
        "frontend",
        json!({ "DnsRecords": [{ "Type": "A", "TTL": 15 }] }),
    )
    .await;
    register(
        &router,
        &other,
        "i-front",
        json!({ "AWS_INSTANCE_IPV4": "10.0.9.9" }),
    )
    .await;
    register(
        &router,
        &service_id,
        "i-3",
        json!({ "AWS_INSTANCE_IPV4": "10.0.0.13" }),
    )
    .await;
    let holds = published(&router, &zone_id, name).await;
    let wanted: BTreeSet<Observed> = [
        record_of("i-3", "10.0.0.13"),
        expected(
            "frontend.deregister.example.com.",
            "A",
            Some("i-front"),
            15,
            Some(true),
            None,
            &["10.0.9.9"],
        ),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        holds, wanted,
        "two services of one namespace publish into the same zone side by side, got {holds:#?}"
    );
}

/// Every hosted zone id a namespace reports RESOLVES in Route 53: an id is reported only
/// for a zone that was created, whatever the namespace is named.
///
/// Cloud Map's own `Name` pattern (`^(?!arn:)[!-~]{1,253}$`,
/// `smithy_query(query="shape",shape="NamespaceNamePrivate")`) admits names that are not
/// domain names at all, so the rows differ along the SHAPE of the name: two labels, several
/// labels, one label, and one carrying characters the pattern allows but a host name does
/// not. Whichever of them Route 53 will hold a zone for, the namespace is created, its
/// services are created, and the id it reports — if it reports one — is a zone Route 53
/// answers for.
#[tokio::test]
async fn every_hosted_zone_id_a_namespace_reports_resolves() {
    let router = emulator();

    for (index, name) in [
        "two-labels.example.com",
        "deep.sub.domain.example.com",
        "single",
        "_underscored.internal",
    ]
    .into_iter()
    .enumerate()
    {
        let created = ok(
            &router,
            "CreatePrivateDnsNamespace",
            json!({ "Name": name, "Vpc": VPC }),
        )
        .await;
        let operation_id = created["OperationId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {name:?}: expected an OperationId, got {created}"))
            .to_string();
        let filed = ok(&router, "GetOperation", json!({ "OperationId": operation_id })).await;
        let namespace_id = filed["Operation"]["Targets"]["NAMESPACE"]
            .as_str()
            .unwrap_or_else(|| panic!("case {name:?}: expected a namespace target, got {filed}"))
            .to_string();
        let read = ok(&router, "GetNamespace", json!({ "Id": namespace_id })).await;
        let reported = read["Namespace"]["Properties"]["DnsProperties"]["HostedZoneId"]
            .as_str()
            .map(str::to_string);

        if let Some(zone_id) = reported.as_deref() {
            let resolved = route53(&router).get_hosted_zone().id(zone_id).send().await;
            assert!(
                resolved.is_ok(),
                "case {name:?}: the reported zone {zone_id:?} must resolve in Route 53, got {:?}",
                resolved.err()
            );
        }

        // The namespace is a DNS namespace either way, so a service may register records in
        // it: the zone is the publication target, not the permission to configure one.
        let service = ok(
            &router,
            "CreateService",
            json!({
                "Name": format!("svc-{index}"),
                "NamespaceId": namespace_id,
                "DnsConfig": { "DnsRecords": [{ "Type": "A", "TTL": 60 }] },
            }),
        )
        .await;
        assert_eq!(
            service["Service"]["Type"],
            json!("DNS_HTTP"),
            "case {name:?}: a service with DNS records in a DNS namespace is DNS_HTTP, got {service}"
        );
    }
}

/// `DnsConfig$DnsRecords` is "an array that contains one DnsRecord object for each Route 53
/// record", so one record TYPE names at most one configuration: a create that states the
/// same type twice states two TTLs for one record set and is refused, exactly as
/// `UpdateService` refuses the same change — which is also what keeps what a service
/// publishes to one record set per instance per type.
#[tokio::test]
async fn a_dns_config_may_name_each_record_type_only_once() {
    let router = emulator();
    let (namespace_id, _zone_id) = namespace_with_zone(
        &router,
        "CreatePrivateDnsNamespace",
        json!({ "Name": "one-per-type.example.com", "Vpc": VPC }),
    )
    .await;

    // (case, the DnsRecords stated, accepted)
    let cases: Vec<(&str, Value, bool)> = vec![
        (
            "one record of each of two types",
            json!([{ "Type": "A", "TTL": 60 }, { "Type": "SRV", "TTL": 60 }]),
            true,
        ),
        (
            "the same type twice",
            json!([{ "Type": "A", "TTL": 60 }, { "Type": "A", "TTL": 30 }]),
            false,
        ),
        (
            "a repeated type among others",
            json!([{ "Type": "A", "TTL": 60 }, { "Type": "SRV", "TTL": 60 },
                   { "Type": "A", "TTL": 60 }]),
            false,
        ),
        (
            "the same type three times",
            json!([{ "Type": "AAAA", "TTL": 1 }, { "Type": "AAAA", "TTL": 2 },
                   { "Type": "AAAA", "TTL": 3 }]),
            false,
        ),
    ];

    for (index, (case, dns_records, accepted)) in cases.into_iter().enumerate() {
        let (status, json) = call(
            &router,
            "CreateService",
            json!({
                "Name": format!("one-per-type-svc-{index}"),
                "NamespaceId": namespace_id,
                "DnsConfig": { "DnsRecords": dns_records },
            }),
        )
        .await;
        assert_eq!(
            status,
            if accepted { 200 } else { 400 },
            "case {case:?}: expected {}, got {status} with {json}",
            if accepted { "200" } else { "400" }
        );
        if !accepted {
            assert_eq!(
                json["__type"].as_str(),
                Some("InvalidInput"),
                "case {case:?}: expected InvalidInput, got {json}"
            );
            // Nothing is stored for a refused create: the name is still free.
            let listed = ok(
                &router,
                "ListServices",
                json!({ "Filters": [{ "Name": "NAMESPACE_ID", "Values": [namespace_id] }] }),
            )
            .await;
            let names: Vec<&str> = listed["Services"]
                .as_array()
                .map(|services| {
                    services
                        .iter()
                        .filter_map(|service| service["Name"].as_str())
                        .collect()
                })
                .unwrap_or_default();
            assert!(
                !names.contains(&format!("one-per-type-svc-{index}").as_str()),
                "case {case:?}: a refused create must store no service, got {names:?}"
            );
        }
    }
}

/// A zone a CLIENT creates for itself carries NO linked service: `HostedZone$LinkedService`
/// reports the other service that created a zone, so it is reported for a zone another
/// service created and for no other.
#[tokio::test]
async fn a_zone_a_client_creates_for_itself_has_no_linked_service() {
    let router = emulator();
    let created = route53(&router)
        .create_hosted_zone()
        .name("client-owned.example.com")
        .caller_reference("client-owned-1")
        .send()
        .await
        .expect("a client may create a hosted zone of its own");
    let zone = created
        .hosted_zone()
        .expect("CreateHostedZone answers with the hosted zone");
    assert!(
        zone.linked_service().is_none(),
        "a zone no other service created reports no linked service, got {zone:?}"
    );
}

/// An HTTP namespace has no hosted zone, so nothing is published for it — and a service in
/// it is still refused a `DnsConfig`.
#[tokio::test]
async fn an_http_namespace_publishes_no_zone() {
    let router = emulator();
    let created = ok(
        &router,
        "CreateHttpNamespace",
        json!({ "Name": "no-zone-http" }),
    )
    .await;
    let operation_id = created["OperationId"]
        .as_str()
        .expect("CreateHttpNamespace returns an OperationId")
        .to_string();
    let filed = ok(&router, "GetOperation", json!({ "OperationId": operation_id })).await;
    let namespace_id = filed["Operation"]["Targets"]["NAMESPACE"]
        .as_str()
        .expect("the CREATE_NAMESPACE operation targets a namespace")
        .to_string();

    let read = ok(&router, "GetNamespace", json!({ "Id": namespace_id })).await;
    assert!(
        read["Namespace"]["Properties"]["DnsProperties"]["HostedZoneId"].is_null(),
        "an HTTP namespace reports no hosted zone, got {read}"
    );
    let (status, json) = call(
        &router,
        "CreateService",
        json!({
            "Name": "backend",
            "NamespaceId": namespace_id,
            "DnsConfig": { "DnsRecords": [{ "Type": "A", "TTL": 60 }] },
        }),
    )
    .await;
    assert_eq!(
        status, 400,
        "a DnsConfig in an HTTP namespace is refused, got {status}: {json}"
    );
}
