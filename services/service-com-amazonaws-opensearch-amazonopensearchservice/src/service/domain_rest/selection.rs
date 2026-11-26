// SPDX-License-Identifier: CC0-1.0

//! WHICH documents a query selects — the one reading of a query this plane has, shared by
//! the search, the count, the delete-by-query and the document-level narrowing a role
//! carries.
//!
//! Two halves: the clause tree a request's query states ([`Selector`]), and how that tree is
//! asked of ONE document of ONE index. A clause is read against the index's own mappings
//! wherever the mapping decides what the value IS rather than how it is written: a field
//! mapped as an instant is compared as an instant, whatever form the request and the stored
//! document each wrote it in, and a field holding text is compared through that field's own
//! analysis chain.
//!
//! The vocabulary is bounded and STATED: a clause outside it is answered the way the engine
//! answers a query name it has no handler registered for, so a search this emulator cannot
//! evaluate never looks like one it did.

use super::*;

/// The member an `exists` names the field it asks about in.
const EXISTS_FIELD: &str = "field";

/// The members a `range` may carry: the four bounds it states, and the weight it gives its
/// own clause, which changes nothing about which documents it selects.
const RANGE_MEMBERS: [&str; 5] = ["gt", "gte", "lt", "lte", "boost"];

/// The members a `bool` may carry: the four clause lists it composes, and the options that
/// change how its own clauses are weighed, counted and reported rather than which documents
/// each selects.
const BOOL_MEMBERS: [&str; 8] = [
    "must",
    "filter",
    "must_not",
    "should",
    "minimum_should_match",
    "adjust_pure_negative",
    "boost",
    "_name",
];

/// The members a clause carries its text, its fields and its options in.
const QUERY_MEMBER: &str = "query";
const FIELDS_MEMBER: &str = "fields";
const MATCH_TYPE_MEMBER: &str = "type";
const DEFAULT_FIELD_MEMBER: &str = "default_field";
const DEFAULT_OPERATOR_MEMBER: &str = "default_operator";

/// The `multi_match` type whose question is asked of the terms IN ORDER.
const PHRASE_MATCH_TYPE: &str = "phrase";

/// The field pattern naming every field the index maps, which is what a clause naming no
/// field of its own asks (`index.query.default_field`).
pub(super) const ALL_FIELDS: &str = "*";

/// The separator a field entry writes the weight it gives that field after
/// (`title^2`) — a weight changes the SCORE a field contributes and not which documents are
/// selected, so the name is what is read off such an entry.
const FIELD_WEIGHT_SEPARATOR: char = '^';

/// How many fields ONE clause may ask its question of, patterns expanded.
///
/// The emulator's OWN bound on the breadth one clause costs: each field is read off every
/// document the scan reaches, so this is what keeps that work a product of constants rather
/// than of a count the query chose and the mappings a caller wrote.
pub(super) const MAX_MATCHED_FIELDS: usize = 64;

/// The field name a field entry names, without the weight it may carry.
fn named_field(entry: &str) -> String {
    entry
        .split_once(FIELD_WEIGHT_SEPARATOR)
        .map_or(entry, |(field, _)| field)
        .to_string()
}

/// The clause asking `text` of the field `field` names — that field itself, or every field of
/// the index a PATTERN names ([`Selector::AnyField`]).
pub(super) fn field_clause(field: &str, text: &str, phrase: bool) -> Selector {
    if field.contains('*') || field.contains('?') {
        return Selector::AnyField {
            pattern: field.to_string(),
            text: text.to_string(),
            phrase,
        };
    }
    Selector::Analyzed {
        field: field.to_string(),
        text: text.to_string(),
        phrase,
    }
}

