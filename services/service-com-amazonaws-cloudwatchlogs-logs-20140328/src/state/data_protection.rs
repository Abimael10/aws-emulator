// SPDX-License-Identifier: CC0-1.0

//! Masking the sensitive data a log group's DATA PROTECTION POLICY names, at the moment an
//! event is ingested.
//!
//! `PutDataProtectionPolicy` states the rule this module implements: "Sensitive data is
//! detected and masked when it is ingested into the log group. When you set a data
//! protection policy, log events ingested into the log group before that time are not
//! masked. By default, when a user views a log event that includes masked data, the
//! sensitive data is replaced by asterisks. A user who has the logs:Unmask permission can
//! use a GetLogEvents or FilterLogEvents operation with the unmask parameter set to true to
//! view the unmasked log events."
//!
//! So masking is decided ONCE, as the event is stored: the event keeps the bytes the request
//! carried — which is what `unmask` reveals — beside the version a reader sees by default
//! ([`super::StoredEvent::masked_message`]), and an event stored before the policy was put
//! carries none because the policy was not in force when it arrived.
//!
//! What a mask replaces is the MATCHED SPAN and nothing else: the characters of the
//! sensitive term become asterisks and the rest of the message is the ingested text, which
//! is what "the sensitive data is replaced by asterisks" says.
//!
//! DETECTION is one step and happens once per ingested message ([`detected`]): a policy's two
//! blocks are an `Audit` action and a `Deidentify` action over the SAME `DataIdentifier` array
//! ([`super::check_data_protection_policy_document`]), so the sensitive data found in a
//! message is what the Deidentify half masks ([`masked_where`]) AND what the Audit half
//! records as a finding ([`super::audit_findings`]). Neither half can therefore find anything
//! the other does not.
//!
//! The policy in force for a group is the group's own TOGETHER WITH the account-level data
//! protection policy that applies to it, because `PutAccountPolicy` states the two are
//! cumulative ([`super::ComAmazonawsCloudwatchlogsLogs20140328State::data_protection_identifiers_for`]).
//!
//! ## Which reads report the masked version
//!
//! The two the sentence above names, `GetLogEvents` and `FilterLogEvents`, through
//! [`super::StoredEvent::message_as_read`]. This service's other event reads — `GetLogRecord`,
//! `GetLogObject` and a Logs Insights query's `unmask` command — deliberately report the
//! ingested message: each reads through a path of its own, and bringing the masked rendering
//! to it belongs with that path rather than here.
//!
//! ## Which terms are detected
//!
//! A policy's `DataIdentifier` entries are the managed data identifiers AWS publishes, each
//! named by an ARN of the form `arn:aws:dataprotection::aws:data-identifier/<name>`
//! (`PutDataProtectionPolicy$policyDocument`: "The `DataIdentifer` array lists the types of
//! sensitive data that you want to mask"). [`DETECTED_IDENTIFIERS`] is the set of them this
//! emulator DETECTS, each with the form its own documentation publishes for the data; an
//! identifier outside that set names a kind of data this emulator does not detect, so a
//! policy that lists only such identifiers masks nothing — the policy is still stored and
//! reported back, because which identifiers exist is not this service's to judge
//! ([`super::check_data_protection_policy_document`]).

use std::sync::OnceLock;

use regex::Regex;

/// The ARN form a managed data identifier is named by:
/// `arn:aws:dataprotection::aws:data-identifier/<name>`, as a policy document's
/// `DataIdentifier` entries carry it. A policy may also name an identifier by its bare name,
/// so the prefix is stripped where it is there.
const MANAGED_DATA_IDENTIFIER_PREFIX: &str = "arn:aws:dataprotection::aws:data-identifier/";

/// One managed data identifier this emulator detects: the name AWS publishes it under, the
/// form of the data it matches, and whatever else makes a match one.
pub struct DetectedIdentifier {
    /// The identifier's name, the last segment of the ARN a policy names it by
    /// (`EmailAddress`).
    pub name: &'static str,
    /// The regular expression the data of that kind has, as the identifier's own
    /// documentation describes it.
    pub pattern: &'static str,
    /// What a matched span must also satisfy to be data of that kind, for an identifier
    /// whose own documentation states a check beyond the form — `None` where the form is the
    /// whole of it.
    pub check: Option<fn(&str) -> bool>,
}

