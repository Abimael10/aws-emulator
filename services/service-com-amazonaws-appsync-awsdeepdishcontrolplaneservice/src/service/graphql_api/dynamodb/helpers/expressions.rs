// SPDX-License-Identifier: CC0-1.0

//! The EXPRESSIONS one request object of the DynamoDB helper module carries: the condition a
//! write holds over, the key condition a query selects by, the filter a selection applies, the
//! attributes a read projects and the update an `UpdateItem` makes.
//!
//! A helper is handed each of them as an OBJECT — one entry per attribute, whose value names
//! the operators it is compared under, with `and`, `or` and `not` grouping entries — and the
//! request object an `AMAZON_DYNAMODB` data source takes carries an expression TEXT beside the
//! two maps its names and values are parameterised by ([`super::super::Expression`]). This
//! module is that translation, and the only place it is made: every attribute name travels in
//! `expressionNames` under a placeholder of its own and every operand in `expressionValues`
//! under one of its own, so a name DynamoDB reserves, a name carrying a character its
//! expression language reads, and an operand of any type are all carried by the maps rather
//! than written into the text.
//!
//! What it does NOT do is decide whether the expression it built is one the call accepts: a
//! key condition may only compare the key attributes, a filter may not name them, and DynamoDB
//! itself is the authority on both — the expression is built as the resolver's code asked for
//! it and that service answers what it makes of it.

use ::std::collections::BTreeMap;

use ::serde_json::{Map as JsonMap, Value as Json};

use crate::service::graphql_api::mapping_template::utilities::dynamodb::{
    attribute_value, LIST, NUMBER,
};
use crate::service::graphql_api::mapping_template::weigh;

use super::super::{EXPRESSION_MEMBER, EXPRESSION_NAMES_MEMBER, EXPRESSION_VALUES_MEMBER};

/// How deeply the groups of one condition may nest, so a condition object built in a loop
/// cannot take the host's stack: a bound of this emulator's own, as every other ceiling the
/// APPSYNC_JS runtime holds a value to is.
const MAX_CONDITION_DEPTH: usize = 16;

/// The one expression object `stated` comes to — `{expression, expressionNames,
/// expressionValues}` — for a PREDICATE: a condition, a key condition or a filter, which are
/// one language and are therefore read in one place.
///
/// `member` is the member of the payload the predicate was stated in, which a refusal names.
pub(super) fn predicate(stated: &Json, member: &str) -> Result<Json, String> {
    written_predicate(stated, member, Dialect::Indexed)
}

/// The same predicate, written in the dialect the `$util.transform` helper AppSync publishes
/// answers it in ([`Dialect::Published`]) rather than in the request object's own.
///
/// The one place that helper's answer is built, so the filter language has ONE reading here
/// whichever of the two forms it is written out in.
pub(super) fn published_predicate(stated: &Json, member: &str) -> Result<Json, String> {
    written_predicate(stated, member, Dialect::Published)
}

/// See [`predicate`] and [`published_predicate`]: one walk of the language, over the dialect
/// the caller's own answer is written in.
fn written_predicate(stated: &Json, member: &str, dialect: Dialect) -> Result<Json, String> {
    let mut parameters = Parameters {
        dialect,
        ..Parameters::default()
    };
    let expression = conjunction(&mut parameters, stated, member, 0)?;
    if expression.is_empty() {
        return Err(format!("{A_PREDICATE_STATES} {member}."));
    }
    Ok(built(expression, parameters))
}

/// The expression object the attributes `stated` lists are PROJECTED by: each of them under a
/// placeholder of its own, which is how a read asks for a subset of an item's attributes.
pub(super) fn projection(stated: &Json, member: &str) -> Result<Json, String> {
    let Some(attributes) = stated.as_array() else {
        return Err(format!("{A_PROJECTION_IS_A_LIST} {member}."));
    };
    if attributes.is_empty() {
        return Err(format!("{A_PROJECTION_NAMES_AN_ATTRIBUTE} {member}."));
    }
    let mut parameters = Parameters::default();
    let mut written = Vec::with_capacity(attributes.len());
    for attribute in attributes {
        let Some(path) = attribute.as_str() else {
            return Err(format!("{A_PROJECTED_ATTRIBUTE_IS_A_NAME} {member}."));
        };
        written.push(parameters.path(path, member)?);
    }
    Ok(built(written.join(", "), parameters))
}