/// Which documents a query selects.
///
/// The vocabulary is bounded and stated: `match_all`, `term`, `terms`, `match`, `exists`,
/// `range`, `multi_match`, `query_string`, `ids` and a `bool` composing them. A clause
/// outside it is answered the way the engine answers a query name it has no handler
/// registered for, rather than silently selecting everything — a search whose clause this
/// emulator cannot evaluate must not look like one it did.
#[derive(Debug)]
pub(super) enum Selector {
    /// Every document.
    All,
    /// A field's value equals one of these, compared as the field's own mapping says its
    /// values ARE ([`equals`]) — which is how a `keyword` term, a `match` on a numeric field
    /// and a term naming an instant in another of its forms all behave.
    Equals { field: String, values: Vec<String> },
    /// A field's text, ANALYZED, shares a term with this text analyzed the same way: what a
    /// `match` asks, and what a `match_phrase` asks of the terms in order (`phrase`).
    Analyzed {
        field: String,
        text: String,
        phrase: bool,
    },
    /// The document's id is one of these.
    Ids(Vec<String>),
    /// The document holds a value at this field at all: what an `exists` asks.
    Exists { field: String },
    /// A value the document holds at this field falls within every one of these bounds,
    /// compared as the field's own mapping says its values ARE ([`Comparable`]): what a
    /// `range` asks.
    Range {
        field: String,
        bounds: Vec<(Limit, Value)>,
    },
    /// The same question as [`Self::Analyzed`], asked of every field of the index whose name
    /// matches this PATTERN — what a clause naming no field of its own asks, and what a
    /// `multi_match` naming a pattern asks.
    ///
    /// Expanded against the mappings of the indexes the request reads before the scan begins
    /// ([`Selector::expanded`]), because which fields a pattern names is a property of those
    /// mappings and not of the query; an unexpanded one names no field, so it selects no
    /// document.
    AnyField {
        pattern: String,
        text: String,
        phrase: bool,
    },
    /// Every one of these selects the document.
    All_(Vec<Selector>),
    /// None of these may select the document.
    None_(Vec<Selector>),
    /// Any one of these selects the document.
    Any(Vec<Selector>),
}

impl Selector {
    /// Whether this selector selects the document `id`/`source`, as the index's own
    /// analysis chain turns that document's text into terms (`terms`).
    pub(super) fn matches(&self, id: &str, source: &Value, terms: &Terms) -> bool {
        match self {
            Self::All => true,
            Self::Equals { field, values } => equals(terms, source, field, values),
            Self::Exists { field } => terms
                .held(source, field)
                .iter()
                .any(|held| !held.is_null()),
            Self::Range { field, bounds } => {
                let kind = terms.mapped_type(field);
                terms.held(source, field).iter().any(|held| {
                    Comparable::of(kind, held).is_some_and(|held| {
                        bounds.iter().all(|(limit, bound)| {
                            Comparable::of(kind, bound)
                                .is_some_and(|bound| limit.admits(&held, &bound))
                        })
                    })
                })
            }
            // A pattern that was never expanded names no field of any index, so there is no
            // value for it to ask its question of.
            Self::AnyField { .. } => false,
            Self::Analyzed {
                field,
                text,
                phrase,
            } => {
                // A field the index holds its values WHOLE for is not analyzed, so the
                // clause is the equality a `term` on it would be.
                let Some(wanted) = terms.query(field, text) else {
                    return equals(terms, source, field, ::std::slice::from_ref(text));
                };
                if wanted.is_empty() {
                    // Every term of the query was dropped by the chain — a text of nothing
                    // but stop words — so there is no term left for a document to carry.
                    return false;
                }
                terms.texts(source, field).iter().any(|held| {
                    let held = terms.document(field, held).unwrap_or_default();
                    if *phrase {
                        held.windows(wanted.len()).any(|run| run == wanted)
                    } else {
                        wanted.iter().any(|term| held.contains(term))
                    }
                })
            }
            Self::Ids(ids) => ids.iter().any(|wanted| wanted == id),
            Self::All_(clauses) => clauses
                .iter()
                .all(|clause| clause.matches(id, source, terms)),
            Self::None_(clauses) => !clauses
                .iter()
                .any(|clause| clause.matches(id, source, terms)),
            Self::Any(clauses) => clauses
                .iter()
                .any(|clause| clause.matches(id, source, terms)),
        }
    }

    /// Collect the clauses this selector asks an ANALYZED question with — the field each
    /// names and the text it asks about — so each index resolves its chains and analyzes
    /// each of those texts once before its documents are scanned.
    pub(super) fn analyzed_clauses(&self, into: &mut BTreeSet<(String, String)>) {
        match self {
            Self::Analyzed { field, text, .. } => {
                into.insert((field.clone(), text.clone()));
            }
            Self::All_(clauses) | Self::None_(clauses) | Self::Any(clauses) => {
                for clause in clauses {
                    clause.analyzed_clauses(into);
                }
            }
            Self::All | Self::Equals { .. } | Self::Ids(_) => {}
            // A `range` and an `exists` ask about the VALUE a field holds rather than about
            // the terms its text analyzes to, and an unexpanded pattern names no field.
            Self::Exists { .. } | Self::Range { .. } | Self::AnyField { .. } => {}
        }
    }

