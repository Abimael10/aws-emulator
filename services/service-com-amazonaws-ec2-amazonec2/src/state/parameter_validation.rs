// SPDX-License-Identifier: CC0-1.0

//! EC2's request-PARAMETER validation refusal: what a request is answered when a member's
//! value is outside the FORM that parameter takes, decided before any resource is looked up.
//!
//! Some of this service's members are read by the request-parameter layer rather than by the
//! operation: that layer knows only the parameter's own name and the text the request sent
//! under it, so it refuses the VALUE (HTTP 400 [`INVALID_PARAMETER_VALUE`]) with a clause
//! that names the parameter's path and the value at fault, and it does so before the
//! operation runs — so a value outside the form is never answered as a resource that is
//! merely absent. A caller that mistyped an id learns the id is unreadable instead of
//! polling for a resource that could never appear.
//!
//! The wording is that layer's, not one operation's: the envelope
//! ([`ONE_PARAMETER_VALIDATION_ERROR`]) carries one clause per faulty parameter, and a
//! clause names the parameter the way the layer paths it ([`parameter_path`],
//! [`parameter_entry_path`]) and renders the value the way the layer prints it
//! ([`rendered_parameter_list`] for a list parameter). Every refusal of this class in the
//! crate is written here, so two operations cannot word one class two ways.
//!
//! Split out of the [`crate::state`] module root so each file holds one concern, and
//! re-exported from it, so a handler reaches these as `crate::state::<item>` like every
//! other primitive there.

use super::*;

/// The envelope the parameter layer wraps the clauses of ONE refusal in, for a request with
/// exactly one faulty parameter: the count of clauses, then the clauses themselves.
///
/// This crate refuses the FIRST parameter it finds at fault rather than walking the whole
/// request, so every refusal of this class carries exactly one clause and this is the whole
/// envelope.
pub const ONE_PARAMETER_VALIDATION_ERROR: &str = "1 validation error detected: ";

/// The path the parameter layer names a member by: the member's WIRE name — the name the
/// request sends it under — with a lower-case initial, as the recorded clauses of this class
/// spell it (`flowLogId` for the `FlowLogId.N` list of `DeleteFlowLogs`, whose model member
/// `FlowLogIds` carries the wire name `FlowLogId`).
///
/// The wire name is what the layer has: it reads the request's own parameters, not the
/// model's member names, so a member whose wire name differs from its model name is pathed
/// by the wire name.
pub fn parameter_path(wire_name: &str) -> String {
    let mut characters = wire_name.chars();
    match characters.next() {
        None => String::new(),
        Some(first) => first.to_lowercase().chain(characters).collect(),
    }
}

/// The path that layer names ONE ENTRY of a list parameter by, down to an entry's own
/// member: the list's [`parameter_path`], the entry's position counting from 1, the fixed
/// `member` segment the layer prints for a list element, and the entry member's own path —
/// `filter.2.member.name` for the `Name` of the second `Filter` a request sent.
///
/// `index` is a position in the list the REQUEST sent, counted from 1 the way the request's
/// own parameters are numbered, so the clause names the entry the caller has to fix.
pub fn parameter_entry_path(wire_name: &str, index: usize, entry_wire_name: &str) -> String {
    format!(
        "{list}.{index}.member.{entry}",
        list = parameter_path(wire_name),
        entry = parameter_path(entry_wire_name),
    )
}

/// How the parameter layer prints the value of a LIST parameter in a clause: the entries it
/// received, comma-separated, inside brackets — `[fl-02a1, fl-bogus]`.
///
/// The layer refuses the PARAMETER, so the clause carries the value the parameter holds,
/// which for a list is every entry the request sent and not only the entry at fault. Each
/// entry is bounded the way every refusal of this crate bounds a value the caller chose the
/// length of ([`echoed_resource_id`]).
pub fn rendered_parameter_list(values: &[String]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| echoed_resource_id(value))
            .collect::<Vec<String>>()
            .join(", "),
    )
}

/// Refuse a parameter whose value is outside the form that parameter takes: HTTP 400
/// [`INVALID_PARAMETER_VALUE`], with the one clause the parameter layer words for it —
/// the value in single quotes, the parameter's path, and the fixed `has an invalid format`.
///
/// `path` is the parameter's own path ([`parameter_path`] or [`parameter_entry_path`]) and
/// `value` is what the request sent, rendered the way the layer prints that parameter's
/// value (a list through [`rendered_parameter_list`], a plain member as itself). The echo is
/// held to [`MAX_ECHOED_RESOURCE_ID_CHARS`] per value, so no refusal reflects a request's
/// bytes back in full.
pub fn invalid_parameter_format<E>(path: &str, value: &str) -> Scaffolded<E> {
    invalid_parameter_value(parameter_format_violation_message(path, value))
}

