// SPDX-License-Identifier: CC0-1.0

//! The CloudWatch Logs configuration an API is created or updated with: the one member of
//! it the service validates, and the refusal a value it cannot assume takes.
//!
//! Both log configurations this service takes carry the same required member under the
//! same name and the same documentation — `LogConfig$cloudWatchLogsRoleArn`, "The service
//! role that AppSync assumes to publish to CloudWatch logs in your account"
//! (`CreateGraphqlApi`/`UpdateGraphqlApi`), and
//! `EventLogConfig$cloudWatchLogsRoleArn`, "The IAM service role that AppSync assumes to
//! publish CloudWatch Logs in your account" (`CreateApi`/`UpdateApi`,
//! `smithy_query(query="shape", shape="LogConfig,EventLogConfig")`). Both target a bare
//! `String`, so the boundary admits any text and reading the ROLE is the service's job.
//!
//! A role is assumed by its ARN, so a value that is not a well-formed IAM role ARN names
//! no role AppSync could assume and nothing could ever publish under it: the call is
//! refused with `BadRequestException` (HTTP 400) — modelled by all four operations — and
//! the API is neither created nor changed. Whether a value is an IAM role ARN is
//! `service_common::arn`'s reading, the one parse of an ARN's fields in this tree, so
//! every member documented as a role ARN is read the same way.

/// AppSync's fixed text for a `cloudWatchLogsRoleArn` that is not a role it could assume,
/// carried by `BadRequestException` (HTTP 400).
///
/// The sentence names the member and points at the documentation, and has no variable part:
/// nothing of the submitted value is echoed back.
pub const INVALID_CLOUD_WATCH_LOGS_ROLE_MESSAGE: &str =
    "The cloudWatchLogsRoleArn is invalid. Please refer to the documentation for more details.";

/// Why the `cloudWatchLogsRoleArn` a request carries is not a role AppSync could assume, as
/// the text a `BadRequestException` (HTTP 400) states it with, or `None` when it is one —
/// including a request that configures no logging at all, which names no role.
pub fn cloud_watch_logs_role_refusal(cloud_watch_logs_role_arn: Option<&str>) -> Option<String> {
    let role_arn = cloud_watch_logs_role_arn?;
    (!::service_common::arn::IAM_ROLE.admits(role_arn))
        .then(|| INVALID_CLOUD_WATCH_LOGS_ROLE_MESSAGE.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a well-formed IAM role ARN is accepted, whatever path, account or Region it
    /// names.
    #[test]
    fn an_iam_role_arn_is_accepted() {
        for (case, candidate) in [
            ("a role of this account", "arn:aws:iam::000000000000:role/appsync-logs"),
            ("a role of another account", "arn:aws:iam::123456789012:role/service-logs"),
            (
                "a role under a path",
                "arn:aws:iam::000000000000:role/service-role/appsync-logs",
            ),
            ("a role whose name carries punctuation", "arn:aws:iam::000000000000:role/a_b.c-d"),
        ] {
            let refusal = cloud_watch_logs_role_refusal(Some(candidate));
            assert!(
                refusal.is_none(),
                "case {case:?} ({candidate}): expected no refusal, got {refusal:?}"
            );
        }

        // A request that configures no logging names no role, so there is nothing to refuse.
        let refusal = cloud_watch_logs_role_refusal(None);
        assert!(
            refusal.is_none(),
            "case no log configuration: expected no refusal, got {refusal:?}"
        );
    }

    /// RULE: a value that is not an IAM ROLE ARN is refused with the member's own sentence.
    #[test]
    fn a_value_that_names_no_role_is_refused() {
        for (case, candidate) in [
            ("no ARN at all", "not-a-valid-arn"),
            ("an empty value", ""),
            ("an ARN of another vendor", "arn:aws:appsync:us-east-1:000000000000:apis/a"),
            ("an IAM user rather than a role", "arn:aws:iam::000000000000:user/someone"),
            ("an IAM policy rather than a role", "arn:aws:iam::000000000000:policy/p"),
            ("an ARN missing its resource", "arn:aws:iam::000000000000:"),
            ("the role word without a name", "arn:aws:iam::000000000000:rolefoo"),
        ] {
            let refusal = cloud_watch_logs_role_refusal(Some(candidate));
            assert_eq!(
                refusal.as_deref(),
                Some(INVALID_CLOUD_WATCH_LOGS_ROLE_MESSAGE),
                "case {case:?} ({candidate}): expected the member's refusal, got {refusal:?}"
            );
        }
    }
}