/// The expression object an `UpdateItem` request's `update` member comes to: one clause per
/// attribute the update states, gathered into the sections DynamoDB's own update language
/// writes them in.
///
/// An attribute the update assigns a plain value to is SET to that value; one it assigns an
/// operation of the module's own ([`super::OPERATION_MARKER`]) is written as the clause that
/// operation performs, in the section that clause belongs to.
pub(super) fn update(stated: &Json, member: &str) -> Result<Json, String> {
    let Some(assignments) = stated.as_object() else {
        return Err(format!("{AN_UPDATE_IS_A_MAP} {member}."));
    };
    if assignments.is_empty() {
        return Err(format!("{AN_UPDATE_STATES_AN_ATTRIBUTE} {member}."));
    }
    let mut parameters = Parameters::default();
    let mut sections: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for (attribute, assigned) in assignments {
        let path = parameters.path(attribute, member)?;
        let (section, clause) = clause_of(&mut parameters, &path, assigned, member)?;
        sections.entry(section).or_default().push(clause);
    }
    // The sections in the order DynamoDB's own update language writes them, each carrying
    // every clause of its kind.
    let expression = UPDATE_SECTIONS
        .iter()
        .filter_map(|section| {
            let clauses = sections.get(section)?;
            Some(format!("{section} {}", clauses.join(", ")))
        })
        .collect::<Vec<String>>()
        .join(" ");
    Ok(built(expression, parameters))
}

/// The sections an update expression is written in, in the order they are written.
const UPDATE_SECTIONS: &[&str] = &["SET", "ADD", "REMOVE"];

/// The section and the clause one assignment of an update comes to.
fn clause_of(
    parameters: &mut Parameters,
    path: &str,
    assigned: &Json,
    member: &str,
) -> Result<(&'static str, String), String> {
    // An update's operands belong to no comparison of an attribute, so nothing names their
    // placeholders after one: the pair a comparison leaves behind is cleared here rather than
    // left to be read by the next operand ([`Parameters::marshalled`]).
    parameters.subject = None;
    parameters.operator = None;
    let Some((operation, arguments)) = super::operation_stated(assigned) else {
        // An attribute assigned a plain value is SET to it, which is what an update of a
        // value rather than of a change states.
        return Ok(("SET", format!("{path} = {}", parameters.value(assigned)?)));
    };
    let operand = |index: usize| arguments.get(index).cloned().unwrap_or(Json::Null);
    let clause = match operation {
        // "replace" assigns the value as it stands, which is what an update states for an
        // attribute whose new value does not depend on the stored one.
        "replace" => ("SET", format!("{path} = {}", parameters.value(&operand(0))?)),
        // An increment and a decrement are arithmetic ON the stored value, counted from zero
        // where the item carries no such attribute yet — so the first increment of an absent
        // attribute writes the amount rather than failing on a value that is not there.
        "increment" => (
            "SET",
            format!(
                "{path} = if_not_exists({path}, {}) + {}",
                parameters.marshalled(zero()),
                parameters.value(&operand(0))?
            ),
        ),
        "decrement" => (
            "SET",
            format!(
                "{path} = if_not_exists({path}, {}) - {}",
                parameters.marshalled(zero()),
                parameters.value(&operand(0))?
            ),
        ),
        // Appending and prepending concatenate a list with the stored one, which is the empty
        // list where the item carries no such attribute yet.
        "append" => (
            "SET",
            format!(
                "{path} = list_append(if_not_exists({path}, {}), {})",
                parameters.marshalled(empty_list()),
                parameters.value(&operand(0))?
            ),
        ),
        "prepend" => (
            "SET",
            format!(
                "{path} = list_append({}, if_not_exists({path}, {}))",
                parameters.value(&operand(0))?,
                parameters.marshalled(empty_list())
            ),
        ),
        // One element of a stored list, addressed by its position.
        "updateListItem" => {
            let index = operand(1)
                .as_u64()
                .ok_or_else(|| format!("{A_LIST_POSITION_IS_A_NUMBER} {member}."))?;
            (
                "SET",
                format!("{path}[{index}] = {}", parameters.value(&operand(0))?),
            )
        }
        // `ADD` adds a number to a number and an element to a set; `REMOVE` drops the
        // attribute itself.
        "add" => ("ADD", format!("{path} {}", parameters.value(&operand(0))?)),
        "remove" => ("REMOVE", path.to_string()),
        other => return Err(format!("{NO_SUCH_OPERATION}{other}")),
    };
    Ok(clause)
}

/// The number an increment and a decrement count from where the item carries no such attribute,
/// as the attribute value DynamoDB holds it as.
fn zero() -> Json {
    ::serde_json::json!({NUMBER: 0})
}

/// The list an append and a prepend concatenate with where the item carries no such attribute.
fn empty_list() -> Json {
    ::serde_json::json!({LIST: []})
}

