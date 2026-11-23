// SPDX-License-Identifier: CC0-1.0

//! End-to-end wire tests for SageMaker's `UpdateDomain`, read back through
//! `DescribeDomain`.
//!
//! Every case goes over the wire (`POST /` with `X-Amz-Target: SageMaker.<Op>`,
//! `awsJson1_1`) through the same `MultiServiceRouter` the production router uses, so what
//! these tests exercise is what a real SDK client gets. The harness (`Emulator`,
//! `error_code`, `message`, `str_field`) is the one `inference_deployment_lifecycle`
//! already defines for this target.
//!
//! The rules under test:
//!
//! * an update states what CHANGES: every member the request carries replaces the stored
//!   one, every member it does not carry is left as it stands, the call answers the domain's
//!   own ARN, the domain reports the non-terminal `Updating` until the clock settles it back
//!   into service, and `DescribeDomain` replays exactly the domain the update left;
//! * an update is refused — and changes nothing — for a `DomainId` this account holds no
//!   domain under (`ResourceNotFound`), for a network identifier no EC2 could have minted
//!   (`ValidationException`), for the one `HomeEfsFileSystemCreation` change the member does
//!   not admit, and while the service is already tearing the domain down (`ResourceInUse`).

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::inference_deployment_lifecycle::{error_code, message, str_field, Emulator};

/// An execution role inside `RoleArn`'s modelled pattern, for the `DefaultUserSettings`
/// every create must carry.
const EXECUTION_ROLE: &str = "arn:aws:iam::123456789012:role/service-role/StudioRole";

/// A second execution role, so a member an update rewrites can only be reporting the update.
const OTHER_EXECUTION_ROLE: &str = "arn:aws:iam::123456789012:role/service-role/OtherRole";

/// The `DefaultUserSettings` a domain whose write path carried only `execution_role` CARRIES:
/// the role as sent, plus the members SageMaker resolves for a domain — whether Studio is
/// admitted, the experience a user lands in, whether the home EFS volume is auto-mounted and
/// how an execution-role session is named — which `CreateDomain` and `UpdateDomain` resolve on
/// the write path and every reader replays. Spelled out here rather than taken from the
/// implementation, so this suite pins what the reader reports.
fn resolved_user_settings(execution_role: &str) -> Value {
    json!({
        "ExecutionRole": execution_role,
        "StudioWebPortal": "ENABLED",
        "DefaultLandingUri": "studio::",
        "AutoMountHomeEFS": "Enabled",
        "StudioWebPortalSettings": { "ExecutionRoleSessionNameMode": "USER_IDENTITY" }
    })
}

/// The Region every case here signs for, and so the Region the domain's ARN belongs to.
const REGION: &str = "us-east-1";

/// The account id every ARN this emulator mints carries.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// How long a poll gives the clock to complete a teardown it already owes. GENEROUS: no case
/// here tests how long a teardown takes.
const TEARDOWN_DEADLINE: Duration = Duration::from_secs(120);

/// How often a poll looks again. Only the loop's condition decides when it stops.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The `CreateDomain` request every case starts from: the three required members, a VPC
/// placement in the published identifier form, and the two optional members an update then
/// rewrites.
fn create_request(domain_name: &str) -> Value {
    json!({
        "DomainName": domain_name,
        "AuthMode": "IAM",
        "DefaultUserSettings": { "ExecutionRole": EXECUTION_ROLE },
        "DomainSettings": { "SecurityGroupIds": ["sg-0a1b2c3d"] },
        "VpcId": "vpc-0a1b2c3d",
        "SubnetIds": ["subnet-0a1b2c3d"],
        "TagPropagation": "DISABLED"
    })
}

/// The ARN a domain of `domain_id` carries in [`REGION`].
fn domain_arn(domain_id: &str) -> String {
    format!("arn:aws:sagemaker:{REGION}:{ACCOUNT}:domain/{domain_id}")
}

