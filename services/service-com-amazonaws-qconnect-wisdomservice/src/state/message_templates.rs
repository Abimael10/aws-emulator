// SPDX-License-Identifier: CC0-1.0

//! The message-template family of this service's state: the record a message template is
//! persisted as, the projections every operation that reports one answers out of, and the
//! lookups that reach it inside its knowledge base.
//!
//! A message template lives INSIDE the knowledge base it was created under — every
//! modelled route is `/knowledgeBases/{knowledgeBaseId}/messageTemplates/…` — so the
//! record names its parent and a lookup is always scoped to the parent the route named.
//! Nothing here is computed twice: a create writes one record and every read of the
//! template renders that record, so a create's answer and a later `GetMessageTemplate`
//! cannot disagree.

use std::collections::HashMap;
use std::time::SystemTime;

use super::*;
use crate::ops::types::{
    ExtendedMessageTemplateData, GroupingConfiguration, MessageTemplateAttributes,
    MessageTemplateContentProvider, MessageTemplateData, MessageTemplateSourceConfiguration,
    MessageTemplateSourceConfigurationSummary,
    WhatsAppMessageTemplateSourceConfigurationSummary,
};

/// Every value the modelled `MessageTemplateAttributeType` enumeration declares
/// (`smithy_query(query="shape",shape="MessageTemplateAttributeType")`), in the order the
/// model declares them — which is the order [`MessageTemplate::attribute_types`] reports
/// them in, so the list a read answers with does not depend on map or field order.
pub const MESSAGE_TEMPLATE_ATTRIBUTE_TYPE_SYSTEM: &str = "SYSTEM";
pub const MESSAGE_TEMPLATE_ATTRIBUTE_TYPE_AGENT: &str = "AGENT";
pub const MESSAGE_TEMPLATE_ATTRIBUTE_TYPE_CUSTOMER_PROFILE: &str = "CUSTOMER_PROFILE";
pub const MESSAGE_TEMPLATE_ATTRIBUTE_TYPE_CUSTOM: &str = "CUSTOM";

/// The message a read or a write of a message template this knowledge base does not hold
/// is refused with, worded as this endpoint words the other not-found resources of this
/// family ([`MESSAGE_KNOWLEDGE_BASE_NOT_FOUND`], [`MESSAGE_QUICK_RESPONSE_NOT_FOUND`]),
/// with the resource token spelled as the API itself names the resource.
pub const MESSAGE_MESSAGE_TEMPLATE_NOT_FOUND: &str = "MessageTemplate does not exist.";

/// The most message templates one knowledge base may hold at a time; a create beyond it
/// is refused with the modelled `ServiceQuotaExceededException` (HTTP 402), which
/// `CreateMessageTemplate` declares.
///
/// CHOSEN: AWS quotas message templates per knowledge base but does not publish the
/// figure. The bound is also what keeps this store finite — a record is released only by
/// a delete, or by the delete of the knowledge base that holds it.
pub const MAX_MESSAGE_TEMPLATES_PER_KNOWLEDGE_BASE: usize = 100;

/// The sentence a create past [`MAX_MESSAGE_TEMPLATES_PER_KNOWLEDGE_BASE`] is refused
/// with, stating the RULE — the ceiling and what to do about it — and carrying nothing
/// from the request or from what the knowledge base already holds.
pub fn message_message_template_quota() -> String {
    format!(
        "The maximum number of message templates ({MAX_MESSAGE_TEMPLATES_PER_KNOWLEDGE_BASE}) \
         in this knowledge base has been reached. Delete a message template before creating \
         another."
    )
}

/// The sentence a create whose `content` does not apply to the `channelSubtype` it names
/// is refused with.
///
/// The `MessageTemplateContentProvider` union declares one member per channel subtype and
/// documents each as the content "that applies to the <X> channel subtype"
/// (`smithy_query(query="shape",shape="MessageTemplateContentProvider")`), so content of
/// one subtype is not content of another and a template cannot be created from it. The
/// variable parts are the subtype the request named and the member it carried, both from
/// the request.
pub fn message_content_channel_subtype_mismatch(channel_subtype: &str, member: &str) -> String {
    format!(
        "The content member {member} does not apply to the {channel_subtype} channel subtype. \
         Provide the content member for that channel subtype and try again."
    )
}

