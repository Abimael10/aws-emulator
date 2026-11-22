// SPDX-License-Identifier: CC0-1.0

//! The WORDING of the `NoSuchEntity` message, end to end over the awsQuery wire, for the
//! surfaces whose sentence is NOT the classic `The <entity> with name <name> cannot
//! be found.` the readers and the tag calls answer — and, for the surfaces that can word
//! their refusal two ways, WHICH of the two a request is answered with.
//!
//! The rules under test, each a TABLE whose rows differ along what the rule generalises
//! over (which operation of the surface is called, the form a name misses a held entity
//! by, the alphabet and case of that name, which of the two entities a request names is
//! missing, and how an identifier misses a held one):
//!
//! 1. the USER permissions-boundary surface — the put and the delete alike — words a name
//!    the account holds no user under as `User <name> was not found.`, the name taken from
//!    the request, so the two halves of that surface cannot describe the same missing user
//!    differently;
//! 2. the service-linked-role deletion-STATUS read words a task id the account holds
//!    nothing under with text that is wholly FIXED, interpolating no part of the id it was
//!    asked about, so every unheld id reads the same sentence;
//! 3. a removal naming both a user and a group resolves the USER first, so a request
//!    naming a user the account does not hold is refused naming THAT user — whether or not
//!    the group is held either — and only a request whose user IS held reports the group;
//! 4. a call addressing one of a USER's own MFA devices resolves that user before the
//!    serial, so a name the account holds no user under is refused naming THAT user
//!    whatever the serial names, and the device-scoped sentences are the answer only once
//!    the user is held.
//!
//! Every refusal here is a 404 whose awsQuery `Code` is `NoSuchEntity`
//! (`smithy_query(query="operation",
//! operation="PutUserPermissionsBoundary,DeleteUserPermissionsBoundary,GetServiceLinkedRoleDeletionStatus,RemoveUserFromGroup,DeactivateMFADevice,ResyncMFADevice")`
//! declares that error for each), and none of them writes anything, which the read-backs
//! at the end of each suite show.
//!
//! The awsQuery harness (`router`, `call`, `element`, `elements`, `error_code`) is the one
//! `tests/all/user_lifecycle.rs` owns and the other suites in this crate share; the
//! boundary fixtures are the ones `tests/all/user_permissions_boundary.rs` owns and the
//! device fixtures the ones `tests/all/virtual_mfa_device_enablement.rs` and
//! `tests/all/mfa_device_listing.rs` own.

use smithy_server_runtime::MultiServiceRouter;

use crate::mfa_device_listing::given_enabled_device;
use crate::user_lifecycle::{call, element, elements, error_code, router};
use crate::user_permissions_boundary::{create_user, put_boundary, AWS_MANAGED_POLICIES};
use crate::virtual_mfa_device_enablement::given_device;

/// Which half of the user permissions-boundary surface a row calls.
#[derive(Clone, Copy, Debug)]
enum Boundary {
    Put,
    Delete,
}

/// One call of the half `which` names, addressed at `user_name`.
async fn boundary_call(
    router: &MultiServiceRouter,
    which: Boundary,
    user_name: &str,
) -> (u16, String) {
    match which {
        Boundary::Put => put_boundary(router, user_name, AWS_MANAGED_POLICIES[0]).await,
        Boundary::Delete => {
            call(
                router,
                "DeleteUserPermissionsBoundary",
                &[("UserName", user_name)],
            )
            .await
        }
    }
}

/// Assert `body` is the `NoSuchEntity` refusal `expected` words, for the row `case` names.
fn assert_no_such_entity(case: &str, status: u16, body: &str, expected: &str) {
    assert_eq!(status, 404, "case {case}: expected 404, got {status}: {body}");
    assert_eq!(
        error_code(body),
        "NoSuchEntity",
        "case {case}: expected NoSuchEntity, got {:?}: {body}",
        error_code(body)
    );
    assert_eq!(
        element(body, "Message"),
        expected,
        "case {case}: expected {expected:?}, got {:?}",
        element(body, "Message")
    );
}

