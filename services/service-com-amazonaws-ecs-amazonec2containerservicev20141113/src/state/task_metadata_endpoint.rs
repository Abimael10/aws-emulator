// SPDX-License-Identifier: CC0-1.0

//! The TASK METADATA ENDPOINT a container reads its own and its task's metadata at: the
//! address Amazon ECS puts in every container's environment, and the documents the agent
//! answers there.
//!
//! # The rule this module implements
//!
//! Amazon ECS gives the containers of a task it launched an endpoint of their own to read what
//! they are: the ECS developer guide's task metadata endpoint version 4 states that "the
//! `ECS_CONTAINER_METADATA_URI_V4` environment variable is injected into each container",
//! that its value is an address under the link-local address [`TASK_METADATA_HOST_OCTETS`], and
//! that
//! a `GET` of that address answers the CONTAINER's metadata while a `GET` of `task` under it
//! answers the metadata of "the task the container belongs to, as well as metadata for the
//! containers in that task".
//!
//! So two facts follow, and this module is both of them:
//!
//! * the variable is part of the environment a session inside a container runs with — the
//!   container's own program ([`super::container_runtime`]), the probe the agent runs in it
//!   ([`super::container_health`]) and the session an `ExecuteCommand` opens
//!   ([`super::exec_sessions`]) each see it, because each of them runs INSIDE the container;
//! * the documents it answers are computed from the task and container records this store
//!   holds, under the member names that endpoint publishes them with, so what a program reads
//!   about itself is what a client reads back through `DescribeTasks`.
//!
//! The endpoint is ONE CONTAINER's: the path carries the id of the container it belongs to
//! ([`container_metadata_uri`]), so a path naming another container's id is not a document
//! this endpoint holds.
//!
//! # No socket, no host
//!
//! Nothing here listens on a port or dials one. The endpoint is a server the container's own
//! HTTP client is given ([`super::container_http_client`]), answering out of this store under
//! its lock — the same seam a container's request to its own task's servers is answered
//! through ([`::service_common::served_http_addresses::ServesHttp`]), so one request of a
//! container's session is read one way whatever answers it.
//!
//! # What is NOT answered
//!
//! The members of those documents that only a container RUNTIME observes — the statistics
//! documents under `stats`, and the per-container instants and image id a daemon on the host
//! reports — are not answered: this emulator holds no such runtime, so a path this endpoint
//! has no document for is answered `404` exactly as the agent answers one it does not serve,
//! and a member the store holds nothing for is left out of the document rather than filled
//! with a value nothing backs.

use ::service_common::served_http_addresses::{AddressAnswer, ServesHttp};

use super::*;

/// The environment variable Amazon ECS injects the address of this endpoint into, as the task
/// metadata endpoint version 4 names it.
pub const TASK_METADATA_URI_VARIABLE: &str = "ECS_CONTAINER_METADATA_URI_V4";

/// The link-local address the endpoint answers at, which is the same address inside every
/// task: the agent on the host serves it in each task's own network namespace, so no security
/// group and no interface of the account stands between a container and it. It is the address
/// the ECS developer guide's task metadata endpoint version 4 names.
///
/// Held as its four OCTETS, the way this emulator holds every link-local address it serves to
/// its own guests rather than dials (the Route 53 Resolver crate holds a VPC's own resolver
/// address the same way): nothing here opens a connection to it or to anything else, and the
/// one use it has is COMPARING the host of the address a container's own client was handed
/// ([`is_task_metadata_host`]), which selects this endpoint as the answerer of that request
/// ([`super::container_http_client`]).
pub const TASK_METADATA_HOST_OCTETS: [u8; 4] = [169, 254, 170, 2];

/// That same address as the HOST of the URL a container's environment carries.
#[must_use]
pub fn task_metadata_host() -> String {
    ::std::net::Ipv4Addr::from(TASK_METADATA_HOST_OCTETS).to_string()
}

/// Whether `host` is the address this endpoint answers at, as a URL carries one: the address
/// itself, however it is spelled.
#[must_use]
pub fn is_task_metadata_host(host: &str) -> bool {
    host.parse::<::std::net::Ipv4Addr>()
        .is_ok_and(|address| address.octets() == TASK_METADATA_HOST_OCTETS)
}

