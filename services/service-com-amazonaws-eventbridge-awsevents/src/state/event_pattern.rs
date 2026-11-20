// SPDX-License-Identifier: CC0-1.0

//! EventBridge's event-pattern language: validate a pattern, and decide whether
//! an event envelope matches one.
//!
//! Moved out of `crate::state` unchanged, so one file holds one concern; the
//! language and the operators it admits are stated where the module is declared
//! ([`crate::state`]).

use std::collections::BTreeMap;
use std::net::IpAddr;

use ::serde_json::value::RawValue;
use ::serde_json::{Map, Value};

/// The comparison operators `numeric` admits.
const NUMERIC_OPERATORS: [&str; 5] = ["=", "<", "<=", ">", ">="];

/// The match type that compares without regard to case, and the one
/// `prefix` and `suffix` accept nested under themselves to fold their own
/// comparison.
const IGNORE_CASE: &str = "equals-ignore-case";

/// The match types that compare one END of a leaf against a string.
const AFFIX_OPERATORS: [&str; 2] = ["prefix", "suffix"];

/// How a refusal that states the plain string-operand requirement names the
/// match type it names.
///
/// The requirement is one every match type that compares TEXT shares, and the
/// service states it for the FIRST of them rather than for the match type that
/// was written wherever the operand is not text at all and no reading of it
/// was attempted ([`validate_operator`]'s `cidr` arm).
const FIRST_STRING_OPERATOR: &str = "prefix";

/// How a refusal names the affix match types NEGATED by `anything-but`.
///
/// Both forms are read there by one reader, so the reason it states names the
/// PAIR it accepts rather than the form that was written
/// ([`validate_negated_operator`]).
const NEGATED_AFFIX: &str = "prefix/suffix";

/// The operators that take a single string and may also appear nested
/// inside `anything-but`.
const STRING_OPERATORS: [&str; 4] = ["prefix", "suffix", IGNORE_CASE, "wildcard"];

/// What `anything-but` negates, as the refusal of a value that is none of
/// those forms states it.
///
/// `anything-but` excludes a VALUE, any value of a list, or whatever one
/// nested string match type matches, so an operand that is none of these is
/// not a value it was ever given: the reason names the forms the match type
/// takes rather than the type the operand that was written must have, because
/// the reader could not read it as any of them
/// ([`validate_negated_operator`]).
const ANYTHING_BUT_VALUE: &str =
    "Value of anything-but must be an array or single string/number value.";

/// The refusal of a member that is no string, of the LIST the case-folding
/// match type is handed under `anything-but`.
///
/// That list holds the strings the comparison folds, so a member of any other
/// kind is not one of them: the reason names the list the member sits in, and
/// the kinds of token that are not strings, rather than the match type's own
/// pattern — what was read is one member of a list and not the operand itself
/// ([`validate_negated_operator`]).
///
/// The list is named for the match type it belongs to, and the validators this
/// service runs a pattern through do not spell that match type alike: the one
/// that compiles a RULE's pattern names it by the SHORT name of the comparison
/// it folds (`ignore-case`), while the others name it in full
/// ([`Validator::anything_but_ignore_case_list`]). Both sentences are otherwise
/// the one sentence, so the kinds of token that are not strings are listed once.
const ANYTHING_BUT_IGNORE_CASE_LIST_FOR_A_RULE: &str =
    "Inside anything-but/ignore-case list, number|start|null|boolean is not supported.";
const ANYTHING_BUT_IGNORE_CASE_LIST: &str =
    "Inside anything-but/equals-ignore-case list, number|start|null|boolean is not supported.";

/// How deeply a pattern may nest objects.
///
/// This module's OWN bound on recursion, not a claim about any limit AWS
/// publishes: `EventPattern` is modelled `length 0..4096`, which bounds a
/// pattern's text but not its depth, so the guard is stated here instead of
/// relying on `serde_json`'s parse-depth default. It is far above any real
/// pattern — the deepest field path an event envelope offers is a few
/// levels inside `detail`.
const MAX_PATTERN_DEPTH: usize = 32;

/// What every refusal of a pattern opens with.
///
/// EventBridge reports an unacceptable pattern as "Event pattern is not
/// valid. Reason: <what is wrong>", so the reason this module computes is
/// stated after it and the whole message is the one
/// `InvalidEventPatternException` carries, whichever operation was given the
/// pattern. A caller therefore never restates the opening itself — the
/// message this module answers with is already the whole one.
pub(super) const INVALID_PATTERN: &str = "Event pattern is not valid. Reason:";

/// A term object names ONE match type, so one carrying several names no
/// single one; this is the reason EventBridge gives for it.
const ONE_KEY_ONLY: &str = "Only one key allowed in match expression";

/// A list with no member lists no value to match, and an object with no
/// member names no match type: neither is a term EventBridge admits.
const EMPTY_ARRAY: &str = "Empty arrays are not allowed";
const EMPTY_OBJECT: &str = "Empty objects are not allowed";

/// `exists` compares against the PRESENCE of a leaf, so its argument is a
/// boolean and nothing else; this is the reason EventBridge gives for any
/// other argument.
const EXISTS_NOT_BOOLEAN: &str = "exists match pattern must be either true or false.";

/// What the argument of a match type that compares TEXT must be, as the
/// reason for any other argument states it ([`argument_type_fault`]).
const A_STRING: &str = "a string";

/// The reason a match type handed an argument of a type it does not take is
/// refused for: that match type's name, then what its pattern must be.
///
/// EventBridge words this refusal for the MATCH TYPE and nothing else — the
/// reason names no member and no field path, and enumerates no alternative
/// form, because the location clause the message carries
/// ([`Refusal::message`]) already points into the submitted pattern at the
/// argument that is wrong. `exists` is refused in the same shape
/// ([`EXISTS_NOT_BOOLEAN`]).
///
/// The sentences the service itself states are the ones for a match type that
/// compares TEXT ([`A_STRING`]), for the affix pair `anything-but` negates
/// ([`NEGATED_AFFIX`]) and for `exists`. The rest — what `cidr`,
/// `anything-but` and `numeric` require — are this module's own reading of the
/// same shape, worded for each match type's argument, and the two
/// `anything-but` ones name the forms it takes because no single type names
/// them.
fn argument_type_fault(operator: &str, must_be: &str) -> String {
    format!("{operator} match pattern must be {must_be}")
}

/// A `numeric` range whose bounds admit no number at all: the reason
/// EventBridge gives for it, in the service's own wording.
const BOTTOM_BELOW_TOP: &str = "Bottom must be less than top";

/// A pattern is a FILTER over an event's fields, so the pattern itself is an
/// object; this is the reason EventBridge gives for text that parses as
/// anything else.
const FILTER_NOT_AN_OBJECT: &str = "Filter is not an object";

/// The member name that names a RELATIONSHIP between pattern objects rather
/// than a field of the event: "at least one branch must match".
///
/// Named here, where the language is read, so every reader of a pattern — the
/// validator, the matcher, and whoever reads what a pattern establishes about a
/// field — spells the relationship one way.
pub(super) const OR_RELATIONSHIP: &str = "$or";

/// How many pattern objects an `$or` relationship relates.
///
/// `$or` is a RELATIONSHIP between pattern objects, and a relationship needs
/// two sides: the service states the bound itself in the refusal the checker
/// gives for a shorter list ([`OR_NEEDS_TWO_OBJECTS`]).
const OR_RELATED_OBJECTS: usize = 2;

/// The reason the operation that only CHECKS a pattern gives for a `$or`
/// relating fewer than [`OR_RELATED_OBJECTS`] pattern objects.
///
/// The two validators this service runs a pattern through agree on every
/// refusal but this one: the checker words it for the relationship, while the
/// compiler reads a list too short to relate anything as the ordinary match
/// term list it is (see [`validate_object`]).
const OR_NEEDS_TWO_OBJECTS: &str = "There must have at least 2 Objects in $or relationship.";

/// The character `wildcard` matches any run of characters with.
const WILDCARD: char = '*';

/// The character that makes the one after it LITERAL inside a `wildcard`
/// operand.
///
/// Only the two characters the operand's own grammar gives a meaning to may be
/// escaped — the wildcard itself and this character — so an escape of anything
/// else is a sequence the operand does not define
/// ([`wildcard_violation`]).
const ESCAPE: char = '\\';

/// The feature a JSON reader needs for the words JSON cannot spell, as the
/// refusal of such a word names it.
///
/// A pattern carrying `Infinity`, `-Infinity` or `NaN` was written by a
/// serializer that had the extension enabled, so the refusal names the
/// extension the reader would need rather than reporting arbitrary unquoted
/// text ([`unreadable_json`]).
const ALLOW_NON_NUMERIC_NUMBERS: &str = "JsonReadFeature.ALLOW_NON_NUMERIC_NUMBERS";

/// What a refusal of a `numeric` bound outside the comparison's own numeric
/// domain states, before the bound as the request wrote it
/// ([`comparable_numeral`]).
const CANNOT_COMPARE_NUMBER: &str = "Cannot compare number : ";

/// Which of the two validators this service runs a pattern through is reading
/// it.
///
/// They refuse the same patterns for the same reasons everywhere but two
/// sentences ([`OR_NEEDS_TWO_OBJECTS`],
/// [`ANYTHING_BUT_IGNORE_CASE_LIST_FOR_A_RULE`]), so the distinction travels as
/// a parameter of the one parse rather than as a second copy of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Validator {
    /// The compiler a RULE's pattern goes through: `PutRule`, and the parse
    /// routing takes of a stored pattern.
    RuleCompiler,
    /// The compiler an ARCHIVE's pattern goes through: `CreateArchive` and
    /// `UpdateArchive`.
    ArchiveCompiler,
    /// The checker `TestEventPattern` answers from, which stores nothing.
    Checker,
}

impl Validator {
    /// Whether this validator reads a list of match terms where the checker
    /// reads a relationship too short to relate anything ([`validate_object`]).
    fn compiles(self) -> bool {
        !matches!(self, Self::Checker)
    }

