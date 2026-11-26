// SPDX-License-Identifier: CC0-1.0

//! The resources a TRAIL REFERENCES, resolved in the services that own them.
//!
//! A trail is a delivery pipeline: "creates a trail that specifies the settings for
//! delivery of log data to an Amazon S3 bucket"
//! (`smithy_query(query="doc",shape="CreateTrail")`). Every resource its settings name
//! — the destination bucket, the notification topic, the encryption key and the
//! CloudWatch Logs destination — belongs to ANOTHER service, and CloudTrail resolves
//! each one before it accepts the trail, because a trail configured against a resource
//! that does not exist, or against one that does not let CloudTrail use it, could never
//! deliver a log file. Each failure has its own modelled error:
//! `S3BucketDoesNotExistException`, `InsufficientS3BucketPolicyException`,
//! `InsufficientSnsTopicPolicyException`, `InsufficientEncryptionPolicyException`,
//! `KmsKeyNotFoundException`, `InvalidCloudWatchLogsLogGroupArnException` and
//! `InvalidCloudWatchLogsRoleArnException`.
//!
//! The resolutions live HERE, once, because both operations that configure a trail
//! (`CreateTrail` and `UpdateTrail`) apply the same rule to the same members: neither
//! can disagree with the other about which destinations, topics, keys or log groups a
//! trail may be pointed at.
//!
//! Whether a referenced resource LETS CloudTrail use it is the ordinary IAM question —
//! may this principal perform this action on this resource? — so it is answered by the
//! shared evaluation of resource policies ([`service_common::iam_authorization`]) over
//! the document the owning service holds, as the service principal
//! [`SERVICE_PRINCIPAL`], carrying the condition keys such a grant is scoped by. Nothing
//! here reads a policy by itself.
//!
//! LOCK ORDER: every function takes the OWNING service's store lock, holds no other
//! while it does and hands back an owned value, so each must be called with NO guard of
//! this service's own store held. That keeps the cross-service access one-directional
//! and a lock cycle between two stores unreachable.

use service_common::iam_authorization::{self, Decision, Request};

/// The service principal CloudTrail delivers a trail's log files and notifications as —
/// the principal a destination bucket's policy and a notification topic's policy name to
/// grant it.
pub const SERVICE_PRINCIPAL: &str = "cloudtrail.amazonaws.com";

/// The Amazon S3 action a log-file delivery performs on the destination bucket.
const PUT_OBJECT_ACTION: &str = "s3:PutObject";

/// The Amazon SNS action a log-file notification performs on the topic.
const PUBLISH_ACTION: &str = "sns:Publish";

/// The condition key naming the canned ACL a request asks for, which a CloudTrail bucket
/// policy scopes its grant by.
const X_AMZ_ACL_CONDITION_KEY: &str = "s3:x-amz-acl";

/// The canned ACL CloudTrail writes a log file under, so the bucket's owner owns the
/// object it delivers.
///
/// The delivery's own constant
/// ([`service_common::trail_log_files::DELIVERY_CANNED_ACL`]): the write names this ACL, so
/// the grant judged here is the grant that write needs and the two cannot drift apart.
const BUCKET_OWNER_FULL_CONTROL: &str = ::service_common::trail_log_files::DELIVERY_CANNED_ACL;

/// The global condition key naming the resource a service-principal call is made ON
/// BEHALF OF: the trail, for a delivery CloudTrail makes for one.
const SOURCE_ARN_CONDITION_KEY: &str = "aws:SourceArn";

/// The global condition key naming the account that resource belongs to.
const SOURCE_ACCOUNT_CONDITION_KEY: &str = "aws:SourceAccount";

/// The global condition key saying whether the call was made over a secure transport, and
/// the value every delivery carries for it: the delivery reaches the owning service over
/// TLS, which is also what that service's own gate answers for every request it judges.
///
/// It is ANSWERED rather than left open because the delivery knows it, and a destination
/// whose policy refuses every write made outside TLS — the clause a hardened destination
/// carries beside the documented grant — admits the delivery on AWS and so must admit it
/// here.
const SECURE_TRANSPORT_CONDITION_KEY: &str = "aws:SecureTransport";
/// See [`SECURE_TRANSPORT_CONDITION_KEY`].
const SECURE_TRANSPORT_VALUE: &str = "true";

