// SPDX-License-Identifier: CC0-1.0

//! `Service$effectivePolicyValues` across EVERY policy that reaches one service, end to end
//! over HTTP.
//!
//! The rule under test: a service is held to the resilience targets of every policy that
//! reaches it — the one its own `policyArn` names, and the `policyArn` of every user journey
//! named by its `associatedSystems[].userJourneyIds` — and for each target it reports the
//! STRICTEST value any of them offers, attributed to the policy that offered it: the highest
//! availability SLO, the lowest RTO, RPO and time between backups, and the most resilient
//! disaster recovery approach. A target no contributing policy sets derives no entry, and a
//! service nothing reaches derives the empty object. Inheritance derives values only: the
//! service's own `policyArn` member stays exactly as stored, and a policy's
//! `associatedServiceCount` keeps counting only the services whose own `policyArn` names it.
//!
//! Because the derivation is resolved at projection time, repointing a journey at another
//! policy or narrowing an association back to the bare system changes what the next
//! `GetService` reports without the service being written to.
//!
//! The wire helpers, the derivation table and the `effectivePolicyValues` assertion are the
//! ones `effective_policy_values.rs` already keeps (both files are modules of this crate's
//! one `tests/all` binary), so the two suites cannot pass against different readings of the
//! member. What is new here — the association routes, the cross-policy oracle and the
//! contribution tables — lives below.
//!
//! Every suite drives the real wire path (`MultiServiceRouter::dispatch`). The suites are
//! TABLES: rows differ along which policies reach the service (own only, one journey, several
//! journeys of one system, journeys of several systems, a journey with no policy), which
//! target blocks each sets, which of them is the stricter for a given target, and every pair
//! of disaster recovery approaches — not one recorded request.

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::effective_policy_values::{
    assert_effective, call, create_policy, create_service, emulator, get_policy, get_service,
    service_arn_of, service_of, DERIVATIONS, DISASTER_RECOVERY_APPROACHES, SOURCE_SELF,
};

/// The modelled routes these suites drive beside the ones
/// `effective_policy_values.rs` already names.
const CREATE_SYSTEM: &str = "/v2/create-system";
const CREATE_USER_JOURNEY: &str = "/v2/create-user-journey";
const UPDATE_USER_JOURNEY: &str = "/v2/update-user-journey";
const UPDATE_SERVICE: &str = "/v2/update-service";

/// The availability targets a policy may carry, so a row that varies the SLO varies it over
/// values a `CreatePolicy` accepts.
///
/// SOURCE: this crate's `state::ALLOWED_AVAILABILITY_SLO_TARGETS` — the discrete service
/// levels `availabilitySlo.target` is admitted at.
const OFFERED_SLO_TARGETS: [f64; 3] = [99.9, 99.95, 99.99];

/// One policy that reaches a service: the name it is created under, and the target blocks it
/// sets.
#[derive(Debug, Clone)]
struct Contributor {
    name: String,
    targets: Value,
}

/// Create a system named `name` and answer its ARN.
async fn create_system(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, body) = call(router, "POST", CREATE_SYSTEM, Some(json!({"name": name}))).await;
    assert_eq!(
        status, 200,
        "case {case:?}: creating system {name:?} expected HTTP 200, got {status} {body:?}"
    );
    body["system"]["systemArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no system.systemArn in {body:?}"))
        .to_string()
}

/// Create a user journey on `system_arn`, optionally held to `policy_arn`, and answer its
/// `userJourneyId`.
async fn create_user_journey(
    router: &MultiServiceRouter,
    case: &str,
    system_arn: &str,
    name: &str,
    policy_arn: Option<&str>,
) -> String {
    let mut body = json!({"systemArn": system_arn, "name": name});
    if let Some(policy_arn) = policy_arn {
        body["policyArn"] = json!(policy_arn);
    }
    let (status, answered) = call(router, "POST", CREATE_USER_JOURNEY, Some(body)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: creating journey {name:?} expected HTTP 200, got {status} {answered:?}"
    );
    answered["userJourney"]["userJourneyId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no userJourney.userJourneyId in {answered:?}"))
        .to_string()
}

