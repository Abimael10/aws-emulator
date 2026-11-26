// SPDX-License-Identifier: CC0-1.0

//! The container-definition rules a REGISTRATION is held to before a revision exists, driven
//! end to end over the shared wire harness in [`crate::ecs_wire`].
//!
//! The rules under test, each over the whole class rather than one request:
//!
//! > 1. A container's start may wait for another container of the same revision to pass its
//! >    health check — `ContainerDependency$condition` `HEALTHY` "validates that the dependent
//! >    container passes its Docker health check before permitting other containers to start.
//! >    This requires that the dependent container has health checks configured" — so a
//! >    registration whose `HEALTHY` dependency names a container that declares no
//! >    `healthCheck` is refused with `ClientException` and the one sentence Amazon ECS states
//! >    for the class, and no revision of the family is created. Every other condition
//! >    (`START`, `COMPLETE`, `SUCCESS`) needs nothing of the container it names.
//! > 2. `linuxParameters` carries two members whose values Amazon ECS publishes a closed
//! >    catalog for that the model cannot hold, and a value outside its catalog is refused
//! >    with `ClientException` and no revision is created:
//! >    `capabilities.add`/`.drop` take only the capability names their docs publish — the
//! >    `capabilities(7)` names WITHOUT the kernel's `CAP_` prefix, plus `ALL` — reported with
//! >    the member at fault and the names it did not recognise, and `tmpfs.mountOptions` takes
//! >    only the `mount(8)` options its own doc publishes, reported with the one option at
//! >    fault. A value either catalog holds is accepted whichever member carries it.
//! > 3. A Fargate task runs on capacity Amazon ECS owns, so a registration that REQUIRES
//! >    Fargate may only declare what that capacity can present to a container: the
//! >    `linuxParameters` members that configure the HOST — `devices`, `sharedMemorySize`,
//! >    `maxSwap` and `swappiness` — are refused with `ClientException` naming the member, and
//! >    adding any kernel capability other than the one `KernelCapabilities$add` documents for
//! >    Fargate ("Tasks launched on Fargate only support adding the `SYS_PTRACE` kernel
//! >    capability") is refused naming the capability, while what Fargate does support is
//! >    registered and the revision reports that compatibility.
//!
//! The tables span the dependency CONDITION and which container is probed, the two capability
//! members, the prefix and the case of the name, how many names one member gets wrong, and the
//! case and the publication of a mount option — rather than the single request each defect was
//! found on. Both registrations that carry container definitions are driven, since the daemon
//! surface declares its own container and `linuxParameters` shapes. Nothing here reaches into
//! the state struct: a refusal is weighed against the listing of the family it would have
//! created, and an accepted registration is read back through `DescribeTaskDefinition`.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{list, ok, refusal, service, string};

/// The refusal class both rules are reported under: `RegisterTaskDefinition` models
/// `ClientException` for a request whose values Amazon ECS cannot build a revision from.
const CLIENT_EXCEPTION: &str = "ClientException";

/// The sentence a `HEALTHY` dependency on an unprobed container earns (`state`'s
/// `HEALTHY_DEPENDENCY_NEEDS_HEALTH_CHECK_MESSAGE`).
const HEALTHY_NEEDS_HEALTH_CHECK: &str =
    "A dependency container with HEALTHY condition must have health check configured";

/// A health check a container definition may declare — the shape's required `command` and
/// nothing else, so what the row is about is whether the check is there at all.
fn health_check() -> Value {
    json!({ "command": ["CMD-SHELL", "exit 0"] })
}

/// One container definition: a name, an image and a memory reservation, plus whatever
/// `extra` members the row adds.
fn container(name: &str, extra: Value) -> Value {
    let mut definition = json!({
        "name": name,
        "image": "public.ecr.aws/docker/library/busybox:1.36",
        "memory": 128,
    });
    crate::ecs_wire::with_members(&mut definition, extra);
    definition
}

/// Register `containers` under `family`, answering the status and body.
async fn register(
    router: &MultiServiceRouter,
    family: &str,
    containers: Value,
) -> (String, String) {
    refusal(
        router,
        "RegisterTaskDefinition",
        json!({ "family": family, "containerDefinitions": containers }),
        family,
    )
    .await
}

/// Whether the account holds any revision of `family` — what proves a refused registration
/// created none.
async fn holds_revision(router: &MultiServiceRouter, family: &str, case: &str) -> bool {
    let listed = ok(
        router,
        "ListTaskDefinitions",
        json!({ "familyPrefix": family }),
        case,
    )
    .await;
    !list(&listed, "taskDefinitionArns", case).is_empty()
}

