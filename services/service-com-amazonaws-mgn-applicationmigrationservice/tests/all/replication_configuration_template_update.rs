// SPDX-License-Identifier: CC0-1.0

//! `UpdateReplicationConfigurationTemplate` over HTTP: the request's settings are written
//! onto the stored template, and the answer is the stored record.
//!
//! The rules under test:
//!
//! * An update is a PATCH of the template the `replicationConfigurationTemplateID`
//!   names: a setting the request carries replaces the stored one, a setting it omits
//!   keeps the value the template already holds, and the answer is the whole record —
//!   the same one `DescribeReplicationConfigurationTemplates` reports afterwards.
//!   `bandwidthThrottling` carries the model's `@default` of 0, so it is applied on every
//!   request.
//! * The template's `tags` are not a member of this request, so an update leaves them
//!   exactly as they were, and neither is the record's own `arn` moved by the `arn`
//!   member a request may carry.
//! * An id the account holds no template of is the modelled `ResourceNotFoundException`
//!   (404) and changes nothing, including an id another account holds a template under.
//! * Until `InitializeService` has run for the account the update is the modelled
//!   `UninitializedAccountException` (400) and changes nothing.
//! * `bandwidthThrottling` is accepted at both published bounds of its modelled range and
//!   refused past either of them, with the stored record untouched.
//!
//! The harness (`Mgn`, `error_code`, the region and account constants) is the one
//! `application_lifecycle.rs` defines.

use serde_json::{json, Map, Value};
use service_com_amazonaws_mgn_applicationmigrationservice::state::{
    replication_configuration_template_arn, ReplicationConfigurationTemplate,
};

use crate::application_lifecycle::{error_code, Mgn, EMULATOR_ACCOUNT, OTHER_ACCOUNT, REGION};

/// The create this suite patches: every member of the shape set, so each update row below
/// can be read as a change to a value the record already holds rather than as a first
/// write of it.
fn base_create() -> Value {
    json!({
        "stagingAreaSubnetId": "subnet-0123456789abcdef",
        "associateDefaultSecurityGroup": true,
        "replicationServersSecurityGroupsIDs": ["sg-00000000000000001"],
        "replicationServerInstanceType": "t3.small",
        "useDedicatedReplicationServer": false,
        "defaultLargeStagingDiskType": "GP2",
        "ebsEncryption": "DEFAULT",
        "bandwidthThrottling": 2_500,
        "dataPlaneRouting": "PRIVATE_IP",
        "createPublicIP": false,
        "stagingAreaTags": {"env": "dev"},
        "useFipsEndpoint": false,
        "tags": {"env": "dev", "team": "migration"},
        "internetProtocol": "IPV4",
        "storeSnapshotOnLocalZone": false,
    })
}

/// The class an `UpdateReplicationConfigurationTemplate` request spans, applied in order
/// to one template so every row also exercises "a member the request omits keeps the
/// value the template holds": how many members the request carries (none through all of
/// them), each modelled enum's values, both published ends of the `bandwidthThrottling`
/// range (0..10000) and a value between them, security-group lists from none through
/// several, and staging-area tag maps from none through several.
///
/// Sources for the value lists:
/// `smithy_query(query="shape",shape="ReplicationConfigurationDefaultLargeStagingDiskType")`
/// → `GP2`, `ST1`, `GP3`; `…shape="ReplicationConfigurationEbsEncryption"` → `DEFAULT`,
/// `CUSTOM`; `…shape="ReplicationConfigurationDataPlaneRouting"` → `PRIVATE_IP`,
/// `PUBLIC_IP`; `…shape="InternetProtocol"` → `IPV4`, `IPV6`.
fn update_cases() -> Vec<(&'static str, Value)> {
    vec![
        (
            "one setting only: the bandwidth throttling at the published upper bound",
            json!({"bandwidthThrottling": 10_000}),
        ),
        (
            "every optional member set: GP3 / CUSTOM with a key ARN / PUBLIC_IP / IPV6, \
             three security groups, new staging-area tags, an FSx ONTAP storage \
             configuration and a bandwidth throttling between the bounds",
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
                "bandwidthThrottling": 1_500,
                "dataPlaneRouting": "PUBLIC_IP",
                "createPublicIP": true,
                "stagingAreaTags": {"env": "prod", "wave": "1"},
                "useFipsEndpoint": true,
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
            "nothing but the id: every setting stays, and the bandwidth throttling falls \
             to the model's default of 0",
            json!({}),
        ),
        (
            "ST1 / PRIVATE_IP / IPV4 again, the security-group list and the staging-area \
             tags cleared, the bandwidth throttling at the published lower bound",
            json!({
                "replicationServersSecurityGroupsIDs": [],
                "defaultLargeStagingDiskType": "ST1",
                "dataPlaneRouting": "PRIVATE_IP",
                "internetProtocol": "IPV4",
                "stagingAreaTags": {},
                "bandwidthThrottling": 0,
            }),
        ),
        (
            "a new staging-area subnet with a longer hex body and a longer instance type",
            json!({
                "stagingAreaSubnetId": "subnet-0a1b2c3d4e5f60718",
                "replicationServerInstanceType": "r5n.24xlarge",
                "bandwidthThrottling": 1,
            }),
        ),
        (
            "one boolean toggled and one staging-area tag put back",
            json!({
                "useDedicatedReplicationServer": false,
                "stagingAreaTags": {"owner": "migration"},
                "bandwidthThrottling": 1,
            }),
        ),
        (
            "the EBS encryption back to DEFAULT while the key ARN the record holds stays",
            json!({"ebsEncryption": "DEFAULT", "bandwidthThrottling": 1}),
        ),
    ]
}

