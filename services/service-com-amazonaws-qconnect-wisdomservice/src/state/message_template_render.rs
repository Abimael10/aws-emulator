// SPDX-License-Identifier: CC0-1.0

//! Rendering one message template: the variable placeholders its content names, the
//! attribute VALUES a render resolves them from, and the content it answers with once they
//! are substituted.
//!
//! A template variable is written `{{<attribute key>}}` in the template's content, and the
//! values that fill it come from two places: the `attributes` object the render request
//! carries, and the `defaultAttributes` the template was created with. What a render
//! answers with is therefore derived entirely from those two objects and the stored
//! content — "if the attribute value is neither provided in the attribute request parameter
//! nor the default attribute of the message template, the rendered message content will
//! keep the variable placeholder as it is and return the attribute keys that are missing"
//! (`smithy_query(query="operation",operation="RenderMessageTemplate")`).
//!
//! Nothing here writes: a render reports what the stored record already says, so two
//! renders of one template with the same attributes answer alike and neither changes what
//! a later caller reads.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::Range;

use super::*;
use crate::ops::types::{
    AgentAttributes, CustomerProfileAttributes, EmailHeader, EmailMessageTemplateContent,
    EmailMessageTemplateContentBody, MessageTemplateAttributes,
    MessageTemplateBodyContentProvider, MessageTemplateContentProvider,
    PushADMMessageTemplateContent, PushAPNSMessageTemplateContent, PushBaiduMessageTemplateContent,
    PushFCMMessageTemplateContent, PushMessageTemplateContent, SMSMessageTemplateContent,
    SMSMessageTemplateContentBody, SystemAttributes, SystemEndpointAttributes,
    WhatsAppMessageTemplateContent,
};

/// The delimiters a template variable is written between.
const PLACEHOLDER_OPEN: &str = "{{";
const PLACEHOLDER_CLOSE: &str = "}}";

/// Every variable placeholder a piece of template text names, in the order the text names
/// them: the byte range the placeholder occupies, and the attribute key it names with any
/// surrounding whitespace trimmed off.
///
/// THE ONE PLACE this service reads placeholders off text, so the keys a quick response
/// reports ([`ComAmazonawsQconnectWisdomserviceState`]'s quick-response projection) and the
/// spans a render substitutes cannot disagree about what a placeholder is. An unterminated
/// or empty placeholder names no attribute and is skipped, so no key this emulator cannot
/// read off the text is ever claimed, and a text that opens a placeholder again before
/// closing the first one names only what the LAST opener began.
///
/// One linear pass, no recursion, and nothing retained but the placeholders found; the
/// ranges are ascending and do not overlap, so a caller may splice over them in order.
pub fn placeholders(text: &str) -> Vec<(Range<usize>, String)> {
    let mut found: Vec<(Range<usize>, String)> = Vec::new();
    let mut cursor = 0usize;
    while let Some(open) = text[cursor..].find(PLACEHOLDER_OPEN) {
        let after_open = cursor + open + PLACEHOLDER_OPEN.len();
        let Some(close) = text[after_open..].find(PLACEHOLDER_CLOSE) else {
            break;
        };
        let span = &text[after_open..after_open + close];
        let end = after_open + close + PLACEHOLDER_CLOSE.len();
        let (key_start, key) = match span.rfind(PLACEHOLDER_OPEN) {
            Some(reopened) => (
                after_open + reopened + PLACEHOLDER_OPEN.len(),
                span[reopened + PLACEHOLDER_OPEN.len()..].trim(),
            ),
            None => (after_open, span.trim()),
        };
        if !key.is_empty() {
            found.push((key_start - PLACEHOLDER_OPEN.len()..end, key.to_string()));
        }
        cursor = end;
    }
    found
}

/// Each attribute key `text` names, once and in the order the text first names it.
///
/// The keys already seen are held in a SET beside the list, so the work of reporting them
/// grows with the length of the text rather than with its length times the number of
/// distinct keys it names.
pub fn text_attribute_keys(text: &str) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for (_, key) in placeholders(text) {
        if seen.insert(key.clone()) {
            keys.push(key);
        }
    }
    keys
}

/// Hold one attribute value under the key its placeholder names, unless that key already
/// has one: the FIRST value held for a key is the one a render resolves it with.
fn hold(values: &mut BTreeMap<String, String>, key: &str, value: Option<&String>) {
    if let Some(value) = value {
        values
            .entry(key.to_string())
            .or_insert_with(|| value.clone());
    }
}

