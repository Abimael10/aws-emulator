// SPDX-License-Identifier: CC0-1.0

//! EVERY operation this model declares answers from the account's state: the modelled
//! response, or one of the errors the model declares for that operation — never the
//! scaffold's `501 NotImplemented`.
//!
//! # The rule
//!
//! `501 NotImplemented` is this emulator's own statement that nobody has written the
//! operation yet; it is not an answer any Elastic Load Balancing API gives. So a well-formed
//! call to an action the model binds is either SERVED with that operation's own document, or
//! REFUSED with one of the error codes the model declares for it — and a client branching on
//! the modelled codes can act on every one of them.
//!
//! # Why a whole-surface table
//!
//! The issue behind this suite recorded ten FAMILIES answered 501 (tagging, the mTLS trust
//! stores, `DeleteTargetGroup`, the rule surface, `SetIpAddressType`, `SetSubnets`, the
//! capacity reservation, the account limits, the TLS policy catalogue and the health-check
//! reconfiguration). Ten families are ten rows of the rule above, so this suite drives EVERY
//! action the model binds — read from the model itself
//! ([`crate::declared_action_set::declared_actions`]) rather than restated here, so an
//! operation the model grows has no row until someone gives it one — each against state this
//! suite really provisioned, and asserts the answer is that operation's own.
//!
//! Every request goes through the router exactly as a client's would — awsQuery form body in,
//! `<{Operation}Response>` XML out. Amazon S3 is mounted beside this service on the same
//! engine because a trust store is built from an object that store holds, and the CA bundle
//! and the revocation list below are real ones this suite staged with `PutObject`.
//!
//! What each operation then does with the state it read — and the WORDING and class of each
//! refusal it answers — is pinned by the per-family suites beside this one: `tag_writes.rs`
//! and `tag_not_found_wording_by_kind.rs` (the tag plane), `trust_store_lifecycle.rs` and the
//! trust-store read suites (the mTLS family), `target_group_in_use_refusal.rs`
//! (`ResourceInUse`), `listener_rules.rs` and `rule_priorities.rs` (the rule surface),
//! `load_balancer_network_capacity_and_policy.rs` (the network writes and the capacity
//! reservation), `catalog_reads.rs` (the account limits and the TLS policies) and
//! `target_group_layer_7_health_check.rs` (the health-check reconfiguration). This suite adds
//! the one thing none of them can state: that every operation of the surface has an answer of
//! its OWN at all.

use std::sync::OnceLock;

use crate::declared_action_set::declared_actions;
use crate::listener_rules::{add_rule, create_listener, create_load_balancer, forward_to};
use crate::provisioning_loop::{action, call, first_text, p, Reply};
use crate::target_group_attributes_and_modification::create_target_group;
use crate::trust_store_lifecycle::{emulator_with_bundles, Emulator, BUNDLE_BUCKET};
use crate::trust_store_revocations::{authority, crl_pem};

/// The form fields of one awsQuery request.
type Form = Vec<(String, String)>;

/// The status and code the scaffold channel answers an operation nobody has written with
/// (`crate::scaffolded`): the one answer no row below may earn.
const SCAFFOLD_STATUS: u16 = 501;
const SCAFFOLD_CODE: &str = "NotImplemented";

/// A Certificate Manager certificate ARN for the TLS listener the fixture carries, and a
/// second one a row adds to it. This service resolves the ARN FORM of a requested
/// certificate and not the certificate itself, as the crate's certificate suites pin, so any
/// well-formed ACM ARN serves here.
const CERTIFICATE: &str =
    "arn:aws:acm:us-west-2:000000000000:certificate/aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
const EXTRA_CERTIFICATE: &str =
    "arn:aws:acm:us-west-2:000000000000:certificate/bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";

/// The VPC every resource of the fixture lives in.
const VPC: &str = "vpc-12345678";

