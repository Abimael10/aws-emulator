// SPDX-License-Identifier: CC0-1.0

//! The SCOPE a tagging `resourceArn` is read against: its partition and its Region.
//!
//! A child suite of `brand_profile_lifecycle`, which owns the tag read and the rest of
//! the ARN resolution every tagging call shares; this file owns the one refusal that
//! reports a scope the request cannot reach. Declared by its parent, because this
//! crate's one test target declares the suites directly under `tests/all/`.
//!
//! The rule under test, over a SET of inputs rather than one recorded request: a
//! well-formed ARN of this service whose partition or Region field is not the one the
//! request was made in names no resource of that request and is refused `400`
//! `ValidationException`, reporting the expected and the named partition and Region as
//! four separate quoted tokens — the expected pair taken from the scope the request was
//! made in, the named pair parsed out of the request's own `resourceArn`. The refusal is
//! the shared resolution's, so the tag READ and the tag WRITE word it identically, and
//! an ARN whose scope DOES agree is served.

use serde_json::json;

use super::{reported_tags, sorted, tag_body, Emulator, Reply};

/// The ARN fields a row addresses a resource through: the scope the refusal reports,
/// beside the request's own.
#[derive(Debug, Clone)]
struct Scope {
    partition: String,
    region: String,
}

/// `arn:<partition>:end-user-messaging:<region>:<account>:<resource>` — the ARN of
/// `resource` in `scope`, rebuilt field by field rather than by rewriting the text of a
/// minted ARN, so a row changes exactly the field it means to.
fn arn_in(scope: &Scope, account: &str, resource: &str) -> String {
    format!(
        "arn:{}:{}:{}:{account}:{resource}",
        scope.partition, super::SIGNING_NAME, scope.region
    )
}

/// The partition, Region, account and resource segments of `arn`.
///
/// The resource field is everything after the fifth colon, so a resource spelling that
/// carries colons of its own survives the split.
fn fields_of(arn: &str) -> (Scope, String, String) {
    let mut fields = arn.splitn(6, ':');
    assert_eq!(
        fields.next(),
        Some("arn"),
        "the minted ARN must begin with the arn literal, got {arn:?}"
    );
    let partition = fields.next().expect("a minted ARN names a partition");
    let vendor = fields.next().expect("a minted ARN names a vendor");
    assert_eq!(
        vendor,
        super::SIGNING_NAME,
        "the minted ARN must name this service's namespace, got {arn:?}"
    );
    let region = fields.next().expect("a minted ARN names a Region");
    let account = fields.next().expect("a minted ARN names an account");
    let resource = fields.next().expect("a minted ARN names a resource");
    (
        Scope {
            partition: partition.to_string(),
            region: region.to_string(),
        },
        account.to_string(),
        resource.to_string(),
    )
}

/// The sentence this API refuses an ARN of another scope with: the expected partition
/// and Region, then the ones the ARN named, each as its own quoted token.
fn scope_mismatch_message(expected: &Scope, named: &Scope) -> String {
    format!(
        "ARN partition/region mismatch: expected partition '{}' region '{}', got partition \
         '{}' region '{}'",
        expected.partition, expected.region, named.partition, named.region
    )
}

/// `TagResource` on `resource_arn`: the tag WRITE, which resolves an ARN through the
/// same path the read does.
async fn tag(em: &Emulator, resource_arn: &str) -> Reply {
    em.call(
        "POST",
        &format!(
            "/v1/tags/{}",
            crate::brand_profile_default_attributes::path_label(resource_arn)
        ),
        Some(json!({ "tags": tag_body(&[("owner".to_string(), "writer".to_string())]) })),
    )
    .await
}

/// The scopes a row addresses the resource through, given the scope the request was made
/// in.
///
/// The rows span what the rule generalises over — which FIELD of the pair disagrees, and
/// how: another Region of the same partition (both a nearby and a far one), another
/// partition at the same Region (each of the partitions `AmazonResourceName`'s
/// `^arn:[A-Za-z0-9_:/-]+$` admits), both fields at once, and the partition-global
/// spelling, whose Region field is empty.
fn foreign_scopes(request: &Scope) -> Vec<(String, Scope)> {
    // Two Region codes that are not the request's own, drawn from real AWS Region
    // names; which two they are does not matter, only that they differ from it.
    let elsewhere: Vec<&str> = ["us-east-1", "us-west-2", "eu-west-1", "ap-southeast-2"]
        .into_iter()
        .filter(|region| *region != request.region)
        .take(2)
        .collect();
    let other_region = elsewhere[0];
    let mut rows = vec![(
        "the partition-global spelling, whose Region field is empty".to_string(),
        Scope {
            partition: request.partition.clone(),
            region: String::new(),
        },
    )];
    for region in &elsewhere {
        rows.push((
            format!("the {region} Region of the request's own partition"),
            Scope {
                partition: request.partition.clone(),
                region: (*region).to_string(),
            },
        ));
    }
    for partition in ["aws", "aws-cn", "aws-us-gov", "aws-iso", "aws-iso-b"] {
        if partition == request.partition {
            continue;
        }
        rows.push((
            format!("the {partition} partition at the request's own Region"),
            Scope {
                partition: partition.to_string(),
                region: request.region.clone(),
            },
        ));
        rows.push((
            format!("the {partition} partition and another Region"),
            Scope {
                partition: partition.to_string(),
                region: other_region.to_string(),
            },
        ));
    }
    rows
}

