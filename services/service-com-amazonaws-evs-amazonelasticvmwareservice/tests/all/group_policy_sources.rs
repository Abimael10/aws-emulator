// SPDX-License-Identifier: CC0-1.0

//! What a caller's GROUP membership puts in force on a request to this service.
//!
//! The rule under test: the identity-based documents in force on an IAM user are its own
//! inline and attached managed policies PLUS the inline and attached managed policies of
//! every group it belongs to — "if you specify a user, the simulation also includes all
//! policies that are attached to any groups the user belongs to"
//! (`SimulatePrincipalPolicy$PolicySourceArn`). So an explicit `Deny` of this service's
//! actions refuses the call whichever of a group's two sources holds it, exactly as it
//! does when the same document sits on the user itself
//! (`tests/all/identity_policy_sources.rs`, which owns the principal's own two sources
//! and whose fixtures this suite reuses).
//!
//! What is NOT in force is as much of the rule as what is: a group the user does not
//! belong to decides nothing, a version of a group's attached policy that is not its
//! default is not the document the attachment puts in force, an attachment the account
//! holds no document for carries no statement this gate can read, and a group bounds the
//! USERS that are its members — never a role session, which belongs to no group.

use serde_json::{json, Value};

use service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam;

use crate::environment_lifecycle::{
    call_as, create, emulator, error_type, Emulator, ACCOUNT, REGION,
};
use crate::identity_policy_sources::{
    create_managed_policy, put_inline_policy, register_principal, Document, Principal,
    ACCESS_DENIED, ALLOW_EVS, DENY_EC2, DENY_EVS, DENY_OTHER_ENVIRONMENT, UNREADABLE_POLICY_ARN,
};

/// Make the user `user_name` a member of the group `group_name`, where
/// `iam:AddUserToGroup` records the membership: in the group-keyed member map, under the
/// key the account holds each entity by.
fn add_user_to_group(engine: &engine::InMemoryEngine, group_name: &str, user_name: &str) {
    let state = iam::service_state(engine).expect("the in-memory engine carries the IAM store");
    state
        .lock()
        .group_members
        .entry(iam::name_key(group_name))
        .or_default()
        .insert(iam::name_key(user_name));
}

/// Embed an inline identity policy on a group, where `iam:PutGroupPolicy` writes one.
fn put_group_policy(
    engine: &engine::InMemoryEngine,
    group_name: &str,
    policy_name: &str,
    document: &str,
) {
    let state = iam::service_state(engine).expect("the in-memory engine carries the IAM store");
    state
        .lock()
        .group_inline_policies
        .entry(iam::name_key(group_name))
        .or_default()
        .insert(
            iam::name_key(policy_name),
            iam::InlinePolicy {
                policy_name: policy_name.to_string(),
                policy_document: document.to_string(),
            },
        );
}

/// Attach the managed policy `policy_arn` to a group, where `iam:AttachGroupPolicy`
/// writes the attachment.
fn attach_group_policy(engine: &engine::InMemoryEngine, group_name: &str, policy_arn: &str) {
    let state = iam::service_state(engine).expect("the in-memory engine carries the IAM store");
    state
        .lock()
        .group_attached_policies
        .entry(iam::name_key(group_name))
        .or_default()
        .insert(policy_arn.to_string());
}

/// Hold `document` on the group `group_name` in the place its variant names — the same
/// three places a document can sit on any IAM entity.
fn hold_on_group(
    router: &Emulator,
    group_name: &str,
    label: &str,
    index: usize,
    document: &Document,
) {
    match document {
        Document::Inline(policy_document) => put_group_policy(
            &router.engine,
            group_name,
            &format!("{label}-group-inline-{index}"),
            policy_document,
        ),
        Document::Attached(versions) => {
            let policy_arn = create_managed_policy(
                &router.engine,
                &format!("{label}-group-managed-{index}"),
                versions,
            );
            attach_group_policy(&router.engine, group_name, &policy_arn);
        }
        Document::AttachedUnreadable(policy_arn) => {
            attach_group_policy(&router.engine, group_name, policy_arn)
        }
    }
}

/// The access key id the caller labelled `label` signs with.
fn access_key_id_for(label: &str) -> String {
    format!("ASIA{}", label.to_ascii_uppercase().replace('-', ""))
}

