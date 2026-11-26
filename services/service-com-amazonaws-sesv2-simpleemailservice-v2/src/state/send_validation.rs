// SPDX-License-Identifier: CC0-1.0

//! What the REQUEST alone settles about an outbound message, and how a defect in it reads.
//!
//! Amazon SES reads a send in two stages: it validates the request and the message content
//! FIRST — the addresses the message names, the recipients it addresses, the headers and the
//! tags it carries, the attachments it includes and the template it renders from — and only
//! then weighs the message against the account's own state (its sending switches, its
//! sending identities, and the resources the content names). A malformed message is
//! therefore the caller's mistake (`BadRequestException`, 400) whatever the account holds,
//! and a client is told which element of its request is wrong rather than being pointed at
//! an identity or a resource.
//!
//! Each reading lives here once, so the two send paths — [`crate::ops::send_email`] and
//! [`crate::ops::send_bulk_email`] — cannot disagree about which message is well formed or
//! about how its defect reads.

use crate::ops::types::{Attachment, Destination, EmailTemplateContent, MessageHeader, MessageTag};

/// Recipients ONE message may address, across its To:, CC: and BCC: lines.
///
/// Amazon SES states one quota for a message's recipient set — the total number of
/// recipients of one message — and counts every recipient line against it, so the ceiling is
/// read over the flattened set ([`destination_recipients`]) rather than per line.
pub const MAX_RECIPIENTS_PER_MESSAGE: usize = 50;

/// How SES refuses a message that addresses more recipients than one message may carry: the
/// fixed text stating the ceiling. Nothing of the request is echoed — the count the caller
/// submitted is not what the rule is about.
#[must_use]
pub fn too_many_recipients() -> String {
    format!("Recipient count exceeds {MAX_RECIPIENTS_PER_MESSAGE}.")
}

/// Characters a message tag's name or value may carry: "It can contain no more than 256
/// characters" (`MessageTag$Name`, `MessageTag$Value`).
pub const MAX_MESSAGE_TAG_LENGTH: usize = 256;

/// How SES refuses a `Destination` that addresses nobody, in a BULK send's per-entry
/// result.
///
/// "The message must include at least one recipient email address", so a destination whose
/// three recipient lines are all absent or empty addresses no mailbox at all. Fixed text:
/// the sentence names the member and nothing from the request. A SINGLE send is recorded
/// wording the same absence as the missing recipient header it is
/// (`crate::ops::send_email`), and the two must not be swapped: a client reading one
/// surface's sentence must not be handed the other's.
pub const EMPTY_DESTINATION: &str = "Destination cannot be empty";

/// Whether inline template content carries what a template has to carry, or the sentence
/// refusing content that does not.
///
/// An email template is "composed of a subject line, an HTML part, and a text-only part"
/// (`EmailTemplateContent`): the subject is the one part no send supplies for itself, and a
/// template with no body part at all composes no message, so content carrying either of them
/// empty is not template content. Content the request carries INLINE is the template, so it
/// is read here the way a STORED one is read when it is written
/// (`crate::ops::create_email_template`).
#[must_use]
pub fn inline_template_defect(content: &EmailTemplateContent) -> Option<&'static str> {
    let stated = |part: &Option<String>| !part.as_deref().unwrap_or_default().is_empty();
    let complete = stated(&content.subject) && (stated(&content.text) || stated(&content.html));
    (!complete).then_some(TEMPLATE_MISSING_REQUIRED_FIELDS)
}

/// How SES refuses inline template content that is not a template: fixed text naming no
/// field, since the content is refused for what it does not carry.
pub const TEMPLATE_MISSING_REQUIRED_FIELDS: &str = "Template is missing required fields.";

/// Every recipient of one `Destination`, To: then CC: then BCC:, in the order the request
/// lists them.
///
/// The lines are flattened because SES treats them as ONE recipient set: each recipient of
/// a message counts once against the account's sending quota, whichever line addresses it.
#[must_use]
pub fn destination_recipients(destination: &Destination) -> Vec<&str> {
    [
        destination.to_addresses.as_ref(),
        destination.cc_addresses.as_ref(),
        destination.bcc_addresses.as_ref(),
    ]
    .into_iter()
    .flatten()
    .flatten()
    .map(String::as_str)
    .collect()
}

