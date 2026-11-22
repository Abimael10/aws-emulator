// SPDX-License-Identifier: CC0-1.0

//! The SENTENCES this service refuses three request faults with, over the wire.
//!
//! RULES under test, one per test below:
//!
//! 1. A `LaunchConfigurationName` is resolved against the launch configurations the account
//!    HOLDS, and a name none answers to is refused with ONE sentence, whichever operation
//!    resolved it: HTTP 400 / `ValidationError` carrying the fixed
//!    `Launch configuration name not found - ` text and, behind it, the statement that no
//!    configuration of that name exists. The name the request submitted is the sentence's
//!    only variable part.
//! 2. `CheckpointPercentages` states thresholds "in ascending order. Each number must be
//!    unique" (`smithy_query(query="doc",shape="RefreshPreferences",
//!    member="CheckpointPercentages")`), and a list that is not strictly ascending is refused
//!    with the sentence stating that RULE — fixed text with no variable part, since the
//!    refusal is about the ordering rather than about the numbers submitted — and no refresh
//!    is recorded.
//! 3. `DescribeInstanceRefreshes` refuses a `NextToken` it did not itself issue with the
//!    present-tense sentence about the token the caller is holding, quoting it and
//!    TERMINATING the sentence with a period, as that read's own recorded refusal states
//!    it: the token is the only variable part. How the sentence ENDS is each read's own, so
//!    the reads that share the open form are left to their own filings.
//!
//! Every sentence is written out here rather than read off the code under test: the WORDING
//! is the contract, and a test re-using the implementation's own literal would assert
//! nothing.
//!
//! The rows span what each rule generalises over — the OPERATION that resolves the name and
//! the FORM of the name, the SHAPE of the ordering fault (a pair, a repeat, a middle
//! threshold, a tail repeat, a wholly descending list), and the FORM of the token — and each
//! test stands beside a request the service SERVES, so no row passes because the operation
//! refuses everything.

use smithy_server_runtime::MultiServiceRouter;

use crate::support::{
    borrowed, new_engine, p, query, router_with, tag_value, tag_values, xml_unescape, REGION,
};

/// The launch template the group of these suites launches from, one the harness already put
/// in the account's own Amazon EC2 store.
const TEMPLATE: &str = "lt-1";

/// The group these suites read and update: created with no instances, so a refresh asked for
/// on it replaces nothing and the suite's subject stays the refusal wording.
const GROUP: &str = "asg-refusal-wording";

/// The whole sentence a request naming a launch configuration the account holds none under is
/// refused with, whose one variable part is the name the request submitted.
fn absent_launch_configuration_message(name: &str) -> String {
    format!("Launch configuration name not found - A launch configuration with the name: {name} does not exist")
}

/// The whole sentence a `CheckpointPercentages` list that is not strictly ascending is
/// refused with: fixed text with no variable part at all.
const UNSORTED_CHECKPOINTS_MESSAGE: &str =
    "Checkpoint percentages must be sorted in ascending order with no duplicates.";

/// One emulator with this service behind it, holding the group above.
async fn seeded() -> MultiServiceRouter {
    let router = router_with(new_engine());
    let (status, body) = query(
        &router,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", GROUP),
            ("LaunchTemplate.LaunchTemplateName", TEMPLATE),
            ("MinSize", "0"),
            ("MaxSize", "1"),
            ("DesiredCapacity", "0"),
            ("AvailabilityZones.member.1", &format!("{REGION}a")),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "the group these suites read must be created, got {status}: {body}"
    );
    router
}

/// Assert `body` is a refusal at HTTP 400 carrying `code` and exactly `message`.
fn assert_refusal(case: &str, status: u16, body: &str, code: &str, message: &str) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status}: {body}"
    );
    let answered_code = tag_value(body, "Code");
    assert_eq!(
        answered_code, code,
        "case {case:?}: expected the {code} code, got {answered_code:?} in {body}"
    );
    let answered_message = xml_unescape(&tag_value(body, "Message"));
    assert_eq!(
        answered_message, message,
        "case {case:?}: expected the message {message:?}, got {answered_message:?}"
    );
}

