// SPDX-License-Identifier: CC0-1.0

//! End-to-end wire tests for SageMaker Studio's USER PROFILE — `CreateUserProfile` and
//! `DescribeUserProfile` — and for the referential rule that profile backs: a Studio app
//! can only be created for a user profile its domain actually holds, so a domain whose app
//! create was refused holds nothing and its `DeleteDomain` is served.
//!
//! Every case goes over the wire (`POST /` with `X-Amz-Target: SageMaker.<Op>`,
//! `awsJson1_1`) through the same `MultiServiceRouter` the production router uses, so what
//! these tests exercise is what a real SDK client gets. The harness (`Emulator`,
//! `error_code`, `message`, `str_field`) is the one `inference_deployment_lifecycle`
//! already defines for this target.
//!
//! The tests assert RULES over a TABLE of inputs rather than one recorded call: the
//! create/read round trip for every optional member of the profile across Regions and name
//! shapes at the modelled extremes; the owner refusal for every way a named profile fails
//! to resolve in a domain; and the teardown for a domain that was left holding no app,
//! with and without a retention policy, beside the one that still holds a real app.

use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Map, Value};
use service_com_amazonaws_sagemaker_sagemaker::ops::types::{UserSettings, USER_PROFILE_STATUS_VALUES};
use service_com_amazonaws_sagemaker_sagemaker::state::{
    self, ComAmazonawsSagemakerSagemakerState, Domain, UserProfile, DOMAIN_STATUS_DELETING,
    DOMAIN_STATUS_IN_SERVICE, USER_PROFILE_STATUS_IN_SERVICE, USER_PROFILE_STATUS_PENDING,
};

use crate::inference_deployment_lifecycle::{error_code, message, str_field, Emulator, ACCOUNT};

/// An execution role inside `RoleArn`'s modelled pattern, for the `DefaultUserSettings`
/// every `CreateDomain` must carry.
pub(crate) const EXECUTION_ROLE: &str = "arn:aws:iam::123456789012:role/service-role/StudioRole";

/// How long a poll below gives the clock to settle a resource: far longer than the spans
/// the state gives a profile's provisioning and a domain's teardown, so a loaded host
/// still settles well inside it.
pub(crate) const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How often a poll looks again. Only the loop's condition decides when it stops.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The smallest `CreateDomain` request for a domain whose members authenticate with
/// `auth_mode` — `AuthMode`'s members are `SSO` (IAM Identity Center) and `IAM`
/// (`smithy_query(query="shape", shape="AuthMode")`).
pub(crate) fn minimal_domain_with_auth(domain_name: &str, auth_mode: &str) -> Value {
    json!({
        "DomainName": domain_name,
        "AuthMode": auth_mode,
        "DefaultUserSettings": { "ExecutionRole": EXECUTION_ROLE }
    })
}

/// `base` with every member of `extra` added.
fn with_members(base: &Value, extra: &Value) -> Value {
    let mut merged = base
        .as_object()
        .expect("the base members form an object")
        .clone();
    for (member, value) in extra.as_object().expect("the extra members form an object") {
        merged.insert(member.clone(), value.clone());
    }
    Value::Object(merged)
}

/// The ARN `UserProfileArn`'s modelled pattern gives the profile `user_profile_name` of
/// `domain_id` in `region`.
pub(crate) fn user_profile_arn(region: &str, domain_id: &str, user_profile_name: &str) -> String {
    format!("arn:aws:sagemaker:{region}:{ACCOUNT}:user-profile/{domain_id}/{user_profile_name}")
}

/// The ARN `DomainArn`'s modelled pattern gives the domain `domain_id` in `region`.
pub(crate) fn domain_arn(region: &str, domain_id: &str) -> String {
    format!("arn:aws:sagemaker:{region}:{ACCOUNT}:domain/{domain_id}")
}

/// A fixed instant for seeded records, so nothing a seeded resource reports depends on
/// when the test ran.
fn seeded_at() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_767_225_600)
}