/// The defect in the recipient set of one message, or `None` for a set SES would read.
///
/// Each address in the set is read in the order the request lists them
/// ([`address_defect`]), so the first malformed recipient is the one reported. How MANY
/// recipients one message may address is a quota on the message rather than a form its
/// members have to meet ([`too_many_recipients`]), and is read where the send weighs the
/// message itself.
#[must_use]
pub fn recipient_set_defect(recipients: &[&str]) -> Option<String> {
    recipients
        .iter()
        .find_map(|recipient| address_defect(recipient))
        .map(str::to_string)
}

/// What is wrong with the `TenantName` a send names, or `None` for a value SES accepts —
/// a send that names no tenant at all included.
///
/// The member names a TENANT of the account, so the value has to be one a tenant can be
/// named by: the documented `TenantName` form, read through the one check every operation
/// that takes the member reads ([`crate::state::tenant_name_is_well_formed`]) and refused
/// with the one sentence they all refuse it with
/// ([`crate::state::invalid_tenant_name`]). Part of the REQUEST's own form, so it is
/// settled before the account's tenant table is read, exactly as the addresses and the
/// recipient set above are.
#[must_use]
pub fn tenant_name_defect(tenant_name: Option<&str>) -> Option<String> {
    let name = tenant_name?;
    (!crate::state::tenant_name_is_well_formed(name))
        .then(|| crate::state::invalid_tenant_name(name))
}

/// The defect in the address a BULK send is SENT FROM, or `None` for a value that names a
/// mailbox.
///
/// The sending identity is read against the same address form as every other address
/// ([`address_defect`]); what differs is the report. `SendBulkEmail` is recorded naming the
/// VALUE it was given, as the member it is, while `SendEmail` is recorded answering the
/// parser's own report of the same value for the same member
/// (`crate::ops::send_email` passes its `FromEmailAddress` to [`address_defect`]), so the
/// two sentences are kept apart rather than one being used for both.
#[must_use]
pub fn sender_defect(value: &str) -> Option<String> {
    address_defect(value).map(|_| invalid_sending_address(value))
}

/// How SES refuses an address that names no mailbox where it names the SENDER: the fixed
/// text `Invalid email address<` and `>.`, with the value the request spelled as its one
/// variable part.
#[must_use]
pub fn invalid_sending_address(value: &str) -> String {
    format!("Invalid email address<{value}>.")
}

/// What is wrong with one caller-supplied email address, in the words SES reports it with,
/// or `None` for a value that names a mailbox.
///
/// WHICH values are addresses is not read here: the form a caller-supplied address has to
/// meet is the tree's one reading of it
/// (`service_common::email_address`), so this surface and the classic SES surface cannot
/// disagree about what an email address is. What belongs to THIS service is the wording —
/// SES reads an address with the RFC 5322 reader of its mail stack, which reports the defect
/// it found rather than the value it found it in, so each sentence below is that report and
/// none of them carries anything from the request.
///
/// A value may arrive behind a display name (`Sender Name <sender@example.com>`), which
/// names the same mailbox, so the addr-spec is what is read ([`crate::state::addr_spec`]).
/// Amazon SES does not support the SMTPUTF8 extension — "the local part of a destination
/// email address … may only contain 7-bit ASCII characters", and a non-ASCII domain "must
/// be encoded using Punycode" (`Destination`) — which is why the shared form admits no
/// non-ASCII character in either half, and such a character reads exactly as a control
/// character does.
#[must_use]
pub fn address_defect(value: &str) -> Option<&'static str> {
    use ::service_common::email_address::AddressDefect;

    let defect = ::service_common::email_address::address_defect(crate::state::addr_spec(value))?;
    Some(match defect {
        AddressDefect::MissingDomainSeparator => MISSING_FINAL_DOMAIN,
        AddressDefect::MissingLocalPart => MISSING_LOCAL_NAME,
        AddressDefect::MissingDomain => MISSING_DOMAIN,
        AddressDefect::LocalControlOrWhitespace => LOCAL_CONTROL_OR_WHITESPACE,
        AddressDefect::LocalIllegalCharacter => LOCAL_ILLEGAL_CHARACTER,
        AddressDefect::LocalStartsWithDot => LOCAL_STARTS_WITH_DOT,
        AddressDefect::LocalEndsWithDot => LOCAL_ENDS_WITH_DOT,
        AddressDefect::LocalDotDot => LOCAL_DOT_DOT,
        AddressDefect::DomainControlOrWhitespace => DOMAIN_CONTROL_OR_WHITESPACE,
        AddressDefect::DomainIllegalCharacter => DOMAIN_ILLEGAL_CHARACTER,
        AddressDefect::DomainDotDot => DOMAIN_DOT_DOT,
        AddressDefect::DomainIsNotAName => DOMAIN_IS_NOT_A_NAME,
    })
}