/// The sentence a create that carries a `sourceConfiguration` for a channel subtype that
/// has none is refused with.
///
/// The member's own documentation states the rule — "Only set this argument for WHATSAPP
/// channel subtype"
/// (`smithy_query(query="doc",shape="CreateMessageTemplateRequest",member="sourceConfiguration")`)
/// — so the one variable part is the subtype the request named.
pub fn message_source_configuration_not_applicable(channel_subtype: &str) -> String {
    format!(
        "sourceConfiguration can only be set for the {CHANNEL_SUBTYPE_WHATSAPP} channel \
         subtype, not for {channel_subtype}."
    )
}

/// The `ChannelSubtype` whose templates are sourced from a WhatsApp Business account
/// rather than written in this API
/// (`smithy_query(query="shape",shape="ChannelSubtype")` declares `EMAIL`, `SMS`,
/// `WHATSAPP` and `PUSH`): the one subtype whose create takes a `sourceConfiguration`.
pub const CHANNEL_SUBTYPE_WHATSAPP: &str = "WHATSAPP";

/// The `ChannelSubtype` of a template sent as an email, the one subtype of Amazon Connect's
/// EMAIL channel ([`channel_of_subtype`]).
pub const CHANNEL_SUBTYPE_EMAIL: &str = "EMAIL";

/// The two Amazon Connect contact channels this model's channel subtypes divide into, as
/// [`channel_of_subtype`] states the division.
pub const CHANNEL_EMAIL: &str = "EMAIL";
pub const CHANNEL_CHAT: &str = "CHAT";

/// The language code a message template takes when the create names none.
///
/// `language` is optional on the create while every read reports the template's EFFECTIVE
/// language — "the language code value for the language in which the … is written", one of
/// the documented codes `de_DE`, `en_US`, `es_ES`, `fr_FR`, `id_ID`, `it_IT`, `ja_JP`,
/// `ko_KR`, `pt_BR`, `zh_CN`, `zh_TW`
/// (`smithy_query(query="doc",shape="CreateMessageTemplateRequest",member="language")`) — so
/// a template created without one is written in the endpoint's default language, U.S.
/// English, and reports that. A create that names a language keeps the one it named.
pub const DEFAULT_MESSAGE_TEMPLATE_LANGUAGE: &str = "en_US";

/// The managed version qualifier that names the version of a message template that is
/// ACTIVATED, the other half of the pair `GetMessageTemplate` documents: the label's
/// qualifier is "either an actual version number or an Amazon Q Connect managed qualifier
/// $ACTIVE_VERSION | $LATEST"
/// (`smithy_query(query="doc",shape="GetMessageTemplate")`), the second of which is
/// [`VERSION_QUALIFIER_LATEST`].
pub const VERSION_QUALIFIER_ACTIVE: &str = "$ACTIVE_VERSION";

/// What a version qualifier on a message-template label addresses — see
/// [`addressed_version`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressedVersion {
    /// The unversioned WORKING COPY, which [`VERSION_QUALIFIER_LATEST`] names and a label
    /// carrying no qualifier addresses implicitly. Not a version: it has no
    /// `versionNumber`, and nothing activates it.
    Latest,
    /// One PUBLISHED version: a version number, or the activated one
    /// [`VERSION_QUALIFIER_ACTIVE`] names.
    Published,
}

/// Which of the two things a message-template label's version qualifier addresses, or
/// `None` when the qualifier is neither — a qualifier the model's `@pattern` admits
/// (`[A-Z0-9_$]+`) but the operation's own documentation does not name.
pub fn addressed_version(qualifier: &str) -> Option<AddressedVersion> {
    if qualifier == VERSION_QUALIFIER_LATEST {
        return Some(AddressedVersion::Latest);
    }
    if qualifier == VERSION_QUALIFIER_ACTIVE {
        return Some(AddressedVersion::Published);
    }
    // The modelled `Version` shape is a long with `range min 1`
    // (`smithy_query(query="shape",shape="ExtendedMessageTemplateData")`, member
    // `versionNumber`), so a numbered qualifier is a positive integer and nothing else.
    qualifier
        .parse::<i64>()
        .ok()
        .filter(|version| *version >= 1)
        .map(|_| AddressedVersion::Published)
}

