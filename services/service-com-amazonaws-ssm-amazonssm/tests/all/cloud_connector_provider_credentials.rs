// SPDX-License-Identifier: CC0-1.0

//! What the provider credential of a cloud connector rests on: the ACCOUNT having the
//! outbound web identity federation that vends the token on, and WHO answered the question
//! about the verified credential itself.
//!
//! A cloud connector acts in the third-party cloud as one application registration of one
//! tenant, and the credential it acts under is created by presenting an Amazon Web Services
//! web identity token to it. Two rules are asserted here:
//!
//! 1. the token is vended only while the ACCOUNT has outbound web identity federation on
//!    ("When true, IAM principals in the account can call the `GetWebIdentityToken` API",
//!    `GetOutboundWebIdentityFederationInfoResponse$JwtVendingEnabled`, IAM's model), so a
//!    create and a configuration-replacing update made for an account that has it off are
//!    refused HTTP 400 `ValidationException` with the fixed sentence for that condition and
//!    write nothing, while the same request for an account that has it on is served — and a
//!    validation reports the same answer as the modelled
//!    `OutboundWebIdentityFederationDisabled` finding;
//! 2. the credential itself can be created only for a tenant and an application this account
//!    already holds a verified credential for — the Amazon Web Services Config connector the
//!    required `ConfigConnectorArn` names
//!    ([`service_com_amazonaws_ssm_amazonssm::state::provider_credentials`]) — and that
//!    refusal rests on what THAT service ANSWERED about the connector, so an emulator with
//!    no Config mounted to ask refuses nothing: the same grounding every question this slice
//!    puts to another service takes ([`crate::cloud_connector_role_and_deletion`]).
//!
//! The rows span what each rule generalises over: which tenant, which application, how many
//! subscriptions a configuration targets at the published bounds of `AzureSubscriptionList`,
//! a `ConfigConnectorArn` of each partition the member's pattern admits, which members an
//! update carries, and how the account came to the state its feature is in. What Config's
//! own answers DO to the same request is the cross-service suite's
//! (`core/router/tests/all/ssm_cloud_connector_provider_credential.rs`), which is the only
//! place both services are mounted over one engine.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloud_connector_validation::{
    enable_outbound_federation, federation_permitted, put_role_policy, trusted_by_ssm,
};
use crate::hybrid_activation_role::{create_role, iam_answer, router_with_iam};
use crate::parameter_store::{call, error_code, error_message, router, ACCOUNT, REGION};

/// An Azure tenant id: `AzureConfiguration$TenantId` is a hyphenated hex UUID
/// (`^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$`).
const TENANT: &str = "3f2504e0-4f89-11d3-9a0c-0305e82c3301";

/// See [`TENANT`]: `AzureConfiguration$ApplicationId` has the same shape.
const APPLICATION: &str = "7b2f0a1c-8d3e-4c5a-9b6d-1e2f3a4b5c6d";

/// A second tenant, so a row can name a different one.
const OTHER_TENANT: &str = "c56a4180-65aa-42ec-a945-5fd21dec0538";

/// A second application registration, in the same shape.
const OTHER_APPLICATION: &str = "d1e2f3a4-b5c6-47d8-9e0f-1a2b3c4d5e6f";

/// The published floor of `AzureSubscriptionList` (`@length(min: 1, max: 75)`), which is how
/// many subscriptions a configuration that carries `Targets` at all must name.
const SUBSCRIPTIONS_MIN: usize = 1;

/// The published ceiling of that same list.
const SUBSCRIPTIONS_MAX: usize = 75;

/// `subscriptions` ids of the `AzureSubscriptionId` form, built so each is distinct.
fn subscription_ids(subscriptions: usize) -> Vec<String> {
    (0..subscriptions)
        .map(|index| format!("aaaaaaa1-0000-0000-0000-{index:012x}"))
        .collect()
}

