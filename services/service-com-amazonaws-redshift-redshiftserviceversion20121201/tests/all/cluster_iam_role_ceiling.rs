// SPDX-License-Identifier: CC0-1.0

//! How MANY IAM roles one cluster may have associated with it, end to end over
//! the awsQuery wire.
//!
//! The RULE under test: a cluster carries at most the quota's worth of
//! associated IAM roles ("The maximum number of IAM roles that you can associate
//! is subject to a quota", `smithy_query(query="doc",
//! operation="ModifyClusterIamRoles")`), so every call that would associate one
//! past that ceiling is refused with `InvalidParameterValue` — naming the number
//! of roles the cluster already carries and the headroom that is left — and
//! associates nothing. The role list is a SET keyed by role ARN, so a role the
//! cluster already carries is not a new association and does not count against
//! the headroom.
//!
//! The rows differ along what the rule generalises over: how many roles the
//! cluster already carries (none, below the ceiling, at it), how many the call
//! would add (one, several, more than the headroom), whether those roles are new
//! or already associated, and which operation associates them — the modify, and
//! the two paths that START a cluster with roles (`CreateCluster` and
//! `RestoreFromClusterSnapshot`).
//!
//! The harness is shared with [`crate::cluster_lifecycle`], which owns it.

use service_com_amazonaws_redshift_redshiftserviceversion20121201::state;

use crate::cluster_lifecycle::{all_text, Answer, Emulator};

/// The ARN of the `index`th role a case associates, in the documented form the
/// model's own example gives for `ClusterIamRole$IamRoleArn`
/// ("arn:aws:iam::123456789012:role/RedshiftCopyUnload").
fn role(index: usize) -> String {
    format!("arn:aws:iam::123456789012:role/CeilingRole{index:03}")
}

/// `count` role ARNs, from `first`.
fn roles(first: usize, count: usize) -> Vec<String> {
    (first..first + count).map(role).collect()
}

/// The sentence a request past the ceiling is refused with, spelled out here
/// rather than read off the crate under test: the fixed text is
/// `Cluster already has `, ` IAM Roles. Only ` and
/// ` new IAM Role(s) may be added to this Cluster.`, and the two numbers are the
/// roles the cluster already carries and the headroom left.
fn quota_refusal(associated: usize, remaining: usize) -> String {
    format!(
        "Cluster already has {associated} IAM Roles. \
         Only {remaining} new IAM Role(s) may be added to this Cluster."
    )
}

/// `call` takes borrowed pairs, so a case that builds owned parameters borrows
/// them through this.
fn borrowed(params: &[(String, String)]) -> Vec<(&str, &str)> {
    params
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect()
}

/// Just the role ARNs a body's `IamRoles` list carries, in report order.
fn associated_arns(body: &str) -> Vec<String> {
    let open = "<IamRoles>";
    let Some(start) = body.find(open) else {
        return Vec::new();
    };
    let rest = &body[start + open.len()..];
    let end = rest.find("</IamRoles>").unwrap_or(rest.len());
    all_text(&rest[..end], "IamRoleArn")
}

/// Create one cluster and bring it to the state a configuration change is
/// admitted in, by ADVANCING this emulator's clock across the provisioning span
/// rather than waiting for it.
async fn available_cluster(emulator: &Emulator, identifier: &str) {
    let created = emulator.create_minimal(identifier).await;
    assert_eq!(
        created.status, 200,
        "case seed cluster {identifier}: expected 200, got {} with body {}",
        created.status, created.body
    );
    emulator.advance_clock(state::PROVISIONING_SPAN);
}

/// `ModifyClusterIamRoles` associating `add` and disassociating `remove`.
async fn modify_roles(
    emulator: &Emulator,
    cluster: &str,
    add: &[String],
    remove: &[String],
) -> Answer {
    let mut params: Vec<(String, String)> =
        vec![("ClusterIdentifier".to_string(), cluster.to_string())];
    for (position, arn) in add.iter().enumerate() {
        params.push((
            format!("AddIamRoles.IamRoleArn.{}", position + 1),
            arn.clone(),
        ));
    }
    for (position, arn) in remove.iter().enumerate() {
        params.push((
            format!("RemoveIamRoles.IamRoleArn.{}", position + 1),
            arn.clone(),
        ));
    }
    emulator
        .call("ModifyClusterIamRoles", &borrowed(&params))
        .await
}

