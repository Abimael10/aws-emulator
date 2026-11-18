// SPDX-License-Identifier: CC0-1.0

//! The `Risk` the risk rules a LENS DOCUMENT authors rate an answer at.
//!
//! A lens document's `riskRules` are the lens author's own grading of a question: an
//! ordered list of `{condition, risk}` entries, the first of which whose condition holds
//! over the choices an answer records as in place decides the answer's `Risk`, and the last
//! of which carries the [`super::LENS_JSON_DEFAULT_CONDITION`] that holds for every
//! selection the rules above it leave ([`authored_lens_risk`]). A condition is a boolean
//! expression over the question's OWN choice ids, so the grading travels with the question
//! the rules were authored for ([`super::RiskRating::AuthoredRiskRules`]) and every read of
//! an answer — the answer itself, the improvement plan of the review, the review's and the
//! workload's `RiskCounts` — derives the same value from it
//! ([`super::AnswerRecord::risk`]).
//!
//! The expression language is the lens JSON format specification's own and the model
//! describes none of it, so it is parsed here rather than by a crate: its atoms are the
//! question's choice ids, which the format allows to be any 1..128-character string
//! ([`super::parse_lens_json`]), and no general expression crate lexes those as names.

use super::*;

/// `HIGH_RISK`, as a lens document's `riskRules` name the risk the API reports as
/// [`RISK_HIGH`]. The document's vocabulary is not the API's `Risk` enum, so the two are
/// mapped ([`authored_lens_risk`]) rather than passed through.
pub const LENS_JSON_HIGH_RISK: &str = "HIGH_RISK";

/// `MEDIUM_RISK` — the API's [`RISK_MEDIUM`]. See [`LENS_JSON_HIGH_RISK`].
pub const LENS_JSON_MEDIUM_RISK: &str = "MEDIUM_RISK";

/// `NO_RISK` — the API's [`RISK_NONE`]. See [`LENS_JSON_HIGH_RISK`].
pub const LENS_JSON_NO_RISK: &str = "NO_RISK";

/// The `Risk` the authored `rules` rate an answer at, `selected` being the ids of the
/// choices that answer records as in place.
///
/// The rules are evaluated IN ORDER and the FIRST one whose condition holds decides the
/// rating, which is what makes the trailing `default` rule a fallback rather than an
/// override: it is reached only by a selection no earlier condition matched. `None` for a
/// rule set that grades nothing — one that is empty, or whose matching rule names a risk
/// outside the document's vocabulary — so a caller can fall back rather than invent a
/// rating.
pub fn authored_lens_risk(
    rules: &[LensRiskRule],
    selected: &BTreeSet<&str>,
) -> Option<&'static str> {
    rules
        .iter()
        .find(|rule| condition_holds(&rule.condition, selected))
        .and_then(|rule| api_risk(&rule.risk))
}

/// The `Risk` enum member a lens document's risk name maps to, and `None` for a name
/// outside [`LENS_JSON_RISK_VALUES`].
fn api_risk(authored: &str) -> Option<&'static str> {
    match authored {
        LENS_JSON_NO_RISK => Some(RISK_NONE),
        LENS_JSON_MEDIUM_RISK => Some(RISK_MEDIUM),
        LENS_JSON_HIGH_RISK => Some(RISK_HIGH),
        _ => None,
    }
}

/// The longest condition this evaluation reads, which is the longest one an import
/// accepts: `condition` is a string property of the lens JSON format, bounded where the
/// document is parsed ([`LENS_JSON_CONDITION_LENGTH`]). Stated again here because a rule
/// also reaches this evaluation from a lens stored before — text past the bound grades
/// nothing rather than being walked.
pub const LENS_JSON_CONDITION_LENGTH: (usize, usize) = (1, 1024);

/// The deepest nesting this evaluation reads: each `!` and each `(` is one level, so the
/// recursive descent below is bounded by a CONSTANT rather than by what a lens document
/// nested. A condition deeper than this grades nothing. Far above any authored rule — the
/// format's conditions are flat lists of choice ids joined by operators — and shallow
/// enough that no input can exhaust the stack.
pub const MAX_CONDITION_DEPTH: usize = 32;

/// Whether one rule's `condition` holds for the choices `selected`.
///
/// [`LENS_JSON_DEFAULT_CONDITION`] is the format's reserved condition and holds for every
/// selection. Every other condition is a boolean expression over the question's choice
/// ids: an id holds when the answer records that choice as in place, `!` negates,
/// `&&` conjoins, `||` disjoins — the usual precedence, `!` tightest and `||` loosest —
/// and parentheses group. An expression that does not parse grades nothing, so it holds
/// for no selection and the rules after it decide the answer; so does one longer than
/// [`LENS_JSON_CONDITION_LENGTH`] or nested deeper than [`MAX_CONDITION_DEPTH`].
pub fn condition_holds(condition: &str, selected: &BTreeSet<&str>) -> bool {
    if condition.trim() == LENS_JSON_DEFAULT_CONDITION {
        return true;
    }
    if condition.len() > LENS_JSON_CONDITION_LENGTH.1 {
        return false;
    }
    let Some(tokens) = tokenize(condition) else {
        return false;
    };
    let mut expression = Condition {
        tokens: &tokens,
        next: 0,
        depth: 0,
        selected,
    };
    let holds = expression.disjunction();
    // A condition is the WHOLE expression, so one with anything left over — a dangling
    // operand, an unbalanced `)` — holds for no selection either.
    match holds {
        Some(holds) if expression.next == tokens.len() => holds,
        _ => false,
    }
}

