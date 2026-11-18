// SPDX-License-Identifier: CC0-1.0

//! The SERVICE-OWNED catalogs: the built-in FIS actions and the target resource types
//! they can be pointed at.
//!
//! No API creates, updates or deletes an entry in either table, so `ListActions`,
//! `GetAction`, `ListTargetResourceTypes` and `GetTargetResourceType` all report from
//! here rather than from anything a caller stored — which is what makes the list and the
//! detail view of one entry agree.

use super::*;

/// One parameter of a built-in FIS action, as [`ACTION_CATALOG`] records it.
#[derive(Debug, Clone, Copy)]
pub struct ActionCatalogParameter {
    /// The key the parameter is named by in `Action$parameters`.
    pub name: &'static str,
    /// `ActionParameter$description`.
    pub description: &'static str,
    /// `ActionParameter$required` — whether an experiment template's action must set it.
    pub required: bool,
}

/// One supported target of a built-in FIS action, as [`ACTION_CATALOG`] records it.
#[derive(Debug, Clone, Copy)]
pub struct ActionCatalogTarget {
    /// The key the target is named by in `Action$targets`, which is also the key an
    /// experiment template's action refers to a target by.
    pub name: &'static str,
    /// `ActionTarget$resourceType` — a `TargetResourceTypeId`.
    pub resource_type: &'static str,
}

/// One built-in FIS action.
///
/// An action is SERVICE-owned: FIS publishes the catalog and no API creates, updates or
/// deletes an entry, so `GetAction` and `ListActions` report from this table rather than
/// from anything a caller stored. The table is what makes those two operations agree.
#[derive(Debug, Clone, Copy)]
pub struct ActionCatalogEntry {
    /// `ActionId` — the fully qualified action id (`aws:<service>:<operation>`).
    pub id: &'static str,
    /// `ActionDescription`.
    pub description: &'static str,
    /// The action's parameters, empty for an action that takes none.
    pub parameters: &'static [ActionCatalogParameter],
    /// The target resource types the action supports, empty for an action that needs no
    /// target.
    pub targets: &'static [ActionCatalogTarget],
    /// The API calls the experiment's execution role has to be allowed to perform for this
    /// action to run, empty for an action that calls no other service's API.
    ///
    /// One entry per CALL the action makes, and each entry holds the documented GRANTS that
    /// authorize that call: a call is allowed when the role may perform ANY of them, because
    /// a reference that documents two spellings of one grant documents two ways of allowing
    /// the same action, not two requirements.
    ///
    /// NOT part of the `Action` shape — `GetAction` reports no permissions — but part of
    /// what running the action MEANS: FIS runs it under the template's `roleArn`, so an
    /// action whose API calls that role is not allowed to make cannot be carried out (see
    /// [`action_permissions`]). The primary API call of each action, as the FIS actions
    /// reference states it for that action; an action this table records no permission for
    /// is never failed for privileges.
    pub permissions: &'static [&'static [&'static str]],
}

impl ActionCatalogEntry {
    /// The `Action` `GetAction` reports for this entry, in `region`.
    ///
    /// `tags` is an empty map: an action is service-owned, so there is nothing for a
    /// caller to have tagged on it.
    pub fn to_action(&self, region: &str) -> Action {
        Action {
            id: Some(self.id.to_string()),
            arn: Some(action_arn(region, self.id)),
            description: Some(self.description.to_string()),
            parameters: Some(
                self.parameters
                    .iter()
                    .map(|parameter| {
                        (
                            parameter.name.to_string(),
                            ActionParameter {
                                description: Some(parameter.description.to_string()),
                                required: Some(parameter.required),
                            },
                        )
                    })
                    .collect(),
            ),
            targets: Some(
                self.targets
                    .iter()
                    .map(|target| {
                        (
                            target.name.to_string(),
                            ActionTarget {
                                resource_type: Some(target.resource_type.to_string()),
                            },
                        )
                    })
                    .collect(),
            ),
            tags: Some(HashMap::new()),
        }
    }

