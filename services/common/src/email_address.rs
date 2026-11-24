// SPDX-License-Identifier: CC0-1.0

//! The form a service reads a CALLER-SUPPLIED email address against.
//!
//! Several services bind a member their model DOCUMENTS as an email address — the
//! address a budget notification is sent to, the endpoint a topic subscription
//! delivers to, the identity a mail service sends from — and the models target those
//! members at a bare string, or at a pattern far too wide to state the form, so the
//! boundary validator admits any text at all and reading the form is the handler's
//! job. It has to be read: a value that is not an address names no mailbox, so such a
//! request is a parameter violation (attributed to the member) rather than a resource
//! that happens to be missing. The one reading lives here so no two services disagree
//! about what an email address is.
//!
//! The form is the one those members document,
//! `UserName@[SubDomain.]Domain.TopLevelDomain`: a local part, a single `@`, and a
//! domain of at least two labels ([`is_domain_name`]). The local part is a dot-atom —
//! one or more atoms of the RFC 5322 atom characters separated by single `.`s — so a
//! leading, trailing or doubled dot makes an EMPTY atom and names no mailbox, exactly
//! as an empty label is not a domain label.
//!
//! The WORDING of a refusal stays with the service, which owns its error envelope:
//! this module answers whether the value is an address ([`is_email_address`]) and, where a
//! service reports WHICH way a value misses the form, which class of defect it has
//! ([`address_defect`]) — never the sentence that states it.
//!
//! `service-com-amazonaws-sns-amazonsimplenotificationservice/src/ops/subscribe.rs` and
//! `service-com-amazonaws-lightsail-lightsail-20161128/src/state.rs` still carry copies
//! of this form, each inlined before this module existed and pinned by that service's own
//! refusal suite; they are to be migrated onto this one rather than copied again, as
//! `service-com-amazonaws-ses-simpleemailservice/src/state.rs` already has been.
//!
//! One service deliberately reads a DIFFERENT form, and is not a copy to migrate:
//! `service-com-amazonaws-account-account` has an email-address `@pattern` in its OWN
//! model, declared on one of its two address members and enforced there by the boundary
//! validator, so it reads the other member against that declared expression and words
//! both refusals with it. That is what keeps one service's two members one rule; a
//! service whose model states no such expression reads against this module instead.

/// The punctuation an ATOM of a local part may carry, beside the ASCII letters and
/// digits: the RFC 5322 atom characters. The `.` that SEPARATES atoms is not one of
/// them — it is what [`is_email_address`] splits a local part on.
const ATOM_PUNCTUATION: &str = "!#$%&'*+-/=?^_`{|}~";

/// RFC 5322's specials, which carry STRUCTURE in an address rather than being part of a
/// mailbox name or a domain name. The `.` and the `@` are not among them: each separates the
/// parts of an address rather than being illegal inside one.
const SPECIALS: &str = "()<>,;:\\\"[]";

/// WHAT is wrong with a value that is not an email address.
///
/// One class per way the form above is missed, so a service can report the defect it found
/// rather than only that the value is not an address. The WORDING stays with the service
/// (see the module header): a mail service words these as its own mail stack's reader does,
/// another service may word them as parameter violations, and this enum is what keeps the
/// two reading one form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressDefect {
    /// The value carries no `@` at all, so it names no domain for the mailbox.
    MissingDomainSeparator,
    /// Nothing precedes the `@`: the value names no mailbox in the domain.
    MissingLocalPart,
    /// Nothing follows the `@`: the value names no domain for the mailbox.
    MissingDomain,
    /// The local part carries a character outside printable 7-bit ASCII — a control
    /// character, a space, or a character no 7-bit address may carry.
    LocalControlOrWhitespace,
    /// The local part carries one of RFC 5322's [`SPECIALS`].
    LocalIllegalCharacter,
    /// The local part begins with the atom separator, so its first atom is empty.
    LocalStartsWithDot,
    /// The local part ends with it, so its last atom is empty.
    LocalEndsWithDot,
    /// The local part has an empty atom inside it.
    LocalDotDot,
    /// The domain carries a character outside printable 7-bit ASCII.
    DomainControlOrWhitespace,
    /// The domain carries a character no domain label may: one of RFC 5322's
    /// [`SPECIALS`], or any other printable character that is neither a letter, a digit
    /// nor a hyphen.
    DomainIllegalCharacter,
    /// The domain has an empty label inside it.
    DomainDotDot,
    /// The domain is no domain NAME: a single label, a leading or trailing separator, or a
    /// top-level label carrying no letter ([`is_domain_name`]).
    DomainIsNotAName,
}

