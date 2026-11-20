// SPDX-License-Identifier: CC0-1.0

//! WHEN the `none` clause of the documented `notifyOnCaseSeverity` rule applies to an
//! `UpdateSlackChannelConfiguration` request.
//!
//! The model words that clause over the request on both counts
//! (`smithy_query(query="doc", shape="UpdateSlackChannelConfigurationRequest", member="notifyOnCaseSeverity",
//! model_path="services/service-com-amazonaws-supportapp-supportapp/smithy-model/com-amazonaws-supportapp-supportapp.json")`):
//! "If you specify `none`, any of the following parameters that you specify in your
//! request must be `false`". So the clause is in scope only for a request that itself
//! carries that severity, and then only over the notification flags that same request
//! carries — an update that carries no severity keeps the stored one ("If you don't
//! specify these parameters in your request, the Amazon Web Services Support App uses
//! the current values by default") and says nothing the clause constrains, whatever
//! severity the record happens to hold.
//!
//! The rows differ along what that rule generalises over: which severity the request
//! carries (none at all, or `none` itself), which of the three notification flags the
//! request turns on or off and how many of them, and which severity and flags the
//! stored record carries — so both directions into a configuration that holds severity
//! `none` beside a true flag are covered: specifying the severity over a record whose
//! flag is already true, and turning a flag on over a record whose severity is already
//! `none`. [`crate::update_severity_rule_scope`] sweeps the whole finite input space of
//! the same rule; these rows pin the antecedent on its own.
//!
//! Every assertion goes over the wire through the production router (the harness in
//! [`crate::slack_channel_configurations`]), and every row reads the configuration back
//! through `ListSlackChannelConfigurations`, so an answer that is not persisted cannot
//! pass.

use serde_json::{json, Value};

use crate::slack_channel_configurations::{role_arn, Emulator};
use crate::update_severity_rule_scope::{assert_configuration, FLAG_MEMBERS};

const TEAM_ID: &str = "T012ABCDEFG";
const CHANNEL_ID: &str = "C01234A5BCD";

/// One row: a stored configuration, the update applied to it, and the answer the
/// documented clause gives.
#[derive(Debug, Clone, Copy)]
struct Row {
    label: &'static str,
    /// The stored record, as a create this API admits establishes it.
    stored_severity: &'static str,
    stored_flags: [bool; 3],
    /// The severity the update carries, `None` when it carries no severity member.
    request_severity: Option<&'static str>,
    /// The three flags the update carries, in the order the request declares them;
    /// `None` is a member the request leaves out.
    request_flags: [Option<bool>; 3],
    /// The status the clause gives this update, 200 or the 400 of the modelled
    /// `ValidationException`.
    expected_status: u16,
    /// The severity and flags the configuration HOLDS afterwards — for a refused row
    /// the stored ones, since a refused update writes nothing.
    expected_severity: &'static str,
    expected_flags: [bool; 3],
}

