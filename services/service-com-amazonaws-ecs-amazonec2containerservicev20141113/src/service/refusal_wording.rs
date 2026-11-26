// SPDX-License-Identifier: CC0-1.0

//! The refusals Amazon ECS words per OPERATION, stated before any handler runs.
//!
//! Most of what this service refuses before dispatch is worded from the request alone — the
//! member at fault and what it failed — and [`super::refusal_message`] renders those from the
//! framework's own walk, the same words whichever operation carried the member. The ones
//! here are not: Amazon ECS publishes a FIXED sentence for them, and WHICH sentence it
//! publishes depends on the operation the request named. The framework's walk is handed the
//! violations without the operation
//! ([`smithy_server_runtime::Service::override_refusal`] takes a refusal and nothing else),
//! so those sentences are decided HERE, where the routing key is still in hand, and the
//! layer that owns the pre-dispatch sequence ([`super::request_pipeline`]) answers them in the
//! modelled error shape each refusal names ([`WordedRefusal`]) — this service's
//! `InvalidParameterException` (HTTP 400) for all but the ones the gateway surface publishes
//! its own shape and status for ([`gateways`]).
//!
//! One function per operation, each reading the request ONCE through the same protocol and
//! generated input schema the dispatch sequence will hand the handler ([`read_input`]), so
//! what is judged here is exactly the value the operation would have acted on. A body that
//! cannot be read at all is no refusal of this layer's — except where the member the read
//! found absent is one whose absence the operation words itself ([`absent_member`]).
//!
//! The conditions, and why each one's words belong to its operation rather than to its
//! member:
//!
//! * **A launch type outside the set the operation accepts.** The sentence lists the launch
//!   types THAT operation publishes, in its order: the operations that place work on
//!   infrastructure publish [`PLACEMENT_LAUNCH_TYPES`] — `CreateService$launchType` names
//!   those three and sends a caller that wants Amazon ECS Managed Instances to
//!   `capacityProviderStrategy` with `launchType` omitted — while the operations that FILTER
//!   a listing by launch type publish the wider [`FILTER_LAUNCH_TYPES`], which a listing can
//!   be asked about whether or not a placement request can name it.
//! * **A deployment strategy outside its set**, which Amazon ECS states as one fixed
//!   sentence about the member rather than by listing the strategies.
//! * **A scheduling strategy outside its set on a LISTING**, whose sentence opens on the
//!   member as a sentence does and lists the two strategies in the order the member
//!   publishes them.
//! * **A target type outside its set on an attribute LISTING**, whose sentence ECHOES the
//!   value the request carried instead of listing the kinds the member takes.
//! * **A stop type outside its set on a request to STOP a rollout**, which Amazon ECS states
//!   as one fixed sentence naming the member in its own spelling, listing neither the stop
//!   types it takes nor the value it was given.
//! * **A status filter whose entries are outside the set**, reported against the LIST — the
//!   whole list as the value, the list under the name the API prints for it, and the
//!   element's own requirement nested inside the list's ([`crate::state::list_member_constraint_message`]),
//!   listing the statuses THAT operation publishes in its order
//!   ([`FILTER_SERVICE_DEPLOYMENT_STATUSES`]).
//! * **A required identifier the request leaves out, or names nothing at all in**, refused
//!   with the one sentence that operation publishes for it — naming no member path, no value
//!   and no count, which is what the framework's clause would state. The gateway surface
//!   publishes one per required member, with the error SHAPE and status it answers each with
//!   ([`gateways`]).
//! * **A list of identifiers that names the same one twice**, refused as the malformed
//!   request Amazon ECS calls it, with the fixed sentence that operation publishes — judged
//!   on the list as the request spells it, before any entry is resolved.
//!
//! An operation absent from here, and a condition absent from the operation's own function,
//! keeps exactly the refusal it had: there is no AWS sentence to put in its place.
//!
//! Beside those sits ONE rule about one MEMBER rather than about an operation
//! ([`cluster_reference_refusal`]): the `cluster` a request names has to be one of the two
//! forms that member documents, and a value that is neither names no cluster in any account.
//! Its sentence is the same whichever operation carried the member, and it is stated here
//! because the model declares no `@pattern` for the framework's walk to key it on — and
//! because it has to be answered BEFORE the cluster is resolved, which each handler does for
//! itself.