/// Assert the gate's answer to one call: refused with the AWS authorization layer's
/// `AccessDeniedException` naming the principal and the action, or served by this
/// service's own handling.
fn assert_answer(case: &str, arn: &str, operation: &str, refused: bool, status: u16, body: &Value) {
    if refused {
        // Every CLIENT error of this awsJson1_0 service is reported with HTTP 400 —
        // `tests/all/error_wire_status.rs` owns that rule.
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} {body}"
        );
        assert_eq!(
            error_type(body),
            ACCESS_DENIED,
            "case {case:?}: expected {ACCESS_DENIED}, got {} in {body}",
            error_type(body)
        );
        let message = body["message"].as_str().unwrap_or_default().to_string();
        for part in [
            arn,
            &format!("evs:{operation}"),
            "with an explicit deny in an identity-based policy",
        ] {
            assert!(
                message.contains(part),
                "case {case:?}: the refusal must name {part:?}, got {message:?}"
            );
        }
    } else {
        assert_ne!(
            error_type(body),
            ACCESS_DENIED,
            "case {case:?}: no document in force denies this request, so the gate must \
             not refuse it, got {status} {body}"
        );
    }
}

/// RULE: a document held on a group is in force on every user that belongs to it, in
/// both of a group's places (embedded inline, or attached as a managed policy whose
/// DEFAULT version is the operative one), so an explicit deny there refuses the member's
/// call — and a document a group holds that does not cover the request refuses nothing.
#[tokio::test]
async fn a_document_held_on_a_group_is_in_force_on_its_members() {
    let router = emulator();
    let environment = create(&router, Some("group-sources")).await;
    let request = json!({ "environmentId": environment });

    // (case, what the group holds, whether the gate refuses the member's call)
    let rows: Vec<(&str, Vec<Document>, bool)> = vec![
        ("group inline deny", vec![Document::Inline(DENY_EVS)], true),
        (
            "group attached deny",
            vec![Document::Attached(&[("v1", DENY_EVS, true)])],
            true,
        ),
        (
            "group attached deny in the default version",
            vec![Document::Attached(&[
                ("v1", ALLOW_EVS, false),
                ("v2", DENY_EVS, true),
            ])],
            true,
        ),
        (
            "group attached deny outside the default version",
            vec![Document::Attached(&[
                ("v1", ALLOW_EVS, true),
                ("v2", DENY_EVS, false),
            ])],
            false,
        ),
        (
            "group inline allow beside a group attached deny",
            vec![
                Document::Inline(ALLOW_EVS),
                Document::Attached(&[("v1", DENY_EVS, true)]),
            ],
            true,
        ),
        (
            "group deny of another service",
            vec![Document::Inline(DENY_EC2)],
            false,
        ),
        (
            "group deny of another environment",
            vec![Document::Attached(&[("v1", DENY_OTHER_ENVIRONMENT, true)])],
            false,
        ),
        (
            "a group attachment the account holds no document for",
            vec![Document::AttachedUnreadable(UNREADABLE_POLICY_ARN)],
            false,
        ),
        ("group allow only", vec![Document::Inline(ALLOW_EVS)], false),
        ("a group holding nothing", Vec::new(), false),
    ];

    // The user's IAM path is carried by its ARN and not by the key its membership is
    // held under, so every row is run for a user at the account's default path and for
    // one under a path of its own.
    let paths: [(&str, &str); 2] = [("at the default path", "/"), ("under a path", "/team/evs/")];

    for (row, (case, documents, refused)) in rows.iter().enumerate() {
        for (path_index, (path_case, path)) in paths.iter().enumerate() {
            let case = format!("{case}, a member {path_case}");
            let label = format!("grp{row}p{path_index}");
            let user_name = format!("evs-user-{label}");
            let group_name = format!("evs-group-{label}");
            let access_key_id = access_key_id_for(&label);
            let arn = register_principal(
                &router.engine,
                &access_key_id,
                Principal::IamUser { path },
                &user_name,
            );
            add_user_to_group(&router.engine, &group_name, &user_name);
            for (index, document) in documents.iter().enumerate() {
                hold_on_group(&router, &group_name, &label, index, document);
            }

            let (status, body) =
                call_as(&router, &access_key_id, REGION, "GetEnvironment", &request).await;
            assert_answer(&case, &arn, "GetEnvironment", *refused, status, &body);
            if !*refused {
                assert_eq!(
                    status, 200,
                    "case {case:?}: the request must reach this service's own handling, \
                     got {status} {body}"
                );
            }
        }
    }
}