/// Hold every entry of one attribute MAP under the key the entry itself carries: "Each key
/// defines a variable or placeholder in the message template"
/// (`smithy_query(query="doc",shape="RenderMessageTemplateRequest",member="attributes")`),
/// so a map entry's key IS the name of the variable it fills.
fn hold_map(values: &mut BTreeMap<String, String>, map: Option<&HashMap<String, String>>) {
    for (key, value) in map.into_iter().flatten() {
        hold(values, key, Some(value));
    }
}

/// The values the `systemAttributes` category carries, under the names the system category
/// qualifies its members with.
fn hold_system(values: &mut BTreeMap<String, String>, system: &SystemAttributes) {
    hold(values, "System.Name", system.name.as_ref());
    let endpoint = |endpoint: &Option<SystemEndpointAttributes>| {
        endpoint
            .as_ref()
            .and_then(|endpoint| endpoint.address.clone())
    };
    hold(
        values,
        "System.CustomerEndpoint.Address",
        endpoint(&system.customer_endpoint).as_ref(),
    );
    hold(
        values,
        "System.SystemEndpoint.Address",
        endpoint(&system.system_endpoint).as_ref(),
    );
}

/// The values the `agentAttributes` category carries, under the names the agent category
/// qualifies its members with.
fn hold_agent(values: &mut BTreeMap<String, String>, agent: &AgentAttributes) {
    hold(values, "Agent.FirstName", agent.first_name.as_ref());
    hold(values, "Agent.LastName", agent.last_name.as_ref());
}

/// The values the `customerProfileAttributes` category carries, under the names the
/// customer category qualifies its members with, plus every entry of the `custom` map it
/// reaches (whose keys name their own variables, see [`hold_map`]).
///
/// One arm per member the model declares on that structure
/// (`smithy_query(query="shape",shape="CustomerProfileAttributes")`), in the order it
/// declares them, so every customer-profile attribute a request can carry fills the
/// variable of the same name.
fn hold_customer_profile(values: &mut BTreeMap<String, String>, profile: &CustomerProfileAttributes) {
    macro_rules! hold_members {
        ($( $field:ident => $name:literal ),* $(,)?) => {
            $( hold(values, $name, profile.$field.as_ref()); )*
        };
    }
    hold_members! {
        profile_id => "Customer.ProfileId",
        profile_arn => "Customer.ProfileARN",
        first_name => "Customer.FirstName",
        middle_name => "Customer.MiddleName",
        last_name => "Customer.LastName",
        account_number => "Customer.AccountNumber",
        email_address => "Customer.EmailAddress",
        phone_number => "Customer.PhoneNumber",
        additional_information => "Customer.AdditionalInformation",
        party_type => "Customer.PartyType",
        business_name => "Customer.BusinessName",
        birth_date => "Customer.BirthDate",
        gender => "Customer.Gender",
        mobile_phone_number => "Customer.MobilePhoneNumber",
        home_phone_number => "Customer.HomePhoneNumber",
        business_phone_number => "Customer.BusinessPhoneNumber",
        business_email_address => "Customer.BusinessEmailAddress",
        address1 => "Customer.Address1",
        address2 => "Customer.Address2",
        address3 => "Customer.Address3",
        address4 => "Customer.Address4",
        city => "Customer.City",
        county => "Customer.County",
        country => "Customer.Country",
        postal_code => "Customer.PostalCode",
        province => "Customer.Province",
        state => "Customer.State",
        shipping_address1 => "Customer.ShippingAddress1",
        shipping_address2 => "Customer.ShippingAddress2",
        shipping_address3 => "Customer.ShippingAddress3",
        shipping_address4 => "Customer.ShippingAddress4",
        shipping_city => "Customer.ShippingCity",
        shipping_county => "Customer.ShippingCounty",
        shipping_country => "Customer.ShippingCountry",
        shipping_postal_code => "Customer.ShippingPostalCode",
        shipping_province => "Customer.ShippingProvince",
        shipping_state => "Customer.ShippingState",
        mailing_address1 => "Customer.MailingAddress1",
        mailing_address2 => "Customer.MailingAddress2",
        mailing_address3 => "Customer.MailingAddress3",
        mailing_address4 => "Customer.MailingAddress4",
        mailing_city => "Customer.MailingCity",
        mailing_county => "Customer.MailingCounty",
        mailing_country => "Customer.MailingCountry",
        mailing_postal_code => "Customer.MailingPostalCode",
        mailing_province => "Customer.MailingProvince",
        mailing_state => "Customer.MailingState",
        billing_address1 => "Customer.BillingAddress1",
        billing_address2 => "Customer.BillingAddress2",
        billing_address3 => "Customer.BillingAddress3",
        billing_address4 => "Customer.BillingAddress4",
        billing_city => "Customer.BillingCity",
        billing_county => "Customer.BillingCounty",
        billing_country => "Customer.BillingCountry",
        billing_postal_code => "Customer.BillingPostalCode",
        billing_province => "Customer.BillingProvince",
        billing_state => "Customer.BillingState",
    }
    hold_map(values, profile.custom.as_ref());
}

