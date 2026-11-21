// SPDX-License-Identifier: CC0-1.0

//! End-to-end wire tests for the ORDER a Studio domain's teardown has to follow: "All of
//! the members of the domain must be deleted first", so `DeleteDomain` is refused while the
//! domain still holds a user profile, `DeleteUserProfile` is what releases that member, and
//! the domain's own teardown is served once nothing it holds is left.
//!
//! Every case goes over the wire (`POST /` with `X-Amz-Target: SageMaker.<Op>`,
//! `awsJson1_1`) through the same `MultiServiceRouter` the production router uses, so what
//! these tests exercise is what a real SDK client gets. The harness (`Emulator`,
//! `error_code`, `message`, `str_field`) is the one `inference_deployment_lifecycle`
//! defines for this target.
//!
//! The tests assert RULES over a TABLE of inputs rather than one recorded call: the
//! teardown order is driven for a domain holding one member and several, with and without
//! an app under the profile, across Regions, and each step is followed to the state the
//! clock settles it in.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::inference_deployment_lifecycle::{error_code, message, str_field, Emulator, ACCOUNT};

/// An execution role inside `RoleArn`'s modelled pattern, for the `DefaultUserSettings`
/// every `CreateDomain` must carry.
const EXECUTION_ROLE: &str = "arn:aws:iam::123456789012:role/service-role/StudioRole";

/// How long a poll below gives the clock to settle a resource: far longer than the spans
/// the state gives a domain's provisioning, a profile's removal and a domain's teardown, so
/// a loaded host still settles well inside it. Only the loop's condition decides when it
/// stops.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How often a poll looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The ARN `DomainArn`'s modelled pattern gives the domain `domain_id` in `region`.
fn domain_arn(region: &str, domain_id: &str) -> String {
    format!("arn:aws:sagemaker:{region}:{ACCOUNT}:domain/{domain_id}")
}