/// The managed data identifiers this emulator detects, in the order they are tried.
///
/// Source: the managed data identifiers AWS publishes for a data protection policy's
/// `DataIdentifier` array ("Types of data that you can mask"), each pattern written for the
/// form that documentation gives the data:
///
/// * `EmailAddress` — a local part, `@`, and a domain with at least one dot;
/// * `IpAddress` — an IPv4 address in dotted-quad form;
/// * `CreditCardNumber` — 13 to 19 digits, written in groups separated by a space or a
///   hyphen or in one run, that pass the Luhn check the card-number standard defines (so a
///   bare run of digits that is no card number — a timestamp in a message — is not one).
///
/// An identifier outside this set names a kind of data this emulator does not detect; see
/// the module docs.
pub const DETECTED_IDENTIFIERS: &[DetectedIdentifier] = &[
    DetectedIdentifier {
        name: "EmailAddress",
        pattern: r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}",
        check: None,
    },
    DetectedIdentifier {
        name: "IpAddress",
        pattern: r"\b(?:[0-9]{1,3}\.){3}[0-9]{1,3}\b",
        check: None,
    },
    DetectedIdentifier {
        name: "CreditCardNumber",
        pattern: r"\b[0-9]{4}(?:[ \-]?[0-9]{4}){2,3}[0-9]{0,3}\b",
        check: Some(passes_luhn),
    },
];

/// Whether a candidate card number passes the Luhn check — the checksum a payment card
/// number carries, which is what separates one from any other run of digits of its length.
///
/// The separators a number may be written with are not part of the digits it checks.
fn passes_luhn(candidate: &str) -> bool {
    let digits: Vec<u32> = candidate.chars().filter_map(|c| c.to_digit(10)).collect();
    if !(13..=19).contains(&digits.len()) {
        return false;
    }
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(position, digit)| {
            if position % 2 == 0 {
                *digit
            } else {
                let doubled = digit * 2;
                if doubled > 9 {
                    doubled - 9
                } else {
                    doubled
                }
            }
        })
        .sum();
    sum.is_multiple_of(10)
}

/// The character a masked span is written with: "the sensitive data is replaced by
/// asterisks".
const MASK_CHARACTER: char = '*';

/// The compiled expressions of [`DETECTED_IDENTIFIERS`], in the same order.
///
/// Compiled once for the life of the process: every pattern above is a literal of this
/// crate, so compiling them cannot fail, and an ingestion must not pay for compiling one per
/// event.
fn compiled() -> &'static [Regex] {
    static COMPILED: OnceLock<Vec<Regex>> = OnceLock::new();
    COMPILED.get_or_init(|| {
        DETECTED_IDENTIFIERS
            .iter()
            .map(|identifier| {
                Regex::new(identifier.pattern).expect("a data identifier's own expression")
            })
            .collect()
    })
}

/// The name one policy entry names an identifier by: the last segment of a managed data
/// identifier's ARN, or the entry itself where it is written as the bare name.
fn identifier_name(entry: &str) -> &str {
    entry
        .strip_prefix(MANAGED_DATA_IDENTIFIER_PREFIX)
        .unwrap_or(entry)
}

/// What a match of one selected identifier is judged by: the identifier itself, the
/// expression it matched, and whatever else the identifier requires of a match
/// ([`DetectedIdentifier::check`]).
type SelectedIdentifier = (
    &'static DetectedIdentifier,
    &'static Regex,
    Option<fn(&str) -> bool>,
);

/// The expressions one policy's `DataIdentifier` entries select, for the identifiers this
/// emulator detects, each with whatever else a match of it must satisfy.
fn selected(identifiers: &[String]) -> Vec<SelectedIdentifier> {
    let compiled = compiled();
    DETECTED_IDENTIFIERS
        .iter()
        .zip(compiled)
        .filter(|(identifier, _)| {
            identifiers
                .iter()
                .any(|entry| identifier_name(entry) == identifier.name)
        })
        .map(|(identifier, expression)| (identifier, expression, identifier.check))
        .collect()
}