/// Every entry of one condition object, joined with `AND` — which is how a condition stating
/// more than one thing reads, each of them having to hold.
fn conjunction(
    parameters: &mut Parameters,
    stated: &Json,
    member: &str,
    depth: usize,
) -> Result<String, String> {
    if depth > MAX_CONDITION_DEPTH {
        return Err(format!("{A_CONDITION_NESTS} {MAX_CONDITION_DEPTH} levels deep."));
    }
    let Some(entries) = stated.as_object() else {
        return Err(format!("{A_CONDITION_IS_A_MAP} {member}."));
    };
    let mut clauses = Vec::with_capacity(entries.len());
    for (named, stated) in entries {
        let clause = match named.as_str() {
            AND_GROUP => group(parameters, stated, "AND", member, depth)?,
            OR_GROUP => group(parameters, stated, "OR", member, depth)?,
            NOT_GROUP => {
                let negated = conjunction(parameters, stated, member, depth.saturating_add(1))?;
                match negated.is_empty() {
                    true => String::new(),
                    false => format!("NOT ({negated})"),
                }
            }
            attribute => compared(parameters, attribute, stated, member)?,
        };
        if !clause.is_empty() {
            clauses.push(clause);
        }
    }
    Ok(clauses.join(" AND "))
}

/// A GROUP of conditions, joined with `joiner` and parenthesised, so the grouping the payload
/// states is the grouping the expression carries.
fn group(
    parameters: &mut Parameters,
    group: &Json,
    joiner: &str,
    member: &str,
    depth: usize,
) -> Result<String, String> {
    let Some(entries) = group.as_array() else {
        return Err(format!("{A_GROUP_IS_A_LIST} {member}."));
    };
    let mut clauses = Vec::with_capacity(entries.len());
    for entry in entries {
        let clause = conjunction(parameters, entry, member, depth.saturating_add(1))?;
        if !clause.is_empty() {
            clauses.push(clause);
        }
    }
    match clauses.is_empty() {
        true => Ok(String::new()),
        false => Ok(format!("({})", clauses.join(&format!(" {joiner} ")))),
    }
}

/// Every comparison one entry states about one attribute, joined with `AND`.
fn compared(
    parameters: &mut Parameters,
    attribute: &str,
    stated: &Json,
    member: &str,
) -> Result<String, String> {
    let path = parameters.path(attribute, member)?;
    let Some(operators) = stated.as_object() else {
        return Err(format!("{A_COMPARISON_NAMES_AN_OPERATOR} {member}."));
    };
    if operators.is_empty() {
        return Err(format!("{A_COMPARISON_NAMES_AN_OPERATOR} {member}."));
    }
    // The attribute every operand of these comparisons belongs to, which is half of what names
    // an operand's placeholder in the published dialect.
    parameters.subject = Some(attribute.to_string());
    let mut clauses = Vec::with_capacity(operators.len());
    for (operator, operand) in operators {
        clauses.push(clause(parameters, &path, operator, operand, member)?);
    }
    Ok(clauses.join(" AND "))
}

