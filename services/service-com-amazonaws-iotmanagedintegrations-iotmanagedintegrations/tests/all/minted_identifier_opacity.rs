// SPDX-License-Identifier: CC0-1.0

//! The identifiers this service MINTS, end to end over the wire.
//!
//! RULE: an identifier this service mints for a resource it creates is OPAQUE — drawn
//! fresh for that resource and derived from nothing else, so it carries neither the
//! creating request, nor the instant of the create, nor the position of the create in any
//! order; two identifiers therefore share no structure beyond their shape, holding one
//! tells a caller nothing about the next, and a second store minting its own first
//! identifier does not mint the same one. Each identifier still matches its own modelled
//! shape, a read reports exactly the value the create answered with, and where the model
//! gives the resource an ARN that ARN's resource segment is that same identifier.
//!
//! The recorded creates this suite was derived from are FIVE rows of the table below: the
//! rows differ along what the rule generalises over — which operation mints the identifier,
//! which output member carries it, how wide the modelled identifier shape is, whether the
//! resource has an ARN, and how many resources one store mints in a row.
//!
//! The same rule holds for a COLLECTION READ: the element a paginated list reports for a
//! resource carries that one minted identifier on its identifier member, the resource
//! segment of that same element's ARN is that identifier too, the read of the resource
//! addressed BY that identifier answers with it, and no page size or token chain changes
//! any of it. [`collections`] is that table, one row per collection read of this service
//! that reports an identifier it minted.
//!
//! Every case drives the real restJson1 request path (`MultiServiceRouter::dispatch`), so
//! the routes, statuses and bodies asserted here are the ones a client sees.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iotmanagedintegrations_iotmanagedintegrations::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region the suite's requests are signed for.
const REGION: &str = "us-east-1";

/// How many resources each row of the table creates in one store. Enough draws for the
/// uniqueness and no-shared-structure assertions to say something, few enough to stay a
/// cheap test.
const CREATES_PER_CASE: usize = 6;

/// The longest prefix two minted identifiers may share, in characters.
///
/// A mint that folded the wall clock or a store counter into its identifiers makes every
/// identifier of one run share a long prefix (or a long suffix); an identifier drawn fresh
/// shares a prefix this long only by a chance of one in 16^8 per pair, so this bound holds
/// for a mint that obeys the rule and fails for one that does not.
const MAX_SHARED_AFFIX: usize = 8;

/// The service namespace every ARN of this service carries, from the model's `sigv4` name
/// (`smithy_query(query="service_info")` -> `"sigv4Name": "iotmanagedintegrations"`), which
/// is also what the ARN patterns of its output shapes require.
const ARN_SERVICE: &str = "iotmanagedintegrations";

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