    /// This selector with every field PATTERN it carries expanded to the fields of `fields`
    /// it names — the mapped fields of the indexes the request reads — so the clause under it
    /// is asked of each of them.
    ///
    /// Performed once per request, before the per-index chains are resolved and before any
    /// document is scanned, and BOUNDED twice over, because the expansion is the one place
    /// where a request's per-document work could be multiplied by a factor the caller chose:
    ///
    /// * one pattern expands to at most [`MAX_MATCHED_FIELDS`] fields, whichever mappings the
    ///   indexes carry;
    /// * the clauses the WHOLE expansion produces are charged against `budget`, the engine's
    ///   own clause ceiling for the request ([`MAX_QUERY_CLAUSES`]), and a request past it is
    ///   REFUSED rather than expanded further — so a query naming many patterns is held to
    ///   exactly the ceiling a query naming those fields itself would be.
    pub(super) fn expanded(
        self,
        fields: &[String],
        budget: &mut usize,
    ) -> Result<Self, Box<RtResponse>> {
        let expand_each = |clauses: Vec<Self>, budget: &mut usize| {
            clauses
                .into_iter()
                .map(|one| one.expanded(fields, budget))
                .collect::<Result<Vec<Self>, Box<RtResponse>>>()
        };
        match self {
            Self::AnyField {
                pattern,
                text,
                phrase,
            } => {
                let mut expanded: Vec<Self> = Vec::new();
                for field in fields.iter().filter(|field| {
                    ::service_common::iam_authorization::wildcard_match(&pattern, field, true)
                }) {
                    // Checked BEFORE the clause is materialised, so neither the tree this
                    // holds nor the per-document work walking it is sized by the pattern the
                    // request sent or by the mappings a caller wrote.
                    if expanded.len() >= MAX_MATCHED_FIELDS || *budget == 0 {
                        return Err(too_many_clauses());
                    }
                    *budget = budget.saturating_sub(1);
                    expanded.push(Self::Analyzed {
                        field: field.clone(),
                        text: text.clone(),
                        phrase,
                    });
                }
                Ok(Self::Any(expanded))
            }
            Self::All_(clauses) => Ok(Self::All_(expand_each(clauses, budget)?)),
            Self::None_(clauses) => Ok(Self::None_(expand_each(clauses, budget)?)),
            Self::Any(clauses) => Ok(Self::Any(expand_each(clauses, budget)?)),
            other => Ok(other),
        }
    }

    /// The first bound this selector carries that the field it is compared against reads no
    /// value of its own type from, or `None` where every bound names a value of its field's
    /// type.
    ///
    /// A bound outside the format set of the field it names states no limit at all, so the
    /// request is REFUSED rather than answered from a comparison nothing could take. Asked
    /// per index, because it is that index's own mappings that decide what the field's values
    /// are.
    pub(super) fn unreadable_bound(
        &self,
        analysis: &state::analysis::IndexAnalysis,
    ) -> Option<String> {
        match self {
            Self::Range { field, bounds } => {
                let kind = analysis.mapped_type(field)?;
                if !state::date_values::is_date_type(kind) {
                    return None;
                }
                bounds
                    .iter()
                    .find(|(_, bound)| state::date_values::instant_millis(bound).is_none())
                    .map(|(_, bound)| scalar_text(bound))
            }
            Self::All_(clauses) | Self::None_(clauses) | Self::Any(clauses) => clauses
                .iter()
                .find_map(|clause| clause.unreadable_bound(analysis)),
            _ => None,
        }
    }

    /// Collect what this query asks of each FIELD, as this index's chains analyze it: the
    /// terms a fragment on that field is wrapped around.
    ///
    /// A clause asking an analyzed question contributes the terms its text produces through
    /// the field's own search chain, and one asking for a value whole contributes that value;
    /// a `must_not` clause contributes nothing, because a document is selected by NOT
    /// carrying its terms, and an `ids` clause names no field at all.
    pub(super) fn wanted(&self, terms: &Terms, into: &mut Wanted) {
        match self {
            Self::All | Self::Ids(_) | Self::None_(_) => {}
            // Neither asks about a TERM of a field, so neither wraps one in a fragment.
            Self::Exists { .. } | Self::Range { .. } | Self::AnyField { .. } => {}
            Self::Equals { field, values } => {
                into.entry(field.clone())
                    .or_default()
                    .extend(values.iter().cloned());
            }
            Self::Analyzed { field, text, .. } => {
                let asked = into.entry(field.clone()).or_default();
                match terms.query(field, text) {
                    Some(analyzed) => asked.extend(analyzed.iter().cloned()),
                    None => {
                        asked.insert(text.clone());
                    }
                }
            }
            Self::All_(clauses) | Self::Any(clauses) => {
                for clause in clauses {
                    clause.wanted(terms, into);
                }
            }
        }
    }

