// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `ExportVerifiedAccessInstanceClientConfiguration`, driven over the
//! wire the service actually speaks — `ec2Query`, through the crate's shared
//! [`super::ec2_query`] harness.
//!
//! The rule under test: the exported configuration is DERIVED from what the store holds for
//! the named Verified Access instance at the instant of the call — never from the request and
//! never from a fixed document. Around that:
//!
//! 1. the kinds of device trust provider the instance has attached are what
//!    `deviceTrustProviderSet` reports, each kind named once and in the order the first
//!    provider of it was attached, and the user trust provider's own client configuration is
//!    read off the STORED trust provider, so every endpoint it reports is one the account
//!    configured;
//! 2. the configuration follows the store: attaching and detaching a trust provider changes
//!    what the next export reports, and an instance with nothing attached exports an empty
//!    configuration rather than a refusal;
//! 3. each network CIDR endpoint of the instance is one OpenVPN client configuration, whose
//!    route is that endpoint's own CIDR and whose base64 document dials that endpoint's
//!    generated DNS name; the endpoints of the other kinds, and the endpoints of another
//!    instance, contribute none;
//! 4. the `region` reported is the Region the request was signed for;
//! 5. an id that names no instance of the account is refused with that resource's own
//!    `Invalid<Kind>.NotFound` code and sentence, a request that names no instance at all is
//!    refused as the absent parameter it is, and a `DryRun` reports the outcome the request
//!    would have had;
//! 6. the export is a READ: exporting twice reports the same configuration and leaves the
//!    instance exactly as it was.
//!
//! Each table row is a distinct input along what the rule generalises over — which kind of
//! trust provider is attached (every kind the model's `USER_TRUST_PROVIDER_TYPE_VALUES` and
//! `DEVICE_TRUST_PROVIDER_TYPE_VALUES` catalogs publish), how many are attached and in which
//! order, how many CIDR endpoints the instance has and what they route, which Region the call
//! is signed for, which form an unresolvable id takes — so no row is the specification.
//!
//! The VPC resources a network CIDR endpoint names are seeded through the same insert seams
//! the creating operations land on, the way the [`super::verified_access_endpoints`] suite
//! seeds them.

use std::collections::BTreeSet;

use ec2::ops::types::{DEVICE_TRUST_PROVIDER_TYPE_VALUES, USER_TRUST_PROVIDER_TYPE_VALUES};
use ec2::state::{
    availability_zones_in_region, ComAmazonawsEc2Amazonec2State as Ec2State, SecurityGroupSpec,
    SubnetSpec, REDACTED_CLIENT_SECRET, TRUST_PROVIDER_TYPE_DEVICE, TRUST_PROVIDER_TYPE_USER,
    USER_TRUST_PROVIDER_TYPE_OIDC, VERIFIED_ACCESS_ENDPOINT_TYPE_CIDR,
    VERIFIED_ACCESS_ENDPOINT_TYPE_RDS,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements, error_code, error_message, indexed};
use super::wire::{child, child_text, document, set_items};

/// The Region every call here is signed for unless the row names another: the exported
/// `region` is the signed one, so a Region that is not the emulator's default is the stronger
/// case.
const REGION: &str = "us-east-2";

/// The version the exported configuration document carries, spelled out here rather than read
/// from the crate so a change to what the handler exports is caught: it is a property of the
/// document FORMAT, so it is the same for every instance and every call.
const CONFIGURATION_VERSION: &str = "1";

/// The OIDC configuration every `oidc` user trust provider under test is created with. The
/// exported client configuration has to report these back, so they are written once here and
/// asserted against.
const OIDC_ISSUER: &str = "https://login.example.test/";
const OIDC_AUTHORIZATION_ENDPOINT: &str = "https://login.example.test/authorize";
const OIDC_TOKEN_ENDPOINT: &str = "https://login.example.test/token";
const OIDC_USER_INFO_ENDPOINT: &str = "https://login.example.test/userinfo";
const OIDC_CLIENT_ID: &str = "client-42";
const OIDC_CLIENT_SECRET: &str = "secret-42";
const OIDC_SCOPE: &str = "openid profile";

/// The public signing key endpoint only the native application OIDC options carry, so the
/// member of the exported configuration that can come from nowhere else.
const NATIVE_PUBLIC_SIGNING_KEY_ENDPOINT: &str = "https://login.example.test/keys";

/// The sentence EC2 answers for an id that names no Verified Access instance of the account,
/// as a template the row fills in: the text is fixed for the class and names the id the
/// REQUEST gave.
fn not_found_message(instance_id: &str) -> String {
    format!("The verified-access-instance ID '{instance_id}' does not exist")
}