/// A router holding one `iotmanagedintegrations` service over a fresh shared engine: the
/// same assembly `core/router` performs, with one service mounted.
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
    path: &str,
    body: Option<Value>,
) -> Reply {
    let payload = body.map_or_else(bytes::Bytes::new, |value| {
        bytes::Bytes::from(serde_json::to_vec(&value).expect("serialize the request body"))
    });
    let request = http::Request::builder()
        .method(method)
        .uri(format!(
            "https://iotmanagedintegrations.{REGION}.amazonaws.com{path}"
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

/// One kind of resource whose identifier this service mints, with everything the rule is
/// asserted through: where it is created, which member carries the minted identifier, the
/// modelled shape that identifier must match, where it is read back, and the ARN segment
/// the resource's ARN carries when the model gives it one.
struct Mint {
    case: &'static str,
    /// The collection route the create is bound to.
    collection: &'static str,
    /// The output member the minted identifier is reported on, by both the create and the
    /// read (the model names the same member on both).
    member: &'static str,
    /// The `@length` bounds of the identifier shape, in characters.
    bounds: (usize, usize),
    /// The `@pattern` of the identifier shape, for the assertion messages.
    pattern: &'static str,
    /// The resource-type segment of the resource's ARN, for the resources whose READ
    /// declares an `Arn` output member; `None` for the ones it does not.
    arn_segment: Option<&'static str>,
    /// Whether the CREATE's own output declares that `Arn` member too: the model gives one
    /// to some of these creates and leaves it to the read for others.
    create_reports_arn: bool,
    /// The create request body for the `n`th resource of this case.
    body: fn(usize) -> Value,
}

/// `CreateCredentialLocker` (POST `/credential-lockers`): `Name` is the only member worth
/// varying here.
fn credential_locker_body(n: usize) -> Value {
    json!({"Name": format!("locker {n}")})
}

/// `CreateManagedThing` (POST `/managed-things`): the three `@required` members.
fn managed_thing_body(n: usize) -> Value {
    json!({
        "Role": "DEVICE",
        // One of `AuthMaterialType`
        // (`smithy_query(query="shape",shape="AuthMaterialType")`).
        "AuthenticationMaterialType": "ZWAVE_QR_BAR_CODE",
        "AuthenticationMaterial": format!("zwave:qr:{n}"),
    })
}

/// `CreateEventLogConfiguration` (POST `/event-log-configurations`): one configuration per
/// target, since a target that already has one is a conflict rather than a second record.
fn event_log_configuration_body(n: usize) -> Value {
    json!({
        // One of `SmartHomeResourceType`, and `LogLevel`
        // (`smithy_query(query="shape",shape="LogLevel")` -> DEBUG | ERROR | INFO | WARN).
        "ResourceType": "managed-thing",
        "ResourceId": format!("target{n}"),
        "EventLogLevel": "INFO",
    })
}

/// `CreateOtaTaskConfiguration` (POST `/ota-task-configurations`): the model marks no member
/// `@required`, so a name is all this needs.
fn ota_task_configuration_body(n: usize) -> Value {
    json!({"Name": format!("ota config {n}")})
}

/// `StartDeviceDiscovery` (POST `/device-discoveries`): a hub discovery names the controller
/// whose network it searches.
fn device_discovery_body(n: usize) -> Value {
    json!({
        // One of `DiscoveryType`
        // (`smithy_query(query="shape",shape="DiscoveryType")`); the hub protocols name a
        // controller.
        "DiscoveryType": "ZWAVE",
        "ControllerIdentifier": format!("controller{n}"),
    })
}

/// Every operation of this service that mints a resource identifier through one request,
/// with the shapes the model declares for those identifiers
/// (`smithy_query(query="shape",shape="CredentialLockerId,ManagedThingId,
/// LogConfigurationId,OtaTaskConfigurationId,DeviceDiscoveryId")`).
fn mints() -> Vec<Mint> {
    vec![
        Mint {
            case: "CreateCredentialLocker",
            collection: "/credential-lockers",
            member: "Id",
            bounds: (1, 64),
            pattern: "^[a-zA-Z0-9]*$",
            arn_segment: Some("credential-locker"),
            create_reports_arn: true,
            body: credential_locker_body,
        },
        Mint {
            case: "CreateManagedThing",
            collection: "/managed-things",
            member: "Id",
            bounds: (1, 64),
            pattern: "^[a-zA-Z0-9:_-]*$",
            arn_segment: Some("managed-thing"),
            create_reports_arn: true,
            body: managed_thing_body,
        },
        Mint {
            case: "CreateEventLogConfiguration",
            collection: "/event-log-configurations",
            member: "Id",
            bounds: (1, 200),
            pattern: "^[A-Za-z0-9]+$",
            arn_segment: None,
            create_reports_arn: false,
            body: event_log_configuration_body,
        },
        Mint {
            case: "CreateOtaTaskConfiguration",
            collection: "/ota-task-configurations",
            member: "TaskConfigurationId",
            bounds: (1, 64),
            pattern: "^[a-zA-Z0-9]*$",
            arn_segment: None,
            create_reports_arn: false,
            body: ota_task_configuration_body,
        },
        Mint {
            case: "StartDeviceDiscovery",
            collection: "/device-discoveries",
            member: "Id",
            bounds: (1, 200),
            pattern: "^[A-Za-z0-9]+$",
            // `StartDeviceDiscovery` answers `Id` and `StartedAt` only; the job's `Arn` is
            // a `@required` member of `GetDeviceDiscovery`'s output.
            arn_segment: Some("device-discovery"),
            create_reports_arn: false,
            body: device_discovery_body,
        },
    ]
}

/// Create the `n`th resource of `mint` through `router`, asserting the modelled 201 and the
/// modelled shape of the identifier it minted, and answer that identifier with the ARN the
/// create reported (when the model gives the resource one).
async fn minted(
    router: &MultiServiceRouter,
    mint: &Mint,
    n: usize,
) -> (String, Option<String>) {
    let case = format!("{} #{n}", mint.case);
    let reply = call(router, "POST", mint.collection, Some((mint.body)(n))).await;
    assert_eq!(
        reply.status, 201,
        "case {case:?}: expected HTTP 201, got {} with body {:?}",
        reply.status, reply.body
    );
    let id = reply
        .string(mint.member)
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected the minted identifier on {:?}, got body {:?}",
                mint.member, reply.body
            )
        })
        .to_string();
    let (min, max) = mint.bounds;
    assert!(
        (min..=max).contains(&id.chars().count())
            && id.chars().all(|c| c.is_ascii_alphanumeric()),
        "case {case:?}: expected an identifier of {min}..={max} characters matching {:?}, \
         got {id:?}",
        mint.pattern
    );
    (id, reply.string("Arn").map(str::to_string))
}

/// Assert `arn` is this service's ARN for `segment` in [`REGION`] and that its resource
/// segment is exactly `id`: a client that parses the ARN gets back the identifier the create
/// minted, and nothing else.
fn assert_arn_carries(case: &str, arn: &str, segment: &str, id: &str) {
    let prefix = format!("arn:aws:{ARN_SERVICE}:{REGION}:");
    assert!(
        arn.starts_with(&prefix),
        "case {case:?}: expected an ARN starting {prefix:?}, got {arn:?}"
    );
    let resource = arn.split(':').next_back().unwrap_or_default();
    assert_eq!(
        resource,
        format!("{segment}/{id}"),
        "case {case:?}: expected the ARN resource {:?}, got {resource:?} in {arn:?}",
        format!("{segment}/{id}")
    );
}

