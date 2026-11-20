// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `UpdateCapacityManagerOrganizationsAccess` — the operation that moves
//! the account's Capacity Manager `OrganizationsAccess` on its own — driven over the wire the
//! service speaks (`ec2Query`, through [`super::ec2_query`]).
//!
//! The rules under test are:
//!
//! 1. SOMETHING TO CHANGE: the update must have something to change, and which of the two
//!    scopes the request asks for decides what:
//!    * asking for cross-account aggregation is a statement about an ORGANIZATION, which is
//!      AWS Organizations' resource, so an account this emulator holds no organization for is
//!      refused 400 `CapacityManager.OrganizationTrustedAccessDisabled` — the half of the rule
//!      the organization's own side of is pinned cross-service in
//!      `core/router/tests/all/ec2_capacity_manager_organization_scope_needs_an_organization.rs`,
//!      where creating one through Organizations opens the success path;
//!    * asking to stop aggregating across the organization while the account is not doing so
//!      leaves nothing to turn off, so it is refused 400 `InvalidParameterValue`;
//!
//!    A refused request stores nothing;
//! 2. ROUND TRIP: where there IS something to change, the update records the scope the request
//!    asked for, answers it read back out of the store, and a later
//!    `GetCapacityManagerAttributes` agrees;
//! 3. ONLY THE SCOPE MOVES: the update neither turns Capacity Manager on nor restarts the
//!    ingestion an enable started, so the status and the ingestion status a report answers are
//!    exactly what they were before it;
//! 4. ACCOUNT SCOPE: the setting is a fact of the ACCOUNT, so an update asked for in one
//!    Region is reported in every other;
//! 5. a `DryRun` request performs nothing and is answered 412 `DryRunOperation`, whether or
//!    not the request it stands for would have been served;
//! 6. `OrganizationsAccess` is `@required`: a request that names no value for it is refused
//!    400 `MissingParameter` naming the member, ahead of the `DryRun` verdict, exactly as
//!    every other absent required member of this service is;
//! 7. a `ClientToken` is held to the bound every client token of this crate is held to
//!    ([`MAX_CLIENT_TOKEN_CHARS`]), and a refused one stores nothing.
//!
//! Each table row is a distinct input along what its rule generalises over — the history
//! behind the account (never enabled, enabled for the account only, enabled for the
//! organization, disabled again), the scope the request asks for, the Region the call was
//! signed for, which other parameters it carries, and how long the token is — so no row is the
//! specification.
//!
//! This suite mounts EC2 alone, as every suite of this crate does, so no organization is
//! reachable from it at all: that is the standalone-account case, and the account whose scope
//! an `EnableCapacityManager` already widened is how a row that needs something to turn off
//! gets there.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use ec2::state::MAX_CLIENT_TOKEN_CHARS;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, error_message};
use super::wire::{param, Param};

/// Regions spanning the partitions and the naming shapes the Region field takes, so no
/// assertion below is a property of one Region string.
const REGIONS: [&str; 4] = ["us-west-2", "eu-central-1", "ap-southeast-1", "us-gov-west-1"];

/// The code EC2 answers a request for cross-account aggregation from an account that cannot
/// reach Capacity Manager through AWS Organizations.
const TRUSTED_ACCESS_DISABLED: &str = "CapacityManager.OrganizationTrustedAccessDisabled";

/// The sentence that refusal carries.
const TRUSTED_ACCESS_DISABLED_MESSAGE: &str = "This account can't access Capacity Manager with \
     AWS Organizations. Enable trusted access or use account mode.";

/// The sentence EC2 refuses a request to stop aggregating across the organization with when the
/// account is not aggregating across it at all.
const NOT_ENABLED_MESSAGE: &str = "Capacity Manager doesn't have organizations access enabled.";

/// The sentence EC2 refuses a request that named no value for this operation's `@required`
/// member with, naming the member as the model declares it.
const MISSING_MEMBER_MESSAGE: &str = "The request must include the OrganizationsAccess \
     parameter. Add the required parameter and retry the request.";

fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

/// One `EnableCapacityManager` call in `region`, with whatever extra parameters the case needs.
async fn enable(router: &MultiServiceRouter, region: &str, extra: &[Param]) -> (u16, String) {
    call(router, region, "EnableCapacityManager", extra).await
}

/// One `UpdateCapacityManagerOrganizationsAccess` call in `region` asking for
/// `organizations_access`, plus whatever extra parameters the case needs.
async fn update(
    router: &MultiServiceRouter,
    region: &str,
    organizations_access: &str,
    extra: &[Param],
) -> (u16, String) {
    let mut params = vec![param("OrganizationsAccess", organizations_access)];
    params.extend_from_slice(extra);
    call(
        router,
        region,
        "UpdateCapacityManagerOrganizationsAccess",
        &params,
    )
    .await
}

