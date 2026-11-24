// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the ROLE a table replication is carried out as:
//! what `GetTableReplicationStatus` reports for a destination when the account's
//! IAM answers that the configuration's role cannot replicate.
//!
//! RULE: a table is replicated ACTING AS the role its configuration names — "the
//! IAM role that S3 Tables assumes to replicate the table on your behalf"
//! (`TableReplicationConfiguration$role`), passed to
//! `replication.s3tables.amazonaws.com` and required to "have permissions to
//! read from the source table and write to all destination tables"
//! (`PutTableReplication`) — so a destination whose replication that role cannot
//! carry out settles at the modelled terminal `failed` state with
//! `failureMessage` stating the refusal IAM answered, instead of reaching
//! `completed`. The two questions the role is asked are the two IAM decides: can
//! the replication principal ASSUME it (its trust policy), and is it ALLOWED the
//! read of the source table and the write at each destination (its
//! identity-based policies). The first is about the configuration as a whole, so
//! every destination of such a configuration fails; the second is per
//! destination, so a role allowed to write to one destination and not another
//! fails exactly the destination it may not write to. Nothing fails on evidence
//! the account's IAM does not hold: a role IAM holds no record of, and a role
//! carrying no readable permission policy at all, leave replication to proceed.
//! The put itself is accepted either way — it mints a `versionToken` and reports
//! success — because the role is exercised by the asynchronous replication, not
//! by the write that configures it.
//!
//! The rows span what the rule generalises over: which form the trust policy
//! names a principal in (one string, a list of them), whether it names the
//! replication principal / only another service / the replication principal
//! under a `Deny`, whether the permission policies allow the source read and the
//! destination write / deny one of them / state nothing about one of them /
//! state nothing at all, and whether IAM holds the role at all. The sentences
//! asserted are IAM's own, spelled here independently of the service under test.
//!
//! The principals are made with IAM's OWN operations over the same engine
//! ([`super::table_encryption_authorization`] owns that harness, shared rather
//! than copied), so the documents this service reads are the ones IAM wrote, and
//! every call drives this service the way a client does.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::harness::{encode, query_string, Reply};
use super::table_encryption_authorization::{created_role, statement, Emulator, ACCOUNT_KEY};

/// The `ReplicationStatus` values a destination reaches, from
/// `smithy_query(query="shape",shape="ReplicationStatus")`.
const PENDING: &str = "pending";
const COMPLETED: &str = "completed";
const FAILED: &str = "failed";

/// The service principal a replication role must trust: the one
/// `PutTableReplication` names as what a replication role is passed to.
const REPLICATION_PRINCIPAL: &str = "replication.s3tables.amazonaws.com";

/// The IAM actions the role performs, as `PutTableReplication` names them: the
/// read of the source table, and the write that leaves the replica at the
/// destination.
const SOURCE_READ: &str = "s3tables:GetTable";
const DESTINATION_WRITE: &str = "s3tables:CreateTable";

/// The namespace the fixture tables live in.
const NAMESPACE: &str = "repl_role_ns";

/// How long a case waits for a destination to settle before giving up.
/// GENEROUS on purpose: the case polls for the condition and asserts nothing
/// about how long it took, so a loaded host only makes it poll more often.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How long a case waits between two polls of a destination's state.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The sentence IAM refuses a principal with when the RESOURCE's own policy
/// grants it nothing — a role's trust policy being that resource-based policy.
fn no_resource_policy_allows(principal: &str, action: &str, resource: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         because no resource-based policy allows the {action} action"
    )
}

/// The sentence IAM refuses a principal with when its OWN identity-based
/// documents do not allow the call, with the clause naming which of IAM's rules
/// withheld the grant.
fn not_authorized_by_identity_policy(
    principal: &str,
    action: &str,
    resource: &str,
    reason: &str,
) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} {reason}"
    )
}

/// The reason clause a principal carrying no grant for `action` is refused with.
fn no_grant(action: &str) -> String {
    format!("because no identity-based policy allows the {action} action")
}

/// The reason clause an explicit `Deny` is refused with.
const EXPLICIT_DENY: &str = "with an explicit deny in an identity-based policy";