/// The length of the longest prefix `left` and `right` share, in characters.
fn shared_prefix(left: &str, right: &str) -> usize {
    left.chars()
        .zip(right.chars())
        .take_while(|(l, r)| l == r)
        .count()
}

/// The length of the longest suffix `left` and `right` share, in characters.
fn shared_suffix(left: &str, right: &str) -> usize {
    left.chars()
        .rev()
        .zip(right.chars().rev())
        .take_while(|(l, r)| l == r)
        .count()
}

/// RULE (see the module docs): every minted identifier matches its modelled shape, is
/// reported unchanged by the read and by the resource's ARN, and is drawn fresh — distinct
/// from every other identifier the service minted, carrying no creation position and sharing
/// no long prefix or suffix with any other.
#[tokio::test]
async fn minted_identifiers_are_opaque_distinct_and_read_back_unchanged() {
    let router = emulator();

    // Every identifier the whole table minted, so the no-shared-structure assertions span
    // the resources of one case AND the resources of different cases: a mint that folded the
    // clock in would make identifiers of DIFFERENT kinds share a prefix too.
    let mut minted_ids: Vec<(String, String)> = Vec::new();

    for mint in mints() {
        for n in 0..CREATES_PER_CASE {
            let case = format!("{} #{n}", mint.case);
            let (id, created_arn) = minted(&router, &mint, n).await;

            // The read reports exactly what the create answered with, on the same member.
            let read = call(
                &router,
                "GET",
                &format!("{}/{id}", mint.collection),
                None,
            )
            .await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: expected HTTP 200 reading back {id:?}, got {} with body {:?}",
                read.status, read.body
            );
            assert_eq!(
                read.string(mint.member),
                Some(id.as_str()),
                "case {case:?}: expected the read to report {id:?} on {:?}, got {:?}",
                mint.member,
                read.string(mint.member)
            );

            // Where the model gives the resource an ARN, that ARN's resource segment is the
            // identifier — on the create's answer and on the read's alike.
            if let Some(segment) = mint.arn_segment {
                let read_arn = read.string("Arn").unwrap_or_default().to_string();
                assert_arn_carries(&format!("{case} (read ARN)"), &read_arn, segment, &id);
            }
            match (mint.create_reports_arn, created_arn.as_deref()) {
                (true, Some(arn)) => assert_arn_carries(
                    &format!("{case} (create ARN)"),
                    arn,
                    mint.arn_segment.unwrap_or_default(),
                    &id,
                ),
                (true, None) => {
                    panic!("case {case:?}: expected the create to report an Arn, got none")
                }
                (false, reported) => assert!(
                    reported.is_none(),
                    "case {case:?}: the create's output declares no Arn member, got \
                     {reported:?}"
                ),
            }

            // The identifier carries no creation position: a mint that appended its store
            // counter would end in exactly this.
            let counter = format!("{n:08x}");
            assert!(
                !id.ends_with(&counter),
                "case {case:?}: expected an identifier carrying no creation counter, got \
                 {id:?} ending in {counter:?}"
            );

            minted_ids.push((case, id));
        }
    }

    let distinct: BTreeSet<&str> = minted_ids.iter().map(|(_, id)| id.as_str()).collect();
    assert_eq!(
        distinct.len(),
        minted_ids.len(),
        "expected {} distinct identifiers, got {} across {:?}",
        minted_ids.len(),
        distinct.len(),
        minted_ids.iter().map(|(_, id)| id).collect::<Vec<_>>()
    );

    for (left_index, (left_case, left)) in minted_ids.iter().enumerate() {
        for (right_case, right) in minted_ids.iter().skip(left_index + 1) {
            let prefix = shared_prefix(left, right);
            assert!(
                prefix <= MAX_SHARED_AFFIX,
                "case {left_case:?} vs {right_case:?}: expected identifiers sharing at most \
                 {MAX_SHARED_AFFIX} leading characters, got {prefix} shared by {left:?} and \
                 {right:?}"
            );
            let suffix = shared_suffix(left, right);
            assert!(
                suffix <= MAX_SHARED_AFFIX,
                "case {left_case:?} vs {right_case:?}: expected identifiers sharing at most \
                 {MAX_SHARED_AFFIX} trailing characters, got {suffix} shared by {left:?} and \
                 {right:?}"
            );
        }
    }
}