/// The sentence a read whose label carries a version qualifier that names neither the
/// working copy nor a version is refused with, stating the RULE the operation's own
/// documentation gives: the qualifier is a version number or one of the two managed
/// qualifiers. Its one variable part is the qualifier the request used.
pub fn message_invalid_version_qualifier(qualifier: &str) -> String {
    format!(
        "The qualifier {qualifier} is not valid. Use a version number, \
         {VERSION_QUALIFIER_ACTIVE} or {VERSION_QUALIFIER_LATEST}."
    )
}

/// This service's ARN for a message template, which names its parent knowledge base too:
/// `arn:aws:wisdom:<region>:<account>:message-template/<knowledgeBaseId>/<messageTemplateId>`.
///
/// The same nested two-id-segment form [`quick_response_arn`] uses, and the one the
/// modelled `ArnWithQualifier` pattern admits for a resource inside another, with the
/// template's own id LAST so [`resolve_id`] reads it back out. A read addressed through a
/// version qualifier reports this ARN with that qualifier appended
/// ([`version_qualified_arn`]).
pub fn message_template_arn(
    region: &str,
    knowledge_base_id: &str,
    message_template_id: &str,
) -> String {
    ::engine::emulator_arn(
        ARN_NAMESPACE,
        region,
        &format!("message-template/{knowledge_base_id}/{message_template_id}"),
    )
}

/// The member of the `MessageTemplateContentProvider` union a piece of content was given
/// under, as a caller names it — the only thing
/// [`message_content_channel_subtype_mismatch`] reports back about the content.
pub fn content_member_name(content: &MessageTemplateContentProvider) -> &'static str {
    match content {
        MessageTemplateContentProvider::Email(_) => "email",
        MessageTemplateContentProvider::Sms(_) => "sms",
        MessageTemplateContentProvider::whatsApp(_) => "whatsApp",
        MessageTemplateContentProvider::Push(_) => "push",
    }
}

/// The `ChannelSubtype` the content under one member of the
/// `MessageTemplateContentProvider` union applies to, as the union's own member
/// documentation states it ("The content of the message template that applies to the
/// email channel subtype", and so on for `sms`, `whatsApp` and `push`).
pub fn content_channel_subtype(content: &MessageTemplateContentProvider) -> &'static str {
    match content {
        MessageTemplateContentProvider::Email(_) => "EMAIL",
        MessageTemplateContentProvider::Sms(_) => "SMS",
        MessageTemplateContentProvider::whatsApp(_) => CHANNEL_SUBTYPE_WHATSAPP,
        MessageTemplateContentProvider::Push(_) => "PUSH",
    }
}

/// The Amazon Connect `channel` a template of one `ChannelSubtype` applies to.
///
/// A channel subtype is a subtype OF a contact channel: `MessageTemplateData.channel` is
/// "the channel of the message template" and `channelSubtype` "the channel subtype this
/// message template applies to" (`smithy_query(query="shape",shape="MessageTemplateData")`),
/// so the channel a template reports is the channel its subtype belongs to rather than a
/// second thing a request names — no message-template request carries a `channel` member at
/// all.
///
/// Amazon Connect contacts reach a customer over one of its contact channels, and the
/// message-template subtypes (`EMAIL`, `SMS`, `WHATSAPP`, `PUSH`,
/// `smithy_query(query="shape",shape="ChannelSubtype")`) divide into two of them: an email
/// template is sent on the EMAIL channel, while the messaging subtypes — SMS, WhatsApp
/// Business and mobile push — are subtypes of Connect's CHAT channel, the channel a quick
/// response's own `channels` member documents for the same endpoint ("The Connect Customer
/// contact channels this quick response applies to. The supported contact channel types
/// include Chat", `smithy_query(query="shape",shape="QuickResponseSummary")`). Both tokens
/// are inside the modelled `Channel` bound (`@length 1..10`).
///
/// THE ONE PLACE this service decides that, so a read of the template, of one of its
/// versions and of a listing cannot disagree about the channel it applies to.
pub fn channel_of_subtype(channel_subtype: &str) -> String {
    match channel_subtype {
        CHANNEL_SUBTYPE_EMAIL => CHANNEL_EMAIL,
        _ => CHANNEL_CHAT,
    }
    .to_string()
}