/// How an operation names a launch configuration, and the other members it needs beside it.
enum Resolver {
    /// `DeleteLaunchConfiguration`, whose whole subject is the name.
    Delete,
    /// `CreateAutoScalingGroup`, which launches the new group from the configuration.
    Create,
    /// `UpdateAutoScalingGroup`, which moves an existing group onto it.
    Update,
}

impl Resolver {
    /// The action this operation is called by.
    fn action(&self) -> &'static str {
        match self {
            Self::Delete => "DeleteLaunchConfiguration",
            Self::Create => "CreateAutoScalingGroup",
            Self::Update => "UpdateAutoScalingGroup",
        }
    }

    /// The request that names `configuration` to this operation.
    fn params(&self, configuration: &str) -> Vec<(String, String)> {
        match self {
            Self::Delete => vec![p("LaunchConfigurationName", configuration)],
            Self::Create => vec![
                p("AutoScalingGroupName", format!("{GROUP}-new")),
                p("LaunchConfigurationName", configuration),
                p("MinSize", "0"),
                p("MaxSize", "1"),
                p("DesiredCapacity", "0"),
                p("AvailabilityZones.member.1", format!("{REGION}a")),
            ],
            Self::Update => vec![
                p("AutoScalingGroupName", GROUP),
                p("LaunchConfigurationName", configuration),
            ],
        }
    }
}

/// RULE 1: every operation that resolves a `LaunchConfigurationName` refuses a name the
/// account holds no configuration under with the same sentence, naming that name.
#[tokio::test]
async fn an_absent_launch_configuration_is_refused_with_one_sentence_everywhere() {
    let router = seeded().await;

    // `LaunchConfigurationName` is `XmlStringMaxLen255`, so the names span that length range
    // from one character to its top, and the forms a name may take inside the member's
    // pattern — punctuation, and characters the answer's envelope has to escape.
    let longest = "n".repeat(255);
    let names: [(&str, &str); 5] = [
        ("a plain name", "lc-never-created"),
        ("a name carrying punctuation", "lc.1_2-3+4"),
        ("a single character", "x"),
        ("a name at the top of the modelled length range", &longest),
        ("a name carrying XML metacharacters", "lc<&'\"name>"),
    ];

    for (name_case, name) in names {
        for resolver in [Resolver::Delete, Resolver::Create, Resolver::Update] {
            let action = resolver.action();
            let case = format!("{action} with {name_case}");
            let params = resolver.params(name);
            let (status, body) = query(&router, action, &borrowed(&params)).await;
            assert_refusal(
                &case,
                status,
                &body,
                "ValidationError",
                &absent_launch_configuration_message(name),
            );
        }
    }

    // The control: the refusals above changed nothing — the group is still held, still
    // launches from its launch template, and the create that was refused left no group
    // behind.
    let (status, body) = query(&router, "DescribeAutoScalingGroups", &[]).await;
    assert_eq!(
        status, 200,
        "DescribeAutoScalingGroups must answer 200, got {status}: {body}"
    );
    assert_eq!(
        tag_values(&body, "AutoScalingGroupName"),
        vec![GROUP.to_string()],
        "the refused requests must leave the account holding only the seeded group, got {body}"
    );
    assert_eq!(
        tag_value(&body, "LaunchTemplateName"),
        TEMPLATE,
        "the refused update must leave the group on its launch template, got {body}"
    );
}

/// The `Preferences.CheckpointPercentages.member.N` parameters that state `thresholds`.
fn checkpoint_params(thresholds: &[i32]) -> Vec<(String, String)> {
    let mut params = vec![p("AutoScalingGroupName", GROUP)];
    for (index, threshold) in thresholds.iter().enumerate() {
        params.push(p(
            &format!("Preferences.CheckpointPercentages.member.{}", index + 1),
            threshold.to_string(),
        ));
    }
    params
}

