// SPDX-License-Identifier: CC0-1.0

//! End-to-end wire tests for what a COMPLETED Studio domain teardown leaves behind.
//!
//! `DeleteDomain` starts an asynchronous removal and the clock finishes it. Past that
//! instant the `DomainId` names nothing in the account, so EVERY operation that addresses
//! a domain by that id has to answer its own not-found refusal, nothing that hung off the
//! domain may be listed under it any more, and the slot the record held has to be free
//! again — a caller that re-sends the identical `CreateDomain` gets a fresh domain under a
//! new id rather than the stale record of the one it deleted.
//!
//! Every case goes over the wire (`POST /` with `X-Amz-Target: SageMaker.<Op>`,
//! `awsJson1_1`) through the same `MultiServiceRouter` the production router uses, so what
//! these tests exercise is what a real SDK client gets. The harness (`Emulator`,
//! `error_code`, `message`, `str_field`) is the one `inference_deployment_lifecycle`
//! already defines for this target, the domain helpers are `studio_user_profiles`' and the
//! wait for a completed removal is `studio_domain_deletion`'s.
//!
//! The rule is asserted over a TABLE of inputs rather than one recorded call: the rows
//! differ along what it generalises over — the Region signed for, both `AuthMode` members,
//! the name shape up to the 63-character maximum `DomainName` admits, and which optional
//! members the create carried — and each row is driven through every operation that
//! addresses a domain by its id, not only through the reader. Each row's domain really
//! HOLDS a user profile and an app before its teardown, so the listings that report
//! nothing afterwards are listings that reported something before.
//!
//! What this suite does NOT repeat: the `Deleting` status the teardown is observable
//! through, the refusals a domain still holding members takes and the audit record the
//! teardown leaves all belong to `studio_domain_deletion` and
//! `studio_domain_member_teardown`.

use serde_json::{json, Value};
use service_com_amazonaws_sagemaker_sagemaker::state::{
    ComAmazonawsSagemakerSagemakerState, DOMAIN_STATUS_IN_SERVICE, DOMAIN_STATUS_PENDING,
};

use crate::inference_deployment_lifecycle::{error_code, message, str_field, Emulator};
use crate::studio_domain_deletion::poll_until_gone;
use crate::studio_user_profiles::{
    domain_arn, minimal_domain_with_auth, wait_for_domain_in_service, EXECUTION_ROLE,
};

/// The user profile each row's domain holds before its teardown, and the app that profile
/// runs: the records whose listings have to stop reporting anything under a gone domain.
const PROFILE: &str = "studio-user";
const APP_TYPE: &str = "JupyterServer";
const APP_NAME: &str = "default";

/// `AuthMode`'s IAM Identity Center member, which is the one a user profile carries a
/// single-sign-on identity for (`smithy_query(query="shape", shape="AuthMode")`).
const SSO_AUTH_MODE: &str = "SSO";

/// One row of the teardown table: the `CreateDomain` request that is created, torn down
/// and then sent again unchanged, and the Region it is signed for.
struct Row {
    case: &'static str,
    region: &'static str,
    create: Value,
}

impl Row {
    /// The `CreateUserProfile` request this row's domain admits: the profile's name, plus
    /// the single-sign-on identity a domain whose members authenticate with IAM Identity
    /// Center requires ("required when the Domain's AuthMode is IAM Identity Center") and
    /// no other domain accepts.
    fn profile(&self, domain_id: &str) -> Value {
        let mut request = json!({ "DomainId": domain_id, "UserProfileName": PROFILE });
        if str_field(&self.create, "AuthMode") == SSO_AUTH_MODE {
            request["SingleSignOnUserIdentifier"] = json!("UserName");
            request["SingleSignOnUserValue"] = json!("directory-user");
        }
        request
    }
}

/// The `DomainId`s listed under a response member, in response order. A listing that
/// reports nothing may leave the member out entirely, which is the same answer as an
/// empty list.
fn listed_domain_ids(body: &Value, member: &str) -> Vec<String> {
    body.get(member)
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .map(|entry| str_field(entry, "DomainId"))
                .collect()
        })
        .unwrap_or_default()
}

