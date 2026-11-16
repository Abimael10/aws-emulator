// SPDX-License-Identifier: CC0-1.0

//! A PROBE of the one thing a monitored deployment's reads must not do: move.
//!
//! The rule under test: bringing a deployment up to what its environment's monitors already
//! made due is REPORTING — the breach fell due at an instant that had passed, so it is
//! materialised once and stays materialised, however often the deployment is read. So a
//! condemned deployment read many times carries exactly ONE `ROLLBACK_STARTED` and one
//! `ROLLBACK_COMPLETED` entry, keeps the instants those entries were logged at, and reports
//! the same terminal state at every look.
//!
//! The rows span the two ways a monitor condemns a deployment — an alarming alarm and a
//! monitor role AppConfig cannot assume — because each is applied by the same path and each
//! would duplicate its event if that path ran twice. The suite is named for the one
//! invariant it probes rather than for an operation: every operation that reports a
//! deployment drives it.

use serde_json::{json, Value};

use crate::appconfig_workflow::{create_application, create_profile, json_call};
use crate::monitored_deployment_rollback::{
    emulator, environment_with_monitors, given_alarm, given_role, given_strategy, host,
    settled_with, start, APPCONFIG_PRINCIPAL, OTHER_PRINCIPAL, STATE_ALARM, STATE_OK,
};

/// How many times each row re-reads its settled deployment. More than one, because what the
/// rule forbids is the SECOND read moving anything; small, because a read that is idempotent
/// twice is idempotent.
const READS: usize = 5;

/// Which monitor condemns the row's deployment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Condemned {
    /// By an alarm held in its alarming state.
    ByAnAlarm,
    /// By a monitor role whose trust policy withholds the grant from AppConfig.
    ByAnUnreadableRole,
}

/// A deployment AppConfig's monitors condemned reports the same rollback at every look: the
/// reads that found it wrote nothing of their own.
#[tokio::test]
async fn reading_a_condemned_deployment_leaves_its_rollback_exactly_as_it_was() {
    let (router, _engine) = emulator();
    let assumable = given_role(&router, "probe-monitor", APPCONFIG_PRINCIPAL).await;
    let unassumable = given_role(&router, "probe-not-monitor", OTHER_PRINCIPAL).await;
    let application = create_application(&router, "monitor-read-probe").await;
    let profile = create_profile(&router, &application, "probed").await;
    let version = host(&router, &application, &profile, br#"{"enabled":true}"#).await;
    let strategy = given_strategy(&router, "probe-bake").await;

    for (index, condemned) in [Condemned::ByAnAlarm, Condemned::ByAnUnreadableRole]
        .into_iter()
        .enumerate()
    {
        let case = match condemned {
            Condemned::ByAnAlarm => "condemned by an alarming alarm",
            Condemned::ByAnUnreadableRole => "condemned by an unreadable monitor role",
        };
        let alarm_name = format!("probe-alarm-{index}");
        let (state_value, role) = match condemned {
            Condemned::ByAnAlarm => (STATE_ALARM, assumable.as_str()),
            Condemned::ByAnUnreadableRole => (STATE_OK, unassumable.as_str()),
        };
        let alarm = given_alarm(&router, &alarm_name, state_value).await;
        let environment = environment_with_monitors(
            &router,
            &application,
            &format!("probed-{index}"),
            &[json!({ "AlarmArn": alarm, "AlarmRoleArn": role })],
        )
        .await;
        let number = start(
            &router, case, &application, &environment, &profile, &strategy, &version,
        )
        .await;
        let held = [(alarm_name, state_value)];
        let settled = settled_with(
            &router,
            case,
            &application,
            &environment,
            number,
            &held,
        )
        .await;
        assert_eq!(
            settled.get("State").and_then(Value::as_str),
            Some("ROLLED_BACK"),
            "case {case:?}: the row's monitor must condemn the deployment, got {settled}"
        );

        for round in 1..=READS {
            let again = json_call(
                &router,
                "GET",
                &format!(
                    "/applications/{application}/environments/{environment}\
                     /deployments/{number}"
                ),
                &Value::Null,
            )
            .await;
            assert_eq!(
                again.status,
                200,
                "case {case:?} (read {round}): GetDeployment must answer 200, got {} / {}",
                again.status,
                String::from_utf8_lossy(&again.body)
            );
            assert_eq!(
                again.json(),
                settled,
                "case {case:?} (read {round}): a report must leave the deployment exactly as \
                 the clock had already left it"
            );
        }
    }
}
