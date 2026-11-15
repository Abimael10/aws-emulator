// SPDX-License-Identifier: CC0-1.0

//! Whether the SNS topic an alarm action names really takes the notification: the decision
//! taken before anything is published to it.
//!
//! Executing an SNS alarm action is a CROSS-SERVICE request this service makes as its own
//! service principal ([`super::CLOUDWATCH_SERVICE_PRINCIPAL`]) on behalf of the alarm whose
//! transition fired it, so it is authorized the ordinary IAM way — may this principal perform
//! this action on this resource? — against the documents in force on the DESTINATION. Two
//! documents are in force, and a publish needs a grant from each:
//!
//! 1. the topic's own access policy, which has to allow [`PUBLISH_ACTION`] for this principal
//!    on that topic. The policy a topic whose owner has set none carries allows every
//!    principal of the topic's own account where `aws:SourceOwner` is that account, which this
//!    request answers for, so a topic nobody has written a policy for takes the publish; a
//!    least-privilege policy that states no such grant withholds it, exactly as the function
//!    action's own grant is read ([`super::lambda_actions`]).
//! 2. the KEY the topic encrypts its messages with, for a topic carrying
//!    [`KMS_MASTER_KEY_ID_ATTRIBUTE`]: Amazon SNS seals the notification under that key as it
//!    is published, so the key's own authorities are in force over the publish — which is why
//!    the permissions AWS documents on such a key are `kms:GenerateDataKey*` and `kms:Decrypt`
//!    for this principal ([`SEALING_OPERATIONS`]).
//!
//! A grant withheld by either document means the publish is NOT MADE: no subscriber of that
//! topic is notified, and the action is recorded in the alarm's history as the authorization
//! failure it was ([`super::action_history_item`]).
//!
//! Each question is answered by the service that OWNS the answer — Amazon SNS for the topic's
//! attributes, AWS KMS for the key's manager, its state and its key policy — reached the way
//! any emulated service reaches another, over the in-process callback. The key is asked of
//! KMS's own API rather than of its store because this crate cannot depend on the KMS service
//! crate at all: that edge is a cycle cargo refuses
//! (`cloudwatch -> kms -> sts -> cognitoidentityprovider -> cloudwatch`), which is also why the
//! wrappers that ask this same question from a crate that CAN depend on it — Amazon SNS's own
//! publish-time seal and an EventBridge delivery to an encrypted topic — are not reachable from
//! here. So what KMS states about the key is read, and the decision over it is the one KMS
//! takes for a SERVICE principal: the key's own policy is the only authority over the use,
//! because such a principal holds no identity-based policies, naming the account grants it
//! nothing, and a grant cannot name it either — "to specify the grantee principal, use the
//! Amazon Resource Name (ARN) of an Amazon Web Services principal"
//! (`CreateGrantRequest$GranteePrincipal`), which a service principal is not, so KMS refuses to
//! issue one for it at all. A key carrying only the policy `CreateKey` generates for the
//! account root therefore admits this publish nothing until a statement names the principal,
//! and an explicit deny in that policy refuses it whatever else the policy allows.
//!
//! An answer that is NOT one of those services stating the destination's own facts (a topic
//! that could not be read, no KMS to ask, an identifier naming no key, a key Amazon Web
//! Services manages, a key no cryptographic operation can use right now) establishes nothing,
//! so it withholds nothing: what such a destination does with the publish is then the publish's
//! own answer, and that is what the action is recorded with.
//!
//! The decision itself is [`::service_common::iam_authorization`], the tree's one evaluation of
//! policy documents against `(action, resource, principal)`, and the sentences a refusal
//! carries are that module's — the same ones KMS answers its own refusals with.

/// The IAM action publishing a notification to a topic performs on that topic, spelled the way
/// Amazon SNS names its own actions in a topic policy (an action name is matched
/// case-insensitively, so the spelling decides only the wording of a refusal).
pub const PUBLISH_ACTION: &str = "SNS:Publish";

/// The topic attribute carrying a topic's access policy, as `GetTopicAttributes` reports it.
const POLICY_ATTRIBUTE: &str = "Policy";

/// The topic attribute naming the customer managed key a topic encrypts its messages with, as
/// `GetTopicAttributes` reports it.
const KMS_MASTER_KEY_ID_ATTRIBUTE: &str = "KmsMasterKeyId";