/// ONE comparison of one attribute: the operator the condition names, over the operand it
/// carries, written as the expression text that asks it.
///
/// The operators are the ones a DynamoDB expression compares under — the orderings, the range,
/// the prefix, the two containment questions, whether the item carries the attribute at all,
/// the type it holds, the length of what it holds and the membership of a list of values. An
/// operator this module publishes none of is refused, so a condition reaching for one fails
/// loudly rather than holding over every item.
fn clause(
    parameters: &mut Parameters,
    path: &str,
    operator: &str,
    operand: &Json,
    member: &str,
) -> Result<String, String> {
    let dialect = parameters.dialect;
    // The operator every operand of THIS comparison is compared under, which is the other half
    // of what names an operand's placeholder in the published dialect.
    parameters.operator = Some(operator.to_string());
    let compared = |parameters: &mut Parameters, symbol: &str| -> Result<String, String> {
        Ok(dialect.held(format!(
            "{path} {symbol} {}",
            parameters.value(operand)?
        )))
    };
    match operator {
        "eq" => compared(parameters, "="),
        "ne" => compared(parameters, "<>"),
        "lt" => compared(parameters, "<"),
        "le" => compared(parameters, "<="),
        "gt" => compared(parameters, ">"),
        "ge" => compared(parameters, ">="),
        "between" => {
            let Some([low, high]) = operand.as_array().and_then(|pair| match pair.as_slice() {
                [low, high] => Some([low.clone(), high.clone()]),
                _ => None,
            }) else {
                return Err(format!("{A_RANGE_IS_TWO_VALUES} {member}."));
            };
            Ok(dialect.held(format!(
                "{path} BETWEEN {} AND {}",
                parameters.value(&low)?,
                parameters.value(&high)?
            )))
        }
        "beginsWith" => Ok(dialect.held(dialect.called(
            "begins_with",
            path,
            &parameters.value(operand)?,
        ))),
        "contains" => Ok(dialect.held(dialect.called(
            "contains",
            path,
            &parameters.value(operand)?,
        ))),
        "notContains" => Ok(dialect.held(format!(
            "NOT {}",
            dialect.called("contains", path, &parameters.value(operand)?)
        ))),
        "attributeExists" => match operand.as_bool() {
            Some(true) => Ok(dialect.held(format!("attribute_exists({path})"))),
            Some(false) => Ok(dialect.held(format!("attribute_not_exists({path})"))),
            None => Err(format!("{AN_EXISTENCE_IS_A_FLAG} {member}.")),
        },
        "attributeType" => match operand.as_str() {
            Some(_) => Ok(dialect.held(dialect.called(
                "attribute_type",
                path,
                &parameters.value(operand)?,
            ))),
            None => Err(format!("{A_TYPE_IS_A_NAME} {member}.")),
        },
        "in" => {
            let Some(values) = operand.as_array().filter(|values| !values.is_empty()) else {
                return Err(format!("{A_MEMBERSHIP_IS_A_LIST} {member}."));
            };
            let mut written = Vec::with_capacity(values.len());
            for value in values {
                written.push(parameters.value(value)?);
            }
            Ok(dialect.held(format!("{path} IN ({})", written.join(", "))))
        }
        // The LENGTH of what the attribute holds, compared the way the attribute itself is:
        // the operand is a comparison of its own.
        "size" => {
            let sized = format!("size({path})");
            let Some(operators) = operand.as_object().filter(|stated| !stated.is_empty()) else {
                return Err(format!("{A_SIZE_NAMES_AN_OPERATOR} {member}."));
            };
            let mut clauses = Vec::with_capacity(operators.len());
            for (operator, inner) in operators {
                if operator == "size" {
                    return Err(format!("{A_SIZE_NAMES_AN_OPERATOR} {member}."));
                }
                clauses.push(clause(parameters, &sized, operator, inner, member)?);
            }
            Ok(clauses.join(" AND "))
        }
        other => Err(format!("{NO_SUCH_OPERATOR}{other}")),
    }
}

/// How one expression is WRITTEN OUT: the two forms this service produces of the one filter
/// language above.
///
/// The language is one — the same entries, the same operators, the same groups, read by the one
/// walk in this module — but the two answers it is written into are not the same document. The
/// `filter` of a REQUEST OBJECT is parameterised however this emulator likes, because nothing
/// publishes those placeholders; the answer the `$util.transform` helper gives a mapping
/// template IS published, down to the placeholder each name and operand travels under and the
/// parentheses around each comparison, so a template that reads the answer sees what the
/// reference states. Hence a dialect rather than a second translation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Dialect {
    /// The request object's own: placeholders numbered as they are minted, comparisons written
    /// bare.
    #[default]
    Indexed,
    /// The published transform helper's: the ATTRIBUTE names a name's placeholder and the
    /// attribute with the operator names an operand's, and every comparison is parenthesised.
    Published,
}

impl Dialect {
    /// One comparison as this dialect writes it.
    fn held(self, clause: String) -> String {
        match self {
            Self::Indexed => clause,
            Self::Published => format!("({clause})"),
        }
    }

    /// One call of a function of DynamoDB's expression language over an attribute and an
    /// operand, as this dialect writes it.
    fn called(self, function: &str, path: &str, operand: &str) -> String {
        match self {
            Self::Indexed => format!("{function}({path}, {operand})"),
            Self::Published => format!("{function}({path},{operand})"),
        }
    }
}

/// The names and the values ONE expression is parameterised by, as they are gathered while it
/// is written.
#[derive(Debug, Default)]
struct Parameters {
    /// `expressionNames`: the placeholder each attribute name travels under.
    names: JsonMap<String, Json>,
    /// `expressionValues`: the placeholder each operand travels under, already marshalled.
    values: JsonMap<String, Json>,
    /// The placeholder already bound to an attribute name, so one name that appears twice in
    /// an expression travels once.
    placeholders: BTreeMap<String, String>,
    /// How this expression is written out ([`Dialect`]).
    dialect: Dialect,
    /// How many placeholders each STEM has already minted, so the next free one is looked up
    /// rather than searched for: the published dialect names placeholders after the attribute
    /// and the operator, which two comparisons can share.
    minted: BTreeMap<String, usize>,
    /// The attribute the comparison being written is about, and the operator it compares
    /// under: what the published dialect names an operand's placeholder after.
    subject: Option<String>,
    operator: Option<String>,
}