/// The condition keys a delivery CloudTrail makes FOR the trail `trail_arn` carries, so
/// a statement scoped by them is decided rather than left undecided.
///
/// These three and no others: the keys a grant to a SERVICE PRINCIPAL is scoped by, naming
/// the resource the call is made on behalf of and the account that resource belongs to, and
/// the transport the delivery is made over. A key about the CALLING account is deliberately
/// not among them — the call is not made by the trail's owner — so a statement conditioned
/// on one stays undecided, which is the fail-closed reading, and never turns into a grant
/// for this delivery.
fn delivery_context(trail_arn: &str) -> Vec<(String, String)> {
    vec![
        (SOURCE_ARN_CONDITION_KEY.to_string(), trail_arn.to_string()),
        (
            SECURE_TRANSPORT_CONDITION_KEY.to_string(),
            SECURE_TRANSPORT_VALUE.to_string(),
        ),
        (
            SOURCE_ACCOUNT_CONDITION_KEY.to_string(),
            ::engine::emulator_account_id().to_string(),
        ),
    ]
}

/// The CloudTrail path a trail's log files are delivered under, below the `S3KeyPrefix`
/// the trail carries: `AWSLogs/<account>/CloudTrail/<region>/`.
///
/// It is the resource a bucket policy's `s3:PutObject` grant is written over, so the
/// permission question is asked about the keys this trail would really write rather than
/// about the bucket as a whole — which is what makes a policy that grants the write
/// somewhere else insufficient for this trail. The layout itself belongs to the delivery
/// that writes those keys ([`service_common::trail_log_files`]), so it is read from there
/// rather than written again here.
fn log_file_key_prefix(s3_key_prefix: Option<&str>, region: &str) -> String {
    ::service_common::trail_log_files::log_file_key_prefix(
        s3_key_prefix,
        ::engine::emulator_account_id(),
        region,
    )
}

/// What the account's object store says about a trail's log-delivery destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DestinationVerdict {
    /// The bucket exists and its policy lets CloudTrail write this trail's log files.
    ///
    /// It is also the answer when the object store cannot be reached at all — an engine
    /// this emulator's object store is not mounted on — which is a fact about the engine
    /// and not about the bucket, so a caller is served rather than refused on a
    /// resolution that never happened.
    Accepted,
    /// The account holds no bucket of that name, so the trail has nowhere to deliver to.
    NoSuchBucket,
    /// The bucket exists, and its policy does not let CloudTrail write this trail's log
    /// files into it.
    PolicyInsufficient,
}

/// Resolve a trail's log-delivery destination: the bucket has to exist, and its policy
/// has to let CloudTrail write the log files this trail would deliver.
///
/// `s3_key_prefix` is the prefix the trail delivers under and `region` its home Region,
/// because together they decide WHICH keys the delivery writes
/// ([`log_file_key_prefix`]); `trail_arn` is the trail the delivery is made for, which a
/// grant to the CloudTrail service principal is scoped by.
pub fn destination_verdict<Eng: Send + Sync + 'static>(
    engine: &Eng,
    bucket: &str,
    s3_key_prefix: Option<&str>,
    region: &str,
    trail_arn: &str,
) -> DestinationVerdict {
    let Some(state) = ::service_com_amazonaws_s3_amazons3::state::service_state(engine) else {
        return DestinationVerdict::Accepted;
    };
    // The object store's lock is taken, read and released before anything is decided, so
    // no other lock is ever held across it.
    let held = {
        let store = state.lock();
        match store.bucket(bucket) {
            None => return DestinationVerdict::NoSuchBucket,
            Some(held) => held.policy.clone(),
        }
    };
    // "A bucket is created WITHOUT a policy": a bucket that carries none grants the
    // CloudTrail service principal nothing, which is exactly the insufficient policy the
    // modelled error is about.
    let Some(policy) = held else {
        return DestinationVerdict::PolicyInsufficient;
    };
    let resource = format!(
        "{}/{}",
        ::service_com_amazonaws_s3_amazons3::state::bucket_arn(bucket),
        log_file_key_prefix(s3_key_prefix, region)
    );
    let principals = [SERVICE_PRINCIPAL.to_string()];
    let mut context = delivery_context(trail_arn);
    context.push((
        X_AMZ_ACL_CONDITION_KEY.to_string(),
        BUCKET_OWNER_FULL_CONTROL.to_string(),
    ));
    let request =
        Request::new(PUT_OBJECT_ACTION, &resource, &principals).with_context(&context);
    match iam_authorization::resource_decision(&policy, &request) {
        Decision::Allowed => DestinationVerdict::Accepted,
        // A policy that states no grant for the delivery is as insufficient as one that
        // denies it: either way CloudTrail may not write the log file.
        Decision::Denied | Decision::NotStated => DestinationVerdict::PolicyInsufficient,
    }
}

