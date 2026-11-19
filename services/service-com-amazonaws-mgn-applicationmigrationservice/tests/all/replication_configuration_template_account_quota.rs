// SPDX-License-Identifier: CC0-1.0

//! The per-account quota `CreateReplicationConfigurationTemplate` enforces, end to end
//! over HTTP.
//!
//! The rule under test: the service admits ONE replication configuration template per
//! account, so a create for an account that already holds one is refused with
//! `ServiceQuotaExceededException` (HTTP 402) and the quota's fixed sentence, and stores
//! nothing — whatever the request carries and however it compares to the template the
//! account already holds. The quota counts what the account HOLDS, so removing its
//! template frees the place again, and another account's templates never count toward
//! it. The account gate stays ahead of the quota: an uninitialized account is told so
//! even when the store already holds a template of it.
//!
//! Every assertion goes through the router's `dispatch`, so it exercises the same
//! restJson1 deserialize → handle → serialize path a client drives. The store is reached
//! directly only to seed a precondition no sequence of calls of this account can reach
//! (another account's template) and to check that a refusal stored nothing.
//!
//! The harness (`Mgn`, `error_code`, the region and account constants) is the one
//! `application_lifecycle.rs` defines.

use serde_json::{json, Value};
use service_com_amazonaws_mgn_applicationmigrationservice::state::ReplicationConfigurationTemplate;

use crate::application_lifecycle::{error_code, Mgn, EMULATOR_ACCOUNT, OTHER_ACCOUNT, REGION};

/// The operation under test, at its modelled `http.uri`
/// (`smithy_query(query="operation",operation="CreateReplicationConfigurationTemplate")`:
/// `POST /CreateReplicationConfigurationTemplate`, code 201).
const CREATE: &str = "CreateReplicationConfigurationTemplate";

/// `DeleteReplicationConfigurationTemplate`, likewise (code 204).
const DELETE: &str = "DeleteReplicationConfigurationTemplate";

/// `DescribeReplicationConfigurationTemplates`, likewise (code 200).
const DESCRIBE: &str = "DescribeReplicationConfigurationTemplates";

/// The status `com.amazonaws.mgn#ServiceQuotaExceededException` carries
/// (`smithy_query(query="shape",shape="ServiceQuotaExceededException")`: `httpError`
/// 402).
const QUOTA_EXCEEDED_STATUS: u16 = 402;

/// The error shape the quota refusal names, in both channels.
const QUOTA_EXCEEDED_CODE: &str = "ServiceQuotaExceededException";

/// How the service words the per-account template quota: fixed text, carrying no id, no
/// account and no limit.
const QUOTA_EXCEEDED_MESSAGE: &str = "Max Replication Configuration Templates per Account exceeded";

/// The `resourceType` a replication-configuration-template refusal names: the modelled
/// shape `com.amazonaws.mgn#ReplicationConfigurationTemplate`.
const TEMPLATE_RESOURCE_TYPE: &str = "ReplicationConfigurationTemplate";

/// The service's `serviceCode`, as its model records it
/// (`smithy_query(query="service_info")` → `awsService.arnNamespace`).
const SERVICE_CODE: &str = "mgn";

/// A `CreateReplicationConfigurationTemplate` body carrying only the members the model
/// marks `@required`, under `subnet`.
fn minimal_request(subnet: &str) -> Value {
    json!({
        "stagingAreaSubnetId": subnet,
        "associateDefaultSecurityGroup": true,
        "replicationServersSecurityGroupsIDs": [],
        "replicationServerInstanceType": "t3.small",
        "useDedicatedReplicationServer": false,
        "defaultLargeStagingDiskType": "GP3",
        "ebsEncryption": "DEFAULT",
        "bandwidthThrottling": 0,
        "dataPlaneRouting": "PRIVATE_IP",
        "createPublicIP": false,
        "stagingAreaTags": {},
    })
}