const ROWS: [Row; 12] = [
    // The clause's antecedent is NOT met: the request carries no severity, so whatever
    // it says about the flags is outside the clause, whatever the record holds.
    Row {
        label: "a flag turned on over a record stored at none, no severity in the request",
        stored_severity: "none",
        stored_flags: [false, false, false],
        request_severity: None,
        request_flags: [None, None, Some(true)],
        expected_status: 200,
        expected_severity: "none",
        expected_flags: [false, false, true],
    },
    Row {
        label: "the create-or-reopen flag turned on over a record stored at none",
        stored_severity: "none",
        stored_flags: [false, false, false],
        request_severity: None,
        request_flags: [Some(true), None, None],
        expected_status: 200,
        expected_severity: "none",
        expected_flags: [true, false, false],
    },
    Row {
        label: "the add-correspondence flag turned on over a record stored at none",
        stored_severity: "none",
        stored_flags: [false, false, false],
        request_severity: None,
        request_flags: [None, Some(true), None],
        expected_status: 200,
        expected_severity: "none",
        expected_flags: [false, true, false],
    },
    Row {
        label: "all three flags turned on over a record stored at none",
        stored_severity: "none",
        stored_flags: [false, false, false],
        request_severity: None,
        request_flags: [Some(true), Some(true), Some(true)],
        expected_status: 200,
        expected_severity: "none",
        expected_flags: [true, true, true],
    },
    Row {
        label: "one flag turned on and another turned off, no severity in the request",
        stored_severity: "none",
        stored_flags: [false, false, false],
        request_severity: None,
        request_flags: [Some(false), None, Some(true)],
        expected_status: 200,
        expected_severity: "none",
        expected_flags: [false, false, true],
    },
    Row {
        label: "a flag turned off over a record stored at none, no severity in the request",
        stored_severity: "none",
        stored_flags: [false, false, false],
        request_severity: None,
        request_flags: [None, None, Some(false)],
        expected_status: 200,
        expected_severity: "none",
        expected_flags: [false, false, false],
    },
    Row {
        label: "a flag turned on over a record stored at high, no severity in the request",
        stored_severity: "high",
        stored_flags: [true, false, false],
        request_severity: None,
        request_flags: [None, None, Some(true)],
        expected_status: 200,
        expected_severity: "high",
        expected_flags: [true, false, true],
    },
    // The antecedent IS met: the request carries `none`, so the flags it carries are in
    // the clause's scope and each of them must be false.
    Row {
        label: "severity none in the request with the resolve flag turned on",
        stored_severity: "none",
        stored_flags: [false, false, false],
        request_severity: Some("none"),
        request_flags: [None, None, Some(true)],
        expected_status: 400,
        expected_severity: "none",
        expected_flags: [false, false, false],
    },
    Row {
        label: "severity none in the request with a stored true flag restated true",
        stored_severity: "all",
        stored_flags: [true, true, true],
        request_severity: Some("none"),
        request_flags: [Some(true), None, None],
        expected_status: 400,
        expected_severity: "all",
        expected_flags: [true, true, true],
    },
    Row {
        label: "severity none in the request with no flag at all",
        stored_severity: "none",
        stored_flags: [false, false, false],
        request_severity: Some("none"),
        request_flags: [None, None, None],
        expected_status: 200,
        expected_severity: "none",
        expected_flags: [false, false, false],
    },
    Row {
        label: "severity none in the request over a record notifying on everything",
        stored_severity: "all",
        stored_flags: [true, true, true],
        request_severity: Some("none"),
        request_flags: [None, None, None],
        expected_status: 200,
        expected_severity: "none",
        expected_flags: [true, true, true],
    },
    Row {
        label: "severity none in the request with one stored flag turned off",
        stored_severity: "all",
        stored_flags: [true, true, true],
        request_severity: Some("none"),
        request_flags: [Some(false), None, None],
        expected_status: 200,
        expected_severity: "none",
        expected_flags: [false, true, true],
    },
];

impl Row {
    /// The create that establishes this row's stored record. Every fixture is a
    /// configuration a create of this API admits, so the row starts from a state the
    /// emulator itself reaches.
    fn create_body(&self) -> Value {
        let mut body = json!({
            "teamId": TEAM_ID,
            "channelId": CHANNEL_ID,
            "notifyOnCaseSeverity": self.stored_severity,
            "channelRoleArn": role_arn("SupportAppRole"),
        });
        for (member, flag) in FLAG_MEMBERS.into_iter().zip(self.stored_flags) {
            body[member] = json!(flag);
        }
        body
    }

    /// The update this row sends: the identifying pair, plus only the members it
    /// carries.
    fn update_body(&self) -> Value {
        let mut body = json!({
            "teamId": TEAM_ID,
            "channelId": CHANNEL_ID,
        });
        if let Some(severity) = self.request_severity {
            body["notifyOnCaseSeverity"] = json!(severity);
        }
        for (member, flag) in FLAG_MEMBERS.into_iter().zip(self.request_flags) {
            if let Some(value) = flag {
                body[member] = json!(value);
            }
        }
        body
    }
}

