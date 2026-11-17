// SPDX-License-Identifier: CC0-1.0

//! Validating a cloud connector, and the federation a create resolves before it mints one.
//!
//! Three rules are asserted here:
//!
//! 1. `ValidateCloudConnector` resolves its `CloudConnectorId` BEFORE it validates
//!    anything: an id the endpoint's Region holds no connector under — one nothing created,
//!    one created through another Region, one already deleted — is refused
//!    `ResourceNotFoundException` with the one sentence every operation of this service
//!    that resolves a connector id states, and never answered with an empty finding list
//!    (`smithy_query(query="operation", operation="ValidateCloudConnector")`: the operation
//!    is `@readonly`, paginated over `ValidationFindings`, and models
//!    `ResourceNotFoundException`);
//! 2. the findings it reports for a connector it DOES hold are read out of that connector's
//!    own configuration — the tenant it targets, the application it authenticates as, the
//!    subscriptions it targets — and out of what IAM answers about the role it holds, so a
//!    role that loses the permission the cross-cloud federation needs turns the findings
//!    from `INFO` into the `ERROR` that says so; the list is paged by `MaxResults`
//!    (`range 0..75`) with the token resuming where the page stopped;
//! 3. a create resolves that same federation before it mints a connector: a `RoleArn` IAM
//!    answers may not obtain an Amazon Web Services web identity token is refused HTTP 400
//!    `ValidationException` with the fixed sentence for that condition, and the Region's
//!    connector set is left exactly as it was.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` ([`call_in_region`], [`error_code`], [`error_message`]) — and
//! every role and policy is written through IAM's OWN handlers, reached through the helpers
//! the sibling role suite owns ([`crate::hybrid_activation_role::router_with_iam`],
//! [`crate::hybrid_activation_role::create_role`],
//! [`crate::hybrid_activation_role::iam`]): nothing reaches into either store.
//!
//! The rows span what the rules generalise over: which Region holds the connector and which
//! id is asked about, how many subscriptions a configuration targets, what the role's
//! permissions say about the federation call, and how large a page the request asks for
//! against the published bounds of `MaxResults`.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::hybrid_activation_role::{create_role, iam, router_with_iam};
use crate::parameter_store::{call_in_region, error_code, error_message, ACCOUNT, REGION};

/// A second Region, to pin that a validation resolves the id at the endpoint's Region.
const OTHER_REGION: &str = "us-east-1";

/// The service principal a connector's role has to admit, in the spelling a trust policy
/// names a service by.
const SSM_PRINCIPAL: &str = "ssm.amazonaws.com";

/// The action such a trust policy grants that principal.
const ASSUME_ROLE: &str = "sts:AssumeRole";

/// The action the connector's role has to be permitted for the federation step: the STS
/// call that vends the web identity token the provider's credential is exchanged for.
const WEB_IDENTITY_TOKEN_ACTION: &str = "sts:GetWebIdentityToken";

/// The sentence a connector id that names no connector is refused with, written out here
/// independently of the service: fixed text for the class, naming the resource class and
/// interpolating nothing from the request.
const NOT_FOUND: &str = "CloudConnector not found for account.";

/// The sentence a create whose role cannot obtain that token is refused with, written out
/// here independently of the service: fixed text for the condition, with no variable parts.
const FEDERATION_REFUSED: &str =
    "Unable to obtain an AWS web identity token for cross-cloud federation. Please verify \
     the cloud connector's AWS IAM role is permitted to call sts:GetWebIdentityToken.";

/// What a `NextToken` this read never issued is refused with: fixed text for the class.
const INVALID_NEXT_TOKEN: &str = "The NextToken value is not valid";

/// The published floor of `ValidateCloudConnectorMaxResults`
/// (`smithy_query(query="operation", operation="ValidateCloudConnector")`: range 0..75).
/// Zero is the shape's own default, which is the value a request naming no page size
/// carries, so the smallest page a caller can ASK for is one finding.
const MAX_RESULTS_MIN: i64 = 1;