/// A value carrying no `@` at all: there is no domain for the mailbox it names.
const MISSING_FINAL_DOMAIN: &str = "Missing final '@domain'";

/// A value whose `@` is the first character: it names no mailbox in the domain.
const MISSING_LOCAL_NAME: &str = "Missing local name";

/// A value whose `@` is the last character: it names no domain for the mailbox.
const MISSING_DOMAIN: &str = "Missing domain";

/// A mailbox name carrying a character that is not one of an address.
const LOCAL_CONTROL_OR_WHITESPACE: &str = "Local address contains control or whitespace";

/// A mailbox name carrying one of RFC 5322's specials.
const LOCAL_ILLEGAL_CHARACTER: &str = "Local address contains illegal character";

/// A mailbox name whose first character is the atom separator, so its first atom is empty.
const LOCAL_STARTS_WITH_DOT: &str = "Local address starts with dot";

/// A mailbox name whose last character is the atom separator, so its last atom is empty.
const LOCAL_ENDS_WITH_DOT: &str = "Local address ends with dot";

/// A mailbox name with an empty atom inside it.
const LOCAL_DOT_DOT: &str = "Local address contains dot-dot";

/// A domain carrying a character that is not one of a domain name.
const DOMAIN_CONTROL_OR_WHITESPACE: &str = "Domain contains control or whitespace";

/// A domain carrying a character no domain label may.
const DOMAIN_ILLEGAL_CHARACTER: &str = "Domain contains illegal character";

/// A domain with an empty label inside it.
const DOMAIN_DOT_DOT: &str = "Domain contains dot-dot";

/// A domain that is no domain NAME: the form an address member documents is
/// `UserName@[SubDomain.]Domain.TopLevelDomain`, so a value whose domain carries no
/// top-level label names no domain at all.
const DOMAIN_IS_NOT_A_NAME: &str = "Illegal address";

/// The defect in the headers a message adds to itself, or `None` when SES would add them
/// all.
///
/// `Message$Headers`/`Template$Headers` are "the list of message headers that will be added
/// to the email message": a header SES writes itself cannot be among them, because the
/// request states the same field through its own members and SES composes the message from
/// those. The first such header the request lists is the one reported.
#[must_use]
pub fn headers_defect(headers: &[MessageHeader]) -> Option<String> {
    headers
        .iter()
        .find(|header| is_reserved_header(&header.name))
        .map(|header| unsupported_header(&header.name))
}

/// How SES refuses a header it writes itself: the fixed text `Header <` and
/// `> is not supported`, with the header name exactly as the request spelled it.
#[must_use]
pub fn unsupported_header(name: &str) -> String {
    format!("Header <{name}> is not supported")
}

/// Whether `name` names a header field of the message SES composes itself, compared the way
/// a header name is compared — "field names … are case-insensitive" (RFC 5322).
fn is_reserved_header(name: &str) -> bool {
    RESERVED_HEADERS
        .iter()
        .any(|reserved| reserved.eq_ignore_ascii_case(name.trim()))
}

/// The header fields of the message Amazon SES writes itself, and therefore refuses in a
/// request's own header list.
///
/// Each one is a field the send already states through its OWN input member — the originator
/// and recipient fields a `FromEmailAddress`, a `Destination`, `ReplyToAddresses` and a
/// `FeedbackForwardingEmailAddress` state, and the subject a `Message$Subject` or a rendered
/// template states — or one of the MIME fields SES composes the message's own structure with
/// from the body parts and the attachments it was given. A request stating such a field twice
/// states two different things about one message, which is why SES refuses it rather than
/// choosing. Written in the spelling the standards use; the comparison is case-insensitive
/// ([`is_reserved_header`]).
const RESERVED_HEADERS: &[&str] = &[
    "Bcc",
    "Cc",
    "Content-Transfer-Encoding",
    "Content-Type",
    "From",
    "MIME-Version",
    "Reply-To",
    "Return-Path",
    "Sender",
    "Subject",
    "To",
];