/// RULE (see the module docs): an identifier is drawn fresh rather than derived from the
/// state of the store, so the FIRST resource two independent stores create does not get the
/// same identifier — which is what a mint counting creates, or seeding itself from anything
/// both stores share, would produce.
#[tokio::test]
async fn two_stores_mint_different_identifiers_for_their_first_resource() {
    let (first, second) = (emulator(), emulator());

    for mint in mints() {
        let (left, _) = minted(&first, &mint, 0).await;
        let (right, _) = minted(&second, &mint, 0).await;
        assert_ne!(
            left, right,
            "case {:?}: expected two stores to mint different identifiers for their first \
             resource, got {left:?} from both",
            mint.case
        );
        let prefix = shared_prefix(&left, &right);
        assert!(
            prefix <= MAX_SHARED_AFFIX,
            "case {:?}: expected the two stores' identifiers to share at most \
             {MAX_SHARED_AFFIX} leading characters, got {prefix} shared by {left:?} and \
             {right:?}",
            mint.case
        );
    }
}

/// RULE (see the module docs): the dedicated label `RegisterCustomEndpoint` mints into the
/// account's endpoint address is minted the same way — fresh per store, carrying no creation
/// counter — while the register itself stays a get-or-create, so repeating it in one store
/// reports the address the first call minted.
#[tokio::test]
async fn a_custom_endpoint_label_is_minted_fresh_per_store() {
    let (first, second) = (emulator(), emulator());
    let mut addresses: Vec<String> = Vec::new();

    for (case, router) in [("first store", &first), ("second store", &second)] {
        let reply = call(router, "POST", "/custom-endpoint", Some(json!({}))).await;
        assert_eq!(
            reply.status, 201,
            "case {case:?}: expected HTTP 201, got {} with body {:?}",
            reply.status, reply.body
        );
        let address = reply
            .string("EndpointAddress")
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected an EndpointAddress, got body {:?}",
                    reply.body
                )
            })
            .to_string();
        // `EndpointAddress`: len 1..256, pattern ^[A-Za-z0-9._@-]+$
        // (`smithy_query(query="shape",shape="EndpointAddress")`).
        assert!(
            (1..=256).contains(&address.chars().count())
                && address
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._@-".contains(c)),
            "case {case:?}: expected an EndpointAddress matching ^[A-Za-z0-9._@-]+$ of \
             1..=256 characters, got {address:?}"
        );
        let label = address.split('.').next().unwrap_or_default();
        assert!(
            !label.ends_with("00000000"),
            "case {case:?}: expected a label carrying no creation counter, got {label:?}"
        );

        // Get-or-create: the second register in the SAME store reports the first address.
        let again = call(router, "POST", "/custom-endpoint", Some(json!({}))).await;
        assert_eq!(
            again.string("EndpointAddress"),
            Some(address.as_str()),
            "case {case:?}: expected the register to report the address it minted, got {:?}",
            again.string("EndpointAddress")
        );
        addresses.push(address);
    }

    assert_ne!(
        addresses[0], addresses[1],
        "expected two stores to mint different endpoint labels, got {:?} from both",
        addresses[0]
    );
}

/// How many resources each row of [`collections`] creates in one store: enough for the page
/// sizes below to divide the store evenly and unevenly, few enough to stay a cheap test.
const RESOURCES_PER_COLLECTION: usize = 4;

/// The `@range` maximum of `MaxResults`, which every collection read of this service shares:
/// `smithy_query(query="operation",operation="ListCredentialLockers,ListManagedThings,
/// ListProvisioningProfiles,ListAccountAssociations,ListDeviceDiscoveries,
/// ListEventLogConfigurations")` reports `MaxResults` range 1..1000 for every one of them, so
/// no row of [`collections`] is paged at a size its own operation would refuse.
const MAX_RESULTS_MAXIMUM: usize = 1000;

/// What a collection row's create needs in the store before it can run.
#[derive(Clone, Copy, Debug)]
enum Prerequisite {
    /// Nothing: the create stands on its own.
    Nothing,
    /// A connector destination, which `CreateAccountAssociation` resolves its `@required`
    /// `ConnectorDestinationId` against.
    ConnectorDestination,
}

impl Prerequisite {
    /// How many resources this prerequisite creates in the store before the row's own creates
    /// run, so a check for a store counter folded into an identifier knows which count that
    /// identifier would carry.
    fn creates(self) -> usize {
        match self {
            Self::Nothing => 0,
            // The C2C connector and the destination that names it.
            Self::ConnectorDestination => 2,
        }
    }
}

