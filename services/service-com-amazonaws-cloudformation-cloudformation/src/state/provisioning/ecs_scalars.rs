// SPDX-License-Identifier: CC0-1.0

//! What Amazon ECS models the members of its create calls as, where that is not TEXT.
//!
//! ## The rule this module implements
//!
//! "All property values in a template are strings" wherever the author quoted one, and every
//! value that reaches a property through a template PARAMETER is text too — a parameter's
//! value travels as text on the wire, whatever `Type` its declaration states. The members
//! those properties are passed to are modelled: `CreateService$desiredCount` is an integer,
//! `CreateService$enableECSManagedTags` a boolean. So the arms that provision the
//! `AWS::ECS::*` types read each member the owning model types as a number or a boolean from
//! the text the declaration carries, at every depth the document nests it, before the request
//! is sent ([`super::modelled_declaration`] is the walk; this module is the member catalogue
//! the three arms consult).
//!
//! A value already of the modelled type travels as written, and one that spells no such value
//! is left exactly as the template wrote it, because Amazon ECS's own validation is what
//! judges it.
//!
//! ## What is deliberately left as written
//!
//! * `value` — the member name is a STRING in every shape these calls carry it in (a `Tag`,
//!   a `KeyValuePair` of a proxy configuration, a cluster `ClusterSetting`, a
//!   `ResourceRequirement`, a `SystemControl`), so reading it as a number would retype a
//!   caller's own datum. It names an integer in one nested shape only
//!   (`ThresholdConfiguration$value`), which a member-name catalogue cannot tell apart, and
//!   Amazon ECS judges that one itself.
//! * a map whose keys are the REQUESTER's own data — a container's `dockerLabels`, a log
//!   driver's `options`, a docker volume's `driverOpts` and `labels` — is classified as the
//!   collection of TEXT the model types it as, so a key that happens to spell a modelled
//!   member name says nothing about a type and a number literal under it travels as the text
//!   such a map takes.
//! * `cpu` and `memory` are a task definition's own SIZE as text
//!   (`RegisterTaskDefinition$cpu`: "the number of CPU units used by the task… as a string
//!   using CPU units") and a container's reservation as an INTEGER
//!   (`ContainerDefinition$cpu`), under the same two names. The walk is told which it is
//!   reading by the TOP-LEVEL member it was entered at ([`task_definition_scalar`]), since
//!   those names stand nowhere else in the call.

use super::ModelledScalar;

/// The members `CreateService` and `UpdateService` model as NUMBERS, at every depth of the
/// request.
///
/// Source: the transitive member list of `CreateService`
/// (`smithy_query(query="closure",operation="CreateService")`), every member whose target is
/// an integer, a long or a double — the service's own `desiredCount` and
/// `healthCheckGracePeriodSeconds`, a load balancer's and a service registry's ports, a
/// capacity provider strategy item's `weight`/`base`, the deployment configuration's
/// percentages and bake times, an early-success criterion's `healthyPercent`, a Service
/// Connect alias's port, its timeouts, a managed EBS volume's sizes and rates, a gateway
/// connector health check's ports, thresholds and intervals, and a metric
/// configuration's resolution.
const SERVICE_NUMBERS: &[&str] = &[
    "bakeTimeInMinutes",
    "base",
    "canaryBakeTimeInMinutes",
    "canaryPercent",
    "containerPort",
    "desiredCount",
    "healthCheckIntervalSeconds",
    "healthCheckGracePeriodSeconds",
    "healthCheckPort",
    "healthCheckTimeoutSeconds",
    "healthyPercent",
    "healthyThresholdCount",
    "idleTimeoutSeconds",
    "ingressPortOverride",
    "iops",
    "maximumPercent",
    "minimumHealthyPercent",
    "perRequestTimeoutSeconds",
    "port",
    "resolutionSeconds",
    "sizeInGiB",
    "stepBakeTimeInMinutes",
    "stepPercent",
    "throughput",
    "timeoutInMinutes",
    "unhealthyThresholdCount",
    "volumeInitializationRate",
    "weight",
];

/// The members those same two calls model as BOOLEANS: the service's two managed-tag and
/// exec flags, Service Connect's `enabled`, the circuit breaker's and the deployment alarms'
/// `enable`/`rollback`/`resetOnHealthyTask`, and a managed EBS volume's `encrypted`.
///
/// Same source as [`SERVICE_NUMBERS`].
const SERVICE_BOOLEANS: &[&str] = &[
    "enable",
    "enableECSManagedTags",
    "enableExecuteCommand",
    "enabled",
    "encrypted",
    "resetOnHealthyTask",
    "rollback",
];

