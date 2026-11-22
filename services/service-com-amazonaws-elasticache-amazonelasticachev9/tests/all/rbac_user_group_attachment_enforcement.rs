// SPDX-License-Identifier: CC0-1.0

//! ATTACHING a user group to a replication group, DETACHING it and attaching it again —
//! what each of those calls answers, and what the cache's own socket makes of the
//! attachment they leave behind.
//!
//! The rule under test, over a SET of inputs rather than the one recorded run: the user
//! groups a replication group holds are the ONE access control the RESP server behind the
//! address `NodeGroup$PrimaryEndpoint` publishes judges a session by, and
//! `ModifyReplicationGroup` is what changes them — so while a group is attached only a user
//! of that group authenticates (a credential the user does not hold is refused, and an
//! accepted one serves the keyspace), once every group is detached the cache is the open
//! cache it was before (an unauthenticated session acts as the unrestricted default user and
//! the SAME credential now authenticates nobody), and attaching the identical group again
//! restores exactly the access it gave the first time. Each of those calls answers with the
//! transient status the service documents for that phase, a report arriving while the clock
//! still owes the settled one answers from the record as it stands, and the FIRST report
//! issued once that change is due carries it — nothing of this service sleeps for a span it
//! scheduled, and no client has to read twice.
//!
//! The rows span what the rule generalises over: WHICH modelled form the detachment takes
//! (`UserGroupIdsToRemove` naming the group, and `RemoveUserGroups` removing whatever is
//! attached — `smithy_query(operation="ModifyReplicationGroup")` lists both beside
//! `UserGroupIdsToAdd`), and WHICH form of credential the member authenticates with (one
//! password, one of SEVERAL passwords it holds, and a user that requires none) — because the
//! rule is about the ATTACHMENT deciding access, whatever the member authenticates with. The
//! recorded flow — one user with one generated password, attached, detached and re-attached —
//! is one row of that class.
//!
//! Every assertion goes over the wire: the `awsQuery` router for the control plane, and a TLS
//! RESP session on the published address for the data plane, because a cache with in-transit
//! encryption is the wire a client of an RBAC-governed group speaks. Nothing here sleeps, and
//! no assertion depends on how fast the host runs: the due instant of every outstanding change
//! is made due and the next call materialises it.

use std::time::Duration;

use crate::cache_data_plane::{command, endpoint_of, open_tls, Resp};
use crate::harness::{
    assert_own_endpoint, every_pending_change_due_in, params, Emulator, Reply, HELD_OUT_OF_REACH,
};

/// The wire code an engine answers an `AUTH` it did not accept with: the pair authenticates
/// nobody, and the reply says nothing about which half was wrong. (`NOAUTH` is the OTHER
/// refusal a client reads as an authentication having been refused — the one a session that
/// has not authenticated at all is answered on a cache that serves it nothing — so a reply
/// carrying neither code means the session was let in.)
const AUTH_REJECTED: &str = "WRONGPASS";

/// Assert that `reply` refused an authentication. `what` says which authentication it was.
fn assert_auth_refused(case: &str, what: &str, reply: &Resp) {
    assert_eq!(
        reply.error_code(),
        Some(AUTH_REJECTED),
        "case {case:?}: {what} must be refused {AUTH_REJECTED}, got {reply:?}"
    );
}

/// One member of the user group: the case it names, the `UserId` it is created with, the
/// passwords it holds, the credential its `AUTH` carries, and whether a credential it does
/// NOT hold is refused.
struct Member {
    case: &'static str,
    id: &'static str,
    /// The `Passwords` the user is created with; empty for a user created
    /// `NoPasswordRequired`.
    passwords: &'static [&'static str],
    /// What this member's `AUTH` sends — the SAME string before the detachment and after the
    /// re-attachment, so what decides the session is the attachment rather than the secret.
    credential: &'static str,
    /// Whether a credential this member does not hold authenticates nobody. A user that
    /// requires no password authenticates with any credential, so for that row there is no
    /// wrong one: its membership is all that admits it.
    wrong_credential_refused: bool,
}

impl Member {
    /// A credential this member does not hold.
    fn wrong_credential(&self) -> String {
        format!("not-{}", self.credential)
    }

    /// The key this member writes while the group is attached, and reads back.
    fn key(&self) -> String {
        format!("{}:key", self.id)
    }
}