/// One persisted message template: the reply an agent sends on one channel subtype, held
/// INSIDE the knowledge base it was created under.
///
/// The record is the single copy of the resource: `CreateMessageTemplate` writes it and
/// every read of it answers out of these fields. `channelSubtype` is stored because the
/// operation's own documentation makes it immutable ("The channel subtype of the message
/// template is immutable and cannot be modified after creation"), so nothing may write it
/// again.
#[derive(Debug, Clone)]
pub struct MessageTemplate {
    pub message_template_id: String,
    /// The template's own ARN, carrying NO version qualifier: the working copy is the
    /// resource, and a read addressed through a qualifier appends it
    /// ([`version_qualified_arn`]).
    pub message_template_arn: String,
    /// The RESOLVED `knowledgeBaseId` of the parent, never the label form the request
    /// used, so a template resolves the same way under either label form.
    pub knowledge_base_id: String,
    pub knowledge_base_arn: String,
    pub name: String,
    /// `ChannelSubtype` on the wire, fixed at create time.
    pub channel_subtype: String,
    pub content: Option<MessageTemplateContentProvider>,
    pub description: Option<String>,
    pub language: Option<String>,
    /// The source configuration the create carried, which only a `WHATSAPP` template has.
    /// Reported as the modelled SUMMARY ([`Self::source_configuration_summary`]), which is
    /// the member `MessageTemplateData` declares.
    pub source_configuration: Option<MessageTemplateSourceConfiguration>,
    pub default_attributes: Option<MessageTemplateAttributes>,
    pub grouping_configuration: Option<GroupingConfiguration>,
    pub tags: HashMap<String, String>,
    pub created_at: SystemTime,
    /// When this record last changed. Equal to [`Self::created_at`] until an update writes
    /// it, so `lastModifiedTime` is never earlier than `createdTime`.
    pub last_modified_at: SystemTime,
    /// The ARN of the identity that last wrote this record ([`caller_principal`]), stamped
    /// by the create and re-stamped by every update.
    pub last_modified_by: String,
    /// The attachments held on the working copy of this template, in the order they were
    /// uploaded. Empty until an attachment is uploaded to it, and part of the content
    /// checksum ([`Self::content_sha256`]) as the model says it is.
    ///
    /// Each one is the stored FILE and its metadata ([`MessageTemplateAttachmentFile`]),
    /// projected onto the modelled shape by [`Self::attachments_out`], so the URL a read
    /// reports addresses bytes this store actually holds.
    pub attachments: Vec<MessageTemplateAttachmentFile>,
}

impl MessageTemplate {
    /// The `messageTemplateContentSha256` every projection of this record reports: the
    /// checksum of the template content referenced by the `$LATEST` qualifier.
    ///
    /// The model states exactly what it is calculated over — "content, language,
    /// `defaultAttributes` and `Attachments` of the message template"
    /// (`smithy_query(query="shape",shape="MessageTemplateData")`, member
    /// `messageTemplateContentSha256`) — so those four parts, and nothing else, go into
    /// the digest: two templates with the same content have the same checksum, and a write
    /// that changes any of the four changes it.
    ///
    /// Each part is length-prefixed ([`request_fingerprint`]) so no value can run into its
    /// neighbour, and the two attribute maps are rendered in KEY order
    /// ([`tags_fingerprint`]) so the checksum does not depend on hash iteration order. The
    /// result is 64 lowercase hex digits, which is what the modelled
    /// `MessageTemplateContentSha256` admits (`len 1..64`, `^[a-zA-Z0-9]+$`).
    pub fn content_sha256(&self) -> String {
        use ::sha2::Digest;

        let attachments = request_fingerprint(
            &self
                .attachments
                .iter()
                .map(|attachment| {
                    request_fingerprint(&[
                        attachment.attachment_id.as_str(),
                        attachment.name.as_str(),
                        attachment.content_disposition.as_str(),
                    ])
                })
                .collect::<Vec<String>>(),
        );
        let parts = request_fingerprint(&[
            content_fingerprint(self.content.as_ref()),
            self.language.clone().unwrap_or_default(),
            attributes_fingerprint(self.default_attributes.as_ref()),
            attachments,
        ]);
        ::hex::encode(::sha2::Sha256::digest(parts.as_bytes()))
    }

