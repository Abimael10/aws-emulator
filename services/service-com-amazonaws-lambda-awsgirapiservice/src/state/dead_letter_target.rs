// SPDX-License-Identifier: CC0-1.0

//! Whether the EXECUTION ROLE of a function may WRITE the dead-letter target the function's
//! configuration names.
//!
//! "The dead-letter queue for failed asynchronous invocations" (`DeadLetterConfig`) names
//! "an Amazon SQS queue or Amazon SNS topic" (`DeadLetterConfig$TargetArn`), and the event
//! of an invocation Lambda gave up on is written there by Lambda ACTING AS the function's
//! own execution role — the role every such delivery is signed as
//! ([`super::async_completion_delivery`]). A role the policies in force on it do not let
//! make that write therefore describes a dead-letter target no event could ever reach, so
//! the question is settled by the call that WRITES the configuration, before anything is
//! persisted: a customer learns the role is short of a grant from the refusal instead of
//! from asynchronous invocations whose events are silently lost.
//!
//! A dead-letter target is a destination of the very kind an asynchronous-invocation
//! configuration names, so what a write to it takes and what evidence the decision is made
//! on are NOT restated here: this module names the target the configuration asks for and
//! hands it to the one reading this crate states for a destination the execution role has to
//! be able to write ([`super::event_invoke_config::withheld_destination_write`]), so the two
//! gates cannot drift apart on the decision.
//!
//! The SENTENCE is this door's own, and the difference is recorded rather than chosen: a
//! `DeadLetterConfig` the role may not write is refused "The provided execution role does not
//! have permissions to call Publish on SNS"
//! ([`super::event_source_authorization::provided_role_refusal_message`]) — wholly fixed
//! text naming the call and the service it would have been made to, and never the target —
//! while the destination of an asynchronous-invocation configuration is refused a sentence
//! that names the destination's own ARN
//! ([`super::event_invoke_config::unwritable_destination`]). Two error classes, two
//! spellings, each of them text a client compares exactly.
//!
//! Nothing is refused on evidence this emulator does not have, exactly as that reading
//! states: an engine carrying no IAM store, a role ARN the store holds no role for, a role
//! carrying an attachment whose document this account does not hold, and a target of a kind
//! no write is stated for all show nothing about the role and let the configuration through.

use crate::ops::types::DeadLetterConfig;

/// The dead-letter target `dead_letter_config` names, as the destinations the function's
/// execution role has to be able to write: the one target it names, or nothing at all.
///
/// `TargetArn`'s own pattern is `^(arn:…)|()$`, so the EMPTY STRING is a value the member
/// admits and the only one that is no resource at all: it is how a caller takes the
/// dead-letter target away, and it names no destination rather than an ARN of nothing —
/// which is the same reading the destination slots of an asynchronous-invocation
/// configuration are given ([`super::event_invoke_config`]).
#[must_use]
pub fn dead_letter_targets(dead_letter_config: Option<&DeadLetterConfig>) -> Vec<String> {
    super::event_invoke_config::named_destination(
        dead_letter_config.and_then(|config| config.target_arn.as_deref()),
    )
    .into_iter()
    .collect()
}

/// The sentence Lambda refuses a `DeadLetterConfig` with, because the policies in force on
/// the execution role `role_arn` withhold the write Lambda would make to deliver a failed
/// invocation's event to the target it names — or `None` when the role may make that write,
/// when the configuration names no target at all, or when nothing of the kind can be shown
/// (see the module docs).
///
/// A READ of both stores: it decides, and changes nothing. The caller refuses with the
/// modelled `InvalidParameterValueException` and persists nothing, which is what makes the
/// refusal and the stored configuration the only two outcomes of such a request.
#[must_use]
pub fn unwritable_dead_letter_target<Eng: Send + Sync + 'static>(
    engine: &Eng,
    role_arn: &str,
    dead_letter_config: Option<&DeadLetterConfig>,
) -> Option<String> {
    let iam = super::event_source_authorization::iam_state(engine)?;
    let held = iam.lock();
    dead_letter_targets(dead_letter_config)
        .iter()
        .find_map(|target| {
            super::event_invoke_config::withheld_destination_write(&held, role_arn, target).map(
                |(action, service)| {
                    super::event_source_authorization::provided_role_refusal_message(
                        action, service,
                    )
                },
            )
        })
}

#[cfg(test)]
mod tests {
    use super::dead_letter_targets;
    use crate::ops::types::DeadLetterConfig;

    /// The target a configuration names is the one destination the role has to be able to
    /// write, and the two ways of naming none — no member at all and the empty string the
    /// member's pattern admits — ask for no write and so for no grant.
    #[test]
    fn the_target_a_configuration_names_is_the_destination_it_asks_a_write_of() {
        // (case, the configuration, the destinations it asks a write of)
        let rows: [(&str, Option<DeadLetterConfig>, Vec<&str>); 5] = [
            ("no configuration at all", None, Vec::new()),
            (
                "a configuration with no target member",
                Some(DeadLetterConfig { target_arn: None }),
                Vec::new(),
            ),
            (
                "the empty target the pattern admits",
                Some(DeadLetterConfig {
                    target_arn: Some(String::new()),
                }),
                Vec::new(),
            ),
            (
                // "an Amazon SQS queue or Amazon SNS topic", from
                // `smithy_query(query="doc", shape="DeadLetterConfig", member="TargetArn")`.
                "a queue",
                Some(DeadLetterConfig {
                    target_arn: Some("arn:aws:sqs:us-east-1:000000000000:dead-letters".to_string()),
                }),
                vec!["arn:aws:sqs:us-east-1:000000000000:dead-letters"],
            ),
            (
                "a topic",
                Some(DeadLetterConfig {
                    target_arn: Some("arn:aws:sns:eu-west-1:000000000000:dead-letters".to_string()),
                }),
                vec!["arn:aws:sns:eu-west-1:000000000000:dead-letters"],
            ),
        ];
        for (case, config, expected) in rows {
            let asked = dead_letter_targets(config.as_ref());
            assert_eq!(
                asked, expected,
                "case {case:?}: expected {expected:?}, got {asked:?}"
            );
        }
    }
}