/// `DescribeClusters` for one cluster.
async fn describe(emulator: &Emulator, cluster: &str) -> Answer {
    emulator
        .call("DescribeClusters", &[("ClusterIdentifier", cluster)])
        .await
}

/// Assert one refusal: the status, the `<Code>` and the whole sentence.
fn assert_refused(case: &str, answer: &Answer, message: &str) {
    assert_eq!(
        (answer.status, answer.error_code().as_deref()),
        (400, Some("InvalidParameterValue")),
        "case {case}: expected 400 InvalidParameterValue, got {} / {:?} with body {}",
        answer.status,
        answer.error_code(),
        answer.body
    );
    assert_eq!(
        answer.text("Message").as_deref(),
        Some(message),
        "case {case}: expected the message {message:?}, got {:?}",
        answer.text("Message")
    );
}

#[tokio::test]
async fn an_association_past_the_ceiling_is_refused_and_associates_nothing() {
    let emulator = Emulator::new();
    let cluster = "iam-ceiling-cluster";
    available_cluster(&emulator, cluster).await;
    let ceiling = state::CLUSTER_IAM_ROLE_QUOTA;

    // Exactly the ceiling, in one call: the most a cluster may carry is served.
    let filled = modify_roles(&emulator, cluster, &roles(0, ceiling), &[]).await;
    assert_eq!(
        filled.status, 200,
        "case the ceiling in one call: expected 200, got {} with body {}",
        filled.status, filled.body
    );
    emulator.advance_clock(state::PROVISIONING_SPAN);
    let read = describe(&emulator, cluster).await;
    assert_eq!(
        associated_arns(&read.body).len(),
        ceiling,
        "case the ceiling in one call: expected {ceiling} associations, got {:?}",
        associated_arns(&read.body)
    );

    // (case, the roles the call would add)
    let over: Vec<(&str, Vec<String>)> = vec![
        ("one role past the ceiling", roles(ceiling, 1)),
        ("two roles past the ceiling", roles(ceiling, 2)),
        ("a whole second ceiling's worth", roles(ceiling, ceiling)),
        (
            "a role already carried beside a new one past the ceiling",
            vec![role(0), role(ceiling)],
        ),
    ];

    for (case, add) in over {
        let refused = modify_roles(&emulator, cluster, &add, &[]).await;
        assert_refused(case, &refused, &quota_refusal(ceiling, 0));
        // Refused BEFORE anything was associated: the cluster still carries what
        // it did, and it was not left applying a change either.
        let read = describe(&emulator, cluster).await;
        assert_eq!(
            associated_arns(&read.body),
            roles(0, ceiling),
            "case {case}: expected the cluster's associations untouched, got {:?}",
            associated_arns(&read.body)
        );
        assert_eq!(
            read.text("ClusterStatus").as_deref(),
            Some(state::STATUS_AVAILABLE),
            "case {case}: expected the refused call to leave the cluster {:?}, got {:?}",
            state::STATUS_AVAILABLE,
            read.text("ClusterStatus")
        );
    }

    // A role the cluster ALREADY carries is no new association, so the same call
    // at the ceiling is served.
    let repeat = modify_roles(&emulator, cluster, &[role(0), role(1)], &[]).await;
    assert_eq!(
        repeat.status, 200,
        "case a role already carried, at the ceiling: expected 200, got {} with body {}",
        repeat.status, repeat.body
    );

    // Headroom is read off the cluster's own state: with one role disassociated
    // the refusal names the smaller count and the room that is left.
    emulator.advance_clock(state::PROVISIONING_SPAN);
    let freed = modify_roles(&emulator, cluster, &[], &[role(0)]).await;
    assert_eq!(
        freed.status, 200,
        "case free one association: expected 200, got {} with body {}",
        freed.status, freed.body
    );
    emulator.advance_clock(state::PROVISIONING_SPAN);
    let refused = modify_roles(&emulator, cluster, &roles(ceiling, 2), &[]).await;
    assert_refused(
        "two roles into one role's headroom",
        &refused,
        &quota_refusal(ceiling - 1, 1),
    );
    // And the one role that DOES fit the headroom is associated.
    let served = modify_roles(&emulator, cluster, &roles(ceiling, 1), &[]).await;
    assert_eq!(
        served.status, 200,
        "case one role into one role's headroom: expected 200, got {} with body {}",
        served.status, served.body
    );
    emulator.advance_clock(state::PROVISIONING_SPAN);
    let read = describe(&emulator, cluster).await;
    assert_eq!(
        associated_arns(&read.body).len(),
        ceiling,
        "case one role into one role's headroom: expected the cluster back at {ceiling} \
         associations, got {:?}",
        associated_arns(&read.body)
    );
}

