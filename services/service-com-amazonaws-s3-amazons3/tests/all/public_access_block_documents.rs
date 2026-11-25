// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the DOCUMENT `PutPublicAccessBlock` carries, and for the ORDER the
//! two refusals an ACL write can earn are judged in. Every assertion drives a real HTTP
//! request through the router (the harness the bucket-lifecycle suite in this crate owns), so
//! what is pinned is the wire behaviour — status, error code, message and what a later read
//! reports — rather than any handler's internals.
//!
//! The rules under test, with their sources:
//!
//! * A request document that names an element the published schema does not declare where it
//!   arrived — misspelled, misnested or invented — does not validate against that schema, and
//!   neither do bytes no XML reader can walk: S3 answers `MalformedXML` (HTTP 400) under that
//!   code's own FIXED published sentence and records nothing. For a
//!   `PublicAccessBlockConfiguration` every member is OPTIONAL
//!   (`smithy_query(query="shape",shape="PublicAccessBlockConfiguration")` reports four
//!   boolean `Setting` members, none `@required`), so a codec that reads an unknown element by
//!   ignoring it parses such a document to a posture with that switch left OFF — which is why
//!   the document is judged from the bytes the request carried rather than from the
//!   configuration parsed out of it. What a switch's TEXT states is a different question: each
//!   of the four is published as "setting this element to TRUE causes the following behavior",
//!   so text that states no true — read in any case, the spelling that documentation uses —
//!   leaves that switch OFF, the reading its own absence has, and the document is served
//!   ([`super::zzz_batch_probe`] drives every spelling of one).
//! * A document the published schema DOES declare is stored in full: "creates or modifies the
//!   PublicAccessBlock configuration for an Amazon S3 bucket" (`PutPublicAccessBlock`), so
//!   what the document names becomes the bucket's configuration — a replacement, not a merge —
//!   and a switch the document omits is one the request does not turn on.
//! * Block Public Access is a GUARDRAIL over the ACL a request names: with `BlockPublicAcls`
//!   on, "PUT Bucket ACL and PUT Object ACL calls fail if the specified ACL is public"
//!   (`PublicAccessBlockConfiguration$BlockPublicAcls`). It is an authorization decision, so
//!   it is taken AHEAD of the bucket's Object Ownership precondition ("requests to set ACLs or
//!   update ACLs fail and return the AccessControlListNotSupported error code",
//!   `PutBucketAcl`): a public ACL is denied 403 `AccessDenied` naming the principal, the
//!   action, the resource and the setting that prevented it, whether or not ACLs are disabled
//!   on the bucket, and the `AccessControlListNotSupported` refusal is what is left for an ACL
//!   the guardrail does not deny.

use std::collections::{BTreeMap, BTreeSet};

use service_com_amazonaws_s3_amazons3::state::{
    block_public_acls_denial, bucket_arn, caller_principal_arn, canonical_user_id,
    ALL_USERS_GROUP_URI, ACL_NOT_SUPPORTED_CODE, ACL_NOT_SUPPORTED_MESSAGE, ACL_REFUSAL_STATUS,
    MALFORMED_XML_CODE, MALFORMED_XML_MESSAGE, MALFORMED_XML_STATUS, PERMISSION_READ,
    PUBLIC_ACCESS_BLOCK_DOCUMENT_SCHEMA,
};
use smithy_server_runtime::MultiServiceRouter;

use super::bucket_acl::{access_control_policy, put_acl, GrantXml};
use super::bucket_lifecycle::{create_bucket, router, send, Answer};
use super::bucket_ownership_controls::put_ownership_controls;
use super::bucket_public_access_block::{
    allow_public_access, get_public_access_block, reported_flags, SETTINGS,
};

/// The XML namespace botocore puts on an S3 request payload.
const XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The four switches a configuration reports, in [`SETTINGS`] order.
type Flags = [bool; 4];

/// The posture every bucket is created with: all four switches on.
const ALL_ON: Flags = [true, true, true, true];

/// `PUT /<bucket>?publicAccessBlock` with `document` as the request body EXACTLY as written —
/// what a document rule has to be driven with, since the point of each case is the bytes the
/// caller sent.
async fn put_document(router: &MultiServiceRouter, bucket: &str, document: &str) -> Answer {
    send(
        router,
        "PUT",
        &format!("/{bucket}?publicAccessBlock"),
        &[],
        document,
    )
    .await
}

