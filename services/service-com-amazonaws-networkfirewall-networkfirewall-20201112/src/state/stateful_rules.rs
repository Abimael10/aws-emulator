// SPDX-License-Identifier: CC0-1.0

//! The stateful rules a caller submits in Suricata flat format: what makes one a
//! rule this service can load, and how a rule group carrying one it cannot is
//! refused.
//!
//! `RulesSource$RulesString` is "stateful inspection criteria, provided in
//! Suricata compatible rules", "with one rule per line"
//! (`CreateRuleGroupRequest$Rules`), so what the service can act on is what the
//! engine can load:
//!
//! * the rule's ACTION is one the service implements — the modelled
//!   `StatefulAction` values ([`STATEFUL_ACTIONS`]), as a rule spells them;
//! * the rule ASSIGNS A SIGNATURE ID, the `sid` keyword Suricata identifies a
//!   signature by, since a rule that assigns none cannot be loaded at all;
//! * and no two rules of one rule group assign the SAME signature id, which would
//!   leave the engine unable to tell which signature a match belongs to.
//!
//! A rule group carrying such a rule is refused rather than retained: a stored
//! rule nothing can load would inspect no traffic while the rule group reports
//! itself active.
//!
//! The reading of the format is NOT hand-written: the rules are tokenized with
//! the `suricatax-rule-parser` crate's scanner, which breaks a rule into its
//! header tokens and its `name[:value]` options the way Suricata's own grammar
//! does — an option value runs to the next unescaped `;`, so an option whose
//! value swallows the text that was meant to be the next option (a missing
//! terminator) assigns nothing, which is one of the defects these checks catch.
//! The scanner reads the rule once, left to right, and allocates nothing per
//! rule beyond the tokens it hands back, so the work here is linear in the
//! submitted rules string — whose length `RulesString` itself bounds
//! (`@length` 0..2000000) — and so is the refusal it builds, which carries at
//! most one line per rule.

use ::std::collections::HashSet;

use ::suricatax_rule_parser::scanner::{RuleScanEvent, RuleScanner};

use crate::ops::types::RuleGroup;

/// The actions a stateful rule may take, as a rule in Suricata flat format
/// spells them: the modelled `StatefulAction` values
/// (`smithy_query(query="shape",shape="StatefulAction")`: `PASS`, `DROP`,
/// `ALERT`, `REJECT`) in the lower case a rule's action is written in.
const STATEFUL_ACTIONS: [&str; 4] = ["pass", "drop", "alert", "reject"];

/// The Suricata rule option that assigns a signature its id.
const SIGNATURE_ID_OPTION: &str = "sid";

/// Why a rule the engine could not load is refused: it assigns no signature id.
const SID_NOT_ASSIGNED: &str = "sid not assigned";

/// The Suricata rule option that assigns a rule's GENERATOR id — the id of what
/// produced the signature.
const GENERATOR_ID_OPTION: &str = "gid";

/// The generator id the service assigns the rules of a rule group a caller
/// defines.
///
/// FIXED text of the refusal, not a value from one run: it is the service's own,
/// not the caller's — a submitted rule carries no generator id, `RuleGroup` models
/// none, and the recorded refusals carry this same value for DIFFERENT rules of
/// different requests (the Suricata-syntax trajectory's successive attempts, whose
/// rules differ in their options and in their action, are each reported with it).
/// So there is nothing in the request or in the store to take it from, and nothing
/// per-run about it.
const GENERATOR_ID: &str = "1234567890";

/// Why `group`'s stateful rules in Suricata flat format are not rules this
/// service can load, or `None` when they are — including for a document that
/// carries no rules string at all, which these checks have nothing to say about.
pub fn breach(group: &RuleGroup) -> Option<String> {
    group
        .rules_source
        .rules_string
        .as_deref()
        .and_then(rules_string_breach)
}

