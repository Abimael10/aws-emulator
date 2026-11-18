// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `StartVpcEndpointServicePrivateDnsVerification`, driven over `ec2Query`
//! (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: starting the verification of an endpoint service's private DNS name puts
//! that name under a verification the service runs ASYNCHRONOUSLY and answers `true` — "consumers
//! of the endpoint service can use the private name only when the state is `verified`"
//! (`PrivateDnsNameConfiguration$State`), and the catalog that member is drawn from carries
//! `pendingVerification` beside it — so the name is reported as still pending when the call
//! returns, is reported as verified once the span this emulator gives a non-terminal state has
//! passed with nobody calling again, and is reported that one way by every operation that reports
//! it. The record the owner has to publish is untouched by the start, a name already verified
//! stays verified, and a request naming a service this account does not offer, or one carrying no
//! private DNS name at all, is refused with the EC2 code for its cause and starts nothing.
//!
//! Each table row is a distinct input along what that rule generalises over: the shape of the
//! private DNS name (how many labels, how long a label, which top-level domain), whether the name
//! was assigned by the create or by a later modify, whether a connection to the service must be
//! accepted, and how many services the account offers while one of them is verified.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, DNS_NAME_STATE_PENDING_VERIFICATION,
    DNS_NAME_STATE_VERIFIED, INVALID_PARAMETER, INVALID_VPC_ENDPOINT_SERVICE_ID_MALFORMED,
    INVALID_VPC_ENDPOINT_SERVICE_ID_NOT_FOUND, TRANSITION_DELAY, VPC_ENDPOINT_SERVICE_ID_KIND,
    id_not_found_message,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, error_code, error_message, indexed};
use super::wire::{Param, child_text, document, param};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The HTTP status EC2 answers `DryRunOperation` with: the request was refused because a
/// precondition the CALLER set holds, not because it was malformed.
const DRY_RUN_STATUS: u16 = 412;

/// The code EC2 answers an authorized `DryRun` with.
const DRY_RUN_OPERATION: &str = "DryRunOperation";

/// How long a poll below is given before it reports that the verification never finished —
/// generously past [`TRANSITION_DELAY`], so a slow or loaded host fails nothing, while a state the
/// loop's guard never leaves still ends the test instead of hanging.
fn poll_deadline() -> Duration {
    TRANSITION_DELAY * 40
}

/// How long a poll waits between reads, as a client's own waiter does.
fn poll_interval() -> Duration {
    TRANSITION_DELAY / 20
}

/// The emulator the tests drive, with the engine beside it: one assertion asks the store itself
/// whether a verification is still owed, which no operation reports.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    super::ec2_query::emulator()
}

/// One `action` call carrying `params`, as its HTTP status and XML body.
async fn call(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    super::ec2_query::call(router, REGION, action, params).await
}

/// One `action` call that has to succeed, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// One element of a response body, or a panic naming the case and showing the body.
fn member(case: &str, body: &str, name: &str) -> String {
    element(body, name).unwrap_or_else(|| panic!("case {case:?}: no <{name}> in {body}"))
}

/// The ARN of the Network Load Balancer `name`, as a request that puts a service behind one
/// carries it. A load balancer is a resource of another service, so this is only ever a
/// well-formed ARN here.
fn load_balancer_arn(name: &str) -> String {
    format!(
        "arn:aws:elasticloadbalancing:{REGION}:{account}:loadbalancer/net/{name}/0123456789abcdef",
        account = engine::emulator_account_id(),
    )
}

/// The `privateDnsNameConfiguration` member `name` of the one service a describe reported, read
/// through the XML parser so a member of that structure is never confused with one of the same
/// name elsewhere in the body.
fn verification_member(case: &str, body: &str, name: &str) -> Option<String> {
    let parsed = document(case, body);
    parsed
        .descendants()
        .find(|node| {
            node.is_element() && node.tag_name().name() == "privateDnsNameConfiguration"
        })
        .and_then(|node| child_text(node, name))
}

/// What `DescribeVpcEndpointServiceConfigurations` reports for the one service `service_id`.
async fn described(case: &str, router: &MultiServiceRouter, service_id: &str) -> String {
    ok(
        case,
        router,
        "DescribeVpcEndpointServiceConfigurations",
        &indexed("ServiceId", &[service_id]),
    )
    .await
}

