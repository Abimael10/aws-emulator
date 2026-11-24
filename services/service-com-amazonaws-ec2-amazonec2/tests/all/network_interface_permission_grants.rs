// SPDX-License-Identifier: CC0-1.0

//! What `CreateNetworkInterfacePermission` records, end to end over the wire the service
//! actually speaks: `ec2Query`, an `application/x-www-form-urlencoded` POST whose body
//! carries `Action=CreateNetworkInterfacePermission`, answered with XML (README.md /
//! OPERATIONS.md "Wire protocol"). ec2Query is selected by the `ec2` signing name in the
//! request's SigV4 credential scope, so every call here carries one.
//!
//! The rules under test:
//!
//! 1. a grant is RECORDED: the call writes one `NetworkInterfacePermission` over an interface
//!    the account holds and answers it as stored, so `DescribeNetworkInterfacePermissions`
//!    reports the very record the create minted — whichever published permission type and
//!    whichever interface the request named;
//! 2. a recorded grant is IN FORCE: it is answered, and read back, in the `GRANTED` state,
//!    with no clock-owed move to wait for — a caller that reads straight after the write sees
//!    the grant it was just given;
//! 3. "You can grant permission to a single Amazon Web Services account only, and only one
//!    account at a time" (`CreateNetworkInterfacePermission`): one grantee's grant of one
//!    permission type over one interface is ONE record, so a repeat answers that grant and a
//!    grant of the OTHER type over the same interface is a second one;
//! 4. the grantee is ANY Amazon Web Services account id — the point of the operation is to
//!    share the interface with another account — while a request that does not name exactly
//!    one readable grantee is refused for THAT (`MissingParameter`,
//!    `InvalidParameterCombination`, `UnsupportedOperation`, `InvalidParameterValue`);
//! 5. the interface has to be one the account holds and manages: something that is not an
//!    interface id is refused before the store is read, an id the account holds no interface
//!    for is `InvalidNetworkInterfaceID.NotFound`, and an interface another Amazon Web
//!    Services service manages is not the account's to share;
//! 6. every refusal records NOTHING, and `DryRun` on a request that would have succeeded
//!    answers `DryRunOperation` and records nothing either.
//!
//! Each table row is a distinct input along what the rule generalises over — which published
//! permission type, which interface, which grantee and in which form, which interface id and
//! whether the request is a dry run — so no row is the specification. The interfaces are
//! created through the API, and the grants are read back through
//! `DescribeNetworkInterfacePermissions`, so what is asserted is the store both operations
//! share.

use ec2::ops::types::INTERFACE_PERMISSION_TYPE_VALUES;
use ec2::state::{
    AWS_ACCOUNT_ID_DIGITS, ComAmazonawsEc2Amazonec2State as Ec2State, DRY_RUN_OPERATION,
    INVALID_NETWORK_INTERFACE_ID_MALFORMED, INVALID_NETWORK_INTERFACE_ID_NOT_FOUND,
    INVALID_PARAMETER_COMBINATION, INVALID_PARAMETER_VALUE, MISSING_PARAMETER,
    NETWORK_INTERFACE_PERMISSION_ID_PREFIX, NETWORK_INTERFACE_PERMISSION_STATE_GRANTED,
    UNSUPPORTED_OPERATION,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, elements, error_code, error_message, filters, indexed};
use super::wire::{Param, param};

/// The Region every call here is signed for: the account's default VPC and its default
/// subnets are seeded for the emulator's default Region, and the interfaces these grants are
/// over are created in one of them.
const REGION: &str = "us-east-1";

/// EC2's code for an action the resource or the caller's standing forbids.
const OPERATION_NOT_PERMITTED: &str = "OperationNotPermitted";

/// One row of a table that expects a refusal: `(case, the parameters, the error code)`.
type RefusalRow = (&'static str, Vec<Param>, &'static str);

/// The one account this emulator serves, which is therefore the account a grant to "another"
/// account is distinguished from. Read from the emulator rather than written as a literal, so
/// the test asks about what the service itself serves.
fn served_account() -> &'static str {
    ::engine::emulator_account_id()
}

fn emulator() -> (MultiServiceRouter, std::sync::Arc<engine::InMemoryEngine>) {
    super::ec2_query::emulator()
}

