// SPDX-License-Identifier: CC0-1.0

//! The RBAC REVOCATION FLOW of a user group attached to a replication group: what each wait
//! of it is answered with, and what the cache's own socket makes of the membership those
//! waits converged on.
//!
//! The rule under test, over a SET of inputs rather than the one recorded run: the call that
//! STARTS work answers with the transient status the service documents for that phase and the
//! clock owes the record its settled state a span later, so a REPORT issued inside the span
//! answers from the record as it stands while the FIRST report issued once the change is due
//! answers with the settled one — nothing of this service sleeps for a span it scheduled, and
//! no client has to read twice after the clock has carried a record. The membership that
//! settled change leaves is then the ONE membership both planes judge by: the control plane
//! reports it, and the RESP server behind the address `NodeGroup$PrimaryEndpoint` publishes
//! admits exactly the users in it — a withdrawn member's credential is refused `WRONGPASS`
//! while the other members keep theirs, and the IDENTICAL credential is accepted again once
//! the group holds the member once more.
//!
//! The rows span what the rule generalises over: WHICH resource the wait is about (a user
//! group being created, a replication group being created, a user group being attached to it,
//! a member being withdrawn, a member being restored), and WHICH form of credential the
//! member authenticates with (a password it holds, and a user that requires none) — because
//! the rule is about the group's MEMBERSHIP deciding access, whatever authenticates the
//! member. The recorded flow — one user with one generated password, withdrawn and re-added —
//! is one row of that class.
//!
//! The resources are placed the way the recorded flow places them: a cache subnet group over
//! three subnets of the account's own VPC and a security group Amazon EC2 minted, so the
//! endpoint the flow dials belongs to a cache inside that VPC. Every assertion goes over the
//! wire — the awsQuery router for the control plane, a TLS RESP session on the published
//! address for the data plane — and the transient half of each wait is asserted with the
//! record's due instant held out of reach, so what it asserts does not depend on how fast the
//! host runs. Nothing here sleeps.

use std::time::Duration;

use crate::cache_data_plane::{command, endpoint_of, open_tls, Resp};
use crate::harness::{
    assert_own_endpoint, every_pending_change_due_in, params, Emulator, Reply, HELD_OUT_OF_REACH,
};

/// The user group the flow revokes access through, and the replication group it is attached
/// to. The built-in `default` user stays in the group throughout: a user group "needs to
/// contain a user with the user name default".
const GROUP: &str = "revocation-group";
/// See [`GROUP`].
const REPLICATION_GROUP: &str = "revocation-rg";