    /// How this validator names the list the case-folding match type is handed
    /// under `anything-but` ([`ANYTHING_BUT_IGNORE_CASE_LIST_FOR_A_RULE`]).
    fn anything_but_ignore_case_list(self) -> &'static str {
        match self {
            Self::RuleCompiler => ANYTHING_BUT_IGNORE_CASE_LIST_FOR_A_RULE,
            Self::ArchiveCompiler | Self::Checker => ANYTHING_BUT_IGNORE_CASE_LIST,
        }
    }
}

/// The most characters the event pattern of a RULE may carry.
///
/// A rule's event pattern is bounded by the service's pattern quota, which counts
/// the pattern's characters AS SUBMITTED — the whitespace between its tokens
/// included, since the bound is on the text and not on a normalisation of it. The
/// modelled `@length` on `EventPattern` is looser (0..4096) and is what the
/// boundary validator enforces, so the quota itself is charged by
/// [`length_violation`], where a rule is written.
pub const MAX_RULE_PATTERN_CHARS: usize = 2048;

/// Why the event pattern of the rule `rule_name` is too long for that rule to
/// carry, or `None` when it is within the quota.
///
/// The refusal names the PARAMETER whose quota was exceeded, the rule it was
/// given for and the quota itself — the three facts a caller cannot read
/// anywhere else — and nothing of the submitted text: a pattern past the bound
/// is as long as the request, and there is no position in it to report the way
/// a reader's refusal has one ([`Refusal::message`]). It carries no
/// [`INVALID_PATTERN`] opening either: a size refusal is not the language
/// refusing the pattern, so it does not reuse the sentence that says it is.
pub fn length_violation(rule_name: &str, pattern: &str) -> Option<String> {
    (pattern.chars().count() > MAX_RULE_PATTERN_CHARS).then(|| {
        format!(
            "Parameter EventPattern for rule {rule_name} exceeds limit of \
             {MAX_RULE_PATTERN_CHARS}."
        )
    })
}

/// Parse `pattern` as the RULE compiler reads it — the validator `PutRule` and
/// routing itself go through (an archive's pattern is read by its own,
/// [`Validator::ArchiveCompiler`]).
///
/// What comes back is the WHOLE message of the `InvalidEventPatternException`
/// a caller answers with: the opening ([`INVALID_PATTERN`]), the reason, and
/// the source location the reader had reached in the submitted text
/// ([`Refusal::message`]). The reason is worded for the CONSTRUCT at fault —
/// the match type, or the member as the pattern spells it — and carries no
/// field path of this module's own making, since the location clause is what
/// points a caller at the text to change.
pub fn parse(pattern: &str) -> Result<Map<String, Value>, String> {
    parse_as(pattern, Validator::RuleCompiler)
}

/// [`parse`], read by `validator`: the same parse, worded as that validator
/// words it.
pub fn parse_as(pattern: &str, validator: Validator) -> Result<Map<String, Value>, String> {
    parse_reason(pattern, validator).map_err(|refusal| refusal.message(pattern))
}

/// The parse itself, reporting the reason the refusal states and where in
/// the submitted text the reader stood when it stopped.
fn parse_reason(pattern: &str, validator: Validator) -> Result<Map<String, Value>, Refusal> {
    let value: Value =
        ::serde_json::from_str(pattern).map_err(|error| unreadable_json(pattern, &error))?;
    let Value::Object(map) = value else {
        return Err(At::root(pattern).refuse(Read::Opening, FILTER_NOT_AN_OBJECT));
    };
    if map.is_empty() {
        return Err(At::root(pattern).refuse(
            Read::Whole,
            "Event pattern must name at least one field",
        ));
    }
    validate_object(&map, &At::root(pattern), 1, validator)?;
    Ok(map)
}

/// The refusal of text that is not JSON at all: WHERE the parse that ran
/// stopped, and what it was expecting there.
///
/// EventBridge refuses a pattern it cannot read with its JSON parser's own
/// diagnostic rather than with a restatement of the member's type, and the
/// three readings that diagnostic is built from — the word JSON has no
/// spelling for that a parse stopped on, the unquoted word it stopped on
/// where a value belongs, and the character or end-of-input it stopped on
/// otherwise, with what was expected there — are the tree's shared ones
/// ([`::service_common::json_parse_fault`]), taken from the parse
/// that actually ran, so this service and every other one handed the same
/// text cannot disagree about where it went wrong. A fault class worded
/// wrongly or missing is corrected THERE, for every service that quotes it.
///
/// What is this service's own is the framing: the opening
/// ([`INVALID_PATTERN`]) and the location clause [`Refusal::message`] builds,
/// which echoes the pattern as submitted (`(String)"<pattern>"`) rather than
/// redacting it, because that is what this service answers.
fn unreadable_json(pattern: &str, error: &::serde_json::Error) -> Refusal {
    use ::service_common::json_parse_fault as fault;

    // A word JSON has no spelling for is named as the non-standard token it
    // is, at the position just PAST it: the reader matches the whole word
    // before it can say the extension that admits it is off, so that is how
    // far it had read. A word where a value belongs is named WHOLE, at the
    // position it STARTS; every other fault names the character, or the end of
    // the input, the parse stopped on, and the position is the parse's own.
    if let Some((token, line, column)) = fault::non_numeric_number_token(pattern, error) {
        return Refusal::parsed(
            format!(
                "Non-standard token '{token}': enable `{ALLOW_NON_NUMERIC_NUMBERS}` to allow"
            ),
            line,
            column + token.chars().count(),
        );
    }
    match fault::unrecognized_token(pattern, error) {
        Some((word, line, column)) => Refusal::parsed(
            format!(
                "Unrecognized token '{word}': was expecting ({})",
                fault::VALUE_CLASSES
            ),
            line,
            column,
        ),
        None => Refusal::parsed(
            fault::unexpected_input(pattern, error),
            error.line(),
            error.column(),
        ),
    }
}

/// A pattern the language does not admit: the reason, and where in the
/// submitted text the reader stood when it refused.
#[derive(Debug)]
struct Refusal {
    /// What is wrong, in the wording the refusal states after
    /// [`INVALID_PATTERN`].
    reason: String,
    /// Where the reader was when it found that out, and `None` for a reason
    /// the reader did not itself produce ([`Refusal::unplaced`]).
    spot: Option<Spot>,
    /// How many readers STATED that position.
    ///
    /// One, for a refusal the reader that found the fault answers with. A
    /// refusal about one argument of a LIST a match type was handed passes
    /// back out through the reader that handed the whole list over, and that
    /// reader states the position again — the same position, neither reader
    /// having moved — so the message carries the location clause once for
    /// each of them ([`Refusal::restated`]).
    stated: usize,
}

impl Refusal {
    /// A refusal the pattern READER did not state: the operand's own parser
    /// did, so there is no reading position to report and the message is the
    /// reason alone.
    fn unplaced(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            spot: None,
            stated: 1,
        }
    }

    /// A refusal the JSON PARSE itself stated, at the position it reports.
    fn parsed(reason: String, line: usize, column: usize) -> Self {
        Self {
            reason,
            spot: Some(Spot::Position { line, column }),
            stated: 1,
        }
    }

    /// This refusal as the reader ABOVE restates it: the same reason at the
    /// same position, stated once more.
    fn restated(self) -> Self {
        Self {
            stated: self.stated + 1,
            ..self
        }
    }

    /// The whole message the refusal carries: the opening, the reason, and —
    /// for a reason the reader stated — the source location, which is the
    /// pattern exactly as it was submitted and the line and column of the
    /// position the reader had reached in it, once for each reader that
    /// stated it.
    ///
    /// Every part of the location is derived from the request, so the
    /// caller's own text is what it points into: a tool that reads the
    /// location can show the offending character of the pattern its user
    /// wrote. The pattern travels as part of a message VALUE, which the
    /// protocol's own serializer encodes, so text carrying quotes or
    /// newlines cannot break the error envelope it is rendered into.
    fn message(&self, pattern: &str) -> String {
        let opening = format!("{INVALID_PATTERN} {}", self.reason);
        match &self.spot {
            None => opening,
            Some(spot) => {
                let (line, column) = spot.line_and_column(pattern);
                let location = format!(
                    "\n at [Source: (String)\"{pattern}\"; line: {line}, column: {column}]"
                );
                format!("{opening}{}", location.repeat(self.stated.max(1)))
            }
        }
    }
}

/// Where in the submitted pattern text a refusal happened.
#[derive(Debug)]
enum Spot {
    /// At the node `steps` names, with `read` of that node consumed: the
    /// position is found by walking the submitted TEXT to it, so it is the
    /// caller's own spelling — whitespace, key order and all — that decides
    /// the line and column.
    Node { steps: Vec<Step>, read: Read },
    /// At a line and column already known, which is all there is to know
    /// about text that does not parse as JSON at all.
    Position { line: usize, column: usize },
}

impl Spot {
    /// The line and column, both counted from 1, this spot is at in
    /// `pattern`.
    ///
    /// A node the walk cannot follow leaves the position at the end of the
    /// text: every node a refusal names comes from the parse of this very
    /// text, so there is no such node — reporting the end of what was read
    /// is the honest answer rather than an invented position.
    fn line_and_column(&self, pattern: &str) -> (usize, usize) {
        match self {
            Self::Position { line, column } => (*line, *column),
            Self::Node { steps, read } => {
                let offset = offset_of(pattern, steps, *read).unwrap_or(pattern.len());
                line_and_column(pattern, offset)
            }
        }
    }
}

/// One step from the pattern's root to one of its nodes, as the walk over
/// the submitted text follows it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    /// The value of the member with this name.
    Key(String),
    /// The value of the member at this position, counted in the order the
    /// TEXT writes the members: how a refusal about a SECOND member of an
    /// object finds it.
    KeyAt(usize),
    /// The member of a list at this position.
    Index(usize),
}

/// How much of the node a refusal names the reader had consumed when it
/// refused — which is what decides the position it reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Read {
    /// Its first character, which is where reading the member NAME that
    /// introduces it leaves the reader: a reader that has just read a
    /// member name has consumed the colon after it and the first character
    /// of the value as well. What a refusal about a member name itself had
    /// read.
    Entered,
    /// Its opening token only — a string's opening quote, an object's or a
    /// list's bracket, or the whole of any other scalar, which is one token.
    /// What a refusal decided from the node's TYPE had read.
    Opening,
    /// All of it, up to and past its closing token: what a refusal that had
    /// to read the node's CONTENT had read.
    Whole,
}