/// The version segment of the paths it answers, which is the version of the endpoint.
const TASK_METADATA_VERSION: &str = "v4";

/// The segment under a container's own path that carries its TASK's document.
const TASK_DOCUMENT_SEGMENT: &str = "task";

/// The media type the documents are served as.
const METADATA_MEDIA_TYPE: &str = "application/json";

/// `Container$type` as the endpoint reports it for a container of the task definition itself,
/// as against one Amazon ECS adds to the task for a feature of its own.
const CONTAINER_TYPE: &str = "NORMAL";

/// The two methods a document server answers: the document itself, and everything about it but
/// its body.
const GET_METHOD: &str = "GET";
const HEAD_METHOD: &str = "HEAD";

/// The status the endpoint answers for a path it holds no document for.
const NOT_FOUND_STATUS: u16 = 404;

/// The status it answers a document with.
const OK_STATUS: u16 = 200;

/// The labels the agent applies to a container it creates, which the endpoint reports as that
/// container's `Labels` — the cluster it runs in, the container's own name, its task and the
/// task definition that task runs.
const CLUSTER_LABEL: &str = "com.amazonaws.ecs.cluster";
const CONTAINER_NAME_LABEL: &str = "com.amazonaws.ecs.container-name";
const TASK_ARN_LABEL: &str = "com.amazonaws.ecs.task-arn";
const TASK_DEFINITION_FAMILY_LABEL: &str = "com.amazonaws.ecs.task-definition-family";
const TASK_DEFINITION_VERSION_LABEL: &str = "com.amazonaws.ecs.task-definition-version";

/// The ADDRESS the endpoint of `container` is reached at, which is what that container's
/// environment carries under [`TASK_METADATA_URI_VARIABLE`].
///
/// The path carries the id of the container the endpoint belongs to: `Container$runtimeId` is
/// "the ID of the Docker container", which is how the agent names the container it created,
/// and a container this store holds no runtime id for yet is named by its own id — the last
/// segment of its ARN, which is the same id for the whole life of the container.
#[must_use]
pub fn container_metadata_uri(container: &ContainerRecord) -> String {
    format!(
        "http://{}/{TASK_METADATA_VERSION}/{}",
        task_metadata_host(),
        container_metadata_id(container)
    )
}

/// The id of one container's endpoint, as [`container_metadata_uri`] spells it into the path.
fn container_metadata_id(container: &ContainerRecord) -> String {
    container.runtime_id.clone().unwrap_or_else(|| {
        container
            .container_arn
            .rsplit('/')
            .next()
            .unwrap_or(container.container_arn.as_str())
            .to_string()
    })
}

/// WHICH document a request to the endpoint of one container asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MetadataDocument {
    /// The container's own metadata, at the address its environment carries.
    Container,
    /// The metadata of the task it belongs to, and of the containers in that task.
    Task,
}

/// The task metadata endpoint of ONE container: the server its session's HTTP client answers a
/// request to the address it answers at ([`is_task_metadata_host`]) from.
///
/// Holds the store rather than a copy of the documents: the metadata of a container is what
/// the task's records say at the instant the container asks, so a container that reads it
/// twice across a status change reads the change.
#[derive(Clone)]
pub struct TaskMetadataEndpoint {
    /// The store the documents are computed from.
    state: ::engine::ServiceState<ComAmazonawsEcsAmazonec2containerservicev20141113State>,
    /// The container whose endpoint this is.
    container_arn: String,
}

/// Written by hand rather than derived: the store carries no `Debug` worth a log line, and
/// what is diagnosable here is which container's endpoint this is.
impl ::std::fmt::Debug for TaskMetadataEndpoint {
    fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        formatter
            .debug_struct("TaskMetadataEndpoint")
            .field("container_arn", &self.container_arn)
            .finish()
    }
}

impl TaskMetadataEndpoint {
    /// The endpoint of the container `container_arn` names, answering out of `state`.
    #[must_use]
    pub fn new(
        state: ::engine::ServiceState<ComAmazonawsEcsAmazonec2containerservicev20141113State>,
        container_arn: String,
    ) -> Self {
        Self {
            state,
            container_arn,
        }
    }
}

