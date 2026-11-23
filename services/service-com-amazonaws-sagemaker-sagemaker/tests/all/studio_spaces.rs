// SPDX-License-Identifier: CC0-1.0

//! End-to-end wire tests for SageMaker Studio's SPACE — `CreateSpace`, `DescribeSpace` and
//! `DeleteSpace` — and for the sentence every create of a MEMBER of a Studio domain words a
//! `DomainId` that resolves to no domain with.
//!
//! Every case goes over the wire (`POST /` with `X-Amz-Target: SageMaker.<Op>`,
//! `awsJson1_1`) through the same `MultiServiceRouter` the production router uses, so what
//! these tests exercise is what a real SDK client gets. The harness (`Emulator`,
//! `error_code`, `message`, `str_field`) is the one `inference_deployment_lifecycle`
//! defines for this target, and the domain seeding is `studio_user_profiles`'.
//!
//! The tests assert RULES over a TABLE of inputs rather than one recorded call: the
//! create/read round trip for every optional member of a space across Regions and name
//! shapes at the modelled extremes; the missing-domain refusal for BOTH member-creates the
//! rule covers, over several Regions and `DomainId` shapes; the refusals a create cannot get
//! past; and the removal the clock owes a deleted space.

use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_sagemaker_sagemaker::ops::types::SPACE_STATUS_VALUES;
use service_com_amazonaws_sagemaker_sagemaker::state::{
    self, ComAmazonawsSagemakerSagemakerState, Space, DOMAIN_STATUS_PENDING,
    SPACE_STATUS_DELETING, SPACE_STATUS_IN_SERVICE, SPACE_STATUS_PENDING,
};

use crate::inference_deployment_lifecycle::{error_code, message, str_field, Emulator, ACCOUNT};
use crate::studio_user_profiles::{
    domain_arn, seed_studio_domain, seed_studio_owner, POLL_INTERVAL, SETTLE_DEADLINE,
};