/// What a call must be answered with.
#[derive(Debug, Clone, Copy)]
enum Outcome {
    /// The operation's own 200 document.
    Served,
    /// A refusal under one of the awsQuery error codes the model declares for it.
    Refused(&'static str),
}

// ---------------------------------------------------------------------------
// The state every row is driven against
// ---------------------------------------------------------------------------

/// The resources one row is driven against: a load balancer with an HTTP and a TLS listener,
/// a rule, two target groups (one held by the listener, one free), a trust store built from a
/// staged CA bundle, and the revocation list added to it.
///
/// Built fresh for every row, because a row may delete what it addresses.
struct Fixture {
    emulator: Emulator,
    load_balancer: String,
    target_group: String,
    spare_target_group: String,
    listener: String,
    tls_listener: String,
    rule: String,
    trust_store: String,
    revocation_id: String,
    bundle_key: String,
    revocation_key: String,
}

/// The priority the fixture's rule is evaluated at.
const RULE_PRIORITY: i32 = 10;

/// The instance target the fixture registers, so the deregistration row has something to
/// withdraw.
const INSTANCE_TARGET: &str = "i-0123456789abcdef0";

/// The name of the fixture's trust store.
const TRUST_STORE_NAME: &str = "sweep-trust-store";

/// The PEM of a real certification authority's certificate and of a revocation list it
/// signed, generated ONCE for the whole suite.
///
/// A fixture is built per row, but this material is the same in every one of them and
/// generating a key pair and signing a list per row is the suite's whole cost — so it is
/// generated once here and only STAGED per fixture, which is the part each row needs its own
/// copy of.
fn ca_material() -> &'static (String, String) {
    static MATERIAL: OnceLock<(String, String)> = OnceLock::new();
    MATERIAL.get_or_init(|| {
        let (certificate, key) = authority();
        (certificate.pem(), crl_pem(&certificate, &key, 2))
    })
}

impl Fixture {
    async fn new() -> Self {
        let emulator = emulator_with_bundles().await;
        // The certification authority's own certificate staged as the CA bundle a trust
        // store is built from, and a revocation list it really signed staged beside it.
        let (bundle, revocations) = ca_material();
        let bundle_key = "sweep/ca-bundle.pem".to_string();
        let revocation_key = "sweep/revocations.pem".to_string();
        emulator.stage(&bundle_key, bundle).await;
        emulator.stage(&revocation_key, revocations).await;

        let load_balancer = create_load_balancer(&emulator.router, "sweep-alb").await;
        let target_group = create_target_group(
            &emulator.router,
            "sweep-tg",
            &[("Protocol", "HTTP"), ("Port", "80"), ("VpcId", VPC)],
        )
        .await;
        let spare_target_group = create_target_group(
            &emulator.router,
            "sweep-spare-tg",
            &[("Protocol", "HTTP"), ("Port", "8080"), ("VpcId", VPC)],
        )
        .await;
        let listener = create_listener(&emulator.router, &load_balancer, &target_group, 80).await;
        let tls_listener = create_tls_listener(&emulator, &load_balancer, &target_group).await;
        let mut rule_fields = vec![
            p("Conditions.member.1.Field", "host-header"),
            p("Conditions.member.1.Values.member.1", "sweep.example.com"),
        ];
        rule_fields.extend(forward_to(&target_group));
        let rule = add_rule(
            &emulator.router,
            "the fixture's rule",
            &listener,
            RULE_PRIORITY,
            &rule_fields,
        )
        .await;
        register_instance(&emulator, &target_group).await;
        tag(&emulator, &target_group).await;

        let trust_store = create_trust_store(&emulator, TRUST_STORE_NAME, &bundle_key).await;
        let revocation_id = add_revocation(&emulator, &trust_store, &revocation_key).await;

        Self {
            emulator,
            load_balancer,
            target_group,
            spare_target_group,
            listener,
            tls_listener,
            rule,
            trust_store,
            revocation_id,
            bundle_key,
            revocation_key,
        }
    }
}

