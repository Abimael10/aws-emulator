// SPDX-License-Identifier: CC0-1.0

//! The ACCESS LOG SUBSCRIPTION slice: what a create stores, what every read of it reports,
//! what a changed destination does to it, and what takes it away.
//!
//! RULE: an access log subscription is a resource of this service's own — "Enables access logs
//! to be sent to Amazon CloudWatch, Amazon S3, and Amazon Kinesis Data Firehose"
//! (`CreateAccessLogSubscription`'s own documentation) — so the create persists the resource it
//! monitors and the destination it delivers to, every later read reports what was persisted
//! rather than echoing a request, and the resource goes when the resource it monitors goes. A
//! `resourceIdentifier` or a `destinationArn` that names nothing is the modelled
//! `ResourceNotFoundException` (HTTP 404), never a stored subscription that could deliver
//! nowhere.
//!
//! The rows differ along what the rule generalises over: which destination TYPE the records go
//! to (a CloudWatch Logs log group, an Amazon S3 bucket, a Firehose delivery stream), the FORM
//! the log-group destination is stated in (the group on its own, and the group together with
//! everything under it), which KIND of resource is monitored (a service network, a service),
//! the FORM its identifier is submitted in (the bare id, the ARN), whether the create states a
//! `serviceNetworkLogType` and whether it carries tags.
//!
//! The refusal rows differ along WHICH half of the create does not resolve: a monitored
//! resource this service does not hold, and a destination ARN that names no resource of a
//! supported destination type at all.

use serde_json::{json, Value};

use crate::resource_not_found_messages::{absent_id, assert_not_found};
use crate::vpclattice_service_slice::{
    as_label, expected_arn, is_lattice_id, is_timestamp, message_of, Answer, Lattice,
};

/// The ARN resource kinds and the id prefixes this suite addresses resources by, as the
/// modelled patterns spell them.
const SUBSCRIPTION_KIND: &str = "accesslogsubscription";
const SUBSCRIPTION_PREFIX: &str = "als-";
const NETWORK_KIND: &str = "servicenetwork";
const SERVICE_KIND: &str = "service";

/// The name an error MESSAGE calls this resource by, which the not-found construction of this
/// service opens with.
const SUBSCRIPTION_LABEL: &str = "AccessLogSubscription";

/// A log group destination, in the two forms the ARN of one comes in: the group on its own, and
/// the group together with everything under it — which is the form `DescribeLogGroups` reports.
/// Both name the same group.
fn log_group_arn(name: &str) -> String {
    format!("arn:aws:logs:us-west-2:000000000000:log-group:{name}")
}

/// A destination of each of the other two supported types.
fn bucket_arn(name: &str) -> String {
    format!("arn:aws:s3:::{name}")
}
fn delivery_stream_arn(name: &str) -> String {
    format!("arn:aws:firehose:us-west-2:000000000000:deliverystream/{name}")
}

/// Create one subscription.
async fn create_subscription(lattice: &Lattice, body: Value) -> Answer {
    lattice
        .call("POST", "/accesslogsubscriptions", Some(body))
        .await
}

/// Read one subscription back, by whichever form the caller holds.
pub(crate) async fn get_subscription(lattice: &Lattice, identifier: &str) -> Answer {
    lattice
        .call(
            "GET",
            &format!("/accesslogsubscriptions/{}", as_label(identifier)),
            None,
        )
        .await
}

/// Every subscription the service lists for one monitored resource.
async fn list_subscriptions(lattice: &Lattice, resource_identifier: &str) -> Answer {
    lattice
        .call(
            "GET",
            &format!(
                "/accesslogsubscriptions?resourceIdentifier={}",
                as_label(resource_identifier)
            ),
            None,
        )
        .await
}

/// A service network of `name`, answered as its id and its ARN.
async fn a_network(lattice: &Lattice, name: &str) -> (String, String) {
    let created = lattice
        .create_service_network(json!({"name": name}))
        .await;
    assert_eq!(
        created.status, 201,
        "the fixture service network must be created, got {} / {}",
        created.status, created.body
    );
    (created.str_field("id"), created.str_field("arn"))
}