/// The verification state `DescribeVpcEndpointServiceConfigurations` reports for `service_id`.
async fn verification_state(case: &str, router: &MultiServiceRouter, service_id: &str) -> String {
    let body = described(case, router, service_id).await;
    verification_member(case, &body, "state")
        .unwrap_or_else(|| panic!("case {case:?}: no verification state in {body}"))
}

/// Poll the describe while the name is still reported as pending, exactly as a client waiting on
/// its own verification does, and answer the state it finally reported.
///
/// The guard is the published `pendingVerification` value, so a state the catalog does not spell
/// leaves this loop at once — which is what the assertion on the result then catches.
async fn poll_until_not_pending(
    case: &str,
    router: &MultiServiceRouter,
    service_id: &str,
) -> String {
    let deadline = Instant::now() + poll_deadline();
    let mut state = verification_state(case, router, service_id).await;
    while state == DNS_NAME_STATE_PENDING_VERIFICATION && Instant::now() < deadline {
        tokio::time::sleep(poll_interval()).await;
        state = verification_state(case, router, service_id).await;
    }
    state
}

/// Whether the store still owes `service_id` the outcome of a verification.
///
/// Asked of the store because no operation reports it: a call that starts nothing must leave
/// nothing owed, which a read of a reported state cannot tell from a verification that has not
/// come due yet.
fn verification_owed(engine: &engine::InMemoryEngine, service_id: &str) -> bool {
    engine
        .state::<Ec2State>()
        .lock()
        .vpc_endpoint_service_private_dns_verified_at
        .contains_key(service_id)
}

/// Where the private DNS name of a service under test came from: the two calls that assign one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AssignedBy {
    /// `CreateVpcEndpointServiceConfigurationRequest$PrivateDnsName`.
    Create,
    /// `ModifyVpcEndpointServiceConfigurationRequest$PrivateDnsName`, on a service created
    /// without one.
    Modify,
}

/// One row of the verification table: an endpoint service of the account carrying a private DNS
/// name, configured the way a service provider that is about to prove ownership configures it.
struct ServiceCase {
    case: &'static str,
    /// The load balancer the service stands behind, which also names the row's service.
    load_balancer: &'static str,
    /// The private DNS name assigned to the service.
    private_dns_name: &'static str,
    /// Which call assigned that name.
    assigned_by: AssignedBy,
    /// What the service's `AcceptanceRequired` is set to.
    acceptance_required: bool,
}

/// The rows: names differing in how many labels they carry, how long their longest label is and
/// which top-level domain they sit under, assigned by each of the two calls that can assign one,
/// on services that differ in whether a connection must be accepted. None of those is what
/// decides the verification.
fn service_cases() -> Vec<ServiceCase> {
    vec![
        ServiceCase {
            case: "a two-label name assigned by the create",
            load_balancer: "payments",
            private_dns_name: "example.com",
            assigned_by: AssignedBy::Create,
            acceptance_required: true,
        },
        ServiceCase {
            case: "a subdomain assigned by the create, with acceptance waived",
            load_balancer: "ledger",
            private_dns_name: "ledger.svc.example.net",
            assigned_by: AssignedBy::Create,
            acceptance_required: false,
        },
        ServiceCase {
            case: "a deep name assigned by a later modify",
            load_balancer: "search",
            private_dns_name: "a.b.c.d.search.example.org",
            assigned_by: AssignedBy::Modify,
            acceptance_required: true,
        },
        ServiceCase {
            // The longest label a DNS name may carry (63 bytes, `MAX_DNS_LABEL_BYTES`).
            case: "a name whose first label is as long as a label may be",
            load_balancer: "long-label",
            private_dns_name:
                "abcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghij123.example.com",
            assigned_by: AssignedBy::Modify,
            acceptance_required: false,
        },
    ]
}

/// Configure one endpoint service of the account behind the Network Load Balancer `name`, with
/// `private_dns_name` assigned at the create when it is given, and answer its `ServiceId`.
async fn create_service(
    case: &str,
    router: &MultiServiceRouter,
    name: &str,
    acceptance_required: bool,
    private_dns_name: Option<&str>,
) -> String {
    let arn = load_balancer_arn(name);
    let mut params = vec![
        param("NetworkLoadBalancerArn.1", &arn),
        param("AcceptanceRequired", &acceptance_required.to_string()),
    ];
    if let Some(private_dns_name) = private_dns_name {
        params.push(param("PrivateDnsName", private_dns_name));
    }
    let body = ok(
        case,
        router,
        "CreateVpcEndpointServiceConfiguration",
        &params,
    )
    .await;
    member(case, &body, "serviceId")
}

