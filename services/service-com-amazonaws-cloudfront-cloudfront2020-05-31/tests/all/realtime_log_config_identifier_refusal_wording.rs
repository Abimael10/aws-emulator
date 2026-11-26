// SPDX-License-Identifier: CC0-1.0

//! How a MUTATION of a real-time log configuration refuses a pair of identifiers that
//! addresses no single configuration, end to end over HTTP.
//!
//! A configuration is addressed "by its name or its Amazon Resource Name (ARN)", and a
//! mutation changes exactly ONE of them — so a request that names neither addresses none, and
//! a request whose `Name` and `ARN` resolve to DIFFERENT configurations addresses none either.
//! Both are the modelled `InvalidArgument` (400), and which SENTENCE each carries belongs to
//! the OPERATION: `UpdateRealtimeLogConfig` words both of its identifier preconditions in one
//! sentence of its own, while `DeleteRealtimeLogConfig` words them in two, so a client keying
//! on the message can tell which precondition failed and on which call.
//!
//! The READS are the other side of the rule: nothing is written, and the documentation states
//! the precedence for them ("if you provide both, CloudFront uses the name to identify the
//! real-time log configuration to get"), so a pair that disagrees is SERVED there rather than
//! refused.
//!
//! The rows differ along what the rule generalises over: which identifier a mutation carries
//! (neither, an empty element for each, one of them, both agreeing, both disagreeing, and a
//! pair where only one side resolves), which way round the disagreement goes (this record's
//! name beside that record's ARN, and the reverse), the operation the pair travelled to, and
//! the configuration the pair names — the rows are the lifecycle suite's own table rather than
//! a second one. Every refusal is also shown to change nothing: each configuration is still
//! readable afterwards, reporting what the create stored.

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::{
    REALTIME_LOG_CONFIG_IDENTIFIERS_DISAGREE_MESSAGE, REALTIME_LOG_CONFIG_IDENTIFIER_INVALID_MESSAGE,
    REALTIME_LOG_CONFIG_IDENTIFIER_REQUIRED_MESSAGE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{call, emulator, Answer, ACCOUNT_ID, API_VERSION};
use crate::realtime_log_config_lifecycle::{
    assert_reports, config_node, config_table, create_accepted, get_config, Submitted,
};
use crate::response_headers_policy_lifecycle::escape;

/// The sampling rate every update below asks for: any value the published range admits, since
/// what these rows are about is the IDENTIFIERS the request carries and not the configuration
/// it would leave behind.
const ASKED_RATE: i64 = 11;

/// `PUT /2020-05-31/realtime-log-config` naming the configuration by `name`, by `arn`, by both
/// or by neither — each `None` leaves that element out of the document entirely.
///
/// The document carries a `SamplingRate` so the request describes a change the operation could
/// make: a refusal below is then about the identifiers and about nothing else.
async fn update_config(
    router: &MultiServiceRouter,
    name: Option<&str>,
    arn: Option<&str>,
) -> Answer {
    let mut body = format!(
        "<UpdateRealtimeLogConfigRequest \
         xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">"
    );
    if let Some(name) = name {
        body.push_str(&format!("<Name>{}</Name>", escape(name)));
    }
    if let Some(arn) = arn {
        body.push_str(&format!("<ARN>{}</ARN>", escape(arn)));
    }
    body.push_str(&format!(
        "<SamplingRate>{ASKED_RATE}</SamplingRate></UpdateRealtimeLogConfigRequest>"
    ));
    call(
        router,
        "PUT",
        &format!("/{API_VERSION}/realtime-log-config"),
        Some(&body),
        None,
    )
    .await
}

/// `POST /2020-05-31/delete-realtime-log-config`, addressing the configuration the same way.
async fn delete_config(
    router: &MultiServiceRouter,
    name: Option<&str>,
    arn: Option<&str>,
) -> Answer {
    let mut body = format!(
        "<DeleteRealtimeLogConfigRequest \
         xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">"
    );
    if let Some(name) = name {
        body.push_str(&format!("<Name>{}</Name>", escape(name)));
    }
    if let Some(arn) = arn {
        body.push_str(&format!("<ARN>{}</ARN>", escape(arn)));
    }
    body.push_str("</DeleteRealtimeLogConfigRequest>");
    call(
        router,
        "POST",
        &format!("/{API_VERSION}/delete-realtime-log-config"),
        Some(&body),
        None,
    )
    .await
}

/// Which mutation a row drives, and the sentence that operation words each precondition in.
#[derive(Debug, Clone, Copy)]
enum Mutation {
    Update,
    Delete,
}

impl Mutation {
    /// The name this mutation is reported under in an assertion.
    fn case(self) -> &'static str {
        match self {
            Self::Update => "UpdateRealtimeLogConfig",
            Self::Delete => "DeleteRealtimeLogConfig",
        }
    }

    /// Send the mutation.
    async fn call(
        self,
        router: &MultiServiceRouter,
        name: Option<&str>,
        arn: Option<&str>,
    ) -> Answer {
        match self {
            Self::Update => update_config(router, name, arn).await,
            Self::Delete => delete_config(router, name, arn).await,
        }
    }

    /// The sentence this operation refuses a request naming NEITHER identifier with.
    fn no_identifier(self) -> &'static str {
        match self {
            Self::Update => REALTIME_LOG_CONFIG_IDENTIFIER_INVALID_MESSAGE,
            Self::Delete => REALTIME_LOG_CONFIG_IDENTIFIER_REQUIRED_MESSAGE,
        }
    }

    /// The sentence this operation refuses a pair of identifiers that disagree with.
    fn disagreeing(self) -> &'static str {
        match self {
            Self::Update => REALTIME_LOG_CONFIG_IDENTIFIER_INVALID_MESSAGE,
            Self::Delete => REALTIME_LOG_CONFIG_IDENTIFIERS_DISAGREE_MESSAGE,
        }
    }
}

