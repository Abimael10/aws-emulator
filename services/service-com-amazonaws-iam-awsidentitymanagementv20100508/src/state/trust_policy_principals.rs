// SPDX-License-Identifier: CC0-1.0

//! WHICH service a role's trust policy may name as a principal that assumes the role, and how
//! IAM refuses one that cannot.
//!
//! A trust policy is a resource-based policy, so its statements name the parties the role
//! trusts — and IAM RESOLVES each SERVICE it is handed there rather than storing the name on
//! trust. A service that does not assume roles under that name has no principal to resolve, so
//! the document states a trust relationship that could never be exercised: the write is refused
//! with the modelled `MalformedPolicyDocumentException` (awsQuery code
//! `MalformedPolicyDocument`, HTTP 400) and the sentence
//! [`invalid_trust_policy_principal`], and nothing is stored. Without that refusal the caller
//! is told the role was created, and every later assume by the service it meant to trust fails
//! with an access error that names nothing — the mistake surfaces nowhere near the call that
//! made it.
//!
//! The services this is about are the ones whose roles AWS assumes under a DIFFERENT name
//! than the one the client wrote, in either of the two ways a client gets that name wrong:
//!
//! * the service is addressed under its own endpoint domain but assumes its roles under
//!   ANOTHER service's principal ([`UNASSUMABLE_SERVICE_PRINCIPALS`]): a managed database
//!   engine delivered on the Amazon RDS platform acts through a role assumed as
//!   [`RDS_SERVICE_PRINCIPAL`], which is the principal that engine's own documentation tells a
//!   client to write;
//! * the service's principal is published WITHOUT a Region ([`REGIONLESS_SERVICE_PRINCIPALS`])
//!   and the client regionalized it — an easy mistake, since the service's ENDPOINT does carry
//!   the Region — so `<service>.<region>.amazonaws.com` names a principal AWS publishes
//!   nowhere while `<service>.amazonaws.com` names one
//!   ([`regionless_service_principal`]).
//!
//! Nothing else about a `Service` value is judged here: which FORMS the policy language admits
//! under a principal type is the shared reading beside this one
//! (`::service_common::iam_policy_principals`), asked of a trust document by the clause above
//! this one ([`super::unreadable_trust_policy_principal`]), and a value in a type's forms that
//! neither catalog carries is the caller's own data, stored as it was written.
//!
//! A name neither catalog carries is a principal IAM has nothing against here: they say which
//! names cannot assume a role, never which services exist. The reading is therefore
//! deliberately UNDER-INCLUSIVE — a value naming no AWS service at all is stored as written,
//! which is the crate's own rule for a `Service` value
//! ([`super::stored_trust_policy`], whose reads report whatever the caller wrote) — and what
//! would make it complete is a registry of the service principals AWS publishes, feature and
//! regional sub-principals included (`monitoring.`, `ecs-tasks.`, `batchoperations.s3.`,
//! `logs.<region>.`), which nothing in this tree holds: resolving against a short list of
//! them would refuse trust policies AWS accepts. Until such a registry exists here, a service
//! whose roles are documented as assumed under another principal, and one whose principal is
//! documented without a Region, are filed below.
//!
//! The reading is shared by every operation that WRITES the member — `CreateRole`,
//! `UpdateAssumeRolePolicy`, and `AcquireRole`, which renders a trust policy from a role
//! template — so one document is judged the same way whichever of them was handed it. A trust
//! document IAM itself writes for a service-linked role
//! ([`super::service_linked_role_trust_policy`]) is not a caller's document and is not read
//! here.

#[allow(unused_imports)]
use super::*;

/// The principal TYPE a statement names a service under, one of the four the policy language
/// keys a `Principal`/`NotPrincipal` map by
/// (`::service_common::iam_policy_principals::PRINCIPAL_TYPES`). A value under any other type
/// names an account, a provider or a canonical user and is no service principal at all.
const SERVICE_PRINCIPAL_TYPE: &str = "Service";

