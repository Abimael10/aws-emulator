// SPDX-License-Identifier: CC0-1.0

//! WHO an access point's policy may grant to, and WHICH resource it may apply
//! to, driven end to end over HTTP against S3 Control, Amazon S3 and IAM mounted
//! on one shared engine.
//!
//! The rules under test:
//!
//! * a resource-based policy states who its grants reach, so every value its
//!   statements name as a principal is a principal of the type it was named
//!   under, and every IAM identity it names by ARN is one this account's IAM
//!   holds — a grant to an identity that does not exist is a grant to nobody, so
//!   the document is refused `MalformedPolicy` quoting the submitted document's
//!   own member that names that principal — the type key and the value together,
//!   written as a policy member is — and nothing is stored;
//! * a resource-based policy governs the resource it is attached to, so every
//!   value its statements name as a resource is the access point the request
//!   addresses, or an object reached through it; anything else — a bucket, a
//!   different access point, a value that is no S3 ARN — is refused
//!   `MalformedPolicy` naming that value.
//!
//! The rows of each table differ along what its rule generalises over: the
//! principal TYPE a value is named under and the form the value takes, whether
//! the identity it names is one IAM holds, and which resource an ARN names.
//!
//! IAM is mounted because the identities are resolved against the account's own
//! IAM records: the roles and users these rows name are created through IAM's
//! own operations, over the same in-process callback the resolution takes in
//! production.

use crate::access_point_lifecycle::{xml_escape, Reply, ACCOUNT, NO_CONTENT, REGION, XMLNS};
use crate::object_store::ObjectStore;

/// The wire `<Code>` every refusal of a policy document carries.
const MALFORMED_POLICY: &str = "MalformedPolicy";

/// The sentence a document naming something that is no principal is refused
/// with, read from the surface that states this service's clauses.
const BAD_PRINCIPAL: &str = service_com_amazonaws_s3control_awss3controlservicev20180820::state::access_point_policy_actions::POLICY_INVALID_PRINCIPAL_MESSAGE;

/// The sentence a document naming a resource the access point does not reach is
/// refused with.
const BAD_RESOURCE: &str = service_com_amazonaws_s3control_awss3controlservicev20180820::state::access_point_policy_actions::POLICY_INVALID_RESOURCE_MESSAGE;

/// The access point every row below attaches a document to.
const ACCESS_POINT: &str = "principal-ap";

/// The bucket it fronts.
const BUCKET: &str = "reports";

/// The role this account's IAM holds, created before the rows run.
const HELD_ROLE: &str = "held-role";

/// The user this account's IAM holds.
const HELD_USER: &str = "held-user";

/// A trust document for a role created through IAM, which `CreateRole` requires.
const TRUST_DOCUMENT: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"s3.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

/// Stand the three services up, create the access point, and record the two
/// identities the accepted rows name.
async fn fixture() -> ObjectStore {
    let (store, iam) = ObjectStore::with_iam();
    store.create_bucket("the fronted bucket", REGION, BUCKET).await;
    let body = format!(
        "<CreateAccessPointRequest xmlns=\"{XMLNS}\"><Bucket>{}</Bucket>\
         </CreateAccessPointRequest>",
        xml_escape(BUCKET)
    );
    let created = store
        .s3control(
            REGION,
            "PUT",
            &format!("/v20180820/accesspoint/{ACCESS_POINT}"),
            &body,
        )
        .await;
    assert_eq!(
        created.status, 200,
        "the access point must be created first, got {} with body {}",
        created.status, created.body
    );
    iam.create_role()
        .role_name(HELD_ROLE)
        .assume_role_policy_document(TRUST_DOCUMENT)
        .send()
        .await
        .expect("the account's IAM records the role");
    iam.create_user()
        .user_name(HELD_USER)
        .send()
        .await
        .expect("the account's IAM records the user");
    store
}

/// Put `document` on the access point.
async fn put_policy(store: &ObjectStore, document: &str) -> Reply {
    let body = format!(
        "<PutAccessPointPolicyRequest xmlns=\"{XMLNS}\"><Policy>{}</Policy>\
         </PutAccessPointPolicyRequest>",
        xml_escape(document)
    );
    store
        .s3control(
            REGION,
            "PUT",
            &format!("/v20180820/accesspoint/{ACCESS_POINT}/policy"),
            &body,
        )
        .await
}

/// The document the access point carries, or `None` when it carries none.
async fn stored_policy(store: &ObjectStore) -> Option<String> {
    let read = store
        .s3control(
            REGION,
            "GET",
            &format!("/v20180820/accesspoint/{ACCESS_POINT}/policy"),
            "",
        )
        .await;
    read.text("Policy")
}

