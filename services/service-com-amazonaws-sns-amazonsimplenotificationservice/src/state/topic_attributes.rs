// SPDX-License-Identifier: CC0-1.0

//! The rules ONE attribute of a topic is held to.
//!
//! Every name in [`super::TOPIC_ATTRIBUTE_NAMES`] carries a value SNS documents a domain
//! for — a closed value set, a numeric range, a policy document, a resource ARN, a
//! length — and some of them apply only to a topic of one KIND or to a topic whose
//! subscriptions can carry what the attribute asks for
//! (`SetTopicAttributesInput$AttributeName` doc). A value outside that domain is refused
//! with `InvalidParameterException`, and the topic is left exactly as it was.
//!
//! The rules live here, beside the state they judge, because they are SHARED: an
//! attribute value `CreateTopic` accepts cannot be one `SetTopicAttributes` refuses, and
//! both calls therefore state the same sentence for the same condition.

use std::collections::HashMap;

use super::{
    delivery_policy_refusal, flag_attribute_refusal, flag_attribute_value, json_object,
    AttributeMember, TopicRecord, ARCHIVE_POLICY_ATTRIBUTE, CONTENT_BASED_DEDUPLICATION_ATTRIBUTE,
    DEFAULT_MAXIMUM_MESSAGE_SIZE, DELIVERY_POLICY_ATTRIBUTE, DISPLAY_NAME_ATTRIBUTE,
    FEEDBACK_ROLE_ARN_SUFFIX, FIFO_THROUGHPUT_SCOPE_ATTRIBUTE, FIFO_THROUGHPUT_SCOPE_MESSAGE_GROUP,
    FIFO_TOPIC_ATTRIBUTE, KMS_MASTER_KEY_ID_ATTRIBUTE, MAXIMUM_MESSAGE_SIZE_ATTRIBUTE,
    MAX_MAXIMUM_MESSAGE_SIZE, MIN_MAXIMUM_MESSAGE_SIZE, POLICY_ATTRIBUTE, TopicPolicyDocument,
};

/// The longest display name SNS accepts: a topic's display name is at most 100
/// characters, and a longer one is refused rather than truncated.
pub const MAX_DISPLAY_NAME_CHARS: usize = 100;

/// The tracing modes SNS defines: "By default TracingConfig is set to PassThrough …
/// If set to Active" (`SetTopicAttributesInput$AttributeName` doc).
pub const TRACING_CONFIG_ATTRIBUTE: &str = "TracingConfig";
const TRACING_CONFIG_VALUES: &[&str] = &["PassThrough", "Active"];

/// The signature versions SNS defines: "The signature version corresponds to the
/// hashing algorithm used while creating the signature … By default SignatureVersion
/// is set to 1" (`SetTopicAttributesInput$AttributeName` doc); SNS publishes 1 (SHA1)
/// and 2 (SHA256).
pub const SIGNATURE_VERSION_ATTRIBUTE: &str = "SignatureVersion";
const SIGNATURE_VERSION_VALUES: &[&str] = &["1", "2"];

/// The deduplication scopes SNS defines for a FIFO topic: "`Topic` – The scope of
/// message deduplication is across the entire topic … `MessageGroup` – The scope of
/// deduplication is within each individual message group"
/// (`SetTopicAttributesInput$AttributeName` doc).
const FIFO_THROUGHPUT_SCOPE_TOPIC: &str = "Topic";
const FIFO_THROUGHPUT_SCOPE_VALUES: &[&str] = &[
    FIFO_THROUGHPUT_SCOPE_TOPIC,
    FIFO_THROUGHPUT_SCOPE_MESSAGE_GROUP,
];

/// The value changes SNS does not perform: `(attribute, from, to)` triples a write is
/// refused for, whatever the value's own domain says.
///
/// Some attribute values are one-way. The throughput scope is one: moving a FIFO topic's
/// deduplication (and its throughput) from the whole topic down into each message group
/// is a change SNS makes, and moving it back up is not — the message groups the topic has
/// been publishing under cannot be re-gathered into one scope. Recorded evidence:
/// `FifoThroughputScope` written back to `Topic` after `MessageGroup` answers `400
/// InvalidParameter` with [`unsupported_change_refusal`]'s sentence, and the topic keeps
/// the scope it held.
const UNSUPPORTED_VALUE_CHANGES: &[(&str, &str, &str)] = &[(
    FIFO_THROUGHPUT_SCOPE_ATTRIBUTE,
    FIFO_THROUGHPUT_SCOPE_MESSAGE_GROUP,
    FIFO_THROUGHPUT_SCOPE_TOPIC,
)];