/// The members `RegisterTaskDefinition` models as NUMBERS, at every depth of the request.
///
/// Source: the transitive member list of `RegisterTaskDefinition`
/// (`smithy_query(query="closure",operation="RegisterTaskDefinition")`), every member whose
/// target is an integer — a container's reservations and start/stop timeouts, a port
/// mapping's ports, a restart policy's exit codes and period, the Linux parameters' memory
/// and swap settings, a health check's timings, a ulimit's limits, a tmpfs's size, a file
/// system's transit-encryption port and the task's ephemeral storage size. `cpu` and
/// `memory` are the two names that are a number in one place and text in another, so they
/// are not here ([`task_definition_scalar`] says which is which).
const TASK_DEFINITION_NUMBERS: &[&str] = &[
    "containerPort",
    "hardLimit",
    "hostPort",
    "ignoredExitCodes",
    "interval",
    "maxSwap",
    "memoryReservation",
    "restartAttemptPeriod",
    "retries",
    "sharedMemorySize",
    "size",
    "sizeInGiB",
    "softLimit",
    "startPeriod",
    "startTimeout",
    "stopTimeout",
    "swappiness",
    "timeout",
    "transitEncryptionPort",
];

/// The members that call models as BOOLEANS: a container's `essential` and its runtime
/// flags, a restart policy's `enabled`, the Linux parameters' `initProcessEnabled`, a mount's
/// and a volume-from's `readOnly`, a docker volume's `autoprovision`, a volume's
/// `configuredAtLaunch` and the task's `enableFaultInjection`.
///
/// Same source as [`TASK_DEFINITION_NUMBERS`].
const TASK_DEFINITION_BOOLEANS: &[&str] = &[
    "autoprovision",
    "configuredAtLaunch",
    "disableNetworking",
    "enableFaultInjection",
    "enabled",
    "essential",
    "initProcessEnabled",
    "interactive",
    "privileged",
    "pseudoTerminal",
    "readOnly",
    "readonlyRootFilesystem",
];

/// The members `CreateCluster` models as NUMBERS: a default capacity provider strategy
/// item's `weight` and `base`
/// (`smithy_query(query="closure",operation="CreateCluster")`).
const CLUSTER_NUMBERS: &[&str] = &["base", "weight"];

/// The members that call models as BOOLEANS: the two encryption flags of the exec-command log
/// configuration. Same source as [`CLUSTER_NUMBERS`].
const CLUSTER_BOOLEANS: &[&str] = &["cloudWatchEncryptionEnabled", "s3EncryptionEnabled"];

/// The members `CreateCapacityProvider` models as NUMBERS: the managed scaling's target
/// capacity, its step sizes and its instance warm-up period, a managed-instances provider's
/// `scaleInAfter` and storage size, and the bounds and price percentages of its instance
/// requirements (`smithy_query(query="closure",operation="CreateCapacityProvider")`; `min`
/// and `max` name a bound of one of those ranges and nothing else in the call).
const CAPACITY_PROVIDER_NUMBERS: &[&str] = &[
    "instanceWarmupPeriod",
    "max",
    "maxSpotPriceAsPercentageOfOptimalOnDemandPrice",
    "maximumScalingStepSize",
    "min",
    "minimumScalingStepSize",
    "onDemandMaxPricePercentageOverLowestPrice",
    "scaleInAfter",
    "spotMaxPricePercentageOverLowestPrice",
    "storageSizeGiB",
    "targetCapacity",
];

/// The members that call models as BOOLEANS: a launch template's metadata-tag propagation and
/// FIPS flags, an instance requirement's hibernate-support flag, and a local-storage
/// configuration's `useLocalStorage`. Same source as [`CAPACITY_PROVIDER_NUMBERS`].
const CAPACITY_PROVIDER_BOOLEANS: &[&str] = &[
    "fipsEnabled",
    "instanceMetadataTagsPropagation",
    "requireHibernateSupport",
    "useLocalStorage",
];

/// The members of these calls that are MAPS keyed by the requester's own data, each a map of
/// string to string: a container's `dockerLabels`, the `options` of its log and Firelens
/// configurations, and a docker volume configuration's `driverOpts` and `labels`.
const DATA_MAPS: &[&str] = &["dockerLabels", "driverOpts", "labels", "options"];