/// The settings `GetCapacityManagerAttributes` reports in `region`: the status, the
/// Organizations access flag, the ingestion status (`None` when the response carries none at
/// all), and the whole body so a failing case can name it.
struct Reported {
    status: Option<String>,
    organizations_access: Option<String>,
    ingestion_status: Option<String>,
    body: String,
}

async fn report(router: &MultiServiceRouter, case: &str, region: &str) -> Reported {
    let (status, body) = call(router, region, "GetCapacityManagerAttributes", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetCapacityManagerAttributes in {region}, \
         got {status} with body {body}"
    );
    Reported {
        status: element(&body, "capacityManagerStatus"),
        organizations_access: element(&body, "organizationsAccess"),
        ingestion_status: element(&body, "ingestionStatus"),
        body,
    }
}

/// How an account reached the state a case starts from: the sequence of calls behind it.
#[derive(Debug, Clone, Copy)]
enum Step {
    /// `EnableCapacityManager`, with what the call says about `OrganizationsAccess` (`None` is
    /// a call that says nothing about it at all).
    Enable(Option<bool>),
    /// `DisableCapacityManager`.
    Disable,
    /// `UpdateCapacityManagerOrganizationsAccess` asking for the scope named.
    Update(bool),
}

/// Run `history` in `region`, asserting each call was served, so a case starts from the account
/// state it names.
async fn run_history(router: &MultiServiceRouter, case: &str, region: &str, history: &[Step]) {
    for (index, step) in history.iter().enumerate() {
        let (status, body) = match step {
            Step::Enable(organizations_access) => {
                let extra: Vec<Param> = organizations_access
                    .map(|value| {
                        vec![param(
                            "OrganizationsAccess",
                            if value { "true" } else { "false" },
                        )]
                    })
                    .unwrap_or_default();
                enable(router, region, &extra).await
            }
            Step::Disable => call(router, region, "DisableCapacityManager", &[]).await,
            Step::Update(organizations_access) => {
                update(
                    router,
                    region,
                    if *organizations_access { "true" } else { "false" },
                    &[],
                )
                .await
            }
        };
        assert_eq!(
            status, 200,
            "case {case:?} history step {index} ({step:?}): expected HTTP 200, \
             got {status} with body {body}"
        );
    }
}

/// RULE 1, first half: with no organization for the account to aggregate across, a request
/// asking for cross-account aggregation is refused, whatever history the account has — and the
/// refusal stores nothing.
#[tokio::test]
async fn asking_for_organizations_access_outside_an_organization_is_refused() {
    /// One row: the history behind the account, and the status a report must still answer after
    /// the refusal.
    struct Row {
        case: &'static str,
        history: &'static [Step],
        status_after: &'static str,
    }
    let rows = [
        Row {
            case: "an account that never enabled Capacity Manager",
            history: &[],
            status_after: "disabled",
        },
        Row {
            case: "enabled with no OrganizationsAccess at all",
            history: &[Step::Enable(None)],
            status_after: "enabled",
        },
        Row {
            case: "enabled for the account only",
            history: &[Step::Enable(Some(false))],
            status_after: "enabled",
        },
        Row {
            case: "enabled for the organization, then disabled",
            history: &[Step::Enable(Some(true)), Step::Disable],
            status_after: "disabled",
        },
        Row {
            case: "enabled for the organization, disabled, enabled for the account only",
            history: &[
                Step::Enable(Some(true)),
                Step::Disable,
                Step::Enable(Some(false)),
            ],
            status_after: "enabled",
        },
    ];

    for Row {
        case,
        history,
        status_after,
    } in rows
    {
        let router = emulator();
        let region = REGIONS[0];
        run_history(&router, case, region, history).await;

        let (status, body) = update(&router, region, "true", &[]).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 asking for Organizations access, \
             got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            TRUSTED_ACCESS_DISABLED,
            "case {case:?}: expected {TRUSTED_ACCESS_DISABLED}, got {body}"
        );
        assert_eq!(
            error_message(&body),
            TRUSTED_ACCESS_DISABLED_MESSAGE,
            "case {case:?}: expected {TRUSTED_ACCESS_DISABLED_MESSAGE:?}, got {}",
            error_message(&body)
        );

        let reported = report(&router, case, region).await;
        assert_eq!(
            reported.status.as_deref(),
            Some(status_after),
            "case {case:?}: expected a refused update to leave the status at {status_after:?}, \
             got {}",
            reported.body
        );
        assert_eq!(
            reported.organizations_access.as_deref(),
            Some("false"),
            "case {case:?}: expected a refused update to store nothing, got {}",
            reported.body
        );
    }
}