/// What the account's Amazon SNS store says about a trail's notification topic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopicVerdict {
    /// The topic's access policy lets CloudTrail publish to it — or the Amazon SNS store
    /// could not be reached, on the terms [`DestinationVerdict::Accepted`] states.
    Accepted,
    /// The topic's access policy does not let CloudTrail publish to it, which is also
    /// the answer for a topic that does not exist: a topic the account does not hold
    /// carries no policy that could permit the publish, and the API models no
    /// topic-does-not-exist error of its own for this member.
    PolicyInsufficient,
}

/// Resolve a trail's notification topic: its access policy has to let CloudTrail publish
/// the log-file-delivered notification the trail asks for.
pub fn notification_topic_verdict<Eng: Send + Sync + 'static>(
    engine: &Eng,
    topic_arn: &str,
    trail_arn: &str,
) -> TopicVerdict {
    let Some(state) = ::service_com_amazonaws_sns_amazonsimplenotificationservice::state::
        service_state(engine)
    else {
        return TopicVerdict::Accepted;
    };
    // The topic reports the policy its owner set, or the one Amazon SNS generates for a
    // topic with none, exactly as `GetTopicAttributes` reports it.
    let policy = {
        let store = state.lock();
        store
            .topics
            .get(topic_arn)
            .map(|topic| topic.policy_attribute())
    };
    let Some(policy) = policy else {
        return TopicVerdict::PolicyInsufficient;
    };
    let principals = [SERVICE_PRINCIPAL.to_string()];
    let context = delivery_context(trail_arn);
    let request = Request::new(PUBLISH_ACTION, topic_arn, &principals).with_context(&context);
    match iam_authorization::resource_decision(&policy, &request) {
        Decision::Allowed => TopicVerdict::Accepted,
        Decision::Denied | Decision::NotStated => TopicVerdict::PolicyInsufficient,
    }
}

/// What the account's KMS store says about a trail's `KmsKeyId`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncryptionKeyVerdict {
    /// The identifier names a key this account and Region holds, and the trail stores it
    /// as the FULLY SPECIFIED key ARN of the key it resolved to — "the value is a fully
    /// specified ARN to a KMS key"
    /// (`smithy_query(query="doc",shape="Trail",member="KmsKeyId")`) — whichever of the
    /// four request forms spelt it.
    ///
    /// The ARN is absent only when the KMS store could not be reached at all, on the
    /// terms [`DestinationVerdict::Accepted`] states: there is no key to name, so the
    /// identifier the request gave is what the trail keeps.
    Accepted { key_arn: Option<String> },
    /// The identifier is a well-formed identifier of a key of the trail's OWN Region and
    /// account that resolves to no usable key: `InsufficientEncryptionPolicyException`,
    /// "thrown when the policy on the S3 bucket or KMS key does not have sufficient
    /// permissions for the operation" — the one a caller fixes by creating the key or
    /// correcting its key policy.
    EncryptionPolicyInsufficient,
    /// The identifier names no key of this Region at all: it is in no form a KMS key or
    /// alias identifier, or it is an ARN pinned to another Region or another account.
    /// `KmsKeyNotFoundException`, "thrown when the KMS key does not exist, when the S3
    /// bucket and the KMS key are not in the same Region" — the one a caller fixes by
    /// naming a key of this Region.
    NotFound,
}

