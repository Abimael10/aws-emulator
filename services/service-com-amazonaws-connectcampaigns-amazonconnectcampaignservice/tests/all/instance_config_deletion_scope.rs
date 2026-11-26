// SPDX-License-Identifier: CC0-1.0

//! What deleting one Amazon Connect instance's Campaigns configuration reaches, end to
//! end over HTTP.
//!
//! `DeleteConnectInstanceConfig` (DELETE `/connect-instance/{connectInstanceId}/config`,
//! `smithy_query(query="operation", operation="DeleteConnectInstanceConfig")`) ends the
//! onboarding of exactly the instance its path label names: the configuration that
//! instance reports is gone together with the onboarding record it is projected from,
//! while every other instance keeps the configuration, the onboarding job and the
//! campaigns it had. The rules under test, each over a table whose rows differ along what
//! the rule generalises over — how many other instances the account has onboarded, how
//! many campaigns each of them owns, and the form of the instance id the request carries:
//!
//! * an accepted delete reaches one instance only: the named instance reports no
//!   configuration and no onboarding job afterwards, and no other instance's
//!   configuration, job or campaign changes;
//! * the campaign gate counts the campaigns of the NAMED instance only, so campaigns
//!   another instance owns neither block its delete nor survive any differently, and an
//!   instance whose own campaigns are gone becomes deletable.
//!
//! The refusal sentences themselves are composed through the crate's own helpers here —
//! `instance_config.rs` is the suite that pins their wording — so these cases stay about
//! which resources the delete reaches.

use serde_json::{json, Value};
use service_com_amazonaws_connectcampaigns_amazonconnectcampaignservice::state::{
    instance_config_absent_message, onboarding_record_absent_message,
    INSTANCE_STILL_HAS_CAMPAIGNS_MESSAGE, ONBOARDING_STATUS_SUCCEEDED,
};

use super::common::{call, job_status, reach_the_due_instant, router, Answer, Harness};

/// The region every case here signs for; nothing in an onboarding record is
/// region-dependent, so one is enough.
const REGION: &str = "us-east-1";

/// `StartInstanceOnboardingJob` for `instance`, with the service's own encryption key.
async fn start(harness: &Harness, instance: &str) -> Answer {
    call(
        harness,
        REGION,
        "PUT",
        &format!("/connect-instance/{instance}/onboarding"),
        &json!({"encryptionConfig": {"enabled": false}}),
    )
    .await
}

/// `GetInstanceOnboardingJobStatus` for `instance`.
async fn status(harness: &Harness, instance: &str) -> Answer {
    call(
        harness,
        REGION,
        "GET",
        &format!("/connect-instance/{instance}/onboarding"),
        &Value::Null,
    )
    .await
}

/// `GetConnectInstanceConfig` for `instance`.
async fn config(harness: &Harness, instance: &str) -> Answer {
    call(
        harness,
        REGION,
        "GET",
        &format!("/connect-instance/{instance}/config"),
        &Value::Null,
    )
    .await
}

/// `DeleteConnectInstanceConfig` for `instance`.
async fn delete_config(harness: &Harness, instance: &str) -> Answer {
    call(
        harness,
        REGION,
        "DELETE",
        &format!("/connect-instance/{instance}/config"),
        &Value::Null,
    )
    .await
}

/// Give the account `instance` and take it through a FINISHED onboarding job, so it has a
/// configuration — the fixture state every case here starts an instance in.
async fn onboarded(harness: &Harness, instance: &str, case: &str) {
    harness.register_connect_instance(REGION, instance);
    let started = start(harness, instance).await;
    assert_eq!(
        started.status, 200,
        "case {case:?}: expected the onboarding of {instance:?} to be served, got {} {}",
        started.status, started.body
    );
    reach_the_due_instant(harness, instance);
}

/// `CreateCampaign` for `instance`, handing back the campaign id it minted.
async fn a_campaign_of(harness: &Harness, instance: &str, name: &str, case: &str) -> String {
    let created = call(
        harness,
        REGION,
        "PUT",
        "/campaigns",
        &json!({
            "name": name,
            "connectInstanceId": instance,
            "dialerConfig": {"progressiveDialerConfig": {"bandwidthAllocation": 0.5}},
            "outboundCallConfig": {"connectContactFlowId": "flow-1"},
        }),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: expected the create of {name:?} for {instance:?} to be served, \
         got {} {}",
        created.status, created.body
    );
    created.body["id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no campaign id in {}", created.body))
        .to_string()
}

