// SPDX-License-Identifier: CC0-1.0

//! How `UpdateContainerInstancesState` NAMES a container instance it could not move,
//! driven end to end over the shared wire harness in [`crate::ecs_wire`].
//!
//! The rule under test: the operation answers HTTP 200 and reports every entry it could
//! not resolve to an instance it can move through `failures` (`UpdateContainerInstancesState`
//! output members `containerInstances`/`failures`, each entry a `Failure` with
//! `arn`/`reason`/`detail`), naming the instance by the container-instance ARN of the
//! INSTANCE ID the entry named and nothing else:
//! `arn:aws:ecs:<region>:<account>:container-instance/<instance-id>`. The cluster segment
//! a stored instance's ARN carries names the cluster it is REGISTERED IN, which an
//! instance this call did not move is not, so it never appears in the resource part —
//! whichever of the member's two forms ("A list of up to 10 container instance IDs or full
//! ARN entries") the entry took.
//!
//! The REASON is the discriminator: `MISSING` is "the described resource could not be
//! found", so it is reserved for an id this service holds no instance of at all, while an
//! instance it HOLDS whose status takes it out of this action's reach is reported with that
//! status — a deregistered instance is still describable with `INACTIVE`
//! (`DeregisterContainerInstance`: "the instance then transitions to an INACTIVE status"),
//! so a mutation of it comes back `INACTIVE` and a client can tell a retired host from a
//! mistyped identifier.
//!
//! The rows vary the entry along what the rule generalises over: its FORM (bare id, an ARN
//! qualified by the cluster of the request, an ARN qualified by another cluster, an ARN
//! already carrying no cluster), the id's shape (the 32 hexadecimal digits and the
//! 36-character hyphenated form, the two lengths a container-instance id has), how many
//! entries are sent at once, whether a resolvable instance is sent beside them, and WHY
//! the entry resolves to nothing (nothing of that id at all, an instance registered into
//! another cluster, an instance of this cluster that was deregistered). Both statuses this
//! action governs are driven over the whole table. The recorded divergence — a
//! cluster-qualified ARN answered with the cluster still in it — is ONE row.
//!
//! The contrast the same suite pins: `DescribeContainerInstances` is a READ, and its
//! failure entry echoes the identifier the request sent, so the two operations answer the
//! same identifier differently.

use serde_json::json;

use crate::ecs_wire::{
    create_cluster, list, ok, register_instance, service, string, ACCOUNT, SIGNED_REGION,
};

/// The cluster every request below names.
const CLUSTER: &str = "drainingdemo";

/// Another cluster, for an entry that names an instance registered elsewhere.
const OTHER_CLUSTER: &str = "drainingelsewhere";

/// The `Failure$reason` for an identifier that names no instance this service holds, from
/// the API failure reasons: "MISSING — the described resource could not be found".
const MISSING: &str = "MISSING";

/// The `Failure$reason` for an instance this service HOLDS that has been deregistered: the
/// status such an instance keeps (`ContainerInstance$status` `INACTIVE`), which is what
/// takes it out of this action's reach.
const INACTIVE: &str = "INACTIVE";

/// A container-instance ARN in the signed region and the caller's account, under the
/// resource path `path`.
fn container_instance_arn(path: &str) -> String {
    format!("arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:container-instance/{path}")
}

/// The ARN the rule requires a failure to name `instance_id` by: the id ALONE in the
/// resource part.
fn unqualified(instance_id: &str) -> String {
    container_instance_arn(instance_id)
}

/// The instance id a container-instance ARN ends in.
fn arn_id(arn: &str) -> &str {
    arn.rsplit('/')
        .next()
        .expect("a container-instance ARN ends in its id")
}

/// One row of the table below: the case, the entries the request names, the failures
/// expected back IN THE REQUEST'S ORDER as `(arn, reason)` pairs, and how many instances
/// the call is expected to report as moved.
type Row = (&'static str, Vec<String>, Vec<(String, &'static str)>, usize);

/// The `(arn, reason)` pair of every failure in a response, in order.
fn failures(body: &serde_json::Value, case: &str) -> Vec<(String, String)> {
    list(body, "failures", case)
        .iter()
        .map(|failure| {
            (
                string(failure, "arn", case),
                string(failure, "reason", case),
            )
        })
        .collect()
}