/// A service of `name`, answered the same way.
async fn a_service_of(lattice: &Lattice, name: &str) -> (String, String) {
    let created = lattice.create_service(json!({"name": name})).await;
    assert_eq!(
        created.status, 201,
        "the fixture service must be created, got {} / {}",
        created.status, created.body
    );
    (created.str_field("id"), created.str_field("arn"))
}

/// Which kind of resource one row monitors.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Monitored {
    Network,
    Service,
}

/// Whether the row submits the monitored resource's bare id or its ARN.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Addressed {
    ById,
    ByArn,
}

/// One create row and what every read of the subscription it makes must report.
struct Row {
    case: &'static str,
    monitored: Monitored,
    addressed: Addressed,
    destination: String,
    log_type: Option<&'static str>,
    tags: Option<Value>,
}

fn rows() -> Vec<Row> {
    vec![
        // The ordinary case: a service network monitored by its ARN, delivering into a log
        // group named together with everything under it.
        Row {
            case: "a service network by ARN, into a log group with everything under it",
            monitored: Monitored::Network,
            addressed: Addressed::ByArn,
            destination: format!("{}:*", log_group_arn("/sim2real/als-one")),
            log_type: None,
            tags: Some(json!({"audit": "one"})),
        },
        // The same destination service, stated as the GROUP's own ARN, with the network
        // addressed by its bare id and a log type declared.
        Row {
            case: "a service network by id, into the log group's own ARN, SERVICE log type",
            monitored: Monitored::Network,
            addressed: Addressed::ById,
            destination: log_group_arn("/sim2real/als-two"),
            log_type: Some("SERVICE"),
            tags: None,
        },
        // A destination of the second supported type, whose ARN carries neither a Region nor
        // an account.
        Row {
            case: "a service network by ARN, into an Amazon S3 bucket",
            monitored: Monitored::Network,
            addressed: Addressed::ByArn,
            destination: bucket_arn("sim2real-als-bucket"),
            log_type: None,
            tags: None,
        },
        // A destination of the third supported type.
        Row {
            case: "a service network by id, into a Firehose delivery stream",
            monitored: Monitored::Network,
            addressed: Addressed::ById,
            destination: delivery_stream_arn("sim2real-als-stream"),
            log_type: Some("RESOURCE"),
            tags: Some(json!({"audit": "stream", "tier": "gold"})),
        },
        // And the other kind of resource a subscription may monitor: a SERVICE rather than a
        // service network.
        Row {
            case: "a service by ARN, into a log group",
            monitored: Monitored::Service,
            addressed: Addressed::ByArn,
            destination: format!("{}:*", log_group_arn("/sim2real/als-service")),
            log_type: None,
            tags: None,
        },
    ]
}

