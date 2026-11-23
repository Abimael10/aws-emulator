// SPDX-License-Identifier: CC0-1.0

//! What a tenant and its reputation entity report for a member NOTHING has written,
//! end to end over HTTP.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with this
//! service registered — through the shared harness in the sibling
//! [`crate::configuration_sets`] suite, so routing, deserialization, the handler and
//! serialization are all exercised.
//!
//! The rule these cases pin: an optional member of a response reports what something
//! SET, so a preference or a status record no request and no service decision has
//! written is reported ABSENT rather than as the value its documented default resolves
//! to — a client can then tell "nobody has stated this" from "it was stated, and this
//! is the value". Three members of this surface follow it: a tenant's
//! `SuppressionAttributes` (stated by `CreateTenant` or `PutTenantSuppressionAttributes`,
//! absent until one of them states it) and a reputation entity's two status records
//! (the customer-managed one written by
//! `UpdateReputationEntityCustomerManagedStatus`, the Amazon-SES-managed one by Amazon
//! SES itself). The members that are always derivable are always reported: the
//! aggregate sending status, over the statuses in force whether a record holds them or
//! not, and the reputation impact, which is the highest impact among the account's
//! findings active against the entity and `NONE` while none is.
//!
//! Two identifiers the same calls report are pinned here too, because a client parses
//! them: a tenant's `TenantId`, which SES hands back as `tn-` followed by 30 lowercase
//! hexadecimal characters, and the resource type a tenant's resource listing reports,
//! which is the resource-type token the resource's own ARN carries.
//!
//! The tables below span what the rule generalises over: the form of the tenant name
//! (shortest allowed, every documented character class, the documented 64-character
//! bound), the Region a call executes in, which members a suppression write states
//! (neither, the scope alone, the reasons alone, both, and an empty structure), which
//! write stated them (the create or the put), every `SendingStatus` a caller may write,
//! each resource kind a tenant may hold, and each way a reputation management policy can
//! name no predefined policy.

use serde_json::{json, Value};

use crate::configuration_sets::{Reply, Ses};
use crate::contact_lists::query_escaped;
use service_com_amazonaws_sesv2_simpleemailservice_v2::state;

/// The one `ReputationEntityType` the model binds, taken from the crate's own constant
/// so the value is stated once
/// (`smithy_query(query="shape",shape="ReputationEntityType")`: `RESOURCE`).
const RESOURCE: &str = state::REPUTATION_ENTITY_TYPE_RESOURCE;

/// The Region a case signs for unless it is about regional scope.
const DEFAULT_REGION: &str = state::DEFAULT_REGION;

/// The one account this emulator serves, and so the account every ARN it mints carries.
const ACCOUNT_ID: &str = "000000000000";

/// The longest `TenantName` SES accepts, from the crate's own constant.
const MAX_TENANT_NAME_CHARS: usize = state::MAX_TENANT_NAME_CHARS;

/// Convenience calls for the surface these cases drive, over the shared harness.
trait Tenants {
    async fn create_tenant(&self, body: Value) -> Reply;
    async fn create_tenant_in(&self, region: &str, body: Value) -> Reply;
    async fn get_tenant(&self, name: &str) -> Reply;
    async fn put_suppression(&self, body: Value) -> Reply;
    async fn associate(&self, tenant_name: &str, resource_arn: &str) -> Reply;
    async fn list_tenant_resources(&self, body: Value) -> Reply;
    async fn get_entity(&self, reference: &str) -> Reply;
    async fn set_customer_managed_status(&self, reference: &str, status: &str) -> Reply;
    async fn set_policy(&self, reference: &str, policy: &str) -> Reply;
}

impl Tenants for Ses {
    async fn create_tenant(&self, body: Value) -> Reply {
        self.create_tenant_in(DEFAULT_REGION, body).await
    }

    async fn create_tenant_in(&self, region: &str, body: Value) -> Reply {
        self.call_in(region, "POST", "/v2/email/tenants", Some(body))
            .await
    }

    async fn get_tenant(&self, name: &str) -> Reply {
        self.call(
            "POST",
            "/v2/email/tenants/get",
            Some(json!({"TenantName": name})),
        )
        .await
    }

