// SPDX-License-Identifier: CC0-1.0

//! End-to-end wire tests for the two ADMISSION rules `CreateUserProfile` weighs a request
//! against before it writes anything: the status the domain the profile would live in has
//! to report, and the account-level bound on how many profiles the service holds.
//!
//! Every case goes over the wire (`POST /` with `X-Amz-Target: SageMaker.<Op>`,
//! `awsJson1_1`) through the same `MultiServiceRouter` the production router uses, so what
//! these tests exercise is what a real SDK client gets. The harness (`Emulator`,
//! `error_code`, `message`, `str_field`) is the one `inference_deployment_lifecycle`
//! defines for this target, and the seeding helpers are `studio_user_profiles`'.
//!
//! The round trip, the single-sign-on rule, the duplicate-name collision and the
//! not-found refusals of the same operation are `studio_user_profiles`'; what is pinned
//! here is the pair of rules that suite leaves open — the domain-status gate over EVERY
//! status a domain of this service reaches, and the bound at each of its published edges.

use std::time::{Duration, Instant, SystemTime};

use serde_json::json;
use service_com_amazonaws_sagemaker_sagemaker::state::{
    self, ComAmazonawsSagemakerSagemakerState, DOMAIN_STATUS_DELETING, DOMAIN_STATUS_FAILED,
    DOMAIN_STATUS_IN_SERVICE, DOMAIN_STATUS_PENDING, USER_PROFILE_QUOTA,
};

use crate::inference_deployment_lifecycle::{error_code, message, str_field, Emulator};
use crate::studio_user_profiles::{
    domain_arn, minimal_domain_with_auth, seed_studio_domain, seed_studio_owner, user_profile_arn,
    wait_for_domain_status, POLL_INTERVAL, SETTLE_DEADLINE,
};

/// How far out this suite pushes a transition it holds OFF, so the status a row asserts
/// the gate against is the one the domain still reports however slowly the host runs.
/// Generous on purpose: no case here tests how long a transition takes.
const HELD_OFF: Duration = Duration::from_secs(3_600);

/// Create a domain in `region` and answer the `DomainId` it minted. The create leaves it
/// `Pending`, which is where each row below takes it from — so this does NOT wait for the
/// domain to settle, as `studio_user_profiles`' own create helper does.
async fn created_domain(emulator: &Emulator, region: &str, name: &str) -> String {
    let (status, created) = emulator
        .call_in(region, "CreateDomain", &minimal_domain_with_auth(name, "IAM"))
        .await;
    assert_eq!(
        status, 200,
        "case create-domain-{name}: expected HTTP 200 from CreateDomain, got {status} with \
         {created}"
    );
    str_field(&created, "DomainId")
}

/// Reach into the shared store the handlers read and change the stored domain `domain_id`.
///
/// What each row changes is only what the CLOCK is weighed against — the instant a
/// transition falls due, and what the create resolved about the domain's placement — so
/// every status asserted below is one the handlers' own paths settle the domain into
/// rather than one hand-written into the store.
fn adjust_stored_domain<F>(emulator: &Emulator, domain_id: &str, change: F)
where
    F: FnOnce(&mut state::Domain),
{
    let store = emulator
        .engine
        .state::<ComAmazonawsSagemakerSagemakerState>();
    let mut store = store.lock();
    let domain = store
        .domains
        .get_mut(domain_id)
        .unwrap_or_else(|| panic!("the created domain {domain_id} is in the store"));
    change(domain);
}

/// One row of the admission table: the status the domain reports when the profile create
/// arrives, the Region the pair is signed for and the name the create carries.
struct AdmissionRow {
    case: &'static str,
    region: &'static str,
    /// A member of `DomainStatus` the domains of this service reach.
    domain_status: &'static str,
    /// Whether a domain in that status admits the profile.
    admits: bool,
    name: String,
}