    /// How many clauses this selector is EVALUATED as: every one of them is asked of every
    /// document the scan reaches, so this is the breadth [`MAX_QUERY_CLAUSES`] bounds.
    ///
    /// Every clause counts, whatever it selects: a clause that selects every document is
    /// still re-asked per document, so one that counted nothing would let a body compose as
    /// many of them as it liked and multiply the scan by a number the caller chose. A `bool`
    /// composition counts the clauses under it rather than itself, as the engine's own
    /// ceiling counts them; what bounds the compositions THEMSELVES is the number of clauses
    /// one body may name at all ([`MAX_QUERY_CLAUSE_NODES`], counted as the query is read).
    pub(super) fn clauses(&self) -> usize {
        match self {
            Self::All
            | Self::Equals { .. }
            | Self::Analyzed { .. }
            | Self::Exists { .. }
            | Self::Range { .. }
            | Self::AnyField { .. }
            | Self::Ids(_) => 1,
            Self::All_(clauses) | Self::None_(clauses) | Self::Any(clauses) => clauses
                .iter()
                .map(Self::clauses)
                .fold(0_usize, usize::saturating_add),
        }
    }
}

/// The scalar values a document holds at `field`, which may be a dotted path into nested
/// objects and may hold an array of values.
pub(super) fn held_values(source: &Value, field: &str) -> Vec<Value> {
    let mut node = source;
    for segment in field.split('.') {
        match node.get(segment) {
            Some(next) => node = next,
            None => return Vec::new(),
        }
    }
    match node {
        Value::Array(elements) => elements.clone(),
        other => vec![other.clone()],
    }
}

/// A scalar's text, which is what a term is compared against.
pub(super) fn scalar_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// Whether the document `source` holds, at `field`, a value EQUAL to one of `wanted`.
///
/// Compared as the field's own mapping says its values ARE, not as they are written: a field
/// mapped as an instant compares the instants both sides parse to, so one value written as a
/// date and the same value written as epoch milliseconds are the same value — while every
/// other field compares the value's text, which is what makes a number and its decimal
/// spelling one value.
fn equals(terms: &Terms, source: &Value, field: &str, wanted: &[String]) -> bool {
    if terms
        .mapped_type(field)
        .is_some_and(state::date_values::is_date_type)
    {
        let held: Vec<i64> = terms
            .held(source, field)
            .iter()
            .filter_map(state::date_values::instant_millis)
            .collect();
        return wanted
            .iter()
            .filter_map(|wanted| state::date_values::parsed_millis(wanted))
            .any(|wanted| held.contains(&wanted));
    }
    terms
        .texts(source, field)
        .iter()
        .any(|held| wanted.iter().any(|wanted| wanted == held))
}

/// One end of a `range`: which side of the bound a value has to be on, and whether the bound
/// itself is admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Limit {
    GreaterThan,
    AtLeast,
    LessThan,
    AtMost,
}

impl Limit {
    /// The member a `range` writes this limit as.
    fn member(self) -> &'static str {
        match self {
            Self::GreaterThan => "gt",
            Self::AtLeast => "gte",
            Self::LessThan => "lt",
            Self::AtMost => "lte",
        }
    }

    /// The four members a `range` may carry, which is the whole of its vocabulary.
    fn all() -> [Self; 4] {
        [Self::GreaterThan, Self::AtLeast, Self::LessThan, Self::AtMost]
    }

    /// Whether `held` is on the side of `bound` this limit admits.
    fn admits(self, held: &Comparable, bound: &Comparable) -> bool {
        let Some(ordering) = held.order(bound) else {
            return false;
        };
        match self {
            Self::GreaterThan => ordering == ::std::cmp::Ordering::Greater,
            Self::AtLeast => ordering != ::std::cmp::Ordering::Less,
            Self::LessThan => ordering == ::std::cmp::Ordering::Less,
            Self::AtMost => ordering != ::std::cmp::Ordering::Greater,
        }
    }
}

/// A value as the field's own mapping says it COMPARES: a magnitude, or text.
///
/// The mapping is what decides which: a field mapped as an instant compares the instant its
/// value parses to, a field mapped as a number compares the number, and a field holding text
/// compares its code points — so `2026-01-01` and `1767225600000` order as one instant on a
/// date field while `10` sorts before `5` on a keyword field, exactly as the engine does it.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Comparable {
    Magnitude(f64),
    Text(String),
}