/// The posture `bucket` reports now, with every switch required to be present: the
/// configuration is the whole response payload, so a reader that reports fewer than four
/// switches fails loudly here instead of defaulting to `false`.
async fn posture(router: &MultiServiceRouter, case: &str, bucket: &str) -> Flags {
    let read = get_public_access_block(router, bucket, &[]).await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: the configuration must read back, got {} with body {:?}",
        read.status,
        String::from_utf8_lossy(&read.body)
    );
    let reported = reported_flags(&read);
    let mut flags: Flags = [false; 4];
    for (index, setting) in SETTINGS.iter().enumerate() {
        flags[index] = reported[index].unwrap_or_else(|| {
            panic!(
                "case {case:?}: the reported configuration must carry {setting}, got body {:?}",
                String::from_utf8_lossy(&read.body)
            )
        });
    }
    flags
}

/// A document that does not validate against the published schema is refused, and the bucket
/// keeps the posture it already had.
///
/// The rows differ along the WAY a document fails to validate — an element the schema declares
/// nowhere, one it declares but not in the position it arrived in, a root it does not declare,
/// an element left unclosed and bytes that are not one XML document — and each of them names a
/// switch the schema DOES declare, so an emulator that read what it could out of the document
/// would store a posture weaker than the one the bucket already carries.
#[tokio::test]
async fn a_public_access_block_document_outside_the_published_schema_is_refused() {
    let router = router();
    let bucket = "pab-document-refusals";
    create_bucket(&router, bucket, None, None).await;
    assert_eq!(
        posture(&router, "the created bucket", bucket).await,
        ALL_ON,
        "a bucket is created with every switch on, which is what each refused row must leave"
    );

    for (case, document) in [
        (
            "a switch whose name is misspelled",
            format!(
                "<PublicAccessBlockConfiguration xmlns=\"{XMLNS}\">\
                 <BlockPublicAcl>false</BlockPublicAcl>\
                 </PublicAccessBlockConfiguration>"
            ),
        ),
        (
            "an element the schema declares nowhere",
            format!(
                "<PublicAccessBlockConfiguration xmlns=\"{XMLNS}\">\
                 <BlockPublicAcls>false</BlockPublicAcls>\
                 <BlockEverything>true</BlockEverything>\
                 </PublicAccessBlockConfiguration>"
            ),
        ),
        (
            "a declared switch one level too deep",
            format!(
                "<PublicAccessBlockConfiguration xmlns=\"{XMLNS}\">\
                 <PublicAccessBlockConfiguration>\
                 <BlockPublicAcls>false</BlockPublicAcls>\
                 </PublicAccessBlockConfiguration>\
                 </PublicAccessBlockConfiguration>"
            ),
        ),
        (
            "every switch inside a container the schema does not declare",
            format!(
                "<PublicAccessBlockConfiguration xmlns=\"{XMLNS}\"><Settings>\
                 <BlockPublicAcls>false</BlockPublicAcls>\
                 <IgnorePublicAcls>false</IgnorePublicAcls>\
                 </Settings></PublicAccessBlockConfiguration>"
            ),
        ),
        (
            "a root element the schema does not declare",
            format!(
                "<PublicAccessBlock xmlns=\"{XMLNS}\">\
                 <BlockPublicAcls>false</BlockPublicAcls>\
                 </PublicAccessBlock>"
            ),
        ),
        (
            "a switch written beside the container instead of inside it",
            format!(
                "<PublicAccessBlockConfiguration xmlns=\"{XMLNS}\">\
                 </PublicAccessBlockConfiguration>\
                 <BlockPublicAcls>false</BlockPublicAcls>"
            ),
        ),
        (
            "a container left unclosed, which is not one XML document",
            format!(
                "<PublicAccessBlockConfiguration xmlns=\"{XMLNS}\">\
                 <BlockPublicAcls>false</BlockPublicAcls>"
            ),
        ),
        (
            "a body that is not markup at all",
            "{\"BlockPublicAcls\": false}".to_string(),
        ),
    ] {
        let refused = put_document(&router, bucket, &document).await;
        assert_eq!(
            refused.status, MALFORMED_XML_STATUS,
            "case {case:?}: expected HTTP {MALFORMED_XML_STATUS}, got {} with body {:?}",
            refused.status,
            String::from_utf8_lossy(&refused.body)
        );
        assert_eq!(
            refused.error_code(),
            MALFORMED_XML_CODE,
            "case {case:?}: expected {MALFORMED_XML_CODE}, got {:?} with body {:?}",
            refused.error_code(),
            String::from_utf8_lossy(&refused.body)
        );
        assert_eq!(
            refused.error_message(),
            MALFORMED_XML_MESSAGE,
            "case {case:?}: expected that code's own published sentence, got {:?}",
            refused.error_message()
        );
        assert_eq!(
            posture(&router, case, bucket).await,
            ALL_ON,
            "case {case:?}: a refused document records nothing, so the bucket must still \
             carry every switch it had"
        );
    }
}