/// One collection read of this service that reports an identifier the service minted, with
/// everything the rule is asserted through: where the resources are created and listed,
/// which member of the element carries the identifier, the modelled shape that identifier
/// must match, and the ARN whose resource segment must embed it.
struct Collection {
    case: &'static str,
    /// The collection route the creates post to and the list reads; the resource's own read
    /// is that route plus the identifier.
    route: &'static str,
    /// The output member the list reports its elements on: `Items` for every collection read
    /// of this service except `ListEventLogConfigurations`
    /// (`smithy_query(query="operation",operation="ListEventLogConfigurations")` ->
    /// `EventLogConfigurationList`).
    items_member: &'static str,
    /// The member carrying the identifier — on the create's output, on the list element and
    /// on the resource's own read alike, since the model names the same member on all three.
    id_member: &'static str,
    /// The `@length` bounds of the identifier shape, in characters.
    bounds: (usize, usize),
    /// The characters beyond ASCII alphanumerics that identifier's `@pattern` admits.
    extra_pattern_chars: &'static str,
    /// The `@pattern` itself, for the assertion messages.
    pattern: &'static str,
    /// The resource-type segment of the resource's ARN, for the resources the model gives
    /// one; `None` for the ones it does not model an ARN for at all.
    arn_segment: Option<&'static str>,
    /// Whether the LIST ELEMENT declares that `Arn` member: the model gives one to some of
    /// these summaries and leaves the ARN to the resource's own read for the others.
    element_declares_arn: bool,
    /// What the create needs in the store first.
    prerequisite: Prerequisite,
    /// The create request body for the `n`th resource of this row, given the identifier its
    /// prerequisite minted (empty when the row needs none).
    body: fn(usize, &str) -> Value,
}

/// `CreateCredentialLocker` (POST `/credential-lockers`).
fn credential_locker_element(n: usize, _prerequisite: &str) -> Value {
    credential_locker_body(n)
}

/// `CreateManagedThing` (POST `/managed-things`).
fn managed_thing_element(n: usize, _prerequisite: &str) -> Value {
    managed_thing_body(n)
}

/// `CreateProvisioningProfile` (POST `/provisioning-profiles`): `ProvisioningType` is the
/// one `@required` member (`smithy_query(query="shape",shape="ProvisioningType")`), and
/// `ProvisioningProfileName` is `^[0-9A-Za-z_-]+$`, len 1..36.
fn provisioning_profile_element(n: usize, _prerequisite: &str) -> Value {
    json!({"ProvisioningType": "FLEET_PROVISIONING", "Name": format!("profile-{n}")})
}

/// `CreateAccountAssociation` (POST `/account-associations`): `ConnectorDestinationId` is
/// `@required` and is resolved against this service's own connector-destination store, so
/// every row names the destination its prerequisite created.
fn account_association_element(n: usize, prerequisite: &str) -> Value {
    json!({
        "ConnectorDestinationId": prerequisite,
        // `AccountAssociationName` is `^[A-Za-z0-9-_ ]+$`, len 1..128.
        "Name": format!("association {n}"),
    })
}

/// `StartDeviceDiscovery` (POST `/device-discoveries`).
fn device_discovery_element(n: usize, _prerequisite: &str) -> Value {
    device_discovery_body(n)
}

/// `CreateEventLogConfiguration` (POST `/event-log-configurations`).
fn event_log_configuration_element(n: usize, _prerequisite: &str) -> Value {
    event_log_configuration_body(n)
}

/// Every collection read this service implements that reports an identifier it minted, with
/// the shapes the model declares for those identifiers and for the ARNs that embed them
/// (`smithy_query(query="shape",shape="CredentialLockerSummary,ManagedThingSummary,
/// ProvisioningProfileSummary,AccountAssociationItem,DeviceDiscoverySummary,
/// EventLogConfigurationSummary")`).
///
/// The rows differ along what the rule generalises over: the operation, the member the
/// identifier is reported on, the member the page is reported on, how wide the modelled
/// identifier shape is, whether the element carries an ARN, whether the resource has an ARN
/// at all, and whether the create needs another resource first.
fn collections() -> Vec<Collection> {
    vec![
        Collection {
            case: "ListCredentialLockers",
            route: "/credential-lockers",
            items_member: "Items",
            id_member: "Id",
            bounds: (1, 64),
            extra_pattern_chars: "",
            pattern: "^[a-zA-Z0-9]*$",
            arn_segment: Some("credential-locker"),
            element_declares_arn: true,
            prerequisite: Prerequisite::Nothing,
            body: credential_locker_element,
        },
        Collection {
            case: "ListManagedThings",
            route: "/managed-things",
            items_member: "Items",
            id_member: "Id",
            bounds: (1, 64),
            extra_pattern_chars: ":_-",
            pattern: "^[a-zA-Z0-9:_-]*$",
            arn_segment: Some("managed-thing"),
            element_declares_arn: true,
            prerequisite: Prerequisite::Nothing,
            body: managed_thing_element,
        },
        Collection {
            case: "ListProvisioningProfiles",
            route: "/provisioning-profiles",
            items_member: "Items",
            id_member: "Id",
            bounds: (1, 64),
            extra_pattern_chars: "-_",
            pattern: "^[A-Za-z0-9-_]+$",
            arn_segment: Some("provisioning-profile"),
            element_declares_arn: true,
            prerequisite: Prerequisite::Nothing,
            body: provisioning_profile_element,
        },
        Collection {
            case: "ListAccountAssociations",
            route: "/account-associations",
            items_member: "Items",
            // The only row whose identifier member is not `Id`: the model names it
            // `AccountAssociationId` on the create, on the element and on the read.
            id_member: "AccountAssociationId",
            bounds: (1, 64),
            extra_pattern_chars: "",
            pattern: "^[0-9a-zA-Z]+$",
            arn_segment: Some("account-association"),
            element_declares_arn: true,
            prerequisite: Prerequisite::ConnectorDestination,
            body: account_association_element,
        },
        Collection {
            case: "ListDeviceDiscoveries",
            route: "/device-discoveries",
            items_member: "Items",
            id_member: "Id",
            bounds: (1, 200),
            extra_pattern_chars: "",
            pattern: "^[A-Za-z0-9]+$",
            // `DeviceDiscoverySummary` declares `Id`, `DiscoveryType` and `Status` only;
            // the job's `Arn` is a `@required` member of `GetDeviceDiscovery`'s output.
            arn_segment: Some("device-discovery"),
            element_declares_arn: false,
            prerequisite: Prerequisite::Nothing,
            body: device_discovery_element,
        },
        Collection {
            case: "ListEventLogConfigurations",
            route: "/event-log-configurations",
            items_member: "EventLogConfigurationList",
            id_member: "Id",
            bounds: (1, 200),
            extra_pattern_chars: "",
            pattern: "^[A-Za-z0-9]+$",
            // The model gives an event log configuration no ARN, on the element or anywhere
            // else.
            arn_segment: None,
            element_declares_arn: false,
            prerequisite: Prerequisite::Nothing,
            body: event_log_configuration_element,
        },
    ]
}