impl Comparable {
    /// The mapped field types whose values are magnitudes: the engine's own numeric types
    /// ("Numeric field types"), which a range on such a field compares as numbers.
    const NUMERIC_TYPES: [&'static str; 9] = [
        "long",
        "integer",
        "short",
        "byte",
        "double",
        "float",
        "half_float",
        "scaled_float",
        "unsigned_long",
    ];

    /// How `value` compares on a field mapped as `kind`, or `None` for a value that field
    /// reads no value of its type from.
    ///
    /// A field the mappings map at all decides by its TYPE; one they map not at all — a field
    /// a write mapped dynamically — decides by the value itself, which is what that write
    /// would have mapped it as.
    fn of(kind: Option<&str>, value: &Value) -> Option<Self> {
        match kind {
            Some(kind) if state::date_values::is_date_type(kind) => {
                #[allow(clippy::cast_precision_loss)]
                state::date_values::instant_millis(value).map(|at| Self::Magnitude(at as f64))
            }
            Some(kind) if Self::NUMERIC_TYPES.contains(&kind) => Self::magnitude(value),
            Some(_) => Some(Self::Text(scalar_text(value))),
            None if value.is_number() => Self::magnitude(value),
            None => (!value.is_null()).then(|| Self::Text(scalar_text(value))),
        }
    }

    /// The magnitude `value` holds — a number, or text a number reads from, since a numeric
    /// field coerces a string by default.
    fn magnitude(value: &Value) -> Option<Self> {
        match value {
            Value::Number(number) => number.as_f64().map(Self::Magnitude),
            Value::String(text) => text.trim().parse().ok().map(Self::Magnitude),
            _ => None,
        }
    }

    /// How two of these order, or `None` where neither reads the other's kind: a magnitude
    /// against text a number reads from is compared as numbers, which is how a bound written
    /// as a string is compared against a numeric field.
    fn order(&self, other: &Self) -> Option<::std::cmp::Ordering> {
        match (self, other) {
            (Self::Magnitude(held), Self::Magnitude(bound)) => held.partial_cmp(bound),
            (Self::Text(held), Self::Text(bound)) => Some(held.cmp(bound)),
            (Self::Magnitude(held), Self::Text(bound)) => {
                held.partial_cmp(&bound.trim().parse().ok()?)
            }
            (Self::Text(held), Self::Magnitude(bound)) => {
                held.trim().parse::<f64>().ok()?.partial_cmp(bound)
            }
        }
    }
}

/// The selector the query clause `query` states, read out of the body text `source` — which
/// a refusal names the POSITION in ([`Counted::source`]), and which code reading a query from
/// somewhere other than a request body has none of.
///
/// The clauses are counted AS THEY ARE READ, against the number one body may name at all
/// ([`MAX_QUERY_CLAUSE_NODES`]): a body past it is refused while it is being read rather than
/// materialised first, so neither the tree this holds nor the per-document work walking it
/// causes is sized by the body a caller sent. A `bool` composition counts here even though
/// the engine's own clause ceiling does not count it ([`Selector::clauses`]), because walking
/// its list is work every document pays for.
pub(super) fn selector(
    query: Option<&Value>,
    depth: usize,
    source: Option<&str>,
) -> Result<Selector, Box<RtResponse>> {
    let mut counted = Counted {
        source,
        ..Counted::default()
    };
    read_selector(query, depth, &mut counted)
}

/// What one reading of a query body has counted: the clauses the scan will EVALUATE per
/// document, and the clause objects the body names at all.
///
/// Two counts because the two ceilings are different things. The engine's own
/// `max_clause_count` counts the clauses a composition holds and not the composition itself,
/// which is what [`MAX_QUERY_CLAUSES`] is; walking a composition's own lists is still work
/// every document pays for, so the objects are counted too, against the emulator's own
/// [`MAX_QUERY_CLAUSE_NODES`].
#[derive(Debug, Default)]
pub(super) struct Counted<'s> {
    pub(super) evaluated: usize,
    pub(super) named: usize,
    /// The request body AS SENT, where this query was read out of one: what a parse failure
    /// takes the position it names from.
    pub(super) source: Option<&'s str>,
}

impl Counted<'_> {
    /// Count one clause OBJECT the body names, refusing past what one body may name.
    pub(super) fn named(&mut self) -> Result<(), Box<RtResponse>> {
        self.named = self.named.saturating_add(1);
        if self.named > MAX_QUERY_CLAUSE_NODES {
            return Err(Box::new(error(
                400,
                ingest::ILLEGAL_ARGUMENT_EXCEPTION,
                &format!("a query may name at most {MAX_QUERY_CLAUSE_NODES} clauses"),
            )));
        }
        Ok(())
    }

    /// Count one clause the scan asks of every document, refusing past the engine's own
    /// ceiling.
    pub(super) fn evaluated(&mut self) -> Result<(), Box<RtResponse>> {
        self.evaluated = self.evaluated.saturating_add(1);
        if self.evaluated > MAX_QUERY_CLAUSES {
            return Err(too_many_clauses());
        }
        Ok(())
    }
}

