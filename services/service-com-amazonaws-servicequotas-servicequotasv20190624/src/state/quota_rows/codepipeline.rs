// SPDX-License-Identifier: CC0-1.0

//! AWS CodePipeline's published default-quota catalog.

use super::*;

/// One CodePipeline quota description for an UPPER bound: the service states them as `The
/// maximum number of <what the quota counts>`.
///
/// SOURCE: the recorded default-quota listing of `codepipeline`, which carries this form on
/// the rows this catalog can cite it for (the webhook plane and the parallel-execution
/// ceiling), so the service's rows are described through this one function rather than in a
/// voice written per row.
fn the_maximum_number_of(counted: &str) -> String {
    format!("The maximum number of {counted}")
}

/// One CodePipeline quota description for what a STAGE, a PIPELINE or a CONDITION may
/// hold: the service states what is "allowed in" that plane, as a sentence.
///
/// SOURCE: the recorded default-quota listing of `codepipeline`, which words every row
/// counted on one of those planes this way (the parallel-execution ceiling among them)
/// while its ACCOUNT-wide planes are counted "per region in an AWS account". One
/// constructor fills the class so two rows of it cannot drift apart.
fn allowed_in(counted: &str, plane: &str) -> String {
    the_maximum_number_of(&format!("{counted} allowed in a {plane}."))
}

/// The LOWER bound of what a stage or a pipeline may hold, in the same voice as
/// [`allowed_in`].
fn least_allowed_in(counted: &str, plane: &str) -> String {
    the_minimum_number_of(&format!("{counted} allowed in a {plane}."))
}

/// One CodePipeline quota description for a LOWER bound: the fewest of something a pipeline
/// or a stage may hold, which the service publishes beside its upper bounds.
fn the_minimum_number_of(counted: &str) -> String {
    format!("The minimum number of {counted}")
}