impl Parameters {
    /// The text `path` is written as in an expression: every NAME of it under a placeholder of
    /// its own, with the positions it addresses written as they stand.
    ///
    /// A path is a name, the dot-separated names of a nested attribute, and the `[i]`
    /// subscripts of a position in a list — the document-path form DynamoDB's own expression
    /// language reads — so a resolver may hold a condition over a member of a map or an element
    /// of a list.
    fn path(&mut self, path: &str, member: &str) -> Result<String, String> {
        let mut written = String::new();
        for (index, segment) in path.split('.').enumerate() {
            let (name, subscripts) = read_segment(segment)
                .ok_or_else(|| format!("{AN_ATTRIBUTE_IS_A_PATH} {member}."))?;
            if index > 0 {
                written.push('.');
            }
            written.push_str(&self.name(name));
            written.push_str(&subscripts);
        }
        Ok(written)
    }

    /// The placeholder `name` travels under, which is the one it was already given where it
    /// appears in this expression more than once.
    fn name(&mut self, name: &str) -> String {
        if let Some(bound) = self.placeholders.get(name) {
            return bound.clone();
        }
        let placeholder = match self.dialect {
            Dialect::Indexed => format!("#{NAME_PLACEHOLDER_PREFIX}{}", self.placeholders.len()),
            // The published dialect names a placeholder after the attribute itself
            // (`#title`), so a name carrying a character a placeholder cannot is written with
            // that character replaced, and a name that would collide with another's takes the
            // next free one.
            Dialect::Published => {
                self.free('#', &placeholder_text(name, NAME_PLACEHOLDER_PREFIX))
            }
        };
        self.placeholders
            .insert(name.to_string(), placeholder.clone());
        self.names
            .insert(placeholder.clone(), Json::String(name.to_string()));
        placeholder
    }

    /// Bind `value` — a plain value the resolver's code handed the helper — to a placeholder of
    /// its own, MARSHALLED as the attribute value DynamoDB holds it as, and answer the
    /// placeholder.
    ///
    /// Every operand of an expression travels this way, in `expressionValues`, so the text an
    /// expression carries is decided by the resolver's own code and never by the values it was
    /// called with.
    fn value(&mut self, value: &Json) -> Result<String, String> {
        // The marshalled copy nests as deeply as its source and holds more than it, so a value
        // past either memory bound of a run is refused here rather than converted — the one
        // weighing every value of this runtime is held to.
        weigh(value, 0)?;
        Ok(self.marshalled(attribute_value(value)))
    }

    /// Bind an operand that is ALREADY an attribute value — one this module wrote itself, such
    /// as the zero an increment counts from — and answer its placeholder.
    fn marshalled(&mut self, value: Json) -> String {
        let placeholder = match self.dialect {
            Dialect::Indexed => format!(":{VALUE_PLACEHOLDER_PREFIX}{}", self.values.len()),
            // The published dialect names an operand's placeholder after the attribute and the
            // operator it is compared under (`:title_contains`).
            Dialect::Published => {
                let stem = match (self.subject.clone(), self.operator.clone()) {
                    (Some(subject), Some(operator)) => format!(
                        "{}_{operator}",
                        placeholder_text(&subject, VALUE_PLACEHOLDER_PREFIX)
                    ),
                    // An operand of no comparison of an attribute — one a section of an update
                    // states — is numbered, since there is nothing to name it after.
                    _ => format!("{VALUE_PLACEHOLDER_PREFIX}{}", self.values.len()),
                };
                self.free(':', &stem)
            }
        };
        self.values.insert(placeholder.clone(), value);
        placeholder
    }

    /// The next free placeholder of one stem: the stem itself the first time and the stem with
    /// the count of the ones already minted after it every time after, counted per stem so
    /// nothing is searched for.
    fn free(&mut self, prefix: char, stem: &str) -> String {
        let minted = self.minted.entry(stem.to_string()).or_insert(0);
        let at = *minted;
        *minted = at.saturating_add(1);
        match at {
            0 => format!("{prefix}{stem}"),
            taken => format!("{prefix}{stem}_{taken}"),
        }
    }
}

/// `name` as the text a published placeholder is named after: its own characters, with every
/// one a DynamoDB placeholder cannot carry replaced, so the name itself always travels in
/// `expressionNames` rather than in the expression text. A name made of nothing a placeholder
/// can carry falls back to `fallback`, which is what the indexed dialect names one by.
fn placeholder_text(name: &str, fallback: &str) -> String {
    let written: String = name
        .chars()
        .map(|character| match character.is_ascii_alphanumeric() || character == '_' {
            true => character,
            false => '_',
        })
        .collect();
    match written.trim_matches('_').is_empty() {
        true => fallback.to_string(),
        false => written,
    }
}