pub mod gateways;

use ::aws_smithy_runtime_api::http::Request as RtRequest;
use ::smithy_server_runtime::ServerHttpProtocol;

use crate::ops::types::*;
use crate::service::{absent_member, read_input};

/// WHICH modelled error shape a refusal worded here is answered as.
///
/// This service words most of what it refuses before dispatch as the one error it declares for
/// a request whose parameters it will not serve, but not all of it: a surface that publishes
/// its own refusal publishes the SHAPE too, and a client branches on that shape — an
/// authorization refusal is a role or a policy to fix, a server failure is a call to retry, a
/// bad parameter is a request to correct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusedShape {
    /// `InvalidParameterException`.
    InvalidParameter,
    /// `AccessDeniedException`.
    AccessDenied,
    /// `ServerException`.
    Server,
}

/// A refusal this service words itself, as the caller sees it: the modelled error shape, the
/// status it is answered with, and the sentence.
///
/// The status travels with the refusal rather than with the shape, because the status a shape
/// is answered with is not always the one the model pins on it — see
/// [`WordedRefusal::access_denied`].
#[derive(Debug, Clone)]
pub struct WordedRefusal {
    pub shape: RefusedShape,
    pub status: u16,
    pub message: String,
}

impl WordedRefusal {
    /// A refusal of the request's PARAMETERS: `InvalidParameterException` at the status the
    /// model pins on it (`@httpError 400`), which is what every refusal this layer worded
    /// before there was anything else to word was answered as.
    pub fn invalid_parameter(message: impl Into<String>) -> Self {
        Self {
            shape: RefusedShape::InvalidParameter,
            status: 400,
            message: message.into(),
        }
    }

    /// An AUTHORIZATION refusal: `AccessDeniedException` at `status`, stated by the caller
    /// because the status such a refusal carries is a property of the surface that publishes
    /// it — this service's IAM gate answers the 403 the model pins on the shape, while a
    /// surface recorded answering one in the 400 class states that.
    pub fn access_denied(status: u16, message: impl Into<String>) -> Self {
        Self {
            shape: RefusedShape::AccessDenied,
            status,
            message: message.into(),
        }
    }

    /// A SERVER-side failure: `ServerException` at the status the model pins on it
    /// (`@httpError 500`), the class an SDK retries.
    pub fn server(message: impl Into<String>) -> Self {
        Self {
            shape: RefusedShape::Server,
            status: 500,
            message: message.into(),
        }
    }
}

/// The member every operation below names the launch type in, as the words its refusal is
/// built from ([`crate::state::member_words`]).
const LAUNCH_TYPE_MEMBER: &str = "launchType";

/// The launch types an operation that PLACES work publishes, in the order it publishes
/// them. `CreateService$launchType` ("The infrastructure that you run your service on")
/// names exactly these three — the `FARGATE`, `EC2` and `EXTERNAL` launch types — and its
/// note sends a caller that wants Amazon ECS Managed Instances to `capacityProviderStrategy`
/// with `launchType` omitted.
const PLACEMENT_LAUNCH_TYPES: [&str; 3] = ["EC2", "FARGATE", "EXTERNAL"];

/// The launch types an operation that FILTERS a listing by launch type publishes, in the
/// order it publishes them: the three a placement names, plus the two kinds of capacity
/// Amazon ECS manages itself, which a listing can be asked about even though a placement
/// request does not name them. RECORDED behaviour of the filter operations.
///
/// `FMI` is published by these operations and is NOT a member of the modelled `LaunchType`
/// (`smithy_query(query="shape",shape="LaunchType")`: EC2, FARGATE, EXTERNAL,
/// MANAGED_INSTANCES), so the generated input schema this service is built from cannot carry
/// it: a listing asked to filter on it is refused rather than served, with the sentence the
/// operation publishes. Serving it would need the model to declare it.
const FILTER_LAUNCH_TYPES: [&str; 5] = ["EC2", "FARGATE", "EXTERNAL", "FMI", "MANAGED_INSTANCES"];