/// Write a Studio domain under `domain_id` into the shared store the handlers read,
/// already settled, so a call that resolves that `DomainId` finds a live domain.
///
/// Shared with this crate's other Studio suites (`crate::studio_user_profiles::…`): a
/// suite that drives apps under `DomainId`s of its own choosing cannot mint those ids
/// through `CreateDomain` (the id is the server's), so it seeds the domain the app lives in
/// here — the same store `state::service_state` hands the handler.
pub(crate) fn seed_studio_domain(emulator: &Emulator, region: &str, domain_id: &str) {
    let store = emulator
        .engine
        .state::<ComAmazonawsSagemakerSagemakerState>();
    let mut store = store.lock();
    let when = seeded_at();
    store
        .domains
        .entry(domain_id.to_string())
        .or_insert_with(|| Domain {
            id: domain_id.to_string(),
            arn: state::resource_arn(region, state::DOMAIN_ARN_KIND, domain_id),
            name: format!("seeded-{domain_id}"),
            url: state::studio_domain_url(region, domain_id),
            auth_mode: "IAM".to_string(),
            default_user_settings: UserSettings {
                execution_role: Some(EXECUTION_ROLE.to_string()),
                ..UserSettings::default()
            },
            domain_settings: None,
            default_space_settings: None,
            subnet_ids: None,
            vpc_id: None,
            kms_key_id: None,
            home_efs_file_system_kms_key_id: None,
            app_network_access_type: state::DEFAULT_APP_NETWORK_ACCESS_TYPE.to_string(),
            app_security_group_management: None,
            home_efs_file_system_creation: state::DEFAULT_HOME_EFS_FILE_SYSTEM_CREATION
                .to_string(),
            tag_propagation: state::DEFAULT_TAG_PROPAGATION.to_string(),
            // Seeded already `InService`, so the clock owes this domain no transition.
            status: DOMAIN_STATUS_IN_SERVICE.to_string(),
            failure_reason: None,
            creation_failure_reason: None,
            tags: Vec::new(),
            creation_time: when,
            last_modified_time: when,
            in_service_due_at: when,
            removal_due_at: None,
        });
}

/// Write a Studio domain under `domain_id` and a user profile under `user_profile_name`
/// into the shared store the handlers read, both already settled, so an app create in that
/// domain resolves both the domain and its owner. See [`seed_studio_domain`] for why a
/// suite seeds rather than creates these.
pub(crate) fn seed_studio_owner(
    emulator: &Emulator,
    region: &str,
    domain_id: &str,
    user_profile_name: &str,
) {
    seed_studio_domain(emulator, region, domain_id);
    let store = emulator
        .engine
        .state::<ComAmazonawsSagemakerSagemakerState>();
    let mut store = store.lock();
    let when = seeded_at();
    store.user_profiles.insert(
        state::user_profile_key(domain_id, user_profile_name),
        UserProfile {
            arn: state::resource_arn(
                region,
                state::USER_PROFILE_ARN_KIND,
                &format!("{domain_id}/{user_profile_name}"),
            ),
            domain_id: domain_id.to_string(),
            name: user_profile_name.to_string(),
            single_sign_on_user_identifier: None,
            single_sign_on_user_value: None,
            user_settings: None,
            tags: Vec::new(),
            // Seeded already `InService`, as the domain above is.
            status: USER_PROFILE_STATUS_IN_SERVICE.to_string(),
            creation_time: when,
            last_modified_time: when,
            in_service_due_at: when,
            // Nothing has asked for this profile's removal, so the clock owes it none.
            removal_due_at: None,
        },
    );
}

/// Create a domain in `region` and answer the `DomainId` it minted.
async fn created_domain(emulator: &Emulator, region: &str, name: &str) -> String {
    created_domain_with_auth(emulator, region, name, "IAM").await
}

/// Create a domain in `region` whose members authenticate with `auth_mode`, and answer the
/// `DomainId` it minted.
///
/// The create leaves the domain `Pending`, and a domain only admits members once its own
/// provisioning has settled, so this waits for the CONDITION — the `InService` the clock
/// owes it — before answering, the way a real integration's retry loop does.
async fn created_domain_with_auth(
    emulator: &Emulator,
    region: &str,
    name: &str,
    auth_mode: &str,
) -> String {
    let (status, created) = emulator
        .call_in(
            region,
            "CreateDomain",
            &minimal_domain_with_auth(name, auth_mode),
        )
        .await;
    assert_eq!(
        status, 200,
        "case create-domain-{name}: expected HTTP 200 from CreateDomain, got {status} with \
         {created}"
    );
    let domain_id = str_field(&created, "DomainId");
    wait_for_domain_in_service(emulator, region, &domain_id).await;
    domain_id
}

/// Poll `DescribeDomain` in `region` until `domain_id` reports `InService`.
///
/// Waits for the CONDITION, never for a fixed time: the domain's provisioning is the
/// clock's, and no call has to make it happen.
pub(crate) async fn wait_for_domain_in_service(emulator: &Emulator, region: &str, domain_id: &str) {
    wait_for_domain_status(emulator, region, domain_id, DOMAIN_STATUS_IN_SERVICE).await;
}