    /// The `attributeTypes` this template reports: the categories of attribute the
    /// template holds, which the member documents as "the types of attributes that the
    /// message template contains".
    ///
    /// A category counts as held when the template's `defaultAttributes` carries it, since
    /// that object is what "defines a variable or placeholder in the message template"
    /// (`smithy_query(query="doc",shape="MessageTemplateData",member="defaultAttributes")`)
    /// — so the list is derived from the stored record rather than kept beside it.
    ///
    /// The list is ALWAYS reported, as the EMPTY list for a template that holds no attribute
    /// of any category: the member is a modelled list whose entries this service computes, so
    /// a client that iterates it to discover which categories a render must supply can tell
    /// "none" from "not returned" — which it cannot when the member is absent.
    ///
    /// The three STRUCTURED categories name their attributes as members, so the category is
    /// held as soon as the structure is there. `customAttributes` is a MAP whose KEYS are
    /// the attributes — "Each key defines a variable or placeholder in the message template"
    /// — so an empty map defines none and the category is held only when it carries an
    /// entry. One rule, read off how the model carries each category.
    pub fn attribute_types(&self) -> Vec<String> {
        let mut types = Vec::new();
        let Some(attributes) = self.default_attributes.as_ref() else {
            return types;
        };
        if attributes.system_attributes.is_some() {
            types.push(MESSAGE_TEMPLATE_ATTRIBUTE_TYPE_SYSTEM.to_string());
        }
        if attributes.agent_attributes.is_some() {
            types.push(MESSAGE_TEMPLATE_ATTRIBUTE_TYPE_AGENT.to_string());
        }
        if attributes.customer_profile_attributes.is_some() {
            types.push(MESSAGE_TEMPLATE_ATTRIBUTE_TYPE_CUSTOMER_PROFILE.to_string());
        }
        if attributes
            .custom_attributes
            .as_ref()
            .is_some_and(|custom| !custom.is_empty())
        {
            types.push(MESSAGE_TEMPLATE_ATTRIBUTE_TYPE_CUSTOM.to_string());
        }
        types
    }

    /// The stored source configuration as the modelled
    /// `MessageTemplateSourceConfigurationSummary` the data shapes declare: the values the
    /// create gave, and nothing this emulator does not hold.
    ///
    /// The summary's `status` and `statusReason` describe the review a WhatsApp Business
    /// account gives the template OUTSIDE this service, which this emulator holds no
    /// record of, so neither is claimed; `name` and `language` are the WhatsApp template's
    /// own, which the create does not carry either.
    pub fn source_configuration_summary(&self) -> Option<MessageTemplateSourceConfigurationSummary> {
        match self.source_configuration.as_ref()? {
            MessageTemplateSourceConfiguration::whatsApp(configuration) => Some(
                MessageTemplateSourceConfigurationSummary::whatsApp(
                    WhatsAppMessageTemplateSourceConfigurationSummary {
                        business_account_id: configuration.business_account_id.clone(),
                        template_id: configuration.template_id.clone(),
                        name: None,
                        language: None,
                        components: configuration.components.clone(),
                        status: None,
                        status_reason: None,
                    },
                ),
            ),
        }
    }

    /// This message template as the `MessageTemplateData` shape `CreateMessageTemplate`
    /// answers with.
    ///
    /// The ARN is the bare one: a create addressed no version, so it reports no qualifier
    /// (see [`version_qualified_arn`]).
    pub fn to_message_template_data(&self) -> MessageTemplateData {
        MessageTemplateData {
            message_template_arn: self.message_template_arn.clone(),
            message_template_id: self.message_template_id.clone(),
            knowledge_base_arn: self.knowledge_base_arn.clone(),
            knowledge_base_id: self.knowledge_base_id.clone(),
            name: self.name.clone(),
            // The channel this template applies to, which is the one its immutable channel
            // subtype names ([`channel_of_subtype`]).
            channel: Some(channel_of_subtype(&self.channel_subtype)),
            channel_subtype: self.channel_subtype.clone(),
            created_time: ::aws_smithy_types::DateTime::from(self.created_at),
            last_modified_time: ::aws_smithy_types::DateTime::from(self.last_modified_at),
            last_modified_by: self.last_modified_by.clone(),
            content: self.content.clone(),
            description: self.description.clone(),
            language: self.language.clone(),
            source_configuration_summary: self.source_configuration_summary(),
            grouping_configuration: self.grouping_configuration.clone(),
            default_attributes: self.default_attributes.clone(),
            attribute_types: Some(self.attribute_types()),
            message_template_content_sha256: self.content_sha256(),
            tags: tags_out(&self.tags),
        }
    }