    async fn put_suppression(&self, body: Value) -> Reply {
        self.call("POST", "/v2/email/tenant/suppression", Some(body))
            .await
    }

    async fn associate(&self, tenant_name: &str, resource_arn: &str) -> Reply {
        self.call(
            "POST",
            "/v2/email/tenants/resources",
            Some(json!({"TenantName": tenant_name, "ResourceArn": resource_arn})),
        )
        .await
    }

    async fn list_tenant_resources(&self, body: Value) -> Reply {
        self.call("POST", "/v2/email/tenants/resources/list", Some(body))
            .await
    }

    async fn get_entity(&self, reference: &str) -> Reply {
        // `ReputationEntityReference` is a non-greedy `@httpLabel`, so an ARN reaches
        // the service percent-encoded, the way an SDK escapes a label.
        self.call(
            "GET",
            &format!(
                "/v2/email/reputation/entities/{RESOURCE}/{}",
                query_escaped(reference)
            ),
            None,
        )
        .await
    }

    async fn set_customer_managed_status(&self, reference: &str, status: &str) -> Reply {
        self.call(
            "PUT",
            &format!(
                "/v2/email/reputation/entities/{RESOURCE}/{}/customer-managed-status",
                query_escaped(reference)
            ),
            Some(json!({"SendingStatus": status})),
        )
        .await
    }

    async fn set_policy(&self, reference: &str, policy: &str) -> Reply {
        self.call(
            "PUT",
            &format!(
                "/v2/email/reputation/entities/{RESOURCE}/{}/policy",
                query_escaped(reference)
            ),
            Some(json!({"ReputationEntityPolicy": policy})),
        )
        .await
    }
}

/// Whether `tenant_id` is in the documented `TenantId` form: the prefix SES hands a
/// tenant's id back under, followed by exactly as many lowercase hexadecimal
/// characters as it does and nothing else.
fn is_a_tenant_id(tenant_id: &str) -> bool {
    let Some(hex) = tenant_id.strip_prefix(state::TENANT_ID_PREFIX) else {
        return false;
    };
    hex.len() == state::TENANT_ID_HEX_DIGITS
        && hex
            .chars()
            .all(|character| character.is_ascii_digit() || ('a'..='f').contains(&character))
}

#[tokio::test]
async fn a_minted_tenant_id_is_in_the_documented_form() {
    let longest = format!(
        "tenant-{}",
        "x".repeat(MAX_TENANT_NAME_CHARS - "tenant-".len())
    );
    // The rows differ along what the id cannot depend on: the form of the name it was
    // minted for and the Region the create executed in. Several tenants per row, so the
    // ids of one Region are checked against each other too.
    for (case, region, names) in [
        (
            "the shortest and longest names the model allows",
            DEFAULT_REGION,
            vec!["a".to_string(), longest.clone()],
        ),
        (
            "every documented character class, in another continent's Region",
            "eu-west-1",
            vec!["Tenant_01-beta".to_string(), "b".to_string()],
        ),
        (
            "several tenants created in one call sequence",
            "ap-southeast-2",
            (0..5).map(|index| format!("tenant-{index}")).collect(),
        ),
    ] {
        let ses = Ses::new();
        let mut minted = Vec::new();
        for name in &names {
            let created = ses
                .create_tenant_in(region, json!({"TenantName": name}))
                .await;
            assert_eq!(
                created.status, 200,
                "case {case:?}: expected HTTP 200 from CreateTenant for {name:?}, got {} {:?}",
                created.status,
                created.message()
            );
            let tenant_id = created
                .member("TenantId")
                .as_str()
                .unwrap_or_default()
                .to_string();
            assert!(
                is_a_tenant_id(&tenant_id),
                "case {case:?}: expected a TenantId of the form {:?} followed by {} \
                 lowercase hexadecimal characters, got {tenant_id:?}",
                state::TENANT_ID_PREFIX,
                state::TENANT_ID_HEX_DIGITS
            );
            assert!(
                !minted.contains(&tenant_id),
                "case {case:?}: every minted TenantId must be unique, but {tenant_id:?} \
                 was handed out twice"
            );

            // The id is minted once and read back off the record: the ARN the create
            // reports carries it, and so does a later read.
            assert_eq!(
                created.member("TenantArn").as_str().unwrap_or_default(),
                format!("arn:aws:ses:{region}:{ACCOUNT_ID}:tenant/{name}/{tenant_id}"),
                "case {case:?}: the tenant's ARN must carry the id it was minted with, \
                 got {:?}",
                created.member("TenantArn")
            );
            minted.push(tenant_id);
        }
    }
}