/// The KMS operations sealing ONE message under a customer managed key performs: the data key
/// the message is encrypted under is generated, and a data key already in use is decrypted to
/// seal another message under it — which is why the permissions AWS documents on the key of a
/// topic that receives alarm notifications are `kms:GenerateDataKey*` and `kms:Decrypt`. A
/// refusal of either withholds the notification.
///
/// Each is spelled as the IAM action it is, since the key policy is what names them.
const SEALING_OPERATIONS: [&str; 2] = ["kms:GenerateDataKey", "kms:Decrypt"];

/// The condition key naming the resource a cross-service request came FROM — the alarm whose
/// transition fired the action — which is what a grant to a service principal is scoped by.
const SOURCE_ARN_CONDITION_KEY: &str = "aws:SourceArn";

/// The condition key naming the ACCOUNT that resource belongs to.
const SOURCE_ACCOUNT_CONDITION_KEY: &str = "aws:SourceAccount";

/// The same account under the key a topic's own generated policy uses for it.
const SOURCE_OWNER_CONDITION_KEY: &str = "aws:SourceOwner";

/// The condition key naming the service a KMS request was made THROUGH, whose value is that
/// service's regional endpoint name: the notification is sealed as it is published, so the key
/// is reached through Amazon SNS.
const VIA_SERVICE_CONDITION_KEY: &str = "kms:ViaService";

/// The ARN namespace of Amazon SNS, which is the name its regional endpoint is built from.
const SNS_ENDPOINT_VENDOR: &str = "sns";

/// The reading that refused a publish an explicit `Deny` in the destination's own policy
/// refuses: a fact about that document, so the sentence names it rather than the action.
const RESOURCE_POLICY_EXPLICIT_DENY: &str = "with an explicit deny in a resource-based policy";

/// The authorization failure the topic `topic_arn` refuses this alarm action with, or `None`
/// when every document in force on it permits the publish.
///
/// `region` is where the topic is addressed, which is also the endpoint its key is asked of and
/// reached through; `alarm_arn` is the alarm whose transition fires the action, the resource the
/// publish is made on behalf of. See the module docs for the rule.
pub async fn publish_refusal(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    client: &::aws_sdk_sns::Client,
    region: &str,
    topic_arn: &str,
    alarm_arn: Option<&str>,
) -> Option<String> {
    // A topic this call could not read states nothing that could withhold the publish; the
    // publish itself then answers for it, and a topic that does not exist refuses it with its
    // own fault, which is the error the action is recorded with.
    let attributes = topic_attributes(client, topic_arn).await?;
    let context = source_context(alarm_arn);
    // The keys this request ANSWERS for are the ones it carries a value for: a statement
    // conditioned on one of them is decided, while one conditioned on a key the request can say
    // nothing about stays undecided and withholds nothing (see [`stated_by`]).
    let published: Vec<&str> = context.iter().map(|(key, _)| key.as_str()).collect();
    // A `Policy` attribute that is empty, or that is no policy document at all, is this
    // evaluation holding no document rather than the topic stating no grant, so it withholds
    // nothing — exactly as a topic reporting no `Policy` at all does.
    let policy = attributes
        .get(POLICY_ATTRIBUTE)
        .filter(|policy| !policy.trim().is_empty());
    if let Some(policy) = policy {
        let principals = [super::CLOUDWATCH_SERVICE_PRINCIPAL.to_string()];
        let request = ::service_common::iam_authorization::Request::new(
            PUBLISH_ACTION,
            topic_arn,
            &principals,
        )
        .with_context(&context)
        .with_published_keys(&published);
        if let Some(refusal) = stated_by(policy, &request, PUBLISH_ACTION, topic_arn).refusal() {
            return Some(refusal);
        }
    }
    let master_key_id = attributes
        .get(KMS_MASTER_KEY_ID_ATTRIBUTE)
        .map(|value| value.trim())
        // An empty `KmsMasterKeyId` is how a caller takes a topic OFF its key, so it is no key
        // identifier and nothing is asked of KMS for it.
        .filter(|value| !value.is_empty())?;
    key_refusal(callback, region, master_key_id, &context).await
}