/// The published ceiling of the same member.
const MAX_RESULTS_MAX: i64 = 75;

/// An id of the form `CloudConnectorId` models (a 36-character lowercase-hex UUID) that
/// this service never minted.
const UNMINTED_ID: &str = "3c9a1f7e-0b2d-4a51-8e6c-7d4b2a9f10c3";

/// A second one, so a case can name two ids nothing created.
const OTHER_UNMINTED_ID: &str = "9f8e7d6c-5b4a-3921-8070-6f5e4d3c2b1a";

/// An Azure tenant id: `AzureConfiguration$TenantId` is a hex UUID.
const TENANT_ID: &str = "22222222-3333-4444-5555-666666666666";

/// See [`TENANT_ID`]: `AzureConfiguration$ApplicationId` has the same shape.
const APPLICATION_ID: &str = "77777777-8888-9999-aaaa-bbbbbbbbbbbb";

/// `AzureSubscription$Id` values, same UUID shape again.
const SUBSCRIPTION_IDS: &[&str] = &[
    "aaaaaaa1-0000-0000-0000-000000000001",
    "aaaaaaa2-0000-0000-0000-000000000002",
    "aaaaaaa3-0000-0000-0000-000000000003",
];

/// The `ValidationFindingType` of a finding reporting a check that passed, and the one
/// reporting a check that failed (`ValidationFindingType`: `INFO | WARN | ERROR`).
const INFO: &str = "INFO";

/// See [`INFO`].
const ERROR: &str = "ERROR";

/// The trust policy of a role this service can act as, in its plainest form.
pub(crate) fn trusted_by_ssm() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": SSM_PRINCIPAL },
            "Action": ASSUME_ROLE,
        }],
    })
}

/// A permissions policy that lets the role it is written for obtain the web identity token
/// the cross-cloud federation is exchanged for.
pub(crate) fn federation_permitted() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": WEB_IDENTITY_TOKEN_ACTION,
            "Resource": "*",
        }],
    })
}

/// The name the inline permissions policy of a role is written under, so a later write
/// under the same name REPLACES what the role is permitted.
pub(crate) const PERMISSIONS_POLICY_NAME: &str = "SsmCloudConnectorFederation";

/// Write `policy` as the inline permissions policy of `role_name` through IAM's own
/// `PutRolePolicy`, under the one policy name above.
pub(crate) async fn put_role_policy(
    router: &MultiServiceRouter,
    case: &str,
    role_name: &str,
    policy: &Value,
) {
    iam(
        router,
        case,
        "PutRolePolicy",
        &[
            ("RoleName", role_name.to_string()),
            ("PolicyName", PERMISSIONS_POLICY_NAME.to_string()),
            ("PolicyDocument", policy.to_string()),
        ],
    )
    .await;
}

/// Create `role_name` trusting this service and permitted to federate, which is the role a
/// served create names.
pub(crate) async fn create_federating_role(
    router: &MultiServiceRouter,
    case: &str,
    role_name: &str,
) {
    create_role(router, case, "/", role_name, &trusted_by_ssm()).await;
    put_role_policy(router, case, role_name, &federation_permitted()).await;
}

/// The ARN of the role `role_name` of this account.
fn role_arn(role_name: &str) -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/{role_name}")
}

/// A create body naming `role_arn`, targeting `subscriptions` of [`SUBSCRIPTION_IDS`].
fn create_body(region: &str, role_arn: &str, subscriptions: &[&str]) -> Value {
    let mut azure = json!({
        "TenantId": TENANT_ID,
        "ApplicationId": APPLICATION_ID,
    });
    if !subscriptions.is_empty() {
        azure["Targets"] = json!({
            "Subscriptions": subscriptions
                .iter()
                .map(|id| json!({ "Id": id }))
                .collect::<Vec<Value>>(),
        });
    }
    json!({
        "DisplayName": "Azure production tenant",
        "RoleArn": role_arn,
        "ConfigConnectorArn": format!("arn:aws:config:{region}:{ACCOUNT}:connector/production"),
        "Configuration": { "AzureConfiguration": azure },
    })
}