/// One accepted suppression write: what the request states, and what a read of the
/// tenant must report afterwards.
struct SuppressionCase {
    /// What this row varies, named in every assertion.
    case: &'static str,
    /// The `SuppressionAttributes` structure `CreateTenant` carries, when it carries
    /// one at all.
    stated: Option<Value>,
    /// The `SuppressionAttributes` a read must report, or `None` when it must report
    /// the member not at all.
    expected: Option<Value>,
}

/// The suppression table: one row per way a request can state (or not state) the
/// preferences, including a structure naming neither member and each scope the model
/// publishes (`smithy_query(query="shape",shape="SuppressionListScope")`: `ACCOUNT`,
/// `TENANT`).
fn suppression_cases() -> Vec<SuppressionCase> {
    vec![
        SuppressionCase {
            case: "no suppression attributes at all",
            stated: None,
            expected: None,
        },
        SuppressionCase {
            case: "a structure naming neither member",
            stated: Some(json!({})),
            expected: Some(json!({"SuppressionScope": "ACCOUNT"})),
        },
        SuppressionCase {
            case: "the account scope stated explicitly",
            stated: Some(json!({"SuppressionScope": "ACCOUNT"})),
            expected: Some(json!({"SuppressionScope": "ACCOUNT"})),
        },
        SuppressionCase {
            case: "the tenant's own list, with no reason named",
            stated: Some(json!({"SuppressionScope": "TENANT"})),
            expected: Some(json!({"SuppressionScope": "TENANT"})),
        },
        SuppressionCase {
            case: "reasons without a scope, which takes the documented default",
            stated: Some(json!({"SuppressedReasons": ["BOUNCE"]})),
            expected: Some(json!({
                "SuppressionScope": "ACCOUNT",
                "SuppressedReasons": ["BOUNCE"],
            })),
        },
        SuppressionCase {
            case: "both members, with every reason in the order supplied",
            stated: Some(json!({
                "SuppressionScope": "TENANT",
                "SuppressedReasons": ["COMPLAINT", "BOUNCE"],
            })),
            expected: Some(json!({
                "SuppressionScope": "TENANT",
                "SuppressedReasons": ["COMPLAINT", "BOUNCE"],
            })),
        },
    ]
}

#[tokio::test]
async fn a_tenant_reports_only_the_suppression_preferences_a_request_stated() {
    for case in suppression_cases() {
        // A fresh service per row, so the row is answered from what it stated and
        // nothing else.
        let ses = Ses::new();
        let mut body = json!({"TenantName": "stated"});
        if let Some(stated) = &case.stated {
            body["SuppressionAttributes"] = stated.clone();
        }
        let created = ses.create_tenant(body).await;
        assert_eq!(
            created.status, 200,
            "case {:?}: expected HTTP 200 from CreateTenant, got {} {:?}",
            case.case,
            created.status,
            created.message()
        );
        assert_eq!(
            created.pointer("/SuppressionAttributes"),
            case.expected,
            "case {:?}: expected CreateTenant to report {:?}",
            case.case,
            case.expected
        );

        // The read answers the same record: `Tenant` carries the member under the same
        // name, so a create and a read of one tenant cannot disagree about it.
        let read = ses.get_tenant("stated").await;
        assert_eq!(
            read.pointer("/Tenant/SuppressionAttributes"),
            case.expected,
            "case {:?}: expected GetTenant to report {:?}, got {:?}",
            case.case,
            case.expected,
            read.member("Tenant")
        );

        // And the other write of this pair states the same preferences: a tenant
        // created with none holds the ones a put states, whatever it stated at create.
        let mut put = json!({"TenantName": "stated"});
        if let Some(stated) = &case.stated {
            if let Some(reasons) = stated.get("SuppressedReasons") {
                put["SuppressedReasons"] = reasons.clone();
            }
            if let Some(scope) = stated.get("SuppressionScope") {
                put["SuppressionScope"] = scope.clone();
            }
        }
        let written = ses.put_suppression(put).await;
        assert_eq!(
            written.status, 200,
            "case {:?}: expected HTTP 200 from PutTenantSuppressionAttributes, got {} {:?}",
            case.case,
            written.status,
            written.message()
        );
        // A put STATES the preferences, so the tenant holds them even where the create
        // stated nothing: the scope it named no value for takes the documented default.
        let after_put = case
            .expected
            .clone()
            .unwrap_or_else(|| json!({"SuppressionScope": "ACCOUNT"}));
        let read = ses.get_tenant("stated").await;
        assert_eq!(
            read.pointer("/Tenant/SuppressionAttributes"),
            Some(after_put.clone()),
            "case {:?}: expected the put to leave {after_put:?}, got {:?}",
            case.case,
            read.member("Tenant")
        );
    }
}

