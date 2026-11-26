// SPDX-License-Identifier: CC0-1.0

//! The per-service ACCESS FAMILY of AWS's own managed-policy catalog: the full-access
//! policy AWS publishes for a service and its read-only counterpart, read over the
//! awsQuery wire.
//!
//! RULE: a policy AWS publishes in its own namespace (`arn:<partition>:iam::aws:policy/…`)
//! exists in every account, so a read addressed by its published ARN is SERVED — `GetPolicy`
//! reports the identity that ARN spells (its `PolicyName` and `Path`), the policy's own
//! `PolicyId` and `DefaultVersionId`, and `IsAttachable`; `GetPolicyVersion` on that default
//! version serves the policy's document; the enumeration of AWS's own scope reports it; and
//! it can be attached to an identity and read back. Membership of the catalog is what decides
//! this, so a name the catalog does not carry — and a carried name spelled under another path,
//! because a published ARN's path is part of the identifier — is the only `NoSuchEntity`
//! (`Policy <PolicyArn> was not found.`).
//!
//! Each suite is a TABLE whose rows differ along what the rule generalises over: the service
//! the family belongs to, which half of the family (full access or read-only) is read, the
//! reader doing the reading, and, for the refusals, the way the ARN fails to name a published
//! policy.
//!
//! The awsQuery harness (`router`, `call`, `element`, `elements`, `error_code`) is the one
//! `tests/all/user_lifecycle.rs` owns and every suite in this crate shares.

use smithy_server_runtime::MultiServiceRouter;

use crate::aws_reference_catalog_metadata::assert_published_policy_id;
use crate::published_policy_families::walk_policies;
use crate::user_lifecycle::{call, document_element, element, elements, error_code, router};

/// One per-service access family: the service its grants are about, and the two policies
/// AWS publishes for it at the account root.
///
/// SOURCE: the AWS Managed Policy Reference ("AWS managed policies for job functions" and the
/// per-service pages of the IAM User Guide), which publishes both halves of each of these
/// families at the path named here under the friendly names below.
struct Family {
    /// The IAM action prefix of the service the family's grants are about, as a policy
    /// document spells it (`dynamodb`, `s3`, `sns`, `sqs`).
    service_prefix: &'static str,
    /// The friendly name of the family's full-access policy.
    full_access: &'static str,
    /// The friendly name of its read-only counterpart.
    read_only: &'static str,
}

/// The families this suite spans: the one the recorded trace read (Amazon DynamoDB) beside
/// three it did not, so the rule is asserted across services whose documents differ in size
/// and in the neighbouring services they reach.
const FAMILIES: &[Family] = &[
    Family {
        service_prefix: "dynamodb",
        full_access: "AmazonDynamoDBFullAccess",
        read_only: "AmazonDynamoDBReadOnlyAccess",
    },
    Family {
        service_prefix: "s3",
        full_access: "AmazonS3FullAccess",
        read_only: "AmazonS3ReadOnlyAccess",
    },
    Family {
        service_prefix: "sns",
        full_access: "AmazonSNSFullAccess",
        read_only: "AmazonSNSReadOnlyAccess",
    },
    Family {
        service_prefix: "sqs",
        full_access: "AmazonSQSFullAccess",
        read_only: "AmazonSQSReadOnlyAccess",
    },
];

/// The path AWS publishes every member of these families under.
const ROOT_PATH: &str = "/";

/// The version every IAM policy document these entries carry is written at.
const POLICY_LANGUAGE_VERSION: &str = "2012-10-17";

/// The ARN a published policy of the account root is addressed by.
fn root_policy_arn(policy_name: &str) -> String {
    format!("arn:aws:iam::aws:policy/{policy_name}")
}

/// One `GetPolicy` call.
async fn get_policy(router: &MultiServiceRouter, policy_arn: &str) -> (u16, String) {
    call(router, "GetPolicy", &[("PolicyArn", policy_arn)]).await
}

