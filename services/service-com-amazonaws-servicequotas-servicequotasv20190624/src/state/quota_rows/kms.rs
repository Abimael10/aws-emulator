// SPDX-License-Identifier: CC0-1.0

//! AWS Key Management Service's published default-quota catalog.

use super::*;

/// The name AWS KMS rejects throttled requests under, which is also the `AWS/Usage` display
/// name its call counts are published under.
const KMS_USAGE_SERVICE: &str = "KMS";

/// One AWS KMS API REQUEST-RATE quota: `<api> request rate`, the most requests to that
/// one operation KMS serves per second.
///
/// KMS publishes a rate quota for each of its API operations, and every member of such
/// a row but the operation's own name and the value is the same across the class: the
/// `QuotaName` is the operation's name followed by `request rate`, the window is one
/// second, the usage metric is that operation's `AWS/Usage` `CallCount`
/// ([`call_count_metric`]) under the `KMS` service dimension, and the `Description` is
/// the sentence KMS publishes for the class with the operation it throttles named in
/// it. Those are the quota's PUBLISHED members, so one constructor fills them and two
/// rows of the class cannot drift apart.
///
/// SOURCE: AWS KMS's published request quotas (the per-operation request-quota table of
/// the KMS Developer Guide, which is what the service's default-quota listing reports),
/// and the recorded default-quota listing of `kms` for the wording and for which
/// `QuotaCode` names which operation's rate.
fn kms_request_rate(api_name: &str, requests_per_second: f64) -> QuotaRow {
    quota(format!("{api_name} request rate"), requests_per_second)
        .period(1, "SECOND")
        .metric(call_count_metric(KMS_USAGE_SERVICE, api_name))
        .describe(&interval_rate_description(KMS_USAGE_SERVICE, api_name))
}

/// AWS Key Management Service's published default-quota catalog.
///
/// SOURCE: AWS's published Service Quotas data for the `kms` service code (what `aws
/// service-quotas list-aws-default-service-quotas --service-code kms` reports, which is
/// the "Quotas" chapter of the AWS Key Management Service Developer Guide), and the
/// recorded default-quota listing of that code for which `QuotaCode` names which quota.
///
/// KMS publishes two kinds of row: a REQUEST-RATE quota per API operation, all of one
/// shape ([`kms_request_rate`]), and quotas that COUNT a resource plane. Every row here
/// is published under the `L-` code the recorded listing of this service states for the
/// quota it names, with that quota's own published value, adjustability and sentence —
/// never a neighbouring row's. The rest of the service's published catalog names no code
/// this catalog can cite for the quota it names, so it is left to the default-quota-set
/// gap rather than published under a code this catalog would be pairing by guess.
pub(in crate::state) fn kms_quotas() -> Vec<QuotaRow> {
    vec![
        // The on-demand rotations one customer managed key may be given. KMS publishes
        // it as a COUNT (no window, no usage metric) and as NOT adjustable, as the
        // recorded listing of this service states for this code.
        //
        // SOURCE for the VALUE: the on-demand rotation limit the KMS Developer Guide
        // publishes for a customer managed key — ten rotations of one key's material.
        // The recorded listing's entry for this code is reported with its value
        // truncated, so the Guide is what the number is taken from rather than that
        // recording.
        quota("On-demand rotations per CMK", 10.0)
            .code("L-0E66C9C0")
            .fixed()
            .describe(
                "The maximum number of on-demand rotations permitted for each customer managed \
                 key. This quota does not apply to AWS managed keys.",
            ),
        // The per-operation request rates whose `QuotaCode`, throttled operation and
        // requests per second the recorded listing of this service states.
        kms_request_rate("ListKeyRotations", 100.0).code("L-171660E4"),
        kms_request_rate("ListKeys", 500.0).code("L-8A08DAEA"),
        kms_request_rate("ListResourceTags", 2000.0).code("L-FB6513A2"),
        // The recording states this code's name but not its value. SOURCE for the 5: the
        // "Request quotas for each AWS KMS API operation" table of the KMS Developer
        // Guide's "Quotas" chapter, whose row for `UpdateCustomKeyStore` — like every
        // custom-key-store operation's — publishes 5 requests per second.
        kms_request_rate("UpdateCustomKeyStore", 5.0).code("L-275D92F3"),
    ]
}