/// RULE: a user profile is a member of the domain the request names, and a domain admits a
/// member only once its own provisioning has settled into service — so the create is
/// served while the domain reports `InService` and refused from every other status it
/// reports, with the service-common `ValidationException` naming the profile it could not
/// create and the domain that stood in the way, and nothing is stored for the refusal.
#[tokio::test]
async fn a_user_profile_is_admitted_only_while_its_domain_reports_in_service() {
    let emulator = Emulator::new();

    // Rows: every `DomainStatus` a domain of this service reaches
    // (`smithy_query(query="shape", shape="DomainStatus")` also carries the update states,
    // which no implemented operation reaches), each in its own Region, with the name shapes
    // `UserProfileName`'s pattern admits at the edges — one character, mixed case, and the
    // 63-character maximum its length constraint allows.
    let rows = [
        AdmissionRow {
            case: "pending-domain",
            region: "us-east-1",
            domain_status: DOMAIN_STATUS_PENDING,
            admits: false,
            name: "studio-user".to_string(),
        },
        AdmissionRow {
            case: "failed-domain",
            region: "eu-west-1",
            domain_status: DOMAIN_STATUS_FAILED,
            admits: false,
            name: "A".to_string(),
        },
        AdmissionRow {
            case: "deleting-domain",
            region: "ap-northeast-1",
            domain_status: DOMAIN_STATUS_DELETING,
            admits: false,
            name: "b".repeat(63),
        },
        AdmissionRow {
            case: "in-service-domain",
            region: "us-west-2",
            domain_status: DOMAIN_STATUS_IN_SERVICE,
            admits: true,
            name: "Data-Scientist".to_string(),
        },
    ];

    for row in &rows {
        let case = row.case;
        let domain_id = created_domain(&emulator, row.region, &format!("admission-{case}")).await;

        // Put the domain in the status this row weighs the create against, through the
        // handlers' own paths wherever a call produces it.
        match row.domain_status {
            DOMAIN_STATUS_PENDING => {
                // The create left it `Pending`; holding its `InService` off keeps it there
                // for however long this case takes.
                adjust_stored_domain(&emulator, &domain_id, |domain| {
                    domain.in_service_due_at = SystemTime::now() + HELD_OFF;
                });
            }
            DOMAIN_STATUS_FAILED => {
                // A domain whose placement the account's network does not admit settles
                // into `Failed` with that reason beside it, which is what the create
                // recorded about the placement.
                adjust_stored_domain(&emulator, &domain_id, |domain| {
                    domain.creation_failure_reason = Some(state::subnet_outside_domain_vpc(
                        "subnet-0123456789abcdef0",
                        "vpc-0123456789abcdef0",
                    ));
                    domain.in_service_due_at = SystemTime::now();
                });
            }
            DOMAIN_STATUS_DELETING => {
                let (status, parsed) = emulator
                    .call_in(row.region, "DeleteDomain", &json!({ "DomainId": domain_id }))
                    .await;
                assert_eq!(
                    status, 200,
                    "case {case}: expected HTTP 200 from DeleteDomain, got {status} with {parsed}"
                );
                // The teardown left it `Deleting`; holding its removal off keeps it there.
                adjust_stored_domain(&emulator, &domain_id, |domain| {
                    domain.removal_due_at = Some(SystemTime::now() + HELD_OFF);
                });
            }
            _ => {}
        }

        // The status the gate is weighed against is the one a reader reports, so each case
        // states what it is testing rather than assuming it.
        wait_for_domain_status(&emulator, row.region, &domain_id, row.domain_status).await;

        let request = json!({ "DomainId": domain_id, "UserProfileName": row.name });
        let (status, parsed) = emulator
            .call_in(row.region, "CreateUserProfile", &request)
            .await;
        let expected_arn = user_profile_arn(row.region, &domain_id, &row.name);

        if row.admits {
            assert_eq!(
                status, 200,
                "case {case}: a domain in {} admits the profile, got {status} with {parsed}",
                row.domain_status
            );
            assert_eq!(
                str_field(&parsed, "UserProfileArn"),
                expected_arn,
                "case {case}: the served create mints the profile's ARN, got {parsed}"
            );
            let (read_status, described) = emulator
                .call_in(row.region, "DescribeUserProfile", &request)
                .await;
            assert_eq!(
                read_status, 200,
                "case {case}: the stored profile reads back, got {read_status} with {described}"
            );
            continue;
        }

        assert_eq!(
            status, 400,
            "case {case}: a domain in {} admits no member, got {status} with {parsed}",
            row.domain_status
        );
        assert_eq!(
            error_code(&parsed),
            "ValidationException",
            "case {case}: expected ValidationException, got {parsed}"
        );
        assert_eq!(
            message(&parsed),
            format!(
                "Unable to create UserProfile [{expected_arn}] because Domain [{}] is not in \
                 {DOMAIN_STATUS_IN_SERVICE}.",
                domain_arn(row.region, &domain_id)
            ),
            "case {case}: the refusal names the profile it could not create and the domain \
             that stood in the way, got {parsed}"
        );

        // Nothing was stored for the refused create.
        let (read_status, described) = emulator
            .call_in(row.region, "DescribeUserProfile", &request)
            .await;
        assert_eq!(
            (read_status, error_code(&described).as_str()),
            (400, "ResourceNotFound"),
            "case {case}: the refused create stored nothing, got {read_status} with {described}"
        );
    }
}