/// RULE 1, second half: asking to stop aggregating across the organization while the account is
/// not aggregating across it leaves nothing to turn off, so it is refused — whatever history
/// the account has — and the refusal stores nothing.
#[tokio::test]
async fn asking_to_stop_aggregating_when_nothing_is_aggregated_is_refused() {
    /// One row: the history behind the account, and the status a report must still answer after
    /// the refusal.
    struct Row {
        case: &'static str,
        history: &'static [Step],
        status_after: &'static str,
    }
    let rows = [
        Row {
            case: "an account that never enabled Capacity Manager",
            history: &[],
            status_after: "disabled",
        },
        Row {
            case: "enabled for the account only",
            history: &[Step::Enable(Some(false))],
            status_after: "enabled",
        },
        Row {
            case: "enabled for the organization, then disabled",
            history: &[Step::Enable(Some(true)), Step::Disable],
            status_after: "disabled",
        },
        Row {
            case: "enabled for the organization, then narrowed back to the account",
            history: &[Step::Enable(Some(true)), Step::Update(false)],
            status_after: "enabled",
        },
    ];

    for Row {
        case,
        history,
        status_after,
    } in rows
    {
        let router = emulator();
        let region = REGIONS[1];
        run_history(&router, case, region, history).await;

        let (status, body) = update(&router, region, "false", &[]).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 asking to stop aggregating, \
             got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidParameterValue",
            "case {case:?}: expected InvalidParameterValue, got {body}"
        );
        assert_eq!(
            error_message(&body),
            NOT_ENABLED_MESSAGE,
            "case {case:?}: expected {NOT_ENABLED_MESSAGE:?}, got {}",
            error_message(&body)
        );

        let reported = report(&router, case, region).await;
        assert_eq!(
            reported.status.as_deref(),
            Some(status_after),
            "case {case:?}: expected a refused update to leave the status at {status_after:?}, \
             got {}",
            reported.body
        );
        assert_eq!(
            reported.organizations_access.as_deref(),
            Some("false"),
            "case {case:?}: expected a refused update to store nothing, got {}",
            reported.body
        );
    }
}