/// The refusal the topic's own ENCRYPTION KEY withholds this publish with, read out of KMS's
/// own account of that key.
///
/// The use is one Amazon SNS makes on this service's behalf rather than one asked of KMS
/// directly, so it is judged as arriving THROUGH that service — the value of
/// [`VIA_SERVICE_CONDITION_KEY`] a key policy scoped by it is written against.
///
/// `None` wherever nothing of the kind can be shown, so a notification is never withheld on a
/// guess: see the module docs.
async fn key_refusal(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    master_key_id: &str,
    context: &[(String, String)],
) -> Option<String> {
    let client = crate::state::kms_in_process::client(callback, region);
    let KeyCondition::Judged { key_arn, policy } = key_condition(&client, master_key_id).await
    else {
        return None;
    };
    let principals = [super::CLOUDWATCH_SERVICE_PRINCIPAL.to_string()];
    let mut context: Vec<(String, String)> = context.to_vec();
    context.push((
        VIA_SERVICE_CONDITION_KEY.to_string(),
        format!("{SNS_ENDPOINT_VENDOR}.{region}.amazonaws.com"),
    ));
    let published: Vec<&str> = context.iter().map(|(key, _)| key.as_str()).collect();
    SEALING_OPERATIONS.iter().find_map(|action| {
        let request =
            ::service_common::iam_authorization::Request::new(action, &key_arn, &principals)
                .with_context(&context)
                .with_published_keys(&published);
        stated_by(&policy, &request, action, &key_arn).refusal()
    })
}

/// The condition keys the publish this alarm's transition fires carries: the alarm itself and
/// the account it belongs to, which are the keys a grant to a service principal is scoped by —
/// the account under both spellings a topic policy uses for it.
fn source_context(alarm_arn: Option<&str>) -> Vec<(String, String)> {
    let mut context: Vec<(String, String)> = Vec::new();
    let Some(alarm_arn) = alarm_arn else {
        return context;
    };
    context.push((SOURCE_ARN_CONDITION_KEY.to_string(), alarm_arn.to_string()));
    if let Some(account) = ::service_common::arn::parse(alarm_arn).map(|arn| arn.account) {
        context.push((
            SOURCE_ACCOUNT_CONDITION_KEY.to_string(),
            account.to_string(),
        ));
        context.push((SOURCE_OWNER_CONDITION_KEY.to_string(), account.to_string()));
    }
    context
}

/// What one resource-based policy document states about this access.
enum Stated {
    /// A statement allows it.
    Allowed,
    /// A statement explicitly denies it, worded as the refusal AWS answers.
    ExplicitDeny(String),
    /// No statement allows it, and the document could be decided: IAM's default deny, worded as
    /// the refusal AWS answers.
    NoAllow(String),
    /// The only statements about the access hinge on a condition key this request carries no
    /// value for, so this evaluation decides nothing — which is this evaluation lacking a value
    /// rather than the destination refusing the call.
    Undecided,
}

impl Stated {
    /// The refusal this reading withholds the access with, or `None` where it withholds
    /// nothing.
    fn refusal(self) -> Option<String> {
        match self {
            Self::Allowed | Self::Undecided => None,
            Self::ExplicitDeny(refusal) | Self::NoAllow(refusal) => Some(refusal),
        }
    }
}

/// What `policy` states about `request`, in the sentences AWS words an authorization failure
/// in: who was refused, what action was asked for, which resource it was asked for, and what
/// refused it.
///
/// A document silent about this principal states no grant for it, because the grant for a
/// cross-service publish is one a destination's owner writes.
fn stated_by(
    policy: &str,
    request: &::service_common::iam_authorization::Request<'_>,
    action: &str,
    resource: &str,
) -> Stated {
    let outcome = ::service_common::iam_authorization::resource_outcome_prepared(
        &::service_common::iam_authorization::PreparedDocument::read(policy),
        request,
    );
    match outcome.decision {
        ::service_common::iam_authorization::Decision::Allowed => Stated::Allowed,
        ::service_common::iam_authorization::Decision::Denied => {
            Stated::ExplicitDeny(format!(
                "{} {RESOURCE_POLICY_EXPLICIT_DENY}",
                ::service_common::iam_authorization::not_authorized_to_perform(
                    super::CLOUDWATCH_SERVICE_PRINCIPAL,
                    action,
                    resource,
                )
            ))
        }
        ::service_common::iam_authorization::Decision::NotStated => {
            if outcome.required_context_keys.is_empty() {
                Stated::NoAllow(
                    ::service_common::iam_authorization::not_authorized_by_resource_policy(
                        super::CLOUDWATCH_SERVICE_PRINCIPAL,
                        action,
                        resource,
                    ),
                )
            } else {
                Stated::Undecided
            }
        }
    }
}

