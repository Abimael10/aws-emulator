// SPDX-License-Identifier: CC0-1.0

//! The value domain one openCypher evaluation works in, the expressions it evaluates,
//! and how a pattern is matched against the stored graph.
//!
//! Everything here READS: it is handed the store ([`PropertyGraph`]) and the parameters
//! the request carried, and answers what the parsed query says about them. The clauses
//! that WRITE live in [`super`], which evaluates with the reader below first and then
//! applies the change — so no expression can mutate the graph as a side effect of being
//! evaluated.
//!
//! The query itself is never read as text: every decision below is taken from the typed
//! AST the `opencypher` crate's parser produced, so what the query MEANS is the parser's
//! and what it does to the graph is this module's and [`super`]'s.
//!
//! Every step charges the evaluation's budget ([`super::Budget`]) before it does the work
//! it pays for, which is what bounds the scan a pattern can ask for and the rows it can
//! build on a graph whose size the caller chose.

use std::collections::{BTreeMap, BTreeSet};

use ::opencypher::{
    BinaryOp, ComparisonOp, Direction, ExistsExpr, Expr, ExprKind, LabelExpr, MapProjectionItem,
    NodePattern, PatternElement, PatternPart, Properties, QuantifierKind, RelationshipPattern,
    UnaryOp,
};
use ::serde_json::Value;

use super::super::property_graph::PropertyGraph;
use super::{
    Budget, QueryFailure, MAX_EVALUATION_DEPTH, MAX_LIST_ITEMS, MAX_REGEX_PROGRAM_BYTES,
    MAX_VALUE_CHARS, MAX_VARIABLE_LENGTH_HOPS,
};

/// One value an openCypher evaluation can hold.
///
/// The domain is openCypher's: the scalars, lists and maps a query can write, plus the
/// two kinds of graph element, each held as the ID it is addressed by. Holding an element
/// by id rather than by value is what makes a row report what the STORE holds when the
/// row is rendered, instead of a copy taken when the element was bound.
#[derive(Debug, Clone, Default)]
pub enum CypherValue {
    /// `null`: the absence of a value, which openCypher propagates through most
    /// operations rather than failing on.
    #[default]
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<CypherValue>),
    Map(BTreeMap<String, CypherValue>),
    /// The node this id names.
    Node(String),
    /// The relationship this id names.
    Relationship(String),
}

/// One row of bindings: what each variable in scope is bound to.
pub type Row = BTreeMap<String, CypherValue>;

/// A partially matched pattern: the bindings so far, the relationships the match has
/// already traversed, and the elements along the path it has walked.
///
/// `used_relationships` is what gives a pattern openCypher's relationship isomorphism: a
/// single match may not traverse the same relationship twice, so `(a)-[]-(b)-[]-(c)`
/// cannot answer `a`, `b`, `a` from one relationship.
#[derive(Debug, Clone)]
pub struct Partial {
    pub row: Row,
    pub used_relationships: BTreeSet<String>,
    pub path: Vec<CypherValue>,
}

impl Partial {
    /// A match that has bound what `row` binds and traversed nothing.
    pub fn starting_from(row: Row) -> Self {
        Self {
            row,
            used_relationships: BTreeSet::new(),
            path: Vec::new(),
        }
    }
}

/// The store, the request's parameters and one evaluation's budget: everything reading
/// the graph needs, and no way to write it.
#[derive(Debug)]
pub struct Reader<'run> {
    pub graph: &'run PropertyGraph,
    pub parameters: &'run BTreeMap<String, Value>,
    pub budget: &'run Budget,
}