/// A one-statement document whose statement carries `members` beside the effect
/// and the action every row states.
fn policy(members: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":"s3:GetObject",{members}}}]}}"#
    )
}

/// The ARN of an IAM role of this account.
fn role_arn(name: &str) -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// The ARN of an IAM user of this account.
fn user_arn(name: &str) -> String {
    format!("arn:aws:iam::{ACCOUNT}:user/{name}")
}

/// The ARN of the objects reached through the access point this suite addresses.
fn access_point_objects() -> String {
    format!("arn:aws:s3:{REGION}:{ACCOUNT}:accesspoint/{ACCESS_POINT}/object/*")
}

/// The member of a submitted document a refusal quotes for the principal named under the
/// `AWS` type as `value`: the type key and the value, written as a policy member is.
fn aws_principal_member(value: &str) -> String {
    format!("\"AWS\" : \"{value}\"")
}

#[tokio::test]
async fn a_document_granting_to_an_identity_iam_does_not_hold_is_refused_naming_it() {
    let store = fixture().await;

    // The rows differ along the identity named and the IAM entity type it is
    // named under: a role and a user this account's IAM holds, the same two
    // named with a name IAM holds nothing under, and — because the refusal is
    // grounded in what IAM ANSWERED — the principals IAM is never asked about.
    let held_role = role_arn(HELD_ROLE);
    let held_user = user_arn(HELD_USER);
    let absent_role = role_arn("no-such-role");
    let absent_user = user_arn("no-such-user");
    let rows: Vec<(&str, String, Option<&str>)> = vec![
        ("a role this account's IAM holds", held_role.clone(), None),
        ("a user this account's IAM holds", held_user.clone(), None),
        (
            "both identities it holds, in one list",
            format!("{held_role}\",\"{held_user}"),
            None,
        ),
        ("the account's own root", format!("arn:aws:iam::{ACCOUNT}:root"), None),
        ("the account by its id", ACCOUNT.to_string(), None),
        // The wildcard principal is not a row here: it opens the access point to
        // everyone, which its own `BlockPublicPolicy` setting refuses before any
        // identity is resolved (the sibling public-access suite pins that).
        (
            "a role of ANOTHER account, whose IAM this is not",
            "arn:aws:iam::123456789012:role/partner".to_string(),
            None,
        ),
        (
            "a role name IAM holds nothing under",
            absent_role.clone(),
            Some(absent_role.as_str()),
        ),
        (
            "a user name IAM holds nothing under",
            absent_user.clone(),
            Some(absent_user.as_str()),
        ),
        (
            "a held identity beside an absent one",
            format!("{held_user}\",\"{absent_role}"),
            Some(absent_role.as_str()),
        ),
    ];

    // The document on the access point at each point in the table: a refused put
    // stores nothing, so it stays what the last ACCEPTED put left.
    let mut stored: Option<String> = None;
    for (case, named, refused) in rows {
        let document = policy(&format!(
            r#""Principal":{{"AWS":["{named}"]}},"Resource":"{}""#,
            access_point_objects()
        ));
        let put = put_policy(&store, &document).await;
        match refused {
            None => {
                assert_eq!(
                    put.status, NO_CONTENT,
                    "case {case:?}: expected the document to be stored, got {} with body {}",
                    put.status, put.body
                );
                stored = Some(document.clone());
            }
            Some(named) => {
                assert_eq!(
                    (put.status, put.code().as_str(), put.text("Message")),
                    (400, MALFORMED_POLICY, Some(BAD_PRINCIPAL.to_string())),
                    "case {case:?}: expected 400 {MALFORMED_POLICY} stating {BAD_PRINCIPAL:?}, \
                     got {} with body {}",
                    put.status,
                    put.body
                );
                assert_eq!(
                    put.text("Detail"),
                    Some(aws_principal_member(named)),
                    "case {case:?}: expected the refusal to quote the document's member naming \
                     {named:?}, got body {}",
                    put.body
                );
            }
        }
        assert_eq!(
            stored_policy(&store).await,
            stored,
            "case {case:?}: expected the access point to carry the last stored document"
        );
    }
}

