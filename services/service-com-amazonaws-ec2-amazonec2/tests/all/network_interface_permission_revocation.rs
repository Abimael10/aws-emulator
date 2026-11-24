// SPDX-License-Identifier: CC0-1.0

//! What `DeleteNetworkInterfacePermission` does to a grant, end to end over the wire the
//! service actually speaks: `ec2Query`, an `application/x-www-form-urlencoded` POST whose body
//! carries `Action=DeleteNetworkInterfacePermission`, answered with XML (README.md /
//! OPERATIONS.md "Wire protocol").
//!
//! The rules under test:
//!
//! 1. a revoke REMOVES the grant it names and answers `return` true: the permission
//!    `CreateNetworkInterfacePermission` recorded is gone from the store, so
//!    `DescribeNetworkInterfacePermissions` refuses that id afterwards, while every other grant
//!    the account holds is left exactly as it was;
//! 2. a grant removed can be granted AGAIN — the create mints a record of its own for it — so
//!    the removal is of the record rather than of the account's standing to share the
//!    interface;
//! 3. an id that is not a permission id at all is refused before the store is read
//!    (`InvalidNetworkInterfacePermissionID.Malformed`), one the account holds no permission for
//!    is `InvalidNetworkInterfacePermissionID.NotFound` — a second revoke of the same grant
//!    included — and neither removes anything;
//! 4. `DryRun` answers the permission question with `DryRunOperation` and removes nothing.
//!
//! Each table row is a distinct input along what those rules generalise over: which published
//! permission type the grant carried, which account it was to, which interface it was over,
//! whether the revoke forced itself past an attachment, and which id the request names and in
//! what way it does not resolve.

use ec2::ops::types::INTERFACE_PERMISSION_TYPE_VALUES;
use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, DRY_RUN_OPERATION,
    INVALID_NETWORK_INTERFACE_PERMISSION_ID_MALFORMED,
    INVALID_NETWORK_INTERFACE_PERMISSION_ID_NOT_FOUND, NETWORK_INTERFACE_PERMISSION_ID_PREFIX,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements, error_code, error_message, indexed};
use super::wire::{Param, param};

/// The Region every call here is signed for: the account's default VPC and its default subnets
/// are seeded for the emulator's default Region, and the interfaces these grants are over are
/// created in one of them.
const REGION: &str = "us-east-1";

/// The action under test.
const REVOKE: &str = "DeleteNetworkInterfacePermission";

/// The action that records the grants it revokes.
const GRANT: &str = "CreateNetworkInterfacePermission";

/// The action that reads them back.
const DESCRIBE: &str = "DescribeNetworkInterfacePermissions";

/// The HTTP status EC2 answers `DryRunOperation` with.
const DRY_RUN_STATUS: u16 = 412;

/// A well-formed `NetworkInterfacePermissionId` the account holds no permission for, spelled
/// from the top of the hexadecimal range: this store mints ids from a monotonic counter, so an
/// id this high cannot collide with one a created grant was given.
const UNHELD_PERMISSION_ID: &str = "eni-perm-0ffffffffffffffff";

fn emulator() -> (MultiServiceRouter, std::sync::Arc<engine::InMemoryEngine>) {
    super::ec2_query::emulator()
}

