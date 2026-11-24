// SPDX-License-Identifier: CC0-1.0

//! The records a TASK is made of: one container per container definition the task's
//! revision stored, and the managed agents each container runs.
//!
//! Both are written from what the ECS AGENT reports about the task
//! (`SubmitContainerStateChange`, `SubmitTaskStateChange$managedAgents`), and both are
//! projected into the `Task` a task-returning operation answers with — which is why they
//! sit together, away from the records a SERVICE is made of.

use super::*;

/// One container of a task, derived from the container definition the task's
/// task-definition revision stored, and from what the ECS agent has reported about it
/// since (`SubmitContainerStateChange`).
#[derive(Debug, Clone)]
pub struct ContainerRecord {
    pub container_arn: String,
    /// Which container of this store's whole history this is, from the counter the id in
    /// its ARN was minted out of (`next_container_id`) — what makes the runtime id Amazon
    /// ECS gives this container when it creates it its own
    /// ([`ContainerRecord::report_runtime_id`]).
    pub sequence: u64,
    pub task_arn: String,
    pub name: Option<String>,
    pub image: Option<String>,
    pub last_status: String,
    pub cpu: Option<String>,
    pub memory: Option<String>,
    pub memory_reservation: Option<String>,
    /// `Container$runtimeId`: "The ID of the Docker container" — the agent's, reported
    /// through `SubmitContainerStateChange$runtimeId`, so it is absent until the agent
    /// reports one.
    pub runtime_id: Option<String>,
    /// `Container$exitCode`: "The exit code returned from the container", as the agent
    /// reported it with the state change.
    pub exit_code: Option<i32>,
    /// `Container$reason`: "A short (255 max characters) human-readable string to provide
    /// additional details about a running or stopped container", as the agent reported it.
    pub reason: Option<String>,
    /// `Container$networkBindings`: the host/container port pairs the agent reported for
    /// this container.
    pub network_bindings: Vec<NetworkBinding>,
    /// `Container$imageDigest`: "The container image manifest digest" — known only once
    /// the agent has pulled the image and reported it
    /// (`ContainerStateChange$imageDigest`), so absent until then.
    pub image_digest: Option<String>,
    /// The UPSTREAM image reference the registry that served the pull resolved this
    /// container's own to, for an image held in a repository that caches one — an Amazon ECR
    /// repository a pull through cache rule covers
    /// ([`::service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::state::registry_pull::cached_upstream_reference`]).
    /// `None` until the image has been pulled, and for every image whose repository holds one
    /// of its own rather than a cached copy: the reference the definition carries then names
    /// the image itself ([`Self::running_image`]).
    ///
    /// Not a member of `Container`: it is what the pull resolved, which the modelled container
    /// reports as its `imageDigest` and its `image`.
    pub cached_image_source: Option<String>,
    /// `Container$managedAgents`: "The details of any Amazon ECS managed agents associated
    /// with the container", each in the state the agent last reported for it
    /// (`SubmitTaskStateChange$managedAgents`). Empty until the agent reports one.
    pub managed_agents: Vec<ManagedAgentRecord>,
    /// Whether this container is ESSENTIAL to its task, as the container definition declared
    /// it. `ContainerDefinition$essential`: "If this parameter is omitted, a container is
    /// assumed to be essential" — the TASK's own `healthStatus` is read off these containers
    /// alone ([`TaskRecord::health_status`]).
    pub essential: bool,
    /// `ContainerDefinition$dependsOn`: "The dependencies defined for container startup and
    /// shutdown", as the definition declared them — the conditions this container waits on
    /// before it starts ([`super::container_health`]).
    pub depends_on: Vec<ContainerDependency>,
    /// The health check this container's definition declared and where its evaluation has
    /// got to, and `None` for a container with no health check — which is one that reports
    /// `UNKNOWN` for ever ([`ContainerHealthRecord`]).
    pub health: Option<ContainerHealthRecord>,
}

/// One Amazon ECS managed agent of a container, in the state the ECS agent last reported
/// for it through `SubmitTaskStateChange$managedAgents` ("The details for the managed
/// agent that's associated with the task").
#[derive(Debug, Clone)]
pub struct ManagedAgentRecord {
    /// `ManagedAgent$name`: "The name of the managed agent", as the report named it.
    pub name: String,
    /// `ManagedAgent$lastStatus`: "The last known status of the managed agent" — the
    /// agent's own word on it, stored as given.
    pub last_status: String,
    /// `ManagedAgent$reason`: "The reason for why the managed agent is in the state it is
    /// in", and absent when the report carried none.
    pub reason: Option<String>,
    /// `ManagedAgent$lastStartedAt`: "The Unix timestamp for the time when the managed
    /// agent was last started" — the instant a report first put it at
    /// [`STATUS_RUNNING`], and `None` for an agent no report has started.
    pub last_started_at: Option<DateTime>,
}

impl ManagedAgentRecord {
    /// The `ManagedAgent` a container reports for this record.
    pub fn view(&self) -> ManagedAgent {
        ManagedAgent {
            name: Some(self.name.clone()),
            last_status: Some(self.last_status.clone()),
            reason: self.reason.clone(),
            last_started_at: self.last_started_at,
        }
    }
}