/// WHICH document `path` asks for of the endpoint of the container whose id is `id` — `None`
/// for every other path, which is one this endpoint holds no document for.
fn document_asked_for(path: &str, id: &str) -> Option<MetadataDocument> {
    let under = path.strip_prefix(&format!("/{TASK_METADATA_VERSION}/{id}"))?;
    match under.trim_end_matches('/') {
        "" => Some(MetadataDocument::Container),
        rest => (rest == format!("/{TASK_DOCUMENT_SEGMENT}")).then_some(MetadataDocument::Task),
    }
}

impl ServesHttp for TaskMetadataEndpoint {
    /// What the endpoint answers for one request of the container's session.
    ///
    /// A READ: the documents are computed from the task and container records and nothing is
    /// written — a container asking what it is moves nothing of the task it belongs to.
    fn serve(&self, method: &str, path: &str) -> Option<AddressAnswer> {
        // A document server answers a `GET` of the document and a `HEAD` carrying everything
        // about it but its body; a request made with any other method is one this endpoint
        // performs nothing of.
        if method != GET_METHOD && method != HEAD_METHOD {
            return None;
        }
        let guard = self.state.lock();
        let (task, container) = guard.container_of(&self.container_arn)?;
        let document = document_asked_for(path, &container_metadata_id(container))
            .map(|asked| match asked {
                MetadataDocument::Container => guard.container_metadata_document(task, container),
                MetadataDocument::Task => guard.task_metadata_document(task),
            });
        let Some(document) = document else {
            // A path this endpoint holds no document for, which the agent answers as a
            // document that is not there.
            return Some(AddressAnswer {
                status: NOT_FOUND_STATUS,
                content_type: None,
                body: Vec::new(),
            });
        };
        Some(AddressAnswer {
            status: OK_STATUS,
            content_type: Some(METADATA_MEDIA_TYPE.to_string()),
            // A `HEAD` carries everything about the document but its body, as a document
            // server answers one.
            body: if method == GET_METHOD {
                document.to_string().into_bytes()
            } else {
                Vec::new()
            },
        })
    }
}

impl ComAmazonawsEcsAmazonec2containerservicev20141113State {
    /// The task and the container the ARN `container_arn` names, and `None` for a container
    /// this store does not hold — one whose task is gone.
    fn container_of(&self, container_arn: &str) -> Option<(&TaskRecord, &ContainerRecord)> {
        self.tasks.values().find_map(|task| {
            task.containers
                .iter()
                .find(|container| container.container_arn == container_arn)
                .map(|container| (task, container))
        })
    }

