// SPDX-License-Identifier: CC0-1.0

//! The identifiers this service GENERATES, end to end over the awsQuery wire.
//!
//! RULE: an identifier IAM mints for a resource type AWS publishes a unique-ID prefix for
//! begins with that prefix, carries only characters its modelled `idType`-family string
//! admits, is unique across every identifier the account mints, and is STORED — every
//! later read of the resource reports the same value rather than a fresh one.
//!
//! The prefixes themselves are the "unique ID prefixes" table of the IAM User Guide's "IAM
//! identifiers" page, which is the catalog every row below is drawn from: `ACCA` for a
//! service-specific credential, `AIDA` for a user, `AGPA` for a group, `AROA` for a role,
//! `AIPA` for an instance profile and `ANPA` for a customer managed policy. The rows
//! therefore span SIX resource types, and the service-specific-credential rows span the
//! whole catalog of services that issue one (`state::SERVICES_WITH_SPECIFIC_CREDENTIALS`),
//! both credential forms, and both credentials a user may hold per service.
//!
//! Every call goes over the wire IAM speaks, through the `MultiServiceRouter` harness
//! `tests/all/user_lifecycle.rs` owns, so what is asserted is the HTTP behaviour a client
//! on the socket sees.

use service_com_amazonaws_iam_awsidentitymanagementv20100508::state;
use smithy_server_runtime::MultiServiceRouter;

use crate::user_lifecycle::{call, element, elements, router};

/// The prefix AWS publishes for a service-specific credential's identifier.
const SERVICE_SPECIFIC_CREDENTIAL_PREFIX: &str = "ACCA";

/// The shortest a `ServiceSpecificCredentialId` may be: `serviceSpecificCredentialId` is
/// len 20..128 (`smithy_query(query="shape", shape="serviceSpecificCredentialId")`).
const MIN_CREDENTIAL_ID_CHARACTERS: usize = 20;

/// The shortest any other identifier below may be: the `idType` family the user, group,
/// role, instance-profile and policy ids target is len 16..128
/// (`smithy_query(query="shape", shape="idType")`).
const MIN_ENTITY_ID_CHARACTERS: usize = 16;

/// The longest any of them may be: both families cap at 128.
const MAX_ID_CHARACTERS: usize = 128;

/// A trust policy with the one element a trust document's statements must name, for the
/// role the prefix rows create.
const TRUST_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

/// An identity policy document the managed-policy create accepts.
const IDENTITY_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:GetObject","Resource":"*"}]}"#;

/// One resource type whose identifier the account mints: the create that mints it, the
/// member it is reported in, and the prefix AWS publishes for that type.
struct Minted {
    case: &'static str,
    action: &'static str,
    /// The create's parameters, as a function of the resource's name.
    params: fn(&str) -> Vec<(String, String)>,
    member: &'static str,
    prefix: &'static str,
    /// The read that must report the SAME identifier back, and its parameter name.
    read_action: &'static str,
    read_parameter: &'static str,
}

/// The resource types this suite drives, one row each. Every row but the first names a
/// type the reported defect did not cite, so the rule is checked beyond the credential
/// paths it was observed on.
const MINTED: &[Minted] = &[
    Minted {
        case: "iam user",
        action: "CreateUser",
        params: |name| vec![("UserName".to_string(), name.to_string())],
        member: "UserId",
        prefix: "AIDA",
        read_action: "GetUser",
        read_parameter: "UserName",
    },
    Minted {
        case: "iam group",
        action: "CreateGroup",
        params: |name| vec![("GroupName".to_string(), name.to_string())],
        member: "GroupId",
        prefix: "AGPA",
        read_action: "GetGroup",
        read_parameter: "GroupName",
    },
    Minted {
        case: "iam role",
        action: "CreateRole",
        params: |name| {
            vec![
                ("RoleName".to_string(), name.to_string()),
                (
                    "AssumeRolePolicyDocument".to_string(),
                    TRUST_POLICY.to_string(),
                ),
            ]
        },
        member: "RoleId",
        prefix: "AROA",
        read_action: "GetRole",
        read_parameter: "RoleName",
    },
    Minted {
        case: "instance profile",
        action: "CreateInstanceProfile",
        params: |name| vec![("InstanceProfileName".to_string(), name.to_string())],
        member: "InstanceProfileId",
        prefix: "AIPA",
        read_action: "GetInstanceProfile",
        read_parameter: "InstanceProfileName",
    },
    Minted {
        case: "customer managed policy",
        action: "CreatePolicy",
        params: |name| {
            vec![
                ("PolicyName".to_string(), name.to_string()),
                ("PolicyDocument".to_string(), IDENTITY_POLICY.to_string()),
            ]
        },
        member: "PolicyId",
        prefix: "ANPA",
        // A managed policy is read by its ARN, which the create reports beside the id.
        read_action: "GetPolicy",
        read_parameter: "PolicyArn",
    },
];

