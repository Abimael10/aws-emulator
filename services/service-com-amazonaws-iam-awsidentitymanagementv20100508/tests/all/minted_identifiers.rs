// SPDX-License-Identifier: CC0-1.0

//! The identifiers this account MINTS, across the resource types that carry one.
//!
//! The rule under test: every one of these members is documented as "the unique identifier
//! for" the resource it names (`SSHPublicKey$SSHPublicKeyId`, `User$UserId`,
//! `Group$GroupId`, `InstanceProfile$InstanceProfileId`), and AWS mints one per resource —
//! so an id carries the published prefix of its resource type, has the length AWS mints
//! that type's ids at, matches the member's modelled pattern, and NO two resources the
//! account holds share one: not two of a type, and not two resource types.
//!
//! Each suite is a TABLE whose rows differ along what the rule generalises over: the
//! resource type whose id is minted, and how many of that type the account already holds.
//! Every row drives the real create over the awsQuery wire, so what is under test is the id
//! a PERSISTED resource was reported under; the harness is the one
//! `tests/all/user_lifecycle.rs` owns.

use crate::instance_profile_listing_document_encoding::CANONICAL_TRUST_POLICY;
use crate::ssh_public_keys::{create_user, modulus, openssh_line, PUBLISHED_MINIMUM_BITS};
use crate::user_lifecycle::{call, element, router};

/// How many characters AWS's own documented examples of each id have, which is the length
/// this suite expects a minted id to carry. The "IAM identifiers" page of the IAM User
/// Guide lists the prefix of each type under "unique ID prefixes", and the examples AWS
/// writes out keep the real length: a user id `AIDACKCEVSQ6C2EXAMPLE` is twenty-one
/// characters, while the "SSH key ID" the CodeCommit User Guide tells a user to sign in
/// with — `APKAEIBAERJR2EXAMPLE` — is twenty.
const UNIQUE_ID_CHARACTERS: usize = 21;
const SSH_PUBLIC_KEY_ID_CHARACTERS: usize = 20;

/// The widest bound any of the members below declares, which every row's id also has to be
/// inside: `publicKeyIdType` is len 20..128 and `idType` len 16..128, so an id of twenty to
/// a hundred and twenty-eight characters satisfies both
/// (`smithy_query(query="shape", shape="publicKeyIdType,idType")`).
const MODELLED_ID_BOUNDS: ::std::ops::RangeInclusive<usize> = 20..=128;

/// One row of the minted-identifier table: a resource type, the call that creates one of it
/// and the member that reports its id.
struct Minted {
    case: &'static str,
    action: &'static str,
    params: Vec<(String, String)>,
    id_tag: &'static str,
    prefix: &'static str,
    characters: usize,
}

/// The managed policy document the policy row carries — one statement that allows one
/// read, which is all a policy needs to exist. The role row's trust policy is the canonical
/// one `tests/all/instance_profile_listing_document_encoding.rs` owns.
const POLICY_DOCUMENT: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"iam:GetUser","Resource":"*"}]}"#;