#[tokio::test]
async fn a_created_subscription_reads_back_as_it_was_created() {
    for (index, row) in rows().into_iter().enumerate() {
        let lattice = Lattice::new();
        let case = row.case;
        let name = format!("als-fixture-{index}");
        let ((id, arn), kind) = match row.monitored {
            Monitored::Network => (a_network(&lattice, &name).await, NETWORK_KIND),
            Monitored::Service => (a_service_of(&lattice, &name).await, SERVICE_KIND),
        };
        let submitted = match row.addressed {
            Addressed::ById => id.clone(),
            Addressed::ByArn => arn.clone(),
        };

        let mut request = json!({
            "resourceIdentifier": submitted,
            "destinationArn": row.destination,
        });
        if let Some(log_type) = row.log_type {
            request["serviceNetworkLogType"] = json!(log_type);
        }
        if let Some(tags) = row.tags.clone() {
            request["tags"] = tags;
        }
        let created = create_subscription(&lattice, request).await;
        assert_eq!(
            created.status, 201,
            "case {case:?}: expected the create to answer 201, got {} / {}",
            created.status, created.body
        );

        // The identity is this service's: the id it minted and the ARN built around it.
        let subscription_id = created.str_field("id");
        let subscription_arn = created.str_field("arn");
        assert!(
            is_lattice_id(&subscription_id, SUBSCRIPTION_PREFIX),
            "case {case:?}: expected a minted `als-` id, got {subscription_id:?}"
        );
        assert_eq!(
            subscription_arn,
            expected_arn(SUBSCRIPTION_KIND, &subscription_id),
            "case {case:?}: expected the ARN this service builds for the id it minted"
        );
        // The create reports the monitored resource as this service resolved it, whichever
        // form the request addressed it in.
        assert_eq!(
            (
                created.body["resourceId"].as_str(),
                created.body["resourceArn"].as_str(),
                created.body["destinationArn"].as_str(),
                created.body["serviceNetworkLogType"].as_str(),
            ),
            (
                Some(id.as_str()),
                Some(arn.as_str()),
                Some(row.destination.as_str()),
                row.log_type,
            ),
            "case {case:?}: expected the resolved resource and the declared destination, got {}",
            created.body
        );
        assert_eq!(
            kind == NETWORK_KIND,
            arn.contains(&format!(":{NETWORK_KIND}/")),
            "case {case:?}: the fixture ARN must name the kind the row monitors"
        );

        // Every read reports what the create persisted, by either form of the identifier, and
        // the instants of this service's clock beside it.
        for identifier in [subscription_id.clone(), subscription_arn.clone()] {
            let read = get_subscription(&lattice, &identifier).await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: expected the read by {identifier:?} to answer 200, got {} / {}",
                read.status, read.body
            );
            assert_eq!(
                (
                    read.body["id"].as_str(),
                    read.body["arn"].as_str(),
                    read.body["resourceId"].as_str(),
                    read.body["resourceArn"].as_str(),
                    read.body["destinationArn"].as_str(),
                    read.body["serviceNetworkLogType"].as_str(),
                ),
                (
                    Some(subscription_id.as_str()),
                    Some(subscription_arn.as_str()),
                    Some(id.as_str()),
                    Some(arn.as_str()),
                    Some(row.destination.as_str()),
                    row.log_type,
                ),
                "case {case:?}: expected the read by {identifier:?} to report what was stored, \
                 got {}",
                read.body
            );
            for member in ["createdAt", "lastUpdatedAt"] {
                assert!(
                    is_timestamp(&read.body[member]),
                    "case {case:?}: expected {member} as the modelled instant, got {}",
                    read.body
                );
            }
        }

        // And the listing for the monitored resource reports that one subscription, by either
        // form of ITS identifier.
        for resource in [id.clone(), arn.clone()] {
            let listed = list_subscriptions(&lattice, &resource).await;
            assert_eq!(
                listed.status, 200,
                "case {case:?}: expected the listing for {resource:?} to answer 200, got {} / {}",
                listed.status, listed.body
            );
            let items = listed.body["items"].as_array().cloned().unwrap_or_default();
            assert_eq!(
                items.len(),
                1,
                "case {case:?}: expected exactly the one subscription for {resource:?}, got {}",
                listed.body
            );
            assert_eq!(
                items[0]["arn"].as_str(),
                Some(subscription_arn.as_str()),
                "case {case:?}: expected the listed subscription to be the created one, got {}",
                listed.body
            );
            assert!(
                listed.body.get("nextToken").is_none(),
                "case {case:?}: expected no cursor for a single-page listing, got {}",
                listed.body
            );
        }

        // The tags the create asked for are the ones the tag read reports.
        let tags = lattice
            .call("GET", &format!("/tags/{}", as_label(&subscription_arn)), None)
            .await;
        assert_eq!(
            tags.body["tags"],
            row.tags.clone().unwrap_or_else(|| json!({})),
            "case {case:?}: expected the declared tags on the subscription, got {}",
            tags.body
        );
    }
}

