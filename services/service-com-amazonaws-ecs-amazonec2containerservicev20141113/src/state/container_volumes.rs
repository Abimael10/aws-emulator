// SPDX-License-Identifier: CC0-1.0

//! WHERE a mounted volume's files live: the task-definition `volumes` a launched container's
//! `mountPoints` name, resolved to the storage each of them is backed by.
//!
//! # The rule this module implements
//!
//! `TaskDefinition$volumes` is "the list of data volume definitions for the task", and each
//! entry "may only specify a `name` and one of either `configuredAtLaunch`,
//! `dockerVolumeConfiguration`, `efsVolumeConfiguration`, `s3filesVolumeConfiguration`,
//! `fsxWindowsFileServerVolumeConfiguration`, or `host`" — so WHICH files a container sees at
//! a mount point follows from the member the volume filled, never from the mount point:
//!
//! * `efsVolumeConfiguration` names an Amazon EFS FILE SYSTEM by its `fileSystemId`, so the
//!   files are that file system's and are held by the service that owns it
//!   ([`::service_com_amazonaws_efs_magnolioapiservice_v20150201::state::mount_file_system`]).
//!   Every task that declares the same file system therefore mounts ONE namespace: what a
//!   container of one task writes, a container of a later task reads. The subtree it is
//!   rooted at is the volume's `rootDirectory` ("the directory within the Amazon EFS file
//!   system to mount as the root directory inside the host"), or — where the
//!   `authorizationConfig` names an access point — the path that access point enforces,
//!   because "the root directory parameter must either be omitted or set to `/` which will
//!   enforce the path set on the EFS access point". An access point that applies a `PosixUser`
//!   is also who the files there belong to.
//! * a `host` volume with a `sourcePath` is a bind mount of the CONTAINER INSTANCE's own
//!   storage, which "persists at the specified location on the host container instance until
//!   you delete it manually", so its files are held for that instance and that path: a later
//!   task placed on the same instance mounts what an earlier one left.
//! * every other volume — an empty `host` volume, a Docker volume — belongs to the TASK: "if
//!   the `host` parameter is empty, then the Docker daemon assigns a host path for your data
//!   volume. However, the data isn't guaranteed to persist after the containers that are
//!   associated with it stop running". Its files are held for that one task, which is what
//!   makes a volume the way two containers of one task hand data over.
//!
//! A `mountPoints` entry whose `sourceVolume` the revision declares no volume for mounts
//! nothing, and neither does one naming a file system this account does not hold: there is no
//! storage to show, so the path is left as the container's own
//! ([`super::container_filesystem`]) rather than given an invented volume.
//!
//! # Where each half happens
//!
//! The REQUESTS are read off the revision under this store's lock, with the launch
//! ([`super::container_runtime`]): they are the task definition's own members. RESOLVING one
//! is a read of ANOTHER service's store (the file system and its access point), so it happens
//! off this store's lock, in the same pass that starts the container's run.
//!
//! LOCK ORDER: [`mounted_volumes`] takes the Amazon EFS store's lock and then this store's,
//! each holding no other while it does, and hands back owned handles — so it is called with NO
//! guard of this store held, the contract every cross-service read here has.

use ::service_com_amazonaws_efs_magnolioapiservice_v20150201::state as efs;

use super::*;

/// How many volumes this store holds storage for at once. THIS NUMBER IS THE EMULATOR'S, not
/// an AWS quota: each entry is memory of this process, and the entries are pruned as the tasks
/// and container instances they belong to go ([`ComAmazonawsEcsAmazonec2containerservicev20141113State::prune_volume_storage`]),
/// so this is the ceiling a caller placing tasks without end runs into.
pub const MAX_VOLUME_STORAGE: usize = 256;

/// Which storage of THIS store a volume's files are held in — what the volume's own
/// configuration says its data belongs to (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum VolumeKey {
    /// A volume whose data belongs to the TASK: an empty `host` volume, or a Docker volume.
    Task {
        /// The task the volume was declared for.
        task_arn: String,
        /// `Volume$name`, which is what a `mountPoints` entry names it by.
        name: String,
    },
    /// A bind mount of the CONTAINER INSTANCE's own storage, which outlives the task that
    /// wrote to it.
    Instance {
        /// The instance the path is on.
        container_instance_arn: String,
        /// `HostVolumeProperties$sourcePath`: "the path on the host container instance that's
        /// presented to the container".
        source_path: String,
    },
}