/// One `GetPolicyVersion` call.
async fn get_policy_version(
    router: &MultiServiceRouter,
    policy_arn: &str,
    version_id: &str,
) -> (u16, String) {
    call(
        router,
        "GetPolicyVersion",
        &[("PolicyArn", policy_arn), ("VersionId", version_id)],
    )
    .await
}

/// Whether `id` is a policy version id of the modelled `policyVersionIdType`
/// (`^v[1-9][0-9]*(\.[A-Za-z0-9-]*)?$`), read here in the form a managed policy's default
/// version carries: `v` and a decimal number with no leading zero.
fn is_version_id(id: &str) -> bool {
    id.strip_prefix('v').is_some_and(|digits| {
        !digits.is_empty()
            && !digits.starts_with('0')
            && digits.chars().all(|c| c.is_ascii_digit())
    })
}

/// The `Statement` list of `document`, which must be a non-empty JSON policy document.
fn statements(case: &str, document: &str) -> Vec<serde_json::Value> {
    let parsed: serde_json::Value = serde_json::from_str(document).unwrap_or_else(|error| {
        panic!("case {case:?}: the served document must be JSON, got {error}: {document}")
    });
    assert_eq!(
        parsed.get("Version").and_then(serde_json::Value::as_str),
        Some(POLICY_LANGUAGE_VERSION),
        "case {case:?}: the document must be written at the policy language version \
         {POLICY_LANGUAGE_VERSION}, got {document}"
    );
    let list = parsed
        .get("Statement")
        .and_then(serde_json::Value::as_array)
        .unwrap_or_else(|| {
            panic!("case {case:?}: the document must carry a Statement list, got {document}")
        })
        .clone();
    assert!(
        !list.is_empty(),
        "case {case:?}: a published grant states at least one statement, got {document}"
    );
    list
}

/// Every action a statement list names, in the spelling the document carries.
fn granted_actions(list: &[serde_json::Value]) -> Vec<String> {
    let mut actions: Vec<String> = Vec::new();
    for statement in list {
        match statement.get("Action") {
            Some(serde_json::Value::String(one)) => actions.push(one.clone()),
            Some(serde_json::Value::Array(many)) => actions.extend(
                many.iter()
                    .filter_map(|action| action.as_str().map(ToString::to_string)),
            ),
            _ => {}
        }
    }
    actions
}