#[tokio::test]
async fn a_reputation_entity_reports_only_the_status_records_something_wrote() {
    // One row per `SendingStatus` a caller may write
    // (`smithy_query(query="shape",shape="SendingStatus")`: `ENABLED`, `DISABLED`,
    // `REINSTATED`), each with the aggregate the statuses in force then imply; the
    // tenant names span the documented form.
    for (case, name, written, aggregate) in [
        (
            "a tenant whose sending nobody has decided anything about",
            "a".to_string(),
            None,
            "ENABLED",
        ),
        (
            "sending prevented for the tenant",
            "Tenant_01-beta".to_string(),
            Some("DISABLED"),
            "DISABLED",
        ),
        (
            "sending allowed even with active findings",
            "reinstated_tenant".to_string(),
            Some("REINSTATED"),
            "REINSTATED",
        ),
        (
            "sending allowed, which is the status in force anyway",
            format!("t{}", "x".repeat(MAX_TENANT_NAME_CHARS - 1)),
            Some("ENABLED"),
            "ENABLED",
        ),
    ] {
        let ses = Ses::new();
        let created = ses.create_tenant(json!({"TenantName": name})).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected HTTP 200 from CreateTenant, got {} {:?}",
            created.status,
            created.message()
        );
        let reference = created
            .member("TenantArn")
            .as_str()
            .unwrap_or_default()
            .to_string();

        // Before any write: neither record exists, so neither is reported, while the
        // members the service derives are.
        let entity = ses.get_entity(&reference).await;
        assert_eq!(
            entity.status, 200,
            "case {case:?}: expected HTTP 200 from GetReputationEntity, got {} {:?}",
            entity.status,
            entity.message()
        );
        for record in ["CustomerManagedStatus", "AwsSesManagedStatus"] {
            assert_eq!(
                entity.pointer(&format!("/ReputationEntity/{record}")),
                None,
                "case {case:?}: nothing has written {record}, so it must not be \
                 reported, got {:?}",
                entity.member("ReputationEntity")
            );
        }
        assert_eq!(
            entity.pointer("/ReputationEntity/ReputationImpact"),
            Some(json!(state::REPUTATION_IMPACT_NONE)),
            "case {case:?}: no finding is active against the entity, so its impact \
             level is reported as none, got {:?}",
            entity.member("ReputationEntity")
        );
        assert_eq!(
            entity.pointer("/ReputationEntity/SendingStatusAggregate"),
            Some(json!("ENABLED")),
            "case {case:?}: the statuses in force on a fresh entity permit sending, \
             got {:?}",
            entity.member("ReputationEntity")
        );

        if let Some(status) = written {
            let update = ses.set_customer_managed_status(&reference, status).await;
            assert_eq!(
                update.status, 200,
                "case {case:?}: expected HTTP 200 writing the customer-managed status, \
                 got {} {:?}",
                update.status,
                update.message()
            );

            let entity = ses.get_entity(&reference).await;
            assert_eq!(
                entity.pointer("/ReputationEntity/CustomerManagedStatus/Status"),
                Some(json!(status)),
                "case {case:?}: the record must report the status that was written, \
                 got {:?}",
                entity.member("ReputationEntity")
            );
            // `StatusRecord$Cause` is "a description of the reason for the current
            // status, such as ... manual status updates": this write is the manual one.
            assert_eq!(
                entity.pointer("/ReputationEntity/CustomerManagedStatus/Cause"),
                Some(json!(state::CUSTOMER_MANAGED_STATUS_CAUSE)),
                "case {case:?}: a status the account set itself states that as its \
                 cause, got {:?}",
                entity.member("ReputationEntity")
            );
            assert!(
                entity
                    .pointer("/ReputationEntity/CustomerManagedStatus/LastUpdatedTimestamp")
                    .and_then(|value| value.as_f64())
                    .is_some(),
                "case {case:?}: the record must report when it was written, got {:?}",
                entity.member("ReputationEntity")
            );
            // "When you update the customer-managed status, the Amazon Web Services
            // Amazon SES-managed status remains unchanged" — and unwritten.
            assert_eq!(
                entity.pointer("/ReputationEntity/AwsSesManagedStatus"),
                None,
                "case {case:?}: the write must leave Amazon SES's own record unwritten, \
                 got {:?}",
                entity.member("ReputationEntity")
            );
            assert_eq!(
                entity.pointer("/ReputationEntity/SendingStatusAggregate"),
                Some(json!(aggregate)),
                "case {case:?}: the aggregate must follow the statuses in force, got {:?}",
                entity.member("ReputationEntity")
            );
            assert_eq!(
                entity.pointer("/ReputationEntity/ReputationImpact"),
                Some(json!(state::REPUTATION_IMPACT_NONE)),
                "case {case:?}: a status write raises no finding, so the impact level \
                 stays none, got {:?}",
                entity.member("ReputationEntity")
            );

            // Reporting only: reading the entity changed nothing a later caller sees.
            let again = ses.get_entity(&reference).await;
            assert_eq!(
                again.member("ReputationEntity"),
                entity.member("ReputationEntity"),
                "case {case:?}: two reads of one entity must agree, got {:?} then {:?}",
                entity.member("ReputationEntity"),
                again.member("ReputationEntity")
            );
        }
    }
}

