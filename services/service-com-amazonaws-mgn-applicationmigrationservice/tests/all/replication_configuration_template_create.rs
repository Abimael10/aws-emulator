// SPDX-License-Identifier: CC0-1.0

//! `CreateReplicationConfigurationTemplate` over HTTP: the request's replication
//! settings become a stored template, and the answer is that stored record.
//!
//! The rules under test:
//!
//! * The operation mints a `replicationConfigurationTemplateID` in the modelled shape
//!   and the template's ARN, stores every replication setting the request supplied, and
//!   answers the stored record — which `DescribeReplicationConfigurationTemplates`
//!   reports unchanged afterwards.
//! * The operation is not `@idempotent` and models no idempotency token, so a repeat is
//!   not replayed: an account holds one template at a time, and the create that would be
//!   its second meets the quota (`replication_configuration_template_account_quota.rs`
//!   pins that rule).
//! * Until `InitializeService` has run for the account, the operation is the modelled
//!   `UninitializedAccountException` and stores nothing.
//! * Past the emulator's own store bound — a backstop behind the account quota, over
//!   every account — the operation is the modelled `ValidationException` and stores
//!   nothing.
//!
//! The harness (`Mgn`, `error_code`, the region and account constants) is the one
//! `application_lifecycle.rs` defines.

use serde_json::{json, Map, Value};
use service_com_amazonaws_mgn_applicationmigrationservice::state::{
    replication_configuration_template_arn, ReplicationConfigurationTemplate,
    REPLICATION_CONFIGURATION_TEMPLATE_QUOTA,
};

use crate::application_lifecycle::{error_code, Mgn, EMULATOR_ACCOUNT, REGION};

/// The class a `CreateReplicationConfigurationTemplate` request spans: which optional
/// settings it carries (none through all of them), each modelled enum's values, both
/// published ends of the `bandwidthThrottling` range (0..10000) and a value between
/// them, security-group lists from none through several, and tag maps from none through
/// several.
///
/// Sources for the value lists:
/// `smithy_query(query="shape",shape="ReplicationConfigurationDefaultLargeStagingDiskType")`
/// → `GP2`, `ST1`, `GP3`; `…shape="ReplicationConfigurationEbsEncryption"` → `DEFAULT`,
/// `CUSTOM`; `…shape="ReplicationConfigurationDataPlaneRouting"` → `PRIVATE_IP`,
/// `PUBLIC_IP`; `…shape="InternetProtocol"` → `IPV4`, `IPV6`. `ST1` and `IPV6` are
/// values the issue's recorded request never named.
fn creation_cases() -> Vec<(&'static str, Value)> {
    vec![
        (
            "only the required members: one security group, no staging-area tags, \
             bandwidth throttling at the lower bound",
            json!({
                "stagingAreaSubnetId": "subnet-0123456789abcdef",
                "associateDefaultSecurityGroup": true,
                "replicationServersSecurityGroupsIDs": ["sg-00000000000000001"],
                "replicationServerInstanceType": "t3.small",
                "useDedicatedReplicationServer": false,
                "defaultLargeStagingDiskType": "GP2",
                "ebsEncryption": "DEFAULT",
                "bandwidthThrottling": 0,
                "dataPlaneRouting": "PRIVATE_IP",
                "createPublicIP": false,
                "stagingAreaTags": {},
            }),
        ),
        (
            "every optional member set: GP3 / CUSTOM with a key ARN / PUBLIC_IP / IPV6, \
             three security groups, both tag maps, an FSx ONTAP storage configuration \
             and bandwidth throttling at the upper bound",
            json!({
                "stagingAreaSubnetId": "subnet-ABCDEF0123456789",
                "associateDefaultSecurityGroup": false,
                "replicationServersSecurityGroupsIDs": [
                    "sg-0000000000000000a",
                    "sg-0000000000000000b",
                    "sg-0000000000000000c",
                ],
                "replicationServerInstanceType": "m5.large",
                "useDedicatedReplicationServer": true,
                "defaultLargeStagingDiskType": "GP3",
                "ebsEncryption": "CUSTOM",
                "ebsEncryptionKeyArn": format!(
                    "arn:aws:kms:{REGION}:{EMULATOR_ACCOUNT}:key/11111111-2222-3333-4444-555555555555"
                ),
                "bandwidthThrottling": 10_000,
                "dataPlaneRouting": "PUBLIC_IP",
                "createPublicIP": true,
                "stagingAreaTags": {"env": "prod", "wave": "1"},
                "useFipsEndpoint": true,
                "tags": {"env": "prod", "team": "migration"},
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
            }),
        ),
        (
            "ST1 disk type, IPV4, an empty security-group list, one staging-area tag, \
             a bandwidth throttling between the bounds",
            json!({
                "stagingAreaSubnetId": "subnet-00000000",
                "associateDefaultSecurityGroup": true,
                "replicationServersSecurityGroupsIDs": [],
                "replicationServerInstanceType": "",
                "useDedicatedReplicationServer": false,
                "defaultLargeStagingDiskType": "ST1",
                "ebsEncryption": "DEFAULT",
                "bandwidthThrottling": 1_500,
                "dataPlaneRouting": "PRIVATE_IP",
                "createPublicIP": true,
                "stagingAreaTags": {"owner": "migration"},
                "internetProtocol": "IPV4",
                "storeSnapshotOnLocalZone": false,
                "useFipsEndpoint": false,
            }),
        ),
        (
            "a long instance type and a staging-area subnet with a longer hex body, \
             no tags at all, bandwidth throttling of one",
            json!({
                "stagingAreaSubnetId": "subnet-0a1b2c3d4e5f60718",
                "associateDefaultSecurityGroup": false,
                "replicationServersSecurityGroupsIDs": [
                    "sg-00000000000000001",
                    "sg-00000000000000002",
                ],
                "replicationServerInstanceType": "r5n.24xlarge",
                "useDedicatedReplicationServer": true,
                "defaultLargeStagingDiskType": "GP2",
                "ebsEncryption": "CUSTOM",
                "ebsEncryptionKeyArn": format!(
                    "arn:aws:kms:{REGION}:{EMULATOR_ACCOUNT}:key/abcdefab-1111-2222-3333-abcdefabcdef"
                ),
                "bandwidthThrottling": 1,
                "dataPlaneRouting": "PUBLIC_IP",
                "createPublicIP": false,
                "stagingAreaTags": {},
            }),
        ),
    ]
}