/// Poll `DescribeDomain` in `region` until `domain_id` reports `expected`.
///
/// The one poll loop every Studio suite waits for a domain's status with: a domain's
/// provisioning and its teardown are both the clock's, so what a caller waits for is the
/// CONDITION rather than a span.
pub(crate) async fn wait_for_domain_status(
    emulator: &Emulator,
    region: &str,
    domain_id: &str,
    expected: &str,
) {
    let describe = json!({ "DomainId": domain_id });
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, described) = emulator.call_in(region, "DescribeDomain", &describe).await;
        if status == 200 && str_field(&described, "Status") == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case status-{domain_id}: the domain must reach {expected} on the clock, last read \
             {status} {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One row of the profile round-trip table: the Region it is signed for, the `AuthMode` of
/// the domain the profile lives in, the name it carries and the optional members beyond the
/// two required ones.
struct ProfileRow {
    case: &'static str,
    region: &'static str,
    auth_mode: &'static str,
    name: String,
    extra: Value,
}

/// RULE: `CreateUserProfile` stores the profile its `DomainId` and `UserProfileName` pair
/// names, mints `arn:aws:sagemaker:<region>:<account>:user-profile/<DomainId>/<name>` for
/// it and launches it `Pending`; `DescribeUserProfile` naming the same pair replays what
/// the create stored, and the clock moves the profile to `InService` on its own.
#[tokio::test]
async fn describe_user_profile_replays_exactly_what_create_user_profile_stored() {
    let emulator = Emulator::new();

    // `UserProfileName` is `len 0..63, pattern ^[a-zA-Z0-9](-*[a-zA-Z0-9]){0,62}$`
    // (smithy_query operation=CreateUserProfile), so this is the longest accepted value.
    let longest_name = "a".repeat(63);
    // Rows differ along what the rule generalises over: the Region signed for, the
    // `AuthMode` the profile's domain authenticates with (which decides whether the
    // single-sign-on members belong on the request), the name shape (one character, mixed
    // case, the 63-character maximum) and which optional members the create carried.
    let rows = vec![
        ProfileRow {
            case: "required-members-only",
            region: "us-east-1",
            auth_mode: "IAM",
            name: "studio-user".to_string(),
            extra: json!({}),
        },
        ProfileRow {
            case: "single-sign-on-members",
            region: "eu-west-1",
            // The members are "required" for a domain whose `AuthMode` is IAM Identity
            // Center, which is `AuthMode`'s `SSO` member, and cannot be specified for any
            // other — so this row's domain is that one.
            auth_mode: "SSO",
            name: "SsoUser".to_string(),
            extra: json!({
                "SingleSignOnUserIdentifier": "UserName",
                "SingleSignOnUserValue": "directory-user"
            }),
        },
        ProfileRow {
            case: "user-settings-and-tags",
            region: "ap-northeast-1",
            auth_mode: "IAM",
            name: "u".to_string(),
            extra: json!({
                "UserSettings": {
                    "ExecutionRole": EXECUTION_ROLE,
                    "StudioWebPortal": "ENABLED"
                },
                "Tags": [{ "Key": "team", "Value": "research" }]
            }),
        },
        ProfileRow {
            case: "longest-name",
            region: "us-west-2",
            auth_mode: "IAM",
            name: longest_name,
            extra: json!({}),
        },
    ];

    let mut created: Vec<(&'static str, &'static str, String, String)> = Vec::new();
    for row in &rows {
        let case = row.case;
        let domain_id = created_domain_with_auth(
            &emulator,
            row.region,
            &format!("domain-{case}"),
            row.auth_mode,
        )
        .await;
        let request = with_members(
            &json!({ "DomainId": domain_id, "UserProfileName": row.name }),
            &row.extra,
        );
        let (status, answer) = emulator
            .call_in(row.region, "CreateUserProfile", &request)
            .await;
        assert_eq!(
            status, 200,
            "case {case}: expected HTTP 200 from CreateUserProfile, got {status} with {answer}"
        );
        let expected_arn = user_profile_arn(row.region, &domain_id, &row.name);
        assert_eq!(
            str_field(&answer, "UserProfileArn"),
            expected_arn,
            "case {case}: CreateUserProfile must mint the profile's ARN, got {answer}"
        );

        let read = json!({ "DomainId": domain_id, "UserProfileName": row.name });
        let (status, described) = emulator
            .call_in(row.region, "DescribeUserProfile", &read)
            .await;
        assert_eq!(
            status, 200,
            "case {case}: expected HTTP 200 from DescribeUserProfile, got {status} with \
             {described}"
        );
        assert_eq!(
            str_field(&described, "UserProfileArn"),
            expected_arn,
            "case {case}: the reader replays the stored ARN, got {described}"
        );
        assert_eq!(
            str_field(&described, "DomainId"),
            domain_id,
            "case {case}: the reader names the domain the profile lives in, got {described}"
        );
        assert_eq!(
            str_field(&described, "UserProfileName"),
            row.name,
            "case {case}: the reader replays the name as the create spelled it, got {described}"
        );
        let status_read = str_field(&described, "Status");
        assert!(
            status_read == USER_PROFILE_STATUS_PENDING
                || status_read == USER_PROFILE_STATUS_IN_SERVICE,
            "case {case}: a just-created profile is launching or up, got {described}"
        );
        assert!(
            USER_PROFILE_STATUS_VALUES.contains(&status_read.as_str()),
            "case {case}: the status must be a member of the modelled catalog, got \
             {described}"
        );
        for member in ["CreationTime", "LastModifiedTime"] {
            assert!(
                described.get(member).is_some_and(Value::is_number),
                "case {case}: {member} must be a stored timestamp, got {described}"
            );
        }
        // Nothing created an EFS volume for the domain and nothing failed the profile's
        // provisioning, so neither member is reported.
        for member in ["HomeEfsFileSystemUid", "FailureReason"] {
            assert!(
                described.get(member).is_none(),
                "case {case}: {member} must be omitted when nothing produced one, got \
                 {described}"
            );
        }
        // Every optional member the create carried is replayed as stored, and one it did
        // not carry is reported by nobody.
        for (member, value) in row
            .extra
            .as_object()
            .expect("the extra members form an object")
        {
            if member == "Tags" {
                // `DescribeUserProfile` models no tag member, so the stored tags are not
                // part of its answer.
                continue;
            }
            assert_eq!(
                described.get(member),
                Some(value),
                "case {case}: {member} must be replayed as stored, got {described}"
            );
        }
        for member in [
            "SingleSignOnUserIdentifier",
            "SingleSignOnUserValue",
            "UserSettings",
        ] {
            if row.extra.get(member).is_none() {
                assert!(
                    described.get(member).is_none(),
                    "case {case}: {member} must be omitted when the create named none, got \
                     {described}"
                );
            }
        }

        created.push((case, row.region, domain_id, row.name.clone()));
    }

    // The launch the clock owes each profile becomes visible without any call having to
    // make it happen, and stably thereafter.
    for (case, region, domain_id, name) in &created {
        let read = json!({ "DomainId": domain_id, "UserProfileName": name });
        // Wait for the CONDITION, not for a fixed time.
        let deadline = Instant::now() + SETTLE_DEADLINE;
        loop {
            let (status, described) = emulator
                .call_in(region, "DescribeUserProfile", &read)
                .await;
            assert_eq!(
                status, 200,
                "case {case}: the profile stays readable while it settles, got {status} with \
                 {described}"
            );
            if str_field(&described, "Status") == USER_PROFILE_STATUS_IN_SERVICE {
                let again = emulator
                    .call_in(region, "DescribeUserProfile", &read)
                    .await;
                assert_eq!(
                    again.1, described,
                    "case {case}: a settled profile must read back identically"
                );
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case}: the profile must reach {USER_PROFILE_STATUS_IN_SERVICE} on the \
                 clock, last read {described}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

/// RULE: the single-sign-on members of a user profile are the user's identity in the
/// directory the domain authenticates against, so the domain's `AuthMode` decides them:
/// both are required for a domain that authenticates with IAM Identity Center and neither
/// can be specified for a domain that does not, and a create outside that rule is refused
/// with HTTP 400 `ValidationException` having stored nothing.
#[tokio::test]
async fn create_user_profile_weighs_the_single_sign_on_members_against_the_domains_auth_mode() {
    let emulator = Emulator::new();

    // The two members the rule governs, in the order the refusals name them
    // (smithy_query operation=CreateUserProfile).
    let identifier = json!({ "SingleSignOnUserIdentifier": "UserName" });
    let value = json!({ "SingleSignOnUserValue": "directory-user" });
    let both = with_members(&identifier, &value);

    // Rows span every combination of the two members against both `AuthMode` members: a
    // domain that authenticates with IAM Identity Center (`SSO`) needs both, and one that
    // does not accepts neither — so each row states which members it carries, the
    // `AuthMode` of its domain, and the refusal it must draw (or `None` for acceptance).
    let rows: Vec<(&str, &str, Value, Option<Vec<&str>>)> = vec![
        ("idc-with-both-members", "SSO", both.clone(), None),
        (
            "idc-with-neither-member",
            "SSO",
            json!({}),
            Some(Vec::new()),
        ),
        (
            "idc-with-identifier-only",
            "SSO",
            identifier.clone(),
            Some(Vec::new()),
        ),
        (
            "idc-with-value-only",
            "SSO",
            value.clone(),
            Some(Vec::new()),
        ),
        ("iam-with-neither-member", "IAM", json!({}), None),
        (
            "iam-with-both-members",
            "IAM",
            both,
            Some(vec!["SingleSignOnUserIdentifier", "SingleSignOnUserValue"]),
        ),
        (
            "iam-with-identifier-only",
            "IAM",
            identifier,
            Some(vec!["SingleSignOnUserIdentifier"]),
        ),
        (
            "iam-with-value-only",
            "IAM",
            value,
            Some(vec!["SingleSignOnUserValue"]),
        ),
    ];

    for (case, auth_mode, members, refused) in &rows {
        let domain_id =
            created_domain_with_auth(&emulator, "us-east-1", &format!("sso-{case}"), auth_mode)
                .await;
        let request = with_members(
            &json!({ "DomainId": domain_id, "UserProfileName": "studio-user" }),
            members,
        );
        let (status, parsed) = emulator.call("CreateUserProfile", &request).await;

        let Some(carried) = refused else {
            assert_eq!(
                status, 200,
                "case {case}: the create is served when the members match the domain's \
                 AuthMode, got {status} with {parsed}"
            );
            continue;
        };
        assert_eq!(
            status, 400,
            "case {case}: a create outside the AuthMode rule is refused, got {status} with \
             {parsed}"
        );
        assert_eq!(
            error_code(&parsed),
            "ValidationException",
            "case {case}: expected ValidationException, got {parsed}"
        );
        // An IAM Identity Center domain states what the request must add; any other domain
        // states exactly which of the members it may not carry.
        let expected = if carried.is_empty() {
            "SingleSignOnUserIdentifier and SingleSignOnUserValue are required when the \
             Domain's AuthMode is IAM Identity Center."
                .to_string()
        } else {
            format!(
                "{} cannot be specified when the Domain's AuthMode is not IAM Identity \
                 Center.",
                carried.join(" and ")
            )
        };
        assert_eq!(
            message(&parsed),
            expected,
            "case {case}: the refusal states the rule and names the members, got {parsed}"
        );

        // Nothing was stored for the refused create.
        let (status, parsed) = emulator
            .call(
                "DescribeUserProfile",
                &json!({ "DomainId": domain_id, "UserProfileName": "studio-user" }),
            )
            .await;
        assert_eq!(
            (status, error_code(&parsed).as_str()),
            (400, "ResourceNotFound"),
            "case {case}: the refused create stored nothing, got {status} with {parsed}"
        );
    }
}

/// RULE: a user profile belongs to one domain and holds its name there alone, so a
/// `CreateUserProfile` naming a `DomainId` this account holds no domain under is refused,
/// a second create of a name the domain already holds — in any spelling, the name is not
/// case sensitive — is refused as in use, and the same name in another domain is a
/// different profile.
#[tokio::test]
async fn create_user_profile_refuses_what_it_cannot_create() {
    let emulator = Emulator::new();

    // Rows: well-formed ids nothing is stored under, in the Regions whose ARNs the refusal
    // has to name. `DomainId` matches `^d-(-*[a-z0-9]){1,61}$`, so each reaches the
    // handler.
    for (case, region, domain_id) in [
        ("absent-id", "us-east-1", "d-000000000000"),
        ("another-absent-id", "us-east-1", "d-abc123def456"),
        ("absent-id-in-another-region", "eu-west-1", "d-0a1b2c3d4e5f"),
        ("shortest-absent-id", "us-west-2", "d-a"),
    ] {
        let (status, parsed) = emulator
            .call_in(
                region,
                "CreateUserProfile",
                &json!({ "DomainId": domain_id, "UserProfileName": "studio-user" }),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case}: a profile cannot be created in a domain that does not exist, got \
             {status} with {parsed}"
        );
        assert_eq!(
            error_code(&parsed),
            "ValidationException",
            "case {case}: expected ValidationException, got {parsed}"
        );
        assert_eq!(
            message(&parsed),
            format!("Domain [{domain_id}] does not exist"),
            "case {case}: the refusal names the domain id the request supplied, got {parsed}"
        );
        // Nothing was stored for the refused create.
        let (status, parsed) = emulator
            .call_in(
                region,
                "DescribeUserProfile",
                &json!({ "DomainId": domain_id, "UserProfileName": "studio-user" }),
            )
            .await;
        assert_eq!(
            (status, error_code(&parsed).as_str()),
            (400, "ResourceNotFound"),
            "case {case}: the refused create stored nothing, got {status} with {parsed}"
        );
        assert_eq!(
            message(&parsed),
            format!(
                "UserProfile [{}] does not exist.",
                user_profile_arn(region, domain_id, "studio-user")
            ),
            "case {case}: the reader's refusal names the profile it could not resolve, got \
             {parsed}"
        );
    }

    // A second create of a name the domain already holds is refused as in use, in every
    // spelling of that name — `UserProfileName` "is not case sensitive".
    let region = "us-east-1";
    let domain_id = created_domain(&emulator, region, "duplicate-profiles").await;
    let other_domain_id = created_domain(&emulator, region, "other-domain").await;
    for (case, name, duplicate) in [
        ("same-spelling", "studio-user", "studio-user"),
        ("upper-case-spelling", "AnalystUser", "ANALYSTUSER"),
        ("mixed-case-spelling", "data-Scientist", "DATA-scientist"),
    ] {
        let (status, created) = emulator
            .call_in(
                region,
                "CreateUserProfile",
                &json!({ "DomainId": domain_id, "UserProfileName": name }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case}: the first create is served, got {status} with {created}"
        );

        let (status, parsed) = emulator
            .call_in(
                region,
                "CreateUserProfile",
                &json!({ "DomainId": domain_id, "UserProfileName": duplicate }),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case}: a name the domain already holds cannot be created again, got \
             {status} with {parsed}"
        );
        assert_eq!(
            error_code(&parsed),
            "ResourceInUse",
            "case {case}: expected ResourceInUse, got {parsed}"
        );
        assert_eq!(
            message(&parsed),
            format!(
                "A UserProfile with name '{duplicate}' already exists in Domain {domain_id}."
            ),
            "case {case}: the refusal names the profile already in use and its domain, got \
             {parsed}"
        );
        // The refused create did not overwrite the stored profile: it still reports the
        // spelling the first create carried.
        let described = emulator
            .ok(
                "DescribeUserProfile",
                &json!({ "DomainId": domain_id, "UserProfileName": duplicate }),
            )
            .await;
        assert_eq!(
            str_field(&described, "UserProfileName"),
            name,
            "case {case}: the stored profile keeps the first create's spelling, got \
             {described}"
        );

        // The same name in ANOTHER domain is a different profile, so that create is served.
        let (status, parsed) = emulator
            .call_in(
                region,
                "CreateUserProfile",
                &json!({ "DomainId": other_domain_id, "UserProfileName": name }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case}: the same name in another domain is a different profile, got \
             {status} with {parsed}"
        );
        assert_eq!(
            str_field(&parsed, "UserProfileArn"),
            user_profile_arn(region, &other_domain_id, name),
            "case {case}: that profile's ARN names its own domain, got {parsed}"
        );
    }
}

/// One row of the owner-refusal table: how the app's `UserProfileName` fails to resolve in
/// the domain the create names.
struct OwnerRow {
    case: &'static str,
    /// The profile to create in the domain first, or `None` for a domain holding none.
    profile: Option<&'static str>,
    /// The `UserProfileName` the app create names.
    named: String,
    app_type: &'static str,
    app_name: &'static str,
}

/// RULE: a Studio app runs for the user profile the request names, so `CreateApp` is
/// refused with HTTP 400 `ValidationException` — `UserProfile [<name>] does not exist.` —
/// whenever the domain holds no profile under that name, and nothing is stored for the
/// refused create.
#[tokio::test]
async fn create_app_refuses_a_user_profile_the_domain_does_not_hold() {
    let emulator = Emulator::new();

    // Rows differ along HOW the named profile fails to resolve: no profile at all, a
    // profile under another name, a near-miss spelling, and a name at the longest shape
    // `UserProfileName` admits (`len 0..63`) — crossed with app types from the modelled
    // catalog.
    let rows = [
        OwnerRow {
            case: "domain-holds-no-profile",
            profile: None,
            named: "studio-user".to_string(),
            app_type: "JupyterServer",
            app_name: "default",
        },
        OwnerRow {
            case: "another-profile-exists",
            profile: Some("analyst"),
            named: "studio-user".to_string(),
            app_type: "JupyterLab",
            app_name: "kernel",
        },
        OwnerRow {
            case: "near-miss-name",
            profile: Some("studio-user"),
            named: "studio-user2".to_string(),
            app_type: "CodeEditor",
            app_name: "editor",
        },
        OwnerRow {
            case: "longest-absent-name",
            profile: None,
            named: "a".repeat(63),
            app_type: "Canvas",
            app_name: "c",
        },
    ];

    for row in &rows {
        let case = row.case;
        let domain_id = created_domain(&emulator, "us-east-1", &format!("owner-{case}")).await;
        if let Some(profile) = row.profile {
            emulator
                .ok(
                    "CreateUserProfile",
                    &json!({ "DomainId": domain_id, "UserProfileName": profile }),
                )
                .await;
        }

        let app = json!({
            "DomainId": domain_id,
            "UserProfileName": row.named,
            "AppType": row.app_type,
            "AppName": row.app_name,
        });
        let (status, parsed) = emulator.call("CreateApp", &app).await;
        assert_eq!(
            status, 400,
            "case {case}: an app for a profile the domain does not hold is refused, got \
             {status} with {parsed}"
        );
        assert_eq!(
            error_code(&parsed),
            "ValidationException",
            "case {case}: expected ValidationException, got {parsed}"
        );
        assert_eq!(
            message(&parsed),
            format!("UserProfile [{}] does not exist.", row.named),
            "case {case}: the refusal names the user profile it could not resolve, got \
             {parsed}"
        );

        // Nothing was stored: no reader resolves the app, and nothing is left to delete.
        for target in ["DescribeApp", "DeleteApp"] {
            let (status, parsed) = emulator.call(target, &app).await;
            assert_eq!(
                (status, error_code(&parsed).as_str()),
                (400, "ResourceNotFound"),
                "case {case}: the refused create stored no app for {target}, got {status} \
                 with {parsed}"
            );
        }
    }

    // A `DomainId` that resolves to no domain is refused for the parent it cannot find
    // rather than for the child, under both owner members: an app lives in its domain
    // whoever owns it.
    for (case, owner_member, owner) in [
        ("absent-domain-user-profile-app", "UserProfileName", "studio-user"),
        ("absent-domain-space-app", "SpaceName", "team-space"),
    ] {
        for (region, domain_id) in [("us-east-1", "d-000000000000"), ("eu-west-1", "d-a")] {
            let mut app = json!({
                "DomainId": domain_id,
                "AppType": "JupyterServer",
                "AppName": "default",
            });
            app.as_object_mut()
                .expect("the identifying members form an object")
                .insert(owner_member.to_string(), json!(owner));
            let (status, parsed) = emulator.call_in(region, "CreateApp", &app).await;
            assert_eq!(
                status, 400,
                "case {case}-{region}: an app in a domain that does not exist is refused, \
                 got {status} with {parsed}"
            );
            assert_eq!(
                error_code(&parsed),
                "ValidationException",
                "case {case}-{region}: expected ValidationException, got {parsed}"
            );
            assert_eq!(
                message(&parsed),
                format!("Domain [{}] does not exist.", domain_arn(region, domain_id)),
                "case {case}-{region}: the refusal names the domain it could not resolve, \
                 got {parsed}"
            );
        }
    }

    // A profile the domain DOES hold takes the app — including a spelling that differs
    // only in case, since the name is not case sensitive.
    for (case, profile, named) in [
        ("exact-spelling", "studio-user", "studio-user"),
        ("other-case-spelling", "AnalystUser", "analystuser"),
    ] {
        let domain_id = created_domain(&emulator, "us-east-1", &format!("owned-{case}")).await;
        emulator
            .ok(
                "CreateUserProfile",
                &json!({ "DomainId": domain_id, "UserProfileName": profile }),
            )
            .await;
        let app = json!({
            "DomainId": domain_id,
            "UserProfileName": named,
            "AppType": "JupyterLab",
            "AppName": "default",
        });
        let (status, parsed) = emulator.call("CreateApp", &app).await;
        assert_eq!(
            status, 200,
            "case {case}: an app for a profile the domain holds is served, got {status} with \
             {parsed}"
        );
        assert_eq!(
            str_field(&parsed, "AppArn"),
            format!(
                "arn:aws:sagemaker:us-east-1:{ACCOUNT}:app/{domain_id}/{named}/JupyterLab/default"
            ),
            "case {case}: the app's ARN names the owner the request gave, got {parsed}"
        );
    }
}

/// RULE: `DeleteDomain` naming a domain that holds no non-deleted app, and that is not
/// already being torn down, is accepted with HTTP 200 and an empty body — a 400 is
/// reserved for the two conditions the model names, so an app create the service refused
/// leaves nothing behind that can block the teardown.
#[tokio::test]
async fn delete_domain_is_served_for_a_domain_left_holding_no_app() {
    let emulator = Emulator::new();

    // Rows differ along what the rule generalises over: the Region signed for, how many
    // app creates were refused in the domain and under which owner member they were
    // attempted, and whether the teardown states a retention policy. `RetentionType`'s
    // members are `Retain` and `Delete` (smithy_query shape=RetentionType).
    let rows: Vec<(&str, &str, Vec<Value>, Value)> = vec![
        ("no-app-create-at-all", "us-east-1", vec![], json!({})),
        (
            "one-refused-app-create",
            "eu-west-1",
            vec![json!({
                "UserProfileName": "studio-user",
                "AppType": "JupyterServer",
                "AppName": "default"
            })],
            json!({}),
        ),
        (
            "several-refused-app-creates-retain",
            "us-west-2",
            vec![
                json!({
                    "UserProfileName": "studio-user",
                    "AppType": "JupyterServer",
                    "AppName": "default"
                }),
                json!({
                    "UserProfileName": "other-user",
                    "AppType": "TensorBoard",
                    "AppName": "kernel"
                }),
            ],
            json!({ "RetentionPolicy": { "HomeEfsFileSystem": "Retain" } }),
        ),
        (
            "refused-app-create-delete-retention",
            "ap-northeast-1",
            vec![json!({
                "UserProfileName": "canvas-user",
                "AppType": "Canvas",
                "AppName": "canvas"
            })],
            json!({ "RetentionPolicy": { "HomeEfsFileSystem": "Delete" } }),
        ),
    ];

    for (case, region, refused_apps, delete_extra) in &rows {
        let domain_id = created_domain(&emulator, region, &format!("teardown-{case}")).await;
        let addressed = json!({ "DomainId": domain_id });
        for app in refused_apps {
            let request = with_members(&addressed, app);
            let (status, parsed) = emulator.call_in(region, "CreateApp", &request).await;
            assert_eq!(
                status, 400,
                "case {case}: the app create naming an absent profile is refused, got \
                 {status} with {parsed}"
            );
        }

        let (status, parsed) = emulator
            .call_in(region, "DeleteDomain", &with_members(&addressed, delete_extra))
            .await;
        assert_eq!(
            status, 200,
            "case {case}: the teardown of a domain holding no app is served, got {status} \
             with {parsed}"
        );
        assert!(
            parsed.as_object().is_none_or(Map::is_empty),
            "case {case}: DeleteDomain answers no members, got {parsed}"
        );

        // The teardown really started: the domain reports `Deleting` until the clock
        // completes the removal, past which no reader resolves it any more.
        let deadline = Instant::now() + SETTLE_DEADLINE;
        loop {
            let (status, described) = emulator
                .call_in(region, "DescribeDomain", &addressed)
                .await;
            if status == 400 {
                assert_eq!(
                    error_code(&described),
                    "ResourceNotFound",
                    "case {case}: a torn-down domain is refused as not found, got {described}"
                );
                break;
            }
            assert_eq!(
                str_field(&described, "Status"),
                DOMAIN_STATUS_DELETING,
                "case {case}: a domain being torn down reports {DOMAIN_STATUS_DELETING}, got \
                 {described}"
            );
            assert!(
                Instant::now() < deadline,
                "case {case}: the domain must be gone once the clock completes the teardown, \
                 last read {described}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    // And the refusal the model DOES reserve still holds: a domain whose profile took a
    // real app is in use until that app is deleted.
    let region = "us-east-1";
    let domain_id = created_domain(&emulator, region, "holds-a-real-app").await;
    let addressed = json!({ "DomainId": domain_id });
    emulator
        .ok(
            "CreateUserProfile",
            &json!({ "DomainId": domain_id, "UserProfileName": "studio-user" }),
        )
        .await;
    let app = with_members(
        &addressed,
        &json!({
            "UserProfileName": "studio-user",
            "AppType": "JupyterServer",
            "AppName": "default"
        }),
    );
    emulator.ok("CreateApp", &app).await;

    let (status, parsed) = emulator.call("DeleteDomain", &addressed).await;
    assert_eq!(
        status, 400,
        "case holds-a-real-app: a domain that still holds an app is not torn down, got \
         {status} with {parsed}"
    );
    assert_eq!(
        error_code(&parsed),
        "ResourceInUse",
        "case holds-a-real-app: expected ResourceInUse, got {parsed}"
    );
    assert_eq!(
        message(&parsed),
        format!(
            "Domain [{}] has non-deleted Apps.",
            domain_arn(region, &domain_id)
        ),
        "case holds-a-real-app: the refusal names the domain it cannot tear down, got {parsed}"
    );

    emulator.ok("DeleteApp", &app).await;
    // The profile the app ran for is a member of the domain too, and members go first, so
    // the teardown is served only once that profile has been released as well.
    emulator
        .ok(
            "DeleteUserProfile",
            &json!({ "DomainId": domain_id, "UserProfileName": "studio-user" }),
        )
        .await;
    let (status, parsed) = emulator.call("DeleteDomain", &addressed).await;
    assert_eq!(
        status, 200,
        "case holds-a-real-app: the teardown is served once the app and the profile are gone, \
         got {status} with {parsed}"
    );
}

/// RULE: everything that hangs off a Studio domain goes when the domain goes — once the
/// clock completes a domain's removal its user profiles resolve to nothing, so no reader
/// reports a profile of a domain that no longer exists.
#[tokio::test]
async fn a_user_profile_goes_with_the_domain_it_lives_in() {
    let emulator = Emulator::new();

    let region = "us-east-1";
    let domain_id = created_domain(&emulator, region, "released-with-its-domain").await;
    for name in ["studio-user", "analyst"] {
        emulator
            .ok(
                "CreateUserProfile",
                &json!({ "DomainId": domain_id, "UserProfileName": name }),
            )
            .await;
    }
    // "All of the members of the domain must be deleted first", so each profile's own
    // removal is asked for before the domain's; what this case asserts is that the records
    // left behind go with the domain rather than outliving it.
    for name in ["studio-user", "analyst"] {
        emulator
            .ok(
                "DeleteUserProfile",
                &json!({ "DomainId": domain_id, "UserProfileName": name }),
            )
            .await;
    }
    emulator
        .ok("DeleteDomain", &json!({ "DomainId": domain_id }))
        .await;

    // Wait for the CONDITION: the domain's removal is the clock's, and the profiles are
    // gone exactly when it is.
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, parsed) = emulator
            .call("DescribeDomain", &json!({ "DomainId": domain_id }))
            .await;
        if status == 400 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case released-with-its-domain: the domain must be gone once the clock completes \
             the teardown, last read {parsed}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    for name in ["studio-user", "analyst"] {
        let (status, parsed) = emulator
            .call(
                "DescribeUserProfile",
                &json!({ "DomainId": domain_id, "UserProfileName": name }),
            )
            .await;
        assert_eq!(
            (status, error_code(&parsed).as_str()),
            (400, "ResourceNotFound"),
            "case released-with-its-domain: the profile {name} of a gone domain resolves to \
             nothing, got {status} with {parsed}"
        );
        assert_eq!(
            message(&parsed),
            format!(
                "UserProfile [{}] does not exist.",
                user_profile_arn(region, &domain_id, name)
            ),
            "case released-with-its-domain: the refusal names the profile, got {parsed}"
        );
    }
    // The store keeps no record of them either, so the quota slots they held are free:
    // the sweep runs where the domain store is weighed (`ListDomains` here), and it
    // materialises only removals the clock had already completed.
    emulator.ok("ListDomains", &json!({})).await;
    let store = emulator
        .engine
        .state::<ComAmazonawsSagemakerSagemakerState>();
    let profiles = store.lock().user_profiles.len();
    assert_eq!(
        profiles, 0,
        "case released-with-its-domain: the gone domain's profiles are swept, store holds \
         {profiles}"
    );
}