/// The sentence EC2 answers for a required parameter the request left out, spelled out here
/// rather than read from the crate: fixed text naming the member the model declares.
fn missing_parameter_message(member: &str) -> String {
    format!(
        "The request must include the {member} parameter. Add the required parameter and retry \
         the request."
    )
}

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// One request's form parameters, as the ec2Query harness takes them.
type Params = Vec<(String, String)>;

/// An emulator with nothing seeded: every resource a row needs is created through the
/// operations that create it.
fn emulator() -> (MultiServiceRouter, std::sync::Arc<engine::InMemoryEngine>) {
    super::ec2_query::emulator()
}

/// What the VPC seed holds, for the network CIDR endpoint rows to name.
struct Fixture {
    /// A subnet a network CIDR endpoint may name.
    subnet_id: String,
    /// A security group a network CIDR endpoint is associated with.
    security_group_id: String,
}

/// Write the VPC resources a Verified Access endpoint references into the engine's store, the
/// way the operations that create them would, and report the ids they were given.
fn seed_vpc(engine: &engine::InMemoryEngine) -> Fixture {
    let state = engine.state::<Ec2State>();
    let mut guard = state.lock();

    let vpc_id = guard.insert_vpc("10.0.0.0/16", false, "default", "available", REGION, Vec::new());
    let zone = &availability_zones_in_region(REGION)[0];
    let subnet_id = guard.insert_subnet(SubnetSpec {
        vpc_id: vpc_id.clone(),
        cidr_block: "10.0.0.0/24".to_string(),
        availability_zone: zone.zone_name.clone(),
        availability_zone_id: zone.zone_id.clone(),
        region: REGION.to_string(),
        state: "available".to_string(),
        default_for_az: false,
        map_public_ip_on_launch: false,
        tags: Vec::new(),
    });
    let security_group_id = guard.insert_security_group(SecurityGroupSpec {
        vpc_id,
        group_name: "verified-access".to_string(),
        description: "reaches the application".to_string(),
        region: REGION.to_string(),
        tags: Vec::new(),
        ip_permissions: Vec::new(),
        ip_permissions_egress: Vec::new(),
    });

    Fixture {
        subnet_id,
        security_group_id,
    }
}

/// One `ExportVerifiedAccessInstanceClientConfiguration` call signed for `region`.
async fn export_in(
    router: &MultiServiceRouter,
    region: &str,
    params: &Params,
) -> (u16, String) {
    call(
        router,
        region,
        "ExportVerifiedAccessInstanceClientConfiguration",
        params,
    )
    .await
}