/// The members every row drives. Each password is within the published length rule for a
/// user's password, and each is a fixture of this suite rather than anyone's secret.
const MEMBERS: [Member; 3] = [
    Member {
        case: "a member with one password",
        id: "attach-one-password",
        passwords: &["a-url-safe_fixture-for-one-member"],
        credential: "a-url-safe_fixture-for-one-member",
        wrong_credential_refused: true,
    },
    Member {
        case: "a member holding two passwords",
        id: "attach-two-passwords",
        passwords: &[
            "the-first-fixture-of-this-member",
            "the-second-fixture-of-this-member",
        ],
        // A user authenticates with ONE of the passwords it holds, so this row sends the
        // second of the two.
        credential: "the-second-fixture-of-this-member",
        wrong_credential_refused: true,
    },
    Member {
        case: "a member that requires no password",
        id: "attach-open-member",
        passwords: &[],
        credential: "any-credential-at-all",
        wrong_credential_refused: false,
    },
];

/// One row of the table: how the attached user group is DETACHED again, with the user group
/// and the replication group that row drives.
struct Detachment {
    case: &'static str,
    /// The `UserGroupId` this row attaches, detaches and attaches again.
    user_group: &'static str,
    /// The `ReplicationGroupId` it is attached to.
    replication_group: &'static str,
    /// The `ModifyReplicationGroup` parameters that detach it, beside `ReplicationGroupId`
    /// and `ApplyImmediately`.
    detach: &'static [(&'static str, &'static str)],
}

/// The two modelled ways a client takes a replication group's user groups away.
const DETACHMENTS: [Detachment; 2] = [
    Detachment {
        case: "detached by naming the group in UserGroupIdsToRemove",
        user_group: "attach-named-group",
        replication_group: "attach-named-rg",
        detach: &[("UserGroupIdsToRemove.member.1", "attach-named-group")],
    },
    Detachment {
        case: "detached by RemoveUserGroups",
        user_group: "attach-removed-group",
        replication_group: "attach-removed-rg",
        detach: &[("RemoveUserGroups", "true")],
    },
];

/// The request `DescribeReplicationGroups` reads one group back with.
fn replication_group_report(id: &str) -> Vec<(String, String)> {
    params(&[("ReplicationGroupId", id)])
}

/// Make `action` with `request`, assert the call itself answered 200 carrying `transient` as
/// the status of the work it started, then read the replication group back inside the span
/// the clock owes it, and ONCE more with that change made due.
///
/// Both halves of the wait: a report arriving before the change falls due answers from the
/// record as it stands, which is what makes the transient phase observable at all, and the
/// FIRST report issued once the change IS due has to carry both the settled status and the
/// attachment the call asked for — a client that looks again after the span must not have to
/// look a second time. The in-span half holds the due instant out of reach first, so what it
/// asserts does not depend on how fast the host runs. `attached` is the `UserGroupIds` the
/// settled group must report, in document order.
async fn start_then_first_report(
    emulator: &Emulator,
    case: &str,
    action: &str,
    request: &[(String, String)],
    transient: &str,
    replication_group: &str,
    attached: &[&str],
) -> Reply {
    let started = emulator.call(action, request).await;
    assert_eq!(
        started.status, 200,
        "case {case:?}: {action} must answer 200, got {} {}",
        started.status, started.body
    );
    assert_eq!(
        started.text("ReplicationGroup/Status"),
        Some(transient),
        "case {case:?}: {action} must answer with the transient status {transient:?}, got {:?} \
         in {}",
        started.text("ReplicationGroup/Status"),
        started.body
    );

    every_pending_change_due_in(emulator, HELD_OUT_OF_REACH);
    let inside = emulator
        .call(
            "DescribeReplicationGroups",
            &replication_group_report(replication_group),
        )
        .await;
    assert_eq!(
        inside.text("ReplicationGroup/Status"),
        Some(transient),
        "case {case:?}: a DescribeReplicationGroups inside the span of {action} must report the \
         record as it stands ({transient:?}), got {:?} in {}",
        inside.text("ReplicationGroup/Status"),
        inside.body
    );

    every_pending_change_due_in(emulator, Duration::ZERO);
    let reported = emulator
        .call(
            "DescribeReplicationGroups",
            &replication_group_report(replication_group),
        )
        .await;
    assert_eq!(
        reported.status, 200,
        "case {case:?}: the first DescribeReplicationGroups once the change is due must answer \
         200, got {} {}",
        reported.status, reported.body
    );
    assert_eq!(
        reported.text("ReplicationGroup/Status"),
        Some("available"),
        "case {case:?}: the FIRST report issued after {action} once the change is due must \
         report the group available, got {:?} in {}",
        reported.text("ReplicationGroup/Status"),
        reported.body
    );
    assert_eq!(
        reported.all("UserGroupIds/member"),
        attached.to_vec(),
        "case {case:?}: the FIRST report issued after {action} once the change is due must \
         report the user groups {attached:?}, got {:?} in {}",
        reported.all("UserGroupIds/member"),
        reported.body
    );
    reported
}