/// The refusal a query naming more clauses than the engine evaluates earns, worded as the
/// engine words it.
pub(super) fn too_many_clauses() -> Box<RtResponse> {
    Box::new(error(
        400,
        TOO_MANY_CLAUSES,
        &format!("maxClauseCount is set to {MAX_QUERY_CLAUSES}"),
    ))
}

/// The selector a query clause states, `depth` levels down a `bool` composition, with
/// `counted` carrying what the whole walk has read so far.
pub(super) fn read_selector(
    query: Option<&Value>,
    depth: usize,
    counted: &mut Counted<'_>,
) -> Result<Selector, Box<RtResponse>> {
    counted.named()?;
    // The engine's own bound on how far a `bool` composition may nest
    // (`indices.query.bool.max_nested_depth`), which is what keeps the walk below from
    // recursing on a depth the caller chose.
    if depth > MAX_BOOL_NESTING {
        return Err(Box::new(error(
            400,
            ingest::ILLEGAL_ARGUMENT_EXCEPTION,
            &format!(
                "The nested depth of the query exceeds the maximum nested depth for bool \
                 queries [{MAX_BOOL_NESTING}]"
            ),
        )));
    }
    let Some(query) = query else {
        counted.evaluated()?;
        return Ok(Selector::All);
    };
    let Value::Object(clause) = query else {
        return Err(Box::new(error(
            400,
            PARSING_EXCEPTION,
            "failed to parse: expected a query to be a JSON object",
        )));
    };
    let Some((name, value)) = clause.iter().next() else {
        counted.evaluated()?;
        return Ok(Selector::All);
    };
    // Every clause but a composition is one the scan asks of each document — including one
    // that selects every document, which is asked as often as any other.
    if name != "bool" {
        counted.evaluated()?;
    }
    match name.as_str() {
        "match_all" => Ok(Selector::All),
        // A `term` asks for the field's value exactly as it is stored; a `match` and a
        // `match_phrase` ask the question of the TERMS the field's analyzer produces, which
        // is what makes an index's own analysis settings decide what a query finds.
        "term" => {
            let (field, wanted) = single_field(name, value)?;
            let wanted = wanted
                .get("value")
                .or_else(|| wanted.get("query"))
                .unwrap_or(&wanted)
                .clone();
            Ok(Selector::Equals {
                field,
                values: vec![scalar_text(&wanted)],
            })
        }
        "match" | "match_phrase" => {
            let (field, wanted) = single_field(name, value)?;
            let wanted = wanted
                .get("query")
                .or_else(|| wanted.get("value"))
                .unwrap_or(&wanted)
                .clone();
            Ok(Selector::Analyzed {
                field,
                text: scalar_text(&wanted),
                phrase: name == "match_phrase",
            })
        }
        "terms" => {
            let (field, wanted) = single_field(name, value)?;
            let values = match wanted {
                Value::Array(elements) => {
                    // The engine's own ceiling on how many terms ONE such clause may name,
                    // past which it refuses the query rather than materialising the array
                    // and comparing every one of them against every document.
                    if elements.len() > MAX_TERMS_COUNT {
                        return Err(Box::new(error(
                            400,
                            ingest::ILLEGAL_ARGUMENT_EXCEPTION,
                            &format!(
                                "The number of terms [{}] used in the Terms Query request \
                                 has exceeded the allowed maximum of [{MAX_TERMS_COUNT}]. \
                                 This maximum can be set by changing the \
                                 [index.max_terms_count] index level setting.",
                                elements.len()
                            ),
                        )));
                    }
                    elements.iter().map(scalar_text).collect()
                }
                other => vec![scalar_text(&other)],
            };
            Ok(Selector::Equals { field, values })
        }
        // An `exists` asks about the document rather than about a field's terms: whether it
        // holds a value at the field at all.
        "exists" => {
            let Some(field) = value.get(EXISTS_FIELD).and_then(Value::as_str) else {
                return Err(Box::new(error(
                    400,
                    PARSING_EXCEPTION,
                    &format!("[{name}] must be provided with a [{EXISTS_FIELD}]"),
                )));
            };
            Ok(Selector::Exists {
                field: field.to_string(),
            })
        }
        // A `range` asks which SIDE of each bound it names the field's value is on, compared
        // as that field's own mapping says its values are ([`Comparable`]).
        "range" => {
            let (field, bounds) = single_field(name, value)?;
            let Value::Object(members) = &bounds else {
                return Err(Box::new(error(
                    400,
                    PARSING_EXCEPTION,
                    &format!("[{name}] query must be an object"),
                )));
            };
            // A member outside the vocabulary this surface READS is refused rather than
            // ignored: a `format` or a `time_zone` silently dropped would compare the bound
            // under a reading the caller did not ask for.
            if let Some(unread) = members
                .keys()
                .find(|member| !RANGE_MEMBERS.contains(&member.as_str()))
            {
                return Err(Box::new(error(
                    400,
                    PARSING_EXCEPTION,
                    &format!("[{name}] query does not support [{unread}]"),
                )));
            }
            let bounds = Limit::all()
                .into_iter()
                .filter_map(|limit| {
                    members
                        .get(limit.member())
                        .filter(|bound| !bound.is_null())
                        .map(|bound| (limit, bound.clone()))
                })
                .collect();
            Ok(Selector::Range { field, bounds })
        }
        // A `multi_match` asks ONE text of SEVERAL fields, and the document is selected by
        // any one of them carrying it — "best_fields", the type such a query takes by
        // default. A field entry naming a pattern asks it of every field that pattern names.
        "multi_match" => {
            let Some(text) = value.get(QUERY_MEMBER).map(scalar_text) else {
                return Err(Box::new(error(
                    400,
                    PARSING_EXCEPTION,
                    &format!("[{name}] query requires '{QUERY_MEMBER}' to be specified"),
                )));
            };
            let phrase = value
                .get(MATCH_TYPE_MEMBER)
                .and_then(Value::as_str)
                .is_some_and(|kind| kind.contains(PHRASE_MATCH_TYPE));
            let named: Vec<String> = match value.get(FIELDS_MEMBER) {
                // A `multi_match` naming no field asks every field the index maps, which is
                // what the default field pattern expands to.
                None | Some(Value::Null) => vec![ALL_FIELDS.to_string()],
                Some(Value::String(one)) => vec![named_field(one)],
                Some(Value::Array(listed)) if listed.len() <= MAX_MATCHED_FIELDS => listed
                    .iter()
                    .map(|entry| named_field(&scalar_text(entry)))
                    .collect(),
                Some(_) => {
                    return Err(Box::new(error(
                        400,
                        PARSING_EXCEPTION,
                        &format!(
                            "[{name}] query may name at most {MAX_MATCHED_FIELDS} fields"
                        ),
                    )))
                }
            };
            Ok(Selector::Any(
                named
                    .iter()
                    .map(|field| field_clause(field, &text, phrase))
                    .collect(),
            ))
        }
        // A `query_string` carries its clauses in the one query-string syntax this plane
        // reads ([`query_string`]), which is the same syntax the URI form of a search carries
        // in its `q` parameter.
        "query_string" => {
            let Some(text) = value.get(QUERY_MEMBER).map(scalar_text) else {
                return Err(Box::new(error(
                    400,
                    PARSING_EXCEPTION,
                    &format!("[{name}] query requires '{QUERY_MEMBER}' to be specified"),
                )));
            };
            query_string::parsed(
                &text,
                value
                    .get(DEFAULT_FIELD_MEMBER)
                    .and_then(Value::as_str)
                    .unwrap_or(ALL_FIELDS),
                query_string::Operator::named(
                    value.get(DEFAULT_OPERATOR_MEMBER).and_then(Value::as_str),
                )?,
            )
        }
        "ids" => {
            let listed = value.get("values").and_then(Value::as_array);
            // An `ids` clause names its ids one by one and every one of them is compared
            // against every document the scan reaches, so it is bounded the same way — by
            // the same count, stated as the emulator's own bound, since the engine words no
            // ceiling of its own for this clause.
            if listed.is_some_and(|values| values.len() > MAX_TERMS_COUNT) {
                return Err(Box::new(error(
                    400,
                    ingest::ILLEGAL_ARGUMENT_EXCEPTION,
                    &format!(
                        "an ids clause may name at most {MAX_TERMS_COUNT} ids, and this one \
                         names {}",
                        listed.map_or(0, Vec::len)
                    ),
                )));
            }
            Ok(Selector::Ids(
                listed
                    .map(|values| values.iter().map(scalar_text).collect())
                    .unwrap_or_default(),
            ))
        }
        "bool" => {
            // A `bool` composes the clause lists it is documented to compose, beside the
            // options that change how its own clauses are weighed and reported. A member
            // outside that vocabulary is REFUSED rather than ignored: a misspelled list
            // silently dropped would answer the documents of a composition the caller did
            // not write — every document, where the clause was meant to narrow them.
            if let Some(members) = value.as_object() {
                if let Some(unsupported) = members
                    .keys()
                    .find(|member| !BOOL_MEMBERS.contains(&member.as_str()))
                {
                    return Err(unknown_field(name, unsupported, counted.source));
                }
            }
            let clauses = |name: &str,
                           counted: &mut Counted<'_>|
             -> Result<Vec<Selector>, Box<RtResponse>> {
                let listed = match value.get(name) {
                    None => Vec::new(),
                    Some(Value::Array(elements)) => elements.clone(),
                    Some(one) => vec![one.clone()],
                };
                listed
                    .iter()
                    .map(|one| read_selector(Some(one), depth.saturating_add(1), counted))
                    .collect()
            };
            let mut required = clauses("must", counted)?;
            required.extend(clauses("filter", counted)?);
            let forbidden = clauses("must_not", counted)?;
            let optional = clauses("should", counted)?;
            let mut composed = vec![Selector::All_(required), Selector::None_(forbidden)];
            if !optional.is_empty() {
                composed.push(Selector::Any(optional));
            }
            Ok(Selector::All_(composed))
        }
        other => Err(unknown_query(other, counted.source)),
    }
}