/// An HTTPS listener carrying [`CERTIFICATE`] and [`EXTRA_CERTIFICATE`], so the certificate
/// rows address a listener that takes certificates at all.
async fn create_tls_listener(emulator: &Emulator, lb_arn: &str, tg_arn: &str) -> String {
    let mut form = action("CreateListener");
    form.push(p("LoadBalancerArn", lb_arn));
    form.push(p("Protocol", "HTTPS"));
    form.push(p("Port", "443"));
    form.push(p("Certificates.member.1.CertificateArn", CERTIFICATE));
    form.push(p("DefaultActions.member.1.Type", "forward"));
    form.push(p("DefaultActions.member.1.TargetGroupArn", tg_arn));
    let reply = call(&emulator.router, &form).await;
    assert_eq!(
        reply.status, 200,
        "case setup: creating the TLS listener expected 200, got {} body {}",
        reply.status, reply.body
    );
    let arn = reply
        .text("ListenerArn")
        .unwrap_or_else(|| panic!("case setup: no ListenerArn in {}", reply.body));

    let mut added = action("AddListenerCertificates");
    added.push(p("ListenerArn", &arn));
    added.push(p("Certificates.member.1.CertificateArn", EXTRA_CERTIFICATE));
    let reply = call(&emulator.router, &added).await;
    assert_eq!(
        reply.status, 200,
        "case setup: giving the TLS listener a second certificate expected 200, got {} body {}",
        reply.status, reply.body
    );
    arn
}

/// Register [`INSTANCE_TARGET`] in `tg_arn`, so the deregistration row withdraws a target
/// that is really registered.
async fn register_instance(emulator: &Emulator, tg_arn: &str) {
    let mut form = action("RegisterTargets");
    form.push(p("TargetGroupArn", tg_arn));
    form.push(p("Targets.member.1.Id", INSTANCE_TARGET));
    let reply = call(&emulator.router, &form).await;
    assert_eq!(
        reply.status, 200,
        "case setup: registering {INSTANCE_TARGET} expected 200, got {} body {}",
        reply.status, reply.body
    );
}

/// The tag key the fixture stores, which the tag rows read and withdraw.
const TAG_KEY: &str = "owner";

/// Store [`TAG_KEY`] on `arn`.
async fn tag(emulator: &Emulator, arn: &str) {
    let mut form = action("AddTags");
    form.push(p("ResourceArns.member.1", arn));
    form.push(p("Tags.member.1.Key", TAG_KEY));
    form.push(p("Tags.member.1.Value", "sweep"));
    let reply = call(&emulator.router, &form).await;
    assert_eq!(
        reply.status, 200,
        "case setup: tagging {arn} expected 200, got {} body {}",
        reply.status, reply.body
    );
}

/// Create a trust store named `name` from the bundle staged at `key`, and answer its ARN.
async fn create_trust_store(emulator: &Emulator, name: &str, key: &str) -> String {
    let mut form = action("CreateTrustStore");
    form.push(p("Name", name));
    form.push(p("CaCertificatesBundleS3Bucket", BUNDLE_BUCKET));
    form.push(p("CaCertificatesBundleS3Key", key));
    let reply = call(&emulator.router, &form).await;
    assert_eq!(
        reply.status, 200,
        "case setup: creating the trust store expected 200, got {} body {}",
        reply.status, reply.body
    );
    first_text(&reply.body, "TrustStoreArn")
        .unwrap_or_else(|| panic!("case setup: no TrustStoreArn in {}", reply.body))
}

/// Add the revocation list staged at `key` to `trust_store`, and answer the revocation id
/// the service minted for it.
async fn add_revocation(emulator: &Emulator, trust_store: &str, key: &str) -> String {
    let mut form = action("AddTrustStoreRevocations");
    form.push(p("TrustStoreArn", trust_store));
    form.push(p("RevocationContents.member.1.S3Bucket", BUNDLE_BUCKET));
    form.push(p("RevocationContents.member.1.S3Key", key));
    let reply = call(&emulator.router, &form).await;
    assert_eq!(
        reply.status, 200,
        "case setup: adding the revocation list expected 200, got {} body {}",
        reply.status, reply.body
    );
    first_text(&reply.body, "RevocationId")
        .unwrap_or_else(|| panic!("case setup: no RevocationId in {}", reply.body))
}

// ---------------------------------------------------------------------------
// One request per declared action, and the answer it is owed
// ---------------------------------------------------------------------------