/// `InvalidParameterException$message` for a `deploymentConfiguration.strategy` that names
/// no deployment strategy: the whole sentence is fixed text about the member, listing
/// neither the strategies it does take nor the value the request carried.
const INVALID_DEPLOYMENT_STRATEGY_MESSAGE: &str = "Invalid deployment configuration strategy.";

/// The scheduling strategies a LISTING publishes, in the order it publishes them:
/// `SchedulingStrategy`'s own declaration order, replicas before daemons, which is the order
/// the member's doc introduces them in ("REPLICA … DAEMON").
const LISTED_SCHEDULING_STRATEGIES: [&str; 2] = ["REPLICA", "DAEMON"];

/// How a LISTING names the scheduling strategy it filters on when it refuses a value outside
/// the set: as a sentence opens, rather than in the lower-case member words a create states.
const LISTED_SCHEDULING_STRATEGY_MEMBER: &str = "Scheduling strategy";

/// The name the API prints for `ListServiceDeployments$status` when it reports the list's
/// entries back — the member that FILTERS the listing by status, not the `status` the wire
/// member is spelled with.
const STATUS_FILTER_MEMBER: &str = "statusFilter";

/// The service-deployment statuses a LISTING publishes for `ListServiceDeployments$status`,
/// in the order it publishes them. RECORDED behaviour of the operation, as with
/// [`FILTER_LAUNCH_TYPES`]: the message is the only place the API states which statuses it
/// filters on, so it prints the set the SERVICE accepts there, which is neither the order nor
/// the membership of the modelled enum
/// (`smithy_query(query="shape",shape="ServiceDeploymentStatus")`: PENDING, SUCCESSFUL,
/// STOPPED, STOP_REQUESTED, IN_PROGRESS, ROLLBACK_REQUESTED, ROLLBACK_IN_PROGRESS,
/// ROLLBACK_SUCCESSFUL, ROLLBACK_FAILED).
///
/// `CREATED` is published by this operation and is NOT a member of that modelled enum, so the
/// generated input schema this service is built from cannot carry it: a listing asked to
/// filter on it is refused rather than served, with this operation's own sentence listing it.
/// Serving it would need the model to declare it.
const FILTER_SERVICE_DEPLOYMENT_STATUSES: [&str; 10] = [
    "IN_PROGRESS",
    "CREATED",
    "ROLLBACK_IN_PROGRESS",
    "STOPPED",
    "STOP_REQUESTED",
    "ROLLBACK_REQUESTED",
    "ROLLBACK_SUCCESSFUL",
    "PENDING",
    "ROLLBACK_FAILED",
    "SUCCESSFUL",
];

/// `ListServiceDeployments$service` is "The ARN or name of the service", and a request that
/// carries none names no service to report the deployments of: the whole sentence is fixed
/// text about the identifier — the one this service spells once
/// ([`crate::state::SERVICE_ARN_INVALID_MESSAGE`]), so this refusal and the reads that answer
/// a SERVICE ARN handed to a member taking no service cannot drift apart.
const SERVICE_ARN_INVALID_MESSAGE: &str = crate::state::SERVICE_ARN_INVALID_MESSAGE;

/// The member that identifies WHICH service's deployments a listing reports.
const SERVICE_MEMBER: &str = "service";

/// `DescribeServicesRequest$services` is "A list of services to describe": a request that
/// carries no list, and one whose list holds nothing, both name no service to describe, and
/// Amazon ECS states that as one sentence for both. RECORDED behaviour, with no trailing
/// period.
const SERVICES_EMPTY_MESSAGE: &str = "Services cannot be empty";

/// The member a service describe names the services to report on.
const SERVICES_MEMBER: &str = "services";

/// `DescribeServicesRequest$services` is "A list of services to describe. You may specify up
/// to 10 services to describe in a single operation": one service is one entry of that list,
/// so a list naming the same one twice asks for one service in two entries and is a MALFORMED
/// request rather than a list of two services. Amazon ECS states that as one fixed sentence,
/// naming neither the repeated entry nor how many times it was repeated. RECORDED behaviour.
const DUPLICATE_SERVICE_NAMES_MESSAGE: &str = "Malformed request with duplicate service names.";

/// `DescribeTasksRequest$tasks` is "A list of up to 100 task IDs or full ARN entries": a
/// request whose list holds nothing names no task to describe. RECORDED behaviour, trailing
/// period included — this API punctuates the two sentences differently, and each is quoted
/// as the response that carries it spells it.
const TASKS_EMPTY_MESSAGE: &str = "Tasks cannot be empty.";

