// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the RESOURCE-ACCESS slice: a resource gateway, the resource
//! configurations reached through one, and the association that shares a configuration
//! into a service network.
//!
//! Every case drives the emulator the way a client does — a restJson1 request through the
//! router that mounts the service — and compares what a read reports against what the
//! create persisted, so nothing here can pass on a canned body.
//!
//! The rules under test, each asserted over a TABLE of inputs rather than one row:
//!
//! 1. A create persists the resource and every read of it — by either modelled identifier
//!    form, through its own `Get…` and through the listing — reports the members the
//!    create was given, with the ids and ARNs the model's own patterns demand.
//! 2. A `portRanges` entry names either one port or a `<first>-<last>` span of real
//!    ports, with the first no greater than the last; anything else is refused HTTP 400
//!    `ValidationException` with `<entry> is not a valid port range` and nothing is
//!    stored. Checked on each published bound (1, 65535), one past it (65536) and on
//!    inverted spans.
//! 3. An identifier that names no persisted resource of its kind is the modelled 404,
//!    worded as that kind's own not-found sentence.
//! 4. A resource that something still depends on cannot be deleted: a gateway with a
//!    configuration, a configuration with an association, a service network with a
//!    resource association.
//! 5. The tags a create carries are the tags the tag read reports for the resource.
//! 6. A create answers the non-terminal status and the resource reaches `ACTIVE` on the
//!    clock alone.

use crate::vpclattice_service_slice::{
    as_label, expected_arn, is_lattice_id, message_of, Answer, Lattice,
};
use service_com_amazonaws_vpclattice_mercurycontrolplane::state;

/// The VPC a gateway of these cases is opened in: a bare VPC id, the one form
/// `vpcIdentifier` is modelled with (`^vpc-(([0-9a-z]{8})|([0-9a-z]{17}))$`).
const VPC_ID: &str = "vpc-0a1b2c3d";
/// The subnet and security group a gateway of these cases is given, in the forms
/// `SubnetList` and `SecurityGroupList` carry.
const SUBNET_ID: &str = "subnet-0a1b2c3d4e5f6a7b8";
const SECURITY_GROUP_ID: &str = "sg-0a1b2c3d4e5f6a7b8";

/// How long a poll for a clock-owed transition may take before the case fails. Generous:
/// the transition is owed after [`state::CREATE_TRANSITION`], and only the condition is
/// asserted, never the elapsed time.
const TRANSITION_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// How long the poll waits between looks. Short enough that a case finishes promptly,
/// long enough that the loop is not a spin.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(25);

async fn create_gateway(lattice: &Lattice, body: serde_json::Value) -> Answer {
    lattice.call("POST", "/resourcegateways", Some(body)).await
}

async fn get_gateway(lattice: &Lattice, identifier: &str) -> Answer {
    lattice
        .call("GET", &format!("/resourcegateways/{identifier}"), None)
        .await
}

async fn delete_gateway(lattice: &Lattice, identifier: &str) -> Answer {
    lattice
        .call("DELETE", &format!("/resourcegateways/{identifier}"), None)
        .await
}

async fn create_configuration(lattice: &Lattice, body: serde_json::Value) -> Answer {
    lattice
        .call("POST", "/resourceconfigurations", Some(body))
        .await
}

async fn get_configuration(lattice: &Lattice, identifier: &str) -> Answer {
    lattice
        .call("GET", &format!("/resourceconfigurations/{identifier}"), None)
        .await
}

async fn delete_configuration(lattice: &Lattice, identifier: &str) -> Answer {
    lattice
        .call(
            "DELETE",
            &format!("/resourceconfigurations/{identifier}"),
            None,
        )
        .await
}

async fn create_resource_association(lattice: &Lattice, body: serde_json::Value) -> Answer {
    lattice
        .call("POST", "/servicenetworkresourceassociations", Some(body))
        .await
}

async fn get_resource_association(lattice: &Lattice, identifier: &str) -> Answer {
    lattice
        .call(
            "GET",
            &format!("/servicenetworkresourceassociations/{identifier}"),
            None,
        )
        .await
}

async fn delete_resource_association(lattice: &Lattice, identifier: &str) -> Answer {
    lattice
        .call(
            "DELETE",
            &format!("/servicenetworkresourceassociations/{identifier}"),
            None,
        )
        .await
}