/// Where one step of the validation is: the submitted text, and the steps that
/// find this node in it.
///
/// A node is only ever a POSITION, never a name a reason states: the reasons
/// this module words name the construct at fault — a match type, or a member
/// as the pattern spells it — and the location clause the message carries
/// ([`Refusal::message`]) is what points at the text. The text travels with
/// the node because a reason about a VALUE spells that value back as the
/// request wrote it ([`Self::written`]), which the parsed tree no longer
/// knows: a number the parse had to read as a double carries the double's
/// own spelling and not the caller's.
#[derive(Debug, Clone)]
struct At<'a> {
    /// The pattern as it was submitted.
    source: &'a str,
    /// The steps from the pattern's root to this node.
    steps: Vec<Step>,
}

impl<'a> At<'a> {
    /// The pattern as a whole.
    fn root(source: &'a str) -> Self {
        Self {
            source,
            steps: Vec::new(),
        }
    }

    /// The value of the member named `key`, whether that member names a field
    /// of the event or a match type and its argument.
    fn key(&self, key: &str) -> Self {
        Self {
            source: self.source,
            steps: self.then(Step::Key(key.to_string())),
        }
    }

    /// The value of the member this node writes at `nth`.
    fn nth_key(&self, nth: usize) -> Self {
        Self {
            source: self.source,
            steps: self.then(Step::KeyAt(nth)),
        }
    }

    /// The member of this list at `index`.
    fn index(&self, index: usize) -> Self {
        Self {
            source: self.source,
            steps: self.then(Step::Index(index)),
        }
    }

    /// This node's steps, with `step` appended.
    fn then(&self, step: Step) -> Vec<Step> {
        let mut steps = self.steps.clone();
        steps.push(step);
        steps
    }

    /// Whether `other` is this very node of the submitted text.
    fn is(&self, other: &Self) -> bool {
        self.steps == other.steps
    }

    /// This node as the submitted text SPELLS it, and `None` when the walk
    /// cannot follow the steps to it.
    ///
    /// A window onto the request's own characters ([`node_at`]), so a reason
    /// that spells a value back spells back what was sent.
    fn written(&self) -> Option<&'a str> {
        node_at(self.source, &self.steps).map(RawValue::get)
    }

    /// Refuse the pattern here, the reader having consumed `read` of this
    /// node.
    fn refuse(&self, read: Read, reason: impl Into<String>) -> Refusal {
        Refusal {
            reason: reason.into(),
            spot: Some(Spot::Node {
                steps: self.steps.clone(),
                read,
            }),
            stated: 1,
        }
    }
}

/// The offset in `pattern` the reader stands at, having walked to the node
/// `steps` names and consumed `read` of it.
///
/// The position a refusal reports is a fact about the submitted TEXT rather
/// than about the parse tree — two patterns that parse to the same tree point
/// at different characters — so the walk needs each node's own characters,
/// which is exactly what `serde_json`'s `RawValue` is: a node read as a
/// WINDOW onto the text it came from, borrowed from it and never
/// re-interpreted, so where that window begins is where the node begins
/// ([`offset_in`]). The reading is `serde_json`'s throughout: nothing here
/// re-scans the document.
fn offset_of(pattern: &str, steps: &[Step], read: Read) -> Option<usize> {
    let text = node_at(pattern, steps)?.get();
    let start = offset_in(pattern, text)?;
    Some(match read {
        // Reading the member name that introduces a value consumes the
        // colon after the name and the value's first character too, so a
        // refusal about the NAME stands one character into the value.
        Read::Entered => start + text.chars().next().map_or(0, char::len_utf8),
        // A string is read as far as its opening quote and an object or a
        // list as far as its bracket; every other value is a single token,
        // so reading it at all reads the whole of it.
        Read::Opening => match text.as_bytes().first() {
            Some(b'"' | b'{' | b'[') => start + 1,
            _ => start + text.len(),
        },
        Read::Whole => start + text.len(),
    })
}

/// The node the walk `steps` names, as the WINDOW onto `pattern` it is, and
/// `None` when the walk cannot follow the steps.
///
/// `serde_json`'s `RawValue` is exactly such a window — a node borrowed from
/// the text it came from and never re-interpreted — so both what a node is
/// SPELLED as ([`At::written`]) and where it BEGINS ([`offset_in`]) are read
/// off the one walk. The reading is `serde_json`'s throughout: nothing here
/// re-scans the document.
fn node_at<'a>(pattern: &'a str, steps: &[Step]) -> Option<&'a RawValue> {
    let mut node: &RawValue = ::serde_json::from_str(pattern).ok()?;
    for step in steps {
        node = match step {
            Step::Index(index) => ::serde_json::from_str::<Vec<&RawValue>>(node.get())
                .ok()?
                .get(*index)
                .copied()?,
            Step::Key(key) => ::serde_json::from_str::<BTreeMap<String, &RawValue>>(node.get())
                .ok()?
                .remove(key.as_str())?,
            // The nth member the TEXT writes, which is the nth by where it
            // begins: a parsed object does not keep the order the caller
            // wrote its members in.
            Step::KeyAt(nth) => {
                let members =
                    ::serde_json::from_str::<BTreeMap<String, &RawValue>>(node.get()).ok()?;
                let mut written: Vec<&RawValue> = members.into_values().collect();
                written.sort_by_key(|member| {
                    offset_in(pattern, member.get()).unwrap_or(usize::MAX)
                });
                *written.get(*nth)?
            }
        };
    }
    Some(node)
}

/// The byte offset at which `window` sits inside `text`, or `None` when it is
/// not a slice of it.
///
/// Both are slices of the one submitted string, so the offset is the distance
/// between where they start — read from the two slices' addresses, never
/// dereferenced — and the bound check is what keeps that an answer about this
/// text rather than about two unrelated strings.
fn offset_in(text: &str, window: &str) -> Option<usize> {
    let offset = (window.as_ptr() as usize).checked_sub(text.as_ptr() as usize)?;
    offset
        .checked_add(window.len())
        .filter(|end| *end <= text.len())
        .map(|_| offset)
}

/// The line and column, both counted from 1, of `offset` in `text`.
///
/// The column counts CHARACTERS from the start of the line, so a multi-byte
/// character before the position counts once.
fn line_and_column(text: &str, offset: usize) -> (usize, usize) {
    let read = text.get(..offset).unwrap_or(text);
    let line = 1 + read.matches('\n').count();
    let last = read.rsplit_once('\n').map_or(read, |(_, after)| after);
    (line, 1 + last.chars().count())
}

/// Whether `event` matches an ALREADY-PARSED pattern, spending `budget`.
///
/// Routing parses each rule's pattern once per request and evaluates it
/// against every entry, so the parse is not repeated per (entry, rule)
/// pair; `budget` is what bounds the total comparison work those pairs can
/// demand. A match that runs out of budget answers "no match", and the
/// caller reports the exhaustion (see [`Budget::spent`]) rather than
/// passing a half-evaluated verdict off as a real one.
pub fn matches_parsed(
    pattern: &Map<String, Value>,
    event: &Value,
    budget: &mut Budget,
) -> bool {
    match_object(pattern, event, &[], budget)
}

/// A work allowance for pattern matching, counted in bytes of text the
/// matcher is allowed to walk.
///
/// Every factor of the routing work is individually bounded — entries per
/// request by the modelled `Entries` maximum, rules per bus by
/// [`super::MAX_RULES_PER_BUS`], an event by
/// [`super::MAX_ENTRY_SIZE_BYTES`], a pattern by `EventPattern`'s modelled
/// 4096 characters — but their PRODUCT is not, so a single request could
/// otherwise demand far more comparison work than any real one does. One
/// budget is created per entry and spent across every rule that entry is
/// matched against, which caps the whole request at the budget times the
/// modelled maximum number of entries.
#[derive(Debug)]
pub struct Budget {
    remaining: u64,
}

impl Budget {
    /// A budget of `units`, each unit one byte of text the matcher may walk.
    pub fn new(units: u64) -> Self {
        Self { remaining: units }
    }

    /// Whether this budget ran out, which means the verdict it produced is
    /// not a real one and the caller must say so.
    pub fn spent(&self) -> bool {
        self.remaining == 0
    }

    /// Charge `units` against the allowance, answering `false` once it is
    /// gone. Charged BEFORE the work it pays for, so no comparison runs
    /// outside the bound.
    fn charge(&mut self, units: usize) -> bool {
        let units = u64::try_from(units).unwrap_or(u64::MAX);
        if self.remaining <= units {
            self.remaining = 0;
            return false;
        }
        self.remaining -= units;
        true
    }
}

/// How many bytes comparing against this leaf may walk: a string's own
/// length, a small constant otherwise. Computed in constant time, so
/// charging is never itself the expensive step.
fn leaf_cost(leaf: &Value) -> usize {
    match leaf {
        Value::String(text) => text.len().max(1),
        Value::Array(members) => members.len().max(1),
        Value::Object(map) => map.len().max(1),
        _ => 1,
    }
}

/// How many bytes of PATTERN a term's argument may make the matcher walk:
/// every string it carries and every member it holds, AT EVERY LEVEL.
///
/// Every level, because the walks this pays for descend: a nested
/// `anything-but` argument (`{"anything-but": {"wildcard": ["*a*", "*b*"]}}`)
/// compares the leaf against every member of its inner list, so a charge that
/// stopped at the outer object would pay one unit for all of it. A pattern is
/// bounded by `EventPattern`'s modelled 4096 characters, so this walk is
/// itself cheap — but the same reading is asked of an EVENT's value (see
/// [`match_term`]'s `exists` arm), which nests as deep as
/// [`super::MAX_DETAIL_DEPTH`] admits. It therefore descends on an EXPLICIT
/// stack rather than on the thread's: how deeply a document nests decides how
/// much heap this walk takes and never whether the process survives it, which
/// is the same guarantee the parse of that document is given
/// ([`super::parse_json_document`]).
fn argument_cost(argument: &Value) -> usize {
    let mut total = 0usize;
    let mut pending: Vec<&Value> = ::std::vec![argument];
    while let Some(value) = pending.pop() {
        let charge = match value {
            Value::String(text) => text.len().max(1),
            Value::Array(members) => {
                pending.extend(members.iter());
                members.len().max(1)
            }
            Value::Object(map) => {
                pending.extend(map.values());
                map.len().max(1)
            }
            _ => 1,
        };
        total = total.saturating_add(charge);
    }
    total
}