/// Create a C2C connector and a connector destination against it, answering the identifier
/// the destination minted.
///
/// `CreateAccountAssociation` resolves its `ConnectorDestinationId` against this service's
/// own connector-destination store, so the associations below are created against a
/// destination a client really created; a destination in turn names a C2C connector
/// (`CloudConnectorId` is `@required`), so the connector comes first.
async fn connector_destination(router: &MultiServiceRouter) -> String {
    let connector = call(
        router,
        "POST",
        "/cloud-connectors",
        Some(json!({
            "Name": "partner cloud connector",
            "EndpointConfig": {"lambda": {"arn": "connector-handler"}},
        })),
    )
    .await;
    assert_eq!(
        connector.status, 201,
        "seeding a cloud connector: expected HTTP 201, got {} with body {:?}",
        connector.status, connector.body
    );
    let destination = call(
        router,
        "POST",
        "/connector-destinations",
        Some(json!({
            "CloudConnectorId": connector.string("Id").unwrap_or_default(),
            "AuthConfig": {},
            // Inside the `SecretsManagerArn` `@pattern` and the `SecretsManagerVersionId`
            // `@length` (32..64).
            "SecretsManager": {
                "arn": format!(
                    "arn:aws:secretsmanager:{REGION}:{}:secret:destination-A1b2C3",
                    engine::EMULATOR_ACCOUNT_ID
                ),
                "versionId": "a".repeat(32),
            },
        })),
    )
    .await;
    assert_eq!(
        destination.status, 201,
        "seeding a connector destination: expected HTTP 201, got {} with body {:?}",
        destination.status, destination.body
    );
    destination.string("Id").unwrap_or_default().to_string()
}

/// `GET` the collection route with the raw `query` (empty for none).
async fn list(router: &MultiServiceRouter, collection: &Collection, query: &str) -> Reply {
    call(
        router,
        "GET",
        &format!("{}{query}", collection.route),
        None,
    )
    .await
}

/// The elements a list answer reports, on the member the model names for this collection.
fn elements_of<'a>(reply: &'a Reply, collection: &Collection) -> Vec<&'a Value> {
    reply
        .body
        .get(collection.items_member)
        .and_then(Value::as_array)
        .map(|items| items.iter().collect())
        .unwrap_or_default()
}

/// One string member of a list element, absent members and nulls alike reported as `None`.
fn element_string<'a>(element: &'a Value, member: &str) -> Option<&'a str> {
    element
        .get(member)
        .filter(|value| !value.is_null())
        .and_then(Value::as_str)
}

/// The identifier a list element reports, which every row must carry.
fn element_id<'a>(case: &str, element: &'a Value, collection: &Collection) -> &'a str {
    element_string(element, collection.id_member).unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the element to report its identifier on {:?}, got \
             {element:?}",
            collection.id_member
        )
    })
}

/// Assert `id` matches the modelled identifier shape of `collection`.
fn assert_identifier_shape(case: &str, collection: &Collection, id: &str) {
    let (min, max) = collection.bounds;
    assert!(
        (min..=max).contains(&id.chars().count())
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || collection.extra_pattern_chars.contains(c)),
        "case {case:?}: expected an identifier of {min}..={max} characters matching {:?}, got \
         {id:?}",
        collection.pattern
    );
}