impl ContainerRecord {
    /// The `Container` every task-returning operation reports, built from what is
    /// stored.
    ///
    /// `attachments` are the TASK's: `Container$networkInterfaces` is "The network
    /// interfaces associated with the container", and an `awsvpc` task's containers share
    /// the one interface the task was allocated — so each container reports the interface
    /// its task holds, with the address that interface actually got, and a container of a
    /// task that was allocated none reports an empty list.
    pub fn view(&self, attachments: &[AttachmentRecord]) -> Container {
        Container {
            container_arn: Some(self.container_arn.clone()),
            task_arn: Some(self.task_arn.clone()),
            name: self.name.clone(),
            image: self.image.clone(),
            last_status: Some(self.last_status.clone()),
            cpu: self.cpu.clone(),
            memory: self.memory.clone(),
            memory_reservation: self.memory_reservation.clone(),
            runtime_id: self.runtime_id.clone(),
            exit_code: self.exit_code,
            reason: self.reason.clone(),
            network_bindings: Some(self.network_bindings.clone()),
            network_interfaces: Some(
                attachments
                    .iter()
                    .filter_map(AttachmentRecord::container_network_interface)
                    .collect(),
            ),
            health_status: Some(self.health_status().to_string()),
            image_digest: self.image_digest.clone(),
            managed_agents: Some(
                self.managed_agents
                    .iter()
                    .map(ManagedAgentRecord::view)
                    .collect(),
            ),
            ..Container::default()
        }
    }

    /// The image this container really RUNS: the upstream reference the repository its own
    /// names holds a cached copy of ([`Self::cached_image_source`]), and the reference its
    /// definition carries otherwise.
    ///
    /// What anything read OFF the image has to be read off — the command the image declares,
    /// its entry point and the files it ships — because a cache repository holds the upstream
    /// image rather than an image of its own, so the two references name ONE image.
    pub fn running_image(&self) -> Option<&str> {
        self.cached_image_source
            .as_deref()
            .or(self.image.as_deref())
    }

    /// The `healthStatus` this container reports.
    ///
    /// `Container$healthStatus`: "If health checks aren't configured for this container in
    /// its task definition, then it reports the health status as `UNKNOWN`" — so a container
    /// with no health check, and one whose first probe has not produced a result yet, both
    /// report `UNKNOWN`; a container with one reports what its probes have made of it
    /// ([`ContainerHealthRecord::status`]).
    pub fn health_status(&self) -> &str {
        self.health
            .as_ref()
            .map_or(HEALTH_UNKNOWN, |health| health.status.as_str())
    }

    /// Move this container ON to `status`, if that is a LATER stage of
    /// [`CONTAINER_LIFECYCLE`] than the one it is already reported at.
    ///
    /// The agent's report and the task's own clock-owed transitions both move a
    /// container, and they can arrive in either order: a container the agent has already
    /// reported STOPPED is not put back to RUNNING by the task reaching RUNNING. A stage
    /// the lifecycle does not name is taken as reported, since it cannot be ranked.
    pub fn advance_to(&mut self, status: &str) {
        let onward = match (container_stage_rank(status), container_stage_rank(&self.last_status)) {
            (Some(next), Some(current)) => next > current,
            _ => true,
        };
        if onward {
            self.last_status = status.to_string();
        }
    }

    /// Report the runtime id Amazon ECS gives this container when it CREATES it on the
    /// capacity the task was placed on, if it does not carry one already.
    ///
    /// `Container$runtimeId` is "The ID of the Docker container": the container exists on
    /// its host from the moment Amazon ECS creates it there — the step the image fetch is
    /// made in, before any program of it runs — so the id is reported from then on, whether
    /// or not that fetch succeeded, and an id the agent itself has reported
    /// (`SubmitContainerStateChange$runtimeId`) is left as the agent gave it.
    ///
    /// `task_id` is the task the container belongs to: Amazon ECS derives the id from the
    /// task it created the container for and the container's own place in this store's
    /// allocations ([`Self::sequence`]), so one container's runtime id is its own and reads
    /// the same every time.
    ///
    /// Answers whether anything changed.
    pub fn report_runtime_id(&mut self, task_id: &str) -> bool {
        if self.runtime_id.is_some() {
            return false;
        }
        self.runtime_id = Some(format!("{task_id}-{}", self.sequence));
        true
    }
}

/// The `Container$lastStatus` a container of a task placed in `task_status` is reported at.
///
/// `Container$lastStatus` is "the last known status of the container", which moves through
/// [`CONTAINER_LIFECYCLE`] — and `PROVISIONING` is not on it: the ECS task lifecycle's
/// `PROVISIONING` is the TASK waiting for the resources Amazon ECS has to put in place
/// before it can be launched, which is not a state a container of it is in. A container of
/// a task that has not been launched yet is `PENDING`, the first stage of its own
/// lifecycle; any other task status a container shares is its own stage.
pub fn initial_container_status(task_status: &str) -> &str {
    match container_stage_rank(task_status) {
        Some(_) => task_status,
        None => STATUS_PENDING,
    }
}