    /// The `ActionSummary` a page of `ListActions` reports for this entry, in `region`.
    ///
    /// A summary carries the same service-owned facts the full `Action` does, minus the
    /// parameter contract: `ActionSummary` models `id`, `arn`, `description`, `targets` and
    /// `tags` and no `parameters`, so a client that needs an action's parameters reads it
    /// with `GetAction`. `tags` is an empty map for the same reason it is on the full
    /// action: an action is service-owned, so there is nothing for a caller to have tagged.
    pub fn to_summary(&self, region: &str) -> ActionSummary {
        ActionSummary {
            id: Some(self.id.to_string()),
            arn: Some(action_arn(region, self.id)),
            description: Some(self.description.to_string()),
            targets: Some(
                self.targets
                    .iter()
                    .map(|target| {
                        (
                            target.name.to_string(),
                            ActionTarget {
                                resource_type: Some(target.resource_type.to_string()),
                            },
                        )
                    })
                    .collect(),
            ),
            tags: Some(HashMap::new()),
        }
    }
}

/// The IAM action the `aws:fis:inject-api-*` family and the EC2 API-error actions need:
/// the FIS actions reference states `ec2:InjectApiError` for them, scoped with the
/// `ec2:FisActionId` condition key — which is why the gate decides with that key in the
/// request's condition context (see [`action_permissions`]), so a policy written the way
/// the reference writes it grants the action it names and only that action.
pub const EC2_INJECT_API_ERROR: &str = "ec2:InjectApiError";

/// The FIS-NAMESPACED grant for `aws:fis:inject-api-internal-error`: the AWS Service
/// Authorization Reference publishes one IAM action per fault the family injects
/// (`fis:InjectApiInternalError`, `fis:InjectApiThrottleError`,
/// `fis:InjectApiUnavailableError`), which is the other documented spelling of the grant
/// [`EC2_INJECT_API_ERROR`] is one spelling of. A role allowed EITHER may perform the
/// action, so the catalog states both and the gate is satisfied by one of them
/// ([`ActionCatalogEntry::permissions`]).
pub const FIS_INJECT_API_INTERNAL_ERROR: &str = "fis:InjectApiInternalError";

/// See [`FIS_INJECT_API_INTERNAL_ERROR`] — the fault `aws:fis:inject-api-throttle-error`
/// injects.
pub const FIS_INJECT_API_THROTTLE_ERROR: &str = "fis:InjectApiThrottleError";

/// See [`FIS_INJECT_API_INTERNAL_ERROR`] — the fault
/// `aws:fis:inject-api-unavailable-error` injects.
pub const FIS_INJECT_API_UNAVAILABLE_ERROR: &str = "fis:InjectApiUnavailableError";

/// The grants that authorize `aws:fis:inject-api-internal-error`'s own call.
const INJECT_API_INTERNAL_ERROR_GRANTS: &[&str] =
    &[EC2_INJECT_API_ERROR, FIS_INJECT_API_INTERNAL_ERROR];

/// The grants that authorize `aws:fis:inject-api-throttle-error`'s own call.
const INJECT_API_THROTTLE_ERROR_GRANTS: &[&str] =
    &[EC2_INJECT_API_ERROR, FIS_INJECT_API_THROTTLE_ERROR];

/// The grants that authorize `aws:fis:inject-api-unavailable-error`'s own call.
const INJECT_API_UNAVAILABLE_ERROR_GRANTS: &[&str] =
    &[EC2_INJECT_API_ERROR, FIS_INJECT_API_UNAVAILABLE_ERROR];

/// The condition key the documented `ec2:InjectApiError` grant is scoped by: "the FIS
/// action id", which is the value the experiment's action carries.
pub const FIS_ACTION_ID_CONDITION_KEY: &str = "ec2:FisActionId";

/// The built-in FIS actions this emulator serves.
///
/// SOURCE: the AWS FIS actions reference
/// (<https://docs.aws.amazon.com/fis/latest/userguide/fis-actions-reference.html>), which
/// `Action`'s own model documentation points at ("Describes an action. For more
/// information, see FIS actions in the Fault Injection Service User Guide"). Each entry
/// records what that reference states for the action: its id, a one-line summary of what
/// it does, the parameters it accepts and which of them are required, the target
/// resource types it supports under the key an experiment template refers to them by, and
/// the IAM action its own API call needs ("the IAM permissions required" the reference
/// lists for each action), which the experiment's execution role has to be allowed to
/// perform for the action to run.
///
/// It holds the catalog that reference PUBLISHES, not a sample of one: a client that reads
/// an action's parameter and target contract before it builds an experiment template gets
/// that contract for every action FIS documents, rather than a 404 for the ones this table
/// happened to leave out. An id the reference publishes NO action for is not an action at
/// all, and answers `ResourceNotFoundException` rather than a fabricated one.
///
/// Three conventions the rows follow, all taken from the reference itself:
///
/// * a row's `description` is the string the catalog PUBLISHES for that action id rather
///   than a restatement of it in this emulator's own words: it is the action's own summary,
///   reported character for character, punctuation included (`aws:ebs:pause-volume-io`
///   publishes no closing period, `aws:directconnect:virtual-interface-disconnect` publishes
///   a noun phrase rather than a sentence), and it is the value a client that renders or
///   snapshots `Action$description` sees. The published strings follow no one pattern — some
///   are imperative (`aws:ec2:reboot-instances`, `aws:ec2:stop-instances`,
///   `aws:ec2:terminate-instances`, `aws:ssm:send-command`,
///   `aws:ssm:start-automation-execution` and `aws:arc:start-zonal-autoshift` are published
///   that way, which is why they read differently from their neighbours), others open with
///   the verb in the third person — so a row carries the string published for ITS id and
///   never one shaped after a sibling row's, and a row whose published string is not
///   established keeps the text it has rather than being restated to look published;
/// * an action FIS performs through another service's agent carries that service's own
///   call as its permission — the `aws:ecs:task-*` actions run through SSM on the task, so
///   `ssm:SendCommand` is what their execution role needs, and the `aws:eks:*` actions
///   reach the cluster's Kubernetes API, so `eks:DescribeCluster` is;
/// * a row records a permission only where the reference states ONE call for the action.
///   An action whose entry records none is never failed for privileges (see
///   [`ActionCatalogEntry::permissions`]), which is the same reading
///   [`action_permissions`] takes everywhere else: this emulator refuses only what it can
///   SHOW, and a guessed permission would refuse what it cannot.
///
/// Held in the order `ListActions` enumerates it in, which for the actions is ascending by
/// id; like the type table below, that order is DATA the table carries, and
/// [`catalog_page`](super::catalog_page) pages it as written rather than re-sorting it.
pub const ACTION_CATALOG: &[ActionCatalogEntry] = &[
    ActionCatalogEntry {
        id: "aws:arc:start-zonal-autoshift",
        description: "Shift application traffic away from, and back to, an Availability Zone.",
        parameters: &[
            ActionCatalogParameter {
                name: "availabilityZoneIdentifier",
                description: "Traffic moves away from this Availability Zone.",
                required: true,
            },
            ActionCatalogParameter {
                name: "duration",
                description: "The length of time to shift away from the Availability Zone \
                              (ISO 8601 duration).",
                required: true,
            },
        ],
        targets: &[ActionCatalogTarget {
            name: "ManagedResources",
            resource_type: "aws:arc:zonal-shift-managed-resource",
        }],
        // The reference states no single API call for this action: FIS performs it through
        // Application Recovery Controller's own zonal-autoshift configuration, so this row
        // records no permission rather than one it would guess (see
        // [`ActionCatalogEntry::permissions`]).
        permissions: &[],
    },
    ActionCatalogEntry {
        id: "aws:cloudwatch:assert-alarm-state",
        description: "Asserts that the CloudWatch alarms are in the expected states.",
        parameters: &[
            ActionCatalogParameter {
                name: "alarmArns",
                description: "The Amazon Resource Names (ARNs) of the alarms, separated by commas.",
                required: true,
            },
            ActionCatalogParameter {
                name: "alarmStates",
                description: "The alarm states, separated by commas.",
                required: true,
            },
        ],
        targets: &[],
        permissions: &[&["cloudwatch:DescribeAlarms"]],
    },
    ActionCatalogEntry {
        id: "aws:directconnect:virtual-interface-disconnect",
        description: "Direct Connect Failover Test.",
        parameters: &[ActionCatalogParameter {
            name: "duration",
            description: "The duration of the action, in ISO 8601 format.",
            required: true,
        }],
        targets: &[ActionCatalogTarget {
            name: "VirtualInterfaces",
            resource_type: "aws:directconnect:virtual-interface",
        }],
        permissions: &[&["directconnect:StartBgpFailoverTest"]],
    },
    ActionCatalogEntry {
        id: "aws:dsql:cluster-connection-failure",
        description: "Simulates connection failures to a DSQL cluster for the specified \
                      duration.",
        parameters: &[ActionCatalogParameter {
            name: "duration",
            description: "The duration of the action, in ISO 8601 format.",
            required: true,
        }],
        targets: &[ActionCatalogTarget {
            name: "Clusters",
            resource_type: "aws:dsql:cluster",
        }],
        // The reference states no single API call for this action, so this row records no
        // permission rather than one it would guess (see
        // [`ActionCatalogEntry::permissions`]).
        permissions: &[],
    },
    ActionCatalogEntry {
        id: "aws:dynamodb:global-table-pause-replication",
        description: "Disrupts DynamoDB Global Table regional connectivity by pausing \
                      replication and blocking table operations in the affected region.",
        parameters: &[ActionCatalogParameter {
            name: "duration",
            description: "The duration of the action, in ISO 8601 format.",
            required: true,
        }],
        targets: &[ActionCatalogTarget {
            name: "Tables",
            resource_type: "aws:dynamodb:global-table",
        }],
        permissions: &[&["dynamodb:PauseReplication"]],
    },
    ActionCatalogEntry {
        id: "aws:ebs:pause-volume-io",
        description: "Pauses IO for a set of EBS volumes",
        parameters: &[ActionCatalogParameter {
            name: "duration",
            description: "The duration of the action, in ISO 8601 format.",
            required: true,
        }],
        targets: &[ActionCatalogTarget {
            name: "Volumes",
            resource_type: "aws:ec2:ebs-volume",
        }],
        permissions: &[&["ec2:PauseVolumeIO"]],
    },
    ActionCatalogEntry {
        id: "aws:ebs:volume-io-latency",
        description: "Sets latency to EBS volume I/O operations.",
        parameters: &[
            ActionCatalogParameter {
                name: "duration",
                description: "The duration of the action, in ISO 8601 format.",
                required: true,
            },
            ActionCatalogParameter {
                name: "latencyMilliseconds",
                description: "The latency added to each I/O operation, in milliseconds.",
                required: true,
            },
        ],
        targets: &[ActionCatalogTarget {
            name: "Volumes",
            resource_type: "aws:ec2:ebs-volume",
        }],
        // See `aws:dsql:cluster-connection-failure`: the reference states no single API call
        // for this action either.
        permissions: &[],
    },
    ActionCatalogEntry {
        id: "aws:ec2:api-insufficient-instance-capacity-error",
        description: "Cause the EC2 service to return insufficient capacity error responses \
                      for specific callers.",
        parameters: &[
            ActionCatalogParameter {
                name: "availabilityZoneIdentifiers",
                description: "The Availability Zones, separated by commas.",
                required: true,
            },
            ActionCatalogParameter {
                name: "duration",
                description: "The duration of the action, in ISO 8601 format.",
                required: true,
            },
            ActionCatalogParameter {
                name: "percentage",
                description: "The percentage of calls to affect.",
                required: false,
            },
        ],
        targets: &[ActionCatalogTarget {
            name: "Roles",
            resource_type: "aws:iam:role",
        }],
        permissions: &[&[EC2_INJECT_API_ERROR]],
    },
    ActionCatalogEntry {
        id: "aws:ec2:asg-insufficient-instance-capacity-error",
        description: "Cause the targeted AutoScaling Groups to receive insufficient instance \
                      capacity errors when attempting to provision new instances.",
        parameters: &[
            ActionCatalogParameter {
                name: "availabilityZoneIdentifiers",
                description: "The Availability Zones, separated by commas.",
                required: false,
            },
            ActionCatalogParameter {
                name: "duration",
                description: "The duration of the action, in ISO 8601 format.",
                required: true,
            },
            ActionCatalogParameter {
                name: "percentage",
                description: "The percentage of calls to affect.",
                required: false,
            },
        ],
        targets: &[ActionCatalogTarget {
            name: "AutoScalingGroups",
            resource_type: "aws:ec2:autoscaling-group",
        }],
        permissions: &[&[EC2_INJECT_API_ERROR]],
    },
    ActionCatalogEntry {
        id: "aws:ec2:reboot-instances",
        description: "Reboot the specified EC2 instances.",
        parameters: &[],
        targets: &[ActionCatalogTarget {
            name: "Instances",
            resource_type: "aws:ec2:instance",
        }],
        permissions: &[&["ec2:RebootInstances"]],
    },
    ActionCatalogEntry {
        id: "aws:ec2:send-spot-instance-interruptions",
        description: "Interrupt the specified EC2 Spot instances.",
        parameters: &[ActionCatalogParameter {
            name: "durationBeforeInterruption",
            description: "The length of time before the interruption notice is sent, in ISO 8601 \
                          format.",
            required: false,
        }],
        targets: &[ActionCatalogTarget {
            name: "SpotInstances",
            resource_type: "aws:ec2:spot-instance",
        }],
        permissions: &[&["ec2:SendSpotInstanceInterruptions"]],
    },
    ActionCatalogEntry {
        id: "aws:ec2:stop-instances",
        description: "Stop the specified EC2 instances.",
        parameters: &[
            ActionCatalogParameter {
                name: "completeIfInstancesTerminated",
                description: "Whether to complete the action if the target instances were \
                              terminated instead of stopped.",
                required: false,
            },
            ActionCatalogParameter {
                name: "startInstancesAfterDuration",
                description: "The length of time to wait before starting the instances again, in \
                              ISO 8601 format.",
                required: false,
            },
        ],
        targets: &[ActionCatalogTarget {
            name: "Instances",
            resource_type: "aws:ec2:instance",
        }],
        permissions: &[&["ec2:StopInstances"]],
    },
    ActionCatalogEntry {
        id: "aws:ec2:terminate-instances",
        description: "Terminate the specified EC2 instances.",
        parameters: &[],
        targets: &[ActionCatalogTarget {
            name: "Instances",
            resource_type: "aws:ec2:instance",
        }],
        permissions: &[&["ec2:TerminateInstances"]],
    },
    ActionCatalogEntry {
        id: "aws:ecs:drain-container-instances",
        description: "Drain percentage of underlying EC2 instances on an ECS cluster.",
        parameters: &[
            ActionCatalogParameter {
                name: "drainagePercentage",
                description: "The percentage of container instances to drain.",
                required: true,
            },
            ActionCatalogParameter {
                name: "duration",
                description: "The duration of the action, in ISO 8601 format.",
                required: true,
            },
        ],
        targets: &[ActionCatalogTarget {
            name: "Clusters",
            resource_type: "aws:ecs:cluster",
        }],
        permissions: &[&["ecs:UpdateContainerInstancesState"]],
    },
    ActionCatalogEntry {
        id: "aws:ecs:stop-task",
        description: "Stop the specified ECS tasks of an ECS cluster.",
        parameters: &[],
        targets: &[ActionCatalogTarget {
            name: "Tasks",
            resource_type: "aws:ecs:task",
        }],
        permissions: &[&["ecs:StopTask"]],
    },
    ActionCatalogEntry {
        id: "aws:ecs:task-cpu-stress",
        description: "It runs CPU stress via stress-ng tool.",
        parameters: &[
            ActionCatalogParameter {
                name: "duration",
                description: "The duration of the action, in ISO 8601 format.",
                required: true,
            },
            ActionCatalogParameter {
                name: "installDependencies",
                description: "Whether to install the dependencies the action needs on the target \
                              tasks.",
                required: false,
            },
            ActionCatalogParameter {
                name: "percent",
                description: "The percentage of CPU load to generate.",
                required: false,
            },
            ActionCatalogParameter {
                name: "workers",
                description: "The number of stress workers to run.",
                required: false,
            },
        ],
        targets: &[ActionCatalogTarget {
            name: "Tasks",
            resource_type: "aws:ecs:task",
        }],
        permissions: &[&["ssm:SendCommand"]],
    },
    ActionCatalogEntry {
        id: "aws:ecs:task-io-stress",
        description: "It runs IO stress via stress-ng tool.",
        parameters: &[
            ActionCatalogParameter {
                name: "duration",
                description: "The duration of the action, in ISO 8601 format.",
                required: true,
            },
            ActionCatalogParameter {
                name: "installDependencies",
                description: "Whether to install the dependencies the action needs on the target \
                              tasks.",
                required: false,
            },
            ActionCatalogParameter {
                name: "percent",
                description: "The percentage of the file system to use.",
                required: false,
            },
            ActionCatalogParameter {
                name: "workers",
                description: "The number of stress workers to run.",
                required: false,
            },
        ],
        targets: &[ActionCatalogTarget {
            name: "Tasks",
            resource_type: "aws:ecs:task",
        }],
        permissions: &[&["ssm:SendCommand"]],
    },
    ActionCatalogEntry {
        id: "aws:ecs:task-kill-process",
        description: "Kills a process on a set of Amazon ECS tasks.",
        parameters: &[
            ActionCatalogParameter {
                name: "installDependencies",
                description: "Whether to install the dependencies the action needs on the target \
                              tasks.",
                required: false,
            },
            ActionCatalogParameter {
                name: "processName",
                description: "The name of the process to kill.",
                required: true,
            },
            ActionCatalogParameter {
                name: "signal",
                description: "The signal to send to the process.",
                required: false,
            },
        ],
        targets: &[ActionCatalogTarget {
            name: "Tasks",
            resource_type: "aws:ecs:task",
        }],
        permissions: &[&["ssm:SendCommand"]],
    },
    ActionCatalogEntry {
        id: "aws:ecs:task-network-blackhole-port",
        description: "Drops traffic on a port of a set of Amazon ECS tasks.",
        parameters: &[
            ActionCatalogParameter {
                name: "duration",
                description: "The duration of the action, in ISO 8601 format.",
                required: true,
            },
            ActionCatalogParameter {
                name: "installDependencies",
                description: "Whether to install the dependencies the action needs on the target \
                              tasks.",
                required: false,
            },
            ActionCatalogParameter {
                name: "port",
                description: "The port number of the traffic to drop.",
                required: true,
            },
            ActionCatalogParameter {
                name: "protocol",
                description: "The protocol of the traffic to drop.",
                required: true,
            },
            ActionCatalogParameter {
                name: "trafficType",
                description: "The type of traffic to drop, ingress or egress.",
                required: true,
            },
        ],
        targets: &[ActionCatalogTarget {
            name: "Tasks",
            resource_type: "aws:ecs:task",
        }],
        permissions: &[&["ssm:SendCommand"]],
    },
    ActionCatalogEntry {
        id: "aws:ecs:task-network-latency",
        description: "Adds latency and jitter to the network traffic of a set of Amazon ECS \
                      tasks.",
        parameters: &[
            ActionCatalogParameter {
                name: "delayMilliseconds",
                description: "The delay to add to the traffic, in milliseconds.",
                required: false,
            },
            ActionCatalogParameter {
                name: "duration",
                description: "The duration of the action, in ISO 8601 format.",
                required: true,
            },
            ActionCatalogParameter {
                name: "installDependencies",
                description: "Whether to install the dependencies the action needs on the target \
                              tasks.",
                required: false,
            },
            ActionCatalogParameter {
                name: "jitterMilliseconds",
                description: "The variation of the delay, in milliseconds.",
                required: false,
            },
            ActionCatalogParameter {
                name: "sources",
                description: "The hosts whose traffic is delayed, separated by commas.",
                required: true,
            },
        ],
        targets: &[ActionCatalogTarget {
            name: "Tasks",
            resource_type: "aws:ecs:task",
        }],
        permissions: &[&["ssm:SendCommand"]],
    },
    ActionCatalogEntry {
        id: "aws:ecs:task-network-packet-loss",
        description: "Drops a percentage of the network packets of a set of Amazon ECS tasks.",
        parameters: &[
            ActionCatalogParameter {
                name: "duration",
                description: "The duration of the action, in ISO 8601 format.",
                required: true,
            },
            ActionCatalogParameter {
                name: "installDependencies",
                description: "Whether to install the dependencies the action needs on the target \
                              tasks.",
                required: false,
            },
            ActionCatalogParameter {
                name: "lossPercent",
                description: "The percentage of packets to drop.",
                required: false,
            },
            ActionCatalogParameter {
                name: "sources",
                description: "The hosts whose packets are dropped, separated by commas.",
                required: true,
            },
        ],
        targets: &[ActionCatalogTarget {
            name: "Tasks",
            resource_type: "aws:ecs:task",
        }],
        permissions: &[&["ssm:SendCommand"]],
    },
    ActionCatalogEntry {
        id: "aws:eks:inject-kubernetes-custom-resource",
        description: "Applies a Kubernetes custom resource to an Amazon EKS cluster.",
        parameters: &[
            ActionCatalogParameter {
                name: "kubernetesApiVersion",
                description: "The API version of the Kubernetes custom resource.",
                required: true,
            },
            ActionCatalogParameter {
                name: "kubernetesKind",
                description: "The kind of the Kubernetes custom resource.",
                required: true,
            },
            ActionCatalogParameter {
                name: "kubernetesNamespace",
                description: "The namespace of the Kubernetes custom resource.",
                required: true,
            },
            ActionCatalogParameter {
                name: "kubernetesSpec",
                description: "The spec of the Kubernetes custom resource, as a JSON object.",
                required: true,
            },
            ActionCatalogParameter {
                name: "maxDuration",
                description: "The longest the action waits for the custom resource to finish, in \
                              ISO 8601 format.",
                required: true,
            },
        ],
        targets: &[ActionCatalogTarget {
            name: "Cluster",
            resource_type: "aws:eks:cluster",
        }],
        permissions: &[&["eks:DescribeCluster"]],
    },
    ActionCatalogEntry {
        id: "aws:eks:pod-cpu-stress",
        description: "Runs CPU stress on a set of Kubernetes pods.",
        parameters: &[
            ActionCatalogParameter {
                name: "duration",
                description: "The duration of the action, in ISO 8601 format.",
                required: true,
            },
            ActionCatalogParameter {
                name: "kubernetesServiceAccount",
                description: "The Kubernetes service account the action runs under.",
                required: true,
            },
            ActionCatalogParameter {
                name: "percent",
                description: "The percentage of CPU load to generate.",
                required: false,
            },
            ActionCatalogParameter {
                name: "workers",
                description: "The number of stress workers to run.",
                required: false,
            },
        ],
        targets: &[ActionCatalogTarget {
            name: "Pods",
            resource_type: "aws:eks:pod",
        }],
        permissions: &[&["eks:DescribeCluster"]],
    },
    ActionCatalogEntry {
        id: "aws:eks:pod-delete",
        description: "Deletes a set of Kubernetes pods.",
        parameters: &[
            ActionCatalogParameter {
                name: "gracePeriodSeconds",
                description: "The time to wait for a pod to terminate, in seconds.",
                required: false,
            },
            ActionCatalogParameter {
                name: "kubernetesServiceAccount",
                description: "The Kubernetes service account the action runs under.",
                required: true,
            },
        ],
        targets: &[ActionCatalogTarget {
            name: "Pods",
            resource_type: "aws:eks:pod",
        }],
        permissions: &[&["eks:DescribeCluster"]],
    },
    ActionCatalogEntry {
        id: "aws:eks:pod-io-stress",
        description: "Runs I/O stress on a set of Kubernetes pods.",
        parameters: &[
            ActionCatalogParameter {
                name: "duration",
                description: "The duration of the action, in ISO 8601 format.",
                required: true,
            },
            ActionCatalogParameter {
                name: "kubernetesServiceAccount",
                description: "The Kubernetes service account the action runs under.",
                required: true,
            },
            ActionCatalogParameter {
                name: "percent",
                description: "The percentage of the file system to use.",
                required: false,
            },
            ActionCatalogParameter {
                name: "workers",
                description: "The number of stress workers to run.",
                required: false,
            },
        ],
        targets: &[ActionCatalogTarget {
            name: "Pods",
            resource_type: "aws:eks:pod",
        }],
        permissions: &[&["eks:DescribeCluster"]],
    },
    ActionCatalogEntry {
        id: "aws:eks:pod-memory-stress",
        description: "Runs memory stress on a set of Kubernetes pods.",
        parameters: &[
            ActionCatalogParameter {
                name: "duration",
                description: "The duration of the action, in ISO 8601 format.",
                required: true,
            },
            ActionCatalogParameter {
                name: "kubernetesServiceAccount",
                description: "The Kubernetes service account the action runs under.",
                required: true,
            },
            ActionCatalogParameter {
                name: "percent",
                description: "The percentage of memory to use.",
                required: false,
            },
            ActionCatalogParameter {
                name: "workers",
                description: "The number of stress workers to run.",
                required: false,
            },
        ],
        targets: &[ActionCatalogTarget {
            name: "Pods",
            resource_type: "aws:eks:pod",
        }],
        permissions: &[&["eks:DescribeCluster"]],
    },
    ActionCatalogEntry {
        id: "aws:eks:pod-network-blackhole-port",
        description: "Drops traffic on a port of a set of Kubernetes pods.",
        parameters: &[
            ActionCatalogParameter {
                name: "duration",
                description: "The duration of the action, in ISO 8601 format.",
                required: true,
            },
            ActionCatalogParameter {
                name: "kubernetesServiceAccount",
                description: "The Kubernetes service account the action runs under.",
                required: true,
            },
            ActionCatalogParameter {
                name: "port",
                description: "The port number of the traffic to drop.",
                required: true,
            },
            ActionCatalogParameter {
                name: "protocol",
                description: "The protocol of the traffic to drop.",
                required: true,
            },
            ActionCatalogParameter {
                name: "trafficType",
                description: "The type of traffic to drop, ingress or egress.",
                required: true,
            },
        ],
        targets: &[ActionCatalogTarget {
            name: "Pods",
            resource_type: "aws:eks:pod",
        }],
        permissions: &[&["eks:DescribeCluster"]],
    },
    ActionCatalogEntry {
        id: "aws:eks:pod-network-latency",
        description: "Adds latency and jitter to the network traffic of a set of Kubernetes \
                      pods.",
        parameters: &[
            ActionCatalogParameter {
                name: "delayMilliseconds",
                description: "The delay to add to the traffic, in milliseconds.",
                required: false,
            },
            ActionCatalogParameter {
                name: "duration",
                description: "The duration of the action, in ISO 8601 format.",
                required: true,
            },
            ActionCatalogParameter {
                name: "jitterMilliseconds",
                description: "The variation of the delay, in milliseconds.",
                required: false,
            },
            ActionCatalogParameter {
                name: "kubernetesServiceAccount",
                description: "The Kubernetes service account the action runs under.",
                required: true,
            },
            ActionCatalogParameter {
                name: "sources",
                description: "The hosts whose traffic is delayed, separated by commas.",
                required: true,
            },
        ],
        targets: &[ActionCatalogTarget {
            name: "Pods",
            resource_type: "aws:eks:pod",
        }],
        permissions: &[&["eks:DescribeCluster"]],
    },
    ActionCatalogEntry {
        id: "aws:eks:pod-network-packet-loss",
        description: "Drops a percentage of the network packets of a set of Kubernetes pods.",
        parameters: &[
            ActionCatalogParameter {
                name: "duration",
                description: "The duration of the action, in ISO 8601 format.",
                required: true,
            },
            ActionCatalogParameter {
                name: "kubernetesServiceAccount",
                description: "The Kubernetes service account the action runs under.",
                required: true,
            },
            ActionCatalogParameter {
                name: "lossPercent",
                description: "The percentage of packets to drop.",
                required: false,
            },
            ActionCatalogParameter {
                name: "sources",
                description: "The hosts whose packets are dropped, separated by commas.",
                required: true,
            },
        ],
        targets: &[ActionCatalogTarget {
            name: "Pods",
            resource_type: "aws:eks:pod",
        }],
        permissions: &[&["eks:DescribeCluster"]],
    },
    ActionCatalogEntry {
        id: "aws:eks:terminate-nodegroup-instances",
        description: "Terminates a percentage of the instances of a set of Amazon EKS node \
                      groups.",
        parameters: &[ActionCatalogParameter {
            name: "instanceTerminationPercentage",
            description: "The percentage of instances to terminate.",
            required: true,
        }],
        targets: &[ActionCatalogTarget {
            name: "Nodegroups",
            resource_type: "aws:eks:nodegroup",
        }],
        permissions: &[&["ec2:TerminateInstances"]],
    },
    ActionCatalogEntry {
        id: "aws:elasticache:replicationgroup-interrupt-az-power",
        description: "Interrupts power in one Availability Zone of a set of ElastiCache \
                      replication groups.",
        parameters: &[ActionCatalogParameter {
            name: "duration",
            description: "The duration of the action, in ISO 8601 format.",
            required: true,
        }],
        targets: &[ActionCatalogTarget {
            name: "ReplicationGroups",
            resource_type: "aws:elasticache:replicationgroup",
        }],
        permissions: &[&["elasticache:InterruptClusterAZPower"]],
    },
    ActionCatalogEntry {
        id: "aws:fis:inject-api-internal-error",
        description: "Injects internal errors into the API requests made with a set of IAM \
                      roles.",
        parameters: &[
            ActionCatalogParameter {
                name: "duration",
                description: "The duration of the action, in ISO 8601 format.",
                required: true,
            },
            ActionCatalogParameter {
                name: "operations",
                description: "The API operations to affect, separated by commas.",
                required: true,
            },
            ActionCatalogParameter {
                name: "percentage",
                description: "The percentage of calls to affect.",
                required: false,
            },
            ActionCatalogParameter {
                name: "service",
                description: "The AWS service whose API operations are affected.",
                required: true,
            },
        ],
        targets: &[ActionCatalogTarget {
            name: "Roles",
            resource_type: "aws:iam:role",
        }],
        permissions: &[INJECT_API_INTERNAL_ERROR_GRANTS],
    },
    ActionCatalogEntry {
        id: "aws:fis:inject-api-throttle-error",
        description: "Injects throttling errors into the API requests made with a set of IAM \
                      roles.",
        parameters: &[
            ActionCatalogParameter {
                name: "duration",
                description: "The duration of the action, in ISO 8601 format.",
                required: true,
            },
            ActionCatalogParameter {
                name: "operations",
                description: "The API operations to affect, separated by commas.",
                required: true,
            },
            ActionCatalogParameter {
                name: "percentage",
                description: "The percentage of calls to affect.",
                required: false,
            },
            ActionCatalogParameter {
                name: "service",
                description: "The AWS service whose API operations are affected.",
                required: true,
            },
        ],
        targets: &[ActionCatalogTarget {
            name: "Roles",
            resource_type: "aws:iam:role",
        }],
        permissions: &[INJECT_API_THROTTLE_ERROR_GRANTS],
    },
    ActionCatalogEntry {
        id: "aws:fis:inject-api-unavailable-error",
        description: "Injects unavailable errors into the API requests made with a set of IAM \
                      roles.",
        parameters: &[
            ActionCatalogParameter {
                name: "duration",
                description: "The duration of the action, in ISO 8601 format.",
                required: true,
            },
            ActionCatalogParameter {
                name: "operations",
                description: "The API operations to affect, separated by commas.",
                required: true,
            },
            ActionCatalogParameter {
                name: "percentage",
                description: "The percentage of calls to affect.",
                required: false,
            },
            ActionCatalogParameter {
                name: "service",
                description: "The AWS service whose API operations are affected.",
                required: true,
            },
        ],
        targets: &[ActionCatalogTarget {
            name: "Roles",
            resource_type: "aws:iam:role",
        }],
        permissions: &[INJECT_API_UNAVAILABLE_ERROR_GRANTS],
    },
    ActionCatalogEntry {
        id: "aws:fis:wait",
        description: "Waits for the specified duration.",
        parameters: &[ActionCatalogParameter {
            name: "duration",
            description: "The duration of the action, in ISO 8601 format.",
            required: true,
        }],
        targets: &[],
        // The wait calls no other service's API, so the execution role needs no
        // permission for it: it is the one action whose whole effect is elapsed time.
        permissions: &[],
    },
    ActionCatalogEntry {
        id: "aws:kinesis:stream-expired-iterator-exception",
        description: "Injects ExpiredIteratorException into the read requests of a Kinesis data \
                      stream.",
        parameters: &[ActionCatalogParameter {
            name: "duration",
            description: "The duration of the action, in ISO 8601 format.",
            required: true,
        }],
        targets: &[ActionCatalogTarget {
            name: "KinesisStreams",
            resource_type: "aws:kinesis:stream",
        }],
        // See `aws:dsql:cluster-connection-failure`: no single API call is stated for it.
        permissions: &[],
    },
    ActionCatalogEntry {
        id: "aws:kinesis:stream-provisioned-throughput-exception",
        description: "Injects ProvisionedThroughputExceededException into the requests of a \
                      Kinesis data stream.",
        parameters: &[ActionCatalogParameter {
            name: "duration",
            description: "The duration of the action, in ISO 8601 format.",
            required: true,
        }],
        targets: &[ActionCatalogTarget {
            name: "KinesisStreams",
            resource_type: "aws:kinesis:stream",
        }],
        // See `aws:dsql:cluster-connection-failure`: no single API call is stated for it.
        permissions: &[],
    },
    ActionCatalogEntry {
        id: "aws:lambda:invocation-add-delay",
        description: "Adds delay to the invocations of a set of Lambda functions.",
        parameters: &[
            ActionCatalogParameter {
                name: "duration",
                description: "The duration of the action, in ISO 8601 format.",
                required: true,
            },
            ActionCatalogParameter {
                name: "invocationPercentage",
                description: "The percentage of invocations to affect.",
                required: false,
            },
            ActionCatalogParameter {
                name: "preventExecution",
                description: "Whether to prevent the function's handler from running.",
                required: false,
            },
            ActionCatalogParameter {
                name: "startupDelayMilliseconds",
                description: "The delay to add before the invocation, in milliseconds.",
                required: true,
            },
        ],
        targets: &[ActionCatalogTarget {
            name: "Functions",
            resource_type: "aws:lambda:function",
        }],
        // The reference states no single API call for the Lambda actions: FIS performs them
        // through its Lambda extension, reading the fault configuration the experiment wrote,
        // so this table records no permission rather than one it would guess.
        permissions: &[],
    },
    ActionCatalogEntry {
        id: "aws:lambda:invocation-error",
        description: "Returns an error from the invocations of a set of Lambda functions.",
        parameters: &[
            ActionCatalogParameter {
                name: "duration",
                description: "The duration of the action, in ISO 8601 format.",
                required: true,
            },
            ActionCatalogParameter {
                name: "invocationPercentage",
                description: "The percentage of invocations to affect.",
                required: false,
            },
            ActionCatalogParameter {
                name: "preventExecution",
                description: "Whether to prevent the function's handler from running.",
                required: false,
            },
        ],
        targets: &[ActionCatalogTarget {
            name: "Functions",
            resource_type: "aws:lambda:function",
        }],
        // See `aws:lambda:invocation-add-delay` for why no permission is recorded.
        permissions: &[],
    },
    ActionCatalogEntry {
        id: "aws:lambda:invocation-http-integration-response",
        description: "Returns the specified HTTP integration response from the invocations of a \
                      set of Lambda functions.",
        parameters: &[
            ActionCatalogParameter {
                name: "contentTypeHeader",
                description: "The value of the response's Content-Type header.",
                required: false,
            },
            ActionCatalogParameter {
                name: "duration",
                description: "The duration of the action, in ISO 8601 format.",
                required: true,
            },
            ActionCatalogParameter {
                name: "invocationPercentage",
                description: "The percentage of invocations to affect.",
                required: false,
            },
            ActionCatalogParameter {
                name: "preventExecution",
                description: "Whether to prevent the function's handler from running.",
                required: false,
            },
            ActionCatalogParameter {
                name: "statusCode",
                description: "The HTTP status code of the response.",
                required: true,
            },
        ],
        targets: &[ActionCatalogTarget {
            name: "Functions",
            resource_type: "aws:lambda:function",
        }],
        // See `aws:lambda:invocation-add-delay` for why no permission is recorded.
        permissions: &[],
    },
    ActionCatalogEntry {
        id: "aws:memorydb:multi-region-cluster-pause-replication",
        description: "Pauses replication for a MemoryDB multi-region cluster.",
        parameters: &[ActionCatalogParameter {
            name: "duration",
            description: "The duration of the action, in ISO 8601 format.",
            required: true,
        }],
        targets: &[ActionCatalogTarget {
            name: "MultiRegionClusters",
            resource_type: "aws:memorydb:multi-region-cluster",
        }],
        // See `aws:dsql:cluster-connection-failure`: no single API call is stated for it.
        permissions: &[],
    },
    ActionCatalogEntry {
        id: "aws:network:disrupt-connectivity",
        description: "Disrupts network connectivity for a set of subnets.",
        parameters: &[
            ActionCatalogParameter {
                name: "duration",
                description: "The duration of the action, in ISO 8601 format.",
                required: true,
            },
            ActionCatalogParameter {
                name: "scope",
                description: "The traffic to disrupt.",
                required: true,
            },
        ],
        targets: &[ActionCatalogTarget {
            name: "Subnets",
            resource_type: "aws:ec2:subnet",
        }],
        permissions: &[&["ec2:CreateNetworkAcl"]],
    },
    ActionCatalogEntry {
        id: "aws:network:disrupt-vpc-endpoint",
        description: "Disrupts connectivity to a VPC endpoint.",
        parameters: &[ActionCatalogParameter {
            name: "duration",
            description: "The duration of the action, in ISO 8601 format.",
            required: true,
        }],
        targets: &[ActionCatalogTarget {
            name: "VPCEndpoints",
            resource_type: "aws:ec2:vpc-endpoint",
        }],
        // See `aws:dsql:cluster-connection-failure`: no single API call is stated for it.
        permissions: &[],
    },
    ActionCatalogEntry {
        id: "aws:network:route-table-disrupt-cross-region-connectivity",
        description: "Disrupts traffic between the subnets a set of route tables serve and \
                      another Region.",
        parameters: &[
            ActionCatalogParameter {
                name: "duration",
                description: "The duration of the action, in ISO 8601 format.",
                required: true,
            },
            ActionCatalogParameter {
                name: "region",
                description: "The Region whose traffic is disrupted.",
                required: true,
            },
        ],
        targets: &[ActionCatalogTarget {
            name: "Subnets",
            resource_type: "aws:ec2:subnet",
        }],
        // The reference lists several route-mutating EC2 calls for this action rather than one
        // primary call, so this table records none rather than one it would guess: the
        // privileges gate shows nothing about an action whose entry records no permission.
        permissions: &[],
    },
    ActionCatalogEntry {
        id: "aws:network:transit-gateway-disrupt-cross-region-connectivity",
        description: "Disrupts traffic between a set of transit gateways and another Region.",
        parameters: &[
            ActionCatalogParameter {
                name: "duration",
                description: "The duration of the action, in ISO 8601 format.",
                required: true,
            },
            ActionCatalogParameter {
                name: "region",
                description: "The Region whose traffic is disrupted.",
                required: true,
            },
        ],
        targets: &[ActionCatalogTarget {
            name: "TransitGateways",
            resource_type: "aws:ec2:transit-gateway",
        }],
        // See `aws:network:route-table-disrupt-cross-region-connectivity` for why no
        // permission is recorded.
        permissions: &[],
    },
    ActionCatalogEntry {
        id: "aws:rds:failover-db-cluster",
        description: "Fails over a set of Amazon Aurora DB clusters.",
        parameters: &[],
        targets: &[ActionCatalogTarget {
            name: "Clusters",
            resource_type: "aws:rds:cluster",
        }],
        permissions: &[&["rds:FailoverDBCluster"]],
    },
    ActionCatalogEntry {
        id: "aws:rds:reboot-db-instances",
        description: "Reboots a set of Amazon RDS DB instances.",
        parameters: &[ActionCatalogParameter {
            name: "forceFailover",
            description: "Whether to force a failover of a Multi-AZ DB instance.",
            required: false,
        }],
        targets: &[ActionCatalogTarget {
            name: "DBInstances",
            resource_type: "aws:rds:db",
        }],
        permissions: &[&["rds:RebootDBInstance"]],
    },
    ActionCatalogEntry {
        id: "aws:s3:bucket-pause-replication",
        description: "Pauses replication from a set of S3 buckets.",
        parameters: &[ActionCatalogParameter {
            name: "duration",
            description: "The duration of the action, in ISO 8601 format.",
            required: true,
        }],
        targets: &[ActionCatalogTarget {
            name: "Buckets",
            resource_type: "aws:s3:bucket",
        }],
        permissions: &[&["s3:PauseReplication"]],
    },
    ActionCatalogEntry {
        id: "aws:ssm:send-command",
        description: "Run the specified SSM document.",
        parameters: &[
            ActionCatalogParameter {
                name: "documentArn",
                description: "The Amazon Resource Name (ARN) of the document.",
                required: true,
            },
            ActionCatalogParameter {
                name: "documentParameters",
                description: "The document parameters, as a JSON object.",
                required: false,
            },
            ActionCatalogParameter {
                name: "documentVersion",
                description: "The version of the document.",
                required: false,
            },
            ActionCatalogParameter {
                name: "duration",
                description: "The duration of the action, in ISO 8601 format.",
                required: true,
            },
        ],
        targets: &[ActionCatalogTarget {
            name: "Instances",
            resource_type: "aws:ec2:instance",
        }],
        permissions: &[&["ssm:SendCommand"]],
    },
    ActionCatalogEntry {
        id: "aws:ssm:start-automation-execution",
        description: "Run the specified SSM automation document.",
        parameters: &[
            ActionCatalogParameter {
                name: "documentArn",
                description: "The Amazon Resource Name (ARN) of the automation document.",
                required: true,
            },
            ActionCatalogParameter {
                name: "documentParameters",
                description: "The document parameters, as a JSON object.",
                required: false,
            },
            ActionCatalogParameter {
                name: "documentVersion",
                description: "The version of the document.",
                required: false,
            },
            ActionCatalogParameter {
                name: "maxConcurrency",
                description: "The maximum number of automation executions to run at the same \
                              time.",
                required: false,
            },
            ActionCatalogParameter {
                name: "maxErrors",
                description: "The maximum number of allowed automation execution failures.",
                required: false,
            },
        ],
        targets: &[],
        permissions: &[&["ssm:StartAutomationExecution"]],
    },
];

/// The catalog entry `id` names, or `None` when FIS publishes no such action.
///
/// `ActionId` is matched exactly: an action id is case-sensitive, so a differently cased
/// or abbreviated id names no action.
pub fn action_catalog_entry(id: &str) -> Option<&'static ActionCatalogEntry> {
    ACTION_CATALOG.iter().find(|entry| entry.id == id)
}