/// How IAM's refusal SPELLS the principal type the rejected value sat under: the type's name
/// in upper case, which is the form its policy validator reports an offending principal with.
const REPORTED_PRINCIPAL_TYPE: &str = "SERVICE";

/// The service principal a role that Amazon RDS — and every managed database engine delivered
/// on the RDS platform — acts through is assumed under.
///
/// It is the principal those engines' own documentation writes into the trust policy of the
/// roles they assume, and the one AWS publishes a service-linked role for
/// ([`super::PUBLISHED_SERVICE_LINKED_ROLE_NAMES`]).
pub const RDS_SERVICE_PRINCIPAL: &str = "rds.amazonaws.com";

/// One service that does NOT assume roles under its own name, and the principal its roles are
/// assumed under instead.
#[derive(Debug, Clone, Copy)]
pub struct UnassumableServicePrincipal {
    /// The DNS-style name a client reaches the service under, and so mistakes for the
    /// principal it would assume a role as. IAM resolves no role-assuming principal for it.
    pub principal: &'static str,
    /// The service principal that service's roles ARE assumed under — what the trust policy
    /// has to name for the role to be usable.
    pub assumed_under: &'static str,
}

impl UnassumableServicePrincipal {
    /// One catalog row: the name that resolves to no role-assuming principal, and the one that
    /// does for the same service.
    const fn new(principal: &'static str, assumed_under: &'static str) -> Self {
        Self {
            principal,
            assumed_under,
        }
    }
}

/// The service principals IAM resolves no role assumption for, ascending by principal.
///
/// SOURCE: the "identity and access management" / service-role page of each service's own user
/// guide, which writes the trust policy of the roles that service assumes — and writes
/// [`RDS_SERVICE_PRINCIPAL`] in it, not the domain the service itself is reached under. Every
/// row is such a service: a managed database engine delivered on the Amazon RDS platform,
/// whose control plane is RDS's and whose role assumption therefore happens as RDS. The
/// substitute each row names is asserted to be a principal this catalog does NOT carry, so a
/// client that follows the refusal arrives at a document IAM accepts.
///
/// A name this catalog does not carry is a principal IAM has nothing against: the catalog says
/// which services cannot assume a role, never which services exist, so a document naming a
/// service principal nothing here mentions is stored as written.
pub const UNASSUMABLE_SERVICE_PRINCIPALS: &[UnassumableServicePrincipal] = &[
    // Amazon Aurora is an engine of Amazon RDS, reached under the RDS API: "Authorizing Amazon
    // Aurora MySQL to access other AWS services on your behalf" writes the cluster's role with
    // a trust policy naming the RDS principal.
    UnassumableServicePrincipal::new("aurora.amazonaws.com", RDS_SERVICE_PRINCIPAL),
    // Amazon DocumentDB (with MongoDB compatibility) is likewise delivered on the RDS
    // platform: the roles it assumes are created with the RDS principal in their trust policy.
    UnassumableServicePrincipal::new("docdb.amazonaws.com", RDS_SERVICE_PRINCIPAL),
    // Amazon Neptune: "Prerequisites: IAM role and Amazon S3 access" writes the bulk loader's
    // role with a trust policy naming the RDS principal, because a Neptune DB cluster assumes
    // its roles as RDS.
    UnassumableServicePrincipal::new("neptune.amazonaws.com", RDS_SERVICE_PRINCIPAL),
];

/// The DNS zone a service principal is published under — the first of the endpoint zones this
/// tree lists (`::service_common::data_plane_addressing::AWS_ENDPOINT_DNS_SUFFIXES`), which is
/// the one every principal named below is published in. A name under another partition's zone
/// is a principal this reading says nothing about, exactly as a name no row carries is.
const SERVICE_PRINCIPAL_DNS_SUFFIX: &str = ".amazonaws.com";