    /// The document the endpoint answers for ONE container: what the container is, under the
    /// member names the task metadata endpoint publishes them with.
    fn container_metadata_document(
        &self,
        task: &TaskRecord,
        container: &ContainerRecord,
    ) -> ::serde_json::Value {
        let definition = self.revision_of_arn(&task.task_definition_arn);
        let stored = definition.zip(container.name.as_deref()).and_then(
            |(definition, name)| {
                definition
                    .container_definitions
                    .iter()
                    .find(|candidate| candidate.name.as_deref() == Some(name))
            },
        );
        let mut document = ::serde_json::Map::new();
        document.insert(
            "DockerId".to_string(),
            container_metadata_id(container).into(),
        );
        insert_text(&mut document, "Name", container.name.clone());
        insert_text(&mut document, "Image", container.image.clone());
        // "The container image manifest digest", known only once the image has been pulled.
        insert_text(&mut document, "ImageID", container.image_digest.clone());
        document.insert("ContainerARN".to_string(), container.container_arn.clone().into());
        document.insert(
            "Labels".to_string(),
            ::serde_json::Value::Object(container_labels(task, container)),
        );
        // What the container is WANTED at and what it is reported at: the agent derives a
        // container's desired status from its task's (`Task$desiredStatus`), and the
        // container's own `lastStatus` is what it is known to be at.
        document.insert("DesiredStatus".to_string(), task.desired_status.clone().into());
        document.insert("KnownStatus".to_string(), container.last_status.clone().into());
        if let Some(limits) = container_limits(container) {
            document.insert("Limits".to_string(), ::serde_json::Value::Object(limits));
        }
        document.insert("Type".to_string(), CONTAINER_TYPE.into());
        // "The exit code returned from the container", reported once it has one.
        if let Some(exit_code) = container.exit_code {
            document.insert("ExitCode".to_string(), exit_code.into());
        }
        // A container with a health check reports what its probes have made of it; one with
        // none has no health to report ([`ContainerRecord::health_status`]).
        if container.health.is_some() {
            document.insert(
                "Health".to_string(),
                ::serde_json::json!({ "status": container.health_status() }),
            );
        }
        // The log driver the container's definition configures and the options it configures
        // it with, which the endpoint reports for the container it delivers through.
        if let Some(logging) = stored.and_then(|stored| stored.log_configuration.as_ref()) {
            document.insert("LogDriver".to_string(), logging.log_driver.clone().into());
            if let Some(options) = logging.options.as_ref().filter(|options| !options.is_empty()) {
                document.insert(
                    "LogOptions".to_string(),
                    ::serde_json::Value::Object(
                        options
                            .iter()
                            .map(|(name, value)| (name.clone(), value.clone().into()))
                            .collect(),
                    ),
                );
            }
        }
        document.insert(
            "Networks".to_string(),
            ::serde_json::Value::Array(vec![task_network(self, task)]),
        );
        ::serde_json::Value::Object(document)
    }

    /// The document the endpoint answers for the TASK a container belongs to: what the task is,
    /// and the containers in it.
    fn task_metadata_document(&self, task: &TaskRecord) -> ::serde_json::Value {
        let definition = self.revision_of_arn(&task.task_definition_arn);
        let mut document = ::serde_json::Map::new();
        document.insert("Cluster".to_string(), task.cluster_arn.clone().into());
        document.insert("TaskARN".to_string(), task.task_arn.clone().into());
        document.insert("Family".to_string(), task.family.clone().into());
        if let Some(revision) = definition.map(|definition| definition.revision) {
            // The endpoint carries the revision as TEXT, which is the form the document
            // publishes it in.
            document.insert("Revision".to_string(), revision.to_string().into());
        }
        document.insert("DesiredStatus".to_string(), task.desired_status.clone().into());
        document.insert("KnownStatus".to_string(), task.last_status.clone().into());
        if let Some(limits) = task_limits(task) {
            document.insert("Limits".to_string(), ::serde_json::Value::Object(limits));
        }
        // The instants only the AGENT observes, reported once a report has carried them.
        insert_instant(&mut document, "PullStartedAt", task.pull_started_at);
        insert_instant(&mut document, "PullStoppedAt", task.pull_stopped_at);
        insert_text(
            &mut document,
            "AvailabilityZone",
            task.availability_zone.clone(),
        );
        insert_text(&mut document, "LaunchType", task.launch_type.clone());
        // The service whose scheduler placed the task, for a task one did.
        insert_text(&mut document, "ServiceName", task.service_name.clone());
        document.insert(
            "Containers".to_string(),
            ::serde_json::Value::Array(
                task.containers
                    .iter()
                    .map(|container| self.container_metadata_document(task, container))
                    .collect(),
            ),
        );
        ::serde_json::Value::Object(document)
    }
}

/// The LABELS the agent applies to a container it created, as the endpoint reports them.
fn container_labels(
    task: &TaskRecord,
    container: &ContainerRecord,
) -> ::serde_json::Map<String, ::serde_json::Value> {
    let mut labels = ::serde_json::Map::new();
    labels.insert(CLUSTER_LABEL.to_string(), task.cluster_arn.clone().into());
    insert_text(&mut labels, CONTAINER_NAME_LABEL, container.name.clone());
    labels.insert(TASK_ARN_LABEL.to_string(), task.task_arn.clone().into());
    labels.insert(
        TASK_DEFINITION_FAMILY_LABEL.to_string(),
        task.family.clone().into(),
    );
    if let Some(revision) = task.task_definition_arn.rsplit(':').next() {
        labels.insert(
            TASK_DEFINITION_VERSION_LABEL.to_string(),
            revision.to_string().into(),
        );
    }
    labels
}