/// The member an attribute LISTING names the kind of resource it reports the attributes of.
const TARGET_TYPE_MEMBER: &str = "targetType";

/// `InvalidParameterException$message` for a `stopType` outside the set the member takes
/// (`smithy_query(query="shape",shape="StopServiceDeploymentStopType")`: ABORT, ROLLBACK).
///
/// The whole sentence is fixed text naming the member in the API's OWN spelling of it, and
/// echoes neither the value the request carried nor the stop types the member takes —
/// RECORDED behaviour, which is why it belongs to this operation rather than to the
/// framework's clause about the member. A stop type that IS in the set and that this API
/// does not carry out is another refusal entirely, and the operation states it itself
/// ([`crate::state::unsupported_stop_type_message`]).
const INVALID_STOP_TYPE_MESSAGE: &str = "The StopType is invalid.";

/// The refusal this service answers `req` with before any handler runs, or `None` when the
/// request earns no refusal of its own here — which leaves it to the framework's walk and to
/// the operation exactly as before.
///
/// Three layers, in this order: the refusal a GATEWAY operation publishes for a member it
/// requires and the request names nothing in ([`gateways`]), the condition the OPERATION words
/// its own sentence for ([`operation_condition_refusal`]), and then the one MEMBER rule every
/// operation that takes it shares — the `cluster` the request names, which has to be one of
/// the two forms that member documents ([`cluster_reference_refusal`]). The three are disjoint:
/// no operation of the gateway surface is keyed in either of the two below it.
pub fn operation_worded_refusal(
    operation: &str,
    protocol: &dyn ServerHttpProtocol,
    req: &RtRequest,
) -> Option<WordedRefusal> {
    gateways::gateway_member_refusal(operation, protocol, req)
        .or_else(|| {
            operation_condition_refusal(operation, protocol, req)
                .map(WordedRefusal::invalid_parameter)
        })
        .or_else(|| {
            cluster_reference_refusal(operation, protocol, req)
                .map(WordedRefusal::invalid_parameter)
        })
}