/// The service principals AWS publishes WITHOUT a Region — the services AWS names as
/// `<service>.amazonaws.com` wherever the resource is, so the REGIONALIZED spelling
/// `<service>.<region>.amazonaws.com` of the same service resolves to no principal at all
/// ([`regionless_service_principal`]).
///
/// SOURCE: the page of each service's own documentation that WRITES the principal — the
/// "identity and access management" or service-role page that states the trust policy of a role
/// that service assumes, or the key- or resource-policy page that states how a policy grants it.
/// Every one of them writes the name below with no Region in it, whichever Region the resource
/// is in. A client that regionalizes such a name (an easy mistake, since the service's ENDPOINT
/// does carry the Region) names a principal AWS publishes nowhere.
///
/// A service this list does not carry is a service this reading has nothing against: some
/// services DO publish a regional sub-principal for the roles they assume
/// (`logs.<region>.amazonaws.com` is the published principal of CloudWatch Logs), and nothing
/// in this tree holds the registry of service principals AWS publishes that would say which —
/// so, exactly like [`UNASSUMABLE_SERVICE_PRINCIPALS`] beside it, the reading is deliberately
/// UNDER-INCLUSIVE: a regionalized name of a service no row carries is stored as written
/// rather than refused, because refusing it would refuse trust policies AWS accepts.
///
/// Sorted by principal, which is also the lookup key.
pub const REGIONLESS_SERVICE_PRINCIPALS: &[&str] = &[
    // Amazon API Gateway: the role it assumes to push logs or call an integration is created
    // with `apigateway.amazonaws.com`.
    "apigateway.amazonaws.com",
    // AWS CloudFormation: a service role a stack is given names
    // `cloudformation.amazonaws.com`.
    "cloudformation.amazonaws.com",
    // Amazon DynamoDB: the principal a key policy or a resource policy names the table service
    // by, published without a Region (the REGIONALIZED spelling of the same service appears in
    // AWS's documentation only as a `kms:ViaService` condition VALUE, which is no principal and
    // is not read here).
    "dynamodb.amazonaws.com",
    // Amazon EC2, the principal of every instance profile's role — the one the model's own
    // `CreateRole` example writes.
    "ec2.amazonaws.com",
    // Amazon ECS: a task execution role and the ECS service role name `ecs.amazonaws.com` and
    // `ecs-tasks.amazonaws.com`, neither of them regionalized.
    "ecs-tasks.amazonaws.com",
    "ecs.amazonaws.com",
    // Amazon EventBridge: a rule's role names `events.amazonaws.com`.
    "events.amazonaws.com",
    // AWS Glue: a crawler's or job's role names `glue.amazonaws.com`.
    "glue.amazonaws.com",
    // AWS Key Management Service: the principal a resource policy grants to KMS names — "AWS
    // KMS" in the key-policy and grant pages — published without a Region.
    "kms.amazonaws.com",
    // AWS Lambda, the principal of every function's execution role.
    "lambda.amazonaws.com",
    // Amazon RDS — the principal every engine of that platform assumes its roles under
    // ([`RDS_SERVICE_PRINCIPAL`]), published without a Region.
    RDS_SERVICE_PRINCIPAL,
    // Amazon S3: a replication role and a batch-operations role name `s3.amazonaws.com`.
    "s3.amazonaws.com",
    // Amazon SNS: the principal of the role a topic logs delivery status through.
    "sns.amazonaws.com",
    // Amazon SQS: the principal a key policy or a bucket policy names the queue service by,
    // published without a Region.
    "sqs.amazonaws.com",
];

/// The principal the service named by `principal` assumes its roles under instead, or `None`
/// when nothing says that name resolves to no role assumption.
///
/// Two readings, one answer: the service that is addressed under its own name but assumes its
/// roles under another service's ([`UNASSUMABLE_SERVICE_PRINCIPALS`]), and the service whose
/// principal AWS publishes without a Region, named here with one
/// ([`regionless_service_principal`]). Both are the same mistake — a name that resolves to no
/// role-assuming principal — and both point the client at the name that does.
///
/// Matched without regard to ASCII case, the way every service principal in this crate is
/// ([`super::published_service_linked_role_name`]): a service principal is a DNS-style name,
/// so a differently-cased spelling addresses the same service.
#[must_use]
pub fn assumed_under_another_principal(principal: &str) -> Option<&'static str> {
    UNASSUMABLE_SERVICE_PRINCIPALS
        .iter()
        .find(|entry| entry.principal.eq_ignore_ascii_case(principal))
        .map(|entry| entry.assumed_under)
        .or_else(|| regionless_service_principal(principal))
}

