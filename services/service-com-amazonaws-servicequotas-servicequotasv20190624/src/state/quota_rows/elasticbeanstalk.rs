// SPDX-License-Identifier: CC0-1.0

//! AWS Elastic Beanstalk's published default-quota catalog.

use super::*;

/// AWS Elastic Beanstalk's published default-quota catalog.
///
/// SOURCE: AWS's published Service Quotas data for the `elasticbeanstalk` service code
/// (the "AWS Elastic Beanstalk service quotas" page of its Developer Guide), and the
/// recorded default-quota listing of that code for which `QuotaCode` names which
/// quota and for each row's published sentence. Elastic Beanstalk publishes the planes of
/// the deployment model it serves — the applications of an account, the versions and
/// configuration templates those applications hold, the environments running them, and
/// the custom platform versions they run on.
///
/// A plane the service counts ACROSS the account's applications rather than inside one
/// says so in a second sentence of its own text, and that sentence names the thing the
/// plane is not counted per: the environment and version planes are not counted per
/// application, and the custom-platform-version plane is not counted per custom platform.
pub(in crate::state) fn elasticbeanstalk_quotas() -> Vec<QuotaRow> {
    // Every plane of this service is a count of what an account may CREATE in the
    // Region, and the service words all of them the same way, so the sentence is
    // built once from what the row counts.
    let created = |counted: &str| {
        format!(
            "The maximum number of {counted} that you can create in this account in the \
             current Region."
        )
    };
    // The second sentence a plane counted across a set of resources carries: the limit is
    // the whole account's in the Region, not one per member of that set.
    let across = |counted_plural: &str, counted_singular: &str| {
        format!(
            " The limit applies across {counted_plural}, not per {counted_singular}."
        )
    };
    vec![
        quota("Applications", 75.0)
            .code("L-1CEABD17")
            .describe(&created("applications")),
        quota("Environments", 200.0)
            .code("L-8EFC1C51")
            // The environment plane is counted across the account's applications, not
            // inside one, which the service states in the row's own text.
            .describe(&format!(
                "{}{}",
                created("environments"),
                across("applications", "application")
            )),
        quota("Configuration templates", 2000.0)
            .code("L-9838E43F")
            .describe(&created("configuration templates")),
        quota("Application versions", 1000.0)
            .code("L-D64F1F14")
            // The version plane is counted across the account's applications, the same way
            // the environment plane is.
            .describe(&format!(
                "{}{}",
                created("application versions"),
                across("applications", "application")
            )),
        quota("Custom platform versions", 50.0)
            .code("L-E593A077")
            // The custom-platform-version plane is counted across the account's custom
            // platforms rather than per platform, which this row's own second sentence
            // states the same way the environment and version rows state theirs.
            .describe(&format!(
                "{}{}",
                created("custom platform versions"),
                across("custom platforms", "custom platform")
            )),
    ]
}