/// A `CloudConnectorConfiguration` naming `tenant` and `application` and targeting
/// `subscriptions` of them (none at all when zero, which is a tenant-level connector).
fn azure_configuration(tenant: &str, application: &str, subscriptions: usize) -> Value {
    let mut azure = json!({
        "TenantId": tenant,
        "TenantDisplayName": "Contoso",
        "ApplicationId": application,
        "ApplicationDisplayName": "Systems Manager connector",
    });
    if subscriptions > 0 {
        azure["Targets"] = json!({
            "Subscriptions": subscription_ids(subscriptions)
                .iter()
                .map(|id| json!({ "Id": id }))
                .collect::<Vec<Value>>(),
        });
    }
    json!({ "AzureConfiguration": azure })
}

/// RULE, grounded: the provider-credential refusal rests on what Amazon Web Services Config
/// ANSWERED about the connector the request names, so with no Config mounted to ask the
/// create is served — whatever tenant, application, targets or Config connector ARN it
/// names — and the connector it mints reports back exactly what was written.
#[tokio::test]
async fn a_create_is_served_where_no_config_answers_about_the_connector() {
    /// One row: the Config connector the create names, and the configuration it asks for.
    struct Row {
        case: &'static str,
        /// The `ConfigConnectorArn`, whose modelled pattern admits the `aws`, `aws-cn` and
        /// `aws-us-gov` partitions
        /// (`^arn:aws(-cn|-us-gov)?:config:([^:]+):\d{12}:connector/.+$`).
        config_connector_arn: String,
        tenant: &'static str,
        application: &'static str,
        subscriptions: usize,
    }

    let rows = vec![
        Row {
            case: "a tenant-level connector",
            config_connector_arn: format!("arn:aws:config:{REGION}:{ACCOUNT}:connector/azure"),
            tenant: TENANT,
            application: APPLICATION,
            subscriptions: 0,
        },
        Row {
            case: "another tenant and application, at the subscription floor",
            config_connector_arn: format!(
                "arn:aws:config:{REGION}:{ACCOUNT}:connector/azure-{OTHER_TENANT}"
            ),
            tenant: OTHER_TENANT,
            application: OTHER_APPLICATION,
            subscriptions: SUBSCRIPTIONS_MIN,
        },
        Row {
            case: "the same tenant under another application, at the subscription ceiling",
            config_connector_arn: format!(
                "arn:aws:config:{REGION}:{ACCOUNT}:connector/azure-{TENANT}"
            ),
            tenant: TENANT,
            application: OTHER_APPLICATION,
            subscriptions: SUBSCRIPTIONS_MAX,
        },
        Row {
            case: "a Config connector of the China partition",
            config_connector_arn: format!("arn:aws-cn:config:{REGION}:{ACCOUNT}:connector/azure"),
            tenant: TENANT,
            application: APPLICATION,
            subscriptions: 2,
        },
        Row {
            case: "a Config connector of the GovCloud partition",
            config_connector_arn: format!(
                "arn:aws-us-gov:config:{REGION}:{ACCOUNT}:connector/azure/nested/name"
            ),
            tenant: OTHER_TENANT,
            application: APPLICATION,
            subscriptions: 3,
        },
    ];

    for row in rows {
        let case = row.case;
        // A router per row, with this service alone: nothing is mounted that could answer
        // about the Config connector the create names.
        let router = router();
        let configuration = azure_configuration(row.tenant, row.application, row.subscriptions);
        let (status, created) = call(
            &router,
            "CreateCloudConnector",
            &json!({
                "DisplayName": case,
                "RoleArn": format!("arn:aws:iam::{ACCOUNT}:role/SsmCloudConnectorRole"),
                "ConfigConnectorArn": row.config_connector_arn,
                "Configuration": configuration,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: with nothing to ask, the create is served, got {status} with \
             {created}"
        );
        let id = created["CloudConnectorId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a served create reports an id, got {created}"))
            .to_string();

        let (status, read) = call(
            &router,
            "GetCloudConnector",
            &json!({ "CloudConnectorId": id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the minted connector must be readable, got {status} with {read}"
        );
        let azure = &read["Configuration"]["AzureConfiguration"];
        let targeted: Vec<String> = azure["Targets"]["Subscriptions"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|subscription| subscription["Id"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            (
                azure["TenantId"].as_str(),
                azure["ApplicationId"].as_str(),
                read["ConfigConnectorArn"].as_str(),
                targeted,
            ),
            (
                Some(row.tenant),
                Some(row.application),
                Some(row.config_connector_arn.as_str()),
                subscription_ids(row.subscriptions),
            ),
            "case {case:?}: expected the connector the create was handed, got {read}"
        );
    }
}

/// RULE, grounded, for the update half: an update that replaces the configuration puts the
/// same question, so with nothing to answer it is applied — every member it carries
/// replacing the stored one and every member it omits left alone.
#[tokio::test]
async fn an_update_is_applied_where_no_config_answers_about_the_connector() {
    /// One row: what the update carries, and the configuration the connector ends up with.
    struct Row {
        case: &'static str,
        display_name: Option<&'static str>,
        /// The tenant, application and subscription count a replacement configuration
        /// names, or `None` for an update that carries none.
        configuration: Option<(&'static str, &'static str, usize)>,
    }

    let rows = vec![
        Row {
            case: "a replacement naming another tenant and application",
            display_name: None,
            configuration: Some((OTHER_TENANT, OTHER_APPLICATION, 0)),
        },
        Row {
            case: "a replacement targeting subscriptions, beside a new name",
            display_name: Some("renamed connector"),
            configuration: Some((TENANT, APPLICATION, 2)),
        },
        Row {
            case: "an update carrying no configuration at all",
            display_name: Some("renamed again"),
            configuration: None,
        },
    ];

    for row in rows {
        let case = row.case;
        let router = router();
        let (status, created) = call(
            &router,
            "CreateCloudConnector",
            &json!({
                "DisplayName": "the connector under update",
                "RoleArn": format!("arn:aws:iam::{ACCOUNT}:role/SsmCloudConnectorRole"),
                "ConfigConnectorArn": format!(
                    "arn:aws:config:{REGION}:{ACCOUNT}:connector/azure-{TENANT}"
                ),
                "Configuration": azure_configuration(TENANT, APPLICATION, SUBSCRIPTIONS_MIN),
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the create must be served, got {status} with {created}"
        );
        let id = created["CloudConnectorId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a served create reports an id, got {created}"))
            .to_string();

        let mut update = json!({ "CloudConnectorId": id });
        if let Some(display_name) = row.display_name {
            update["DisplayName"] = json!(display_name);
        }
        if let Some((tenant, application, subscriptions)) = row.configuration {
            update["Configuration"] = azure_configuration(tenant, application, subscriptions);
        }
        let (status, updated) = call(&router, "UpdateCloudConnector", &update).await;
        assert_eq!(
            status, 200,
            "case {case:?}: with nothing to ask, the update is applied, got {status} with \
             {updated}"
        );

        let (status, read) = call(
            &router,
            "GetCloudConnector",
            &json!({ "CloudConnectorId": id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the updated connector must be readable, got {status} with {read}"
        );
        let azure = &read["Configuration"]["AzureConfiguration"];
        // An update carrying no configuration leaves the created one, which is the floor
        // case of the create above.
        let (tenant, application, subscriptions) = row
            .configuration
            .unwrap_or((TENANT, APPLICATION, SUBSCRIPTIONS_MIN));
        let expected_name = row.display_name.unwrap_or("the connector under update");
        let targeted = azure["Targets"]["Subscriptions"]
            .as_array()
            .map(|list| list.len())
            .unwrap_or_default();
        assert_eq!(
            (
                azure["TenantId"].as_str(),
                azure["ApplicationId"].as_str(),
                targeted,
                read["DisplayName"].as_str(),
            ),
            (
                Some(tenant),
                Some(application),
                subscriptions,
                Some(expected_name),
            ),
            "case {case:?}: expected the members the update carried, got {read}"
        );
    }
}

/// The sentence a request made for an account that has not enabled outbound web identity
/// federation is refused with: entirely fixed text, interpolating nothing from the request.
const OUTBOUND_FEDERATION_DISABLED: &str =
    "Outbound web identity federation is disabled for this AWS account. Enable it to use \
     cross-cloud authentication with Azure.";

/// The `ValidationFindingCode` a validation reports that condition under, from the modelled
/// enum (`smithy_query(query="shape", shape="ValidationFindingCode")`: `…
/// WebIdentityTokenFailed | OutboundWebIdentityFederationDisabled |
/// ProviderCredentialCreationFailed | TenantSummary | SubscriptionAccessible`).
const OUTBOUND_FEDERATION_FINDING_CODE: &str = "OutboundWebIdentityFederationDisabled";

/// The role every case below names, created in IAM trusting this service and permitted to
/// obtain the token, so what the rows vary is the ACCOUNT's own feature and nothing else.
const FEDERATING_ROLE: &str = "SsmCloudConnectorFederatingRole";

/// Create that role through IAM's own handlers, without touching the account's feature.
async fn create_permitted_role(router: &MultiServiceRouter, case: &str) {
    create_role(router, case, "/", FEDERATING_ROLE, &trusted_by_ssm()).await;
    put_role_policy(router, case, FEDERATING_ROLE, &federation_permitted()).await;
}

/// Turn the account's outbound web identity federation off again through IAM's own
/// `DisableOutboundWebIdentityFederation`.
async fn disable_outbound_federation(router: &MultiServiceRouter, case: &str) {
    let (status, reported) =
        iam_answer(router, "DisableOutboundWebIdentityFederation", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the account's feature must be turned off, got {status} with {reported}"
    );
}

/// A create body naming the federating role and a configuration of `tenant`.
fn federating_create_body(display_name: &str, tenant: &str, application: &str) -> Value {
    json!({
        "DisplayName": display_name,
        "RoleArn": format!("arn:aws:iam::{ACCOUNT}:role/{FEDERATING_ROLE}"),
        "ConfigConnectorArn": format!(
            "arn:aws:config:{REGION}:{ACCOUNT}:connector/azure-{tenant}"
        ),
        "Configuration": azure_configuration(tenant, application, 1),
    })
}

/// RULE: the web identity token a connector's federation presents is vended only while the
/// ACCOUNT has outbound web identity federation on, so a create made for an account that has
/// it off is refused with the sentence for that condition and mints nothing — whatever the
/// role it names is permitted.
#[tokio::test]
async fn a_create_needs_the_accounts_outbound_web_identity_federation_on() {
    /// One row: how the account came to the state its feature is in, and whether the create
    /// is served.
    struct Row {
        case: &'static str,
        /// Whether to enable the feature, and whether to disable it again afterwards.
        enabled: bool,
        disabled_again: bool,
        served: bool,
    }

    let rows = vec![
        Row {
            case: "an account that never enabled the feature",
            enabled: false,
            disabled_again: false,
            served: false,
        },
        Row {
            case: "an account that enabled it",
            enabled: true,
            disabled_again: false,
            served: true,
        },
        Row {
            case: "an account that enabled it and turned it off again",
            enabled: true,
            disabled_again: true,
            served: false,
        },
    ];

    for row in rows {
        let case = row.case;
        // A router per row, so each row's account holds only its own answer. IAM is mounted,
        // so the account's record is one that ANSWERS.
        let router = router_with_iam();
        create_permitted_role(&router, case).await;
        if row.enabled {
            enable_outbound_federation(&router, case).await;
        }
        if row.disabled_again {
            disable_outbound_federation(&router, case).await;
        }

        let (status, body) = call(
            &router,
            "CreateCloudConnector",
            &federating_create_body(case, TENANT, APPLICATION),
        )
        .await;

        if row.served {
            assert_eq!(
                status, 200,
                "case {case:?}: the create must be served, got {status} with {body}"
            );
            assert!(
                body["CloudConnectorId"].as_str().is_some(),
                "case {case:?}: a served create reports an id, got {body}"
            );
        } else {
            assert_eq!(
                status, 400,
                "case {case:?}: the create must be refused, got {status} with {body}"
            );
            assert_eq!(
                error_code(&body),
                "ValidationException",
                "case {case:?}: expected a ValidationException, got {body}"
            );
            assert_eq!(
                error_message(&body),
                OUTBOUND_FEDERATION_DISABLED,
                "case {case:?}: expected {OUTBOUND_FEDERATION_DISABLED:?}, got {:?}",
                error_message(&body)
            );
            let (status, listing) = call(&router, "ListCloudConnectors", &json!({})).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the listing must be served, got {status} with {listing}"
            );
            assert_eq!(
                listing["CloudConnectors"].as_array().map(Vec::len),
                Some(0),
                "case {case:?}: a refused create mints nothing, got {listing}"
            );
        }
    }
}

/// RULE: the same answer reaches the two operations that put the question about a connector
/// already held — an update replacing the configuration is refused with that sentence and
/// writes nothing, and a validation reports it as the `ERROR` finding of that step, scoped
/// to the tenant the connector targets.
#[tokio::test]
async fn an_account_that_turns_the_feature_off_blocks_the_update_and_is_reported() {
    let case = "an account that turns the feature off after the connector was minted";
    let router = router_with_iam();
    create_permitted_role(&router, case).await;
    enable_outbound_federation(&router, case).await;

    let (status, created) = call(
        &router,
        "CreateCloudConnector",
        &federating_create_body("the connector under update", TENANT, APPLICATION),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the create must be served, got {status} with {created}"
    );
    let id = created["CloudConnectorId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a served create reports an id, got {created}"))
        .to_string();

    // While the feature is on, the validation reports no finding for this step.
    let (status, validated) = call(
        &router,
        "ValidateCloudConnector",
        &json!({ "CloudConnectorId": id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the validation must be served, got {status} with {validated}"
    );
    assert!(
        !codes_of(&validated).contains(&OUTBOUND_FEDERATION_FINDING_CODE.to_string()),
        "case {case:?}: an account holding the feature on reports no \
         {OUTBOUND_FEDERATION_FINDING_CODE} finding, got {validated}"
    );

    disable_outbound_federation(&router, case).await;

    let (status, refused) = call(
        &router,
        "UpdateCloudConnector",
        &json!({
            "CloudConnectorId": id,
            "DisplayName": "renamed under a disabled account",
            "Configuration": azure_configuration(OTHER_TENANT, OTHER_APPLICATION, 0),
        }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: the update must be refused, got {status} with {refused}"
    );
    assert_eq!(
        error_message(&refused),
        OUTBOUND_FEDERATION_DISABLED,
        "case {case:?}: expected {OUTBOUND_FEDERATION_DISABLED:?}, got {:?}",
        error_message(&refused)
    );

    let (status, read) = call(
        &router,
        "GetCloudConnector",
        &json!({ "CloudConnectorId": id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the connector must still be readable, got {status} with {read}"
    );
    let azure = &read["Configuration"]["AzureConfiguration"];
    assert_eq!(
        (
            azure["TenantId"].as_str(),
            azure["ApplicationId"].as_str(),
            read["DisplayName"].as_str(),
        ),
        (
            Some(TENANT),
            Some(APPLICATION),
            Some("the connector under update"),
        ),
        "case {case:?}: a refused update writes nothing, got {read}"
    );

    let (status, validated) = call(
        &router,
        "ValidateCloudConnector",
        &json!({ "CloudConnectorId": id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the validation must still be served, got {status} with {validated}"
    );
    let reported: Vec<(String, String, String, String)> = validated["ValidationFindings"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|finding| {
            (
                finding["Type"].as_str().unwrap_or_default().to_string(),
                finding["Code"].as_str().unwrap_or_default().to_string(),
                finding["Message"].as_str().unwrap_or_default().to_string(),
                finding["Scope"]["Id"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    assert!(
        reported.contains(&(
            "ERROR".to_string(),
            OUTBOUND_FEDERATION_FINDING_CODE.to_string(),
            OUTBOUND_FEDERATION_DISABLED.to_string(),
            TENANT.to_string(),
        )),
        "case {case:?}: expected an ERROR {OUTBOUND_FEDERATION_FINDING_CODE} finding for the \
         tenant, got {reported:?}"
    );
}

/// The `Code` of each finding a validation reported.
fn codes_of(body: &Value) -> Vec<String> {
    body["ValidationFindings"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|finding| finding["Code"].as_str().unwrap_or_default().to_string())
        .collect()
}
