// SPDX-License-Identifier: CC0-1.0

//! AWS Cloud9's published default-quota catalog.

use super::*;

/// One AWS Cloud9 DEVELOPMENT-ENVIRONMENT plane: the environments of one kind (`SSH` or
/// `EC2`) that may exist on one plane (the account's or each user's).
///
/// Cloud9 publishes four rows of this class — a kind on each plane — and every member but
/// the kind, the plane and the value is the same across it: the `QuotaName` names the kind
/// and the plane, and the `Description` is the sentence Cloud9 publishes for the class,
/// which NAMES THE SERVICE ("AWS Cloud9 <kind> development environments") and the plane the
/// environments are created on. One constructor fills them, so two rows of the class cannot
/// drift apart and no row loses the service's name out of its text.
fn development_environments(code: &str, kind: &str, plane: &str, value: f64) -> QuotaRow {
    // On the account's plane the sentence is about what the caller's own account holds; on
    // the user's plane the same sentence names the user the environments are counted for.
    let per_user = if plane == "this user" {
        " for each user"
    } else {
        ""
    };
    quota(format!("{kind} development environments for {plane}"), value)
        .code(code)
        .describe(&format!(
            "The maximum number of AWS Cloud9 {kind} development environments that you can \
             create{per_user} in this account in the current Region."
        ))
}

/// AWS Cloud9's published default-quota catalog.
///
/// SOURCE: AWS's published Service Quotas data for the `cloud9` service code (what `aws
/// service-quotas list-aws-default-service-quotas --service-code cloud9` reports, which is
/// the "AWS Cloud9 service quotas" table of the AWS Cloud9 User Guide), and the recorded
/// default-quota listing of that code for which `QuotaCode` names which quota and for the
/// text each row publishes. Cloud9 counts its development environments on TWO planes — the
/// account's and each user's — and publishes a quota per environment KIND on each, so a row
/// names the plane and the kind it counts.
pub(in crate::state) fn cloud9_quotas() -> Vec<QuotaRow> {
    vec![
        development_environments("L-5D26553C", "SSH", "this user", 100.0),
        // The one row of this catalog that counts the PEOPLE on an environment rather than
        // the environments themselves, so it is published under its own name and sentence.
        quota("Members per development environment", 8.0)
            .code("L-65F36924")
            .fixed()
            .describe(
                "The maximum number of users that can participate in a single AWS Cloud9 \
                 development environment in this account in the current Region.",
            ),
        development_environments("L-8ECD6BDB", "EC2", "this account", 200.0),
        development_environments("L-A29EDECE", "SSH", "this account", 200.0),
        development_environments("L-C1302C17", "EC2", "this user", 100.0),
    ]
}