/// RULE: the user-profile store is bounded, so the account holds at most its quota of
/// profiles: the create that fills the last slot is served, the one past the bound is
/// refused with the operation's modelled `ResourceLimitExceeded` and stores nothing, and
/// the bound is a live count — once a profile's removal completes, its slot takes a create
/// again.
#[tokio::test]
async fn a_user_profile_create_past_the_account_bound_is_refused_with_the_modelled_limit() {
    let emulator = Emulator::new();
    let region = "us-east-1";
    let domain_id = "d-boundeduserprofiles";

    // The bound is the account's, so it is reached by filling the store itself: one slot
    // short of `state::USER_PROFILE_QUOTA`, in a domain that is already in service.
    seed_studio_domain(&emulator, region, domain_id);
    for index in 0..USER_PROFILE_QUOTA - 1 {
        seed_studio_owner(&emulator, region, domain_id, &format!("seeded-{index:04}"));
    }

    // One short of the bound: the create that takes the last slot is served.
    let last_slot = json!({ "DomainId": domain_id, "UserProfileName": "last-slot" });
    let (status, parsed) = emulator
        .call_in(region, "CreateUserProfile", &last_slot)
        .await;
    assert_eq!(
        status, 200,
        "case last-slot: the create that fills the bound is served, got {status} with {parsed}"
    );
    assert_eq!(
        str_field(&parsed, "UserProfileArn"),
        user_profile_arn(region, domain_id, "last-slot"),
        "case last-slot: the served create mints the profile's ARN, got {parsed}"
    );

    // One past it: refused with the modelled limit failure, which states the bound and
    // what the account already holds.
    let past_the_bound = json!({ "DomainId": domain_id, "UserProfileName": "past-the-bound" });
    let (status, parsed) = emulator
        .call_in(region, "CreateUserProfile", &past_the_bound)
        .await;
    assert_eq!(
        status, 400,
        "case past-the-bound: a create past the bound is refused, got {status} with {parsed}"
    );
    assert_eq!(
        error_code(&parsed),
        "ResourceLimitExceeded",
        "case past-the-bound: the refusal uses the operation's modelled limit error, got {parsed}"
    );
    assert_eq!(
        message(&parsed),
        format!(
            "The account-level service limit for user profiles is {USER_PROFILE_QUOTA}, with \
             current utilization of {USER_PROFILE_QUOTA} and a request delta of 1."
        ),
        "case past-the-bound: the refusal states the limit it hit and what the account already \
         holds, got {parsed}"
    );

    // Nothing was stored for the refused create.
    let (read_status, described) = emulator
        .call_in(region, "DescribeUserProfile", &past_the_bound)
        .await;
    assert_eq!(
        (read_status, error_code(&described).as_str()),
        (400, "ResourceNotFound"),
        "case past-the-bound: the refused create stored nothing, got {read_status} with {described}"
    );

    // The bound counts the profiles the account HOLDS, not the creates it has made: the
    // removal of one frees its slot, and the same create is then served. The removal is
    // the clock's, so this waits for the CONDITION rather than for a span.
    let (status, parsed) = emulator
        .call_in(region, "DeleteUserProfile", &last_slot)
        .await;
    assert_eq!(
        status, 200,
        "case freed-slot: expected HTTP 200 from DeleteUserProfile, got {status} with {parsed}"
    );
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, parsed) = emulator
            .call_in(region, "CreateUserProfile", &past_the_bound)
            .await;
        if status == 200 {
            assert_eq!(
                str_field(&parsed, "UserProfileArn"),
                user_profile_arn(region, domain_id, "past-the-bound"),
                "case freed-slot: the create served from the freed slot mints the profile's \
                 ARN, got {parsed}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case freed-slot: the slot a completed removal freed must take a create, last \
             answer {status} with {parsed}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