async fn list_tags(lattice: &Lattice, arn: &str) -> Answer {
    lattice
        .call("GET", &format!("/tags/{}", as_label(arn)), None)
        .await
}

/// A gateway persisted for the cases that need one, answering its (id, ARN).
async fn gateway_fixture(lattice: &Lattice, name: &str) -> (String, String) {
    let created = create_gateway(
        lattice,
        serde_json::json!({
            "name": name,
            "vpcIdentifier": VPC_ID,
            "subnetIds": [SUBNET_ID],
            "securityGroupIds": [SECURITY_GROUP_ID],
        }),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "fixture gateway {name}: expected 201, got {} / {}",
        created.status, created.body
    );
    (created.str_field("id"), created.str_field("arn"))
}

/// Rule 1: the create of a gateway persists it, and every read reports what it was given.
#[tokio::test]
async fn a_resource_gateway_is_persisted_and_read_back_by_either_identifier_form() {
    let lattice = Lattice::new();
    let created = create_gateway(
        &lattice,
        serde_json::json!({
            "name": "gw-fixture-one",
            "vpcIdentifier": VPC_ID,
            "subnetIds": [SUBNET_ID],
            "securityGroupIds": [SECURITY_GROUP_ID],
            "ipAddressType": "IPV4",
        }),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "create: expected 201, got {} / {}",
        created.status, created.body
    );
    let id = created.str_field("id");
    let arn = created.str_field("arn");
    assert!(
        is_lattice_id(&id, state::PREFIX_RESOURCE_GATEWAY),
        "create: expected a minted rgw- id, got {id:?}"
    );
    assert_eq!(
        arn,
        expected_arn(state::KIND_RESOURCE_GATEWAY, &id),
        "create: expected the gateway's own ARN"
    );
    assert_eq!(
        created.str_field("status"),
        "CREATE_IN_PROGRESS",
        "create: a gateway is created asynchronously, got {}",
        created.body
    );

    // Both modelled identifier forms name the same persisted gateway.
    for (case, identifier) in [("bare id", id.clone()), ("ARN", as_label(&arn))] {
        let read = get_gateway(&lattice, &identifier).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected 200, got {} / {}",
            read.status, read.body
        );
        assert_eq!(
            read.str_field("id"),
            id,
            "case {case:?}: expected the persisted id, got {}",
            read.body
        );
        assert_eq!(
            read.str_field("arn"),
            arn,
            "case {case:?}: expected the persisted ARN, got {}",
            read.body
        );
        assert_eq!(
            read.str_field("name"),
            "gw-fixture-one",
            "case {case:?}: expected the declared name, got {}",
            read.body
        );
        assert_eq!(
            read.body["vpcId"].as_str(),
            Some(VPC_ID),
            "case {case:?}: expected the declared VPC, got {}",
            read.body
        );
        assert_eq!(
            read.body["subnetIds"],
            serde_json::json!([SUBNET_ID]),
            "case {case:?}: expected the declared subnets, got {}",
            read.body
        );
        assert_eq!(
            read.body["securityGroupIds"],
            serde_json::json!([SECURITY_GROUP_ID]),
            "case {case:?}: expected the declared security groups, got {}",
            read.body
        );
        assert!(
            !read.body["createdAt"].is_null(),
            "case {case:?}: expected the instant the create recorded, got {}",
            read.body
        );
    }

    // The listing reports the gateway the create persisted.
    let listed = lattice.call("GET", "/resourcegateways", None).await;
    assert_eq!(
        listed.status, 200,
        "list: expected 200, got {} / {}",
        listed.status, listed.body
    );
    let items = listed.body["items"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("list: expected items, got {}", listed.body));
    assert_eq!(
        items.len(),
        1,
        "list: expected exactly the persisted gateway, got {}",
        listed.body
    );
    assert_eq!(
        items[0]["arn"].as_str(),
        Some(arn.as_str()),
        "list: expected the persisted ARN, got {}",
        listed.body
    );
}