/// The `cluster` reference `req` carries, judged against the two forms that member documents
/// ("The short name or full Amazon Resource Name (ARN) of the cluster") — the sentence a
/// value that is NEITHER earns ([`crate::state::cluster_reference_violation`]), or `None`
/// when the request names a cluster in one of those forms, names none at all, or names the
/// operation nothing of this kind.
///
/// Stated HERE rather than in each operation because it is ONE rule about ONE member, worded
/// the same way whichever operation carried it: the `cluster` a request names is the scope it
/// acts in, and a value that is neither a cluster ARN nor a cluster name names no cluster in
/// any account — so it is answered as the bad parameter it is, before the cluster is resolved
/// and so before a request that could never name a cluster is reported as one that names a
/// cluster this account does not hold. The model declares no `@pattern` on the member, so the
/// framework's own walk has nothing to key it on.
///
/// The operations listed are the ones whose `cluster` member names a cluster of this
/// service's own API. The daemon, gateway and express-gateway surfaces take a `clusterArn`
/// (and `DescribeClusters` a `clusters` LIST) whose refusals those operations word
/// themselves, so neither is keyed here.
fn cluster_reference_refusal(
    operation: &str,
    protocol: &dyn ServerHttpProtocol,
    req: &RtRequest,
) -> Option<String> {
    /// The `cluster` of an input whose member is optional ("If you do not specify a cluster,
    /// the default cluster is assumed").
    macro_rules! optional_cluster {
        ($input:ty, $schema:expr) => {
            read_input::<$input>(protocol, $schema, req).and_then(|input| input.cluster)
        };
    }
    /// The `cluster` of an input that requires the member.
    macro_rules! required_cluster {
        ($input:ty, $schema:expr) => {
            read_input::<$input>(protocol, $schema, req).map(|input| input.cluster)
        };
    }
    let reference: Option<String> = match operation {
        "CreateCapacityProvider" => {
            optional_cluster!(CreateCapacityProviderInput, CREATE_CAPACITY_PROVIDER_INPUT_SCHEMA)
        }
        "CreateService" => optional_cluster!(CreateServiceInput, CREATE_SERVICE_INPUT_SCHEMA),
        "CreateTaskSet" => required_cluster!(CreateTaskSetInput, CREATE_TASK_SET_INPUT_SCHEMA),
        "DeleteAttributes" => {
            optional_cluster!(DeleteAttributesInput, DELETE_ATTRIBUTES_INPUT_SCHEMA)
        }
        "DeleteCapacityProvider" => {
            optional_cluster!(DeleteCapacityProviderInput, DELETE_CAPACITY_PROVIDER_INPUT_SCHEMA)
        }
        "DeleteCluster" => required_cluster!(DeleteClusterInput, DELETE_CLUSTER_INPUT_SCHEMA),
        "DeleteService" => optional_cluster!(DeleteServiceInput, DELETE_SERVICE_INPUT_SCHEMA),
        "DeleteTaskSet" => required_cluster!(DeleteTaskSetInput, DELETE_TASK_SET_INPUT_SCHEMA),
        "DeregisterContainerInstance" => optional_cluster!(
            DeregisterContainerInstanceInput,
            DEREGISTER_CONTAINER_INSTANCE_INPUT_SCHEMA
        ),
        "DescribeCapacityProviders" => optional_cluster!(
            DescribeCapacityProvidersInput,
            DESCRIBE_CAPACITY_PROVIDERS_INPUT_SCHEMA
        ),
        "DescribeContainerInstances" => optional_cluster!(
            DescribeContainerInstancesInput,
            DESCRIBE_CONTAINER_INSTANCES_INPUT_SCHEMA
        ),
        "DescribeServices" => {
            optional_cluster!(DescribeServicesInput, DESCRIBE_SERVICES_INPUT_SCHEMA)
        }
        "DescribeTaskSets" => {
            required_cluster!(DescribeTaskSetsInput, DESCRIBE_TASK_SETS_INPUT_SCHEMA)
        }
        "DescribeTasks" => optional_cluster!(DescribeTasksInput, DESCRIBE_TASKS_INPUT_SCHEMA),
        "DiscoverPollEndpoint" => {
            optional_cluster!(DiscoverPollEndpointInput, DISCOVER_POLL_ENDPOINT_INPUT_SCHEMA)
        }
        "ExecuteCommand" => optional_cluster!(ExecuteCommandInput, EXECUTE_COMMAND_INPUT_SCHEMA),
        "GetTaskProtection" => {
            required_cluster!(GetTaskProtectionInput, GET_TASK_PROTECTION_INPUT_SCHEMA)
        }
        "ListAttributes" => optional_cluster!(ListAttributesInput, LIST_ATTRIBUTES_INPUT_SCHEMA),
        "ListContainerInstances" => {
            optional_cluster!(ListContainerInstancesInput, LIST_CONTAINER_INSTANCES_INPUT_SCHEMA)
        }
        "ListServiceDeployments" => {
            optional_cluster!(ListServiceDeploymentsInput, LIST_SERVICE_DEPLOYMENTS_INPUT_SCHEMA)
        }
        "ListServices" => optional_cluster!(ListServicesInput, LIST_SERVICES_INPUT_SCHEMA),
        "ListTasks" => optional_cluster!(ListTasksInput, LIST_TASKS_INPUT_SCHEMA),
        "PutAttributes" => optional_cluster!(PutAttributesInput, PUT_ATTRIBUTES_INPUT_SCHEMA),
        "PutClusterCapacityProviders" => required_cluster!(
            PutClusterCapacityProvidersInput,
            PUT_CLUSTER_CAPACITY_PROVIDERS_INPUT_SCHEMA
        ),
        "RegisterContainerInstance" => optional_cluster!(
            RegisterContainerInstanceInput,
            REGISTER_CONTAINER_INSTANCE_INPUT_SCHEMA
        ),
        "RunTask" => optional_cluster!(RunTaskInput, RUN_TASK_INPUT_SCHEMA),
        "StartTask" => optional_cluster!(StartTaskInput, START_TASK_INPUT_SCHEMA),
        "StopTask" => optional_cluster!(StopTaskInput, STOP_TASK_INPUT_SCHEMA),
        "SubmitAttachmentStateChanges" => optional_cluster!(
            SubmitAttachmentStateChangesInput,
            SUBMIT_ATTACHMENT_STATE_CHANGES_INPUT_SCHEMA
        ),
        "SubmitContainerStateChange" => optional_cluster!(
            SubmitContainerStateChangeInput,
            SUBMIT_CONTAINER_STATE_CHANGE_INPUT_SCHEMA
        ),
        "SubmitTaskStateChange" => {
            optional_cluster!(SubmitTaskStateChangeInput, SUBMIT_TASK_STATE_CHANGE_INPUT_SCHEMA)
        }
        "UpdateCapacityProvider" => {
            optional_cluster!(UpdateCapacityProviderInput, UPDATE_CAPACITY_PROVIDER_INPUT_SCHEMA)
        }
        "UpdateCluster" => required_cluster!(UpdateClusterInput, UPDATE_CLUSTER_INPUT_SCHEMA),
        "UpdateClusterSettings" => {
            required_cluster!(UpdateClusterSettingsInput, UPDATE_CLUSTER_SETTINGS_INPUT_SCHEMA)
        }
        "UpdateContainerAgent" => {
            optional_cluster!(UpdateContainerAgentInput, UPDATE_CONTAINER_AGENT_INPUT_SCHEMA)
        }
        "UpdateContainerInstancesState" => optional_cluster!(
            UpdateContainerInstancesStateInput,
            UPDATE_CONTAINER_INSTANCES_STATE_INPUT_SCHEMA
        ),
        "UpdateService" => optional_cluster!(UpdateServiceInput, UPDATE_SERVICE_INPUT_SCHEMA),
        "UpdateServicePrimaryTaskSet" => required_cluster!(
            UpdateServicePrimaryTaskSetInput,
            UPDATE_SERVICE_PRIMARY_TASK_SET_INPUT_SCHEMA
        ),
        "UpdateTaskProtection" => {
            required_cluster!(UpdateTaskProtectionInput, UPDATE_TASK_PROTECTION_INPUT_SCHEMA)
        }
        "UpdateTaskSet" => required_cluster!(UpdateTaskSetInput, UPDATE_TASK_SET_INPUT_SCHEMA),
        _ => None,
    };
    crate::state::cluster_reference_violation(reference.as_deref())
}