#[tokio::test]
async fn a_state_change_names_an_unresolvable_instance_by_its_id_alone() {
    let router = service();
    create_cluster(&router, CLUSTER, "setup").await;
    create_cluster(&router, OTHER_CLUSTER, "setup").await;
    let stored = register_instance(&router, CLUSTER, "i-0a0a0a0a0a0a0a0a0", "setup").await;
    let elsewhere =
        register_instance(&router, OTHER_CLUSTER, "i-0b0b0b0b0b0b0b0b0", "setup").await;
    let elsewhere_id = arn_id(&elsewhere).to_string();

    // An instance of THIS cluster that is no longer registered in it: a deregistered one
    // "transitions to an INACTIVE status", so it is no instance of the cluster to move.
    let retired = register_instance(&router, CLUSTER, "i-0c0c0c0c0c0c0c0c0", "setup").await;
    ok(
        &router,
        "DeregisterContainerInstance",
        json!({ "cluster": CLUSTER, "containerInstance": retired.clone() }),
        "setup",
    )
    .await;
    let retired_id = arn_id(&retired).to_string();

    // The two lengths a container-instance id has: the 32 hexadecimal digits this store
    // mints and the 36 characters the hyphenated UUID form spells.
    let uuid_form = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_string();
    let other_uuid_form = "11111111-2222-3333-4444-555555555555".to_string();

    // Rows: the case, the entries the request names, the failures expected back IN THE
    // REQUEST'S ORDER as `(arn, reason)`, and how many instances the call is expected to
    // report as moved.
    let rows: Vec<Row> = vec![
        (
            "a bare 32-hex id that names nothing",
            vec!["f".repeat(32)],
            vec![(unqualified(&"f".repeat(32)), MISSING)],
            0,
        ),
        (
            "a full ARN qualified by the cluster of the request",
            vec![container_instance_arn(&format!(
                "{CLUSTER}/{}",
                "7".repeat(32)
            ))],
            vec![(unqualified(&"7".repeat(32)), MISSING)],
            0,
        ),
        (
            "a full ARN qualified by another cluster",
            vec![container_instance_arn(&format!(
                "{OTHER_CLUSTER}/{}",
                "6".repeat(32)
            ))],
            vec![(unqualified(&"6".repeat(32)), MISSING)],
            0,
        ),
        (
            "a full ARN already carrying no cluster",
            vec![unqualified(&"8".repeat(32))],
            vec![(unqualified(&"8".repeat(32)), MISSING)],
            0,
        ),
        (
            "a bare id in the hyphenated uuid form",
            vec![uuid_form.clone()],
            vec![(unqualified(&uuid_form), MISSING)],
            0,
        ),
        (
            "the hyphenated uuid form inside a cluster-qualified ARN",
            vec![container_instance_arn(&format!(
                "{CLUSTER}/{other_uuid_form}"
            ))],
            vec![(unqualified(&other_uuid_form), MISSING)],
            0,
        ),
        (
            "three unknown entries at once, one per form",
            vec![
                "a".repeat(32),
                container_instance_arn(&format!("{CLUSTER}/{}", "b".repeat(32))),
                container_instance_arn(&format!("{OTHER_CLUSTER}/{}", "c".repeat(32))),
            ],
            vec![
                (unqualified(&"a".repeat(32)), MISSING),
                (unqualified(&"b".repeat(32)), MISSING),
                (unqualified(&"c".repeat(32)), MISSING),
            ],
            0,
        ),
        (
            "the registered instance beside an unknown cluster-qualified ARN",
            vec![
                stored.clone(),
                container_instance_arn(&format!("{CLUSTER}/{}", "d".repeat(32))),
            ],
            vec![(unqualified(&"d".repeat(32)), MISSING)],
            1,
        ),
        (
            "an instance registered into another cluster, by its full ARN",
            vec![elsewhere.clone()],
            vec![(unqualified(&elsewhere_id), MISSING)],
            0,
        ),
        (
            "an instance registered into another cluster, by its bare id",
            vec![elsewhere_id.clone()],
            vec![(unqualified(&elsewhere_id), MISSING)],
            0,
        ),
        (
            // The instance IS held, under `INACTIVE`, so the reason is that status rather
            // than the one reserved for an id naming nothing.
            "a deregistered instance of this cluster, by its own ARN",
            vec![retired.clone()],
            vec![(unqualified(&retired_id), INACTIVE)],
            0,
        ),
        (
            "a deregistered instance beside an id naming nothing",
            vec![retired.clone(), "e".repeat(32)],
            vec![
                (unqualified(&retired_id), INACTIVE),
                (unqualified(&"e".repeat(32)), MISSING),
            ],
            0,
        ),
    ];

    // "The only valid values for this action are ACTIVE and DRAINING": the rule holds for
    // either state the call asks for.
    for status in ["DRAINING", "ACTIVE"] {
        for (row, named, expected, moved) in &rows {
            let case = format!("{row}, moving to {status}");
            let body = ok(
                &router,
                "UpdateContainerInstancesState",
                json!({
                    "cluster": CLUSTER,
                    "containerInstances": named,
                    "status": status,
                }),
                &case,
            )
            .await;
            let reported = failures(&body, &case);
            let want: Vec<(String, String)> = expected
                .iter()
                .map(|(arn, reason)| (arn.clone(), (*reason).to_string()))
                .collect();
            assert_eq!(
                reported, want,
                "case {case:?}: expected the failures {want:?}, got {reported:?} in {body}"
            );
            // The resource part is the instance id and nothing else, so no cluster name
            // reaches it however the entry spelled the instance.
            for (arn, _) in &reported {
                assert!(
                    !arn.contains(CLUSTER) && !arn.contains(OTHER_CLUSTER),
                    "case {case:?}: expected no cluster segment in the failure ARN, got \
                     {arn:?}"
                );
            }
            assert_eq!(
                list(&body, "containerInstances", &case).len(),
                *moved,
                "case {case:?}: expected {moved} instance(s) reported as moved, got {body}"
            );
        }

        // The instance that IS registered here moved, and the move reached the store: the
        // failures above are about entries this call could not resolve, not about a write
        // that was skipped.
        let case = format!("the registered instance reads back {status}");
        let read = ok(
            &router,
            "DescribeContainerInstances",
            json!({ "cluster": CLUSTER, "containerInstances": [stored.clone()] }),
            &case,
        )
        .await;
        assert_eq!(
            string(&list(&read, "containerInstances", &case)[0], "status", &case),
            status,
            "case {case:?}: expected the stored instance to report {status}, got {read}"
        );
    }
}