/// Why the rules in `rules` — one per line ([`super::suricata_rule_lines`]) —
/// are not rules this service can load.
///
/// EVERY rule that cannot be loaded is reported, in the order the rules were
/// written, one refusal per line: the whole load is attempted and what it
/// answers is everything it could not load, so a client fixing a rules string
/// sees each offending rule rather than discovering them one request at a time.
/// A rule's own defect is reported before the signature ids are compared across
/// rules, since a rule that cannot be read assigns nothing to compare.
///
/// Each rule is tokenized ONCE, into the pairs both checks then read, so the work
/// is one scan per submitted rule however many checks are applied to it.
fn rules_string_breach(rules: &str) -> Option<String> {
    let scanned: Vec<(&str, ScannedRule)> = super::suricata_rule_lines(rules)
        .map(|rule| (rule, scan(rule)))
        .collect();
    let mut refusals: Vec<String> = scanned
        .iter()
        .filter_map(|(rule, scanned)| {
            rule_defect(scanned).map(|reason| refusal_naming(&loaded_rule(rule), &reason))
        })
        .collect();
    refusals.extend(duplicate_signature_refusals(&scanned));
    (!refusals.is_empty()).then(|| refusals.join("\n"))
}

/// What a rule the service could not load is refused with: a fixed stem, then
/// the reason it could not be loaded.
///
/// Its own built shape, rather than the parameter-bracket one the rest of this
/// service's request-derived refusals take ([`super::parameter_message`]).
fn refusal(reason: &str) -> String {
    format!("stateful rule is invalid, reason: {reason}")
}

/// The same refusal with the offending RULE named before the reason, which is
/// how a defect of one rule's own text is reported: the rule as the service
/// loaded it ([`loaded_rule`]), so a client can tell which line of a submitted
/// rules string the reason is about.
fn refusal_naming(rule: &str, reason: &str) -> String {
    format!("stateful rule is invalid, rule: {rule}, reason: {reason}")
}

/// Why the rule `scanned` was read from is not a rule the engine can load, or
/// `None` when it is one.
///
/// The defects are reported in the order the rule is read: its action heads the
/// rule, and its options follow.
fn rule_defect(scanned: &ScannedRule) -> Option<String> {
    if let Some(action) = scanned
        .action
        .as_deref()
        .filter(|action| !STATEFUL_ACTIONS.contains(action))
    {
        return Some(format!("Action {action} is not supported"));
    }
    scanned
        .signature_id
        .is_none()
        .then(|| SID_NOT_ASSIGNED.to_owned())
}

/// One refusal per rule whose signature id an EARLIER rule of the same rules
/// string already assigned, in the order the rules were written.
///
/// The rules are loaded in order and each one registers its signature id, so the
/// rule a clash is reported for is the one being loaded — the first rule to
/// assign an id keeps it, and every later rule assigning the same one is a
/// duplicate of a signature the engine already holds. The refusal names that
/// rule inside the reason rather than in a `rule:` segment of its own, which is
/// how a clash between rules is reported as against a defect in one rule's text.
fn duplicate_signature_refusals(scanned: &[(&str, ScannedRule)]) -> Vec<String> {
    let mut assigned: HashSet<&str> = HashSet::new();
    scanned
        .iter()
        .filter(|(_, scanned)| {
            scanned
                .signature_id
                .as_deref()
                .is_some_and(|signature_id| !assigned.insert(signature_id))
        })
        .map(|(rule, _)| refusal(&format!("Duplicate signature \"{}\"", loaded_rule(rule))))
        .collect()
}

/// `rule` as the service LOADED it, which is how a refusal reports it: the rule
/// the caller wrote, carrying the generator id the service assigns it
/// ([`GENERATOR_ID`]) as the last of its options.
///
/// A rule's options are the parenthesised list that closes it, so the id is
/// stated inside that list, after the option the caller wrote last — terminated,
/// since every option of a loaded rule is: a rule whose last option the caller
/// left unterminated is reported with the terminator the load reads it with. A
/// rule that carries no option list at all is reported as it was written, there
/// being no options to state the id among.
fn loaded_rule(rule: &str) -> String {
    let assigned = format!("{GENERATOR_ID_OPTION}:{GENERATOR_ID};");
    let Some(close) = rule.rfind(')') else {
        return rule.to_owned();
    };
    let written = rule[..close].trim_end();
    let closes = &rule[close..];
    if written.ends_with('(') {
        return format!("{written}{assigned}{closes}");
    }
    let terminator = if written.ends_with(';') { "" } else { ";" };
    format!("{written}{terminator} {assigned}{closes}")
}