/// One call of `action` signed for [`REGION`].
async fn invoke(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// One call of `action` requiring success, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = invoke(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// The first default subnet of the account's default VPC — a subnet the store really holds, so
/// an interface created into it is one the service itself minted.
fn default_subnet(engine: &engine::InMemoryEngine) -> String {
    let state = engine.state::<Ec2State>();
    let store = state.lock();
    let vpc_id = store
        .default_vpc_id(REGION)
        .expect("a fresh account has its default VPC");
    store
        .subnets
        .iter()
        .find(|(_, subnet)| subnet.vpc_id.as_deref() == Some(vpc_id.as_str()))
        .map(|(subnet_id, _)| subnet_id.clone())
        .expect("the default VPC comes with its default subnets")
}

/// Create one network interface through the API and answer the id it was given.
async fn create_interface(
    router: &MultiServiceRouter,
    subnet_id: &str,
    description: &str,
) -> String {
    let body = ok(
        description,
        router,
        "CreateNetworkInterface",
        &[param("SubnetId", subnet_id), param("Description", description)],
    )
    .await;
    element(&body, "networkInterfaceId")
        .unwrap_or_else(|| panic!("CreateNetworkInterface answered no id: {body}"))
}

/// Grant `permission` over `interface_id` to `grantee` and answer the permission id recorded.
async fn grant(
    router: &MultiServiceRouter,
    case: &str,
    interface_id: &str,
    grantee: &str,
    permission: &str,
) -> String {
    let body = ok(
        case,
        router,
        GRANT,
        &[
            param("NetworkInterfaceId", interface_id),
            param("AwsAccountId", grantee),
            param("Permission", permission),
        ],
    )
    .await;
    let permission_id = element(&body, "networkInterfacePermissionId")
        .unwrap_or_else(|| panic!("case {case:?}: the grant answered no id: {body}"));
    assert!(
        permission_id.starts_with(&format!("{NETWORK_INTERFACE_PERMISSION_ID_PREFIX}-")),
        "case {case:?}: expected a {NETWORK_INTERFACE_PERMISSION_ID_PREFIX}- id, got \
         {permission_id:?}"
    );
    permission_id
}

/// The permission ids the describe reports for the whole account, in id order.
async fn described_ids(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let body = ok(case, router, DESCRIBE, &[]).await;
    let mut ids = elements(&body, "networkInterfacePermissionId");
    ids.sort();
    ids
}

/// The store's permissions as a string, for the "nothing was removed" assertions.
fn snapshot(engine: &engine::InMemoryEngine) -> String {
    let state = engine.state::<Ec2State>();
    let store = state.lock();
    format!("{:?}", store.network_interface_permissions)
}

/// What one row of the revoke table holds: a grant of its own, and how the revoke is made.
struct RevokeCase {
    case: &'static str,
    /// Which of the seeded interfaces the grant is over.
    interface: usize,
    /// The account the grant was to.
    grantee: &'static str,
    /// One member of the `InterfacePermissionType` enum.
    permission: &'static str,
    /// Whether the revoke carries "Specify true to remove the permission even if the network
    /// interface is attached to an instance" (`DeleteNetworkInterfacePermissionRequest$Force`).
    force: Option<bool>,
}

// ---------------------------------------------------------------------------
// Rules 1 and 2: the revoke removes the grant, and the grant can be made again
// ---------------------------------------------------------------------------

/// Every grant the account holds is revoked the same way: `return` true, the id gone from the
/// describe, every other grant untouched — and the interface can be granted again afterwards.
#[tokio::test]
async fn a_revoke_removes_the_grant_it_names_and_leaves_every_other_one() {
    let (router, engine) = emulator();
    let subnet_id = default_subnet(&engine);
    let interfaces = [
        create_interface(&router, &subnet_id, "shared once").await,
        create_interface(&router, &subnet_id, "shared twice").await,
    ];

    // The rows differ along everything the rule generalises over: the permission type, the
    // grantee account, the interface the grant is over, and whether the revoke forces itself
    // past an attachment.
    let rows = [
        RevokeCase {
            case: "the first published type, to another account, not forced",
            interface: 0,
            grantee: "111122223333",
            permission: INTERFACE_PERMISSION_TYPE_VALUES[0],
            force: None,
        },
        RevokeCase {
            case: "the second published type, to another account, forced",
            interface: 0,
            grantee: "111122223333",
            permission: INTERFACE_PERMISSION_TYPE_VALUES[1],
            force: Some(true),
        },
        RevokeCase {
            case: "the first published type over the second interface, force false",
            interface: 1,
            grantee: "444455556666",
            permission: INTERFACE_PERMISSION_TYPE_VALUES[0],
            force: Some(false),
        },
    ];

    // Every grant is recorded first, so each revoke is asserted against a store that holds
    // more than the grant it removes.
    let mut held: Vec<String> = Vec::new();
    for row in &rows {
        held.push(
            grant(
                &router,
                row.case,
                &interfaces[row.interface],
                row.grantee,
                row.permission,
            )
            .await,
        );
    }
    let mut expected = held.clone();
    expected.sort();
    assert_eq!(
        described_ids(&router, "before any revoke").await,
        expected,
        "the describe must report every grant the creates recorded"
    );

    for (index, row) in rows.iter().enumerate() {
        let permission_id = held[index].clone();
        let mut params = vec![param("NetworkInterfacePermissionId", &permission_id)];
        if let Some(force) = row.force {
            params.push(param("Force", if force { "true" } else { "false" }));
        }
        let body = ok(row.case, &router, REVOKE, &params).await;
        assert_eq!(
            element(&body, "return").as_deref(),
            Some("true"),
            "case {case:?}: expected return true, got {body}",
            case = row.case,
        );

        // The round trip: the id is gone, so the describe refuses it — and every grant that
        // was not named is still reported.
        let (status, refused) = invoke(
            &router,
            DESCRIBE,
            &indexed("NetworkInterfacePermissionId", &[&permission_id]),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the describe to refuse the revoked id, got {status} with \
             body {refused}",
            case = row.case,
        );
        assert_eq!(
            error_code(&refused),
            INVALID_NETWORK_INTERFACE_PERMISSION_ID_NOT_FOUND,
            "case {case:?}: expected {INVALID_NETWORK_INTERFACE_PERMISSION_ID_NOT_FOUND}, got \
             {refused}",
            case = row.case,
        );
        let mut still_held: Vec<String> = held[index + 1..].to_vec();
        still_held.sort();
        assert_eq!(
            described_ids(&router, row.case).await,
            still_held,
            "case {case:?}: the revoke must leave every other grant exactly as it was",
            case = row.case,
        );
    }

    // Rule 2: the record went, not the account's standing to share — so the same grant can be
    // made again, as a record of its own.
    for row in &rows {
        let again = grant(
            &router,
            row.case,
            &interfaces[row.interface],
            row.grantee,
            row.permission,
        )
        .await;
        assert!(
            !held.contains(&again),
            "case {case:?}: a grant made after the revoke is a record of its own, got \
             {again:?}",
            case = row.case,
        );
    }
}

// ---------------------------------------------------------------------------
// Rules 3 and 4: what the revoke refuses, and that a refusal removes nothing
// ---------------------------------------------------------------------------

/// An id the request cannot name, and a dry run, each answer without removing anything: the
/// rows differ in how the id fails to resolve — not an id of this kind in three ways, a
/// well-formed id the account holds no permission for, and the id of a grant a revoke has
/// already removed — and in whether the dry run would have succeeded.
#[tokio::test]
async fn a_revoke_that_cannot_proceed_answers_and_removes_nothing() {
    let (router, engine) = emulator();
    let subnet_id = default_subnet(&engine);
    let interface_id = create_interface(&router, &subnet_id, "not revoked").await;
    let held = grant(
        &router,
        "the grant the refusals are asserted against",
        &interface_id,
        "111122223333",
        INTERFACE_PERMISSION_TYPE_VALUES[0],
    )
    .await;
    // A grant that IS revoked, so its id is one the account held a permission for and does
    // not any more.
    let revoked = grant(
        &router,
        "the grant revoked first",
        &interface_id,
        "111122223333",
        INTERFACE_PERMISSION_TYPE_VALUES[1],
    )
    .await;
    let _ = ok(
        "the first revoke",
        &router,
        REVOKE,
        &[param("NetworkInterfacePermissionId", &revoked)],
    )
    .await;

    // (case, the parameters, the status, the code)
    let rows: Vec<(&str, Vec<Param>, u16, &str)> = vec![
        (
            "something that is not a permission id at all",
            vec![param("NetworkInterfacePermissionId", "not-a-permission")],
            400,
            INVALID_NETWORK_INTERFACE_PERMISSION_ID_MALFORMED,
        ),
        (
            "another resource kind's id",
            vec![param(
                "NetworkInterfacePermissionId",
                "eni-0123456789abcdef0",
            )],
            400,
            INVALID_NETWORK_INTERFACE_PERMISSION_ID_MALFORMED,
        ),
        (
            "the prefix alone",
            vec![param("NetworkInterfacePermissionId", "eni-perm-")],
            400,
            INVALID_NETWORK_INTERFACE_PERMISSION_ID_MALFORMED,
        ),
        (
            "a well-formed id the account holds no permission for",
            vec![param("NetworkInterfacePermissionId", UNHELD_PERMISSION_ID)],
            400,
            INVALID_NETWORK_INTERFACE_PERMISSION_ID_NOT_FOUND,
        ),
        (
            "a second revoke of a grant already removed",
            vec![param("NetworkInterfacePermissionId", &revoked)],
            400,
            INVALID_NETWORK_INTERFACE_PERMISSION_ID_NOT_FOUND,
        ),
        (
            "a dry run of a revoke that would have succeeded",
            vec![
                param("NetworkInterfacePermissionId", &held),
                param("DryRun", "true"),
            ],
            DRY_RUN_STATUS,
            DRY_RUN_OPERATION,
        ),
        (
            "a dry run of a revoke that names no permission",
            vec![
                param("NetworkInterfacePermissionId", UNHELD_PERMISSION_ID),
                param("DryRun", "true"),
            ],
            400,
            INVALID_NETWORK_INTERFACE_PERMISSION_ID_NOT_FOUND,
        ),
    ];

    let before = snapshot(&engine);
    for (case, params, expected_status, expected_code) in rows {
        let (status, body) = invoke(&router, REVOKE, &params).await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected HTTP {expected_status}, got {status} with body {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, expected_code,
            "case {case:?}: expected {expected_code}, got {code} with message {:?}",
            error_message(&body)
        );
        assert!(
            !error_message(&body).is_empty(),
            "case {case:?}: a refusal has to say what was refused, got {body}"
        );
        assert_eq!(
            snapshot(&engine),
            before,
            "case {case:?}: a revoke that cannot proceed must remove nothing"
        );
    }

    assert_eq!(
        described_ids(&router, "after every refusal").await,
        vec![held.clone()],
        "the refusals must have left the grant the account holds exactly as it was"
    );
}