/// Assert `id` carries `prefix` and then only characters an uppercase-alphanumeric unique
/// id is spelled in, inside the length its modelled string admits.
fn assert_shaped(case: &str, id: &str, prefix: &str, min_characters: usize) {
    let suffix = id.strip_prefix(prefix);
    assert!(
        suffix.is_some(),
        "case {case:?}: expected an id beginning with the published prefix {prefix:?}, got {id:?}"
    );
    let suffix = suffix.unwrap_or_default();
    assert!(
        !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()),
        "case {case:?}: expected uppercase alphanumerics after {prefix:?}, got {id:?}"
    );
    let characters = id.chars().count();
    assert!(
        (min_characters..=MAX_ID_CHARACTERS).contains(&characters),
        "case {case:?}: expected {min_characters}..={MAX_ID_CHARACTERS} characters, \
         got {characters} in {id:?}"
    );
}

/// One `CreateServiceSpecificCredential` call, asserted to succeed.
async fn create_credential(router: &MultiServiceRouter, user: &str, service: &str) -> String {
    let (status, body) = call(
        router,
        "CreateServiceSpecificCredential",
        &[("UserName", user), ("ServiceName", service)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {service:?}: expected the credential to be created, got {status}: {body}"
    );
    body
}

/// RULE, on the identifier of a service-specific credential: it begins with the `ACCA`
/// prefix AWS publishes for that resource type, is unique across every credential the
/// account mints, and is the STORED handle — the listing reads it back and a reset reports
/// the same id rather than minting another.
#[tokio::test]
async fn service_specific_credential_ids_carry_the_published_prefix() {
    let router = router();
    let mut minted: Vec<String> = Vec::new();

    for (row, service) in state::SERVICES_WITH_SPECIFIC_CREDENTIALS
        .iter()
        .enumerate()
    {
        let case = service.service_name;
        let user = format!("prefix-user-{row}");
        let (status, body) = call(&router, "CreateUser", &[("UserName", &user)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the fixture user to be created, got {status}: {body}"
        );

        // Both credentials a user may hold for one service, so the rule is shown on an id
        // minted beside one the account already holds.
        let mut held = Vec::new();
        for _ in 0..state::MAX_SERVICE_SPECIFIC_CREDENTIALS_PER_SERVICE {
            let created = create_credential(&router, &user, service.service_name).await;
            let id = element(&created, "ServiceSpecificCredentialId");
            assert_shaped(
                case,
                &id,
                SERVICE_SPECIFIC_CREDENTIAL_PREFIX,
                MIN_CREDENTIAL_ID_CHARACTERS,
            );
            assert!(
                !minted.contains(&id),
                "case {case:?}: expected an id no other credential carries, got the repeat {id:?}"
            );
            minted.push(id.clone());
            held.push(id);
        }

        // The id is stored, not recomputed: the listing reports exactly the ids the
        // creates answered.
        let (status, listed) = call(
            &router,
            "ListServiceSpecificCredentials",
            &[("UserName", &user)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the listing to succeed, got {status}: {listed}"
        );
        let mut reported = elements(&listed, "ServiceSpecificCredentialId");
        reported.sort();
        let mut expected = held.clone();
        expected.sort();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the listing to report the minted ids {expected:?}, \
             got {reported:?}"
        );

        // A reset replaces the secret and keeps the identifier, so the handle a client
        // holds stays the one the create reported.
        let subject = held.first().expect("a credential was minted");
        let (status, reset) = call(
            &router,
            "ResetServiceSpecificCredential",
            &[
                ("UserName", &user),
                ("ServiceSpecificCredentialId", subject),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the reset to succeed, got {status}: {reset}"
        );
        assert_eq!(
            &element(&reset, "ServiceSpecificCredentialId"),
            subject,
            "case {case:?}: expected the reset to keep the id {subject:?}, got {:?}",
            element(&reset, "ServiceSpecificCredentialId")
        );
    }

    assert_eq!(
        minted.len(),
        state::SERVICES_WITH_SPECIFIC_CREDENTIALS.len()
            * state::MAX_SERVICE_SPECIFIC_CREDENTIALS_PER_SERVICE,
        "every service in the catalog must have been driven, got {minted:?}"
    );
}

/// RULE, on every OTHER resource type whose identifier this account mints: each begins
/// with the prefix AWS publishes for that type, carries only characters its modelled id
/// admits, is unique across types, and is reported identically by the create and by a
/// later read.
#[tokio::test]
async fn entity_ids_carry_the_published_prefix_of_their_resource_type() {
    let router = router();
    let mut minted: Vec<String> = Vec::new();

    for row in MINTED {
        let case = row.case;
        let name = format!("prefix-{}", row.member);
        let params = (row.params)(&name);
        let borrowed: Vec<(&str, &str)> = params
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        let (status, created) = call(&router, row.action, &borrowed).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected {} to succeed, got {status}: {created}",
            row.action
        );

        let id = element(&created, row.member);
        assert_shaped(case, &id, row.prefix, MIN_ENTITY_ID_CHARACTERS);
        assert!(
            !minted.contains(&id),
            "case {case:?}: expected an id no other resource carries, got the repeat {id:?}"
        );
        minted.push(id.clone());

        // A policy is addressed by its ARN; every other row by the name it was created
        // under.
        let handle = if row.read_parameter == "PolicyArn" {
            element(&created, "Arn")
        } else {
            name.clone()
        };
        let (status, read) = call(
            &router,
            row.read_action,
            &[(row.read_parameter, handle.as_str())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected {} to succeed, got {status}: {read}",
            row.read_action
        );
        assert_eq!(
            element(&read, row.member),
            id,
            "case {case:?}: expected the stored id {id:?}, got {:?}",
            element(&read, row.member)
        );
    }
}
