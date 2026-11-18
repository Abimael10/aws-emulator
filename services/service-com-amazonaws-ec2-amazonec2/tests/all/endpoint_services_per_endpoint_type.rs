// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for what ONE row of `DescribeVpcEndpointServices` is, driven over the wire
//! the service speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST whose body
//! carries `Action=DescribeVpcEndpointServices`, answered with XML (README.md / OPERATIONS.md
//! "Wire protocol").
//!
//! The rule under test: an endpoint service is one (service name, endpoint type) pair, so the
//! report carries one `serviceDetail` per pair — a label reachable both through a gateway and
//! through an interface endpoint publishes TWO endpoint services under the one name — and
//! everything that follows from the type is the row's own: its `serviceId`, its single
//! `serviceType` entry, the `privateDnsName` only an interface endpoint's private hosted zone
//! answers on, and the `supportedIpAddressTypeSet` the way that row is reached. `serviceName`
//! and `baseEndpointDnsNameSet` are facts of the service, so the rows published under one name
//! agree on them, and `serviceNameSet` reports the name once.
//!
//! Each row of the table below is a distinct input along what the rule generalises over: how
//! many endpoint types the label is reachable through (one, or both), which type that is
//! (gateway-only, interface-only), which published name family the service belongs to
//! (`com.amazonaws`, `aws.api`, `api.aws`), and which Region the call asks about. The
//! `service-type` filter, the id shape, the address types (cross-checked against the Region's
//! own published prefix lists) and a `MaxResults` walk that cuts BETWEEN two rows of one name
//! are each driven over the same table.

use std::collections::BTreeSet;

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{elements, filters, indexed};
use super::wire::{child, child_text, document, param, set_items, Param};

/// The action every call below drives.
const ACTION: &str = "DescribeVpcEndpointServices";

/// The Regions the table is driven in: two Regions of different partitions' naming, so nothing
/// about the report is tied to one Region.
const REGIONS: [&str; 2] = ["us-west-2", "eu-central-1"];

/// The PrivateLink endpoint domain this emulator publishes its endpoint DNS names in: the
/// `vpce` label in the zone a resolver answers with the loopback interface, which is where the
/// core binds its listeners unless the operator names another address. A base endpoint DNS name
/// is a name a consumer resolves and then dials, so it names this process rather than a host in
/// a zone Amazon Web Services serves.
const ENDPOINT_DOMAIN: &str = "vpce.localhost";

/// The `ServiceType` member an interface endpoint service reports.
const INTERFACE: &str = "Interface";

/// The `ServiceType` member a gateway endpoint service reports.
const GATEWAY: &str = "Gateway";

/// One row of the table: a service of the Region's published catalogue, the endpoint types it
/// is reachable through — one endpoint service each, in the order the report carries them —
/// and whether it publishes a default public DNS name.
struct Row {
    case: &'static str,
    /// The service's name without its Region, as `{prefix}{region}.{label}` is assembled.
    prefix: &'static str,
    /// The service's label under the Region.
    label: &'static str,
    /// The HOST form of that label — its dotted components in reverse order, since an endpoint
    /// service name is the reverse-domain spelling of the host the service answers on. Written
    /// out here, independently of the code that derives it.
    dns_host: &'static str,
    /// The endpoint types published under the name, in report order.
    types: &'static [&'static str],
    /// Whether the service publishes a default public DNS name, which only its INTERFACE
    /// endpoint service reports.
    private_dns: bool,
}

