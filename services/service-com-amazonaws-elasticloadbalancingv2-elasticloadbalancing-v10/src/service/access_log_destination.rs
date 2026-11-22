// SPDX-License-Identifier: CC0-1.0

//! Whether the Amazon S3 destination a load balancer's log attributes name is one the
//! delivery could ever write into — judged BEFORE the attribute write is stored.
//!
//! A load balancer's log destinations are attribute TRIPLES of the one catalog of load
//! balancer attributes ([`crate::state::LOAD_BALANCER_ATTRIBUTES`]): a `<name>.s3.enabled`
//! switch beside the `<name>.s3.bucket` and `<name>.s3.prefix` that say where the delivery
//! goes ("`access_logs.s3.enabled` - Indicates whether access logs are enabled. The value is
//! true or false", "`access_logs.s3.bucket` - The name of the S3 bucket for the access logs",
//! "`access_logs.s3.prefix` - The prefix for the location in the S3 bucket for the access
//! logs", and the same three for the connection and health-check logs). So the families are
//! READ off that catalog rather than listed again here, and a destination family the service
//! models tomorrow is judged by the same rule.
//!
//! # Why a write is judged at all
//!
//! The bucket is not this service's resource: it is Amazon S3's, and so is the policy that
//! says who may put an object in it. A load balancer told to deliver into a bucket that does
//! not exist, or whose policy admits the delivery nowhere near the prefix the attribute
//! names, is a load balancer that believes it is logging to a destination that can never
//! receive a log — a misconfiguration discovered only as silently missing objects. So a write
//! that leaves a destination SWITCHED ON resolves it where it lives, and a destination the
//! delivery could not write into is refused as a configuration this load balancer cannot
//! hold.
//!
//! Amazon S3 is asked the way any emulated service reaches another — the in-process callback,
//! through this tree's one construction of the object-store client
//! ([`::service_common::s3_client`]) — so nothing here opens a socket or leaves the process.
//! An emulator with NO object store mounted cannot be asked at all, and silence refuses
//! nothing: the question was never answered, so no refusal is grounded in it.
//!
//! What a destination this service ACCEPTS then receives is the sibling module's
//! ([`super::access_logs`]): the file the delivery verifies the destination with, and the
//! record of every request the load balancer answers.

use ::smithy_server_runtime::OperationContext;

use crate::state::LoadBalancerRecord;

/// The three suffixes the attributes of ONE S3 log destination end in, which is what makes a
/// triple of the catalog's keys one destination.
const ENABLED_SUFFIX: &str = ".s3.enabled";
const BUCKET_SUFFIX: &str = ".s3.bucket";
const PREFIX_SUFFIX: &str = ".s3.prefix";

/// The value a `<name>.s3.enabled` attribute carries when the destination is switched on —
/// "The value is true or false. The default is false".
const SWITCHED_ON: &str = "true";

/// The key segment the DELIVERY itself writes, under whatever prefix the attribute names:
/// `<prefix>/AWSLogs/<account-id>/…`. A caller's own prefix may not BEGIN with it — the
/// segment belongs to the layout, so a prefix opening with it names a folder the delivery
/// would have written itself.
const DELIVERED_LOGS_SEGMENT: &str = "AWSLogs";

/// The action the delivery writes its objects with, and so the permission the destination
/// bucket's policy has to admit for the path the logs land under.
const DELIVERY_WRITE_ACTION: &str = "s3:PutObject";

/// The identities a log-delivery bucket policy grants that write to: the service principal
/// Elastic Load Balancing delivers a load balancer's logs as, the one AWS publishes for log
/// delivery generally, and the service's own principal name.
const LOG_DELIVERY_SERVICE_PRINCIPALS: [&str; 3] = [
    "logdelivery.elasticloadbalancing.amazonaws.com",
    "delivery.logs.amazonaws.com",
    "elasticloadbalancing.amazonaws.com",
];

/// The error code Amazon S3 answers a policy read of a bucket carrying NO policy with, which
/// its own documentation names. For this question it is an answer and not a failure: a bucket
/// with no policy grants the delivery nothing.
const NO_SUCH_BUCKET_POLICY: &str = "NoSuchBucketPolicy";

/// Amazon S3's ARN vendor: `arn:<partition>:s3:::<bucket>` IS the bucket, and an object of it
/// is that followed by `/<key>`.
const S3_ARN_VENDOR: &str = "s3";