/// The export of one instance, signed for [`REGION`], which must succeed.
async fn export(router: &MultiServiceRouter, case: &str, instance_id: &str) -> String {
    let (status, body) = export_in(
        router,
        REGION,
        &vec![p("VerifiedAccessInstanceId", instance_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from the export, got {status} with body {body}"
    );
    body
}

/// Create one Verified Access instance that must succeed, and answer the id it was minted.
async fn seed_instance(router: &MultiServiceRouter, case: &str, description: &str) -> String {
    let (status, body) = call(
        router,
        REGION,
        "CreateVerifiedAccessInstance",
        &[p("Description", description)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateVerifiedAccessInstance, got {status} with \
         body {body}"
    );
    element(&body, "verifiedAccessInstanceId").unwrap_or_else(|| {
        panic!("case {case:?}: the create must report the instance it minted, got {body}")
    })
}

/// The parameters one KIND of trust provider needs: its sub-type, plus the options the
/// modelled docs call required for that kind. `reference` makes the policy reference name
/// distinct, so several providers of one kind can be created side by side.
fn provider_params(
    trust_provider_type: &str,
    sub_type: &str,
    reference: usize,
    native_options: bool,
) -> Params {
    let mut params = vec![
        p("TrustProviderType", trust_provider_type),
        p("Description", format!("the {sub_type} provider")),
        p(
            "PolicyReferenceName",
            format!("ref{}{reference}", sub_type.replace('-', "")),
        ),
    ];
    if trust_provider_type == TRUST_PROVIDER_TYPE_USER {
        params.push(p("UserTrustProviderType", sub_type));
        if sub_type == USER_TRUST_PROVIDER_TYPE_OIDC {
            params.extend([
                p("OidcOptions.Issuer", OIDC_ISSUER),
                p("OidcOptions.AuthorizationEndpoint", OIDC_AUTHORIZATION_ENDPOINT),
                p("OidcOptions.TokenEndpoint", OIDC_TOKEN_ENDPOINT),
                p("OidcOptions.UserInfoEndpoint", OIDC_USER_INFO_ENDPOINT),
                p("OidcOptions.ClientId", OIDC_CLIENT_ID),
                p("OidcOptions.ClientSecret", OIDC_CLIENT_SECRET),
                p("OidcOptions.Scope", OIDC_SCOPE),
            ]);
        }
        if native_options {
            params.extend([
                p(
                    "NativeApplicationOidcOptions.PublicSigningKeyEndpoint",
                    NATIVE_PUBLIC_SIGNING_KEY_ENDPOINT,
                ),
                p("NativeApplicationOidcOptions.Issuer", OIDC_ISSUER),
            ]);
        }
    } else {
        params.push(p("DeviceTrustProviderType", sub_type));
        params.extend([
            p("DeviceOptions.TenantId", format!("tenant-{sub_type}")),
            p(
                "DeviceOptions.PublicSigningKeyUrl",
                "https://device.example.test/keys",
            ),
        ]);
    }
    params
}

/// Create one Verified Access trust provider of that kind that must succeed, and answer the id
/// it was minted.
async fn seed_provider(
    router: &MultiServiceRouter,
    case: &str,
    trust_provider_type: &str,
    sub_type: &str,
    reference: usize,
    native_options: bool,
) -> String {
    let (status, body) = call(
        router,
        REGION,
        "CreateVerifiedAccessTrustProvider",
        &provider_params(trust_provider_type, sub_type, reference, native_options),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateVerifiedAccessTrustProvider, got {status} \
         with body {body}"
    );
    element(&body, "verifiedAccessTrustProviderId").unwrap_or_else(|| {
        panic!("case {case:?}: the create must report the provider it minted, got {body}")
    })
}

/// Attach one trust provider to one instance, which must succeed.
async fn attach(router: &MultiServiceRouter, case: &str, instance_id: &str, provider_id: &str) {
    let (status, body) = call(
        router,
        REGION,
        "AttachVerifiedAccessTrustProvider",
        &[
            p("VerifiedAccessInstanceId", instance_id),
            p("VerifiedAccessTrustProviderId", provider_id),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from the attach of {provider_id}, got {status} with \
         body {body}"
    );
}

/// Detach one trust provider from one instance, which must succeed.
async fn detach(router: &MultiServiceRouter, case: &str, instance_id: &str, provider_id: &str) {
    let (status, body) = call(
        router,
        REGION,
        "DetachVerifiedAccessTrustProvider",
        &[
            p("VerifiedAccessInstanceId", instance_id),
            p("VerifiedAccessTrustProviderId", provider_id),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from the detach of {provider_id}, got {status} with \
         body {body}"
    );
}

/// The `deviceTrustProviderSet` an export reports, in document order. Read through the XML
/// parser, so a set the service wrote EMPTY reads as no entries rather than as absent.
fn exported_device_kinds(case: &str, body: &str) -> Vec<String> {
    let parsed = document(case, body);
    set_items(parsed.root_element(), "deviceTrustProviderSet")
        .into_iter()
        .map(|item| item.text().unwrap_or_default().to_string())
        .collect()
}

/// One member of the `userTrustProvider` an export reports, or `None` when the export carries
/// no such member — or no user trust provider at all.
fn exported_user_member(case: &str, body: &str, name: &str) -> Option<String> {
    let parsed = document(case, body);
    child(parsed.root_element(), "userTrustProvider")
        .and_then(|block| child_text(block, name))
}

/// Whether an export reports a user trust provider at all.
fn exports_user_trust_provider(case: &str, body: &str) -> bool {
    let parsed = document(case, body);
    child(parsed.root_element(), "userTrustProvider").is_some()
}

/// The `openVpnConfigurationSet` an export reports, as `(the decoded configuration, its
/// routes)` per entry in document order.
fn exported_open_vpn(case: &str, body: &str) -> Vec<(String, Vec<String>)> {
    use base64::Engine as _;

    let parsed = document(case, body);
    set_items(parsed.root_element(), "openVpnConfigurationSet")
        .into_iter()
        .map(|item| {
            let encoded = child_text(item, "config").unwrap_or_else(|| {
                panic!("case {case:?}: an OpenVPN entry must carry a config, got {body}")
            });
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(&encoded)
                .unwrap_or_else(|error| {
                    panic!("case {case:?}: the config must be base64 ({error}): {encoded}")
                });
            let configuration = String::from_utf8(decoded).unwrap_or_else(|error| {
                panic!("case {case:?}: the config must be text ({error}): {encoded}")
            });
            let routes = set_items(item, "routeSet")
                .into_iter()
                .map(|route| child_text(route, "cidr").unwrap_or_default())
                .collect();
            (configuration, routes)
        })
        .collect()
}

/// The `lastUpdatedTime` `DescribeVerifiedAccessInstances` reports for one instance — what an
/// operation that WROTE to the instance would have moved.
async fn last_updated_time(router: &MultiServiceRouter, case: &str, instance_id: &str) -> String {
    let (status, body) = call(
        router,
        REGION,
        "DescribeVerifiedAccessInstances",
        &[p("VerifiedAccessInstanceId.1", instance_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeVerifiedAccessInstances, got {status} \
         with body {body}"
    );
    element(&body, "lastUpdatedTime").unwrap_or_else(|| {
        panic!("case {case:?}: the instance must report a lastUpdatedTime, got {body}")
    })
}

/// Create one network CIDR endpoint on `group_id` routing `cidr`, and answer
/// `(the endpoint id, the DNS name generated for it)`.
async fn seed_cidr_endpoint(
    router: &MultiServiceRouter,
    case: &str,
    group_id: &str,
    cidr: &str,
    fixture: &Fixture,
) -> (String, String) {
    let params = [
        vec![
            p("VerifiedAccessGroupId", group_id),
            p("EndpointType", VERIFIED_ACCESS_ENDPOINT_TYPE_CIDR),
            p("AttachmentType", "vpc"),
            p("CidrOptions.Protocol", "tcp"),
            p("CidrOptions.Cidr", cidr),
            p("CidrOptions.PortRange.1.FromPort", "5000"),
            p("CidrOptions.PortRange.1.ToPort", "5000"),
        ],
        indexed("CidrOptions.SubnetId", &[fixture.subnet_id.as_str()]),
        indexed("SecurityGroupId", &[fixture.security_group_id.as_str()]),
    ]
    .concat();
    let (status, body) = call(router, REGION, "CreateVerifiedAccessEndpoint", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateVerifiedAccessEndpoint, got {status} with \
         body {body}"
    );
    let endpoint_id = element(&body, "verifiedAccessEndpointId").unwrap_or_default();
    let endpoint_domain = element(&body, "endpointDomain").unwrap_or_default();
    assert!(
        !endpoint_id.is_empty() && !endpoint_domain.is_empty(),
        "case {case:?}: the create must report the endpoint and the name generated for it, got \
         {body}"
    );
    (endpoint_id, endpoint_domain)
}

/// Create one Verified Access group on `instance_id`, and answer the id it was minted.
async fn seed_group(router: &MultiServiceRouter, case: &str, instance_id: &str) -> String {
    let (status, body) = call(
        router,
        REGION,
        "CreateVerifiedAccessGroup",
        &[p("VerifiedAccessInstanceId", instance_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateVerifiedAccessGroup, got {status} with \
         body {body}"
    );
    element(&body, "verifiedAccessGroupId").unwrap_or_else(|| {
        panic!("case {case:?}: the create must report the group it minted, got {body}")
    })
}

/// RULE 1 over the published user-kind catalog: the exported `userTrustProvider` is the STORED
/// trust provider the instance has attached — its kind, and for an OIDC provider every
/// endpoint and identifier it was configured with. A provider that is not OIDC configures no
/// endpoint, so it exports its kind and nothing else.
#[tokio::test]
async fn every_published_user_kind_is_exported_from_the_stored_trust_provider() {
    // (the published kind, whether the provider also carries native application options).
    let rows: Vec<(&str, bool)> = USER_TRUST_PROVIDER_TYPE_VALUES
        .iter()
        .flat_map(|kind| [(*kind, false), (*kind, true)])
        .collect();
    for (kind, native_options) in rows {
        let case = format!("{kind}, native options: {native_options}");
        let (router, _engine) = emulator();
        let instance_id = seed_instance(&router, &case, "the sales applications").await;
        let provider_id = seed_provider(
            &router,
            &case,
            TRUST_PROVIDER_TYPE_USER,
            kind,
            1,
            native_options,
        )
        .await;
        attach(&router, &case, &instance_id, &provider_id).await;

        let body = export(&router, &case, &instance_id).await;
        assert_eq!(
            element(&body, "version"),
            Some(CONFIGURATION_VERSION.to_string()),
            "case {case:?}: expected the configuration version, got {body}"
        );
        assert_eq!(
            element(&body, "verifiedAccessInstanceId"),
            Some(instance_id.clone()),
            "case {case:?}: expected the instance the export was asked for, got {body}"
        );
        assert_eq!(
            element(&body, "region"),
            Some(REGION.to_string()),
            "case {case:?}: expected the signed Region, got {body}"
        );
        assert_eq!(
            exported_user_member(&case, &body, "type"),
            Some(kind.to_string()),
            "case {case:?}: expected the attached provider's kind, got {body}"
        );
        assert!(
            exported_device_kinds(&case, &body).is_empty(),
            "case {case:?}: a user provider is no device provider, got {body}"
        );
        assert!(
            exported_open_vpn(&case, &body).is_empty(),
            "case {case:?}: an instance with no network CIDR endpoint tunnels nowhere, got {body}"
        );

        // The OIDC members come from the provider's own options, so a provider configured with
        // none reports none: `iam-identity-center` is not OIDC.
        let oidc = kind == USER_TRUST_PROVIDER_TYPE_OIDC;
        for (member, configured) in [
            ("issuer", OIDC_ISSUER),
            ("authorizationEndpoint", OIDC_AUTHORIZATION_ENDPOINT),
            ("tokenEndpoint", OIDC_TOKEN_ENDPOINT),
            ("userInfoEndpoint", OIDC_USER_INFO_ENDPOINT),
            ("clientId", OIDC_CLIENT_ID),
            ("scopes", OIDC_SCOPE),
        ] {
            let actual = exported_user_member(&case, &body, member);
            // A provider carrying only the native application options still configures an
            // issuer, which is the member both blocks carry.
            let expected = if oidc || (native_options && member == "issuer") {
                Some(configured.to_string())
            } else {
                None
            };
            assert_eq!(
                actual, expected,
                "case {case:?}: expected <{member}> {expected:?}, got {actual:?} in {body}"
            );
        }
        // The client secret a provider was configured with is never stored, so what a read
        // reports is the mask the service answers in its place.
        assert_eq!(
            exported_user_member(&case, &body, "clientSecret"),
            oidc.then(|| REDACTED_CLIENT_SECRET.to_string()),
            "case {case:?}: expected the stored client secret member, got {body}"
        );
        // The public signing key endpoint is carried by the native application options alone.
        assert_eq!(
            exported_user_member(&case, &body, "publicSigningKeyEndpoint"),
            native_options.then(|| NATIVE_PUBLIC_SIGNING_KEY_ENDPOINT.to_string()),
            "case {case:?}: expected the configured public signing key endpoint, got {body}"
        );
    }
}

/// RULE 1 over the published device-kind catalog: `deviceTrustProviderSet` reports the KINDS
/// of device trust provider the instance has attached — each kind once however many providers
/// of it are attached, in the order the first of them was attached.
#[tokio::test]
async fn the_exported_device_kinds_are_the_kinds_attached() {
    // (case, the device kinds attached in order, the kinds the export must report).
    let mut rows: Vec<(String, Vec<&str>, Vec<&str>)> = DEVICE_TRUST_PROVIDER_TYPE_VALUES
        .iter()
        .map(|kind| (format!("one {kind} provider"), vec![*kind], vec![*kind]))
        .collect();
    let catalog: Vec<&str> = DEVICE_TRUST_PROVIDER_TYPE_VALUES.to_vec();
    let mut reversed = catalog.clone();
    reversed.reverse();
    rows.push((
        "every published kind, in catalog order".to_string(),
        catalog.clone(),
        catalog.clone(),
    ));
    rows.push((
        "every published kind, attached in reverse".to_string(),
        reversed.clone(),
        reversed,
    ));
    rows.push((
        "two providers of one kind".to_string(),
        vec![catalog[0], catalog[0]],
        vec![catalog[0]],
    ));
    rows.push((
        "a repeated kind between two others".to_string(),
        vec![catalog[0], catalog[1], catalog[0]],
        vec![catalog[0], catalog[1]],
    ));
    rows.push(("nothing attached".to_string(), Vec::new(), Vec::new()));

    for (case, attached, expected) in rows {
        let (router, _engine) = emulator();
        let instance_id = seed_instance(&router, &case, "the support applications").await;
        for (reference, kind) in attached.iter().enumerate() {
            let provider_id = seed_provider(
                &router,
                &case,
                TRUST_PROVIDER_TYPE_DEVICE,
                kind,
                reference,
                false,
            )
            .await;
            attach(&router, &case, &instance_id, &provider_id).await;
        }

        let body = export(&router, &case, &instance_id).await;
        let actual = exported_device_kinds(&case, &body);
        assert_eq!(
            actual,
            expected
                .iter()
                .map(|kind| (*kind).to_string())
                .collect::<Vec<String>>(),
            "case {case:?}: expected the attached kinds {expected:?}, got {actual:?} in {body}"
        );
        assert!(
            !exports_user_trust_provider(&case, &body),
            "case {case:?}: no user provider is attached, so none is exported, got {body}"
        );
    }
}

/// RULE 2: the exported configuration FOLLOWS the store — a detach takes a trust provider out
/// of it, and an instance with nothing attached exports an empty configuration rather than a
/// refusal.
#[tokio::test]
async fn the_export_follows_what_the_instance_has_attached() {
    let case = "attach both kinds, then detach each";
    let (router, _engine) = emulator();
    let instance_id = seed_instance(&router, case, "the sales applications").await;

    let empty = export(&router, case, &instance_id).await;
    assert!(
        exported_device_kinds(case, &empty).is_empty()
            && !exports_user_trust_provider(case, &empty)
            && exported_open_vpn(case, &empty).is_empty(),
        "case {case:?}: a fresh instance exports an empty configuration, got {empty}"
    );
    assert_eq!(
        element(&empty, "verifiedAccessInstanceId"),
        Some(instance_id.clone()),
        "case {case:?}: an empty configuration still names its instance, got {empty}"
    );

    let device_kind = DEVICE_TRUST_PROVIDER_TYPE_VALUES[0];
    let device_id = seed_provider(
        &router,
        case,
        TRUST_PROVIDER_TYPE_DEVICE,
        device_kind,
        1,
        false,
    )
    .await;
    let user_id = seed_provider(
        &router,
        case,
        TRUST_PROVIDER_TYPE_USER,
        USER_TRUST_PROVIDER_TYPE_OIDC,
        2,
        false,
    )
    .await;
    attach(&router, case, &instance_id, &device_id).await;
    attach(&router, case, &instance_id, &user_id).await;

    let both = export(&router, case, &instance_id).await;
    assert_eq!(
        exported_device_kinds(case, &both),
        vec![device_kind.to_string()],
        "case {case:?}: the attached device kind is exported, got {both}"
    );
    assert_eq!(
        exported_user_member(case, &both, "type"),
        Some(USER_TRUST_PROVIDER_TYPE_OIDC.to_string()),
        "case {case:?}: the attached user provider is exported, got {both}"
    );

    detach(&router, case, &instance_id, &device_id).await;
    let without_device = export(&router, case, &instance_id).await;
    assert!(
        exported_device_kinds(case, &without_device).is_empty(),
        "case {case:?}: a detached device provider is no longer exported, got {without_device}"
    );
    assert_eq!(
        exported_user_member(case, &without_device, "type"),
        Some(USER_TRUST_PROVIDER_TYPE_OIDC.to_string()),
        "case {case:?}: detaching one provider leaves the other, got {without_device}"
    );

    detach(&router, case, &instance_id, &user_id).await;
    let without_either = export(&router, case, &instance_id).await;
    assert!(
        !exports_user_trust_provider(case, &without_either),
        "case {case:?}: a detached user provider is no longer exported, got {without_either}"
    );
}

/// RULE 3: each network CIDR endpoint of the instance is one exported OpenVPN client
/// configuration — routing that endpoint's own CIDR and dialling the DNS name generated for
/// it — and an endpoint of another kind, or of another instance, contributes none.
#[tokio::test]
async fn network_cidr_endpoints_are_the_exported_open_vpn_configurations() {
    // (case, the CIDRs the instance's network CIDR endpoints route).
    let rows: Vec<(&str, Vec<&str>)> = vec![
        ("no network CIDR endpoint", Vec::new()),
        ("one network CIDR endpoint", vec!["10.0.8.0/24"]),
        (
            "three network CIDR endpoints",
            vec!["10.0.9.0/24", "10.0.10.0/23", "172.16.0.0/16"],
        ),
    ];
    for (case, cidrs) in rows {
        let (router, engine) = emulator();
        let fixture = seed_vpc(&engine);
        let instance_id = seed_instance(&router, case, "the sales applications").await;
        let group_id = seed_group(&router, case, &instance_id).await;

        // Another instance with a network CIDR endpoint of its own: an export names ONE
        // instance, so this one's endpoint must not appear in it.
        let other_instance_id = seed_instance(&router, case, "the support applications").await;
        let other_group_id = seed_group(&router, case, &other_instance_id).await;
        let (_, other_domain) =
            seed_cidr_endpoint(&router, case, &other_group_id, "192.168.0.0/24", &fixture).await;

        // An endpoint of another kind reaches its application over its own domain rather than
        // through a tunnel, so it contributes no configuration.
        let rds_params = [
            vec![
                p("VerifiedAccessGroupId", group_id.clone()),
                p("EndpointType", VERIFIED_ACCESS_ENDPOINT_TYPE_RDS),
                p("AttachmentType", "vpc"),
                p("RdsOptions.Protocol", "tcp"),
                p("RdsOptions.Port", "5432"),
                p(
                    "RdsOptions.RdsDbInstanceArn",
                    "arn:aws:rds:us-east-2:000000000000:db:ledger",
                ),
                p("RdsOptions.RdsEndpoint", "ledger.example.test"),
            ],
            indexed("RdsOptions.SubnetId", &[fixture.subnet_id.as_str()]),
            indexed("SecurityGroupId", &[fixture.security_group_id.as_str()]),
        ]
        .concat();
        let (status, body) = call(&router, REGION, "CreateVerifiedAccessEndpoint", &rds_params).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the other kind of endpoint must be created, got {status} with body \
             {body}"
        );

        let mut domains: Vec<String> = Vec::new();
        for cidr in &cidrs {
            let (_, domain) = seed_cidr_endpoint(&router, case, &group_id, cidr, &fixture).await;
            domains.push(domain);
        }

        let exported = export(&router, case, &instance_id).await;
        let configurations = exported_open_vpn(case, &exported);
        assert_eq!(
            configurations.len(),
            cidrs.len(),
            "case {case:?}: expected one configuration per network CIDR endpoint, got {exported}"
        );
        let routed: BTreeSet<String> = configurations
            .iter()
            .flat_map(|(_, routes)| routes.clone())
            .collect();
        let expected_routes: BTreeSet<String> =
            cidrs.iter().map(|cidr| (*cidr).to_string()).collect();
        assert_eq!(
            routed, expected_routes,
            "case {case:?}: expected the endpoints' own CIDRs as the routes, got {exported}"
        );
        // Each configuration dials ONE of this instance's endpoints, and every one of them is
        // dialled: the document is derived from the endpoint, not written for the instance.
        for domain in &domains {
            let dialling = configurations
                .iter()
                .filter(|(configuration, _)| {
                    configuration
                        .lines()
                        .any(|line| line == format!("remote {domain} 443"))
                })
                .count();
            assert_eq!(
                dialling, 1,
                "case {case:?}: expected exactly one configuration dialling {domain}, got \
                 {exported}"
            );
        }
        for (configuration, routes) in &configurations {
            assert!(
                configuration.lines().any(|line| line == "client")
                    && configuration.lines().any(|line| line == "dev tun"),
                "case {case:?}: expected an OpenVPN client document, got {configuration:?}"
            );
            assert!(
                !configuration.contains(other_domain.as_str()),
                "case {case:?}: another instance's endpoint is not dialled, got {configuration:?}"
            );
            assert_eq!(
                routes.len(),
                1,
                "case {case:?}: an endpoint routes its one CIDR, got {routes:?}"
            );
        }
    }
}

/// RULE 4: the `region` exported is the Region the request was signed for — EC2 is reached
/// through a regional endpoint, and the credential scope is what names the Region.
#[tokio::test]
async fn the_exported_region_is_the_region_the_request_was_signed_for() {
    let (router, _engine) = emulator();
    let case = "one instance, exported from several Regions";
    let instance_id = seed_instance(&router, case, "the sales applications").await;
    for region in ["us-east-1", "eu-west-1", "ap-southeast-2", REGION] {
        let (status, body) = export_in(
            &router,
            region,
            &vec![p("VerifiedAccessInstanceId", instance_id.clone())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {region:?}: expected HTTP 200 from the export, got {status} with body {body}"
        );
        assert_eq!(
            element(&body, "region"),
            Some(region.to_string()),
            "case {region:?}: expected the signed Region, got {body}"
        );
        assert_eq!(
            element(&body, "verifiedAccessInstanceId"),
            Some(instance_id.clone()),
            "case {region:?}: expected the instance the export was asked for, got {body}"
        );
    }
}

/// RULE 5: an id that names no Verified Access instance of the account is refused with that
/// resource's own code and sentence, whatever shape the id takes, and a request that names no
/// instance at all is refused as the absent parameter it is.
#[tokio::test]
async fn an_unresolvable_or_absent_instance_is_refused() {
    let (router, _engine) = emulator();
    let case = "the ids that resolve to nothing";
    let held_instance = seed_instance(&router, case, "the sales applications").await;
    let provider_id = seed_provider(
        &router,
        case,
        TRUST_PROVIDER_TYPE_DEVICE,
        DEVICE_TRUST_PROVIDER_TYPE_VALUES[0],
        1,
        false,
    )
    .await;

    // (case, the id the request names, the code, the sentence it carries).
    let rows: Vec<(&str, String, &str, String)> = vec![
        (
            "an id this emulator never minted",
            "vai-0123456789abcdef0".to_string(),
            "InvalidVerifiedAccessInstanceId.NotFound",
            not_found_message("vai-0123456789abcdef0"),
        ),
        (
            "another resource's id",
            provider_id.clone(),
            "InvalidVerifiedAccessInstanceId.NotFound",
            not_found_message(&provider_id),
        ),
        (
            "text of no EC2 id form at all",
            "the-sales-instance".to_string(),
            "InvalidVerifiedAccessInstanceId.NotFound",
            not_found_message("the-sales-instance"),
        ),
        (
            "no instance named at all",
            String::new(),
            "MissingParameter",
            missing_parameter_message("VerifiedAccessInstanceId"),
        ),
    ];
    for (row, instance_id, code, message) in rows {
        let (status, body) = export_in(
            &router,
            REGION,
            &vec![p("VerifiedAccessInstanceId", instance_id.clone())],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {row:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            code,
            "case {row:?}: expected the code {code}, got {body}"
        );
        assert_eq!(
            error_message(&body),
            message,
            "case {row:?}: expected the sentence of that refusal, got {body}"
        );
    }

    // The instance the account DOES hold still exports, so no refusal above left anything
    // behind.
    let body = export(&router, case, &held_instance).await;
    assert_eq!(
        element(&body, "verifiedAccessInstanceId"),
        Some(held_instance),
        "case {case:?}: the held instance still exports, got {body}"
    );
}

/// RULE 5 and RULE 6: a `DryRun` reports the outcome the request would have had — the
/// permitted answer for an instance the account holds, and the id's own refusal for one it
/// does not — and the export itself is a READ: two exports agree and the instance is left
/// exactly as it was.
#[tokio::test]
async fn a_dry_run_reports_the_outcome_and_the_export_changes_nothing() {
    let case = "a dry run beside two exports";
    let (router, _engine) = emulator();
    let instance_id = seed_instance(&router, case, "the sales applications").await;
    let provider_id = seed_provider(
        &router,
        case,
        TRUST_PROVIDER_TYPE_USER,
        USER_TRUST_PROVIDER_TYPE_OIDC,
        1,
        false,
    )
    .await;
    attach(&router, case, &instance_id, &provider_id).await;
    let before = last_updated_time(&router, case, &instance_id).await;

    let (status, body) = export_in(
        &router,
        REGION,
        &vec![
            p("VerifiedAccessInstanceId", instance_id.clone()),
            p("DryRun", "true"),
        ],
    )
    .await;
    assert_eq!(
        status, 412,
        "case {case:?}: expected HTTP 412 from the dry run, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "DryRunOperation",
        "case {case:?}: expected DryRunOperation, got {body}"
    );

    // A dry run naming an instance the account does not hold is refused with the reason the
    // request would have been refused for.
    let (status, body) = export_in(
        &router,
        REGION,
        &vec![
            p("VerifiedAccessInstanceId", "vai-0123456789abcdef0"),
            p("DryRun", "true"),
        ],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400 for the unresolvable dry run, got {status} with body \
         {body}"
    );
    assert_eq!(
        error_code(&body),
        "InvalidVerifiedAccessInstanceId.NotFound",
        "case {case:?}: expected the id's own refusal, got {body}"
    );

    let first = export(&router, case, &instance_id).await;
    let second = export(&router, case, &instance_id).await;
    assert_eq!(
        elements(&first, "type"),
        elements(&second, "type"),
        "case {case:?}: two exports of one unchanged instance agree, got {first} then {second}"
    );
    assert_eq!(
        element(&first, "verifiedAccessInstanceId"),
        element(&second, "verifiedAccessInstanceId"),
        "case {case:?}: two exports of one unchanged instance agree, got {first} then {second}"
    );
    assert_eq!(
        last_updated_time(&router, case, &instance_id).await,
        before,
        "case {case:?}: exporting an instance does not write to it"
    );
}
