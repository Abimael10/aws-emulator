// SPDX-License-Identifier: CC0-1.0

//! The Connect instance a `ListCampaigns` request FILTERS by, end to end over the
//! wire.
//!
//! Every case drives real HTTP through the router the production entrypoint serves, so
//! each assertion exercises the restJson1 wire protocol, the modelled constraints and
//! the handler together. The rule under test: a filtered listing resolves the instance
//! its `filters.instanceIdFilter` names BEFORE listing anything, so an instance that
//! is not onboarded to Campaigns in the region the request was signed for is refused
//! with HTTP 400 `ValidationException` and the fixed sentence — a caller probing for
//! an instance's campaigns is never told that an instance which is no campaign target
//! merely owns none.
//!
//! The rows differ along what the rule generalises over: WHY the filtered instance is
//! not onboarded (unknown to Amazon Connect, registered but never onboarded, its
//! onboarding job still in progress, onboarded only through another region's endpoint,
//! its configuration deleted since) and the form of the `InstanceId` the filter
//! carries (the length bounds, the punctuation its pattern admits, the uuid form a
//! Connect instance id takes). The positive controls alongside them: an onboarded
//! instance with campaigns lists its own, an onboarded instance with none lists
//! nothing, and an UNFILTERED listing is never refused.

use serde_json::{json, Value};
use service_com_amazonaws_connectcampaignsv2_amazonconnectcampaignservicev2::state::INSTANCE_NOT_ONBOARDED_MESSAGE;

use super::common::{
    call, create_campaign, error_code, error_message, onboard_instance, router, Harness,
};

/// The region every case signs for unless it is about more than one.
const REGION: &str = "us-west-2";

/// A second region, for the row whose instance is onboarded only through another
/// region's endpoint: an onboarding record is that region's resource, so this one
/// holds none.
const OTHER_REGION: &str = "eu-west-1";

/// `ListCampaigns` filtered by `instance`, as the model binds the operation
/// (`POST /v2/campaigns-summary`), with `Eq` — the one `InstanceIdFilterOperator` the
/// model declares.
async fn list_filtered(router: &Harness, region: &str, instance: &str) -> (u16, Value) {
    call(
        router,
        region,
        "POST",
        "/v2/campaigns-summary",
        &json!({"filters": {"instanceIdFilter": {"value": instance, "operator": "Eq"}}}),
    )
    .await
}

/// Instance ids spanning `InstanceId` (len 1..256, pattern `^[-_.a-zA-Z0-9]+$`, from
/// `smithy_query(query="shape", shape="InstanceId")`): the shortest id, the uuid form a
/// Connect instance id takes, every punctuation character the pattern admits, and the
/// longest id the length bound allows.
fn instance_ids() -> Vec<(&'static str, String)> {
    vec![
        ("one character (length minimum)", "a".to_string()),
        (
            "uuid form",
            "7c9e6679-7425-40de-944b-e07fc1f90ae7".to_string(),
        ),
        ("every admitted punctuation", "A-z_0.9".to_string()),
        ("256 characters (length maximum)", "f".repeat(256)),
    ]
}

/// Why the instance a row filters by is not onboarded here, and how the harness gets
/// it there.
#[derive(Clone, Copy, Debug)]
enum Setup {
    /// Amazon Connect holds no instance under the id, so nothing was ever onboardable.
    Unknown,
    /// The account's Connect registry holds the instance, but Campaigns was never
    /// asked to onboard it.
    Registered,
    /// Onboarding was started and the clock has NOT yet finished the job.
    Onboarding,
    /// The instance is onboarded through the OTHER region's endpoint only.
    OnboardedElsewhere,
    /// The instance was onboarded here and its configuration deleted since.
    ConfigDeleted,
}