/// One refusal row: a create whose request names something nothing holds.
struct Refused {
    case: &'static str,
    /// The resource the create says it monitors, or `None` to use the fixture network.
    resource_identifier: Option<String>,
    destination: String,
    /// The message the refusal must carry, built over what the REQUEST submitted.
    expected_message: fn(&Refused, &str) -> String,
}

/// The message a create naming a resource this service does not hold answers with.
fn resource_not_found_message(row: &Refused, _fixture: &str) -> String {
    let identifier = row
        .resource_identifier
        .clone()
        .unwrap_or_else(|| "sn-00000000000000000".to_string());
    // The kind a `resourceIdentifier` names is the one its own prefix says it is.
    let label = if identifier.contains("svc-") {
        "Service"
    } else {
        "ServiceNetwork"
    };
    format!("{label} {identifier} cannot be found.")
}

/// The message a create naming a destination no supported service holds answers with.
fn destination_not_found_message(row: &Refused, _fixture: &str) -> String {
    format!(
        "{SUBSCRIPTION_LABEL} destination {} cannot be found.",
        row.destination
    )
}

fn refused() -> Vec<Refused> {
    vec![
        // A service network this account does not hold.
        Refused {
            case: "a service network nothing created",
            resource_identifier: Some(absent_id("sn-", "a1b2c3d4e5")),
            destination: format!("{}:*", log_group_arn("/sim2real/als-refused")),
            expected_message: resource_not_found_message,
        },
        // A service this account does not hold, which is the same rule over the other kind.
        Refused {
            case: "a service nothing created",
            resource_identifier: Some(absent_id("svc-", "0f1e2d3c4b")),
            destination: bucket_arn("sim2real-als-refused"),
            expected_message: resource_not_found_message,
        },
        // A destination that is no resource of a supported destination type at all: an Amazon
        // SNS topic is none of the three `CreateAccessLogSubscription` documents.
        Refused {
            case: "a destination that is no supported destination type",
            resource_identifier: None,
            destination: "arn:aws:sns:us-west-2:000000000000:sim2real-als".to_string(),
            expected_message: destination_not_found_message,
        },
        // And one more of that class, over another service's resource.
        Refused {
            case: "a DynamoDB table named as the destination",
            resource_identifier: None,
            destination: "arn:aws:dynamodb:us-west-2:000000000000:table/sim2real-als".to_string(),
            expected_message: destination_not_found_message,
        },
    ]
}

#[tokio::test]
async fn a_create_that_names_nothing_is_refused_rather_than_stored() {
    for (index, row) in refused().into_iter().enumerate() {
        let lattice = Lattice::new();
        let case = row.case;
        let (_, network_arn) = a_network(&lattice, &format!("als-refused-{index}")).await;
        let submitted = row
            .resource_identifier
            .clone()
            .unwrap_or_else(|| network_arn.clone());
        let answered = create_subscription(
            &lattice,
            json!({
                "resourceIdentifier": submitted,
                "destinationArn": row.destination,
            }),
        )
        .await;
        assert_eq!(
            answered.status, 404,
            "case {case:?}: expected the modelled 404, got {} / {}",
            answered.status, answered.body
        );
        assert_eq!(
            answered.error_code.as_deref(),
            Some("ResourceNotFoundException"),
            "case {case:?}: expected the ResourceNotFoundException wire code, got {:?}",
            answered.error_code
        );
        assert_eq!(
            message_of(case, &answered),
            (row.expected_message)(&row, &network_arn),
            "case {case:?}: expected the refusal to name back what the request submitted"
        );
        // Nothing was stored: the network lists no subscription.
        let listed = list_subscriptions(&lattice, &network_arn).await;
        assert_eq!(
            listed.body["items"].as_array().map(Vec::len),
            Some(0),
            "case {case:?}: expected no subscription stored for a refused create, got {}",
            listed.body
        );
    }
}