/// What one rule's text says, as far as the scanner could read it.
#[derive(Debug, Default)]
struct ScannedRule {
    /// The action heading the rule, absent where the scanner could not read one.
    action: Option<String>,
    /// The value of the rule's [`SIGNATURE_ID_OPTION`], absent where its options
    /// assign none.
    signature_id: Option<String>,
}

/// `rule` tokenized: its action and the signature id its options assign.
///
/// The scan stops where the scanner stops reading the rule — a rule it cannot
/// tokenize says nothing beyond the point it failed at, and a scanner that
/// reports the same failure for the same position would otherwise be asked
/// again.
fn scan(rule: &str) -> ScannedRule {
    let mut scanned = ScannedRule::default();
    for event in RuleScanner::new(rule) {
        match event {
            Ok(RuleScanEvent::Action(action)) => {
                scanned.action = stated(&action);
            }
            Ok(RuleScanEvent::Option { name, value }) => {
                if name.trim() == SIGNATURE_ID_OPTION {
                    scanned.signature_id = value
                        .as_deref()
                        .and_then(stated)
                        .or(scanned.signature_id);
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    scanned
}

/// What a scanned token states, or `None` for one that states nothing: a value
/// surrounded by whitespace states what it spells, and an empty one assigns
/// nothing at all.
fn stated(token: &str) -> Option<String> {
    let token = token.trim();
    (!token.is_empty()).then(|| token.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a rule assigns a signature id exactly when its options carry a `sid`
    /// with a value, as Suricata's grammar reads them.
    ///
    /// The rows span: the plain form; a `sid` after other options; one before
    /// them; one whose value is swallowed by an option value that was never
    /// terminated; a rule carrying no `sid` at all; one carrying the keyword with
    /// no value; a rule with no options block; and a rule the scanner cannot
    /// tokenize at all.
    #[test]
    fn a_rule_assigns_a_signature_id_only_when_its_options_carry_one() {
        // (case, the rule, the signature id it assigns)
        let rows: &[(&str, &str, Option<&str>)] = &[
            (
                "the plain form",
                "pass tcp any any -> any any (sid:1;)",
                Some("1"),
            ),
            (
                "after other options",
                r#"alert tcp any any -> any 443 (msg:"egress"; sid:1001; rev:1;)"#,
                Some("1001"),
            ),
            (
                "before other options",
                r#"drop tcp any any -> any any (sid:2; msg:"blocked";)"#,
                Some("2"),
            ),
            (
                "swallowed by an unterminated option value",
                r#"pass tcp 10.1.0.0/16 any -> any 443 (msg:"egress 443" sid:1001; rev:1;)"#,
                None,
            ),
            (
                "no sid at all",
                r#"pass tcp any any -> any 443 (msg:"egress 443"; rev:1;)"#,
                None,
            ),
            (
                "the keyword with no value",
                "pass tcp any any -> any any (sid;)",
                None,
            ),
            ("no options at all", "pass tcp any any -> any any", None),
            ("not a rule the scanner can read", "sid:1;", None),
        ];

        for (case, rule, assigned) in rows {
            let scanned = scan(rule).signature_id;
            assert_eq!(
                scanned.as_deref(),
                *assigned,
                "case {case:?}: expected the signature id {assigned:?}, got {scanned:?}"
            );
        }
    }

    /// RULE: a rule's own defect is its unsupported action, or the signature id
    /// its options assign none of — and a rule with a supported action and a
    /// signature id has none.
    ///
    /// The rows span every modelled action, two actions the service does not
    /// implement (one of them a stateless action's name), a rule whose action is
    /// written in upper case as the model spells it rather than as a rule does,
    /// and a rule that is defective in both ways at once.
    #[test]
    fn a_rules_own_defect_is_its_action_or_its_missing_signature_id() {
        // (case, the rule, the defect)
        let rows: &[(&str, &str, Option<&str>)] = &[
            ("pass", "pass tcp any any -> any any (sid:1;)", None),
            ("drop", "drop tcp any any -> any any (sid:1;)", None),
            ("alert", "alert tcp any any -> any any (sid:1;)", None),
            ("reject", "reject tcp any any -> any any (sid:1;)", None),
            (
                "an action the service does not implement",
                "allow tcp any any -> any any (sid:1;)",
                Some("Action allow is not supported"),
            ),
            (
                "a stateless action's name",
                "aws:pass tcp any any -> any any (sid:1;)",
                Some("Action aws:pass is not supported"),
            ),
            (
                "the modelled spelling rather than a rule's",
                "PASS tcp any any -> any any (sid:1;)",
                Some("Action PASS is not supported"),
            ),
            (
                "no signature id",
                r#"pass tcp any any -> any any (msg:"nothing";)"#,
                Some("sid not assigned"),
            ),
            (
                "neither a supported action nor a signature id",
                r#"allow tcp any any -> any any (msg:"nothing";)"#,
                Some("Action allow is not supported"),
            ),
        ];

        for (case, rule, defect) in rows {
            let actual = rule_defect(&scan(rule));
            assert_eq!(
                actual.as_deref(),
                *defect,
                "case {case:?}: expected the defect {defect:?}, got {actual:?}"
            );
        }
    }

    /// RULE: every rule that cannot be loaded is reported, in the order the rules
    /// were written, one refusal per line — and the rule each refusal names
    /// carries the generator id the service assigns it.
    #[test]
    fn every_rule_that_cannot_be_loaded_is_reported() {
        let rules = "pass tcp any any -> any any (sid:1;)\n\
                     # a comment, which carries no rule\n\
                     \n\
                     pass udp any any -> any any (msg:\"second\";)\n\
                     allow ip any any -> any any (sid:3;)";
        let reported = rules_string_breach(rules);
        assert_eq!(
            reported.as_deref(),
            Some(
                "stateful rule is invalid, rule: pass udp any any -> any any (msg:\"second\"; \
                 gid:1234567890;), reason: sid not assigned\n\
                 stateful rule is invalid, rule: allow ip any any -> any any (sid:3; \
                 gid:1234567890;), reason: Action allow is not supported"
            ),
            "expected both unloadable rules reported in order, got {reported:?}"
        );

        let every_rule_loads = "pass tcp any any -> any any (sid:1;)\n\
                                pass udp any any -> any any (sid:2;)";
        let reported = rules_string_breach(every_rule_loads);
        assert_eq!(
            reported, None,
            "expected no breach for rules that each load, got {reported:?}"
        );
    }

    /// RULE: a rule whose signature id an earlier rule already assigned is
    /// reported, the rule that assigned it first is not, and a set of distinct
    /// ids is accepted.
    #[test]
    fn a_rule_repeating_an_earlier_signature_id_is_reported() {
        let rules = "pass tcp 10.1.0.0/16 any -> any 443 (sid:1001; rev:1;)\n\
                     pass udp 10.2.0.0/16 any -> any 443 (sid:1002; rev:1;)\n\
                     drop tcp 10.3.0.0/16 any -> any 443 (sid:1001; rev:2;)\n\
                     drop udp 10.4.0.0/16 any -> any 443 (sid:1001; rev:3;)";
        let reported = rules_string_breach(rules);
        assert_eq!(
            reported.as_deref(),
            Some(
                "stateful rule is invalid, reason: Duplicate signature \"drop tcp 10.3.0.0/16 \
                 any -> any 443 (sid:1001; rev:2; gid:1234567890;)\"\n\
                 stateful rule is invalid, reason: Duplicate signature \"drop udp 10.4.0.0/16 \
                 any -> any 443 (sid:1001; rev:3; gid:1234567890;)\""
            ),
            "expected the two rules repeating an assigned id reported, got {reported:?}"
        );

        let distinct = "pass tcp any any -> any any (sid:1;)\n\
                        pass udp any any -> any any (sid:2;)\n\
                        pass ip any any -> any any (sid:3;)";
        let reported = rules_string_breach(distinct);
        assert_eq!(
            reported, None,
            "expected no breach for distinct signature ids, got {reported:?}"
        );
    }
}