/// The record the service owes once `update` has been applied to `previous`: every member
/// the request carries replaces the stored one, every member it omits is preserved, and
/// `bandwidthThrottling` carries the model's default when the request names none. An empty
/// collection is reported absent, the way every other record of this store reports one.
///
/// The request's `arn` is not a setting of the record (the ARN is derived from the id the
/// service minted), and `tags` is not a member of the request at all, so both are taken
/// from `previous`.
fn owed_after(previous: &Value, update: &Value) -> Value {
    let mut owed: Map<String, Value> = previous
        .as_object()
        .expect("a stored record is a JSON object")
        .clone();
    for (member, value) in update
        .as_object()
        .expect("an update request is a JSON object")
    {
        if member == "replicationConfigurationTemplateID" || member == "arn" {
            continue;
        }
        let empty = match value {
            Value::Array(entries) => entries.is_empty(),
            Value::Object(entries) => entries.is_empty(),
            _ => false,
        };
        if empty {
            owed.remove(member);
        } else {
            owed.insert(member.clone(), value.clone());
        }
    }
    if !update
        .as_object()
        .is_some_and(|members| members.contains_key("bandwidthThrottling"))
    {
        owed.insert("bandwidthThrottling".to_string(), json!(0));
    }
    Value::Object(owed)
}

/// `request` with the template id added, as every update request carries it.
fn addressed(template_id: &str, request: &Value) -> Value {
    let mut body = request
        .as_object()
        .expect("an update request is a JSON object")
        .clone();
    body.insert(
        "replicationConfigurationTemplateID".to_string(),
        json!(template_id),
    );
    Value::Object(body)
}

/// A template created from [`base_create`], answering its id and the record the create
/// left.
async fn created_template(mgn: &Mgn) -> (String, Value) {
    let (status, created) = mgn
        .call("CreateReplicationConfigurationTemplate", &base_create())
        .await;
    assert_eq!(
        status, 201,
        "CreateReplicationConfigurationTemplate expected 201, got {status} with {created:?}"
    );
    let template_id = created["replicationConfigurationTemplateID"]
        .as_str()
        .unwrap_or_else(|| panic!("the create answers no id: {created:?}"))
        .to_string();
    (template_id, created)
}