/// The sentence [`invalid_parameter_format`] carries, for the one caller that words a refusal
/// without building the error channel: the service-wide seam that states EC2's own code and
/// sentence for a refusal the FRAMEWORK found against a rule the MODEL declares
/// ([`crate::service`]). The ONE template, so the refusal a handler gives and the refusal the
/// boundary gives for the same class cannot be worded two ways.
pub fn parameter_format_violation_message(path: &str, value: &str) -> String {
    format!("{ONE_PARAMETER_VALIDATION_ERROR}Value '{value}' at '{path}' has an invalid format")
}

/// The sentence that layer refuses a parameter whose value is outside the PATTERN the
/// parameter's own validator holds it to: the same envelope and the same `Value '<value>' at
/// '<path>'` opening as [`parameter_format_violation_message`], closed by the fixed
/// `failed to satisfy constraint: Member must satisfy regular expression pattern: ` and the
/// pattern itself.
///
/// The clause AWS words when a member is checked against a published regular expression
/// rather than merely read for its form, so the caller is told WHICH pattern the value has to
/// satisfy. `pattern` is the pattern that validator publishes, named by the member's own
/// module beside the reader that enforces it; `value` is what the request sent, bounded the
/// way every refusal of this crate bounds a value the caller chose the length of.
pub fn parameter_pattern_violation_message(path: &str, value: &str, pattern: &str) -> String {
    format!(
        "{ONE_PARAMETER_VALIDATION_ERROR}Value '{value}' at '{path}' failed to satisfy \
         constraint: Member must satisfy regular expression pattern: {pattern}",
        value = echoed_resource_id(value),
    )
}

/// Is `allowed` one of the value sets the VPC flow-log surface's parameters take?
///
/// The flow-log operations are the ones whose members this service has recorded the parameter
/// layer validating — a `Filter` entry's name and a `FlowLogId` are each refused through it —
/// so a value outside one of THEIR catalogs is refused by that same layer rather than
/// reported as a model-constraint violation. Keyed on the TYPE's value set, the way every
/// catalog-keyed refusal of this crate is ([`is_modelled_catalog`] records that rule): the
/// traffic a flow log captures (`crate::ops::types::TRAFFIC_TYPE_VALUES`), where it publishes
/// (`LOG_DESTINATION_TYPE_VALUES`), what it monitors (`FLOW_LOGS_RESOURCE_TYPE_VALUES`) and
/// the file format it is delivered in (`DESTINATION_FILE_FORMAT_VALUES`) — none of which any
/// other operation of this model carries.
pub fn is_flow_log_parameter_catalog(allowed: &[String]) -> bool {
    [
        crate::ops::types::TRAFFIC_TYPE_VALUES,
        crate::ops::types::LOG_DESTINATION_TYPE_VALUES,
        crate::ops::types::FLOW_LOGS_RESOURCE_TYPE_VALUES,
        crate::ops::types::DESTINATION_FILE_FORMAT_VALUES,
    ]
    .iter()
    .any(|catalog| is_modelled_catalog(allowed, catalog))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A path is the wire name with a lower-case initial, whatever case the wire name is in.
    #[test]
    fn a_parameter_path_lower_cases_the_wire_name_s_initial() {
        // (case, wire name, the path the layer names it by)
        let cases: [(&str, &str, &str); 5] = [
            ("one word", "Filter", "filter"),
            ("a compound name", "FlowLogId", "flowLogId"),
            ("already lower case", "nextToken", "nextToken"),
            ("a single letter", "N", "n"),
            ("nothing at all", "", ""),
        ];
        for (case, wire_name, expected) in cases {
            let actual = parameter_path(wire_name);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the path {expected:?}, got {actual:?}"
            );
        }
    }

    /// An entry's path carries the list, its 1-based position and the entry's own member.
    #[test]
    fn an_entry_path_names_the_position_the_request_sent_it_at() {
        let actual = parameter_entry_path("Filter", 2, "Name");
        assert_eq!(
            actual, "filter.2.member.name",
            "expected the second filter's name, got {actual:?}"
        );
    }

    /// A list parameter's value prints as its entries in brackets, each bounded.
    #[test]
    fn a_list_parameter_prints_every_entry_it_received() {
        let one = rendered_parameter_list(&["fl-bogus".to_string()]);
        assert_eq!(one, "[fl-bogus]", "one entry: got {one:?}");
        let two = rendered_parameter_list(&["fl-bogus".to_string(), "fl-0".to_string()]);
        assert_eq!(two, "[fl-bogus, fl-0]", "two entries: got {two:?}");
        let long = "x".repeat(MAX_ECHOED_RESOURCE_ID_CHARS + 50);
        let bounded = rendered_parameter_list(&[long]);
        assert_eq!(
            bounded.chars().count(),
            MAX_ECHOED_RESOURCE_ID_CHARS + 2,
            "an over-long entry must be bounded, got {bounded:?}"
        );
    }
}