impl Reader<'_> {
    // ---------------------------------------------------------------- expressions

    /// What `expr` evaluates to for `row`.
    ///
    /// `group` is the rows one aggregate covers, present only while a projection's
    /// aggregates are being computed; an aggregate outside such a projection is refused
    /// rather than answered, because there is no set for it to be taken over.
    pub fn eval(
        &self,
        expr: &Expr,
        row: &Row,
        group: Option<&[Row]>,
    ) -> Result<CypherValue, QueryFailure> {
        self.budget.charge(1)?;
        // One level of the evaluation's descent, given back when this call returns: the
        // query chooses how deeply its own expressions nest, so the DEPTH of the
        // recursion is bounded as well as the number of steps it takes.
        let _descent = self.budget.enter()?;
        match &expr.kind {
            ExprKind::Null => Ok(CypherValue::Null),
            ExprKind::Boolean(flag) => Ok(CypherValue::Bool(*flag)),
            ExprKind::Integer(number) => Ok(CypherValue::Int(*number)),
            ExprKind::Float(number) => Ok(CypherValue::Float(*number)),
            ExprKind::String(text) => Ok(CypherValue::Str(text.clone())),
            ExprKind::Parameter(name) => self.parameter(name),
            ExprKind::Variable(variable) => match row.get(variable.0.as_str()) {
                Some(bound) => Ok(bound.clone()),
                None => Err(QueryFailure::Refused(format!(
                    "Variable `{variable}` not defined"
                ))),
            },
            ExprKind::List(items) => {
                self.budget.charge_size(items.len(), MAX_LIST_ITEMS)?;
                let mut values = Vec::with_capacity(items.len());
                for item in items {
                    values.push(self.eval(item, row, group)?);
                }
                Ok(CypherValue::List(values))
            }
            ExprKind::Map(entries) => {
                let mut map = BTreeMap::new();
                for (key, value) in entries {
                    map.insert(key.0.clone(), self.eval(value, row, group)?);
                }
                Ok(CypherValue::Map(map))
            }
            ExprKind::Unary { op, operand } => {
                let value = self.eval(operand, row, group)?;
                unary(*op, &value)
            }
            ExprKind::Binary { op, lhs, rhs } => {
                let left = self.eval(lhs, row, group)?;
                // `AND` and `OR` answer from the left operand alone where it decides the
                // result, which is what openCypher's three-valued logic states.
                match (op, truth(&left)) {
                    (BinaryOp::And, Some(false)) => return Ok(CypherValue::Bool(false)),
                    (BinaryOp::Or, Some(true)) => return Ok(CypherValue::Bool(true)),
                    _ => {}
                }
                let right = self.eval(rhs, row, group)?;
                self.binary(*op, &left, &right)
            }
            ExprKind::Comparison { first, rest } => {
                // `a < b <= c` means `a < b AND b <= c`: each operand is compared with
                // the one before it, and a `null` anywhere makes the chain `null`.
                let mut left = self.eval(first, row, group)?;
                let mut answer = CypherValue::Bool(true);
                for (op, operand) in rest {
                    let right = self.eval(operand, row, group)?;
                    let step = compare(*op, &left, &right);
                    match truth(&step) {
                        Some(false) => return Ok(CypherValue::Bool(false)),
                        None => answer = CypherValue::Null,
                        Some(true) => {}
                    }
                    left = right;
                }
                Ok(answer)
            }
            ExprKind::Property { base, key } => {
                let value = self.eval(base, row, group)?;
                Ok(self.property(&value, key.0.as_str()))
            }
            ExprKind::Index { base, index } => {
                let value = self.eval(base, row, group)?;
                let at = self.eval(index, row, group)?;
                self.index(&value, &at)
            }
            ExprKind::Slice { base, from, to } => {
                let value = self.eval(base, row, group)?;
                let start = match from {
                    Some(bound) => Some(self.eval(bound, row, group)?),
                    None => None,
                };
                let end = match to {
                    Some(bound) => Some(self.eval(bound, row, group)?),
                    None => None,
                };
                slice(&value, start.as_ref(), end.as_ref())
            }
            ExprKind::HasLabels { base, labels } => {
                let value = self.eval(base, row, group)?;
                match &value {
                    CypherValue::Null => Ok(CypherValue::Null),
                    CypherValue::Node(id) => Ok(CypherValue::Bool(
                        self.graph
                            .node(id)
                            .is_some_and(|node| label_matches(labels, &node.labels)),
                    )),
                    CypherValue::Relationship(id) => Ok(CypherValue::Bool(
                        self.graph.relationship(id).is_some_and(|relationship| {
                            label_matches(labels, ::std::slice::from_ref(&relationship.rel_type))
                        }),
                    )),
                    _ => Err(QueryFailure::Refused(
                        "Type mismatch: a label predicate takes a node or a relationship"
                            .to_string(),
                    )),
                }
            }
            ExprKind::IsNull(inner) => {
                let value = self.eval(inner, row, group)?;
                Ok(CypherValue::Bool(matches!(value, CypherValue::Null)))
            }
            ExprKind::IsNotNull(inner) => {
                let value = self.eval(inner, row, group)?;
                Ok(CypherValue::Bool(!matches!(value, CypherValue::Null)))
            }
            ExprKind::CountStar => match group {
                Some(rows) => Ok(CypherValue::Int(i64::try_from(rows.len()).unwrap_or(i64::MAX))),
                None => Err(QueryFailure::Refused(
                    "Invalid use of aggregating function count(*) in this context".to_string(),
                )),
            },
            ExprKind::FunctionCall {
                name,
                distinct,
                args,
            } => self.function(name.0.as_str(), *distinct, args, row, group),
            ExprKind::Case {
                operand,
                whens,
                default,
            } => {
                let subject = match operand {
                    Some(expr) => Some(self.eval(expr, row, group)?),
                    None => None,
                };
                for (when, then) in whens {
                    let candidate = self.eval(when, row, group)?;
                    let holds = match &subject {
                        // The simple form compares the operand with each `WHEN` value.
                        Some(subject) => {
                            truth(&compare(ComparisonOp::Eq, subject, &candidate)) == Some(true)
                        }
                        None => truth(&candidate) == Some(true),
                    };
                    if holds {
                        return self.eval(then, row, group);
                    }
                }
                match default {
                    Some(expr) => self.eval(expr, row, group),
                    None => Ok(CypherValue::Null),
                }
            }
            ExprKind::ListComprehension {
                variable,
                list,
                predicate,
                projection,
            } => {
                let source = self.eval(list, row, group)?;
                let items = list_items(&source)?;
                let mut out = Vec::new();
                for item in items {
                    // Charged per item whether or not a predicate is written, because
                    // every item costs a row of its own either way.
                    self.budget.charge(1)?;
                    self.budget.charge_copy(row_bytes(row))?;
                    let mut scope = row.clone();
                    scope.insert(variable.0.clone(), item);
                    if let Some(predicate) = predicate {
                        if truth(&self.eval(predicate, &scope, group)?) != Some(true) {
                            continue;
                        }
                    }
                    let value = match projection {
                        Some(projection) => self.eval(projection, &scope, group)?,
                        None => scope
                            .get(variable.0.as_str())
                            .cloned()
                            .unwrap_or(CypherValue::Null),
                    };
                    out.push(value);
                }
                Ok(CypherValue::List(out))
            }
            ExprKind::Quantifier {
                kind,
                variable,
                list,
                predicate,
            } => {
                let source = self.eval(list, row, group)?;
                let items = list_items(&source)?;
                let mut holding = 0usize;
                for item in items {
                    // Charged per item whether or not a predicate is written.
                    self.budget.charge(1)?;
                    self.budget.charge_copy(row_bytes(row))?;
                    let mut scope = row.clone();
                    scope.insert(variable.0.clone(), item);
                    let holds = match predicate {
                        Some(predicate) => truth(&self.eval(predicate, &scope, group)?) == Some(true),
                        None => true,
                    };
                    if holds {
                        holding = holding.saturating_add(1);
                        if matches!(kind, QuantifierKind::Any) {
                            return Ok(CypherValue::Bool(true));
                        }
                        if matches!(kind, QuantifierKind::None) {
                            return Ok(CypherValue::Bool(false));
                        }
                    } else if matches!(kind, QuantifierKind::All) {
                        return Ok(CypherValue::Bool(false));
                    }
                }
                Ok(CypherValue::Bool(match kind {
                    QuantifierKind::All | QuantifierKind::None => true,
                    QuantifierKind::Any => false,
                    QuantifierKind::Single => holding == 1,
                }))
            }
            ExprKind::Pattern(element) => {
                let part = PatternPart {
                    variable: None,
                    element: element.as_ref().clone(),
                    span: expr.span,
                };
                let matches = self.match_patterns(::std::slice::from_ref(&part), None, row)?;
                Ok(CypherValue::Bool(!matches.is_empty()))
            }
            ExprKind::Exists(exists) => match exists.as_ref() {
                ExistsExpr::Pattern {
                    patterns,
                    where_clause,
                } => {
                    let matches = self.match_patterns(patterns, where_clause.as_ref(), row)?;
                    Ok(CypherValue::Bool(!matches.is_empty()))
                }
                _ => Err(QueryFailure::Refused(
                    "An EXISTS subquery is not supported by this engine; use EXISTS { <pattern> }"
                        .to_string(),
                )),
            },
            ExprKind::MapProjection { base, items } => {
                let subject = self.eval(base, row, group)?;
                let mut map = BTreeMap::new();
                for item in items {
                    match item {
                        MapProjectionItem::Property(key) => {
                            map.insert(key.0.clone(), self.property(&subject, key.0.as_str()));
                        }
                        MapProjectionItem::Literal(key, value) => {
                            map.insert(key.0.clone(), self.eval(value, row, group)?);
                        }
                        MapProjectionItem::Variable(variable) => {
                            map.insert(
                                variable.0.clone(),
                                row.get(variable.0.as_str())
                                    .cloned()
                                    .unwrap_or(CypherValue::Null),
                            );
                        }
                        MapProjectionItem::All => {
                            for (key, value) in self.all_properties(&subject) {
                                map.insert(key, value);
                            }
                        }
                        _ => {
                            return Err(QueryFailure::Refused(
                                "Unsupported map projection selector".to_string(),
                            ))
                        }
                    }
                }
                Ok(CypherValue::Map(map))
            }
            _ => Err(QueryFailure::Refused(
                "Unsupported expression in this query".to_string(),
            )),
        }
    }

    /// The value the request's parameters bind `name` to.
    ///
    /// A query that references a parameter the request did not carry is refused: the
    /// query cannot be evaluated without it, and treating the missing one as `null`
    /// would answer a question the caller did not ask.
    fn parameter(&self, name: &str) -> Result<CypherValue, QueryFailure> {
        match self.parameters.get(name) {
            Some(value) => Ok(stored_value(value)),
            None => Err(QueryFailure::MissingParameter(name.to_string())),
        }
    }

    /// The value of the parameters map `name` names, as a map of properties.
    fn parameter_map(&self, name: &str) -> Result<BTreeMap<String, CypherValue>, QueryFailure> {
        match self.parameter(name)? {
            CypherValue::Map(entries) => Ok(entries),
            _ => Err(QueryFailure::Refused(format!(
                "Parameter `{name}` must be a map of properties"
            ))),
        }
    }

    /// The property `key` of `value`: of a graph element, what the store holds for it; of
    /// a map, the entry; of `null`, `null`.
    pub fn property(&self, value: &CypherValue, key: &str) -> CypherValue {
        match value {
            CypherValue::Node(id) => self
                .graph
                .node(id)
                .and_then(|node| node.properties.get(key))
                .map_or(CypherValue::Null, stored_value),
            CypherValue::Relationship(id) => self
                .graph
                .relationship(id)
                .and_then(|relationship| relationship.properties.get(key))
                .map_or(CypherValue::Null, stored_value),
            CypherValue::Map(entries) => entries.get(key).cloned().unwrap_or(CypherValue::Null),
            _ => CypherValue::Null,
        }
    }

    /// Every property of `value`, for `.*` and for `properties()`.
    fn all_properties(&self, value: &CypherValue) -> BTreeMap<String, CypherValue> {
        match value {
            CypherValue::Node(id) => self
                .graph
                .node(id)
                .map(|node| properties_of(&node.properties))
                .unwrap_or_default(),
            CypherValue::Relationship(id) => self
                .graph
                .relationship(id)
                .map(|relationship| properties_of(&relationship.properties))
                .unwrap_or_default(),
            CypherValue::Map(entries) => entries.clone(),
            _ => BTreeMap::new(),
        }
    }

    /// `base[at]`: an item of a list, by position from either end, or an entry of a map.
    fn index(&self, base: &CypherValue, at: &CypherValue) -> Result<CypherValue, QueryFailure> {
        match (base, at) {
            (CypherValue::Null, _) | (_, CypherValue::Null) => Ok(CypherValue::Null),
            (CypherValue::List(items), CypherValue::Int(position)) => {
                let length = i64::try_from(items.len()).unwrap_or(i64::MAX);
                let resolved = if *position < 0 {
                    length.saturating_add(*position)
                } else {
                    *position
                };
                Ok(usize::try_from(resolved)
                    .ok()
                    .and_then(|index| items.get(index))
                    .cloned()
                    .unwrap_or(CypherValue::Null))
            }
            (subject, CypherValue::Str(key)) => Ok(self.property(subject, key)),
            _ => Err(QueryFailure::Refused(
                "Type mismatch: an index takes an integer for a list or a string for a map"
                    .to_string(),
            )),
        }
    }

    /// One binary operation.
    fn binary(
        &self,
        op: BinaryOp,
        left: &CypherValue,
        right: &CypherValue,
    ) -> Result<CypherValue, QueryFailure> {
        match op {
            BinaryOp::And | BinaryOp::Or | BinaryOp::Xor => {
                Ok(match (op, truth(left), truth(right)) {
                    (BinaryOp::And, Some(false), _) | (BinaryOp::And, _, Some(false)) => {
                        CypherValue::Bool(false)
                    }
                    (BinaryOp::Or, Some(true), _) | (BinaryOp::Or, _, Some(true)) => {
                        CypherValue::Bool(true)
                    }
                    (BinaryOp::And, Some(true), Some(true)) => CypherValue::Bool(true),
                    (BinaryOp::Or, Some(false), Some(false)) => CypherValue::Bool(false),
                    (BinaryOp::Xor, Some(one), Some(other)) => CypherValue::Bool(one != other),
                    _ => CypherValue::Null,
                })
            }
            BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod
            | BinaryOp::Pow => arithmetic(self.budget, op, left, right),
            BinaryOp::In => match right {
                CypherValue::Null => Ok(CypherValue::Null),
                CypherValue::List(items) => {
                    let mut unknown = false;
                    for item in items {
                        match truth(&compare(ComparisonOp::Eq, left, item)) {
                            Some(true) => return Ok(CypherValue::Bool(true)),
                            None => unknown = true,
                            Some(false) => {}
                        }
                    }
                    Ok(if unknown {
                        CypherValue::Null
                    } else {
                        CypherValue::Bool(false)
                    })
                }
                _ => Err(QueryFailure::Refused(
                    "Type mismatch: IN takes a list on its right-hand side".to_string(),
                )),
            },
            BinaryOp::Contains | BinaryOp::StartsWith | BinaryOp::EndsWith => {
                match (text_of(left), text_of(right)) {
                    (Some(subject), Some(probe)) => Ok(CypherValue::Bool(match op {
                        BinaryOp::Contains => subject.contains(&probe),
                        BinaryOp::StartsWith => subject.starts_with(&probe),
                        _ => subject.ends_with(&probe),
                    })),
                    _ => Ok(CypherValue::Null),
                }
            }
            BinaryOp::RegexMatch => match (text_of(left), text_of(right)) {
                (Some(subject), Some(pattern)) => {
                    // The pattern comes from the query, so the compiled program is
                    // bounded BEFORE it is built: a pattern past the ceiling is refused
                    // rather than compiled.
                    let compiled = ::regex::RegexBuilder::new(&pattern)
                        .size_limit(MAX_REGEX_PROGRAM_BYTES)
                        .build()
                        .map_err(|refused| {
                            QueryFailure::Refused(format!(
                                "Invalid regular expression in the query: {refused}"
                            ))
                        })?;
                    // openCypher's `=~` holds when the whole string matches.
                    Ok(CypherValue::Bool(
                        compiled
                            .find(&subject)
                            .is_some_and(|found| found.start() == 0 && found.end() == subject.len()),
                    ))
                }
                _ => Ok(CypherValue::Null),
            },
        }
    }

    // ------------------------------------------------------------------ functions

    /// One function call. An aggregate is answered over `group`, which is the set of rows
    /// the projection is computing it for.
    fn function(
        &self,
        name: &str,
        distinct: bool,
        args: &[Expr],
        row: &Row,
        group: Option<&[Row]>,
    ) -> Result<CypherValue, QueryFailure> {
        if is_aggregate(name) {
            let Some(rows) = group else {
                return Err(QueryFailure::Refused(format!(
                    "Invalid use of aggregating function {name}() in this context"
                )));
            };
            return self.aggregate(name, distinct, args, rows);
        }
        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            values.push(self.eval(arg, row, group)?);
        }
        self.scalar(name, &values)
    }

    /// One aggregate over the rows of a group.
    fn aggregate(
        &self,
        name: &str,
        distinct: bool,
        args: &[Expr],
        rows: &[Row],
    ) -> Result<CypherValue, QueryFailure> {
        let Some(argument) = args.first() else {
            return Err(QueryFailure::Refused(format!(
                "Aggregating function {name}() takes one argument"
            )));
        };
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut values: Vec<CypherValue> = Vec::new();
        for row in rows {
            self.budget.charge(1)?;
            let value = self.eval(argument, row, None)?;
            if matches!(value, CypherValue::Null) {
                continue;
            }
            if distinct && !seen.insert(group_key(::std::slice::from_ref(&value))) {
                continue;
            }
            // The values an aggregate gathers are a value of their own, so the list it
            // builds is bounded like any other.
            self.budget.charge_item(values.len(), MAX_LIST_ITEMS)?;
            values.push(value);
        }
        let lowered = name.to_ascii_lowercase();
        match lowered.as_str() {
            "count" => Ok(CypherValue::Int(
                i64::try_from(values.len()).unwrap_or(i64::MAX),
            )),
            "collect" => Ok(CypherValue::List(values)),
            "sum" => {
                let mut total = CypherValue::Int(0);
                for value in &values {
                    total = arithmetic(self.budget, BinaryOp::Add, &total, value)?;
                }
                Ok(total)
            }
            "avg" => {
                if values.is_empty() {
                    return Ok(CypherValue::Null);
                }
                let mut total = 0.0_f64;
                for value in &values {
                    let Some(number) = float_of(value) else {
                        return Err(QueryFailure::Refused(
                            "Type mismatch: avg() takes numbers".to_string(),
                        ));
                    };
                    total += number;
                }
                #[allow(clippy::cast_precision_loss)]
                finite(total / values.len() as f64)
            }
            "min" | "max" => {
                let mut chosen: Option<&CypherValue> = None;
                for value in &values {
                    // min() and max() order the values of the group, so a kind this
                    // engine does not order is refused rather than answered with
                    // whichever one happened to be evaluated first.
                    if matches!(
                        value,
                        CypherValue::Map(_) | CypherValue::Node(_) | CypherValue::Relationship(_)
                    ) {
                        return Err(type_mismatch(name, "values it can order"));
                    }
                    let replaces = match chosen {
                        None => true,
                        Some(held) => {
                            let ordering = order(value, held);
                            if lowered == "min" {
                                ordering == ::std::cmp::Ordering::Less
                            } else {
                                ordering == ::std::cmp::Ordering::Greater
                            }
                        }
                    };
                    if replaces {
                        chosen = Some(value);
                    }
                }
                Ok(chosen.cloned().unwrap_or(CypherValue::Null))
            }
            _ => Err(QueryFailure::Refused(format!("Unknown function {name}()"))),
        }
    }

    /// One scalar function, over the values its arguments evaluated to.
    ///
    /// The set below is the one this engine implements; a function outside it is refused
    /// by name rather than answered with something that looks computed.
    fn scalar(&self, name: &str, args: &[CypherValue]) -> Result<CypherValue, QueryFailure> {
        let first = args.first().cloned().unwrap_or(CypherValue::Null);
        let lowered = name.to_ascii_lowercase();
        let answer = match lowered.as_str() {
            "id" => match &first {
                CypherValue::Node(id) | CypherValue::Relationship(id) => {
                    CypherValue::Str(id.clone())
                }
                CypherValue::Null => CypherValue::Null,
                _ => return Err(type_mismatch(name, "a node or a relationship")),
            },
            "labels" => match &first {
                CypherValue::Node(id) => CypherValue::List(
                    self.graph
                        .node(id)
                        .map(|node| {
                            node.labels
                                .iter()
                                .map(|label| CypherValue::Str(label.clone()))
                                .collect()
                        })
                        .unwrap_or_default(),
                ),
                CypherValue::Null => CypherValue::Null,
                _ => return Err(type_mismatch(name, "a node")),
            },
            "type" => match &first {
                CypherValue::Relationship(id) => self
                    .graph
                    .relationship(id)
                    .map_or(CypherValue::Null, |relationship| {
                        CypherValue::Str(relationship.rel_type.clone())
                    }),
                CypherValue::Null => CypherValue::Null,
                _ => return Err(type_mismatch(name, "a relationship")),
            },
            "properties" => match &first {
                CypherValue::Null => CypherValue::Null,
                subject => {
                    let properties = self.all_properties(subject);
                    self.budget.charge_size(properties.len(), MAX_LIST_ITEMS)?;
                    CypherValue::Map(properties)
                }
            },
            "keys" => match &first {
                CypherValue::Null => CypherValue::Null,
                subject => {
                    let properties = self.all_properties(subject);
                    self.budget.charge_size(properties.len(), MAX_LIST_ITEMS)?;
                    CypherValue::List(properties.into_keys().map(CypherValue::Str).collect())
                }
            },
            "startnode" | "endnode" => match &first {
                CypherValue::Relationship(id) => self
                    .graph
                    .relationship(id)
                    .map_or(CypherValue::Null, |relationship| {
                        CypherValue::Node(if lowered == "startnode" {
                            relationship.from_node.clone()
                        } else {
                            relationship.to_node.clone()
                        })
                    }),
                CypherValue::Null => CypherValue::Null,
                _ => return Err(type_mismatch(name, "a relationship")),
            },
            "size" | "length" => match &first {
                CypherValue::Null => CypherValue::Null,
                CypherValue::List(items) => {
                    CypherValue::Int(i64::try_from(items.len()).unwrap_or(i64::MAX))
                }
                CypherValue::Str(text) => {
                    CypherValue::Int(i64::try_from(text.chars().count()).unwrap_or(i64::MAX))
                }
                CypherValue::Map(entries) => {
                    CypherValue::Int(i64::try_from(entries.len()).unwrap_or(i64::MAX))
                }
                _ => return Err(type_mismatch(name, "a list, a string or a map")),
            },
            "head" | "last" => match &first {
                CypherValue::Null => CypherValue::Null,
                CypherValue::List(items) => {
                    let item = if lowered == "head" {
                        items.first()
                    } else {
                        items.last()
                    };
                    item.cloned().unwrap_or(CypherValue::Null)
                }
                _ => return Err(type_mismatch(name, "a list")),
            },
            "tail" => match &first {
                CypherValue::Null => CypherValue::Null,
                CypherValue::List(items) => {
                    self.budget.charge_size(items.len(), MAX_LIST_ITEMS)?;
                    CypherValue::List(items.iter().skip(1).cloned().collect())
                }
                _ => return Err(type_mismatch(name, "a list")),
            },
            "reverse" => match &first {
                CypherValue::Null => CypherValue::Null,
                CypherValue::List(items) => {
                    self.budget.charge_size(items.len(), MAX_LIST_ITEMS)?;
                    CypherValue::List(items.iter().rev().cloned().collect())
                }
                CypherValue::Str(text) => {
                    self.budget.charge_size(text.len(), MAX_VALUE_CHARS)?;
                    CypherValue::Str(text.chars().rev().collect())
                }
                _ => return Err(type_mismatch(name, "a list or a string")),
            },
            "coalesce" => args
                .iter()
                .find(|value| !matches!(value, CypherValue::Null))
                .cloned()
                .unwrap_or(CypherValue::Null),
            "exists" => CypherValue::Bool(!matches!(first, CypherValue::Null)),
            "tostring" => match &first {
                CypherValue::Null => CypherValue::Null,
                value => CypherValue::Str(text_of(value).unwrap_or_else(|| display(value))),
            },
            "tointeger" => match &first {
                CypherValue::Null => CypherValue::Null,
                CypherValue::Int(number) => CypherValue::Int(*number),
                #[allow(clippy::cast_possible_truncation)]
                CypherValue::Float(number) => CypherValue::Int(*number as i64),
                CypherValue::Str(text) => text
                    .trim()
                    .parse::<i64>()
                    .map_or(CypherValue::Null, CypherValue::Int),
                CypherValue::Bool(flag) => CypherValue::Int(i64::from(*flag)),
                _ => return Err(type_mismatch(name, "a number or a string")),
            },
            "tofloat" => match &first {
                CypherValue::Null => CypherValue::Null,
                #[allow(clippy::cast_precision_loss)]
                CypherValue::Int(number) => CypherValue::Float(*number as f64),
                CypherValue::Float(number) => CypherValue::Float(*number),
                // A text that reads as a number this engine cannot hold (an infinity, a
                // NaN) is read as no number at all: nothing in the value domain is
                // non-finite.
                CypherValue::Str(text) => text
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|number| number.is_finite())
                    .map_or(CypherValue::Null, CypherValue::Float),
                _ => return Err(type_mismatch(name, "a number or a string")),
            },
            "toboolean" => match &first {
                CypherValue::Null => CypherValue::Null,
                CypherValue::Bool(flag) => CypherValue::Bool(*flag),
                CypherValue::Str(text) => match text.trim().to_ascii_lowercase().as_str() {
                    "true" => CypherValue::Bool(true),
                    "false" => CypherValue::Bool(false),
                    _ => CypherValue::Null,
                },
                _ => return Err(type_mismatch(name, "a boolean or a string")),
            },
            "toupper" | "tolower" | "trim" | "ltrim" | "rtrim" => match text_of(&first) {
                None => CypherValue::Null,
                Some(text) => {
                    self.budget.charge_size(text.len(), MAX_VALUE_CHARS)?;
                    CypherValue::Str(match lowered.as_str() {
                        "toupper" => text.to_uppercase(),
                        "tolower" => text.to_lowercase(),
                        "trim" => text.trim().to_string(),
                        "ltrim" => text.trim_start().to_string(),
                        _ => text.trim_end().to_string(),
                    })
                }
            },
            "substring" => {
                let Some(text) = text_of(&first) else {
                    return Ok(CypherValue::Null);
                };
                let characters: Vec<char> = text.chars().collect();
                // Both arguments are integers, and neither may be negative: openCypher
                // refuses such a call rather than reading it as the whole string or as an
                // empty one.
                let start = integer_argument(args.get(1), name)?;
                let taken = match args.get(2) {
                    Some(_) => integer_argument(args.get(2), name)?,
                    None => i64::try_from(characters.len()).unwrap_or(i64::MAX),
                };
                if start < 0 || taken < 0 {
                    return Err(QueryFailure::Refused(format!(
                        "{name}() takes a start and a length that are not negative"
                    )));
                }
                let from = usize::try_from(start).unwrap_or(0).min(characters.len());
                let taken = usize::try_from(taken).unwrap_or(0);
                self.budget.charge_size(taken.min(characters.len()), MAX_VALUE_CHARS)?;
                CypherValue::Str(characters.iter().skip(from).take(taken).collect())
            }
            "replace" => match (text_of(&first), text_of_arg(args.get(1)), text_of_arg(args.get(2)))
            {
                // An empty search string matches nowhere: openCypher does not expand on
                // one, and expanding would multiply the subject by the replacement.
                (Some(text), Some(from), Some(_)) if from.is_empty() => CypherValue::Str(text),
                (Some(text), Some(from), Some(to)) => {
                    let occurrences = text.matches(from.as_str()).count();
                    let produced = text
                        .len()
                        .saturating_add(occurrences.saturating_mul(to.len()));
                    self.budget.charge_size(produced, MAX_VALUE_CHARS)?;
                    CypherValue::Str(text.replace(&from, &to))
                }
                _ => CypherValue::Null,
            },
            "split" => match (text_of(&first), text_of_arg(args.get(1))) {
                (Some(text), Some(separator)) if !separator.is_empty() => {
                    self.budget.charge_size(text.len(), MAX_VALUE_CHARS)?;
                    CypherValue::List(
                        text.split(separator.as_str())
                            .map(|part| CypherValue::Str(part.to_string()))
                            .collect(),
                    )
                }
                (Some(text), Some(_)) => CypherValue::List(vec![CypherValue::Str(text)]),
                _ => CypherValue::Null,
            },
            "abs" | "ceil" | "floor" | "round" | "sign" | "sqrt" => match &first {
                CypherValue::Null => CypherValue::Null,
                CypherValue::Int(number) if lowered == "abs" => {
                    CypherValue::Int(number.checked_abs().ok_or_else(overflowed)?)
                }
                CypherValue::Int(number) if lowered == "sign" => {
                    CypherValue::Int(number.signum())
                }
                CypherValue::Int(number) if matches!(lowered.as_str(), "ceil" | "floor" | "round") => {
                    CypherValue::Int(*number)
                }
                value => {
                    let Some(number) = float_of(value) else {
                        return Err(type_mismatch(name, "a number"));
                    };
                    // `sqrt` of a negative number has no value in this engine's domain,
                    // so it is refused rather than carried as a non-number.
                    return finite(match lowered.as_str() {
                        "abs" => number.abs(),
                        "ceil" => number.ceil(),
                        "floor" => number.floor(),
                        "round" => number.round(),
                        "sign" => number.signum(),
                        _ => number.sqrt(),
                    });
                }
            },
            "range" => {
                let start = integer_argument(args.first(), name)?;
                let end = integer_argument(args.get(1), name)?;
                let step = match args.get(2) {
                    Some(_) => integer_argument(args.get(2), name)?,
                    None => 1,
                };
                if step == 0 {
                    return Err(QueryFailure::Refused(
                        "range() takes a step other than zero".to_string(),
                    ));
                }
                let mut items = Vec::new();
                let mut at = start;
                while (step > 0 && at <= end) || (step < 0 && at >= end) {
                    self.budget.charge_item(items.len(), MAX_LIST_ITEMS)?;
                    items.push(CypherValue::Int(at));
                    at = at.checked_add(step).ok_or_else(overflowed)?;
                }
                CypherValue::List(items)
            }
            "nodes" | "relationships" => match &first {
                CypherValue::Null => CypherValue::Null,
                CypherValue::List(items) => {
                    self.budget.charge_size(items.len(), MAX_LIST_ITEMS)?;
                    CypherValue::List(
                        items
                            .iter()
                            .filter(|item| {
                                if lowered == "nodes" {
                                    matches!(item, CypherValue::Node(_))
                                } else {
                                    matches!(item, CypherValue::Relationship(_))
                                }
                            })
                            .cloned()
                            .collect(),
                    )
                }
                _ => return Err(type_mismatch(name, "a path")),
            },
            _ => return Err(QueryFailure::Refused(format!("Unknown function {name}()"))),
        };
        Ok(answer)
    }

    // ------------------------------------------------------------------- matching

    /// Every way `patterns` matches the graph, given what `row` already binds, filtered
    /// by `where_clause`.
    pub fn match_patterns(
        &self,
        patterns: &[PatternPart],
        where_clause: Option<&Expr>,
        row: &Row,
    ) -> Result<Vec<Row>, QueryFailure> {
        self.budget.charge_copy(row_bytes(row))?;
        let mut partials = vec![Partial::starting_from(row.clone())];
        for part in patterns {
            let mut next = Vec::new();
            for partial in partials {
                self.match_part(part, partial, &mut next)?;
            }
            partials = next;
        }
        let mut rows = Vec::with_capacity(partials.len());
        for partial in partials {
            if let Some(predicate) = where_clause {
                if truth(&self.eval(predicate, &partial.row, None)?) != Some(true) {
                    continue;
                }
            }
            rows.push(partial.row);
        }
        Ok(rows)
    }

    /// Every way one comma-separated pattern part matches, extending `partial`.
    fn match_part(
        &self,
        part: &PatternPart,
        partial: Partial,
        out: &mut Vec<Partial>,
    ) -> Result<(), QueryFailure> {
        let element = &part.element;
        for start in self.node_candidates(&element.start, &partial.row)? {
            self.budget.charge_copy(row_bytes(&partial.row))?;
            let mut begun = partial.clone();
            if !self.bind(&mut begun.row, element.start.variable.as_ref(), &CypherValue::Node(start.clone())) {
                continue;
            }
            begun.path.push(CypherValue::Node(start.clone()));
            let mut walked = Vec::new();
            self.walk(element, 0, &start, begun, &mut walked)?;
            for mut reached in walked {
                if let Some(variable) = &part.variable {
                    let path = CypherValue::List(reached.path.clone());
                    if !self.bind(&mut reached.row, Some(variable), &path) {
                        continue;
                    }
                }
                out.push(reached);
            }
        }
        Ok(())
    }

    /// Walk the hops of `element` from `at`, collecting every full match.
    fn walk(
        &self,
        element: &PatternElement,
        hop: usize,
        at: &str,
        partial: Partial,
        out: &mut Vec<Partial>,
    ) -> Result<(), QueryFailure> {
        self.budget.charge(1)?;
        // One level per hop: the query chooses how many hops its pattern has, so the
        // depth of this walk is bounded as well as the work it does.
        let _descent = self.budget.enter()?;
        let Some((relationship, node)) = element.chain.get(hop) else {
            out.push(partial);
            return Ok(());
        };
        match relationship.range {
            None => {
                for (rel_id, other) in self.single_hops(at, relationship, &partial)? {
                    self.budget.charge_copy(row_bytes(&partial.row))?;
                    let mut stepped = partial.clone();
                    stepped.used_relationships.insert(rel_id.clone());
                    if !self.bind(
                        &mut stepped.row,
                        relationship.variable.as_ref(),
                        &CypherValue::Relationship(rel_id.clone()),
                    ) {
                        continue;
                    }
                    if !self.node_matches(node, &other, &stepped.row)? {
                        continue;
                    }
                    if !self.bind(
                        &mut stepped.row,
                        node.variable.as_ref(),
                        &CypherValue::Node(other.clone()),
                    ) {
                        continue;
                    }
                    stepped.path.push(CypherValue::Relationship(rel_id));
                    stepped.path.push(CypherValue::Node(other.clone()));
                    self.walk(element, hop.saturating_add(1), &other, stepped, out)?;
                }
            }
            Some(range) => {
                // A variable-length hop binds its variable to the LIST of relationships
                // it traversed. A range the engine cannot walk is REFUSED with the
                // ceiling it met: narrowing it and answering anyway would report a short
                // result as a complete one.
                let asked_minimum = range.min.unwrap_or(1);
                let asked_maximum = range.max.unwrap_or_else(|| {
                    i64::try_from(MAX_VARIABLE_LENGTH_HOPS).unwrap_or(i64::MAX)
                });
                if asked_minimum < 0 || asked_maximum < 0 {
                    return Err(QueryFailure::Refused(
                        "A variable-length pattern walks a range of zero or more hops"
                            .to_string(),
                    ));
                }
                let asked = usize::try_from(asked_maximum.max(asked_minimum)).unwrap_or(usize::MAX);
                if asked > MAX_VARIABLE_LENGTH_HOPS {
                    return Err(QueryFailure::TooManyHops {
                        asked,
                        ceiling: MAX_VARIABLE_LENGTH_HOPS,
                    });
                }
                let minimum = usize::try_from(asked_minimum).unwrap_or(0);
                let maximum = usize::try_from(asked_maximum).unwrap_or(MAX_VARIABLE_LENGTH_HOPS);
                let mut reached = Vec::new();
                self.variable_hops(
                    at,
                    relationship,
                    &partial,
                    minimum,
                    maximum,
                    Vec::new(),
                    Vec::new(),
                    &mut reached,
                )?;
                for (traversed, walked, other) in reached {
                    self.budget.charge_copy(row_bytes(&partial.row))?;
                    let mut stepped = partial.clone();
                    for rel_id in &traversed {
                        stepped.used_relationships.insert(rel_id.clone());
                    }
                    let bound = CypherValue::List(
                        traversed
                            .iter()
                            .map(|rel_id| CypherValue::Relationship(rel_id.clone()))
                            .collect(),
                    );
                    if !self.bind(&mut stepped.row, relationship.variable.as_ref(), &bound) {
                        continue;
                    }
                    if !self.node_matches(node, &other, &stepped.row)? {
                        continue;
                    }
                    if !self.bind(
                        &mut stepped.row,
                        node.variable.as_ref(),
                        &CypherValue::Node(other.clone()),
                    ) {
                        continue;
                    }
                    stepped.path.extend(walked);
                    self.walk(element, hop.saturating_add(1), &other, stepped, out)?;
                }
            }
        }
        Ok(())
    }

    /// Every one-hop step from `at` along `relationship` the match has not already
    /// traversed, as `(relationship id, the node at the other end)`.
    fn single_hops(
        &self,
        at: &str,
        relationship: &RelationshipPattern,
        partial: &Partial,
    ) -> Result<Vec<(String, String)>, QueryFailure> {
        let mut hops = Vec::new();
        for rel_id in self.graph.relationship_ids() {
            self.budget.charge(1)?;
            if partial.used_relationships.contains(&rel_id) {
                continue;
            }
            let Some(stored) = self.graph.relationship(&rel_id) else {
                continue;
            };
            let other = match relationship.direction {
                Direction::Outgoing if stored.from_node == at => &stored.to_node,
                Direction::Incoming if stored.to_node == at => &stored.from_node,
                Direction::Undirected | Direction::Both if stored.from_node == at => {
                    &stored.to_node
                }
                Direction::Undirected | Direction::Both if stored.to_node == at => {
                    &stored.from_node
                }
                _ => continue,
            };
            if let Some(types) = &relationship.types {
                if !label_matches(types, ::std::slice::from_ref(&stored.rel_type)) {
                    continue;
                }
            }
            if !self.properties_match(
                relationship.properties.as_ref(),
                &properties_of(&stored.properties),
                &partial.row,
            )? {
                continue;
            }
            hops.push((rel_id, other.clone()));
        }
        Ok(hops)
    }

    /// Every path of between `minimum` and `maximum` hops from `at` along
    /// `relationship`, as `(the relationships traversed, the elements walked, the node
    /// reached)`.
    #[allow(clippy::too_many_arguments)]
    fn variable_hops(
        &self,
        at: &str,
        relationship: &RelationshipPattern,
        partial: &Partial,
        minimum: usize,
        maximum: usize,
        traversed: Vec<String>,
        walked: Vec<CypherValue>,
        out: &mut Vec<(Vec<String>, Vec<CypherValue>, String)>,
    ) -> Result<(), QueryFailure> {
        self.budget.charge(1)?;
        // One level per hop walked, so this descent is bounded by the engine as well as
        // by the range the pattern asked for.
        let _descent = self.budget.enter()?;
        if traversed.len() >= minimum {
            out.push((traversed.clone(), walked.clone(), at.to_string()));
        }
        if traversed.len() >= maximum {
            return Ok(());
        }
        let mut reached = partial.clone();
        for rel_id in &traversed {
            reached.used_relationships.insert(rel_id.clone());
        }
        for (rel_id, other) in self.single_hops(at, relationship, &reached)? {
            let mut onwards = traversed.clone();
            onwards.push(rel_id.clone());
            let mut steps = walked.clone();
            steps.push(CypherValue::Relationship(rel_id));
            steps.push(CypherValue::Node(other.clone()));
            self.variable_hops(
                &other,
                relationship,
                partial,
                minimum,
                maximum,
                onwards,
                steps,
                out,
            )?;
        }
        Ok(())
    }

    /// The nodes a node pattern can match, given what `row` already binds: the one node
    /// its variable is already bound to, or every node in the store the pattern fits.
    fn node_candidates(
        &self,
        pattern: &NodePattern,
        row: &Row,
    ) -> Result<Vec<String>, QueryFailure> {
        if let Some(variable) = &pattern.variable {
            if let Some(bound) = row.get(variable.0.as_str()) {
                let CypherValue::Node(id) = bound else {
                    return Ok(Vec::new());
                };
                let id = id.clone();
                return Ok(if self.node_matches(pattern, &id, row)? {
                    vec![id]
                } else {
                    Vec::new()
                });
            }
        }
        let mut candidates = Vec::new();
        for id in self.graph.node_ids() {
            self.budget.charge(1)?;
            if self.node_matches(pattern, &id, row)? {
                candidates.push(id);
            }
        }
        Ok(candidates)
    }

    /// Whether the node `id` names satisfies `pattern`'s labels and inline properties.
    fn node_matches(
        &self,
        pattern: &NodePattern,
        id: &str,
        row: &Row,
    ) -> Result<bool, QueryFailure> {
        let Some(node) = self.graph.node(id) else {
            return Ok(false);
        };
        if let Some(variable) = &pattern.variable {
            if let Some(CypherValue::Node(bound)) = row.get(variable.0.as_str()) {
                if bound != id {
                    return Ok(false);
                }
            }
        }
        if let Some(labels) = &pattern.labels {
            if !label_matches(labels, &node.labels) {
                return Ok(false);
            }
        }
        self.properties_match(
            pattern.properties.as_ref(),
            &properties_of(&node.properties),
            row,
        )
    }

    /// Whether `held` carries every property `pattern` asks for, with the value it asks
    /// for.
    fn properties_match(
        &self,
        pattern: Option<&Properties>,
        held: &BTreeMap<String, CypherValue>,
        row: &Row,
    ) -> Result<bool, QueryFailure> {
        let asked = match pattern {
            None => return Ok(true),
            Some(Properties::Map(entries)) => {
                let mut asked = BTreeMap::new();
                for (key, value) in entries {
                    asked.insert(key.0.clone(), self.eval(value, row, None)?);
                }
                asked
            }
            Some(Properties::Param(name)) => self.parameter_map(name)?,
        };
        for (key, wanted) in &asked {
            let found = held.get(key).cloned().unwrap_or(CypherValue::Null);
            if truth(&compare(ComparisonOp::Eq, &found, wanted)) != Some(true) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Bind `variable` to `value` in `row`, answering whether the binding HOLDS: a
    /// variable already bound to something else makes the match fail rather than
    /// rebinding it, which is how a pattern that names the same variable twice is
    /// constrained.
    fn bind(
        &self,
        row: &mut Row,
        variable: Option<&::opencypher::Variable>,
        value: &CypherValue,
    ) -> bool {
        let Some(variable) = variable else {
            return true;
        };
        match row.get(variable.0.as_str()) {
            Some(bound) => same_element(bound, value),
            None => {
                row.insert(variable.0.clone(), value.clone());
                true
            }
        }
    }

    /// The properties a pattern asks to be SET on an element it creates, as JSON the
    /// store holds.
    pub fn pattern_properties(
        &self,
        pattern: Option<&Properties>,
        row: &Row,
    ) -> Result<BTreeMap<String, Value>, QueryFailure> {
        let values = match pattern {
            None => return Ok(BTreeMap::new()),
            Some(Properties::Map(entries)) => {
                let mut values = BTreeMap::new();
                for (key, value) in entries {
                    values.insert(key.0.clone(), self.eval(value, row, None)?);
                }
                values
            }
            Some(Properties::Param(name)) => self.parameter_map(name)?,
        };
        let mut stored = BTreeMap::new();
        for (key, value) in values {
            // Assigning null sets nothing: openCypher has no property holding null.
            if matches!(value, CypherValue::Null) {
                continue;
            }
            stored.insert(key, property_json(&value)?);
        }
        Ok(stored)
    }

    /// One value as this service spells it on the wire. A graph element is spelled from
    /// the STORE, so a row reports what the store holds rather than a copy taken when the
    /// element was bound.
    ///
    /// Rendering is charged for, and descends no deeper than the evaluation does: a
    /// result is built from values whose own size is bounded, and this is what bounds
    /// what one request can be ANSWERED with.
    pub fn document(&self, value: &CypherValue) -> Result<Value, QueryFailure> {
        let _descent = self.budget.enter()?;
        match value {
            CypherValue::Null => Ok(Value::Null),
            CypherValue::Bool(flag) => Ok(Value::Bool(*flag)),
            CypherValue::Int(number) => Ok(Value::from(*number)),
            // Nothing in this engine's value domain is infinite or NaN (see `finite`), so
            // a number that cannot be written is reported rather than rendered as an
            // absent value a caller could not tell from a property that was never set.
            CypherValue::Float(number) => ::serde_json::Number::from_f64(*number)
                .map(Value::Number)
                .ok_or_else(|| {
                    QueryFailure::Refused(
                        "This query answers a number that cannot be written".to_string(),
                    )
                }),
            CypherValue::Str(text) => {
                self.budget.charge_size(text.len(), MAX_VALUE_CHARS)?;
                Ok(Value::String(text.clone()))
            }
            CypherValue::List(items) => {
                self.budget.charge_size(items.len(), MAX_LIST_ITEMS)?;
                let mut rendered = Vec::with_capacity(items.len());
                for item in items {
                    rendered.push(self.document(item)?);
                }
                Ok(Value::Array(rendered))
            }
            CypherValue::Map(entries) => {
                self.budget.charge_size(entries.len(), MAX_LIST_ITEMS)?;
                let mut rendered = ::serde_json::Map::new();
                for (key, entry) in entries {
                    rendered.insert(key.clone(), self.document(entry)?);
                }
                Ok(Value::Object(rendered))
            }
            // An element is rendered from the STORE, so what it costs is the size of the
            // element the store holds — charged per reference, since one answer may carry
            // the same element many times.
            CypherValue::Node(id) => {
                self.budget
                    .charge_size(self.graph.element_bytes(id), MAX_VALUE_CHARS)?;
                Ok(self.graph.node_document(id))
            }
            CypherValue::Relationship(id) => {
                self.budget
                    .charge_size(self.graph.element_bytes(id), MAX_VALUE_CHARS)?;
                Ok(self.graph.relationship_document(id))
            }
        }
    }
}

/// How many characters one row of bindings holds, which is what a copy of it costs.
#[must_use]
pub fn row_bytes(row: &Row) -> usize {
    row.iter().fold(0, |total, (name, value)| {
        total
            .saturating_add(name.len())
            .saturating_add(value_size(value, 0))
    })
}

/// How many characters one value holds.
///
/// A value of a fixed size counts as the space its binding takes; a text, a list and a
/// map count their contents, so the measure follows the bytes a copy of the value really
/// takes. The descent is capped the way every other reading of a value is.
fn value_size(value: &CypherValue, depth: usize) -> usize {
    /// What a value of a fixed size counts as.
    const SCALAR_CHARS: usize = 16;
    if depth >= MAX_EVALUATION_DEPTH {
        return SCALAR_CHARS;
    }
    let deeper = depth.saturating_add(1);
    match value {
        CypherValue::Null | CypherValue::Bool(_) | CypherValue::Int(_) | CypherValue::Float(_) => {
            SCALAR_CHARS
        }
        CypherValue::Str(text) => SCALAR_CHARS.saturating_add(text.len()),
        CypherValue::Node(id) | CypherValue::Relationship(id) => {
            SCALAR_CHARS.saturating_add(id.len())
        }
        CypherValue::List(items) => items.iter().fold(SCALAR_CHARS, |total, item| {
            total.saturating_add(value_size(item, deeper))
        }),
        CypherValue::Map(entries) => entries.iter().fold(SCALAR_CHARS, |total, (key, entry)| {
            total
                .saturating_add(key.len())
                .saturating_add(value_size(entry, deeper))
        }),
    }
}

/// Whether two bindings name the same thing, which is what a repeated variable in one
/// pattern requires.
fn same_element(held: &CypherValue, candidate: &CypherValue) -> bool {
    group_key(::std::slice::from_ref(held)) == group_key(::std::slice::from_ref(candidate))
}

/// The function names this engine reads as aggregates.
///
/// Source: openCypher's aggregating functions, which are the ones a projection groups
/// its other items by.
const AGGREGATES: &[&str] = &["avg", "collect", "count", "max", "min", "sum"];

/// The scalar functions this engine implements, in openCypher's own spelling.
///
/// This is the list [`Reader::scalar`] answers, and the one a query is checked against
/// before it is evaluated: a function outside it is refused by name rather than answered
/// with something that looks computed.
const SCALARS: &[&str] = &[
    "abs",
    "ceil",
    "coalesce",
    "endNode",
    "exists",
    "floor",
    "head",
    "id",
    "keys",
    "labels",
    "last",
    "length",
    "nodes",
    "properties",
    "range",
    "relationships",
    "replace",
    "reverse",
    "round",
    "sign",
    "size",
    "split",
    "sqrt",
    "startNode",
    "substring",
    "tail",
    "toBoolean",
    "toFloat",
    "toInteger",
    "toLower",
    "toString",
    "toUpper",
    "trim",
    "ltrim",
    "rtrim",
    "type",
];

/// Whether `name` names a function this engine implements, aggregate or scalar. Function
/// names are case-insensitive.
#[must_use]
pub fn is_implemented(name: &str) -> bool {
    is_aggregate(name)
        || SCALARS
            .iter()
            .any(|implemented| name.eq_ignore_ascii_case(implemented))
}

/// Whether `name` names an aggregating function. Function names are case-insensitive.
#[must_use]
pub fn is_aggregate(name: &str) -> bool {
    AGGREGATES
        .iter()
        .any(|aggregate| name.eq_ignore_ascii_case(aggregate))
}

/// Whether `labels` satisfies the label (or relationship-type) expression `wanted`.
///
/// A label expression is written by the query, so it can nest; the depth it is read to is
/// bounded ([`MAX_EVALUATION_DEPTH`]) and an expression nesting deeper matches nothing,
/// which never widens what a query reaches.
#[must_use]
pub fn label_matches(wanted: &LabelExpr, labels: &[String]) -> bool {
    matched_labels(wanted, labels, 0)
}

/// [`label_matches`], counting how deep the label expression has been read.
fn matched_labels(wanted: &LabelExpr, labels: &[String], depth: usize) -> bool {
    if depth >= MAX_EVALUATION_DEPTH {
        return false;
    }
    let deeper = depth.saturating_add(1);
    match wanted {
        LabelExpr::Leaf(name) => labels.iter().any(|held| held == name),
        LabelExpr::Wildcard => !labels.is_empty(),
        LabelExpr::Negation(inner) => !matched_labels(inner, labels, deeper),
        LabelExpr::Conjunction(one, other) => {
            matched_labels(one, labels, deeper) && matched_labels(other, labels, deeper)
        }
        LabelExpr::Disjunction(one, other) => {
            matched_labels(one, labels, deeper) || matched_labels(other, labels, deeper)
        }
        // A label expression this engine does not read matches nothing, which is the
        // conservative answer: it never widens what a query reaches.
        _ => false,
    }
}

/// Every label named by a label expression that only NAMES labels, which is what a
/// created element can carry. A pattern that negates or combines labels describes what to
/// match, not what to create, so it is refused.
pub fn created_labels(wanted: &LabelExpr) -> Result<Vec<String>, QueryFailure> {
    labels_to_create(wanted, 0)
}

/// [`created_labels`], counting how deep the label expression has been read, so a query
/// cannot drive this descent past what the evaluation admits.
fn labels_to_create(wanted: &LabelExpr, depth: usize) -> Result<Vec<String>, QueryFailure> {
    if depth >= MAX_EVALUATION_DEPTH {
        return Err(QueryFailure::TooNested {
            ceiling: MAX_EVALUATION_DEPTH,
        });
    }
    match wanted {
        LabelExpr::Leaf(name) if !name.is_empty() => Ok(vec![name.clone()]),
        LabelExpr::Conjunction(one, other) => {
            let mut labels = labels_to_create(one, depth.saturating_add(1))?;
            labels.extend(labels_to_create(other, depth.saturating_add(1))?);
            Ok(labels)
        }
        _ => Err(QueryFailure::Refused(
            "A pattern that creates an element names its labels directly".to_string(),
        )),
    }
}

/// One stored property as the evaluation holds it.
#[must_use]
pub fn stored_value(stored: &Value) -> CypherValue {
    value_held(stored, 0)
}

/// [`stored_value`], counting how deep the value has been read. A value this engine
/// stored came through [`property_json`], and one a parameter carried came out of a
/// bounded JSON text, so nothing it is handed nests deeper than the evaluation admits; a
/// value that somehow did is read as absent rather than descended into.
fn value_held(stored: &Value, depth: usize) -> CypherValue {
    if depth >= MAX_EVALUATION_DEPTH {
        return CypherValue::Null;
    }
    let deeper = depth.saturating_add(1);
    match stored {
        Value::Null => CypherValue::Null,
        Value::Bool(flag) => CypherValue::Bool(*flag),
        Value::Number(number) => number.as_i64().map_or_else(
            || {
                number
                    .as_f64()
                    .filter(|number| number.is_finite())
                    .map_or(CypherValue::Null, CypherValue::Float)
            },
            CypherValue::Int,
        ),
        Value::String(text) => CypherValue::Str(text.clone()),
        Value::Array(items) => {
            CypherValue::List(items.iter().map(|item| value_held(item, deeper)).collect())
        }
        Value::Object(entries) => CypherValue::Map(
            entries
                .iter()
                .map(|(key, value)| (key.clone(), value_held(value, deeper)))
                .collect(),
        ),
    }
}

/// An element's stored properties as the evaluation holds them.
#[must_use]
pub fn properties_of(properties: &BTreeMap<String, Value>) -> BTreeMap<String, CypherValue> {
    properties
        .iter()
        .map(|(key, value)| (key.clone(), stored_value(value)))
        .collect()
}

/// One value as a property the store can hold.
///
/// A property is a primitive or a list of them: a graph element is not a property value,
/// so writing one is refused rather than stored as something else. The depth it is
/// written to is bounded, so a value cannot be stored more deeply nested than the
/// evaluation reads one back.
pub fn property_json(value: &CypherValue) -> Result<Value, QueryFailure> {
    property_stored(value, 0)
}

/// [`property_json`], counting how deep the value has been written.
fn property_stored(value: &CypherValue, depth: usize) -> Result<Value, QueryFailure> {
    if depth >= MAX_EVALUATION_DEPTH {
        return Err(QueryFailure::TooNested {
            ceiling: MAX_EVALUATION_DEPTH,
        });
    }
    match value {
        CypherValue::Null => Ok(Value::Null),
        CypherValue::Bool(flag) => Ok(Value::Bool(*flag)),
        CypherValue::Int(number) => Ok(Value::from(*number)),
        // Nothing in this engine's value domain is infinite or NaN (see `finite`), so a
        // number that cannot be written is a bug rather than a value a caller can send;
        // it is refused rather than stored as an absent property.
        CypherValue::Float(number) => ::serde_json::Number::from_f64(*number)
            .map(Value::Number)
            .ok_or_else(|| {
                QueryFailure::Constraint(
                    "A property value must be a number that can be written".to_string(),
                )
            }),
        CypherValue::Str(text) => Ok(Value::String(text.clone())),
        CypherValue::List(items) => {
            if items.len() > MAX_LIST_ITEMS {
                return Err(QueryFailure::ValueTooLarge {
                    size: items.len(),
                    ceiling: MAX_LIST_ITEMS,
                });
            }
            let mut stored = Vec::with_capacity(items.len());
            for item in items {
                stored.push(property_stored(item, depth.saturating_add(1))?);
            }
            Ok(Value::Array(stored))
        }
        CypherValue::Map(_) | CypherValue::Node(_) | CypherValue::Relationship(_) => {
            Err(QueryFailure::Constraint(
                "Property values can only be of primitive types or arrays of them".to_string(),
            ))
        }
    }
}

/// `value` read as truth, or `None` for a value that decides nothing (`null`).
#[must_use]
pub fn truth(value: &CypherValue) -> Option<bool> {
    match value {
        CypherValue::Bool(flag) => Some(*flag),
        CypherValue::Null => None,
        // Anything else in a boolean position is a type error the engine reports as
        // "unknown" rather than guessing a truth for it.
        _ => None,
    }
}

/// `value` as text, where it has one.
fn text_of(value: &CypherValue) -> Option<String> {
    match value {
        CypherValue::Str(text) => Some(text.clone()),
        _ => None,
    }
}

/// An argument as text, where it has one.
fn text_of_arg(value: Option<&CypherValue>) -> Option<String> {
    value.and_then(text_of)
}

/// `value` as a float, where it is a number.
fn float_of(value: &CypherValue) -> Option<f64> {
    match value {
        #[allow(clippy::cast_precision_loss)]
        CypherValue::Int(number) => Some(*number as f64),
        CypherValue::Float(number) => Some(*number),
        _ => None,
    }
}

/// `value` as an integer, where it is one.
fn integer_of(value: &CypherValue) -> Option<i64> {
    match value {
        CypherValue::Int(number) => Some(*number),
        _ => None,
    }
}

/// One integer argument a function requires.
fn integer_argument(value: Option<&CypherValue>, name: &str) -> Result<i64, QueryFailure> {
    value
        .and_then(integer_of)
        .ok_or_else(|| type_mismatch(name, "an integer"))
}

/// A value rendered the way `toString` renders one that is not already text.
fn display(value: &CypherValue) -> String {
    match value {
        CypherValue::Null => String::new(),
        CypherValue::Bool(flag) => flag.to_string(),
        CypherValue::Int(number) => number.to_string(),
        CypherValue::Float(number) => number.to_string(),
        CypherValue::Str(text) => text.clone(),
        CypherValue::Node(id) | CypherValue::Relationship(id) => id.clone(),
        CypherValue::List(_) | CypherValue::Map(_) => group_key(::std::slice::from_ref(value)),
    }
}

/// The items of a list expression's value.
fn list_items(value: &CypherValue) -> Result<Vec<CypherValue>, QueryFailure> {
    match value {
        CypherValue::List(items) => Ok(items.clone()),
        CypherValue::Null => Ok(Vec::new()),
        _ => Err(QueryFailure::Refused(
            "Type mismatch: expected a list".to_string(),
        )),
    }
}

/// `base[from..to]`: the items of a list between two bounds, each of which may be given
/// from either end and either of which may be left out.
fn slice(
    value: &CypherValue,
    from: Option<&CypherValue>,
    to: Option<&CypherValue>,
) -> Result<CypherValue, QueryFailure> {
    let items = match value {
        CypherValue::List(items) => items,
        CypherValue::Null => return Ok(CypherValue::Null),
        _ => {
            return Err(QueryFailure::Refused(
                "Type mismatch: a slice takes a list".to_string(),
            ))
        }
    };
    let length = i64::try_from(items.len()).unwrap_or(i64::MAX);
    // A bound given from the end counts back from the length; one outside the list is
    // clamped to it, which is what openCypher answers for a slice past either end.
    let resolve = |bound: Option<&CypherValue>, default: i64| -> Result<i64, QueryFailure> {
        match bound {
            None | Some(CypherValue::Null) => Ok(default),
            Some(CypherValue::Int(at)) => Ok(if *at < 0 {
                length.saturating_add(*at).max(0)
            } else {
                (*at).min(length)
            }),
            Some(_) => Err(QueryFailure::Refused(
                "Type mismatch: a slice bound is an integer".to_string(),
            )),
        }
    };
    let start = resolve(from, 0)?;
    let end = resolve(to, length)?;
    let taken = usize::try_from(end.saturating_sub(start)).unwrap_or(0);
    let skipped = usize::try_from(start).unwrap_or(0);
    Ok(CypherValue::List(
        items.iter().skip(skipped).take(taken).cloned().collect(),
    ))
}

/// One unary operation.
fn unary(op: UnaryOp, value: &CypherValue) -> Result<CypherValue, QueryFailure> {
    match op {
        UnaryOp::Not => Ok(truth(value).map_or(CypherValue::Null, |flag| CypherValue::Bool(!flag))),
        UnaryOp::Plus => Ok(value.clone()),
        UnaryOp::Minus => match value {
            CypherValue::Null => Ok(CypherValue::Null),
            CypherValue::Int(number) => Ok(CypherValue::Int(
                number.checked_neg().ok_or_else(overflowed)?,
            )),
            CypherValue::Float(number) => Ok(CypherValue::Float(-number)),
            _ => Err(QueryFailure::Refused(
                "Type mismatch: unary minus takes a number".to_string(),
            )),
        },
    }
}

/// One arithmetic operation, in openCypher's domain: integers stay integers, a float
/// anywhere makes the result a float, and `+` also joins strings and lists.
///
/// `budget` is charged for the SIZE of the value the operation is about to build, and
/// refuses one larger than this engine holds: `+` on two strings or two lists produces a
/// value as large as both operands together, so a query chaining such operations would
/// otherwise double what it allocates for a flat charge of one step.
fn arithmetic(
    budget: &Budget,
    op: BinaryOp,
    left: &CypherValue,
    right: &CypherValue,
) -> Result<CypherValue, QueryFailure> {
    if matches!(left, CypherValue::Null) || matches!(right, CypherValue::Null) {
        return Ok(CypherValue::Null);
    }
    if matches!(op, BinaryOp::Add) {
        match (left, right) {
            (CypherValue::Str(one), other) => {
                let joined = display(other);
                budget.charge_size(one.len().saturating_add(joined.len()), MAX_VALUE_CHARS)?;
                return Ok(CypherValue::Str(format!("{one}{joined}")));
            }
            (one, CypherValue::Str(other)) => {
                let joined = display(one);
                budget.charge_size(joined.len().saturating_add(other.len()), MAX_VALUE_CHARS)?;
                return Ok(CypherValue::Str(format!("{joined}{other}")));
            }
            (CypherValue::List(one), CypherValue::List(other)) => {
                budget.charge_size(one.len().saturating_add(other.len()), MAX_LIST_ITEMS)?;
                let mut joined = one.clone();
                joined.extend(other.iter().cloned());
                return Ok(CypherValue::List(joined));
            }
            (CypherValue::List(one), other) => {
                budget.charge_size(one.len().saturating_add(1), MAX_LIST_ITEMS)?;
                let mut joined = one.clone();
                joined.push(other.clone());
                return Ok(CypherValue::List(joined));
            }
            _ => {}
        }
    }
    match (left, right) {
        (CypherValue::Int(one), CypherValue::Int(other)) => {
            let answer = match op {
                BinaryOp::Add => one.checked_add(*other),
                BinaryOp::Sub => one.checked_sub(*other),
                BinaryOp::Mul => one.checked_mul(*other),
                BinaryOp::Div => {
                    if *other == 0 {
                        return Err(QueryFailure::Refused("/ by zero".to_string()));
                    }
                    one.checked_div(*other)
                }
                BinaryOp::Mod => {
                    if *other == 0 {
                        return Err(QueryFailure::Refused("/ by zero".to_string()));
                    }
                    one.checked_rem(*other)
                }
                // Exponentiation answers a float, as openCypher states for `^`.
                #[allow(clippy::cast_precision_loss)]
                _ => {
                    return finite((*one as f64).powf(*other as f64));
                }
            };
            Ok(CypherValue::Int(answer.ok_or_else(overflowed)?))
        }
        _ => {
            let (Some(one), Some(other)) = (float_of(left), float_of(right)) else {
                return Err(QueryFailure::Refused(format!(
                    "Type mismatch: `{}` takes numbers",
                    op.as_str()
                )));
            };
            if matches!(op, BinaryOp::Div | BinaryOp::Mod) && other == 0.0 {
                return Err(QueryFailure::Refused("/ by zero".to_string()));
            }
            finite(match op {
                BinaryOp::Add => one + other,
                BinaryOp::Sub => one - other,
                BinaryOp::Mul => one * other,
                BinaryOp::Div => one / other,
                BinaryOp::Mod => one % other,
                _ => one.powf(other),
            })
        }
    }
}

/// `number` as a value, or a refusal for a result that is not a number.
///
/// Nothing in this engine's value domain is infinite or NaN: a result that is not finite
/// is reported as the arithmetic failure it is, rather than carried on to be rendered as
/// an absent value, where a caller could not tell it from a property that was never set.
fn finite(number: f64) -> Result<CypherValue, QueryFailure> {
    if number.is_finite() {
        Ok(CypherValue::Float(number))
    } else {
        Err(QueryFailure::Refused(
            "The result of this arithmetic is not a number".to_string(),
        ))
    }
}

/// One comparison, in openCypher's three-valued logic: a comparison with `null` is
/// `null`, and two values of different kinds are not equal.
#[must_use]
pub fn compare(op: ComparisonOp, left: &CypherValue, right: &CypherValue) -> CypherValue {
    if matches!(left, CypherValue::Null) || matches!(right, CypherValue::Null) {
        return CypherValue::Null;
    }
    if matches!(op, ComparisonOp::Eq | ComparisonOp::Neq) {
        // Two numbers are equal by their numeric value, so an integer equals the float
        // of the same number; everything else is equal exactly when it is the same value
        // of the same kind, which is what the grouping key states.
        let equal = match (float_of(left), float_of(right)) {
            (Some(one), Some(other)) => one == other,
            _ => group_key(::std::slice::from_ref(left)) == group_key(::std::slice::from_ref(right)),
        };
        return CypherValue::Bool(if matches!(op, ComparisonOp::Eq) {
            equal
        } else {
            !equal
        });
    }
    if !comparable(left, right) {
        return CypherValue::Null;
    }
    let ordering = order(left, right);
    CypherValue::Bool(match op {
        ComparisonOp::Lt => ordering == ::std::cmp::Ordering::Less,
        ComparisonOp::Lte => ordering != ::std::cmp::Ordering::Greater,
        ComparisonOp::Gt => ordering == ::std::cmp::Ordering::Greater,
        ComparisonOp::Gte => ordering != ::std::cmp::Ordering::Less,
        // Handled above.
        ComparisonOp::Eq | ComparisonOp::Neq => false,
    })
}

/// Whether two values are of kinds openCypher orders against each other: numbers with
/// numbers, strings with strings, booleans with booleans, lists with lists.
///
/// A map and a graph element are NOT ordered: openCypher answers `null` for `<` on them,
/// so they are left out rather than compared by something this engine made up.
fn comparable(left: &CypherValue, right: &CypherValue) -> bool {
    matches!(
        (left, right),
        (CypherValue::Int(_) | CypherValue::Float(_), CypherValue::Int(_) | CypherValue::Float(_))
            | (CypherValue::Str(_), CypherValue::Str(_))
            | (CypherValue::Bool(_), CypherValue::Bool(_))
            | (CypherValue::List(_), CypherValue::List(_))
    )
}

/// A total order over values, which is what `ORDER BY`, `min()` and `max()` need.
///
/// Within one kind the order is the kind's own; across kinds it is the order of the kinds
/// below, with `null` last, so a sort is total whatever a row carries and two reads of
/// the same rows order them the same way. Numbers are ordered with `f64::total_cmp`, so
/// the relation stays TRANSITIVE whatever a row carries — a comparison that answered
/// "equal" for a value that is equal to nothing would make a sort's result depend on the
/// order the rows arrived in. Two maps, and two elements of the graph, are ordered by
/// their kind alone, which is all the order they have: `compare` answers `null` for `<`
/// on them rather than reaching here.
#[must_use]
pub fn order(left: &CypherValue, right: &CypherValue) -> ::std::cmp::Ordering {
    ordered(left, right, 0)
}

/// [`order`], counting how deep the comparison has descended: a nested list is compared
/// item by item, and the depth of a value is bounded ([`MAX_EVALUATION_DEPTH`]) so the
/// comparison cannot recurse further than the values it was handed.
fn ordered(left: &CypherValue, right: &CypherValue, depth: usize) -> ::std::cmp::Ordering {
    use ::std::cmp::Ordering;
    if depth >= MAX_EVALUATION_DEPTH {
        return Ordering::Equal;
    }
    match (left, right) {
        (CypherValue::Null, CypherValue::Null) => Ordering::Equal,
        (CypherValue::Null, _) => Ordering::Greater,
        (_, CypherValue::Null) => Ordering::Less,
        (CypherValue::Bool(one), CypherValue::Bool(other)) => one.cmp(other),
        (CypherValue::Int(one), CypherValue::Int(other)) => one.cmp(other),
        (CypherValue::Str(one), CypherValue::Str(other)) => one.cmp(other),
        (CypherValue::Node(one), CypherValue::Node(other))
        | (CypherValue::Relationship(one), CypherValue::Relationship(other)) => one.cmp(other),
        (CypherValue::List(one), CypherValue::List(other)) => {
            for (left, right) in one.iter().zip(other) {
                let ordering = ordered(left, right, depth.saturating_add(1));
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
            one.len().cmp(&other.len())
        }
        _ => match (float_of(left), float_of(right)) {
            (Some(one), Some(other)) => one.total_cmp(&other),
            // Different kinds: ordered by the kind, so the order is total.
            _ => kind_rank(left).cmp(&kind_rank(right)),
        },
    }
}

/// Where each kind of value sits in the order across kinds.
fn kind_rank(value: &CypherValue) -> u8 {
    match value {
        CypherValue::Bool(_) => 0,
        CypherValue::Int(_) | CypherValue::Float(_) => 1,
        CypherValue::Str(_) => 2,
        CypherValue::List(_) => 3,
        CypherValue::Map(_) => 4,
        CypherValue::Node(_) => 5,
        CypherValue::Relationship(_) => 6,
        CypherValue::Null => 7,
    }
}

/// A key that two value tuples share exactly when they are the same values: what
/// `DISTINCT` de-duplicates on and what a projection groups its aggregates by.
///
/// Built as text so a group can be held in an ordered map, and the encoding is
/// INJECTIVE: the kind is part of the key, so a number and the string of that number are
/// different keys, and every name and text is written with its length in front of it, so
/// no value can be spelled with the separators of another and collide with it.
#[must_use]
pub fn group_key(values: &[CypherValue]) -> String {
    let mut key = String::new();
    for value in values {
        append_key(&mut key, value, 0);
        key.push('\u{1f}');
    }
    key
}

/// One value's part of a [`group_key`], at `depth` levels of nesting. The depth of a
/// value is bounded ([`MAX_EVALUATION_DEPTH`]), so a value nested deeper than the
/// evaluation admits cannot be reached here.
fn append_key(key: &mut String, value: &CypherValue, depth: usize) {
    if depth >= MAX_EVALUATION_DEPTH {
        key.push_str("deeper");
        return;
    }
    match value {
        CypherValue::Null => key.push_str("null"),
        CypherValue::Bool(flag) => key.push_str(&format!("bool:{flag}")),
        CypherValue::Int(number) => key.push_str(&format!("int:{number}")),
        CypherValue::Float(number) => key.push_str(&format!("float:{number}")),
        CypherValue::Str(text) => key.push_str(&format!("str:{}:{text}", text.len())),
        CypherValue::Node(id) => key.push_str(&format!("node:{}:{id}", id.len())),
        CypherValue::Relationship(id) => key.push_str(&format!("rel:{}:{id}", id.len())),
        CypherValue::List(items) => {
            key.push_str(&format!("list:{}[", items.len()));
            for item in items {
                append_key(key, item, depth.saturating_add(1));
                key.push('\u{1e}');
            }
            key.push(']');
        }
        CypherValue::Map(entries) => {
            key.push_str(&format!("map:{}{{", entries.len()));
            for (name, entry) in entries {
                // The name carries its own length, so a name holding the separators
                // below cannot be read as a second entry.
                key.push_str(&format!("{}:{name}=", name.len()));
                append_key(key, entry, depth.saturating_add(1));
                key.push('\u{1e}');
            }
            key.push('}');
        }
    }
}

/// The refusal for a function handed a value of the wrong kind.
fn type_mismatch(name: &str, wanted: &str) -> QueryFailure {
    QueryFailure::Refused(format!("Type mismatch: {name}() takes {wanted}"))
}

/// The refusal for an integer operation whose result does not fit.
fn overflowed() -> QueryFailure {
    QueryFailure::Refused("Integer overflow evaluating the query".to_string())
}