/// ONE mount a launched container needs, read off the revision under this store's lock: the
/// mount point the container declared and the volume it names.
#[derive(Debug, Clone)]
pub struct MountRequest {
    /// `MountPoint$containerPath`.
    container_path: String,
    /// `MountPoint$readOnly`.
    read_only: bool,
    /// Where the files are to come from.
    source: VolumeSource,
}

/// The storage one [`MountRequest`] is backed by.
#[derive(Debug, Clone)]
enum VolumeSource {
    /// An Amazon EFS file system, with the subtree and the access point the volume named.
    FileSystem {
        /// `EFSVolumeConfiguration$fileSystemId`.
        file_system_id: String,
        /// `EFSVolumeConfiguration$rootDirectory`, when the volume named one.
        root_directory: Option<String>,
        /// `EFSAuthorizationConfig$accessPointId`, when the volume named one.
        access_point_id: Option<String>,
    },
    /// Storage of this store's own, under the key the volume's configuration gives it.
    Held(VolumeKey),
}

/// The mounts a container of `task` running `definition` under the container definition
/// `container_name` needs, in the order the container declared them.
///
/// Read off the revision's `volumes` and that container definition's `mountPoints`: a mount
/// point naming no declared volume, or a volume with no configuration this emulator holds
/// storage for, is left out — such a path is the container's own rather than a mount of
/// nothing.
pub(super) fn mount_requests(
    definition: &TaskDefinitionRecord,
    container: &ContainerDefinition,
    task: &TaskRecord,
) -> Vec<MountRequest> {
    let mut requests = Vec::new();
    for mount_point in container.mount_points.iter().flatten() {
        let Some(container_path) = mount_point.container_path.as_deref() else {
            continue;
        };
        let Some(source_volume) = mount_point.source_volume.as_deref() else {
            continue;
        };
        let Some(volume) = definition
            .volumes
            .iter()
            .find(|volume| volume.name.as_deref() == Some(source_volume))
        else {
            // "Must be a volume name referenced in the `name` parameter of task definition
            // `volume`": a name no volume of the revision carries names no storage.
            continue;
        };
        let Some(source) = volume_source(volume, task) else {
            continue;
        };
        requests.push(MountRequest {
            container_path: container_path.to_string(),
            // "If this value is `false`, then the container can write to the volume. The
            // default value is `false`."
            read_only: mount_point.read_only.unwrap_or(false),
            source,
        });
    }
    requests
}

/// Which storage one declared volume is backed by (see the module docs), and `None` for a
/// volume this emulator holds no storage for — one configured at launch, and the file systems
/// of the services this one does not reach.
fn volume_source(volume: &Volume, task: &TaskRecord) -> Option<VolumeSource> {
    let name = volume.name.clone()?;
    if let Some(configuration) = volume.efs_volume_configuration.as_ref() {
        return Some(VolumeSource::FileSystem {
            file_system_id: configuration.file_system_id.clone(),
            root_directory: configuration.root_directory.clone(),
            access_point_id: configuration
                .authorization_config
                .as_ref()
                .and_then(|authorization| authorization.access_point_id.clone()),
        });
    }
    if volume.s3files_volume_configuration.is_some()
        || volume.fsx_windows_file_server_volume_configuration.is_some()
        || volume.configured_at_launch == Some(true)
    {
        // The storage of another service, or one a launch configures: not held here, so the
        // mount point is not given storage this emulator invented for it.
        return None;
    }
    let source_path = volume
        .host
        .as_ref()
        .and_then(|host| host.source_path.as_deref())
        .map(str::trim)
        .filter(|path| !path.is_empty());
    match (source_path, task.container_instance_arn.as_deref()) {
        (Some(source_path), Some(container_instance_arn)) => {
            Some(VolumeSource::Held(VolumeKey::Instance {
                container_instance_arn: container_instance_arn.to_string(),
                source_path: source_path.to_string(),
            }))
        }
        _ => Some(VolumeSource::Held(VolumeKey::Task {
            task_arn: task.task_arn.clone(),
            name,
        })),
    }
}