/// A body carrying every optional member too, so the second request can differ from the
/// first in every axis the create generalises over rather than only in a value.
///
/// Sources for the value lists:
/// `smithy_query(query="shape",shape="ReplicationConfigurationDefaultLargeStagingDiskType")`
/// → `GP2`, `GP3`, `ST1`; `…shape="ReplicationConfigurationEbsEncryption"` → `DEFAULT`,
/// `CUSTOM`; `…shape="ReplicationConfigurationDataPlaneRouting"` → `PRIVATE_IP`,
/// `PUBLIC_IP`; `…shape="InternetProtocol"` → `IPV4`, `IPV6`; the `bandwidthThrottling`
/// range 0..10000 is the member's own `@range`.
fn full_request(subnet: &str) -> Value {
    json!({
        "stagingAreaSubnetId": subnet,
        "associateDefaultSecurityGroup": false,
        "replicationServersSecurityGroupsIDs": [
            "sg-00000000000000001",
            "sg-00000000000000002",
        ],
        "replicationServerInstanceType": "m5.large",
        "useDedicatedReplicationServer": true,
        "defaultLargeStagingDiskType": "ST1",
        "ebsEncryption": "CUSTOM",
        "ebsEncryptionKeyArn": format!(
            "arn:aws:kms:{REGION}:{EMULATOR_ACCOUNT}:key/11111111-2222-3333-4444-555555555555"
        ),
        "bandwidthThrottling": 10_000,
        "dataPlaneRouting": "PUBLIC_IP",
        "createPublicIP": true,
        "stagingAreaTags": {"env": "prod", "wave": "1"},
        "useFipsEndpoint": true,
        "tags": {"team": "migration"},
        "internetProtocol": "IPV6",
        "storeSnapshotOnLocalZone": true,
        "storageConfiguration": {
            "storageType": "FSX_ONTAP",
            "fsxOntapConfiguration": {
                "storageVirtualMachineId": "svm-0123456789abcdef0",
                "credentialsSecretArn": format!(
                    "arn:aws:secretsmanager:{REGION}:{EMULATOR_ACCOUNT}:secret:ontap"
                ),
            },
        },
    })
}

/// A body with none of the optional members and every enum at its other admitted value,
/// so a row exists whose members differ from both of the above.
fn spare_request(subnet: &str) -> Value {
    json!({
        "stagingAreaSubnetId": subnet,
        "associateDefaultSecurityGroup": true,
        "replicationServersSecurityGroupsIDs": ["sg-0000000000000000a"],
        "replicationServerInstanceType": "",
        "useDedicatedReplicationServer": false,
        "defaultLargeStagingDiskType": "GP2",
        "ebsEncryption": "DEFAULT",
        "bandwidthThrottling": 1_500,
        "dataPlaneRouting": "PRIVATE_IP",
        "createPublicIP": true,
        "stagingAreaTags": {"owner": "migration"},
        "internetProtocol": "IPV4",
    })
}

/// The class the quota refusal spans: which template the account already holds, and how
/// the request that meets the quota compares to it — identical to it (the recorded case),
/// the same settings under another subnet, a request carrying every optional member where
/// the first carried none, a request carrying none where the first carried all, and a
/// request differing in each modelled enum's value.
fn quota_cases() -> Vec<(&'static str, Value, Value)> {
    vec![
        (
            "the identical request a second time",
            minimal_request("subnet-0123456789abcdef"),
            minimal_request("subnet-0123456789abcdef"),
        ),
        (
            "the same settings under a different staging-area subnet",
            minimal_request("subnet-0123456789abcdef"),
            minimal_request("subnet-ffffffffffffffff"),
        ),
        (
            "a request carrying every optional member where the first carried none",
            minimal_request("subnet-00000000"),
            full_request("subnet-abcdef0123456789"),
        ),
        (
            "a request carrying no optional member where the first carried all",
            full_request("subnet-abcdef0123456789"),
            minimal_request("subnet-00000000"),
        ),
        (
            "a request differing in every modelled enum value",
            full_request("subnet-0a1b2c3d4e5f60718"),
            spare_request("subnet-0a1b2c3d4e5f60718"),
        ),
    ]
}

/// `CreateReplicationConfigurationTemplate`, asserting the modelled 201 and answering the
/// minted `replicationConfigurationTemplateID`.
async fn create(mgn: &Mgn, case: &str, request: &Value) -> String {
    let (status, created) = mgn.call(CREATE, request).await;
    assert_eq!(
        status, 201,
        "case {case:?}: {CREATE} expected 201, got {status} with {created:?}"
    );
    created["replicationConfigurationTemplateID"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: no replicationConfigurationTemplateID in {created:?}")
        })
        .to_string()
}