/// The sentence the condition an OPERATION words its own way earns, or `None` when the
/// request earns none.
///
/// `operation` is the routing key the request resolved to, so a condition one operation
/// words its own way cannot reach another. The operations named below are read TWICE — once
/// here and once by the dispatch sequence that follows — which is the cost of wording a
/// refusal the framework's own walk cannot word: a second pass over a body the framework has
/// already bounded, for the handful of operations listed.
fn operation_condition_refusal(
    operation: &str,
    protocol: &dyn ServerHttpProtocol,
    req: &RtRequest,
) -> Option<String> {
    match operation {
        "CreateService" => {
            let input = read_input::<CreateServiceInput>(protocol, CREATE_SERVICE_INPUT_SCHEMA, req)?;
            launch_type_refusal(&PLACEMENT_LAUNCH_TYPES, input.launch_type.as_deref()).or_else(
                || deployment_strategy_refusal(input.deployment_configuration.as_ref()),
            )
        }
        "UpdateService" => {
            let input = read_input::<UpdateServiceInput>(protocol, UPDATE_SERVICE_INPUT_SCHEMA, req)?;
            deployment_strategy_refusal(input.deployment_configuration.as_ref())
        }
        "RunTask" => {
            let input = read_input::<RunTaskInput>(protocol, RUN_TASK_INPUT_SCHEMA, req)?;
            launch_type_refusal(&PLACEMENT_LAUNCH_TYPES, input.launch_type.as_deref())
        }
        "CreateTaskSet" => {
            let input = read_input::<CreateTaskSetInput>(protocol, CREATE_TASK_SET_INPUT_SCHEMA, req)?;
            launch_type_refusal(&PLACEMENT_LAUNCH_TYPES, input.launch_type.as_deref())
        }
        "ListTasks" => {
            let input = read_input::<ListTasksInput>(protocol, LIST_TASKS_INPUT_SCHEMA, req)?;
            launch_type_refusal(&FILTER_LAUNCH_TYPES, input.launch_type.as_deref())
        }
        "ListServices" => {
            let input = read_input::<ListServicesInput>(protocol, LIST_SERVICES_INPUT_SCHEMA, req)?;
            launch_type_refusal(&FILTER_LAUNCH_TYPES, input.launch_type.as_deref())
                .or_else(|| scheduling_strategy_refusal(input.scheduling_strategy.as_deref()))
        }
        "ListServiceDeployments" => {
            match read_input::<ListServiceDeploymentsInput>(
                protocol,
                LIST_SERVICE_DEPLOYMENTS_INPUT_SCHEMA,
                req,
            ) {
                Some(input) => status_filter_refusal(input.status.as_deref()),
                // The identifier the listing requires: absent, it names no service, which is
                // the one condition this operation words itself.
                None => absent_member::<ListServiceDeploymentsInput>(
                    protocol,
                    LIST_SERVICE_DEPLOYMENTS_INPUT_SCHEMA,
                    req,
                )
                .filter(|absent| absent == SERVICE_MEMBER)
                .map(|_| SERVICE_ARN_INVALID_MESSAGE.to_string()),
            }
        }
        "DescribeServices" => {
            match read_input::<DescribeServicesInput>(protocol, DESCRIBE_SERVICES_INPUT_SCHEMA, req)
            {
                Some(input) => input
                    .services
                    .is_empty()
                    .then(|| SERVICES_EMPTY_MESSAGE.to_string())
                    .or_else(|| duplicate_services_refusal(&input.services)),
                None => absent_member::<DescribeServicesInput>(
                    protocol,
                    DESCRIBE_SERVICES_INPUT_SCHEMA,
                    req,
                )
                .filter(|absent| absent == SERVICES_MEMBER)
                .map(|_| SERVICES_EMPTY_MESSAGE.to_string()),
            }
        }
        "ListAttributes" => {
            let input =
                read_input::<ListAttributesInput>(protocol, LIST_ATTRIBUTES_INPUT_SCHEMA, req)?;
            target_type_refusal(&input.target_type)
        }
        "DescribeTasks" => {
            let input = read_input::<DescribeTasksInput>(protocol, DESCRIBE_TASKS_INPUT_SCHEMA, req)?;
            input
                .tasks
                .is_empty()
                .then(|| TASKS_EMPTY_MESSAGE.to_string())
        }
        "StopServiceDeployment" => {
            let input = read_input::<StopServiceDeploymentInput>(
                protocol,
                STOP_SERVICE_DEPLOYMENT_INPUT_SCHEMA,
                req,
            )?;
            stop_type_refusal(input.stop_type.as_deref())
        }
        _ => None,
    }
}

