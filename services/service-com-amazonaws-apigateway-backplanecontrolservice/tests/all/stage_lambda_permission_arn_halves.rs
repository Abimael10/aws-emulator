// SPDX-License-Identifier: CC0-1.0

//! WHICH grants on a proxy integration's function admit a deployed stage's request, over
//! every half of the method ARN such a grant may be narrowed by.
//!
//! The rule under test, one level above the recorded case:
//!
//! > API Gateway invokes a proxy integration's function as its own service principal, so the
//! > FUNCTION's resource policy decides each request — judged against the ARN of the METHOD
//! > the request reached,
//! > `arn:aws:execute-api:{region}:{account}:{api-id}/{stage}/{verb}/{path}`, and against the
//! > account that owns the invoking api. A grant admits the request only where EVERY half of
//! > the ARN it names matches that one: a narrowing naming another Region, account, api,
//! > stage, verb or path admits nothing, and a wildcard half admits every value of it. A
//! > request no grant admits is answered through the api's `API_CONFIGURATION_ERROR` gateway
//! > response (`500` with `{"message":"Internal server error"}`) with the function never
//! > invoked, and the stage's execution log records the step that stopped it.
//!
//! The suite beside this one ([`crate::stage_scoped_lambda_permission`]) drives the STAGE and
//! PATH halves of that ARN, which is the half of the rule the recorded case shows. This one
//! drives the halves it leaves: the Region, the account, the api id, the verb, a stage name
//! the grant names only a PROPER PREFIX of, and the `SourceAccount` narrowing that names no
//! ARN at all — each row driven through BOTH stages of one deployment, so a grant's reach
//! over the two is read off the same wiring.
//!
//! The wiring, the grants and the per-leg check are that suite's: one api, one function, one
//! deployment released to two stages, and one reading of what a served and a refused leg look
//! like. Only the METHOD differs — it is put on the verb the requests carry rather than on the
//! `ANY` catch-all, so the verb the method ARN names is the request's own and the verb half of
//! a grant can be varied against it.

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::stage_scoped_lambda_permission::{
    apigw, emulator, grant, grant_narrowed, member, one_leg, revoke, wired, FIRST_STAGE, REGION,
    RESOURCE_PATH, SECOND_STAGE,
};

/// The verb the method is put on, which is the verb every request below carries and therefore
/// the verb the method ARN each request is judged against names.
const VERB: &str = "GET";

/// A verb the method is NOT addressed by, for the row that narrows a grant to it.
const OTHER_VERB: &str = "POST";

/// The Region the api lives in — the one the method ARN names — is the sibling suite's
/// [`REGION`], the Region its own helpers address every call in; this is one it does not live
/// in, for the row that narrows a grant to another Region.
const OTHER_REGION: &str = "eu-west-1";

/// An account that is not the one this emulator owns its resources under, from the account
/// AWS's own policy documentation writes its examples with.
const OTHER_ACCOUNT: &str = "111122223333";

/// How a grant on the function is NARROWED: `AddPermission` takes `SourceArn`, "for Amazon Web
/// Services services, the ARN of the Amazon Web Services resource that invokes the function",
/// and `SourceAccount`, "the ID of the Amazon Web Services account that owns the resource" —
/// so a grant to a service principal is narrowed by the invoking RESOURCE, by the account that
/// owns it, or by neither.
enum Narrowed {
    /// A `SourceArn`, as a template over the halves of the method ARN (see [`resolved`]).
    SourceArn(&'static str),
    /// A `SourceAccount`: the account the invoking api belongs to.
    SourceAccount(&'static str),
}

/// One row: the grants the function carries, and which of the two stages they admit.
struct Row {
    case: &'static str,
    narrowings: &'static [Narrowed],
    /// Whether a request through [`FIRST_STAGE`] is invoked.
    first_invoked: bool,
    /// Whether a request through [`SECOND_STAGE`] is invoked.
    second_invoked: bool,
}

/// The rows: one half of the method ARN varied per row, plus the two narrowings that name an
/// account rather than a resource.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the full method ARN of the first stage, with no wildcard at all",
            narrowings: &[Narrowed::SourceArn(
                "arn:aws:execute-api:{region}:{account}:{api}/{first}/{verb}/{path}",
            )],
            first_invoked: true,
            second_invoked: false,
        },
        Row {
            case: "the verb half naming the verb the requests carry, over every stage",
            narrowings: &[Narrowed::SourceArn(
                "arn:aws:execute-api:{region}:{account}:{api}/*/{verb}/{path}",
            )],
            first_invoked: true,
            second_invoked: true,
        },
        Row {
            case: "the verb half naming a verb the requests do not carry",
            narrowings: &[Narrowed::SourceArn(
                "arn:aws:execute-api:{region}:{account}:{api}/*/{other-verb}/{path}",
            )],
            first_invoked: false,
            second_invoked: false,
        },
        Row {
            case: "the api-id half naming another api of this account",
            narrowings: &[Narrowed::SourceArn(
                "arn:aws:execute-api:{region}:{account}:{other-api}/*/*/*",
            )],
            first_invoked: false,
            second_invoked: false,
        },
        Row {
            case: "the stage half naming a proper prefix of the stage reached",
            narrowings: &[Narrowed::SourceArn(
                "arn:aws:execute-api:{region}:{account}:{api}/{first-prefix}/{verb}/{path}",
            )],
            first_invoked: false,
            second_invoked: false,
        },
        Row {
            case: "the Region half naming another Region",
            narrowings: &[Narrowed::SourceArn(
                "arn:aws:execute-api:{other-region}:{account}:{api}/*/*/*",
            )],
            first_invoked: false,
            second_invoked: false,
        },
        Row {
            case: "the account half naming another account",
            narrowings: &[Narrowed::SourceArn(
                "arn:aws:execute-api:{region}:{other-account}:{api}/*/*/*",
            )],
            first_invoked: false,
            second_invoked: false,
        },
        Row {
            case: "narrowed by the account that owns the api, naming no resource at all",
            narrowings: &[Narrowed::SourceAccount("{account}")],
            first_invoked: true,
            second_invoked: true,
        },
        Row {
            case: "narrowed by another account",
            narrowings: &[Narrowed::SourceAccount("{other-account}")],
            first_invoked: false,
            second_invoked: false,
        },
        Row {
            case: "one grant naming another api beside one naming this api's first stage",
            narrowings: &[
                Narrowed::SourceArn("arn:aws:execute-api:{region}:{account}:{other-api}/*/*/*"),
                Narrowed::SourceArn(
                    "arn:aws:execute-api:{region}:{account}:{api}/{first}/{verb}/{path}",
                ),
            ],
            first_invoked: true,
            second_invoked: false,
        },
    ]
}