/// One member of the group, as the rows of the table below span them: the case it names, the
/// `UserId` it is created with, and the credential its `AUTH` carries — which is the SAME
/// string before the withdrawal and after the restore, so what decides the session is the
/// membership rather than the secret.
type Member = (&'static str, &'static str, &'static str);

/// The members every row drives. The second needs no password, so the credential it sends is
/// not what lets it in: its membership is.
const MEMBERS: [Member; 2] = [
    (
        "a member with a password",
        "revocation-member",
        // The SHAPE of credential the recorded flow generates: a URL-safe string longer than
        // the minimum length the service states for a password, carrying the `-` and `_` such
        // a string is built from. A fixture of this suite, not a secret of anyone's.
        "a-url-safe_fixture-for-this-member",
    ),
    (
        "a member that needs no password",
        "revocation-open-member",
        "any-credential-at-all",
    ),
];

/// One wait of the flow: the call that starts the work, the transient status its OWN answer
/// has to carry, and the report a client looks at afterwards.
struct Wait {
    case: String,
    /// The call that starts the work, and the request it is made with.
    start: &'static str,
    start_request: Vec<(String, String)>,
    /// Where that call's answer carries the resource's status, and the transient value it
    /// must report.
    transient_path: &'static str,
    transient: &'static str,
    /// The report issued after it, the request it is made with, and where IT carries the
    /// status.
    report: &'static str,
    report_request: Vec<(String, String)>,
    report_status_path: &'static str,
}

/// Drive one wait: the starting call, the report issued inside the span, and the FIRST report
/// issued once the clock owes the change.
///
/// `settled` is what the client is waiting for, read off the report's own answer, and
/// `settled_description` says what that is.
async fn transient_then_settled_on_the_first_read(
    emulator: &Emulator,
    wait: &Wait,
    settled: impl Fn(&Reply) -> bool,
    settled_description: &str,
) {
    let started = emulator.call(wait.start, &wait.start_request).await;
    assert_eq!(
        started.status, 200,
        "case {:?}: {} must answer 200, got {} {}",
        wait.case, wait.start, started.status, started.body
    );
    assert_eq!(
        started.text(wait.transient_path),
        Some(wait.transient),
        "case {:?}: {} must answer with the transient status {:?}, got {:?} in {}",
        wait.case,
        wait.start,
        wait.transient,
        started.text(wait.transient_path),
        started.body
    );

    // Inside the span: the report answers from the record as it stands, which is what makes
    // the transient phase observable at all. The due instant is held out of reach first, so
    // this half of the row is about the rule and not about how fast the host runs.
    every_pending_change_due_in(emulator, HELD_OUT_OF_REACH);
    let inside = emulator.call(wait.report, &wait.report_request).await;
    assert_eq!(
        inside.status, 200,
        "case {:?}: a {} inside the span must answer 200, got {} {}",
        wait.case, wait.report, inside.status, inside.body
    );
    assert_eq!(
        inside.text(wait.report_status_path),
        Some(wait.transient),
        "case {:?}: a {} inside the span must report the record as it stands ({:?}), got {:?} \
         in {}",
        wait.case,
        wait.report,
        wait.transient,
        inside.text(wait.report_status_path),
        inside.body
    );
    assert!(
        !settled(&inside),
        "case {:?}: a {} inside the span must not report {settled_description} yet, got {}",
        wait.case,
        wait.report,
        inside.body
    );

    // And once the clock owes the change, the FIRST report that observes the record carries
    // it: ONE call, not a poll, because a client that looks again after the span must not
    // have to look a second time.
    every_pending_change_due_in(emulator, Duration::ZERO);
    let first = emulator.call(wait.report, &wait.report_request).await;
    assert_eq!(
        first.status, 200,
        "case {:?}: the first {} once the change is due must answer 200, got {} {}",
        wait.case, wait.report, first.status, first.body
    );
    assert!(
        settled(&first),
        "case {:?}: the FIRST {} issued once the change is due must report \
         {settled_description}, got {}",
        wait.case,
        wait.report,
        first.body
    );
}

/// The request `DescribeUserGroups` reads [`GROUP`] back with.
fn user_group_report() -> Vec<(String, String)> {
    params(&[("UserGroupId", GROUP)])
}

/// The request `DescribeReplicationGroups` reads [`REPLICATION_GROUP`] back with.
fn replication_group_report() -> Vec<(String, String)> {
    params(&[("ReplicationGroupId", REPLICATION_GROUP)])
}

/// Place the flow's network the way the recorded run places it: a cache subnet group over
/// three subnets of ONE VPC the account has, and a security group minted by the service that
/// owns security groups. Answers the security group id the cache is created with.
async fn placement(emulator: &Emulator) -> String {
    let subnets = emulator.account_subnets().await;
    let vpc = subnets
        .first()
        .expect("the account must report a subnet")
        .vpc
        .clone();
    let placed: Vec<_> = subnets
        .iter()
        .filter(|subnet| subnet.vpc == vpc)
        .take(3)
        .cloned()
        .collect();
    assert_eq!(
        placed.len(),
        3,
        "the recorded flow spans three subnets of one VPC; the account reported {subnets:?}"
    );
    let mut request = params(&[
        ("CacheSubnetGroupName", "revocation-subnets"),
        (
            "CacheSubnetGroupDescription",
            "the placement of the revocation flow",
        ),
    ]);
    for (index, subnet) in placed.iter().enumerate() {
        request.push((
            format!("SubnetIds.SubnetIdentifier.{}", index + 1),
            subnet.id.clone(),
        ));
    }
    let created = emulator.call("CreateCacheSubnetGroup", &request).await;
    assert_eq!(
        created.status, 200,
        "CreateCacheSubnetGroup must answer 200, got {} {}",
        created.status, created.body
    );
    emulator.security_group_named("revocation-cache").await
}

/// Create the two members of [`MEMBERS`], each with the credential form its row is about.
async fn create_members(emulator: &Emulator) {
    for (case, id, secret) in MEMBERS {
        let mut request = params(&[
            ("UserId", id),
            ("UserName", id),
            ("Engine", "redis"),
            ("AccessString", "on ~* +@all"),
        ]);
        if case == MEMBERS[0].0 {
            request.extend(params(&[("Passwords.member.1", secret)]));
        } else {
            request.extend(params(&[("NoPasswordRequired", "true")]));
        }
        let created = emulator.call("CreateUser", &request).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreateUser {id} must answer 200, got {} {}",
            created.status, created.body
        );
    }
}