/// What comparing ONE leaf against `argument` may walk: the argument's own
/// text at every level, PLUS the leaf once for EVERY argument the comparison
/// can try it against.
///
/// The leaf is charged per argument because each try walks it again — a
/// `wildcard` or `prefix` comparison scans the whole leaf — so a list of
/// arguments costs the leaf's length times its length, which is the work
/// [`match_operator`] can actually perform.
fn comparison_cost(argument: &Value, leaf: &Value) -> usize {
    argument_cost(argument)
        .saturating_add(leaf_cost(leaf).saturating_mul(argument_terms(argument)))
}

/// How many separate arguments one comparison can try against the leaf: the
/// members of a list, the members of a nested operator's list, and one for a
/// single argument.
fn argument_terms(argument: &Value) -> usize {
    match argument {
        Value::Array(members) => members.len().max(1),
        Value::Object(map) => map
            .values()
            .map(|nested| match nested {
                Value::Array(members) => members.len().max(1),
                _ => 1,
            })
            .sum::<usize>()
            .max(1),
        _ => 1,
    }
}

/// Every field of a pattern object: an array of match terms, a nested
/// object, or the `$or` branch list. `depth` counts the objects entered so
/// far, so recursion is bounded by [`MAX_PATTERN_DEPTH`] whatever a caller
/// sends.
fn validate_object(
    pattern: &Map<String, Value>,
    at: &At<'_>,
    depth: usize,
    validator: Validator,
) -> Result<(), Refusal> {
    if depth > MAX_PATTERN_DEPTH {
        return Err(at.refuse(
            Read::Opening,
            format!("Event pattern nests objects more than {MAX_PATTERN_DEPTH} levels deep"),
        ));
    }
    for (key, spec) in pattern {
        let at = at.key(key);
        if key == OR_RELATIONSHIP {
            let Some(branches) = spec.as_array() else {
                return Err(at.refuse(
                    Read::Opening,
                    format!("\"{key}\" must be an array of patterns"),
                ));
            };
            if branches.is_empty() {
                return Err(at.refuse(Read::Whole, EMPTY_ARRAY));
            }
            // `$or` relates pattern objects to one another, and a
            // relationship needs [`OR_RELATED_OBJECTS`] sides: a list too
            // short to relate anything is no relationship, so the checker
            // says so ([`OR_NEEDS_TWO_OBJECTS`]) and the compiler reads the
            // list as the ordinary match TERM list a field's value is —
            // which is what makes a lone object naming no match type an
            // unrecognized match type rather than a branch of an `$or`.
            if branches.len() < OR_RELATED_OBJECTS {
                if !validator.compiles() {
                    return Err(at.refuse(Read::Whole, OR_NEEDS_TWO_OBJECTS));
                }
                branches
                    .iter()
                    .enumerate()
                    .try_for_each(|(index, term)| {
                        validate_term(term, key, &at.index(index), validator)
                    })?;
                continue;
            }
            for (index, branch) in branches.iter().enumerate() {
                let at = at.index(index);
                let Some(object) = branch.as_object() else {
                    return Err(at.refuse(
                        Read::Opening,
                        format!("every branch of \"{key}\" must be a pattern object"),
                    ));
                };
                if object.is_empty() {
                    return Err(at.refuse(Read::Whole, EMPTY_OBJECT));
                }
                validate_object(object, &at, depth + 1, validator)?;
            }
            continue;
        }
        match spec {
            Value::Array(terms) => {
                if terms.is_empty() {
                    return Err(at.refuse(Read::Whole, EMPTY_ARRAY));
                }
                for (index, term) in terms.iter().enumerate() {
                    validate_term(term, key, &at.index(index), validator)?;
                }
            }
            Value::Object(child) => {
                if child.is_empty() {
                    return Err(at.refuse(Read::Whole, EMPTY_OBJECT));
                }
                validate_object(child, &at, depth + 1, validator)?;
            }
            _ => {
                // The reader names the MEMBER whose value is neither, as it
                // is spelled in the pattern.
                return Err(at.refuse(
                    Read::Opening,
                    format!("\"{key}\" must be an object or an array"),
                ));
            }
        }
    }
    Ok(())
}

/// One match term: a literal value, or a single-key operator object. `member`
/// is the member whose term list this term belongs to, as the pattern spells
/// it, and `at` locates the term itself in the submitted text.
///
/// A term object is a MATCH EXPRESSION, which names one match type: an
/// object carrying several keys names several ([`ONE_KEY_ONLY`]) and one
/// carrying none names no match type at all ([`EMPTY_OBJECT`]).
///
/// Every literal value is admitted, a NUMBER included whichever number it
/// names: a literal term is matched by VALUE and not by comparison, so it
/// needs no double to carry it ([`same_value`]) — which is why the numbers the
/// comparisons refuse are still terms this one admits.
fn validate_term(
    term: &Value,
    member: &str,
    at: &At<'_>,
    validator: Validator,
) -> Result<(), Refusal> {
    match term {
        Value::String(_) | Value::Number(_) | Value::Bool(_) | Value::Null => Ok(()),
        Value::Object(map) if map.len() == 1 => {
            let (operator, argument) = map
                .iter()
                .next()
                .expect("a single-entry map has a first entry");
            let operand = at.key(operator);
            validate_operator(operator, argument, &operand, &operand, validator)
        }
        Value::Object(map) if map.is_empty() => Err(at.refuse(Read::Whole, EMPTY_OBJECT)),
        // The reader finds this out on the SECOND match type the object
        // names, having read that name and entered its argument, which is
        // as far as it had got.
        Value::Object(_) => Err(at.nth_key(1).refuse(Read::Entered, ONE_KEY_ONLY)),
        _ => Err(at.refuse(
            Read::Opening,
            format!("\"{member}\" admits literal values and single-operator objects only"),
        )),
    }
}