/// Every variable one `MessageTemplateAttributes` object fills, keyed by the placeholder
/// name that names it.
///
/// The object "contains different categories of key-value pairs. Each key defines a
/// variable or placeholder in the message template. The corresponding value defines the
/// value for that variable"
/// (`smithy_query(query="doc",shape="RenderMessageTemplateRequest",member="attributes")`),
/// so the name of a variable is the key of the attribute that fills it: a MAP category
/// carries its keys itself, and the three structured categories name theirs by member,
/// qualified by the category — `System.Name`, `Agent.FirstName`, `Customer.FirstName` and
/// the rest.
///
/// The categories are read in the order the model declares them on the structure and the
/// first value found for a key is kept, so one object cannot fill one variable twice.
pub fn attribute_values(attributes: &MessageTemplateAttributes) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    if let Some(system) = attributes.system_attributes.as_ref() {
        hold_system(&mut values, system);
    }
    if let Some(agent) = attributes.agent_attributes.as_ref() {
        hold_agent(&mut values, agent);
    }
    if let Some(profile) = attributes.customer_profile_attributes.as_ref() {
        hold_customer_profile(&mut values, profile);
    }
    hold_map(&mut values, attributes.custom_attributes.as_ref());
    values
}

/// The most bytes one render may produce, counted across every text the content carries.
///
/// A render SUBSTITUTES: one placeholder becomes one attribute value, so the output grows
/// with the number of placeholders the stored content names TIMES the length of the values
/// the request carries, and neither the modelled attribute value nor the content shapes
/// bound that product. This ceiling is what keeps it finite: past it the render is refused
/// rather than answered, so a small request cannot make this emulator build an arbitrarily
/// large response.
///
/// CHOSEN: AWS publishes no size for a rendered message template. The figure is the largest
/// body this emulator transports at all (`vendor/server/src/lib.rs`'s
/// `MAX_PRODUCTION_BODY_BYTES`, 16 MiB), since a rendering past it could not be served even
/// if it were built.
pub const MAX_RENDERED_CONTENT_BYTES: usize = 16 * 1024 * 1024;

/// The sentence a render whose substitutions would run past [`MAX_RENDERED_CONTENT_BYTES`]
/// is refused with, stating the RULE — the ceiling and what to do about it — and carrying
/// nothing from the request.
pub fn message_rendered_content_too_large() -> String {
    format!(
        "The rendered message template content must not exceed {MAX_RENDERED_CONTENT_BYTES} \
         bytes. Provide shorter attribute values, or a message template whose content uses \
         fewer variables, and try again."
    )
}

/// The content one render answers with, and the keys it could not resolve.
#[derive(Debug, Clone, Default)]
pub struct RenderedMessageTemplate {
    /// The stored content with every variable the attributes fill substituted, or `None`
    /// for a template that holds no content at all.
    pub content: Option<MessageTemplateContentProvider>,
    /// The attribute keys the content names that neither the request's attributes nor the
    /// template's default attributes fill, each once and in the order the content names
    /// them. Their placeholders are left in the content as they stand.
    pub attributes_not_interpolated: Vec<String>,
}

/// The variables a render of `template` resolves from `attributes`: every variable the
/// request fills, plus every one the template's own `defaultAttributes` fills and the
/// request does not — the request parameter is consulted first, and the default attribute
/// of the message template is the fallback.
fn render_values(
    template: &MessageTemplate,
    attributes: &MessageTemplateAttributes,
) -> BTreeMap<String, String> {
    let mut values = attribute_values(attributes);
    for (key, value) in template
        .default_attributes
        .as_ref()
        .map(attribute_values)
        .unwrap_or_default()
    {
        values.entry(key).or_insert(value);
    }
    values
}