/// The name and the subscripts one segment of a document path carries, or `None` for a segment
/// that names no attribute at all.
fn read_segment(segment: &str) -> Option<(&str, String)> {
    let opened = segment.find('[').unwrap_or(segment.len());
    let (name, mut remaining) = segment.split_at(opened);
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let mut subscripts = String::new();
    while !remaining.is_empty() {
        let (stated, tail) = remaining.strip_prefix('[')?.split_once(']')?;
        let position: usize = stated.trim().parse().ok()?;
        subscripts.push_str(&format!("[{position}]"));
        remaining = tail;
    }
    Some((name, subscripts))
}

/// One expression object of a request: the text, beside the maps it is parameterised by — each
/// stated only where the expression has one, because a call carries no empty map.
fn built(expression: String, parameters: Parameters) -> Json {
    let mut object = JsonMap::new();
    object.insert(
        EXPRESSION_MEMBER.to_string(),
        Json::String(expression),
    );
    if !parameters.names.is_empty() {
        object.insert(
            EXPRESSION_NAMES_MEMBER.to_string(),
            Json::Object(parameters.names),
        );
    }
    if !parameters.values.is_empty() {
        object.insert(
            EXPRESSION_VALUES_MEMBER.to_string(),
            Json::Object(parameters.values),
        );
    }
    Json::Object(object)
}

/// The entries of a condition that GROUP other conditions rather than comparing an attribute.
const AND_GROUP: &str = "and";
const OR_GROUP: &str = "or";
const NOT_GROUP: &str = "not";

/// The prefixes the placeholders of one expression are numbered under.
const NAME_PLACEHOLDER_PREFIX: &str = "n";
const VALUE_PLACEHOLDER_PREFIX: &str = "v";

/// The sentences a payload this module cannot build an expression from is refused with.
///
/// Each names the rule the payload broke and ends in the member of the payload a client has to
/// correct; nothing of one payload's own values is written into them.
const A_PREDICATE_STATES: &str =
    "A condition of a DynamoDB request compares at least one attribute, and none is stated in";
const A_CONDITION_IS_A_MAP: &str =
    "A condition of a DynamoDB request is an object of attributes and groups, which is not what \
     is carried at";
const A_CONDITION_NESTS: &str = "The groups of a DynamoDB condition nest at most";
const A_GROUP_IS_A_LIST: &str =
    "An and/or group of a DynamoDB condition is a list of conditions, which is not what is \
     carried at";
const A_COMPARISON_NAMES_AN_OPERATOR: &str =
    "A condition compares an attribute under at least one named operator, and one entry names \
     none in";
const AN_ATTRIBUTE_IS_A_PATH: &str =
    "An attribute of a DynamoDB expression is named by its path, and one entry names none in";
const A_RANGE_IS_TWO_VALUES: &str =
    "A between comparison carries the two values it ranges over, which is not what is carried \
     at";
const AN_EXISTENCE_IS_A_FLAG: &str =
    "An attributeExists comparison is true or false, which is not what is carried at";
const A_TYPE_IS_A_NAME: &str =
    "An attributeType comparison names the type it asks for, which is not what is carried at";
const A_MEMBERSHIP_IS_A_LIST: &str =
    "An in comparison carries the values it admits as a non-empty list, which is not what is \
     carried at";
const A_SIZE_NAMES_AN_OPERATOR: &str =
    "A size comparison compares the length under a named operator of its own, and one names \
     none in";
const NO_SUCH_OPERATOR: &str = "No such comparison of a DynamoDB expression: ";
const A_PROJECTION_IS_A_LIST: &str =
    "A projection of a DynamoDB request is a list of attribute names, which is not what is \
     carried at";
const A_PROJECTION_NAMES_AN_ATTRIBUTE: &str =
    "A projection names at least one attribute, and none is stated in";
const A_PROJECTED_ATTRIBUTE_IS_A_NAME: &str =
    "Every attribute a projection names is text, which is not what is carried at";
const AN_UPDATE_IS_A_MAP: &str =
    "An update of a DynamoDB request is an object of the attributes it changes, which is not \
     what is carried at";
const AN_UPDATE_STATES_AN_ATTRIBUTE: &str =
    "An update changes at least one attribute, and none is stated in";
const A_LIST_POSITION_IS_A_NUMBER: &str =
    "An updateListItem operation states the position it writes as a whole number, which is not \
     what is carried at";
const NO_SUCH_OPERATION: &str = "No such update operation of a DynamoDB request: ";

#[cfg(test)]
mod tests {
    use ::serde_json::json;

    use super::*;

