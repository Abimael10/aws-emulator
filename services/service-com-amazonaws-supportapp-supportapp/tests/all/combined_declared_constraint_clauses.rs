// SPDX-License-Identifier: CC0-1.0

//! ONE counted envelope for a request whose SEVERAL members are each outside a constraint
//! the model declares for them.
//!
//! THE RULE UNDER TEST: when more than one member of a single request is outside its own
//! declared constraint, the request is refused with ONE standard validation envelope — `N
//! validation errors detected: ` over one clause per violation, in the order the model
//! declares the members — and never with whichever clause some layer happened to read
//! first. The count and the clause order are therefore a property of the REQUEST, not of
//! which reading of a constraint caught which member.
//!
//! Why both layers have to be driven: this service applies part of several declared
//! constraints by hand, because the request boundary's centralized constraint walk cannot
//! apply them in full — `teamId` and `channelId` declare `^\S+$` and the walk's `\s` misses
//! U+FEFF, `channelName` declares `^.+$` and the walk's `.` excludes a line feed alone, and
//! `channelRoleArn` came off that boundary whole for the pass-role step, declared `@length`
//! maximum included. A request can therefore be refused by the boundary's own engine or by
//! the rule a handler applies, and the rule above says the two answer the same shape: the
//! same clause count, for the same members, in the same order.
//!
//! The table is that rule rather than one recorded request: its rows are the subsets of the
//! members this service judges by hand — each member alone, the pair, each id with the name,
//! the pair with the name, the name with the role ARN, and all four at once — crossed with
//! the layer that reads the constraint, and every row is driven on BOTH writes
//! (`CreateSlackChannelConfiguration` and `UpdateSlackChannelConfiguration`), which are
//! required to answer it identically. A subset naming `channelRoleArn` is driven on the
//! handler layer only: that member has no boundary rule left to break, so such a request
//! would mix the two layers, and the boundary answers before any handler runs.
//!
//! Nothing is recorded or rewritten for any row: a refused create stores no configuration
//! and a refused update leaves the stored one exactly as it stands.

use serde_json::{json, Value};

use crate::error_message_templates::refusal;
use crate::slack_channel_configurations::{role_arn, Emulator};

/// The `(teamId, channelId)` the fixture configuration is stored under, and the valid value
/// of each member a row does not spoil.
const TEAM_ID: &str = "T012ABCDEFG";
const CHANNEL_ID: &str = "C01234A5BCD";
const CHANNEL_NAME: &str = "aws-support";
const ROLE_NAME: &str = "SupportAppRole";

/// The expression `teamId` and `channelId` declare, as a clause STATES it: the model's
/// `^\S+$` (`smithy_query(query="shape",shape="teamId,channelId")`) without the anchor pair
/// this service drops from a reported expression. Written out here so the text this suite
/// expects is not read back off the code under test.
const SLACK_ID_STATED_PATTERN: &str = r"\S+";

/// The same for `channelName`'s `^.+$` (`smithy_query(query="shape",shape="channelName")`).
const CHANNEL_NAME_STATED_PATTERN: &str = r".+";

/// `channelRoleArn`'s declared `@length` maximum (`roleArn`, `len 31..2048`,
/// `smithy_query(query="operation",operation="CreateSlackChannelConfiguration")`), stated
/// here for the same reason.
const ROLE_ARN_MAX_LEN: usize = 2048;

/// The members of a channel write whose declared constraints this service applies by hand,
/// in the order the model declares them — which is the order a clause about each of them
/// appears in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Member {
    TeamId,
    ChannelId,
    ChannelName,
    ChannelRoleArn,
}

/// Those members in model-declaration order, stated once.
const MEMBERS_IN_MODEL_ORDER: [Member; 4] = [
    Member::TeamId,
    Member::ChannelId,
    Member::ChannelName,
    Member::ChannelRoleArn,
];

