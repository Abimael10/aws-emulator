// SPDX-License-Identifier: CC0-1.0

//! What a COLLECTION READ reports for a resource once its state has CHANGED, end to end over
//! the wire.
//!
//! RULE: the identifier this service mints for a resource is fixed at the create that minted
//! it, so every later echo of it is that one value: after a state change that follows the
//! create — an update of the resource, a tag written on it, or the removal of a different
//! resource from the same collection — the collection read still reports, on the element of
//! that resource, the identifier its create answered with; where the element's shape carries
//! an ARN, the resource segment of that same element's ARN is that identifier; and the
//! resource's own read, addressed BY the identifier the element reported, answers with it.
//!
//! [`minted_identifier_opacity`](super::minted_identifier_opacity) pins what a create and a
//! collection read report for a resource NOTHING has touched since. What is pinned here is
//! the same identity across the state changes a client makes afterwards, so a list-then-act
//! flow keeps addressing the resource it enumerated.
//!
//! The rows of the table below differ along what the rule generalises over: which resource
//! the collection reports, the member its page is reported on, how wide the modelled
//! identifier shape is, whether the element carries an ARN, and which kind of state change
//! precedes the read (an in-place update, a tag write, the deletion of a neighbour).
//!
//! Every case drives the real restJson1 request path (`MultiServiceRouter::dispatch`), so the
//! routes, statuses and bodies asserted here are the ones a client sees.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iotmanagedintegrations_iotmanagedintegrations::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::support::label;

/// The region the suite's requests are signed for.
const REGION: &str = "us-east-1";

/// The account id every generated emulator presents (`engine::EMULATOR_ACCOUNT_ID`), which is
/// the account segment of every ARN it mints.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The service namespace every ARN of this service carries, from the model's `sigv4` name
/// (`smithy_query(query="service_info")` -> `"sigv4Name": "iotmanagedintegrations"`), which is
/// also what the ARN patterns of its output shapes require.
const ARN_SERVICE: &str = "iotmanagedintegrations";

/// The member every summary and every read of these rows reports the identifier on
/// (`smithy_query(query="shape",shape="CredentialLockerSummary,ManagedThingSummary,
/// ProvisioningProfileSummary,EventLogConfigurationSummary")` -> `Id` on each).
const ID_MEMBER: &str = "Id";

/// How many resources each row creates, so a row has a resource to change, a neighbour to
/// delete and a third the changes must leave alone.
const RESOURCES_PER_CASE: usize = 3;

/// One HTTP answer, reduced to what the assertions read.
struct Reply {
    status: u16,
    body: Value,
}

impl Reply {
    fn string(&self, name: &str) -> Option<&str> {
        self.body
            .get(name)
            .filter(|value| !value.is_null())
            .and_then(Value::as_str)
    }
}

/// A router holding one `iotmanagedintegrations` service over a fresh shared engine: the same
/// assembly `core/router` performs, with one service mounted.
fn emulator() -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    // The signature itself is not under test here, so requests are unauthenticated.
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::new(engine::InMemoryEngine::new())));
    router
}

/// Send one request, signed for [`REGION`], and reduce the answer. The `Authorization`
/// credential scope is what routes the request to this service and names its region.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    path_and_query: &str,
    body: Option<Value>,
) -> Reply {
    let payload = body.map_or_else(bytes::Bytes::new, |value| {
        bytes::Bytes::from(serde_json::to_vec(&value).expect("serialize the request body"))
    });
    let request = http::Request::builder()
        .method(method)
        .uri(format!(
            "https://iotmanagedintegrations.{REGION}.amazonaws.com{path_and_query}"
        ))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTTESTTESTTEST/20260101/{REGION}/\
                 iotmanagedintegrations/aws4_request, SignedHeaders=host, Signature=0"
            ),
        )
        .header("content-type", "application/json")
        .body(payload)
        .expect("a well-formed request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    Reply { status, body }
}

/// The ARN a resource of `segment` and `id` carries in [`REGION`], spelled as the ARN patterns
/// of this service's output shapes require
/// (`^arn:aws:iotmanagedintegrations:[0-9a-zA-Z-]+:[0-9]+:<segment>/…$`).
fn resource_arn(segment: &str, id: &str) -> String {
    format!("arn:aws:{ARN_SERVICE}:{REGION}:{ACCOUNT}:{segment}/{id}")
}

