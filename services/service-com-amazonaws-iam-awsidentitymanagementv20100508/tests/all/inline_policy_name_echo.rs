// SPDX-License-Identifier: CC0-1.0

//! How an inline-policy READ reports the names the request addressed it with, end to end
//! over the awsQuery wire.
//!
//! An inline policy is not a resource of its own: it has no record, no ARN and no identity
//! apart from the entity that embeds it, and IAM resolves both the entity name and the
//! policy name without regard to case. So `GetUserPolicy`, `GetRolePolicy` and
//! `GetGroupPolicy` restate the names the REQUEST spelled, while the document they report
//! is the one the store holds — URL-encoded, as the contract for every policy document
//! says. The listings, which are the readers of the entity's own records, keep reporting
//! the spelling the account holds.
//!
//! Every test is a TABLE whose rows differ along what these rules generalise over: the
//! entity family (user, role, group), the case of each of the two names, the alphabet the
//! names are drawn from, and the form of the document. The awsQuery harness (router, call,
//! element, document_element) is the one `tests/all/user_lifecycle.rs` owns and every suite
//! in this crate shares.

use smithy_server_runtime::MultiServiceRouter;

use crate::policy_document_encoding::assert_percent_encoded;
use crate::user_lifecycle::{call, document_element, element, elements, router};

/// A trust policy with a single statement — the role rows need one, and the trust policy is
/// not what is under test here.
const TRUST_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"dlm.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

/// One inline-policy family: the entity kind a policy is embedded in, and the operations and
/// the input member that address it. The three families are the three entities IAM embeds an
/// inline policy in (`smithy_query(query="digest")`: `PutUserPolicy`/`GetUserPolicy`,
/// `PutRolePolicy`/`GetRolePolicy`, `PutGroupPolicy`/`GetGroupPolicy`).
struct Family {
    kind: &'static str,
    create_action: &'static str,
    put_action: &'static str,
    get_action: &'static str,
    list_action: &'static str,
    /// The member both the create and the policy operations name the entity with.
    name_member: &'static str,
    /// The path the entity is created under, so the rule is exercised at more than one depth.
    path: &'static str,
}

const FAMILIES: &[Family] = &[
    Family {
        kind: "user",
        create_action: "CreateUser",
        put_action: "PutUserPolicy",
        get_action: "GetUserPolicy",
        list_action: "ListUserPolicies",
        name_member: "UserName",
        path: "/",
    },
    Family {
        kind: "role",
        create_action: "CreateRole",
        put_action: "PutRolePolicy",
        get_action: "GetRolePolicy",
        list_action: "ListRolePolicies",
        name_member: "RoleName",
        path: "/eng/",
    },
    Family {
        kind: "group",
        create_action: "CreateGroup",
        put_action: "PutGroupPolicy",
        get_action: "GetGroupPolicy",
        list_action: "ListGroupPolicies",
        name_member: "GroupName",
        path: "/eng/tools/",
    },
];

/// Create the entity this family embeds a policy in.
async fn create_entity(router: &MultiServiceRouter, family: &Family, name: &str) {
    let mut params = vec![(family.name_member, name), ("Path", family.path)];
    if family.kind == "role" {
        params.push(("AssumeRolePolicyDocument", TRUST_POLICY));
    }
    let (status, body) = call(router, family.create_action, &params).await;
    assert_eq!(
        status, 200,
        "case {:?}/{name:?}: {} must succeed, got {status}: {body}",
        family.kind, family.create_action
    );
}

/// Embed `document` under `policy_name`.
async fn put_policy(
    router: &MultiServiceRouter,
    family: &Family,
    entity_name: &str,
    policy_name: &str,
    document: &str,
) -> (u16, String) {
    call(
        router,
        family.put_action,
        &[
            (family.name_member, entity_name),
            ("PolicyName", policy_name),
            ("PolicyDocument", document),
        ],
    )
    .await
}