/// The record the service owes for `request`, once it has minted `template_id`: every
/// member the request supplied, plus the id and the ARN. An empty collection is reported
/// absent, the way every other record of this store reports one.
fn expected_record(request: &Value, template_id: &str) -> Value {
    let supplied = request
        .as_object()
        .expect("a creation request is a JSON object");
    let mut owed = Map::new();
    owed.insert(
        "replicationConfigurationTemplateID".to_string(),
        json!(template_id),
    );
    owed.insert(
        "arn".to_string(),
        json!(replication_configuration_template_arn(REGION, template_id)),
    );
    for (member, value) in supplied {
        let empty = match value {
            Value::Array(entries) => entries.is_empty(),
            Value::Object(entries) => entries.is_empty(),
            _ => false,
        };
        if !empty {
            owed.insert(member.clone(), value.clone());
        }
    }
    Value::Object(owed)
}

/// The `replicationConfigurationTemplateID` of a create answer, checked against the
/// modelled `^rct-[0-9a-zA-Z]{17}$` (length 21..21) before it is used as a key.
fn minted_id(case: &str, created: &Value) -> String {
    let id = created["replicationConfigurationTemplateID"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the answer carries no id: {created:?}"))
        .to_string();
    let body = id.strip_prefix("rct-").unwrap_or_else(|| {
        panic!("case {case:?}: expected an id prefixed 'rct-', got {id:?}")
    });
    assert!(
        body.len() == 17 && body.chars().all(|c| c.is_ascii_alphanumeric()),
        "case {case:?}: expected 17 alphanumeric characters after 'rct-', got {id:?}"
    );
    id
}

/// The template `template_id` names, as
/// `DescribeReplicationConfigurationTemplates` reports it.
async fn read_back(mgn: &Mgn, case: &str, template_id: &str) -> Value {
    let (status, listed) = mgn
        .call(
            "DescribeReplicationConfigurationTemplates",
            &json!({"replicationConfigurationTemplateIDs": [template_id]}),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: reading {template_id:?} back expected 200, got {status} with \
         {listed:?}"
    );
    let items = listed["items"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no items array in {listed:?}"));
    assert_eq!(
        items.len(),
        1,
        "case {case:?}: reading {template_id:?} back must report exactly one template, got \
         {listed:?}"
    );
    items[0].clone()
}

/// A create stores the supplied settings and answers the stored record, which a later
/// read reports unchanged.
#[tokio::test]
async fn a_create_stores_the_supplied_settings_and_reads_back_the_same_record() {
    for (case, request) in creation_cases() {
        let mgn = Mgn::new();
        mgn.initialize().await;

        let (status, created) = mgn
            .call("CreateReplicationConfigurationTemplate", &request)
            .await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected 201, got {status} with {created:?}"
        );
        let template_id = minted_id(case, &created);
        let owed = expected_record(&request, &template_id);
        assert_eq!(
            created, owed,
            "case {case:?}: the answer must be the stored record, got {created:?}"
        );

        let reported = read_back(&mgn, case, &template_id).await;
        assert_eq!(
            reported, owed,
            "case {case:?}: a later read must report the same record, got {reported:?}"
        );

        // The record is the store's, not the answer's: the state holds it under the
        // minted id, in the account the request was signed for.
        let held = mgn
            .store()
            .lock()
            .replication_configuration_templates_of(EMULATOR_ACCOUNT)
            .iter()
            .map(|template| template.replication_configuration_template_id.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            held,
            vec![template_id.clone()],
            "case {case:?}: the account must hold exactly the created template, got {held:?}"
        );
    }
}

/// The operation is not idempotent and replays nothing: each call that serves mints a
/// template of its own, and since an account holds one at a time the next call serves
/// only once the held one is removed. The ids are distinct, and the account lists the one
/// it holds.
#[tokio::test]
async fn every_call_that_serves_mints_a_template_of_its_own() {
    let mgn = Mgn::new();
    mgn.initialize().await;

    let mut minted: Vec<String> = Vec::new();
    for (case, request) in creation_cases() {
        let (status, created) = mgn
            .call("CreateReplicationConfigurationTemplate", &request)
            .await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected 201, got {status} with {created:?}"
        );
        let template_id = minted_id(case, &created);
        assert!(
            !minted.contains(&template_id),
            "case {case:?}: {template_id:?} was already minted for an earlier call; got \
             {minted:?}"
        );
        minted.push(template_id.clone());

        let (status, listed) = mgn
            .call("DescribeReplicationConfigurationTemplates", &json!({}))
            .await;
        let reported: Vec<String> = listed["items"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|item| {
                item["replicationConfigurationTemplateID"]
                    .as_str()
                    .unwrap_or("<absent>")
                    .to_string()
            })
            .collect();
        assert_eq!(
            (status, reported),
            (200, vec![template_id.clone()]),
            "case {case:?}: the account must list the template it holds, got {listed:?}"
        );

        // The account's one place is freed again, so the next row's call serves.
        let (status, _, body) = mgn
            .call_reporting_error_type(
                "DeleteReplicationConfigurationTemplate",
                &json!({"replicationConfigurationTemplateID": &template_id}),
            )
            .await;
        assert_eq!(
            status, 204,
            "case {case:?}: the delete expected 204, got {status} with {body:?}"
        );
    }
}