/// One parameter of a target resource type, as [`TARGET_RESOURCE_TYPE_CATALOG`] records
/// it.
///
/// A target resource type parameter is what an experiment template's target sets in its
/// own `parameters` map to narrow target resolution, which is why the catalog records the
/// same two things the modelled `TargetResourceTypeParameter` carries: what the parameter
/// means and whether a target of this type has to set it.
#[derive(Debug, Clone, Copy)]
pub struct TargetResourceTypeCatalogParameter {
    /// The key the parameter is named by in `TargetResourceType$parameters`, which is also
    /// the key an experiment template's target sets it under.
    pub name: &'static str,
    /// `TargetResourceTypeParameter$description`.
    pub description: &'static str,
    /// `TargetResourceTypeParameter$required` — whether a target of this resource type has
    /// to set the parameter.
    pub required: bool,
}

/// One target resource type FIS publishes, as [`TARGET_RESOURCE_TYPE_CATALOG`] records it.
///
/// Like an action, a target resource type is SERVICE-owned: no API creates, updates or
/// deletes one, so `ListTargetResourceTypes` and `GetTargetResourceType` report from this
/// table rather than from anything a caller stored.
#[derive(Debug, Clone, Copy)]
pub struct TargetResourceTypeCatalogEntry {
    /// `TargetResourceTypeId` — the fully qualified resource type (`aws:<service>:<type>`),
    /// the same value an experiment template's target and a catalogued action's
    /// `ActionTarget$resourceType` name.
    pub id: &'static str,
    /// `TargetResourceTypeSummary$description`.
    pub description: &'static str,
    /// The parameters a target of this resource type may set, empty for a type that takes
    /// none — which is most of them, because a target is usually named by its resource ids,
    /// ARNs or tags alone.
    ///
    /// NOT part of `TargetResourceTypeSummary`, which a list reports: only the full
    /// `TargetResourceType` of `GetTargetResourceType` carries them.
    pub parameters: &'static [TargetResourceTypeCatalogParameter],
}

