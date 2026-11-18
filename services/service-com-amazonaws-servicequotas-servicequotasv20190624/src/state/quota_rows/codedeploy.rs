// SPDX-License-Identifier: CC0-1.0

//! AWS CodeDeploy's published default-quota catalog.

use super::*;

/// One CodeDeploy quota description: the service states its limits as `Maximum number of
/// <what the quota counts>`, with no trailing full stop and no sentence about the account
/// around it.
///
/// SOURCE: the recorded default-quota listing of `codedeploy`, which carries exactly this
/// form on the rows this catalog can cite it for (the event-notification-trigger and the
/// ECS traffic-route listener rows), so every row of the service is described through this
/// one function rather than in a voice written per row.
fn maximum_number_of(counted: &str) -> String {
    format!("Maximum number of {counted}")
}

/// AWS CodeDeploy's published default-quota catalog.
///
/// SOURCE: AWS's published Service Quotas data for the `codedeploy` service code (what `aws
/// service-quotas list-aws-default-service-quotas --service-code codedeploy` reports, which
/// is the "CodeDeploy quotas" page of the AWS CodeDeploy User Guide), and the recorded
/// default-quota listing of that code for which `QuotaCode` names which quota, for the
/// wording its rows carry ([`maximum_number_of`]) and for each row's unit and adjustability.
/// CodeDeploy counts its planes per ACCOUNT, per APPLICATION, per DEPLOYMENT GROUP and per
/// DEPLOYMENT, and publishes time limits beside those counts, so a row names the plane it is
/// counted on.
pub(in crate::state) fn codedeploy_quotas() -> Vec<QuotaRow> {
    vec![
        quota("Applications in an AWS account", 1000.0)
            .code("L-0CB3C26F")
            .fixed()
            .describe(&maximum_number_of("applications in an AWS account")),
        // The batch read of on-premises instances: the API action's own page size, which is
        // what a caller sizes a batch of instance ids against.
        quota(
            "Number of instances that can be passed to the BatchGetOnPremisesInstances API \
             action",
            100.0,
        )
        .code("L-31E782CD")
        .fixed()
        .describe(&maximum_number_of(
            "instances that can be passed to the BatchGetOnPremisesInstances API action",
        )),
        quota("Concurrent deployments in an AWS account", 1000.0)
            .code("L-3F19B6A5")
            .describe(&maximum_number_of("concurrent deployments in an AWS account")),
        quota("Custom deployment configurations in an AWS account", 50.0)
            .code("L-464411D9")
            .describe(&maximum_number_of(
                "custom deployment configurations in an AWS account",
            )),
        quota("Instances in a single deployment", 1000.0)
            .code("L-5AD34096")
            .fixed()
            .describe(&maximum_number_of("instances in a single deployment")),
        quota("Instances in all concurrent deployments in an AWS account", 1000.0)
            .code("L-5FB5F09F")
            .fixed()
            .describe(&maximum_number_of(
                "instances in all concurrent deployments in an AWS account",
            )),
        quota("On-premises instances registered in an AWS account", 1000.0)
            .code("L-604B54B8")
            .fixed()
            .describe(&maximum_number_of(
                "on-premises instances registered in an AWS account",
            )),
        quota("Tags in an on-premises instance", 10.0)
            .code("L-69184564")
            .fixed()
            .describe(&maximum_number_of("tags in an on-premises instance")),
        quota("Amazon EC2 tag filters in a deployment group", 10.0)
            .code("L-696A9A65")
            .fixed()
            .describe(&maximum_number_of(
                "Amazon EC2 tag filters in a deployment group",
            )),
        quota("Auto Scaling groups in a deployment group", 10.0)
            .code("L-6BCCFC85")
            .describe(&maximum_number_of(
                "Auto Scaling groups in a deployment group",
            )),
        quota("Amazon ECS services in a deployment group", 1.0)
            .code("L-6DACB4EE")
            .describe(&maximum_number_of(
                "Amazon ECS services in a deployment group",
            )),
        quota("Lambda functions in a deployment group", 1.0)
            .code("L-826E27D8")
            .fixed()
            .describe(&maximum_number_of(
                "AWS Lambda functions in a deployment group",
            )),
        quota("Minutes a deployment lifecycle event can run", 60.0)
            .code("L-86A1A2E1")
            .unit("Minutes")
            .fixed()
            .describe(&maximum_number_of(
                "minutes a deployment lifecycle event can run",
            )),
        quota("Event notification triggers in a deployment group", 10.0)
            .code("L-877B748B")
            .describe(&maximum_number_of(
                "event notification triggers in a deployment group",
            )),
        // The lifecycle-event start timeout. The recorded listing publishes it as a COUNT
        // (`Unit` `None`) rather than as a measured span, so the minutes are named by the
        // quota's own text and the unit states none.
        quota(
            "Minutes until a deployment fails if a lifecycle event doesn't start",
            5.0,
        )
        .code("L-9231961D")
        .fixed()
        .describe(&maximum_number_of(
            "minutes until a deployment fails if a lifecycle event doesn't start",
        )),
        quota(
            "Target groups for a traffic route during an Amazon ECS deployment",
            2.0,
        )
        .code("L-9F835576")
        .describe(&maximum_number_of(
            "target groups for a traffic route during an Amazon ECS deployment",
        )),
        quota("Alarms in a deployment group", 10.0)
            .code("L-A58C473C")
            .fixed()
            .describe(&maximum_number_of("alarms in a deployment group")),
        quota("Concurrent deployments per deployment group", 1.0)
            .code("L-A8B8B32B")
            .fixed()
            .describe(&maximum_number_of(
                "concurrent deployments in a deployment group",
            )),
        quota("Hours a deployment can run", 8.0)
            .code("L-AB125F0B")
            .unit("Hours")
            .describe(&maximum_number_of("hours a deployment can run")),
        quota("Deployments in progress to a single instance", 1.0)
            .code("L-B0CB7B38")
            .fixed()
            .describe(&maximum_number_of(
                "deployments in progress to a single instance",
            )),
        quota(
            "Hours original instances are kept after a blue/green deployment",
            48.0,
        )
        .code("L-B2AEFF5A")
        .unit("Hours")
        .fixed()
        .describe(&maximum_number_of(
            "hours original instances are kept after a blue/green deployment",
        )),
        quota("Lifecycle event hooks in a deployment", 10.0)
            .code("L-B8C6A115")
            .fixed()
            .describe(&maximum_number_of("lifecycle event hooks in a deployment")),
        quota("Deployment groups an on-premises instance can be in", 10.0)
            .code("L-BB837EF6")
            .fixed()
            .describe(&maximum_number_of(
                "deployment groups an on-premises instance can be in",
            )),
        quota(
            "Number of listeners for a traffic route during an Amazon ECS deployment",
            1.0,
        )
        .code("L-C77AFF36")
        .fixed()
        .describe(&maximum_number_of(
            "listeners for a traffic route during an Amazon ECS deployment",
        )),
        quota("Tags in a deployment group", 50.0)
            .code("L-D4ED2A8C")
            .fixed()
            .describe(&maximum_number_of("tags in a deployment group")),
        quota("Deployment groups associated with a single application", 1000.0)
            .code("L-D9088B77")
            .describe(&maximum_number_of(
                "deployment groups associated with a single application",
            )),
        quota("GitHub token connections in an AWS account", 25.0)
            .code("L-DC0E5D95")
            .fixed()
            .describe(&maximum_number_of(
                "GitHub token connections in an AWS account",
            )),
        quota("Revisions kept for an application", 1000.0)
            .code("L-F0A94AA0")
            .fixed()
            .describe(&maximum_number_of("revisions kept for an application")),
    ]
}