/// One lexeme of a condition: an operator, a bracket, or one of the question's choice ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token<'condition> {
    And,
    Or,
    Not,
    Open,
    Close,
    Choice(&'condition str),
}

/// The characters that start an operator or a bracket, and so end a choice id.
fn is_operator_char(character: char) -> bool {
    matches!(character, '&' | '|' | '!' | '(' | ')')
}

/// `condition` as lexemes, and `None` for text that cannot be lexed at all — a lone `&`
/// or `|`, which neither starts an operator nor can belong to a choice id.
fn tokenize(condition: &str) -> Option<Vec<Token<'_>>> {
    let mut tokens = Vec::new();
    let mut rest = condition.trim_start();
    while !rest.is_empty() {
        let tail = if let Some(tail) = rest.strip_prefix("&&") {
            tokens.push(Token::And);
            tail
        } else if let Some(tail) = rest.strip_prefix("||") {
            tokens.push(Token::Or);
            tail
        } else if let Some(tail) = rest.strip_prefix('!') {
            tokens.push(Token::Not);
            tail
        } else if let Some(tail) = rest.strip_prefix('(') {
            tokens.push(Token::Open);
            tail
        } else if let Some(tail) = rest.strip_prefix(')') {
            tokens.push(Token::Close);
            tail
        } else {
            // A choice id runs up to the next operator, bracket or space: the format puts
            // no further restriction on an id, so every character that is not one of those
            // belongs to it.
            let end = rest
                .find(|character: char| character.is_whitespace() || is_operator_char(character))
                .unwrap_or(rest.len());
            if end == 0 {
                return None;
            }
            tokens.push(Token::Choice(&rest[..end]));
            &rest[end..]
        };
        rest = tail.trim_start();
    }
    Some(tokens)
}

/// One condition being evaluated against a selection: the lexemes, how far they have been
/// read, and the choices the answer records as in place.
///
/// A recursive descent over the grammar the doc of [`condition_holds`] states, evaluating
/// as it goes: `None` from any of the three levels is text that is not an expression, which
/// is what makes an unparsable condition hold for no selection rather than panic.
struct Condition<'condition, 'selection> {
    tokens: &'condition [Token<'condition>],
    next: usize,
    /// How deep the descent is, so it stops at [`MAX_CONDITION_DEPTH`] instead of
    /// recursing as deep as the condition nests.
    depth: usize,
    selected: &'condition BTreeSet<&'selection str>,
}

