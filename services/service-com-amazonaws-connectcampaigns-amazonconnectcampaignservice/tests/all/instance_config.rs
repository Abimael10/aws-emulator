// SPDX-License-Identifier: CC0-1.0

//! The Campaigns configuration of an Amazon Connect instance, end to end over HTTP.
//!
//! Every case drives a real request through the router the production entrypoint
//! serves, so each assertion exercises the restJson1 wire protocol, the modelled
//! constraints and the handlers together. The rules under test, each over a table whose
//! rows differ along what the rule generalises over rather than along one request:
//!
//! * an instance's configuration IS its finished onboarding record projected onto the
//!   modelled `InstanceConfig` — the instance the request named, the `encryptionConfig`
//!   the `StartInstanceOnboardingJob` call persisted, and the account's Campaigns
//!   service-linked role — whatever the instance id or the encryption config;
//! * an instance whose onboarding job has not FINISHED has no configuration: one nobody
//!   onboarded and one whose job is still in progress are both HTTP 404
//!   `ResourceNotFoundException` naming the configuration and the instance the request
//!   asked about;
//! * reading the configuration only REPORTS: it neither starts a job nor finishes one,
//!   and repeated reads at the same point of the job agree;
//! * DELETING the configuration is what ends an instance's onboarding: it is refused
//!   while the instance still owns campaigns, in whatever state those campaigns are, and
//!   once none is left the configuration is gone — so the instance reports no
//!   configuration and can be onboarded again.

use serde_json::{json, Value};
use service_com_amazonaws_connectcampaigns_amazonconnectcampaignservice::state::{
    instance_config_absent_message, service_linked_role_arn,
    ComAmazonawsConnectcampaignsAmazonconnectcampaignserviceState,
    INSTANCE_STILL_HAS_CAMPAIGNS_MESSAGE, ONBOARDING_STATUS_IN_PROGRESS,
    ONBOARDING_STATUS_SUCCEEDED,
};

use super::common::{call, job_status, reach_the_due_instant, router, Answer, Harness, KEY_ARN};

/// The region every case here signs for; nothing in an onboarding record is
/// region-dependent, so one is enough.
const REGION: &str = "us-east-1";

/// The sentence a request asking for the configuration of an instance that has none is
/// refused with, as fixed text around the id the request carried. Written out here
/// rather than read back out of the implementation, so this suite pins the wording
/// itself; `{id}` is where the request's instance id goes.
const CONFIG_ABSENT_WORDING: &str = "ConnectInstanceConfig not found {id}.";

/// The sentence a request asking to delete the configuration of an instance that still
/// owns campaigns is refused with. Written out here for the same reason as the one above;
/// it names neither the instance nor the campaigns, so there is no place to fill.
const STILL_HAS_CAMPAIGNS_WORDING: &str = "This instance still has campaigns";

/// IAM files a service-linked role under the reserved
/// `role/aws-service-role/<service principal>/` path, and the Campaigns role of an
/// account is `AWSServiceRoleForAmazonConnectCampaigns`. IAM is global, so the ARN
/// carries no region. `{account}` is where the account id goes.
const SERVICE_LINKED_ROLE_WORDING: &str = "arn:aws:iam::{account}:role/aws-service-role/\
     connect-campaigns.amazonaws.com/AWSServiceRoleForAmazonConnectCampaigns";

/// `wording` with the request's instance `id` — and the caller's account id, which the
/// emulator presents as its own canonical one — in the places it names.
fn worded(wording: &str, id: &str) -> String {
    wording
        .replace("{id}", id)
        .replace("{account}", emulator_server::emulator_account_id())
}