/// The configurations the rows address, created through the API: `(row, its stored ARN)`.
async fn created(router: &MultiServiceRouter) -> Vec<(Submitted, String)> {
    let mut out = Vec::new();
    for row in config_table() {
        let created = create_accepted(router, &row).await;
        let arn = config_node(&created, row.case, "the create's answer")
            .text_at("ARN")
            .unwrap_or_default()
            .to_string();
        assert!(
            !arn.is_empty(),
            "case {}: the create must report the ARN it minted, got {}",
            row.case,
            created.raw
        );
        out.push((row, arn));
    }
    out
}

/// Assert `answer` is the modelled `InvalidArgument` carrying `expected`.
fn assert_refused(answer: &Answer, case: &str, expected: &str) {
    assert_eq!(
        answer.status, 400,
        "case {case}: expected HTTP 400, got {} with {}",
        answer.status, answer.raw
    );
    assert_eq!(
        answer.error_code(),
        "InvalidArgument",
        "case {case}: expected InvalidArgument, got {:?} in {}",
        answer.error_code(),
        answer.raw
    );
    let message = answer.root().find_text("Message").unwrap_or_default();
    assert_eq!(
        message, expected,
        "case {case}: expected the sentence {expected:?}, got {message:?}"
    );
}

/// A mutation that names NEITHER identifier addresses no configuration at all, and each
/// operation refuses it in its own words — whatever the store holds.
#[tokio::test]
async fn a_mutation_naming_no_identifier_is_refused_in_the_operations_own_words() {
    let router = emulator();

    // Asserted before anything is stored and again with configurations in the store: the
    // condition is a property of the request, not of the store.
    for context in ["an empty store", "a populated store"] {
        if context == "a populated store" {
            let _ = created(&router).await;
        }
        for mutation in [Mutation::Update, Mutation::Delete] {
            // Every way a request can carry no identifier: no element at all, and an empty
            // element for each of them, which names nothing.
            for (form, name, arn) in [
                ("neither element present", None, None),
                ("an empty Name element", Some(""), None),
                ("an empty ARN element", None, Some("")),
                ("both elements empty", Some(""), Some("")),
            ] {
                let answer = mutation.call(&router, name, arn).await;
                assert_refused(
                    &answer,
                    &format!("{} / {form} / {context}", mutation.case()),
                    mutation.no_identifier(),
                );
            }
        }
    }

    // None of those refusals removed or changed a record.
    for row in config_table() {
        let read = get_config(&router, Some(row.name.as_str()), None).await;
        assert_eq!(
            read.status, 200,
            "case {}: expected the configuration to be readable after the refusals, got {} with \
             {}",
            row.case, read.status, read.raw
        );
        assert_reports(
            config_node(&read, row.case, "after the refused mutations"),
            &row,
            "after the refused mutations",
            &read.raw,
        );
    }
}

/// A mutation whose `Name` and `ARN` resolve to DIFFERENT configurations addresses neither, and
/// each operation refuses it in its own words — in both directions, for every pair of stored
/// configurations, and without removing or changing anything.
#[tokio::test]
async fn a_mutation_whose_identifiers_disagree_is_refused_and_changes_nothing() {
    let router = emulator();
    let stored = created(&router).await;
    assert!(
        stored.len() >= 2,
        "the fixture must hold at least two configurations to disagree about, got {}",
        stored.len()
    );

    for mutation in [Mutation::Update, Mutation::Delete] {
        for (left, (row, _)) in stored.iter().enumerate() {
            for (right, (other, arn)) in stored.iter().enumerate() {
                if left == right {
                    continue;
                }
                let case = format!(
                    "{} / the name of {:?} beside the ARN of {:?}",
                    mutation.case(),
                    row.name,
                    other.name
                );
                let answer = mutation
                    .call(&router, Some(row.name.as_str()), Some(arn.as_str()))
                    .await;
                assert_refused(&answer, &case, mutation.disagreeing());
            }
        }
    }

    // Every refusal above left every record exactly as the create stored it — nothing was
    // removed and no sampling rate was moved onto the rate the updates asked for.
    for (row, _) in &stored {
        let read = get_config(&router, Some(row.name.as_str()), None).await;
        assert_eq!(
            read.status, 200,
            "case {}: expected the configuration to be readable after the refusals, got {} with \
             {}",
            row.case, read.status, read.raw
        );
        assert_reports(
            config_node(&read, row.case, "after the refused mutations"),
            row,
            "after the refused mutations",
            &read.raw,
        );
    }
}