/// The two names that are a container's INTEGER reservation and a task's own size as TEXT.
const SIZE_MEMBERS: &[&str] = &["cpu", "memory"];

/// The top-level member of `RegisterTaskDefinition` whose subtree spells [`SIZE_MEMBERS`] as
/// the integers a container definition models them as.
pub(in crate::state) const CONTAINER_DEFINITIONS: &str = "containerDefinitions";

/// What `CreateService`/`UpdateService` model the member `member` as.
pub(in crate::state) fn service_scalar(member: &str) -> ModelledScalar {
    classify(member, SERVICE_NUMBERS, SERVICE_BOOLEANS)
}

/// What `CreateCluster` models the member `member` as.
pub(in crate::state) fn cluster_scalar(member: &str) -> ModelledScalar {
    classify(member, CLUSTER_NUMBERS, CLUSTER_BOOLEANS)
}

/// What `CreateCapacityProvider` models the member `member` as.
pub(in crate::state) fn capacity_provider_scalar(member: &str) -> ModelledScalar {
    classify(member, CAPACITY_PROVIDER_NUMBERS, CAPACITY_PROVIDER_BOOLEANS)
}

/// What `RegisterTaskDefinition` models the member `member` as, for a walk entered at the
/// top-level member `entered_at`.
///
/// `cpu` and `memory` are read as the integers a container definition models them as only
/// inside that member's own subtree; everywhere else — the task's own size — they are the
/// text that call models them as.
pub(in crate::state) fn task_definition_scalar(
    member: &str,
    entered_at: &str,
) -> ModelledScalar {
    if SIZE_MEMBERS.contains(&member) {
        return if entered_at == CONTAINER_DEFINITIONS {
            ModelledScalar::Number
        } else {
            ModelledScalar::AsWritten
        };
    }
    classify(member, TASK_DEFINITION_NUMBERS, TASK_DEFINITION_BOOLEANS)
}