/// RULE 1: both halves of the user permissions-boundary surface word a name the account
/// holds no user under as `User <name> was not found.`
#[tokio::test]
async fn the_user_boundary_surface_words_an_absent_user_the_same_way() {
    let router = router();
    create_user(&router, "HeldUser", "/").await;
    create_user(&router, "nested_user+1", "/eng/tools/").await;

    // Rows spanning the forms a name can miss every held user by: never created, a near
    // miss of a held name, a held name with a separator the folded name does not carry,
    // the whole `^[\w+=,.@-]+$` alphabet the name member admits, and the shortest name
    // `userNameType` allows (len 1..64).
    let absent = [
        "NoSuchUser",
        "HeldUser2",
        "held_user",
        "another+name=entirely.x@-",
        "Z",
    ];
    for which in [Boundary::Put, Boundary::Delete] {
        for user_name in absent {
            let case = format!("{which:?} {user_name:?}");
            let (status, body) = boundary_call(&router, which, user_name).await;
            assert_no_such_entity(
                &case,
                status,
                &body,
                &format!("User {user_name} was not found."),
            );
        }
    }

    // Nothing the refusals touched was written: both held users still read back, carrying
    // no boundary at all.
    for user_name in ["HeldUser", "nested_user+1"] {
        let (status, body) = call(&router, "GetUser", &[("UserName", user_name)]).await;
        assert_eq!(
            status, 200,
            "case {user_name:?}: the held user must survive the refusals, got {status}: {body}"
        );
        assert!(
            elements(&body, "PermissionsBoundaryArn").is_empty(),
            "case {user_name:?}: a refused put must persist no boundary, got {body}"
        );
    }
}