/// Put `instance` into the state `setup` describes, in a freshly built router that
/// already holds one onboarded instance with a campaign of its own — so a refusal is
/// the filtered instance and not an empty store.
async fn set_up(setup: Setup, instance: &str) -> (Harness, String) {
    let router = router();
    let bystander = "listing-filter-bystander";
    let kept = create_campaign(&router, REGION, bystander, "bystander-campaign", None).await;
    match setup {
        Setup::Unknown => {}
        Setup::Registered => router.register_connect_instance(REGION, instance),
        Setup::Onboarding => {
            router.register_connect_instance(REGION, instance);
            // Started and deliberately NOT waited for: the job is due a span out, so
            // the listing below runs while it is still in progress.
            let (status, body) = call(
                &router,
                REGION,
                "PUT",
                &format!("/v2/connect-instance/{instance}/onboarding"),
                &json!({"encryptionConfig": {"enabled": false}}),
            )
            .await;
            assert_eq!(
                status, 200,
                "setup {setup:?}: expected the job to start, got {status} {body}"
            );
        }
        Setup::OnboardedElsewhere => onboard_instance(&router, OTHER_REGION, instance).await,
        Setup::ConfigDeleted => {
            onboard_instance(&router, REGION, instance).await;
            let (status, body) = call(
                &router,
                REGION,
                "DELETE",
                &format!("/v2/connect-instance/{instance}/config"),
                &Value::Null,
            )
            .await;
            assert_eq!(
                status, 200,
                "setup {setup:?}: expected the configuration to be deleted, got {status} {body}"
            );
        }
    }
    (router, kept)
}

#[tokio::test]
async fn a_filter_naming_an_instance_that_is_not_onboarded_is_refused() {
    for setup in [
        Setup::Unknown,
        Setup::Registered,
        Setup::Onboarding,
        Setup::OnboardedElsewhere,
        Setup::ConfigDeleted,
    ] {
        for (id_case, instance) in instance_ids() {
            let case = format!("{id_case} / {setup:?}");
            let (router, kept) = set_up(setup, &instance).await;

            let (status, body) = list_filtered(&router, REGION, &instance).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400 for an instance this region has not \
                 onboarded, got {status} {body}"
            );
            assert!(
                error_code(&body).ends_with("ValidationException"),
                "case {case:?}: expected ValidationException, got {}",
                error_code(&body)
            );
            assert_eq!(
                error_message(&body),
                INSTANCE_NOT_ONBOARDED_MESSAGE,
                "case {case:?}: expected the fixed sentence, got {:?}",
                error_message(&body)
            );
            assert!(
                body.get("campaignSummaryList").is_none(),
                "case {case:?}: a refusal lists nothing, got {body}"
            );

            // The refusal is about the FILTER: the same store answers an unfiltered
            // listing, and a listing filtered by the onboarded bystander, with the
            // campaign it holds.
            for (what, filtered) in [("unfiltered", false), ("the onboarded instance", true)] {
                let (status, body) = if filtered {
                    list_filtered(&router, REGION, "listing-filter-bystander").await
                } else {
                    call(&router, REGION, "POST", "/v2/campaigns-summary", &json!({})).await
                };
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the {what} listing to be served, got {status} {body}"
                );
                let ids: Vec<&str> = body["campaignSummaryList"]
                    .as_array()
                    .map(|entries| {
                        entries
                            .iter()
                            .filter_map(|entry| entry["id"].as_str())
                            .collect()
                    })
                    .unwrap_or_default();
                assert_eq!(
                    ids,
                    vec![kept.as_str()],
                    "case {case:?}: expected the {what} listing to carry the stored campaign, \
                     got {ids:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_filter_naming_an_onboarded_instance_lists_exactly_its_campaigns() {
    let router = router();

    // (the instance, how many campaigns it owns) — including an onboarded instance
    // that owns NONE, which is the empty page the refusal above must not be confused
    // with.
    let population: &[(&str, usize)] = &[
        ("listing-filter-a", 2),
        ("listing-filter.b", 1),
        ("listing-filter_c", 0),
    ];
    let mut owned: Vec<(&str, Vec<String>)> = Vec::new();
    for (instance, count) in population {
        onboard_instance(&router, REGION, instance).await;
        let mut ids = Vec::new();
        for index in 0..*count {
            ids.push(
                create_campaign(
                    &router,
                    REGION,
                    instance,
                    &format!("{instance}-{index}"),
                    None,
                )
                .await,
            );
        }
        owned.push((instance, ids));
    }

    for (instance, ids) in &owned {
        let (status, body) = list_filtered(&router, REGION, instance).await;
        assert_eq!(
            status, 200,
            "case {instance:?}: expected HTTP 200 for an onboarded instance, got {status} {body}"
        );
        let listed: Vec<&str> = body["campaignSummaryList"]
            .as_array()
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|entry| entry["id"].as_str())
                    .collect()
            })
            .unwrap_or_default();
        let expected: Vec<&str> = ids.iter().map(String::as_str).collect();
        assert_eq!(
            listed, expected,
            "case {instance:?}: expected exactly that instance's campaigns {expected:?}, \
             got {listed:?}"
        );
    }
}