/// The Region-less principal of the service a REGIONALIZED service name addresses, when that
/// service's role-assuming principal is one AWS publishes without a Region
/// ([`REGIONLESS_SERVICE_PRINCIPALS`]), and `None` for every other value.
///
/// `None` therefore covers a name that carries no Region at all (the published form, which is
/// what this reading points a client at), a name whose middle label is no Region code — a
/// FEATURE sub-principal such as `<namespace>.application-autoscaling.amazonaws.com` is
/// written exactly like a regionalized one and is a published principal — and a service no row
/// carries.
#[must_use]
pub fn regionless_service_principal(principal: &str) -> Option<&'static str> {
    // A service principal is a DNS-style name, so the comparison is case-insensitive: the
    // whole value is folded once and the Region code and the zone are read off the folded
    // form.
    let folded = principal.to_ascii_lowercase();
    let labels = folded.strip_suffix(SERVICE_PRINCIPAL_DNS_SUFFIX)?;
    let (service, region) = labels.rsplit_once('.')?;
    if service.is_empty() || !is_region_code(region) {
        return None;
    }
    let published = format!("{service}{SERVICE_PRINCIPAL_DNS_SUFFIX}");
    REGIONLESS_SERVICE_PRINCIPALS
        .iter()
        .copied()
        .find(|named| *named == published)
}

/// Whether `label` is shaped like an AWS Region code: lowercase hyphen-joined words ending in
/// a number, at least three groups long — `us-east-1`, `ap-southeast-4`, `us-gov-west-1`.
///
/// This is what tells a Region label apart from the other labels a service principal is
/// written with: a feature sub-principal's label (`application-autoscaling`, `s3`) is no
/// Region, so the name it sits in is not a regionalized one.
fn is_region_code(label: &str) -> bool {
    if label.len() > ::service_common::arn::MAX_REGION_LENGTH {
        return false;
    }
    // The last hyphen group is the Region's number and every group before it is a word,
    // walked without collecting the caller-supplied label.
    let Some((words, number)) = label.rsplit_once('-') else {
        return false;
    };
    words.split('-').count() >= 2
        && !number.is_empty()
        && number.bytes().all(|byte| byte.is_ascii_digit())
        && words
            .split('-')
            .all(|word| !word.is_empty() && word.bytes().all(|byte| byte.is_ascii_lowercase()))
}

/// The first service principal `document` names that IAM resolves no role assumption for, as
/// the document wrote it, or `None` when every service it names can assume a role.
///
/// Every statement is read, both elements that name principals, and both forms a principal
/// type's values take (one string or a list of them) — so where in the document the name sits
/// does not change the answer. A `document` that is no policy at all is `None`: that is the
/// grammar's own clause ([`super::trust_policy_document_defect`]), which the operations read
/// first.
#[must_use]
pub fn unassumable_service_principal(document: &str) -> Option<String> {
    named_trust_policy_principals(document, SERVICE_PRINCIPAL_TYPE)
        .into_iter()
        .find(|named| assumed_under_another_principal(named).is_some())
}

/// Every principal the statements of `document` name under the principal type
/// `principal_type` (one of `::service_common::iam_policy_principals::PRINCIPAL_TYPES`), in
/// the order the document writes them.
///
/// The ONE walk of a trust document's principals, shared by every resolution of one — the
/// service principals here and the account identities beside them
/// ([`super::trust_policy_identity_principals`]) — so where in the document a name sits
/// never changes whether it is resolved: every statement is read, both elements that name
/// principals, and both forms a type's values take (one string or a list of them). A
/// `document` that is no policy at all names nothing: that is the grammar's own clause
/// ([`super::trust_policy_document_defect`]), which the operations read first.
#[must_use]
pub fn named_trust_policy_principals(document: &str, principal_type: &str) -> Vec<String> {
    let statements = match super::policy_statements(document) {
        Ok(::serde_json::Value::Array(entries)) => entries,
        Ok(single) => vec![single],
        Err(_) => return Vec::new(),
    };
    statements
        .iter()
        .filter_map(::serde_json::Value::as_object)
        .flat_map(|statement| named_principals(statement, principal_type))
        .collect()
}