/// Read one embedded policy back.
async fn get_policy(
    router: &MultiServiceRouter,
    family: &Family,
    entity_name: &str,
    policy_name: &str,
) -> (u16, String) {
    call(
        router,
        family.get_action,
        &[
            (family.name_member, entity_name),
            ("PolicyName", policy_name),
        ],
    )
    .await
}

/// The names the entity holds its inline policies under, as its own listing reports them.
async fn listed_names(
    router: &MultiServiceRouter,
    family: &Family,
    entity_name: &str,
) -> Vec<String> {
    let (status, body) = call(
        router,
        family.list_action,
        &[(family.name_member, entity_name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {:?}: {} must succeed, got {status}: {body}",
        family.kind, family.list_action
    );
    elements(&body, "member")
}

/// Assert that `reported` is the URL-encoded form of `stored`: percent-decoding it returns
/// the stored text, and the text on the wire is percent-encoded, which a raw-JSON body is
/// not.
///
/// The shape half is the crate's one definition of that check
/// (`tests/all/policy_document_encoding.rs`), so a document reported wrongly cannot pass here
/// and fail there.
fn assert_reported_document(case: &str, body: &str, stored: &str) {
    let decoded = document_element(body, "PolicyDocument");
    assert_eq!(
        decoded, stored,
        "case {case:?}: decoding the reported document must return the stored text, got \
         {decoded:?}"
    );
    assert_percent_encoded(case, &element(body, "PolicyDocument"));
}

/// One read: how the request spells the entity and the policy.
struct Spelling {
    case: &'static str,
    entity: &'static str,
    policy: &'static str,
}

/// The name the three families' entities are created under, and the name the policy is
/// embedded under. Both are drawn from the `^[\w+=,.@-]+$` alphabet entity and policy names
/// share (`smithy_query(query="operation", operation="PutRolePolicy")`) and both carry
/// letters, which is what a case rule generalises over.
const HELD_ENTITY: &str = "Case_Holder-1";
const HELD_POLICY: &str = "Bucket-Read.v1+x";

/// Rows spanning the spelling the account holds, both names folded down, both folded up,
/// each one folded on its own, and the inverted spelling of each.
const SPELLINGS: &[Spelling] = &[
    Spelling {
        case: "both names as the account holds them",
        entity: HELD_ENTITY,
        policy: HELD_POLICY,
    },
    Spelling {
        case: "both names in lower case",
        entity: "case_holder-1",
        policy: "bucket-read.v1+x",
    },
    Spelling {
        case: "both names in upper case",
        entity: "CASE_HOLDER-1",
        policy: "BUCKET-READ.V1+X",
    },
    Spelling {
        case: "the policy name folded, the entity name as held",
        entity: HELD_ENTITY,
        policy: "bucket-READ.v1+X",
    },
    Spelling {
        case: "the entity name folded, the policy name as held",
        entity: "cASE_hOLDER-1",
        policy: HELD_POLICY,
    },
    Spelling {
        case: "both names inverted",
        entity: "cASE_hOLDER-1",
        policy: "bUCKET-rEAD.V1+X",
    },
];

/// A read of an inline policy reports BOTH names as the request spelled them, whatever
/// spelling the account holds, and the document as the store holds it.
#[tokio::test]
async fn an_inline_policy_read_reports_the_names_the_request_spelled() {
    // The rows also differ along the form of the document: a compact one and one written
    // with the spaces between its tokens, since the encoding reports every byte.
    let documents = [
        (
            "a compact document",
            r#"{"Version":"2012-10-17","Statement":[{"Sid":"Read","Effect":"Allow","Action":"s3:GetObject","Resource":"*"}]}"#,
        ),
        (
            "a document written with spaces",
            r#"{"Version": "2012-10-17", "Statement": [{"Sid": "Read", "Effect": "Allow", "Action": "s3:GetObject", "Resource": "*"}]}"#,
        ),
    ];

    let router = router();
    for family in FAMILIES {
        create_entity(&router, family, HELD_ENTITY).await;
        for (document_case, document) in documents {
            let (status, body) =
                put_policy(&router, family, HELD_ENTITY, HELD_POLICY, document).await;
            assert_eq!(
                status, 200,
                "case {:?}/{document_case:?}: {} must succeed, got {status}: {body}",
                family.kind, family.put_action
            );

            for Spelling {
                case,
                entity,
                policy,
            } in SPELLINGS
            {
                let case = format!("{}/{document_case}/{case}", family.kind);
                let (status, body) = get_policy(&router, family, entity, policy).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: the read must reach the policy whatever the case, got \
                     {status}: {body}"
                );
                let reported_entity = element(&body, family.name_member);
                assert_eq!(
                    reported_entity, *entity,
                    "case {case:?}: expected the {} the request spelled, got \
                     {reported_entity:?}",
                    family.name_member
                );
                let reported_policy = element(&body, "PolicyName");
                assert_eq!(
                    reported_policy, *policy,
                    "case {case:?}: expected the PolicyName the request spelled, got \
                     {reported_policy:?}"
                );
                assert_reported_document(&case, &body, document);
            }
        }
    }
}

/// The ECHO is of the request, not of the store: a put under another spelling replaces the
/// one policy, the entity's own listing reports the spelling that put chose, and a read
/// still reports whatever spelling it was asked with.
#[tokio::test]
async fn the_echo_follows_the_request_and_the_listing_follows_the_store() {
    let first = r#"{"Statement":[{"Effect":"Allow","Action":"s3:GetObject"}]}"#;
    let replacement = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Deny","Action":"*"}]}"#;

    let router = router();
    for family in FAMILIES {
        create_entity(&router, family, HELD_ENTITY).await;
        let (status, body) = put_policy(&router, family, HELD_ENTITY, HELD_POLICY, first).await;
        assert_eq!(
            status, 200,
            "case {:?}: the first put must succeed, got {status}: {body}",
            family.kind
        );
        let listed = listed_names(&router, family, HELD_ENTITY).await;
        assert_eq!(
            listed,
            vec![HELD_POLICY.to_string()],
            "case {:?}: the listing must report the name the put chose, got {listed:?}",
            family.kind
        );

        // A put of the same name in another case REPLACES that policy rather than adding a
        // second one.
        let replacing_spelling = "BUCKET-READ.V1+X";
        let (status, body) = put_policy(
            &router,
            family,
            "case_holder-1",
            replacing_spelling,
            replacement,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: the replacing put must succeed, got {status}: {body}",
            family.kind
        );
        let listed = listed_names(&router, family, HELD_ENTITY).await;
        assert_eq!(
            listed,
            vec![replacing_spelling.to_string()],
            "case {:?}: the listing must report the one policy under the latest put's \
             spelling, got {listed:?}",
            family.kind
        );

        // Whatever the store now holds, the read restates the request.
        for (case, entity, policy) in [
            ("the original spelling", HELD_ENTITY, HELD_POLICY),
            ("the replacing spelling", "case_holder-1", replacing_spelling),
            ("a third spelling", "CASE_HOLDER-1", "Bucket-Read.V1+x"),
        ] {
            let case = format!("{}/{case}", family.kind);
            let (status, body) = get_policy(&router, family, entity, policy).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the replaced policy must read back, got {status}: {body}"
            );
            let reported_entity = element(&body, family.name_member);
            assert_eq!(
                reported_entity, entity,
                "case {case:?}: expected the {} the request spelled, got {reported_entity:?}",
                family.name_member
            );
            let reported_policy = element(&body, "PolicyName");
            assert_eq!(
                reported_policy, policy,
                "case {case:?}: expected the PolicyName the request spelled, got \
                 {reported_policy:?}"
            );
            assert_reported_document(&case, &body, replacement);
        }
    }
}