/// Whether `value` is an email address: a dot-atom local part, a single `@`, and a
/// domain name ([`is_domain_name`]).
///
/// A value carrying a second `@` is not an address either: everything after the first
/// one is read as the domain, and `@` is not a character a domain label may carry.
/// Nothing in an address is whitespace, since neither an atom nor a label admits any.
#[must_use]
pub fn is_email_address(value: &str) -> bool {
    address_defect(value).is_none()
}

/// WHAT is wrong with `value`, or `None` when it is an email address
/// ([`is_email_address`]).
///
/// The halves are read in the order the form states them — the local part, then the domain
/// — and each half's own classes in the order a reader walks the characters, so the FIRST
/// defect of the value is the one reported. The last check of each half is the half's own
/// predicate, so a value this returns `None` for is an address under [`is_email_address`]
/// and nothing else can be.
#[must_use]
pub fn address_defect(value: &str) -> Option<AddressDefect> {
    let Some((local, domain)) = value.split_once('@') else {
        return Some(AddressDefect::MissingDomainSeparator);
    };
    local_part_defect(local).or_else(|| domain_defect(domain))
}

/// The defect in the LOCAL part of an address — the mailbox name before the `@`.
fn local_part_defect(local: &str) -> Option<AddressDefect> {
    if local.is_empty() {
        return Some(AddressDefect::MissingLocalPart);
    }
    if local.chars().any(is_outside_printable_ascii) {
        return Some(AddressDefect::LocalControlOrWhitespace);
    }
    if local.chars().any(|character| SPECIALS.contains(character)) {
        return Some(AddressDefect::LocalIllegalCharacter);
    }
    if local.starts_with('.') {
        return Some(AddressDefect::LocalStartsWithDot);
    }
    if local.ends_with('.') {
        return Some(AddressDefect::LocalEndsWithDot);
    }
    if local.contains("..") {
        return Some(AddressDefect::LocalDotDot);
    }
    (!is_dot_atom(local)).then_some(AddressDefect::LocalIllegalCharacter)
}

/// The defect in the DOMAIN of an address — the name after the `@`.
fn domain_defect(domain: &str) -> Option<AddressDefect> {
    if domain.is_empty() {
        return Some(AddressDefect::MissingDomain);
    }
    if domain.chars().any(is_outside_printable_ascii) {
        return Some(AddressDefect::DomainControlOrWhitespace);
    }
    if domain
        .chars()
        .any(|character| !character.is_ascii_alphanumeric() && character != '-' && character != '.')
    {
        return Some(AddressDefect::DomainIllegalCharacter);
    }
    if domain.contains("..") {
        return Some(AddressDefect::DomainDotDot);
    }
    (!is_domain_name(domain)).then_some(AddressDefect::DomainIsNotAName)
}

/// Whether `character` is outside printable 7-bit ASCII: a control character or a space
/// below the range, and everything above it — no address carries either, since the form
/// admits no non-ASCII character at all.
fn is_outside_printable_ascii(character: char) -> bool {
    character <= ' ' || character >= '\u{7f}'
}

/// Whether `value` is a domain name of at least two non-empty labels — the
/// `[SubDomain.]Domain.TopLevelDomain` half of the form.
///
/// A label is ASCII letters, digits and hyphens, and the top-level label carries at
/// least one letter, which admits an internationalised (`xn--…`) top-level domain and
/// refuses an all-numeric one (an address's domain is a name, not an address literal).
#[must_use]
pub fn is_domain_name(value: &str) -> bool {
    // A name with no `.` is a single label, which is no domain under this form.
    let Some((host, top_level)) = value.rsplit_once('.') else {
        return false;
    };
    host.split('.').all(is_domain_label)
        && is_domain_label(top_level)
        && top_level.chars().any(|character| character.is_ascii_alphabetic())
}

/// Whether `value` is ONE label of a domain name: non-empty, ASCII letters, digits and
/// hyphens.
fn is_domain_label(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
}

/// Whether `value` is a dot-atom: one or more non-empty atoms of the ASCII letters,
/// digits and [`ATOM_PUNCTUATION`], separated by single `.`s.
///
/// The empty string is not one: splitting it yields one empty atom, so a member that is
/// present but empty names no mailbox.
fn is_dot_atom(value: &str) -> bool {
    value.split('.').all(|atom| {
        !atom.is_empty()
            && atom.chars().all(|character| {
                character.is_ascii_alphanumeric() || ATOM_PUNCTUATION.contains(character)
            })
    })
}