/// The in-place update of a resource, as the model binds it: the method and instance route it
/// is sent on, its success status, and a body that really changes the record.
struct Update {
    method: &'static str,
    status: u16,
    body: fn() -> Value,
}

/// `UpdateManagedThing` (PUT `/managed-things/{Identifier}`, 204): `Name` is
/// `^[\p{L}\p{N} ._-]+$`, len 1..128.
fn renamed_managed_thing() -> Value {
    json!({"Name": "thing after the update"})
}

/// `UpdateEventLogConfiguration` (PATCH `/event-log-configurations/{Id}`, 200):
/// `EventLogLevel` is `@required` and one of `LogLevel`
/// (`smithy_query(query="shape",shape="LogLevel")` -> DEBUG | ERROR | INFO | WARN), so the
/// update names a level the create did not.
fn relevelled_event_log_configuration() -> Value {
    json!({"EventLogLevel": "DEBUG"})
}

/// One row: a collection read of this service, the create that seeds it, and the state changes
/// the model lets a client make to a resource of it afterwards.
struct Collection {
    /// What an assertion calls this row.
    case: &'static str,
    /// The collection route the create and the list are bound to; the instance route the read,
    /// the update and the delete are bound to is that route plus the identifier.
    route: &'static str,
    /// The output member the list reports its page on: `Items` for every collection read of
    /// this service except `ListEventLogConfigurations`
    /// (`smithy_query(query="operation",operation="ListEventLogConfigurations")` ->
    /// `EventLogConfigurationList`).
    items_member: &'static str,
    /// The resource-type segment this resource's ARN carries, or `None` where the model gives
    /// the element no ARN at all.
    arn_segment: Option<&'static str>,
    /// The success status of the create, and of the delete that removes a neighbour.
    create_status: u16,
    delete_status: u16,
    /// The create request body for the `n`th resource of this row.
    body: fn(usize) -> Value,
    /// The in-place update of the resource, where the model declares one.
    update: Option<Update>,
    /// Whether the ARN-addressed tag plane (`TagResource`, POST `/tags/{ResourceArn}`)
    /// resolves this kind of resource, so a tag write is one of this row's state changes.
    taggable: bool,
}

/// `CreateCredentialLocker` (POST `/credential-lockers`, 201): `Name` is
/// `^[A-Za-z0-9-_ ]+$`, len 1..128.
fn credential_locker_body(n: usize) -> Value {
    json!({"Name": format!("locker {n}")})
}

/// `CreateManagedThing` (POST `/managed-things`, 201): the three `@required` members, one of
/// `AuthMaterialType` (`smithy_query(query="shape",shape="AuthMaterialType")`).
fn managed_thing_body(n: usize) -> Value {
    json!({
        "Role": "DEVICE",
        "AuthenticationMaterialType": "ZWAVE_QR_BAR_CODE",
        "AuthenticationMaterial": format!("zwave:qr:{n}"),
    })
}

/// `CreateProvisioningProfile` (POST `/provisioning-profiles`, 201): `ProvisioningType` is the
/// one `@required` member, and `ProvisioningProfileName` is `^[0-9A-Za-z_-]+$`, len 1..36.
fn provisioning_profile_body(n: usize) -> Value {
    json!({"ProvisioningType": "FLEET_PROVISIONING", "Name": format!("profile-{n}")})
}

/// `CreateEventLogConfiguration` (POST `/event-log-configurations`, 201): one configuration per
/// target, since a target that already has one is a conflict rather than a second record.
fn event_log_configuration_body(n: usize) -> Value {
    json!({
        "ResourceType": "managed-thing",
        "ResourceId": format!("target{n}"),
        "EventLogLevel": "INFO",
    })
}