#[tokio::test]
async fn the_none_clause_applies_only_to_an_update_that_specifies_that_severity() {
    for row in ROWS {
        let case = row.label;
        let emulator = Emulator::new();
        let created = emulator
            .create_in_registered_workspace(row.create_body())
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: the fixture configuration must be created, got {} / {}",
            created.status, created.body
        );

        let reply = emulator.update(row.update_body()).await;
        assert_eq!(
            reply.status, row.expected_status,
            "case {case:?}: expected HTTP {}, got {} / {}",
            row.expected_status, reply.status, reply.body
        );
        if row.expected_status == 400 {
            assert_eq!(
                reply.error_type, "ValidationException",
                "case {case:?}: expected ValidationException, got {:?}",
                reply.error_type
            );
        } else {
            assert_configuration(case, &reply.body, row.expected_severity, row.expected_flags);
        }

        // What a later caller sees: the answer above is only trustworthy if the store
        // reports the same configuration, and a refused update must have written
        // nothing.
        let listed = emulator
            .listed(TEAM_ID, CHANNEL_ID)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: expected the configuration to be listed"));
        assert_configuration(case, &listed, row.expected_severity, row.expected_flags);
    }
}

#[tokio::test]
async fn both_directions_reach_the_same_configuration_at_severity_none() {
    // The two ways into a configuration that holds severity `none` beside a true flag:
    // specify the severity over a record whose flag is already true, or turn the flag on
    // over a record whose severity is already `none`. Neither is something the `none`
    // clause constrains, so both must answer the same configuration.
    let case = "specify the severity, then turn the flag on";
    let by_severity = Emulator::new();
    let created = by_severity
        .create_in_registered_workspace(json!({
            "teamId": TEAM_ID,
            "channelId": CHANNEL_ID,
            "notifyOnCaseSeverity": "high",
            "notifyOnResolveCase": true,
            "channelRoleArn": role_arn("SupportAppRole"),
        }))
        .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: the fixture configuration must be created, got {} / {}",
        created.status, created.body
    );
    let moved_to_none = by_severity
        .update(json!({
            "teamId": TEAM_ID,
            "channelId": CHANNEL_ID,
            "notifyOnCaseSeverity": "none",
        }))
        .await;
    assert_eq!(
        moved_to_none.status, 200,
        "case {case:?}: expected HTTP 200, got {} / {}",
        moved_to_none.status, moved_to_none.body
    );

    let case = "turn the flag on over a record already at severity none";
    let by_flag = Emulator::new();
    let created = by_flag
        .create_in_registered_workspace(json!({
            "teamId": TEAM_ID,
            "channelId": CHANNEL_ID,
            "notifyOnCaseSeverity": "none",
            "channelRoleArn": role_arn("SupportAppRole"),
        }))
        .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: the fixture configuration must be created, got {} / {}",
        created.status, created.body
    );
    let flag_turned_on = by_flag
        .update(json!({
            "teamId": TEAM_ID,
            "channelId": CHANNEL_ID,
            "notifyOnResolveCase": true,
        }))
        .await;
    assert_eq!(
        flag_turned_on.status, 200,
        "case {case:?}: expected HTTP 200, got {} / {}",
        flag_turned_on.status, flag_turned_on.body
    );

    for (case, emulator) in [
        ("specify the severity, then turn the flag on", &by_severity),
        (
            "turn the flag on over a record already at severity none",
            &by_flag,
        ),
    ] {
        let listed = emulator
            .listed(TEAM_ID, CHANNEL_ID)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: expected the configuration to be listed"));
        assert_configuration(case, &listed, "none", [false, false, true]);
    }
}