impl ComAmazonawsEcsAmazonec2containerservicev20141113State {
    /// The storage this store holds under `key`, minted on the first mount of it and kept from
    /// then on — which is what makes two containers mounting the same volume share one
    /// namespace.
    ///
    /// `None` once this store already holds [`MAX_VOLUME_STORAGE`] volumes and `key` is not
    /// one of them: the mount is then not attached at all rather than given storage that is
    /// dropped.
    pub(super) fn volume_storage(&mut self, key: VolumeKey) -> Option<VolumeFiles> {
        if !self.volume_storage.contains_key(&key) && self.volume_storage.len() >= MAX_VOLUME_STORAGE
        {
            ::tracing::warn!(
                limit = MAX_VOLUME_STORAGE,
                "this emulator is already holding as many task volumes as it holds at once, \
                 so this volume is not mounted"
            );
            return None;
        }
        Some(
            self.volume_storage
                .entry(key)
                .or_insert_with(VolumeFiles::new)
                .clone(),
        )
    }

    /// Forget the storage of every volume whose task or container instance this store no
    /// longer holds: a task's volume goes with the task, and an instance's bind mount with the
    /// instance.
    pub(super) fn prune_volume_storage(&mut self) {
        let tasks = &self.tasks;
        let instances = &self.container_instances;
        self.volume_storage.retain(|key, _| match key {
            VolumeKey::Task { task_arn, .. } => tasks.contains_key(task_arn),
            VolumeKey::Instance {
                container_instance_arn,
                ..
            } => instances.contains_key(container_instance_arn),
        });
    }
}

/// The mounts `requests` resolve to, each with the storage its volume is backed by.
///
/// A request whose file system this account does not hold, or whose access point it does not
/// hold, resolves to NO mount: there is nothing to show at that path, and nothing is invented
/// for it. See the module docs for the lock order this is called under.
pub(super) fn mounted_volumes<Eng: Send + Sync + 'static>(
    engine: &Eng,
    state: &::engine::ServiceState<ComAmazonawsEcsAmazonec2containerservicev20141113State>,
    requests: &[MountRequest],
) -> Vec<ContainerMount> {
    let mut mounts = Vec::new();
    for request in requests {
        let attached = match &request.source {
            VolumeSource::FileSystem {
                file_system_id,
                root_directory,
                access_point_id,
            } => mounted_file_system(
                engine,
                request,
                file_system_id,
                root_directory.as_deref(),
                access_point_id.as_deref(),
            ),
            VolumeSource::Held(key) => {
                // Bound to its own statement, so this store's lock is released before the
                // mount is attached — attaching takes the VOLUME's own lock, and the contract
                // this module states is that neither is held while the other is taken.
                let files = state.lock().volume_storage(key.clone());
                files.and_then(|files| {
                    ContainerMount::attach(
                        &request.container_path,
                        efs::ROOT_DIRECTORY,
                        request.read_only,
                        FileOwner::default(),
                        MountedFiles::Volume(files),
                    )
                })
            }
        };
        match attached {
            Some(mount) => mounts.push(mount),
            None => ::tracing::warn!(
                container_path = %request.container_path,
                "the volume a mount point names has no storage this emulator holds, so \
                 nothing is mounted at that path"
            ),
        }
    }
    mounts
}

/// [`mounted_volumes`] for one `efsVolumeConfiguration` request: the file system's own files,
/// rooted where the configuration or its access point says, owned by the access point's
/// `PosixUser` where it enforces one.
fn mounted_file_system<Eng: Send + Sync + 'static>(
    engine: &Eng,
    request: &MountRequest,
    file_system_id: &str,
    root_directory: Option<&str>,
    access_point_id: Option<&str>,
) -> Option<ContainerMount> {
    let files = efs::mount_file_system(engine, file_system_id)?;
    let (root, owner) = match access_point_id {
        // An access point "enforce[s] the path set on the EFS access point" whatever the
        // volume's own `rootDirectory` says, and applies its own identity to every request
        // made through it.
        Some(access_point_id) => (
            efs::access_point_root(engine, file_system_id, access_point_id)?,
            efs::access_point_owner(engine, access_point_id)
                .map(|(uid, gid)| FileOwner::of_ids(uid, gid))
                .unwrap_or_default(),
        ),
        None => (
            root_directory
                .map(str::trim)
                .filter(|directory| !directory.is_empty())
                .unwrap_or(efs::ROOT_DIRECTORY)
                .to_string(),
            FileOwner::default(),
        ),
    };
    ContainerMount::attach(
        &request.container_path,
        &root,
        request.read_only,
        owner,
        MountedFiles::FileSystem(files),
    )
}