/// The failure a destination of an unassumable role states: the assume the
/// replication principal could not make on that role.
fn assume_refused(role_arn: &str, _source: &str, _destination: &str) -> String {
    no_resource_policy_allows(REPLICATION_PRINCIPAL, "sts:AssumeRole", role_arn)
}

/// The failure a destination states when the role's policies say nothing about
/// reading the source table.
fn source_read_not_allowed(role_arn: &str, source: &str, _destination: &str) -> String {
    not_authorized_by_identity_policy(role_arn, SOURCE_READ, source, &no_grant(SOURCE_READ))
}

/// The failure a destination states when the role's policies DENY reading the
/// source table.
fn source_read_denied(role_arn: &str, source: &str, _destination: &str) -> String {
    not_authorized_by_identity_policy(role_arn, SOURCE_READ, source, EXPLICIT_DENY)
}

/// The failure a destination states when the role's policies say nothing about
/// the write at that destination.
fn destination_write_not_allowed(role_arn: &str, _source: &str, destination: &str) -> String {
    not_authorized_by_identity_policy(
        role_arn,
        DESTINATION_WRITE,
        destination,
        &no_grant(DESTINATION_WRITE),
    )
}

/// The failure a destination states when the role's policies DENY the write at
/// that destination.
fn destination_write_denied(role_arn: &str, _source: &str, destination: &str) -> String {
    not_authorized_by_identity_policy(role_arn, DESTINATION_WRITE, destination, EXPLICIT_DENY)
}

/// A trust policy whose one statement names `service` under `Principal.Service`
/// with the effect `effect`. `service` is either one name or a list of them, the
/// two forms the policy language writes that element in.
fn trust(effect: &str, service: Value) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": effect,
            "Principal": {"Service": service},
            "Action": "sts:AssumeRole",
        }],
    })
}

/// A trust policy that lets the replication principal assume the role.
fn trusts_replication() -> Value {
    trust("Allow", json!(REPLICATION_PRINCIPAL))
}

/// One row: the documents the replication role carries, and what the
/// destination replicated to must settle at.
struct Case {
    label: &'static str,
    /// The role's trust policy.
    trust: Value,
    /// The inline permission policies embedded in the role, as a function of
    /// the source table's ARN and the destination table bucket's ARN — the two
    /// resources the replication's own read and write address, so a row names
    /// them rather than spelling an ARN of its own.
    policies: fn(&str, &str) -> Vec<String>,
    /// `None` when replication must reach `completed`, else the sentence the
    /// failed destination must state, built from the role, the source table and
    /// the destination table bucket.
    failure: Option<fn(&str, &str, &str) -> String>,
}

/// Every action of this service on every resource — what a replication role
/// carries when it is granted the replication outright.
fn every_action(_source: &str, _destination: &str) -> Vec<String> {
    vec![statement("Allow", "s3tables:*", "*")]
}

/// No permission policy at all, the routine state of a role provisioned out of
/// band.
fn no_policies(_source: &str, _destination: &str) -> Vec<String> {
    Vec::new()
}

/// The replication's own read and write, each granted on the resource it
/// addresses and nothing wider.
fn the_read_and_the_write(source: &str, destination: &str) -> Vec<String> {
    vec![
        statement("Allow", SOURCE_READ, source),
        statement("Allow", DESTINATION_WRITE, destination),
    ]
}

/// Every action of this service, but only on the SOURCE table: nothing is
/// granted at the destination.
fn the_source_only(source: &str, _destination: &str) -> Vec<String> {
    vec![statement("Allow", "s3tables:*", source)]
}

/// The write at the destination alone: nothing is granted on the source.
fn the_write_only(_source: &str, destination: &str) -> Vec<String> {
    vec![statement("Allow", DESTINATION_WRITE, destination)]
}

/// Every action, with the write at the destination explicitly denied.
fn everything_but_the_write(_source: &str, destination: &str) -> Vec<String> {
    vec![
        statement("Allow", "s3tables:*", "*"),
        statement("Deny", DESTINATION_WRITE, destination),
    ]
}

/// Every action, with the read of the source explicitly denied.
fn everything_but_the_read(source: &str, _destination: &str) -> Vec<String> {
    vec![
        statement("Allow", "s3tables:*", "*"),
        statement("Deny", SOURCE_READ, source),
    ]
}

