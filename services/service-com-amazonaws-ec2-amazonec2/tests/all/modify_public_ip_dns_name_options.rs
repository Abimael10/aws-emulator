// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `ModifyPublicIpDnsNameOptions`, driven over the wire the service
//! actually speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST whose body
//! carries `Action=…`, answered with XML (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test is that THE PUBLIC HOSTNAME TYPE OF A NETWORK INTERFACE IS WHATEVER
//! THE LAST SUCCESSFUL CALL CHOSE FOR THAT INTERFACE, OUT OF THE MODELLED `PublicIpDnsOption`
//! CATALOG, AND CHOOSING ONE LEAVES THE ADDRESSES THE INTERFACE HOLDS AND THE HOSTNAMES
//! NAMING THEM UNTOUCHED: every report of the interface — its own shape through
//! `DescribeNetworkInterfaces`, and the projection of it the instance holding it carries —
//! then reports that type as `publicIpDnsNameOptions/dnsHostnameType`, beside the public
//! hostname of the address the interface holds.
//!
//! The rows span what that rule generalises over: every member of the model's own catalog, in
//! an order that makes each call REPLACE the type the call before it chose and that comes back
//! to a type already chosen; interfaces that differ in whether they hold a public address at
//! all; and interfaces that came to exist in the two ways this service mints one — a launch's
//! primary interface and a `CreateNetworkInterface`. The refusals are a table too: an id that
//! is no interface id, a well-formed id (long and short form) the account holds no interface
//! for, a hostname type outside the catalog, and the `DryRun` probe — each asserted to leave
//! the interface's chosen type exactly as it was.
//!
//! This operation answers ONE refusal for an interface id it cannot resolve, whatever form the
//! value is in (`InvalidNetworkInterfaceId.Malformed`), rather than the malformed/not-found
//! pair the readers of one named interface split an unresolvable id into — which refusal an
//! operation answers is a fact of that operation.

use ec2::state::ComAmazonawsEc2Amazonec2State as Ec2State;
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, error_code, error_message, filters};
use super::wire::{Param, child, child_text, document, param};

/// The Region every call here is signed for: the default VPC and its per-zone subnets are
/// seeded for the emulator's default Region, and the fixtures below live in them.
const REGION: &str = "us-east-1";

/// An AMI id of the right FORM: which image the fixture launch names is not the subject here.
const AMI: &str = "ami-0123456789abcdef0";

/// The members of `PublicIpDnsOption` written out as the model spells them
/// (`smithy_query(query="shape",shape="PublicIpDnsOption")` reports
/// `public-dual-stack-dns-name | public-ipv4-dns-name | public-ipv6-dns-name`), so the table
/// below names the catalog rather than echoing the crate's own copy of it.
const MODELLED_HOSTNAME_TYPES: [&str; 3] = [
    "public-dual-stack-dns-name",
    "public-ipv4-dns-name",
    "public-ipv6-dns-name",
];

/// The type an interface nobody has chosen one for answers to: the IPv4 one, the only kind of
/// public hostname anything in this store holds.
const DEFAULT_HOSTNAME_TYPE: &str = "public-ipv4-dns-name";

/// A well-formed network interface id, in the long form, that no interface of the account has.
const ABSENT_INTERFACE: &str = "eni-0000000000000beef";

/// A well-formed interface id in the SHORT form (8 hex digits), which no interface of the
/// account has either — the id form is one of the axes the refusal table drives.
const ABSENT_SHORT_INTERFACE: &str = "eni-0badf00d";

/// Another well-formed long id no interface of the account has: the all-zero one, which is a
/// well-formed id for an interface.
const ABSENT_ZERO_INTERFACE: &str = "eni-00000000000000000";

/// A value that is not a network interface id at all.
const MALFORMED_INTERFACE: &str = "eni-not-an-id";

/// Another resource kind's id, which is not an interface id either.
const SUBNET_SHAPED_ID: &str = "subnet-0123456789abcdef0";

/// The code the centralized request walk answers for an input that breaks a MODELLED
/// constraint — here a `HostnameType` outside the `PublicIpDnsOption` enum, refused before any
/// handler runs, so it carries the boundary's own code rather than one a handler chose.
const BOUNDARY_CONSTRAINT_CODE: &str = "ValidationError";