/// RULE 1 (the other direction): once the user IS held, the same two calls are served, so
/// the sentence above is the answer to the user's ABSENCE and not to the operation itself.
#[tokio::test]
async fn the_user_boundary_surface_serves_a_user_the_account_holds() {
    let router = router();
    for (user_name, path) in [("BoundedUser", "/"), ("bounded_2@x", "/eng/")] {
        create_user(&router, user_name, path).await;
        let (status, body) = put_boundary(&router, user_name, AWS_MANAGED_POLICIES[1]).await;
        assert_eq!(
            status, 200,
            "case {user_name:?}: the put must be served, got {status}: {body}"
        );
        let (status, body) = call(
            &router,
            "DeleteUserPermissionsBoundary",
            &[("UserName", user_name)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {user_name:?}: the delete must be served, got {status}: {body}"
        );
    }
}

/// RULE 2: the deletion-status read words an id the account holds no task under with text
/// that interpolates nothing of that id.
#[tokio::test]
async fn an_unknown_deletion_task_id_is_worded_without_the_id() {
    let router = router();

    // One task the account DOES hold, so a row can miss a held id by its uuid alone.
    let (status, body) = call(
        &router,
        "CreateServiceLinkedRole",
        &[("AWSServiceName", "autoscaling.amazonaws.com")],
    )
    .await;
    assert_eq!(
        status, 200,
        "the fixture CreateServiceLinkedRole must succeed, got {status}: {body}"
    );
    let role_name = element(&body, "RoleName");
    let (status, body) = call(
        &router,
        "DeleteServiceLinkedRole",
        &[("RoleName", role_name.as_str())],
    )
    .await;
    assert_eq!(
        status, 200,
        "the fixture DeleteServiceLinkedRole must succeed, got {status}: {body}"
    );
    let held = element(&body, "DeletionTaskId");
    let near_miss = {
        let (head, _) = held
            .rsplit_once('/')
            .expect("a DeletionTaskId carries a task uuid");
        format!("{head}/99999999-8888-7777-6666-555555555555")
    };

    // The whole sentence IAM answers: fixed text, naming neither the task nor the role or
    // the service the id spells (`crate::state::deletion_task_not_found`).
    let expected = "Cannot find deletion status for given id.";

    // Rows: well-formed ids — "task/aws-service-role/<service-principal-name>/<role-name>/
    // <task-uuid>", the format `DeleteServiceLinkedRole$DeletionTaskId` documents — for a
    // two-label and a five-label service principal, for a role name the account never
    // held, for a role name differing from the held one in case, and the near miss of the
    // held id.
    let unheld = [
        "task/aws-service-role/svc.amazonaws.com/AWSServiceRoleForSvc/11111111-2222-3333-4444-555555555555".to_string(),
        "task/aws-service-role/a.b.c.d.amazonaws.com/AWSServiceRoleForABCD/00000000-0000-0000-0000-000000000000".to_string(),
        format!("task/aws-service-role/autoscaling.amazonaws.com/{}/abcdefab-cdef-abcd-efab-cdefabcdefab", role_name.to_ascii_uppercase()),
        near_miss,
    ];
    for deletion_task_id in &unheld {
        let (status, body) = call(
            &router,
            "GetServiceLinkedRoleDeletionStatus",
            &[("DeletionTaskId", deletion_task_id.as_str())],
        )
        .await;
        assert_no_such_entity(&format!("{deletion_task_id:?}"), status, &body, expected);
        // The sentence carries no part of the id the request supplied, so two unheld ids
        // cannot be told apart by the message.
        let message = element(&body, "Message");
        for part in deletion_task_id.split('/').filter(|part| part.len() > 4) {
            assert!(
                !message.contains(part),
                "case {deletion_task_id:?}: the refusal must name no part of the id, got \
                 {message:?} carrying {part:?}"
            );
        }
    }

    // The task the account does hold is still reported, so no refused read disturbed it.
    let (status, body) = call(
        &router,
        "GetServiceLinkedRoleDeletionStatus",
        &[("DeletionTaskId", held.as_str())],
    )
    .await;
    assert_eq!(
        status, 200,
        "the held task must still be reported, got {status}: {body}"
    );
    assert!(
        !element(&body, "Status").is_empty(),
        "the held task must report a status, got {body}"
    );
}

/// RULE 3: a removal resolves the user before the group, so the refusal names the user
/// whenever the user is the entity the account does not hold.
#[tokio::test]
async fn removing_a_user_from_a_group_reports_an_absent_user_before_the_group() {
    let router = router();
    let held_group = "HeldGroup";
    let held_user = "held.user+1";
    let (status, body) = call(&router, "CreateGroup", &[("GroupName", held_group)]).await;
    assert_eq!(
        status, 200,
        "the fixture CreateGroup must succeed, got {status}: {body}"
    );
    create_user(&router, held_user, "/eng/").await;
    let (status, body) = call(
        &router,
        "AddUserToGroup",
        &[("GroupName", held_group), ("UserName", held_user)],
    )
    .await;
    assert_eq!(
        status, 200,
        "the fixture AddUserToGroup must succeed, got {status}: {body}"
    );

    // Rows: (group the request names, user it names, which entity the refusal must name).
    // They span a held group with an absent user, an absent group with an absent user —
    // where the two possible answers differ, and the user is the one IAM reports — and an
    // absent group with a held user, over names missing by a near miss, by a separator the
    // folded held name does not carry, and across the `^[\w+=,.@-]+$` alphabet in both
    // cases.
    let rows: &[(&str, &str, &str)] = &[
        (held_group, "NoSuchUser", "user"),
        (held_group, "held_user+1", "user"),
        (held_group, "HELD.USER+2", "user"),
        ("NoSuchGroup", "NoSuchUser", "user"),
        ("ghost_7", "void.x@-", "user"),
        ("HeldGroup2", "another=name", "user"),
        ("NoSuchGroup", held_user, "group"),
        ("HeldGroup2", "HELD.USER+1", "group"),
        ("ghost_7", "held.user+1", "group"),
    ];
    for (group_name, user_name, names) in rows {
        let case = format!("group {group_name:?} user {user_name:?}");
        let (status, body) = call(
            &router,
            "RemoveUserFromGroup",
            &[("GroupName", group_name), ("UserName", user_name)],
        )
        .await;
        let expected = match *names {
            "user" => format!("The user with name {user_name} cannot be found."),
            _ => format!("The group with name {group_name} cannot be found."),
        };
        assert_no_such_entity(&case, status, &body, &expected);
    }

    // Nothing the refusals touched was written: the fixture membership still stands.
    let (status, body) = call(&router, "GetGroup", &[("GroupName", held_group)]).await;
    assert_eq!(
        status, 200,
        "the held group must survive the refusals, got {status}: {body}"
    );
    assert_eq!(
        elements(&body, "UserName"),
        vec![held_user.to_string()],
        "the fixture membership must stand after the refused removals, got {body}"
    );

    // And the removal the account CAN serve is still served, so the refusals above are
    // about the absent entity rather than the operation.
    let (status, body) = call(
        &router,
        "RemoveUserFromGroup",
        &[("GroupName", held_group), ("UserName", held_user)],
    )
    .await;
    assert_eq!(
        status, 200,
        "the held membership must be removable, got {status}: {body}"
    );
    let (status, body) = call(&router, "GetGroup", &[("GroupName", held_group)]).await;
    assert_eq!(
        status, 200,
        "the group must still be reported after the removal, got {status}: {body}"
    );
    assert!(
        elements(&body, "UserName").is_empty(),
        "the removal must leave the group holding nobody, got {body}"
    );
}

/// WHICH of the calls addressing one of a user's own MFA devices by its serial a row makes.
#[derive(Clone, Copy, Debug)]
enum DeviceCall {
    Deactivate,
    Resync,
}

/// A pair of six-digit codes, the form `AuthenticationCode1` takes ("a string of six
/// digits"), for the rows whose refusal is decided before any code is authenticated.
const CODES: (&str, &str) = ("123456", "234567");

impl DeviceCall {
    /// One call of this kind, naming `user_name` and addressing `serial_number`.
    async fn send(
        self,
        router: &MultiServiceRouter,
        user_name: &str,
        serial_number: &str,
    ) -> (u16, String) {
        match self {
            Self::Deactivate => {
                call(
                    router,
                    "DeactivateMFADevice",
                    &[("UserName", user_name), ("SerialNumber", serial_number)],
                )
                .await
            }
            Self::Resync => {
                call(
                    router,
                    "ResyncMFADevice",
                    &[
                        ("UserName", user_name),
                        ("SerialNumber", serial_number),
                        ("AuthenticationCode1", CODES.0),
                        ("AuthenticationCode2", CODES.1),
                    ],
                )
                .await
            }
        }
    }
}

/// RULE 4: a call addressing one of a user's own MFA devices resolves the USER before the
/// serial, so a name the account holds no user under is refused naming THAT user whatever
/// the serial names — and the two device-scoped sentences are the answer only once the user
/// is held.
#[tokio::test]
async fn an_mfa_device_call_reports_an_absent_user_before_the_device() {
    let case = "mfa device user resolution";
    let held_user = "device.holder+1";
    let router = router();
    create_user(&router, held_user, "/eng/").await;

    // The three kinds of serial a row can address: the device the held user holds, a device
    // the account holds that nobody holds, and two serials naming no device at all (an ARN
    // the account never minted and a bare serial, both inside `serialNumberType`'s
    // `^[\w+=/:,.@-]+$` and its 9..256 length).
    let enrolled = given_enabled_device(&router, case, held_user, "held-phone").await;
    let (unassigned, _) = given_device(&router, "free-phone").await;
    let absent_arn = format!("{enrolled}-absent");
    let bare_serial = "GAHT12345678";

    // Rows spanning the forms a name can miss every held user by: never created, a near miss
    // of the held name, the held name with a separator the folded name does not carry, the
    // whole `^[\w+=,.@-]+$` alphabet `existingUserNameType` admits, and the shortest name it
    // allows (len 1..128).
    let absent_users = [
        "NoSuchUser",
        "device.holder+2",
        "device_holder_1",
        "another+name=entirely.x@-",
        "Z",
    ];
    for which in [DeviceCall::Deactivate, DeviceCall::Resync] {
        for user_name in absent_users {
            for serial_number in [
                enrolled.as_str(),
                unassigned.as_str(),
                absent_arn.as_str(),
                bare_serial,
            ] {
                let row = format!("{which:?} user {user_name:?} serial {serial_number:?}");
                let (status, body) = which.send(&router, user_name, serial_number).await;
                assert_no_such_entity(
                    &row,
                    status,
                    &body,
                    &format!("The user with name {user_name} cannot be found."),
                );
            }
        }

        // The user IS held: now the refusal is about the DEVICE, in the sentence of its own
        // class — the serial naming no device of the account at all, and the device the
        // account holds but this user has not enrolled, which the two calls word
        // differently: the read of a device names the device and the user, while
        // `DeactivateMFADevice` addresses the ENROLMENT and words its absence with its own
        // fixed sentence. Both are written out as the wire text they are.
        let not_this_users = match which {
            DeviceCall::Deactivate => "MFA Device invalid for user.".to_string(),
            DeviceCall::Resync => {
                format!("MFA Device {unassigned} cannot be found for user {held_user}.")
            }
        };
        for (serial_number, expected) in [
            (
                absent_arn.as_str(),
                format!("MFA Device with serial number {absent_arn} does not exist."),
            ),
            (
                bare_serial,
                format!("MFA Device with serial number {bare_serial} does not exist."),
            ),
            (unassigned.as_str(), not_this_users),
        ] {
            let row = format!("{which:?} held user serial {serial_number:?}");
            let (status, body) = which.send(&router, held_user, serial_number).await;
            assert_no_such_entity(&row, status, &body, &expected);
        }
    }

    // None of the refusals touched anything: the held user still holds the device it was
    // enrolled with, and the unassigned device is still the account's.
    let (status, body) = call(&router, "ListMFADevices", &[("UserName", held_user)]).await;
    assert_eq!(
        status, 200,
        "the held user's devices must be listable, got {status}: {body}"
    );
    assert_eq!(
        elements(&body, "SerialNumber"),
        vec![enrolled.clone()],
        "the refusals must leave the enrolment alone, got {:?}",
        elements(&body, "SerialNumber")
    );

    // And the deactivation the account CAN serve is served, so the sentences above are
    // about the absent entity rather than the operation.
    let (status, body) = DeviceCall::Deactivate
        .send(&router, held_user, &enrolled)
        .await;
    assert_eq!(
        status, 200,
        "the held user's own device must be deactivatable, got {status}: {body}"
    );
    let (status, body) = call(&router, "ListMFADevices", &[("UserName", held_user)]).await;
    assert_eq!(
        status, 200,
        "the held user's devices must be listable, got {status}: {body}"
    );
    assert!(
        elements(&body, "SerialNumber").is_empty(),
        "the deactivation must leave the user holding no device, got {body}"
    );

    // Once the user is deleted, the same call that was served reports the USER rather than
    // the device it used to hold — the divergence this rule is about.
    let (status, body) = call(&router, "DeleteUser", &[("UserName", held_user)]).await;
    assert_eq!(
        status, 200,
        "the fixture DeleteUser must succeed, got {status}: {body}"
    );
    for which in [DeviceCall::Deactivate, DeviceCall::Resync] {
        for serial_number in [enrolled.as_str(), unassigned.as_str(), absent_arn.as_str()] {
            let row = format!("{which:?} deleted user serial {serial_number:?}");
            let (status, body) = which.send(&router, held_user, serial_number).await;
            assert_no_such_entity(
                &row,
                status,
                &body,
                &format!("The user with name {held_user} cannot be found."),
            );
        }
    }
}