/// Create a domain and answer its `DomainId`.
async fn created_domain(emulator: &Emulator, case: &str, domain_name: &str) -> String {
    let (status, created) = emulator
        .call_in(REGION, "CreateDomain", &create_request(domain_name))
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateDomain must be served, got {status} with {created}"
    );
    str_field(&created, "DomainId")
}

/// `DescribeDomain` for `domain_id`, which must answer.
async fn described(emulator: &Emulator, case: &str, domain_id: &str) -> Value {
    let (status, read) = emulator
        .call_in(REGION, "DescribeDomain", &json!({ "DomainId": domain_id }))
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeDomain must answer the stored domain, got {status} with {read}"
    );
    read
}

/// `DescribeDomain` for `domain_id` until it reports `wanted`, and answer the reading that
/// did.
///
/// Waits for the CONDITION, never for a fixed span: a domain the clock owes a transition is
/// read again until it reports the status the rule says it reaches.
async fn domain_resting_in(emulator: &Emulator, case: &str, domain_id: &str, wanted: &str) -> Value {
    let deadline = Instant::now() + TEARDOWN_DEADLINE;
    loop {
        let read = described(emulator, case, domain_id).await;
        if str_field(&read, "Status") == wanted {
            return read;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the domain to reach {wanted:?}, got {read}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One row of the update table: the members an update carries, and the members
/// `DescribeDomain` must then report.
struct UpdateRow {
    case: &'static str,
    update: Value,
    /// `(member, expected JSON)` pairs the reader must report after the update.
    expected: Vec<(&'static str, Value)>,
}

/// RULE: `UpdateDomain` applies the members the request carries to the domain its `DomainId`
/// names, leaves every member it does not carry as it stands, answers that domain's own ARN,
/// and `DescribeDomain` replays the result — while the domain's identity (its id, ARN, name
/// and Studio URL) is untouched.
#[tokio::test]
async fn update_domain_applies_the_members_it_carries_and_leaves_the_rest() {
    // The rows differ along what the rule generalises over: which member an update rewrites
    // (a required settings structure, the network placement, an enum-typed member, the
    // settings this operation takes in its own update shape, several at once) and an update
    // that carries no member at all.
    let rows: Vec<UpdateRow> = vec![
        UpdateRow {
            case: "no member but the id",
            update: json!({}),
            expected: vec![
                (
                    "DefaultUserSettings",
                    resolved_user_settings(EXECUTION_ROLE),
                ),
                ("TagPropagation", json!("DISABLED")),
            ],
        },
        UpdateRow {
            case: "the default user settings",
            update: json!({
                "DefaultUserSettings": { "ExecutionRole": OTHER_EXECUTION_ROLE }
            }),
            expected: vec![
                (
                    "DefaultUserSettings",
                    resolved_user_settings(OTHER_EXECUTION_ROLE),
                ),
                // The members the update did not carry are left as the create stored them.
                ("SubnetIds", json!(["subnet-0a1b2c3d"])),
                ("TagPropagation", json!("DISABLED")),
            ],
        },
        UpdateRow {
            case: "the network placement",
            update: json!({
                "VpcId": "vpc-1a2b3c4d",
                "SubnetIds": ["subnet-1a2b3c4d", "subnet-2a3b4c5d"]
            }),
            expected: vec![
                ("VpcId", json!("vpc-1a2b3c4d")),
                (
                    "SubnetIds",
                    json!(["subnet-1a2b3c4d", "subnet-2a3b4c5d"]),
                ),
            ],
        },
        UpdateRow {
            case: "the enum-typed members",
            update: json!({
                "TagPropagation": "ENABLED",
                "AppSecurityGroupManagement": "Service",
                "AppNetworkAccessType": "VpcOnly"
            }),
            expected: vec![
                ("TagPropagation", json!("ENABLED")),
                ("AppSecurityGroupManagement", json!("Service")),
                ("AppNetworkAccessType", json!("VpcOnly")),
            ],
        },
        UpdateRow {
            case: "the domain settings, in this operation's own update shape",
            update: json!({
                "DomainSettingsForUpdate": {
                    "SecurityGroupIds": ["sg-1a2b3c4d", "sg-2a3b4c5d"],
                    "ExecutionRoleIdentityConfig": "USER_PROFILE_NAME"
                }
            }),
            expected: vec![(
                "DomainSettings",
                json!({
                    "SecurityGroupIds": ["sg-1a2b3c4d", "sg-2a3b4c5d"],
                    "ExecutionRoleIdentityConfig": "USER_PROFILE_NAME"
                }),
            )],
        },
        UpdateRow {
            case: "the EFS member from Disabled to Enabled is the direction it admits",
            update: json!({ "HomeEfsFileSystemCreation": "Enabled" }),
            expected: vec![("HomeEfsFileSystemCreation", json!("Enabled"))],
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let emulator = Emulator::new();
        let case = row.case;
        let name = format!("updated{index}");
        let domain_id = created_domain(&emulator, case, &name).await;
        let before = described(&emulator, case, &domain_id).await;

        let mut request = row.update.clone();
        request["DomainId"] = json!(domain_id);
        let (status, updated) = emulator.call_in(REGION, "UpdateDomain", &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: UpdateDomain must be served, got {status} with {updated}"
        );
        assert_eq!(
            str_field(&updated, "DomainArn"),
            domain_arn(&domain_id),
            "case {case:?}: the update answers the domain's own ARN, got {updated}"
        );

        let after = described(&emulator, case, &domain_id).await;
        for (member, expected) in &row.expected {
            assert_eq!(
                after.get(*member),
                Some(expected),
                "case {case:?}: expected {member} to read {expected}, got {after}"
            );
        }
        // SageMaker applies the change after it has answered, so the domain reports the
        // non-terminal `Updating` straight after the call — the span is never zero — and the
        // clock owes it its way back into service.
        assert_eq!(
            after.get("Status"),
            Some(&json!("Updating")),
            "case {case:?}: an updated domain reports Updating until the clock settles it, \
             got {after}"
        );
        let settled = domain_resting_in(&emulator, case, &domain_id, "InService").await;
        for (member, expected) in &row.expected {
            assert_eq!(
                settled.get(*member),
                Some(expected),
                "case {case:?}: the settled domain still reads {member} as {expected}, got \
                 {settled}"
            );
        }
        // The domain's identity is not something an update can change.
        for member in ["DomainId", "DomainArn", "DomainName", "Url", "AuthMode"] {
            assert_eq!(
                settled.get(member),
                before.get(member),
                "case {case:?}: an update leaves {member} alone, got {settled}"
            );
        }
    }
}

/// RULE: an update that cannot be applied changes nothing and is refused with the failure
/// the condition calls for — `ResourceNotFound` for an id no domain of the account resolves,
/// a `ValidationException` for a network identifier no EC2 could have minted and for the one
/// `HomeEfsFileSystemCreation` change the member does not admit.
#[tokio::test]
async fn update_domain_refuses_what_it_cannot_apply_and_changes_nothing() {
    let emulator = Emulator::new();
    let domain_id = created_domain(&emulator, "refusals", "refusals").await;
    let before = described(&emulator, "refusals", &domain_id).await;

    // (case, the request, the expected error code, the expected message)
    let rows: Vec<(&str, Value, &str, String)> = vec![
        (
            "an id this account holds no domain under",
            json!({ "DomainId": "d-ffffffffffff" }),
            "ResourceNotFound",
            format!("Domain [{}] does not exist.", domain_arn("d-ffffffffffff")),
        ),
        (
            "a malformed VPC id",
            json!({ "DomainId": domain_id, "VpcId": "not-a-real-vpc-id" }),
            "ValidationException",
            "1 validation error detected: Value 'not-a-real-vpc-id' at 'vpcId' failed to \
             satisfy constraint: Member must satisfy regular expression pattern: \
             vpc-[a-z0-9]{8,}"
                .to_string(),
        ),
        (
            "a malformed subnet id",
            json!({ "DomainId": domain_id, "SubnetIds": ["subnet-1"] }),
            "ValidationException",
            "1 validation error detected: Value '[subnet-1]' at 'subnetIds' failed to \
             satisfy constraint: Member must satisfy constraint: [Member must have length \
             less than or equal to 32, Member must have length greater than or equal to 0, \
             Member must satisfy regular expression pattern: subnet-[a-z0-9]{8,}]"
                .to_string(),
        ),
        (
            // The create left the domain's file system Enabled (the documented default), and
            // that is the one change the member does not admit.
            "the EFS member from Enabled to Disabled",
            json!({ "DomainId": domain_id, "HomeEfsFileSystemCreation": "Disabled" }),
            "ValidationException",
            "HomeEfsFileSystemCreation cannot be changed from Enabled to Disabled.".to_string(),
        ),
    ];

    for (case, request, code, expected) in rows {
        let (status, refused) = emulator.call_in(REGION, "UpdateDomain", &request).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, code),
            "case {case:?}: expected a 400 {code}, got {status} with {refused}"
        );
        assert_eq!(
            message(&refused),
            expected,
            "case {case:?}: expected {expected:?}, got {:?}",
            message(&refused)
        );
        assert!(
            refused.get("DomainArn").is_none(),
            "case {case:?}: a refused update answers no ARN, got {refused}"
        );
        let after = described(&emulator, case, &domain_id).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused update changes nothing"
        );
    }
}

/// RULE: a domain the service is already tearing down takes no update — the teardown is in
/// flight, which is the in-use failure this operation models — and once the clock has
/// completed that teardown the id resolves to no domain at all.
#[tokio::test]
async fn update_domain_refuses_a_domain_on_its_way_out() {
    let emulator = Emulator::new();
    let case = "a domain being torn down";
    let domain_id = created_domain(&emulator, case, "tearing-down").await;

    let (status, deleted) = emulator
        .call_in(REGION, "DeleteDomain", &json!({ "DomainId": domain_id }))
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DeleteDomain must be served, got {status} with {deleted}"
    );

    // Which of the two refusals arrives depends only on whether the clock has completed the
    // teardown yet, so both are admitted here and the one after the poll below is exact.
    let request = json!({ "DomainId": domain_id });
    let (status, refused) = emulator.call_in(REGION, "UpdateDomain", &request).await;
    assert_eq!(
        status, 400,
        "case {case:?}: the update must be refused, got {status} with {refused}"
    );
    let expected = if error_code(&refused) == "ResourceInUse" {
        format!(
            "Domain [{}] cannot be updated while it is in Deleting status.",
            domain_arn(&domain_id)
        )
    } else {
        assert_eq!(
            error_code(&refused),
            "ResourceNotFound",
            "case {case:?}: the only other refusal is not-found, got {refused}"
        );
        format!("Domain [{}] does not exist.", domain_arn(&domain_id))
    };
    assert_eq!(
        message(&refused),
        expected,
        "case {case:?}: expected {expected:?}, got {:?}",
        message(&refused)
    );

    // Once the clock has completed the teardown the id resolves to nothing, and the update
    // answers the not-found refusal every operation whose subject is the domain words.
    let deadline = Instant::now() + TEARDOWN_DEADLINE;
    loop {
        let (status, read) = emulator
            .call_in(REGION, "DescribeDomain", &request)
            .await;
        if status == 400 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the domain must be gone once the clock completes the teardown, \
             last read {status} {read}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    let (status, gone) = emulator.call_in(REGION, "UpdateDomain", &request).await;
    assert_eq!(
        (status, error_code(&gone).as_str()),
        (400, "ResourceNotFound"),
        "case {case:?}: expected a 400 ResourceNotFound once the domain is gone, got \
         {status} with {gone}"
    );
    assert_eq!(
        message(&gone),
        format!("Domain [{}] does not exist.", domain_arn(&domain_id)),
        "case {case:?}: expected the not-found sentence, got {:?}",
        message(&gone)
    );
}