/// The catalog the table drives has to be the model's own: a member the model gains cannot be
/// left untested here, and a word the emulator reports has to be a modelled one.
#[test]
fn the_hostname_type_catalog_is_the_models_own() {
    let mut modelled = MODELLED_HOSTNAME_TYPES.to_vec();
    modelled.sort_unstable();
    let mut generated = ec2::ops::types::PUBLIC_IP_DNS_OPTION_VALUES.to_vec();
    generated.sort_unstable();
    assert_eq!(
        generated, modelled,
        "case {:?}: expected the generated catalog to be the model's own, got {generated:?}",
        "the PublicIpDnsOption catalog"
    );
    assert!(
        MODELLED_HOSTNAME_TYPES.contains(&DEFAULT_HOSTNAME_TYPE),
        "case {DEFAULT_HOSTNAME_TYPE:?}: the default type has to be a modelled member of \
         {MODELLED_HOSTNAME_TYPES:?}"
    );
}

/// The public-hostname members one reported interface carries: the hostname of the address it
/// holds, and the `(dnsHostnameType, publicIpv4DnsName)` of the options reporting it.
///
/// `None` for the hostname means the element was ABSENT; `Some("")` means present and empty.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reported {
    public_dns_name: Option<String>,
    hostname_type: Option<String>,
    public_ipv4_dns_name: Option<String>,
    options_present: bool,
}

/// The public-hostname members of `node`, read through the hostname member name the shape uses.
fn reported_of(node: roxmltree::Node<'_, '_>, hostname_member: &str) -> Reported {
    let options = child(node, "publicIpDnsNameOptions");
    Reported {
        public_dns_name: child_text(node, hostname_member),
        hostname_type: options.and_then(|options| child_text(options, "dnsHostnameType")),
        public_ipv4_dns_name: options.and_then(|options| child_text(options, "publicIpv4DnsName")),
        options_present: options.is_some(),
    }
}

fn emulator() -> (MultiServiceRouter, std::sync::Arc<engine::InMemoryEngine>) {
    super::ec2_query::emulator()
}

/// The `SubnetId` of a subnet of the account's default VPC.
fn a_default_subnet(engine: &engine::InMemoryEngine) -> String {
    engine
        .state::<Ec2State>()
        .lock()
        .subnets
        .keys()
        .next()
        .cloned()
        .expect("a fresh account has the default VPC's subnets")
}

/// One call that has to be served, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// One `CreateNetworkInterface` in `subnet_id`, as the interface id it minted.
async fn create_interface(router: &MultiServiceRouter, case: &str, subnet_id: &str) -> String {
    let body = ok(
        case,
        router,
        "CreateNetworkInterface",
        &[
            param("SubnetId", subnet_id),
            param("Description", "an interface the public-hostname suite created"),
        ],
    )
    .await;
    super::ec2_query::element(&body, "networkInterfaceId").unwrap_or_else(|| {
        panic!("case {case:?}: no <networkInterfaceId> in the CreateNetworkInterface {body}")
    })
}

/// One instance launched into the account's default network, as its id — an instance of that
/// network holds a public address, so its primary interface holds a public hostname.
async fn launch(router: &MultiServiceRouter, case: &str) -> String {
    let body = ok(
        case,
        router,
        "RunInstances",
        &[
            param("ImageId", AMI),
            param("MinCount", "1"),
            param("MaxCount", "1"),
        ],
    )
    .await;
    let parsed = document(case, &body);
    parsed
        .descendants()
        .find(|node| {
            node.is_element()
                && child(*node, "instanceId").is_some()
                && child(*node, "instanceState").is_some()
        })
        .and_then(|instance| child_text(instance, "instanceId"))
        .unwrap_or_else(|| panic!("case {case:?}: no instanceId in the RunInstances {body}"))
}

/// What `DescribeNetworkInterfaces` reports for the one interface `network_interface_id` names.
async fn described_interface(
    router: &MultiServiceRouter,
    case: &str,
    network_interface_id: &str,
) -> Reported {
    let body = ok(
        case,
        router,
        "DescribeNetworkInterfaces",
        &[param("NetworkInterfaceId.1", network_interface_id)],
    )
    .await;
    let parsed = document(case, &body);
    let interface = parsed
        .descendants()
        .find(|node| {
            node.is_element()
                && child_text(*node, "networkInterfaceId").as_deref()
                    == Some(network_interface_id)
        })
        .unwrap_or_else(|| {
            panic!("case {case:?}: no reported {network_interface_id} in {body}")
        });
    reported_of(interface, "publicDnsName")
}