/// The argument an operator takes. `at` locates that ARGUMENT in the
/// submitted text, and `handed` the whole argument the match type was HANDED,
/// which is a LIST where the one nested under `anything-but` was given one.
///
/// A match type handed an argument whose TYPE it does not take is refused
/// with that match type's own sentence — its name, then what its pattern
/// must be ([`argument_type_fault`]) — and no field path: the location
/// clause the message carries ([`Refusal::message`]) is what points at the
/// argument, and the sentence `exists` is refused with
/// ([`EXISTS_NOT_BOOLEAN`]) is worded the same way.
///
/// The two positions differ for a refusal the match type states once it has
/// COLLECTED the arguments it compares with rather than while reading each of
/// them: a value's own type is read as that value is read, so it is reported
/// there, while the grammar of a `wildcard` operand is read when the match
/// type is built out of every operand it was handed, which is past the whole
/// list. They are the same node everywhere a match type takes one argument.
///
/// A refusal about ONE argument of such a list is also stated twice, once by
/// this reader and once by the reader that handed the list over
/// ([`Refusal::restated`]).
fn validate_operator(
    operator: &str,
    argument: &Value,
    at: &At<'_>,
    handed: &At<'_>,
    validator: Validator,
) -> Result<(), Refusal> {
    // A type the operator does not take is decided from the argument's
    // opening token alone, which is as much of it as the reader had read.
    let bad = |what: &str| {
        Err(operand_refusal(
            at.refuse(Read::Opening, argument_type_fault(operator, what)),
            at,
            handed,
        ))
    };
    match operator {
        "exists" => {
            if argument.is_boolean() {
                Ok(())
            } else {
                Err(at.refuse(Read::Opening, EXISTS_NOT_BOOLEAN))
            }
        }
        // These compare a string against a string, so a string is what the
        // match pattern must be — either written out or nested under the
        // case-folding match type, whose own argument is a string too.
        "prefix" | "suffix" => match argument {
            Value::String(_) => Ok(()),
            Value::Object(map) if map.len() == 1 => match map.get(IGNORE_CASE) {
                Some(nested) => {
                    let folded = at.key(IGNORE_CASE);
                    validate_operator(IGNORE_CASE, nested, &folded, &folded, validator)
                }
                None => bad(A_STRING),
            },
            _ => bad(A_STRING),
        },
        "equals-ignore-case" => {
            if argument.is_string() {
                Ok(())
            } else {
                bad(A_STRING)
            }
        }
        // A wildcard value is a string whose `*` stands for any run of
        // characters, the language admits no two of them in a row, and a
        // backslash makes the character after it literal.
        //
        // The grammar is read when the match type is BUILT out of every
        // operand it was handed, so a refusal is stated there: past the whole
        // of that argument, which is the list where one was given.
        "wildcard" => match argument.as_str() {
            Some(text) => wildcard_violation(text)
                .map_or(Ok(()), |reason| Err(handed.refuse(Read::Whole, reason))),
            None => bad(A_STRING),
        },
        "cidr" => match argument.as_str().map(parse_cidr) {
            Some(Ok(_)) => Ok(()),
            // A rule of the CIDR NOTATION is read by the address parser out
            // of the block's own text, and the service words it for the
            // notation, so what that parser says is the whole reason and
            // there is no reading position to state.
            Some(Err(CidrRefusal::Notation(reason))) => Err(Refusal::unplaced(reason)),
            // Text that is no block at all is this operator's own argument
            // check, worded and placed as every other one is.
            Some(Err(CidrRefusal::NotABlock)) => bad("an IPv4 or IPv6 CIDR block"),
            // An operand that is no TEXT is refused before any block is read
            // out of it: it is the plain string-operand requirement every
            // match type that compares text states, and the one the service
            // words for the first of them ([`FIRST_STRING_OPERATOR`]) — the
            // block's own notation is not what such an operand is wrong
            // about.
            None => Err(at.refuse(
                Read::Opening,
                argument_type_fault(FIRST_STRING_OPERATOR, A_STRING),
            )),
        },
        // What `anything-but` negates is EXCLUDED by comparison, so a NUMBER
        // it negates is one the comparison has to be able to carry
        // ([`comparable_numeral`]), whether it was written out on its own or
        // as a member of the list. A number of a LIST is converted as that
        // member is read, so the refusal stands just past it; one written out
        // on its own is converted where the match type is built out of it,
        // and that refusal carries no reading position at all
        // ([`uncomparable_number`]).
        "anything-but" => match argument {
            Value::Number(_) => match uncomparable_number(at) {
                Some(written) => {
                    Err(Refusal::unplaced(format!("{CANNOT_COMPARE_NUMBER}{written}")))
                }
                None => Ok(()),
            },
            Value::String(_) => Ok(()),
            Value::Array(values) if !values.is_empty() => {
                if !values
                    .iter()
                    .all(|value| value.is_string() || value.is_number())
                {
                    return bad("a list of strings or a list of numbers");
                }
                for (index, _) in values
                    .iter()
                    .enumerate()
                    .filter(|(_, value)| value.is_number())
                {
                    let member = at.index(index);
                    if let Some(written) = uncomparable_number(&member) {
                        return Err(member.refuse(
                            Read::Whole,
                            format!("{CANNOT_COMPARE_NUMBER}{written}"),
                        ));
                    }
                }
                Ok(())
            }
            Value::Object(map) if map.len() == 1 => {
                let (nested, nested_argument) = map
                    .iter()
                    .next()
                    .expect("a single-entry map has a first entry");
                // A nested string operator may take a LIST of arguments,
                // any one of which excludes the value: `anything-but` is the
                // one place the pattern language admits that form. Any other
                // match type nested under it is one it does not negate, and
                // the reader has entered that operator's argument by the
                // time it can say so.
                let nested_at = at.key(nested);
                if !STRING_OPERATORS.contains(&nested.as_str()) {
                    return Err(nested_at.refuse(
                        Read::Opening,
                        format!("Unsupported {operator} pattern: {nested}"),
                    ));
                }
                match nested_argument {
                    Value::Array(arguments) if !arguments.is_empty() => {
                        arguments.iter().enumerate().try_for_each(|(index, one)| {
                            validate_negated_operator(
                                nested,
                                one,
                                &nested_at.index(index),
                                &nested_at,
                                validator,
                            )
                        })
                    }
                    one => {
                        validate_negated_operator(nested, one, &nested_at, &nested_at, validator)
                    }
                }
            }
            _ => bad("a value, a list of values, or one nested string operator"),
        },
        "numeric" => {
            let Some(terms) = argument.as_array() else {
                return bad("an array of operator/value pairs");
            };
            if terms.is_empty() {
                return Err(at.refuse(Read::Whole, EMPTY_ARRAY));
            }
            // The list is read as the operator/value PAIRS it is: an
            // operator the comparison set does not hold, and a bound that
            // is not a number (a bound the list stops short of included),
            // are each named for what they are.
            for (pair_index, pair) in terms.chunks(2).enumerate() {
                // `chunks` never yields an empty chunk; reading the
                // operator as an OPTION keeps that a fact about this code
                // rather than an assumption about a caller's list.
                let Some(spelled) = pair.first() else {
                    continue;
                };
                let spelled_at = pair_index * 2;
                let Some(comparison) = spelled
                    .as_str()
                    .filter(|operator| NUMERIC_OPERATORS.contains(operator))
                else {
                    // The list is read PAIR by pair — an operator and the
                    // bound it compares against — so the reader has taken
                    // in the whole pair by the time it can say that the
                    // operator names no comparison, and the position it
                    // reports is just past the pair's last member. A pair
                    // the list stops short of a bound for ends at the list
                    // itself, which is as far as the reader got.
                    let reason = format!(
                        "Unrecognized numeric range operator: {}",
                        spelled_out(spelled)
                    );
                    return Err(match pair.len() {
                        1 => at.refuse(Read::Whole, reason),
                        _ => at.index(spelled_at + 1).refuse(Read::Whole, reason),
                    });
                };
                match pair.get(1) {
                    // A `numeric` comparison is a comparison of DOUBLES, so a
                    // bound no double carries is one it cannot compare
                    // against: refused, naming the bound as the request wrote
                    // it, rather than silently compared as the different
                    // number the conversion would give
                    // ([`comparable_numeral`]). The bound is read from the
                    // submitted TEXT, because that is the spelling the reason
                    // states and the one whose digits decide the question: a
                    // parsed number no longer knows them. The whole list of
                    // pairs is the argument the reader was reading, so the
                    // position is the one it reports for every other fault
                    // read out of that list.
                    Some(Value::Number(bound)) => {
                        let bound_at = at.index(spelled_at + 1);
                        let written = bound_at
                            .written()
                            .map_or_else(|| bound.to_string(), str::to_string);
                        if !comparable_numeral(&written) {
                            return Err(at.refuse(
                                Read::Whole,
                                format!("{CANNOT_COMPARE_NUMBER}{written}"),
                            ));
                        }
                    }
                    Some(_) => {
                        return Err(at
                            .index(spelled_at + 1)
                            .refuse(Read::Opening, format!("Value of {comparison} must be numeric")));
                    }
                    // The list stops short of the bound, so the reader has
                    // read the whole of it before it can tell.
                    None => {
                        return Err(at.refuse(
                            Read::Whole,
                            format!("Value of {comparison} must be numeric"),
                        ));
                    }
                }
            }
            // A range whose bottom is not below its top admits no number at
            // all, so it is a pattern that can never match rather than a
            // filter: refused instead of stored, which the whole list had to
            // be read to decide.
            if let Some(reason) = degenerate_range(terms) {
                return Err(at.refuse(Read::Whole, reason));
            }
            Ok(())
        }
        // The reader has entered the argument this name introduces, which
        // is as far as it gets before the name turns out to name no match
        // type at all.
        _ => Err(at.refuse(
            Read::Opening,
            format!("Unrecognized match type {operator}"),
        )),
    }
}

/// The argument of a string match type NEGATED by `anything-but`.
///
/// The two affix match types are read there by ONE reader, which words the
/// type its argument must have for the pair of forms it accepts rather than
/// for the one that was written ([`NEGATED_AFFIX`]); every other negated
/// match type is read as it is anywhere else and states its own sentence.
///
/// Only an argument whose own TYPE is wrong is worded for the pair: a nested
/// match expression is read as it is anywhere else, so what the affix forms
/// admit nested under them is admitted here too and the reader and the
/// matcher go on agreeing — and a nested expression naming something else is
/// refused by the match type that was written, which is the reader that
/// entered it.
///
/// `handed` is the whole argument the affix form was given, which is a LIST
/// where it was given one: a refusal about one member of that list is stated
/// by this reader and by the one that handed the list over
/// ([`operand_refusal`]).
///
/// An operand that is an OBJECT is only one of the forms `anything-but`
/// negates where the match type under it takes an object — the affix pair's
/// case-folding form — so any other object is no value `anything-but` admits
/// at all, and that is what the refusal states ([`ANYTHING_BUT_VALUE`]). A
/// member of the LIST the case-folding match type was handed is refused for
/// the list it sits in ([`ANYTHING_BUT_IGNORE_CASE_LIST`]).
fn validate_negated_operator(
    operator: &str,
    argument: &Value,
    at: &At<'_>,
    handed: &At<'_>,
    validator: Validator,
) -> Result<(), Refusal> {
    if operator == IGNORE_CASE && !at.is(handed) && !argument.is_string() {
        return Err(operand_refusal(
            at.refuse(Read::Opening, validator.anything_but_ignore_case_list()),
            at,
            handed,
        ));
    }
    if let Value::Object(map) = argument {
        let folds_its_comparison = AFFIX_OPERATORS.contains(&operator)
            && map.len() == 1
            && map.contains_key(IGNORE_CASE);
        if !folds_its_comparison {
            return Err(at.refuse(Read::Opening, ANYTHING_BUT_VALUE));
        }
    }
    if AFFIX_OPERATORS.contains(&operator) && !argument.is_string() && !argument.is_object() {
        return Err(operand_refusal(
            at.refuse(Read::Opening, argument_type_fault(NEGATED_AFFIX, A_STRING)),
            at,
            handed,
        ));
    }
    validate_operator(operator, argument, at, handed, validator)
}

/// `refusal`, as the readers of the operand at `at` state it: once by the
/// reader that found the fault, and once more by the reader that handed the
/// whole argument over when that argument is a LIST this operand is only one
/// member of ([`Refusal::restated`]).
///
/// A refusal about one member of a list passes back out through the reader
/// that collected the list, which states the position it is at as well — the
/// same position, since neither reader moved — so the message carries the
/// location clause twice. An argument a match type was handed whole is read
/// by one reader only and its position is stated once.
fn operand_refusal(refusal: Refusal, at: &At<'_>, handed: &At<'_>) -> Refusal {
    if at.is(handed) {
        refusal
    } else {
        refusal.restated()
    }
}

/// The number written at `at` as the request SPELLS it, when no comparison of
/// this language can carry that number ([`comparable_numeral`]) — and `None`
/// when a double carries it, or when the node is no number of the submitted
/// text at all.
///
/// The spelling is read from the text ([`At::written`]) because that is what
/// the reason states and what decides the question: a parsed number no longer
/// knows the digits it was written with. Where the refusal is PLACED is the
/// caller's to say, because it differs with the form the number was given in.
fn uncomparable_number<'a>(at: &At<'a>) -> Option<&'a str> {
    at.written().filter(|written| !comparable_numeral(written))
}

/// A pattern value as the refusal spells it back: a string as its own text,
/// anything else as the JSON it was written as.
fn spelled_out(value: &Value) -> String {
    match value.as_str() {
        Some(text) => text.to_string(),
        None => value.to_string(),
    }
}