    /// RULE: a predicate is written as the expression text that asks it, with every attribute
    /// name and every operand carried by the maps the request is parameterised by rather than
    /// written into the text.
    #[test]
    fn a_predicate_is_the_expression_that_asks_it() {
        // The rows differ along the OPERATOR, the TYPE of the operand, how the entries are
        // grouped and how deep the path of the attribute runs.
        let rows: &[(&str, Json, Json)] = &[
            (
                "an equality over text",
                json!({"board": {"eq": "ideas"}}),
                json!({
                    "expression": "#n0 = :v0",
                    "expressionNames": {"#n0": "board"},
                    "expressionValues": {":v0": {"S": "ideas"}},
                }),
            ),
            (
                "an inequality over a number",
                json!({"votes": {"gt": 2}}),
                json!({
                    "expression": "#n0 > :v0",
                    "expressionNames": {"#n0": "votes"},
                    "expressionValues": {":v0": {"N": 2}},
                }),
            ),
            (
                "a range",
                json!({"votes": {"between": [1, 4]}}),
                json!({
                    "expression": "#n0 BETWEEN :v0 AND :v1",
                    "expressionNames": {"#n0": "votes"},
                    "expressionValues": {":v0": {"N": 1}, ":v1": {"N": 4}},
                }),
            ),
            (
                "a prefix",
                json!({"id": {"beginsWith": "N-"}}),
                json!({
                    "expression": "begins_with(#n0, :v0)",
                    "expressionNames": {"#n0": "id"},
                    "expressionValues": {":v0": {"S": "N-"}},
                }),
            ),
            (
                "the two containment questions",
                json!({"body": {"contains": "a", "notContains": "z"}}),
                json!({
                    "expression": "contains(#n0, :v0) AND NOT contains(#n0, :v1)",
                    "expressionNames": {"#n0": "body"},
                    "expressionValues": {":v0": {"S": "a"}, ":v1": {"S": "z"}},
                }),
            ),
            (
                "an existence that holds and one that does not",
                json!({"author": {"attributeExists": true}, "deleted": {"attributeExists": false}}),
                json!({
                    "expression": "attribute_exists(#n0) AND attribute_not_exists(#n1)",
                    "expressionNames": {"#n0": "author", "#n1": "deleted"},
                }),
            ),
            (
                "a type",
                json!({"votes": {"attributeType": "N"}}),
                json!({
                    "expression": "attribute_type(#n0, :v0)",
                    "expressionNames": {"#n0": "votes"},
                    "expressionValues": {":v0": {"S": "N"}},
                }),
            ),
            (
                "a membership",
                json!({"board": {"in": ["ideas", "todo"]}}),
                json!({
                    "expression": "#n0 IN (:v0, :v1)",
                    "expressionNames": {"#n0": "board"},
                    "expressionValues": {":v0": {"S": "ideas"}, ":v1": {"S": "todo"}},
                }),
            ),
            (
                "a length",
                json!({"body": {"size": {"le": 8}}}),
                json!({
                    "expression": "size(#n0) <= :v0",
                    "expressionNames": {"#n0": "body"},
                    "expressionValues": {":v0": {"N": 8}},
                }),
            ),
            (
                "an or group",
                json!({"or": [{"board": {"eq": "ideas"}}, {"board": {"eq": "todo"}}]}),
                json!({
                    "expression": "(#n0 = :v0 OR #n0 = :v1)",
                    "expressionNames": {"#n0": "board"},
                    "expressionValues": {":v0": {"S": "ideas"}, ":v1": {"S": "todo"}},
                }),
            ),
            (
                "an and group inside a negation",
                json!({"not": {"and": [{"board": {"eq": "ideas"}}, {"votes": {"lt": 2}}]}}),
                json!({
                    "expression": "NOT ((#n0 = :v0 AND #n1 < :v1))",
                    "expressionNames": {"#n0": "board", "#n1": "votes"},
                    "expressionValues": {":v0": {"S": "ideas"}, ":v1": {"N": 2}},
                }),
            ),
            (
                "a member of a nested map and an element of a list",
                json!({"meta.owner": {"eq": "ann"}, "tags[1]": {"eq": "red"}}),
                json!({
                    "expression": "#n0.#n1 = :v0 AND #n2[1] = :v1",
                    "expressionNames": {"#n0": "meta", "#n1": "owner", "#n2": "tags"},
                    "expressionValues": {":v0": {"S": "ann"}, ":v1": {"S": "red"}},
                }),
            ),
            (
                "a boolean and a list operand, which travel marshalled",
                json!({"pinned": {"eq": true}, "tags": {"eq": ["red"]}}),
                json!({
                    "expression": "#n0 = :v0 AND #n1 = :v1",
                    "expressionNames": {"#n0": "pinned", "#n1": "tags"},
                    "expressionValues": {
                        ":v0": {"BOOL": true},
                        ":v1": {"L": [{"S": "red"}]},
                    },
                }),
            ),
        ];
        for (case, stated, expected) in rows {
            let actual = predicate(stated, "condition");
            assert_eq!(
                actual.as_ref(),
                Ok(expected),
                "case {case:?}: expected {expected}, got {actual:?}"
            );
        }
    }