/// The `DomainId`s `target` reports for the domain `domain_id`, which is how a caller asks
/// a listing what the domain holds.
async fn listed_under(
    emulator: &Emulator,
    region: &str,
    case: &str,
    target: &str,
    member: &str,
    domain_id: &str,
) -> Vec<String> {
    let (status, page) = emulator
        .call_in(region, target, &json!({ "DomainIdEquals": domain_id }))
        .await;
    assert_eq!(
        status, 200,
        "case {case}: expected HTTP 200 from {target}, got {status} with {page}"
    );
    listed_domain_ids(&page, member)
}

/// The listings whose entries hang off the domain, as `(operation, response member)`.
const DOMAIN_LISTINGS: [(&str, &str); 2] =
    [("ListUserProfiles", "UserProfiles"), ("ListApps", "Apps")];

/// RULE: a Studio domain whose removal the clock has completed is gone from the account —
/// every operation that addresses a domain by its `DomainId` answers the refusal it words
/// that fact with, nothing is listed under that id any more, the id is never handed back
/// to a later create, and the slot the record held is free, so the identical
/// `CreateDomain` is served again under a fresh id.
#[tokio::test]
async fn a_completed_domain_teardown_releases_the_account() {
    let emulator = Emulator::new();

    // Rows differ along what the rule generalises over: the Region signed for, both
    // `AuthMode` members, the name shape (one character, hyphenated, the 63-character
    // maximum `DomainName` admits) and which optional members the create carried.
    let longest_name = format!("a{}", "b".repeat(62));
    let rows: Vec<Row> = vec![
        Row {
            case: "minimal-iam",
            region: "us-east-1",
            create: minimal_domain_with_auth("studio", "IAM"),
        },
        Row {
            case: "single-character-name-sso",
            region: "eu-west-1",
            create: minimal_domain_with_auth("a", "SSO"),
        },
        Row {
            case: "optional-members-and-tags",
            region: "ap-northeast-1",
            create: json!({
                "DomainName": "studio-with-options",
                "AuthMode": "IAM",
                "DefaultUserSettings": { "ExecutionRole": EXECUTION_ROLE },
                // `VpcId` and `SubnetIds` are optional only for a domain created from
                // SageMaker Unified Studio, so a SageMaker AI domain names both; this row
                // names two subnets where the others name one.
                "VpcId": "vpc-0a1b2c3d",
                "SubnetIds": ["subnet-0a1b2c3d", "subnet-4e5f6a7b"],
                "AppNetworkAccessType": "PublicInternetOnly",
                "HomeEfsFileSystemCreation": "Disabled",
                "TagPropagation": "ENABLED",
                "KmsKeyId": "alias/studio",
                "Tags": [{ "Key": "team", "Value": "research" }]
            }),
        },
        Row {
            case: "longest-name",
            region: "us-west-2",
            create: minimal_domain_with_auth(&longest_name, "IAM"),
        },
        // A second domain in a Region another row already uses, so the listings below are
        // narrowed by the `DomainIdEquals` they state rather than reporting everything the
        // Region holds — and so the teardown of one domain is seen not to take the other.
        Row {
            case: "second-domain-in-a-region-another-row-uses",
            region: "us-east-1",
            create: minimal_domain_with_auth("studio-beside-another", "IAM"),
        },
    ];

    // Every row is created first and the clock-owed spans are waited out across the whole
    // table rather than once per row: the creates fall due within milliseconds of one
    // another, so the first wait covers them all.
    let mut created_ids: Vec<(&Row, String)> = Vec::new();
    for row in &rows {
        let (status, created) = emulator
            .call_in(row.region, "CreateDomain", &row.create)
            .await;
        assert_eq!(
            status, 200,
            "case {}: expected HTTP 200 from CreateDomain, got {status} with {created}",
            row.case
        );
        created_ids.push((row, str_field(&created, "DomainId")));
    }

    // A domain admits members only once its own provisioning has settled, so each row's
    // domain really holds a user profile and an app before its teardown — which is what
    // makes the empty listings below a statement about the removal rather than about a
    // domain that never held anything.
    for (row, domain_id) in &created_ids {
        let case = row.case;
        let region = row.region;
        wait_for_domain_in_service(&emulator, region, domain_id).await;

        let profile = row.profile(domain_id);
        let (status, created) = emulator
            .call_in(region, "CreateUserProfile", &profile)
            .await;
        assert_eq!(
            status, 200,
            "case {case}: expected HTTP 200 from CreateUserProfile, got {status} with {created}"
        );
        let app = json!({
            "DomainId": domain_id,
            "UserProfileName": PROFILE,
            "AppType": APP_TYPE,
            "AppName": APP_NAME,
        });
        let (status, created) = emulator.call_in(region, "CreateApp", &app).await;
        assert_eq!(
            status, 200,
            "case {case}: expected HTTP 200 from CreateApp, got {status} with {created}"
        );

        // What the listings report while the domain holds them, so the reading below is a
        // change rather than a constant.
        for (target, member) in DOMAIN_LISTINGS {
            let listed = listed_under(&emulator, region, case, target, member, domain_id).await;
            assert_eq!(
                listed,
                vec![domain_id.clone()],
                "case {case}: {target} reports what the live domain holds, got {listed:?}"
            );
        }

        // "All of the members of the domain must be deleted first", so each member's own
        // removal is asked for before the domain's; the ordering itself is
        // `studio_domain_member_teardown`'s rule, not this suite's.
        let delete_profile = json!({ "DomainId": domain_id, "UserProfileName": PROFILE });
        for (target, request) in [("DeleteApp", &app), ("DeleteUserProfile", &delete_profile)] {
            let (status, parsed) = emulator.call_in(region, target, request).await;
            assert_eq!(
                status, 200,
                "case {case}: expected HTTP 200 from {target}, got {status} with {parsed}"
            );
        }

        let (status, deleted) = emulator
            .call_in(region, "DeleteDomain", &json!({ "DomainId": domain_id }))
            .await;
        assert_eq!(
            status, 200,
            "case {case}: expected HTTP 200 from DeleteDomain, got {status} with {deleted}"
        );
    }

    for (row, gone_id) in &created_ids {
        let case = row.case;
        let region = row.region;
        let gone_arn = domain_arn(region, gone_id);
        let addressed = json!({ "DomainId": gone_id });

        // The reader of the domain itself: the error the operation models for a resource
        // it cannot find, worded with the ARN the requested id names in the signed Region.
        let refusal = poll_until_gone(&emulator, region, case, gone_id).await;
        assert_eq!(
            error_code(&refusal),
            "ResourceNotFound",
            "case {case}: DescribeDomain refuses a gone domain as not found, got {refusal}"
        );
        assert_eq!(
            message(&refusal),
            format!("Domain [{gone_arn}] does not exist."),
            "case {case}: the refusal names the domain it cannot resolve, got {refusal}"
        );

        // The teardown itself resolves the id exactly as the reader does: there is no
        // second removal to ask for.
        let (status, parsed) = emulator.call_in(region, "DeleteDomain", &addressed).await;
        assert_eq!(
            (status, error_code(&parsed).as_str()),
            (400, "ResourceNotFound"),
            "case {case}: DeleteDomain refuses a gone domain as not found, got {status} with \
             {parsed}"
        );
        assert_eq!(
            message(&parsed),
            format!("Domain [{gone_arn}] does not exist."),
            "case {case}: the teardown's refusal names the same domain, got {parsed}"
        );

        // Every create whose subject is a MEMBER of the domain resolves the same id, and
        // none of them may attach a resource to a domain that exists nowhere. These three
        // model no not-found error, so the fact travels as the service-common
        // `ValidationException`. The two wordings are the crate's two subject forms: an
        // operation whose subject is a member of the domain names the `DomainId` the
        // REQUEST supplied, while one whose subject is the domain itself names the ARN that
        // id resolves to — spelled out here rather than taken from the helpers that
        // produce them, so a change of wording fails these rows.
        let member_creates: Vec<(&str, Value, String)> = vec![
            (
                "CreateUserProfile",
                row.profile(gone_id),
                format!("Domain [{gone_id}] does not exist"),
            ),
            (
                "CreateSpace",
                json!({ "DomainId": gone_id, "SpaceName": "shared" }),
                format!("Domain [{gone_id}] does not exist"),
            ),
            (
                "CreateApp",
                json!({
                    "DomainId": gone_id,
                    "UserProfileName": PROFILE,
                    "AppType": APP_TYPE,
                    "AppName": APP_NAME
                }),
                format!("Domain [{gone_arn}] does not exist."),
            ),
        ];
        for (target, request, expected) in member_creates {
            let (status, parsed) = emulator.call_in(region, target, &request).await;
            assert_eq!(
                (status, error_code(&parsed).as_str()),
                (400, "ValidationException"),
                "case {case}: {target} cannot create a member of a gone domain, got {status} \
                 with {parsed}"
            );
            assert_eq!(
                message(&parsed),
                expected,
                "case {case}: {target}'s refusal names the domain it cannot resolve, got \
                 {parsed}"
            );
        }

        // And the records the domain DID hold are reported nowhere under its id any more:
        // the profile and the app the listings answered above went with the domain.
        for (target, member) in DOMAIN_LISTINGS {
            let listed = listed_under(&emulator, region, case, target, member, gone_id).await;
            assert!(
                listed.is_empty(),
                "case {case}: {target} reports nothing under a gone domain, got {listed:?}"
            );
        }
    }

    // The account is clean: the identical create is served again, under an id of its own,
    // and what it answers is a fresh domain rather than the record the teardown took.
    for (row, gone_id) in &created_ids {
        let case = row.case;
        let region = row.region;
        let (status, recreated) = emulator
            .call_in(region, "CreateDomain", &row.create)
            .await;
        assert_eq!(
            status, 200,
            "case {case}: the identical create is served once the account is clean, got \
             {status} with {recreated}"
        );
        let new_id = str_field(&recreated, "DomainId");
        assert_ne!(
            &new_id, gone_id,
            "case {case}: a torn-down id is never handed back, got {recreated}"
        );
        assert_eq!(
            str_field(&recreated, "DomainArn"),
            domain_arn(region, &new_id),
            "case {case}: the new domain's ARN names its own id, got {recreated}"
        );

        let (status, described) = emulator
            .call_in(region, "DescribeDomain", &json!({ "DomainId": new_id }))
            .await;
        assert_eq!(
            status, 200,
            "case {case}: expected HTTP 200 from DescribeDomain of the new domain, got \
             {status} with {described}"
        );
        assert_eq!(
            str_field(&described, "DomainName"),
            str_field(&row.create, "DomainName"),
            "case {case}: the new domain carries the configuration the create sent, got \
             {described}"
        );
        let status_read = str_field(&described, "Status");
        assert!(
            status_read == DOMAIN_STATUS_PENDING || status_read == DOMAIN_STATUS_IN_SERVICE,
            "case {case}: the new domain is a live one, never one carrying the gone domain's \
             teardown, got {described}"
        );
        let url = str_field(&described, "Url");
        assert!(
            url.contains(&new_id) && !url.contains(gone_id.as_str()),
            "case {case}: the new domain is served at its own address, got {url}"
        );
        // It holds nothing either: the members of the gone domain are not inherited by the
        // one created in its place.
        for (target, member) in DOMAIN_LISTINGS {
            let listed = listed_under(&emulator, region, case, target, member, &new_id).await;
            assert!(
                listed.is_empty(),
                "case {case}: {target} reports nothing under the new domain, got {listed:?}"
            );
        }

        // And the gone id still resolves to nothing: re-creating a domain resurrects no
        // record.
        let (status, parsed) = emulator
            .call_in(region, "DescribeDomain", &json!({ "DomainId": gone_id }))
            .await;
        assert_eq!(
            (status, error_code(&parsed).as_str()),
            (400, "ResourceNotFound"),
            "case {case}: the torn-down id stays gone after the new create, got {status} with \
             {parsed}"
        );
    }

    // The slot each gone record held is free: the store holds exactly the domains the
    // re-creates minted, read straight from the shared engine the handlers write.
    let store = emulator
        .engine
        .state::<ComAmazonawsSagemakerSagemakerState>();
    let held = store.lock().domains.len();
    assert_eq!(
        held,
        rows.len(),
        "case store: a completed teardown frees the slot its domain held, so only the {} \
         re-created domains are still held, got {held}",
        rows.len()
    );
}