impl Member {
    /// The member path a clause about it names, in the lowerCamel spelling the envelope
    /// states a member with.
    fn path(self) -> &'static str {
        match self {
            Member::TeamId => "teamId",
            Member::ChannelId => "channelId",
            Member::ChannelName => "channelName",
            Member::ChannelRoleArn => "channelRoleArn",
        }
    }

    /// The value the request carries for it when the row leaves the member alone.
    fn admitted_value(self) -> String {
        match self {
            Member::TeamId => TEAM_ID.to_string(),
            Member::ChannelId => CHANNEL_ID.to_string(),
            Member::ChannelName => CHANNEL_NAME.to_string(),
            Member::ChannelRoleArn => role_arn(ROLE_NAME),
        }
    }
}

/// Which reading of a declared constraint a row's values are caught by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layer {
    /// Characters the request boundary's own constraint walk already rejects: a space is
    /// outside `\S` and a line feed is outside `.` for that walk's engine too.
    Boundary,
    /// Values only the rule this service applies by hand rejects: U+FEFF, which that
    /// walk's `\s` does not hold, the line separator U+2028, which its `.` admits, and a
    /// `channelRoleArn` past the declared maximum, the whole member having come off the
    /// boundary.
    Handler,
}

impl Layer {
    fn label(self) -> &'static str {
        match self {
            Layer::Boundary => "caught by the request boundary",
            Layer::Handler => "caught by the rule the handler applies",
        }
    }
}

/// A `channelRoleArn` of the caller's own account whose whole value is one character past
/// the declared maximum, padded in the role name — the one part of the member's form that
/// is free. The account is the caller's, so the value is refused for its SIZE and not by
/// the pass-role attempt it never reaches.
fn role_arn_one_past_the_maximum() -> String {
    let prefix = format!("arn:aws:iam::{}:role/", engine::EMULATOR_ACCOUNT_ID);
    let padding = (ROLE_ARN_MAX_LEN + 1)
        .checked_sub(prefix.chars().count())
        .expect("the declared maximum is longer than the ARN prefix it pads");
    format!("{prefix}{}", "R".repeat(padding))
}

/// The value a row submits for `member` to put it outside its declared constraint at
/// `layer`, and the clause that value draws — or `None` when that layer has no rule of that
/// member left to break.
fn outside_its_constraint(member: Member, layer: Layer) -> Option<(String, String)> {
    let value = match (member, layer) {
        (Member::TeamId, Layer::Boundary) => "T012 ABCDEFG".to_string(),
        (Member::TeamId, Layer::Handler) => "T012\u{FEFF}ABCDEFG".to_string(),
        (Member::ChannelId, Layer::Boundary) => "C0123 45".to_string(),
        (Member::ChannelId, Layer::Handler) => "C0123\u{FEFF}45".to_string(),
        (Member::ChannelName, Layer::Boundary) => "aws\nsupport".to_string(),
        (Member::ChannelName, Layer::Handler) => "aws\u{2028}support".to_string(),
        // `channelRoleArn` is off the request boundary altogether, so the boundary has no
        // rule of this member to catch anything with.
        (Member::ChannelRoleArn, Layer::Boundary) => return None,
        (Member::ChannelRoleArn, Layer::Handler) => role_arn_one_past_the_maximum(),
    };
    let clause = match member {
        Member::TeamId | Member::ChannelId => pattern_clause(member, &value, SLACK_ID_STATED_PATTERN),
        Member::ChannelName => pattern_clause(member, &value, CHANNEL_NAME_STATED_PATTERN),
        Member::ChannelRoleArn => format!(
            "Value '{value}' at '{path}' failed to satisfy constraint: Member must have length \
             less than or equal to {ROLE_ARN_MAX_LEN}",
            path = member.path()
        ),
    };
    Some((value, clause))
}

/// The clause a value outside its member's declared `@pattern` is reported in.
fn pattern_clause(member: Member, value: &str, stated_pattern: &str) -> String {
    format!(
        "Value '{value}' at '{path}' failed to satisfy constraint: Member must satisfy regular \
         expression pattern: {stated_pattern}",
        path = member.path()
    )
}