/// The template `template_id` names, as `DescribeReplicationConfigurationTemplates`
/// reports it.
async fn read_back(mgn: &Mgn, case: &str, template_id: &str) -> Value {
    let (status, listed) = mgn
        .call(
            "DescribeReplicationConfigurationTemplates",
            &json!({"replicationConfigurationTemplateIDs": [template_id]}),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: reading {template_id:?} back expected 200, got {status} with {listed:?}"
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

/// An update writes the settings it carries, keeps the ones it omits, and answers the
/// stored record — which a later read reports unchanged.
#[tokio::test]
async fn an_update_patches_the_stored_settings_and_reads_back_the_same_record() {
    let mgn = Mgn::new();
    mgn.initialize().await;
    let (template_id, created) = created_template(&mgn).await;

    let mut held = created;
    for (case, request) in update_cases() {
        let owed = owed_after(&held, &request);
        let (status, answered) = mgn
            .call(
                "UpdateReplicationConfigurationTemplate",
                &addressed(&template_id, &request),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} with {answered:?}"
        );
        assert_eq!(
            answered, owed,
            "case {case:?}: the answer must be the patched record, got {answered:?}"
        );

        let reported = read_back(&mgn, case, &template_id).await;
        assert_eq!(
            reported, owed,
            "case {case:?}: a later read must report the same record, got {reported:?}"
        );
        held = reported;
    }

    // The tags the create supplied are not a member of the update request, so every one
    // of them survived the whole sequence.
    assert_eq!(
        held["tags"],
        base_create()["tags"],
        "the update carries no tags member, so the record's tags must be the ones the \
         create supplied, got {:?}",
        held["tags"]
    );
    // The account still holds exactly the one template, under its minted id: an update
    // patches a record rather than minting one.
    let ids = mgn
        .store()
        .lock()
        .replication_configuration_templates_of(EMULATOR_ACCOUNT)
        .iter()
        .map(|template| template.replication_configuration_template_id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        vec![template_id],
        "the account must still hold exactly the patched template, got {ids:?}"
    );
}

/// The `arn` member a request may carry is not a setting of the record: the template keeps
/// the ARN the service derived from the id it minted.
#[tokio::test]
async fn the_requests_arn_member_does_not_move_the_records_arn() {
    let mgn = Mgn::new();
    mgn.initialize().await;
    let (template_id, created) = created_template(&mgn).await;
    let owned_arn = json!(replication_configuration_template_arn(
        REGION,
        &template_id
    ));
    assert_eq!(
        created["arn"], owned_arn,
        "the create must answer the ARN derived from the minted id, got {:?}",
        created["arn"]
    );

    // (case, the arn the request carries)
    let rows = [
        (
            "another account's template ARN",
            format!("arn:aws:mgn:{REGION}:{OTHER_ACCOUNT}:replication-configuration-template/rct-0123456789abcdefg"),
        ),
        (
            "an ARN of another resource kind entirely",
            format!("arn:aws:mgn:{REGION}:{EMULATOR_ACCOUNT}:application/app-0123456789abcdefg"),
        ),
    ];
    for (case, arn) in rows {
        let (status, answered) = mgn
            .call(
                "UpdateReplicationConfigurationTemplate",
                &addressed(&template_id, &json!({"arn": arn, "bandwidthThrottling": 7})),
            )
            .await;
        assert_eq!(
            (status, &answered["arn"]),
            (200, &owned_arn),
            "case {case:?}: the record must keep its own ARN, got {status} with {answered:?}"
        );
        let reported = read_back(&mgn, case, &template_id).await;
        assert_eq!(
            reported["arn"], owned_arn,
            "case {case:?}: a later read must report the record's own ARN, got {reported:?}"
        );
    }
}

/// An id the account holds no template of is the modelled refusal and changes nothing —
/// including an id another account holds a template under, which is no more writable from
/// this account than it is readable.
#[tokio::test]
async fn an_id_the_account_holds_no_template_of_is_refused_and_changes_nothing() {
    let mgn = Mgn::new();
    mgn.initialize().await;
    let (template_id, created) = created_template(&mgn).await;

    // A template of ANOTHER account, a precondition no sequence of this caller's calls
    // can reach.
    let foreign_id = "rct-0000000000000000f";
    {
        let store = mgn.store();
        let mut state = store.lock();
        let stored =
            state.insert_replication_configuration_template(ReplicationConfigurationTemplate {
                arn: replication_configuration_template_arn(REGION, foreign_id),
                replication_configuration_template_id: foreign_id.to_string(),
                account_id: OTHER_ACCOUNT.to_string(),
                ..ReplicationConfigurationTemplate::default()
            });
        assert!(stored, "seeding {foreign_id:?} must store it");
    }

    // (case, the id the request addresses) — each one well formed against the modelled
    // `^rct-[0-9a-zA-Z]{17}$`, so the handler is what refuses.
    let rows = [
        ("an id nothing in the store holds", "rct-fffffffffffffffff"),
        ("an id in the pattern but all digits", "rct-00000000000000001"),
        ("a template of another account", foreign_id),
    ];
    for (case, addressed_id) in rows {
        let (status, error_type, answered) = mgn
            .call_reporting_error_type(
                "UpdateReplicationConfigurationTemplate",
                &addressed(addressed_id, &json!({"bandwidthThrottling": 42})),
            )
            .await;
        assert_eq!(
            (status, error_code(&answered)),
            (404, "ResourceNotFoundException"),
            "case {case:?}: expected 404 ResourceNotFoundException, got {status} with \
             {answered:?}"
        );
        assert_eq!(
            error_type.as_deref(),
            Some("ResourceNotFoundException"),
            "case {case:?}: expected the same code in x-amzn-errortype, got {error_type:?}"
        );
    }

    // The caller's own template is as the create left it, and the other account's record
    // is untouched.
    let reported = read_back(&mgn, "after the refusals", &template_id).await;
    assert_eq!(
        reported, created,
        "a refused update must change nothing, got {reported:?}"
    );
    let foreign_bandwidth = mgn
        .store()
        .lock()
        .replication_configuration_template_of(OTHER_ACCOUNT, foreign_id)
        .map(|template| template.bandwidth_throttling);
    assert_eq!(
        foreign_bandwidth,
        Some(0),
        "a refused update must leave the other account's record alone, got \
         {foreign_bandwidth:?}"
    );
}

/// Until `InitializeService` has run for the account, the update is the modelled refusal
/// and changes nothing — even for a template the store holds.
#[tokio::test]
async fn an_uninitialized_account_is_refused_and_changes_nothing() {
    let mgn = Mgn::new();
    let template_id = "rct-0000000000000000a";
    {
        let store = mgn.store();
        let mut state = store.lock();
        let stored =
            state.insert_replication_configuration_template(ReplicationConfigurationTemplate {
                arn: replication_configuration_template_arn(REGION, template_id),
                replication_configuration_template_id: template_id.to_string(),
                account_id: EMULATOR_ACCOUNT.to_string(),
                bandwidth_throttling: 2_500,
                ..ReplicationConfigurationTemplate::default()
            });
        assert!(stored, "seeding {template_id:?} must store it");
    }

    for (case, request) in update_cases() {
        let (status, answered) = mgn
            .call(
                "UpdateReplicationConfigurationTemplate",
                &addressed(template_id, &request),
            )
            .await;
        assert_eq!(
            (status, error_code(&answered)),
            (400, "UninitializedAccountException"),
            "case {case:?}: an uninitialized account must be refused, got {status} with \
             {answered:?}"
        );
    }

    let held = mgn
        .store()
        .lock()
        .replication_configuration_template_of(EMULATOR_ACCOUNT, template_id)
        .map(|template| template.bandwidth_throttling);
    assert_eq!(
        held,
        Some(2_500),
        "a refused update must write nothing, got {held:?}"
    );
}

/// `bandwidthThrottling` is served at each published end of its modelled range (0..10000)
/// and refused past either of them, and a refused request leaves the stored record exactly
/// as it was.
#[tokio::test]
async fn bandwidth_throttling_is_served_at_its_bounds_and_refused_past_them() {
    let mgn = Mgn::new();
    mgn.initialize().await;
    let (template_id, _) = created_template(&mgn).await;

    for (case, value) in [
        ("the published lower bound", 0_i64),
        ("the published upper bound", 10_000),
        ("a value between the bounds", 4_096),
    ] {
        let (status, answered) = mgn
            .call(
                "UpdateReplicationConfigurationTemplate",
                &addressed(&template_id, &json!({"bandwidthThrottling": value})),
            )
            .await;
        assert_eq!(
            (status, answered["bandwidthThrottling"].as_i64()),
            (200, Some(value)),
            "case {case:?}: expected 200 carrying {value}, got {status} with {answered:?}"
        );
    }

    // Past either bound the request never reaches the handler: the modelled range is
    // refused at the request boundary, and the record keeps the last served value.
    for (case, value) in [
        ("one past the upper bound", 10_001_i64),
        ("one below the lower bound", -1),
    ] {
        let (status, answered) = mgn
            .call(
                "UpdateReplicationConfigurationTemplate",
                &addressed(&template_id, &json!({"bandwidthThrottling": value})),
            )
            .await;
        assert_eq!(
            (status, error_code(&answered)),
            (400, "BadRequestException"),
            "case {case:?}: expected 400 BadRequestException, got {status} with {answered:?}"
        );
        let reported = read_back(&mgn, case, &template_id).await;
        assert_eq!(
            reported["bandwidthThrottling"].as_i64(),
            Some(4_096),
            "case {case:?}: a refused update must leave the stored value alone, got \
             {reported:?}"
        );
    }
}