/// Place the flow's network the way the recorded run places it: a cache subnet group over two
/// subnets of ONE VPC the account has, and a security group minted by the service that owns
/// security groups. Answers the security group id every cache below is created with.
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
        .take(2)
        .cloned()
        .collect();
    assert_eq!(
        placed.len(),
        2,
        "the recorded flow spans two subnets of one VPC; the account reported {subnets:?}"
    );
    let mut request = params(&[
        ("CacheSubnetGroupName", "attach-subnets"),
        (
            "CacheSubnetGroupDescription",
            "the placement of the attachment flow",
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
    emulator.security_group_named("attach-flow-cache").await
}

/// Create every member of [`MEMBERS`], each with the credential form its row is about.
async fn create_members(emulator: &Emulator) {
    for member in &MEMBERS {
        let mut request = params(&[
            ("UserId", member.id),
            ("UserName", member.id),
            ("Engine", "redis"),
            ("AccessString", "on ~* &* +@all"),
        ]);
        if member.passwords.is_empty() {
            request.extend(params(&[("NoPasswordRequired", "true")]));
        } else {
            for (index, password) in member.passwords.iter().enumerate() {
                let member_password = format!("Passwords.member.{}", index + 1);
                request.push((member_password, (*password).to_string()));
            }
        }
        let created = emulator.call("CreateUser", &request).await;
        assert_eq!(
            created.status, 200,
            "case {:?}: CreateUser {} must answer 200, got {} {}",
            member.case, member.id, created.status, created.body
        );
    }
}

/// Create the user group `id` holding the built-in `default` user — a user group "needs to
/// contain a user with the user name default" — and every member of [`MEMBERS`], then read it
/// back ONCE with the change the clock owes it due: the first report has to carry it active.
async fn create_user_group(emulator: &Emulator, case: &str, id: &str) {
    let mut request = params(&[
        ("UserGroupId", id),
        ("Engine", "redis"),
        ("UserIds.member.1", "default"),
    ]);
    for (index, member) in MEMBERS.iter().enumerate() {
        request.push((
            format!("UserIds.member.{}", index + 2),
            member.id.to_string(),
        ));
    }
    let created = emulator.call("CreateUserGroup", &request).await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: CreateUserGroup {id} must answer 200, got {} {}",
        created.status, created.body
    );
    assert_eq!(
        created.text("Status"),
        Some("creating"),
        "case {case:?}: CreateUserGroup must answer with the transient status \"creating\", got \
         {:?} in {}",
        created.text("Status"),
        created.body
    );

    every_pending_change_due_in(emulator, Duration::ZERO);
    let reported = emulator
        .call("DescribeUserGroups", &params(&[("UserGroupId", id)]))
        .await;
    assert_eq!(
        reported.text("member/Status"),
        Some("active"),
        "case {case:?}: the FIRST DescribeUserGroups issued once the change is due must report \
         {id} active, got {:?} in {}",
        reported.text("member/Status"),
        reported.body
    );
}

#[tokio::test]
async fn attaching_and_detaching_a_user_group_decides_who_the_replication_group_serves() {
    let emulator = Emulator::new();
    let security_group = placement(&emulator).await;
    create_members(&emulator).await;

    for detachment in &DETACHMENTS {
        let case = detachment.case;
        let group = detachment.user_group;
        let replication_group = detachment.replication_group;
        create_user_group(&emulator, case, group).await;

        // The replication group, with in-transit encryption on — the wire a client of an
        // RBAC-governed group speaks — placed in the VPC above.
        start_then_first_report(
            &emulator,
            case,
            "CreateReplicationGroup",
            &params(&[
                ("ReplicationGroupId", replication_group),
                ("ReplicationGroupDescription", "the attachment flow's group"),
                ("Engine", "redis"),
                ("CacheNodeType", "cache.t3.micro"),
                ("NumCacheClusters", "1"),
                ("AutomaticFailoverEnabled", "false"),
                ("TransitEncryptionEnabled", "true"),
                ("CacheSubnetGroupName", "attach-subnets"),
                ("SecurityGroupIds.SecurityGroupId.1", &security_group),
            ]),
            "creating",
            replication_group,
            &[],
        )
        .await;

        // ATTACHED: the address the flow dials is read out of the report the attachment
        // settled on, exactly as a client reads it.
        let attached = start_then_first_report(
            &emulator,
            case,
            "ModifyReplicationGroup",
            &params(&[
                ("ReplicationGroupId", replication_group),
                ("UserGroupIdsToAdd.member.1", group),
                ("ApplyImmediately", "true"),
            ]),
            "modifying",
            replication_group,
            &[group],
        )
        .await;
        let (address, port) = endpoint_of(
            &attached,
            "NodeGroup/PrimaryEndpoint/Address",
            "NodeGroup/PrimaryEndpoint/Port",
            case,
        );
        assert_own_endpoint(
            case,
            "NodeGroups[0].PrimaryEndpoint",
            &address,
            Some(&port),
        );

        for member in &MEMBERS {
            let row = format!("{case}, {}", member.case);
            // A credential the member does not hold authenticates nobody, for a member that
            // holds one at all.
            if member.wrong_credential_refused {
                let mut session = open_tls(&row, &address, &port).await;
                let refused = command(
                    &row,
                    &mut session,
                    &["AUTH", member.id, &member.wrong_credential()],
                )
                .await;
                assert_auth_refused(&row, "a credential the member does not hold", &refused);
            }

            // The credential the member DOES hold, and the keyspace the session it opens
            // serves.
            let mut session = open_tls(&row, &address, &port).await;
            let accepted =
                command(&row, &mut session, &["AUTH", member.id, member.credential]).await;
            assert_eq!(
                accepted,
                Resp::Status("OK".to_string()),
                "case {row:?}: a member of the attached group must authenticate, got {accepted:?}"
            );
            let value = format!("{}-while-attached", member.id);
            let written = command(&row, &mut session, &["SET", &member.key(), &value]).await;
            assert_eq!(
                written,
                Resp::Status("OK".to_string()),
                "case {row:?}: the authenticated session must write {}, got {written:?}",
                member.key()
            );
            let read = command(&row, &mut session, &["GET", &member.key()]).await;
            assert_eq!(
                read,
                Resp::Bulk(value.clone()),
                "case {row:?}: the authenticated session must read {value:?} back, got {read:?}"
            );
        }

        // DETACHED, by this row's modelled form.
        let mut detach = params(&[
            ("ReplicationGroupId", replication_group),
            ("ApplyImmediately", "true"),
        ]);
        detach.extend(params(detachment.detach));
        start_then_first_report(
            &emulator,
            case,
            "ModifyReplicationGroup",
            &detach,
            "modifying",
            replication_group,
            &[],
        )
        .await;

        // With no user group attached the cache is the open cache it was: an unauthenticated
        // session acts as the unrestricted default user, and the keyspace it reaches is the
        // one the members wrote — the cache's own, not a second one.
        let mut open_session = open_tls(case, &address, &port).await;
        let open_key = format!("{replication_group}:default-access");
        let open_value = "written-with-no-group-attached";
        let written = command(case, &mut open_session, &["SET", &open_key, open_value]).await;
        assert_eq!(
            written,
            Resp::Status("OK".to_string()),
            "case {case:?}: an unauthenticated session must write once every user group is \
             detached, got {written:?}"
        );
        let read = command(case, &mut open_session, &["GET", &open_key]).await;
        assert_eq!(
            read,
            Resp::Bulk(open_value.to_string()),
            "case {case:?}: the unauthenticated session must read {open_value:?} back, got \
             {read:?}"
        );
        for member in &MEMBERS {
            let row = format!("{case}, {}", member.case);
            let kept = command(&row, &mut open_session, &["GET", &member.key()]).await;
            assert_eq!(
                kept,
                Resp::Bulk(format!("{}-while-attached", member.id)),
                "case {row:?}: the cache keeps what the member wrote through the detachment, \
                 got {kept:?}"
            );

            // The IDENTICAL credential, now that no group of this replication group holds the
            // member.
            let mut session = open_tls(&row, &address, &port).await;
            let refused =
                command(&row, &mut session, &["AUTH", member.id, member.credential]).await;
            assert_auth_refused(
                &row,
                "the credential of a member no attached group holds any more",
                &refused,
            );
        }

        // RE-ATTACHED: the identical group, and so the identical access.
        start_then_first_report(
            &emulator,
            case,
            "ModifyReplicationGroup",
            &params(&[
                ("ReplicationGroupId", replication_group),
                ("UserGroupIdsToAdd.member.1", group),
                ("ApplyImmediately", "true"),
            ]),
            "modifying",
            replication_group,
            &[group],
        )
        .await;

        for member in &MEMBERS {
            let row = format!("{case}, {}", member.case);
            let mut session = open_tls(&row, &address, &port).await;
            let reaccepted =
                command(&row, &mut session, &["AUTH", member.id, member.credential]).await;
            assert_eq!(
                reaccepted,
                Resp::Status("OK".to_string()),
                "case {row:?}: the identical credential must be accepted once the group is \
                 attached again, got {reaccepted:?}"
            );
            let restored = format!("{}-after-the-reattachment", member.id);
            let written = command(&row, &mut session, &["SET", &member.key(), &restored]).await;
            assert_eq!(
                written,
                Resp::Status("OK".to_string()),
                "case {row:?}: the re-admitted session must write {}, got {written:?}",
                member.key()
            );
            let read = command(&row, &mut session, &["GET", &member.key()]).await;
            assert_eq!(
                read,
                Resp::Bulk(restored.clone()),
                "case {row:?}: the re-admitted session must read {restored:?} back, got {read:?}"
            );
        }
    }
}
