// SPDX-License-Identifier: CC0-1.0

//! Which endpoint services a Region's catalogue holds, driven over the wire this service
//! speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST answered with XML
//! (README.md / OPERATIONS.md "Wire protocol").
//!
//! THE RULE: every Amazon Web Services service that publishes an interface endpoint service
//! is in each Region's catalogue under `com.amazonaws.<region>.<label>`, so
//! `DescribeVpcEndpointServices` answers it with a `ServiceDetail` whose name echoes the
//! request, whose type is `Interface` and whose Availability Zones are the Region's own, and
//! `CreateVpcEndpoint` creates an interface endpoint to it. `InvalidServiceName` is reserved
//! for a label no service publishes — so a request listing several published names resolves
//! all of them, and one mixing a published name with a label nothing publishes is refused
//! whole, naming the value at fault.
//!
//! The rows span what the rule generalises over: which service's label, whether the label is
//! plain or dotted, which Region the call asks about (and so how many zones the service is
//! reported in), one name per request against several at once, and the read path against the
//! create path. Each label's source is the Amazon Web Services PrivateLink User Guide's list
//! of the services that integrate with PrivateLink, the same source the Region's catalogue is
//! built from.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, emulator, error_code, error_message, indexed};
use super::published_endpoint_service_zones_and_private_dns::zones_of_region;
use super::wire::{child, child_text, document, param, set_items, Param};

/// The action the read path below drives.
const ACTION: &str = "DescribeVpcEndpointServices";

/// One row: the Region to ask in, and the label of a service Amazon Web Services publishes an
/// interface endpoint service for.
type Row = (&'static str, &'static str);

/// Published labels the catalogue has to hold, each asked for in a Region of its own so the
/// rows differ in the number of zones the service is reported in too. `imagebuilder` is EC2
/// Image Builder's; `codestar-connections.api`, `iotsitewise.data` and
/// `sagemaker.featurestore-runtime` are dotted labels; `elasticbeanstalk-health`, `kms-fips`
/// and `transcribestreaming` sit beside a plainer label of the same service; `schemas` and
/// `servicequotas` are labels the issue did not cite.
const ROWS: &[Row] = &[
    ("us-east-1", "imagebuilder"),
    ("eu-west-1", "imagebuilder"),
    ("ap-northeast-1", "imagebuilder"),
    ("us-west-2", "kms-fips"),
    ("us-east-1", "elasticbeanstalk-health"),
    ("ap-northeast-1", "codestar-connections.api"),
    ("us-west-2", "iotsitewise.data"),
    ("eu-west-1", "sagemaker.featurestore-runtime"),
    ("us-east-1", "schemas"),
    ("eu-west-1", "servicequotas"),
    ("us-west-2", "transcribestreaming"),
];

/// The services a build network with no route to the internet reaches EC2 Image Builder and
/// its build instance's agents through: the names the one request below lists together.
const PRIVATE_BUILD_NETWORK_LABELS: &[&str] =
    &["imagebuilder", "ssm", "ssmmessages", "ec2messages", "logs"];

/// The endpoint service name of `label` in `region`, as Amazon Web Services spells it.
fn service_name(region: &str, label: &str) -> String {
    format!("com.amazonaws.{region}.{label}")
}

/// One call signed for `region` that has to succeed, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, region: &str, params: &[Param]) -> String {
    let (status, body) = call(router, region, ACTION, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {ACTION}, got {status} with body {body}"
    );
    body
}

/// The text of every `<item>` of the `set` child of the response envelope.
fn envelope_items(case: &str, body: &str, set: &str) -> Vec<String> {
    let parsed = document(case, body);
    set_items(parsed.root_element(), set)
        .into_iter()
        .map(|item| item.text().unwrap_or_default().to_string())
        .collect()
}

/// Every reported `serviceDetailSet` item of a body, as `(serviceName, serviceTypes, zones)`.
fn reported_details(case: &str, body: &str) -> Vec<(String, Vec<String>, Vec<String>)> {
    let parsed = document(case, body);
    let root = parsed.root_element();
    let Some(set) = child(root, "serviceDetailSet") else {
        return Vec::new();
    };
    set.children()
        .filter(|node| node.is_element() && node.tag_name().name() == "item")
        .map(|item| {
            let types = set_items(item, "serviceType")
                .into_iter()
                .filter_map(|entry| child_text(entry, "serviceType"))
                .collect();
            let zones = set_items(item, "availabilityZoneSet")
                .into_iter()
                .map(|zone| zone.text().unwrap_or_default().to_string())
                .collect();
            (
                child_text(item, "serviceName").unwrap_or_default(),
                types,
                zones,
            )
        })
        .collect()
}

/// The zone NAMES `DescribeAvailabilityZones` reports for `region`, which an endpoint service
/// of that Region's catalogue is available in — read through the one reader of that report this
/// crate's suites share (`super::published_endpoint_service_zones_and_private_dns`), which
/// answers `(name, id)` pairs, so no suite holds a second reading of it.
async fn zone_names_of_region(
    case: &str,
    router: &MultiServiceRouter,
    region: &str,
) -> Vec<String> {
    zones_of_region(case, router, region)
        .await
        .into_iter()
        .map(|(zone_name, _zone_id)| zone_name)
        .collect()
}