/// AWS CodePipeline's published default-quota catalog.
///
/// SOURCE: AWS's published Service Quotas data for the `codepipeline` service code (what
/// `aws service-quotas list-aws-default-service-quotas --service-code codepipeline` reports,
/// which is the "Quotas in AWS CodePipeline" page of the AWS CodePipeline User Guide), and
/// the recorded default-quota listing of that code for which `QuotaCode` names which quota,
/// for the `Total …` / `Minimum …` / `Maximum …` wording its rows carry and for each row's
/// unit and adjustability. CodePipeline publishes LOWER bounds beside its upper ones — the
/// fewest stages and actions a pipeline may have — so a row names which bound it is.
pub(in crate::state) fn codepipeline_quotas() -> Vec<QuotaRow> {
    vec![
        // The ceiling on executions in flight at once, which applies to a pipeline whose
        // execution mode runs them in PARALLEL.
        quota("Active executions per pipeline", 50.0)
            .code("L-0097A9B4")
            .fixed()
            .describe(&allowed_in(
                "active executions",
                "PARALLEL execution mode pipeline",
            )),
        quota("Total actions per pipeline", 1000.0)
            .code("L-1402209C")
            .fixed()
            .describe(&allowed_in("actions", "pipeline")),
        quota("Total V2 type pipelines", 1000.0)
            .code("L-182AE5D8")
            .fixed()
            .describe(&the_maximum_number_of(
                "V2 type pipelines per region in an AWS account",
            )),
        quota("Maximum timeout for build actions", 8.0)
            .code("L-1E23CE75")
            .unit("Hours")
            .fixed()
            .describe("The maximum timeout, in hours, for a build action."),
        quota("Total parallel actions per stage", 100.0)
            .code("L-2B3011E2")
            .fixed()
            .describe(&allowed_in("parallel actions", "stage")),
        quota("Total sequential actions per stage", 100.0)
            .code("L-31A751EC")
            .fixed()
            .describe(&allowed_in("sequential actions", "stage")),
        quota("Maximum actions per stage", 50.0)
            .code("L-3ECA8567")
            .fixed()
            .describe(&allowed_in("actions", "stage")),
        quota("Total variables per pipeline", 50.0)
            .code("L-447DD651")
            .describe(&allowed_in("variables", "pipeline")),
        quota("Total custom actions", 50.0)
            .code("L-519D5A90")
            .describe(&the_maximum_number_of(
                "custom actions per region in an AWS account",
            )),
        quota("Total triggers per pipeline", 50.0)
            .code("L-570F1605")
            .fixed()
            .describe(&allowed_in("triggers", "pipeline")),
        quota("Total rules per condition", 5.0)
            .code("L-5AA45A27")
            .describe(&allowed_in("rules", "condition")),
        quota("Total conditions per stage", 5.0)
            .code("L-6F95DA01")
            .fixed()
            .describe(&allowed_in("conditions", "stage")),
        quota("Total pipelines", 1000.0)
            .code("L-78D200AD")
            .fixed()
            .describe(&the_maximum_number_of(
                "pipelines per region in an AWS account",
            )),
        quota("Maximum revision size", 5.0)
            .code("L-87C878A5")
            .unit("Megabytes")
            .describe("The maximum size, in megabytes, of a source revision."),
        quota("Maximum timeout for a manual approval action", 7.0)
            .code("L-8DF1BAAD")
            .unit("Days")
            .fixed()
            .describe("The maximum timeout, in days, for a manual approval action."),
        quota("Maximum timeout for custom actions", 24.0)
            .code("L-912817AD")
            .unit("Hours")
            .describe("The maximum timeout, in hours, for a custom action."),
        quota("Maximum timeout for Lambda invoke actions", 20.0)
            .code("L-9D72EF0D")
            .unit("Minutes")
            .fixed()
            .describe("The maximum timeout, in minutes, for a Lambda invoke action."),
        quota("Total stages per pipeline", 50.0)
            .code("L-A0A99E23")
            .fixed()
            .describe(&allowed_in("stages", "pipeline")),
        quota("Maximum timeout for test actions", 8.0)
            .code("L-A95CB60D")
            .unit("Hours")
            .fixed()
            .describe("The maximum timeout, in hours, for a test action."),
        quota("Maximum timeout for deploy actions", 1.0)
            .code("L-ABB870A7")
            .unit("Hours")
            .describe("The maximum timeout, in hours, for a deploy action."),
        quota("Minimum actions", 1.0)
            .code("L-C57FFB43")
            .fixed()
            .describe(&least_allowed_in("actions", "stage")),
        quota(
            "Total pipelines with change detection set to periodically checking for source \
             changes",
            300.0,
        )
        .code("L-C5885C1C")
        .fixed()
        .describe(&the_maximum_number_of(
            "pipelines with change detection set to periodically checking for source changes \
             per region in an AWS account",
        )),
        quota("Minimum stages per pipeline", 2.0)
            .code("L-C8CB041B")
            .fixed()
            .describe(&least_allowed_in("stages", "pipeline")),
        quota("Total tags per pipeline", 50.0)
            .code("L-CE1F2EE9")
            .describe(&allowed_in("tags", "pipeline")),
        quota("Total action types", 50.0)
            .code("L-DCBA35EE")
            .fixed()
            .describe(&the_maximum_number_of(
                "action types per region in an AWS account",
            )),
        quota("Maximum timeout for source actions", 1.0)
            .code("L-F2AD11A6")
            .unit("Hours")
            .fixed()
            .describe("The maximum timeout, in hours, for a source action."),
        quota("Maximum size of a pipeline structure", 100.0)
            .code("L-F7192911")
            .unit("Kilobytes")
            .fixed()
            .describe("The maximum size, in kilobytes, of a pipeline structure."),
        quota("Months of pipeline execution history", 12.0)
            .code("L-F8F56C9F")
            .unit("Months")
            .fixed()
            .describe(&the_maximum_number_of(
                "months of pipeline execution history",
            )),
        quota("Total webhooks", 300.0)
            .code("L-FE939BB2")
            .describe(&the_maximum_number_of(
                "webhooks per region in an AWS account",
            )),
    ]
}