/// Resolve a trail's `KmsKeyId`, in every form the member documents: "an alias name
/// prefixed by `alias/`, a fully specified ARN to an alias, a fully specified ARN to a
/// key, or a globally unique identifier"
/// (`smithy_query(query="doc",shape="CreateTrailRequest",member="KmsKeyId")`). The forms
/// differ only in how they SPELL one key, so the one resolver KMS itself uses reads all
/// of them.
///
/// `region` is the Region the request was signed for, and a KMS key is a regional
/// resource, so the Region is what splits the two refusals the API models: an identifier
/// this Region's endpoint could have served, which did not resolve, is a key-or-policy
/// problem ([`EncryptionKeyVerdict::EncryptionPolicyInsufficient`]), while one it could
/// never serve — a value in no key identifier form, or an ARN of another Region or
/// account — is a key that is not there ([`EncryptionKeyVerdict::NotFound`]).
pub fn encryption_key_verdict<Eng: Send + Sync + 'static>(
    engine: &Eng,
    region: &str,
    key_id: &str,
) -> EncryptionKeyVerdict {
    let Some(state) = ::service_com_amazonaws_kms_trentservice::state::service_state(engine)
    else {
        return EncryptionKeyVerdict::Accepted { key_arn: None };
    };
    let resolved = {
        let store = state.lock();
        match store.resolve(region, key_id) {
            ::service_com_amazonaws_kms_trentservice::state::KeyRef::Found(key) => {
                Some(key.arn.clone())
            }
            _ => None,
        }
    };
    match resolved {
        Some(key_arn) => EncryptionKeyVerdict::Accepted {
            key_arn: Some(key_arn),
        },
        None if names_a_key_of_this_region(region, key_id) => {
            EncryptionKeyVerdict::EncryptionPolicyInsufficient
        }
        None => EncryptionKeyVerdict::NotFound,
    }
}

/// Whether `key_id` is an identifier THIS Region's KMS endpoint could have served: a key
/// id or `alias/<name>` of the account's own, or a key or alias ARN of this Region and
/// account.
///
/// It is what the two modelled refusals are split on ([`encryption_key_verdict`]). The
/// identifier forms and the key-id grammar are KMS's own
/// ([`service_com_amazonaws_kms_trentservice::state::names_a_key_id`]), read from there
/// rather than written again here, so CloudTrail and KMS cannot disagree about which
/// values name a key.
fn names_a_key_of_this_region(region: &str, key_id: &str) -> bool {
    use ::service_com_amazonaws_kms_trentservice::state as kms;

    if let Some(arn) = ::service_common::arn::parse(key_id) {
        if arn.vendor != kms::ARN_SERVICE
            || arn.region != region
            || arn.account != ::engine::emulator_account_id()
        {
            return false;
        }
        return match arn.resource.split_once('/') {
            Some((KEY_ARN_RESOURCE_TYPE, id)) => kms::names_a_key_id(id),
            Some((ALIAS_ARN_RESOURCE_TYPE, name)) => !name.is_empty(),
            _ => false,
        };
    }
    match key_id.strip_prefix(kms::ALIAS_PREFIX) {
        Some(name) => !name.is_empty(),
        None => kms::names_a_key_id(key_id),
    }
}

/// The resource type of a KMS key ARN, `arn:aws:kms:<region>:<account>:key/<key id>`.
const KEY_ARN_RESOURCE_TYPE: &str = "key";

/// The resource type of a KMS ALIAS ARN, `arn:aws:kms:<region>:<account>:alias/<name>` —
/// the other resource a `KmsKeyId` may name.
const ALIAS_ARN_RESOURCE_TYPE: &str = "alias";

/// What the account's CloudWatch Logs store says about the log-group half of a trail's
/// CloudWatch Logs configuration — and about the PAIR that configuration is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudWatchLogsVerdict {
    /// The trail delivers to no log group, or to a log group this account and Region
    /// holds with a role to assume for the write.
    Accepted,
    /// One half of the configuration is there and the other is not. "Not required unless
    /// you specify CloudWatchLogsRoleArn"
    /// (`smithy_query(query="doc",shape="CreateTrailRequest",member="CloudWatchLogsLogGroupArn")`):
    /// neither half configures a delivery on its own, so the half that is MISSING is the
    /// one the refusal is reported as.
    HalfMissing(CloudWatchLogsHalf),
    /// The role half is not an IAM role ARN of this account at all, so CloudTrail cannot
    /// read which role it would assume: "You must use a role that exists in your
    /// account"
    /// (`smithy_query(query="doc",shape="CreateTrailRequest",member="CloudWatchLogsRoleArn")`).
    UnreadableRoleArn,
    /// The log-group half is not a log-group ARN of this account and Region at all, so
    /// CloudTrail cannot read which log group it names.
    UnreadableLogGroupArn,
    /// The log-group half is a log-group ARN of this account and Region naming a log
    /// group that does not exist: "You must use a log group that exists in your account."
    LogGroupAbsent,
}