impl<'condition> Condition<'condition, '_> {
    /// The lexeme to read next, and `None` at the end of the condition.
    fn peek(&self) -> Option<Token<'condition>> {
        self.tokens.get(self.next).copied()
    }

    /// `and ("||" and)*` — the loosest level.
    fn disjunction(&mut self) -> Option<bool> {
        let mut holds = self.conjunction()?;
        while self.peek() == Some(Token::Or) {
            self.next += 1;
            // The operand is read whatever the left side came out as, because the whole
            // condition still has to be consumed for it to hold at all.
            holds = self.conjunction()? || holds;
        }
        Some(holds)
    }

    /// `unary ("&&" unary)*`.
    fn conjunction(&mut self) -> Option<bool> {
        let mut holds = self.unary()?;
        while self.peek() == Some(Token::And) {
            self.next += 1;
            holds = self.unary()? && holds;
        }
        Some(holds)
    }

    /// The tightest level, with the descent bounded: nesting past
    /// [`MAX_CONDITION_DEPTH`] is text this evaluation does not read, so no condition can
    /// drive the recursion deeper than that constant.
    fn unary(&mut self) -> Option<bool> {
        if self.depth >= MAX_CONDITION_DEPTH {
            return None;
        }
        self.depth += 1;
        let holds = self.operand();
        self.depth -= 1;
        holds
    }

    /// `"!" unary | "(" disjunction ")" | <choice id>`.
    fn operand(&mut self) -> Option<bool> {
        match self.peek()? {
            Token::Not => {
                self.next += 1;
                Some(!self.unary()?)
            }
            Token::Open => {
                self.next += 1;
                let holds = self.disjunction()?;
                if self.peek() != Some(Token::Close) {
                    return None;
                }
                self.next += 1;
                Some(holds)
            }
            Token::Choice(choice_id) => {
                self.next += 1;
                Some(self.selected.contains(choice_id))
            }
            Token::And | Token::Or | Token::Close => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A selection of choice ids, as a stored answer's in-place choices reach the
    /// evaluation.
    fn selection(ids: &[&'static str]) -> BTreeSet<&'static str> {
        ids.iter().copied().collect()
    }

    #[test]
    fn a_condition_is_a_boolean_expression_over_the_selected_choice_ids() {
        let cases: &[(&str, &[&str], bool)] = &[
            ("default", &[], true),
            ("default", &["c1"], true),
            ("c1", &["c1"], true),
            ("c1", &["c2"], false),
            ("c1 && c2", &["c1"], false),
            ("c1 && c2", &["c1", "c2"], true),
            ("c1 || c2", &["c2"], true),
            ("c1 || c2", &["c3"], false),
            ("!c1", &["c2"], true),
            ("!c1", &["c1"], false),
            ("c1 && !c2", &["c1", "c2"], false),
            ("(c1 || c2) && c3", &["c2", "c3"], true),
            ("(c1 || c2) && c3", &["c3"], false),
            // Ids the format allows that are not Rust-style identifiers.
            (
                "choice.one-a && choice.two-b",
                &["choice.one-a", "choice.two-b"],
                true,
            ),
            // Text that is not an expression holds for nothing.
            ("c1 &&", &["c1"], false),
            ("(c1", &["c1"], false),
            ("c1 & c2", &["c1", "c2"], false),
            ("", &["c1"], false),
        ];
        for (condition, selected, expected) in cases {
            let holds = condition_holds(condition, &selection(selected));
            assert_eq!(
                holds, *expected,
                "case {condition:?} over {selected:?}: expected {expected}, got {holds}"
            );
        }
    }

    #[test]
    fn the_first_rule_that_holds_decides_the_risk() {
        let rules = vec![
            LensRiskRule {
                condition: "c1 && c2".to_string(),
                risk: LENS_JSON_NO_RISK.to_string(),
            },
            LensRiskRule {
                condition: "c1".to_string(),
                risk: LENS_JSON_MEDIUM_RISK.to_string(),
            },
            LensRiskRule {
                condition: LENS_JSON_DEFAULT_CONDITION.to_string(),
                risk: LENS_JSON_HIGH_RISK.to_string(),
            },
        ];
        let cases: &[(&[&str], &str)] = &[
            (&["c1", "c2"], RISK_NONE),
            (&["c1"], RISK_MEDIUM),
            (&["c2"], RISK_HIGH),
            (&[], RISK_HIGH),
        ];
        for (selected, expected) in cases {
            let rated = authored_lens_risk(&rules, &selection(selected));
            assert_eq!(
                rated,
                Some(*expected),
                "case {selected:?}: expected {expected}, got {rated:?}"
            );
        }
    }

    #[test]
    fn text_that_is_not_a_bounded_expression_grades_nothing() {
        // Malformed, truncated, unbalanced, oversized and over-nested text: each holds for
        // no selection, so the rules after it decide the answer.
        let nested_to_the_limit = format!(
            "{}c1{}",
            "(".repeat(MAX_CONDITION_DEPTH - 1),
            ")".repeat(MAX_CONDITION_DEPTH - 1)
        );
        let too_deep = format!(
            "{}c1{}",
            "(".repeat(MAX_CONDITION_DEPTH + 1),
            ")".repeat(MAX_CONDITION_DEPTH + 1)
        );
        let negated_too_deep = format!("{}c1", "!".repeat(MAX_CONDITION_DEPTH + 1));
        let oversized = format!(
            "c1 || {}",
            "c2 || ".repeat(LENS_JSON_CONDITION_LENGTH.1 / 6 + 1)
        );
        let cases: &[(&str, &str, bool)] = &[
            ("malformed-operator", "c1 & c2", false),
            ("truncated-operator", "c1 &&", false),
            ("truncated-group", "(c1 && c2", false),
            ("unbalanced-close", "c1)", false),
            ("empty", "", false),
            ("operator-only", "&&", false),
            ("dangling-not", "!", false),
            ("nested-to-the-limit", nested_to_the_limit.as_str(), true),
            ("nested-past-the-limit", too_deep.as_str(), false),
            ("negated-past-the-limit", negated_too_deep.as_str(), false),
            ("oversized", oversized.as_str(), false),
        ];
        for (case, condition, expected) in cases {
            let holds = condition_holds(condition, &selection(&["c1"]));
            assert_eq!(
                holds, *expected,
                "case {case}: expected {expected} for a condition of {} bytes, got {holds}",
                condition.len()
            );
        }
    }

    #[test]
    fn a_rule_set_that_grades_nothing_rates_nothing() {
        let unmodelled = vec![LensRiskRule {
            condition: LENS_JSON_DEFAULT_CONDITION.to_string(),
            risk: "LOW_RISK".to_string(),
        }];
        for (case, rules) in [("empty", Vec::new()), ("unmodelled-risk", unmodelled)] {
            let rated = authored_lens_risk(&rules, &selection(&["c1"]));
            assert_eq!(rated, None, "case {case}: expected no rating, got {rated:?}");
        }
    }
}