/// Create a connector at `region` under a role that trusts this service and may federate,
/// answering its id.
async fn create_connector(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    role_name: &str,
    subscriptions: &[&str],
) -> String {
    create_federating_role(router, case, role_name).await;
    let (status, created) = call_in_region(
        router,
        region,
        "CreateCloudConnector",
        &create_body(region, &role_arn(role_name), subscriptions),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the create must be served, got {status} with {created}"
    );
    created["CloudConnectorId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a served create reports an id, got {created}"))
        .to_string()
}

/// The connector ids `ListCloudConnectors` reports at `region`. Shared with the sibling
/// suite that resolves a connector's role through IAM
/// ([`crate::cloud_connector_role_and_deletion`]).
pub(crate) async fn listed(router: &MultiServiceRouter, region: &str) -> Vec<String> {
    let (status, body) = call_in_region(router, region, "ListCloudConnectors", &json!({})).await;
    assert_eq!(
        status, 200,
        "ListCloudConnectors must be served, got {status} with {body}"
    );
    body["CloudConnectors"]
        .as_array()
        .unwrap_or_else(|| panic!("a listing reports CloudConnectors, got {body}"))
        .iter()
        .map(|summary| {
            summary["CloudConnectorId"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// One finding as a case holds it: its type, its code, and the scope it is reported at.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Finding {
    finding_type: String,
    code: String,
    scope_type: String,
    scope_id: String,
}

/// The findings `body` reports, each read out of the members `ValidationFinding` declares.
fn findings_of(case: &str, body: &Value) -> Vec<Finding> {
    body["ValidationFindings"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: a validation reports ValidationFindings, got {body}"))
        .iter()
        .map(|finding| {
            let scope = &finding["Scope"];
            assert!(
                finding["Message"]
                    .as_str()
                    .is_some_and(|message| !message.is_empty()),
                "case {case:?}: every finding describes itself, got {finding}"
            );
            Finding {
                finding_type: finding["Type"].as_str().unwrap_or_default().to_string(),
                code: finding["Code"].as_str().unwrap_or_default().to_string(),
                scope_type: scope["Type"].as_str().unwrap_or_default().to_string(),
                scope_id: scope["Id"].as_str().unwrap_or_default().to_string(),
            }
        })
        .collect()
}

/// The findings a healthy connector targeting `subscriptions` reports: the tenant it
/// targets, then one finding per subscription in the order its configuration lists them.
fn healthy_findings(subscriptions: &[&str]) -> Vec<Finding> {
    let mut expected = vec![Finding {
        finding_type: INFO.to_string(),
        code: "TenantSummary".to_string(),
        scope_type: "azure:tenant".to_string(),
        scope_id: TENANT_ID.to_string(),
    }];
    for subscription in subscriptions {
        expected.push(Finding {
            finding_type: INFO.to_string(),
            code: "SubscriptionAccessible".to_string(),
            scope_type: "azure:subscription".to_string(),
            scope_id: (*subscription).to_string(),
        });
    }
    expected
}

/// RULE: a validation resolves the connector id first — an id the endpoint's Region holds
/// no connector under is refused `ResourceNotFoundException` with the one sentence for the
/// class, and no finding list is answered for it.
#[tokio::test]
async fn a_validation_is_refused_for_an_id_that_names_no_connector() {
    let case = "one connector held here and one held elsewhere";
    let router = router_with_iam();
    let held = create_connector(&router, case, REGION, "SsmHeldRole", &SUBSCRIPTION_IDS[..1]).await;
    let elsewhere =
        create_connector(&router, case, OTHER_REGION, "SsmElsewhereRole", &[]).await;
    // One connector created here and deleted again, so an id that WAS minted names nothing.
    let deleted = create_connector(&router, case, REGION, "SsmDeletedRole", &[]).await;
    let (status, body) = call_in_region(
        &router,
        REGION,
        "DeleteCloudConnector",
        &json!({ "CloudConnectorId": deleted }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the delete must be served, got {status} with {body}"
    );

    // (case, the id the validation names)
    let rows: Vec<(&str, String)> = vec![
        ("an id nothing created", UNMINTED_ID.to_string()),
        ("another id nothing created", OTHER_UNMINTED_ID.to_string()),
        ("an id this service minted and deleted", deleted.clone()),
        ("an id created through another Region", elsewhere.clone()),
    ];

    for (case, id) in rows {
        let (status, body) = call_in_region(
            &router,
            REGION,
            "ValidateCloudConnector",
            &json!({ "CloudConnectorId": id }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: the validation must be refused, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {body}"
        );
        assert_eq!(
            error_message(&body),
            NOT_FOUND,
            "case {case:?}: expected {NOT_FOUND:?}, got {:?}",
            error_message(&body)
        );
        assert!(
            body["ValidationFindings"].is_null(),
            "case {case:?}: a refused validation reports no findings, got {body}"
        );
    }

    // The connector this Region does hold still validates, so the refusals above are about
    // the ids and not about the operation.
    let (status, body) = call_in_region(
        &router,
        REGION,
        "ValidateCloudConnector",
        &json!({ "CloudConnectorId": held }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the held connector must validate, got {status} with {body}"
    );
}

/// RULE: the findings report the connector's own configuration — the tenant it targets and
/// each subscription it targets, in the order the configuration lists them.
#[tokio::test]
async fn a_validation_reports_the_targets_the_connector_holds() {
    // (case, the subscriptions the configuration targets)
    let rows: Vec<(&str, &[&str])> = vec![
        ("a tenant-level connector targeting no subscription", &[]),
        ("a connector targeting one subscription", &SUBSCRIPTION_IDS[..1]),
        ("a connector targeting two subscriptions", &SUBSCRIPTION_IDS[..2]),
        ("a connector targeting three subscriptions", SUBSCRIPTION_IDS),
    ];

    for (case, subscriptions) in rows {
        // A router per row, so the Region holds only this row's connector.
        let router = router_with_iam();
        let id = create_connector(&router, case, REGION, "SsmValidatedRole", subscriptions).await;
        let (status, body) = call_in_region(
            &router,
            REGION,
            "ValidateCloudConnector",
            &json!({ "CloudConnectorId": id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the validation must be served, got {status} with {body}"
        );
        assert_eq!(
            findings_of(case, &body),
            healthy_findings(subscriptions),
            "case {case:?}: expected the connector's own targets to be reported"
        );
        assert!(
            body["NextToken"].is_null(),
            "case {case:?}: every finding fits one page, so no token is handed back, got {body}"
        );
    }
}

/// RULE, grounded: a finding rests on what IAM ANSWERS about the connector's role, so a
/// role that loses the federation permission after the connector was minted validates with
/// the error that says so — and every target it holds is reported as one it cannot reach.
#[tokio::test]
async fn a_validation_reports_what_iam_answers_about_the_role() {
    let case = "a role whose federation permission is withdrawn";
    let router = router_with_iam();
    let subscriptions = &SUBSCRIPTION_IDS[..2];
    let id = create_connector(&router, case, REGION, "SsmFederatingRole", subscriptions).await;

    // While the role may federate, the findings are the healthy ones.
    let (status, body) = call_in_region(
        &router,
        REGION,
        "ValidateCloudConnector",
        &json!({ "CloudConnectorId": id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the validation must be served, got {status} with {body}"
    );
    assert_eq!(
        findings_of(case, &body),
        healthy_findings(subscriptions),
        "case {case:?}: a role permitted to federate validates clean"
    );

    // The role's one permissions policy is replaced by a document that permits something
    // else, so IAM now answers that the federation call is not allowed.
    put_role_policy(
        &router,
        case,
        "SsmFederatingRole",
        &json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Action": "ssm:DescribeParameters",
                "Resource": "*",
            }],
        }),
    )
    .await;

    let (status, body) = call_in_region(
        &router,
        REGION,
        "ValidateCloudConnector",
        &json!({ "CloudConnectorId": id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the validation must still be served, got {status} with {body}"
    );
    let mut expected = vec![
        Finding {
            finding_type: INFO.to_string(),
            code: "TenantSummary".to_string(),
            scope_type: "azure:tenant".to_string(),
            scope_id: TENANT_ID.to_string(),
        },
        Finding {
            finding_type: ERROR.to_string(),
            code: "WebIdentityTokenFailed".to_string(),
            scope_type: "azure:tenant".to_string(),
            scope_id: TENANT_ID.to_string(),
        },
    ];
    for subscription in subscriptions {
        expected.push(Finding {
            finding_type: ERROR.to_string(),
            code: "TargetInaccessible".to_string(),
            scope_type: "azure:subscription".to_string(),
            scope_id: (*subscription).to_string(),
        });
    }
    assert_eq!(
        findings_of(case, &body),
        expected,
        "case {case:?}: expected the federation failure and every target it makes unreachable"
    );
    // The finding carries AWS's own sentence for that condition.
    let reported = body["ValidationFindings"][1]["Message"]
        .as_str()
        .unwrap_or_default();
    assert_eq!(
        reported, FEDERATION_REFUSED,
        "case {case:?}: expected {FEDERATION_REFUSED:?}, got {reported:?}"
    );
}

/// RULE: the findings are paged by `MaxResults`, and the token a page hands back resumes
/// where it stopped — so a walk at any page size reports the same findings in the same
/// order as one unbounded page.
#[tokio::test]
async fn a_validation_pages_its_findings() {
    let case = "a connector targeting three subscriptions";
    let router = router_with_iam();
    let id = create_connector(&router, case, REGION, "SsmPagedRole", SUBSCRIPTION_IDS).await;
    let whole = healthy_findings(SUBSCRIPTION_IDS);

    // The page sizes a request may ask for, spanning the published bounds of `MaxResults`
    // and the absent member: one finding, the page that stops mid-list, the exact length of
    // the list, the published ceiling, and no member at all.
    let rows: Vec<(&str, Option<i64>)> = vec![
        ("the published floor", Some(MAX_RESULTS_MIN)),
        ("a page shorter than the list", Some(2)),
        ("a page as long as the list", Some(whole.len() as i64)),
        ("the published ceiling", Some(MAX_RESULTS_MAX)),
        ("no MaxResults at all", None),
        ("the shape's own default", Some(0)),
    ];

    for (case, max_results) in rows {
        let mut walked: Vec<Finding> = Vec::new();
        let mut next_token: Option<String> = None;
        // Every row reports four findings, so a walk that takes more pages than that at the
        // smallest page size is looping rather than paging.
        for page in 0..=whole.len() {
            let mut body = json!({ "CloudConnectorId": id });
            if let Some(max_results) = max_results {
                body["MaxResults"] = json!(max_results);
            }
            if let Some(token) = &next_token {
                body["NextToken"] = json!(token);
            }
            let (status, response) =
                call_in_region(&router, REGION, "ValidateCloudConnector", &body).await;
            assert_eq!(
                status, 200,
                "case {case:?}: page {page} must be served, got {status} with {response}"
            );
            let reported = findings_of(case, &response);
            let bound = max_results.filter(|asked| *asked > 0).unwrap_or(MAX_RESULTS_MAX);
            assert!(
                reported.len() as i64 <= bound,
                "case {case:?}: page {page} must hold at most {bound} findings, got {}",
                reported.len()
            );
            walked.extend(reported);
            next_token = response["NextToken"].as_str().map(str::to_string);
            if next_token.is_none() {
                break;
            }
        }
        assert!(
            next_token.is_none(),
            "case {case:?}: the walk must end without a token, got {next_token:?}"
        );
        assert_eq!(
            walked, whole,
            "case {case:?}: a paged walk reports the same findings as one page"
        );
    }

    // A token this read never issued names no page of it to resume.
    for (case, token) in [
        ("a token past the findings", "9"),
        ("a token that is no position", "not-a-position"),
    ] {
        let (status, body) = call_in_region(
            &router,
            REGION,
            "ValidateCloudConnector",
            &json!({ "CloudConnectorId": id, "NextToken": token }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: the token must be refused, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected a ValidationException, got {body}"
        );
        assert_eq!(
            error_message(&body),
            INVALID_NEXT_TOKEN,
            "case {case:?}: expected {INVALID_NEXT_TOKEN:?}, got {:?}",
            error_message(&body)
        );
    }
}

/// RULE: a create resolves the connector's cross-cloud federation before it mints
/// anything — a role IAM answers may not obtain the web identity token is refused
/// `ValidationException` with the fixed sentence for that condition, and nothing is minted.
#[tokio::test]
async fn a_create_is_refused_for_a_role_that_cannot_obtain_a_web_identity_token() {
    /// One row: what the role's permissions say, and whether the create is served.
    struct Row {
        case: &'static str,
        /// The inline permissions policy to write for the role, or `None` to write none.
        permissions: Option<Value>,
        served: bool,
    }

    let rows = vec![
        Row {
            case: "a role permitted to obtain the token",
            permissions: Some(federation_permitted()),
            served: true,
        },
        Row {
            case: "a role permitted to obtain the token among other actions",
            permissions: Some(json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Action": ["ssm:DescribeParameters", WEB_IDENTITY_TOKEN_ACTION],
                    "Resource": "*",
                }],
            })),
            served: true,
        },
        Row {
            case: "a role carrying no permissions policy at all",
            permissions: None,
            served: false,
        },
        Row {
            case: "a role permitted other actions only",
            permissions: Some(json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Action": "ssm:DescribeParameters",
                    "Resource": "*",
                }],
            })),
            served: false,
        },
        Row {
            case: "a role explicitly denied the token",
            permissions: Some(json!({
                "Version": "2012-10-17",
                "Statement": [
                    {
                        "Effect": "Allow",
                        "Action": WEB_IDENTITY_TOKEN_ACTION,
                        "Resource": "*",
                    },
                    {
                        "Effect": "Deny",
                        "Action": WEB_IDENTITY_TOKEN_ACTION,
                        "Resource": "*",
                    },
                ],
            })),
            served: false,
        },
    ];

    for row in rows {
        let case = row.case;
        // A router per row, so each row's IAM store holds only its own role.
        let router = router_with_iam();
        let role = "SsmFederationRole";
        create_role(&router, case, "/", role, &trusted_by_ssm()).await;
        if let Some(permissions) = row.permissions.as_ref() {
            put_role_policy(&router, case, role, permissions).await;
        }

        let before = listed(&router, REGION).await;
        let (status, body) = call_in_region(
            &router,
            REGION,
            "CreateCloudConnector",
            &create_body(REGION, &role_arn(role), &SUBSCRIPTION_IDS[..1]),
        )
        .await;
        if row.served {
            assert_eq!(
                status, 200,
                "case {case:?}: the create must be served, got {status} with {body}"
            );
            let id = body["CloudConnectorId"].as_str().unwrap_or_else(|| {
                panic!("case {case:?}: a served create reports an id, got {body}")
            });
            assert_eq!(
                listed(&router, REGION).await,
                vec![id.to_string()],
                "case {case:?}: the minted connector is reported"
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
                FEDERATION_REFUSED,
                "case {case:?}: expected {FEDERATION_REFUSED:?}, got {:?}",
                error_message(&body)
            );
            assert_eq!(
                listed(&router, REGION).await,
                before,
                "case {case:?}: a refused create leaves the Region's connectors as they were"
            );
        }
    }
}