/// A document the published schema admits is stored in full and read back — including one
/// that names only some of the switches, and one that names none at all.
///
/// The rows differ along what the rule generalises over: WHICH of the four switches the
/// document names, HOW MANY of them, the ORDER it names them in, the SPELLING it states each
/// value in (including text that states no true, which leaves that switch off) and whether it
/// carries the namespace a client's XML writer puts on the container. Each row is
/// sent to the SAME bucket, so each one also pins that the document replaces the posture
/// rather than merging into it.
#[tokio::test]
async fn a_public_access_block_document_the_schema_admits_is_stored_in_full() {
    let router = router();
    let bucket = "pab-document-round-trip";
    create_bucket(&router, bucket, None, None).await;

    for (case, document, expected) in [
        (
            "every switch, all on",
            format!(
                "<PublicAccessBlockConfiguration xmlns=\"{XMLNS}\">\
                 <BlockPublicAcls>true</BlockPublicAcls>\
                 <IgnorePublicAcls>true</IgnorePublicAcls>\
                 <BlockPublicPolicy>true</BlockPublicPolicy>\
                 <RestrictPublicBuckets>true</RestrictPublicBuckets>\
                 </PublicAccessBlockConfiguration>"
            ),
            [true, true, true, true],
        ),
        (
            "one switch alone, on — the three it omits are not turned on",
            format!(
                "<PublicAccessBlockConfiguration xmlns=\"{XMLNS}\">\
                 <BlockPublicAcls>true</BlockPublicAcls>\
                 </PublicAccessBlockConfiguration>"
            ),
            [true, false, false, false],
        ),
        (
            "the switches in reverse document order, mixed values",
            format!(
                "<PublicAccessBlockConfiguration xmlns=\"{XMLNS}\">\
                 <RestrictPublicBuckets>true</RestrictPublicBuckets>\
                 <BlockPublicPolicy>false</BlockPublicPolicy>\
                 <IgnorePublicAcls>true</IgnorePublicAcls>\
                 <BlockPublicAcls>false</BlockPublicAcls>\
                 </PublicAccessBlockConfiguration>"
            ),
            [false, true, false, true],
        ),
        (
            "the policy half alone, with no namespace on the container",
            "<PublicAccessBlockConfiguration>\
             <BlockPublicPolicy>true</BlockPublicPolicy>\
             <RestrictPublicBuckets>true</RestrictPublicBuckets>\
             </PublicAccessBlockConfiguration>"
                .to_string(),
            [false, false, true, true],
        ),
        (
            "a container naming no switch at all, which turns none of them on",
            format!("<PublicAccessBlockConfiguration xmlns=\"{XMLNS}\"></PublicAccessBlockConfiguration>"),
            [false, false, false, false],
        ),
        (
            "each switch in a different spelling of the boolean it states",
            format!(
                "<PublicAccessBlockConfiguration xmlns=\"{XMLNS}\">\
                 <BlockPublicAcls>1</BlockPublicAcls>\
                 <IgnorePublicAcls>0</IgnorePublicAcls>\
                 <BlockPublicPolicy> true </BlockPublicPolicy>\
                 <RestrictPublicBuckets>false</RestrictPublicBuckets>\
                 </PublicAccessBlockConfiguration>"
            ),
            [true, false, true, false],
        ),
        (
            "a switch whose text states no true, which leaves it off",
            format!(
                "<PublicAccessBlockConfiguration xmlns=\"{XMLNS}\">\
                 <BlockPublicAcls>yes</BlockPublicAcls>\
                 <IgnorePublicAcls>true</IgnorePublicAcls>\
                 </PublicAccessBlockConfiguration>"
            ),
            [false, true, false, false],
        ),
        (
            "a switch written self-closing, which states no true either",
            format!(
                "<PublicAccessBlockConfiguration xmlns=\"{XMLNS}\">\
                 <BlockPublicAcls/>\
                 <BlockPublicPolicy>true</BlockPublicPolicy>\
                 </PublicAccessBlockConfiguration>"
            ),
            [false, false, true, false],
        ),
    ] {
        let stored = put_document(&router, bucket, &document).await;
        assert_eq!(
            stored.status, 200,
            "case {case:?}: the published schema admits this document, so it must be stored, \
             got {} with body {:?}",
            stored.status,
            String::from_utf8_lossy(&stored.body)
        );
        assert!(
            stored.body.is_empty(),
            "case {case:?}: the modelled output is Unit, so the body must be empty, got {:?}",
            String::from_utf8_lossy(&stored.body)
        );
        assert_eq!(
            posture(&router, case, bucket).await,
            expected,
            "case {case:?}: the document's own switches must be what the bucket reports"
        );
    }
}