/// Rule 1, for a resource configuration, over a table of the types and definitions the
/// model admits: every member the create was given is what the read reports.
#[tokio::test]
async fn resource_configurations_are_persisted_and_read_back_over_every_declared_shape() {
    let lattice = Lattice::new();
    let (gateway_id, gateway_arn) = gateway_fixture(&lattice, "gw-config-cases").await;

    // (case, the create body's own members beyond the name)
    let rows: Vec<(&str, serde_json::Value)> = vec![
        (
            "a single IP resource on one port",
            serde_json::json!({
                "type": "SINGLE",
                "resourceGatewayIdentifier": gateway_id.clone(),
                "resourceConfigurationDefinition": {"ipResource": {"ipAddress": "10.121.0.50"}},
                "portRanges": ["443"],
            }),
        ),
        (
            "a single DNS resource over a span of ports, named by the gateway's ARN",
            serde_json::json!({
                "type": "SINGLE",
                "resourceGatewayIdentifier": gateway_arn.clone(),
                "resourceConfigurationDefinition": {
                    "dnsResource": {"domainName": "db.example.internal", "ipAddressType": "IPV4"}
                },
                "portRanges": ["5432-5440"],
                "protocol": "TCP",
            }),
        ),
        (
            "an ARN resource with two port entries",
            serde_json::json!({
                "type": "ARN",
                "resourceGatewayIdentifier": gateway_id.clone(),
                "resourceConfigurationDefinition": {
                    "arnResource": {
                        "arn": "arn:aws:rds:us-east-1:123456789012:db:example-instance"
                    }
                },
                "portRanges": ["80", "8000-9000"],
            }),
        ),
        (
            "a group, which carries no definition of its own",
            serde_json::json!({
                "type": "GROUP",
                "resourceGatewayIdentifier": gateway_id.clone(),
                "portRanges": ["1-65535"],
            }),
        ),
    ];

    for (index, (case, members)) in rows.iter().enumerate() {
        let mut body = members.clone();
        let name = format!("cfg-case-{index}");
        body["name"] = serde_json::Value::String(name.clone());
        let created = create_configuration(&lattice, body.clone()).await;
        assert_eq!(
            created.status, 201,
            "case {case:?}: expected 201, got {} / {}",
            created.status, created.body
        );
        let id = created.str_field("id");
        assert!(
            is_lattice_id(&id, state::PREFIX_RESOURCE_CONFIGURATION),
            "case {case:?}: expected a minted rcfg- id, got {id:?}"
        );
        assert_eq!(
            created.str_field("arn"),
            expected_arn(state::KIND_RESOURCE_CONFIGURATION, &id),
            "case {case:?}: expected the configuration's own ARN, got {}",
            created.body
        );

        let read = get_configuration(&lattice, &id).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected 200, got {} / {}",
            read.status, read.body
        );
        assert_eq!(
            read.str_field("name"),
            name,
            "case {case:?}: expected the declared name, got {}",
            read.body
        );
        assert_eq!(
            read.body["type"],
            body["type"],
            "case {case:?}: expected the declared type, got {}",
            read.body
        );
        assert_eq!(
            read.body["portRanges"],
            body["portRanges"],
            "case {case:?}: expected the declared port ranges, got {}",
            read.body
        );
        assert_eq!(
            read.body["resourceConfigurationDefinition"],
            body.get("resourceConfigurationDefinition")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
            "case {case:?}: expected the declared definition, got {}",
            read.body
        );
        assert_eq!(
            read.body["resourceGatewayId"].as_str(),
            Some(gateway_id.as_str()),
            "case {case:?}: expected the gateway the create named, got {}",
            read.body
        );
        // The protocol a create omits is the one that member's own documentation gives
        // the types it applies to, and nothing for a type it does not apply to.
        let configuration_type = body["type"].as_str().unwrap_or_default();
        let expected_protocol = state::PROTOCOL_TYPES
            .contains(&configuration_type)
            .then_some(state::DEFAULT_RESOURCE_CONFIGURATION_PROTOCOL);
        assert_eq!(
            read.body["protocol"].as_str(),
            expected_protocol,
            "case {case:?}: expected the documented default protocol, got {}",
            read.body
        );
    }

    // Every configuration the cases persisted is listed under the gateway they share,
    // and under no other gateway.
    let listed = lattice
        .call(
            "GET",
            &format!("/resourceconfigurations?resourceGatewayIdentifier={gateway_id}"),
            None,
        )
        .await;
    assert_eq!(
        listed.status, 200,
        "list by gateway: expected 200, got {} / {}",
        listed.status, listed.body
    );
    assert_eq!(
        listed.body["items"].as_array().map(Vec::len),
        Some(rows.len()),
        "list by gateway: expected every persisted configuration, got {}",
        listed.body
    );
}