/// The ARN `SpaceArn`'s modelled pattern gives the space `space_name` of `domain_id` in
/// `region`.
fn space_arn(region: &str, domain_id: &str, space_name: &str) -> String {
    format!("arn:aws:sagemaker:{region}:{ACCOUNT}:space/{domain_id}/{space_name}")
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

/// A `DomainId` inside the modelled pattern `^d-(-*[a-z0-9]){1,61}$` whose 63 characters
/// are the longest `DomainId`'s modelled length admits.
fn longest_domain_id() -> String {
    format!("d-{}", "a".repeat(61))
}

/// A `SpaceName` inside the modelled pattern `^[a-zA-Z0-9](-*[a-zA-Z0-9]){0,62}$` whose 63
/// characters are the longest `SpaceName`'s modelled length admits.
fn longest_space_name() -> String {
    format!("s{}", "a".repeat(62))
}

/// Seed the domain `domain_id` in `region` and leave it `Pending` with its `InService` due
/// far beyond any test's lifetime, so a create addressing it is weighed against a domain
/// that has NOT finished provisioning, whatever the host's speed.
fn seed_pending_domain(emulator: &Emulator, region: &str, domain_id: &str) {
    seed_studio_domain(emulator, region, domain_id);
    let store = emulator
        .engine
        .state::<ComAmazonawsSagemakerSagemakerState>();
    let mut store = store.lock();
    let domain = store
        .domains
        .get_mut(domain_id)
        .expect("the seeded domain is in the store");
    domain.status = DOMAIN_STATUS_PENDING.to_string();
    domain.in_service_due_at = SystemTime::now() + Duration::from_secs(3_600);
}

/// Write a space into the shared store the handlers read, `Deleting` with its removal due
/// at `removal_due_at`, so a call that resolves it meets a space whose teardown is in
/// flight (a due instant in the future) or already completed (one in the past) without any
/// test depending on how fast the host runs.
fn seed_deleting_space(
    emulator: &Emulator,
    region: &str,
    domain_id: &str,
    space_name: &str,
    removal_due_at: SystemTime,
) {
    seed_studio_domain(emulator, region, domain_id);
    let store = emulator
        .engine
        .state::<ComAmazonawsSagemakerSagemakerState>();
    let mut store = store.lock();
    let when = SystemTime::UNIX_EPOCH + Duration::from_secs(1_767_225_600);
    store.spaces.insert(
        state::space_key(domain_id, space_name),
        Space {
            arn: space_arn(region, domain_id, space_name),
            domain_id: domain_id.to_string(),
            name: space_name.to_string(),
            display_name: None,
            space_settings: None,
            ownership_settings: None,
            space_sharing_settings: None,
            url: state::studio_space_url(
                &state::studio_domain_url(region, domain_id),
                space_name,
            ),
            tags: Vec::new(),
            status: SPACE_STATUS_DELETING.to_string(),
            creation_time: when,
            last_modified_time: when,
            in_service_due_at: when,
            removal_due_at: Some(removal_due_at),
        },
    );
}

/// One row of the create/read round trip: the Region the call signs for, the space's name
/// and the optional members the create carries.
struct SpaceRow {
    case: &'static str,
    region: &'static str,
    name: String,
    extra: Value,
}

/// RULE: `CreateSpace` stores the space the request's `DomainId` and `SpaceName` pair
/// identifies and answers its ARN, and `DescribeSpace` replays exactly what the create
/// carried — the name as spelled, every optional member stored verbatim, nothing the
/// request never named — with the space launching `Pending` and reaching `InService` on the
/// clock.
#[tokio::test]
async fn create_space_round_trips_through_describe_space() {
    let emulator = Emulator::new();

    let rows: Vec<SpaceRow> = vec![
        SpaceRow {
            case: "minimal",
            region: "us-east-1",
            name: "team-space".to_string(),
            extra: json!({}),
        },
        SpaceRow {
            case: "display-name",
            region: "us-east-1",
            name: "named-space".to_string(),
            extra: json!({ "SpaceDisplayName": "Team Space" }),
        },
        SpaceRow {
            case: "private-sharing",
            region: "eu-west-1",
            name: "private-space".to_string(),
            // `SharingType`'s members are `Private` and `Shared`
            // (smithy_query(query="shape", shape="SharingType")).
            extra: json!({ "SpaceSharingSettings": { "SharingType": "Private" } }),
        },
        SpaceRow {
            case: "shared-sharing-and-settings",
            region: "eu-west-1",
            name: "shared-space".to_string(),
            extra: json!({
                "SpaceSharingSettings": { "SharingType": "Shared" },
                "SpaceSettings": { "AppType": "JupyterLab" }
            }),
        },
        SpaceRow {
            case: "settings-and-tags",
            region: "ap-northeast-1",
            name: "s".to_string(),
            extra: json!({
                "SpaceSettings": { "AppType": "CodeEditor" },
                "Tags": [{ "Key": "team", "Value": "research" }]
            }),
        },
        SpaceRow {
            case: "longest-name",
            region: "us-west-2",
            name: longest_space_name(),
            extra: json!({}),
        },
    ];

    let mut created: Vec<(&'static str, &'static str, String, String)> = Vec::new();
    for row in &rows {
        let case = row.case;
        // The `DomainId` is the server's, so each row's domain is seeded already settled —
        // a domain admits members only once it reports `InService`.
        let domain_id = format!("d-{case}").to_lowercase().replace('_', "-");
        seed_studio_domain(&emulator, row.region, &domain_id);

        let request = with_members(
            &json!({ "DomainId": domain_id, "SpaceName": row.name }),
            &row.extra,
        );
        let (status, answer) = emulator.call_in(row.region, "CreateSpace", &request).await;
        assert_eq!(
            status, 200,
            "case {case}: expected HTTP 200 from CreateSpace, got {status} with {answer}"
        );
        let expected_arn = space_arn(row.region, &domain_id, &row.name);
        assert_eq!(
            str_field(&answer, "SpaceArn"),
            expected_arn,
            "case {case}: CreateSpace must mint the space's ARN, got {answer}"
        );

        let read = json!({ "DomainId": domain_id, "SpaceName": row.name });
        let (status, described) = emulator.call_in(row.region, "DescribeSpace", &read).await;
        assert_eq!(
            status, 200,
            "case {case}: expected HTTP 200 from DescribeSpace, got {status} with {described}"
        );
        assert_eq!(
            str_field(&described, "SpaceArn"),
            expected_arn,
            "case {case}: the reader replays the stored ARN, got {described}"
        );
        assert_eq!(
            str_field(&described, "DomainId"),
            domain_id,
            "case {case}: the reader names the domain the space lives in, got {described}"
        );
        assert_eq!(
            str_field(&described, "SpaceName"),
            row.name,
            "case {case}: the reader replays the name as the create spelled it, got \
             {described}"
        );
        let status_read = str_field(&described, "Status");
        assert!(
            status_read == SPACE_STATUS_PENDING || status_read == SPACE_STATUS_IN_SERVICE,
            "case {case}: a just-created space is launching or up, got {described}"
        );
        assert!(
            SPACE_STATUS_VALUES.contains(&status_read.as_str()),
            "case {case}: the status must be a member of the modelled catalog, got \
             {described}"
        );
        for member in ["CreationTime", "LastModifiedTime"] {
            assert!(
                described.get(member).is_some_and(Value::is_number),
                "case {case}: {member} must be a stored timestamp, got {described}"
            );
        }
        // The space is reached through its domain, so its address is the domain's Studio
        // host with the space on it.
        assert_eq!(
            str_field(&described, "Url"),
            format!(
                "{}/space/{}",
                state::studio_domain_url(row.region, &domain_id),
                row.name
            ),
            "case {case}: the reader reports the space's address under its domain, got \
             {described}"
        );
        // Nothing created an EFS volume for the domain and nothing failed the space's
        // provisioning, so neither member is reported.
        for member in ["HomeEfsFileSystemUid", "FailureReason"] {
            assert!(
                described.get(member).is_none(),
                "case {case}: {member} must be omitted when nothing produced one, got \
                 {described}"
            );
        }
        // Every optional member the create carried is replayed as stored.
        for (member, value) in row
            .extra
            .as_object()
            .expect("the extra members form an object")
        {
            if member == "Tags" {
                // `DescribeSpace` models no tag member: the stored tags are what `ListTags`
                // reports for the space's ARN.
                let tags = emulator
                    .call_in(
                        row.region,
                        "ListTags",
                        &json!({ "ResourceArn": expected_arn }),
                    )
                    .await
                    .1;
                assert_eq!(
                    tags.get("Tags"),
                    Some(value),
                    "case {case}: the tags the create carried must be the space's, got {tags}"
                );
                continue;
            }
            assert_eq!(
                described.get(member),
                Some(value),
                "case {case}: {member} must be replayed as stored, got {described}"
            );
        }
        // A member the create never named is reported by nobody.
        for member in [
            "SpaceDisplayName",
            "SpaceSettings",
            "SpaceSharingSettings",
            "OwnershipSettings",
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

    // The launch the clock owes each space becomes visible without any call having to make
    // it happen, and stably thereafter.
    for (case, region, domain_id, name) in &created {
        let read = json!({ "DomainId": domain_id, "SpaceName": name });
        // Wait for the CONDITION, not for a fixed time.
        let deadline = Instant::now() + SETTLE_DEADLINE;
        loop {
            let (status, described) = emulator.call_in(region, "DescribeSpace", &read).await;
            assert_eq!(
                status, 200,
                "case {case}: the space stays readable while it settles, got {status} with \
                 {described}"
            );
            if str_field(&described, "Status") == SPACE_STATUS_IN_SERVICE {
                let again = emulator.call_in(region, "DescribeSpace", &read).await;
                assert_eq!(
                    again.1, described,
                    "case {case}: a settled space must read back identically"
                );
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case}: the space must reach {SPACE_STATUS_IN_SERVICE} on the clock, \
                 last read {described}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

/// RULE: a create whose subject is a MEMBER of a Studio domain — a user profile or a space —
/// states a `DomainId` this account holds no domain under with HTTP 400
/// `ValidationException` whose message is `Domain [<DomainId>] does not exist`: the id
/// EXACTLY as the request spelled it, and no closing period. The ARN form of that sentence
/// belongs to the operations whose subject is the domain ITSELF, and nothing is stored for
/// the refused create.
#[tokio::test]
async fn studio_member_create_names_the_request_domain_id_when_no_domain_resolves() {
    let emulator = Emulator::new();

    // Rows: well-formed `DomainId`s nothing is stored under — the modelled pattern is
    // `^d-(-*[a-z0-9]){1,61}$`, so each reaches the handler — in the Regions whose ARNs a
    // sentence naming one would carry.
    let longest = longest_domain_id();
    let domain_ids: Vec<(&str, &str, &str)> = vec![
        ("absent-id", "us-east-1", "d-000000000000"),
        ("hex-id", "us-east-1", "d-abc123def456"),
        ("id-in-another-region", "eu-west-1", "d-0a1b2c3d4e5f"),
        ("shortest-id", "us-west-2", "d-a"),
        ("embedded-dashes", "ap-northeast-1", "d--a-b"),
        ("longest-id", "us-west-2", longest.as_str()),
    ];

    // Both member-creates the rule covers, each with the member that names the member
    // resource it would create.
    let creates: [(&str, Value); 2] = [
        ("CreateUserProfile", json!({ "UserProfileName": "studio-user" })),
        ("CreateSpace", json!({ "SpaceName": "team-space" })),
    ];

    for (case, region, domain_id) in &domain_ids {
        for (operation, member) in &creates {
            let case = format!("{case}-{operation}");
            let request = with_members(&json!({ "DomainId": domain_id }), member);
            let (status, parsed) = emulator.call_in(region, operation, &request).await;
            assert_eq!(
                status, 400,
                "case {case}: a member cannot be created in a domain that does not exist, \
                 got {status} with {parsed}"
            );
            assert_eq!(
                error_code(&parsed),
                "ValidationException",
                "case {case}: expected ValidationException, got {parsed}"
            );
            let said = message(&parsed);
            assert_eq!(
                said,
                format!("Domain [{domain_id}] does not exist"),
                "case {case}: the refusal names the domain id the request supplied, got \
                 {parsed}"
            );
            // The two ways the sentence used to diverge: an ARN interpolated where the
            // request's id belongs, and a period AWS's sentence does not carry.
            assert!(
                !said.contains("arn:"),
                "case {case}: the sentence must carry no ARN, got {said:?}"
            );
            assert!(
                !said.ends_with('.'),
                "case {case}: the sentence must have no closing period, got {said:?}"
            );
        }

        // Nothing was stored for either refused create.
        let (status, parsed) = emulator
            .call_in(
                region,
                "DescribeSpace",
                &json!({ "DomainId": domain_id, "SpaceName": "team-space" }),
            )
            .await;
        assert_eq!(
            (status, error_code(&parsed).as_str()),
            (400, "ResourceNotFound"),
            "case {case}: the refused create stored nothing, got {status} with {parsed}"
        );
        assert_eq!(
            message(&parsed),
            format!("Space [{}] does not exist.", space_arn(region, domain_id, "team-space")),
            "case {case}: the space's own reader names the space it could not resolve, got \
             {parsed}"
        );
    }

    // The operations whose SUBJECT is the domain keep naming it by the ARN its id resolves
    // to: the member form above is the sentence for a create addressing something INSIDE
    // the domain, not a change to the one sentence the domain's own readers word.
    let (status, parsed) = emulator
        .call_in(
            "us-east-1",
            "DescribeDomain",
            &json!({ "DomainId": "d-000000000000" }),
        )
        .await;
    assert_eq!(
        (status, error_code(&parsed).as_str()),
        (400, "ResourceNotFound"),
        "case domain-reader: a domain that does not exist is refused, got {status} with \
         {parsed}"
    );
    assert_eq!(
        message(&parsed),
        format!(
            "Domain [{}] does not exist.",
            domain_arn("us-east-1", "d-000000000000")
        ),
        "case domain-reader: the domain's own reader names it by ARN, got {parsed}"
    );
}

/// RULE: a space holds its name in its domain alone and hangs off resources that exist, so
/// `CreateSpace` refuses a second space of a name the domain already holds as in use, a
/// domain that has not reached `InService`, and an `OwnerUserProfileName` the domain holds
/// no user profile under — while a name differing only in case is a different space, since
/// `SpaceName` carries no documented case-insensitivity.
#[tokio::test]
async fn create_space_refuses_what_it_cannot_create() {
    let emulator = Emulator::new();
    let region = "us-east-1";

    // A name the domain already holds, in the spelling the create carried.
    let domain_id = "d-duplicate-space";
    seed_studio_domain(&emulator, region, domain_id);
    for (case, name) in [("first", "team-space"), ("other-case-spelling", "Team-Space")] {
        let (status, answer) = emulator
            .call_in(
                region,
                "CreateSpace",
                &json!({ "DomainId": domain_id, "SpaceName": name }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case}: a name the domain does not hold yet is created, got {status} with \
             {answer}"
        );
        assert_eq!(
            str_field(&answer, "SpaceArn"),
            space_arn(region, domain_id, name),
            "case {case}: each spelling is its own space, got {answer}"
        );

        let (status, parsed) = emulator
            .call_in(
                region,
                "CreateSpace",
                &json!({ "DomainId": domain_id, "SpaceName": name }),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case}: a second create of the same name is refused, got {status} with \
             {parsed}"
        );
        assert_eq!(
            error_code(&parsed),
            "ResourceInUse",
            "case {case}: expected the modelled ResourceInUse, got {parsed}"
        );
        assert_eq!(
            message(&parsed),
            format!("A Space with name '{name}' already exists in Domain {domain_id}."),
            "case {case}: the refusal names the space and the domain it is taken in, got \
             {parsed}"
        );
    }

    // The same name in another domain is a different space.
    let other_domain_id = "d-other-space-domain";
    seed_studio_domain(&emulator, region, other_domain_id);
    let answer = emulator
        .ok(
            "CreateSpace",
            &json!({ "DomainId": other_domain_id, "SpaceName": "team-space" }),
        )
        .await;
    assert_eq!(
        str_field(&answer, "SpaceArn"),
        space_arn("us-east-1", other_domain_id, "team-space"),
        "case another-domain: the same name in another domain is its own space, got {answer}"
    );

    // A domain that has not finished provisioning admits no member.
    let pending_domain_id = "d-pending-space-domain";
    seed_pending_domain(&emulator, region, pending_domain_id);
    let (status, parsed) = emulator
        .call_in(
            region,
            "CreateSpace",
            &json!({ "DomainId": pending_domain_id, "SpaceName": "team-space" }),
        )
        .await;
    assert_eq!(
        status, 400,
        "case pending-domain: a space cannot be created in a domain that is still \
         provisioning, got {status} with {parsed}"
    );
    assert_eq!(
        error_code(&parsed),
        "ValidationException",
        "case pending-domain: expected ValidationException, got {parsed}"
    );
    assert_eq!(
        message(&parsed),
        format!(
            "Unable to create Space [{}] because Domain [{}] is not in InService.",
            space_arn(region, pending_domain_id, "team-space"),
            domain_arn(region, pending_domain_id)
        ),
        "case pending-domain: the refusal names the space, the domain and the status it \
         needs, got {parsed}"
    );

    // An owner the domain holds no user profile under leaves the space unowned, so the
    // create is refused; the same request is served once that owner exists.
    let owner_domain_id = "d-owned-space-domain";
    seed_studio_domain(&emulator, region, owner_domain_id);
    for (case, owner, refused) in [
        ("absent-owner", "ghost-user", true),
        ("present-owner", "studio-user", false),
    ] {
        if !refused {
            seed_studio_owner(&emulator, region, owner_domain_id, owner);
        }
        let request = json!({
            "DomainId": owner_domain_id,
            "SpaceName": format!("space-for-{owner}"),
            "OwnershipSettings": { "OwnerUserProfileName": owner },
            "SpaceSharingSettings": { "SharingType": "Private" }
        });
        let (status, parsed) = emulator.call_in(region, "CreateSpace", &request).await;
        if !refused {
            assert_eq!(
                status, 200,
                "case {case}: an owner the domain holds is served, got {status} with {parsed}"
            );
            continue;
        }
        assert_eq!(
            status, 400,
            "case {case}: an owner that does not exist is refused, got {status} with {parsed}"
        );
        assert_eq!(
            error_code(&parsed),
            "ValidationException",
            "case {case}: expected ValidationException, got {parsed}"
        );
        assert_eq!(
            message(&parsed),
            format!("UserProfile [{owner}] does not exist."),
            "case {case}: the refusal names the owner it could not resolve, got {parsed}"
        );
        // Nothing was stored for the refused create.
        let (status, read) = emulator
            .call_in(
                region,
                "DescribeSpace",
                &json!({ "DomainId": owner_domain_id, "SpaceName": format!("space-for-{owner}") }),
            )
            .await;
        assert_eq!(
            (status, error_code(&read).as_str()),
            (400, "ResourceNotFound"),
            "case {case}: the refused create stored nothing, got {status} with {read}"
        );
    }
}

/// RULE: `DeleteSpace` starts the removal of the space its `DomainId` and `SpaceName` pair
/// addresses — the space reports `Deleting` until the clock completes it and resolves to
/// nothing afterwards — while a pair this account holds no space under is refused the
/// modelled `ResourceNotFound` and a space whose removal is already in flight the modelled
/// `ResourceInUse`.
#[tokio::test]
async fn delete_space_releases_the_space_on_the_clock() {
    let emulator = Emulator::new();

    // A pair no space is stored under is refused rather than answered a success a caller
    // could not tell from a teardown that happened — in every Region, so the sentence names
    // the ARN the pair resolves to there.
    for (case, region, domain_id) in [
        ("absent-space", "us-east-1", "d-delete-absent"),
        ("absent-space-elsewhere", "eu-west-1", "d-delete-absent-eu"),
    ] {
        seed_studio_domain(&emulator, region, domain_id);
        let (status, parsed) = emulator
            .call_in(
                region,
                "DeleteSpace",
                &json!({ "DomainId": domain_id, "SpaceName": "team-space" }),
            )
            .await;
        assert_eq!(
            (status, error_code(&parsed).as_str()),
            (400, "ResourceNotFound"),
            "case {case}: a space that does not exist cannot be deleted, got {status} with \
             {parsed}"
        );
        assert_eq!(
            message(&parsed),
            format!(
                "Space [{}] does not exist.",
                space_arn(region, domain_id, "team-space")
            ),
            "case {case}: the refusal names the space it could not resolve, got {parsed}"
        );
    }

    // A space whose removal is already in flight cannot take a second deletion: seeded with
    // its removal due far ahead, so the work is in flight whatever the host's speed.
    let region = "us-east-1";
    let in_flight_domain = "d-delete-in-flight";
    seed_deleting_space(
        &emulator,
        region,
        in_flight_domain,
        "team-space",
        SystemTime::now() + Duration::from_secs(3_600),
    );
    let (status, parsed) = emulator
        .call_in(
            region,
            "DeleteSpace",
            &json!({ "DomainId": in_flight_domain, "SpaceName": "team-space" }),
        )
        .await;
    assert_eq!(
        (status, error_code(&parsed).as_str()),
        (400, "ResourceInUse"),
        "case in-flight: a space already being removed cannot take a second delete, got \
         {status} with {parsed}"
    );
    assert_eq!(
        message(&parsed),
        format!(
            "Space [{}] cannot be deleted while it is in {SPACE_STATUS_DELETING} status.",
            space_arn(region, in_flight_domain, "team-space")
        ),
        "case in-flight: the refusal reports the work in flight, got {parsed}"
    );
    // That space still reads back, reporting the status it is in.
    let described = emulator
        .ok(
            "DescribeSpace",
            &json!({ "DomainId": in_flight_domain, "SpaceName": "team-space" }),
        )
        .await;
    assert_eq!(
        str_field(&described, "Status"),
        SPACE_STATUS_DELETING,
        "case in-flight: a space on its way out reports Deleting, got {described}"
    );

    // A space whose removal the clock already completed resolves to nothing, whether or not
    // a sweep has dropped the record.
    let released_domain = "d-delete-released";
    seed_deleting_space(
        &emulator,
        region,
        released_domain,
        "team-space",
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_767_225_600),
    );
    let (status, parsed) = emulator
        .call_in(
            region,
            "DescribeSpace",
            &json!({ "DomainId": released_domain, "SpaceName": "team-space" }),
        )
        .await;
    assert_eq!(
        (status, error_code(&parsed).as_str()),
        (400, "ResourceNotFound"),
        "case released: a space whose removal is complete resolves to nothing, got {status} \
         with {parsed}"
    );

    // The whole round trip: a created space is deleted, and the removal the clock owes
    // becomes visible without any call having to make it happen.
    let domain_id = "d-delete-round-trip";
    seed_studio_domain(&emulator, region, domain_id);
    let read = json!({ "DomainId": domain_id, "SpaceName": "team-space" });
    emulator.ok("CreateSpace", &read).await;
    emulator.ok("DeleteSpace", &read).await;
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, parsed) = emulator.call_in(region, "DescribeSpace", &read).await;
        if status == 400 {
            assert_eq!(
                error_code(&parsed),
                "ResourceNotFound",
                "case round-trip: the removed space is refused as not found, got {parsed}"
            );
            assert_eq!(
                message(&parsed),
                format!(
                    "Space [{}] does not exist.",
                    space_arn(region, domain_id, "team-space")
                ),
                "case round-trip: the refusal names the space that is gone, got {parsed}"
            );
            break;
        }
        assert_eq!(
            str_field(&parsed, "Status"),
            SPACE_STATUS_DELETING,
            "case round-trip: a space on its way out reports Deleting, got {parsed}"
        );
        assert!(
            Instant::now() < deadline,
            "case round-trip: the space must be gone on the clock, last read {parsed}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