/// The NETWORK the containers of `task` are on: the mode its task definition runs them in, and
/// the addresses the interfaces the task holds were given.
fn task_network(
    state: &ComAmazonawsEcsAmazonec2containerservicev20141113State,
    task: &TaskRecord,
) -> ::serde_json::Value {
    let mut network = ::serde_json::Map::new();
    if let Some(mode) = state
        .revision_of_arn(&task.task_definition_arn)
        .map(|definition| definition.network_mode.clone())
    {
        network.insert("NetworkMode".to_string(), mode.into());
    }
    network.insert(
        "IPv4Addresses".to_string(),
        ::serde_json::Value::Array(
            task.attachments
                .iter()
                .filter_map(AttachmentRecord::private_ipv4_address)
                .map(|address| address.to_string().into())
                .collect(),
        ),
    );
    ::serde_json::Value::Object(network)
}

/// What one TASK was given, as the endpoint reports it: the vCPUs its `cpu` stands for and the
/// mebibytes of its `memory` — and `None` for a task whose definition states neither, which
/// has no limit of its own to report.
fn task_limits(task: &TaskRecord) -> Option<::serde_json::Map<String, ::serde_json::Value>> {
    let mut limits = ::serde_json::Map::new();
    // "The number of CPU units used by the task", which the endpoint reports as the vCPUs
    // those units come to ([`super::task_size::CPU_UNITS_PER_VCPU`]).
    if let Some(units) = task.cpu.as_deref().and_then(cpu_units) {
        limits.insert(
            "CPU".to_string(),
            ((units as f64) / CPU_UNITS_PER_VCPU).into(),
        );
    }
    if let Some(mib) = task.memory.as_deref().and_then(memory_mib) {
        limits.insert("Memory".to_string(), mib.into());
    }
    (!limits.is_empty()).then_some(limits)
}

/// What one CONTAINER was given: the CPU UNITS its definition reserves for it — "the number of
/// `cpu` units the Amazon ECS container agent reserves for the container", which is the unit
/// the container's own limit is stated in — and the mebibytes of its memory limit.
///
/// A container whose definition reserves NO cpu units carries no CPU limit at all: the member
/// is stored as zero for such a container, which is a reservation of nothing rather than a
/// limit of nothing, so none is reported for it. The memory LIMIT is the hard one —
/// `ContainerDefinition$memory` is "the amount (in MiB) of memory to present to the container",
/// which the container is killed for exceeding, as against `$memoryReservation`, "the soft
/// limit ... to reserve" — so a container that states only the soft reservation carries no
/// memory limit here either.
fn container_limits(
    container: &ContainerRecord,
) -> Option<::serde_json::Map<String, ::serde_json::Value>> {
    let mut limits = ::serde_json::Map::new();
    if let Some(units) = container
        .cpu
        .as_deref()
        .and_then(cpu_units)
        .filter(|units| *units > 0)
    {
        limits.insert("CPU".to_string(), units.into());
    }
    if let Some(mib) = container.memory.as_deref().and_then(memory_mib) {
        limits.insert("Memory".to_string(), mib.into());
    }
    (!limits.is_empty()).then_some(limits)
}

/// Add `name` carrying `value` when the record holds one: a member the store holds nothing for
/// is LEFT OUT rather than reported empty, exactly as the API's own projection leaves it out.
fn insert_text(
    document: &mut ::serde_json::Map<String, ::serde_json::Value>,
    name: &str,
    value: Option<String>,
) {
    if let Some(value) = value {
        document.insert(name.to_string(), value.into());
    }
}

