// SPDX-License-Identifier: CC0-1.0

//! What a DELETE of this service says when the resource it addresses is not there.
//!
//! The RULE under test: a refusal carries the text of the OPERATION that raised it, so a
//! delete that cannot find its resource answers in the words of ITS OWN error class —
//! composed from the identifiers the request's own path labels carried — and never the
//! sentence the READ of that same resource answers. `ResourceNotFoundException` (HTTP 434)
//! is the shape both carry; the text is what tells a caller which call failed.
//!
//! The suites assert that over the class of inputs that reach a delete's not-found branch
//! rather than the one recorded call:
//!
//! * `DeleteConfig` over every one of the model's eight `ConfigCapabilityType` values,
//!   crossed with ids that never named a config and an id the same type was created and
//!   deleted under — and, for each, against what `GetConfig` and `UpdateConfig` answer for
//!   the very same pair;
//! * the three resources of this service whose delete is addressed by an id — a config, a
//!   dataflow endpoint group and a mission profile — each answering its delete's own text
//!   and not its read's.
//!
//! Every request goes through the real front door — the router, deserialization,
//! constraint validation, the handler and serialization — on the harness
//! `scheduling_loop.rs` stands up.

use serde_json::json;
use service_com_amazonaws_groundstation_groundstation::state::{
    config_delete_not_found_message, config_mutation_not_found_message, config_not_found_message,
    dataflow_endpoint_group_delete_not_found_message, mission_profile_delete_not_found_message,
    DATAFLOW_ENDPOINT_GROUP_NOT_FOUND, MISSION_PROFILE_NOT_FOUND,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::config_update_refusal_precedence::{get_config, update_config};
use crate::dataflow_endpoint_groups::{create_group, delete_group, get_group, security_details};
use crate::not_found_messages::{not_found_message, ABSENT_UUIDS};
use crate::scheduling_loop::{
    antenna_uplink_config_data, config_data_table, create_config, create_mission_profile, harness,
    harness_with_engine, send, stock_ground_stations, Reply, STATION,
};

/// `DELETE /config/{configType}/{configId}`.
async fn delete_config(router: &MultiServiceRouter, config_type: &str, config_id: &str) -> Reply {
    send(
        router,
        "DELETE",
        &format!("/config/{config_type}/{config_id}"),
        None,
    )
    .await
}

/// The `{missionProfileId}` an ARN this service minted addresses.
fn id_of(arn: &str) -> String {
    arn.split('/')
        .next_back()
        .expect("an ARN this service minted ends in the resource id")
        .to_string()
}

#[tokio::test]
async fn delete_config_answers_its_own_text_for_every_capability_and_id_it_cannot_find() {
    let router = harness();

    // The config an `uplinkEchoConfig` body must point at, so every row below can carry a
    // well-formed body for its own capability.
    let (_, _, antenna_uplink_arn) = create_config(
        &router,
        "echo source",
        json!({"antennaUplinkConfig": antenna_uplink_config_data()}),
        None,
    )
    .await;

    for (config_type, config_data) in config_data_table(&antenna_uplink_arn) {
        // The ids this type's delete is addressed by: ones that never named a config, and
        // one a config of this very type was created and then deleted under — a delete of
        // an id that is already gone reads exactly as a delete of one that never was.
        let (created_id, _, _) = create_config(
            &router,
            &format!("doomed {config_type}"),
            config_data.clone(),
            None,
        )
        .await;
        let removed = delete_config(&router, config_type, &created_id).await;
        assert_eq!(
            removed.status, 200,
            "case {config_type}: expected the first DeleteConfig to succeed, got {} {}",
            removed.status, removed.body
        );

        let mut ids: Vec<String> = ABSENT_UUIDS.iter().map(|id| (*id).to_string()).collect();
        ids.push(created_id);

        for config_id in &ids {
            let case = format!("{config_type}/{config_id}");

            let deleted = delete_config(&router, config_type, config_id).await;
            let delete_text = not_found_message(&case, &deleted);
            assert_eq!(
                delete_text,
                format!("Config {config_id} with type {config_type} not found"),
                "case {case}: the delete's refusal is the fixed words around the pair its \
                 path carried, got {delete_text:?}"
            );
            assert_eq!(
                delete_text,
                config_delete_not_found_message(config_type, config_id),
                "case {case}: the crate's exported template must be this text, got \
                 {delete_text:?}"
            );
            // Outside the two slots nothing of the environment belongs in the text.
            assert!(
                !delete_text
                    .replace(config_id.as_str(), "")
                    .contains(engine::EMULATOR_ACCOUNT_ID),
                "case {case}: the account the call was scoped to is not part of the delete's \
                 text, got {delete_text:?}"
            );

            // The READ of the same pair keeps its own wording, and the two are different
            // texts: that is what tells a failed read from a failed delete.
            let read = get_config(&router, config_type, config_id).await;
            let read_text = not_found_message(&case, &read);
            assert_eq!(
                read_text,
                config_not_found_message(config_type, config_id),
                "case {case}: GetConfig must keep its own text, got {read_text:?}"
            );
            assert_ne!(
                delete_text, read_text,
                "case {case}: the delete and the read must not answer one text"
            );

            // And the UPDATE of the same pair keeps its own, account-qualified text, so no
            // two of the three operations share a template.
            let updated = update_config(
                &router,
                config_type,
                config_id,
                "resurrected",
                &config_data,
            )
            .await;
            let update_text = not_found_message(&case, &updated);
            assert_eq!(
                update_text,
                config_mutation_not_found_message(config_type, config_id),
                "case {case}: UpdateConfig must keep its own text, got {update_text:?}"
            );
            assert_ne!(
                delete_text, update_text,
                "case {case}: the delete and the update must not answer one text"
            );
        }
    }
}

#[tokio::test]
async fn every_delete_of_this_service_names_the_resource_its_read_refuses_to_name() {
    let (router, engine) = harness_with_engine();
    stock_ground_stations(&engine, STATION);

    // One resource of each kind whose delete is addressed by an id, created and then
    // deleted, so each row's id is one the account held and no longer does.
    let (config_id, _, _) = create_config(
        &router,
        "doomed tracking",
        json!({"trackingConfig": {"autotrack": "REQUIRED"}}),
        None,
    )
    .await;
    let removed_config = delete_config(&router, "tracking", &config_id).await;
    assert_eq!(
        removed_config.status, 200,
        "case setup: expected DeleteConfig 200, got {} {}",
        removed_config.status, removed_config.body
    );

    let created_group = create_group(
        &router,
        json!({"endpointDetails": [{
            "endpoint": {
                "name": "downlink-0",
                "address": {"name": "172.31.0.10", "port": 55_888},
            },
            "securityDetails": security_details(),
        }]}),
    )
    .await;
    assert_eq!(
        created_group.status, 200,
        "case setup: expected CreateDataflowEndpointGroup 200, got {} {}",
        created_group.status, created_group.body
    );
    let group_id = created_group.string("dataflowEndpointGroupId");
    let removed_group = delete_group(&router, &group_id).await;
    assert_eq!(
        removed_group.status, 200,
        "case setup: expected DeleteDataflowEndpointGroup 200, got {} {}",
        removed_group.status, removed_group.body
    );

    let profile_id = id_of(&create_mission_profile(&router, "doomed profile", 180, None, None).await);
    let removed_profile = send(
        &router,
        "DELETE",
        &format!("/missionprofile/{profile_id}"),
        None,
    )
    .await;
    assert_eq!(
        removed_profile.status, 200,
        "case setup: expected DeleteMissionProfile 200, got {} {}",
        removed_profile.status, removed_profile.body
    );

    // Each row: the kind of resource, the id it was held under, what its DELETE answers and
    // what its READ answers. The ids include one that never named anything, so a row covers
    // a resource that was deleted and one that never existed.
    for absent in ABSENT_UUIDS {
        for (kind, id, delete_reply, delete_text, read_reply, read_text) in [
            (
                "config",
                config_id.clone(),
                delete_config(&router, "tracking", &config_id).await,
                config_delete_not_found_message("tracking", &config_id),
                get_config(&router, "tracking", &config_id).await,
                config_not_found_message("tracking", &config_id),
            ),
            (
                "config never created",
                (*absent).to_string(),
                delete_config(&router, "tracking", absent).await,
                config_delete_not_found_message("tracking", absent),
                get_config(&router, "tracking", absent).await,
                config_not_found_message("tracking", absent),
            ),
            (
                "dataflow endpoint group",
                group_id.clone(),
                delete_group(&router, &group_id).await,
                dataflow_endpoint_group_delete_not_found_message(&group_id),
                get_group(&router, &group_id).await,
                DATAFLOW_ENDPOINT_GROUP_NOT_FOUND.to_string(),
            ),
            (
                "dataflow endpoint group never created",
                (*absent).to_string(),
                delete_group(&router, absent).await,
                dataflow_endpoint_group_delete_not_found_message(absent),
                get_group(&router, absent).await,
                DATAFLOW_ENDPOINT_GROUP_NOT_FOUND.to_string(),
            ),
            (
                "mission profile",
                profile_id.clone(),
                send(
                    &router,
                    "DELETE",
                    &format!("/missionprofile/{profile_id}"),
                    None,
                )
                .await,
                mission_profile_delete_not_found_message(&profile_id),
                send(
                    &router,
                    "GET",
                    &format!("/missionprofile/{profile_id}"),
                    None,
                )
                .await,
                MISSION_PROFILE_NOT_FOUND.to_string(),
            ),
            (
                "mission profile never created",
                (*absent).to_string(),
                send(&router, "DELETE", &format!("/missionprofile/{absent}"), None).await,
                mission_profile_delete_not_found_message(absent),
                send(&router, "GET", &format!("/missionprofile/{absent}"), None).await,
                MISSION_PROFILE_NOT_FOUND.to_string(),
            ),
        ] {
            let case = format!("{kind} {id}");
            let answered_delete = not_found_message(&format!("delete {case}"), &delete_reply);
            assert_eq!(
                answered_delete, delete_text,
                "case {case}: the delete answers the text of its own error class, got \
                 {answered_delete:?}"
            );
            assert!(
                answered_delete.contains(id.as_str()),
                "case {case}: a delete reports WHICH resource it could not find, got \
                 {answered_delete:?}"
            );

            let answered_read = not_found_message(&format!("read {case}"), &read_reply);
            assert_eq!(
                answered_read, read_text,
                "case {case}: the read keeps its own text, got {answered_read:?}"
            );
            assert_ne!(
                answered_delete, answered_read,
                "case {case}: the delete and the read must not answer one text"
            );
        }
    }
}