/// One log destination a write leaves a load balancer configured with: which attributes said
/// so, and where they point.
#[derive(Debug, Clone)]
pub(crate) struct PendingDestination {
    /// The `<name>` of this destination family, as the catalog spells it (`access_logs`).
    family: String,
    /// The `<name>.s3.bucket` attribute of this family, which a refusal of the bucket names.
    bucket_key: String,
    /// The `<name>.s3.prefix` attribute of this family, which a refusal of the prefix names.
    prefix_key: String,
    /// The partition of the load balancer's own ARN, so every ARN this judges stays in it.
    partition: String,
    /// The load balancer the logs are for, which is what the delivery's write names as its
    /// source ([`super::access_logs::delivery_condition_context`]).
    load_balancer_arn: String,
    /// Where the delivery would write: the bucket, the prefix, the Region and the account —
    /// the same reading of a destination the delivery itself uses.
    destination: super::access_logs::Destination,
}

/// Every destination family the load balancer attribute catalog carries, as the `<name>` of
/// its `<name>.s3.enabled` switch.
fn destination_families() -> Vec<&'static str> {
    crate::state::LOAD_BALANCER_ATTRIBUTES
        .iter()
        .filter_map(|spec| spec.key.strip_suffix(ENABLED_SUFFIX))
        .collect()
}

/// The destinations `writes` leaves `record` configured with, for every family the write
/// NAMES an attribute of: a family the request said nothing about is left exactly as it was
/// and is not judged again.
///
/// A family whose switch is off after the write has no destination to judge — nothing is
/// delivered — so it yields none.
pub(crate) fn pending_destinations(
    record: &LoadBalancerRecord,
    writes: &[(String, String)],
) -> Vec<PendingDestination> {
    let Some(parsed) = ::service_common::arn::parse(&record.arn) else {
        return Vec::new();
    };
    destination_families()
        .into_iter()
        .filter_map(|family| {
            let enabled_key = format!("{family}{ENABLED_SUFFIX}");
            let bucket_key = format!("{family}{BUCKET_SUFFIX}");
            let prefix_key = format!("{family}{PREFIX_SUFFIX}");
            let named = [&enabled_key, &bucket_key, &prefix_key]
                .into_iter()
                .any(|key| writes.iter().any(|(written, _)| written == key));
            if !named {
                return None;
            }
            let after = |key: &str| -> Option<String> {
                writes
                    .iter()
                    .find(|(written, _)| written == key)
                    .map(|(_, value)| value.clone())
                    .or_else(|| attribute(record, key))
            };
            if !after(&enabled_key)
                .is_some_and(|value| value.trim().eq_ignore_ascii_case(SWITCHED_ON))
            {
                return None;
            }
            let prefix = after(&prefix_key)
                .map(|prefix| prefix.trim().trim_matches('/').to_string())
                .filter(|prefix| !prefix.is_empty());
            Some(PendingDestination {
                destination: super::access_logs::Destination {
                    bucket: after(&bucket_key).unwrap_or_default().trim().to_string(),
                    prefix,
                    region: record.region.clone(),
                    account_id: parsed.account.to_string(),
                },
                bucket_key,
                prefix_key,
                family: family.to_string(),
                partition: parsed.partition.to_string(),
                load_balancer_arn: record.arn.clone(),
            })
        })
        .collect()
}

/// The family whose delivery is the load balancer's ACCESS log, from the catalog's own
/// naming: the one destination Elastic Load Balancing verifies with a file of its own.
const ACCESS_LOGS_FAMILY: &str = "access_logs";

/// What `key` holds for `record` before this write: a caller's own earlier write, else the
/// value the attribute is carried at — the sibling module's one reading of an attribute of a
/// load balancer ([`super::access_logs::attribute`]), so what is judged here and what is
/// delivered cannot drift.
fn attribute(record: &LoadBalancerRecord, key: &str) -> Option<String> {
    super::access_logs::attribute(record, key)
}