/// `template` with every half of the method ARN filled in from this run's own records.
///
/// `{first-prefix}` is a PROPER PREFIX of the first stage's name, which is a value no segment
/// of the method ARN equals: a stage name is matched as the whole segment it is.
fn resolved(template: &str, api: &str, other_api: &str) -> String {
    let first_prefix = &FIRST_STAGE[..FIRST_STAGE.len() - 1];
    template
        .replace("{region}", REGION)
        .replace("{other-region}", OTHER_REGION)
        .replace("{account}", engine::emulator_account_id())
        .replace("{other-account}", OTHER_ACCOUNT)
        .replace("{api}", api)
        .replace("{other-api}", other_api)
        .replace("{first-prefix}", first_prefix)
        .replace("{first}", FIRST_STAGE)
        .replace("{second}", SECOND_STAGE)
        .replace("{other-verb}", OTHER_VERB)
        .replace("{verb}", VERB)
        .replace("{path}", RESOURCE_PATH)
}

/// Grant `apigateway.amazonaws.com` `lambda:InvokeFunction` on the function, narrowed by
/// `SourceAccount` rather than by a source ARN.
async fn grant_by_account(
    router: &MultiServiceRouter,
    case: &str,
    statement: &str,
    account: &str,
) {
    grant_narrowed(router, case, statement, &[("SourceAccount", account)]).await;
}

/// Another REST api of the same account, for the rows whose grant names an api the requests
/// do not address. It carries no method of its own: only its ID is under test.
async fn another_api(router: &MultiServiceRouter) -> String {
    let created = apigw(
        router,
        "CreateRestApi (the api the grants do not address)",
        "POST",
        "/restapis",
        Some(json!({ "name": "arn-halves-other-api" })),
        201,
    )
    .await;
    member("CreateRestApi", &created, "id")
}

/// A grant admits a deployed stage's request only where every half of the method ARN it names
/// — the Region, the account, the api, the stage, the verb and the path — matches the ARN of
/// the method that request reached, and a request no grant admits is answered through the
/// api's configuration-error gateway response with the refusal in that stage's execution log.
#[tokio::test]
async fn every_half_of_the_method_arn_a_grant_names_has_to_match_the_request() {
    assert_ne!(
        OTHER_ACCOUNT,
        engine::emulator_account_id(),
        "the account this suite narrows a grant to must not be the one the api belongs to"
    );

    let router = emulator();
    let api = wired(&router, VERB).await;
    let other_api = another_api(&router).await;

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        for (position, narrowing) in row.narrowings.iter().enumerate() {
            let statement = &format!("apigateway-invoke-{position}");
            match narrowing {
                Narrowed::SourceArn(template) => {
                    grant(
                        &router,
                        case,
                        statement,
                        &resolved(template, &api, &other_api),
                    )
                    .await;
                }
                Narrowed::SourceAccount(account) => {
                    grant_by_account(
                        &router,
                        case,
                        statement,
                        &resolved(account, &api, &other_api),
                    )
                    .await;
                }
            }
        }

        for (stage, invoked) in [
            (FIRST_STAGE, row.first_invoked),
            (SECOND_STAGE, row.second_invoked),
        ] {
            // This request's own marker, which is how its run — if it was made at all — is
            // found among the runs the function's group already holds.
            let marker = format!("h{index}-{stage}");
            one_leg(&router, &api, case, stage, &marker, invoked).await;
        }

        for position in 0..row.narrowings.len() {
            revoke(&router, case, &format!("apigateway-invoke-{position}")).await;
        }
    }
}