#[tokio::test]
async fn a_healthy_dependency_needs_the_container_it_names_to_be_probed() {
    let router = service();

    // (case, the condition the dependency carries, whether the named container is probed,
    //  whether the registration is refused)
    let rows: &[(&str, &str, bool, bool)] = &[
        ("HEALTHY on an unprobed container", "HEALTHY", false, true),
        ("HEALTHY on a probed container", "HEALTHY", true, false),
        ("START on an unprobed container", "START", false, false),
        ("COMPLETE on an unprobed container", "COMPLETE", false, false),
        ("SUCCESS on an unprobed container", "SUCCESS", false, false),
        ("START on a probed container", "START", true, false),
    ];
    for (index, (case, condition, probed, refused)) in rows.iter().enumerate() {
        let family = format!("dependency{index}");
        let sidecar = container(
            "sidecar",
            if *probed {
                json!({ "healthCheck": health_check() })
            } else {
                json!({})
            },
        );
        let app = container(
            "app",
            json!({
                "dependsOn": [{ "containerName": "sidecar", "condition": condition }],
            }),
        );
        let request = json!({
            "family": &family,
            "containerDefinitions": [sidecar, app],
        });
        if *refused {
            let (code, message) =
                refusal(&router, "RegisterTaskDefinition", request, case).await;
            assert_eq!(
                code, CLIENT_EXCEPTION,
                "case {case:?}: expected a {CLIENT_EXCEPTION}, got {code:?} with {message:?}"
            );
            assert_eq!(
                message, HEALTHY_NEEDS_HEALTH_CHECK,
                "case {case:?}: expected the fixed sentence {HEALTHY_NEEDS_HEALTH_CHECK:?}, \
                 got {message:?}"
            );
            assert!(
                !holds_revision(&router, &family, case).await,
                "case {case:?}: a refused registration creates no revision of {family}"
            );
            continue;
        }
        let body = ok(&router, "RegisterTaskDefinition", request, case).await;
        let revision = string(&body["taskDefinition"], "taskDefinitionArn", case);
        // Read the revision back through the API: the dependency is stored as the request
        // spelled it.
        let described = ok(
            &router,
            "DescribeTaskDefinition",
            json!({ "taskDefinition": &revision }),
            case,
        )
        .await;
        let containers = list(&described["taskDefinition"], "containerDefinitions", case);
        let stored = containers
            .iter()
            .find(|definition| definition["name"] == json!("app"))
            .unwrap_or_else(|| panic!("case {case:?}: expected the app container, got {described}"));
        assert_eq!(
            stored["dependsOn"],
            json!([{ "containerName": "sidecar", "condition": condition }]),
            "case {case:?}: expected the dependency back as it was registered, got {described}"
        );
    }
}

#[tokio::test]
async fn a_dependency_on_a_container_the_revision_does_not_describe_is_not_this_refusal() {
    let router = service();
    let family = "dependencyelsewhere";
    // The rule is about a container of the list that is NOT probed; a dependency naming no
    // container of the revision is another condition, and this sentence is not it.
    let app = container(
        "app",
        json!({ "dependsOn": [{ "containerName": "absent", "condition": "HEALTHY" }] }),
    );
    let (status, body) = crate::ecs_wire::answered(
        &router,
        "RegisterTaskDefinition",
        json!({ "family": family, "containerDefinitions": [app] }),
    )
    .await;
    assert!(
        status != 400 || body["message"] != json!(HEALTHY_NEEDS_HEALTH_CHECK),
        "case {family:?}: the health-check sentence belongs to a container of the list, got \
         {body}"
    );
}