/// One member against a call's own catalogues, with the data maps every one of these calls
/// carries read as the collections of text they are.
fn classify(member: &str, numbers: &[&str], booleans: &[&str]) -> ModelledScalar {
    if DATA_MAPS.contains(&member) {
        return ModelledScalar::TextThroughout;
    }
    if numbers.contains(&member) {
        return ModelledScalar::Number;
    }
    if booleans.contains(&member) {
        return ModelledScalar::Boolean;
    }
    ModelledScalar::AsWritten
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a member one of these calls models as a number or a boolean is classified as
    /// that type, a map keyed by the requester's own data as the collection of text it is,
    /// and every other member as written.
    ///
    /// The rows span what the rule generalises over: which call the member belongs to, the
    /// modelled type, how deep in the request the member stands, members the issue that led
    /// here did not cite, and the two names whose type depends on where they stand.
    #[test]
    fn each_member_is_classified_as_its_call_models_it() {
        // (case, member, what CreateService models it as)
        let service: &[(&str, &str, ModelledScalar)] = &[
            (
                "the service's own task count",
                "desiredCount",
                ModelledScalar::Number,
            ),
            (
                "the grace period a health check is given",
                "healthCheckGracePeriodSeconds",
                ModelledScalar::Number,
            ),
            (
                "a load balancer binding's container port",
                "containerPort",
                ModelledScalar::Number,
            ),
            (
                "a capacity provider strategy item's weight",
                "weight",
                ModelledScalar::Number,
            ),
            (
                "the deployment configuration's upper bound",
                "maximumPercent",
                ModelledScalar::Number,
            ),
            (
                "a managed EBS volume's provisioned throughput",
                "throughput",
                ModelledScalar::Number,
            ),
            (
                "the managed-tag flag",
                "enableECSManagedTags",
                ModelledScalar::Boolean,
            ),
            (
                "the circuit breaker's own flag",
                "enable",
                ModelledScalar::Boolean,
            ),
            (
                "a managed EBS volume's encryption flag",
                "encrypted",
                ModelledScalar::Boolean,
            ),
            (
                "a log driver's own options, keyed by the requester",
                "options",
                ModelledScalar::TextThroughout,
            ),
            (
                "the task definition the service runs",
                "taskDefinition",
                ModelledScalar::AsWritten,
            ),
            (
                "a member that names a caller's own datum",
                "value",
                ModelledScalar::AsWritten,
            ),
        ];
        for (case, member, expected) in service {
            let actual = service_scalar(member);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected CreateService to model {member:?} as {expected:?}, \
                 got {actual:?}"
            );
        }

        // (case, member, the top-level member the walk was entered at, the type)
        let task_definition: &[(&str, &str, &str, ModelledScalar)] = &[
            (
                "a container's CPU reservation",
                "cpu",
                CONTAINER_DEFINITIONS,
                ModelledScalar::Number,
            ),
            (
                "a container's memory reservation",
                "memory",
                CONTAINER_DEFINITIONS,
                ModelledScalar::Number,
            ),
            (
                "the task's own CPU size, which is text",
                "cpu",
                "cpu",
                ModelledScalar::AsWritten,
            ),
            (
                "the task's own memory size, which is text",
                "memory",
                "memory",
                ModelledScalar::AsWritten,
            ),
            (
                "a port mapping's host port",
                "hostPort",
                CONTAINER_DEFINITIONS,
                ModelledScalar::Number,
            ),
            (
                "a health check's retry count",
                "retries",
                CONTAINER_DEFINITIONS,
                ModelledScalar::Number,
            ),
            (
                "a ulimit's soft limit",
                "softLimit",
                CONTAINER_DEFINITIONS,
                ModelledScalar::Number,
            ),
            (
                "the ephemeral storage a task is given",
                "sizeInGiB",
                "ephemeralStorage",
                ModelledScalar::Number,
            ),
            (
                "a container's essential flag",
                "essential",
                CONTAINER_DEFINITIONS,
                ModelledScalar::Boolean,
            ),
            (
                "a mount point's read-only flag",
                "readOnly",
                CONTAINER_DEFINITIONS,
                ModelledScalar::Boolean,
            ),
            (
                "the fault-injection flag",
                "enableFaultInjection",
                "enableFaultInjection",
                ModelledScalar::Boolean,
            ),
            (
                "a container's docker labels, keyed by the requester",
                "dockerLabels",
                CONTAINER_DEFINITIONS,
                ModelledScalar::TextThroughout,
            ),
            (
                "the image a container runs",
                "image",
                CONTAINER_DEFINITIONS,
                ModelledScalar::AsWritten,
            ),
        ];
        for (case, member, entered_at, expected) in task_definition {
            let actual = task_definition_scalar(member, entered_at);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected RegisterTaskDefinition to model {member:?} as \
                 {expected:?}, got {actual:?}"
            );
        }

        // (case, member, what CreateCluster models it as)
        let cluster: &[(&str, &str, ModelledScalar)] = &[
            (
                "a default strategy item's base",
                "base",
                ModelledScalar::Number,
            ),
            (
                "the CloudWatch encryption flag of the exec log configuration",
                "cloudWatchEncryptionEnabled",
                ModelledScalar::Boolean,
            ),
            (
                "the Amazon S3 encryption flag beside it",
                "s3EncryptionEnabled",
                ModelledScalar::Boolean,
            ),
            (
                "a cluster setting's value, which is a caller's own datum",
                "value",
                ModelledScalar::AsWritten,
            ),
        ];
        for (case, member, expected) in cluster {
            let actual = cluster_scalar(member);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected CreateCluster to model {member:?} as {expected:?}, \
                 got {actual:?}"
            );
        }

        // (case, member, what CreateCapacityProvider models it as)
        let capacity_provider: &[(&str, &str, ModelledScalar)] = &[
            (
                "the managed scaling's target capacity",
                "targetCapacity",
                ModelledScalar::Number,
            ),
            (
                "its smallest scaling step",
                "minimumScalingStepSize",
                ModelledScalar::Number,
            ),
            (
                "the warm-up a launched instance is given",
                "instanceWarmupPeriod",
                ModelledScalar::Number,
            ),
            (
                "a bound of an instance-requirement range",
                "min",
                ModelledScalar::Number,
            ),
            (
                "a launch template's FIPS flag",
                "fipsEnabled",
                ModelledScalar::Boolean,
            ),
            (
                "a local-storage configuration's own flag",
                "useLocalStorage",
                ModelledScalar::Boolean,
            ),
            (
                "the Auto Scaling group the provider is backed by",
                "autoScalingGroupArn",
                ModelledScalar::AsWritten,
            ),
        ];
        for (case, member, expected) in capacity_provider {
            let actual = capacity_provider_scalar(member);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected CreateCapacityProvider to model {member:?} as \
                 {expected:?}, got {actual:?}"
            );
        }
    }
}