/// What `DescribeInstances` reports for the interface `network_interface_id` of `instance_id`,
/// under the instance holding it.
async fn described_under_instance(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
    network_interface_id: &str,
) -> Reported {
    let body = ok(
        case,
        router,
        "DescribeInstances",
        &[param("InstanceId.1", instance_id)],
    )
    .await;
    let parsed = document(case, &body);
    let interface = parsed
        .descendants()
        .find(|node| {
            node.is_element()
                && child_text(*node, "networkInterfaceId").as_deref()
                    == Some(network_interface_id)
        })
        .unwrap_or_else(|| {
            panic!("case {case:?}: the instance {instance_id} reports no {network_interface_id} in {body}")
        });
    reported_of(interface, "publicDnsName")
}

/// One `ModifyPublicIpDnsNameOptions` call, as its HTTP status and XML body.
async fn modify(
    router: &MultiServiceRouter,
    network_interface_id: &str,
    hostname_type: &str,
    extra: &[Param],
) -> (u16, String) {
    let mut params = vec![
        param("NetworkInterfaceId", network_interface_id),
        param("HostnameType", hostname_type),
    ];
    params.extend(extra.iter().cloned());
    call(router, REGION, "ModifyPublicIpDnsNameOptions", &params).await
}

/// One `ModifyPublicIpDnsNameOptions` that has to be served, asserted to report the request as
/// successful.
async fn modify_ok(
    router: &MultiServiceRouter,
    case: &str,
    network_interface_id: &str,
    hostname_type: &str,
) {
    let (status, body) = modify(router, network_interface_id, hostname_type, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ModifyPublicIpDnsNameOptions, got {status} \
         with body {body}"
    );
    let successful = super::ec2_query::element(&body, "successful");
    assert_eq!(
        successful.as_deref(),
        Some("true"),
        "case {case:?}: expected <successful>true</successful>, got {successful:?} in {body}"
    );
}

/// THE RULE: each member of the modelled catalog can be chosen for an interface, the choice
/// REPLACES the one before it, and every report of the interface then carries that type beside
/// the hostname of the address the interface holds — which the choice never touches.
#[tokio::test]
async fn the_chosen_hostname_type_is_read_back_off_every_report_of_the_interface() {
    let (router, engine) = emulator();
    let subnet_id = a_default_subnet(&engine);
    let instance_id = launch(&router, "the launched instance").await;
    let primary = super::ec2_query::primary_network_interface(
        &router,
        REGION,
        "the launched instance",
        &instance_id,
    )
    .await;
    let standalone = create_interface(&router, "the created interface", &subnet_id).await;

    // (case, the interface, whether the instance holding it reports it too)
    let interfaces = [
        ("a launch's primary interface", primary.clone(), true),
        ("an interface a create minted", standalone.clone(), false),
    ];

    // The public hostname each interface holds BEFORE any type is chosen, and the options a
    // report carries for it then: the IPv4 default for one holding a hostname, and no options
    // at all for one holding none.
    for (case, network_interface_id, _) in &interfaces {
        let before = described_interface(&router, case, network_interface_id).await;
        let holds_hostname = before.public_dns_name.as_deref().unwrap_or_default() != "";
        let expected_type = holds_hostname.then(|| DEFAULT_HOSTNAME_TYPE.to_string());
        assert_eq!(
            before.hostname_type, expected_type,
            "case {case:?}: expected the type of an interface nobody chose one for to be \
             {expected_type:?}, got {before:?}"
        );
        assert_eq!(
            before.options_present, holds_hostname,
            "case {case:?}: expected options only for an interface holding a hostname, got \
             {before:?}"
        );
    }
    assert!(
        described_interface(&router, "the primary interface's address", &primary)
            .await
            .public_dns_name
            .as_deref()
            .unwrap_or_default()
            .starts_with("ec2-"),
        "the launch fixture must hold a public hostname for the rows below to carry one"
    );

    // Every member of the catalog in turn, and then back to the FIRST one: each call replaces
    // the type the call before it chose, and choosing a type already chosen is still that type.
    let mut chosen: Vec<&str> = MODELLED_HOSTNAME_TYPES.to_vec();
    chosen.push(MODELLED_HOSTNAME_TYPES[0]);
    for (interface_case, network_interface_id, under_instance) in &interfaces {
        let held = described_interface(&router, interface_case, network_interface_id)
            .await
            .public_dns_name;
        for hostname_type in &chosen {
            let case = format!("{interface_case} set to {hostname_type}");
            modify_ok(&router, &case, network_interface_id, hostname_type).await;

            let reported = described_interface(&router, &case, network_interface_id).await;
            assert_eq!(
                reported.hostname_type.as_deref(),
                Some(*hostname_type),
                "case {case:?}: expected the chosen type to be reported, got {reported:?}"
            );
            // The hostname and the hostnames the options NAME belong to the address the
            // interface holds, which a type choice does not touch.
            assert_eq!(
                reported.public_dns_name, held,
                "case {case:?}: expected the public hostname {held:?} to be left as it \
                 stands, got {reported:?}"
            );
            let expected_name = held
                .as_deref()
                .filter(|hostname| !hostname.is_empty())
                .map(str::to_string);
            assert_eq!(
                reported.public_ipv4_dns_name, expected_name,
                "case {case:?}: expected the options to name {expected_name:?}, got \
                 {reported:?}"
            );

            if *under_instance {
                let under = described_under_instance(
                    &router,
                    &case,
                    &instance_id,
                    network_interface_id,
                )
                .await;
                assert_eq!(
                    under, reported,
                    "case {case:?}: expected the interface under its instance to report \
                     {reported:?}, got {under:?}"
                );
            }
        }
    }
}