/// Rule 2: the port-range rule, on each published bound and one past it.
#[tokio::test]
async fn a_port_range_entry_must_name_a_non_empty_range_of_real_ports() {
    let lattice = Lattice::new();
    let (gateway_id, _) = gateway_fixture(&lattice, "gw-port-ranges").await;

    // (case, the entries the create sends, whether the service accepts them)
    // The bounds are `PortRange`'s own: a port runs from 1 to 65535, so 0 is short of the
    // first one and 65536 is past the last, while the modelled pattern admits both.
    let rows: Vec<(&str, Vec<&str>, bool)> = vec![
        ("the first port", vec!["1"], true),
        ("the last port", vec!["65535"], true),
        ("a span of one port", vec!["8080-8080"], true),
        ("the whole range", vec!["1-65535"], true),
        ("two entries, each a valid range", vec!["80", "443-444"], true),
        ("one short of the first port", vec!["0"], false),
        ("a span that starts short of the first port", vec!["0-80"], false),
        ("one past the last port", vec!["65536"], false),
        ("a span past the last port", vec!["65000-70000"], false),
        ("a span whose first port is the greater", vec!["100-50"], false),
        ("a span inverted by one", vec!["9-8"], false),
        (
            "a valid entry beside an inverted one",
            vec!["443", "100-50"],
            false,
        ),
    ];

    for (index, (case, entries, accepted)) in rows.iter().enumerate() {
        let name = format!("cfg-port-{index}");
        let answer = create_configuration(
            &lattice,
            serde_json::json!({
                "name": name,
                "type": "SINGLE",
                "resourceGatewayIdentifier": gateway_id,
                "resourceConfigurationDefinition": {"ipResource": {"ipAddress": "10.0.0.7"}},
                "portRanges": entries,
            }),
        )
        .await;
        if *accepted {
            assert_eq!(
                answer.status, 201,
                "case {case:?}: expected the create to be accepted, got {} / {}",
                answer.status, answer.body
            );
            let read = get_configuration(&lattice, &answer.str_field("id")).await;
            assert_eq!(
                read.body["portRanges"],
                serde_json::json!(entries),
                "case {case:?}: expected the declared port ranges to be persisted, got {}",
                read.body
            );
            continue;
        }
        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected the modelled 400, got {} / {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code.as_deref(),
            Some("ValidationException"),
            "case {case:?}: expected the ValidationException wire code, got {:?} / {}",
            answer.error_code,
            answer.body
        );
        // The refusal names the FIRST entry at fault, which is a value the request sent.
        let offending = entries
            .iter()
            .find(|entry| {
                let (first, last) = entry.split_once('-').unwrap_or((entry, entry));
                let first: u32 = first.parse().unwrap_or(u32::MAX);
                let last: u32 = last.parse().unwrap_or(u32::MAX);
                first > last || first < state::MIN_PORT || last > state::MAX_PORT
            })
            .expect("a refused case has an entry at fault");
        assert_eq!(
            message_of(case, &answer),
            state::invalid_port_range_message(offending),
            "case {case:?}: expected AWS's port-range refusal, got {}",
            answer.body
        );
        assert_eq!(
            answer.body["fieldList"][0]["name"].as_str(),
            Some(state::PORT_RANGES_MEMBER),
            "case {case:?}: expected the refusal to name the member at fault, got {}",
            answer.body
        );
        // A refused create stores nothing, so the name it asked for is free.
        let listed = lattice.call("GET", "/resourceconfigurations", None).await;
        assert!(
            !listed
                .body
                .to_string()
                .contains(&format!("\"{name}\"")),
            "case {case:?}: a refused create must persist nothing, got {}",
            listed.body
        );
    }
}

