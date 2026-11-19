// SPDX-License-Identifier: CC0-1.0

//! `DeleteReplicationConfigurationTemplate`, end to end over HTTP.
//!
//! The rule these tests pin: a delete removes the one replication configuration template
//! the caller's account holds under the named id in a single state mutation and concludes
//! with the modelled no-content answer, so a later read no longer reports it while every
//! other template of the account is untouched; an id the account holds no template of
//! (never created, already deleted, or a template of another account) removes nothing and
//! is refused with the modelled 404 `ResourceNotFoundException`; and because the removal
//! is one mutation, callers racing to delete the same id resolve to exactly one
//! no-content answer with that same refusal for each of the others.
//!
//! The reader behind the delete is `DescribeReplicationConfigurationTemplates`: selecting
//! an id that matches no template of the account is an empty `items` list with 200, never
//! an error, so a removed id reads back as nothing rather than as a failure.
//!
//! Every assertion goes through the router's `dispatch`, so it exercises the same
//! restJson1 deserialize → handle → serialize path a client drives, and the templates are
//! created through `CreateReplicationConfigurationTemplate`. The store is reached directly
//! only to seed a precondition no sequence of calls can reach (another account's template,
//! or a second template of the caller's own account, which the service's per-account quota
//! admits only one of) and to check that a removal left no record behind, the way the other
//! mgn suites do.
//!
//! The harness (`Mgn`, `error_code`, the region and account constants) is the one
//! `application_lifecycle.rs` defines.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_mgn_applicationmigrationservice::state::ReplicationConfigurationTemplate;

use crate::application_lifecycle::{error_code, Mgn, EMULATOR_ACCOUNT, OTHER_ACCOUNT, REGION};

/// The operation under test, at its modelled `http.uri`
/// (`smithy_query(query="operation",operation="DeleteReplicationConfigurationTemplate")`:
/// `POST /DeleteReplicationConfigurationTemplate`, code 204), without the leading slash
/// the harness adds.
const DELETE: &str = "DeleteReplicationConfigurationTemplate";

/// `CreateReplicationConfigurationTemplate`, likewise (code 201).
const CREATE: &str = "CreateReplicationConfigurationTemplate";

/// `DescribeReplicationConfigurationTemplates`, likewise (code 200).
const DESCRIBE: &str = "DescribeReplicationConfigurationTemplates";

/// The status the delete's modelled `@http(code: 204)` answer carries.
const NO_CONTENT: u16 = 204;

/// The `resourceType` a replication-configuration-template refusal names: the modelled
/// shape `com.amazonaws.mgn#ReplicationConfigurationTemplate`.
const TEMPLATE_RESOURCE_TYPE: &str = "ReplicationConfigurationTemplate";

/// The refusal a caller naming an id the account holds no template of must read.
fn not_found_message(template_id: &str) -> String {
    format!("Replication Configuration Template with Id: '{template_id}' was not found")
}

/// A `CreateReplicationConfigurationTemplate` body carrying only the members the model
/// marks `@required`
/// (`smithy_query(query="operation",operation="CreateReplicationConfigurationTemplate")`),
/// with `subnet` as the staging-area subnet so one account can hold several distinct
/// templates.
fn minimal_request(subnet: &str) -> Value {
    json!({
        "stagingAreaSubnetId": subnet,
        "associateDefaultSecurityGroup": true,
        "replicationServersSecurityGroupsIDs": [],
        "replicationServerInstanceType": "t3.small",
        "useDedicatedReplicationServer": false,
        "defaultLargeStagingDiskType": "GP2",
        "ebsEncryption": "DEFAULT",
        "bandwidthThrottling": 0,
        "dataPlaneRouting": "PRIVATE_IP",
        "createPublicIP": false,
        "stagingAreaTags": {},
    })
}