#[tokio::test]
async fn every_wait_of_the_revocation_flow_is_answered_by_the_first_read_once_due() {
    let emulator = Emulator::new();
    let security_group = placement(&emulator).await;
    create_members(&emulator).await;

    // The user group the flow revokes through, holding the built-in `default` user and both
    // members.
    let mut group_request = params(&[("UserGroupId", GROUP), ("Engine", "redis")]);
    group_request.extend(params(&[("UserIds.member.1", "default")]));
    for (index, (_, id, _)) in MEMBERS.iter().enumerate() {
        group_request.push((format!("UserIds.member.{}", index + 2), (*id).to_string()));
    }
    transient_then_settled_on_the_first_read(
        &emulator,
        &Wait {
            case: "the user group being created".to_string(),
            start: "CreateUserGroup",
            start_request: group_request,
            transient_path: "Status",
            transient: "creating",
            report: "DescribeUserGroups",
            report_request: user_group_report(),
            report_status_path: "member/Status",
        },
        |reply| reply.text("member/Status") == Some("active"),
        "the group active",
    )
    .await;

    // The replication group the group is attached to, with in-transit encryption on — the
    // wire a client of an RBAC-governed group speaks — placed in the VPC above.
    transient_then_settled_on_the_first_read(
        &emulator,
        &Wait {
            case: "the replication group being created".to_string(),
            start: "CreateReplicationGroup",
            start_request: params(&[
                ("ReplicationGroupId", REPLICATION_GROUP),
                ("ReplicationGroupDescription", "the revocation flow's group"),
                ("Engine", "redis"),
                ("CacheNodeType", "cache.t3.micro"),
                ("NumCacheClusters", "1"),
                ("TransitEncryptionEnabled", "true"),
                ("CacheSubnetGroupName", "revocation-subnets"),
                ("SecurityGroupIds.SecurityGroupId.1", &security_group),
            ]),
            transient_path: "ReplicationGroup/Status",
            transient: "creating",
            report: "DescribeReplicationGroups",
            report_request: replication_group_report(),
            report_status_path: "ReplicationGroup/Status",
        },
        |reply| reply.text("ReplicationGroup/Status") == Some("available"),
        "the replication group available",
    )
    .await;

    transient_then_settled_on_the_first_read(
        &emulator,
        &Wait {
            case: "the user group being attached".to_string(),
            start: "ModifyReplicationGroup",
            start_request: params(&[
                ("ReplicationGroupId", REPLICATION_GROUP),
                ("UserGroupIdsToAdd.member.1", GROUP),
                ("ApplyImmediately", "true"),
            ]),
            transient_path: "ReplicationGroup/Status",
            transient: "modifying",
            report: "DescribeReplicationGroups",
            report_request: replication_group_report(),
            report_status_path: "ReplicationGroup/Status",
        },
        |reply| {
            reply.text("ReplicationGroup/Status") == Some("available")
                && reply.all("UserGroupIds/member").contains(&GROUP)
        },
        "the replication group available with the group attached",
    )
    .await;

    // The address the flow dials, read out of the control plane exactly as the client reads
    // it: an address of the emulator's OWN, which is the only kind a client can reach.
    let described = emulator
        .call("DescribeReplicationGroups", &replication_group_report())
        .await;
    let (address, port) = endpoint_of(
        &described,
        "NodeGroup/PrimaryEndpoint/Address",
        "NodeGroup/PrimaryEndpoint/Port",
        "the attached replication group",
    );
    assert_own_endpoint(
        "the attached replication group",
        "NodeGroups[0].PrimaryEndpoint",
        &address,
        Some(&port),
    );

    for (case, id, secret) in MEMBERS {
        // The positive control: the member authenticates and the session serves it.
        let mut session = open_tls(case, &address, &port).await;
        let accepted = command(case, &mut session, &["AUTH", id, secret]).await;
        assert_eq!(
            accepted,
            Resp::Status("OK".to_string()),
            "case {case:?}: a member of the attached group must authenticate, got {accepted:?}"
        );
        let pinged = command(case, &mut session, &["PING"]).await;
        assert_eq!(
            pinged,
            Resp::Status("PONG".to_string()),
            "case {case:?}: the authenticated session must answer PING, got {pinged:?}"
        );

        transient_then_settled_on_the_first_read(
            &emulator,
            &Wait {
                case: format!("{case} being withdrawn"),
                start: "ModifyUserGroup",
                start_request: params(&[
                    ("UserGroupId", GROUP),
                    ("UserIdsToRemove.member.1", id),
                ]),
                transient_path: "Status",
                transient: "modifying",
                report: "DescribeUserGroups",
                report_request: user_group_report(),
                report_status_path: "member/Status",
            },
            |reply| {
                reply.text("member/Status") == Some("active")
                    && !reply.all("UserIds/member").contains(&id)
            },
            "the group active without the withdrawn member",
        )
        .await;
        // A membership change of the user group is not a change to the replication group it
        // is attached to, so the group a client polls beside it is available throughout.
        let reported = emulator
            .call("DescribeReplicationGroups", &replication_group_report())
            .await;
        assert_eq!(
            reported.text("ReplicationGroup/Status"),
            Some("available"),
            "case {case:?}: the attached replication group must stay available across the \
             membership change, got {:?} in {}",
            reported.text("ReplicationGroup/Status"),
            reported.body
        );

        // The IDENTICAL credential, now that the group no longer holds the member.
        let mut refused_session = open_tls(case, &address, &port).await;
        let refused = command(case, &mut refused_session, &["AUTH", id, secret]).await;
        assert!(
            matches!(&refused, Resp::Error(message) if message.starts_with("WRONGPASS")),
            "case {case:?}: a user the attached group no longer holds must authenticate \
             nobody, got {refused:?}"
        );
        // The refusal is this member's own: the OTHER member of the group reaches the cache
        // with its own credential while this one is withdrawn.
        let (other_case, other_id, other_secret) = MEMBERS
            .iter()
            .find(|(_, other, _)| *other != id)
            .copied()
            .expect("the table holds a second member");
        let mut other = open_tls(other_case, &address, &port).await;
        let still_accepted = command(other_case, &mut other, &["AUTH", other_id, other_secret])
            .await;
        assert_eq!(
            still_accepted,
            Resp::Status("OK".to_string()),
            "case {case:?}: withdrawing one member must leave {other_id} its access, got \
             {still_accepted:?}"
        );

        transient_then_settled_on_the_first_read(
            &emulator,
            &Wait {
                case: format!("{case} being restored"),
                start: "ModifyUserGroup",
                start_request: params(&[("UserGroupId", GROUP), ("UserIdsToAdd.member.1", id)]),
                transient_path: "Status",
                transient: "modifying",
                report: "DescribeUserGroups",
                report_request: user_group_report(),
                report_status_path: "member/Status",
            },
            |reply| {
                reply.text("member/Status") == Some("active")
                    && reply.all("UserIds/member").contains(&id)
            },
            "the group active with the member restored",
        )
        .await;

        // The IDENTICAL credential once more, on a fresh session.
        let mut again = open_tls(case, &address, &port).await;
        let reaccepted = command(case, &mut again, &["AUTH", id, secret]).await;
        assert_eq!(
            reaccepted,
            Resp::Status("OK".to_string()),
            "case {case:?}: the identical credential must be accepted once the group holds \
             the member again, got {reaccepted:?}"
        );
        let pinged_again = command(case, &mut again, &["PING"]).await;
        assert_eq!(
            pinged_again,
            Resp::Status("PONG".to_string()),
            "case {case:?}: the restored session must answer PING, got {pinged_again:?}"
        );
    }
}