#[tokio::test]
async fn a_tenants_resources_are_reported_under_the_token_their_arn_names() {
    let ses = Ses::new();
    let created = ses.create_tenant(json!({"TenantName": "holder"})).await;
    assert_eq!(
        created.status, 200,
        "case {:?}: expected HTTP 200 from CreateTenant, got {} {:?}",
        "holder",
        created.status,
        created.message()
    );

    // One row per kind a tenant may hold, as (the ARN of a resource of that kind, the
    // token the listing reports it under, the modelled `ResourceType` the REQUEST names
    // that kind by). The token is the resource-type token the ARN itself carries, which
    // is what SES is recorded serving, and the modelled value stays what a filter
    // names: both are the pairs `state::TENANT_RESOURCE_TYPES` holds.
    let kinds: Vec<(String, &str, &str)> = vec![
        (
            format!("arn:aws:ses:us-east-1:{ACCOUNT_ID}:identity/sender@example.com"),
            "identity",
            "EMAIL_IDENTITY",
        ),
        (
            format!("arn:aws:ses:us-east-1:{ACCOUNT_ID}:configuration-set/nightly"),
            "configuration-set",
            "CONFIGURATION_SET",
        ),
        (
            format!("arn:aws:ses:us-east-1:{ACCOUNT_ID}:template/welcome"),
            "template",
            "EMAIL_TEMPLATE",
        ),
    ];
    for (resource_arn, _, _) in &kinds {
        let associated = ses.associate("holder", resource_arn).await;
        assert_eq!(
            associated.status, 200,
            "case {resource_arn:?}: expected HTTP 200 from \
             CreateTenantResourceAssociation, got {} {:?}",
            associated.status,
            associated.message()
        );
    }

    let listed = ses
        .list_tenant_resources(json!({"TenantName": "holder"}))
        .await;
    assert_eq!(
        listed.status, 200,
        "case {:?}: expected HTTP 200 from ListTenantResources, got {} {:?}",
        "an unfiltered listing",
        listed.status,
        listed.message()
    );
    let rows = listed
        .member("TenantResources")
        .as_array()
        .cloned()
        .unwrap_or_default();
    for (resource_arn, token, modelled) in &kinds {
        let row = rows
            .iter()
            .find(|row| row.get("ResourceArn") == Some(&json!(resource_arn)))
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            row.get("ResourceType"),
            Some(&json!(token)),
            "case {resource_arn:?}: expected the resource to be listed as {token:?}, \
             got {row:?}"
        );

        // The filter is the request side, where a kind is named by the modelled
        // `ResourceType`: it keeps exactly the resources of that kind.
        let filtered = ses
            .list_tenant_resources(json!({
                "TenantName": "holder",
                "Filter": {"RESOURCE_TYPE": modelled},
            }))
            .await;
        let kept: Vec<Value> = filtered
            .member("TenantResources")
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            kept,
            vec![row.clone()],
            "case {resource_arn:?}: a listing filtered on {modelled:?} reports exactly \
             the resources of that kind, got {kept:?}"
        );
    }
}