#[tokio::test]
async fn a_changed_destination_reaches_the_subscription_and_a_delete_takes_it_away() {
    // (case, the destination the update asks for)
    let rows: [(&str, String); 3] = [
        (
            "re-pointed at another log group",
            format!("{}:*", log_group_arn("/sim2real/als-moved")),
        ),
        (
            "re-pointed at a bucket",
            bucket_arn("sim2real-als-moved"),
        ),
        (
            "re-pointed at a delivery stream",
            delivery_stream_arn("sim2real-als-moved"),
        ),
    ];

    for (case, destination) in rows {
        let lattice = Lattice::new();
        let (_, network_arn) = a_network(&lattice, "als-updated").await;
        let created = create_subscription(
            &lattice,
            json!({
                "resourceIdentifier": network_arn,
                "destinationArn": log_group_arn("/sim2real/als-before"),
            }),
        )
        .await;
        assert_eq!(
            created.status, 201,
            "case {case:?}: the subscription under test must be created, got {} / {}",
            created.status, created.body
        );
        let subscription_arn = created.str_field("arn");
        let subscription_id = created.str_field("id");
        let created_at = get_subscription(&lattice, &subscription_arn).await.body["createdAt"]
            .as_str()
            .map(ToString::to_string);

        let updated = lattice
            .call(
                "PATCH",
                &format!("/accesslogsubscriptions/{}", as_label(&subscription_arn)),
                Some(json!({"destinationArn": destination})),
            )
            .await;
        assert_eq!(
            (updated.status, updated.body["arn"].as_str()),
            (200, Some(subscription_arn.as_str())),
            "case {case:?}: expected the update to answer the same subscription, got {} / {}",
            updated.status,
            updated.body
        );
        let read = get_subscription(&lattice, &subscription_arn).await;
        assert_eq!(
            read.body["destinationArn"].as_str(),
            Some(destination.as_str()),
            "case {case:?}: expected the new destination on the same subscription, got {}",
            read.body
        );
        assert_eq!(
            read.body["createdAt"].as_str().map(ToString::to_string),
            created_at,
            "case {case:?}: expected the instant the create recorded to be kept, got {}",
            read.body
        );

        // The delete takes it away, and a second delete of the same subscription is the
        // modelled not-found over the identifier the request submitted.
        let deleted = lattice
            .call(
                "DELETE",
                &format!("/accesslogsubscriptions/{}", as_label(&subscription_arn)),
                None,
            )
            .await;
        assert_eq!(
            deleted.status, 204,
            "case {case:?}: expected the delete to answer 204, got {} / {}",
            deleted.status, deleted.body
        );
        let read = get_subscription(&lattice, &subscription_id).await;
        assert_not_found(&read, case, SUBSCRIPTION_LABEL, &subscription_id);
        let listed = list_subscriptions(&lattice, &network_arn).await;
        assert_eq!(
            listed.body["items"].as_array().map(Vec::len),
            Some(0),
            "case {case:?}: expected the deleted subscription to leave the listing, got {}",
            listed.body
        );
    }
}