/// `DescribeCampaign` for `id`.
async fn describe_campaign(harness: &Harness, id: &str) -> Answer {
    call(
        harness,
        REGION,
        "GET",
        &format!("/campaigns/{id}"),
        &Value::Null,
    )
    .await
}

/// Assert that `instance` still reports the configuration of a finished onboarding job,
/// and that the job itself is still the finished one.
async fn assert_still_configured(harness: &Harness, instance: &str, case: &str) {
    let served = config(harness, instance).await;
    assert_eq!(
        (
            served.status,
            served
                .body
                .pointer("/connectInstanceConfig/connectInstanceId")
                .and_then(Value::as_str)
        ),
        (200, Some(instance)),
        "case {case:?}: expected {instance:?} to still report its own config, got {} {}",
        served.status,
        served.body
    );
    let polled = status(harness, instance).await;
    assert_eq!(
        (polled.status, job_status(&polled)),
        (200, ONBOARDING_STATUS_SUCCEEDED),
        "case {case:?}: expected {instance:?} to still report its finished job, got {} {}",
        polled.status,
        polled.body
    );
}

/// An accepted delete reaches the named instance only: that instance's configuration and
/// the onboarding record it was projected from are gone, and every other onboarded
/// instance keeps its configuration, its job and its campaigns — however many of them
/// there are and whatever id form the request carried.
///
/// `InstanceId` is a string of at most 256 characters matching `^[a-zA-Z0-9_\-.]*$`
/// (`smithy_query(query="operation", operation="DeleteConnectInstanceConfig")`), so the
/// rows span the shortest id, the uuid form a Connect instance id takes, every
/// punctuation character the pattern admits and the longest id the bound allows.
#[tokio::test]
async fn an_accepted_config_delete_reaches_the_named_instance_only() {
    // `(case, the instance whose config is deleted, the other onboarded instances, how
    // many campaigns each of those others owns)`.
    let rows: Vec<(&str, String, Vec<&str>, usize)> = vec![
        (
            "one character, one other instance owning nothing",
            "t".to_string(),
            vec!["other-1"],
            0,
        ),
        (
            "uuid form, three others each owning one campaign",
            "3b7c9d41-85a2-4e60-9f13-72ad60c1e5b8".to_string(),
            vec!["other-1", "other-2", "other-3"],
            1,
        ),
        (
            "every admitted punctuation, one other owning two campaigns",
            "a.b_c-D9".to_string(),
            vec!["sibling.of-the_target"],
            2,
        ),
        (
            "256 characters (the length maximum), two others owning one campaign each",
            "t".repeat(256),
            vec!["other-1", "other-2"],
            1,
        ),
    ];

    for (case, target, others, campaigns_each) in rows {
        let harness = router();
        onboarded(&harness, &target, case).await;
        let mut other_campaigns: Vec<(&str, Vec<String>)> = Vec::new();
        for other in &others {
            onboarded(&harness, other, case).await;
            let mut ids = Vec::new();
            for index in 0..campaigns_each {
                ids.push(a_campaign_of(&harness, other, &format!("campaign-{index}"), case).await);
            }
            other_campaigns.push((other, ids));
        }

        // The named instance owns no campaign, so the delete is served — the campaigns the
        // OTHER instances own are not its own and do not hold its configuration open.
        let deleted = delete_config(&harness, &target).await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: expected HTTP 200 for the config delete, got {} {}",
            deleted.status, deleted.body
        );
        assert_eq!(
            deleted.body,
            Value::Null,
            "case {case:?}: expected the modelled Unit output to carry no body, got {}",
            deleted.body
        );

        // What the delete ended, read back through the operations that report it: the
        // instance has no configuration and no onboarding job any more.
        let read_back = config(&harness, &target).await;
        assert_eq!(
            (
                read_back.status,
                read_back.message().to_string(),
                read_back.error_code.ends_with("ResourceNotFoundException")
            ),
            (404, instance_config_absent_message(&target), true),
            "case {case:?}: expected the deleted config to be a 404 not-found afterwards, \
             got {} {:?} {}",
            read_back.status,
            read_back.error_code,
            read_back.body
        );
        let polled = status(&harness, &target).await;
        assert_eq!(
            (
                polled.status,
                polled.message().to_string(),
                polled.error_code.ends_with("ResourceNotFoundException")
            ),
            (404, onboarding_record_absent_message(&target), true),
            "case {case:?}: expected the onboarding record the config was projected from \
             to be gone, got {} {:?} {}",
            polled.status,
            polled.error_code,
            polled.body
        );

        // Every other instance is exactly as it was: its configuration, its job and each
        // campaign it owns.
        for (other, ids) in &other_campaigns {
            assert_still_configured(&harness, other, case).await;
            for id in ids {
                let described = describe_campaign(&harness, id).await;
                assert_eq!(
                    (
                        described.status,
                        described
                            .body
                            .pointer("/campaign/connectInstanceId")
                            .and_then(Value::as_str)
                    ),
                    (200, Some(*other)),
                    "case {case:?}: expected campaign {id} of {other:?} to be untouched, \
                     got {} {}",
                    described.status,
                    described.body
                );
            }
        }
    }
}