/// Whether Amazon SES can read text in the character set a `Content$Charset` names, or the
/// sentence refusing one it cannot.
///
/// "The character set for the content. Because of the constraints of the SMTP protocol,
/// Amazon SES uses 7-bit ASCII by default. If the text includes characters outside of the
/// ASCII range, you have to specify a character set. For example, you could specify UTF-8,
/// ISO-8859-1, or Shift_JIS": the member names a character set, so a value that names none
/// is the caller's mistake. Which names are character sets is the ENCODING STANDARD's own
/// register of labels and their aliases (`encoding_rs`), not a list kept here, so `utf8`,
/// `UTF-8` and `ISO-8859-1` are the sets they name and nothing else has to be enumerated.
/// A member stated empty names no set and is read as the default, like an absent one.
#[must_use]
pub fn charset_defect(charset: &str) -> Option<String> {
    let label = charset.trim();
    if label.is_empty() {
        return None;
    }
    ::encoding_rs::Encoding::for_label(label.as_bytes())
        .is_none()
        .then(|| unsupported_charset(charset))
}

/// How SES refuses a character set it cannot read: the fixed text `Unsupported charset '`
/// and `'.`, with the value the request spelled as its one variable part.
#[must_use]
pub fn unsupported_charset(charset: &str) -> String {
    format!("Unsupported charset '{charset}'.")
}

/// The defect in the tags a message carries, or `None` when SES would publish them all.
///
/// A message tag's name and value each have to meet one stated form: "It can only contain
/// ASCII letters (a–z, A–Z), numbers (0–9), underscores (_), or dashes (-)" and "It can
/// contain no more than 256 characters" (`MessageTag$Name`, `MessageTag$Value`). The name is
/// what names the tag, so it carries at least one character, and it names ONE tag of the
/// message: a second tag of the same name states a second value for the same
/// characteristic, which is no tag set. A value may be empty. The first tag with a defect,
/// in the order the request lists them, is the one reported.
#[must_use]
pub fn tags_defect(tags: &[MessageTag]) -> Option<String> {
    let mut seen: ::std::collections::BTreeSet<&str> = ::std::collections::BTreeSet::new();
    for tag in tags {
        if tag.name.is_empty() || !is_message_tag_text(&tag.name) {
            return Some(invalid_tag_name(&tag.name));
        }
        if !is_message_tag_text(&tag.value) {
            return Some(invalid_tag_value(&tag.value));
        }
        if !seen.insert(tag.name.as_str()) {
            return Some(duplicate_tag(&tag.name));
        }
    }
    None
}

/// How SES refuses a message that carries two tags of ONE name: the fixed text
/// `Duplicate tag <` and `>`, with the name the request repeated as its one variable part.
#[must_use]
pub fn duplicate_tag(name: &str) -> String {
    format!("Duplicate tag <{name}>")
}

/// Whether `value` is text a message tag's name or value may be.
fn is_message_tag_text(value: &str) -> bool {
    value.chars().count() <= MAX_MESSAGE_TAG_LENGTH
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_' || character == '-')
}

/// How SES refuses a message tag whose NAME is outside the form its member states: the
/// fixed text around the name the request spelled, stating the characters the form admits.
#[must_use]
pub fn invalid_tag_name(name: &str) -> String {
    format!("Invalid tag name <{name}>: {TAG_TEXT_FORM}")
}

/// How SES refuses a message tag whose VALUE is outside that same form.
#[must_use]
pub fn invalid_tag_value(value: &str) -> String {
    format!("Invalid tag value <{value}>: {TAG_TEXT_FORM}")
}

/// The form a message tag's name and value have to meet, as SES states it in both
/// refusals: the characters `MessageTag$Name` and `MessageTag$Value` admit.
const TAG_TEXT_FORM: &str = "only alphanumeric ASCII characters, '_',  '-' are allowed.";