#[tokio::test]
async fn a_cluster_cannot_be_started_with_more_roles_than_the_ceiling() {
    let emulator = Emulator::new();
    let ceiling = state::CLUSTER_IAM_ROLE_QUOTA;

    // (case, the identifier, how many roles the create names, whether it is
    // served)
    let rows: Vec<(&str, &str, usize, bool)> = vec![
        ("no role at all", "iam-create-none", 0, true),
        ("one role", "iam-create-one", 1, true),
        ("exactly the ceiling", "iam-create-ceiling", ceiling, true),
        (
            "one role past the ceiling",
            "iam-create-over",
            ceiling + 1,
            false,
        ),
        (
            "far past the ceiling",
            "iam-create-far-over",
            ceiling * 2,
            false,
        ),
    ];

    for (case, identifier, count, served) in rows {
        let mut params: Vec<(String, String)> = vec![
            ("ClusterIdentifier".to_string(), identifier.to_string()),
            ("NodeType".to_string(), "ra3.xlplus".to_string()),
            ("MasterUsername".to_string(), "admin".to_string()),
            (
                "MasterUserPassword".to_string(),
                "Probe-Passw0rd".to_string(),
            ),
        ];
        for (position, arn) in roles(0, count).iter().enumerate() {
            params.push((
                format!("IamRoles.IamRoleArn.{}", position + 1),
                arn.clone(),
            ));
        }
        let answer = emulator.call("CreateCluster", &borrowed(&params)).await;
        if served {
            assert_eq!(
                answer.status, 200,
                "case {case}: expected the create to be served, got {} with body {}",
                answer.status, answer.body
            );
            assert_eq!(
                associated_arns(&answer.body).len(),
                count,
                "case {case}: expected {count} associations on the new cluster, got {:?}",
                associated_arns(&answer.body)
            );
        } else {
            // A create that names more roles than a cluster may carry is
            // refused, and no cluster is left behind for it.
            assert_refused(case, &answer, &quota_refusal(0, ceiling));
            let read = describe(&emulator, identifier).await;
            assert_eq!(
                read.error_code().as_deref(),
                Some("ClusterNotFound"),
                "case {case}: expected the refused create to leave no cluster, got {} with \
                 body {}",
                read.status,
                read.body
            );
        }
    }

    // The same ceiling holds on the other path that STARTS a cluster with roles:
    // a restore from a snapshot of one.
    emulator.advance_clock(state::PROVISIONING_SPAN);
    let taken = emulator
        .call(
            "CreateClusterSnapshot",
            &[
                ("SnapshotIdentifier", "iam-ceiling-snapshot"),
                ("ClusterIdentifier", "iam-create-one"),
            ],
        )
        .await;
    assert_eq!(
        taken.status, 200,
        "case seed the snapshot: expected 200, got {} with body {}",
        taken.status, taken.body
    );
    emulator.advance_clock(state::PROVISIONING_SPAN);

    for (case, identifier, count, served) in [
        (
            "a restore naming exactly the ceiling",
            "iam-restore-ceiling",
            ceiling,
            true,
        ),
        (
            "a restore naming one role past the ceiling",
            "iam-restore-over",
            ceiling + 1,
            false,
        ),
    ] {
        let mut params: Vec<(String, String)> = vec![
            ("ClusterIdentifier".to_string(), identifier.to_string()),
            (
                "SnapshotIdentifier".to_string(),
                "iam-ceiling-snapshot".to_string(),
            ),
        ];
        for (position, arn) in roles(0, count).iter().enumerate() {
            params.push((
                format!("IamRoles.IamRoleArn.{}", position + 1),
                arn.clone(),
            ));
        }
        let answer = emulator
            .call("RestoreFromClusterSnapshot", &borrowed(&params))
            .await;
        if served {
            assert_eq!(
                answer.status, 200,
                "case {case}: expected the restore to be served, got {} with body {}",
                answer.status, answer.body
            );
        } else {
            assert_refused(case, &answer, &quota_refusal(0, ceiling));
        }
    }
}