/// The sentence a `launchType` outside the modelled set earns, listing the launch types
/// `published` — or `None` when the request names a launch type the member takes, or names
/// none at all.
///
/// The set a value is CHECKED against is the member's modelled one ([`LAUNCH_TYPE_VALUES`]),
/// so no request's outcome changes here; the set the sentence LISTS is the one the operation
/// publishes. The two differ at their edges, in both directions, and each difference is what
/// the model leaves this service able to serve:
///
/// * `FMI`, which the filter operations publish, is not a modelled member, so a listing
///   asked for it is refused — with that operation's own sentence, which is the sentence
///   Amazon ECS answers it (see [`FILTER_LAUNCH_TYPES`]);
/// * `MANAGED_INSTANCES`, which the placement sentence does not list, IS a modelled member,
///   and this service stores and reports a service placed with it (`ListServices` filters on
///   it and `Cluster$statistics` counts it), so a placement that names it is still served.
///   No recorded response refuses one; what the recording pins is the SENTENCE, which is
///   the published list either way.
fn launch_type_refusal(published: &[&str], requested: Option<&str>) -> Option<String> {
    let requested = requested?;
    if LAUNCH_TYPE_VALUES.contains(&requested) {
        return None;
    }
    Some(crate::state::value_set_message(
        &crate::state::member_words(LAUNCH_TYPE_MEMBER),
        published,
    ))
}