#[tokio::test]
async fn deleting_the_monitored_resource_takes_its_subscriptions_with_it() {
    // "If you delete a service network, all resources related to the service network, such as
    // the resource policy, auth policy, and access log subscriptions, are also deleted"
    // (`DeleteServiceNetwork`'s own documentation), and `DeleteService` documents the same for
    // a service — so the rule holds for either kind of monitored resource.
    // (case, which kind is deleted, the path its delete is bound to)
    let rows: [(&str, Monitored, &str); 2] = [
        (
            "a service network with two subscriptions",
            Monitored::Network,
            "/servicenetworks",
        ),
        (
            "a service with two subscriptions",
            Monitored::Service,
            "/services",
        ),
    ];

    for (case, monitored, collection) in rows {
        let lattice = Lattice::new();
        let (_, arn) = match monitored {
            Monitored::Network => a_network(&lattice, "als-cascade").await,
            Monitored::Service => a_service_of(&lattice, "als-cascade").await,
        };
        let mut subscriptions = Vec::new();
        for destination in [
            format!("{}:*", log_group_arn("/sim2real/als-cascade-one")),
            bucket_arn("sim2real-als-cascade"),
        ] {
            let created = create_subscription(
                &lattice,
                json!({"resourceIdentifier": arn, "destinationArn": destination}),
            )
            .await;
            assert_eq!(
                created.status, 201,
                "case {case:?}: each subscription under test must be created, got {} / {}",
                created.status, created.body
            );
            subscriptions.push(created.str_field("id"));
        }
        assert_eq!(
            list_subscriptions(&lattice, &arn).await.body["items"]
                .as_array()
                .map(Vec::len),
            Some(2),
            "case {case:?}: both subscriptions must be listed before the delete"
        );

        let deleted = lattice
            .call(
                "DELETE",
                &format!("{collection}/{}", as_label(&arn)),
                None,
            )
            .await;
        assert!(
            (200..300).contains(&deleted.status),
            "case {case:?}: expected the monitored resource's delete to succeed, got {} / {}",
            deleted.status,
            deleted.body
        );
        for id in &subscriptions {
            let read = get_subscription(&lattice, id).await;
            assert_not_found(&read, case, SUBSCRIPTION_LABEL, id);
        }
    }
}

#[tokio::test]
async fn listing_the_service_networks_pages_the_account_in_creation_order() {
    // `ListServiceNetworks` reports what the account holds, in the one stable order every
    // listing of this service pages by: the order the networks were created in.
    // (case, how many networks the account holds, the page size the listing asks for)
    let rows: [(&str, usize, Option<u32>); 4] = [
        ("a single network, no page size", 1, None),
        ("three networks, no page size", 3, None),
        ("three networks, one per page", 3, Some(1)),
        ("five networks, two per page", 5, Some(2)),
    ];

    for (case, held, page_size) in rows {
        let lattice = Lattice::new();
        let mut created: Vec<String> = Vec::new();
        for index in 0..held {
            let (_, arn) = a_network(&lattice, &format!("als-listed-{index}")).await;
            created.push(arn);
        }

        let mut reported: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut path = "/servicenetworks".to_string();
            let mut query: Vec<String> = Vec::new();
            if let Some(size) = page_size {
                query.push(format!("maxResults={size}"));
            }
            if let Some(cursor) = &token {
                query.push(format!("nextToken={cursor}"));
            }
            if !query.is_empty() {
                path = format!("{path}?{}", query.join("&"));
            }
            let page = lattice.call("GET", &path, None).await;
            assert_eq!(
                page.status, 200,
                "case {case:?}: expected the listing to answer 200, got {} / {}",
                page.status, page.body
            );
            let items = page.body["items"].as_array().cloned().unwrap_or_default();
            if let Some(size) = page_size {
                assert!(
                    items.len() <= size as usize,
                    "case {case:?}: expected at most {size} per page, got {}",
                    page.body
                );
            }
            for item in &items {
                assert!(
                    is_lattice_id(item["id"].as_str().unwrap_or_default(), "sn-"),
                    "case {case:?}: expected a minted `sn-` id in the summary, got {item}"
                );
                assert!(
                    is_timestamp(&item["createdAt"]) && is_timestamp(&item["lastUpdatedAt"]),
                    "case {case:?}: expected the summary's instants, got {item}"
                );
                assert_eq!(
                    (
                        item["numberOfAssociatedServices"].as_i64(),
                        item["numberOfAssociatedVPCs"].as_i64(),
                    ),
                    (Some(0), Some(0)),
                    "case {case:?}: expected no association counted for a fresh network, got \
                     {item}"
                );
                reported.push(
                    item["arn"]
                        .as_str()
                        .unwrap_or_else(|| panic!("case {case:?}: no arn in {item}"))
                        .to_string(),
                );
            }
            token = page.body["nextToken"].as_str().map(ToString::to_string);
            if token.is_none() {
                break;
            }
        }
        assert_eq!(
            reported, created,
            "case {case:?}: expected every network the account holds, in creation order"
        );
    }
}
