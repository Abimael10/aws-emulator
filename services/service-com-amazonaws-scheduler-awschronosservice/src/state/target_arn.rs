// SPDX-License-Identifier: CC0-1.0

//! The FORM of the ARN a schedule's target names.
//!
//! `Target$Arn` is "the Amazon Resource Name (ARN) of the target"
//! (`smithy_query(query="doc", shape="com.amazonaws.scheduler#Target", member="Arn")`), and
//! the model constrains it by LENGTH alone — `TargetArn` is a plain `@length 1..1600`
//! string with no `@pattern`
//! (`smithy_query(query="shape", shape="com.amazonaws.scheduler#TargetArn")`) — so the
//! request boundary admits any text of that length. The ARN form is therefore the
//! SERVICE's own check, made when the schedule is WRITTEN: a value that is no ARN names no
//! resource and no API operation, so no occurrence of such a schedule could ever be
//! delivered. EventBridge Scheduler refuses the write rather than storing a schedule whose
//! target points nowhere, and hands the caller back the value it submitted.
//!
//! What the ARN POINTS AT is not checked here and is not this service's to check: a target
//! may name a resource in another account or Region, and a universal target names an API
//! operation rather than a resource at all ([`super::universal_target`], which reads the
//! grammar of that one ARN class). Only the form every ARN shares is read, from the one
//! ARN parse this tree keeps (`::service_common::arn::parse`).
//!
//! The target's other ARN-valued members need nothing from this module: `Target$RoleArn`,
//! `DeadLetterConfig$Arn` and `KmsKeyArn` each carry a modelled `@pattern` that the request
//! boundary already applies, which refuses text that is not an ARN of the documented
//! resource before a handler runs.
//!
//! One concern, shared by the two operations that WRITE a schedule: both carry the member,
//! so an update cannot move a stored schedule onto a target a create would have refused.
//!
//! The refusal is the 400 `ValidationException` every broken bound of this surface takes.
//! Its sentence is the Amazon EventBridge family's statement about a member documented as
//! an ARN that holds text that is not one, stated once for the family in
//! [`::service_common::arn::not_an_arn_refusal`].

/// Refuse a `Target$Arn` that is not a well-formed ARN; `None` for one that is.
///
/// The value is reported back as the request submitted it, which is the only place a
/// caller reads which of its parameters was rejected.
pub fn target_arn_violation(target_arn: &str) -> Option<String> {
    ::service_common::arn::parse(target_arn)
        .is_none()
        .then(|| ::service_common::arn::not_an_arn_refusal(target_arn))
}

#[cfg(test)]
mod tests {
    //! The form this module owns, over a SET of values rather than one. The same rule is
    //! driven through real requests, on both operations that write a schedule, in
    //! `tests/all/target_arn_form.rs`.

    use super::*;

    /// Every ARN class a schedule may target is admitted: a templated target naming a
    /// resource of any service, one naming a resource in another partition, Region or
    /// account, one whose resource field carries further colons or slashes, and a
    /// universal target, whose Region and account fields are empty by construction.
    #[test]
    fn a_well_formed_arn_is_admitted() {
        let rows: [(&str, &str); 7] = [
            ("a queue", "arn:aws:sqs:eu-west-2:000000000000:q"),
            (
                "a function, whose resource field carries a colon",
                "arn:aws:lambda:us-east-1:000000000000:function:fn",
            ),
            (
                "a stream, whose resource field carries a slash",
                "arn:aws:kinesis:us-east-1:000000000000:stream/s",
            ),
            (
                "a resource in another partition",
                "arn:aws-cn:sqs:cn-north-1:000000000000:q",
            ),
            (
                "a partition-global resource, with no Region or account",
                "arn:aws:s3:::bucket/key",
            ),
            (
                "a universal target, which names an API operation",
                "arn:aws:scheduler:::aws-sdk:sqs:sendMessage",
            ),
            (
                "a state machine in another account",
                "arn:aws:states:us-east-1:111122223333:stateMachine:sm",
            ),
        ];

        for (case, value) in rows {
            let refusal = target_arn_violation(value);
            assert!(
                refusal.is_none(),
                "case {case:?}: expected {value} to be admitted, got {refusal:?}"
            );
        }
    }

    /// Anything that is not an ARN is refused, with the value quoted back: text with no
    /// fields at all, too few fields, a first field that is not `arn`, and one of the
    /// three fields an ARN may not leave empty missing.
    #[test]
    fn a_value_that_is_no_arn_is_refused_with_the_value_quoted() {
        let rows: [(&str, &str); 7] = [
            ("text that is no ARN at all", "not-an-arn"),
            ("the empty value", ""),
            ("too few fields", "arn:aws:sqs:eu-west-2:000000000000"),
            ("a first field that is not arn", "urn:aws:sqs:r:a:q"),
            ("no partition", "arn::sqs:eu-west-2:000000000000:q"),
            ("no vendor", "arn:aws::eu-west-2:000000000000:q"),
            ("no resource", "arn:aws:sqs:eu-west-2:000000000000:"),
        ];

        for (case, value) in rows {
            let refusal = target_arn_violation(value);
            assert_eq!(
                refusal.as_deref(),
                Some(::service_common::arn::not_an_arn_refusal(value).as_str()),
                "case {case:?}: expected {value} refused with the value quoted, got {refusal:?}"
            );
        }
    }
}