    /// This message template as the `ExtendedMessageTemplateData` shape
    /// `GetMessageTemplate` answers with: the same stored fields
    /// [`Self::to_message_template_data`] reports, plus the attachments the template holds
    /// and the version the request addressed.
    ///
    /// `qualifier` is the version qualifier the request's label carried. The ARN carries it
    /// back ([`version_qualified_arn`]), and `isActive`/`versionNumber` are claimed only
    /// for a NUMBERED version: the working copy [`VERSION_QUALIFIER_LATEST`] names is not a
    /// version, so neither member applies to it.
    pub fn to_extended_message_template_data(
        &self,
        qualifier: &str,
        version_number: Option<i64>,
        is_active: Option<bool>,
    ) -> ExtendedMessageTemplateData {
        ExtendedMessageTemplateData {
            message_template_arn: version_qualified_arn(&self.message_template_arn, qualifier),
            message_template_id: self.message_template_id.clone(),
            knowledge_base_arn: self.knowledge_base_arn.clone(),
            knowledge_base_id: self.knowledge_base_id.clone(),
            name: self.name.clone(),
            // See [`Self::to_message_template_data`].
            channel: Some(channel_of_subtype(&self.channel_subtype)),
            channel_subtype: self.channel_subtype.clone(),
            created_time: ::aws_smithy_types::DateTime::from(self.created_at),
            last_modified_time: ::aws_smithy_types::DateTime::from(self.last_modified_at),
            last_modified_by: self.last_modified_by.clone(),
            content: self.content.clone(),
            description: self.description.clone(),
            language: self.language.clone(),
            source_configuration_summary: self.source_configuration_summary(),
            grouping_configuration: self.grouping_configuration.clone(),
            default_attributes: self.default_attributes.clone(),
            attribute_types: Some(self.attribute_types()),
            attachments: self.attachments_out(),
            is_active,
            version_number,
            message_template_content_sha256: self.content_sha256(),
            tags: tags_out(&self.tags),
        }
    }
}

/// The content union as ONE part of a [`request_fingerprint`].
///
/// No member the `MessageTemplateContentProvider` union reaches is a map — the shapes it
/// nests carry strings, structures, unions and the email header LIST, whose order is the
/// caller's and is part of the content — so the shape's own `Debug` rendering is a
/// deterministic, field-ordered rendering of it. THE ONE PLACE that decision is made, so
/// the content checksum and a create's idempotency fingerprint render the content alike.
pub fn content_fingerprint(content: Option<&MessageTemplateContentProvider>) -> String {
    content.map(|content| format!("{content:?}")).unwrap_or_default()
}

/// The source configuration as ONE part of a [`request_fingerprint`]: the members the
/// stored configuration carries, named rather than rendered, so the fingerprint does not
/// depend on how any shape derives `Debug`.
pub fn source_configuration_fingerprint(
    configuration: Option<&MessageTemplateSourceConfiguration>,
) -> String {
    let Some(MessageTemplateSourceConfiguration::whatsApp(configuration)) = configuration else {
        return String::new();
    };
    let mut parts = vec![
        configuration.business_account_id.clone(),
        configuration.template_id.clone(),
    ];
    parts.extend(configuration.components.clone().unwrap_or_default());
    request_fingerprint(&parts)
}