/// WHAT one message was found to carry, for one data identifier a policy named: the
/// identifier's own name and the spans of the message its data was found in.
///
/// The matched TEXT is deliberately not carried: the two halves a data protection policy acts
/// through — masking the stored event and recording the detection — both need to know WHERE
/// the sensitive data is and WHICH kind it is, and neither needs the value itself, so nothing
/// downstream of a detection can leak one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detected {
    /// The identifier's name, as AWS publishes it (`EmailAddress`).
    pub name: &'static str,
    /// The CHARACTER spans of the message the identifier matched, as `(start, end)` with the
    /// end exclusive, in the order they were found.
    pub spans: Vec<(usize, usize)>,
}

/// The sensitive data the policy naming `identifiers` finds in `message`: one entry per named
/// identifier that matched, in the order [`DETECTED_IDENTIFIERS`] tries them.
///
/// The ONE scan of a message a data protection policy performs: an ingestion detects once and
/// both halves of the policy act on what this answered — the Deidentify half masks the spans
/// ([`masked`]) and the Audit half records which identifiers were seen
/// ([`super::audit_findings`]) — so the two can never disagree about what was found.
///
/// Empty where the policy finds nothing: it names no identifier this emulator detects, or the
/// message carries none of the data they describe.
#[must_use]
pub fn detected(message: &str, identifiers: &[String]) -> Vec<Detected> {
    let expressions = selected(identifiers);
    if expressions.is_empty() {
        return Vec::new();
    }
    // The byte offset of each character, so a match's byte range selects the characters it
    // covers — a span is reported, and masked, in characters.
    let offsets: Vec<usize> = message.char_indices().map(|(offset, _)| offset).collect();
    let mut found = Vec::new();
    for (identifier, expression, check) in expressions {
        let mut spans = Vec::new();
        for matched in expression.find_iter(message) {
            if check.is_some_and(|check| !check(matched.as_str())) {
                continue;
            }
            let first = offsets.partition_point(|offset| *offset < matched.start());
            let last = offsets.partition_point(|offset| *offset < matched.end());
            spans.push((first, last));
        }
        if !spans.is_empty() {
            found.push(Detected {
                name: identifier.name,
                spans,
            });
        }
    }
    found
}

/// The message as a reader sees it by DEFAULT once the policy naming `identifiers` is in
/// force: every matched span replaced by asterisks, the rest of the message verbatim.
///
/// `None` where the policy masks nothing of this message, which is what says the event needs
/// no masked version stored beside the ingested bytes.
pub fn masked(message: &str, identifiers: &[String]) -> Option<String> {
    masked_where(message, &detected(message, identifiers))
}

/// [`masked`] over a detection already taken: the spans `found` names replaced by asterisks,
/// so an ingestion that has to record the detection as well scans the message once.
///
/// Two identifiers matching overlapping text mask the union of what they found, since every
/// span is written over the same rendering.
#[must_use]
pub fn masked_where(message: &str, found: &[Detected]) -> Option<String> {
    if found.is_empty() {
        return None;
    }
    let mut masked: Vec<char> = message.chars().collect();
    for detected in found {
        for (first, last) in &detected.spans {
            for character in masked.iter_mut().take(*last).skip(*first) {
                *character = MASK_CHARACTER;
            }
        }
    }
    Some(masked.iter().collect())
}

/// The data identifiers a log group's OWN stored data protection policy names, or the empty
/// list for a group that holds no policy or one whose document states none.
///
/// Read off the STORED document, which is the one in force: the shape of a document is what
/// [`super::check_data_protection_policy_document`] answers, and a document that is in the
/// store was accepted by it. A group may ALSO be covered by an account-level policy, whose
/// identifiers are cumulative with these
/// ([`super::ComAmazonawsCloudwatchlogsLogs20140328State::data_protection_identifiers_for`]).
pub fn identifiers_of(group: &super::LogGroupRecord) -> Vec<String> {
    group
        .data_protection_policy
        .as_ref()
        .map(|record| identifiers_in(&record.policy_document))
        .unwrap_or_default()
}