#[tokio::test]
async fn a_deregistered_instance_is_reported_inactive_whatever_form_names_it() {
    // The rule's discriminator, over the forms the member accepts: a deregistered instance
    // is one this service HOLDS — `DescribeContainerInstances` still returns it with status
    // `INACTIVE` — so a mutation of it is reported `INACTIVE`, while an id of the same
    // shape that this service holds nothing for is `MISSING`. The rows vary the FORM the
    // entry takes (bare id, the instance's own cluster-qualified ARN, an ARN qualified by
    // another cluster, an ARN carrying no cluster) and both statuses the action governs.
    let router = service();
    create_cluster(&router, CLUSTER, "setup").await;
    create_cluster(&router, OTHER_CLUSTER, "setup").await;
    let retired = register_instance(&router, CLUSTER, "i-0d0d0d0d0d0d0d0d0", "setup").await;
    ok(
        &router,
        "DeregisterContainerInstance",
        json!({ "cluster": CLUSTER, "containerInstance": retired.clone() }),
        "setup",
    )
    .await;
    let retired_id = arn_id(&retired).to_string();

    // The premise the reason rests on: the record survives the deregistration and is
    // describable under `INACTIVE`.
    let case = "the deregistered instance is still describable";
    let read = ok(
        &router,
        "DescribeContainerInstances",
        json!({ "cluster": CLUSTER, "containerInstances": [retired.clone()] }),
        case,
    )
    .await;
    let described = list(&read, "containerInstances", case);
    assert_eq!(
        described.len(),
        1,
        "case {case:?}: expected the deregistered instance back, got {read}"
    );
    assert_eq!(
        string(&described[0], "status", case),
        INACTIVE,
        "case {case:?}: expected the deregistered instance to report INACTIVE, got {read}"
    );
    assert!(
        failures(&read, case).is_empty(),
        "case {case:?}: a held instance is no failure of a read, got {read}"
    );

    // Each form, with the reason the entry earns: the two that RESOLVE to the held record
    // report its status, and a form that resolves to no instance of this cluster is
    // `MISSING` like any other identifier naming nothing — an ARN is a whole identifier,
    // so one qualified by another cluster names no instance of this one.
    let forms: [(&str, String, &str); 4] = [
        ("its bare id", retired_id.clone(), INACTIVE),
        ("its own full ARN", retired.clone(), INACTIVE),
        (
            "an ARN qualified by another cluster",
            container_instance_arn(&format!("{OTHER_CLUSTER}/{retired_id}")),
            MISSING,
        ),
        (
            "an ARN carrying no cluster",
            unqualified(&retired_id),
            MISSING,
        ),
    ];

    for status in ["DRAINING", "ACTIVE"] {
        for (form, reference, reason) in &forms {
            let case = format!("the deregistered instance by {form}, moving to {status}");
            // An id this service holds nothing for rides along in the same request, so the
            // reason of a held instance and the reason of an unknown id are read off ONE
            // response.
            let unknown = "9".repeat(32);
            let body = ok(
                &router,
                "UpdateContainerInstancesState",
                json!({
                    "cluster": CLUSTER,
                    "containerInstances": [reference.clone(), unknown.clone()],
                    "status": status,
                }),
                &case,
            )
            .await;
            assert_eq!(
                failures(&body, &case),
                vec![
                    (unqualified(&retired_id), (*reason).to_string()),
                    (unqualified(&unknown), MISSING.to_string()),
                ],
                "case {case:?}: expected the entry reported {reason} and the unknown id \
                 MISSING, got {body}"
            );
            assert!(
                list(&body, "containerInstances", &case).is_empty(),
                "case {case:?}: neither entry is an instance this call moved, got {body}"
            );
        }
    }

    // And the refused mutation left the record where the deregistration put it.
    let case = "the deregistered instance was not moved";
    let after = ok(
        &router,
        "DescribeContainerInstances",
        json!({ "cluster": CLUSTER, "containerInstances": [retired.clone()] }),
        case,
    )
    .await;
    assert_eq!(
        string(&list(&after, "containerInstances", case)[0], "status", case),
        INACTIVE,
        "case {case:?}: expected the instance to stay INACTIVE, got {after}"
    );
}