/// The choice is per INTERFACE: a type chosen for one interface says nothing about another's.
#[tokio::test]
async fn the_chosen_type_belongs_to_the_one_interface_the_request_named() {
    let (router, engine) = emulator();
    let subnet_id = a_default_subnet(&engine);
    let first = create_interface(&router, "the first interface", &subnet_id).await;
    let second = create_interface(&router, "the second interface", &subnet_id).await;

    // (case, which interface the call names, what each of the two must then report)
    let rows = [
        (
            "a type chosen for the first interface",
            first.clone(),
            MODELLED_HOSTNAME_TYPES[2],
            [Some(MODELLED_HOSTNAME_TYPES[2]), None],
        ),
        (
            "a different type chosen for the second",
            second.clone(),
            MODELLED_HOSTNAME_TYPES[0],
            [
                Some(MODELLED_HOSTNAME_TYPES[2]),
                Some(MODELLED_HOSTNAME_TYPES[0]),
            ],
        ),
        (
            "the first interface moved again",
            first.clone(),
            MODELLED_HOSTNAME_TYPES[1],
            [
                Some(MODELLED_HOSTNAME_TYPES[1]),
                Some(MODELLED_HOSTNAME_TYPES[0]),
            ],
        ),
    ];
    for (case, named, hostname_type, expected) in rows {
        modify_ok(&router, case, &named, hostname_type).await;
        for (network_interface_id, expected) in [&first, &second].into_iter().zip(expected) {
            let reported = described_interface(&router, case, network_interface_id).await;
            assert_eq!(
                reported.hostname_type.as_deref(),
                expected,
                "case {case:?}: expected {network_interface_id} to report {expected:?}, got \
                 {reported:?}"
            );
        }
    }
}