/// Add `name` carrying the date-time text of `at`, when the record holds that instant — the
/// one reading of an instant as text this service takes
/// ([`super::task_events::date_time_text`]).
fn insert_instant(
    document: &mut ::serde_json::Map<String, ::serde_json::Value>,
    name: &str,
    at: Option<DateTime>,
) {
    insert_text(document, name, at.and_then(super::task_events::date_time_text));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One container record for the rows below.
    fn container(container_arn: &str, runtime_id: Option<&str>) -> ContainerRecord {
        ContainerRecord {
            container_arn: container_arn.to_string(),
            sequence: 1,
            task_arn: "arn:aws:ecs:us-west-2:000000000000:task/c/t".to_string(),
            name: Some("app".to_string()),
            image: None,
            last_status: STATUS_RUNNING.to_string(),
            cpu: None,
            memory: None,
            memory_reservation: None,
            runtime_id: runtime_id.map(ToString::to_string),
            exit_code: None,
            reason: None,
            network_bindings: Vec::new(),
            image_digest: None,
            cached_image_source: None,
            managed_agents: Vec::new(),
            essential: true,
            depends_on: Vec::new(),
            health: None,
        }
    }

    /// RULE: the address a container's environment carries is the link-local endpoint under the
    /// version segment, with the container's own id as its last segment — the runtime id the
    /// agent gave it, and the id in its ARN until it has one.
    #[test]
    fn the_endpoint_address_names_the_container_it_belongs_to() {
        // (case, the container's ARN, its runtime id, the id the address carries)
        let rows: &[(&str, &str, Option<&str>, &str)] = &[
            (
                "a container the agent has reported a runtime id for",
                "arn:aws:ecs:us-west-2:000000000000:container/c/t/abc",
                Some("t-1"),
                "t-1",
            ),
            (
                "a container with no runtime id yet",
                "arn:aws:ecs:us-west-2:000000000000:container/c/t/abc",
                None,
                "abc",
            ),
            ("an ARN of no segments", "container", None, "container"),
        ];
        for (case, container_arn, runtime_id, id) in rows {
            let uri = container_metadata_uri(&container(container_arn, *runtime_id));
            let expected = format!(
                "http://{}/{TASK_METADATA_VERSION}/{id}",
                task_metadata_host()
            );
            assert_eq!(
                uri, expected,
                "case {case:?}: expected {expected:?}, got {uri:?}"
            );
        }
    }

    /// RULE: the host of that address is the endpoint's, however a URL spells the address, and
    /// no other host is.
    #[test]
    fn the_endpoints_host_is_its_own_address() {
        let host = task_metadata_host();
        // (case, the host as a URL carries it, whether it is this endpoint's)
        let rows: &[(&str, &str, bool)] = &[
            ("the endpoint's own address", host.as_str(), true),
            ("the loopback inside the task", "127.0.0.1", false),
            ("another workload's address", "10.4.1.9", false),
            ("a name rather than an address", "localhost", false),
            ("the empty host", "", false),
        ];
        for (case, candidate, expected) in rows {
            let answered = is_task_metadata_host(candidate);
            assert_eq!(
                answered, *expected,
                "case {case:?}: expected {expected}, got {answered}"
            );
        }
    }

    /// RULE: the endpoint of one container answers the container's own document at the address
    /// its environment carries and its TASK's document under `task`; every other path is one it
    /// holds no document for, including one naming another container's id.
    #[test]
    fn the_paths_of_one_containers_endpoint_are_its_own() {
        let id = "t-1";
        // (case, the path asked for, the document it asks for)
        let rows: &[(&str, &str, Option<MetadataDocument>)] = &[
            ("the container's own document", "/v4/t-1", Some(MetadataDocument::Container)),
            (
                "the same path with a trailing slash",
                "/v4/t-1/",
                Some(MetadataDocument::Container),
            ),
            ("the task's document", "/v4/t-1/task", Some(MetadataDocument::Task)),
            (
                "the task's document with a trailing slash",
                "/v4/t-1/task/",
                Some(MetadataDocument::Task),
            ),
            ("the statistics of the container", "/v4/t-1/stats", None),
            ("the statistics of the task", "/v4/t-1/task/stats", None),
            ("another container's document", "/v4/t-2", None),
            ("another version of the endpoint", "/v2/t-1", None),
            ("the root of the endpoint", "/", None),
        ];
        for (case, path, expected) in rows {
            let asked = document_asked_for(path, id);
            assert_eq!(
                asked, *expected,
                "case {case:?}: expected {expected:?}, got {asked:?}"
            );
        }
    }
}