/// Assert the element's own ARN — where the model declares one on the element — embeds the
/// identifier that SAME element reports, and answer it.
fn assert_element_arn_carries_its_id<'a>(
    case: &str,
    element: &'a Value,
    collection: &Collection,
    id: &str,
) -> Option<&'a str> {
    match (collection.element_declares_arn, collection.arn_segment) {
        (true, Some(segment)) => {
            let arn = element_string(element, "Arn").unwrap_or_else(|| {
                panic!("case {case:?}: expected the element to report an Arn, got {element:?}")
            });
            assert_arn_carries(&format!("{case} (element ARN)"), arn, segment, id);
            Some(arn)
        }
        _ => {
            assert!(
                element_string(element, "Arn").is_none(),
                "case {case:?}: this element's shape declares no Arn member, got {element:?}"
            );
            None
        }
    }
}

/// Create [`RESOURCES_PER_COLLECTION`] resources of `collection` through the wire, answering
/// the identifiers the creates minted in creation order.
async fn seed(router: &MultiServiceRouter, collection: &Collection) -> Vec<String> {
    let prerequisite = match collection.prerequisite {
        Prerequisite::Nothing => String::new(),
        Prerequisite::ConnectorDestination => connector_destination(router).await,
    };

    let mut minted: Vec<String> = Vec::new();
    for n in 0..RESOURCES_PER_COLLECTION {
        let case = format!("{} create #{n}", collection.case);
        let reply = call(
            router,
            "POST",
            collection.route,
            Some((collection.body)(n, &prerequisite)),
        )
        .await;
        assert_eq!(
            reply.status, 201,
            "case {case:?}: expected HTTP 201 from the create, got {} with body {:?}",
            reply.status, reply.body
        );
        let id = reply
            .string(collection.id_member)
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected the minted identifier on {:?}, got body {:?}",
                    collection.id_member, reply.body
                )
            })
            .to_string();
        assert_identifier_shape(&case, collection, &id);
        minted.push(id);
    }

    let distinct: BTreeSet<&str> = minted.iter().map(String::as_str).collect();
    assert_eq!(
        distinct.len(),
        minted.len(),
        "case {:?}: expected {} distinct identifiers from {} creates, got {} across {minted:?}",
        collection.case,
        minted.len(),
        minted.len(),
        distinct.len()
    );
    minted
}

/// RULE (see the module docs): every element of a collection read reports, on its identifier
/// member, exactly the identifier the create minted for that resource; the resource segment
/// of that same element's ARN is that identifier; the resource's own read, addressed BY the
/// identifier the element reported, answers with it and with the same ARN; and repeating the
/// list reports the same identifiers, since a list is a pure read.
#[tokio::test]
async fn a_collection_read_reports_the_identifier_each_create_minted() {
    for collection in collections() {
        // One store per row, so each row's page is exactly the resources it created.
        let router = emulator();
        let minted = seed(&router, &collection).await;

        let listed = list(&router, &collection, "").await;
        assert_eq!(
            listed.status, 200,
            "case {:?}: expected HTTP 200 from the list, got {} with body {:?}",
            collection.case, listed.status, listed.body
        );
        let page = elements_of(&listed, &collection);
        assert_eq!(
            page.len(),
            minted.len(),
            "case {:?}: expected one element per created resource on {:?}, got body {:?}",
            collection.case,
            collection.items_member,
            listed.body
        );

        for (position, (element, created)) in page.iter().zip(minted.iter()).enumerate() {
            let case = format!("{} element #{position}", collection.case);
            let id = element_id(&case, element, &collection);
            assert_eq!(
                id, created,
                "case {case:?}: expected the element to report the identifier the create \
                 minted ({created:?}), got {id:?}"
            );
            assert_identifier_shape(&case, &collection, id);
            let element_arn = assert_element_arn_carries_its_id(&case, element, &collection, id);

            // The identifier the element reports is the handle a later call addresses the
            // resource by: the read of THAT identifier answers with it.
            let read = call(
                &router,
                "GET",
                &format!("{}/{id}", collection.route),
                None,
            )
            .await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: expected HTTP 200 reading {id:?} back, got {} with body {:?}",
                read.status, read.body
            );
            assert_eq!(
                read.string(collection.id_member),
                Some(id),
                "case {case:?}: expected the read to report {id:?} on {:?}, got {:?}",
                collection.id_member,
                read.string(collection.id_member)
            );

            // Where the resource has an ARN, the read's ARN embeds that same identifier, and
            // an element that carries an ARN carries exactly the read's.
            match collection.arn_segment {
                Some(segment) => {
                    let read_arn = read.string("Arn").unwrap_or_else(|| {
                        panic!(
                            "case {case:?}: expected the read to report an Arn, got body {:?}",
                            read.body
                        )
                    });
                    assert_arn_carries(&format!("{case} (read ARN)"), read_arn, segment, id);
                    if let Some(arn) = element_arn {
                        assert_eq!(
                            arn, read_arn,
                            "case {case:?}: expected the element's Arn to be the one the read \
                             reports ({read_arn:?}), got {arn:?}"
                        );
                    }
                }
                None => assert!(
                    read.string("Arn").is_none(),
                    "case {case:?}: the model declares no Arn for this resource, got {:?}",
                    read.string("Arn")
                ),
            }
        }

        // A list is a pure read: repeating it reports the same identifiers.
        let relisted = list(&router, &collection, "").await;
        let again: Vec<&str> = elements_of(&relisted, &collection)
            .iter()
            .enumerate()
            .map(|(position, element)| {
                element_id(
                    &format!("{} reread #{position}", collection.case),
                    element,
                    &collection,
                )
            })
            .collect();
        assert_eq!(
            again,
            minted.iter().map(String::as_str).collect::<Vec<_>>(),
            "case {:?}: expected a reread to report the same identifiers, got {again:?}",
            collection.case
        );

        // The identifiers a PAGE serves are opaque on the same terms the create side is held
        // to above: a mint that folded the wall clock into them would make the identifiers one
        // page reports share a long prefix, and one that appended its store counter would end
        // each in that counter — so a client enumerating a collection can read no creation
        // instant and no creation order out of the page it was served.
        for (position, listed) in again.iter().enumerate() {
            let case = format!("{} element #{position}", collection.case);
            // The count this resource's create saw, prerequisite creates included, which is
            // what a mint appending its store counter would have appended here.
            let counter = format!("{:08x}", collection.prerequisite.creates() + position);
            assert!(
                !listed.ends_with(&counter),
                "case {case:?}: expected a listed identifier carrying no creation counter, got \
                 {listed:?} ending in {counter:?}"
            );
            for other in again.iter().skip(position + 1) {
                let prefix = shared_prefix(listed, other);
                assert!(
                    prefix <= MAX_SHARED_AFFIX,
                    "case {case:?}: expected listed identifiers sharing at most \
                     {MAX_SHARED_AFFIX} leading characters, got {prefix} shared by {listed:?} \
                     and {other:?}"
                );
                let suffix = shared_suffix(listed, other);
                assert!(
                    suffix <= MAX_SHARED_AFFIX,
                    "case {case:?}: expected listed identifiers sharing at most \
                     {MAX_SHARED_AFFIX} trailing characters, got {suffix} shared by {listed:?} \
                     and {other:?}"
                );
            }
        }
    }
}