impl TargetResourceTypeCatalogEntry {
    /// The `TargetResourceTypeSummary` a page of `ListTargetResourceTypes` reports for
    /// this entry.
    pub fn to_summary(&self) -> TargetResourceTypeSummary {
        TargetResourceTypeSummary {
            resource_type: Some(self.id.to_string()),
            description: Some(self.description.to_string()),
        }
    }

    /// The `TargetResourceType` `GetTargetResourceType` reports for this entry.
    ///
    /// A resource type is service-owned and belongs to no account or region, so there is
    /// nothing here to scope: the reported value is the catalogued row and the parameter
    /// contract that row records, with an EMPTY parameter map for a type that takes no
    /// parameters rather than an absent one — the type has a parameter contract either way.
    pub fn to_target_resource_type(&self) -> TargetResourceType {
        TargetResourceType {
            resource_type: Some(self.id.to_string()),
            description: Some(self.description.to_string()),
            parameters: Some(
                self.parameters
                    .iter()
                    .map(|parameter| {
                        (
                            parameter.name.to_string(),
                            TargetResourceTypeParameter {
                                description: Some(parameter.description.to_string()),
                                required: Some(parameter.required),
                            },
                        )
                    })
                    .collect(),
            ),
        }
    }
}

/// The target resource types FIS PUBLISHES, as `ListTargetResourceTypes` enumerates them
/// and `GetTargetResourceType` reports them one at a time.
///
/// SOURCE: the published type listing itself — its membership, its order and each row's
/// description are catalog DATA the API serves and the model does not carry, so a row holds
/// the very string the listing publishes for that id rather than a restatement of it. The
/// text is reported character for character, punctuation included: most rows end in a
/// period and a few publish none (`aws:dynamodb:global-table`, `aws:ec2:autoscaling-group`,
/// `aws:ec2:ebs-volume`, `aws:s3:bucket`), and a row names its service the way the listing
/// names it, which is not always the way another row does. The parameters a row records are
/// the ones the AWS FIS actions reference
/// (<https://docs.aws.amazon.com/fis/latest/userguide/fis-actions-reference.html>)
/// documents for that type — the types whose targets cannot be named by ids, ARNs or tags
/// alone (an ECS task is named through its cluster and service, an EKS pod through its
/// cluster, namespace and selector) — and only where it states them, so no parameter
/// contract here is invented for a type documented without one.
///
/// The published LISTING is its own catalog, independent of [`ACTION_CATALOG`] in both
/// directions: a published action names the type it is pointed at rather than a row of this
/// table, and the listing can carry a type no catalogued action targets, which is why what
/// makes a type TARGETABLE is this table or the action table
/// ([`action_catalog_targets_resource_type`]) rather than this table alone.
///
/// ORDER: the order the published listing enumerates the types in, which is the order
/// `ListTargetResourceTypes` reports them in and the index a client that pages or indexes
/// the list reads an entry at — ids ascending. The order is DATA, not a consequence of the
/// ids, which is why [`catalog_page`](super::catalog_page) pages the table as written rather
/// than re-sorting it. Each row's parameters are kept sorted by name.
pub const TARGET_RESOURCE_TYPE_CATALOG: &[TargetResourceTypeCatalogEntry] = &[
    TargetResourceTypeCatalogEntry {
        id: "aws:arc:zonal-shift-managed-resource",
        description: "An ARC Zonal Shift Managed Resource.",
        parameters: &[
            TargetResourceTypeCatalogParameter {
                name: "managedResourceTypes",
                description: "The resource types that will use zonal autoshift.",
                required: true,
            },
            TargetResourceTypeCatalogParameter {
                name: "zonalAutoshiftStatus",
                description: "The zonalAutoshiftStatus of the resources you want to target. \
                              The default is enabled.",
                required: false,
            },
        ],
    },
    TargetResourceTypeCatalogEntry {
        id: "aws:directconnect:virtual-interface",
        description: "A Direct Connect Virtual Interface (VIF).",
        parameters: &[],
    },
    TargetResourceTypeCatalogEntry {
        id: "aws:dsql:cluster",
        description: "An Amazon Aurora DSQL cluster.",
        parameters: &[],
    },
    TargetResourceTypeCatalogEntry {
        id: "aws:dynamodb:global-table",
        description: "An Amazon DynamoDB global table",
        parameters: &[],
    },
    TargetResourceTypeCatalogEntry {
        id: "aws:ec2:autoscaling-group",
        description: "An Amazon EC2 Auto Scaling group",
        parameters: &[],
    },
    TargetResourceTypeCatalogEntry {
        id: "aws:ec2:ebs-volume",
        description: "An Amazon Elastic Block Store volume",
        parameters: &[],
    },
    TargetResourceTypeCatalogEntry {
        id: "aws:ec2:instance",
        description: "An Amazon EC2 instance.",
        parameters: &[],
    },
    TargetResourceTypeCatalogEntry {
        id: "aws:ec2:spot-instance",
        description: "An Amazon EC2 Spot Instance.",
        parameters: &[],
    },
    TargetResourceTypeCatalogEntry {
        id: "aws:ec2:subnet",
        description: "An Amazon EC2 subnet.",
        parameters: &[],
    },
    TargetResourceTypeCatalogEntry {
        id: "aws:ec2:transit-gateway",
        description: "An Amazon EC2 Transit Gateway.",
        parameters: &[],
    },
    TargetResourceTypeCatalogEntry {
        id: "aws:ec2:vpc-endpoint",
        description: "A Virtual Private Cloud (VPC) endpoint.",
        parameters: &[],
    },
    TargetResourceTypeCatalogEntry {
        id: "aws:ecs:cluster",
        description: "An Amazon ECS cluster.",
        parameters: &[],
    },
    TargetResourceTypeCatalogEntry {
        id: "aws:ecs:task",
        description: "An Amazon ECS task.",
        parameters: &[
            TargetResourceTypeCatalogParameter {
                name: "cluster",
                description: "The name or ARN of the cluster the tasks run on.",
                required: true,
            },
            TargetResourceTypeCatalogParameter {
                name: "service",
                description: "The name of the service whose tasks are identified.",
                required: true,
            },
        ],
    },
    TargetResourceTypeCatalogEntry {
        id: "aws:eks:cluster",
        description: "An Amazon EKS cluster.",
        parameters: &[],
    },
    TargetResourceTypeCatalogEntry {
        id: "aws:eks:nodegroup",
        description: "An Amazon EKS node group.",
        parameters: &[],
    },
    TargetResourceTypeCatalogEntry {
        id: "aws:eks:pod",
        description: "A Kubernetes pod.",
        parameters: &[
            TargetResourceTypeCatalogParameter {
                name: "availabilityZoneIdentifier",
                description: "The Availability Zone the pods are identified in.",
                required: false,
            },
            TargetResourceTypeCatalogParameter {
                name: "clusterIdentifier",
                description: "The name or ARN of the cluster the pods run on.",
                required: true,
            },
            TargetResourceTypeCatalogParameter {
                name: "namespace",
                description: "The namespace the pods are identified in.",
                required: true,
            },
            TargetResourceTypeCatalogParameter {
                name: "selectorType",
                description: "The type of selector the pods are identified by.",
                required: true,
            },
            TargetResourceTypeCatalogParameter {
                name: "selectorValue",
                description: "The value the selector matches on.",
                required: false,
            },
        ],
    },
    TargetResourceTypeCatalogEntry {
        id: "aws:elasticache:replicationgroup",
        description: "An ElastiCache Replication Group.",
        parameters: &[],
    },
    TargetResourceTypeCatalogEntry {
        id: "aws:iam:role",
        description: "An IAM role.",
        parameters: &[],
    },
    TargetResourceTypeCatalogEntry {
        id: "aws:kinesis:stream",
        description: "AWS Kinesis Data Stream target for the FIS actions.",
        parameters: &[],
    },
    TargetResourceTypeCatalogEntry {
        id: "aws:lambda:function",
        description: "A Lambda Function.",
        parameters: &[],
    },
    TargetResourceTypeCatalogEntry {
        id: "aws:memorydb:multi-region-cluster",
        description: "A MemoryDB multi-region cluster.",
        parameters: &[],
    },
    TargetResourceTypeCatalogEntry {
        id: "aws:rds:cluster",
        description: "An Amazon Aurora DB cluster.",
        parameters: &[],
    },
    TargetResourceTypeCatalogEntry {
        id: "aws:rds:db",
        description: "An Amazon RDS DB instance.",
        parameters: &[],
    },
    TargetResourceTypeCatalogEntry {
        id: "aws:s3:bucket",
        description: "An Amazon S3 bucket",
        parameters: &[],
    },
];

/// The catalog entry `id` names, or `None` when FIS publishes no such target resource
/// type.
///
/// `TargetResourceTypeId` is matched exactly: a resource type id is case-sensitive, so a
/// differently cased or abbreviated id names no resource type.
pub fn target_resource_type_catalog_entry(
    id: &str,
) -> Option<&'static TargetResourceTypeCatalogEntry> {
    TARGET_RESOURCE_TYPE_CATALOG
        .iter()
        .find(|entry| entry.id == id)
}

/// Whether a built-in action can be pointed at the resource type `id`.
///
/// A catalogued action's `ActionTarget$resourceType` names a type an experiment template's
/// target may carry, and that is a different catalog from the published type LISTING: the
/// listing enumerates the types FIS publishes ([`TARGET_RESOURCE_TYPE_CATALOG`]) while
/// [`ACTION_CATALOG`] publishes the actions and the type each of them targets, so a type an
/// action targets is targetable whether or not the listing carries it. Matched exactly, as
/// [`target_resource_type_catalog_entry`] matches a published id.
pub fn action_catalog_targets_resource_type(id: &str) -> bool {
    ACTION_CATALOG.iter().any(|action| {
        action
            .targets
            .iter()
            .any(|target| target.resource_type == id)
    })
}