/// Every principal of ONE type that ONE statement names, in the order it wrote them: the
/// values under that type of each element a statement names principals in
/// (`::service_common::iam_policy_principals::PRINCIPAL_STATEMENT_ELEMENTS`, the tree's one
/// list of those elements).
fn named_principals(
    statement: &::serde_json::Map<String, ::serde_json::Value>,
    principal_type: &str,
) -> Vec<String> {
    ::service_common::iam_policy_principals::PRINCIPAL_STATEMENT_ELEMENTS
        .into_iter()
        .filter_map(|element| statement.get(element))
        .filter_map(::serde_json::Value::as_object)
        .filter_map(|types| types.get(principal_type))
        .flat_map(principal_values)
        .collect()
}

/// The values one principal type names: the policy language writes them as one string or as a
/// list of them, and anything else names no principal.
fn principal_values(named: &::serde_json::Value) -> Vec<String> {
    match named {
        ::serde_json::Value::String(value) => vec![value.clone()],
        ::serde_json::Value::Array(entries) => entries.iter().flat_map(principal_values).collect(),
        _ => Vec::new(),
    }
}

/// How IAM refuses a trust policy naming a service principal it resolves no role assumption
/// for: the fixed text `Invalid principal in policy: ` followed by the principal TYPE the value
/// sat under, in upper case, and the value itself, each in double quotes and separated by a
/// colon.
///
/// The one variable part is the rejected value, taken from the request exactly as it was
/// written, so a caller that mis-typed a principal is told which name was refused.
#[must_use]
pub fn invalid_trust_policy_principal(principal: &str) -> String {
    invalid_policy_principal(REPORTED_PRINCIPAL_TYPE, principal)
}