/// Create a domain in `region` and answer the `DomainId` it minted, once the clock has
/// brought it up: a domain admits members only in `InService`.
async fn in_service_domain(emulator: &Emulator, region: &str, name: &str) -> String {
    let (status, created) = emulator
        .call_in(
            region,
            "CreateDomain",
            &json!({
                "DomainName": name,
                "AuthMode": "IAM",
                "DefaultUserSettings": { "ExecutionRole": EXECUTION_ROLE }
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {name}: expected HTTP 200 from CreateDomain, got {status} with {created}"
    );
    let domain_id = str_field(&created, "DomainId");

    // Wait for the CONDITION: the provisioning is the clock's, and no call has to make it
    // happen.
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, described) = emulator
            .call_in(region, "DescribeDomain", &json!({ "DomainId": domain_id }))
            .await;
        if status == 200 && str_field(&described, "Status") == "InService" {
            return domain_id;
        }
        assert!(
            Instant::now() < deadline,
            "case {name}: the domain must reach InService on the clock, last read {status} \
             {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Poll `target` with `body` in `region` until it is refused, answering the refusal — what
/// a client watching a teardown settle does.
async fn refused_eventually(
    emulator: &Emulator,
    region: &str,
    case: &str,
    target: &str,
    body: &Value,
) -> Value {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, parsed) = emulator.call_in(region, target, body).await;
        if status != 200 {
            return parsed;
        }
        assert!(
            Instant::now() < deadline,
            "case {case}: {target} must be refused once the clock completes the removal, \
             last read {parsed}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: a Studio domain's members have to be deleted before the domain itself, so
/// `DeleteDomain` is refused with the modelled `ResourceInUse` — leaving the domain and the
/// profile exactly as they were — while the domain still holds a user profile no
/// `DeleteUserProfile` has released; once each member's own removal has been asked for, the
/// identical `DeleteDomain` is served and the clock takes the domain and everything in it.
#[tokio::test]
async fn deleting_a_studio_domain_follows_the_order_its_members_impose() {
    let emulator = Emulator::new();

    // Rows differ along what the rule generalises over: the Region signed for, how many
    // profiles the domain holds, and whether one of them is running an app (which the
    // profile's own teardown has to be ordered after as well).
    let rows: Vec<(&str, &str, Vec<&str>, bool)> = vec![
        ("one-profile", "us-east-1", vec!["studio-user"], false),
        (
            "several-profiles",
            "eu-west-1",
            vec!["studio-user", "analyst", "data-scientist"],
            false,
        ),
        (
            "profile-running-an-app",
            "ap-northeast-1",
            vec!["studio-user"],
            true,
        ),
    ];

    for (case, region, profiles, with_app) in rows {
        let domain_id = in_service_domain(&emulator, region, &format!("teardown-{case}")).await;
        let addressed = json!({ "DomainId": domain_id });
        for profile in &profiles {
            let (status, created) = emulator
                .call_in(
                    region,
                    "CreateUserProfile",
                    &json!({ "DomainId": domain_id, "UserProfileName": profile }),
                )
                .await;
            assert_eq!(
                status, 200,
                "case {case}: expected HTTP 200 from CreateUserProfile {profile}, got \
                 {status} with {created}"
            );
        }
        let app = json!({
            "DomainId": domain_id,
            "UserProfileName": profiles[0],
            "AppType": "JupyterServer",
            "AppName": "default",
        });
        if with_app {
            let (status, created) = emulator.call_in(region, "CreateApp", &app).await;
            assert_eq!(
                status, 200,
                "case {case}: expected HTTP 200 from CreateApp, got {status} with {created}"
            );
        }

        // While the domain holds a member, its teardown is refused and nothing is torn
        // down. With an app running, the app is the member that stands in the way first.
        let (status, parsed) = emulator.call_in(region, "DeleteDomain", &addressed).await;
        assert_eq!(
            status, 400,
            "case {case}: a domain that still holds a member is not torn down, got {status} \
             with {parsed}"
        );
        assert_eq!(
            error_code(&parsed),
            "ResourceInUse",
            "case {case}: error code, got {parsed}"
        );
        let expected = if with_app {
            format!("Domain [{}] has non-deleted Apps.", domain_arn(region, &domain_id))
        } else {
            format!(
                "Unable to delete Domain [{}] because UserProfile(s) are associated with it.",
                domain_arn(region, &domain_id)
            )
        };
        assert_eq!(
            message(&parsed),
            expected,
            "case {case}: the refusal must name the domain and the member in the way, got \
             {parsed}"
        );
        // Nothing moved: the domain is still the one the clock brought up, and so is every
        // profile in it.
        let (status, described) = emulator.call_in(region, "DescribeDomain", &addressed).await;
        assert_eq!(
            status, 200,
            "case {case}: expected HTTP 200 from DescribeDomain, got {status} with {described}"
        );
        assert_eq!(
            str_field(&described, "Status"),
            "InService",
            "case {case}: a refused teardown starts no teardown, got {described}"
        );
        for profile in &profiles {
            let (status, read) = emulator
                .call_in(
                    region,
                    "DescribeUserProfile",
                    &json!({ "DomainId": domain_id, "UserProfileName": profile }),
                )
                .await;
            assert_eq!(
                status, 200,
                "case {case}: expected HTTP 200 from DescribeUserProfile {profile}, got \
                 {status} with {read}"
            );
            assert!(
                ["Pending", "InService"].contains(&str_field(&read, "Status").as_str()),
                "case {case}: the profile {profile} is untouched by the refusal, got {read}"
            );
        }

        // The members go first, in the order they hang off one another: the app, then the
        // profile that owns it.
        if with_app {
            let (status, parsed) = emulator.call_in(region, "DeleteApp", &app).await;
            assert_eq!(
                status, 200,
                "case {case}: expected HTTP 200 from DeleteApp, got {status} with {parsed}"
            );
        }
        for profile in &profiles {
            let (status, parsed) = emulator
                .call_in(
                    region,
                    "DeleteUserProfile",
                    &json!({ "DomainId": domain_id, "UserProfileName": profile }),
                )
                .await;
            assert_eq!(
                status, 200,
                "case {case}: expected HTTP 200 from DeleteUserProfile {profile}, got \
                 {status} with {parsed}"
            );
            // The profile reports its own removal until the clock completes it, past which
            // no reader resolves it.
            let refusal = refused_eventually(
                &emulator,
                region,
                case,
                "DescribeUserProfile",
                &json!({ "DomainId": domain_id, "UserProfileName": profile }),
            )
            .await;
            assert_eq!(
                error_code(&refusal),
                "ResourceNotFound",
                "case {case}: a released profile reads as not found, got {refusal}"
            );
        }

        // With nothing left in it, the identical teardown is served and the clock takes
        // the domain.
        let (status, parsed) = emulator.call_in(region, "DeleteDomain", &addressed).await;
        assert_eq!(
            status, 200,
            "case {case}: the teardown is served once the domain holds nothing, got {status} \
             with {parsed}"
        );
        let refusal =
            refused_eventually(&emulator, region, case, "DescribeDomain", &addressed).await;
        assert_eq!(
            error_code(&refusal),
            "ResourceNotFound",
            "case {case}: a released domain reads as not found, got {refusal}"
        );
        // And so does everything that hung off it.
        if with_app {
            let refusal = refused_eventually(&emulator, region, case, "DescribeApp", &app).await;
            assert_eq!(
                error_code(&refusal),
                "ResourceNotFound",
                "case {case}: an app of a released domain reads as not found, got {refusal}"
            );
        }
    }
}

/// RULE: `DeleteUserProfile` tears down the profile its `DomainId` and `UserProfileName`
/// name — parking it in the non-terminal `Deleting` until the clock completes the removal —
/// and refuses a pair this account holds no profile under with the modelled
/// `ResourceNotFound`, naming it by the ARN that pair resolves to.
#[tokio::test]
async fn deleting_a_user_profile_reports_the_removal_and_refuses_what_it_cannot_find() {
    let emulator = Emulator::new();

    let region = "us-east-1";
    let domain_id = in_service_domain(&emulator, region, "profile-teardown").await;
    emulator
        .ok(
            "CreateUserProfile",
            &json!({ "DomainId": domain_id, "UserProfileName": "studio-user" }),
        )
        .await;

    let addressed = json!({ "DomainId": domain_id, "UserProfileName": "studio-user" });
    let (status, parsed) = emulator.call("DeleteUserProfile", &addressed).await;
    assert_eq!(
        status, 200,
        "case served-delete: expected HTTP 200 from DeleteUserProfile, got {status} with \
         {parsed}"
    );
    assert!(
        parsed.as_object().is_none_or(serde_json::Map::is_empty),
        "case served-delete: DeleteUserProfile answers no members, got {parsed}"
    );

    // Either the transient status or the not-found refusal is correct here, and WHICH is
    // decided by the clock: a reader that runs inside the removal's span sees `Deleting`.
    let (status, described) = emulator.call("DescribeUserProfile", &addressed).await;
    if status == 200 {
        assert_eq!(
            str_field(&described, "Status"),
            "Deleting",
            "case transient-status: a profile being removed reports Deleting, got {described}"
        );
    } else {
        assert_eq!(
            error_code(&described),
            "ResourceNotFound",
            "case transient-status: the only other answer is the not-found refusal, got \
             {described}"
        );
    }

    // Rows: every way the pair fails to resolve — a profile the clock has released, a name
    // the domain never held, and a domain this account holds nothing under.
    refused_eventually(
        &emulator,
        region,
        "released",
        "DescribeUserProfile",
        &addressed,
    )
    .await;
    let cases: Vec<(&str, Value)> = vec![
        ("released-profile", addressed.clone()),
        (
            "name-the-domain-never-held",
            json!({ "DomainId": domain_id, "UserProfileName": "nobody" }),
        ),
        (
            "domain-that-does-not-exist",
            json!({ "DomainId": "d-000000000000", "UserProfileName": "studio-user" }),
        ),
    ];
    for (case, request) in cases {
        let (status, parsed) = emulator.call("DeleteUserProfile", &request).await;
        assert_eq!(
            status, 400,
            "case {case}: expected HTTP 400 from DeleteUserProfile, got {status} with {parsed}"
        );
        assert_eq!(
            error_code(&parsed),
            "ResourceNotFound",
            "case {case}: error code, got {parsed}"
        );
        let domain = str_field(&request, "DomainId");
        let name = str_field(&request, "UserProfileName");
        assert_eq!(
            message(&parsed),
            format!(
                "UserProfile [arn:aws:sagemaker:{region}:{ACCOUNT}:user-profile/{domain}/{name}] \
                 does not exist."
            ),
            "case {case}: the refusal must name the profile the pair resolves to, got {parsed}"
        );
    }
}