/// A request this service cannot serve is refused with the code and the sentence that say why,
/// and leaves the interface's chosen type exactly as it was.
///
/// The sentence for an interface id this operation cannot resolve is ITS OWN — `The
/// network-interface ID '<id>' is malformed`, whatever form the value is in — rather than the
/// malformed/not-found pair the readers of one named interface split an unresolvable id into.
#[tokio::test]
async fn a_request_that_cannot_be_served_changes_nothing() {
    let (router, engine) = emulator();
    let subnet_id = a_default_subnet(&engine);
    let network_interface_id = create_interface(&router, "the interface", &subnet_id).await;
    // The type the refusals below must leave in place.
    let standing = MODELLED_HOSTNAME_TYPES[2];
    modify_ok(&router, "the standing type", &network_interface_id, standing).await;

    // (case, the id the request names, the hostname type it asks for, the extra members, the
    //  status, the code, the sentence — `None` for a refusal this operation does not word)
    type Row = (
        &'static str,
        String,
        &'static str,
        Vec<Param>,
        u16,
        &'static str,
        Option<String>,
    );
    let rows: Vec<Row> = vec![
        (
            "a value that is no interface id",
            MALFORMED_INTERFACE.to_string(),
            DEFAULT_HOSTNAME_TYPE,
            Vec::new(),
            400,
            "InvalidNetworkInterfaceId.Malformed",
            Some(format!("The network-interface ID '{MALFORMED_INTERFACE}' is malformed")),
        ),
        (
            "an id of another resource kind",
            SUBNET_SHAPED_ID.to_string(),
            DEFAULT_HOSTNAME_TYPE,
            Vec::new(),
            400,
            "InvalidNetworkInterfaceId.Malformed",
            Some(format!("The network-interface ID '{SUBNET_SHAPED_ID}' is malformed")),
        ),
        (
            "a long id the account holds no interface for",
            ABSENT_INTERFACE.to_string(),
            DEFAULT_HOSTNAME_TYPE,
            Vec::new(),
            400,
            "InvalidNetworkInterfaceId.Malformed",
            Some(format!("The network-interface ID '{ABSENT_INTERFACE}' is malformed")),
        ),
        (
            "the all-zero long id",
            ABSENT_ZERO_INTERFACE.to_string(),
            DEFAULT_HOSTNAME_TYPE,
            Vec::new(),
            400,
            "InvalidNetworkInterfaceId.Malformed",
            Some(format!(
                "The network-interface ID '{ABSENT_ZERO_INTERFACE}' is malformed"
            )),
        ),
        (
            "a short id the account holds no interface for",
            ABSENT_SHORT_INTERFACE.to_string(),
            DEFAULT_HOSTNAME_TYPE,
            Vec::new(),
            400,
            "InvalidNetworkInterfaceId.Malformed",
            Some(format!(
                "The network-interface ID '{ABSENT_SHORT_INTERFACE}' is malformed"
            )),
        ),
        (
            "a hostname type outside the modelled catalog",
            network_interface_id.clone(),
            "public-ipv5-dns-name",
            Vec::new(),
            400,
            BOUNDARY_CONSTRAINT_CODE,
            None,
        ),
        (
            "the DryRun probe of a request that would have succeeded",
            network_interface_id.clone(),
            DEFAULT_HOSTNAME_TYPE,
            vec![param("DryRun", "true")],
            412,
            "DryRunOperation",
            None,
        ),
        (
            "the DryRun probe of a request that would have been refused",
            ABSENT_INTERFACE.to_string(),
            DEFAULT_HOSTNAME_TYPE,
            vec![param("DryRun", "true")],
            400,
            "InvalidNetworkInterfaceId.Malformed",
            Some(format!("The network-interface ID '{ABSENT_INTERFACE}' is malformed")),
        ),
    ];

    for (case, named, hostname_type, extra, status, code, message) in rows {
        let (answered, body) = modify(&router, &named, hostname_type, &extra).await;
        assert_eq!(
            answered, status,
            "case {case:?}: expected HTTP {status}, got {answered} with body {body}"
        );
        assert_eq!(
            error_code(&body).as_str(),
            code,
            "case {case:?}: expected the code {code:?}, got {body}"
        );
        if let Some(message) = message {
            assert_eq!(
                error_message(&body),
                message,
                "case {case:?}: expected the sentence {message:?}, got {body}"
            );
        }
        let reported = described_interface(&router, case, &network_interface_id).await;
        assert_eq!(
            reported.hostname_type.as_deref(),
            Some(standing),
            "case {case:?}: a refused request must leave {standing:?} in place, got \
             {reported:?}"
        );
    }
}

/// An interface of the account is still reported with its chosen type when it is read through a
/// report that names NO interface id at all — the account-wide describe — so the choice is a
/// fact of the record rather than of the narrowed read.
#[tokio::test]
async fn the_account_wide_report_carries_the_chosen_type() {
    let (router, engine) = emulator();
    let subnet_id = a_default_subnet(&engine);
    let network_interface_id = create_interface(&router, "the interface", &subnet_id).await;
    let hostname_type = MODELLED_HOSTNAME_TYPES[0];
    modify_ok(
        &router,
        "the account-wide report",
        &network_interface_id,
        hostname_type,
    )
    .await;

    let case = "the account-wide report";
    let body = ok(case, &router, "DescribeNetworkInterfaces", &filters(&[])).await;
    let parsed = document(case, &body);
    let interface = parsed
        .descendants()
        .find(|node| {
            node.is_element()
                && child_text(*node, "networkInterfaceId").as_deref()
                    == Some(network_interface_id.as_str())
        })
        .unwrap_or_else(|| panic!("case {case:?}: no reported {network_interface_id} in {body}"));
    let reported = reported_of(interface, "publicDnsName");
    assert_eq!(
        reported.hostname_type.as_deref(),
        Some(hostname_type),
        "case {case:?}: expected {network_interface_id} to report {hostname_type:?}, got \
         {reported:?}"
    );
}