#[tokio::test]
async fn a_linux_capability_outside_the_published_catalog_is_refused() {
    let router = service();

    // (case, the capabilities member, the names it carries, the sentence the refusal
    //  carries). The catalog is `KernelCapabilities$add`/`$drop`'s own ("Valid values: ALL |
    //  AUDIT_CONTROL | ... | WAKE_ALARM"), so a row naming an entry of it must be accepted —
    //  including `NET_BIND_SERVICE` and `MKNOD`, which the defect this suite pins did not
    //  cite.
    let rows: &[(&str, &str, &[&str], Option<&str>)] = &[
        (
            "a CAP_-prefixed name in add",
            "add",
            &["CAP_SYS_ADMIN"],
            Some("Unrecognized Linux capabilities in add: [CAP_SYS_ADMIN]"),
        ),
        (
            "a CAP_-prefixed name in drop",
            "drop",
            &["CAP_MKNOD"],
            Some("Unrecognized Linux capabilities in drop: [CAP_MKNOD]"),
        ),
        (
            "a lower-case catalog name",
            "add",
            &["sys_ptrace"],
            Some("Unrecognized Linux capabilities in add: [sys_ptrace]"),
        ),
        (
            "two unrecognised names in one member",
            "add",
            &["CAP_CHOWN", "NOT_A_CAPABILITY"],
            Some("Unrecognized Linux capabilities in add: [CAP_CHOWN, NOT_A_CAPABILITY]"),
        ),
        (
            "a catalog name beside an unrecognised one",
            "drop",
            &["MKNOD", "CAP_KILL"],
            Some("Unrecognized Linux capabilities in drop: [CAP_KILL]"),
        ),
        ("a catalog name in add", "add", &["SYS_PTRACE"], None),
        (
            "two catalog names in add",
            "add",
            &["NET_BIND_SERVICE", "CHOWN"],
            None,
        ),
        ("the whole default set in drop", "drop", &["ALL"], None),
        ("a catalog name in drop", "drop", &["MKNOD"], None),
    ];
    for (index, (case, member, names, refused)) in rows.iter().enumerate() {
        let family = format!("capabilities{index}");
        let definition = container(
            "app",
            json!({
                "linuxParameters": { "capabilities": { (*member): names } },
            }),
        );
        let request = json!({
            "family": &family,
            "containerDefinitions": [definition],
        });
        match refused {
            Some(expected) => {
                let (code, message) =
                    refusal(&router, "RegisterTaskDefinition", request, case).await;
                assert_eq!(
                    code, CLIENT_EXCEPTION,
                    "case {case:?}: expected a {CLIENT_EXCEPTION}, got {code:?} with \
                     {message:?}"
                );
                assert_eq!(
                    message, *expected,
                    "case {case:?}: expected {expected:?}, got {message:?}"
                );
                assert!(
                    !holds_revision(&router, &family, case).await,
                    "case {case:?}: a refused registration creates no revision of {family}"
                );
            }
            None => {
                let body = ok(&router, "RegisterTaskDefinition", request, case).await;
                let revision = string(&body["taskDefinition"], "taskDefinitionArn", case);
                let described = ok(
                    &router,
                    "DescribeTaskDefinition",
                    json!({ "taskDefinition": &revision }),
                    case,
                )
                .await;
                let containers =
                    list(&described["taskDefinition"], "containerDefinitions", case);
                let stored = containers
                    .first()
                    .unwrap_or_else(|| panic!("case {case:?}: expected a container, got {described}"));
                assert_eq!(
                    stored["linuxParameters"]["capabilities"][member],
                    json!(names),
                    "case {case:?}: expected the capabilities back as registered, got \
                     {described}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_tmpfs_mount_option_outside_the_published_catalog_is_refused() {
    let router = service();

    // (case, the options one tmpfs mount declares, the sentence the refusal carries). The
    // catalog is `Tmpfs$mountOptions`' own ("Valid values: defaults | ro | rw | ... | mpol"),
    // so a row naming an entry of it must be accepted — including `nr_inodes`, which the
    // defect this suite pins did not cite.
    let rows: &[(&str, &[&str], Option<&str>)] = &[
        (
            "an upper-case option",
            &["NOEXEC"],
            Some("The NOEXEC tmpfs mount option specified is invalid."),
        ),
        (
            "an option nothing publishes",
            &["nowhere"],
            Some("The nowhere tmpfs mount option specified is invalid."),
        ),
        (
            "a catalog option beside one outside it",
            &["rw", "NOSUID"],
            Some("The NOSUID tmpfs mount option specified is invalid."),
        ),
        ("catalog options", &["noexec", "nosuid", "rw"], None),
        ("another catalog option", &["nr_inodes"], None),
    ];
    for (index, (case, options, refused)) in rows.iter().enumerate() {
        let family = format!("tmpfs{index}");
        let definition = container(
            "app",
            json!({
                "linuxParameters": {
                    "tmpfs": [{
                        "containerPath": "/run",
                        "size": 64,
                        "mountOptions": options,
                    }],
                },
            }),
        );
        let request = json!({
            "family": &family,
            "containerDefinitions": [definition],
        });
        match refused {
            Some(expected) => {
                let (code, message) =
                    refusal(&router, "RegisterTaskDefinition", request, case).await;
                assert_eq!(
                    code, CLIENT_EXCEPTION,
                    "case {case:?}: expected a {CLIENT_EXCEPTION}, got {code:?} with \
                     {message:?}"
                );
                assert_eq!(
                    message, *expected,
                    "case {case:?}: expected {expected:?}, got {message:?}"
                );
                assert!(
                    !holds_revision(&router, &family, case).await,
                    "case {case:?}: a refused registration creates no revision of {family}"
                );
            }
            None => {
                let body = ok(&router, "RegisterTaskDefinition", request, case).await;
                let revision = string(&body["taskDefinition"], "taskDefinitionArn", case);
                let described = ok(
                    &router,
                    "DescribeTaskDefinition",
                    json!({ "taskDefinition": &revision }),
                    case,
                )
                .await;
                let containers =
                    list(&described["taskDefinition"], "containerDefinitions", case);
                let stored = containers.first().unwrap_or_else(|| {
                    panic!("case {case:?}: expected a container, got {described}")
                });
                assert_eq!(
                    stored["linuxParameters"]["tmpfs"][0]["mountOptions"],
                    json!(options),
                    "case {case:?}: expected the mount options back as registered, got \
                     {described}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_fargate_registration_is_refused_for_what_fargate_capacity_lacks() {
    let router = service();

    // (case, the linuxParameters one container declares, the sentence the refusal carries).
    // Each member's own doc says what Fargate does not support, and Amazon ECS names the
    // member — or, for the capabilities, the capability.
    let rows: &[(&str, Value, Option<&str>)] = &[
        (
            "a device of the instance",
            json!({ "devices": [{ "hostPath": "/dev/nvidia0" }] }),
            Some("Fargate compatible task definitions do not support devices"),
        ),
        (
            "the size of the shared-memory volume",
            json!({ "sharedMemorySize": 64 }),
            Some("Fargate compatible task definitions do not support sharedMemorySize"),
        ),
        (
            "swap the instance would provide",
            json!({ "maxSwap": 0 }),
            Some("Fargate compatible task definitions do not support maxSwap"),
        ),
        (
            "how the instance swaps",
            json!({ "swappiness": 60 }),
            Some("Fargate compatible task definitions do not support swappiness"),
        ),
        // "Tasks launched on Fargate only support adding the SYS_PTRACE kernel capability":
        // every other capability of the catalog is refused by name.
        (
            "another capability added",
            json!({ "capabilities": { "add": ["NET_ADMIN"] } }),
            Some("NET_ADMIN is not allowed on Fargate."),
        ),
        (
            "the whole default set added",
            json!({ "capabilities": { "add": ["ALL"] } }),
            Some("ALL is not allowed on Fargate."),
        ),
        // The members and values Fargate DOES support are registered.
        (
            "a member that configures the container",
            json!({ "initProcessEnabled": true }),
            None,
        ),
        (
            "the one capability Fargate supports adding",
            json!({ "capabilities": { "add": ["SYS_PTRACE"] } }),
            None,
        ),
        (
            "capabilities dropped rather than added",
            json!({ "capabilities": { "drop": ["NET_ADMIN", "MKNOD"] } }),
            None,
        ),
    ];
    for (index, (case, parameters, refused)) in rows.iter().enumerate() {
        let family = format!("fargatelinux{index}");
        let definition = container("app", json!({ "linuxParameters": parameters }));
        let request = json!({
            "family": &family,
            "networkMode": "awsvpc",
            "cpu": "256",
            "memory": "512",
            "requiresCompatibilities": ["FARGATE"],
            "containerDefinitions": [definition],
        });
        match refused {
            Some(expected) => {
                let (code, message) =
                    refusal(&router, "RegisterTaskDefinition", request, case).await;
                assert_eq!(
                    code, CLIENT_EXCEPTION,
                    "case {case:?}: expected a {CLIENT_EXCEPTION}, got {code:?} with \
                     {message:?}"
                );
                assert_eq!(
                    message, *expected,
                    "case {case:?}: expected {expected:?}, got {message:?}"
                );
                assert!(
                    !holds_revision(&router, &family, case).await,
                    "case {case:?}: a refused registration creates no revision of {family}"
                );
            }
            None => {
                let body = ok(&router, "RegisterTaskDefinition", request, case).await;
                let compatibilities = list(&body["taskDefinition"], "compatibilities", case);
                assert!(
                    compatibilities.contains(&json!("FARGATE")),
                    "case {case:?}: a revision Fargate supports reports that \
                     compatibility, got {body}"
                );
            }
        }
    }
}

#[tokio::test]
async fn the_daemon_registration_is_held_to_the_same_container_rules() {
    let router = service();

    // The daemon surface declares its own container and `linuxParameters` shapes, so the two
    // rules are driven through its own operation as well: same conditions, same sentences.
    let probed = container("sidecar", json!({ "healthCheck": health_check() }));
    let unprobed = container("sidecar", json!({}));
    let waiting = container(
        "app",
        json!({ "dependsOn": [{ "containerName": "sidecar", "condition": "HEALTHY" }] }),
    );

    // (case, the containers the registration carries, the sentence it earns)
    let rows: &[(&str, Value, Option<&str>)] = &[
        (
            "a HEALTHY dependency on an unprobed container",
            json!([unprobed, waiting.clone()]),
            Some(HEALTHY_NEEDS_HEALTH_CHECK),
        ),
        (
            "a HEALTHY dependency on a probed container",
            json!([probed, waiting]),
            None,
        ),
        (
            "a CAP_-prefixed capability",
            json!([container(
                "app",
                json!({ "linuxParameters": { "capabilities": { "add": ["CAP_SYS_ADMIN"] } } }),
            )]),
            Some("Unrecognized Linux capabilities in add: [CAP_SYS_ADMIN]"),
        ),
        (
            "an upper-case tmpfs mount option",
            json!([container(
                "app",
                json!({
                    "linuxParameters": {
                        "tmpfs": [{
                            "containerPath": "/run",
                            "size": 64,
                            "mountOptions": ["NOEXEC"],
                        }],
                    },
                }),
            )]),
            Some("The NOEXEC tmpfs mount option specified is invalid."),
        ),
        (
            "capabilities and mount options the catalogs hold",
            json!([container(
                "app",
                json!({
                    "linuxParameters": {
                        "capabilities": { "add": ["SYS_PTRACE"], "drop": ["MKNOD"] },
                        "tmpfs": [{
                            "containerPath": "/run",
                            "size": 64,
                            "mountOptions": ["noexec", "nosuid"],
                        }],
                    },
                }),
            )]),
            None,
        ),
    ];
    for (index, (case, containers, refused)) in rows.iter().enumerate() {
        let family = format!("daemonstartup{index}");
        let request = json!({
            "family": &family,
            "containerDefinitions": containers,
        });
        match refused {
            Some(expected) => {
                let (code, message) =
                    refusal(&router, "RegisterDaemonTaskDefinition", request, case).await;
                assert_eq!(
                    code, CLIENT_EXCEPTION,
                    "case {case:?}: expected a {CLIENT_EXCEPTION}, got {code:?} with \
                     {message:?}"
                );
                assert_eq!(
                    message, *expected,
                    "case {case:?}: expected {expected:?}, got {message:?}"
                );
                let listed = ok(
                    &router,
                    "ListDaemonTaskDefinitions",
                    json!({ "familyPrefix": &family }),
                    case,
                )
                .await;
                assert!(
                    list(&listed, "daemonTaskDefinitions", case).is_empty(),
                    "case {case:?}: a refused registration creates no revision of {family}, \
                     got {listed}"
                );
            }
            None => {
                let body = ok(&router, "RegisterDaemonTaskDefinition", request, case).await;
                assert!(
                    string(&body, "daemonTaskDefinitionArn", case).ends_with(&format!(
                        "daemon-task-definition/{family}:1"
                    )),
                    "case {case:?}: expected the first revision of {family} back, got {body}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_capability_refusal_names_the_member_it_was_carried_in() {
    let router = service();
    // One request getting BOTH members wrong is reported against the member the shape
    // declares first, so the sentence a caller reads is deterministic.
    let definition = container(
        "app",
        json!({
            "linuxParameters": {
                "capabilities": { "add": ["CAP_CHOWN"], "drop": ["CAP_KILL"] },
            },
        }),
    );
    let (code, message) = register(&router, "capabilitiesboth", json!([definition])).await;
    assert_eq!(
        code, CLIENT_EXCEPTION,
        "case \"both members wrong\": expected a {CLIENT_EXCEPTION}, got {code:?} with \
         {message:?}"
    );
    assert_eq!(
        message, "Unrecognized Linux capabilities in add: [CAP_CHOWN]",
        "case \"both members wrong\": expected the add member to be reported, got {message:?}"
    );
}