/// Which half of a trail's CloudWatch Logs configuration a request left out
/// ([`CloudWatchLogsVerdict::HalfMissing`]). Each half has its own modelled error —
/// `InvalidCloudWatchLogsLogGroupArnException` and
/// `InvalidCloudWatchLogsRoleArnException` — so which one is missing decides which error
/// the pair is refused with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudWatchLogsHalf {
    /// `CloudWatchLogsLogGroupArn`, the log group the trail's events are delivered to.
    LogGroup,
    /// `CloudWatchLogsRoleArn`, the role CloudTrail assumes to write them.
    Role,
}

/// Resolve the CloudWatch Logs destination a trail would deliver to: the two members are
/// one configuration, so they are judged together — the ROLE half first, because a
/// delivery whose role CloudTrail cannot read writes nothing wherever the log group is —
/// and the log group the other half names has to be one this account and Region holds.
///
/// `log_group_arn` and `role_arn` are the halves the REQUEST carries, and `None` is a
/// half it does not configure, which an empty value also is: neither names a log group or
/// a role.
pub fn cloud_watch_logs_verdict<Eng: Send + Sync + 'static>(
    engine: &Eng,
    region: &str,
    log_group_arn: Option<&str>,
    role_arn: Option<&str>,
) -> CloudWatchLogsVerdict {
    match (log_group_arn, role_arn) {
        (None, None) => return CloudWatchLogsVerdict::Accepted,
        (Some(_), None) => {
            return CloudWatchLogsVerdict::HalfMissing(CloudWatchLogsHalf::Role);
        }
        (None, Some(_)) => {
            return CloudWatchLogsVerdict::HalfMissing(CloudWatchLogsHalf::LogGroup);
        }
        (Some(_), Some(_)) => {}
    }
    // The role is an IAM role of THIS account — the identity CloudTrail assumes to write
    // the log stream — read against the tree's one reading of what a role ARN is
    // ([`service_common::arn::IAM_ROLE`]).
    let role = role_arn.unwrap_or_default();
    if !::service_common::arn::IAM_ROLE.admits(role)
        || ::service_common::arn::parse(role)
            .is_none_or(|arn| arn.account != ::engine::emulator_account_id())
    {
        return CloudWatchLogsVerdict::UnreadableRoleArn;
    }
    let arn = log_group_arn.unwrap_or_default();
    let Some(parsed) = ::service_common::arn::parse(arn) else {
        return CloudWatchLogsVerdict::UnreadableLogGroupArn;
    };
    if parsed.vendor != LOG_GROUP_ARN_VENDOR
        || parsed.region != region
        || parsed.account != ::engine::emulator_account_id()
        || parsed.resource_type() != LOG_GROUP_ARN_RESOURCE_TYPE
    {
        return CloudWatchLogsVerdict::UnreadableLogGroupArn;
    }
    let name = ::service_com_amazonaws_cloudwatchlogs_logs_20140328::state::
        log_group_name_from_identifier(arn);
    if !::service_common::cloudwatch_logs::is_log_group_name(name) {
        return CloudWatchLogsVerdict::UnreadableLogGroupArn;
    }
    let Some(state) =
        ::service_com_amazonaws_cloudwatchlogs_logs_20140328::state::service_state(engine)
    else {
        return CloudWatchLogsVerdict::Accepted;
    };
    let held = {
        let store = state.lock();
        store.log_groups.contains_key(name)
    };
    if held {
        CloudWatchLogsVerdict::Accepted
    } else {
        CloudWatchLogsVerdict::LogGroupAbsent
    }
}

/// The ARN vendor of a CloudWatch Logs resource, and the resource type of a log group's
/// own ARN: `arn:aws:logs:<region>:<account>:log-group:<name>`.
const LOG_GROUP_ARN_VENDOR: &str = "logs";
/// See [`LOG_GROUP_ARN_VENDOR`].
const LOG_GROUP_ARN_RESOURCE_TYPE: &str = "log-group";