/// Which groups a document has to be held on to decide a request: every one the user
/// BELONGS to, and no other — and a group bounds its member users, never a role session,
/// which belongs to no group.
#[tokio::test]
async fn only_the_groups_the_caller_belongs_to_decide_the_request() {
    let router = emulator();
    let environment = create(&router, Some("group-membership")).await;
    let environment_arn = format!("arn:aws:evs:{REGION}:{ACCOUNT}:environment/{environment}");

    // One caller per membership shape: the label, whether the caller is a user, the
    // groups it belongs to with what each holds, the groups it does NOT belong to with
    // what each holds, the documents on the principal itself, and whether the gate
    // refuses it.
    struct Row {
        case: &'static str,
        user: bool,
        member_of: &'static [&'static str],
        holds: &'static [(&'static str, &'static str)],
        own: &'static [&'static str],
        refused: bool,
    }
    let rows: &[Row] = &[
        Row {
            case: "the deny is on the only group the user belongs to",
            user: true,
            member_of: &["one"],
            holds: &[("one", DENY_EVS)],
            own: &[],
            refused: true,
        },
        Row {
            case: "the deny is on the second of two groups the user belongs to",
            user: true,
            member_of: &["one", "two"],
            holds: &[("one", ALLOW_EVS), ("two", DENY_EVS)],
            own: &[],
            refused: true,
        },
        Row {
            case: "the deny is on a group the user does not belong to",
            user: true,
            member_of: &["one"],
            holds: &[("one", ALLOW_EVS), ("two", DENY_EVS)],
            own: &[],
            refused: false,
        },
        Row {
            case: "the user's own policy allows and its group denies",
            user: true,
            member_of: &["one"],
            holds: &[("one", DENY_EVS)],
            own: &[ALLOW_EVS],
            refused: true,
        },
        Row {
            case: "the user's own policy denies and its group allows",
            user: true,
            member_of: &["one"],
            holds: &[("one", ALLOW_EVS)],
            own: &[DENY_EVS],
            refused: true,
        },
        Row {
            case: "the deny is on a group named like the role a session speaks for",
            user: false,
            member_of: &[],
            holds: &[("one", DENY_EVS)],
            own: &[ALLOW_EVS],
            refused: false,
        },
    ];

    // Each operation shape the gate decides: one addressing an environment, one
    // addressing an environment through its ARN, and one addressing no resource at all.
    let requests: Vec<(&str, Value)> = vec![
        (
            "AssociateEipToVlan",
            json!({
                "environmentId": environment,
                "vlanName": "hcx",
                "allocationId": "eipalloc-0123456789abcdef0",
            }),
        ),
        (
            "ListTagsForResource",
            json!({ "resourceArn": environment_arn }),
        ),
        ("ListEnvironments", json!({})),
    ];

    for (index, row) in rows.iter().enumerate() {
        let label = format!("mem{index}");
        let name = format!("evs-{label}");
        let access_key_id = access_key_id_for(&label);
        let principal = if row.user {
            Principal::IamUser { path: "/" }
        } else {
            Principal::AssumedRole
        };
        let arn = register_principal(&router.engine, &access_key_id, principal, &name);
        for document in row.own {
            put_inline_policy(&router.engine, principal, &name, &label, document);
        }
        // A group named for the role in the role row, so the only thing that can put its
        // document in force is a membership — which a role session never has.
        let group_name = |group: &str| {
            if row.user {
                format!("evs-group-{label}-{group}")
            } else {
                name.clone()
            }
        };
        for group in row.member_of {
            add_user_to_group(&router.engine, &group_name(group), &name);
        }
        for (group, document) in row.holds {
            put_group_policy(
                &router.engine,
                &group_name(group),
                &format!("{label}-{group}"),
                document,
            );
        }

        for (operation, request) in &requests {
            let case = format!("{} on {operation}", row.case);
            let (status, body) = call_as(&router, &access_key_id, REGION, operation, request).await;
            assert_answer(&case, &arn, operation, row.refused, status, &body);
        }
    }
}