/// The defect in the attachments a message includes, or `None` when SES would attach them
/// all.
///
/// An attachment is named by the file name it "will appear in the email" under, and "Amazon
/// SES restricts certain file extensions" (`Attachment$FileName`): a file name whose
/// extension is among the types SES does not carry, and one that names no file at all, is
/// not a file name SES will attach. `Attachment$ContentType` is "the MIME type of the
/// attachment" ("Example: application/pdf, image/jpeg"), which SES reads with its mail
/// stack's media-type reader, so a value that is no media type is reported as that read.
/// The first attachment with a defect, in the order the request lists them, is the one
/// reported.
#[must_use]
pub fn attachments_defect(attachments: &[Attachment]) -> Option<String> {
    for attachment in attachments {
        if !is_attachable_file_name(&attachment.file_name) {
            return Some(illegal_file_name(&attachment.file_name));
        }
        if let Some(content_type) = attachment.content_type.as_deref() {
            if let Some(defect) = content_type_defect(content_type) {
                return Some(defect);
            }
        }
    }
    None
}

/// What is wrong with a media type, in the words SES's media-type reader reports it with, or
/// `None` for a value that is one.
///
/// A media type is a primary type and a subtype separated by `/`: the reader walks the
/// primary type, then expects that `/`, and reports what it found where it expected it —
/// `null` for a value that simply ended there.
fn content_type_defect(content_type: &str) -> Option<String> {
    let Some((_, subtype)) = content_type.split_once('/') else {
        return Some(format!(
            "In Content-Type string <{content_type}>, expected '/', got null"
        ));
    };
    subtype.trim().is_empty().then(|| {
        format!("In Content-Type string <{content_type}>, expected MIME subtype, got null")
    })
}

/// Whether SES carries a file under the name `file_name`: a name that names a file, whose
/// extension is not one of the types SES does not carry
/// ([`UNSUPPORTED_ATTACHMENT_EXTENSIONS`]).
fn is_attachable_file_name(file_name: &str) -> bool {
    let name = file_name.trim();
    if name.is_empty() {
        return false;
    }
    match file_extension(name) {
        None => true,
        Some(extension) => !UNSUPPORTED_ATTACHMENT_EXTENSIONS
            .iter()
            .any(|unsupported| unsupported.eq_ignore_ascii_case(&extension)),
    }
}

/// The extension of `file_name`, without its dot, or `None` for a name that carries none.
fn file_extension(file_name: &str) -> Option<String> {
    file_name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_string())
        .filter(|extension| !extension.is_empty())
}

/// How SES refuses an attachment it will not carry under the name the request gave it: the
/// fixed text `Illegal filename '` and `'.`, with the `Attachment$FileName` the request
/// spelled as its one variable part.
#[must_use]
pub fn illegal_file_name(file_name: &str) -> String {
    format!("Illegal filename '{file_name}'.")
}

/// The attachment file extensions Amazon SES does not carry, from the unsupported attachment
/// types the Amazon SES Developer Guide lists: the executable, script and installer types a
/// mail stack refuses to hand on.
///
/// Compared without regard to case ([`attachments_defect`]), since a file name's extension
/// names the same type in any case.
const UNSUPPORTED_ATTACHMENT_EXTENSIONS: &[&str] = &[
    "ade", "adp", "app", "asp", "bas", "bat", "cer", "chm", "cmd", "com", "cpl", "crt", "csh",
    "der", "exe", "fxp", "gadget", "hlp", "hta", "inf", "ins", "isp", "its", "jar", "js", "jse",
    "ksh", "lib", "lnk", "mad", "maf", "mag", "mam", "maq", "mar", "mas", "mat", "mau", "mav",
    "maw", "mda", "mdb", "mde", "mdt", "mdw", "mdz", "msc", "msi", "msp", "mst", "nsh", "pif",
    "pl", "prf", "prg", "ps1", "ps2", "reg", "scf", "scr", "sct", "shb", "shs", "sys", "url",
    "vb", "vbe", "vbs", "vps", "vsmacros", "vss", "vst", "vsw", "vxd", "ws", "wsc", "wsf", "wsh",
    "xnk",
];