/// The rows: a label reachable BOTH ways (two endpoint services under one name), a gateway-only
/// label, a plain interface label, an interface label whose own label is dotted, an interface
/// service that publishes no default public DNS name, and one service from each of the two
/// other published name families.
const ROWS: &[Row] = &[
    Row {
        case: "a label reachable both ways",
        prefix: "com.amazonaws.",
        label: "s3",
        dns_host: "s3",
        types: &[GATEWAY, INTERFACE],
        private_dns: true,
    },
    Row {
        case: "a gateway-only label",
        prefix: "com.amazonaws.",
        label: "dynamodb",
        dns_host: "dynamodb",
        types: &[GATEWAY],
        private_dns: false,
    },
    Row {
        case: "a plain interface label",
        prefix: "com.amazonaws.",
        label: "sqs",
        dns_host: "sqs",
        types: &[INTERFACE],
        private_dns: true,
    },
    Row {
        case: "an interface label of two components",
        prefix: "com.amazonaws.",
        label: "sagemaker.runtime",
        dns_host: "runtime.sagemaker",
        types: &[INTERFACE],
        private_dns: true,
    },
    Row {
        case: "an interface service publishing no default public name",
        prefix: "com.amazonaws.",
        label: "iot.data",
        dns_host: "data.iot",
        types: &[INTERFACE],
        private_dns: false,
    },
    Row {
        case: "a service of the aws.api family",
        prefix: "aws.api.",
        label: "emr-service-cell01",
        dns_host: "emr-service-cell01",
        types: &[INTERFACE],
        private_dns: true,
    },
    Row {
        case: "a service of the api.aws family",
        prefix: "api.aws.",
        label: "cassandra-streams",
        dns_host: "cassandra-streams",
        types: &[INTERFACE],
        private_dns: true,
    },
];

/// The name row `row` is published under in `region`.
fn service_name(row: &Row, region: &str) -> String {
    format!("{prefix}{region}.{label}", prefix = row.prefix, label = row.label)
}

/// One `DescribeVpcEndpointServices` call signed for `region` that has to succeed, and the body
/// it answered.
async fn ok(case: &str, router: &MultiServiceRouter, region: &str, params: &[Param]) -> String {
    let (status, body) = super::ec2_query::call(router, region, ACTION, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with body {body}"
    );
    body
}

/// One reported endpoint service: the members a row of this suite is about.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reported {
    name: String,
    id: String,
    types: Vec<String>,
    base_dns_names: Vec<String>,
    private_dns_name: Option<String>,
    ip_address_types: Vec<String>,
}

/// Every endpoint service a body reports, in the order it reported them.
fn reported(case: &str, body: &str) -> Vec<Reported> {
    let parsed = document(case, body);
    let root = parsed.root_element();
    let Some(set) = child(root, "serviceDetailSet") else {
        return Vec::new();
    };
    set.children()
        .filter(|node| node.is_element() && node.tag_name().name() == "item")
        .map(|item| Reported {
            name: child_text(item, "serviceName").unwrap_or_default(),
            id: child_text(item, "serviceId").unwrap_or_default(),
            types: set_items(item, "serviceType")
                .into_iter()
                .filter_map(|entry| child_text(entry, "serviceType"))
                .collect(),
            base_dns_names: text_items(item, "baseEndpointDnsNameSet"),
            private_dns_name: child_text(item, "privateDnsName"),
            ip_address_types: text_items(item, "supportedIpAddressTypeSet"),
        })
        .collect()
}

/// The text of every `<item>` of the `set` child of `node`.
fn text_items(node: roxmltree::Node<'_, '_>, set: &str) -> Vec<String> {
    set_items(node, set)
        .into_iter()
        .map(|item| item.text().unwrap_or_default().to_string())
        .collect()
}

/// The names `serviceNameSet` carries.
fn reported_names(case: &str, body: &str) -> Vec<String> {
    let parsed = document(case, body);
    child(parsed.root_element(), "serviceNameSet")
        .map(text_items_of)
        .unwrap_or_default()
}

/// The text of every `<item>` of the set element `set`.
fn text_items_of(set: roxmltree::Node<'_, '_>) -> Vec<String> {
    set.children()
        .filter(|item| item.is_element() && item.tag_name().name() == "item")
        .map(|item| item.text().unwrap_or_default().to_string())
        .collect()
}