#[tokio::test]
async fn an_arn_outside_the_requests_scope_is_refused_as_a_scope_mismatch() {
    // Every row is a real AWS Region name: the rule holds for whichever Region the
    // request was made in, so the whole table is driven once per request scope.
    for request_region in ["us-east-1", "eu-west-1", "ap-southeast-2", "us-gov-west-1"] {
        let em = Emulator::in_region(request_region);
        let tags = vec![("team".to_string(), "alpha".to_string())];
        let created = em
            .create(json!({
                "brandProfileName": "scoped-profile",
                "tags": tag_body(&tags),
            }))
            .await;
        assert_eq!(
            created.status, 200,
            "case {request_region}: the create must succeed, got {} / {:?}",
            created.status, created.body
        );
        let arn = created.string("brandProfileArn");
        let (minted, account, resource) = fields_of(&arn);
        assert_eq!(
            minted.region, request_region,
            "case {request_region}: a minted ARN carries the request's own Region, got {arn:?}"
        );
        // The scope the refusal must report as EXPECTED is the one the request was made
        // in: the partition this emulator serves — read off the ARN it just minted
        // rather than written here — and the request's own Region.
        let expected = Scope {
            partition: minted.partition.clone(),
            region: request_region.to_string(),
        };

        // Positive control: the resource's own ARN is in scope and reports its tags.
        let listed = em.list_tags(&arn).await;
        assert_eq!(
            listed.status, 200,
            "case {request_region}: the resource's own ARN must be reachable, got {} / {:?}",
            listed.status, listed.body
        );
        assert_eq!(
            reported_tags(&listed),
            sorted(&tags),
            "case {request_region}: unexpected tags for {arn}, got {:?}",
            listed.body
        );

        for (row, named) in foreign_scopes(&expected) {
            let foreign = arn_in(&named, &account, &resource);
            let case = format!("request={request_region} row={row} arn={foreign}");
            let message = scope_mismatch_message(&expected, &named);

            // The refusal belongs to the resolution both tagging calls share, so the
            // read and the write answer it identically.
            for (operation, refused) in [
                ("ListTagsForResource", em.list_tags(&foreign).await),
                ("TagResource", tag(&em, &foreign).await),
            ] {
                assert_eq!(
                    refused.status, 400,
                    "case {case} on {operation}: an ARN outside the request's scope is a \
                     ValidationException (400), got {} / {:?}",
                    refused.status, refused.body
                );
                assert_eq!(
                    refused.error.as_deref(),
                    Some("ValidationException"),
                    "case {case} on {operation}: unexpected error code, got {:?}",
                    refused.error
                );
                assert_eq!(
                    refused.string("message"),
                    message,
                    "case {case} on {operation}: expected {message:?}, got {:?}",
                    refused.body
                );
                assert_eq!(
                    refused.member("fieldList")[0]["path"],
                    json!("/resourceArn"),
                    "case {case} on {operation}: the refusal must name the member it \
                     refused, got {:?}",
                    refused.body
                );
                assert_eq!(
                    refused.member("fieldList")[0]["message"],
                    json!(message),
                    "case {case} on {operation}: the entry on the refused member must state \
                     the same mismatch, got {:?}",
                    refused.body
                );
            }

            // A refused write leaves the resource's own tags alone: the scope is judged
            // before anything is looked up or written.
            let after = em.list_tags(&arn).await;
            assert_eq!(
                reported_tags(&after),
                sorted(&tags),
                "case {case}: a refused write must leave the stored tags alone, got {:?}",
                after.body
            );
        }
    }
}

#[tokio::test]
async fn a_scope_mismatch_is_reported_for_every_taggable_resource_type() {
    // The rule is about the ARN's scope, not about one resource type, so the other
    // taggable resource of this service answers it the same way — and so does an ARN
    // of a resource type this store holds no record of at all, since the scope is
    // judged before any lookup.
    let em = Emulator::new();
    let created = em
        .create_notify_code_configuration(json!({
            "notifyCodeConfigurationName": "scoped-configuration",
            "tags": tag_body(&[("team".to_string(), "alpha".to_string())]),
        }))
        .await;
    assert_eq!(
        created.status, 200,
        "the create must succeed, got {} / {:?}",
        created.status, created.body
    );
    let arn = created.member("notifyCodeConfiguration")["notifyCodeConfigurationArn"]
        .as_str()
        .unwrap_or_else(|| panic!("the create reported no ARN, got {:?}", created.body))
        .to_string();
    let (minted, account, resource) = fields_of(&arn);
    let expected = Scope {
        partition: minted.partition.clone(),
        region: minted.region.clone(),
    };

    let reachable = em.list_tags(&arn).await;
    assert_eq!(
        reachable.status, 200,
        "the configuration's own ARN must be reachable, got {} / {:?}",
        reachable.status, reachable.body
    );

    // (case, the resource field the ARN carries)
    let resources = [
        ("the stored configuration", resource.as_str()),
        ("a resource this store holds no record of", "phone-number/pn-0000000000000000"),
        ("a resource spelled with a colon", "brand-profile:bp-00000000000000000"),
    ];
    for (case, resource_field) in resources {
        for (row, named) in foreign_scopes(&expected) {
            let foreign = arn_in(&named, &account, resource_field);
            let message = scope_mismatch_message(&expected, &named);
            let refused = em.list_tags(&foreign).await;
            assert_eq!(
                refused.status, 400,
                "case {case} / {row} ({foreign}): expected 400, got {} / {:?}",
                refused.status, refused.body
            );
            assert_eq!(
                refused.error.as_deref(),
                Some("ValidationException"),
                "case {case} / {row} ({foreign}): unexpected error code, got {:?}",
                refused.error
            );
            assert_eq!(
                refused.string("message"),
                message,
                "case {case} / {row} ({foreign}): expected {message:?}, got {:?}",
                refused.body
            );
        }
    }
}