/// The rows: a collection read whose element the model gives an ARN and whose resource has no
/// update (a credential locker), one with an ARN and a 204 update (a managed thing), a second
/// ARN-bearing row whose identifier shape admits `-` and `_` (a provisioning profile), and one
/// whose element carries no ARN, whose identifier shape is four times as long, and whose update
/// is a PATCH (an event log configuration).
fn collections() -> Vec<Collection> {
    vec![
        Collection {
            case: "ListCredentialLockers",
            route: "/credential-lockers",
            items_member: "Items",
            arn_segment: Some("credential-locker"),
            create_status: 201,
            // `DeleteCredentialLocker` -> DELETE `/credential-lockers/{Identifier}`, 200.
            delete_status: 200,
            body: credential_locker_body,
            // The model declares no update for a credential locker.
            update: None,
            taggable: true,
        },
        Collection {
            case: "ListManagedThings",
            route: "/managed-things",
            items_member: "Items",
            arn_segment: Some("managed-thing"),
            create_status: 201,
            // `DeleteManagedThing` -> DELETE `/managed-things/{Identifier}`, 204.
            delete_status: 204,
            body: managed_thing_body,
            update: Some(Update {
                method: "PUT",
                status: 204,
                body: renamed_managed_thing,
            }),
            taggable: true,
        },
        Collection {
            case: "ListProvisioningProfiles",
            route: "/provisioning-profiles",
            items_member: "Items",
            arn_segment: Some("provisioning-profile"),
            create_status: 201,
            // `DeleteProvisioningProfile` -> DELETE `/provisioning-profiles/{Identifier}`, 200.
            delete_status: 200,
            body: provisioning_profile_body,
            // The model declares no update for a provisioning profile.
            update: None,
            taggable: true,
        },
        Collection {
            case: "ListEventLogConfigurations",
            route: "/event-log-configurations",
            items_member: "EventLogConfigurationList",
            // `EventLogConfigurationSummary` declares `Id`, `ResourceType`, `ResourceId` and
            // `EventLogLevel`: the model gives an event log configuration no ARN anywhere.
            arn_segment: None,
            create_status: 201,
            // `DeleteEventLogConfiguration` -> DELETE `/event-log-configurations/{Id}`, 200.
            delete_status: 200,
            body: event_log_configuration_body,
            update: Some(Update {
                method: "PATCH",
                status: 200,
                body: relevelled_event_log_configuration,
            }),
            // The `IoTManagedIntegrationsResourceARN` pattern names no event log
            // configuration, so no tagging call can address one.
            taggable: false,
        },
    ]
}

/// Create [`RESOURCES_PER_CASE`] resources of `collection` through the wire, answering the
/// identifiers the creates minted, in creation order.
async fn seed(router: &MultiServiceRouter, collection: &Collection) -> Vec<String> {
    let mut minted = Vec::new();
    for n in 0..RESOURCES_PER_CASE {
        let case = format!("{} create #{n}", collection.case);
        let reply = call(router, "POST", collection.route, Some((collection.body)(n))).await;
        assert_eq!(
            reply.status, collection.create_status,
            "case {case:?}: expected HTTP {} from the create, got {} with body {:?}",
            collection.create_status, reply.status, reply.body
        );
        let id = reply
            .string(ID_MEMBER)
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected the minted identifier on {ID_MEMBER:?}, got body \
                     {:?}",
                    reply.body
                )
            })
            .to_string();
        minted.push(id);
    }
    minted
}

