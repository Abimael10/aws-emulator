// SPDX-License-Identifier: CC0-1.0

//! What a client's TEARDOWN sees once a role is gone, end to end over the awsQuery wire.
//!
//! RULE: a role the account no longer holds resolves nowhere — every `GetRole` naming it
//! is refused with the modelled `NoSuchEntityException` (HTTP 404, sender-classified,
//! awsQuery `Code` `NoSuchEntity`), worded `The role with name <RoleName> cannot be
//! found.` with the name taken from the REQUEST, and every other reader of the account
//! stops reporting it too — whether the name was released by `DeleteRole` after its
//! subordinates were removed, or the account never held a role under it. A client that
//! polls `GetRole` to confirm its delete completed converges on that refusal; a reader
//! that kept answering 200 would spin forever and then collide with a re-create of the
//! same name.
//!
//! Each suite is a TABLE whose rows differ along what the rule generalises over: the
//! role's name alphabet and length, its path's depth, the CASE the read spells the name
//! in, which subordinates the role held before its teardown (an instance-profile binding,
//! attached managed policies, embedded inline policies, several at once, none), the
//! position in the account's role set the released role occupied, and the form an unheld
//! name misses every held one by.
//!
//! The awsQuery harness (`router`, `call`, `element`, `elements`, `error_code`) is the
//! one `tests/all/user_lifecycle.rs` owns and every suite in this crate shares, so a
//! request is built and read exactly one way.

use smithy_server_runtime::MultiServiceRouter;

use crate::user_lifecycle::{call, element, elements, error_code, router};

/// A trust policy with a single statement — the form `CreateRole`'s own modelled example
/// carries. Every role here needs one to exist at all; none of these assertions is about
/// it.
const TRUST_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"eks.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

/// An inline policy document: a JSON object carrying a `Statement`, which is what
/// `PutRolePolicy` requires of one.
const INLINE_POLICY: &str =
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:GetObject","Resource":"*"}]}"#;

/// ARNs of AWS-managed policies, spanning the paths the managed-policy ARN grammar
/// admits (the account root, the `/service-role/` path and the `/job-function/` path).
/// The names and paths are entries of the AWS Managed Policy Reference, as this crate's
/// `tests/all/role_attached_policies.rs` reads them.
const AWS_MANAGED_POLICIES: &[&str] = &[
    "arn:aws:iam::aws:policy/AmazonEKSClusterPolicy",
    "arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole",
    "arn:aws:iam::aws:policy/job-function/ViewOnlyAccess",
];

/// The fixture roles: `(RoleName, Path)` rows spanning the whole `^[\w+=,.@-]+$` name
/// alphabet, the shortest and longest names `roleNameType` admits (len 1..64,
/// `smithy_query(query="operation", operation="GetRole")`), and the root, one-segment and
/// nested paths `pathType` admits.
fn roles() -> Vec<(String, &'static str)> {
    vec![
        ("probe-eks-cluster-role".to_string(), "/"),
        ("q".to_string(), "/"),
        ("Role_7+x=y,z.w@example-1".to_string(), "/service-role/"),
        ("nested".to_string(), "/eng/tools/eks/"),
        ("n".repeat(64), "/"),
    ]
}