/// A `MessageTemplateAttributes` object as ONE part of a [`request_fingerprint`], rendered
/// so the same attributes always render the same way: the two maps it reaches
/// (`customAttributes`, and the `custom` map of `customerProfileAttributes`) go in in KEY
/// order, and the rest of the structure through its `Debug` rendering, which is a fixed
/// field order.
///
/// THE ONE PLACE this service renders that shape for a digest, so the content checksum and
/// a create's idempotency fingerprint cannot disagree about it. A `HashMap`'s iteration — and
/// therefore its `Debug` — order is not stable across two map instances holding the same
/// pairs, so rendering this shape through `Debug` alone would make a replayed identical
/// create fingerprint differently and be refused as an idempotency conflict.
pub fn attributes_fingerprint(attributes: Option<&MessageTemplateAttributes>) -> String {
    let Some(attributes) = attributes else {
        return String::new();
    };
    let mut customer_profile = attributes.customer_profile_attributes.clone();
    let customer_profile_custom = customer_profile
        .as_mut()
        .and_then(|profile| profile.custom.take());
    request_fingerprint(&[
        format!("{:?}", attributes.system_attributes),
        format!("{:?}", attributes.agent_attributes),
        format!("{customer_profile:?}"),
        customer_profile_custom
            .as_ref()
            .map(tags_fingerprint)
            .unwrap_or_default(),
        attributes
            .custom_attributes
            .as_ref()
            .map(tags_fingerprint)
            .unwrap_or_default(),
    ])
}

/// The metadata `UpdateMessageTemplateMetadata` writes: the three members that operation
/// carries, each `None` when the request did not carry it — "Partial update is supported.
/// If any field is not supplied, it will remain unchanged for the message template"
/// (`smithy_query(query="doc",shape="UpdateMessageTemplateMetadata")`).
#[derive(Debug, Clone, Default)]
pub struct MessageTemplateMetadata {
    pub name: Option<String>,
    pub description: Option<String>,
    pub grouping_configuration: Option<GroupingConfiguration>,
}

impl MessageTemplateMetadata {
    /// Write the members this metadata carries onto one record, leaving every member it
    /// does not carry exactly as it was.
    fn write_onto(&self, template: &mut MessageTemplate) {
        if let Some(name) = self.name.clone() {
            template.name = name;
        }
        if self.description.is_some() {
            template.description = self.description.clone();
        }
        if self.grouping_configuration.is_some() {
            template.grouping_configuration = self.grouping_configuration.clone();
        }
    }
}

impl ComAmazonawsQconnectWisdomserviceState {
    /// A `messageTemplateId` no stored message template holds, or `None` when
    /// [`MINT_ATTEMPTS`] candidates of this sequence number's block were all taken.
    pub fn mint_message_template_id(&mut self) -> Option<String> {
        let seq = self.next_seq();
        (0..MINT_ATTEMPTS as u64)
            .map(|attempt| mint_candidate(seq, attempt))
            .find(|candidate| !self.message_templates.contains_key(candidate))
    }

    /// Store a message template under the (already resolved) knowledge base it names.
    pub fn insert_message_template(&mut self, message_template: MessageTemplate) {
        self.message_templates.insert(
            message_template.message_template_id.clone(),
            message_template,
        );
    }

    /// The message template a (resolved `knowledgeBaseId`, resolved `messageTemplateId`)
    /// pair names, or `None` when that knowledge base holds none under that id —
    /// including a template of ANOTHER knowledge base, which this one does not hold.
    ///
    /// The id is taken ALREADY RESOLVED (and already stripped of any version qualifier,
    /// which [`resolve_qualified_id`] separates), so one lookup serves every label form.
    pub fn message_template(
        &self,
        knowledge_base_id: &str,
        message_template_id: &str,
    ) -> Option<&MessageTemplate> {
        self.message_templates
            .get(message_template_id)
            .filter(|template| template.knowledge_base_id == knowledge_base_id)
    }

    /// The same message template as [`Self::message_template`], for a caller that WRITES
    /// to it: the seam `UpdateMessageTemplate` applies its changes through, so an update
    /// and a read reach one record rather than two copies.
    pub fn message_template_mut(
        &mut self,
        knowledge_base_id: &str,
        message_template_id: &str,
    ) -> Option<&mut MessageTemplate> {
        self.message_templates
            .get_mut(message_template_id)
            .filter(|template| template.knowledge_base_id == knowledge_base_id)
    }