/// The ids `DescribeReplicationConfigurationTemplates` reports for the caller's account.
async fn described_ids(mgn: &Mgn, case: &str) -> Vec<String> {
    let (status, listed) = mgn.call(DESCRIBE, &json!({})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {DESCRIBE} expected 200, got {status} with {listed:?}"
    );
    listed["items"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: no items array in {listed:?}"))
        .iter()
        .map(|item| {
            item["replicationConfigurationTemplateID"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: no id in {item:?}"))
                .to_string()
        })
        .collect()
}

/// Assert `body` is the per-account quota refusal: the modelled status, the error shape
/// in both channels, the fixed sentence and the members that say which quota was met.
fn assert_quota_exceeded(case: &str, status: u16, error_type: Option<&str>, body: &Value) {
    assert_eq!(
        status, QUOTA_EXCEEDED_STATUS,
        "case {case:?}: a create meeting the account quota expected {QUOTA_EXCEEDED_STATUS}, \
         got {status} with {body:?}"
    );
    assert_eq!(
        error_code(body),
        QUOTA_EXCEEDED_CODE,
        "case {case:?}: the refusal must name {QUOTA_EXCEEDED_CODE}, got {body:?}"
    );
    assert_eq!(
        error_type,
        Some(QUOTA_EXCEEDED_CODE),
        "case {case:?}: the refusal's x-amzn-errortype must state the shape, got {error_type:?}"
    );
    assert_eq!(
        body["message"].as_str(),
        Some(QUOTA_EXCEEDED_MESSAGE),
        "case {case:?}: the refusal must carry the quota's fixed sentence, got {body:?}"
    );
    assert_eq!(
        body["resourceType"].as_str(),
        Some(TEMPLATE_RESOURCE_TYPE),
        "case {case:?}: the refusal must name the resource kind, got {body:?}"
    );
    assert_eq!(
        body["serviceCode"].as_str(),
        Some(SERVICE_CODE),
        "case {case:?}: the refusal must name the service code, got {body:?}"
    );
    assert_eq!(
        body["quotaValue"].as_i64(),
        Some(1),
        "case {case:?}: the refusal must name the quota that was met, got {body:?}"
    );
}

/// A create for an account that already holds a template meets the quota, whatever the
/// request carries, and stores nothing.
#[tokio::test]
async fn a_second_template_in_one_account_meets_the_quota_and_stores_nothing() {
    for (case, first, second) in quota_cases() {
        let mgn = Mgn::new();
        mgn.initialize().await;
        let held = create(&mgn, case, &first).await;

        let (status, error_type, body) = mgn.call_reporting_error_type(CREATE, &second).await;
        assert_quota_exceeded(case, status, error_type.as_deref(), &body);

        // The refusal stored nothing: the account still holds exactly the template it
        // held before the call, in the store and through the reader.
        let reported = described_ids(&mgn, case).await;
        assert_eq!(
            reported,
            vec![held.clone()],
            "case {case:?}: a refused create must leave the account holding only its first \
             template, got {reported:?}"
        );
        let stored = mgn
            .store()
            .lock()
            .replication_configuration_templates_of(EMULATOR_ACCOUNT)
            .iter()
            .map(|template| template.replication_configuration_template_id.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            stored,
            vec![held.clone()],
            "case {case:?}: the store must hold only the first template, got {stored:?}"
        );

        // A THIRD attempt is refused the same way: the refusal is the account's state,
        // not a one-off answer to the second call.
        let (status, error_type, body) = mgn.call_reporting_error_type(CREATE, &first).await;
        assert_quota_exceeded(
            &format!("{case} (repeated once more)"),
            status,
            error_type.as_deref(),
            &body,
        );
    }
}

/// The quota counts what the account HOLDS: removing its template frees the place, so the
/// next create serves and mints an id of its own. Driven round three times, since the
/// quota is a count rather than a one-time allowance.
#[tokio::test]
async fn deleting_the_account_s_template_frees_its_place_again() {
    let mgn = Mgn::new();
    mgn.initialize().await;

    let mut minted: Vec<String> = Vec::new();
    for round in 0..3u32 {
        let case = format!("round {round}");
        let request = match round {
            0 => minimal_request("subnet-0123456789abcdef"),
            1 => full_request("subnet-abcdef0123456789"),
            _ => spare_request("subnet-00000000"),
        };
        let template_id = create(&mgn, &case, &request).await;
        assert!(
            !minted.contains(&template_id),
            "case {case:?}: {template_id:?} was already minted for an earlier round; got \
             {minted:?}"
        );
        minted.push(template_id.clone());

        // While it is held, the account's place is taken.
        let (status, error_type, body) = mgn.call_reporting_error_type(CREATE, &request).await;
        assert_quota_exceeded(&case, status, error_type.as_deref(), &body);

        let (status, _, body) = mgn
            .call_reporting_error_type(
                DELETE,
                &json!({"replicationConfigurationTemplateID": &template_id}),
            )
            .await;
        assert_eq!(
            status, 204,
            "case {case:?}: {DELETE} expected 204, got {status} with {body:?}"
        );
    }

    let reported = described_ids(&mgn, "after the last delete").await;
    assert!(
        reported.is_empty(),
        "every template was deleted, so the account must hold none, got {reported:?}"
    );
}

/// The quota is the account's own: a template another account holds does not take this
/// account's place, and this account's own template does not take the other's.
#[tokio::test]
async fn another_account_s_template_does_not_count_toward_this_account_s_quota() {
    let mgn = Mgn::new();
    mgn.initialize().await;

    // Another account's template: nothing a call of this account can create, since the
    // operation carries no `accountID` member, so it is seeded.
    let foreign = "rct-aaaaaaaaaaaaaaaaa";
    {
        let store = mgn.store();
        let mut guard = store.lock();
        let stored =
            guard.insert_replication_configuration_template(ReplicationConfigurationTemplate {
                replication_configuration_template_id: foreign.to_string(),
                arn: format!(
                    "arn:aws:mgn:{REGION}:{OTHER_ACCOUNT}:replication-configuration-template/{foreign}"
                ),
                account_id: OTHER_ACCOUNT.to_string(),
                ..ReplicationConfigurationTemplate::default()
            });
        assert!(stored, "seeding another account's template must store it");
    }

    let own = create(
        &mgn,
        "with another account already holding one",
        &minimal_request("subnet-0123456789abcdef"),
    )
    .await;
    assert_ne!(
        own, foreign,
        "the create must mint a template of its own, not report the other account's"
    );

    // Now this account's own place is taken, while the other account keeps its record.
    let (status, error_type, body) = mgn
        .call_reporting_error_type(CREATE, &minimal_request("subnet-0123456789abcdef"))
        .await;
    assert_quota_exceeded(
        "the account's second template",
        status,
        error_type.as_deref(),
        &body,
    );

    let store = mgn.store();
    let guard = store.lock();
    let others = guard
        .replication_configuration_templates_of(OTHER_ACCOUNT)
        .iter()
        .map(|template| template.replication_configuration_template_id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        others,
        vec![foreign.to_string()],
        "the other account must still hold exactly its own template, got {others:?}"
    );
    let ours = guard
        .replication_configuration_templates_of(EMULATOR_ACCOUNT)
        .iter()
        .map(|template| template.replication_configuration_template_id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        ours,
        vec![own],
        "this account must hold exactly its own template, got {ours:?}"
    );
}

/// The account gate stays ahead of the quota: an uninitialized account is told it is
/// uninitialized, even when the store already holds a template of it.
#[tokio::test]
async fn an_uninitialized_account_is_refused_ahead_of_the_quota() {
    let mgn = Mgn::new();
    let held = "rct-11111111111111111";
    {
        let store = mgn.store();
        let mut guard = store.lock();
        let stored =
            guard.insert_replication_configuration_template(ReplicationConfigurationTemplate {
                replication_configuration_template_id: held.to_string(),
                arn: format!(
                    "arn:aws:mgn:{REGION}:{EMULATOR_ACCOUNT}:replication-configuration-template/{held}"
                ),
                account_id: EMULATOR_ACCOUNT.to_string(),
                ..ReplicationConfigurationTemplate::default()
            });
        assert!(stored, "seeding this account's template must store it");
    }

    for (case, request) in [
        (
            "the required members only",
            minimal_request("subnet-00000000"),
        ),
        ("every member", full_request("subnet-abcdef0123456789")),
    ] {
        let (status, body) = mgn.call(CREATE, &request).await;
        assert_eq!(
            (status, error_code(&body)),
            (400, "UninitializedAccountException"),
            "case {case:?}: an uninitialized account must be refused ahead of the quota, got \
             {status} with {body:?}"
        );
    }

    // Once the account is initialized the SAME request meets the quota instead: the gate
    // was the only thing ahead of it.
    mgn.initialize().await;
    let (status, error_type, body) = mgn
        .call_reporting_error_type(CREATE, &minimal_request("subnet-00000000"))
        .await;
    assert_quota_exceeded("once initialized", status, error_type.as_deref(), &body);
}