/// The bounds of a delivery-status sample rate: "the sample rate percentage (0-100)"
/// (`SetTopicAttributesInput$AttributeName` doc).
const MIN_SAMPLE_RATE: i64 = 0;
const MAX_SAMPLE_RATE: i64 = 100;

/// The suffix of every delivery-status sample-rate attribute
/// (`HTTPSuccessFeedbackSampleRate`, `SQSSuccessFeedbackSampleRate`, …).
const SAMPLE_RATE_SUFFIX: &str = "SampleRate";

/// The resource part an IAM ROLE's ARN carries (`arn:aws:iam::<account>:role/<name>`),
/// which is the form every delivery-status feedback role is named by.
const ROLE_RESOURCE_PREFIX: &str = "role/";

/// The service an IAM role's ARN names.
const IAM_SERVICE: &str = "iam";

/// The subscription protocols a message larger than [`DEFAULT_MAXIMUM_MESSAGE_SIZE`] can
/// be delivered over: "each subscription must be an Amazon SQS, Amazon Data Firehose, or
/// Lambda subscription" (`SetTopicAttributesInput$AttributeName` doc, of a
/// `MaximumMessageSize` above 256 KiB).
pub const LARGE_PAYLOAD_PROTOCOLS: &[&str] = &["sqs", "firehose", "lambda"];

/// How many subscriptions a topic raising its `MaximumMessageSize` above 256 KiB may
/// hold: "A topic with a MaximumMessageSize above 256 KiB must have 100 or fewer
/// subscriptions" (`SetTopicAttributesInput$AttributeName` doc).
pub const MAX_LARGE_PAYLOAD_SUBSCRIPTIONS: usize = 100;

/// The attributes SNS documents for a FIFO topic alone: "The following attribute applies
/// only to FIFO topics: ArchivePolicy … ContentBasedDeduplication"
/// (`SetTopicAttributesInput$AttributeName` doc), and the deduplication scope that only
/// a FIFO topic deduplicates by.
pub const FIFO_ONLY_ATTRIBUTES: &[&str] = &[
    ARCHIVE_POLICY_ATTRIBUTE,
    CONTENT_BASED_DEDUPLICATION_ATTRIBUTE,
    FIFO_THROUGHPUT_SCOPE_ATTRIBUTE,
];

/// The attributes SNS documents for a STANDARD topic alone: tracing, which "is only
/// supported on standard topics" (`SetTopicAttributesInput$AttributeName` doc).
pub const STANDARD_ONLY_ATTRIBUTES: &[&str] = &[TRACING_CONFIG_ATTRIBUTE];

/// The topic a write is judged against: what the attribute map the write would leave
/// has to be consistent WITH, as against the value's own domain.
///
/// It is the topic as it will be once the write lands — its kind, and the subscriptions
/// it holds — so a create judges the kind it is creating and an update the kind the
/// topic already is.
#[derive(Debug, Clone, Copy)]
pub struct TopicSubject<'a> {
    /// Whether this is (or is being created as) a FIFO topic.
    pub is_fifo: bool,
    /// The delivery protocol of every subscription the topic holds.
    pub subscription_protocols: &'a [String],
}

impl<'a> TopicSubject<'a> {
    /// The subject a write to the topic `topic` holding the subscriptions of
    /// `subscription_protocols` is judged against.
    pub fn of(topic: &TopicRecord, subscription_protocols: &'a [String]) -> Self {
        Self {
            is_fifo: topic.is_fifo,
            subscription_protocols,
        }
    }
}

/// Why a write of ONE topic attribute is refused: the sentence, and which modelled
/// error the service states it with.
///
/// Nearly every condition is a parameter fault (`InvalidParameterException`), but a role
/// SNS could not use on the caller's behalf is an AUTHORIZATION fault
/// (`AuthorizationErrorException`): the value names a real role in the real form, and
/// what fails is the service's ability to act as it.
#[derive(Debug, Clone)]
pub enum TopicAttributeRefusal {
    /// The value is not one the attribute takes.
    InvalidParameter(String),
    /// The value names a resource this account's SNS may not act as.
    Unauthorized(String),
}

impl TopicAttributeRefusal {
    /// The sentence this refusal states.
    pub fn message(&self) -> &str {
        match self {
            Self::InvalidParameter(message) | Self::Unauthorized(message) => message,
        }
    }
}