/// The AWS validation envelope over `clauses`: counted, and the clauses joined in the order
/// they were collected.
fn envelope(clauses: &[String]) -> String {
    if clauses.len() == 1 {
        return format!("1 validation error detected: {}", clauses[0]);
    }
    format!(
        "{} validation errors detected: {}",
        clauses.len(),
        clauses.join("; ")
    )
}

/// One row: which members are outside their declared constraint, and which layer reads it.
#[derive(Debug)]
struct Case {
    label: String,
    spoiled: Vec<Member>,
    layer: Layer,
}

impl Case {
    /// The value this row submits for every member, spoiled or not.
    fn submitted(&self, member: Member) -> String {
        if self.spoiled.contains(&member) {
            outside_its_constraint(member, self.layer)
                .expect("a row only spoils a member its layer can catch")
                .0
        } else {
            member.admitted_value()
        }
    }

    /// The whole message both writes owe this row: one envelope, one clause per spoiled
    /// member, in the order the model declares the members.
    fn expected_message(&self) -> String {
        let clauses: Vec<String> = MEMBERS_IN_MODEL_ORDER
            .iter()
            .filter(|member| self.spoiled.contains(member))
            .map(|member| {
                outside_its_constraint(*member, self.layer)
                    .expect("a row only spoils a member its layer can catch")
                    .1
            })
            .collect();
        envelope(&clauses)
    }

    /// The request body, which carries every member this rule reads.
    fn body(&self) -> Value {
        json!({
            "teamId": self.submitted(Member::TeamId),
            "channelId": self.submitted(Member::ChannelId),
            "channelName": self.submitted(Member::ChannelName),
            "notifyOnCaseSeverity": "none",
            "channelRoleArn": self.submitted(Member::ChannelRoleArn),
        })
    }
}

/// The subsets of the by-hand-judged members a row spoils: each member alone, the pair, each
/// id with the name, the pair with the name, the name with the role ARN, and all four.
fn spoiled_subsets() -> Vec<(&'static str, Vec<Member>)> {
    vec![
        ("teamId alone", vec![Member::TeamId]),
        ("channelId alone", vec![Member::ChannelId]),
        ("channelName alone", vec![Member::ChannelName]),
        ("channelRoleArn alone", vec![Member::ChannelRoleArn]),
        ("the whole pair", vec![Member::TeamId, Member::ChannelId]),
        (
            "teamId and channelName",
            vec![Member::TeamId, Member::ChannelName],
        ),
        (
            "channelId and channelName",
            vec![Member::ChannelId, Member::ChannelName],
        ),
        (
            "the pair and channelName",
            vec![Member::TeamId, Member::ChannelId, Member::ChannelName],
        ),
        (
            "channelName and channelRoleArn",
            vec![Member::ChannelName, Member::ChannelRoleArn],
        ),
        (
            "every by-hand member at once",
            vec![
                Member::TeamId,
                Member::ChannelId,
                Member::ChannelName,
                Member::ChannelRoleArn,
            ],
        ),
    ]
}

/// The whole table: every subset at every layer that can catch all of its members.
fn cases() -> Vec<Case> {
    let mut cases = Vec::new();
    for layer in [Layer::Boundary, Layer::Handler] {
        for (label, spoiled) in spoiled_subsets() {
            if spoiled
                .iter()
                .any(|member| outside_its_constraint(*member, layer).is_none())
            {
                continue;
            }
            cases.push(Case {
                label: format!("{label}, {}", layer.label()),
                spoiled,
                layer,
            });
        }
    }
    cases
}

/// The member paths a message's clauses name, in the order they appear — the shape of the
/// answer, with the values the clauses quote left out.
fn clause_members(message: &str) -> Vec<String> {
    message
        .split("; ")
        .filter_map(|clause| clause.split_once(" at '"))
        .filter_map(|(_, rest)| rest.split_once('\''))
        .map(|(member, _)| member.to_string())
        .collect()
}