/// The request that drives `operation` against `fixture`, and the answer it is owed.
///
/// Every action the model binds has a row (`smithy_query(query="list_operations")`: 51),
/// and an action with none fails the case rather than going undriven. A row is `Served`
/// wherever the operation has something to do against the fixture, and `Refused` with a code
/// the MODEL declares for that operation wherever the request is one the service has to
/// refuse (`smithy_query(query="operation",operation="<Operation>")`: each error's
/// `awsQueryError` code).
fn request_for(operation: &str, fixture: &Fixture) -> (Form, Outcome) {
    let mut form = action(operation);
    let outcome = match operation {
        "AddListenerCertificates" => {
            form.push(p("ListenerArn", &fixture.tls_listener));
            form.push(p("Certificates.member.1.CertificateArn", EXTRA_CERTIFICATE));
            Outcome::Served
        }
        "AddTags" => {
            form.push(p("ResourceArns.member.1", &fixture.target_group));
            form.push(p("Tags.member.1.Key", "tier"));
            form.push(p("Tags.member.1.Value", "edge"));
            Outcome::Served
        }
        "AddTrustStoreRevocations" => {
            form.push(p("TrustStoreArn", &fixture.trust_store));
            form.push(p("RevocationContents.member.1.S3Bucket", BUNDLE_BUCKET));
            form.push(p("RevocationContents.member.1.S3Key", &fixture.revocation_key));
            Outcome::Served
        }
        "CreateListener" => {
            form.push(p("LoadBalancerArn", &fixture.load_balancer));
            form.push(p("Protocol", "HTTP"));
            form.push(p("Port", "8080"));
            form.push(p("DefaultActions.member.1.Type", "forward"));
            form.push(p("DefaultActions.member.1.TargetGroupArn", &fixture.target_group));
            Outcome::Served
        }
        "CreateLoadBalancer" => {
            form.push(p("Name", "sweep-second-alb"));
            form.push(p("Subnets.member.1", "subnet-11111111"));
            form.push(p("Subnets.member.2", "subnet-22222222"));
            Outcome::Served
        }
        "CreateRule" => {
            form.push(p("ListenerArn", &fixture.listener));
            form.push(p("Priority", "20"));
            form.push(p("Conditions.member.1.Field", "path-pattern"));
            form.push(p("Conditions.member.1.Values.member.1", "/sweep"));
            form.extend(forward_to(&fixture.target_group));
            Outcome::Served
        }
        "CreateTargetGroup" => {
            form.push(p("Name", "sweep-new-tg"));
            form.push(p("Protocol", "HTTP"));
            form.push(p("Port", "80"));
            form.push(p("VpcId", VPC));
            Outcome::Served
        }
        "CreateTrustStore" => {
            form.push(p("Name", "sweep-second-trust-store"));
            form.push(p("CaCertificatesBundleS3Bucket", BUNDLE_BUCKET));
            form.push(p("CaCertificatesBundleS3Key", &fixture.bundle_key));
            Outcome::Served
        }
        "DeleteListener" => {
            form.push(p("ListenerArn", &fixture.listener));
            Outcome::Served
        }
        "DeleteLoadBalancer" => {
            form.push(p("LoadBalancerArn", &fixture.load_balancer));
            Outcome::Served
        }
        "DeleteRule" => {
            form.push(p("RuleArn", &fixture.rule));
            Outcome::Served
        }
        "DeleteSharedTrustStoreAssociation" => {
            form.push(p("TrustStoreArn", &fixture.trust_store));
            // A resource of ANOTHER account, which is what a SHARED association is between:
            // this emulator holds none, so the modelled `AssociationNotFound` is owed.
            form.push(p(
                "ResourceArn",
                "arn:aws:elasticloadbalancing:us-west-2:111122223333:listener/app/shared/\
                 0123456789abcdef/fedcba9876543210",
            ));
            Outcome::Refused("AssociationNotFound")
        }
        "DeleteTargetGroup" => {
            // The free group: the one the listener forwards to is refused with
            // `ResourceInUse`, which `target_group_in_use_refusal.rs` tables.
            form.push(p("TargetGroupArn", &fixture.spare_target_group));
            Outcome::Served
        }
        "DeleteTrustStore" => {
            form.push(p("TrustStoreArn", &fixture.trust_store));
            Outcome::Served
        }
        "DeregisterTargets" => {
            form.push(p("TargetGroupArn", &fixture.target_group));
            form.push(p("Targets.member.1.Id", INSTANCE_TARGET));
            Outcome::Served
        }
        "DescribeAccountLimits" => Outcome::Served,
        "DescribeCapacityReservation" => {
            form.push(p("LoadBalancerArn", &fixture.load_balancer));
            Outcome::Served
        }
        "DescribeListenerAttributes" => {
            form.push(p("ListenerArn", &fixture.listener));
            Outcome::Served
        }
        "DescribeListenerCertificates" => {
            form.push(p("ListenerArn", &fixture.tls_listener));
            Outcome::Served
        }
        "DescribeListeners" => {
            form.push(p("LoadBalancerArn", &fixture.load_balancer));
            Outcome::Served
        }
        "DescribeLoadBalancerAttributes" => {
            form.push(p("LoadBalancerArn", &fixture.load_balancer));
            Outcome::Served
        }
        "DescribeLoadBalancers" => Outcome::Served,
        "DescribeRules" => {
            form.push(p("ListenerArn", &fixture.listener));
            Outcome::Served
        }
        "DescribeSSLPolicies" => Outcome::Served,
        "DescribeTags" => {
            form.push(p("ResourceArns.member.1", &fixture.target_group));
            Outcome::Served
        }
        "DescribeTargetGroupAttributes" => {
            form.push(p("TargetGroupArn", &fixture.target_group));
            Outcome::Served
        }
        "DescribeTargetGroups" => Outcome::Served,
        "DescribeTargetHealth" => {
            form.push(p("TargetGroupArn", &fixture.target_group));
            Outcome::Served
        }
        "DescribeTrustStoreAssociations" => {
            form.push(p("TrustStoreArn", &fixture.trust_store));
            Outcome::Served
        }
        "DescribeTrustStoreRevocations" => {
            form.push(p("TrustStoreArn", &fixture.trust_store));
            Outcome::Served
        }
        "DescribeTrustStores" => Outcome::Served,
        "GetResourcePolicy" => {
            form.push(p("ResourceArn", &fixture.trust_store));
            Outcome::Served
        }
        "GetTrustStoreCaCertificatesBundle" => {
            form.push(p("TrustStoreArn", &fixture.trust_store));
            Outcome::Served
        }
        "GetTrustStoreRevocationContent" => {
            form.push(p("TrustStoreArn", &fixture.trust_store));
            form.push(p("RevocationId", &fixture.revocation_id));
            Outcome::Served
        }
        "ModifyCapacityReservation" => {
            form.push(p("LoadBalancerArn", &fixture.load_balancer));
            form.push(p("ResetCapacityReservation", "true"));
            Outcome::Served
        }
        "ModifyIpPools" => {
            form.push(p("LoadBalancerArn", &fixture.load_balancer));
            form.push(p("IpamPools.Ipv4IpamPoolId", "ipam-pool-0123456789abcdef0"));
            Outcome::Served
        }
        "ModifyListener" => {
            form.push(p("ListenerArn", &fixture.listener));
            form.push(p("Port", "8081"));
            Outcome::Served
        }
        "ModifyListenerAttributes" => {
            form.push(p("ListenerArn", &fixture.listener));
            // An attribute of the Application family, which the fixture's listener belongs
            // to (`ListenerAttribute$Key`'s own documentation lists it there).
            form.push(p(
                "Attributes.member.1.Key",
                "routing.http.response.server.enabled",
            ));
            form.push(p("Attributes.member.1.Value", "false"));
            Outcome::Served
        }
        "ModifyLoadBalancerAttributes" => {
            form.push(p("LoadBalancerArn", &fixture.load_balancer));
            form.push(p("Attributes.member.1.Key", "idle_timeout.timeout_seconds"));
            form.push(p("Attributes.member.1.Value", "120"));
            Outcome::Served
        }
        "ModifyRule" => {
            form.push(p("RuleArn", &fixture.rule));
            form.extend(forward_to(&fixture.spare_target_group));
            Outcome::Served
        }
        "ModifyTargetGroup" => {
            form.push(p("TargetGroupArn", &fixture.target_group));
            form.push(p("HealthCheckIntervalSeconds", "31"));
            Outcome::Served
        }
        "ModifyTargetGroupAttributes" => {
            form.push(p("TargetGroupArn", &fixture.target_group));
            form.push(p(
                "Attributes.member.1.Key",
                "deregistration_delay.timeout_seconds",
            ));
            form.push(p("Attributes.member.1.Value", "60"));
            Outcome::Served
        }
        "ModifyTrustStore" => {
            form.push(p("TrustStoreArn", &fixture.trust_store));
            form.push(p("CaCertificatesBundleS3Bucket", BUNDLE_BUCKET));
            form.push(p("CaCertificatesBundleS3Key", &fixture.bundle_key));
            Outcome::Served
        }
        "RegisterTargets" => {
            form.push(p("TargetGroupArn", &fixture.target_group));
            form.push(p("Targets.member.1.Id", "i-0fedcba9876543210"));
            Outcome::Served
        }
        "RemoveListenerCertificates" => {
            form.push(p("ListenerArn", &fixture.tls_listener));
            form.push(p("Certificates.member.1.CertificateArn", EXTRA_CERTIFICATE));
            Outcome::Served
        }
        "RemoveTags" => {
            form.push(p("ResourceArns.member.1", &fixture.target_group));
            form.push(p("TagKeys.member.1", TAG_KEY));
            Outcome::Served
        }
        "RemoveTrustStoreRevocations" => {
            form.push(p("TrustStoreArn", &fixture.trust_store));
            form.push(p("RevocationIds.member.1", &fixture.revocation_id));
            Outcome::Served
        }
        "SetIpAddressType" => {
            form.push(p("LoadBalancerArn", &fixture.load_balancer));
            form.push(p("IpAddressType", "ipv4"));
            Outcome::Served
        }
        "SetRulePriorities" => {
            form.push(p("RulePriorities.member.1.RuleArn", &fixture.rule));
            form.push(p("RulePriorities.member.1.Priority", "30"));
            Outcome::Served
        }
        "SetSecurityGroups" => {
            form.push(p("LoadBalancerArn", &fixture.load_balancer));
            form.push(p("SecurityGroups.member.1", "sg-0123456789abcdef0"));
            Outcome::Served
        }
        "SetSubnets" => {
            form.push(p("LoadBalancerArn", &fixture.load_balancer));
            form.push(p("Subnets.member.1", "subnet-11111111"));
            form.push(p("Subnets.member.2", "subnet-33333333"));
            Outcome::Served
        }
        other => panic!(
            "case {other:?}: every action the model binds is driven here — add a row for this one"
        ),
    };
    (form, outcome)
}