/// How many instance refreshes the group has on record.
async fn recorded_refreshes(router: &MultiServiceRouter, case: &str) -> usize {
    let (status, body) = query(
        router,
        "DescribeInstanceRefreshes",
        &[("AutoScalingGroupName", GROUP)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeInstanceRefreshes must answer 200, got {status}: {body}"
    );
    tag_values(&body, "InstanceRefreshId").len()
}

/// RULE 2: checkpoint thresholds that are not strictly ascending are refused with the
/// sentence stating that rule, and the refused refresh is not recorded.
#[tokio::test]
async fn unsorted_checkpoint_percentages_are_refused_with_the_rule_they_broke() {
    let router = seeded().await;

    // The rows differ in the SHAPE of the ordering fault: which pair is out of order, how
    // long the list is, and whether the fault is a repeat or a descent.
    let rows: [(&str, &[i32]); 6] = [
        ("a pair out of order", &[60, 20]),
        ("a threshold repeated", &[50, 50]),
        ("the middle threshold the lowest", &[50, 20, 100]),
        ("the last two thresholds equal", &[10, 20, 40, 40]),
        ("a wholly descending list", &[100, 75, 50, 25]),
        ("the first two thresholds equal, a higher one after", &[25, 25, 100]),
    ];

    for (case, thresholds) in rows {
        let params = checkpoint_params(thresholds);
        let (status, body) = query(&router, "StartInstanceRefresh", &borrowed(&params)).await;
        assert_refusal(
            case,
            status,
            &body,
            "ValidationError",
            UNSORTED_CHECKPOINTS_MESSAGE,
        );
        let recorded = recorded_refreshes(&router, case).await;
        assert_eq!(
            recorded, 0,
            "case {case:?}: a refused refresh must not be recorded, got {recorded} on record"
        );
    }

    // The control: thresholds that ARE strictly ascending start a refresh, so no row above
    // passed because the operation refuses every list.
    let params = checkpoint_params(&[20, 60, 100]);
    let (status, body) = query(&router, "StartInstanceRefresh", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "ascending thresholds must be served, got {status}: {body}"
    );
    let recorded = recorded_refreshes(&router, "ascending thresholds").await;
    assert_eq!(
        recorded, 1,
        "the served refresh must be on record, got {recorded} on record"
    );
}

/// RULE 3: the whole sentence `DescribeInstanceRefreshes` refuses a `NextToken` it did not
/// issue with — the present-tense sentence about the token the caller is holding, terminated
/// by a period — whose one variable part is the token the request submitted.
fn invalid_token_message(token: &str) -> String {
    format!("The token '{token}' is invalid.")
}

#[tokio::test]
async fn a_next_token_this_read_did_not_issue_is_refused_quoting_it() {
    let router = seeded().await;

    // Tokens a client can submit that this read never issued: free text, the marker alone,
    // the first page's own offset (reached with no token at all), an offset past the end of
    // the list, a marker carrying something that is not a number, and text carrying
    // characters the envelope escapes.
    let tokens = [
        "not-a-real-token",
        "next-",
        "next-0",
        "next-9",
        "next-abc",
        "page 2 of 3",
        "<token & 'quoted'>",
    ];

    for token in tokens {
        let case = format!("the token {token:?}");
        let (status, body) = query(
            &router,
            "DescribeInstanceRefreshes",
            &[("AutoScalingGroupName", GROUP), ("NextToken", token)],
        )
        .await;
        assert_refusal(
            &case,
            status,
            &body,
            "InvalidNextToken",
            &invalid_token_message(token),
        );
    }

    // The control: the same read with no token at all is served, so no row above passed
    // because the read refuses everything.
    let (status, body) = query(
        &router,
        "DescribeInstanceRefreshes",
        &[("AutoScalingGroupName", GROUP)],
    )
    .await;
    assert_eq!(
        status, 200,
        "the read with no token must be served, got {status}: {body}"
    );
}