/// The rows: one per resource type whose create mints an identifier, each named `at` so a
/// row can be driven again for a second resource of the same type.
///
/// `UploadSSHPublicKey` is one row among the other types the same rule holds for, which is
/// why they are rows of one table rather than suites of their own.
fn minted(at: usize) -> Vec<Minted> {
    vec![
        Minted {
            case: "a user's UserId",
            action: "CreateUser",
            params: vec![("UserName".to_string(), format!("minted-user-{at}"))],
            id_tag: "UserId",
            prefix: "AIDA",
            characters: UNIQUE_ID_CHARACTERS,
        },
        Minted {
            case: "a group's GroupId",
            action: "CreateGroup",
            params: vec![("GroupName".to_string(), format!("minted-group-{at}"))],
            id_tag: "GroupId",
            prefix: "AGPA",
            characters: UNIQUE_ID_CHARACTERS,
        },
        Minted {
            case: "a role's RoleId",
            action: "CreateRole",
            params: vec![
                ("RoleName".to_string(), format!("minted-role-{at}")),
                (
                    "AssumeRolePolicyDocument".to_string(),
                    CANONICAL_TRUST_POLICY.to_string(),
                ),
            ],
            id_tag: "RoleId",
            prefix: "AROA",
            characters: UNIQUE_ID_CHARACTERS,
        },
        Minted {
            case: "an instance profile's InstanceProfileId",
            action: "CreateInstanceProfile",
            params: vec![(
                "InstanceProfileName".to_string(),
                format!("minted-profile-{at}"),
            )],
            id_tag: "InstanceProfileId",
            prefix: "AIPA",
            characters: UNIQUE_ID_CHARACTERS,
        },
        Minted {
            case: "a managed policy's PolicyId",
            action: "CreatePolicy",
            params: vec![
                ("PolicyName".to_string(), format!("minted-policy-{at}")),
                ("PolicyDocument".to_string(), POLICY_DOCUMENT.to_string()),
            ],
            id_tag: "PolicyId",
            prefix: "ANPA",
            characters: UNIQUE_ID_CHARACTERS,
        },
        Minted {
            case: "an SSH public key's SSHPublicKeyId",
            action: "UploadSSHPublicKey",
            params: vec![
                ("UserName".to_string(), KEY_HOLDER.to_string()),
                (
                    "SSHPublicKeyBody".to_string(),
                    // One key per row, so a repeat is a new key rather than a duplicate of
                    // one the user already holds.
                    openssh_line(&modulus(PUBLISHED_MINIMUM_BITS, 40 + at as u8), ""),
                ),
            ],
            id_tag: "SSHPublicKeyId",
            prefix: "APKA",
            characters: SSH_PUBLIC_KEY_ID_CHARACTERS,
        },
    ]
}

/// The user the SSH-public-key row associates its keys with.
const KEY_HOLDER: &str = "minted-key-holder";

/// Drive one row against `router` and answer the id the create reported.
async fn mint(
    router: &::smithy_server_runtime::MultiServiceRouter,
    case: &Minted,
) -> String {
    let params: Vec<(&str, &str)> = case
        .params
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    let (status, body) = call(router, case.action, &params).await;
    assert_eq!(
        status, 200,
        "case {:?}: expected {} to be served, got {status}: {body}",
        case.case, case.action
    );
    let id = element(&body, case.id_tag);
    assert!(
        !id.is_empty(),
        "case {:?}: expected the create to report {}, got {body}",
        case.case,
        case.id_tag
    );
    id
}

/// Whether `id` is spelled the way the members' shared pattern `^[\w]+$` admits
/// (`smithy_query(query="shape", shape="publicKeyIdType")`), and in the uppercase
/// alphanumerics AWS spells a unique id in.
fn is_unique_id_spelling(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

/// RULE: a minted id carries its type's published prefix, the length AWS mints that type's
/// ids at, and the spelling the member's pattern admits.
#[tokio::test]
async fn a_minted_identifier_is_shaped_as_aws_mints_it() {
    let router = router();
    create_user(&router, "the key holder", KEY_HOLDER).await;

    for case in minted(0) {
        let id = mint(&router, &case).await;
        assert!(
            id.starts_with(case.prefix),
            "case {:?}: expected the published prefix {:?}, got {id:?}",
            case.case,
            case.prefix
        );
        assert_eq!(
            id.chars().count(),
            case.characters,
            "case {:?}: expected {} characters, got {id:?}",
            case.case,
            case.characters
        );
        assert!(
            MODELLED_ID_BOUNDS.contains(&id.chars().count()),
            "case {:?}: expected an id inside the modelled bounds {MODELLED_ID_BOUNDS:?}, \
             got {id:?}",
            case.case
        );
        assert!(
            is_unique_id_spelling(&id),
            "case {:?}: expected the spelling the member's pattern admits, got {id:?}",
            case.case
        );
    }
}

/// RULE: no two resources share an id — not two of one type, and not two of different
/// types, however many the account already holds.
#[tokio::test]
async fn no_two_resources_of_one_account_share_a_minted_identifier() {
    let router = router();
    create_user(&router, "the key holder", KEY_HOLDER).await;

    let mut seen: Vec<(String, String)> = Vec::new();
    // Three of every type, so the rows differ in how many of that type the account already
    // held when the id was minted.
    for at in 0..3 {
        for case in minted(at) {
            let id = mint(&router, &case).await;
            assert!(
                !seen.iter().any(|(_, held)| held == &id),
                "case {:?} (the {} of its type): expected an id no other resource carries, \
                 got the repeat {id:?} among {seen:?}",
                case.case,
                at + 1
            );
            seen.push((case.case.to_string(), id));
        }
    }
}