/// Why `value` is not a wildcard value the language admits, or `None` when
/// it is one.
///
/// A `*` stands for any run of characters, so two in a row ask for nothing
/// the one does not: EventBridge refuses them and names the POSITION of the
/// second of the pair, counted in characters from the start of the value.
///
/// An [`ESCAPE`] makes the character after it LITERAL, and the operand's
/// grammar gives a meaning to exactly two characters — the wildcard and the
/// escape itself — so those two are what may be escaped and an escape of
/// anything else (a run of text that ends with one included) is a sequence the
/// grammar does not define. An escaped character is literal text rather than a
/// wildcard, so it neither opens nor continues a run of wildcards: `*\**` asks
/// for any text around a literal asterisk and is a value the language admits.
///
/// Both positions are the one just PAST the character that showed the fault —
/// the first of a pair of wildcards, and the escape marker — which is where
/// reading that character leaves the reader.
fn wildcard_violation(value: &str) -> Option<String> {
    let mut previous_was_wildcard = false;
    let mut characters = value.chars();
    let mut at = 0usize;
    while let Some(character) = characters.next() {
        match character {
            ESCAPE => {
                match characters.next() {
                    Some(escaped) if escaped == WILDCARD || escaped == ESCAPE => {}
                    _ => return Some(format!("Invalid escape character at pos {}", at + 1)),
                }
                // The escape and the character it makes literal are one
                // character of the text the operand compares.
                at += 1;
                previous_was_wildcard = false;
            }
            WILDCARD => {
                if previous_was_wildcard {
                    return Some(format!("Consecutive wildcard characters at pos {at}"));
                }
                previous_was_wildcard = true;
            }
            _ => previous_was_wildcard = false,
        }
        at += 1;
    }
    None
}

/// Whether a `numeric` comparison can compare against the number the numeral
/// `written` names: whether an IEEE-754 binary64 double carries that number
/// exactly.
///
/// A `numeric` comparison compares DOUBLES, so a numeral the conversion to a
/// double does not preserve names a number the comparison cannot answer about:
/// the double it would be compared as is a DIFFERENT number, and a verdict
/// drawn from it would be a verdict about that other number. The test is the
/// round trip through the double — the numeral and the shortest decimal of the
/// double it converts to must denote the same value ([`decimal_reading`]) —
/// which admits every numeral a double carries, the ones no finite binary
/// expansion states exactly (`0.1`) included, and refuses the ones it does not
/// (an integer above 2^53 that the conversion rounds to its neighbour, a
/// fraction carrying more digits than a double keeps). A numeral naming no
/// finite number at all is not one either.
fn comparable_numeral(written: &str) -> bool {
    let Ok(value) = written.parse::<f64>() else {
        return false;
    };
    if !value.is_finite() {
        return false;
    }
    match (decimal_reading(written), decimal_reading(&format!("{value}"))) {
        (Some(numeral), Some(represented)) => numeral == represented,
        _ => false,
    }
}

/// The number an event's leaf compares as where a `numeric` comparison is
/// concerned: a number the comparison can answer about
/// ([`comparable_numeral`]) and nothing else.
///
/// A leaf carrying a number no double carries is not a leaf the comparison
/// withholds a verdict about — it is one the comparison does not consider, so
/// a term over it holds for no such leaf, exactly as a term over a number
/// holds for no leaf carrying text. The leaf is read through the numeral the
/// parsed number spells, which is the event's own digits for every integer
/// this answers `None` for.
fn comparable_leaf(leaf: &Value) -> Option<f64> {
    match leaf {
        Value::Number(number) => number
            .as_f64()
            .filter(|_| comparable_numeral(&number.to_string())),
        _ => None,
    }
}

/// A decimal numeral as the VALUE it denotes: whether it is negative, its
/// significant digits with no leading or trailing zero, and the power of ten
/// the last of those digits carries.
///
/// Two numerals denote the same number exactly when this reading of them is
/// equal, whichever of the forms a decimal may be written in each used
/// (`1e3`, `1000`, `1000.00`), so comparing the readings compares the numbers
/// and never their spelling. Zero is one value however it is spelled, and
/// `None` is text this reading is not of a decimal numeral at all.
fn decimal_reading(numeral: &str) -> Option<(bool, String, i64)> {
    let (negative, rest) = match numeral.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, numeral),
    };
    let (mantissa, exponent) = match rest.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, exponent.parse::<i64>().ok()?),
        None => (rest, 0),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if whole.is_empty() && fraction.is_empty() {
        return None;
    }
    if !whole
        .chars()
        .chain(fraction.chars())
        .all(|character| character.is_ascii_digit())
    {
        return None;
    }
    // Each digit of the fraction lowers the power of ten its last digit
    // carries, which is what makes `1.5` and `15e-1` one number.
    let exponent = exponent.checked_sub(i64::try_from(fraction.len()).ok()?)?;
    let digits = format!("{whole}{fraction}");
    let significant = digits.trim_start_matches('0');
    let carried = significant.trim_end_matches('0');
    let exponent = exponent.checked_add(i64::try_from(significant.len() - carried.len()).ok()?)?;
    if carried.is_empty() {
        return Some((false, String::new(), 0));
    }
    Some((negative, carried.to_string(), exponent))
}

/// Why the bounds of a `numeric` term list admit no number, or `None` when at
/// least one number satisfies all of them.
///
/// The bounds are reduced to the TIGHTEST lower and upper one, which is what
/// the conjunction of the pairs means, and the range is empty when the lower
/// one is above the upper one — or equal to it while either end is exclusive.
fn degenerate_range(terms: &[Value]) -> Option<&'static str> {
    // (the bound, whether that end is exclusive)
    let mut lower: Option<(f64, bool)> = None;
    let mut upper: Option<(f64, bool)> = None;
    for pair in terms.chunks(2) {
        let (Some(operator), Some(bound)) = (
            pair.first().and_then(Value::as_str),
            pair.get(1).and_then(Value::as_f64),
        ) else {
            continue;
        };
        let tighten = |held: Option<(f64, bool)>, bound: f64, exclusive: bool, upwards: bool| {
            match held {
                Some((kept, kept_exclusive))
                    if (upwards && kept > bound) || (!upwards && kept < bound) =>
                {
                    Some((kept, kept_exclusive))
                }
                Some((kept, kept_exclusive)) if kept == bound => {
                    Some((kept, kept_exclusive || exclusive))
                }
                _ => Some((bound, exclusive)),
            }
        };
        match operator {
            ">" => lower = tighten(lower, bound, true, true),
            ">=" => lower = tighten(lower, bound, false, true),
            "<" => upper = tighten(upper, bound, true, false),
            "<=" => upper = tighten(upper, bound, false, false),
            "=" => {
                lower = tighten(lower, bound, false, true);
                upper = tighten(upper, bound, false, false);
            }
            _ => {}
        }
    }
    match (lower, upper) {
        (Some((low, low_exclusive)), Some((high, high_exclusive)))
            if low > high || (low == high && (low_exclusive || high_exclusive)) =>
        {
            Some(BOTTOM_BELOW_TOP)
        }
        _ => None,
    }
}

/// Every field of a pattern object must match, which is what makes a
/// pattern a conjunction over fields.
///
/// `prefix` is the field PATH this pattern object's own fields sit under, in
/// segments. A pattern selects on the path a value sits at, so a nested
/// pattern object does not descend into a sub-document of the event: it
/// lengthens that path, and the leaf terms underneath it are resolved
/// against the event from its root ([`select`]). That is what makes a
/// pattern match a value only at the WHOLE path it names, and never a
/// member of the same name at some other depth.
fn match_object(
    pattern: &Map<String, Value>,
    event: &Value,
    prefix: &[&str],
    budget: &mut Budget,
) -> bool {
    for (key, spec) in pattern {
        if !budget.charge(1) {
            return false;
        }
        let held = if key == OR_RELATIONSHIP && relates_objects(spec) {
            match_any_branch(spec, event, prefix, budget)
        } else {
            // A member name IS the path it spells, so one written with
            // separators contributes a segment per separator
            // ([`PATH_SEPARATOR`]).
            let mut steps: Vec<&str> = prefix.to_vec();
            steps.extend(key.split(PATH_SEPARATOR));
            match spec {
                // One of the listed terms must match, which is what makes a
                // term list a disjunction — against any value that path
                // selects, since one path can be spelled several ways and
                // can run through an array.
                Value::Array(terms) => {
                    let mut found: Vec<&Value> = ::std::vec::Vec::new();
                    if !select(event, &steps, budget, &mut found) {
                        return false;
                    }
                    let mut held = false;
                    for term in terms {
                        // "exists only works on leaf nodes": a path the
                        // event carries NO value at — because the leaf is
                        // missing, or because an intermediate parent is —
                        // is evaluated against absence rather than failed
                        // outright, so an `exists: false` term holds for it.
                        let matched = if found.is_empty() {
                            match_term(term, None, budget)
                        } else {
                            found
                                .iter()
                                .any(|value| match_term(term, Some(value), budget))
                        };
                        if matched {
                            held = true;
                            break;
                        }
                    }
                    held
                }
                Value::Object(child) => match_object(child, event, &steps, budget),
                _ => false,
            }
        };
        if !held {
            return false;
        }
    }
    true
}

/// What separates one segment of a field path from the next.
///
/// An event is matched by the PATHS its values sit at, and a path is written
/// with its segments separated by this character — which is why a member name
/// carrying one is indistinguishable from the path it spells: `{"a.b": "v"}`
/// and `{"a": {"b": "v"}}` carry `"v"` at the same path, so a pattern over
/// either matches both.
pub(super) const PATH_SEPARATOR: char = '.';