impl PendingDestination {
    /// Why this destination cannot receive the load balancer's logs, or `None` for one the
    /// delivery could write into.
    ///
    /// Call it with NO store guard held — it awaits. Three clauses, in the order a caller can
    /// act on them, and in the CLASS Elastic Load Balancing answers each with: the prefix and
    /// the bucket name are the request's own values, so a prefix opening with the reserved
    /// segment, a write that names no bucket and a name Amazon S3 holds no bucket under are
    /// each the request to fix ([`crate::state::Refusal::Validation`]); the bucket's POLICY is
    /// a fact about resources that cannot hold the configuration together, which is the
    /// modelled "the requested configuration is not valid"
    /// ([`crate::state::Refusal::Configuration`]).
    pub(crate) async fn refusal(
        &self,
        ctx: &OperationContext<'_>,
    ) -> Option<crate::state::Refusal> {
        if let Some(prefix) = self.destination.prefix.as_deref() {
            if prefix.starts_with(DELIVERED_LOGS_SEGMENT) {
                return Some(crate::state::Refusal::Validation(reserved_prefix_message(
                    &self.prefix_key,
                )));
            }
        }
        let bucket = self.destination.bucket.as_str();
        if bucket.is_empty() {
            // A write that switches the delivery on without naming a bucket leaves the
            // destination incomplete: the switch alone says to deliver, and there is nowhere
            // to deliver to.
            return Some(crate::state::Refusal::Validation(missing_bucket_message(
                &self.bucket_key,
                &self.family,
            )));
        }
        match self.resolve(ctx).await {
            Resolution::Admitted | Resolution::Unknown => None,
            Resolution::NotFound => {
                Some(crate::state::Refusal::Validation(absent_bucket_message(bucket)))
            }
            Resolution::NotGranted => Some(crate::state::Refusal::Configuration(
                ungranted_bucket_message(bucket),
            )),
        }
    }

    /// What Amazon S3 says about this destination: whether it holds the bucket, and whether
    /// that bucket's policy admits the delivery's write under the path the attributes name.
    async fn resolve(&self, ctx: &OperationContext<'_>) -> Resolution {
        let bucket = self.destination.bucket.as_str();
        let region = self.destination.region.as_str();
        let client = ::service_common::s3_client::s3_client(ctx.callback(), region);
        // Narrowed to the name, since "all bucket names in the response begin with the
        // specified bucket name prefix".
        match client.list_buckets().prefix(bucket).send().await {
            Ok(answer) => {
                if !answer
                    .buckets()
                    .iter()
                    .any(|held| held.name() == Some(bucket))
                {
                    return Resolution::NotFound;
                }
            }
            Err(error) => {
                self.unresolved(&error);
                return Resolution::Unknown;
            }
        }
        match client.get_bucket_policy().bucket(bucket).send().await {
            Ok(answer) => match answer.policy() {
                Some(policy) if self.policy_admits_delivery(policy) => Resolution::Admitted,
                _ => Resolution::NotGranted,
            },
            Err(error)
                if ::aws_sdk_s3::error::ProvideErrorMetadata::code(&error)
                    == Some(NO_SUCH_BUCKET_POLICY) =>
            {
                Resolution::NotGranted
            }
            Err(error) => {
                self.unresolved(&error);
                Resolution::Unknown
            }
        }
    }

    /// Whether `policy` allows the delivery's own write ([`DELIVERY_WRITE_ACTION`]) under the
    /// path these attributes send the logs to.
    ///
    /// The decision is the shared IAM evaluator's
    /// ([`::service_common::iam_authorization::resource_decision`]), the one every service in
    /// this workspace decides a resource policy with — so `Principal`, `Resource`,
    /// `Condition` and an explicit `Deny` count here exactly as they count everywhere else.
    /// What this supplies is who is asking and what for: the log-delivery identities
    /// ([`Self::delivery_principals`]), and the destination's own ROOT
    /// (`super::access_logs::delivery_prefix`, `<prefix>/AWSLogs/<account>/`) — the first key
    /// the delivery writes there is the file that verifies the destination
    /// ([`super::access_logs::test_file_key`]), so the grant is judged at the shallowest path
    /// the delivery actually puts an object at, and a grant scoped to ANOTHER prefix of the
    /// same bucket does not admit this destination.
    ///
    /// The request also carries the CONDITION CONTEXT that write is made under
    /// ([`super::access_logs::delivery_condition_context`]) — the canned ACL the delivery
    /// names, the account and the load balancer the logs are for, and the transport it writes
    /// over — because a clause the request carries no value for leaves its statement undecided,
    /// and the log-delivery policy AWS publishes conditions its grant on exactly those facts.
    fn policy_admits_delivery(&self, policy: &str) -> bool {
        use ::service_common::iam_authorization::{resource_decision, Decision, Request};

        let resource = format!(
            "arn:{}:{S3_ARN_VENDOR}:::{}/{}",
            self.partition,
            self.destination.bucket,
            super::access_logs::delivery_prefix(&self.destination)
        );
        let principals = self.delivery_principals(policy);
        let context = super::access_logs::delivery_condition_context(
            &self.destination,
            &self.load_balancer_arn,
        );
        let request = Request::new(DELIVERY_WRITE_ACTION, &resource, &principals)
            .with_context(&context)
            .with_published_keys(super::access_logs::DELIVERY_CONDITION_KEYS);
        resource_decision(policy, &request) == Decision::Allowed
    }