/// The delete is held open by the campaigns of the instance it names and by no others: an
/// instance that owns one is refused while a sibling that owns none is served, and the
/// refused instance becomes deletable once its own campaign is gone. Nothing of a refused
/// delete is written.
#[tokio::test]
async fn the_config_delete_is_gated_on_the_named_instances_own_campaigns() {
    let case = "one instance owns a campaign, its sibling owns none";
    let harness = router();
    let holder = "campaign-holder";
    let empty_handed = "no-campaigns-here";
    onboarded(&harness, holder, case).await;
    onboarded(&harness, empty_handed, case).await;
    let campaign = a_campaign_of(&harness, holder, "the-one-campaign", case).await;

    // The instance that owns the campaign keeps its configuration, and the refusal wrote
    // nothing: the configuration and the job are still what they were.
    let refused = delete_config(&harness, holder).await;
    assert_eq!(
        (
            refused.status,
            refused.message().to_string(),
            refused.error_code.ends_with("InvalidStateException")
        ),
        (409, INSTANCE_STILL_HAS_CAMPAIGNS_MESSAGE.to_string(), true),
        "case {case:?}: expected HTTP 409 InvalidStateException for the instance that \
         still owns a campaign, got {} {:?} {}",
        refused.status,
        refused.error_code,
        refused.body
    );
    assert_still_configured(&harness, holder, case).await;

    // Its sibling owns none of those campaigns, so its own delete is served — and the
    // instance that was refused is untouched by it.
    let served = delete_config(&harness, empty_handed).await;
    assert_eq!(
        served.status, 200,
        "case {case:?}: expected the sibling's delete to be served, got {} {}",
        served.status, served.body
    );
    let sibling_read = config(&harness, empty_handed).await;
    assert_eq!(
        (sibling_read.status, sibling_read.message().to_string()),
        (404, instance_config_absent_message(empty_handed)),
        "case {case:?}: expected the sibling to report no config afterwards, got {} {}",
        sibling_read.status,
        sibling_read.body
    );
    assert_still_configured(&harness, holder, case).await;
    let described = describe_campaign(&harness, &campaign).await;
    assert_eq!(
        described.status, 200,
        "case {case:?}: expected the campaign to survive the sibling's delete, got {} {}",
        described.status, described.body
    );

    // Once the instance's own campaign is gone, the same call is served, and a second
    // delete of a configuration that no longer exists is the not-found refusal.
    let dropped = call(
        &harness,
        REGION,
        "DELETE",
        &format!("/campaigns/{campaign}"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        dropped.status, 200,
        "case {case:?}: expected the campaign delete to be served, got {} {}",
        dropped.status, dropped.body
    );
    let now_served = delete_config(&harness, holder).await;
    assert_eq!(
        now_served.status, 200,
        "case {case:?}: expected the config delete to be served once no campaign is left, \
         got {} {}",
        now_served.status, now_served.body
    );
    let again = delete_config(&harness, holder).await;
    assert_eq!(
        (
            again.status,
            again.message().to_string(),
            again.error_code.ends_with("ResourceNotFoundException")
        ),
        (404, instance_config_absent_message(holder), true),
        "case {case:?}: expected a second delete of the same config to be the not-found \
         refusal, got {} {:?} {}",
        again.status,
        again.error_code,
        again.body
    );
}