    /// RULE: a payload no expression can be built from is refused by the member to correct,
    /// rather than built into an expression that asks something else.
    #[test]
    fn a_predicate_that_cannot_be_built_is_refused() {
        let rows: &[(&str, Json)] = &[
            ("a condition that is not an object", json!("board = ideas")),
            ("a condition comparing nothing", json!({})),
            ("an entry naming no operator", json!({"board": "ideas"})),
            ("an entry naming no operator of this language", json!({"board": {"like": "i%"}})),
            ("a group that is not a list", json!({"or": {"board": {"eq": "ideas"}}})),
            ("a range that is not two values", json!({"votes": {"between": [1]}})),
            ("an existence that is not a flag", json!({"author": {"attributeExists": "yes"}})),
            ("a membership over no value at all", json!({"board": {"in": []}})),
            ("a length naming no operator", json!({"body": {"size": 8}})),
            ("an attribute naming no path", json!({"": {"eq": "ideas"}})),
            ("an element at no position", json!({"tags[x]": {"eq": "red"}})),
        ];
        for (case, stated) in rows {
            let actual = predicate(stated, "condition");
            assert!(
                actual.is_err(),
                "case {case:?}: expected the payload to be refused, got {actual:?}"
            );
        }
    }

    /// RULE: every update operation of the module is written as the clause that performs it, in
    /// the section of the update language that clause belongs to.
    #[test]
    fn an_update_is_the_clauses_its_operations_perform() {
        let operation = |name: &str, arguments: Json| {
            let marker = super::super::OPERATION_MARKER;
            let stated = super::super::OPERATION_ARGUMENTS;
            json!({marker: name, stated: arguments})
        };
        let rows: &[(&str, Json, &str)] = &[
            ("a plain value", json!({"body": "second"}), "SET #n0 = :v0"),
            (
                "a replacement",
                json!({"body": operation("replace", json!(["second"]))}),
                "SET #n0 = :v0",
            ),
            (
                "an increment",
                json!({"votes": operation("increment", json!([2]))}),
                "SET #n0 = if_not_exists(#n0, :v0) + :v1",
            ),
            (
                "a decrement",
                json!({"votes": operation("decrement", json!([1]))}),
                "SET #n0 = if_not_exists(#n0, :v0) - :v1",
            ),
            (
                "an append",
                json!({"tags": operation("append", json!([["red"]]))}),
                "SET #n0 = list_append(if_not_exists(#n0, :v0), :v1)",
            ),
            (
                "a prepend",
                json!({"tags": operation("prepend", json!([["red"]]))}),
                "SET #n0 = list_append(:v0, if_not_exists(#n0, :v1))",
            ),
            (
                "an element of a list",
                json!({"tags": operation("updateListItem", json!(["red", 1]))}),
                "SET #n0[1] = :v0",
            ),
            (
                "an add",
                json!({"votes": operation("add", json!([1]))}),
                "ADD #n0 :v0",
            ),
            (
                "a removal",
                json!({"body": operation("remove", json!([]))}),
                "REMOVE #n0",
            ),
            (
                "a set and a removal together, in the sections' own order",
                json!({
                    "body": "second",
                    "votes": operation("remove", json!([])),
                }),
                "SET #n0 = :v0 REMOVE #n1",
            ),
        ];
        for (case, stated, expected) in rows {
            let built = update(stated, "update").unwrap_or_else(|refusal| {
                panic!("case {case:?}: the update must be built, got {refusal:?}")
            });
            let actual = built.get(EXPRESSION_MEMBER).and_then(Json::as_str);
            assert_eq!(
                actual,
                Some(*expected),
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: a projection names every attribute it reads under a placeholder of its own.
    #[test]
    fn a_projection_names_each_attribute_it_reads() {
        let built = projection(&json!(["id", "meta.owner"]), "projection")
            .unwrap_or_else(|refusal| panic!("the projection must be built, got {refusal:?}"));
        assert_eq!(
            built,
            json!({
                "expression": "#n0, #n1.#n2",
                "expressionNames": {"#n0": "id", "#n1": "meta", "#n2": "owner"},
            }),
            "case a projection of a plain and a nested attribute: got {built}"
        );
        for (case, stated) in [
            ("a projection that is not a list", json!("id")),
            ("a projection naming no attribute", json!([])),
            ("a projection naming something that is not text", json!([3])),
        ] {
            let actual = projection(&stated, "projection");
            assert!(
                actual.is_err(),
                "case {case:?}: expected the projection to be refused, got {actual:?}"
            );
        }
    }
}