/// Point the journey `user_journey_id` of `system_arn` at `policy_arn`.
async fn repoint_journey(
    router: &MultiServiceRouter,
    case: &str,
    system_arn: &str,
    user_journey_id: &str,
    policy_arn: &str,
) {
    let (status, answered) = call(
        router,
        "POST",
        UPDATE_USER_JOURNEY,
        Some(json!({
            "systemArn": system_arn,
            "userJourneyId": user_journey_id,
            "policyArn": policy_arn,
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: repointing the journey expected HTTP 200, got {status} {answered:?}"
    );
    assert_eq!(
        answered["userJourney"]["policyArn"].as_str(),
        Some(policy_arn),
        "case {case:?}: expected the journey to carry {policy_arn}, got {:?}",
        answered["userJourney"]["policyArn"]
    );
}

/// `UpdateService`, setting `associatedSystems` to `associated`, and answer the `Service` it
/// projected.
async fn associate(
    router: &MultiServiceRouter,
    case: &str,
    service_arn: &str,
    associated: Value,
) -> Value {
    let (status, answered) = call(
        router,
        "POST",
        UPDATE_SERVICE,
        Some(json!({"serviceArn": service_arn, "associatedSystems": associated})),
    )
    .await;
    service_of(case, status, &answered)
}

/// `Policy$associatedServiceCount` for `policy_arn`.
async fn associated_service_count(router: &MultiServiceRouter, case: &str, policy_arn: &str) -> i64 {
    let (status, answered) = get_policy(router, policy_arn).await;
    assert_eq!(
        status, 200,
        "case {case:?}: reading {policy_arn} expected HTTP 200, got {status} {answered:?}"
    );
    answered["policy"]["associatedServiceCount"]
        .as_i64()
        .unwrap_or_else(|| panic!("case {case:?}: no policy.associatedServiceCount in {answered:?}"))
}

/// How strict one offered value is, as a number where LOWER is stricter.
///
/// An availability SLO is stricter the higher the percentage, an RTO/RPO/time-between-backups
/// stricter the lower the number of minutes, and a disaster recovery approach stricter the
/// nearer the front of the ladder [`DISASTER_RECOVERY_APPROACHES`] spells (the order the
/// model declares its approach enums in, from the approach that keeps a second copy serving
/// traffic down to the one that restores from a backup).
fn strictness(member: &str, target_member: &str, value: &Value) -> f64 {
    if member == "availabilitySlo" {
        return -value
            .as_f64()
            .unwrap_or_else(|| panic!("an availability SLO target is a number, got {value:?}"));
    }
    if target_member == "disasterRecoveryApproach" {
        let approach = value
            .as_str()
            .unwrap_or_else(|| panic!("a disaster recovery approach is a string, got {value:?}"));
        let position = DISASTER_RECOVERY_APPROACHES
            .iter()
            .position(|known| *known == approach)
            .unwrap_or_else(|| panic!("{approach} is not a modelled approach"));
        return position as f64;
    }
    value
        .as_f64()
        .unwrap_or_else(|| panic!("a target in minutes is a number, got {value:?}"))
}

/// The rule, written once: the `EffectivePolicyValues` a service reaches by `contributors`,
/// in contribution order.
///
/// Per `EffectivePolicyValues` member, the strictest value any contributor sets, carrying the
/// name of the contributor that set it; a member no contributor sets derives no entry. Where
/// two contributors offer the same value, the earlier one keeps the attribution — the service
/// is held to the same target either way.
fn strictest_of(contributors: &[Contributor]) -> Value {
    let mut expected = Map::new();
    for (member, block, target_member) in DERIVATIONS {
        let mut winner: Option<(f64, &Value, &str)> = None;
        for contributor in contributors {
            let offered = contributor
                .targets
                .get(block)
                .and_then(|block| block.get(target_member))
                .filter(|value| !value.is_null());
            let Some(offered) = offered else {
                continue;
            };
            let rank = strictness(member, target_member, offered);
            if winner
                .as_ref()
                .is_none_or(|(held, _, _)| rank < *held)
            {
                winner = Some((rank, offered, &contributor.name));
            }
        }
        if let Some((_, value, policy_name)) = winner {
            expected.insert(
                member.to_string(),
                json!({
                    "value": value.clone(),
                    "policyName": policy_name,
                    "source": SOURCE_SELF,
                }),
            );
        }
    }
    Value::Object(expected)
}

/// One row of the contribution table: which policies reach the service.
#[derive(Debug, Clone)]
struct Row {
    label: &'static str,
    /// The target blocks of the service's OWN policy, or `None` for a service with no
    /// policy of its own.
    own: Option<Value>,
    /// One entry per system the service is associated with, each listing the journeys of
    /// that system the association names: `Some(targets)` for a journey held to a policy
    /// with those targets, `None` for a journey with no policy at all.
    systems: Vec<Vec<Option<Value>>>,
}

fn contribution_rows() -> Vec<Row> {
    vec![
        Row {
            label: "no own policy, one journey whose policy sets an availability SLO",
            own: None,
            systems: vec![vec![Some(json!({"availabilitySlo": {"target": 99.95}}))]],
        },
        Row {
            label: "no own policy, one journey whose policy sets every target block",
            own: None,
            systems: vec![vec![Some(json!({
                "availabilitySlo": {"target": 99.9},
                "multiAz": {"rtoInMinutes": 30, "rpoInMinutes": 5, "disasterRecoveryApproach": "HOT_STANDBY"},
                "multiRegion": {"rtoInMinutes": 240, "rpoInMinutes": 60, "disasterRecoveryApproach": "PILOT_LIGHT"},
                "dataRecovery": {"timeBetweenBackupsInMinutes": 1440},
            }))]],
        },
        Row {
            label: "no own policy, a journey whose policy sets nothing at all",
            own: None,
            systems: vec![vec![Some(json!({}))]],
        },
        Row {
            label: "no own policy, a journey with no policy of its own",
            own: None,
            systems: vec![vec![None]],
        },
        Row {
            label: "an own policy and a journey with no policy of its own",
            own: Some(json!({"multiAz": {"rtoInMinutes": 30, "rpoInMinutes": 10}})),
            systems: vec![vec![None]],
        },
        Row {
            label: "own and journey policies, each the stricter for a different target",
            own: Some(json!({
                "availabilitySlo": {"target": 99.95},
                "multiAz": {"rtoInMinutes": 30, "rpoInMinutes": 10, "disasterRecoveryApproach": "HOT_STANDBY"},
            })),
            systems: vec![vec![Some(json!({
                "availabilitySlo": {"target": 99.99},
                "multiAz": {"rtoInMinutes": 15, "rpoInMinutes": 20, "disasterRecoveryApproach": "HOT_STANDBY"},
                "multiRegion": {"rtoInMinutes": 60, "rpoInMinutes": 15, "disasterRecoveryApproach": "PILOT_LIGHT"},
            }))]],
        },
        Row {
            label: "an own policy stricter than the journey's on every target it shares",
            own: Some(json!({
                "availabilitySlo": {"target": 99.99},
                "multiAz": {"rtoInMinutes": 5, "rpoInMinutes": 1},
                "dataRecovery": {"timeBetweenBackupsInMinutes": 15},
            })),
            systems: vec![vec![Some(json!({
                "availabilitySlo": {"target": 99.9},
                "multiAz": {"rtoInMinutes": 60, "rpoInMinutes": 30},
                "dataRecovery": {"timeBetweenBackupsInMinutes": 1440},
            }))]],
        },
        Row {
            label: "two policies offering one target the same value",
            own: Some(json!({"multiAz": {"rtoInMinutes": 20}})),
            systems: vec![vec![Some(json!({"multiAz": {"rtoInMinutes": 20}}))]],
        },
        Row {
            label: "two journeys of one system, plus an own policy",
            own: Some(json!({
                "availabilitySlo": {"target": 99.9},
                "multiAz": {"rtoInMinutes": 45},
            })),
            systems: vec![vec![
                Some(json!({
                    "multiAz": {"rpoInMinutes": 10, "disasterRecoveryApproach": "WARM_STANDBY"},
                    "dataRecovery": {"timeBetweenBackupsInMinutes": 720},
                })),
                Some(json!({
                    "availabilitySlo": {"target": 99.99},
                    "multiAz": {"rtoInMinutes": 30, "rpoInMinutes": 15, "disasterRecoveryApproach": "ACTIVE_ACTIVE"},
                    "dataRecovery": {"timeBetweenBackupsInMinutes": 60},
                })),
            ]],
        },
        Row {
            label: "journeys of two different systems, no own policy",
            own: None,
            systems: vec![
                vec![Some(json!({
                    "multiRegion": {"rtoInMinutes": 300, "rpoInMinutes": 90},
                }))],
                vec![Some(json!({
                    "multiRegion": {"rtoInMinutes": 120, "rpoInMinutes": 120, "disasterRecoveryApproach": "BACKUP_AND_RESTORE"},
                    "availabilitySlo": {"target": 99.9},
                }))],
            ],
        },
        Row {
            label: "a weaker own approach beside a stronger journey approach",
            own: Some(json!({
                "multiAz": {"disasterRecoveryApproach": "BACKUP_AND_RESTORE"},
                "multiRegion": {"disasterRecoveryApproach": "WARM_STANDBY"},
            })),
            systems: vec![vec![Some(json!({
                "multiAz": {"disasterRecoveryApproach": "ACTIVE_ACTIVE"},
                "multiRegion": {"disasterRecoveryApproach": "PILOT_LIGHT"},
            }))]],
        },
        Row {
            label: "a journey whose policy sets a target at the modelled minimum",
            own: Some(json!({"multiAz": {"rtoInMinutes": 1, "rpoInMinutes": 1}})),
            systems: vec![vec![Some(
                json!({"multiAz": {"rtoInMinutes": 0, "rpoInMinutes": 0}}),
            )]],
        },
    ]
}

/// A `CreatePolicy` body: a name plus the target blocks `targets` carries.
fn policy_body(name: &str, targets: &Value) -> Value {
    let mut body = json!({"name": name});
    for (key, value) in targets
        .as_object()
        .expect("a row's target blocks are an object")
    {
        body[key] = value.clone();
    }
    body
}

#[tokio::test]
async fn the_policies_of_the_journeys_a_service_is_associated_through_reach_it() {
    let router = emulator();
    for (index, row) in contribution_rows().into_iter().enumerate() {
        let case = row.label;

        // The service's own policy, if the row gives it one.
        let own = match row.own.as_ref() {
            Some(targets) => {
                let name = format!("Own{index}");
                let arn = create_policy(&router, case, policy_body(&name, targets)).await;
                Some((
                    Contributor {
                        name,
                        targets: targets.clone(),
                    },
                    arn,
                ))
            }
            None => None,
        };

        // The systems, their journeys, and the policies those journeys are held to. Every
        // contributor is collected in the order the association will name it: the service's
        // own policy first, then each system's journeys in order.
        let mut contributors: Vec<Contributor> = own.iter().map(|(own, _)| own.clone()).collect();
        let mut journey_policies: Vec<(String, String)> = Vec::new();
        let mut associated = Vec::new();
        for (system_index, journeys) in row.systems.iter().enumerate() {
            let system_arn =
                create_system(&router, case, &format!("Sys{index}N{system_index}")).await;
            let mut journey_ids = Vec::new();
            for (journey_index, targets) in journeys.iter().enumerate() {
                let policy_arn = match targets {
                    Some(targets) => {
                        let name = format!("Jrn{index}N{system_index}N{journey_index}");
                        let arn = create_policy(&router, case, policy_body(&name, targets)).await;
                        contributors.push(Contributor {
                            name: name.clone(),
                            targets: targets.clone(),
                        });
                        journey_policies.push((name, arn.clone()));
                        Some(arn)
                    }
                    None => None,
                };
                let journey_id = create_user_journey(
                    &router,
                    case,
                    &system_arn,
                    &format!("Journey {system_index} {journey_index}"),
                    policy_arn.as_deref(),
                )
                .await;
                journey_ids.push(journey_id);
            }
            associated.push(json!({"systemArn": system_arn, "userJourneyIds": journey_ids}));
        }

        // Before the association, only the service's own policy reaches it.
        let own_arn = own.as_ref().map(|(_, arn)| arn.clone());
        let (status, answered) =
            create_service(&router, &format!("Svc{index}"), own_arn.as_deref()).await;
        let created = service_of(case, status, &answered);
        let own_only: Vec<Contributor> = own.iter().map(|(own, _)| own.clone()).collect();
        assert_effective(case, &created, &strictest_of(&own_only));
        let service_arn = service_arn_of(case, &created);

        // Associating the systems and their journeys brings those journeys' policies in.
        let expected = strictest_of(&contributors);
        let updated = associate(
            &router,
            case,
            &service_arn,
            Value::Array(associated.clone()),
        )
        .await;
        assert_effective(case, &updated, &expected);

        // The same projection answers a later read, so the member cannot drift from what
        // the update reported.
        let (status, answered) = get_service(&router, &service_arn).await;
        let read_back = service_of(case, status, &answered);
        assert_effective(case, &read_back, &expected);

        // Inheritance derives VALUES: the service's own `policyArn` member is untouched, so
        // a service with no policy of its own still reports none.
        assert_eq!(
            read_back["policyArn"].as_str(),
            own_arn.as_deref(),
            "case {case:?}: expected policyArn {own_arn:?}, got {:?}",
            read_back["policyArn"]
        );

        // And only an OWN `policyArn` counts as an association of the policy.
        if let Some(own_arn) = own_arn.as_deref() {
            let count = associated_service_count(&router, case, own_arn).await;
            assert_eq!(
                count, 1,
                "case {case:?}: expected the own policy to count one associated service, got \
                 {count}"
            );
        }
        for (name, arn) in &journey_policies {
            let count = associated_service_count(&router, case, arn).await;
            assert_eq!(
                count, 0,
                "case {case:?}: expected the journey policy {name} to count no associated \
                 service, got {count}"
            );
        }
    }
}

#[tokio::test]
async fn repointing_a_journey_rederives_what_the_service_it_reaches_reports() {
    // The derivation is resolved at projection time, so pointing the journey at another
    // policy changes the next read of every service associated through it — with no write to
    // the service. Rows: the service has a policy of its own, or none.
    let router = emulator();
    let first_targets = json!({"availabilitySlo": {"target": 99.9}});
    let second_targets = json!({
        "multiAz": {"rtoInMinutes": 45, "rpoInMinutes": 5, "disasterRecoveryApproach": "WARM_STANDBY"},
    });

    for (index, own_targets) in [
        None,
        Some(json!({"multiAz": {"rtoInMinutes": 90}, "dataRecovery": {"timeBetweenBackupsInMinutes": 30}})),
    ]
    .into_iter()
    .enumerate()
    {
        let case = if own_targets.is_some() {
            "a service with a policy of its own, associated through a journey"
        } else {
            "a service with no policy of its own, associated through a journey"
        };
        let first_name = format!("Repoint{index}First");
        let second_name = format!("Repoint{index}Second");
        let first =
            create_policy(&router, case, policy_body(&first_name, &first_targets)).await;
        let second =
            create_policy(&router, case, policy_body(&second_name, &second_targets)).await;
        let own = match own_targets.as_ref() {
            Some(targets) => {
                let name = format!("RepointOwn{index}");
                let arn = create_policy(&router, case, policy_body(&name, targets)).await;
                Some((
                    Contributor {
                        name,
                        targets: targets.clone(),
                    },
                    arn,
                ))
            }
            None => None,
        };
        let own_contributors: Vec<Contributor> = own.iter().map(|(own, _)| own.clone()).collect();

        let system_arn = create_system(&router, case, &format!("RepointSys{index}")).await;
        let journey_id = create_user_journey(
            &router,
            case,
            &system_arn,
            "Checkout",
            Some(first.as_str()),
        )
        .await;

        let own_arn = own.as_ref().map(|(_, arn)| arn.clone());
        let (status, answered) =
            create_service(&router, &format!("RepointSvc{index}"), own_arn.as_deref()).await;
        let created = service_of(case, status, &answered);
        let service_arn = service_arn_of(case, &created);

        let with_journey = json!([{"systemArn": system_arn, "userJourneyIds": [journey_id]}]);
        let updated = associate(&router, case, &service_arn, with_journey.clone()).await;
        let mut expected_contributors = own_contributors.clone();
        expected_contributors.push(Contributor {
            name: first_name.clone(),
            targets: first_targets.clone(),
        });
        assert_effective(case, &updated, &strictest_of(&expected_contributors));

        // Repoint the journey: the next read of the service reports the new policy's
        // targets, with no call having touched the service.
        repoint_journey(&router, case, &system_arn, &journey_id, &second).await;
        let mut after_repoint = own_contributors.clone();
        after_repoint.push(Contributor {
            name: second_name.clone(),
            targets: second_targets.clone(),
        });
        let (status, answered) = get_service(&router, &service_arn).await;
        let read_back = service_of(case, status, &answered);
        assert_effective(case, &read_back, &strictest_of(&after_repoint));

        // Narrowing the association back to the bare system drops the inherited values and
        // leaves only what the service's own policy derives.
        let bare = json!([{"systemArn": system_arn}]);
        let narrowed = associate(&router, case, &service_arn, bare).await;
        assert_effective(case, &narrowed, &strictest_of(&own_contributors));
        let (status, answered) = get_service(&router, &service_arn).await;
        let read_back = service_of(case, status, &answered);
        assert_effective(case, &read_back, &strictest_of(&own_contributors));

        // Naming the journey again brings its policy back.
        let reassociated = associate(&router, case, &service_arn, with_journey).await;
        assert_effective(case, &reassociated, &strictest_of(&after_repoint));
    }
}

#[tokio::test]
async fn the_more_resilient_of_two_offered_approaches_is_the_one_in_force() {
    // Every PAIR of the approaches the model publishes, offered one by the service's own
    // policy and one by the policy of a journey it is associated through: the one nearer the
    // front of the ladder is in force for both the multi-AZ and the multi-Region member,
    // whichever side of the association offered it.
    let router = emulator();
    for (stronger_index, stronger) in DISASTER_RECOVERY_APPROACHES.into_iter().enumerate() {
        for (weaker_index, weaker) in DISASTER_RECOVERY_APPROACHES.into_iter().enumerate() {
            if weaker_index <= stronger_index {
                continue;
            }
            for own_is_stronger in [true, false] {
                let case = format!(
                    "{stronger} against {weaker}, own policy offers the \
                     {} one",
                    if own_is_stronger { "stronger" } else { "weaker" }
                );
                let tag = format!("{stronger_index}{weaker_index}{}", own_is_stronger as u8);
                let (own_approach, journey_approach) = if own_is_stronger {
                    (stronger, weaker)
                } else {
                    (weaker, stronger)
                };
                let own_targets = json!({
                    "multiAz": {"disasterRecoveryApproach": own_approach},
                    "multiRegion": {"disasterRecoveryApproach": own_approach},
                });
                let journey_targets = json!({
                    "multiAz": {"disasterRecoveryApproach": journey_approach},
                    "multiRegion": {"disasterRecoveryApproach": journey_approach},
                });
                let own_name = format!("ApproachOwn{tag}");
                let journey_name = format!("ApproachJrn{tag}");
                let own = create_policy(&router, &case, policy_body(&own_name, &own_targets)).await;
                let journey_policy = create_policy(
                    &router,
                    &case,
                    policy_body(&journey_name, &journey_targets),
                )
                .await;

                let system_arn = create_system(&router, &case, &format!("ApproachSys{tag}")).await;
                let journey_id = create_user_journey(
                    &router,
                    &case,
                    &system_arn,
                    "Browse",
                    Some(journey_policy.as_str()),
                )
                .await;
                let (status, answered) =
                    create_service(&router, &format!("ApproachSvc{tag}"), Some(&own)).await;
                let created = service_of(&case, status, &answered);
                let service_arn = service_arn_of(&case, &created);
                let updated = associate(
                    &router,
                    &case,
                    &service_arn,
                    json!([{"systemArn": system_arn, "userJourneyIds": [journey_id]}]),
                )
                .await;

                let expected = strictest_of(&[
                    Contributor {
                        name: own_name,
                        targets: own_targets,
                    },
                    Contributor {
                        name: journey_name,
                        targets: journey_targets,
                    },
                ]);
                for member in ["multiAzDrApproach", "multiRegionDrApproach"] {
                    assert_eq!(
                        updated["effectivePolicyValues"][member]["value"].as_str(),
                        Some(stronger),
                        "case {case:?}: expected {member} {stronger}, got {:?}",
                        updated["effectivePolicyValues"][member]
                    );
                }
                assert_effective(&case, &updated, &expected);
            }
        }
    }
}

#[tokio::test]
async fn one_policy_reaching_a_service_twice_contributes_it_once() {
    // A policy attached to the service AND named by a journey it is associated through
    // offers the same values either way, so the service reports exactly that policy's
    // targets — and counts as ONE associated service, because only its own `policyArn`
    // counts.
    let router = emulator();
    let case = "the service's own policy is also its journey's policy";
    let targets = json!({
        "availabilitySlo": {"target": 99.95},
        "multiAz": {"rtoInMinutes": 25, "rpoInMinutes": 5, "disasterRecoveryApproach": "HOT_STANDBY"},
    });
    let name = "SharedPolicy".to_string();
    let policy_arn = create_policy(&router, case, policy_body(&name, &targets)).await;
    let system_arn = create_system(&router, case, "SharedSys").await;
    let journey_id = create_user_journey(
        &router,
        case,
        &system_arn,
        "Shared journey",
        Some(policy_arn.as_str()),
    )
    .await;
    let (status, answered) = create_service(&router, "SharedSvc", Some(&policy_arn)).await;
    let created = service_of(case, status, &answered);
    let service_arn = service_arn_of(case, &created);
    let updated = associate(
        &router,
        case,
        &service_arn,
        json!([{"systemArn": system_arn, "userJourneyIds": [journey_id]}]),
    )
    .await;
    let expected = strictest_of(&[Contributor {
        name: name.clone(),
        targets,
    }]);
    assert_effective(case, &updated, &expected);
    let count = associated_service_count(&router, case, &policy_arn).await;
    assert_eq!(
        count, 1,
        "case {case:?}: expected one associated service, got {count}"
    );
}

#[tokio::test]
async fn every_offered_availability_slo_is_inherited_through_a_journey() {
    // The SLO rows of the table, driven through a journey alone: each availability level the
    // service offers is inherited by a service with no policy of its own, and where two
    // levels meet the HIGHER one is in force.
    let router = emulator();
    for (index, target) in OFFERED_SLO_TARGETS.into_iter().enumerate() {
        for (other_index, other) in OFFERED_SLO_TARGETS.into_iter().enumerate() {
            let case = format!("a journey policy at {target} beside an own policy at {other}");
            let tag = format!("{index}{other_index}");
            let journey_targets = json!({"availabilitySlo": {"target": target}});
            let own_targets = json!({"availabilitySlo": {"target": other}});
            let journey_name = format!("SloJrn{tag}");
            let own_name = format!("SloOwn{tag}");
            let journey_policy = create_policy(
                &router,
                &case,
                policy_body(&journey_name, &journey_targets),
            )
            .await;
            let own = create_policy(&router, &case, policy_body(&own_name, &own_targets)).await;
            let system_arn = create_system(&router, &case, &format!("SloSys{tag}")).await;
            let journey_id = create_user_journey(
                &router,
                &case,
                &system_arn,
                "Search",
                Some(journey_policy.as_str()),
            )
            .await;
            let (status, answered) =
                create_service(&router, &format!("SloSvc{tag}"), Some(&own)).await;
            let created = service_of(&case, status, &answered);
            let service_arn = service_arn_of(&case, &created);
            let updated = associate(
                &router,
                &case,
                &service_arn,
                json!([{"systemArn": system_arn, "userJourneyIds": [journey_id]}]),
            )
            .await;
            let expected = strictest_of(&[
                Contributor {
                    name: own_name,
                    targets: own_targets,
                },
                Contributor {
                    name: journey_name,
                    targets: journey_targets,
                },
            ]);
            assert_effective(&case, &updated, &expected);
            let in_force = updated["effectivePolicyValues"]["availabilitySlo"]["value"].as_f64();
            let higher = target.max(other);
            assert_eq!(
                in_force,
                Some(higher),
                "case {case:?}: expected the higher SLO {higher}, got {in_force:?}"
            );
        }
    }
}