#[tokio::test]
async fn a_document_naming_something_that_is_no_principal_is_refused_naming_it() {
    let store = fixture().await;

    // The rows differ along the FORM the principal element takes: the untyped
    // shorthand, which the language defines for the wildcard alone; a type the
    // language does not define; and a value outside the forms of each type it
    // does define. What the refusal quotes is the document's own member in each
    // case — the type key and the value, or the statement element itself where
    // the shorthand names no type.
    let rows: [(&str, &str, &str); 6] = [
        (
            "a bare word where only the wildcard is written untyped",
            r#""Principal":"everyone""#,
            r#""Principal" : "everyone""#,
        ),
        (
            "a type the policy language does not define",
            r#""Principal":{"Account":"123456789012"}"#,
            r#""Account" : "123456789012""#,
        ),
        (
            "an account id of the wrong length",
            r#""Principal":{"AWS":"1234"}"#,
            r#""AWS" : "1234""#,
        ),
        (
            "an ARN that names no identity",
            r#""Principal":{"AWS":"arn:aws:s3:::reports"}"#,
            r#""AWS" : "arn:aws:s3:::reports""#,
        ),
        (
            "a service principal that is no DNS name",
            r#""Principal":{"Service":"lambda"}"#,
            r#""Service" : "lambda""#,
        ),
        (
            "a canonical user id of the wrong length",
            r#""Principal":{"CanonicalUser":"abc"}"#,
            r#""CanonicalUser" : "abc""#,
        ),
    ];

    for (case, element, member) in rows {
        let put = put_policy(&store, &policy(element)).await;
        assert_eq!(
            (put.status, put.code().as_str(), put.text("Message")),
            (400, MALFORMED_POLICY, Some(BAD_PRINCIPAL.to_string())),
            "case {case:?}: expected 400 {MALFORMED_POLICY} stating {BAD_PRINCIPAL:?}, got {} \
             with body {}",
            put.status,
            put.body
        );
        assert_eq!(
            put.text("Detail").as_deref(),
            Some(member),
            "case {case:?}: expected the refusal to quote {member:?}, got body {}",
            put.body
        );
        assert_eq!(
            stored_policy(&store).await,
            None,
            "case {case:?}: expected the refused put to store nothing"
        );
    }
}

#[tokio::test]
async fn the_refusal_quotes_the_member_of_the_document_that_names_the_principal() {
    let store = fixture().await;
    let held = user_arn(HELD_USER);
    let absent = role_arn("no-such-role");
    let objects = access_point_objects();

    // Each row is a whole submitted document and the member its refusal must quote. The
    // rows differ along what the rule generalises over: the LAYOUT the caller wrote the
    // document in (compact, indented, a space before the separator, line breaks inside the
    // member), the form the principal element takes (one value, a one-value list, a list
    // whose second value is the offending one), which statement element names it
    // (`Principal` and its complement), which statement of several carries it, and whether
    // the clause is the identity not being there or the value being no principal of its
    // type. The quoted member is the same text in every layout: a stored document is
    // reported in the layout this service's policy engine writes, and so is a member of one
    // quoted back in a refusal.
    let rows: Vec<(&str, String, String)> = vec![
        (
            "a compact document",
            format!(
                r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":"s3:GetObject","Principal":{{"AWS":"{absent}"}},"Resource":"{objects}"}}]}}"#
            ),
            format!("\"AWS\" : \"{absent}\""),
        ),
        (
            "a document indented with a space after the separator",
            format!(
                "{{\n  \"Version\": \"2012-10-17\",\n  \"Statement\": [\n    {{\n      \
                 \"Effect\": \"Allow\",\n      \"Action\": \"s3:GetObject\",\n      \
                 \"Principal\": {{\n        \"AWS\": \"{absent}\"\n      }},\n      \
                 \"Resource\": \"{objects}\"\n    }}\n  ]\n}}"
            ),
            format!("\"AWS\" : \"{absent}\""),
        ),
        (
            "a document written with a space on both sides of the separator",
            format!(
                "{{ \"Version\" : \"2012-10-17\", \"Statement\" : [ {{ \"Effect\" : \"Allow\", \
                 \"Action\" : \"s3:GetObject\", \"Principal\" : {{ \"AWS\" : \"{absent}\" }}, \
                 \"Resource\" : \"{objects}\" }} ] }}"
            ),
            format!("\"AWS\" : \"{absent}\""),
        ),
        (
            "the identity written as a one-value list",
            format!(
                r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":"s3:GetObject","Principal":{{"AWS":["{absent}"]}},"Resource":"{objects}"}}]}}"#
            ),
            format!("\"AWS\" : \"{absent}\""),
        ),
        (
            "the offending identity second in a list beside a held one",
            format!(
                r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":"s3:GetObject","Principal":{{"AWS":["{held}","{absent}"]}},"Resource":"{objects}"}}]}}"#
            ),
            format!("\"AWS\" : \"{absent}\""),
        ),
        (
            "the identity named by the complement element",
            format!(
                r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Deny","Action":"s3:GetObject","NotPrincipal":{{"AWS":"{absent}"}},"Resource":"{objects}"}}]}}"#
            ),
            format!("\"AWS\" : \"{absent}\""),
        ),
        (
            "a value that is no principal of its type, in the second statement",
            format!(
                r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":"s3:GetObject","Principal":{{"AWS":"{held}"}},"Resource":"{objects}"}},{{"Effect":"Allow","Action":"s3:GetObject","Principal":{{"Service":"lambda"}},"Resource":"{objects}"}}]}}"#
            ),
            "\"Service\" : \"lambda\"".to_string(),
        ),
    ];

    for (case, document, member) in rows {
        let put = put_policy(&store, &document).await;
        assert_eq!(
            (put.status, put.code().as_str(), put.text("Message")),
            (400, MALFORMED_POLICY, Some(BAD_PRINCIPAL.to_string())),
            "case {case:?}: expected 400 {MALFORMED_POLICY} stating {BAD_PRINCIPAL:?}, got {} \
             with body {}",
            put.status,
            put.body
        );
        assert_eq!(
            put.text("Detail"),
            Some(member.clone()),
            "case {case:?}: expected the refusal to quote {member:?}, got body {}",
            put.body
        );
        assert_eq!(
            stored_policy(&store).await,
            None,
            "case {case:?}: expected the refused put to store nothing"
        );
    }
}