#[tokio::test]
async fn a_reputation_policy_naming_no_predefined_policy_is_refused() {
    let ses = Ses::new();
    let created = ses.create_tenant(json!({"TenantName": "policy"})).await;
    assert_eq!(
        created.status, 200,
        "case {:?}: expected HTTP 200 from CreateTenant, got {} {:?}",
        "policy",
        created.status,
        created.message()
    );
    let reference = created
        .member("TenantArn")
        .as_str()
        .unwrap_or_default()
        .to_string();
    let in_force = ses
        .get_entity(&reference)
        .await
        .pointer("/ReputationEntity/ReputationManagementPolicy");

    // One row per way a value can name no Amazon-SES-managed policy: a policy name that
    // is no ARN, an ARN naming a policy that does not exist, one of another vendor, one
    // of the caller's own account, and an empty-handed value. Every one is refused with
    // AWS's fixed sentence, which carries no part of the request.
    for (case, policy) in [
        ("a bare policy name", "standard".to_string()),
        (
            "an ARN naming a policy that does not exist",
            "arn:aws:ses:us-east-1:aws:reputation-policy/not-a-real-policy".to_string(),
        ),
        (
            "an ARN of the caller's own account",
            format!("arn:aws:ses:us-east-1:{ACCOUNT_ID}:reputation-policy/standard"),
        ),
        (
            "another vendor's resource",
            format!("arn:aws:sns:us-east-1:{ACCOUNT_ID}:alerts-topic"),
        ),
        ("a value that is no ARN at all", "-".to_string()),
    ] {
        let refused = ses.set_policy(&reference, &policy).await;
        assert_eq!(
            (refused.status, refused.error_code().as_str()),
            (400, "BadRequestException"),
            "case {case:?}: expected 400 BadRequestException, got {} {:?}",
            refused.status,
            refused.error_code()
        );
        assert_eq!(
            refused.message(),
            state::INVALID_RESOURCE_IDENTIFIER,
            "case {case:?}: expected the refusal to read {:?}, got {:?}",
            state::INVALID_RESOURCE_IDENTIFIER,
            refused.message()
        );

        // A refused write applies nothing: the policy in force is the one it was.
        assert_eq!(
            ses.get_entity(&reference)
                .await
                .pointer("/ReputationEntity/ReputationManagementPolicy"),
            in_force,
            "case {case:?}: a refused write must leave the policy in force as it was"
        );
    }

    // Every predefined policy is accepted, so the refusal is about the value naming
    // none of them rather than about the write.
    for policy_name in state::REPUTATION_MANAGEMENT_POLICIES {
        let policy = state::reputation_management_policy_arn(DEFAULT_REGION, policy_name);
        let applied = ses.set_policy(&reference, &policy).await;
        assert_eq!(
            applied.status, 200,
            "case {policy_name:?}: expected HTTP 200 applying a predefined policy, got \
             {} {:?}",
            applied.status,
            applied.message()
        );
        assert_eq!(
            ses.get_entity(&reference)
                .await
                .pointer("/ReputationEntity/ReputationManagementPolicy"),
            Some(json!(policy)),
            "case {policy_name:?}: the entity must report the policy just applied"
        );
    }
}