/// Whether `value` is an endpoint service id of the shape AWS publishes: `vpce-svc-` followed
/// by 17 lowercase hexadecimal digits.
fn is_endpoint_service_id(value: &str) -> bool {
    value.strip_prefix("vpce-svc-").is_some_and(|suffix| {
        suffix.len() == 17
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

/// The address families a GATEWAY endpoint service of `label` answers on: IPv4, and IPv6 when
/// the Region publishes that service's IPv6 prefix list beside its IPv4 one — the lists a route
/// to a gateway endpoint names. Read through `DescribeManagedPrefixLists`, so the expectation
/// comes from the Region's own published lists rather than from this suite.
async fn gateway_address_types(
    router: &MultiServiceRouter,
    region: &str,
    label: &str,
) -> Vec<String> {
    let (status, body) =
        super::ec2_query::call(router, region, "DescribeManagedPrefixLists", &[]).await;
    assert_eq!(
        status, 200,
        "DescribeManagedPrefixLists for {region} must succeed, got {status}: {body}"
    );
    let published = elements(&body, "prefixListName");
    let mut types = vec!["ipv4".to_string()];
    if published.contains(&format!("com.amazonaws.{region}.ipv6.{label}")) {
        types.push("ipv6".to_string());
    }
    types
}

/// The address families the endpoint service of `row` reached through `endpoint_type` answers
/// on: an interface endpoint service answers on both families, the interface endpoint's own
/// addresses being what an endpoint of either family is created with, while a gateway endpoint
/// service answers on the families the Region publishes a prefix list for it in.
async fn expected_address_types(
    router: &MultiServiceRouter,
    region: &str,
    row: &Row,
    endpoint_type: &str,
) -> Vec<String> {
    if endpoint_type == INTERFACE {
        return vec!["ipv4".to_string(), "ipv6".to_string()];
    }
    gateway_address_types(router, region, row.label).await
}

/// Every row, in every Region: the name is reported once and carries one `serviceDetail` per
/// endpoint type, each with its own id, its own single type, its own private DNS name and the
/// one base endpoint DNS name the service publishes.
#[tokio::test]
async fn one_row_per_endpoint_service_published_under_a_name() {
    let router = super::ec2_query::emulator().0;

    for region in REGIONS {
        for row in ROWS {
            let name = service_name(row, region);
            let case = format!("{} ({name} in {region})", row.case);
            let body = ok(&case, &router, region, &indexed("ServiceName", &[&name])).await;
            let services = reported(&case, &body);
            assert_eq!(
                services.len(),
                row.types.len(),
                "case {case:?}: expected one endpoint service per type {:?}, got {services:?}",
                row.types
            );
            assert_eq!(
                reported_names(&case, &body),
                vec![name.clone()],
                "case {case:?}: serviceNameSet reports the name once however many endpoint \
                 services carry it, got {body}"
            );
            let ids: BTreeSet<&String> = services.iter().map(|service| &service.id).collect();
            assert_eq!(
                ids.len(),
                services.len(),
                "case {case:?}: every endpoint service carries an id of its own, got \
                 {services:?}"
            );
            for (service, endpoint_type) in services.iter().zip(row.types) {
                let case = format!("{case} ({endpoint_type})");
                assert_eq!(
                    service.name, name,
                    "case {case:?}: every row names the service it belongs to, got {:?}",
                    service.name
                );
                assert_eq!(
                    service.types,
                    vec![(*endpoint_type).to_string()],
                    "case {case:?}: a row reports the one type it is reached through, got {:?}",
                    service.types
                );
                assert!(
                    is_endpoint_service_id(&service.id),
                    "case {case:?}: expected a vpce-svc- id of 17 hexadecimal digits, got {:?}",
                    service.id
                );
                let expected_dns = vec![format!("{}.{region}.{ENDPOINT_DOMAIN}", row.dns_host)];
                assert_eq!(
                    service.base_dns_names, expected_dns,
                    "case {case:?}: expected the base endpoint DNS name {expected_dns:?}, got \
                     {:?}",
                    service.base_dns_names
                );
                let expected_private = (*endpoint_type == INTERFACE && row.private_dns)
                    .then(|| format!("{}.{region}.", row.dns_host));
                assert_eq!(
                    service.private_dns_name.is_some(),
                    expected_private.is_some(),
                    "case {case:?}: only the interface endpoint service of a service that \
                     publishes a default public name reports one, got {:?}",
                    service.private_dns_name
                );
                if let (Some(reported), Some(prefix)) =
                    (service.private_dns_name.as_deref(), expected_private.as_deref())
                {
                    assert!(
                        reported.starts_with(prefix),
                        "case {case:?}: expected the service's own default public name under \
                         {prefix:?}, got {reported:?}"
                    );
                }
                let expected_types =
                    expected_address_types(&router, region, row, endpoint_type).await;
                assert_eq!(
                    service.ip_address_types, expected_types,
                    "case {case:?}: expected the address types {expected_types:?}, got {:?}",
                    service.ip_address_types
                );
            }
        }
    }
}

/// The `service-type` filter selects ENDPOINT SERVICES: asking a name for one of its types
/// reports exactly that row, and asking it for a type it is not reachable through reports none.
#[tokio::test]
async fn the_service_type_filter_selects_one_endpoint_service_of_a_name() {
    let router = super::ec2_query::emulator().0;
    let region = REGIONS[0];

    // Every type of the `ServiceType` enum, so each row is asked about the types it offers and
    // the ones it does not.
    const EVERY_TYPE: [&str; 3] = [GATEWAY, INTERFACE, "GatewayLoadBalancer"];

    for row in ROWS {
        let name = service_name(row, region);
        for endpoint_type in EVERY_TYPE {
            let case = format!("{} as {endpoint_type} ({name})", row.case);
            let mut params = indexed("ServiceName", &[&name]);
            params.extend(filters(&[("service-type", &[endpoint_type])]));
            let body = ok(&case, &router, region, &params).await;
            let services = reported(&case, &body);
            let offered = row.types.contains(&endpoint_type);
            assert_eq!(
                services.len(),
                usize::from(offered),
                "case {case:?}: expected {} row(s) for a type the service {} offer, got \
                 {services:?}",
                usize::from(offered),
                if offered { "does" } else { "does not" }
            );
            if let Some(service) = services.first() {
                assert_eq!(
                    service.types,
                    vec![endpoint_type.to_string()],
                    "case {case:?}: the selected row is the one of that type, got {:?}",
                    service.types
                );
            }
            assert_eq!(
                reported_names(&case, &body),
                if offered { vec![name.clone()] } else { Vec::new() },
                "case {case:?}: serviceNameSet reports the same selection, got {body}"
            );
        }
    }
}

/// A `MaxResults` walk reports every endpoint service of the selection exactly once, in one
/// stable order, cutting BETWEEN the two rows one name carries — the cursor names an endpoint
/// service, not a name — and the last page carries no token.
#[tokio::test]
async fn a_page_walk_cuts_between_two_endpoint_services_of_one_name() {
    let router = super::ec2_query::emulator().0;
    let region = REGIONS[0];
    // A selection that holds a name with two endpoint services beside names with one, so a
    // page boundary must be able to fall inside one name.
    let selection = filters(&[("service-name", &["*.s3", "*.dynamodb"])]);

    let whole = reported(
        "the whole selection",
        &ok("the whole selection", &router, region, &selection).await,
    );
    assert!(
        whole.len() > 2,
        "case \"the whole selection\": expected several endpoint services, got {whole:?}"
    );

    // Pages of one — which is what forces a cut inside the name carrying two rows — and a page
    // size that divides the selection unevenly.
    for page_size in [1usize, 2] {
        let case = format!("pages of {page_size}");
        let mut walked: Vec<Reported> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0usize;
        loop {
            let mut params = selection.clone();
            params.push(param("MaxResults", &page_size.to_string()));
            if let Some(token) = token.as_deref() {
                params.push(param("NextToken", token));
            }
            let body = ok(&case, &router, region, &params).await;
            let page = reported(&case, &body);
            assert!(
                page.len() <= page_size,
                "case {case:?}: a page holds at most {page_size} endpoint services, got \
                 {page:?}"
            );
            walked.extend(page);
            pages += 1;
            assert!(
                pages <= whole.len() + 1,
                "case {case:?}: the walk must terminate, got {pages} pages"
            );
            let parsed = document(&case, &body);
            match child_text(parsed.root_element(), "nextToken").filter(|next| !next.is_empty()) {
                None => break,
                Some(next) => token = Some(next),
            }
        }
        assert_eq!(
            walked, whole,
            "case {case:?}: the walk must report the selection exactly once, in its order"
        );
    }
}

/// Two endpoint services published under ONE name are two services: different ids, the same
/// name, and the same base endpoint DNS name the service publishes.
#[tokio::test]
async fn the_two_endpoint_services_of_one_name_differ_only_in_what_the_type_decides() {
    let router = super::ec2_query::emulator().0;
    let region = REGIONS[0];
    let both_ways = ROWS
        .iter()
        .find(|row| row.types.len() > 1)
        .expect("the table must carry a label reachable through more than one endpoint type");
    let name = service_name(both_ways, region);
    let case = format!("the two endpoint services of {name}");

    let services = reported(
        &case,
        &ok(&case, &router, region, &indexed("ServiceName", &[&name])).await,
    );
    let [gateway, interface] = services.as_slice() else {
        panic!("case {case:?}: expected exactly two endpoint services, got {services:?}");
    };
    assert_eq!(
        (gateway.types.as_slice(), interface.types.as_slice()),
        ([GATEWAY.to_string()].as_slice(), [INTERFACE.to_string()].as_slice()),
        "case {case:?}: expected the gateway service then the interface one, got {services:?}"
    );
    assert_ne!(
        gateway.id, interface.id,
        "case {case:?}: two endpoint services of one name are two services, got the one id \
         {:?}",
        gateway.id
    );
    assert_eq!(
        gateway.name, interface.name,
        "case {case:?}: both are published under the one service name, got {:?} and {:?}",
        gateway.name, interface.name
    );
    assert_eq!(
        gateway.base_dns_names, interface.base_dns_names,
        "case {case:?}: the base endpoint DNS name is a fact of the service, got {:?} and {:?}",
        gateway.base_dns_names, interface.base_dns_names
    );
    assert!(
        gateway.private_dns_name.is_none() && interface.private_dns_name.is_some(),
        "case {case:?}: a private hosted zone is an interface endpoint's option, got {:?} and \
         {:?}",
        gateway.private_dns_name,
        interface.private_dns_name
    );

    // And a second identical call reports the same two services: a describe is a read, so
    // neither the ids nor the order move.
    let again = reported(
        &case,
        &ok(&case, &router, region, &indexed("ServiceName", &[&name])).await,
    );
    assert_eq!(
        again, services,
        "case {case:?}: a second call must report the same endpoint services"
    );
}

/// No two endpoint services of one Region share an id, across the whole catalogue — the id is
/// derived from the name AND the endpoint type, so the two rows of one name differ too.
#[tokio::test]
async fn every_endpoint_service_of_a_region_carries_its_own_id() {
    let router = super::ec2_query::emulator().0;

    for region in REGIONS {
        let case = format!("the whole catalogue of {region}");
        let mut services: Vec<Reported> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut params: Vec<Param> = vec![param("MaxResults", "100")];
            if let Some(token) = token.as_deref() {
                params.push(param("NextToken", token));
            }
            let body = ok(&case, &router, region, &params).await;
            services.extend(reported(&case, &body));
            let parsed = document(&case, &body);
            match child_text(parsed.root_element(), "nextToken").filter(|next| !next.is_empty()) {
                None => break,
                Some(next) => token = Some(next),
            }
        }
        assert!(
            services.len() > ROWS.len(),
            "case {case:?}: expected the Region's whole catalogue, got {} rows",
            services.len()
        );
        let ids: BTreeSet<&String> = services.iter().map(|service| &service.id).collect();
        assert_eq!(
            ids.len(),
            services.len(),
            "case {case:?}: two endpoint services of one Region must not share an id, got {} \
             ids for {} rows",
            ids.len(),
            services.len()
        );
        for service in &services {
            assert!(
                is_endpoint_service_id(&service.id),
                "case {case:?}: expected a vpce-svc- id of 17 hexadecimal digits, got {:?}",
                service.id
            );
            assert_eq!(
                service.types.len(),
                1,
                "case {case:?}: a row reports the one type it is reached through, got {service:?}"
            );
        }
    }
}