/// The refusal a write of the topic attribute `name` to `value` answers, or `None` when
/// the value is one the attribute accepts on this topic.
///
/// Each message is the whole sentence SNS states for that condition — the attribute as
/// the service names it, and the value and the bounds only where that sentence carries
/// them — rather than a diagnostic a caller wraps.
///
/// `member` is the input member the write arrived in ([`AttributeMember`]), which is what
/// frames a condition SNS states as a SENTENCE OF ITS OWN: such a sentence names no
/// attribute, so a write carried in an `Attributes` MAP is told which member of the request
/// holds the value at fault, while one that was given the attribute BY NAME needs no such
/// frame — the name being the whole input. Recorded evidence: a `TracingConfig` of `Sampled`
/// answers `400 InvalidParameter` with `Invalid parameter: Attributes Reason: Invalid tracing
/// config value: Sampled` through `CreateTopic`, and with `Invalid parameter: Invalid tracing
/// config value: Sampled` through `SetTopicAttributes`.
pub fn topic_attribute_refusal(
    name: &str,
    value: &str,
    subject: TopicSubject<'_>,
    member: AttributeMember,
) -> Option<TopicAttributeRefusal> {
    if let Some(message) = kind_refusal(name, subject.is_fifo) {
        return Some(TopicAttributeRefusal::InvalidParameter(message));
    }
    if name.ends_with(FEEDBACK_ROLE_ARN_SUFFIX) {
        return feedback_role_refusal(name, value);
    }
    value_refusal(name, value, subject, member).map(TopicAttributeRefusal::InvalidParameter)
}

/// The sentence SNS refuses a value CHANGE it does not perform with: the attribute, the
/// value it holds and the value the write asks for.
///
/// Fixed text around the three names, all of them the service's own: the attribute and
/// the two values are what a caller has to act on, and nothing else of the request is
/// echoed.
fn unsupported_change_refusal(name: &str, from: &str, to: &str) -> String {
    format!("Invalid parameter: Modifying {name} from {from} to {to} is not supported")
}

/// The refusal a write of the topic attribute `name` to `value` answers because of the
/// value the topic ALREADY holds for it, or `None` when the change is one SNS performs.
///
/// A value's own domain is not the whole rule: a one-way attribute
/// ([`UNSUPPORTED_VALUE_CHANGES`]) takes a value the domain admits only from the values
/// it can still be reached from, so the write is judged against the attribute map the
/// topic holds now. A write that lands the value the attribute already holds changes
/// nothing and is not a transition at all.
pub fn topic_attribute_change_refusal(
    name: &str,
    value: &str,
    held: &HashMap<String, String>,
) -> Option<TopicAttributeRefusal> {
    let holds = held.get(name)?.as_str();
    UNSUPPORTED_VALUE_CHANGES
        .iter()
        .find(|(attribute, from, to)| *attribute == name && *from == holds && *to == value)
        .map(|(attribute, from, to)| {
            TopicAttributeRefusal::InvalidParameter(unsupported_change_refusal(attribute, from, to))
        })
}