    /// Every spelling of the log-delivery identity a bucket policy may grant that write to:
    /// the service principals AWS publishes for it, and the AWS ACCOUNTS `policy` itself names
    /// — because the older documented log-delivery policy grants the write to a per-Region
    /// Elastic Load Balancing account, whose id AWS publishes per Region and which this
    /// emulator does not model, so there is no id of its own to compare against. Only
    /// account-level principals count for that second reading (a root ARN or a bare account
    /// id, [`account_identities_named_by`]): a grant to one IAM user or role of an account is
    /// not a grant to the delivery.
    fn delivery_principals(&self, policy: &str) -> Vec<String> {
        let mut principals: Vec<String> = LOG_DELIVERY_SERVICE_PRINCIPALS
            .iter()
            .map(|principal| (*principal).to_string())
            .collect();
        principals.extend(account_identities_named_by(policy));
        principals
    }

    /// Verify this destination the way Elastic Load Balancing verifies one it accepted: by
    /// writing the file it names for exactly that into the destination's own folder. Only the
    /// ACCESS log destination has such a file.
    pub(crate) async fn verify(&self, ctx: &OperationContext<'_>) {
        if self.family != ACCESS_LOGS_FAMILY {
            return;
        }
        super::access_logs::deliver_test_file(ctx, &self.destination).await;
    }

    /// Trace a destination Amazon S3 could not be asked about, which is a deployment with no
    /// object store mounted rather than a bucket reported missing. The bucket name is the
    /// caller's own text, rendered in its escaped `Debug` form so a value carrying a newline
    /// cannot forge a log line.
    fn unresolved<E: ::std::error::Error + 'static>(&self, error: &E) {
        ::tracing::warn!(
            bucket = ?self.destination.bucket,
            error = %::aws_smithy_types::error::display::DisplayErrorContext(error),
            "the log destination a load balancer's attributes name could not be resolved \
             against Amazon S3"
        );
    }
}

/// What asking Amazon S3 about a destination answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Resolution {
    /// S3 holds the bucket and its policy admits the delivery's write under this path.
    Admitted,
    /// S3 answered, and it holds no bucket under that name.
    NotFound,
    /// S3 holds the bucket, and nothing in its policy admits the delivery's write here.
    NotGranted,
    /// S3 could not be ASKED at all, so nothing is known about the destination.
    Unknown,
}

/// The AWS-ACCOUNT identities `policy` names as principals: an account by its id, and an
/// account's root by its ARN. An identity INSIDE an account — an IAM user, a role, a session —
/// is not one of them. Read with the tree's own reading of the forms a policy names a
/// principal in ([`::service_common::iam_policy_principals`]), so nothing else counts as one.
fn account_identities_named_by(policy: &str) -> Vec<String> {
    let Ok(::serde_json::Value::Object(document)) = ::serde_json::from_str(policy) else {
        return Vec::new();
    };
    let Some(::serde_json::Value::Array(statements)) = document.get("Statement") else {
        return Vec::new();
    };
    statements
        .iter()
        .filter_map(|statement| statement.get("Principal"))
        .filter_map(|principal| principal.get("AWS"))
        .flat_map(|named| match named {
            ::serde_json::Value::String(value) => vec![value.clone()],
            ::serde_json::Value::Array(values) => values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect(),
            _ => Vec::new(),
        })
        .filter(|value| names_an_account(value))
        .collect()
}