/// The data identifiers one accepted policy document names.
fn identifiers_in(document: &str) -> Vec<String> {
    super::check_data_protection_policy_document(document)
        .map(|policy| policy.data_identifiers)
        .unwrap_or_default()
}

impl super::ComAmazonawsCloudwatchlogsLogs20140328State {
    /// The data identifiers in force for an event ingested into `group`: the ones the
    /// group's own data protection policy names TOGETHER WITH the ones the account-level
    /// data protection policy that applies to the group names.
    ///
    /// Both, because `PutAccountPolicy` states how the two resolve: "Data protection — The
    /// two policies are cumulative. Any sensitive term specified in either the account-level
    /// or the log-group-level policy is masked." Which account-level policy applies to a
    /// group is that question's one answer ([`super::applicable_account_policy`]).
    pub fn data_protection_identifiers_for(&self, group: &super::LogGroupRecord) -> Vec<String> {
        let mut identifiers = identifiers_of(group);
        let account_level = super::applicable_account_policy(
            &self.account_policies,
            super::POLICY_TYPE_DATA_PROTECTION,
            group,
        );
        if let Some(policy) = account_level {
            for identifier in identifiers_in(&policy.policy_document) {
                if !identifiers.contains(&identifier) {
                    identifiers.push(identifier);
                }
            }
        }
        identifiers
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every identifier this emulator detects has an expression that compiles and finds the
    /// form its own documentation publishes, and the mask replaces exactly the matched span.
    #[test]
    fn each_detected_identifier_masks_the_span_it_matches() {
        let cases: &[(&str, &str, &str, &str)] = &[
            (
                "EmailAddress",
                "EmailAddress",
                "signup from ada@example.com accepted",
                "signup from *************** accepted",
            ),
            (
                "IpAddress",
                "IpAddress",
                "request from 198.51.100.7 served",
                "request from ************ served",
            ),
            (
                "CreditCardNumber",
                "CreditCardNumber",
                "card 4111 1111 1111 1111 charged",
                "card ******************* charged",
            ),
            (
                "CreditCardNumber written as one run of digits",
                "CreditCardNumber",
                "card 4111111111111111 charged",
                "card **************** charged",
            ),
        ];
        for (case, identifier, message, expected) in cases {
            let arn = format!("{MANAGED_DATA_IDENTIFIER_PREFIX}{identifier}");
            let actual = masked(message, &[arn]);
            assert_eq!(
                actual.as_deref(),
                Some(*expected),
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// A policy naming an identifier this emulator does not detect masks nothing, and so does
    /// one whose identifiers find none of their data in the message.
    #[test]
    fn nothing_is_masked_without_a_match() {
        let cases: &[(&str, &[&str], &str)] = &[
            (
                "an identifier this emulator does not detect",
                &["arn:aws:dataprotection::aws:data-identifier/DriversLicense-US"],
                "signup from ada@example.com accepted",
            ),
            (
                "no identifier at all",
                &[],
                "signup from ada@example.com accepted",
            ),
            (
                "a message carrying none of the data",
                &["arn:aws:dataprotection::aws:data-identifier/EmailAddress"],
                "a benign message",
            ),
            // A card number carries the Luhn checksum, so a run of digits that is not one —
            // the epoch-millisecond instant a message states — is not data of that kind.
            (
                "a run of digits that is no card number",
                &["arn:aws:dataprotection::aws:data-identifier/CreditCardNumber"],
                "handled at 1767225600123 in 4 ms",
            ),
        ];
        for (case, identifiers, message) in cases {
            let listed: Vec<String> = identifiers.iter().map(|entry| entry.to_string()).collect();
            let actual = masked(message, &listed);
            assert_eq!(
                actual, None,
                "case {case:?}: expected nothing masked, got {actual:?}"
            );
        }
    }

    /// A message carrying MORE than one term of a named identifier has every one of them
    /// masked, and a policy naming several identifiers masks what each of them finds.
    #[test]
    fn every_matched_term_is_masked() {
        let identifiers = vec![
            "arn:aws:dataprotection::aws:data-identifier/EmailAddress".to_string(),
            "IpAddress".to_string(),
        ];
        let message = "ada@example.com and bob@example.org from 203.0.113.9";
        let actual = masked(message, &identifiers);
        assert_eq!(
            actual.as_deref(),
            Some("*************** and *************** from ***********"),
            "case \"two addresses and an IP\": got {actual:?}"
        );
    }

    /// One row of the detection table: what a failing assertion calls it, the identifiers the
    /// policy names, the message scanned, and the `(identifier, spans)` pairs expected.
    type DetectionRow = (
        &'static str,
        &'static [&'static str],
        &'static str,
        &'static [(&'static str, &'static [(usize, usize)])],
    );

    /// RULE: one scan reports WHICH identifiers were found and WHERE, and the masked
    /// rendering is exactly those spans — so the detection the Audit half records and the
    /// masking the Deidentify half applies describe the same findings.
    #[test]
    fn a_detection_names_the_identifiers_found_and_their_spans() {
        // (case, the identifiers the policy names, the message, the detections expected)
        let rows: &[DetectionRow] = &[
            (
                "one address in the middle of the message",
                &["EmailAddress"],
                "signup from ada@example.com accepted",
                &[("EmailAddress", &[(12, 27)])],
            ),
            (
                "two terms of one identifier",
                &["EmailAddress"],
                "ada@example.com invited bob@example.org",
                &[("EmailAddress", &[(0, 15), (24, 39)])],
            ),
            (
                "two identifiers, each found once",
                &["EmailAddress", "IpAddress"],
                "ada@example.com from 203.0.113.9",
                &[("EmailAddress", &[(0, 15)]), ("IpAddress", &[(21, 32)])],
            ),
            (
                "a term after a multi-byte character, counted in characters",
                &["EmailAddress"],
                "café ada@example.com",
                &[("EmailAddress", &[(5, 20)])],
            ),
            (
                "an identifier named but not found",
                &["IpAddress"],
                "signup from ada@example.com accepted",
                &[],
            ),
            (
                "an identifier this emulator does not detect",
                &["DriversLicense-US"],
                "signup from ada@example.com accepted",
                &[],
            ),
        ];
        for (case, identifiers, message, expected) in rows {
            let listed: Vec<String> = identifiers
                .iter()
                .map(|name| format!("{MANAGED_DATA_IDENTIFIER_PREFIX}{name}"))
                .collect();
            let found = detected(message, &listed);
            let reported: Vec<(&str, Vec<(usize, usize)>)> = found
                .iter()
                .map(|entry| (entry.name, entry.spans.clone()))
                .collect();
            let wanted: Vec<(&str, Vec<(usize, usize)>)> = expected
                .iter()
                .map(|(name, spans)| (*name, spans.to_vec()))
                .collect();
            assert_eq!(
                reported, wanted,
                "case {case:?}: expected the detections {wanted:?}, got {reported:?}"
            );
            // The masked rendering is those spans and nothing else: every reported span is
            // asterisks, and the message is otherwise unchanged.
            let rendered = masked_where(message, &found);
            match rendered {
                None => assert!(
                    found.is_empty(),
                    "case {case:?}: a detection must have a masked rendering, got none"
                ),
                Some(rendered) => {
                    let characters: Vec<char> = rendered.chars().collect();
                    let original: Vec<char> = message.chars().collect();
                    for (index, character) in characters.iter().enumerate() {
                        let inside = found.iter().any(|entry| {
                            entry
                                .spans
                                .iter()
                                .any(|(first, last)| (*first..*last).contains(&index))
                        });
                        let expected = if inside { '*' } else { original[index] };
                        assert_eq!(
                            *character, expected,
                            "case {case:?}: at character {index} expected {expected:?}, got \
                             {character:?} in {rendered:?}"
                        );
                    }
                }
            }
        }
    }
}