/// The attributes the topic `topic_arn` reports, or `None` when this call could not read them
/// — where there is nothing to decide and the publish itself answers.
async fn topic_attributes(
    client: &::aws_sdk_sns::Client,
    topic_arn: &str,
) -> Option<::std::collections::HashMap<String, String>> {
    let answered = client
        .get_topic_attributes()
        .topic_arn(topic_arn)
        .send()
        .await;
    match answered {
        Ok(answer) => Some(answer.attributes.unwrap_or_default()),
        Err(error) => {
            // The topic's own ARN is rendered in its escaped `Debug` form, so an action naming
            // an identifier that carries a newline cannot forge a log line.
            ::tracing::warn!(
                topic = ?topic_arn,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "this alarm action's topic could not be read, so the documents in force on it \
                 are not applied here and the publish answers for it"
            );
            None
        }
    }
}

/// What KMS states about the key a topic seals its messages under.
enum KeyCondition {
    /// A key whose own key policy decides the seal: the key's ARN (a key policy's `Resource`
    /// element is `*`, which means the key the policy is attached to) and the policy itself.
    Judged { key_arn: String, policy: String },
    /// Nothing KMS stated decides the seal: see the module docs.
    Unjudged,
}

/// What KMS holds for the key `master_key_id` names, asked of this emulator's own KMS.
///
/// Two reads, because the decision needs both: WHO manages the key and what state it is in
/// (`DescribeKey`, which is also what resolves an Amazon Web Services managed key from a
/// predefined alias) and the key policy itself (`GetKeyPolicy`).
async fn key_condition(client: &::aws_sdk_kms::Client, master_key_id: &str) -> KeyCondition {
    let described = match client.describe_key().key_id(master_key_id).send().await {
        Ok(described) => described,
        Err(failure) => return unjudged_key(master_key_id, &failure),
    };
    let Some(metadata) = described.key_metadata() else {
        return KeyCondition::Unjudged;
    };
    // "The manager of the KMS key. KMS keys in your Amazon Web Services account are either
    // customer managed or Amazon Web Services managed" (`KeyMetadata$KeyManager`): the policy of
    // a key Amazon Web Services manages is written by Amazon Web Services for the service that
    // uses the key, so no statement of this account's is asked for. A key that is not ENABLED
    // can take no cryptographic operation at all right now, which is a different failure from a
    // permission the key withholds, and the publish reports that one itself.
    if metadata.key_manager() != Some(&::aws_sdk_kms::types::KeyManagerType::Customer)
        || metadata.key_state() != Some(&::aws_sdk_kms::types::KeyState::Enabled)
    {
        return KeyCondition::Unjudged;
    }
    let Some(key_arn) = metadata.arn().map(str::to_string) else {
        return KeyCondition::Unjudged;
    };
    match client.get_key_policy().key_id(master_key_id).send().await {
        Ok(answer) => match answer.policy {
            Some(policy) => KeyCondition::Judged { key_arn, policy },
            None => KeyCondition::Unjudged,
        },
        Err(failure) => unjudged_key(master_key_id, &failure),
    }
}

/// Note that KMS did not state the key's facts, and so that the key decides nothing here
/// ([`crate::state::kms_in_process::unstated_key`], the one note both readers of a key's facts
/// leave behind).
fn unjudged_key<E: ::std::error::Error + 'static, R: ::std::fmt::Debug>(
    master_key_id: &str,
    failure: &::aws_sdk_kms::error::SdkError<E, R>,
) -> KeyCondition {
    crate::state::kms_in_process::unstated_key(master_key_id, failure, UNJUDGED_KEY_NOTE);
    KeyCondition::Unjudged
}

/// What an operator is told when KMS stated nothing about a topic's key: this half of the
/// decision did not run, so the publish itself answers for the key.
const UNJUDGED_KEY_NOTE: &str =
    "this alarm action's topic names an encryption key KMS did not state its facts for, so the \
     key decides nothing here and the publish answers for it";