/// A mutation the pair of identifiers AGREES on is served: both naming the same configuration
/// addresses it, so the rows above are about the disagreement and not about carrying two
/// identifiers at all.
#[tokio::test]
async fn a_mutation_whose_identifiers_agree_is_served() {
    let router = emulator();
    let stored = created(&router).await;

    for (row, arn) in &stored {
        // The update applies, and reports the rate it was asked for.
        let answer = update_config(&router, Some(row.name.as_str()), Some(arn.as_str())).await;
        assert_eq!(
            answer.status, 200,
            "case {}: expected HTTP 200 from an update both identifiers agree on, got {} with {}",
            row.case, answer.status, answer.raw
        );
        let reported = config_node(&answer, row.case, "the update's answer")
            .text_at("SamplingRate")
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            reported,
            ASKED_RATE.to_string(),
            "case {}: expected the rate the update asked for, got {reported:?} in {}",
            row.case,
            answer.raw
        );

        // And the delete removes exactly the configuration both identifiers name.
        let removed = delete_config(&router, Some(row.name.as_str()), Some(arn.as_str())).await;
        assert_eq!(
            removed.status, 204,
            "case {}: expected HTTP 204 from a delete both identifiers agree on, got {} with {}",
            row.case, removed.status, removed.raw
        );
        let gone = get_config(&router, Some(row.name.as_str()), None).await;
        assert_eq!(
            gone.error_code(),
            "NoSuchRealtimeLogConfig",
            "case {}: expected the deleted configuration to be gone, got {}",
            row.case,
            gone.raw
        );
    }
}

/// The READ is the other side of the rule: it writes nothing, so the documented precedence
/// applies and a pair that disagrees is served from the configuration the NAME identifies.
#[tokio::test]
async fn a_read_whose_identifiers_disagree_is_served_by_the_name() {
    let router = emulator();
    let stored = created(&router).await;
    let (first, _) = stored.first().expect("the fixture created configurations");
    let (_, other_arn) = stored.last().expect("the fixture created configurations");

    let read = get_config(&router, Some(first.name.as_str()), Some(other_arn.as_str())).await;
    assert_eq!(
        read.status, 200,
        "case a read whose identifiers disagree: expected HTTP 200, got {} with {}",
        read.status, read.raw
    );
    assert_reports(
        config_node(&read, first.case, "a read whose identifiers disagree"),
        first,
        "a read whose identifiers disagree",
        &read.raw,
    );
}

/// An identifier the store holds nothing under is the NOT-FOUND condition rather than a
/// disagreement — including a pair whose two identifiers name the same absent configuration.
/// A pair where only ONE of them resolves does NOT name one configuration, so it is the
/// disagreement: the two identifiers refer to different things, one of which does not exist.
#[tokio::test]
async fn an_identifier_no_configuration_answers_to_is_still_not_found() {
    let router = emulator();
    let stored = created(&router).await;
    let (held, held_arn) = stored.first().expect("the fixture created configurations");
    let absent = "never-created";
    let absent_arn = format!("arn:aws:cloudfront::{ACCOUNT_ID}:realtime-log-config/{absent}");

    for mutation in [Mutation::Update, Mutation::Delete] {
        for (form, name, arn) in [
            ("a name nothing holds", Some(absent), None),
            ("an ARN nothing holds", None, Some(absent_arn.as_str())),
            (
                "both naming the same absent configuration",
                Some(absent),
                Some(absent_arn.as_str()),
            ),
        ] {
            let answer = mutation.call(&router, name, arn).await;
            let case = format!("{} / {form}", mutation.case());
            assert_eq!(
                answer.status, 404,
                "case {case}: expected HTTP 404, got {} with {}",
                answer.status, answer.raw
            );
            assert_eq!(
                answer.error_code(),
                "NoSuchRealtimeLogConfig",
                "case {case}: expected NoSuchRealtimeLogConfig, got {}",
                answer.raw
            );
        }

        // A pair where one side resolves and the other does not: they refer to different
        // things, which is the disagreement rather than the not-found condition.
        for (form, name, arn) in [
            (
                "a name nothing holds beside an ARN that resolves",
                Some(absent),
                Some(held_arn.as_str()),
            ),
            (
                "a name that resolves beside an ARN nothing holds",
                Some(held.name.as_str()),
                Some(absent_arn.as_str()),
            ),
        ] {
            let answer = mutation.call(&router, name, arn).await;
            assert_refused(
                &answer,
                &format!("{} / {form}", mutation.case()),
                mutation.disagreeing(),
            );
        }
    }

    // None of those refusals removed the configuration that does exist.
    let read = get_config(&router, Some(held.name.as_str()), None).await;
    assert_eq!(
        read.status, 200,
        "case the held configuration: expected it to survive the refusals, got {} with {}",
        read.status, read.raw
    );
}