/// The role every row's replication runs as: an IAM role carrying `trust` and
/// one inline policy per document in `policies`, made with IAM's own operations
/// through the shared creator and answered by the ARN IAM minted for it.
async fn role(aws: &Emulator, role_name: &str, trust: &Value, policies: &[String]) -> String {
    created_role(aws, role_name, &trust.to_string(), policies).await
}

/// A table bucket holding one namespace with the tables `tables` names,
/// answering `(bucket ARN, the ARN of each table in order)`.
async fn fixture(aws: &Emulator, bucket: &str, tables: &[&str]) -> (String, Vec<String>) {
    let arn = aws
        .ok("PUT", "/buckets", Some(&json!({ "name": bucket })))
        .await
        .text("arn");
    aws.ok(
        "PUT",
        &format!("/namespaces/{}", encode(&arn)),
        Some(&json!({ "namespace": [NAMESPACE] })),
    )
    .await;
    let mut arns = Vec::new();
    for table in tables {
        let made = aws
            .ok(
                "PUT",
                &format!("/tables/{}/{}", encode(&arn), encode(NAMESPACE)),
                Some(&json!({ "name": table, "format": "ICEBERG" })),
            )
            .await;
        arns.push(made.text("tableARN"));
    }
    (arn, arns)
}

/// `PutTableReplication` on `table_arn` under `role_arn`, replicating to every
/// bucket `destinations` names. The put is expected to be ACCEPTED whatever the
/// role is: the role is exercised by the replication, not by this write.
async fn put_replication(
    aws: &Emulator,
    case: &str,
    table_arn: &str,
    role_arn: &str,
    destinations: &[String],
) -> Reply {
    let configuration = json!({
        "role": role_arn,
        "rules": [{
            "destinations": destinations
                .iter()
                .map(|destination| json!({ "destinationTableBucketARN": destination }))
                .collect::<Vec<Value>>(),
        }],
    });
    let put = aws
        .call_as(
            ACCOUNT_KEY,
            "PUT",
            &format!(
                "/table-replication{}",
                query_string(&[("tableArn", table_arn.to_string())])
            ),
            Some(&json!({ "configuration": configuration })),
        )
        .await;
    assert_eq!(
        put.status, 200,
        "case {case:?}: the put must be accepted whatever the role is, got {status} {body}",
        status = put.status,
        body = put.body
    );
    assert!(
        !put.text("versionToken").is_empty(),
        "case {case:?}: an accepted put mints a version token, got {body}",
        body = put.body
    );
    put
}

/// Poll `GetTableReplicationStatus(tableArn)` until the destination at `index`
/// reports a TERMINAL state — `completed` or `failed` — and answer that entry.
///
/// The wait is for the CONDITION: the read is taken again until the state
/// settles or [`SETTLE_DEADLINE`] passes, and nothing is asserted about how long
/// that took.
async fn settled(aws: &Emulator, case: &str, table_arn: &str, index: usize) -> Value {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let read = aws
            .call_as(
                ACCOUNT_KEY,
                "GET",
                &format!(
                    "/replication-status{}",
                    query_string(&[("tableArn", table_arn.to_string())])
                ),
                None,
            )
            .await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: the status read must be served, got {status} {body}",
            status = read.status,
            body = read.body
        );
        let destination = read
            .list("destinations")
            .get(index)
            .cloned()
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: the read reports a destination at {index}, got {body}",
                    body = read.body
                )
            });
        let state = destination
            .get("replicationStatus")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if state == COMPLETED || state == FAILED {
            return destination;
        }
        assert_eq!(
            state, PENDING,
            "case {case:?}: a destination reports a modelled state, got {destination}"
        );
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the destination never settled, last read {destination}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Assert the settled destination is what the row expects: `completed` with no
/// failure stated, or `failed` stating IAM's own sentence for the refusal.
fn assert_settled(case: &Case, settled: &Value, role_arn: &str, source: &str, destination: &str) {
    let state = settled
        .get("replicationStatus")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let stated = settled.get("failureMessage").and_then(Value::as_str);
    match case.failure {
        None => {
            assert_eq!(
                state, COMPLETED,
                "case {label:?}: expected {COMPLETED}, got {settled}",
                label = case.label
            );
            assert_eq!(
                stated, None,
                "case {label:?}: a destination that is not failed states no failure, got \
                 {stated:?}",
                label = case.label
            );
        }
        Some(expected) => {
            assert_eq!(
                state, FAILED,
                "case {label:?}: expected {FAILED}, got {settled}",
                label = case.label
            );
            let expected = expected(role_arn, source, destination);
            assert_eq!(
                stated,
                Some(expected.as_str()),
                "case {label:?}: expected IAM's sentence {expected:?}, got {stated:?}",
                label = case.label
            );
            assert!(
                settled.get("lastSuccessfulReplicatedUpdate").is_none(),
                "case {label:?}: a failed destination reports no replicated update, got {settled}",
                label = case.label
            );
        }
    }
}