/// `StartInstanceOnboardingJob` for `instance` with `encryption_config`.
async fn start(harness: &Harness, instance: &str, encryption_config: &Value) -> Answer {
    call(
        harness,
        REGION,
        "PUT",
        &format!("/connect-instance/{instance}/onboarding"),
        &json!({"encryptionConfig": encryption_config}),
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

/// `CreateCampaign` for `instance`, handing back the campaign id it minted.
async fn a_campaign_of(harness: &Harness, instance: &str, name: &str) -> String {
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
        "case \"create {name} for {instance}\": expected HTTP 200, got {} {}",
        created.status, created.body
    );
    created.body["id"]
        .as_str()
        .unwrap_or_else(|| panic!("case \"create {name}\": no id in {}", created.body))
        .to_string()
}

/// Move the campaign `id` into the run state the suffixes of `transitions` lead to — the
/// run-state plane of `/campaigns/{id}/<suffix>` — asserting each step was served.
async fn move_campaign(harness: &Harness, id: &str, transitions: &[&str], case: &str) {
    for suffix in transitions {
        let moved = call(
            harness,
            REGION,
            "POST",
            &format!("/campaigns/{id}/{suffix}"),
            &Value::Null,
        )
        .await;
        assert_eq!(
            moved.status, 200,
            "case {case:?}: expected the {suffix} of campaign {id} to be served, got {} {}",
            moved.status, moved.body
        );
    }
}

/// `DeleteCampaign` for `id`, asserting it was served.
async fn delete_campaign(harness: &Harness, id: &str, case: &str) {
    let deleted = call(
        harness,
        REGION,
        "DELETE",
        &format!("/campaigns/{id}"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        deleted.status, 200,
        "case {case:?}: expected the delete of campaign {id} to be served, got {} {}",
        deleted.status, deleted.body
    );
}

/// The `connectInstanceConfig` member of a served answer, as the three members the
/// modelled `InstanceConfig` carries: the instance id, the service-linked role ARN, and
/// the `enabled` / `keyArn` / `encryptionType` of its encryption config.
type ReportedConfig = (String, String, (bool, Option<String>, Option<String>));

/// [`ReportedConfig`] read off `answer`, or `None` when it carries no configuration at
/// all — so a case compares the whole reported shape rather than one member of it.
fn reported_config(answer: &Answer) -> Option<ReportedConfig> {
    let reported = answer.body.get("connectInstanceConfig")?;
    Some((
        reported
            .get("connectInstanceId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        reported
            .get("serviceLinkedRoleArn")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        encryption_members(reported.get("encryptionConfig").unwrap_or(&Value::Null)),
    ))
}

/// The `enabled` / `keyArn` / `encryptionType` of an `EncryptionConfig` as JSON — the
/// one reader both a request body and a response body are read through, so a case
/// compares what it sent with what came back instead of with a literal written twice.
fn encryption_members(encryption_config: &Value) -> (bool, Option<String>, Option<String>) {
    (
        encryption_config
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        encryption_config
            .get("keyArn")
            .and_then(Value::as_str)
            .map(str::to_string),
        encryption_config
            .get("encryptionType")
            .and_then(Value::as_str)
            .map(str::to_string),
    )
}

/// The configuration an instance onboarded with `encryption_config` is expected to
/// report: the instance it names, the account's one Campaigns service-linked role, and
/// the encryption config exactly as sent.
fn expected_config(instance: &str, encryption_config: &Value) -> ReportedConfig {
    (
        instance.to_string(),
        worded(SERVICE_LINKED_ROLE_WORDING, instance),
        encryption_members(encryption_config),
    )
}

/// `enabled: false` — the service encrypts with its own key, so no `keyArn`.
fn service_key_config() -> Value {
    json!({"enabled": false})
}

/// Instance ids and encryption configs spanning what the rule generalises over:
/// `InstanceId` (len 0..256, pattern `^[a-zA-Z0-9_\-.]*$`, from
/// `smithy_query(query="shape", shape="InstanceId")`) — the shortest id a path label can
/// carry, the uuid form a Connect instance id takes, every punctuation character the
/// pattern admits, the longest id the bound allows — each paired with a different
/// `EncryptionConfig` form (`smithy_query(query="shape", shape="EncryptionConfig")`).
fn config_rows() -> Vec<(&'static str, String, Value)> {
    vec![
        (
            "one character, the service's own key",
            "a".to_string(),
            service_key_config(),
        ),
        (
            "uuid form, a customer KMS key",
            "6f2b8c10-1d47-4a93-8b22-55d1f0a7c311".to_string(),
            json!({"enabled": true, "encryptionType": "KMS", "keyArn": KEY_ARN}),
        ),
        (
            "every admitted punctuation, the supported type with no key",
            "a.b_c-D9".to_string(),
            json!({"enabled": false, "encryptionType": "KMS"}),
        ),
        (
            "256 characters, a key with no type",
            "i".repeat(256),
            json!({"enabled": true, "keyArn": KEY_ARN}),
        ),
    ]
}

#[tokio::test]
async fn a_finished_onboarding_gives_the_instance_the_config_it_was_onboarded_with() {
    for (case, instance, encryption_config) in config_rows() {
        let harness = router();
        harness.register_connect_instance(REGION, &instance);

        let started = start(&harness, &instance, &encryption_config).await;
        assert_eq!(
            started.status, 200,
            "case {case:?}: expected HTTP 200 from StartInstanceOnboardingJob, got {} {}",
            started.status, started.body
        );

        reach_the_due_instant(&harness, &instance);

        // The configuration is the finished record projected: every member comes from
        // what the onboarding call persisted or from the account, never from the read.
        let served = config(&harness, &instance).await;
        assert_eq!(
            served.status, 200,
            "case {case:?}: expected HTTP 200 for an onboarded instance, got {} {}",
            served.status, served.body
        );
        assert_eq!(
            reported_config(&served),
            Some(expected_config(&instance, &encryption_config)),
            "case {case:?}: expected the stored onboarding projected onto InstanceConfig, \
             got {}",
            served.body
        );

        // A read reports and nothing else: the job is still the finished one, and a
        // second read of the configuration agrees with the first.
        let polled = status(&harness, &instance).await;
        assert_eq!(
            (polled.status, job_status(&polled)),
            (200, ONBOARDING_STATUS_SUCCEEDED),
            "case {case:?}: reading the config left the job as it was, got {} {}",
            polled.status,
            polled.body
        );
        let again = config(&harness, &instance).await;
        assert_eq!(
            (again.status, reported_config(&again)),
            (served.status, reported_config(&served)),
            "case {case:?}: expected two reads to agree, got {} {}",
            again.status,
            again.body
        );
    }
}

#[tokio::test]
async fn an_instance_whose_job_has_not_finished_has_no_config() {
    for (case, instance, encryption_config) in config_rows() {
        let harness = router();
        harness.register_connect_instance(REGION, &instance);

        // Before anything onboarded it, the instance has no configuration — even though
        // the account holds the instance itself.
        let before = config(&harness, &instance).await;
        assert_eq!(
            before.status, 404,
            "case {case:?}: expected HTTP 404 before the instance is onboarded, got {} {}",
            before.status, before.body
        );

        let started = start(&harness, &instance, &encryption_config).await;
        assert_eq!(
            (started.status, job_status(&started)),
            (200, ONBOARDING_STATUS_IN_PROGRESS),
            "case {case:?}: expected a job that has only just started, got {} {}",
            started.status,
            started.body
        );

        // While the job is in progress there is still no configuration, and asking for
        // one neither finishes the job nor stores anything: the span is never zero, so
        // two reads taken straight after the start call are both refused.
        for read in 1..=2 {
            let during = config(&harness, &instance).await;
            assert_eq!(
                during.status, 404,
                "case {case:?} read {read}: expected HTTP 404 while the job is in progress, \
                 got {} {}",
                during.status, during.body
            );
            assert!(
                during.error_code.ends_with("ResourceNotFoundException"),
                "case {case:?} read {read}: expected ResourceNotFoundException, got {:?}",
                during.error_code
            );
            assert_eq!(
                during.message(),
                worded(CONFIG_ABSENT_WORDING, &instance),
                "case {case:?} read {read}: expected the config-absent wording around the id \
                 the request carried, got {}",
                during.body
            );
            assert!(
                during.body.get("connectInstanceConfig").is_none(),
                "case {case:?} read {read}: a refused read reports no config, got {}",
                during.body
            );
        }
        let polled = status(&harness, &instance).await;
        assert_eq!(
            (polled.status, job_status(&polled)),
            (200, ONBOARDING_STATUS_IN_PROGRESS),
            "case {case:?}: reading the config must not finish the job, got {} {}",
            polled.status,
            polled.body
        );

        // The clock alone makes the configuration readable — nothing the reads did.
        reach_the_due_instant(&harness, &instance);
        let after = config(&harness, &instance).await;
        assert_eq!(
            (after.status, reported_config(&after)),
            (200, Some(expected_config(&instance, &encryption_config))),
            "case {case:?}: expected the finished job's config, got {} {}",
            after.status,
            after.body
        );
    }
}

#[tokio::test]
async fn an_instance_nobody_onboarded_has_no_config() {
    let harness = router();
    let onboarded = "onboarded-instance";
    harness.register_connect_instance(REGION, onboarded);
    let started = start(&harness, onboarded, &service_key_config()).await;
    assert_eq!(
        started.status, 200,
        "case \"onboard one instance\": expected HTTP 200, got {} {}",
        started.status, started.body
    );
    reach_the_due_instant(&harness, onboarded);

    // One onboarded instance configures no other: a record is keyed by its own instance
    // id, so an id the account holds without having onboarded it, an id it does not hold
    // at all, and an id that merely resembles the onboarded one are all misses.
    let unonboarded = "held-but-not-onboarded";
    harness.register_connect_instance(REGION, unonboarded);
    let rows: Vec<(&str, String)> = vec![
        (
            "an instance the account holds but nobody onboarded",
            unonboarded.to_string(),
        ),
        ("an id the account does not hold", "never-registered".to_string()),
        ("the onboarded id with a suffix", format!("{onboarded}-2")),
        (
            "the uuid form",
            "b9e74c20-3a16-4f58-9d07-1c82ae53f604".to_string(),
        ),
        ("one character", "q".to_string()),
        ("every admitted punctuation", "x.y_z-W8".to_string()),
        ("256 characters (the length maximum)", "z".repeat(256)),
    ];

    for (case, instance) in rows {
        let refused = config(&harness, &instance).await;
        assert_eq!(
            refused.status, 404,
            "case {case:?}: expected HTTP 404 for an instance with no config, got {} {}",
            refused.status, refused.body
        );
        assert!(
            refused.error_code.ends_with("ResourceNotFoundException"),
            "case {case:?}: expected ResourceNotFoundException, got {:?}",
            refused.error_code
        );
        assert_eq!(
            refused.message(),
            worded(CONFIG_ABSENT_WORDING, &instance),
            "case {case:?}: expected the config-absent wording around the id the request \
             carried, got {}",
            refused.body
        );
        assert!(
            refused.body.get("connectInstanceConfig").is_none(),
            "case {case:?}: a refused read reports no config, got {}",
            refused.body
        );
    }

    // The reads refused above left the one onboarded instance's configuration alone.
    let served = config(&harness, onboarded).await;
    assert_eq!(
        (served.status, reported_config(&served)),
        (
            200,
            Some(expected_config(onboarded, &service_key_config()))
        ),
        "case \"the onboarded instance afterwards\": expected its config to be unchanged, \
         got {} {}",
        served.status,
        served.body
    );
}

#[tokio::test]
async fn the_config_follows_the_store_and_not_the_read() {
    let harness = router();
    let instance = "store-backed-instance";
    harness.register_connect_instance(REGION, instance);
    let sent = json!({"enabled": true, "encryptionType": "KMS", "keyArn": KEY_ARN});
    let started = start(&harness, instance, &sent).await;
    assert_eq!(
        started.status, 200,
        "case \"onboard the instance\": expected HTTP 200, got {} {}",
        started.status, started.body
    );
    reach_the_due_instant(&harness, instance);

    let served = config(&harness, instance).await;
    assert_eq!(
        reported_config(&served),
        Some(expected_config(instance, &sent)),
        "case \"the onboarded config\": expected the config that was persisted, got {}",
        served.body
    );

    // The response is the STORE's record and not the request's: dropping the record
    // leaves the instance with no configuration, and nothing else had to change.
    let state = harness
        .engine()
        .state::<ComAmazonawsConnectcampaignsAmazonconnectcampaignserviceState>();
    assert!(
        state.lock().instance_onboarding.remove(instance).is_some(),
        "case \"drop the stored record\": expected the onboarding call to have stored one"
    );
    let refused = config(&harness, instance).await;
    assert_eq!(
        (refused.status, refused.message().to_string()),
        (404, worded(CONFIG_ABSENT_WORDING, instance)),
        "case \"the dropped record\": expected HTTP 404 with the config-absent wording, \
         got {} {}",
        refused.status,
        refused.body
    );
}

#[test]
fn the_config_wordings_are_the_ones_this_suite_pins() {
    // The handler composes its refusal and the role ARN through the crate's own
    // helpers, so the sentences pinned above are checked against those helpers too — a
    // change to either side has to change both.
    for (case, instance) in [
        (
            "the uuid form",
            "0d9f1c38-7b25-4e61-9a40-cc83e2715d06".to_string(),
        ),
        ("one character", "a".to_string()),
        ("256 characters (the length maximum)", "y".repeat(256)),
    ] {
        assert_eq!(
            instance_config_absent_message(&instance),
            worded(CONFIG_ABSENT_WORDING, &instance),
            "case {case:?}: expected {:?}, got {:?}",
            worded(CONFIG_ABSENT_WORDING, &instance),
            instance_config_absent_message(&instance)
        );
    }
    assert_eq!(
        service_linked_role_arn(),
        worded(SERVICE_LINKED_ROLE_WORDING, ""),
        "case \"the service-linked role ARN\": expected {:?}, got {:?}",
        worded(SERVICE_LINKED_ROLE_WORDING, ""),
        service_linked_role_arn()
    );
    assert_eq!(
        INSTANCE_STILL_HAS_CAMPAIGNS_MESSAGE, STILL_HAS_CAMPAIGNS_WORDING,
        "case \"the in-use wording\": expected {STILL_HAS_CAMPAIGNS_WORDING:?}, got \
         {INSTANCE_STILL_HAS_CAMPAIGNS_MESSAGE:?}"
    );
}

/// An instance that still owns campaigns keeps its configuration, whatever state those
/// campaigns are in and however many there are; once every one of them is deleted the
/// configuration is deleted too, and the instance then reports none.
///
/// The rows differ along what the rule generalises over: the run state the campaigns are
/// in — a campaign the create left `Initialized`, and the states the run-state plane moves
/// one into — and how many campaigns the instance owns.
#[tokio::test]
async fn an_instance_that_still_owns_campaigns_keeps_its_config() {
    // `(case, how many campaigns, the run-state path suffixes each is moved through)`.
    let rows: Vec<(&str, usize, Vec<&str>)> = vec![
        ("one campaign the create left Initialized", 1, vec![]),
        ("one Running campaign", 1, vec!["start"]),
        ("one Paused campaign", 1, vec!["start", "pause"]),
        ("one Stopped campaign", 1, vec!["start", "stop"]),
        ("three campaigns in the initial state", 3, vec![]),
        ("two Running campaigns", 2, vec!["start"]),
    ];

    for (case, count, transitions) in rows {
        let harness = router();
        let instance = "instance-with-campaigns";
        harness.register_connect_instance(REGION, instance);
        let started = start(&harness, instance, &service_key_config()).await;
        assert_eq!(
            started.status, 200,
            "case {case:?}: expected the onboarding to be served, got {} {}",
            started.status, started.body
        );
        reach_the_due_instant(&harness, instance);

        let mut campaigns = Vec::new();
        for index in 0..count {
            let id = a_campaign_of(&harness, instance, &format!("campaign-{index}")).await;
            move_campaign(&harness, &id, &transitions, case).await;
            campaigns.push(id);
        }

        // Every campaign but the last one is deleted one at a time: while ANY campaign of
        // the instance is left, the configuration stays and the refusal is the same.
        for (index, id) in campaigns.iter().enumerate() {
            let refused = delete_config(&harness, instance).await;
            assert_eq!(
                refused.status, 409,
                "case {case:?} ({} campaigns left): expected HTTP 409 while the instance \
                 still owns campaigns, got {} {}",
                campaigns.len() - index,
                refused.status,
                refused.body
            );
            assert!(
                refused.error_code.ends_with("InvalidStateException"),
                "case {case:?} ({} campaigns left): expected InvalidStateException, got {:?}",
                campaigns.len() - index,
                refused.error_code
            );
            assert_eq!(
                refused.message(),
                STILL_HAS_CAMPAIGNS_WORDING,
                "case {case:?} ({} campaigns left): expected the in-use sentence, got {}",
                campaigns.len() - index,
                refused.body
            );

            // The refusal wrote nothing: the configuration is still readable, and the
            // onboarding job it was projected from is still the finished one.
            let still_there = config(&harness, instance).await;
            assert_eq!(
                (still_there.status, reported_config(&still_there)),
                (
                    200,
                    Some(expected_config(instance, &service_key_config()))
                ),
                "case {case:?} ({} campaigns left): a refused delete leaves the config \
                 readable, got {} {}",
                campaigns.len() - index,
                still_there.status,
                still_there.body
            );
            let polled = status(&harness, instance).await;
            assert_eq!(
                (polled.status, job_status(&polled)),
                (200, ONBOARDING_STATUS_SUCCEEDED),
                "case {case:?} ({} campaigns left): a refused delete leaves the job as it \
                 was, got {} {}",
                campaigns.len() - index,
                polled.status,
                polled.body
            );

            delete_campaign(&harness, id, case).await;
        }

        // With no campaign left, the delete is served: a Unit output, so no body at all.
        let deleted = delete_config(&harness, instance).await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: expected HTTP 200 once no campaign is left, got {} {}",
            deleted.status, deleted.body
        );
        assert_eq!(
            deleted.body,
            Value::Null,
            "case {case:?}: expected the delete to answer no body, got {}",
            deleted.body
        );

        // The configuration is gone — read back through the operation that reports it —
        // and so is the onboarding record it was projected from.
        let read_back = config(&harness, instance).await;
        assert_eq!(
            (read_back.status, read_back.message().to_string()),
            (404, worded(CONFIG_ABSENT_WORDING, instance)),
            "case {case:?}: expected the deleted config to be a 404 afterwards, got {} {}",
            read_back.status,
            read_back.body
        );
        assert!(
            read_back.error_code.ends_with("ResourceNotFoundException"),
            "case {case:?}: expected ResourceNotFoundException, got {:?}",
            read_back.error_code
        );

        // The onboarding lifecycle has an exit AND a way back in: the instance can be
        // onboarded again, and the new job's own config is what it then reports.
        let again_config = json!({"enabled": true, "encryptionType": "KMS", "keyArn": KEY_ARN});
        let restarted = start(&harness, instance, &again_config).await;
        assert_eq!(
            (restarted.status, job_status(&restarted)),
            (200, ONBOARDING_STATUS_IN_PROGRESS),
            "case {case:?}: expected a fresh onboarding job after the delete, got {} {}",
            restarted.status,
            restarted.body
        );
        reach_the_due_instant(&harness, instance);
        let reonboarded = config(&harness, instance).await;
        assert_eq!(
            (reonboarded.status, reported_config(&reonboarded)),
            (200, Some(expected_config(instance, &again_config))),
            "case {case:?}: expected the re-onboarded config, got {} {}",
            reonboarded.status,
            reonboarded.body
        );
    }
}

/// An instance with no configuration has none to delete: the delete answers the same
/// not-found class, and the same sentence, that the read of a configuration answers.
#[tokio::test]
async fn an_instance_with_no_config_has_none_to_delete() {
    let harness = router();

    // An instance whose job has only just started has no configuration yet, so neither
    // the read nor the delete finds one.
    let in_progress = "job-in-progress";
    harness.register_connect_instance(REGION, in_progress);
    let started = start(&harness, in_progress, &service_key_config()).await;
    assert_eq!(
        (started.status, job_status(&started)),
        (200, ONBOARDING_STATUS_IN_PROGRESS),
        "case \"start a job\": expected a job that has only just started, got {} {}",
        started.status,
        started.body
    );

    // One instance IS onboarded, so the misses below are misses of their own instance and
    // not of an empty store.
    let onboarded = "onboarded-instance";
    harness.register_connect_instance(REGION, onboarded);
    let onboarding = start(&harness, onboarded, &service_key_config()).await;
    assert_eq!(
        onboarding.status, 200,
        "case \"onboard one instance\": expected HTTP 200, got {} {}",
        onboarding.status, onboarding.body
    );
    reach_the_due_instant(&harness, onboarded);

    let held_not_onboarded = "held-but-not-onboarded";
    harness.register_connect_instance(REGION, held_not_onboarded);
    let rows: Vec<(&str, String)> = vec![
        (
            "an instance the account holds but nobody onboarded",
            held_not_onboarded.to_string(),
        ),
        (
            "an instance whose onboarding job has not finished",
            in_progress.to_string(),
        ),
        (
            "an id the account does not hold",
            "never-registered".to_string(),
        ),
        ("the onboarded id with a suffix", format!("{onboarded}-2")),
        (
            "the uuid form",
            "4e1b7a92-5c38-4f0d-8a16-93cd70e2b418".to_string(),
        ),
        ("one character", "q".to_string()),
        ("every admitted punctuation", "x.y_z-W8".to_string()),
        ("256 characters (the length maximum)", "z".repeat(256)),
    ];

    for (case, instance) in rows {
        let refused = delete_config(&harness, &instance).await;
        assert_eq!(
            refused.status, 404,
            "case {case:?}: expected HTTP 404 for an instance with no config, got {} {}",
            refused.status, refused.body
        );
        assert!(
            refused.error_code.ends_with("ResourceNotFoundException"),
            "case {case:?}: expected ResourceNotFoundException, got {:?}",
            refused.error_code
        );
        assert_eq!(
            refused.message(),
            worded(CONFIG_ABSENT_WORDING, &instance),
            "case {case:?}: expected the config-absent wording around the id the request \
             carried, got {}",
            refused.body
        );
    }

    // None of those refusals touched the instance that IS onboarded, nor the job that is
    // still running.
    let served = config(&harness, onboarded).await;
    assert_eq!(
        (served.status, reported_config(&served)),
        (
            200,
            Some(expected_config(onboarded, &service_key_config()))
        ),
        "case \"the onboarded instance afterwards\": expected its config to be unchanged, \
         got {} {}",
        served.status,
        served.body
    );
    let polled = status(&harness, in_progress).await;
    assert_eq!(
        (polled.status, job_status(&polled)),
        (200, ONBOARDING_STATUS_IN_PROGRESS),
        "case \"the unfinished job afterwards\": expected it to be running still, got {} {}",
        polled.status,
        polled.body
    );

    // The delete that found no configuration is repeatable, and so is one that followed a
    // served delete: the instance has none either way.
    reach_the_due_instant(&harness, in_progress);
    let now_deletable = delete_config(&harness, in_progress).await;
    assert_eq!(
        now_deletable.status, 200,
        "case \"the job has since finished\": expected the delete to be served, got {} {}",
        now_deletable.status, now_deletable.body
    );
    let again = delete_config(&harness, in_progress).await;
    assert_eq!(
        (again.status, again.message().to_string()),
        (404, worded(CONFIG_ABSENT_WORDING, in_progress)),
        "case \"delete the same config twice\": expected the second to be a 404, got {} {}",
        again.status,
        again.body
    );
}