/// One grant that has to succeed, and the `NetworkInterfacePermissionId` it answered.
async fn granted_id(
    router: &MultiServiceRouter,
    interface_id: &str,
    grantee: &str,
    permission: &str,
) -> String {
    let (status, body) = grant(
        router,
        &[
            param("NetworkInterfaceId", interface_id),
            param("AwsAccountId", grantee),
            param("Permission", permission),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "a grant of {permission} over {interface_id} to {grantee} must succeed, got {status} \
         with body {body}"
    );
    element(&body, "networkInterfacePermissionId").unwrap_or_default()
}

/// One `CreateNetworkInterfacePermission` call carrying `params`.
async fn grant(router: &MultiServiceRouter, params: &[Param]) -> (u16, String) {
    super::ec2_query::call(
        router,
        REGION,
        "CreateNetworkInterfacePermission",
        params,
    )
    .await
}

/// One `DescribeNetworkInterfacePermissions` call carrying `params`.
async fn describe(router: &MultiServiceRouter, params: &[Param]) -> (u16, String) {
    super::ec2_query::call(
        router,
        REGION,
        "DescribeNetworkInterfacePermissions",
        params,
    )
    .await
}

/// The permission ids the describe reports for the whole account.
async fn described_ids(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let (status, body) = describe(router, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from the describe, got {status} with body {body}"
    );
    let mut ids = elements(&body, "networkInterfacePermissionId");
    ids.sort();
    ids
}

/// The first default subnet of the account's default VPC — a subnet the store really holds,
/// so an interface created into it is one the service itself minted.
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
    let (status, body) = super::ec2_query::call(
        router,
        REGION,
        "CreateNetworkInterface",
        &[
            param("SubnetId", subnet_id),
            param("Description", description),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateNetworkInterface must succeed, got {status} with body {body}"
    );
    element(&body, "networkInterfaceId").unwrap_or_default()
}

/// The store's permissions as a string, for the "nothing was recorded" assertions: a refused
/// call must leave every grant exactly as it found it.
fn snapshot(engine: &engine::InMemoryEngine) -> String {
    let state = engine.state::<Ec2State>();
    let store = state.lock();
    format!("{:?}", store.network_interface_permissions)
}

/// The member `name` of the one permission the describe reports for `permission_id`.
async fn described_member(
    router: &MultiServiceRouter,
    case: &str,
    permission_id: &str,
    name: &str,
) -> Option<String> {
    let (status, body) = describe(
        router,
        &indexed("NetworkInterfacePermissionId", &[permission_id]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 for {permission_id}, got {status} with body {body}"
    );
    element(&body, name)
}

// ---------------------------------------------------------------------------
// Rules 1 and 2: the grant is recorded, and it is in force
// ---------------------------------------------------------------------------

/// What one row of the success table grants: a published permission type over one of the two
/// interfaces the account created, to one grantee account.
struct GrantCase {
    case: &'static str,
    /// Which of the seeded interfaces the grant is over.
    interface: usize,
    /// The Amazon Web Services account id the interface is shared with.
    grantee: String,
    /// One member of the `InterfacePermissionType` enum
    /// (`INTERFACE_PERMISSION_TYPE_VALUES`, the whole published catalog).
    permission: &'static str,
}

/// Each published permission type, over more than one interface, granted to accounts of its
/// own: every row mints its own permission, reports it in force, and is
/// reported back by the describe with the members it was written with. The ids are distinct,
/// so each row is its own record.
#[tokio::test]
async fn every_published_permission_type_is_recorded_and_reads_back_from_the_store() {
    let (router, engine) = emulator();
    let subnet_id = default_subnet(&engine);
    let interfaces = [
        create_interface(&router, &subnet_id, "shared").await,
        create_interface(&router, &subnet_id, "also shared").await,
    ];

    // The rows differ along the three things the rule generalises over: which published type
    // is granted, which interface it is over, and which account it is granted to — an account
    // other than the one the emulator serves (which is the whole point of the operation), the
    // served account itself, and an account a digit away from it.
    let neighbouring_account = {
        let mut digits: Vec<char> = served_account().chars().collect();
        let last = digits.len() - 1;
        digits[last] = if digits[last] == '9' { '8' } else { '9' };
        digits.into_iter().collect::<String>()
    };
    let cases = [
        GrantCase {
            case: "the first published type over the first interface, to another account",
            interface: 0,
            grantee: "111122223333".to_string(),
            permission: INTERFACE_PERMISSION_TYPE_VALUES[0],
        },
        GrantCase {
            case: "the second published type over the first interface, to the served account",
            interface: 0,
            grantee: served_account().to_string(),
            permission: INTERFACE_PERMISSION_TYPE_VALUES[1],
        },
        GrantCase {
            case: "the second published type over the second interface, to a neighbouring \
                   account",
            interface: 1,
            grantee: neighbouring_account,
            permission: INTERFACE_PERMISSION_TYPE_VALUES[1],
        },
    ];

    let mut minted: Vec<String> = Vec::new();
    for case in &cases {
        let interface_id = interfaces[case.interface].as_str();
        let (status, body) = grant(
            &router,
            &[
                param("NetworkInterfaceId", interface_id),
                param("AwsAccountId", &case.grantee),
                param("Permission", case.permission),
            ],
        )
        .await;
        let label = case.case;
        assert_eq!(
            status, 200,
            "case {label:?}: expected HTTP 200, got {status} with body {body}"
        );
        let permission_id = element(&body, "networkInterfacePermissionId").unwrap_or_default();
        assert!(
            permission_id.starts_with(&format!("{NETWORK_INTERFACE_PERMISSION_ID_PREFIX}-")),
            "case {label:?}: expected a {NETWORK_INTERFACE_PERMISSION_ID_PREFIX}- id, got \
             {permission_id:?} from {body}"
        );
        assert!(
            !minted.contains(&permission_id),
            "case {label:?}: expected an id of its own, got {permission_id:?} again"
        );

        // Every member the request chose, and the state the service owns, as the call
        // answered them.
        for (member, expected) in [
            ("networkInterfaceId", interface_id.to_string()),
            ("awsAccountId", case.grantee.clone()),
            ("permission", case.permission.to_string()),
            (
                "state",
                NETWORK_INTERFACE_PERMISSION_STATE_GRANTED.to_string(),
            ),
        ] {
            let actual = element(&body, member);
            assert_eq!(
                actual.as_deref(),
                Some(expected.as_str()),
                "case {label:?}: expected {member} {expected:?}, got {actual:?} from {body}"
            );
        }
        assert_eq!(
            element(&body, "awsService"),
            None,
            "case {label:?}: a grant to an account names no service, got {body}"
        );

        // The round trip: the describe reports the record this call wrote, member for member.
        for (member, expected) in [
            ("networkInterfacePermissionId", permission_id.clone()),
            ("networkInterfaceId", interface_id.to_string()),
            ("awsAccountId", case.grantee.clone()),
            ("permission", case.permission.to_string()),
            (
                "state",
                NETWORK_INTERFACE_PERMISSION_STATE_GRANTED.to_string(),
            ),
        ] {
            let actual = described_member(&router, label, &permission_id, member).await;
            assert_eq!(
                actual.as_deref(),
                Some(expected.as_str()),
                "case {label:?}: expected the describe to report {member} {expected:?}, got \
                 {actual:?}"
            );
        }
        minted.push(permission_id);
    }

    let mut expected = minted.clone();
    expected.sort();
    assert_eq!(
        described_ids(&router, "every grant").await,
        expected,
        "the describe must report every grant the creates recorded"
    );
}

// ---------------------------------------------------------------------------
// Rule 3: one grantee, one permission type, one interface is ONE record
// ---------------------------------------------------------------------------

/// A repeat of a grant the account already holds answers THAT grant rather than minting a
/// second id beside it, while a grant of the other published type over the same interface,
/// the same type over another interface, and the same type to another GRANTEE are records of
/// their own. The rows differ along what makes a grant the same one: the permission type, the
/// interface and the grantee.
#[tokio::test]
async fn a_repeat_of_a_held_grant_answers_that_grant_and_a_different_one_mints_its_own() {
    let (router, engine) = emulator();
    let subnet_id = default_subnet(&engine);
    let first = create_interface(&router, &subnet_id, "granted twice").await;
    let second = create_interface(&router, &subnet_id, "granted once").await;
    const GRANTEE: &str = "111122223333";
    const OTHER_GRANTEE: &str = "444455556666";

    let original = granted_id(&router, &first, GRANTEE, INTERFACE_PERMISSION_TYPE_VALUES[0]).await;
    for case in ["the first repeat", "the second repeat"] {
        let repeated =
            granted_id(&router, &first, GRANTEE, INTERFACE_PERMISSION_TYPE_VALUES[0]).await;
        assert_eq!(
            repeated, original,
            "case {case:?}: expected the grant already held ({original:?}), got {repeated:?}"
        );
    }

    let other_type =
        granted_id(&router, &first, GRANTEE, INTERFACE_PERMISSION_TYPE_VALUES[1]).await;
    let other_interface =
        granted_id(&router, &second, GRANTEE, INTERFACE_PERMISSION_TYPE_VALUES[0]).await;
    let other_grantee =
        granted_id(&router, &first, OTHER_GRANTEE, INTERFACE_PERMISSION_TYPE_VALUES[0]).await;
    for (case, id) in [
        ("the other permission type", &other_type),
        ("the other interface", &other_interface),
        ("the other grantee", &other_grantee),
    ] {
        assert_ne!(
            id, &original,
            "case {case:?}: expected a record of its own, got the held grant {original:?}"
        );
    }

    let mut expected = vec![original, other_type, other_interface, other_grantee];
    expected.sort();
    assert_eq!(
        described_ids(&router, "after the repeats").await,
        expected,
        "the repeats must have added no record: the store holds one grant per grantee, \
         permission type and interface"
    );
}

// ---------------------------------------------------------------------------
// Rules 4, 5 and 6: who may be granted, over what, and that a refusal records nothing
// ---------------------------------------------------------------------------

/// A request that does not name exactly one readable grantee, and an interface the request
/// cannot name, are each refused with the code that says WHICH of them it was — and none of
/// them records anything.
///
/// The rows differ along every form the two can take: an account that is not an account id
/// in three different ways, no grantee at all, two grantees, a service grantee, an interface
/// id of another shape and a well-formed id the account holds no interface for.
#[tokio::test]
async fn a_grantee_or_an_interface_the_service_cannot_accept_is_refused_and_records_nothing() {
    let (router, engine) = emulator();
    let subnet_id = default_subnet(&engine);
    let interface_id = create_interface(&router, &subnet_id, "not shareable like that").await;

    // A grant the account DOES hold, so every row below is asserted against a populated
    // store rather than an empty one.
    let held = {
        let (status, body) = grant(
            &router,
            &[
                param("NetworkInterfaceId", &interface_id),
                param("AwsAccountId", served_account()),
                param("Permission", INTERFACE_PERMISSION_TYPE_VALUES[0]),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "the grant the refusals are asserted against must succeed, got {status}: {body}"
        );
        element(&body, "networkInterfacePermissionId").unwrap_or_default()
    };

    // A well-formed interface id of the long form (17 hexadecimal digits) the account holds
    // no interface for.
    const UNHELD_INTERFACE: &str = "eni-000000000000000ff";

    let rows: Vec<RefusalRow> = vec![
        (
            "an account id too short to be one",
            vec![
                param("NetworkInterfaceId", &interface_id),
                param("AwsAccountId", "12345"),
                param("Permission", INTERFACE_PERMISSION_TYPE_VALUES[0]),
            ],
            INVALID_PARAMETER_VALUE,
        ),
        (
            "an account id of the right length that is not digits",
            vec![
                param("NetworkInterfaceId", &interface_id),
                param("AwsAccountId", "abcdefghijkl"),
                param("Permission", INTERFACE_PERMISSION_TYPE_VALUES[0]),
            ],
            INVALID_PARAMETER_VALUE,
        ),
        (
            "an account id one digit too long",
            vec![
                param("NetworkInterfaceId", &interface_id),
                param("AwsAccountId", "0000000000000"),
                param("Permission", INTERFACE_PERMISSION_TYPE_VALUES[0]),
            ],
            INVALID_PARAMETER_VALUE,
        ),
        (
            "no grantee at all",
            vec![
                param("NetworkInterfaceId", &interface_id),
                param("Permission", INTERFACE_PERMISSION_TYPE_VALUES[0]),
            ],
            MISSING_PARAMETER,
        ),
        (
            "both grantees",
            vec![
                param("NetworkInterfaceId", &interface_id),
                param("AwsAccountId", served_account()),
                param("AwsService", "elasticloadbalancing.amazonaws.com"),
                param("Permission", INTERFACE_PERMISSION_TYPE_VALUES[0]),
            ],
            INVALID_PARAMETER_COMBINATION,
        ),
        (
            "a service grantee, which the model documents as not supported",
            vec![
                param("NetworkInterfaceId", &interface_id),
                param("AwsService", "elasticloadbalancing.amazonaws.com"),
                param("Permission", INTERFACE_PERMISSION_TYPE_VALUES[0]),
            ],
            UNSUPPORTED_OPERATION,
        ),
        (
            "something that is not an interface id at all",
            vec![
                param("NetworkInterfaceId", "not-an-interface"),
                param("AwsAccountId", served_account()),
                param("Permission", INTERFACE_PERMISSION_TYPE_VALUES[0]),
            ],
            INVALID_NETWORK_INTERFACE_ID_MALFORMED,
        ),
        (
            "an interface id the account holds no interface for",
            vec![
                param("NetworkInterfaceId", UNHELD_INTERFACE),
                param("AwsAccountId", served_account()),
                param("Permission", INTERFACE_PERMISSION_TYPE_VALUES[0]),
            ],
            INVALID_NETWORK_INTERFACE_ID_NOT_FOUND,
        ),
    ];

    let before = snapshot(&engine);
    for (case, params, expected_code) in rows {
        let (status, body) = grant(&router, &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
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
            "case {case:?}: a refused grant must record nothing"
        );
    }
    assert_eq!(
        described_ids(&router, "after every refusal").await,
        vec![held],
        "the refusals must have left the grant the account holds exactly as it was"
    );
}

/// `DryRun` asks whether the request would have succeeded: one that would is answered
/// `DryRunOperation` with nothing written, and one that would not is answered with the
/// refusal it earns — including the repeat of a grant already held, which would have
/// succeeded.
#[tokio::test]
async fn a_dry_run_answers_what_the_request_would_have_done_and_records_nothing() {
    let (router, engine) = emulator();
    let subnet_id = default_subnet(&engine);
    let interface_id = create_interface(&router, &subnet_id, "dry run").await;

    // A dry run BEFORE anything is granted: it would have succeeded, so nothing is recorded
    // and the account still holds no grant.
    let rows: Vec<(&str, Vec<Param>, u16, &str)> = vec![
        (
            "a grant that would have succeeded",
            vec![
                param("NetworkInterfaceId", &interface_id),
                param("AwsAccountId", served_account()),
                param("Permission", INTERFACE_PERMISSION_TYPE_VALUES[0]),
                param("DryRun", "true"),
            ],
            412,
            DRY_RUN_OPERATION,
        ),
        (
            "a grant to another account, which would have succeeded too",
            vec![
                param("NetworkInterfaceId", &interface_id),
                param("AwsAccountId", "111122223333"),
                param("Permission", INTERFACE_PERMISSION_TYPE_VALUES[0]),
                param("DryRun", "true"),
            ],
            412,
            DRY_RUN_OPERATION,
        ),
        (
            "a grant whose account is not an account id",
            vec![
                param("NetworkInterfaceId", &interface_id),
                param("AwsAccountId", "abcdefghijkl"),
                param("Permission", INTERFACE_PERMISSION_TYPE_VALUES[0]),
                param("DryRun", "true"),
            ],
            400,
            INVALID_PARAMETER_VALUE,
        ),
        (
            "a grant over an interface the account holds none of",
            vec![
                param("NetworkInterfaceId", "eni-000000000000000ff"),
                param("AwsAccountId", served_account()),
                param("Permission", INTERFACE_PERMISSION_TYPE_VALUES[1]),
                param("DryRun", "true"),
            ],
            400,
            INVALID_NETWORK_INTERFACE_ID_NOT_FOUND,
        ),
    ];
    for (case, params, expected_status, expected_code) in rows {
        let (status, body) = grant(&router, &params).await;
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
            described_ids(&router, case).await.is_empty(),
            "case {case:?}: a dry run must record nothing"
        );
    }

    // And once a grant IS held, a dry run of the repeat still answers `DryRunOperation` and
    // leaves the store where it was.
    let (status, body) = grant(
        &router,
        &[
            param("NetworkInterfaceId", &interface_id),
            param("AwsAccountId", served_account()),
            param("Permission", INTERFACE_PERMISSION_TYPE_VALUES[0]),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "the grant the dry-run repeat is asserted against must succeed, got {status}: {body}"
    );
    let before = snapshot(&engine);
    let (status, body) = grant(
        &router,
        &[
            param("NetworkInterfaceId", &interface_id),
            param("AwsAccountId", served_account()),
            param("Permission", INTERFACE_PERMISSION_TYPE_VALUES[0]),
            param("DryRun", "true"),
        ],
    )
    .await;
    assert_eq!(
        status, 412,
        "the dry run of a held grant would have succeeded, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        DRY_RUN_OPERATION,
        "the dry run of a held grant answers {DRY_RUN_OPERATION}, got {body}"
    );
    assert_eq!(
        snapshot(&engine),
        before,
        "the dry run of a held grant must leave the store exactly as it was"
    );
}

/// An interface another Amazon Web Services service created and manages for one of its own
/// resources — an interface VPC endpoint's — is not the account's to share, so a grant over
/// it is refused and records nothing, while a grant over an interface the account created
/// itself, in the same store, is recorded.
#[tokio::test]
async fn a_grant_over_an_interface_another_service_manages_is_refused() {
    let (router, engine) = emulator();
    let subnet_id = default_subnet(&engine);
    let own_interface = create_interface(&router, &subnet_id, "the account's own").await;

    // The endpoint whose interfaces the service manages, created through the API.
    let (status, body) = super::ec2_query::call(
        &router,
        REGION,
        "CreateVpcEndpoint",
        &[
            param("VpcEndpointType", "Interface"),
            param("ServiceName", &format!("com.amazonaws.{REGION}.sts")),
            param("VpcId", &vpc_of(&engine, &subnet_id)),
            param("SubnetId.1", &subnet_id),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateVpcEndpoint must succeed, got {status} with body {body}"
    );

    // The managed interface is found the way a client finds one: the describe's own
    // `requester-managed` filter.
    let (status, body) = super::ec2_query::call(
        &router,
        REGION,
        "DescribeNetworkInterfaces",
        &filters(&[("requester-managed", &["true"])]),
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeNetworkInterfaces must report the managed interfaces, got {status}: {body}"
    );
    let managed = elements(&body, "networkInterfaceId");
    let managed_interface = managed
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("an interface endpoint is served by a managed interface: {body}"));

    let before = snapshot(&engine);
    for permission in INTERFACE_PERMISSION_TYPE_VALUES {
        let (status, body) = grant(
            &router,
            &[
                param("NetworkInterfaceId", &managed_interface),
                param("AwsAccountId", served_account()),
                param("Permission", permission),
            ],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {permission:?}: expected HTTP 400 for a managed interface, got {status} \
             with body {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, OPERATION_NOT_PERMITTED,
            "case {permission:?}: expected {OPERATION_NOT_PERMITTED}, got {code} with message \
             {:?}",
            error_message(&body)
        );
        assert_eq!(
            snapshot(&engine),
            before,
            "case {permission:?}: a refused grant must record nothing"
        );
    }

    // The interface the account created itself, in the same store, IS shareable — so the
    // refusal above is about the interface rather than about the operation.
    let (status, body) = grant(
        &router,
        &[
            param("NetworkInterfaceId", &own_interface),
            param("AwsAccountId", served_account()),
            param("Permission", INTERFACE_PERMISSION_TYPE_VALUES[0]),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "a grant over the account's own interface must succeed, got {status}: {body}"
    );
    let permission_id = element(&body, "networkInterfacePermissionId").unwrap_or_default();
    assert_eq!(
        described_ids(&router, "the one grant the account holds").await,
        vec![permission_id],
        "the store must hold exactly the grant the successful call recorded"
    );
}

/// The VPC the subnet `subnet_id` sits in, read from the store the service itself holds.
fn vpc_of(engine: &engine::InMemoryEngine, subnet_id: &str) -> String {
    let state = engine.state::<Ec2State>();
    let store = state.lock();
    store
        .subnets
        .get(subnet_id)
        .and_then(|subnet| subnet.vpc_id.clone())
        .expect("a seeded default subnet sits in the default VPC")
}

/// The account id length the service reads an `AwsAccountId` by
/// (`AWS_ACCOUNT_ID_DIGITS`), asserted here so the table's "too short" and "too long" rows
/// are rows of the published rule rather than of a number written twice.
#[test]
fn the_account_id_length_the_rows_are_built_from_is_the_services_own() {
    assert_eq!(
        AWS_ACCOUNT_ID_DIGITS,
        served_account().len(),
        "the served account is an account id of the length the service reads one by"
    );
}