/// Each published label is in its Region's catalogue, reported as an interface service of
/// that Region available in the Region's zones.
#[tokio::test]
async fn a_published_service_is_in_the_regions_endpoint_service_catalogue() {
    let router = emulator().0;

    for (region, label) in ROWS {
        let name = service_name(region, label);
        let case = format!("{name} described in {region}");
        let body = ok(&case, &router, region, &indexed("ServiceName", &[&name])).await;

        let details = reported_details(&case, &body);
        assert_eq!(
            details.len(),
            1,
            "case {case:?}: expected one reported endpoint service, got {details:?} from {body}"
        );
        let (reported_name, types, zones) = &details[0];
        assert_eq!(
            reported_name, &name,
            "case {case:?}: the report echoes the requested name, got {reported_name:?}"
        );
        assert_eq!(
            types,
            &vec!["Interface".to_string()],
            "case {case:?}: expected an Interface endpoint service, got {types:?}"
        );
        let expected_zones = zone_names_of_region(&case, &router, region).await;
        assert!(
            !expected_zones.is_empty(),
            "case {case:?}: the Region must have zones, got {expected_zones:?}"
        );
        assert_eq!(
            zones, &expected_zones,
            "case {case:?}: expected the Region's zones {expected_zones:?}, got {zones:?}"
        );
        // `ServiceNames` and `ServiceDetails` report the same selection.
        assert_eq!(
            envelope_items(&case, &body, "serviceNameSet"),
            vec![name.clone()],
            "case {case:?}: the reported names are the reported services, got {body}"
        );
    }
}

/// A request listing several published names resolves every one of them, rather than losing
/// the lot to one name the catalogue happens not to hold.
#[tokio::test]
async fn a_request_naming_several_published_services_resolves_all_of_them() {
    let router = emulator().0;

    for region in ["us-east-1", "eu-west-1"] {
        let names: Vec<String> = PRIVATE_BUILD_NETWORK_LABELS
            .iter()
            .map(|label| service_name(region, label))
            .collect();
        let case = format!("the private build network's services in {region}");
        let requested: Vec<&str> = names.iter().map(String::as_str).collect();
        let body = ok(&case, &router, region, &indexed("ServiceName", &requested)).await;

        let details = reported_details(&case, &body);
        let reported: Vec<String> = details
            .iter()
            .map(|(name, _, _)| name.clone())
            .collect();
        for name in &names {
            assert!(
                reported.contains(name),
                "case {case:?}: expected {name} among the reported services, got {reported:?}"
            );
        }
        // Every reported service carries the zones that make the request answerable: the
        // check that picks a zone offering all of them intersects these lists.
        for (name, _, zones) in &details {
            assert!(
                !zones.is_empty(),
                "case {case:?}: {name} must report the zones it is available in, got {body}"
            );
        }
    }
}

/// An interface endpoint is created to a published service of the catalogue, and the stored
/// endpoint reads back as one to that service.
#[tokio::test]
async fn an_interface_endpoint_is_created_to_a_published_service() {
    let router = emulator().0;
    let region = "us-east-1";

    let vpc_id = {
        let (status, body) = call(
            &router,
            region,
            "CreateVpc",
            &[param("CidrBlock", "10.60.0.0/16")],
        )
        .await;
        assert_eq!(status, 200, "the network must be created, got {body}");
        super::ec2_query::element(&body, "vpcId").expect("a created VPC reports its id")
    };
    let subnet_id = {
        let (status, body) = call(
            &router,
            region,
            "CreateSubnet",
            &[
                param("VpcId", &vpc_id),
                param("CidrBlock", "10.60.1.0/24"),
                param("AvailabilityZone", "us-east-1a"),
            ],
        )
        .await;
        assert_eq!(status, 200, "the subnet must be created, got {body}");
        super::ec2_query::element(&body, "subnetId").expect("a created subnet reports its id")
    };

    for label in ["imagebuilder", "schemas"] {
        let name = service_name(region, label);
        let case = format!("an interface endpoint to {name}");
        let (status, body) = call(
            &router,
            region,
            "CreateVpcEndpoint",
            &[
                param("VpcId", &vpc_id),
                param("ServiceName", &name),
                param("VpcEndpointType", "Interface"),
                param("SubnetId.1", &subnet_id),
                param("PrivateDnsEnabled", "false"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with body {body}"
        );
        let endpoint_id = super::ec2_query::element(&body, "vpcEndpointId")
            .unwrap_or_else(|| panic!("case {case:?}: no endpoint id in {body}"));

        let (status, described) = call(
            &router,
            region,
            "DescribeVpcEndpoints",
            &[param("VpcEndpointId.1", &endpoint_id)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the endpoint must read back, got {status} with body {described}"
        );
        assert_eq!(
            super::ec2_query::element(&described, "serviceName").as_deref(),
            Some(name.as_str()),
            "case {case:?}: the stored endpoint names the service it was created to, got \
             {described}"
        );
    }
}

/// A label no service publishes is still refused, with the code and the sentence EC2 words
/// that refusal in — and a request mixing one with published names is refused whole.
#[tokio::test]
async fn a_label_no_service_publishes_is_refused() {
    let router = emulator().0;
    let region = "us-east-1";

    let unpublished = service_name(region, "not-a-published-service");
    let published = service_name(region, "imagebuilder");
    for requested in [
        vec![unpublished.as_str()],
        vec![published.as_str(), unpublished.as_str()],
        vec![unpublished.as_str(), published.as_str()],
    ] {
        let case = format!("{requested:?}");
        let (status, body) =
            call(&router, region, ACTION, &indexed("ServiceName", &requested)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidServiceName",
            "case {case:?}: expected InvalidServiceName, got {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("The Vpc Endpoint Service '{unpublished}' does not exist"),
            "case {case:?}: the refusal names the value at fault, got {body}"
        );
    }
}