/// Until `InitializeService` has run for the account, the create is the modelled
/// refusal and stores nothing.
#[tokio::test]
async fn an_uninitialized_account_is_refused_and_stores_nothing() {
    for (case, request) in creation_cases() {
        let mgn = Mgn::new();
        let (status, answer) = mgn
            .call("CreateReplicationConfigurationTemplate", &request)
            .await;
        assert_eq!(
            (status, error_code(&answer)),
            (400, "UninitializedAccountException"),
            "case {case:?}: an uninitialized account must be refused, got {status} with \
             {answer:?}"
        );
        let held = mgn
            .store()
            .lock()
            .replication_configuration_templates_of(EMULATOR_ACCOUNT)
            .len();
        assert_eq!(
            held, 0,
            "case {case:?}: a refused create must store nothing, got {held} templates"
        );
    }
}

/// Past the store bound the create is the modelled `ValidationException` and stores
/// nothing: the emulator refuses rather than growing its heap without end.
///
/// The bound is over every account and sits BEHIND the per-account quota, so reaching it
/// takes one template for each of as many accounts as the bound itself — a precondition
/// no sequence of calls of one account can reach, hence the seeding. The caller's own
/// account holds none, so the account quota is not what refuses here.
#[tokio::test]
async fn a_full_store_is_refused_and_stores_nothing() {
    let mgn = Mgn::new();
    mgn.initialize().await;

    {
        let store = mgn.store();
        let mut state = store.lock();
        for index in 0..REPLICATION_CONFIGURATION_TEMPLATE_QUOTA {
            let template_id = format!("rct-{index:017x}");
            // One template per account, the most the service admits an account, so the
            // store fills without any account holding two.
            let account = format!("{:012}", index + 1);
            let stored = state.insert_replication_configuration_template(
                ReplicationConfigurationTemplate {
                    arn: replication_configuration_template_arn(REGION, &template_id),
                    replication_configuration_template_id: template_id.clone(),
                    account_id: account,
                    ..ReplicationConfigurationTemplate::default()
                },
            );
            assert!(stored, "seeding {template_id:?} must store it");
        }
    }

    let (case, request) = creation_cases()[0].clone();
    let (status, answer) = mgn
        .call("CreateReplicationConfigurationTemplate", &request)
        .await;
    assert_eq!(
        (status, error_code(&answer)),
        (400, "ValidationException"),
        "case {case:?}: a full store must be refused, got {status} with {answer:?}"
    );
    let held = mgn
        .store()
        .lock()
        .replication_configuration_templates_of(EMULATOR_ACCOUNT)
        .len();
    assert_eq!(
        held, 0,
        "case {case:?}: a refused create must store nothing, got {held} templates"
    );
}