/// The grantee a grant header names the AllUsers group by, in the `type=value` form such a
/// header carries ([`ALL_USERS_GROUP_URI`] under the `uri` type) — asserted against that
/// constant by the schema test below rather than taken on trust.
const ALL_USERS_GRANT: &str = "uri=http://acs.amazonaws.com/groups/global/AllUsers";

/// One way a request names an ACL: the case, the headers it travels in, and the body.
type AclForm = (&'static str, Vec<(&'static str, &'static str)>, String);

/// The three ways one ACL write can name a PUBLIC ACL, each as the headers and body it
/// travels in.
fn public_acl_forms() -> Vec<AclForm> {
    vec![
        (
            "a public canned ACL",
            vec![("x-amz-acl", "public-read")],
            String::new(),
        ),
        (
            "a grant header naming the AllUsers group",
            vec![("x-amz-grant-read", ALL_USERS_GRANT)],
            String::new(),
        ),
        (
            "an AccessControlPolicy body granting the AllUsers group",
            vec![],
            access_control_policy(
                None,
                &[GrantXml::group(ALL_USERS_GROUP_URI, PERMISSION_READ)],
            ),
        ),
    ]
}

/// The IAM action `PutBucketAcl` is performed under — `s3:<OperationName>`, as the IAM Service
/// Authorization Reference names the actions of Amazon S3 — which the guardrail's refusal
/// names.
const PUT_BUCKET_ACL_ACTION: &str = "s3:PutBucketAcl";

/// With `BlockPublicAcls` in force, a public ACL is denied for being public — whatever the
/// bucket's Object Ownership setting says about ACLs.
///
/// The rows differ along the two things the rule generalises over: the FORM the public ACL
/// arrives in (a canned ACL, a grant header, an `AccessControlPolicy` document) and the
/// bucket's Object Ownership setting (the enforced default, under which ACLs are disabled, and
/// `ObjectWriter`, under which they affect permissions). The refusal is the same for all of
/// them, which is what makes the guardrail the decision taken first.
#[tokio::test]
async fn a_public_acl_is_denied_by_the_guardrail_before_the_ownership_precondition() {
    let router = router();

    // (case, the ownership setting the bucket is switched to, or None for the default)
    for (index, (ownership_case, ownership)) in [
        ("the enforced default, under which ACLs are disabled", None),
        (
            "the ObjectWriter setting, under which ACLs affect permissions",
            Some("ObjectWriter"),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        for (form_index, (form, headers, body)) in public_acl_forms().into_iter().enumerate() {
            let bucket = format!("pab-acl-order-{index}-{form_index}");
            create_bucket(&router, &bucket, None, None).await;
            if let Some(ownership) = ownership {
                let enabled = put_ownership_controls(&router, &bucket, &[ownership]).await;
                assert_eq!(
                    enabled.status, 200,
                    "case {ownership_case:?}: the bucket must take the {ownership} setting, \
                     got {} with body {:?}",
                    enabled.status,
                    String::from_utf8_lossy(&enabled.body)
                );
            }

            let case = format!("{form} on a bucket with {ownership_case}");
            let denied = put_acl(&router, &bucket, &headers, &body).await;
            assert_eq!(
                denied.status, 403,
                "case {case:?}: the guardrail denies the call, got {} with body {:?}",
                denied.status,
                String::from_utf8_lossy(&denied.body)
            );
            assert_eq!(
                denied.error_code(),
                "AccessDenied",
                "case {case:?}: expected the guardrail's own code, got {:?} with body {:?}",
                denied.error_code(),
                String::from_utf8_lossy(&denied.body)
            );
            assert_eq!(
                denied.error_message(),
                block_public_acls_denial(
                    &caller_principal_arn(),
                    PUT_BUCKET_ACL_ACTION,
                    &bucket_arn(&bucket)
                ),
                "case {case:?}: the refusal names the principal, the action, the resource and \
                 the setting that prevented it, got {:?}",
                denied.error_message()
            );
        }
    }
}

/// Once the guardrail is lifted, the SAME public ACL is answered about the bucket's Object
/// Ownership setting: refused `AccessControlListNotSupported` where ACLs are disabled, and
/// served where they affect permissions.
///
/// The rows differ along the same two axes as the guardrail table above, so the pair of tests
/// pins the ORDER rather than either refusal alone.
#[tokio::test]
async fn without_the_guardrail_a_public_acl_is_answered_about_object_ownership() {
    let router = router();

    for (index, (ownership_case, ownership, served)) in [
        ("the enforced default, under which ACLs are disabled", None, false),
        (
            "the ObjectWriter setting, under which ACLs affect permissions",
            Some("ObjectWriter"),
            true,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        for (form_index, (form, headers, body)) in public_acl_forms().into_iter().enumerate() {
            let bucket = format!("pab-acl-lifted-{index}-{form_index}");
            create_bucket(&router, &bucket, None, None).await;
            if let Some(ownership) = ownership {
                put_ownership_controls(&router, &bucket, &[ownership]).await;
            }
            allow_public_access(&router, &bucket).await;

            let case = format!("{form} on a bucket with {ownership_case}");
            let answer = put_acl(&router, &bucket, &headers, &body).await;
            if served {
                assert_eq!(
                    answer.status, 200,
                    "case {case:?}: with the guardrail lifted and ACLs enabled the write is \
                     served, got {} with body {:?}",
                    answer.status,
                    String::from_utf8_lossy(&answer.body)
                );
                continue;
            }
            assert_eq!(
                answer.status, ACL_REFUSAL_STATUS,
                "case {case:?}: expected HTTP {ACL_REFUSAL_STATUS}, got {} with body {:?}",
                answer.status,
                String::from_utf8_lossy(&answer.body)
            );
            assert_eq!(
                answer.error_code(),
                ACL_NOT_SUPPORTED_CODE,
                "case {case:?}: expected {ACL_NOT_SUPPORTED_CODE}, got {:?}",
                answer.error_code()
            );
            assert_eq!(
                answer.error_message(),
                ACL_NOT_SUPPORTED_MESSAGE,
                "case {case:?}: expected that code's own published sentence, got {:?}",
                answer.error_message()
            );
        }
    }
}

/// An ACL the guardrail does not deny is answered about Object Ownership even with every
/// switch on: the guardrail is about a PUBLIC ACL, so an ACL that reaches no further than the
/// bucket owner is not what it refuses.
#[tokio::test]
async fn an_acl_the_guardrail_does_not_deny_is_answered_about_object_ownership() {
    let router = router();
    let owner = canonical_user_id();

    // Neither row names a grantee beyond the bucket's owner, so neither is a public ACL.
    for (index, (case, headers, body)) in [
        (
            "the private canned ACL",
            vec![("x-amz-acl", "private")],
            String::new(),
        ),
        (
            "an AccessControlPolicy body granting the owner alone",
            vec![],
            access_control_policy(None, &[GrantXml::user(&owner, PERMISSION_READ)]),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let bucket = format!("pab-acl-not-public-{index}");
        create_bucket(&router, &bucket, None, None).await;

        let refused = put_acl(&router, &bucket, &headers, &body).await;
        assert_eq!(
            refused.error_code(),
            ACL_NOT_SUPPORTED_CODE,
            "case {case:?}: the guardrail denies only a public ACL, so this one is answered \
             about the bucket's ownership setting, got {:?} with status {}",
            refused.error_code(),
            refused.status
        );
        assert_eq!(
            refused.status, ACL_REFUSAL_STATUS,
            "case {case:?}: expected HTTP {ACL_REFUSAL_STATUS}, got {}",
            refused.status
        );
    }
}

/// The schema the Block Public Access document is walked against, re-derived from the model
/// itself: the root is the element the operation's own payload member is written under, and its
/// children are that container's members under the names restXml spells them with.
///
/// Source: the model this crate ships, read here rather than restated — so a model update that
/// renames, adds or removes one of these elements cannot leave the table behind.
#[test]
fn the_public_access_block_document_schema_matches_the_model() {
    let model_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/smithy-model/com-amazonaws-s3-amazons3.json"
    );
    let model: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(model_path).expect("the service's Smithy model must be readable"),
    )
    .expect("the Smithy model must parse as JSON");
    let shapes = model["shapes"]
        .as_object()
        .expect("the model must carry a shapes object");

    /// The element a member is written under: its `@xmlName`, else its own name.
    fn element_of(name: &str, member: &serde_json::Value) -> String {
        member["traits"]["smithy.api#xmlName"]
            .as_str()
            .unwrap_or(name)
            .to_string()
    }

    let payload =
        &shapes["com.amazonaws.s3#PutPublicAccessBlockRequest"]["members"]
            ["PublicAccessBlockConfiguration"];
    let root = element_of("PublicAccessBlockConfiguration", payload);
    let container = &shapes[payload["target"]
        .as_str()
        .expect("the payload member names a shape")];
    let members = container["members"]
        .as_object()
        .expect("the payload container is a structure");

    let mut derived: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let children = derived.entry(root.clone()).or_default();
    for (name, member) in members {
        children.insert(element_of(name, member));
        // Each switch targets a boolean, so it is a LEAF: it carries text, not elements, and
        // the table declares no row for it.
        assert_eq!(
            shapes[member["target"].as_str().expect("a member names a shape")]["type"],
            "boolean",
            "every member of the container is a boolean switch, so {name} must be one"
        );
        // And none of them is required, which is why the schema requires no child.
        assert!(
            member["traits"].get("smithy.api#required").is_none(),
            "no member of the container is required, so {name} must not be"
        );
    }

    assert_eq!(
        PUBLIC_ACCESS_BLOCK_DOCUMENT_SCHEMA[0].0, root,
        "the schema's first row is the payload container, got {:?}",
        PUBLIC_ACCESS_BLOCK_DOCUMENT_SCHEMA[0].0
    );
    let declared: BTreeMap<String, BTreeSet<String>> = PUBLIC_ACCESS_BLOCK_DOCUMENT_SCHEMA
        .iter()
        .map(|(element, children)| {
            (
                (*element).to_string(),
                children.iter().map(|child| (*child).to_string()).collect(),
            )
        })
        .collect();
    assert_eq!(
        declared, derived,
        "PUBLIC_ACCESS_BLOCK_DOCUMENT_SCHEMA must declare exactly the container's own \
         elements; declared {declared:?}, the model reports {derived:?}"
    );

    // The elements the suite above sends are the same ones, so the switch list the round-trip
    // table drives is the model's own.
    let settings: BTreeSet<String> = SETTINGS.iter().map(|name| (*name).to_string()).collect();
    assert_eq!(
        settings,
        derived[&root],
        "the suite's switch list must be the container's own members, got {settings:?}"
    );

    // And the grant header the ACL tables send names the predefined group by the URI the
    // service publishes for it, in the `type=value` form such a header carries.
    assert_eq!(
        ALL_USERS_GRANT,
        format!("uri={ALL_USERS_GROUP_URI}"),
        "the grant header must name the AllUsers group by its published URI, got {ALL_USERS_GRANT:?}"
    );
}