    /// Every message template of one knowledge base, in `messageTemplateId` order, so
    /// what a read reports does not depend on map iteration order.
    pub fn message_templates_of(&self, knowledge_base_id: &str) -> Vec<&MessageTemplate> {
        let mut templates: Vec<&MessageTemplate> = self
            .message_templates
            .values()
            .filter(|template| template.knowledge_base_id == knowledge_base_id)
            .collect();
        templates.sort_by(|left, right| {
            left.message_template_id.cmp(&right.message_template_id)
        });
        templates
    }

    /// The live message template of this knowledge base already using `name`, if any: the
    /// operation's own documentation states the rule ("The name of the message template
    /// has to be unique for each knowledge base"), so a second create claiming it
    /// conflicts with the first rather than replacing it.
    pub fn message_template_with_name(
        &self,
        knowledge_base_id: &str,
        name: &str,
    ) -> Option<&MessageTemplate> {
        self.message_templates_of(knowledge_base_id)
            .into_iter()
            .find(|template| template.name == name)
    }

    /// Forget ONE message template of a knowledge base, with every version it published
    /// — what `DeleteMessageTemplate` calls, whose documentation states that deleting a
    /// template with no version qualifier deletes "the message template and all available
    /// versions".
    ///
    /// Answers whether that knowledge base held a template under the id at all, so the
    /// caller can report the resource as missing without resolving it twice; a template of
    /// ANOTHER knowledge base is not this one's to forget.
    pub fn remove_message_template(
        &mut self,
        knowledge_base_id: &str,
        message_template_id: &str,
    ) -> bool {
        if self
            .message_template(knowledge_base_id, message_template_id)
            .is_none()
        {
            return false;
        }
        self.message_templates.remove(message_template_id);
        self.forget_message_template_versions_of(message_template_id);
        true
    }

    /// Apply `metadata` to the message template this knowledge base holds under
    /// `message_template_id` AND to every version that template has published, answering
    /// whether the template was there at all.
    ///
    /// That reach is the operation's own documentation: "any modification to the message
    /// template's name, description and grouping configuration will applied to the message
    /// template pointed by the $LATEST qualifier and all available versions"
    /// (`smithy_query(query="doc",shape="UpdateMessageTemplateMetadata")`) — so the working
    /// copy and each published snapshot carry the same name, description and grouping
    /// configuration once this returns, while everything else a version published (its
    /// content, its attributes, the instants it was published with) is left as it was.
    ///
    /// The working copy is re-stamped with when it was written and by whom, since
    /// `lastModifiedTime` is "the timestamp when the message template data was last
    /// modified" (`smithy_query(query="shape",shape="MessageTemplateData")`).
    pub fn write_message_template_metadata(
        &mut self,
        knowledge_base_id: &str,
        message_template_id: &str,
        metadata: &MessageTemplateMetadata,
        at: SystemTime,
        by: &str,
    ) -> bool {
        let Some(template) = self.message_template_mut(knowledge_base_id, message_template_id)
        else {
            return false;
        };
        metadata.write_onto(template);
        template.last_modified_at = at;
        template.last_modified_by = by.to_string();
        for version in self
            .message_template_versions
            .get_mut(message_template_id)
            .into_iter()
            .flatten()
        {
            metadata.write_onto(&mut version.snapshot);
        }
        true
    }

    /// Forget every message template of one knowledge base, answering with the ids it
    /// dropped — what `remove_knowledge_base` calls so a knowledge base that is gone takes
    /// its templates with it, releasing the names they held.
    ///
    /// A template takes its published versions with it
    /// ([`ComAmazonawsQconnectWisdomserviceState::forget_message_template_versions_of`]):
    /// a version exists only inside the template that published it.
    pub fn forget_message_templates_of(&mut self, knowledge_base_id: &str) -> Vec<String> {
        let orphaned: Vec<String> = self
            .message_templates_of(knowledge_base_id)
            .into_iter()
            .map(|template| template.message_template_id.clone())
            .collect();
        for message_template_id in &orphaned {
            self.message_templates.remove(message_template_id);
            self.forget_message_template_versions_of(message_template_id);
        }
        orphaned
    }
}
