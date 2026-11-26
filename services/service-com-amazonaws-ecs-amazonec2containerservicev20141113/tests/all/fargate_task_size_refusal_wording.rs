// SPDX-License-Identifier: CC0-1.0

//! WHICH refusal a Fargate task size Amazon ECS has no configuration for earns, driven end to
//! end over the shared wire harness in [`crate::ecs_wire`].
//!
//! RULE under test: **the Fargate task-size catalog is keyed by the task `cpu`, so the two
//! values of a rejected size fail differently.** A `cpu` that names none of the published task
//! CPU sizes — the "Available cpu values" of the `RegisterTaskDefinition$memory` doc's list of
//! legal pairs — is a SETTING of the task that names nothing Fargate offers, and the refusal
//! names that member and carries no value of the request: `Invalid 'cpu' setting for task. For
//! more information, see the Troubleshooting section of the Amazon ECS Developer Guide.` A
//! `cpu` that names one of those sizes has to be paired with a `memory` that size supports,
//! and a pair the catalog does not hold is refused as the CONFIGURATION it asks for, naming
//! both values as the units they stand for: `No Fargate configuration exists for given values:
//! <cpu> CPU, <memory> memory. See the Amazon ECS documentation for the valid values.` Either
//! way the error is the modelled `ClientException` at HTTP 400 and nothing is registered.
//!
//! Why it matters: the message is what a client reads to learn WHICH of the two values it has
//! to change, and what a log or alert matcher keys on; a registration refused with the wrong
//! sentence points the caller at the wrong member, and a CloudFormation stack that relays the
//! refusal reports it on the resource's own failure.
//!
//! The rows differ along what the rule generalises over: whether the `cpu` names a published
//! size at all (below the smallest, between two of them, past the largest, a fraction of a
//! vCPU, and a value that spells no number of units), which published size it names when it
//! does, how the value is SPELLED (CPU units and the vCPU form the member documents), and
//! whether the `memory` beside it is one that size supports — with the accepted pairs in the
//! table too, so the boundary is pinned from both sides.

use serde_json::{json, Value};

use crate::ecs_wire::{list, ok, refusal, service};

/// The modelled error a refused registration carries (`ClientException`, `@httpError(400)`).
const CLIENT_EXCEPTION: &str = "ClientException";

/// The task CPU sizes Fargate publishes a configuration for, in CPU units.
///
/// Source: the `RegisterTaskDefinition$memory` doc's own list of legal pairs, whose "Available
/// cpu values" are these seven. Written here so the rows below can be read against the catalog
/// rather than against one example of it.
const PUBLISHED_TASK_CPU_UNITS: [i64; 7] = [256, 512, 1024, 2048, 4096, 8192, 16384];

/// A minimal, valid container definition, so every refusal below is reached by the task size
/// under test rather than by an unrunnable container.
fn container() -> Value {
    json!({ "name": "app", "image": "public.ecr.aws/nginx/nginx:latest", "memory": 128 })
}

/// The refusal a task-level SETTING that names nothing Fargate offers carries.
fn invalid_setting(member: &str) -> String {
    format!(
        "Invalid '{member}' setting for task. For more information, see the Troubleshooting \
         section of the Amazon ECS Developer Guide."
    )
}

/// The refusal a pair outside the catalog carries, for the values as the units they stand for.
fn no_configuration(cpu: &str, memory: &str) -> String {
    format!(
        "No Fargate configuration exists for given values: {cpu} CPU, {memory} memory. See the \
         Amazon ECS documentation for the valid values."
    )
}

/// What a row expects of its registration.
enum Expected {
    /// Fargate has a configuration for the pair: the revision registers.
    Registered,
    /// The `cpu` names no published task CPU size, so the refusal names that member.
    InvalidCpuSetting,
    /// The `cpu` names one and the `memory` is not one it supports, so the refusal names the
    /// configuration, stating both values as the units they stand for.
    NoConfiguration {
        stated_cpu: &'static str,
        stated_memory: &'static str,
    },
}

/// One row: the case, the task size the registration declares, and what it earns.
struct Row {
    case: &'static str,
    cpu: &'static str,
    memory: &'static str,
    expected: Expected,
}