/// RULE: every member of a per-service access family AWS publishes resolves through every
/// reader of the catalog, and the version its `DefaultVersionId` names serves a document that
/// grants that service's actions.
#[tokio::test]
async fn every_published_service_access_policy_is_readable_and_serves_its_document() {
    let router = router();
    assert!(
        FAMILIES.len() >= 2,
        "the table must span more than one service family, got {}",
        FAMILIES.len()
    );

    for family in FAMILIES {
        for policy_name in [family.full_access, family.read_only] {
            let arn = root_policy_arn(policy_name);
            let case = format!("{} family: {arn}", family.service_prefix);

            let (status, body) = get_policy(&router, &arn).await;
            assert_eq!(
                status, 200,
                "case {case:?}: GetPolicy on a policy AWS publishes must succeed, got \
                 {status}: {body}"
            );
            assert_eq!(
                element(&body, "Arn"),
                arn,
                "case {case:?}: the reported Arn must be the ARN asked for, got {:?}",
                element(&body, "Arn")
            );
            assert_eq!(
                element(&body, "PolicyName"),
                policy_name,
                "case {case:?}: the reported PolicyName must be the name the ARN spells, got \
                 {:?}",
                element(&body, "PolicyName")
            );
            assert_eq!(
                element(&body, "Path"),
                ROOT_PATH,
                "case {case:?}: the reported Path must be the published one, got {:?}",
                element(&body, "Path")
            );
            assert_eq!(
                element(&body, "IsAttachable"),
                "true",
                "case {case:?}: AWS publishes this policy as attachable, got {:?}",
                element(&body, "IsAttachable")
            );
            let policy_id = element(&body, "PolicyId");
            // The ANPA form every published policy's id carries, asserted through the one
            // statement of it this crate shares
            // (`tests/all/aws_reference_catalog_metadata.rs`).
            assert_published_policy_id(&case, &policy_id);
            let default_version = element(&body, "DefaultVersionId");
            assert!(
                is_version_id(&default_version),
                "case {case:?}: expected a DefaultVersionId of the modelled form (v and a \
                 version number), got {default_version:?}"
            );
            assert!(
                !element(&body, "CreateDate").is_empty()
                    && !element(&body, "UpdateDate").is_empty(),
                "case {case:?}: a published policy reports both of its instants: {body}"
            );

            let (status, version) = get_policy_version(&router, &arn, &default_version).await;
            assert_eq!(
                status, 200,
                "case {case:?}: GetPolicyVersion must serve the default version \
                 {default_version:?}, got {status}: {version}"
            );
            assert_eq!(
                element(&version, "IsDefaultVersion"),
                "true",
                "case {case:?}: the version GetPolicy named is the default one, got {:?}",
                element(&version, "IsDefaultVersion")
            );
            let document = document_element(&version, "Document");
            let list = statements(&case, &document);
            let actions = granted_actions(&list);
            assert!(
                actions
                    .iter()
                    .any(|action| action.starts_with(&format!("{}:", family.service_prefix))),
                "case {case:?}: the family's grants are about {}, but the document names none \
                 of its actions, got {actions:?}",
                family.service_prefix
            );

            // The other readers of the same ARN agree with GetPolicy: the version listing
            // reports the version it named, and the enumeration of AWS's own scope reports
            // the policy, so it is discoverable and not merely addressable.
            let (status, versions) =
                call(&router, "ListPolicyVersions", &[("PolicyArn", &arn)]).await;
            assert_eq!(
                status, 200,
                "case {case:?}: ListPolicyVersions must serve a policy GetPolicy reported, \
                 got {status}: {versions}"
            );
            assert_eq!(
                elements(&versions, "VersionId"),
                vec![default_version.clone()],
                "case {case:?}: the listing must report the version GetPolicy named: \
                 {versions}"
            );
            // Walked through the one pager every suite in this crate walks the catalog with
            // (`tests/all/published_policy_families.rs`).
            assert!(
                walk_policies(&router, &case, &[("Scope", "AWS")], "Arn")
                    .await
                    .contains(&arn),
                "case {case:?}: ListPolicies(Scope=AWS) must report a policy AWS publishes"
            );

            // A read changes nothing a later caller sees: the second read is the first.
            let (repeat_status, repeat) = get_policy(&router, &arn).await;
            assert_eq!(
                (
                    repeat_status,
                    element(&repeat, "PolicyId"),
                    element(&repeat, "DefaultVersionId")
                ),
                (200, policy_id.clone(), default_version.clone()),
                "case {case:?}: a read must report the same policy the read before it did"
            );
        }
    }
}

/// RULE, on the pairing the family is published as: the full-access half grants the whole
/// service and the read-only half does not, so the two members of one family are different
/// grants rather than one document under two names.
#[tokio::test]
async fn the_two_halves_of_a_family_are_different_grants() {
    let router = router();

    for family in FAMILIES {
        let service_wildcard = format!("{}:*", family.service_prefix);
        for (policy_name, grants_the_service) in
            [(family.full_access, true), (family.read_only, false)]
        {
            let arn = root_policy_arn(policy_name);
            let case = format!("{} family: {arn}", family.service_prefix);
            let (status, body) = get_policy(&router, &arn).await;
            assert_eq!(
                status, 200,
                "case {case:?}: GetPolicy must serve a published policy, got {status}: {body}"
            );
            let default_version = element(&body, "DefaultVersionId");
            let (status, version) = get_policy_version(&router, &arn, &default_version).await;
            assert_eq!(
                status, 200,
                "case {case:?}: GetPolicyVersion must serve the default version, got {status}: \
                 {version}"
            );
            let document = document_element(&version, "Document");
            let actions = granted_actions(&statements(&case, &document));
            assert_eq!(
                actions.contains(&service_wildcard),
                grants_the_service,
                "case {case:?}: expected the grant of {service_wildcard:?} to be \
                 {grants_the_service}, got the actions {actions:?}"
            );
        }
    }
}