/// Assign `private_dns_name` to the service `service_id` through the modify.
async fn assign_private_dns_name(
    case: &str,
    router: &MultiServiceRouter,
    service_id: &str,
    private_dns_name: &str,
) {
    let body = ok(
        case,
        router,
        "ModifyVpcEndpointServiceConfiguration",
        &[
            param("ServiceId", service_id),
            param("PrivateDnsName", private_dns_name),
        ],
    )
    .await;
    assert_eq!(
        element(&body, "return").as_deref(),
        Some("true"),
        "case {case:?}: the modify that assigns a private DNS name must succeed, got {body}"
    );
}

/// Start the verification of `service_id`'s private DNS name, asserting the answered flag.
async fn start_verification(case: &str, router: &MultiServiceRouter, service_id: &str) {
    let body = ok(
        case,
        router,
        "StartVpcEndpointServicePrivateDnsVerification",
        &[param("ServiceId", service_id)],
    )
    .await;
    assert_eq!(
        element(&body, "return").as_deref(),
        Some("true"),
        "case {case:?}: expected the start to answer true, got {body}"
    );
}

/// A service provider that assigns a private DNS name, starts its verification and waits for it:
/// the name is pending when the start returns, verified once the clock has carried it there with
/// nothing called in between, and reported that one way by both describes — while a service of the
/// same account that nobody started a verification for is left pending.
#[tokio::test]
async fn starting_verification_carries_a_private_dns_name_from_pending_to_verified() {
    let (router, engine) = emulator();

    // The service nobody starts a verification for: its name stays unproven however many
    // verifications the account runs beside it.
    let untouched = create_service(
        "the untouched service",
        &router,
        "untouched",
        true,
        Some("untouched.example.com"),
    )
    .await;

    // Every row is configured first, so the services are verified side by side rather than one
    // span after another, and each is read back before and straight after its own start.
    let mut started: Vec<(&'static str, String, String)> = Vec::new();
    for row in service_cases() {
        let case = row.case;
        let assigned_at_create = match row.assigned_by {
            AssignedBy::Create => Some(row.private_dns_name),
            AssignedBy::Modify => None,
        };
        let service_id = create_service(
            case,
            &router,
            row.load_balancer,
            row.acceptance_required,
            assigned_at_create,
        )
        .await;
        if row.assigned_by == AssignedBy::Modify {
            assign_private_dns_name(case, &router, &service_id, row.private_dns_name).await;
        }

        // "If you set or modify the private DNS name, you must prove that you own the private
        // DNS domain name": a name nobody has proved anything about yet is pending, with the
        // record that proof is published as.
        let before = described(case, &router, &service_id).await;
        assert_eq!(
            element(&before, "privateDnsName").as_deref(),
            Some(row.private_dns_name),
            "case {case:?}: expected the assigned private DNS name, got {before}"
        );
        assert_eq!(
            verification_member(case, &before, "state").as_deref(),
            Some(DNS_NAME_STATE_PENDING_VERIFICATION),
            "case {case:?}: expected an unproven name before the start, got {before}"
        );
        let record = (
            verification_member(case, &before, "type"),
            verification_member(case, &before, "name"),
            verification_member(case, &before, "value"),
        );
        assert!(
            !verification_owed(&engine, &service_id),
            "case {case:?}: nothing must be owed on {service_id:?} before the start"
        );

        start_verification(case, &router, &service_id).await;

        // The span is never zero, so a caller reading straight back still finds the
        // verification under way — and the record it has to publish is the one it was given.
        let after = described(case, &router, &service_id).await;
        assert_eq!(
            verification_member(case, &after, "state").as_deref(),
            Some(DNS_NAME_STATE_PENDING_VERIFICATION),
            "case {case:?}: expected the verification still under way straight after the \
             start, got {after}"
        );
        assert_eq!(
            (
                verification_member(case, &after, "type"),
                verification_member(case, &after, "name"),
                verification_member(case, &after, "value"),
            ),
            record,
            "case {case:?}: the record the owner must publish must be untouched by the start, \
             got {after}"
        );
        assert!(
            verification_owed(&engine, &service_id),
            "case {case:?}: the start must leave the outcome owed on {service_id:?}"
        );

        let service_name = member(case, &before, "serviceName");
        started.push((case, service_id, service_name));
    }

    // The wait a provider makes, on each service: nothing but the clock moves the name.
    for (case, service_id, service_name) in &started {
        let polled = poll_until_not_pending(case, &router, service_id).await;
        assert_eq!(
            polled, DNS_NAME_STATE_VERIFIED,
            "case {case:?}: expected the poll on {service_id:?} to end at \
             {DNS_NAME_STATE_VERIFIED:?}, got {polled:?}"
        );
        assert!(
            !verification_owed(&engine, service_id),
            "case {case:?}: the verification of {service_id:?} must be owed no longer once it \
             has run"
        );

        // The same materialized state is what a consumer's own describe reports, so one name is
        // never reported two ways.
        let consumer_view = ok(
            case,
            &router,
            "DescribeVpcEndpointServices",
            &indexed("ServiceName", &[service_name.as_str()]),
        )
        .await;
        assert_eq!(
            element(&consumer_view, "privateDnsNameVerificationState").as_deref(),
            Some(DNS_NAME_STATE_VERIFIED),
            "case {case:?}: expected the consumer's describe of {service_name:?} to report the \
             verified name, got {consumer_view}"
        );
    }

    // The service nobody started a verification for is untouched by every verification beside
    // it: its name is still waiting on a proof.
    let left_alone = verification_state("the untouched service", &router, &untouched).await;
    assert_eq!(
        left_alone, DNS_NAME_STATE_PENDING_VERIFICATION,
        "a service nobody started a verification for must stay \
         {DNS_NAME_STATE_PENDING_VERIFICATION:?}, got {left_alone:?}"
    );
}

/// A name already verified stays verified when the verification is started again: the proof the
/// owner published is still published, so the call answers true and takes nothing away from the
/// consumers already using the name.
#[tokio::test]
async fn starting_verification_again_leaves_a_verified_name_verified() {
    let case = "a second start on a verified name";
    let (router, engine) = emulator();
    let service_id = create_service(case, &router, "repeated", true, Some("again.example.com")).await;

    start_verification(case, &router, &service_id).await;
    let polled = poll_until_not_pending(case, &router, &service_id).await;
    assert_eq!(
        polled, DNS_NAME_STATE_VERIFIED,
        "case {case:?}: expected the first verification to finish, got {polled:?}"
    );

    start_verification(case, &router, &service_id).await;
    let after = verification_state(case, &router, &service_id).await;
    assert_eq!(
        after, DNS_NAME_STATE_VERIFIED,
        "case {case:?}: expected a verified name to stay {DNS_NAME_STATE_VERIFIED:?}, got \
         {after:?}"
    );
    assert!(
        !verification_owed(&engine, &service_id),
        "case {case:?}: a verified name owes no further outcome"
    );
}

/// Replacing the private DNS name cancels the verification owed to the name it replaced, and the
/// new name is verified only once its OWN verification has been started and run.
#[tokio::test]
async fn replacing_the_private_dns_name_restarts_the_verification() {
    let case = "a name replaced while its verification was owed";
    let (router, engine) = emulator();
    let service_id = create_service(case, &router, "replaced", true, Some("first.example.com")).await;

    start_verification(case, &router, &service_id).await;
    assert!(
        verification_owed(&engine, &service_id),
        "case {case:?}: the start must leave the outcome owed"
    );

    assign_private_dns_name(case, &router, &service_id, "second.example.com").await;
    assert!(
        !verification_owed(&engine, &service_id),
        "case {case:?}: the verification of the name that was replaced is owed no longer"
    );
    let replaced = verification_state(case, &router, &service_id).await;
    assert_eq!(
        replaced, DNS_NAME_STATE_PENDING_VERIFICATION,
        "case {case:?}: a name just assigned must be {DNS_NAME_STATE_PENDING_VERIFICATION:?}, \
         got {replaced:?}"
    );

    start_verification(case, &router, &service_id).await;
    let polled = poll_until_not_pending(case, &router, &service_id).await;
    assert_eq!(
        polled, DNS_NAME_STATE_VERIFIED,
        "case {case:?}: expected the verification of the new name to finish, got {polled:?}"
    );
}

/// An authorized `DryRun` reports that the request would have succeeded and starts nothing: the
/// name is left where it was, with no outcome owed, and a later real start still verifies it.
#[tokio::test]
async fn a_dry_run_start_verifies_nothing() {
    let case = "a dry run on a service with a private DNS name";
    let (router, engine) = emulator();
    let service_id = create_service(case, &router, "dry-run", true, Some("dry.example.com")).await;

    let (status, body) = call(
        &router,
        "StartVpcEndpointServicePrivateDnsVerification",
        &[param("ServiceId", &service_id), param("DryRun", "true")],
    )
    .await;
    assert_eq!(
        status, DRY_RUN_STATUS,
        "case {case:?}: expected HTTP {DRY_RUN_STATUS} from an authorized dry run, got {status} \
         with body {body}"
    );
    assert_eq!(
        error_code(&body),
        DRY_RUN_OPERATION,
        "case {case:?}: expected {DRY_RUN_OPERATION}, got {body}"
    );
    assert!(
        !verification_owed(&engine, &service_id),
        "case {case:?}: a dry run must leave no outcome owed"
    );
    let unchanged = verification_state(case, &router, &service_id).await;
    assert_eq!(
        unchanged, DNS_NAME_STATE_PENDING_VERIFICATION,
        "case {case:?}: a dry run must leave the name at \
         {DNS_NAME_STATE_PENDING_VERIFICATION:?}, got {unchanged:?}"
    );

    start_verification(case, &router, &service_id).await;
    let polled = poll_until_not_pending(case, &router, &service_id).await;
    assert_eq!(
        polled, DNS_NAME_STATE_VERIFIED,
        "case {case:?}: expected the real start to verify the name, got {polled:?}"
    );
}

/// What a request the service cannot start a verification for is refused with: a `ServiceId` that
/// is no id of this kind, one this account offers no service under, and a service that carries no
/// private DNS name to prove ownership of — each with the code EC2 answers that cause with, and
/// each starting nothing.
#[tokio::test]
async fn a_verification_the_service_cannot_start_is_refused() {
    let (router, engine) = emulator();

    // The service the refusals are measured against: one carrying no private DNS name at all.
    let nameless = create_service("no private DNS name", &router, "nameless", true, None).await;
    // A well-formed id of this kind the account offers no service under.
    let absent = "vpce-svc-0123456789abcdef0";

    let rows: [(&str, &str, &str, String); 4] = [
        (
            "a value that is no id at all",
            "my-endpoint-service",
            INVALID_VPC_ENDPOINT_SERVICE_ID_MALFORMED,
            String::new(),
        ),
        (
            "an id of another resource",
            "vpce-0123456789abcdef0",
            INVALID_VPC_ENDPOINT_SERVICE_ID_MALFORMED,
            String::new(),
        ),
        (
            "an id naming no service of the account",
            absent,
            INVALID_VPC_ENDPOINT_SERVICE_ID_NOT_FOUND,
            id_not_found_message(VPC_ENDPOINT_SERVICE_ID_KIND, absent),
        ),
        (
            "a service carrying no private DNS name",
            nameless.as_str(),
            INVALID_PARAMETER,
            format!("The service {nameless} does not have a private DNS name configured."),
        ),
    ];

    for (case, service_id, expected_code, expected_message) in rows {
        let (status, body) = call(
            &router,
            "StartVpcEndpointServicePrivateDnsVerification",
            &[param("ServiceId", service_id)],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            expected_code,
            "case {case:?}: expected {expected_code}, got {body}"
        );
        if !expected_message.is_empty() {
            assert_eq!(
                error_message(&body),
                expected_message,
                "case {case:?}: expected the sentence EC2 words for this cause, got {body}"
            );
        }
        assert!(
            !verification_owed(&engine, service_id),
            "case {case:?}: a refused request must start nothing for {service_id:?}"
        );
    }

    // The service that was refused is untouched: it still carries no private DNS name, so there
    // is nothing for a consumer to resolve.
    let body = described("no private DNS name", &router, &nameless).await;
    assert_eq!(
        element(&body, "privateDnsName"),
        None,
        "a refused start must leave the service without a private DNS name, got {body}"
    );
}