/// Every value of `event` that sits at the field path `steps`, appended to
/// `found`.
///
/// One path can select several values, because an event can spell it several
/// ways at once and because a path may run THROUGH an array: a member of an
/// array sits at the path its container sits at, so an object nested in an
/// array is entered without consuming a segment and `{"a": [{"b": "v"}]}`
/// carries `"v"` at the same path `{"a": {"b": "v"}}` does. A segment is
/// consumed by any member name that spells a whole run of the remaining
/// segments ([`segments_spanned`]).
///
/// The walk descends an EVENT's value, which nests as deep as
/// [`super::MAX_DETAIL_DEPTH`] admits, so it descends on an explicit stack for
/// the reason [`argument_cost`] gives. It is charged against `budget` by what
/// it walks, and answers `false` once that allowance is gone — the selection
/// is then incomplete, so the caller stops rather than drawing a verdict from
/// it ([`Budget::spent`]).
fn select<'a>(
    event: &'a Value,
    steps: &[&str],
    budget: &mut Budget,
    found: &mut Vec<&'a Value>,
) -> bool {
    let mut pending: Vec<(&'a Value, usize)> = ::std::vec![(event, 0)];
    while let Some((value, reached)) = pending.pop() {
        if !budget.charge(leaf_cost(value)) {
            return false;
        }
        if reached == steps.len() {
            found.push(value);
            continue;
        }
        match value {
            Value::Array(members) => {
                pending.extend(members.iter().map(|member| (member, reached)));
            }
            Value::Object(map) => {
                for (name, child) in map {
                    if let Some(spanned) = segments_spanned(name, &steps[reached..]) {
                        pending.push((child, reached + spanned));
                    }
                }
            }
            // A scalar sits at the end of its own path: there is nothing
            // further down it to select.
            _ => {}
        }
    }
    true
}

/// How many leading segments of `steps` the member name `name` spells, or
/// `None` when it spells no whole run of them.
///
/// `"a"` spells one segment of `["a", "b"]` and `"a.b"` spells both, so a
/// name carrying separators selects the same value the nesting it spells
/// would ([`PATH_SEPARATOR`]). A name that is only part of a segment
/// (`"a"` against `["ab"]`), or that spells MORE segments than remain,
/// spells none of them.
fn segments_spanned(name: &str, steps: &[&str]) -> Option<usize> {
    let mut rest = name;
    for (spanned, step) in steps.iter().enumerate() {
        rest = rest.strip_prefix(*step)?;
        if rest.is_empty() {
            return Some(spanned + 1);
        }
        rest = rest.strip_prefix(PATH_SEPARATOR)?;
    }
    None
}

/// Whether `spec` is the or-RELATIONSHIP `$or` names: a list of at least
/// [`OR_RELATED_OBJECTS`] pattern objects.
///
/// The same reading the parse takes ([`validate_object`]), so what a pattern
/// was ADMITTED as is what it is matched as: a `$or` relating too little to
/// be a relationship is the ordinary field — named `$or`, which no event
/// envelope carries — that the parse read it as.
fn relates_objects(spec: &Value) -> bool {
    spec.as_array().is_some_and(|branches| {
        branches.len() >= OR_RELATED_OBJECTS && branches.iter().all(Value::is_object)
    })
}

/// `$or`: at least one branch, itself a pattern object, must match.
///
/// Each branch's fields sit at the same path the `$or` itself does, so the
/// branches are matched under the relationship's own `prefix`.
fn match_any_branch(
    spec: &Value,
    event: &Value,
    prefix: &[&str],
    budget: &mut Budget,
) -> bool {
    let Some(branches) = spec.as_array() else {
        return false;
    };
    for branch in branches {
        let Some(object) = branch.as_object() else {
            continue;
        };
        if match_object(object, event, prefix, budget) {
            return true;
        }
    }
    false
}

/// One term against the event's value for that field, absent or present.
fn match_term(term: &Value, field: Option<&Value>, budget: &mut Budget) -> bool {
    let Value::Object(map) = term else {
        // A literal term matches the value, or any member of it when the
        // event carries a list there.
        let Some(value) = field else {
            return false;
        };
        return any_leaf(budget, value, |budget, leaf| {
            budget.charge(comparison_cost(term, leaf)) && same_value(leaf, term)
        });
    };
    let Some((operator, argument)) = map.iter().next().filter(|_| map.len() == 1) else {
        return false;
    };
    if operator == "exists" {
        // The only operator that is about PRESENCE, so it is the only one
        // that can match an absent field. "exists only works on leaf nodes.
        // It does not work on intermediate nodes": a field whose value is an
        // object, or an array carrying no leaf at all, is therefore absent as
        // far as this operator is concerned ([`leaf_exists`]).
        // Charged by what the presence test may WALK: "exists only works on
        // leaf nodes", and finding whether the field carries one descends
        // every member of an array-valued field ([`leaf_exists`]), so the
        // charge is the size of the value rather than one unit.
        return budget.charge(field.map_or(1, argument_cost))
            && argument.as_bool() == Some(field.is_some_and(leaf_exists));
    }
    let Some(value) = field else {
        return false;
    };
    any_leaf(budget, value, |budget, leaf| {
        // Charged BEFORE the comparison, by the number of bytes the
        // comparison may walk — the argument at every level, and the leaf
        // once per argument tried against it ([`comparison_cost`]) — so the
        // budget bounds the work rather than merely counting calls.
        budget.charge(comparison_cost(argument, leaf))
            && match_operator(operator, argument, leaf)
    })
}

/// Whether `value` is a LEAF the `exists` operator sees: any scalar — a
/// string, a number, a boolean, or `null` — or an array carrying at least
/// one such member.
///
/// `null` IS such a leaf: a field whose value is `null` is one the event
/// carries, so `exists: true` holds for it and `exists: false` does not. An
/// object and an empty array are what carry no leaf, so a field holding one
/// of them is absent for `exists` — which is what "exists only works on leaf
/// nodes. It does not work on intermediate nodes" means.
///
/// The search descends an EVENT's value, which nests as deep as
/// [`super::MAX_DETAIL_DEPTH`] admits, so it descends on an explicit stack for
/// the reason [`argument_cost`] gives.
fn leaf_exists(value: &Value) -> bool {
    let mut pending: Vec<&Value> = ::std::vec![value];
    while let Some(value) = pending.pop() {
        match value {
            Value::Object(_) => {}
            Value::Array(members) => pending.extend(members.iter()),
            _ => return true,
        }
    }
    false
}

/// Whether a pattern's literal term and an event's leaf are the same value.
///
/// Type-sensitive, as EventBridge's exact matching is — a string never equals a
/// number — but two NUMBERS are compared numerically rather than by their JSON
/// spelling, so `1` and `1.0` are one value and a pattern does not depend on
/// how the publisher wrote it. That comparison runs through the double the
/// numerals convert to, so it is only an answer about numbers a double carries
/// ([`comparable_numeral`]): two numerals the conversion rounds to the one
/// double are still the two different numbers they were written as, and a term
/// matches such a leaf only when the leaf names the very number the term does.
fn same_value(leaf: &Value, term: &Value) -> bool {
    match (leaf, term) {
        (Value::Number(found), Value::Number(wanted)) => {
            found == wanted
                || match (comparable_leaf(leaf), comparable_leaf(term)) {
                    (Some(found), Some(wanted)) => found == wanted,
                    _ => false,
                }
        }
        (found, wanted) => found == wanted,
    }
}

/// Apply `predicate` to every VALUE the event field `value` carries, and
/// answer whether one of them held.
///
/// A term is compared against the values a field carries rather than against
/// the lists holding them: a field holding one value offers that value, and a
/// field holding a list offers the values ITS OWN members carry — so a list
/// nested inside a list contributes the values at the bottom of it and a list
/// carrying no value at all offers nothing, which is why an empty list matches
/// no term. This is what EventBridge's any-element matching over an
/// array-valued field means, and it is the same reading the `exists` operator
/// takes of such a field ([`leaf_exists`]).
///
/// The values are walked in the order the event writes them, on an EXPLICIT
/// stack for the reason [`argument_cost`] gives: how deeply an event's field
/// nests decides how much heap this walk takes and never whether the process
/// survives it. The DESCENT itself is charged against `budget`, beside what
/// `predicate` charges for each comparison, so a field nesting many lists that
/// carry no value cannot demand unbounded walking either.
fn any_leaf<F: FnMut(&mut Budget, &Value) -> bool>(
    budget: &mut Budget,
    value: &Value,
    mut predicate: F,
) -> bool {
    let mut pending: Vec<&Value> = ::std::vec![value];
    while let Some(value) = pending.pop() {
        match value {
            Value::Array(members) => {
                if !budget.charge(members.len().max(1)) {
                    return false;
                }
                // Reversed, so the members are popped in the order they were
                // written: which value matched is not observable, but which
                // the budget was spent on is.
                pending.extend(members.iter().rev());
            }
            leaf => {
                if predicate(budget, leaf) {
                    return true;
                }
            }
        }
    }
    false
}

/// One operator against one leaf value.
fn match_operator(operator: &str, argument: &Value, leaf: &Value) -> bool {
    match operator {
        "prefix" => with_string_argument(argument, leaf, Affix::Prefix),
        "suffix" => with_string_argument(argument, leaf, Affix::Suffix),
        "equals-ignore-case" => match (leaf.as_str(), argument.as_str()) {
            (Some(text), Some(wanted)) => text.eq_ignore_ascii_case(wanted),
            _ => false,
        },
        "wildcard" => match (leaf.as_str(), argument.as_str()) {
            (Some(text), Some(wanted)) => matches_wildcard(text, wanted),
            _ => false,
        },
        "cidr" => match (
            leaf.as_str().and_then(|text| text.parse::<IpAddr>().ok()),
            argument.as_str().and_then(|block| parse_cidr(block).ok()),
        ) {
            (Some(address), Some((network, bits))) => in_cidr(address, network, bits),
            _ => false,
        },
        "anything-but" => !matches_anything_but(argument, leaf),
        "numeric" => match (comparable_leaf(leaf), argument.as_array()) {
            (Some(number), Some(terms)) => {
                terms.chunks(2).all(|pair| matches_numeric(pair, number))
            }
            _ => false,
        },
        _ => false,
    }
}

/// Which end of the leaf a `prefix`/`suffix` term anchors at.
#[derive(Clone, Copy)]
enum Affix {
    Prefix,
    Suffix,
}

/// `prefix`/`suffix`, whose argument is either a plain string or a nested
/// `{"equals-ignore-case": "<string>"}` asking for a case-folded compare.
fn with_string_argument(argument: &Value, leaf: &Value, affix: Affix) -> bool {
    let Some(text) = leaf.as_str() else {
        return false;
    };
    match argument {
        Value::String(wanted) => match affix {
            Affix::Prefix => text.starts_with(wanted.as_str()),
            Affix::Suffix => text.ends_with(wanted.as_str()),
        },
        Value::Object(map) => map
            .get("equals-ignore-case")
            .and_then(Value::as_str)
            .is_some_and(|wanted| matches_folded_affix(text, wanted, affix)),
        _ => false,
    }
}

/// A case-folded `prefix`/`suffix` compare that walks only as many
/// characters as the ARGUMENT has.
///
/// Deliberately not `text.to_lowercase()`: folding the whole leaf would
/// allocate a copy of it — up to a whole event's `Detail` — for every term
/// of every rule, while only the argument's own length is ever compared.
fn matches_folded_affix(text: &str, wanted: &str, affix: Affix) -> bool {
    match affix {
        Affix::Prefix => {
            let mut found = text.chars();
            wanted
                .chars()
                .all(|expected| found.next().is_some_and(|actual| folds_to(actual, expected)))
        }
        Affix::Suffix => {
            let mut found = text.chars().rev();
            wanted
                .chars()
                .rev()
                .all(|expected| found.next().is_some_and(|actual| folds_to(actual, expected)))
        }
    }
}

/// Whether two characters are equal ignoring case.
fn folds_to(left: char, right: char) -> bool {
    left == right
        || left
            .to_lowercase()
            .eq(right.to_lowercase())
}

/// What `anything-but` negates: a value, any member of a list of values, or
/// one nested string operator.
fn matches_anything_but(argument: &Value, leaf: &Value) -> bool {
    match argument {
        Value::Array(values) => values.iter().any(|value| same_value(leaf, value)),
        Value::Object(map) => map.iter().next().is_some_and(|(nested, nested_argument)| {
            match nested_argument {
                // A nested string operator may list several arguments, any
                // one of which excludes the leaf.
                Value::Array(arguments) => arguments
                    .iter()
                    .any(|one| match_operator(nested, one, leaf)),
                one => match_operator(nested, one, leaf),
            }
        }),
        value => same_value(leaf, value),
    }
}

/// One `numeric` operator/value pair against a number.
fn matches_numeric(pair: &[Value], number: f64) -> bool {
    let (Some(operator), Some(bound)) = (
        pair.first().and_then(Value::as_str),
        pair.get(1).and_then(Value::as_f64),
    ) else {
        return false;
    };
    match operator {
        "=" => number == bound,
        "<" => number < bound,
        "<=" => number <= bound,
        ">" => number > bound,
        ">=" => number >= bound,
        _ => false,
    }
}

/// `wildcard`: `*` stands for any run of characters, an [`ESCAPE`] makes the
/// character after it literal, and every other character is literal.
///
/// Matched by splitting the operand into the literal runs its wildcards
/// separate ([`wildcard_segments`]) and locating each run in order, advancing
/// through the text MONOTONICALLY: the first run anchors at the start, the
/// last at the end, and each one in between is found at its leftmost
/// remaining position. Greedy-leftmost is exact for a `*`-only glob — a `*`
/// absorbs any run, so a later run can always use whatever an earlier one
/// left — and because the cursor never rewinds, the whole match costs
/// O(len(text) + len(pattern)). The previous shape (a cursor that rewound to
/// the last `*` on a mismatch) was O(len(text) x len(pattern)) and could be
/// driven quadratically by a caller choosing both operands.
fn matches_wildcard(text: &str, pattern: &str) -> bool {
    let segments = wildcard_segments(pattern);
    // There is always at least one run; exactly one means the operand has no
    // wildcard at all and is therefore a literal.
    let (Some(first), Some(last)) = (segments.first(), segments.last()) else {
        return false;
    };
    if segments.len() == 1 {
        return text == first.as_str();
    }
    if !text.starts_with(first.as_str()) || !text.ends_with(last.as_str()) {
        return false;
    }
    // The two anchors must not overlap: `a*b` needs room for both.
    let mut rest = &text[first.len()..];
    if rest.len() < last.len() {
        return false;
    }
    rest = &rest[..rest.len() - last.len()];
    for segment in &segments[1..segments.len() - 1] {
        match rest.find(segment.as_str()) {
            Some(at) => rest = &rest[at + segment.len()..],
            None => return false,
        }
    }
    true
}

/// The literal runs a `wildcard` operand's wildcards separate, each as the
/// text it compares against.
///
/// An ESCAPED character is one character of LITERAL text, so it belongs to the
/// run it sits in and never separates two: `*\**` asks for any text, then a
/// literal asterisk, then any text — which is why the escapes are read here
/// rather than by splitting the operand on its own `*` characters. An escape
/// the grammar does not define never reaches a match, because a pattern
/// carrying one is refused before it is stored or tested
/// ([`wildcard_violation`]); one left in an operand compares as the two
/// characters it was written as, which invents no meaning for it.
fn wildcard_segments(pattern: &str) -> Vec<String> {
    let mut segments: Vec<String> = ::std::vec![String::new()];
    let mut characters = pattern.chars();
    while let Some(character) = characters.next() {
        // The list is created with a run in it and only ever grows, so there
        // is always a run to append to.
        let Some(run) = segments.last_mut() else {
            return segments;
        };
        match character {
            ESCAPE => match characters.next() {
                Some(escaped) if escaped == WILDCARD || escaped == ESCAPE => run.push(escaped),
                Some(escaped) => {
                    run.push(ESCAPE);
                    run.push(escaped);
                }
                None => run.push(ESCAPE),
            },
            WILDCARD => segments.push(String::new()),
            literal => run.push(literal),
        }
    }
    segments
}

/// Why text is not a CIDR block this language compares against.
///
/// The two cases are refused by DIFFERENT wordings, so they are kept apart
/// rather than flattened into one message: a mask too wide is a rule of the
/// notation, which the service states for the notation, while text that is
/// no block at all is the `cidr` operator's own argument check (see
/// [`validate_operator`]).
enum CidrRefusal {
    /// A rule of the NOTATION itself, carrying the reason the notation states
    /// for it: the text carries exactly one mask separator, and the mask
    /// leaves a RANGE of addresses.
    Notation(&'static str),
    /// The text is not an address and a mask length at all.
    NotABlock,
}

/// What the notation requires of the text that names a block: one address,
/// one separator, and the mask length after it.
///
/// The reason the ADDRESS parser states for text carrying no separator, or
/// more than one, so the whole message is that reason
/// ([`Refusal::unplaced`]).
const ONE_MASK_SEPARATOR: &str = "Malformed CIDR, one '/' required";

/// Split `a.b.c.d/len` into the network address of its block and its prefix
/// length, or say why the text is not a CIDR block this language compares
/// against.
///
/// The mask must leave a RANGE of addresses for the comparison to be a
/// range comparison at all, so it is narrower than the address family's
/// width — a mask as wide as the family names one address rather than a
/// block, and EventBridge refuses it naming that width. The text names the
/// mask with ONE separator ([`ONE_MASK_SEPARATOR`]), so text carrying none,
/// or more than one, names no block either.
///
/// An address carrying bits BEYOND the prefix names a host inside a block,
/// and the block it names is the one the comparison is about: the bits past
/// the prefix are masked off and the operand stands for that containing
/// block, so a block written from any address inside it compares exactly as
/// the same block written canonically does.
fn parse_cidr(block: &str) -> Result<(IpAddr, u8), CidrRefusal> {
    let mut written = block.split('/');
    let (Some(address), Some(length), None) = (written.next(), written.next(), written.next())
    else {
        return Err(CidrRefusal::Notation(ONE_MASK_SEPARATOR));
    };
    let Ok(address) = address.parse::<IpAddr>() else {
        return Err(CidrRefusal::NotABlock);
    };
    let Ok(bits) = length.parse::<u8>() else {
        return Err(CidrRefusal::NotABlock);
    };
    let width = if address.is_ipv4() { 32u8 } else { 128u8 };
    if bits >= width {
        return Err(CidrRefusal::Notation(mask_too_wide(address.is_ipv4())));
    }
    Ok((network_address(address, bits), bits))
}

/// The reason the notation states for a mask as wide as the address family it
/// is written for, which names that family and its width.
fn mask_too_wide(ipv4: bool) -> &'static str {
    if ipv4 {
        "IPv4 mask bits must be < 32"
    } else {
        "IPv6 mask bits must be < 128"
    }
}

/// `address` with every bit outside its first `bits` cleared: the network
/// address of the block it sits in.
fn network_address(address: IpAddr, bits: u8) -> IpAddr {
    let mut remaining = usize::from(bits);
    let mut mask_octet = |octet: &mut u8| {
        let take = remaining.min(8);
        remaining -= take;
        let mask = u8::try_from(0xffu16 << (8 - take) & 0xff).unwrap_or(0xff);
        *octet &= mask;
    };
    match address {
        IpAddr::V4(address) => {
            let mut octets = address.octets();
            octets.iter_mut().for_each(&mut mask_octet);
            IpAddr::V4(::std::net::Ipv4Addr::from(octets))
        }
        IpAddr::V6(address) => {
            let mut octets = address.octets();
            octets.iter_mut().for_each(&mut mask_octet);
            IpAddr::V6(::std::net::Ipv6Addr::from(octets))
        }
    }
}

/// Whether `address` is inside the block `network/bits`.
fn in_cidr(address: IpAddr, network: IpAddr, bits: u8) -> bool {
    let (left, right) = match (address, network) {
        (IpAddr::V4(address), IpAddr::V4(network)) => {
            (address.octets().to_vec(), network.octets().to_vec())
        }
        (IpAddr::V6(address), IpAddr::V6(network)) => {
            (address.octets().to_vec(), network.octets().to_vec())
        }
        // Different families never overlap.
        _ => return false,
    };
    let mut remaining = usize::from(bits);
    for (a, b) in left.iter().zip(right.iter()) {
        if remaining == 0 {
            break;
        }
        let take = remaining.min(8);
        let mask = u8::try_from(0xffu16 << (8 - take) & 0xff).unwrap_or(0xff);
        if a & mask != b & mask {
            return false;
        }
        remaining -= take;
    }
    true
}