/// RULE (see the module docs), asserted over the whole collection: the list reports one element
/// per surviving resource, in creation order, each carrying the identifier its create minted —
/// and, where the element has an ARN, that identifier inside its own ARN — and the read
/// addressed by the identifier the element reported answers with it.
async fn assert_collection_reports(
    router: &MultiServiceRouter,
    collection: &Collection,
    expected: &[String],
    step: &str,
) {
    let case = format!("{} {step}", collection.case);
    let listed = call(router, "GET", collection.route, None).await;
    assert_eq!(
        listed.status, 200,
        "case {case:?}: expected HTTP 200 from the list, got {} with body {:?}",
        listed.status, listed.body
    );
    let page: Vec<&Value> = listed
        .body
        .get(collection.items_member)
        .and_then(Value::as_array)
        .map(|items| items.iter().collect())
        .unwrap_or_default();
    assert_eq!(
        page.len(),
        expected.len(),
        "case {case:?}: expected {} element(s) on {:?}, got body {:?}",
        expected.len(),
        collection.items_member,
        listed.body
    );

    for (position, (element, minted)) in page.iter().zip(expected).enumerate() {
        let case = format!("{case} element #{position}");
        let reported = element.get(ID_MEMBER).and_then(Value::as_str);
        assert_eq!(
            reported,
            Some(minted.as_str()),
            "case {case:?}: expected the identifier the create minted ({minted:?}) on \
             {ID_MEMBER:?}, got {reported:?}"
        );

        match collection.arn_segment {
            Some(segment) => {
                let expected_arn = resource_arn(segment, minted);
                let reported_arn = element.get("Arn").and_then(Value::as_str);
                assert_eq!(
                    reported_arn,
                    Some(expected_arn.as_str()),
                    "case {case:?}: expected the element's own ARN to embed the identifier it \
                     reports ({expected_arn:?}), got {reported_arn:?}"
                );
            }
            None => assert!(
                element.get("Arn").is_none(),
                "case {case:?}: this element's shape declares no Arn member, got {element:?}"
            ),
        }

        // The identifier the element reports is the handle a later call addresses the resource
        // by: the read of THAT identifier resolves and answers with it.
        let read = call(
            router,
            "GET",
            &format!("{}/{minted}", collection.route),
            None,
        )
        .await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected HTTP 200 reading {minted:?} back, got {} with body {:?}",
            read.status, read.body
        );
        assert_eq!(
            read.string(ID_MEMBER),
            Some(minted.as_str()),
            "case {case:?}: expected the read to report {minted:?} on {ID_MEMBER:?}, got {:?}",
            read.string(ID_MEMBER)
        );
        if let Some(segment) = collection.arn_segment {
            let expected_arn = resource_arn(segment, minted);
            assert_eq!(
                read.string("Arn"),
                Some(expected_arn.as_str()),
                "case {case:?}: expected the read's ARN to embed {minted:?} ({expected_arn:?}), \
                 got {:?}",
                read.string("Arn")
            );
        }
    }
}

/// RULE (see the module docs): no state change that follows a create moves the identifier it
/// minted — an update of the resource, a tag written on it and the removal of a neighbour all
/// leave the collection read reporting that identifier, and that identifier inside the
/// element's own ARN.
#[tokio::test]
async fn state_changes_leave_the_identifier_each_create_minted_in_place() {
    for collection in collections() {
        // One store per row, so each row's page is exactly the resources it created.
        let router = emulator();
        let mut expected = seed(&router, &collection).await;
        assert_collection_reports(&router, &collection, &expected, "after the creates").await;

        // An in-place update of the FIRST resource: it changes a member of the record, never
        // its identity.
        if let Some(update) = &collection.update {
            let target = expected[0].clone();
            let reply = call(
                &router,
                update.method,
                &format!("{}/{target}", collection.route),
                Some((update.body)()),
            )
            .await;
            assert_eq!(
                reply.status, update.status,
                "case {:?}: expected HTTP {} updating {target:?}, got {} with body {:?}",
                collection.case, update.status, reply.status, reply.body
            );
            assert_collection_reports(&router, &collection, &expected, "after the update").await;
        }

        // A tag written on the first resource, addressed by its ARN.
        if collection.taggable {
            let segment = collection
                .arn_segment
                .expect("a resource an ARN-addressed call resolves has an ARN");
            let target = expected[0].clone();
            let arn = resource_arn(segment, &target);
            let reply = call(
                &router,
                "POST",
                &format!("/tags/{}", label(&arn)),
                Some(json!({"Tags": {"owner": "fleet"}})),
            )
            .await;
            assert_eq!(
                reply.status, 200,
                "case {:?}: expected HTTP 200 tagging {arn:?}, got {} with body {:?}",
                collection.case, reply.status, reply.body
            );
            assert_collection_reports(&router, &collection, &expected, "after the tag write")
                .await;
        }

        // The removal of a DIFFERENT resource from the same collection: the page shrinks, and
        // every surviving element still reports its own minted identifier.
        let neighbour = expected.pop().expect("the seed created resources");
        let reply = call(
            &router,
            "DELETE",
            &format!("{}/{neighbour}", collection.route),
            None,
        )
        .await;
        assert_eq!(
            reply.status, collection.delete_status,
            "case {:?}: expected HTTP {} deleting the neighbour {neighbour:?}, got {} with body \
             {:?}",
            collection.case, collection.delete_status, reply.status, reply.body
        );
        assert_collection_reports(
            &router,
            &collection,
            &expected,
            "after a neighbour was deleted",
        )
        .await;
    }
}