/// Create one role at `path`, asserting the fixture create succeeded.
async fn create_role(router: &MultiServiceRouter, role_name: &str, path: &str) {
    let (status, body) = call(
        router,
        "CreateRole",
        &[
            ("RoleName", role_name),
            ("Path", path),
            ("AssumeRolePolicyDocument", TRUST_POLICY),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {role_name:?} at {path:?}: the fixture CreateRole must succeed, got \
         {status}: {body}"
    );
}

/// The whole refusal a read of a role nothing holds must answer, as AWS words it: the
/// fixed text `The role with name ` and ` cannot be found.` around the name the REQUEST
/// supplied.
fn absent_role_message(requested: &str) -> String {
    format!("The role with name {requested} cannot be found.")
}

/// Assert `GetRole` reports no role under `requested`: the modelled `NoSuchEntity`
/// envelope, with the message naming the role exactly as the request spelled it.
async fn assert_get_role_is_not_found(router: &MultiServiceRouter, case: &str, requested: &str) {
    let (status, body) = call(router, "GetRole", &[("RoleName", requested)]).await;
    assert_eq!(
        status, 404,
        "case {case}: expected GetRole 404 for a role the account does not hold, got \
         {status}: {body}"
    );
    assert_eq!(
        error_code(&body),
        "NoSuchEntity",
        "case {case}: expected the modelled awsQuery code NoSuchEntity, got {:?}: {body}",
        error_code(&body)
    );
    assert_eq!(
        element(&body, "Type"),
        "Sender",
        "case {case}: expected the refusal classified Sender, got {:?}: {body}",
        element(&body, "Type")
    );
    let expected = absent_role_message(requested);
    assert_eq!(
        element(&body, "Message"),
        expected,
        "case {case}: expected {expected:?}, got {:?}",
        element(&body, "Message")
    );
}

/// Assert the account reports a role under `role_name`, answering its `RoleId`.
async fn assert_role_is_held(router: &MultiServiceRouter, case: &str, role_name: &str) -> String {
    let (status, body) = call(router, "GetRole", &[("RoleName", role_name)]).await;
    assert_eq!(
        status, 200,
        "case {case}: the role must still be reported, got {status}: {body}"
    );
    element(&body, "RoleId")
}

/// The role names `ListRoles` reports for the whole account.
async fn listed_roles(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let (status, body) = call(router, "ListRoles", &[]).await;
    assert_eq!(
        status, 200,
        "case {case}: ListRoles must answer 200, got {status}: {body}"
    );
    elements(&body, "RoleName")
}

/// RULE: releasing a role makes its name resolve nowhere, however a later read spells it
/// — IAM entity names "are not distinguished by case", so each spelling is refused, and
/// each refusal names the role as THAT request spelled it. Every role still held reads
/// back unchanged, so the release took exactly the one role it named.
///
/// The rows are the fixture roles, released one after another, so each release is taken
/// against a different remaining set.
#[tokio::test]
async fn a_released_role_is_refused_however_a_later_read_spells_it() {
    let router = router();
    for (role_name, path) in roles() {
        create_role(&router, &role_name, path).await;
    }

    // The ids the creates minted, so "still held, unchanged" is asserted against a
    // stored value rather than against the fact that a read answered at all.
    let mut held: Vec<(String, String)> = Vec::new();
    for (role_name, _) in roles() {
        let role_id = assert_role_is_held(&router, &format!("fixture {role_name:?}"), &role_name)
            .await;
        held.push((role_name, role_id));
    }

    while let Some((role_name, _)) = held.first().cloned() {
        let (status, body) = call(&router, "DeleteRole", &[("RoleName", role_name.as_str())]).await;
        assert_eq!(
            status, 200,
            "case {role_name:?}: expected DeleteRole 200 for a held role, got {status}: {body}"
        );
        held.remove(0);

        // However the read spells the released name, it resolves nothing — and the
        // refusal carries the spelling the request used, not a folded one.
        for requested in [
            role_name.clone(),
            role_name.to_ascii_uppercase(),
            role_name.to_ascii_lowercase(),
        ] {
            assert_get_role_is_not_found(
                &router,
                &format!("{role_name:?} read as {requested:?}"),
                &requested,
            )
            .await;
        }

        // The account's other readers agree: the released name is not in the listing,
        // and a second delete is refused rather than reported as done.
        let listing = listed_roles(&router, &format!("{role_name:?} released")).await;
        assert!(
            !listing.contains(&role_name),
            "case {role_name:?}: expected the released role out of the listing, got {listing:?}"
        );
        let (status, body) = call(&router, "DeleteRole", &[("RoleName", role_name.as_str())]).await;
        assert_eq!(
            status, 404,
            "case {role_name:?}: expected a second DeleteRole 404, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            "NoSuchEntity",
            "case {role_name:?}: expected the modelled awsQuery code on the second delete, \
             got {body}"
        );

        // Every role still held is untouched by the release.
        for (still_held, role_id) in &held {
            let case = format!("{role_name:?} released, {still_held:?} still held");
            assert_eq!(
                assert_role_is_held(&router, &case, still_held).await,
                *role_id,
                "case {case}: releasing {role_name:?} changed the stored {still_held:?}"
            );
            assert!(
                listed_roles(&router, &case).await.contains(still_held),
                "case {case}: releasing {role_name:?} dropped {still_held:?} from the listing"
            );
        }
    }
}

/// One role's teardown: what it held before the delete could be served.
struct TeardownCase {
    /// What the row exercises, named in every assertion.
    case: &'static str,
    /// How many of [`AWS_MANAGED_POLICIES`] are attached to the role.
    attached: usize,
    /// How many inline policies are embedded in the role.
    embedded: usize,
    /// Whether an instance profile holds the role.
    in_instance_profile: bool,
}

/// Rows spanning what can hold a role back from being deleted: nothing, each kind of
/// subordinate on its own, several of one kind, and all of them at once.
const TEARDOWN_CASES: &[TeardownCase] = &[
    TeardownCase {
        case: "nothing to remove first",
        attached: 0,
        embedded: 0,
        in_instance_profile: false,
    },
    TeardownCase {
        case: "one attached managed policy",
        attached: 1,
        embedded: 0,
        in_instance_profile: false,
    },
    TeardownCase {
        case: "two embedded inline policies",
        attached: 0,
        embedded: 2,
        in_instance_profile: false,
    },
    TeardownCase {
        case: "an instance-profile binding",
        attached: 0,
        embedded: 0,
        in_instance_profile: true,
    },
    TeardownCase {
        case: "three managed policies, one inline policy and a binding",
        attached: 3,
        embedded: 1,
        in_instance_profile: true,
    },
];

/// RULE (the same one, reached through the documented teardown): a role is released only
/// once its subordinates are removed — "before you delete a role, remove the following
/// attached items: inline policies, attached managed policies, instance profile"
/// (`DeleteRole`) — and from the moment it IS released, `GetRole` refuses its name with
/// the modelled `NoSuchEntity`, whatever the role had held.
#[tokio::test]
async fn a_role_released_after_its_subordinates_were_removed_is_refused() {
    for TeardownCase {
        case,
        attached,
        embedded,
        in_instance_profile,
    } in TEARDOWN_CASES
    {
        // A router of its own per row, so one row's teardown never explains another's.
        let router = router();
        let role_name = format!("teardown-{attached}-{embedded}-{in_instance_profile}");
        let profile_name = format!("{role_name}-profile");
        create_role(&router, &role_name, "/").await;

        for policy_arn in AWS_MANAGED_POLICIES.iter().take(*attached) {
            let (status, body) = call(
                &router,
                "AttachRolePolicy",
                &[("RoleName", role_name.as_str()), ("PolicyArn", policy_arn)],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: attaching {policy_arn:?} must succeed, got {status}: {body}"
            );
        }
        let inline_names: Vec<String> = (0..*embedded)
            .map(|index| format!("inline-{index}"))
            .collect();
        for policy_name in &inline_names {
            let (status, body) = call(
                &router,
                "PutRolePolicy",
                &[
                    ("RoleName", role_name.as_str()),
                    ("PolicyName", policy_name.as_str()),
                    ("PolicyDocument", INLINE_POLICY),
                ],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: embedding {policy_name:?} must succeed, got {status}: {body}"
            );
        }
        if *in_instance_profile {
            let (status, body) = call(
                &router,
                "CreateInstanceProfile",
                &[("InstanceProfileName", profile_name.as_str())],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the fixture CreateInstanceProfile must succeed, got \
                 {status}: {body}"
            );
            let (status, body) = call(
                &router,
                "AddRoleToInstanceProfile",
                &[
                    ("InstanceProfileName", profile_name.as_str()),
                    ("RoleName", role_name.as_str()),
                ],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the fixture AddRoleToInstanceProfile must succeed, got \
                 {status}: {body}"
            );
        }

        // While a subordinate still holds the role, the delete is refused and the role is
        // STILL reported: it is the delete that releases it, not the teardown's start.
        let holds_something = *attached > 0 || *embedded > 0 || *in_instance_profile;
        if holds_something {
            let (status, body) =
                call(&router, "DeleteRole", &[("RoleName", role_name.as_str())]).await;
            assert_eq!(
                status, 409,
                "case {case:?}: expected DeleteRole 409 while a subordinate holds the role, \
                 got {status}: {body}"
            );
            assert_eq!(
                error_code(&body),
                "DeleteConflict",
                "case {case:?}: expected the modelled awsQuery code, got {body}"
            );
            assert_role_is_held(&router, case, &role_name).await;
        }

        // The teardown a client follows, in the order `DeleteRole` documents.
        if *in_instance_profile {
            let (status, body) = call(
                &router,
                "RemoveRoleFromInstanceProfile",
                &[
                    ("InstanceProfileName", profile_name.as_str()),
                    ("RoleName", role_name.as_str()),
                ],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: RemoveRoleFromInstanceProfile must succeed, got {status}: {body}"
            );
        }
        for policy_name in &inline_names {
            let (status, body) = call(
                &router,
                "DeleteRolePolicy",
                &[
                    ("RoleName", role_name.as_str()),
                    ("PolicyName", policy_name.as_str()),
                ],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: DeleteRolePolicy must succeed for {policy_name:?}, got \
                 {status}: {body}"
            );
        }
        for policy_arn in AWS_MANAGED_POLICIES.iter().take(*attached) {
            let (status, body) = call(
                &router,
                "DetachRolePolicy",
                &[("RoleName", role_name.as_str()), ("PolicyArn", policy_arn)],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: DetachRolePolicy must succeed for {policy_arn:?}, got \
                 {status}: {body}"
            );
        }

        let (status, body) = call(&router, "DeleteRole", &[("RoleName", role_name.as_str())]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected DeleteRole 200 once every subordinate is removed, got \
             {status}: {body}"
        );

        // The teardown is complete: the role resolves nowhere.
        assert_get_role_is_not_found(&router, case, &role_name).await;
        let listing = listed_roles(&router, case).await;
        assert!(
            !listing.contains(&role_name),
            "case {case:?}: expected the released role out of the listing, got {listing:?}"
        );
    }
}

/// RULE (the other side of the same refusal): a name the account NEVER held is refused
/// exactly the same way, so a client cannot tell "deleted" from "never created" and the
/// refusal is about the role's absence rather than about the delete.
///
/// The rows are names that miss every held role: an unrelated one, two near misses, a
/// held name with one character appended, a held name differing only by a separator the
/// folded name does not carry, and a name at the longest length `roleNameType` admits.
#[tokio::test]
async fn a_name_the_account_never_held_is_refused_the_same_way() {
    let router = router();
    create_role(&router, "held-role", "/").await;
    let role_id = assert_role_is_held(&router, "the held fixture", "held-role").await;

    let longest = "z".repeat(64);
    for requested in [
        "nobody",
        "held-rol",
        "held-roles",
        "held-role1",
        "held_role",
        longest.as_str(),
    ] {
        assert_get_role_is_not_found(&router, &format!("unheld {requested:?}"), requested).await;
    }

    // The refusals wrote nothing: the one held role is still there, with the id its
    // create minted.
    assert_eq!(
        assert_role_is_held(&router, "after the refused reads", "held-role").await,
        role_id,
        "the refused reads changed the held role"
    );
}