/// What the object store holds of a trail's DELIVERED log files: the instant the newest one
/// under this trail's own log-file area was written, or `None` for a trail that has
/// delivered none.
///
/// It is read in the store that OWNS those objects rather than tracked here, so a trail
/// reports a delivery exactly when the bucket really holds the file it delivered: the one
/// account of what was delivered is the delivered object itself. The area read is the
/// trail's whole log-file prefix ([`service_common::trail_log_files::trail_log_file_prefix`])
/// — every Region the trail logs delivers under it — and only the objects a log file's name
/// ends with are counted, so an unrelated object a client put under the same prefix is not
/// read as a delivery.
pub fn latest_log_file_delivery<Eng: Send + Sync + 'static>(
    engine: &Eng,
    bucket: &str,
    s3_key_prefix: Option<&str>,
) -> Option<::std::time::SystemTime> {
    let state = ::service_com_amazonaws_s3_amazons3::state::service_state(engine)?;
    let prefix = ::service_common::trail_log_files::trail_log_file_prefix(
        s3_key_prefix,
        ::engine::emulator_account_id(),
    );
    let store = state.lock();
    store
        .bucket(bucket)?
        .objects()
        .filter(|(key, _)| {
            key.starts_with(&prefix)
                && key.ends_with(::service_common::trail_log_files::LOG_FILE_SUFFIX)
        })
        .map(|(_, object)| object.last_modified)
        .max()
}

/// The refusal text for `InsufficientS3BucketPolicyException`: the destination bucket
/// whose policy does not let CloudTrail write the trail's log files.
///
/// Request-derived — the `S3BucketName` the caller sent — because the caller fixes the
/// call by putting a bucket policy on THAT bucket.
pub fn insufficient_s3_bucket_policy_message(bucket: &str) -> String {
    format!("Incorrect S3 bucket policy is detected for bucket: {bucket}")
}

/// The HTTP status an insufficient-bucket-policy refusal answers with.
///
/// The model annotates `InsufficientS3BucketPolicyException` `@httpError(403)`, but
/// CloudTrail answers 400 for it on the wire: the recorded response of `CreateTrail`
/// against an existing bucket that carries no CloudTrail delivery policy is HTTP 400
/// with that code. The status is what a client's retry and error classification branches
/// on before the payload, and a misconfigured bucket policy is a request it cannot
/// satisfy rather than a call it should re-sign. The error CODE and the body stay the
/// model's; only the status is stated here, through
/// [`crate::scaffolded::Scaffolded::modeled_with_status`] — the same deviation
/// [`crate::state::S3_BUCKET_DOES_NOT_EXIST_STATUS`] already carries for the other
/// refusal of the same destination.
pub const INSUFFICIENT_S3_BUCKET_POLICY_STATUS: u16 = 400;

/// The refusal text for `InsufficientSnsTopicPolicyException`: a topic the trail may not
/// be notified through, because the account holds no topic of that name or because the
/// topic's access policy does not let CloudTrail publish to it.
///
/// ENTIRELY FIXED text — it names neither the topic nor the trail: the one sentence
/// covers both ways the reference fails, which is exactly the pair
/// [`TopicVerdict::PolicyInsufficient`] stands for. A topic name that is not a topic name
/// at all is the separate `InvalidSnsTopicNameException`
/// ([`invalid_sns_topic_name_message`]), which does name the value.
pub const INSUFFICIENT_SNS_TOPIC_POLICY_MESSAGE: &str =
    "SNS Topic does not exist or the topic policy is incorrect!";

/// The refusal text for `InvalidSnsTopicNameException`, "thrown when the provided SNS
/// topic name is not valid": the `SnsTopicName` the request gave, which is its one
/// variable part.
pub fn invalid_sns_topic_name_message(topic_name: &str) -> String {
    format!("Invalid SNS topic name: {topic_name} is found!")
}

/// Whether `topic` names a topic at all: the member is "the name of the Amazon SNS topic
/// defined for notification of log file delivery", and the trail keeps the ARN either
/// spelling resolves to, so a topic name SNS would accept and the ARN of such a topic are
/// both values that name one.
///
/// The name rule and the ARN form are Amazon SNS's own
/// ([`service_com_amazonaws_sns_amazonsimplenotificationservice::state::topic_name_is_valid`]),
/// read from there rather than written again here: a name CloudTrail accepts for a trail
/// cannot be one Amazon SNS could not hold a topic under. A value in neither form is the
/// modelled `InvalidSnsTopicNameException` ([`invalid_sns_topic_name_message`]), refused
/// before any topic is looked up.
pub fn names_a_topic(topic: &str) -> bool {
    use ::service_com_amazonaws_sns_amazonsimplenotificationservice::state as sns;

    sns::topic_name_is_valid(topic) || sns::topic_name_from_arn(topic).is_some()
}