/// Substitutes the variables of one render into the template's content, collecting the keys
/// it holds no value for and stopping as soon as the output would run past the ceiling.
struct Renderer {
    values: BTreeMap<String, String>,
    /// The keys no value was found for, in the order the content named them.
    missing: Vec<String>,
    /// The same keys as a SET, so one key is reported once however often the content names
    /// it and the work grows with the length of the content rather than with its length
    /// times the number of distinct keys it leaves unresolved.
    missing_seen: BTreeSet<String>,
    /// How many more bytes this render may produce — [`MAX_RENDERED_CONTENT_BYTES`] before
    /// it renders its first text. Checked BEFORE each piece is appended, so no allocation
    /// of this render runs past the ceiling.
    remaining: usize,
    /// Whether the ceiling was reached, which makes the whole render a refusal.
    exceeded: bool,
}

impl Renderer {
    /// Append `piece` while the remaining budget covers it, and record that the ceiling was
    /// reached when it does not.
    fn append(&mut self, rendered: &mut String, piece: &str) {
        if self.exceeded {
            return;
        }
        match self.remaining.checked_sub(piece.len()) {
            Some(remaining) => {
                self.remaining = remaining;
                rendered.push_str(piece);
            }
            None => self.exceeded = true,
        }
    }

    /// One piece of template text with every variable it names that this render fills
    /// replaced by that variable's value, and every one it does not left exactly as it
    /// stands — the key of each of those recorded once, in the order the content names it.
    ///
    /// A render that has reached the ceiling builds nothing further: it is already a
    /// refusal, so no more is allocated for it.
    fn text(&mut self, text: &str) -> String {
        if self.exceeded {
            return String::new();
        }
        let mut rendered = String::with_capacity(text.len().min(self.remaining));
        let mut cursor = 0usize;
        for (span, key) in placeholders(text) {
            let literal = cursor..span.start;
            self.append(&mut rendered, &text[literal]);
            // The value is taken out of the map before it is appended, because appending
            // spends this render's budget and so borrows the renderer.
            match self.values.get(&key).cloned() {
                Some(value) => self.append(&mut rendered, &value),
                None => {
                    let placeholder = span.clone();
                    self.append(&mut rendered, &text[placeholder]);
                    if self.missing_seen.insert(key.clone()) {
                        self.missing.push(key);
                    }
                }
            }
            if self.exceeded {
                return String::new();
            }
            cursor = span.end;
        }
        self.append(&mut rendered, &text[cursor..]);
        rendered
    }

    /// An optional piece of template text: a member the content does not carry stays
    /// absent rather than becoming empty.
    fn optional(&mut self, text: Option<&String>) -> Option<String> {
        text.map(|text| self.text(text))
    }

    /// A `MessageTemplateBodyContentProvider`, whose one member carries body text.
    fn body(
        &mut self,
        body: Option<&MessageTemplateBodyContentProvider>,
    ) -> Option<MessageTemplateBodyContentProvider> {
        body.map(|body| match body {
            MessageTemplateBodyContentProvider::Content(content) => {
                MessageTemplateBodyContentProvider::Content(self.text(content))
            }
        })
    }

    fn email(&mut self, email: &EmailMessageTemplateContent) -> EmailMessageTemplateContent {
        EmailMessageTemplateContent {
            subject: self.optional(email.subject.as_ref()),
            body: email
                .body
                .as_ref()
                .map(|body| EmailMessageTemplateContentBody {
                    plain_text: self.body(body.plain_text.as_ref()),
                    html: self.body(body.html.as_ref()),
                }),
            headers: email.headers.as_ref().map(|headers| {
                headers
                    .iter()
                    .map(|header| EmailHeader {
                        name: self.optional(header.name.as_ref()),
                        value: self.optional(header.value.as_ref()),
                    })
                    .collect()
            }),
        }
    }

    fn sms(&mut self, sms: &SMSMessageTemplateContent) -> SMSMessageTemplateContent {
        SMSMessageTemplateContent {
            body: sms.body.as_ref().map(|body| SMSMessageTemplateContentBody {
                plain_text: self.body(body.plain_text.as_ref()),
            }),
        }
    }