/// The sentence a `deploymentConfiguration.strategy` that names no deployment strategy
/// earns, or `None` when the configuration names one the member takes (or names none, or is
/// absent).
fn deployment_strategy_refusal(configuration: Option<&DeploymentConfiguration>) -> Option<String> {
    let requested = configuration?.strategy.as_deref()?;
    (!DEPLOYMENT_STRATEGY_VALUES.contains(&requested))
        .then(|| INVALID_DEPLOYMENT_STRATEGY_MESSAGE.to_string())
}

/// The sentence an attribute LISTING's `targetType` outside its set earns: Amazon ECS states
/// this one by ECHOING the value it was given rather than by listing the kinds it takes —
/// `ListAttributes$targetType` is "The type of the target to list attributes with", and the
/// value is the one thing in the request the caller has to change.
///
/// The set a value is checked against is the member's modelled one
/// ([`TARGET_TYPE_VALUES`]), so no request's outcome changes here — only the words the
/// refusal carries. The sentence belongs to this operation: a `targetType` beside an
/// ATTRIBUTE being written (`Attribute$targetType`) is a member of another shape on another
/// operation, which publishes no such sentence, so it keeps the refusal it had.
fn target_type_refusal(requested: &str) -> Option<String> {
    (!TARGET_TYPE_VALUES.contains(&requested))
        .then(|| format!("Unexpected {TARGET_TYPE_MEMBER} '{requested}'."))
}

/// The sentence a `stopType` outside the modelled set earns, or `None` when the request
/// names a stop type the member takes (or names none at all, which is the stop type the
/// member documents as valid).
///
/// The set a value is CHECKED against is the member's modelled one
/// ([`STOP_SERVICE_DEPLOYMENT_STOP_TYPE_VALUES`]), so no request's outcome changes here —
/// only the words the refusal carries ([`INVALID_STOP_TYPE_MESSAGE`]).
fn stop_type_refusal(requested: Option<&str>) -> Option<String> {
    let requested = requested?;
    (!STOP_SERVICE_DEPLOYMENT_STOP_TYPE_VALUES.contains(&requested))
        .then(|| INVALID_STOP_TYPE_MESSAGE.to_string())
}

/// The sentence a LISTING's `schedulingStrategy` outside its set earns, naming the member
/// and the two strategies it publishes.
fn scheduling_strategy_refusal(requested: Option<&str>) -> Option<String> {
    let requested = requested?;
    (!SCHEDULING_STRATEGY_VALUES.contains(&requested)).then(|| {
        crate::state::value_set_message(
            LISTED_SCHEDULING_STRATEGY_MEMBER,
            &LISTED_SCHEDULING_STRATEGIES,
        )
    })
}

/// The sentence a `services` list that names one service more than once earns, or `None`
/// when each entry names a different one (and for a list of one, which can repeat nothing).
///
/// The list is judged AS THE REQUEST SPELLS IT, before any entry is resolved against the
/// store: this is the shape of the request rather than a statement about what the entries
/// name, which is what the sentence it earns calls it — so a request is refused for repeating
/// an entry whether or not the service it names is stored, and never for naming one service
/// the two ways a reference may spell it.
fn duplicate_services_refusal(requested: &[String]) -> Option<String> {
    let mut named = ::std::collections::HashSet::new();
    (!requested.iter().all(|service| named.insert(service.as_str())))
        .then(|| DUPLICATE_SERVICE_NAMES_MESSAGE.to_string())
}

/// The clause a `status` filter holding an entry outside the status set earns, reported
/// against the LIST the entries came in, or `None` when every entry names a status (and for
/// a request that carries no filter).
///
/// As with [`launch_type_refusal`], the set a value is CHECKED against is the member's
/// modelled one ([`SERVICE_DEPLOYMENT_STATUS_VALUES`]), so no request's outcome changes with
/// the set the sentence LISTS — the statuses this operation publishes
/// ([`FILTER_SERVICE_DEPLOYMENT_STATUSES`]).
fn status_filter_refusal(requested: Option<&[String]>) -> Option<String> {
    let requested = requested?;
    let outside = requested
        .iter()
        .any(|status| !SERVICE_DEPLOYMENT_STATUS_VALUES.contains(&status.as_str()));
    outside.then(|| {
        crate::state::list_member_constraint_message(
            STATUS_FILTER_MEMBER,
            requested,
            &FILTER_SERVICE_DEPLOYMENT_STATUSES,
        )
    })
}