/// The sentence a value outside the attribute's own documented domain is refused with,
/// or `None` when the attribute accepts it.
///
/// A sentence that NAMES the attribute states which input is at fault itself and stands as
/// SNS words it, whichever call carried the write; one SNS states as a sentence of its own
/// names none, so it is framed by the member the value arrived in
/// ([`AttributeMember::whole_sentence_refusal`], and see [`topic_attribute_refusal`]).
fn value_refusal(
    name: &str,
    value: &str,
    subject: TopicSubject<'_>,
    member: AttributeMember,
) -> Option<String> {
    match name {
        // SNS reports a `Policy` it cannot read as a policy document by naming the
        // attribute and the parse that failed, not the document the caller sent.
        POLICY_ATTRIBUTE => serde_json::from_str::<TopicPolicyDocument>(value)
            .err()
            .map(|_| format!("Invalid parameter: {name} Error: null")),
        DELIVERY_POLICY_ATTRIBUTE => {
            // A delivery policy is a JSON DOCUMENT whose members the effective policy
            // is computed from, so the value is refused unless THAT read can honour it:
            // a bare scalar names no member, and a member of the wrong type would be
            // stored only to be left out of the effective policy.
            delivery_policy_refusal(value)
                .map(|reason| format!("Invalid parameter: {name}: {reason}"))
                // …then the numbers IN that document: each member's own documented
                // bounds, which one catalog states for both documents a retry policy is
                // written in ([`super::retry_policy_bounds`])…
                .or_else(|| {
                    super::retry_policy_bounds::topic_delivery_policy_bounds_refusal(value)
                })
                // …and the rules those members hold EACH OTHER to, which no single
                // member's bound decides.
                .or_else(|| {
                    super::delivery_policy_member_agreement_reason(value)
                        .map(|reason| format!("Invalid parameter: {name}: {reason}"))
                })
        }
        ARCHIVE_POLICY_ATTRIBUTE => json_object(value).is_none().then(|| {
            format!(
                "Invalid parameter: AttributeValue Reason: {name} - the value must be \
                 an archive policy document: a JSON object"
            )
        }),
        MAXIMUM_MESSAGE_SIZE_ATTRIBUTE => maximum_message_size_refusal(name, value, subject),
        DISPLAY_NAME_ATTRIBUTE => (value.chars().count() > MAX_DISPLAY_NAME_CHARS)
            .then(|| format!("Invalid parameter: {name}")),
        TRACING_CONFIG_ATTRIBUTE => (!TRACING_CONFIG_VALUES.contains(&value)).then(|| {
            member.whole_sentence_refusal(&format!("Invalid tracing config value: {value}"))
        }),
        SIGNATURE_VERSION_ATTRIBUTE => (!SIGNATURE_VERSION_VALUES.contains(&value)).then(|| {
            format!("Invalid parameter: {name}: Invalid value '[{value}]'. Must be 1 or 2.")
        }),
        FIFO_THROUGHPUT_SCOPE_ATTRIBUTE => (!FIFO_THROUGHPUT_SCOPE_VALUES.contains(&value))
            .then(|| member.whole_sentence_refusal(&format!("Invalid {name}: {value}"))),
        // The two flag-valued attributes: a word, in either case, because SNS carries
        // every attribute value as a string. The sentence is the one every flag-valued
        // attribute of either record kind is refused with
        // ([`super::flag_attribute_refusal`]); a topic's write names the attribute
        // directly, whichever call carried it, so it takes no member frame.
        FIFO_TOPIC_ATTRIBUTE | CONTENT_BASED_DEDUPLICATION_ATTRIBUTE => {
            flag_attribute_value(value)
                .is_none()
                .then(|| flag_attribute_refusal(name, value, AttributeMember::Named))
        }
        name if name.ends_with(SAMPLE_RATE_SUFFIX) => match value.parse::<i64>() {
            Ok(rate) if (MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&rate) => None,
            _ => Some(format!(
                "Invalid parameter: {name}: {value} value provided is not an integer \
                 between {MIN_SAMPLE_RATE}-{MAX_SAMPLE_RATE}"
            )),
        },
        // `KmsMasterKeyId` is "the ID of an Amazon Web Services managed customer master
        // key (CMK) for Amazon SNS or a custom CMK" — a key id, an alias or an ARN, so
        // SNS documents no form for it beyond its being a value at all (an empty one
        // REMOVES the attribute, see [`super::clears_attribute`]).
        KMS_MASTER_KEY_ID_ATTRIBUTE => None,
        _ => None,
    }
}

/// The sentence an attribute SNS documents for ONE KIND of topic is refused with when
/// the topic is of the other kind, or `None` when the attribute applies to this topic.
///
/// An attribute the topic's kind gives no meaning to is not an attribute of THAT topic at
/// all, so SNS refuses it the way it refuses a name it defines for nothing: with the bare
/// refusal of the member the name arrived in. Recorded evidence: `FifoThroughputScope` on
/// a standard topic answers `400 InvalidParameter` with exactly
/// [`super::INVALID_ATTRIBUTE_NAME_REFUSAL`].
fn kind_refusal(name: &str, is_fifo: bool) -> Option<String> {
    let scoped = if is_fifo {
        STANDARD_ONLY_ATTRIBUTES
    } else {
        FIFO_ONLY_ATTRIBUTES
    };
    scoped
        .contains(&name)
        .then(|| super::INVALID_ATTRIBUTE_NAME_REFUSAL.to_string())
}