/// Assert that `reply` is not the scaffold channel's answer: an operation nobody has written
/// answers `501 NotImplemented` whatever the request carries, so this is what tells an
/// implemented operation from an absent one.
fn assert_not_the_scaffold(case: &str, reply: &Reply) {
    assert_ne!(
        reply.status, SCAFFOLD_STATUS,
        "case {case:?}: an operation of this service must answer from state, not the \
         scaffold's {SCAFFOLD_STATUS}, got body {}",
        reply.body
    );
    assert_ne!(
        reply.error_code(),
        SCAFFOLD_CODE,
        "case {case:?}: {SCAFFOLD_CODE} is not an answer this API gives, got {} body {}",
        reply.status,
        reply.body
    );
}

#[tokio::test]
async fn every_declared_operation_answers_its_own_document_or_a_modelled_error() {
    let actions = declared_actions();
    assert!(
        actions.len() >= 51,
        "expected the actions the model binds, got {actions:?}"
    );

    for operation in actions {
        // Fresh state per row: a row may delete the very resource it addresses.
        let fixture = Fixture::new().await;
        let (form, outcome) = request_for(&operation, &fixture);
        let reply = call(&fixture.emulator.router, &form).await;
        assert_not_the_scaffold(&operation, &reply);
        match outcome {
            Outcome::Served => {
                assert_eq!(
                    reply.status, 200,
                    "case {operation:?}: expected the operation's own document, got {} body {}",
                    reply.status, reply.body
                );
                let envelope = format!("<{operation}Response");
                assert!(
                    reply.body.contains(&envelope),
                    "case {operation:?}: expected an {envelope}> envelope, got {}",
                    reply.body
                );
            }
            Outcome::Refused(code) => {
                assert_eq!(
                    (reply.status, reply.error_code().as_str()),
                    (400, code),
                    "case {operation:?}: expected 400 {code}, got {} body {}",
                    reply.status,
                    reply.body
                );
            }
        }
    }
}