const ROWS: &[Row] = &[
    Row {
        case: "a cpu below the smallest published size",
        cpu: "100",
        memory: "512",
        expected: Expected::InvalidCpuSetting,
    },
    Row {
        case: "a cpu between two published sizes",
        cpu: "768",
        memory: "2048",
        expected: Expected::InvalidCpuSetting,
    },
    Row {
        case: "a cpu past the largest published size",
        cpu: "32768",
        memory: "65536",
        expected: Expected::InvalidCpuSetting,
    },
    Row {
        case: "a fraction of a vCPU no published size names",
        cpu: "0.1 vCPU",
        memory: "512",
        expected: Expected::InvalidCpuSetting,
    },
    Row {
        case: "a cpu that spells no number of units at all",
        cpu: "all of it",
        memory: "512",
        expected: Expected::InvalidCpuSetting,
    },
    Row {
        case: "the smallest published size with a memory past the ones it supports",
        cpu: "256",
        memory: "4096",
        expected: Expected::NoConfiguration {
            stated_cpu: "256",
            stated_memory: "4096",
        },
    },
    Row {
        case: "a published size with a memory below the ones it supports",
        cpu: "512",
        memory: "512",
        expected: Expected::NoConfiguration {
            stated_cpu: "512",
            stated_memory: "512",
        },
    },
    Row {
        case: "a published size off the increment its memory range steps in",
        cpu: "8192",
        memory: "18432",
        expected: Expected::NoConfiguration {
            stated_cpu: "8192",
            stated_memory: "18432",
        },
    },
    Row {
        case: "a published size in the vCPU form, with a memory it does not support",
        cpu: "1 vCPU",
        memory: "1GB",
        expected: Expected::NoConfiguration {
            stated_cpu: "1024",
            stated_memory: "1024",
        },
    },
    Row {
        case: "the largest published size at its lowest memory",
        cpu: "16384",
        memory: "32768",
        expected: Expected::Registered,
    },
    Row {
        case: "the smallest published size at its lowest memory",
        cpu: "256",
        memory: "512",
        expected: Expected::Registered,
    },
    Row {
        case: "a published size in the vCPU and GB forms",
        cpu: "1 vCPU",
        memory: "2GB",
        expected: Expected::Registered,
    },
];

/// The registration one row makes, under a family of its own so a refused one leaves a family
/// that can be read back empty.
fn registration(family: &str, row: &Row) -> Value {
    json!({
        "family": family,
        "requiresCompatibilities": ["FARGATE"],
        "networkMode": "awsvpc",
        "cpu": row.cpu,
        "memory": row.memory,
        "containerDefinitions": [container()],
    })
}

#[tokio::test]
async fn a_rejected_fargate_task_size_names_the_cpu_or_the_configuration() {
    let router = service();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let family = format!("fargate-size-{index}");
        let request = registration(&family, row);

        let expected = match row.expected {
            Expected::Registered => {
                let registered = ok(&router, "RegisterTaskDefinition", request, case).await;
                let revision = &registered["taskDefinition"];
                assert_eq!(
                    revision["status"].as_str(),
                    Some("ACTIVE"),
                    "case {case:?}: expected the pair to register, got {revision}"
                );
                continue;
            }
            Expected::InvalidCpuSetting => invalid_setting("cpu"),
            Expected::NoConfiguration {
                stated_cpu,
                stated_memory,
            } => {
                // A row that names the configuration names a cpu the catalog IS keyed by, which
                // is what makes the other refusal the wrong one for it.
                let units: i64 = stated_cpu
                    .parse()
                    .unwrap_or_else(|_| panic!("case {case:?}: a stated cpu is in CPU units"));
                assert!(
                    PUBLISHED_TASK_CPU_UNITS.contains(&units),
                    "case {case:?}: expected a published cpu size, got {units}"
                );
                no_configuration(stated_cpu, stated_memory)
            }
        };

        let (code, message) = refusal(&router, "RegisterTaskDefinition", request, case).await;
        assert_eq!(
            code, CLIENT_EXCEPTION,
            "case {case:?}: expected a {CLIENT_EXCEPTION}, got {code:?} with {message:?}"
        );
        assert_eq!(
            message, expected,
            "case {case:?}: expected the refusal to read {expected:?}, got {message:?}"
        );

        // Nothing was registered for it: the family holds no revision.
        let listed = ok(
            &router,
            "ListTaskDefinitions",
            json!({ "familyPrefix": family }),
            case,
        )
        .await;
        assert!(
            list(&listed, "taskDefinitionArns", case).is_empty(),
            "case {case:?}: expected the refused registration to leave no revision, got {listed}"
        );
    }
}