/// Rules 1 and 6, for the association that shares a configuration into a service network:
/// it is persisted with both parents, reaches `ACTIVE` on the clock alone, is listed under
/// either parent, and is gone after the delete.
#[tokio::test]
async fn a_resource_association_links_both_records_and_reaches_active_on_its_own() {
    let lattice = Lattice::new();
    let (gateway_id, _) = gateway_fixture(&lattice, "gw-association").await;
    let network = lattice
        .create_service_network(serde_json::json!({ "name": "cfg-net" }))
        .await;
    let network_id = network.str_field("id");
    let configuration = create_configuration(
        &lattice,
        serde_json::json!({
            "name": "cfg-associated",
            "type": "SINGLE",
            "resourceGatewayIdentifier": gateway_id,
            "resourceConfigurationDefinition": {"ipResource": {"ipAddress": "10.0.0.11"}},
            "portRanges": ["443"],
        }),
    )
    .await;
    let configuration_id = configuration.str_field("id");
    let configuration_arn = configuration.str_field("arn");

    let created = create_resource_association(
        &lattice,
        serde_json::json!({
            "resourceConfigurationIdentifier": configuration_id,
            "serviceNetworkIdentifier": network_id,
        }),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "create: expected 201, got {} / {}",
        created.status, created.body
    );
    let association_id = created.str_field("id");
    assert!(
        is_lattice_id(&association_id, state::PREFIX_RESOURCE_ASSOCIATION),
        "create: expected a minted snra- id, got {association_id:?}"
    );
    assert_eq!(
        created.str_field("arn"),
        expected_arn(state::KIND_RESOURCE_ASSOCIATION, &association_id),
        "create: expected the association's own ARN, got {}",
        created.body
    );
    assert_eq!(
        created.str_field("status"),
        "CREATE_IN_PROGRESS",
        "create: an association is made asynchronously, got {}",
        created.body
    );

    let read = get_resource_association(&lattice, &association_id).await;
    assert_eq!(
        read.status, 200,
        "read: expected 200, got {} / {}",
        read.status, read.body
    );
    for (member, expected) in [
        ("resourceConfigurationId", configuration_id.as_str()),
        ("resourceConfigurationArn", configuration_arn.as_str()),
        ("resourceConfigurationName", "cfg-associated"),
        ("serviceNetworkId", network_id.as_str()),
    ] {
        assert_eq!(
            read.body[member].as_str(),
            Some(expected),
            "case {member:?}: expected the parent the create named, got {}",
            read.body
        );
    }

    // The status the clock owes the association, waited for by looking again rather than
    // by sleeping a fixed time.
    let deadline = std::time::Instant::now() + TRANSITION_DEADLINE;
    loop {
        let polled = get_resource_association(&lattice, &association_id).await;
        if polled.str_field("status") == state::STATUS_ACTIVE {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the association never reached ACTIVE, last read {}",
            polled.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    // Listed under either parent, and under neither when the filter names something else.
    for (case, filter) in [
        (
            "by service network",
            format!("serviceNetworkIdentifier={network_id}"),
        ),
        (
            "by resource configuration",
            format!("resourceConfigurationIdentifier={configuration_id}"),
        ),
    ] {
        let listed = lattice
            .call(
                "GET",
                &format!("/servicenetworkresourceassociations?{filter}"),
                None,
            )
            .await;
        assert_eq!(
            listed.status, 200,
            "case {case:?}: expected 200, got {} / {}",
            listed.status, listed.body
        );
        let ids: Vec<&str> = listed.body["items"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item["id"].as_str())
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            ids,
            vec![association_id.as_str()],
            "case {case:?}: expected exactly the persisted association, got {}",
            listed.body
        );
    }

    // The delete removes exactly the association: the read of it is the modelled 404 while
    // both parents still answer.
    let deleted = delete_resource_association(&lattice, &association_id).await;
    assert_eq!(
        deleted.status, 200,
        "delete: expected 200, got {} / {}",
        deleted.status, deleted.body
    );
    let gone = get_resource_association(&lattice, &association_id).await;
    assert_eq!(
        gone.status, 404,
        "after the delete: expected 404, got {} / {}",
        gone.status, gone.body
    );
    assert_eq!(
        get_configuration(&lattice, &configuration_id).await.status,
        200,
        "after the delete: the configuration must still answer"
    );
    assert_eq!(
        lattice.get_service_network(&network_id).await.status,
        200,
        "after the delete: the service network must still answer"
    );
}

/// Rule 3: an identifier that names no persisted resource of its kind is the modelled 404,
/// worded as that kind's own not-found sentence.
#[tokio::test]
async fn unknown_identifiers_of_this_slice_answer_their_own_not_found_sentence() {
    let lattice = Lattice::new();

    let absent_gateway = format!("{}0{}", state::PREFIX_RESOURCE_GATEWAY, "a".repeat(16));
    let absent_configuration =
        format!("{}0{}", state::PREFIX_RESOURCE_CONFIGURATION, "b".repeat(16));
    let absent_association =
        format!("{}0{}", state::PREFIX_RESOURCE_ASSOCIATION, "c".repeat(16));

    // (case, the answer, the identifier the request named, the kind it names)
    let rows: Vec<(&str, Answer, String, state::ResourceKind)> = vec![
        (
            "GetResourceGateway",
            get_gateway(&lattice, &absent_gateway).await,
            absent_gateway.clone(),
            state::ResourceKind::ResourceGateway,
        ),
        (
            "DeleteResourceGateway",
            delete_gateway(&lattice, &absent_gateway).await,
            absent_gateway.clone(),
            state::ResourceKind::ResourceGateway,
        ),
        (
            "GetResourceConfiguration",
            get_configuration(&lattice, &absent_configuration).await,
            absent_configuration.clone(),
            state::ResourceKind::ResourceConfiguration,
        ),
        (
            "DeleteResourceConfiguration",
            delete_configuration(&lattice, &absent_configuration).await,
            absent_configuration.clone(),
            state::ResourceKind::ResourceConfiguration,
        ),
        (
            "GetServiceNetworkResourceAssociation",
            get_resource_association(&lattice, &absent_association).await,
            absent_association.clone(),
            state::ResourceKind::ResourceAssociation,
        ),
        (
            "DeleteServiceNetworkResourceAssociation",
            delete_resource_association(&lattice, &absent_association).await,
            absent_association.clone(),
            state::ResourceKind::ResourceAssociation,
        ),
    ];

    for (case, answer, identifier, kind) in rows {
        assert_eq!(
            answer.status, 404,
            "case {case:?}: expected 404, got {} / {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code.as_deref(),
            Some("ResourceNotFoundException"),
            "case {case:?}: expected the ResourceNotFoundException wire code, got {:?} / {}",
            answer.error_code,
            answer.body
        );
        assert_eq!(
            message_of(case, &answer),
            state::resource_access_not_found(kind, &identifier).message,
            "case {case:?}: expected this kind's own not-found sentence, got {}",
            answer.body
        );
        assert_eq!(
            answer.body["resourceType"].as_str(),
            Some(kind.wire_type()),
            "case {case:?}: expected the kind's resourceType token, got {}",
            answer.body
        );
    }

    // The same refusal for a read that addressed the resource by its ARN: the sentence names
    // the resource's own ID either way, while `resourceId` names what the request sent.
    for (case, id, kind, answer) in [
        (
            "GetResourceGateway by ARN",
            absent_gateway.clone(),
            state::ResourceKind::ResourceGateway,
            get_gateway(
                &lattice,
                &as_label(&expected_arn(state::KIND_RESOURCE_GATEWAY, &absent_gateway)),
            )
            .await,
        ),
        (
            "GetResourceConfiguration by ARN",
            absent_configuration.clone(),
            state::ResourceKind::ResourceConfiguration,
            get_configuration(
                &lattice,
                &as_label(&expected_arn(
                    state::KIND_RESOURCE_CONFIGURATION,
                    &absent_configuration,
                )),
            )
            .await,
        ),
        (
            "GetServiceNetworkResourceAssociation by ARN",
            absent_association.clone(),
            state::ResourceKind::ResourceAssociation,
            get_resource_association(
                &lattice,
                &as_label(&expected_arn(
                    state::KIND_RESOURCE_ASSOCIATION,
                    &absent_association,
                )),
            )
            .await,
        ),
    ] {
        assert_eq!(
            answer.status, 404,
            "case {case:?}: expected 404, got {} / {}",
            answer.status, answer.body
        );
        assert_eq!(
            message_of(case, &answer),
            state::resource_access_not_found(kind, &id).message,
            "case {case:?}: the sentence must name the resource's own id, got {}",
            answer.body
        );
    }
}

/// Rule 4: a resource something still depends on cannot be deleted, and the refusal names
/// the resource that still has dependents.
#[tokio::test]
async fn a_resource_with_dependents_is_refused_until_they_are_gone() {
    let lattice = Lattice::new();
    let (gateway_id, _) = gateway_fixture(&lattice, "gw-dependents").await;
    let network = lattice
        .create_service_network(serde_json::json!({ "name": "dependents-net" }))
        .await;
    let network_id = network.str_field("id");
    let configuration = create_configuration(
        &lattice,
        serde_json::json!({
            "name": "cfg-dependents",
            "type": "SINGLE",
            "resourceGatewayIdentifier": gateway_id,
            "resourceConfigurationDefinition": {"ipResource": {"ipAddress": "10.0.0.21"}},
            "portRanges": ["443"],
        }),
    )
    .await;
    let configuration_id = configuration.str_field("id");
    let association = create_resource_association(
        &lattice,
        serde_json::json!({
            "resourceConfigurationIdentifier": configuration_id,
            "serviceNetworkIdentifier": network_id,
        }),
    )
    .await;
    let association_id = association.str_field("id");

    // (case, the answer, the id the refusal must name)
    let blocked: Vec<(&str, Answer, &str)> = vec![
        (
            "a gateway whose configuration still exists",
            delete_gateway(&lattice, &gateway_id).await,
            gateway_id.as_str(),
        ),
        (
            "a configuration still shared into a service network",
            delete_configuration(&lattice, &configuration_id).await,
            configuration_id.as_str(),
        ),
        (
            "a service network with a resource configuration associated",
            lattice
                .call("DELETE", &format!("/servicenetworks/{network_id}"), None)
                .await,
            network_id.as_str(),
        ),
    ];
    for (case, answer, named) in blocked {
        assert_eq!(
            answer.status, 409,
            "case {case:?}: expected the modelled 409, got {} / {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code.as_deref(),
            Some("ConflictException"),
            "case {case:?}: expected the ConflictException wire code, got {:?} / {}",
            answer.error_code,
            answer.body
        );
        assert!(
            message_of(case, &answer).contains(named),
            "case {case:?}: the refusal must name {named}, got {}",
            answer.body
        );
    }

    // Dropped in dependency order, each delete now goes through and the resource is gone.
    assert_eq!(
        delete_resource_association(&lattice, &association_id)
            .await
            .status,
        200,
        "the association must be deletable"
    );
    assert_eq!(
        delete_configuration(&lattice, &configuration_id).await.status,
        204,
        "the configuration must be deletable once nothing shares it"
    );
    assert_eq!(
        delete_gateway(&lattice, &gateway_id).await.status,
        200,
        "the gateway must be deletable once no configuration uses it"
    );
    assert_eq!(
        get_gateway(&lattice, &gateway_id).await.status,
        404,
        "a deleted gateway must be gone"
    );
    assert_eq!(
        get_configuration(&lattice, &configuration_id).await.status,
        404,
        "a deleted configuration must be gone"
    );
}

/// Rule 5: the tags a create carries are the tags the tag read reports for the resource.
#[tokio::test]
async fn the_tags_a_create_carries_are_what_the_tag_read_reports() {
    let lattice = Lattice::new();
    let tags = serde_json::json!({ "team": "platform", "env": "test" });

    let gateway = create_gateway(
        &lattice,
        serde_json::json!({
            "name": "gw-tagged",
            "vpcIdentifier": VPC_ID,
            "subnetIds": [SUBNET_ID],
            "tags": tags,
        }),
    )
    .await;
    let gateway_arn = gateway.str_field("arn");
    let configuration = create_configuration(
        &lattice,
        serde_json::json!({
            "name": "cfg-tagged",
            "type": "SINGLE",
            "resourceGatewayIdentifier": gateway.str_field("id"),
            "resourceConfigurationDefinition": {"ipResource": {"ipAddress": "10.0.0.31"}},
            "portRanges": ["443"],
            "tags": tags,
        }),
    )
    .await;
    let network = lattice
        .create_service_network(serde_json::json!({ "name": "tagged-net" }))
        .await;
    let association = create_resource_association(
        &lattice,
        serde_json::json!({
            "resourceConfigurationIdentifier": configuration.str_field("id"),
            "serviceNetworkIdentifier": network.str_field("id"),
            "tags": tags,
        }),
    )
    .await;

    for (case, arn) in [
        ("resource gateway", gateway_arn),
        ("resource configuration", configuration.str_field("arn")),
        ("resource association", association.str_field("arn")),
    ] {
        let read = list_tags(&lattice, &arn).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected 200, got {} / {}",
            read.status, read.body
        );
        assert_eq!(
            read.body["tags"], tags,
            "case {case:?}: expected the tags the create carried, got {}",
            read.body
        );
    }
}