/// RULE (see the module docs): paging changes no identifier — whatever page size a client
/// asks for, the token chain reports exactly the identifiers the creates minted, in creation
/// order, each still matching its modelled shape and still embedded in its own element's
/// ARN.
///
/// Each resource's own lifecycle suite already pins that its token chain reports its records
/// once, in insertion order; what is pinned here is the identifier INSIDE each element of
/// those pages — that no page size and no resumed cursor can hand a client an element whose
/// identifier is not the minted one, or whose ARN embeds a different one — over every
/// collection read of this service at once.
#[tokio::test]
async fn paging_a_collection_read_changes_no_identifier() {
    for collection in collections() {
        let router = emulator();
        let minted = seed(&router, &collection).await;

        // The page sizes: the modelled minimum (a page per record), a size that divides the
        // store unevenly (a part-full last page), and the modelled maximum (one page for the
        // whole store). Each resource's own lifecycle suite sweeps the sizes between them, so
        // this walk pays only for what the element-level checks need.
        for page_size in [1, 3, MAX_RESULTS_MAXIMUM] {
            let case = format!("{} MaxResults={page_size}", collection.case);
            let mut seen: Vec<String> = Vec::new();
            let mut token: Option<String> = None;
            // Bounded by the PAGES walked, not by the elements seen, so a page that answered
            // a token while reporting nothing fails this test instead of looping forever.
            let mut pages = 0usize;
            loop {
                let query = match &token {
                    None => format!("?MaxResults={page_size}"),
                    Some(next) => format!("?MaxResults={page_size}&NextToken={next}"),
                };
                let reply = list(&router, &collection, &query).await;
                assert_eq!(
                    reply.status, 200,
                    "case {case:?}: expected HTTP 200, got {} with body {:?}",
                    reply.status, reply.body
                );
                let page = elements_of(&reply, &collection);
                assert!(
                    page.len() <= page_size,
                    "case {case:?}: expected at most {page_size} elements, got {:?}",
                    reply.body
                );
                for element in page {
                    let id = element_id(&case, element, &collection);
                    assert_identifier_shape(&case, &collection, id);
                    assert_element_arn_carries_its_id(&case, element, &collection, id);
                    seen.push(id.to_string());
                }
                pages += 1;
                assert!(
                    pages <= minted.len() + 1,
                    "case {case:?}: the token chain did not terminate within {} pages, saw \
                     {seen:?}",
                    minted.len() + 1
                );
                match reply.string("NextToken") {
                    Some(next) => token = Some(next.to_string()),
                    None => break,
                }
            }
            assert_eq!(
                seen, minted,
                "case {case:?}: expected the token chain to report exactly the identifiers the \
                 creates minted, got {seen:?}"
            );
        }
    }
}