/// The message a `MaximumMessageSize` is refused with, or `None` when the topic may take
/// it: the documented range, and — above the default ceiling — the subscriptions the
/// topic has to hold for a message that large to be deliverable at all.
fn maximum_message_size_refusal(
    name: &str,
    value: &str,
    subject: TopicSubject<'_>,
) -> Option<String> {
    let size = match value.parse::<u64>() {
        Ok(size) if (MIN_MAXIMUM_MESSAGE_SIZE..=MAX_MAXIMUM_MESSAGE_SIZE).contains(&size) => size,
        _ => {
            return Some(format!(
                "Invalid parameter: {name}: {value} is not an integer between \
                 {MIN_MAXIMUM_MESSAGE_SIZE} and {MAX_MAXIMUM_MESSAGE_SIZE} bytes"
            ))
        }
    };
    if size <= DEFAULT_MAXIMUM_MESSAGE_SIZE as u64 {
        return None;
    }
    // "A topic with a MaximumMessageSize above 256 KiB must have 100 or fewer
    // subscriptions, and each subscription must be an Amazon SQS, Amazon Data Firehose,
    // or Lambda subscription … If the topic doesn't meet these requirements when you set
    // a value above 256 KiB, Amazon SNS returns an InvalidParameter error." The sentence
    // names the bound and the protocols of the topic that cannot carry a message that
    // large, which is what the owner has to unsubscribe.
    let held = subject.subscription_protocols.len();
    if held > MAX_LARGE_PAYLOAD_SUBSCRIPTIONS {
        return Some(format!(
            "Invalid parameter: {name} greater than {DEFAULT_MAXIMUM_MESSAGE_SIZE} bytes \
             is not supported for a topic with more than \
             {MAX_LARGE_PAYLOAD_SUBSCRIPTIONS} subscriptions"
        ));
    }
    let mut ineligible: Vec<&str> = subject
        .subscription_protocols
        .iter()
        .map(String::as_str)
        .filter(|protocol| !LARGE_PAYLOAD_PROTOCOLS.contains(protocol))
        .collect();
    ineligible.sort_unstable();
    ineligible.dedup();
    (!ineligible.is_empty()).then(|| {
        format!(
            "Invalid parameter: {name} greater than {DEFAULT_MAXIMUM_MESSAGE_SIZE} bytes \
             is not supported for the following protocol: [{}]",
            ineligible.join(", ")
        )
    })
}

/// The sentence SNS states for a value it cannot use as a delivery-status logging role: the
/// attribute the value arrived in, and the value itself.
///
/// ONE sentence for the whole condition, because the caller has one thing to fix — the role
/// the attribute names — whichever half of the rule the value broke: the ARN's own form, read
/// here, or the trust relationship the account's IAM holds for the role it names, read in
/// [`super::feedback_role`].
pub fn not_a_valid_logging_role(name: &str, value: &str) -> String {
    format!(
        "Invalid parameter: {name}: {value} is not a valid role to allow SNS to write to \
         Cloudwatch Logs"
    )
}

/// The refusal a delivery-status feedback role ARN answers, or `None` when the value is
/// a role this account's SNS could write a delivery's status record as.
///
/// "The <ENDPOINT>SuccessFeedbackRoleArn and <ENDPOINT>FailureFeedbackRoleArn attributes
/// are used to give Amazon SNS write access to use CloudWatch Logs on your behalf"
/// (`SetTopicAttributesInput$AttributeName` doc), so the value names an IAM ROLE and the
/// two ways it can fail are different faults:
///
/// * a value that is not a role ARN at all is a PARAMETER fault, and SNS names the
///   attribute and the value it could not use as a logging role;
/// * a role ARN of ANOTHER ACCOUNT is an AUTHORIZATION fault — the role is real and well
///   formed, and what fails is this account's SNS being able to assume it.
///
/// Those are the two faults the VALUE itself carries. The third — a role of this account whose
/// trust policy does not let this service assume it — is a fact about the role rather than
/// about the ARN, so it is resolved against IAM in [`super::feedback_role`] and stated with the
/// same parameter sentence.
fn feedback_role_refusal(name: &str, value: &str) -> Option<TopicAttributeRefusal> {
    let account = super::owner_account_id();
    // An IAM role's ARN is `arn:<partition>:iam::<account-id>:role/<name>`: IAM is a
    // GLOBAL service, so the region field of one of its ARNs is empty — which is why the
    // shape is read here rather than through the regional ARN reader.
    let fields: Vec<&str> = value.splitn(6, ':').collect();
    let named_account = match fields[..] {
        ["arn", partition, IAM_SERVICE, "", named_account, resource]
            if !partition.is_empty()
                && !named_account.is_empty()
                && resource
                    .strip_prefix(ROLE_RESOURCE_PREFIX)
                    .is_some_and(|role| !role.is_empty()) =>
        {
            named_account
        }
        _ => {
            return Some(TopicAttributeRefusal::InvalidParameter(
                not_a_valid_logging_role(name, value),
            ))
        }
    };
    (named_account != account).then(|| {
        // A role of another account is one SNS would have to be PASSED to act as, which
        // this account's SNS is not allowed to do — the sentence states that condition
        // and nothing of the request.
        TopicAttributeRefusal::Unauthorized("Cross-account pass role is not allowed.".to_string())
    })
}