/// RULES 2 and 3: where there is something to change, the update records the scope the request
/// asked for, answers it read back out of the store, and leaves the status and the ingestion
/// the enable started exactly where they were — and the refusals of this harness leave all
/// three alone.
///
/// The sequence runs over one account whose scope `EnableCapacityManager` widened: narrowing it
/// is served, narrowing it again has nothing to turn off, and widening it again is refused
/// here because this harness mounts no organization (the served widening is pinned
/// cross-service, see this suite's own documentation).
#[tokio::test]
async fn the_update_records_the_scope_it_is_asked_for_and_moves_nothing_else() {
    /// What one step of the sequence expects back.
    #[derive(Debug, Clone, Copy)]
    enum Answer {
        /// HTTP 200, with the scope the update recorded.
        Served(&'static str),
        /// HTTP 400 with this code, and the account left where it was.
        Refused(&'static str),
    }
    /// One step: the scope asked for, what it is answered, and the flag a report must answer
    /// afterwards.
    struct Attempt {
        asks_for: &'static str,
        answer: Answer,
        organizations_access_after: &'static str,
    }
    let steps = [
        // Narrow the scope back to the account: there is something to turn off.
        Attempt {
            asks_for: "false",
            answer: Answer::Served("false"),
            organizations_access_after: "false",
        },
        // Nothing left to turn off.
        Attempt {
            asks_for: "false",
            answer: Answer::Refused("InvalidParameterValue"),
            organizations_access_after: "false",
        },
        // Widen it again: no organization is reachable from this harness.
        Attempt {
            asks_for: "true",
            answer: Answer::Refused(TRUSTED_ACCESS_DISABLED),
            organizations_access_after: "false",
        },
    ];

    let region = REGIONS[2];
    let router = emulator();
    let case = "an account enabled for the organization";
    run_history(&router, case, region, &[Step::Enable(Some(true))]).await;
    let before = report(&router, case, region).await;

    for (index, step) in steps.iter().enumerate() {
        let (status, body) = update(&router, region, step.asks_for, &[]).await;
        match step.answer {
            Answer::Served(wanted) => {
                assert_eq!(
                    status, 200,
                    "case {case:?} step {index} (asking for {}): expected HTTP 200, \
                     got {status} with body {body}",
                    step.asks_for
                );
                assert_eq!(
                    element(&body, "organizationsAccess").as_deref(),
                    Some(wanted),
                    "case {case:?} step {index}: expected organizationsAccess {wanted:?}, \
                     got {body}"
                );
                assert_eq!(
                    element(&body, "capacityManagerStatus").as_deref(),
                    Some("enabled"),
                    "case {case:?} step {index}: expected the status enabled, got {body}"
                );
            }
            Answer::Refused(code) => {
                assert_eq!(
                    status, 400,
                    "case {case:?} step {index} (asking for {}): expected HTTP 400, \
                     got {status} with body {body}",
                    step.asks_for
                );
                assert_eq!(
                    error_code(&body),
                    code,
                    "case {case:?} step {index}: expected {code}, got {body}"
                );
            }
        }

        // Whichever way the call was answered, the account sits where that answer says, and
        // the status and the ingestion the enable started have not moved.
        let expected = step.organizations_access_after;
        let reported = report(&router, case, region).await;
        assert_eq!(
            reported.organizations_access.as_deref(),
            Some(expected),
            "case {case:?} step {index}: expected the stored organizationsAccess {expected:?}, \
             got {}",
            reported.body
        );
        assert_eq!(
            reported.status.as_deref(),
            Some("enabled"),
            "case {case:?} step {index}: expected the status to stay enabled, got {}",
            reported.body
        );
        assert_eq!(
            reported.ingestion_status, before.ingestion_status,
            "case {case:?} step {index}: expected the ingestion status to stay at {:?}, \
             got {:?}",
            before.ingestion_status, reported.ingestion_status
        );
    }
}

/// RULE 4: the setting is a fact of the account, so an update asked for in one Region is
/// reported in every other — including one asked for in a Region other than the enable's.
#[tokio::test]
async fn the_scope_the_update_records_is_reported_in_every_region() {
    for (enabled_in, updated_in) in [
        (REGIONS[0], REGIONS[0]),
        (REGIONS[1], REGIONS[2]),
        (REGIONS[3], REGIONS[1]),
    ] {
        let router = emulator();
        let case = format!("enabled in {enabled_in}, updated in {updated_in}");
        run_history(&router, &case, enabled_in, &[Step::Enable(Some(true))]).await;
        let (status, body) = update(&router, updated_in, "false", &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with body {body}"
        );

        for region in REGIONS {
            let reported = report(&router, &case, region).await;
            assert_eq!(
                reported.organizations_access.as_deref(),
                Some("false"),
                "case {case:?}: expected organizationsAccess false asking in {region}, got {}",
                reported.body
            );
            assert_eq!(
                reported.status.as_deref(),
                Some("enabled"),
                "case {case:?}: expected the status enabled asking in {region}, got {}",
                reported.body
            );
        }
    }
}

/// RULE 5: `DryRun` is answered 412 `DryRunOperation` and leaves the setting where it was —
/// including for a request that would have been refused, since the flag asks about the CALLER
/// rather than about the work.
#[tokio::test]
async fn dry_run_changes_nothing() {
    /// One dry-run row: the history behind the account, the scope the dry run asks for, and the
    /// flag a report must still answer afterwards.
    struct Row {
        case: &'static str,
        history: &'static [Step],
        asks_for: &'static str,
        organizations_access_after: &'static str,
    }
    let rows = [
        Row {
            case: "asking for Organizations access outside an organization",
            history: &[Step::Enable(Some(false))],
            asks_for: "true",
            organizations_access_after: "false",
        },
        Row {
            case: "asking for Organizations access where it is already on",
            history: &[Step::Enable(Some(true))],
            asks_for: "true",
            organizations_access_after: "true",
        },
        Row {
            case: "asking to stop aggregating where the account is",
            history: &[Step::Enable(Some(true))],
            asks_for: "false",
            organizations_access_after: "true",
        },
        Row {
            case: "asking to stop aggregating where there is nothing to stop",
            history: &[],
            asks_for: "false",
            organizations_access_after: "false",
        },
    ];

    for Row {
        case,
        history,
        asks_for,
        organizations_access_after,
    } in rows
    {
        let router = emulator();
        let region = REGIONS[3];
        run_history(&router, case, region, history).await;

        let (status, body) = update(&router, region, asks_for, &[param("DryRun", "true")]).await;
        assert_eq!(
            status, 412,
            "case {case:?}: expected HTTP 412 from a DryRun update, \
             got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "DryRunOperation",
            "case {case:?}: expected DryRunOperation, got {body}"
        );

        let reported = report(&router, case, region).await;
        assert_eq!(
            reported.organizations_access.as_deref(),
            Some(organizations_access_after),
            "case {case:?}: expected a DryRun to leave organizationsAccess at \
             {organizations_access_after:?}, got {}",
            reported.body
        );
    }

    // `DryRun=false` is an ordinary update.
    let router = emulator();
    let region = REGIONS[0];
    let case = "DryRun=false";
    run_history(&router, case, region, &[Step::Enable(Some(true))]).await;
    let (status, body) = update(&router, region, "false", &[param("DryRun", "false")]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with body {body}"
    );
    let reported = report(&router, case, region).await;
    assert_eq!(
        reported.organizations_access.as_deref(),
        Some("false"),
        "case {case:?}: expected the update to be performed, got {}",
        reported.body
    );
}

/// RULE 6: a request that names no value for the `@required` `OrganizationsAccess` is refused
/// `MissingParameter` naming the member, whatever else it carries — a `ClientToken`, the
/// `DryRun` flag whose verdict every absent required member of this service is refused ahead
/// of — and it performs nothing.
#[tokio::test]
async fn a_request_naming_no_scope_is_refused_missing_parameter() {
    /// One row: the parameters the request carries BESIDES the member it leaves out.
    struct Row {
        case: &'static str,
        extra: &'static [(&'static str, &'static str)],
    }
    let rows = [
        Row {
            case: "nothing but the action",
            extra: &[],
        },
        Row {
            case: "a client token",
            extra: &[("ClientToken", "a-token-of-this-request")],
        },
        Row {
            case: "the DryRun flag",
            extra: &[("DryRun", "true")],
        },
        Row {
            case: "a client token and the DryRun flag",
            extra: &[("ClientToken", "another-token"), ("DryRun", "true")],
        },
    ];

    for Row { case, extra } in rows {
        let router = emulator();
        let region = REGIONS[1];
        // The account is aggregating across the organization, so the request would have had
        // something to change had it named a scope: the refusal is about the absent member.
        run_history(&router, case, region, &[Step::Enable(Some(true))]).await;

        let params: Vec<Param> = extra.iter().map(|(name, value)| param(name, value)).collect();
        let (status, body) = call(
            &router,
            region,
            "UpdateCapacityManagerOrganizationsAccess",
            &params,
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for a request naming no scope, \
             got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            ec2::state::MISSING_PARAMETER,
            "case {case:?}: expected a missing-parameter refusal, got {body}"
        );
        assert_eq!(
            error_message(&body),
            MISSING_MEMBER_MESSAGE,
            "case {case:?}: expected {MISSING_MEMBER_MESSAGE:?}, got {}",
            error_message(&body)
        );

        let reported = report(&router, case, region).await;
        assert_eq!(
            reported.organizations_access.as_deref(),
            Some("true"),
            "case {case:?}: expected the refused request to perform nothing, got {}",
            reported.body
        );
    }
}

/// RULE 7: a `ClientToken` is accepted up to [`MAX_CLIENT_TOKEN_CHARS`] and refused one
/// character past it, with nothing stored for the refused request.
#[tokio::test]
async fn a_client_token_is_bounded() {
    let region = REGIONS[0];

    // One character past the published bound: refused, and the setting stays where it was.
    let router = emulator();
    let case = "a token one character past the bound";
    run_history(&router, case, region, &[Step::Enable(Some(true))]).await;
    let too_long = "t".repeat(MAX_CLIENT_TOKEN_CHARS + 1);
    let (status, body) = update(&router, region, "false", &[param("ClientToken", &too_long)]).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "InvalidParameterValue",
        "case {case:?}: expected InvalidParameterValue, got {body}"
    );
    let reported = report(&router, case, region).await;
    assert_eq!(
        reported.organizations_access.as_deref(),
        Some("true"),
        "case {case:?}: expected a refused update to store nothing, got {}",
        reported.body
    );

    // Tokens AT and below the bound are accepted.
    for length in [1, MAX_CLIENT_TOKEN_CHARS / 2, MAX_CLIENT_TOKEN_CHARS] {
        let router = emulator();
        let token = "t".repeat(length);
        let case = format!("a {length}-character token");
        run_history(&router, &case, region, &[Step::Enable(Some(true))]).await;
        let (status, body) =
            update(&router, region, "false", &[param("ClientToken", &token)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with body {body}"
        );
        assert_eq!(
            element(&body, "organizationsAccess").as_deref(),
            Some("false"),
            "case {case:?}: expected organizationsAccess false, got {body}"
        );
        let reported = report(&router, &case, region).await;
        assert_eq!(
            reported.organizations_access.as_deref(),
            Some("false"),
            "case {case:?}: expected the stored organizationsAccess false, got {}",
            reported.body
        );
    }
}