/// Run every row of `cases` against its own table and one shared destination
/// table bucket, each under its own role.
async fn run(aws: &Emulator, prefix: &str, cases: &[Case]) {
    let names: Vec<String> = (0..cases.len()).map(|index| format!("t{index}")).collect();
    let tables: Vec<&str> = names.iter().map(String::as_str).collect();
    let (_, table_arns) = fixture(aws, &format!("s3t-{prefix}-src"), &tables).await;
    let destination = aws
        .ok(
            "PUT",
            "/buckets",
            Some(&json!({ "name": format!("s3t-{prefix}-dst") })),
        )
        .await
        .text("arn");

    for (index, case) in cases.iter().enumerate() {
        let table_arn = table_arns[index].clone();
        let role_arn = role(
            aws,
            &format!("{prefix}-role-{index}"),
            &case.trust,
            &(case.policies)(&table_arn, &destination),
        )
        .await;
        put_replication(
            aws,
            case.label,
            &table_arn,
            &role_arn,
            std::slice::from_ref(&destination),
        )
        .await;
        let settled = settled(aws, case.label, &table_arn, 0).await;
        assert_settled(case, &settled, &role_arn, &table_arn, &destination);
    }
}

#[tokio::test]
async fn a_destination_of_a_role_the_replication_principal_cannot_assume_settles_at_failed() {
    let aws = Emulator::new();
    let cases = vec![
        Case {
            label: "a trust policy naming the replication principal",
            trust: trusts_replication(),
            policies: every_action,
            failure: None,
        },
        Case {
            label: "a trust policy naming the replication principal in a LIST of services",
            trust: trust("Allow", json!(["ec2.amazonaws.com", REPLICATION_PRINCIPAL])),
            policies: every_action,
            failure: None,
        },
        Case {
            label: "a trust policy naming only another service",
            trust: trust("Allow", json!("ec2.amazonaws.com")),
            policies: no_policies,
            failure: Some(assume_refused),
        },
        Case {
            label: "a trust policy naming only another service, with every permission granted",
            trust: trust("Allow", json!("ec2.amazonaws.com")),
            policies: every_action,
            failure: Some(assume_refused),
        },
        Case {
            label: "a trust policy that DENIES the replication principal",
            trust: trust("Deny", json!(REPLICATION_PRINCIPAL)),
            policies: every_action,
            failure: Some(assume_refused),
        },
    ];
    run(&aws, "repl-trust", &cases).await;
}

#[tokio::test]
async fn a_destination_the_role_may_not_read_or_write_settles_at_failed() {
    let aws = Emulator::new();
    let cases = vec![
        Case {
            label: "a role granted the source read and the destination write by name",
            trust: trusts_replication(),
            policies: the_read_and_the_write,
            failure: None,
        },
        Case {
            label: "a role granted every action of this service on the SOURCE TABLE only",
            trust: trusts_replication(),
            policies: the_source_only,
            failure: Some(destination_write_not_allowed),
        },
        Case {
            label: "a role granted everything and denied the destination write",
            trust: trusts_replication(),
            policies: everything_but_the_write,
            failure: Some(destination_write_denied),
        },
        Case {
            label: "a role granted only the destination write",
            trust: trusts_replication(),
            policies: the_write_only,
            failure: Some(source_read_not_allowed),
        },
        Case {
            label: "a role granted everything and denied the source read",
            trust: trusts_replication(),
            policies: everything_but_the_read,
            failure: Some(source_read_denied),
        },
    ];
    run(&aws, "repl-grant", &cases).await;
}