#[tokio::test]
async fn the_read_echoes_the_identifier_the_state_change_normalises() {
    let router = service();
    create_cluster(&router, CLUSTER, "setup").await;
    create_cluster(&router, OTHER_CLUSTER, "setup").await;

    // Rows: the case, the entry, the ARN the STATE CHANGE names it by (the id alone) and
    // the ARN the READ echoes (the identifier as sent — the value itself when it is
    // already an ARN, otherwise the id alone).
    let qualified = container_instance_arn(&format!("{CLUSTER}/{}", "4".repeat(32)));
    let foreign = container_instance_arn(&format!("{OTHER_CLUSTER}/{}", "5".repeat(32)));
    let rows: Vec<(&str, String, String, String)> = vec![
        (
            "a cluster-qualified ARN",
            qualified.clone(),
            unqualified(&"4".repeat(32)),
            qualified.clone(),
        ),
        (
            "an ARN qualified by another cluster",
            foreign.clone(),
            unqualified(&"5".repeat(32)),
            foreign.clone(),
        ),
        (
            // A bare id carries no cluster to begin with, so both answer alike.
            "a bare id",
            "3".repeat(32),
            unqualified(&"3".repeat(32)),
            unqualified(&"3".repeat(32)),
        ),
    ];

    for (case, named, updated_arn, described_arn) in &rows {
        let update = ok(
            &router,
            "UpdateContainerInstancesState",
            json!({
                "cluster": CLUSTER,
                "containerInstances": [named],
                "status": "DRAINING",
            }),
            case,
        )
        .await;
        assert_eq!(
            failures(&update, case),
            vec![(updated_arn.clone(), MISSING.to_string())],
            "case {case:?}: expected the state change to name {updated_arn:?}, got {update}"
        );

        let describe = ok(
            &router,
            "DescribeContainerInstances",
            json!({ "cluster": CLUSTER, "containerInstances": [named] }),
            case,
        )
        .await;
        assert_eq!(
            failures(&describe, case),
            vec![(described_arn.clone(), MISSING.to_string())],
            "case {case:?}: expected the read to echo {described_arn:?}, got {describe}"
        );
    }
}