/// Whether `value` names a whole AWS account: its bare id, or its root's ARN — the two
/// spellings of the same principal.
fn names_an_account(value: &str) -> bool {
    ::service_common::arn::is_account_id(value)
        || ::service_common::arn::parse(value)
            .is_some_and(|arn| arn.vendor == "iam" && arn.resource == "root")
}

/// Why a bucket Amazon S3 holds nothing under is refused, naming the bucket the attributes
/// sent the logs to: the caller has to recognise which bucket it asked for.
///
/// Recorded Elastic Load Balancing wording: the fixed text is `S3Bucket: ` and ` does not
/// exist`, and the only variable part is the name the request carried — the member is named as
/// the log bucket whichever of the destination families named it, because what is missing is
/// the BUCKET rather than the attribute.
pub(crate) fn absent_bucket_message(bucket: &str) -> String {
    format!("S3Bucket: {bucket} does not exist")
}

/// Why a write that switches a destination on without naming a bucket is refused: the switch
/// alone says to deliver and leaves nowhere to deliver to, so the key the request has to
/// carry is named, beside the delivery it was switching on — the family's own name written as
/// words ([`delivery_label`]).
pub(crate) fn missing_bucket_message(bucket_key: &str, family: &str) -> String {
    format!(
        "The key '{bucket_key}' must set in order to enable {}",
        delivery_label(family)
    )
}

/// The delivery a destination family IS, as a sentence names it: the family's own name from
/// the attribute catalog with its separators written as spaces (`access_logs` -> `access
/// logs`).
fn delivery_label(family: &str) -> String {
    family.replace('_', " ")
}

/// Why a bucket whose policy admits the delivery's write nowhere under this destination's own
/// path is refused, naming the bucket: what the caller has to go and fix is that bucket's
/// policy.
pub(crate) fn ungranted_bucket_message(bucket: &str) -> String {
    format!("Access Denied for bucket: {bucket}. Please check S3bucket permission")
}

/// Why a prefix that opens with the segment the layout owns is refused, naming the attribute:
/// the segment is written by the delivery itself, so a prefix beginning with it names a folder
/// no caller supplies.
pub(crate) fn reserved_prefix_message(prefix_key: &str) -> String {
    format!("The value of '{prefix_key}' cannot start with '{DELIVERED_LOGS_SEGMENT}'")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the destination families are read off the one catalog of load balancer
    /// attributes, so every `<name>.s3.enabled` switch it carries is a family this judges.
    #[test]
    fn every_log_destination_family_of_the_catalog_is_judged() {
        let families = destination_families();
        for family in &families {
            for suffix in [ENABLED_SUFFIX, BUCKET_SUFFIX, PREFIX_SUFFIX] {
                let key = format!("{family}{suffix}");
                assert!(
                    crate::state::load_balancer_attribute(&key).is_some(),
                    "case {family:?}: the catalog must carry {key:?} beside the switch"
                );
            }
        }
        assert!(
            families.contains(&ACCESS_LOGS_FAMILY),
            "the access-log family must be among the destinations judged, got {families:?}"
        );
    }

    /// RULE: the account identities a policy names are the ones a grant may be read for; a
    /// service principal or a value in no principal form is not an account.
    #[test]
    fn only_the_account_identities_a_policy_names_are_read_off_it() {
        let rows: [(&str, &str, Vec<&str>); 5] = [
            (
                "an account root",
                r#"{"Statement":[{"Principal":{"AWS":"arn:aws:iam::123456789012:root"}}]}"#,
                vec!["arn:aws:iam::123456789012:root"],
            ),
            (
                "a list of an id and a non-principal",
                r#"{"Statement":[{"Principal":{"AWS":["210987654321","nobody"]}}]}"#,
                vec!["210987654321"],
            ),
            (
                "a service principal alone",
                r#"{"Statement":[{"Principal":{"Service":"x.amazonaws.com"}}]}"#,
                vec![],
            ),
            (
                "an identity inside an account, which is not the account",
                r#"{"Statement":[{"Principal":{"AWS":"arn:aws:iam::123456789012:role/delivery"}}]}"#,
                vec![],
            ),
            ("text that is no policy", "not-a-policy", vec![]),
        ];
        for (case, policy, expected) in rows {
            let read = account_identities_named_by(policy);
            assert_eq!(
                read, expected,
                "case {case:?}: expected {expected:?}, got {read:?}"
            );
        }
    }
}