/// The refusal text for `InsufficientEncryptionPolicyException` on a trail's `KmsKeyId`:
/// the two resources whose policies a log-file delivery needs, the trail's destination
/// bucket and the key the identifier names.
///
/// `bucket` is the `S3BucketName` the trail delivers to and `key_arn` the identifier
/// expanded to the fully specified ARN of the key or alias it names
/// ([`fully_specified_key_arn`]), because those are what the caller grants CloudTrail on.
pub fn insufficient_encryption_policy_message(bucket: &str, key_arn: &str) -> String {
    format!("Insufficient permissions to access S3 bucket {bucket} or KMS key {key_arn}.")
}

/// The refusal text for `KmsKeyNotFoundException` on a trail's `KmsKeyId`, inside the
/// rule the model states for that error ("thrown when the KMS key does not exist, when
/// the S3 bucket and the KMS key are not in the same Region").
///
/// The two variable parts are the same as
/// [`insufficient_encryption_policy_message`]'s: the trail's destination bucket and the
/// identifier as a fully specified ARN, since the sentence is about the two being in one
/// Region.
pub fn kms_key_not_found_message(bucket: &str, key_arn: &str) -> String {
    format!(
        "KMS key ID {key_arn} does not exist, or s3 bucket {bucket} and key are not in \
         the same region."
    )
}

/// A `KmsKeyId` as the fully specified ARN of the key or alias it names, which is the one
/// form a trail reports its key in and the form the refusals above name it in.
///
/// The member accepts four spellings of one key — "an alias name prefixed by `alias/`, a
/// fully specified ARN to an alias, a fully specified ARN to a key, or a globally unique
/// identifier" — so an identifier that is already an ARN is the ARN it is, a bare
/// `alias/<name>` is that alias of `region`, and anything else is a key id of `region`.
/// The ARN builders are KMS's own, so the ARN CloudTrail names a key by is the one KMS
/// issues for it.
pub fn fully_specified_key_arn(region: &str, key_id: &str) -> String {
    use ::service_com_amazonaws_kms_trentservice::state as kms;

    if ::service_common::arn::parse(key_id).is_some() {
        return key_id.to_string();
    }
    if key_id.starts_with(kms::ALIAS_PREFIX) {
        return kms::alias_arn(region, key_id);
    }
    kms::key_arn(region, key_id)
}

/// The refusal text for a CloudWatch Logs log group that does not exist: "You must use a
/// log group that exists in your account"
/// (`smithy_query(query="doc",shape="CreateTrailRequest",member="CloudWatchLogsLogGroupArn")`).
///
/// FIXED text, with no part derived from the request — one of the three sentences that
/// tell a caller WHICH half of the CloudWatch Logs configuration is wrong
/// ([`CloudWatchLogsVerdict`]).
pub const LOG_GROUP_ABSENT_MESSAGE: &str = "You must specify an existing log group.";

/// The refusal text for a `CloudWatchLogsLogGroupArn` that is not a log-group ARN of this
/// account and Region: CloudTrail cannot read which log group it names, so there is
/// nothing to look up. See [`LOG_GROUP_ABSENT_MESSAGE`].
pub const UNREADABLE_LOG_GROUP_ARN_MESSAGE: &str =
    "CloudTrail cannot validate the specified log group ARN.";

/// The refusal text for a `CloudWatchLogsRoleArn` that is not an IAM role ARN of this
/// account: the same sentence the log-group half takes for a value CloudTrail cannot read
/// as the ARN it needs ([`UNREADABLE_LOG_GROUP_ARN_MESSAGE`]), about the role.
pub const UNREADABLE_ROLE_ARN_MESSAGE: &str =
    "CloudTrail cannot validate the specified role ARN.";

/// The refusal text for a CloudWatch Logs configuration with only one of its two halves:
/// a log group without a role to write it with, or a role with no log group to write to,
/// configures no delivery. See [`LOG_GROUP_ABSENT_MESSAGE`].
pub const LOG_GROUP_AND_ROLE_REQUIRED_MESSAGE: &str =
    "You must specify a log group and a role ARN.";