/// The refusal a query naming a clause the engine has nothing registered for earns.
///
/// The engine reads a query clause as a NAMED OBJECT — the name decides which reader the
/// object is handed to — so the failure is the parse failure of the query, caused by the
/// lookup of that name finding nothing, and the cause names WHERE in the body the name was
/// read ([`body_position`]). A query read from somewhere other than a request body has no such
/// position and carries no cause.
fn unknown_query(name: &str, source: Option<&str>) -> Box<RtResponse> {
    let caused_by = source
        .and_then(|source| body_position::member_value_at(source, name))
        .map(|(line, column)| {
            ::serde_json::json!({
                "type": NAMED_OBJECT_NOT_FOUND_EXCEPTION,
                "reason": format!("[{line}:{column}] unknown field [{name}]"),
            })
        });
    Box::new(caused_error(
        400,
        PARSING_EXCEPTION,
        &format!("unknown query [{name}]"),
        caused_by,
    ))
}

/// The engine's error type for the lookup of a named object's name finding no reader for it.
const NAMED_OBJECT_NOT_FOUND_EXCEPTION: &str = "named_object_not_found_exception";

/// The refusal a clause carrying a member its own reader has no place for earns.
///
/// Such a reader reads a clause member by member, so what it reports is the FIELD it could
/// not place and the position that field's NAME sits at in the body as sent — a different
/// failure from a clause name nothing is registered for ([`unknown_query`]), which is why it
/// is worded and typed differently. A clause read from somewhere other than a request body
/// has no position to name and is refused without one.
fn unknown_field(clause: &str, field: &str, source: Option<&str>) -> Box<RtResponse> {
    let at = source.and_then(|source| body_position::member_name_at(source, field));
    let reason = match at {
        Some((line, column)) => {
            format!("[{line}:{column}] [{clause}] unknown field [{field}]")
        }
        None => format!("[{clause}] unknown field [{field}]"),
    };
    Box::new(error(400, X_CONTENT_PARSE_EXCEPTION, &reason))
}

/// The one field a `term`/`terms`/`match`/`range` clause names, and the value it wants.
///
/// Such a clause asks its question of ONE field — the one member of its own object — so a
/// clause naming two is refused, and the refusal names the clause and the two fields it found,
/// which is what tells a caller which member of its body to drop.
pub(super) fn single_field(
    clause: &str,
    value: &Value,
) -> Result<(String, Value), Box<RtResponse>> {
    let Value::Object(members) = value else {
        return Err(Box::new(error(
            400,
            PARSING_EXCEPTION,
            &format!("[{clause}] query must name exactly one field"),
        )));
    };
    let mut named = members.iter();
    match (named.next(), named.next()) {
        (Some((field, wanted)), None) => Ok((field.clone(), wanted.clone())),
        (Some((first, _)), Some((second, _))) => Err(Box::new(error(
            400,
            PARSING_EXCEPTION,
            &format!(
                "[{clause}] query doesn't support multiple fields, found [{first}] and \
                 [{second}]"
            ),
        ))),
        _ => Err(Box::new(error(
            400,
            PARSING_EXCEPTION,
            &format!("[{clause}] query does not support an empty field list"),
        ))),
    }
}