#[tokio::test]
async fn a_role_the_account_states_nothing_about_leaves_replication_to_proceed() {
    let aws = Emulator::new();
    let (_, table_arns) = fixture(&aws, "s3t-repl-silent-src", &["held", "unheld"]).await;
    let destination = aws
        .ok(
            "PUT",
            "/buckets",
            Some(&json!({ "name": "s3t-repl-silent-dst" })),
        )
        .await
        .text("arn");

    // A role IAM HOLDS that carries no permission policy at all: the routine
    // state of a role provisioned out of band, so its documents state nothing
    // about what it may do and replication proceeds.
    let carries_no_policy = role(&aws, "repl-silent-role", &trusts_replication(), &[]).await;
    // And a role IAM holds no record of at all — another account's role is in
    // that position too.
    let unheld = format!(
        "arn:aws:iam::{account}:role/never-created",
        account = engine::emulator_account_id()
    );

    for (case, table_arn, role_arn) in [
        (
            "a held role carrying no permission policy",
            &table_arns[0],
            &carries_no_policy,
        ),
        ("a role IAM holds no record of", &table_arns[1], &unheld),
    ] {
        put_replication(
            &aws,
            case,
            table_arn,
            role_arn,
            std::slice::from_ref(&destination),
        )
        .await;
        let settled = settled(&aws, case, table_arn, 0).await;
        let state = settled
            .get("replicationStatus")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert_eq!(
            state, COMPLETED,
            "case {case:?}: nothing is failed on evidence IAM does not hold, got {settled}"
        );
        assert_eq!(
            settled.get("failureMessage"),
            None,
            "case {case:?}: a destination that is not failed states no failure, got {settled}"
        );
    }
}

#[tokio::test]
async fn the_destination_write_is_decided_per_destination() {
    let aws = Emulator::new();
    let (_, table_arns) = fixture(&aws, "s3t-repl-perdst-src", &["spread"]).await;
    let table_arn = table_arns[0].clone();
    // TWO destination table buckets the store holds, so the only difference
    // between them is what the role is allowed at each.
    let mut destinations = Vec::new();
    for name in ["s3t-repl-perdst-a", "s3t-repl-perdst-b"] {
        destinations.push(
            aws.ok("PUT", "/buckets", Some(&json!({ "name": name })))
                .await
                .text("arn"),
        );
    }
    let granted = destinations[0].clone();
    let withheld = destinations[1].clone();
    let role_arn = role(
        &aws,
        "repl-perdst-role",
        &trusts_replication(),
        &[
            statement("Allow", SOURCE_READ, "*"),
            statement("Allow", DESTINATION_WRITE, &granted),
        ],
    )
    .await;
    put_replication(
        &aws,
        "a role allowed the write at one destination only",
        &table_arn,
        &role_arn,
        &destinations,
    )
    .await;

    let allowed = settled(&aws, "the granted destination", &table_arn, 0).await;
    assert_eq!(
        allowed.get("replicationStatus").and_then(Value::as_str),
        Some(COMPLETED),
        "case \"the granted destination\": expected {COMPLETED}, got {allowed}"
    );
    assert_eq!(
        allowed.get("destinationTableBucketArn").and_then(Value::as_str),
        Some(granted.as_str()),
        "case \"the granted destination\": the entries keep the rule's order, got {allowed}"
    );

    let refused = settled(&aws, "the withheld destination", &table_arn, 1).await;
    assert_eq!(
        refused.get("replicationStatus").and_then(Value::as_str),
        Some(FAILED),
        "case \"the withheld destination\": expected {FAILED}, got {refused}"
    );
    assert_eq!(
        refused.get("failureMessage").and_then(Value::as_str),
        Some(
            not_authorized_by_identity_policy(
                &role_arn,
                DESTINATION_WRITE,
                &withheld,
                &no_grant(DESTINATION_WRITE),
            )
            .as_str()
        ),
        "case \"the withheld destination\": expected IAM's sentence naming that destination, got \
         {refused}"
    );
}