#[tokio::test]
async fn a_document_applies_to_the_access_point_it_is_attached_to_and_nothing_else() {
    let store = fixture().await;

    // The rows differ along which resource the value names: the access point
    // addressed in each form its ARN takes, and the values that name something
    // else — a bucket, that bucket's objects, another access point, the
    // language's bare wildcard, and a value that is no ARN at all.
    let objects = access_point_objects();
    let itself = format!("arn:aws:s3:{REGION}:{ACCOUNT}:accesspoint/{ACCESS_POINT}");
    let elsewhere = format!("arn:aws:s3:{REGION}:{ACCOUNT}:accesspoint/finance-ap");
    let rows: Vec<(&str, String, bool)> = vec![
        ("the objects reached through it", objects.clone(), true),
        ("the access point itself", itself.clone(), true),
        (
            "its ARN with neither Region nor account",
            format!("arn:aws:s3:::accesspoint/{ACCESS_POINT}"),
            true,
        ),
        (
            "its name written with the language's wildcard",
            format!("arn:aws:s3:{REGION}:{ACCOUNT}:accesspoint/principal-*/object/*"),
            true,
        ),
        ("a bucket's own ARN", format!("arn:aws:s3:::{BUCKET}"), false),
        (
            "the objects of the bucket it fronts",
            format!("arn:aws:s3:::{BUCKET}/*"),
            false,
        ),
        ("another access point", elsewhere.clone(), false),
        ("the language's bare wildcard", "*".to_string(), false),
        ("a value that is no ARN", BUCKET.to_string(), false),
        (
            "an ARN of another service",
            format!("arn:aws:sqs:{REGION}:{ACCOUNT}:queue"),
            false,
        ),
    ];

    let mut stored: Option<String> = None;
    for (case, resource, applies) in rows {
        let document = policy(&format!(
            r#""Principal":{{"AWS":"{}"}},"Resource":"{resource}""#,
            role_arn(HELD_ROLE)
        ));
        let put = put_policy(&store, &document).await;
        if applies {
            assert_eq!(
                put.status, NO_CONTENT,
                "case {case:?}: expected the document to be stored, got {} with body {}",
                put.status, put.body
            );
            stored = Some(document.clone());
        } else {
            assert_eq!(
                (put.status, put.code().as_str(), put.text("Message")),
                (400, MALFORMED_POLICY, Some(BAD_RESOURCE.to_string())),
                "case {case:?}: expected 400 {MALFORMED_POLICY} stating {BAD_RESOURCE:?}, got {} \
                 with body {}",
                put.status,
                put.body
            );
            assert_eq!(
                put.text("Detail").as_deref(),
                Some(resource.as_str()),
                "case {case:?}: expected the refusal to name {resource:?}, got body {}",
                put.body
            );
        }
        assert_eq!(
            stored_policy(&store).await,
            stored,
            "case {case:?}: expected the access point to carry the last stored document"
        );
    }
}