/// The class a deletable template spans, one row per axis the rule generalises over: how
/// many of the optional members the record carries, which of each modelled enum's values
/// it was created under, both ends of the `bandwidthThrottling` range, how many
/// replication-server security groups it names, and whether it carries tags, staging-area
/// tags or a storage configuration. A delete must remove each one the same way — the
/// recorded create-then-delete race is ONE row.
///
/// Sources for the value lists:
/// `smithy_query(query="shape",shape="ReplicationConfigurationDefaultLargeStagingDiskType")`
/// → `GP2`, `GP3`, `ST1`; `…shape="ReplicationConfigurationEbsEncryption"` → `DEFAULT`,
/// `CUSTOM`; `…shape="ReplicationConfigurationDataPlaneRouting"` → `PRIVATE_IP`,
/// `PUBLIC_IP`; `…shape="InternetProtocol"` → `IPV4`, `IPV6`; the `bandwidthThrottling`
/// range 0..10000 is the member's own `@range`.
fn create_cases() -> Vec<(&'static str, Value)> {
    vec![
        (
            "the required members only, bandwidth throttling at the lower bound",
            minimal_request("subnet-0123456789abcdef"),
        ),
        (
            "GP3 / CUSTOM / PUBLIC_IP / IPV6, every optional member, bandwidth throttling \
             at the upper bound",
            json!({
                "stagingAreaSubnetId": "subnet-abcdef0123456789",
                "associateDefaultSecurityGroup": false,
                "replicationServersSecurityGroupsIDs": [
                    "sg-00000000000000001",
                    "sg-00000000000000002",
                    "sg-00000000000000003",
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
                "stagingAreaTags": {"env": "prod"},
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
            "ST1 / IPV4 with one security group and a mid-range bandwidth throttling",
            json!({
                "stagingAreaSubnetId": "subnet-00000000aaaaaaaa",
                "associateDefaultSecurityGroup": true,
                "replicationServersSecurityGroupsIDs": ["sg-0000000000000000a"],
                "replicationServerInstanceType": "c5.xlarge",
                "useDedicatedReplicationServer": false,
                "defaultLargeStagingDiskType": "ST1",
                "ebsEncryption": "DEFAULT",
                "bandwidthThrottling": 1_500,
                "dataPlaneRouting": "PRIVATE_IP",
                "createPublicIP": false,
                "stagingAreaTags": {"owner": "migration", "wave": "1"},
                "internetProtocol": "IPV4",
            }),
        ),
        (
            "tags only, under the longest subnet id the modelled pattern admits here",
            json!({
                "stagingAreaSubnetId": "subnet-0123456789abcdef0123456789abcdef",
                "associateDefaultSecurityGroup": false,
                "replicationServersSecurityGroupsIDs": [],
                "replicationServerInstanceType": "t3.medium",
                "useDedicatedReplicationServer": true,
                "defaultLargeStagingDiskType": "GP2",
                "ebsEncryption": "DEFAULT",
                "bandwidthThrottling": 9_999,
                "dataPlaneRouting": "PUBLIC_IP",
                "createPublicIP": true,
                "stagingAreaTags": {},
                "tags": {"env": "dev"},
            }),
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

/// One `DeleteReplicationConfigurationTemplate` call, answering the status, the
/// `x-amzn-errortype` header and the parsed body.
async fn delete(mgn: &Mgn, template_id: &str) -> (u16, Option<String>, Value) {
    mgn.call_reporting_error_type(
        DELETE,
        &json!({"replicationConfigurationTemplateID": template_id}),
    )
    .await
}

/// The ids `DescribeReplicationConfigurationTemplates` reports for `request`, asserting the
/// modelled 200 — the reader a removed id must read back as nothing through.
async fn described_ids(mgn: &Mgn, case: &str, request: &Value) -> Vec<String> {
    let (status, listed) = mgn.call(DESCRIBE, request).await;
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

/// Assert `body` is this service's not-found refusal naming `template_id`: the modelled
/// status, the error shape in both channels, the worded message and the members that say
/// which resource was missing.
fn assert_template_not_found(
    case: &str,
    operation: &str,
    status: u16,
    error_type: Option<&str>,
    body: &Value,
    template_id: &str,
) {
    assert_eq!(
        status, 404,
        "case {case:?}: {operation} on an id the account holds no template of expected 404, \
         got {status} with {body:?}"
    );
    assert_eq!(
        error_code(body),
        "ResourceNotFoundException",
        "case {case:?}: {operation} must refuse with ResourceNotFoundException, got {body:?}"
    );
    assert_eq!(
        error_type,
        Some("ResourceNotFoundException"),
        "case {case:?}: the refusal's x-amzn-errortype must state the shape, got {error_type:?}"
    );
    assert_eq!(
        body["message"].as_str(),
        Some(not_found_message(template_id).as_str()),
        "case {case:?}: {operation} must word the refusal for the id asked for, got {body:?}"
    );
    assert_eq!(
        body["resourceId"].as_str(),
        Some(template_id),
        "case {case:?}: the refusal must name the id asked for, got {body:?}"
    );
    assert_eq!(
        body["resourceType"].as_str(),
        Some(TEMPLATE_RESOURCE_TYPE),
        "case {case:?}: the refusal must name the resource kind, got {body:?}"
    );
}

/// A delete removes the named template, concludes with the modelled no-content answer, and
/// leaves the id reading back as nothing — for every shape of template the create admits.
#[tokio::test]
async fn a_delete_removes_the_named_template_and_concludes_with_no_content() {
    for (case, request) in create_cases() {
        let mgn = Mgn::new();
        mgn.initialize().await;
        let template_id = create(&mgn, case, &request).await;
        // A second template of the same account, so the removal is shown to take only the
        // record it named. The service admits one template per account, so no sequence of
        // calls of this account can create it: it is seeded, the way another account's
        // template is below.
        let survivor = "rct-fffffffffffffffff".to_string();
        {
            let store = mgn.store();
            let mut guard = store.lock();
            let stored = guard.insert_replication_configuration_template(
                ReplicationConfigurationTemplate {
                    replication_configuration_template_id: survivor.clone(),
                    arn: format!(
                        "arn:aws:mgn:{REGION}:{EMULATOR_ACCOUNT}:replication-configuration-template/{survivor}"
                    ),
                    account_id: EMULATOR_ACCOUNT.to_string(),
                    ..ReplicationConfigurationTemplate::default()
                },
            );
            assert!(
                stored,
                "case {case:?}: seeding the account's second template must store it"
            );
        }

        // The delete itself: the modelled 204, with no entity at all.
        let (status, _, raw) = mgn
            .call_http_raw(
                "POST",
                &format!("/{DELETE}"),
                Some(&json!({"replicationConfigurationTemplateID": template_id})),
            )
            .await;
        assert_eq!(
            status,
            NO_CONTENT,
            "case {case:?}: {DELETE} expected {NO_CONTENT}, got {status} with {:?}",
            String::from_utf8_lossy(&raw)
        );
        assert!(
            raw.is_empty(),
            "case {case:?}: a {NO_CONTENT} answer carries no body, got {:?}",
            String::from_utf8_lossy(&raw)
        );

        // The removed id reads back as nothing: an id filter matching no template of the
        // account is an empty list with 200, not a refusal.
        let selected = described_ids(
            &mgn,
            case,
            &json!({"replicationConfigurationTemplateIDs": [&template_id]}),
        )
        .await;
        assert!(
            selected.is_empty(),
            "case {case:?}: selecting the removed id must report no template, got {selected:?}"
        );

        // Every other template of the account is untouched.
        let remaining = described_ids(&mgn, case, &json!({})).await;
        assert_eq!(
            remaining,
            vec![survivor.clone()],
            "case {case:?}: only the template the delete did not name may remain, got \
             {remaining:?}"
        );

        // A repeat of the delete removes nothing and is refused: the record is gone, so
        // the id names nothing.
        let (status, error_type, body) = delete(&mgn, &template_id).await;
        assert_template_not_found(
            case,
            "the repeated delete",
            status,
            error_type.as_deref(),
            &body,
            &template_id,
        );

        // In the store: no record and no place in the creation order is left behind.
        {
            let store = mgn.store();
            let guard = store.lock();
            assert!(
                guard
                    .replication_configuration_template_of(EMULATOR_ACCOUNT, &template_id)
                    .is_none(),
                "case {case:?}: the removed template must leave the store"
            );
            assert!(
                !guard
                    .replication_configuration_template_order
                    .contains(&template_id),
                "case {case:?}: the removed template must leave the creation order too, got \
                 {:?}",
                guard.replication_configuration_template_order
            );
            assert!(
                guard
                    .replication_configuration_template_of(EMULATOR_ACCOUNT, &survivor)
                    .is_some(),
                "case {case:?}: the other template must still be held"
            );
        }
    }
}

/// An id the account holds no template of removes nothing and is refused with the modelled
/// 404 — for each way an id can name nothing: never created, well-formed but unknown, and a
/// template another account holds.
#[tokio::test]
async fn an_id_the_account_holds_no_template_of_is_refused_and_removes_nothing() {
    let mgn = Mgn::new();
    mgn.initialize().await;
    let own = create(
        &mgn,
        "the account's own template",
        &minimal_request("subnet-0123456789abcdef"),
    )
    .await;

    // Another account's template: nothing a call of this account can create, so it is
    // seeded.
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

    // Each id is inside the modelled `^rct-[0-9a-zA-Z]{17}$` (length 21..21), so the
    // refusal comes from the handler rather than from the request boundary.
    let cases = [
        ("a well-formed id no account holds", "rct-zzzzzzzzzzzzzzzzz"),
        ("an id of all zeroes", "rct-00000000000000000"),
        ("an id mixing cases and digits", "rct-A1b2C3d4E5f6G7h8I"),
        ("a template of another account", foreign),
    ];
    for (case, template_id) in cases {
        let (status, error_type, body) = delete(&mgn, template_id).await;
        assert_template_not_found(
            case,
            DELETE,
            status,
            error_type.as_deref(),
            &body,
            template_id,
        );
    }

    // Nothing was removed: the other account still holds its record, and this account
    // still holds its own.
    {
        let store = mgn.store();
        let guard = store.lock();
        assert!(
            guard
                .replication_configuration_template_of(OTHER_ACCOUNT, foreign)
                .is_some(),
            "a refused delete must leave the other account's template in place"
        );
    }
    let remaining = described_ids(&mgn, "after the refusals", &json!({})).await;
    assert_eq!(
        remaining,
        vec![own],
        "the account's own template must be untouched by the refusals, got {remaining:?}"
    );
}

/// Until `InitializeService` has run for the account the delete is the modelled
/// `UninitializedAccountException`, ahead of the per-resource lookup: the account gate is
/// answered both for an id the store does hold and for one it does not, and the refusal
/// removes nothing.
#[tokio::test]
async fn an_uninitialized_account_is_refused_before_the_lookup() {
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

    for (case, template_id) in [
        ("an id this account's store does hold", held),
        ("an id nothing holds", "rct-22222222222222222"),
    ] {
        let (status, _, body) = delete(&mgn, template_id).await;
        assert_eq!(
            (status, error_code(&body)),
            (400, "UninitializedAccountException"),
            "case {case:?}: an uninitialized account must be refused, got {status} with {body:?}"
        );
    }

    // The refusal removed nothing: once the account is initialized the same delete serves.
    mgn.initialize().await;
    let (status, _, body) = delete(&mgn, held).await;
    assert_eq!(
        status, NO_CONTENT,
        "the refused delete must have left the template in place, got {status} with {body:?}"
    );
}

/// Callers racing to delete the SAME template resolve to exactly one no-content answer and
/// the modelled not-found refusal for each of the others — for 2, 3 and 5 concurrent
/// callers. The recorded two-worker race is ONE row.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_deletes_of_one_template_resolve_to_one_success_and_not_found() {
    for callers in [2usize, 3, 5] {
        let case = format!("{callers} concurrent deletes");
        let mgn = Arc::new(Mgn::new());
        mgn.initialize().await;
        let template_id = create(&mgn, &case, &minimal_request("subnet-0123456789abcdef")).await;

        let mut racing = Vec::new();
        for _ in 0..callers {
            let mgn = Arc::clone(&mgn);
            let template_id = template_id.clone();
            racing.push(tokio::spawn(async move {
                delete(&mgn, &template_id).await
            }));
        }
        let mut outcomes = Vec::new();
        for caller in racing {
            outcomes.push(caller.await.expect("each racing delete answers"));
        }

        let succeeded = outcomes
            .iter()
            .filter(|(status, _, _)| *status == NO_CONTENT)
            .count();
        assert_eq!(
            succeeded,
            1,
            "case {case:?}: exactly one racing delete may remove the template, got {:?}",
            outcomes
                .iter()
                .map(|(status, _, body)| (*status, error_code(body).to_string()))
                .collect::<Vec<_>>()
        );
        for (status, error_type, body) in &outcomes {
            if *status == NO_CONTENT {
                assert_eq!(
                    body,
                    &Value::Null,
                    "case {case:?}: the winning delete carries no body, got {body:?}"
                );
                continue;
            }
            assert_template_not_found(
                &case,
                "a losing racing delete",
                *status,
                error_type.as_deref(),
                body,
                &template_id,
            );
        }

        // After the race resolves the id reads back as nothing, with 200.
        let selected = described_ids(
            &mgn,
            &case,
            &json!({"replicationConfigurationTemplateIDs": [&template_id]}),
        )
        .await;
        assert!(
            selected.is_empty(),
            "case {case:?}: the raced id must report no template afterwards, got {selected:?}"
        );
    }
}