    fn push(&mut self, push: &PushMessageTemplateContent) -> PushMessageTemplateContent {
        PushMessageTemplateContent {
            adm: push.adm.as_ref().map(|adm| PushADMMessageTemplateContent {
                title: self.optional(adm.title.as_ref()),
                body: self.body(adm.body.as_ref()),
                action: self.optional(adm.action.as_ref()),
                sound: self.optional(adm.sound.as_ref()),
                url: self.optional(adm.url.as_ref()),
                image_url: self.optional(adm.image_url.as_ref()),
                image_icon_url: self.optional(adm.image_icon_url.as_ref()),
                small_image_icon_url: self.optional(adm.small_image_icon_url.as_ref()),
                raw_content: self.body(adm.raw_content.as_ref()),
            }),
            apns: push
                .apns
                .as_ref()
                .map(|apns| PushAPNSMessageTemplateContent {
                    title: self.optional(apns.title.as_ref()),
                    body: self.body(apns.body.as_ref()),
                    action: self.optional(apns.action.as_ref()),
                    sound: self.optional(apns.sound.as_ref()),
                    url: self.optional(apns.url.as_ref()),
                    media_url: self.optional(apns.media_url.as_ref()),
                    raw_content: self.body(apns.raw_content.as_ref()),
                }),
            fcm: push.fcm.as_ref().map(|fcm| PushFCMMessageTemplateContent {
                title: self.optional(fcm.title.as_ref()),
                body: self.body(fcm.body.as_ref()),
                action: self.optional(fcm.action.as_ref()),
                sound: self.optional(fcm.sound.as_ref()),
                url: self.optional(fcm.url.as_ref()),
                image_url: self.optional(fcm.image_url.as_ref()),
                image_icon_url: self.optional(fcm.image_icon_url.as_ref()),
                small_image_icon_url: self.optional(fcm.small_image_icon_url.as_ref()),
                raw_content: self.body(fcm.raw_content.as_ref()),
            }),
            baidu: push
                .baidu
                .as_ref()
                .map(|baidu| PushBaiduMessageTemplateContent {
                    title: self.optional(baidu.title.as_ref()),
                    body: self.body(baidu.body.as_ref()),
                    action: self.optional(baidu.action.as_ref()),
                    sound: self.optional(baidu.sound.as_ref()),
                    url: self.optional(baidu.url.as_ref()),
                    image_url: self.optional(baidu.image_url.as_ref()),
                    image_icon_url: self.optional(baidu.image_icon_url.as_ref()),
                    small_image_icon_url: self.optional(baidu.small_image_icon_url.as_ref()),
                    raw_content: self.body(baidu.raw_content.as_ref()),
                }),
        }
    }

    /// The whole content union, under the SAME member the stored content was given under:
    /// a render fills the template's variables, it does not move its content to another
    /// channel subtype.
    ///
    /// Every text the content carries is template text, so each one is rendered the same
    /// way; a member that names no variable comes back exactly as it was stored.
    fn content(&mut self, content: &MessageTemplateContentProvider) -> MessageTemplateContentProvider {
        match content {
            MessageTemplateContentProvider::Email(email) => {
                MessageTemplateContentProvider::Email(self.email(email))
            }
            MessageTemplateContentProvider::Sms(sms) => {
                MessageTemplateContentProvider::Sms(self.sms(sms))
            }
            MessageTemplateContentProvider::whatsApp(whats_app) => {
                MessageTemplateContentProvider::whatsApp(WhatsAppMessageTemplateContent {
                    data: self.optional(whats_app.data.as_ref()),
                })
            }
            MessageTemplateContentProvider::Push(push) => {
                MessageTemplateContentProvider::Push(self.push(push))
            }
        }
    }
}

/// One message template rendered with the attributes a render request carried, or the
/// sentence the render is refused with when its substitutions would run past
/// [`MAX_RENDERED_CONTENT_BYTES`].
///
/// The content is the STORED content with every variable the attributes fill substituted
/// for its value; a variable neither the request's attributes nor the template's
/// `defaultAttributes` fill keeps its placeholder, and its key is reported in
/// `attributesNotInterpolated`. A template that holds no content renders none and names no
/// missing key, because it names no variable.
///
/// The caller renders a COPY of the stored record, so the substitution never runs while the
/// store is held.
pub fn render_message_template(
    template: &MessageTemplate,
    attributes: &MessageTemplateAttributes,
) -> Result<RenderedMessageTemplate, String> {
    let mut renderer = Renderer {
        values: render_values(template, attributes),
        missing: Vec::new(),
        missing_seen: BTreeSet::new(),
        remaining: MAX_RENDERED_CONTENT_BYTES,
        exceeded: false,
    };
    let content = template
        .content
        .as_ref()
        .map(|content| renderer.content(content));
    if renderer.exceeded {
        return Err(message_rendered_content_too_large());
    }
    Ok(RenderedMessageTemplate {
        content,
        attributes_not_interpolated: renderer.missing,
    })
}