/// An emulator holding one stored configuration under the fixture pair, which every row's
/// refusal must leave exactly as it stands.
async fn with_fixture_configuration(case: &str) -> Emulator {
    let emulator = Emulator::new();
    let created = emulator
        .create_in_registered_workspace(json!({
            "teamId": TEAM_ID,
            "channelId": CHANNEL_ID,
            "channelName": CHANNEL_NAME,
            "notifyOnCaseSeverity": "none",
            "channelRoleArn": role_arn(ROLE_NAME),
        }))
        .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: expected the fixture create to answer HTTP 200, got {} / {}",
        created.status, created.body
    );
    emulator
}

/// What the listing reports for the fixture pair: the channel name and the role ARN it holds.
async fn stored_fixture(emulator: &Emulator) -> Option<(String, String)> {
    emulator.listed(TEAM_ID, CHANNEL_ID).await.map(|listed| {
        let member = |name: &str| {
            listed
                .get(name)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        (member("channelName"), member("channelRoleArn"))
    })
}

#[tokio::test]
async fn a_create_names_every_member_outside_its_declared_constraint() {
    for case in cases() {
        let label = &case.label;
        let expected = case.expected_message();
        let emulator = Emulator::new();

        // The workspace is registered under the VALID team id, so a row that spoils
        // `teamId` is still refused for its own values and not for a missing workspace.
        let registered = emulator.register_workspace(TEAM_ID).await;
        assert_eq!(
            registered.status, 200,
            "case {label:?}: expected the workspace registration to answer HTTP 200, got {} / {}",
            registered.status, registered.body
        );

        let reply = emulator.create(case.body()).await;
        let message = refusal(label, &reply, 400, "ValidationException");
        assert_eq!(
            message, expected,
            "case {label:?}: expected {expected:?}, got {message:?}"
        );

        // And nothing was recorded for a refused create, under the submitted pair or the
        // valid one.
        let stored = stored_fixture(&emulator).await;
        assert!(
            stored.is_none(),
            "case {label:?}: expected a refused create to record nothing, got {stored:?}"
        );
    }
}

#[tokio::test]
async fn an_update_answers_a_create_identically_and_rewrites_nothing() {
    let untouched = (CHANNEL_NAME.to_string(), role_arn(ROLE_NAME));
    for case in cases() {
        let label = &case.label;
        let expected = case.expected_message();
        let emulator = with_fixture_configuration(label).await;

        let reply = emulator.update(case.body()).await;
        let message = refusal(label, &reply, 400, "ValidationException");
        assert_eq!(
            message, expected,
            "case {label:?} (update): expected {expected:?}, got {message:?}"
        );

        let stored = stored_fixture(&emulator).await;
        assert_eq!(
            stored,
            Some(untouched.clone()),
            "case {label:?} (update): expected the stored configuration to be untouched, got \
             {stored:?}"
        );
    }
}

#[tokio::test]
async fn both_layers_answer_one_subset_of_members_the_same_shape() {
    // The same members outside the same declared constraints, read by the two different
    // layers: the clause count and the members each clause names, in order, are the same on
    // the wire — only the values the clauses quote differ, because that is all the rows
    // differ in.
    for (label, spoiled) in spoiled_subsets() {
        let layers: Vec<Layer> = [Layer::Boundary, Layer::Handler]
            .into_iter()
            .filter(|layer| {
                spoiled
                    .iter()
                    .all(|member| outside_its_constraint(*member, *layer).is_some())
            })
            .collect();
        if layers.len() < 2 {
            continue;
        }

        let expected_members: Vec<String> = MEMBERS_IN_MODEL_ORDER
            .iter()
            .filter(|member| spoiled.contains(member))
            .map(|member| member.path().to_string())
            .collect();
        for layer in layers {
            let case = Case {
                label: format!("{label}, {}", layer.label()),
                spoiled: spoiled.clone(),
                layer,
            };
            let emulator = with_fixture_configuration(&case.label).await;
            let reply = emulator.update(case.body()).await;
            let message = refusal(&case.label, &reply, 400, "ValidationException");
            let named = clause_members(&message);
            assert_eq!(
                named, expected_members,
                "case {label:?} ({}): expected the clauses to name {expected_members:?} in that \
                 order, got {named:?} from {message:?}",
                layer.label()
            );
        }
    }
}