/// The sentence above for a value of ANY principal type: the fixed text `Invalid principal in
/// policy: `, then the TYPE the rejected value sat under in upper case and the value itself,
/// each in double quotes and separated by a colon.
///
/// One spelling for every principal IAM resolves nothing for, whatever its type, so the
/// service-principal refusal here and the account-identity one beside it
/// ([`super::trust_policy_identity_principals`]) cannot word the same refusal two ways. The
/// one variable part stays the rejected value, as the request wrote it.
#[must_use]
pub fn invalid_policy_principal(principal_type: &str, principal: &str) -> String {
    format!(
        "Invalid principal in policy: \"{}\":\"{principal}\"",
        principal_type.to_ascii_uppercase()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trust policy whose single statement names `principal` as written (a JSON fragment, so
    /// a row can write one string or a list).
    fn trust(principal: &str) -> String {
        format!(
            "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
             \"Principal\":{{\"Service\":{principal}}},\"Action\":\"sts:AssumeRole\"}}]}}"
        )
    }

    /// RULE: the principal type this reading looks under is one of the four the policy
    /// language defines, and the refusal spells that type in upper case.
    #[test]
    fn the_service_type_is_one_the_policy_language_defines() {
        assert!(
            ::service_common::iam_policy_principals::PRINCIPAL_TYPES
                .contains(&SERVICE_PRINCIPAL_TYPE),
            "expected {SERVICE_PRINCIPAL_TYPE:?} to be one of the principal types the language \
             defines, got {:?}",
            ::service_common::iam_policy_principals::PRINCIPAL_TYPES
        );
        assert_eq!(
            REPORTED_PRINCIPAL_TYPE,
            SERVICE_PRINCIPAL_TYPE.to_ascii_uppercase(),
            "expected the refusal to spell the type in upper case, got \
             {REPORTED_PRINCIPAL_TYPE:?}"
        );
    }

    /// RULE: a trust policy naming a service that assumes its roles under another service's
    /// principal is reported, whichever statement names it, under either element that names
    /// principals, in either form the value takes and in any casing; every other document is
    /// `None`.
    #[test]
    fn a_service_that_cannot_assume_a_role_is_the_one_reported() {
        // The rows differ along what the rule generalises over: which catalog entry is named
        // (each of the three, none of them cited by one request), the form of the value (a
        // string, a list, the second entry of a list), the element it sits under, the
        // position of the statement, the form `Statement` takes, and the casing.
        let rows: [(&str, String, Option<&str>); 14] = [
            (
                "the RDS principal, which every row's service assumes its roles under",
                trust("\"rds.amazonaws.com\""),
                None,
            ),
            (
                "another service's principal",
                trust("\"lambda.amazonaws.com\""),
                None,
            ),
            (
                "a service principal of the same family that does assume roles",
                trust("\"neptune-graph.amazonaws.com\""),
                None,
            ),
            (
                "a value that is no DNS name at all, which this reading does not judge",
                trust("\"210987654321\""),
                None,
            ),
            (
                "a statement naming no principal",
                "{\"Statement\":[{\"Effect\":\"Allow\",\"Action\":\"sts:AssumeRole\"}]}"
                    .to_string(),
                None,
            ),
            ("no statement at all", "{\"Statement\":[]}".to_string(), None),
            ("not a policy at all", "not a policy".to_string(), None),
            (
                "the cited engine's own domain",
                trust("\"neptune.amazonaws.com\""),
                Some("neptune.amazonaws.com"),
            ),
            (
                "a second engine of the same platform",
                trust("\"docdb.amazonaws.com\""),
                Some("docdb.amazonaws.com"),
            ),
            (
                "a third engine of the same platform",
                trust("\"aurora.amazonaws.com\""),
                Some("aurora.amazonaws.com"),
            ),
            (
                "the second value of a list, beside a principal that can assume roles",
                trust("[\"rds.amazonaws.com\",\"docdb.amazonaws.com\"]"),
                Some("docdb.amazonaws.com"),
            ),
            (
                "the complement element names it",
                "{\"Statement\":[{\"Effect\":\"Deny\",\
                 \"NotPrincipal\":{\"Service\":\"aurora.amazonaws.com\"},\
                 \"Action\":\"sts:AssumeRole\"}]}"
                    .to_string(),
                Some("aurora.amazonaws.com"),
            ),
            (
                "the second statement names it",
                "{\"Statement\":[{\"Effect\":\"Allow\",\
                 \"Principal\":{\"Service\":\"rds.amazonaws.com\"},\
                 \"Action\":\"sts:AssumeRole\"},{\"Effect\":\"Allow\",\
                 \"Principal\":{\"Service\":\"neptune.amazonaws.com\"},\
                 \"Action\":\"sts:AssumeRole\"}]}"
                    .to_string(),
                Some("neptune.amazonaws.com"),
            ),
            (
                "the single statement object the grammar admits, named in another casing",
                "{\"Statement\":{\"Effect\":\"Allow\",\
                 \"Principal\":{\"Service\":\"Neptune.AmazonAWS.com\"},\
                 \"Action\":\"sts:AssumeRole\"}}"
                    .to_string(),
                Some("Neptune.AmazonAWS.com"),
            ),
        ];

        for (case, document, expected) in rows {
            let found = unassumable_service_principal(&document);
            assert_eq!(
                found.as_deref(),
                expected,
                "case {case:?}: expected {expected:?}, got {found:?} for {document}"
            );
        }
    }

    /// RULE: the refusal is the fixed text around the principal TYPE in upper case and the
    /// rejected value exactly as the request wrote it.
    #[test]
    fn the_refusal_names_the_rejected_principal_as_it_was_written() {
        for (case, principal, expected) in [
            (
                "the cited engine",
                "neptune.amazonaws.com",
                "Invalid principal in policy: \"SERVICE\":\"neptune.amazonaws.com\"",
            ),
            (
                "another engine of the same platform",
                "docdb.amazonaws.com",
                "Invalid principal in policy: \"SERVICE\":\"docdb.amazonaws.com\"",
            ),
            (
                "a value written in another casing",
                "Aurora.AmazonAWS.com",
                "Invalid principal in policy: \"SERVICE\":\"Aurora.AmazonAWS.com\"",
            ),
        ] {
            let message = invalid_trust_policy_principal(principal);
            assert_eq!(
                message, expected,
                "case {case:?}: expected {expected:?}, got {message:?}"
            );
        }
    }

    /// RULE: every row's substitute is a principal a role CAN be assumed under — a client that
    /// follows the refusal and writes it reaches a document this reading accepts — and the
    /// catalog is strictly ascending by principal, so a service is filed once.
    #[test]
    fn each_row_names_a_principal_a_role_can_be_assumed_under() {
        for entry in UNASSUMABLE_SERVICE_PRINCIPALS {
            assert!(
                assumed_under_another_principal(entry.assumed_under).is_none(),
                "case {:?}: the substitute {:?} must itself be assumable",
                entry.principal,
                entry.assumed_under
            );
            let found = unassumable_service_principal(&trust(&format!(
                "\"{}\"",
                entry.assumed_under
            )));
            assert_eq!(
                found, None,
                "case {:?}: a document naming {:?} must be accepted, got {found:?}",
                entry.principal, entry.assumed_under
            );
        }

        for pair in UNASSUMABLE_SERVICE_PRINCIPALS.windows(2) {
            assert!(
                pair[0].principal < pair[1].principal,
                "the catalog must ascend by principal, got {:?} before {:?}",
                pair[0].principal,
                pair[1].principal
            );
        }
    }

    /// RULE: a service whose role-assuming principal AWS publishes without a Region resolves
    /// to nothing when a document regionalizes it, and the principal that service's roles ARE
    /// assumed under is the published, Region-less name.
    #[test]
    fn a_regionalized_spelling_of_a_regionless_principal_resolves_to_the_published_one() {
        // The rows differ along what the rule generalises over: which catalog entry is named
        // (none of them the one the cited request carried), the Region named (several
        // geographies, a three-word Region name, the longest published Region label), the
        // casing, and the ways a name only LOOKS regionalized.
        let rows: [(&str, String, Option<&str>); 17] = [
            (
                "the published principal itself, which carries no Region",
                "lambda.amazonaws.com".to_string(),
                None,
            ),
            (
                "a service principal no row carries, regionalized — some services publish one",
                "logs.eu-west-1.amazonaws.com".to_string(),
                None,
            ),
            (
                "a FEATURE sub-principal, written like a regionalized name",
                "dynamodb.application-autoscaling.amazonaws.com".to_string(),
                None,
            ),
            (
                "a feature sub-principal of a service a row carries",
                "replication.dynamodb.amazonaws.com".to_string(),
                None,
            ),
            (
                "a middle label whose last group is no number",
                "s3.batchoperations.amazonaws.com".to_string(),
                None,
            ),
            (
                "a middle label of two groups only",
                "sns.east-1.amazonaws.com".to_string(),
                None,
            ),
            (
                "a name in another partition's zone, which this reading says nothing about",
                "sns.cn-north-1.amazonaws.com.cn".to_string(),
                None,
            ),
            (
                "a value that is no DNS name at all",
                "210987654321".to_string(),
                None,
            ),
            (
                "a regionalized Amazon EC2 principal",
                "ec2.us-east-1.amazonaws.com".to_string(),
                Some("ec2.amazonaws.com"),
            ),
            (
                "a regionalized AWS Lambda principal, in another geography",
                "lambda.ap-southeast-2.amazonaws.com".to_string(),
                Some("lambda.amazonaws.com"),
            ),
            (
                "a regionalized Amazon SQS principal, in a three-word Region",
                "sqs.us-gov-west-1.amazonaws.com".to_string(),
                Some("sqs.amazonaws.com"),
            ),
            (
                "a regionalized Amazon RDS principal",
                format!("rds.eu-central-1{SERVICE_PRINCIPAL_DNS_SUFFIX}"),
                Some(RDS_SERVICE_PRINCIPAL),
            ),
            (
                "a regionalized Amazon S3 principal, written in another casing",
                "S3.US-WEST-2.AmazonAWS.com".to_string(),
                Some("s3.amazonaws.com"),
            ),
            (
                "a regionalized principal whose service label carries a hyphen",
                "ecs-tasks.eu-west-3.amazonaws.com".to_string(),
                Some("ecs-tasks.amazonaws.com"),
            ),
            (
                "the cited service, regionalized",
                "sns.us-east-1.amazonaws.com".to_string(),
                Some("sns.amazonaws.com"),
            ),
            (
                // The published bound on a Region label
                // (`::service_common::arn::MAX_REGION_LENGTH`): a label AT it is a Region, so
                // the name is a regionalized one.
                "a middle label as long as a Region label may be",
                format!(
                    "sns.ap-{}-1{SERVICE_PRINCIPAL_DNS_SUFFIX}",
                    "s".repeat(::service_common::arn::MAX_REGION_LENGTH - 5)
                ),
                Some("sns.amazonaws.com"),
            ),
            (
                // One past that bound: a longer middle label is no Region of any AWS endpoint,
                // so the name is not a regionalized one.
                "a middle label one character past the Region-label bound",
                format!(
                    "sns.ap-{}-1{SERVICE_PRINCIPAL_DNS_SUFFIX}",
                    "s".repeat(::service_common::arn::MAX_REGION_LENGTH - 4)
                ),
                None,
            ),
        ];

        for (case, principal, expected) in rows {
            let found = assumed_under_another_principal(&principal);
            assert_eq!(
                found, expected,
                "case {case:?}: expected {expected:?}, got {found:?} for {principal}"
            );
            // The whole document is read the same way, so the refusal names the value exactly
            // as the request wrote it.
            let document = trust(&format!("\"{principal}\""));
            let named = unassumable_service_principal(&document);
            assert_eq!(
                named.as_deref(),
                expected.map(|_| principal.as_str()),
                "case {case:?}: expected the document to report {principal:?} for {expected:?}, \
                 got {named:?}"
            );
        }
    }

    /// RULE: every Region-less row is a principal a role CAN be assumed under — a client that
    /// follows the refusal and drops the Region reaches a document this reading accepts — the
    /// Region-less zone is one the tree publishes endpoints in, and the catalog is strictly
    /// ascending, so a service is filed once.
    #[test]
    fn each_regionless_row_is_itself_a_principal_a_role_can_be_assumed_under() {
        assert!(
            ::service_common::data_plane_addressing::AWS_ENDPOINT_DNS_SUFFIXES
                .contains(&SERVICE_PRINCIPAL_DNS_SUFFIX),
            "expected {SERVICE_PRINCIPAL_DNS_SUFFIX:?} to be one of the endpoint zones the tree \
             lists, got {:?}",
            ::service_common::data_plane_addressing::AWS_ENDPOINT_DNS_SUFFIXES
        );

        for published in REGIONLESS_SERVICE_PRINCIPALS {
            assert!(
                published.ends_with(SERVICE_PRINCIPAL_DNS_SUFFIX),
                "case {published:?}: a published service principal sits in the zone AWS \
                 publishes them under"
            );
            assert!(
                assumed_under_another_principal(published).is_none(),
                "case {published:?}: the published principal must itself be assumable, got {:?}",
                assumed_under_another_principal(published)
            );
            // A document naming the published principal is stored, and the regionalized
            // spelling of the SAME service is refused and pointed back at it.
            let regionalized = published.replace(
                SERVICE_PRINCIPAL_DNS_SUFFIX,
                &format!(".us-east-1{SERVICE_PRINCIPAL_DNS_SUFFIX}"),
            );
            assert_eq!(
                assumed_under_another_principal(&regionalized),
                Some(*published),
                "case {published:?}: the regionalized spelling {regionalized:?} must point at it"
            );
        }

        for pair in REGIONLESS_SERVICE_PRINCIPALS.windows(2) {
            assert!(
                pair[0] < pair[1],
                "the catalog must ascend by principal, got {:?} before {:?}",
                pair[0],
                pair[1]
            );
        }
    }
}