/// RULE, on the writes: a policy AWS publishes can be attached to an identity and is reported
/// back as attached, so a client that resolves a family member by ARN can go on to use it.
#[tokio::test]
async fn a_published_family_member_attaches_to_a_user_and_reads_back() {
    let router = router();

    for (index, family) in FAMILIES.iter().enumerate() {
        for (half, policy_name) in [("full access", family.full_access), ("read only", family.read_only)] {
            let user = format!("family-{index}-{}", half.replace(' ', "-"));
            let arn = root_policy_arn(policy_name);
            let case = format!("{} {half}: {arn}", family.service_prefix);

            let (status, created) = call(&router, "CreateUser", &[("UserName", &user)]).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the fixture user must be created, got {status}: {created}"
            );
            let (status, attached) = call(
                &router,
                "AttachUserPolicy",
                &[("UserName", &user), ("PolicyArn", &arn)],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: attaching a policy AWS publishes must succeed, got {status}: \
                 {attached}"
            );
            let (status, listed) =
                call(&router, "ListAttachedUserPolicies", &[("UserName", &user)]).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the attachment listing must succeed, got {status}: {listed}"
            );
            assert_eq!(
                elements(&listed, "PolicyArn"),
                vec![arn.clone()],
                "case {case:?}: the user must hold the policy that was attached: {listed}"
            );
            assert_eq!(
                elements(&listed, "PolicyName"),
                vec![policy_name.to_string()],
                "case {case:?}: the listing names the policy by its friendly name: {listed}"
            );
        }
    }
}

/// One way an `arn:aws:iam::aws:policy/…` ARN can fail to name a policy AWS publishes.
struct Unpublished {
    case: &'static str,
    arn: &'static str,
}

/// The ARNs that name no published policy: a name of no published policy at all, a name whose
/// spelling is not one AWS's catalog carries, and a name the catalog DOES carry spelled under
/// another path, because a published ARN's path is part of the identifier.
const UNPUBLISHED: &[Unpublished] = &[
    Unpublished {
        case: "a name no catalog carries",
        arn: "arn:aws:iam::aws:policy/Sim2realNoSuchManagedPolicy",
    },
    Unpublished {
        case: "a name of the published form that names nothing",
        arn: "arn:aws:iam::aws:policy/AmazonDynamoDBNoSuchAccess",
    },
    Unpublished {
        case: "a carried name under the service-role path",
        arn: "arn:aws:iam::aws:policy/service-role/AmazonDynamoDBReadOnlyAccess",
    },
    Unpublished {
        case: "a carried name under the job-function path",
        arn: "arn:aws:iam::aws:policy/job-function/AmazonSQSReadOnlyAccess",
    },
];

/// RULE, the other side of membership: an ARN in AWS's namespace that names no policy the
/// catalog publishes is refused as the missing entity, in the one sentence this service words
/// a policy's absence with.
#[tokio::test]
async fn an_arn_naming_no_published_policy_is_the_missing_entity() {
    let router = router();

    for unpublished in UNPUBLISHED {
        let case = unpublished.case;
        let arn = unpublished.arn;
        let (status, body) = get_policy(&router, arn).await;
        assert_eq!(
            status, 404,
            "case {case:?}: GetPolicy({arn:?}) must be refused, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            "NoSuchEntity",
            "case {case:?}: expected the missing-entity code, got {:?}: {body}",
            error_code(&body)
        );
        // The sentence itself, written out rather than read back from the formatter the
        // handler calls, so a change to that wording fails here.
        assert_eq!(
            element(&body, "Message"),
            format!("Policy {arn} was not found."),
            "case {case:?}: expected the sentence a policy's absence is worded with, got {:?}",
            element(&body, "Message")
        );
    }
}
